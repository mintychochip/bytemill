"""Build a `Shape` from a hand-written rstom packet file."""

from __future__ import annotations

import re
from dataclasses import replace
from pathlib import Path

from bps.emit_rstom import WIRE_TYPES
from bps.shapes.items import impl_body_bounds, scan_items
from bps.shapes.model import FieldShape, NewShape, SerializerShape, Shape, ShapeError
from bps.shapes.scan import (
    _advance_item_start,
    _collect_attrs_before,
    _docs_and_attrs_before,
    _find_fn_body_start,
    _leading_module_docs,
    _matching_brace,
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


_STRUCT_HEAD = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?struct\s+(\w+)", re.MULTILINE)
_USE_HEAD = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?use\s", re.MULTILINE)
_WIRE_CONSTS = frozenset(
    {wire for wire, _ in WIRE_TYPES.values()} | {"VAR_INT", "RAW_BYTES", "FIXED_BYTES"}
)
_GENERATOR_NAMES = frozenset(
    {
        "Arc",
        "ClientPacket",
        "ClientPacketGroup",
        "ConstantSerializer",
        "CoordVec",
        "NetworkBuffer",
        "Packet",
        "PacketSerializer",
        "PacketValue",
        "ServerPacket",
        "ServerPacketGroup",
        "Uuid",
        "downcast",
        "fixed_raw_bytes",
    }
) | _WIRE_CONSTS
_COORD_VEC_PATHS = frozenset({"crate::coordinate::Vec", "crate::coordinate::vec::Vec"})
_GENERATED_TAIL_USE = "pub use ext::*;"


def extract_shape(
    source: str, cls: str, module: str, *, packet_path: Path | None = None
) -> Shape:
    stem = module.rsplit("/", 1)[-1]
    items = scan_items(source)
    coord_names = _coordinate_names([it.text for it in items if it.kind == "use"])
    packet = [it for it in items if it.kind == "struct" and it.name == cls]
    if not packet:
        raise ShapeError(f"missing struct {cls}")
    struct_docs, derives, form, raw_fields = _packet_struct(packet[0].text, cls)
    fields = tuple(
        FieldShape(f.name, _normalise_types(f.type, coord_names), f.docs) for f in raw_fields
    )
    field_names = tuple(f.name for f in fields)
    new_shape = NewShape(False, False, False, (), ())
    ser_fn_docs: tuple[str, ...] = ()
    ser_fn_must_use = False
    ser_style = "constant"
    ser_name = ""
    serializer_structs: list[SerializerShape] = []
    serializer_impls: list[tuple[str, str]] = []
    use_texts: list[str] = []
    main_impl_seen = False
    inherent_extra = False
    module_extra = False
    inline_tests = False

    for item in items:
        if item.kind == "use":
            use_texts.append(item.text)
        elif item.kind == "struct" and item.name == cls:
            pass
        elif item.kind == "struct" and (item.name or "").endswith("Serializer"):
            head = _STRUCT_HEAD.search(item.text)
            docs, _ = _docs_and_attrs_before(item.text, head.start())
            serializer_structs.append(
                SerializerShape("struct", item.name or "", head.group(0).startswith("pub"), docs)
            )
        elif item.kind == "impl":
            body_start, body_end = impl_body_bounds(item.text)
            body = item.text[body_start:body_end]
            if item.name == cls and item.trait is None:
                new, ser_docs, ser_must_use, style, sname, extra = _parse_inherent_impl(
                    body, field_names
                )
                if new.present or sname:
                    if main_impl_seen:
                        module_extra = True
                    else:
                        main_impl_seen = True
                        new_shape, ser_fn_docs, ser_fn_must_use = new, ser_docs, ser_must_use
                        ser_style, ser_name, inherent_extra = style, sname, extra
                else:
                    module_extra = True
            elif item.name == cls and item.trait in _ALLOWED_TRAITS_FOR_CLS:
                pass
            elif item.trait == "PacketSerializer" and (item.name or "").endswith("Serializer"):
                serializer_impls.append((item.name or "", body))
            else:
                module_extra = True
        elif item.kind == "mod":
            has_block = item.text.rstrip().endswith("}")
            if has_block and item.cfg_test:
                inline_tests = True
            elif not has_block and (item.name == "ext" or (item.name == "tests" and item.cfg_test)):
                pass
            else:
                module_extra = True
        else:
            module_extra = True

    if ser_style == "struct":
        chosen = [s for s in serializer_structs if s.name == ser_name]
        if not chosen:
            raise ShapeError(f"missing serializer struct for {cls}")
    else:
        chosen = serializer_structs[:1]
    serializer_struct: SerializerShape | None = chosen[0] if chosen else None
    module_extra = module_extra or len(serializer_structs) > len(chosen)
    for target, body in serializer_impls:
        if serializer_struct is not None and target == serializer_struct.name:
            serializer_struct = replace(
                serializer_struct, read_via_new=_serializer_read_via_new(body, cls)
            )
        else:
            module_extra = True
    ser = (
        serializer_struct
        if serializer_struct is not None
        else SerializerShape(ser_style, ser_name, False, ())
    )

    raw_params = _normalise_types(new_shape.params or "", coord_names)
    default_params = ", ".join(f"{f.name}: {f.type}" for f in fields)
    params = raw_params if new_shape.present and raw_params != default_params else None
    new_shape = replace(new_shape, params=params)
    haystack = " ".join([*(f.type for f in fields), raw_params, *new_shape.body])
    sibling = packet_path.parent if packet_path is not None else None
    imports = _hand_imports(use_texts, haystack + _sibling_text(sibling, stem))

    tests_name = f"{stem}.tests.rs"
    tests = tests_name if inline_tests or _sibling_exists(sibling, tests_name) else None
    has_extra = module_extra or inherent_extra or _sibling_exists(sibling, f"{stem}.ext.rs")
    return Shape(
        cls=cls,
        module=module,
        module_docs=_leading_module_docs(source),
        struct_docs=struct_docs,
        derives=derives,
        form=form,
        fields=fields,
        new=new_shape,
        serializer_fn_docs=ser_fn_docs,
        serializer_fn_must_use=ser_fn_must_use,
        serializer=ser,
        extras=f"{stem}.ext.rs" if has_extra else None,
        imports=imports,
        tests=tests,
    )


def _sibling_exists(directory: Path | None, name: str) -> bool:
    return directory is not None and (directory / name).is_file()


def _sibling_text(directory: Path | None, stem: str) -> str:
    """Text of the packet's sibling files (`<stem>.*.rs`); their names count as uses."""
    if directory is None:
        return ""
    return "\n".join(p.read_text(encoding="utf-8") for p in sorted(directory.glob(f"{stem}.*.rs")))


def _packet_struct(
    text: str, cls: str
) -> tuple[tuple[str, ...], tuple[str, ...], str, tuple[FieldShape, ...]]:
    head = _STRUCT_HEAD.search(text)
    if head is None:
        raise ShapeError(f"malformed struct {cls}")
    docs, attr_pos = _docs_and_attrs_before(text, head.start())
    derives = _parse_struct_attrs(text, attr_pos, head.start())
    pos = _skip_trivia(text, head.end())
    if pos < len(text) and text[pos] == ";":
        return docs, derives, "unit", ()
    if pos < len(text) and text[pos] == "{":
        body_end = _matching_brace(text, pos)
        return docs, derives, "braced", _parse_struct_fields(text[pos + 1 : body_end])
    raise ShapeError(f"malformed struct {cls}")


def _normalise_types(text: str, coord_names: frozenset[str]) -> str:
    """The coordinate `Vec` under any bound name becomes `CoordVec`; `std::vec::Vec` becomes `Vec`."""
    if coord_names:
        alts = "|".join(re.escape(n) for n in sorted(coord_names))
        text = re.sub(rf"(?<![\w:])(?:{alts})(?!\w|\s*<)", "CoordVec", text)
    return text.replace("std::vec::Vec", "Vec")


def _use_statement(text: str) -> str:
    head = _USE_HEAD.search(text)
    statement = " ".join(text[head.start() :].split())
    return statement.replace("{ ", "{").replace(" }", "}").replace(",}", "}")


def _split_use(statement: str) -> tuple[str, tuple[str, ...]]:
    """`use a::{b, c as d};` gives (`use a::`, (`b`, `c as d`)); a leading `pub` stays in the head."""
    body = statement.removesuffix(";")
    if "{" in body:
        head, inner = body.split("{", 1)
        leaves = tuple(p.strip() for p in inner.removesuffix("}").split(",") if p.strip())
        return head, leaves
    if "::" in body:
        head, leaf = body.rsplit("::", 1)
        return head + "::", (leaf,)
    head, leaf = body.rsplit(" ", 1)
    return head + " ", (leaf,)


def _leaf_binding(head: str, leaf: str) -> tuple[str, str]:
    """Full path and bound name of one `use` leaf; `self` binds the module named by the head."""
    module = re.sub(r"^(?:pub(?:\([^)]*\))?\s+)?use\s+", "", head).removesuffix("::").strip()
    name, _, alias = leaf.partition(" as ")
    name = name.strip()
    if name == "self":
        return module, module.rsplit("::", 1)[-1]
    path = f"{module}::{name}" if module else name
    return path, alias.strip() or name


def _use_bindings(text: str) -> list[tuple[str, str]]:
    head, leaves = _split_use(_use_statement(text))
    return [_leaf_binding(head, leaf) for leaf in leaves]


def _coordinate_names(use_texts: list[str]) -> frozenset[str]:
    """Names the coordinate `Vec` is bound to in this file: `Vec`, or its `as` alias."""
    return frozenset(
        bound
        for text in use_texts
        for path, bound in _use_bindings(text)
        if path in _COORD_VEC_PATHS
    )


def _rebuild_use(head: str, leaves: list[str]) -> str | None:
    if not leaves:
        return None
    if len(leaves) == 1:
        if leaves[0] == "self":
            return head.removesuffix("::") + ";"
        return f"{head}{leaves[0]};"
    return f"{head}{{{', '.join(leaves)}}};"


def _hand_imports(use_texts: list[str], needed: str) -> tuple[str, ...]:
    """Hand `use` lines pruned to the names the packet mentions; `pub use` lines verbatim."""
    kept: list[str] = []
    for text in use_texts:
        statement = _use_statement(text)
        if statement == _GENERATED_TAIL_USE:
            continue
        if statement.startswith("pub"):
            kept.append(statement)
            continue
        head, leaves = _split_use(statement)
        survivors: list[str] = []
        for leaf in leaves:
            path, bound = _leaf_binding(head, leaf)
            if leaf == "*" or path in _COORD_VEC_PATHS or bound in _GENERATOR_NAMES:
                continue
            if bound == "_" or re.search(rf"\b{re.escape(bound)}\b", needed):
                survivors.append(leaf)
        rebuilt = _rebuild_use(head, survivors)
        if rebuilt is not None:
            kept.append(rebuilt)
    return tuple(kept)


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
                new = NewShape(
                    True, is_const, must_use, other_attrs, docs, new_body, _raw_params(body, pos)
                )
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


def _raw_params(body: str, after_name: int) -> str:
    """The `fn new(...)` parameter list, whitespace collapsed, without a trailing comma."""
    open_paren = body.index("(", after_name)
    close = _skip_balanced(body, open_paren, "(", ")") - 1
    return " ".join(body[open_paren + 1 : close].split()).rstrip(",").rstrip()
