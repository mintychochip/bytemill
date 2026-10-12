"""Build a `Shape` from a hand-written rstom packet file."""

from __future__ import annotations

import re
from pathlib import Path

from bps.shapes.model import FieldShape, NewShape, SerializerShape, Shape, ShapeError
from bps.shapes.scan import (
    _advance_item_start,
    _before_cfg_test,
    _collect_attrs_before,
    _docs_and_attrs_before,
    _find_fn_body_start,
    _is_module_level_item,
    _leading_module_docs,
    _matching_brace,
    _parse_impl_header,
    _parse_struct_attrs,
    _read_ident,
    _read_type,
    _skip_balanced,
    _skip_item,
    _skip_trivia,
)


_PACKET_ROOT = Path("crates") / "rstom" / "src" / "network" / "packet"


_ALLOWED_TRAITS_FOR_CLS = frozenset(
    {"Packet", "ClientPacket", "ServerPacket", "ClientPacketGroup", "ServerPacketGroup"}
)


def extract_shape(
    source: str, cls: str, module: str, *, packet_path: Path | None = None
) -> Shape:
    stem = module.rsplit("/", 1)[-1]
    src = _before_cfg_test(source)
    module_docs = _leading_module_docs(src)
    pos = _skip_trivia(src, 0)
    struct_docs: tuple[str, ...] = ()
    derives: tuple[str, ...] = ()
    fields: tuple[FieldShape, ...] = ()
    form = "braced"
    serializer_struct: SerializerShape | None = None
    new_shape = NewShape(False, False, False, (), ())
    ser_fn_docs: tuple[str, ...] = ()
    ser_fn_must_use = False
    ser_style = "constant"
    ser_name = ""
    extras: str | None = None
    inherent_extra = False
    module_level_extra = False

    while pos < len(src):
        pos = _skip_trivia(src, pos)
        if pos >= len(src):
            break
        if src.startswith("pub struct ", pos) or src.startswith("struct ", pos):
            is_pub = src.startswith("pub struct ", pos)
            kw_len = len("pub struct ") if is_pub else len("struct ")
            name, after_name = _read_ident(src, pos + kw_len)
            if name != cls:
                if name.endswith("Serializer"):
                    docs, _ = _docs_and_attrs_before(src, pos)
                    serializer_struct = SerializerShape(
                        "struct", name, is_pub, docs, read_via_new=False
                    )
                    end = _skip_trivia(src, after_name)
                    if end < len(src) and src[end] == ";":
                        pos = end + 1
                    elif end < len(src) and src[end] == "{":
                        pos = _skip_balanced(src, end, "{", "}")
                    else:
                        pos = after_name
                    continue
                raise ShapeError(f"unexpected public type {name!r}")
            docs, attr_pos = _docs_and_attrs_before(src, pos)
            struct_docs, derives = docs, _parse_struct_attrs(src, attr_pos, pos)
            pos = _skip_trivia(src, after_name)
            if pos < len(src) and src[pos] == ";":
                form = "unit"
                fields = ()
                pos += 1
            elif pos < len(src) and src[pos] == "{":
                form = "braced"
                body_end = _matching_brace(src, pos)
                fields = _parse_struct_fields(src[pos + 1 : body_end])
                pos = body_end + 1
            else:
                raise ShapeError(f"malformed struct {cls}")
            continue

        if src.startswith("impl ", pos):
            trait, target, body_start, body_end, pos = _parse_impl_header(src, pos)
            body = src[body_start:body_end]
            if trait is None:
                if target == cls:
                    new_shape, ser_fn_docs, ser_fn_must_use, ser_style, ser_name, extra = (
                        _parse_inherent_impl(body, tuple(f.name for f in fields))
                    )
                    if extra:
                        inherent_extra = True
                continue
            if target == cls:
                if trait not in _ALLOWED_TRAITS_FOR_CLS:
                    raise ShapeError(f"impl {trait} for {cls}")
                continue
            if trait == "PacketSerializer" and target.endswith("Serializer"):
                via_new = _serializer_read_via_new(body, cls)
                if serializer_struct is not None and serializer_struct.name == target:
                    serializer_struct = SerializerShape(
                        serializer_struct.style,
                        serializer_struct.name,
                        serializer_struct.public,
                        serializer_struct.docs,
                        read_via_new=via_new,
                    )
                continue
            if target.endswith("Serializer"):
                raise ShapeError(f"impl {trait} for {target}")
            continue

        if _is_module_level_item(src, pos):
            if src.startswith("pub fn ", pos) or src.startswith("fn ", pos):
                if src.startswith("pub fn ", pos):
                    raise ShapeError("free function")
                module_level_extra = True
                pos = _skip_item(src, pos)
                continue
            if src.startswith("const fn ", pos) or src.startswith("pub const fn ", pos):
                module_level_extra = True
                pos = _skip_item(src, pos)
                continue
            if src.startswith("pub const ", pos):
                module_level_extra = True
                pos = _skip_item(src, pos)
                continue
            if src.startswith("const ", pos) and not (pos >= 4 and src[pos - 4 : pos] == "pub "):
                module_level_extra = True
                pos = _skip_item(src, pos)
                continue
            if src.startswith("pub enum ", pos) or src.startswith("pub type ", pos):
                raise ShapeError("unexpected public type")
        pos = _skip_item(src, pos)

    if serializer_struct is None and ser_style == "struct":
        raise ShapeError(f"missing serializer struct for {cls}")
    if serializer_struct is not None:
        ser = serializer_struct
    else:
        ser = SerializerShape(ser_style, ser_name, False, ())
    if inherent_extra or module_level_extra:
        extras = f"{stem}.ext.rs"
    elif packet_path is not None:
        ext_path = packet_path.parent / f"{stem}.ext.rs"
        if ext_path.is_file():
            extras = f"{stem}.ext.rs"
    return Shape(
        cls=cls,
        module=module,
        module_docs=module_docs,
        struct_docs=struct_docs,
        derives=derives,
        form=form,
        fields=fields,
        new=new_shape,
        serializer_fn_docs=ser_fn_docs,
        serializer_fn_must_use=ser_fn_must_use,
        serializer=ser,
        extras=extras,
    )


def _parse_struct_fields(body: str) -> tuple[FieldShape, ...]:
    fields: list[FieldShape] = []
    pos = 0
    while pos < len(body):
        pos = _skip_trivia(body, pos)
        if pos >= len(body):
            break
        if body[pos] == "}":
            break
        item = _advance_item_start(body, pos)
        docs, _ = _docs_and_attrs_before(body, item)
        pos = item
        if not body.startswith("pub ", pos):
            raise ShapeError("non-public struct field")
        pos += 4
        name, pos = _read_ident(body, pos)
        pos = _skip_trivia(body, pos)
        if pos >= len(body) or body[pos] != ":":
            raise ShapeError(f"malformed field {name}")
        pos += 1
        typ, pos = _read_type(body, pos)
        pos = _skip_trivia(body, pos)
        if pos < len(body) and body[pos] == ",":
            pos += 1
        fields.append(FieldShape(name, typ, docs))
    return tuple(fields)


def _is_trivial_new_body(inner: str, field_names: tuple[str, ...]) -> bool:
    stripped_lines = [
        line.strip()
        for line in inner.splitlines()
        if line.strip() and not line.strip().startswith("//")
    ]
    compact = " ".join(" ".join(stripped_lines).split())
    if compact == "Self":
        return len(field_names) == 0
    if not (compact.startswith("Self {") and compact.endswith("}")):
        return False
    inner_part = compact[len("Self {") : -1].strip()
    if not inner_part:
        return len(field_names) == 0
    parts = [part.strip() for part in inner_part.split(",") if part.strip()]
    if len(parts) != len(field_names):
        return False
    for part, name in zip(parts, field_names, strict=True):
        if part == name or part == f"{name}: {name}":
            continue
        return False
    return True


def _verbatim_new_body_lines(inner: str) -> tuple[str, ...]:
    physical = [ln for ln in inner.splitlines() if ln.strip()]
    if not physical:
        return ()
    min_indent = min(len(ln) - len(ln.lstrip()) for ln in physical)
    return tuple("        " + ln[min_indent:].rstrip() for ln in physical)


def _new_body_lines(fn_body: str, field_names: tuple[str, ...]) -> tuple[str, ...]:
    """Capture a non-trivial `new` body for the shape ledger (8-space indent)."""
    fn_body = fn_body.lstrip()
    if not fn_body.startswith("{"):
        return ()
    end = _matching_brace(fn_body, 0)
    inner = fn_body[1:end]
    if _is_trivial_new_body(inner, field_names):
        return ()
    return _verbatim_new_body_lines(inner)


def _serializer_read_via_new(impl_body: str, cls: str) -> bool:
    pos = 0
    while pos < len(impl_body):
        pos = _skip_trivia(impl_body, pos)
        if pos >= len(impl_body):
            break
        item = _advance_item_start(impl_body, pos)
        pos = item
        if not impl_body.startswith("fn ", pos):
            pos = _skip_item(impl_body, pos)
            continue
        pos += 3
        name, pos = _read_ident(impl_body, pos)
        if name != "read":
            sig_end = _find_fn_body_start(impl_body, pos)
            body_end = _matching_brace(impl_body, sig_end - 1) if sig_end > 0 else len(impl_body)
            pos = body_end + 1 if body_end < len(impl_body) else len(impl_body)
            continue
        sig_end = _find_fn_body_start(impl_body, pos)
        body_end = _matching_brace(impl_body, sig_end - 1) if sig_end > 0 else len(impl_body)
        read_body = impl_body[sig_end - 1 : body_end + 1] if sig_end > 0 else ""
        return bool(re.search(rf"Box::new\(\s*{re.escape(cls)}::new\s*\(", read_body))
    return False


def _parse_inherent_impl(
    body: str, field_names: tuple[str, ...]
) -> tuple[NewShape, tuple[str, ...], bool, str, str, bool]:
    new = NewShape(False, False, False, (), ())
    ser_docs: tuple[str, ...] = ()
    ser_must_use = False
    style = "constant"
    ser_name = ""
    extra = False
    pos = 0
    while pos < len(body):
        pos = _skip_trivia(body, pos)
        if pos >= len(body):
            break
        item = _advance_item_start(body, pos)
        docs, _ = _docs_and_attrs_before(body, item)
        attrs = _collect_attrs_before(body, item)
        pos = item
        is_const = False
        if body.startswith("pub ", pos):
            pos += 4
        if body.startswith("const ", pos):
            is_const = True
            pos += len("const ")
        if not body.startswith("fn ", pos):
            name, pos = _read_ident(body, pos)
            if name != "CLASS_NAME":
                extra = True
            pos = _skip_item(body, pos)
            continue
        if body.startswith("fn ", pos):
            pos += 3
            name, pos = _read_ident(body, pos)
            sig_end = _find_fn_body_start(body, pos)
            fn_body = body[sig_end:]
            body_end = _matching_brace(body, sig_end - 1) if sig_end > 0 else len(body)
            if name == "new":
                must_use = any(a.strip().startswith("#[must_use") for a in attrs)
                other_attrs = tuple(a for a in attrs if not a.strip().startswith("#[must_use"))
                braced = body[sig_end - 1 : body_end + 1] if sig_end > 0 else ""
                new_body = _new_body_lines(braced, field_names)
                new = NewShape(True, is_const, must_use, other_attrs, docs, new_body)
            elif name == "serializer":
                ser_docs = docs
                ser_must_use = any(a.strip().startswith("#[must_use") for a in attrs)
                style, ser_name = _parse_serializer_body(fn_body)
            else:
                extra = True
            pos = body_end + 1 if body_end < len(body) else len(body)
            continue
        pos = _skip_item(body, pos)
    return new, ser_docs, ser_must_use, style, ser_name, extra


def _parse_serializer_body(body: str) -> tuple[str, str]:
    if "ConstantSerializer::new" in body:
        m2 = re.search(r"ConstantSerializer::new\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)", body)
        if m2:
            return "constant", m2.group(1)
    m = re.search(r"Arc::new\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)", body)
    if m and m.group(1) != "ConstantSerializer":
        return "struct", m.group(1)
    raise ShapeError("unrecognized serializer() body")
