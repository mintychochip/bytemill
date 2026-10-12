"""Extract the public API shape of a hand-written rstom packet file."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path

from bps.ast import PacketDecl, ResolvedSchema
from bps.emit_rstom import _packet_fields
from bps.errors import EmitError
from bps.minestom import Unit, build_units

_PACKET_ROOT = Path("crates") / "rstom" / "src" / "network" / "packet"

_ALLOWED_TRAITS_FOR_CLS = frozenset(
    {"Packet", "ClientPacket", "ServerPacket", "ClientPacketGroup", "ServerPacketGroup"}
)


class ShapeError(Exception):
    """The hand file has a construct the extractor cannot represent."""


@dataclass(frozen=True)
class FieldShape:
    name: str
    type: str
    docs: tuple[str, ...]


@dataclass(frozen=True)
class NewShape:
    present: bool
    const: bool
    must_use: bool
    attrs: tuple[str, ...]
    docs: tuple[str, ...]
    body: tuple[str, ...] = ()


@dataclass(frozen=True)
class SerializerShape:
    style: str
    name: str
    public: bool
    docs: tuple[str, ...]
    read_via_new: bool = False


@dataclass(frozen=True)
class Shape:
    cls: str
    module: str
    module_docs: tuple[str, ...]
    struct_docs: tuple[str, ...]
    derives: tuple[str, ...]
    form: str
    fields: tuple[FieldShape, ...]
    new: NewShape
    serializer_fn_docs: tuple[str, ...]
    serializer_fn_must_use: bool
    serializer: SerializerShape
    extras: str | None


def hand_packet_path(workspace: Path, unit: Unit) -> Path:
    return (
        workspace
        / _PACKET_ROOT
        / unit.cls.group
        / unit.cls.package
        / f"{unit.cls.stem}.rs"
    )


def module_path(unit: Unit) -> str:
    return f"{unit.cls.group}/{unit.cls.package}/{unit.cls.stem}"


def bps_field_pairs(pkt: PacketDecl, resolved: ResolvedSchema) -> list[tuple[str, str]]:
    fields, _ = _packet_fields(pkt, resolved)
    out: list[tuple[str, str]] = []
    for f in fields:
        base = f"Vec<{f.rtype}>" if f.repeat_mode is not None else f.rtype
        mapped = f"Option<{base}>" if f.guard is not None else base
        out.append((f.ident, mapped))
    return out


def mismatch(shape: Shape, bps_fields: list[tuple[str, str]]) -> list[str]:
    hand = [(f.name, f.type) for f in shape.fields]
    if len(hand) != len(bps_fields):
        return [f"field count: hand has {len(hand)}, BPS has {len(bps_fields)}"]
    for (hn, ht), (bn, bt) in zip(hand, bps_fields):
        if hn != bn:
            return [f"field {hn}: hand name {hn!r}, BPS name {bn!r}"]
        if ht != bt:
            return [f"field {hn}: hand type {ht!r}, BPS type {bt!r}"]
    return []


def shape_to_json(s: Shape) -> str:
    obj = {
        "class": s.cls,
        "module": s.module,
        "module_docs": list(s.module_docs),
        "struct": {
            "docs": list(s.struct_docs),
            "derives": list(s.derives),
            "form": s.form,
        },
        "fields": [{"name": f.name, "type": f.type, "docs": list(f.docs)} for f in s.fields],
        "new": {
            "present": s.new.present,
            "must_use": s.new.must_use,
            "const": s.new.const,
            "attrs": list(s.new.attrs),
            "docs": list(s.new.docs),
            "body": list(s.new.body),
        },
        "serializer_fn": {
            "docs": list(s.serializer_fn_docs),
            "must_use": s.serializer_fn_must_use,
        },
        "serializer_struct": {
            "style": s.serializer.style,
            "name": s.serializer.name,
            "pub": s.serializer.public,
            "docs": list(s.serializer.docs),
            "read_via_new": s.serializer.read_via_new,
        },
        "extras": s.extras,
    }
    return json.dumps(obj, indent=2) + "\n"


def shape_from_json(text: str) -> Shape:
    obj = json.loads(text)
    return Shape(
        cls=obj["class"],
        module=obj["module"],
        module_docs=tuple(obj["module_docs"]),
        struct_docs=tuple(obj["struct"]["docs"]),
        derives=tuple(obj["struct"]["derives"]),
        form=obj["struct"]["form"],
        fields=tuple(
            FieldShape(name=f["name"], type=f["type"], docs=tuple(f["docs"])) for f in obj["fields"]
        ),
        new=NewShape(
            present=obj["new"]["present"],
            const=obj["new"]["const"],
            must_use=obj["new"]["must_use"],
            attrs=tuple(obj["new"]["attrs"]),
            docs=tuple(obj["new"]["docs"]),
            body=tuple(obj["new"].get("body", ())),
        ),
        serializer_fn_docs=tuple(obj["serializer_fn"]["docs"]),
        serializer_fn_must_use=obj["serializer_fn"]["must_use"],
        serializer=SerializerShape(
            style=obj["serializer_struct"]["style"],
            name=obj["serializer_struct"]["name"],
            public=obj["serializer_struct"]["pub"],
            docs=tuple(obj["serializer_struct"]["docs"]),
            read_via_new=bool(obj["serializer_struct"].get("read_via_new", False)),
        ),
        extras=obj["extras"],
    )


def load_ledger(dir: Path) -> dict[str, Shape]:
    out: dict[str, Shape] = {}
    for path in sorted(dir.rglob("*.json")):
        rel = path.relative_to(dir)
        module = rel.with_suffix("").as_posix()
        out[module] = shape_from_json(path.read_text(encoding="utf-8"))
    return out


def packet_report(
    unit: Unit,
    resolved: ResolvedSchema,
    workspace: Path,
) -> str:
    path = hand_packet_path(workspace, unit)
    if not path.is_file():
        return f"missing hand file {path}"
    try:
        shape = extract_shape(
            path.read_text(encoding="utf-8"),
            unit.cls.name,
            module_path(unit),
            packet_path=path,
        )
    except ShapeError as err:
        return str(err)
    reasons = mismatch(shape, bps_field_pairs(unit.pkt, resolved))
    if reasons:
        return "; ".join(reasons)
    return "eligible"


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


def _before_cfg_test(source: str) -> str:
    idx = source.find("#[cfg(test)]")
    return source[:idx] if idx >= 0 else source


def _doc_line_text(line: str) -> str:
    if line.startswith("//!"):
        rest = line[3:]
        return rest[1:] if rest.startswith(" ") else rest
    if line.startswith("///"):
        rest = line[3:]
        return rest[1:] if rest.startswith(" ") else rest
    return line


def _leading_module_docs(src: str) -> tuple[str, ...]:
    lines: list[str] = []
    for line in src.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("//!"):
            text = _doc_line_text(stripped)
            if text.startswith("Generated by"):
                continue
            if not text:
                continue
            lines.append(text)
        elif stripped == "" and not lines:
            continue
        else:
            break
    return tuple(lines)


def _skip_trivia(src: str, pos: int) -> int:
    while pos < len(src):
        if src.startswith("//", pos) and not src.startswith("///", pos):
            nl = src.find("\n", pos)
            pos = len(src) if nl < 0 else nl + 1
            continue
        if src.startswith("/*", pos):
            end = src.find("*/", pos + 2)
            pos = end + 2 if end >= 0 else len(src)
            continue
        if src[pos] in " \t\r\n":
            pos += 1
            continue
        return pos
    return pos


def _line_start(src: str, pos: int) -> int:
    nl = src.rfind("\n", 0, pos)
    return 0 if nl < 0 else nl + 1


def _attr_block_end(src: str, hash_pos: int) -> int:
    """Index after the `]` that closes `#[` at ``hash_pos``."""
    if not src.startswith("#[", hash_pos):
        raise ShapeError("expected #[")
    depth = 0
    i = hash_pos + 1
    while i < len(src):
        if src[i] in "\"'":
            i = _skip_rust_string(src, i)
            continue
        ch = src[i]
        if ch == "[":
            depth += 1
        elif ch == "]":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(src)


def _line_in_attr_prefix(src: str, line_begin: int, item_line: int) -> bool:
    i = 0
    while i < item_line:
        if src.startswith("#[", i):
            end = _attr_block_end(src, i)
            if line_begin < end and line_begin >= _line_start(src, i):
                return True
            i = end
            continue
        i += 1
    return False


def _docs_and_attrs_before(src: str, item_pos: int) -> tuple[tuple[str, ...], int]:
    item = _advance_item_start(src, item_pos)
    item_line = _line_start(src, item)
    attr_start = item
    doc_lines: list[str] = []
    line_begin = item_line
    if line_begin > 0:
        line_begin = _line_start(src, line_begin - 1)
    else:
        return (), item
    while line_begin >= 0:
        line_end = src.find("\n", line_begin)
        if line_end < 0:
            line_end = len(src)
        stripped = src[line_begin:line_end].strip()
        if stripped.startswith("///"):
            doc_lines.insert(0, _doc_line_text(stripped))
        elif stripped.startswith("#["):
            attr_start = min(attr_start, line_begin)
        elif stripped == "":
            pass
        elif _line_in_attr_prefix(src, line_begin, item_line):
            pass
        else:
            break
        if line_begin == 0:
            break
        line_begin = _line_start(src, line_begin - 1)
    return tuple(doc_lines), attr_start


def _read_ident(src: str, pos: int) -> tuple[str, int]:
    pos = _skip_trivia(src, pos)
    m = re.match(r"[A-Za-z_][A-Za-z0-9_]*", src[pos:])
    if not m:
        raise ShapeError("expected identifier")
    return m.group(0), pos + m.end()


def _matching_brace(src: str, open_pos: int) -> int:
    return _skip_balanced(src, open_pos, "{", "}") - 1


def _skip_rust_string(src: str, pos: int) -> int:
    if pos >= len(src):
        return pos
    if src[pos] == '"':
        return _skip_string(src, pos)
    if src[pos] == "'":
        nxt = pos + 1
        if nxt < len(src) and (src[nxt].isalnum() or src[nxt] == "_"):
            nxt += 1
            while nxt < len(src) and (src[nxt].isalnum() or src[nxt] == "_"):
                nxt += 1
            return nxt
        return _skip_string(src, pos)
    return pos + 1


def _skip_balanced(src: str, pos: int, open_ch: str, close_ch: str) -> int:
    if pos >= len(src) or src[pos] != open_ch:
        return pos
    depth = 0
    i = pos
    while i < len(src):
        ch = src[i]
        if ch == '"' or ch == "'":
            i = _skip_rust_string(src, i)
            continue
        if ch == open_ch:
            depth += 1
        elif ch == close_ch:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(src)


def _skip_string(src: str, pos: int) -> int:
    quote = src[pos]
    i = pos + 1
    while i < len(src):
        if src[i] == "\\":
            i += 2
            continue
        if src[i] == quote:
            return i + 1
        i += 1
    return len(src)


def _parse_struct_attrs(src: str, attr_start: int, struct_pos: int) -> tuple[str, ...]:
    derives: list[str] = []
    pos = attr_start
    while pos < struct_pos:
        pos = _skip_trivia(src, pos)
        if pos >= struct_pos:
            break
        if not src.startswith("#[", pos):
            break
        end = _matching_attr(src, pos)
        attr_text = src[pos:end]
        inner = attr_text[2:-1].strip()
        if inner.startswith("derive("):
            derives = _split_derives(inner[len("derive(") : inner.rfind(")")])
        else:
            raise ShapeError(f"attribute {inner.split('(')[0]}")
        pos = end
    if not derives:
        raise ShapeError("missing derive on struct")
    return tuple(derives)


def _matching_attr(src: str, pos: int) -> int:
    if not src.startswith("#[", pos):
        return pos
    depth = 0
    i = pos + 1
    while i < len(src):
        ch = src[i]
        if ch == "[":
            depth += 1
        elif ch == "]":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return len(src)


def _split_derives(inner: str) -> list[str]:
    out: list[str] = []
    depth = 0
    cur: list[str] = []
    for ch in inner:
        if ch in "(<[":
            depth += 1
        elif ch in ")>]":
            depth -= 1
        elif ch == "," and depth == 0:
            part = "".join(cur).strip()
            if part:
                out.append(part)
            cur = []
            continue
        cur.append(ch)
    part = "".join(cur).strip()
    if part:
        out.append(part)
    return out


def _advance_item_start(src: str, pos: int) -> int:
    while True:
        pos = _skip_trivia(src, pos)
        if pos >= len(src):
            return pos
        if src.startswith("#[", pos):
            pos = _matching_attr(src, pos)
            continue
        line_end = src.find("\n", pos)
        if line_end < 0:
            line_end = len(src)
        line = src[pos:line_end].strip()
        if line.startswith("///") or line.startswith("#["):
            pos = line_end + 1
            continue
        return pos


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


def _read_type(src: str, pos: int) -> tuple[str, int]:
    pos = _skip_trivia(src, pos)
    start = pos
    depth_angle = 0
    depth_paren = 0
    while pos < len(src):
        ch = src[pos]
        if ch == '"' or ch == "'":
            pos = _skip_rust_string(src, pos)
            continue
        if ch == "<":
            depth_angle += 1
        elif ch == ">" and depth_angle > 0:
            depth_angle -= 1
        elif ch == "(":
            depth_paren += 1
        elif ch == ")":
            depth_paren -= 1
        elif ch == "," and depth_angle == 0 and depth_paren == 0:
            break
        elif ch == "}" and depth_angle == 0 and depth_paren == 0:
            break
        pos += 1
    return src[start:pos].strip(), pos


def _parse_impl_header(src: str, pos: int) -> tuple[str | None, str, int, int, int]:
    pos += len("impl ")
    pos = _skip_trivia(src, pos)
    trait: str | None
    if _lookahead_impl_for(src, pos):
        trait, pos = _read_ident(src, pos)
        pos = _skip_trivia(src, pos)
        if not src.startswith("for ", pos):
            raise ShapeError("malformed impl")
        pos += 4
        target, pos = _read_ident(src, pos)
    else:
        trait = None
        target, pos = _read_ident(src, pos)
    pos = _skip_trivia(src, pos)
    if pos >= len(src) or src[pos] != "{":
        raise ShapeError("malformed impl body")
    body_start = pos + 1
    body_end = _matching_brace(src, pos)
    return trait, target, body_start, body_end, body_end + 1


def _lookahead_impl_for(src: str, pos: int) -> bool:
    name, after = _read_ident(src, pos)
    after = _skip_trivia(src, after)
    return src.startswith("for ", after)


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


def _rskip_trivia(src: str, pos: int) -> int:
    while pos > 0 and src[pos - 1] in " \t\r\n":
        pos -= 1
    return pos


def _collect_attrs_before(src: str, item_pos: int) -> list[str]:
    attrs: list[str] = []
    pos = _rskip_trivia(src, item_pos)
    while pos > 0:
        pos = _rskip_trivia(src, pos)
        if pos <= 0:
            break
        if src[pos - 1] != "]":
            line_begin = _line_start(src, pos - 1)
            stripped = src[line_begin:pos].strip()
            if stripped.startswith("///"):
                break
            if stripped.startswith("pub ") or stripped.startswith("fn "):
                break
            pos = line_begin
            continue
        close = pos - 1
        depth = 0
        open_idx = None
        i = close
        while i >= 0:
            if src[i] == "]":
                depth += 1
            elif src[i] == "[":
                depth -= 1
                if depth == 0:
                    open_idx = i
                    break
            i -= 1
        if open_idx is None or open_idx == 0 or src[open_idx - 1] != "#":
            break
        start = open_idx - 1
        attrs.insert(0, src[start : close + 1])
        pos = start
    return attrs


def _find_fn_body_start(src: str, pos: int) -> int:
    depth_paren = 0
    depth_angle = 0
    i = pos
    while i < len(src):
        ch = src[i]
        if ch == '"' or ch == "'":
            i = _skip_rust_string(src, i)
            continue
        if ch == "(":
            depth_paren += 1
        elif ch == ")":
            depth_paren -= 1
        elif ch == "<":
            depth_angle += 1
        elif ch == ">" and depth_angle > 0:
            depth_angle -= 1
        elif ch == "{" and depth_paren == 0 and depth_angle == 0:
            return i + 1
        i += 1
    return len(src)


def _parse_serializer_body(body: str) -> tuple[str, str]:
    if "ConstantSerializer::new" in body:
        m2 = re.search(r"ConstantSerializer::new\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)", body)
        if m2:
            return "constant", m2.group(1)
    m = re.search(r"Arc::new\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*\)", body)
    if m and m.group(1) != "ConstantSerializer":
        return "struct", m.group(1)
    raise ShapeError("unrecognized serializer() body")


def _is_module_level_item(src: str, pos: int) -> bool:
    return src.startswith(("pub ", "fn ", "const ", "use ", "impl ", "struct ", "#["), pos)


def _skip_item(src: str, pos: int) -> int:
    pos = _skip_trivia(src, pos)
    if src.startswith("use ", pos):
        semi = src.find(";", pos)
        return semi + 1 if semi >= 0 else len(src)
    if src.startswith("#[", pos):
        return _matching_attr(src, pos)
    if src.startswith("impl ", pos):
        _, _, _, _, end = _parse_impl_header(src, pos)
        return end
    if src.startswith(("pub struct ", "struct ", "pub enum ", "pub fn ", "fn ", "pub const ", "const "), pos):
        return _skip_declaration(src, pos)
    return pos + 1


def _skip_until_semicolon(src: str, pos: int) -> int:
    depth_paren = 0
    depth_brace = 0
    while pos < len(src):
        ch = src[pos]
        if ch in "\"'":
            pos = _skip_rust_string(src, pos)
            continue
        if depth_paren == 0 and depth_brace == 0 and ch == ";":
            return pos
        if ch == "(":
            depth_paren += 1
        elif ch == ")":
            depth_paren -= 1
        elif ch == "{":
            depth_brace += 1
        elif ch == "}":
            depth_brace -= 1
        pos += 1
    return len(src)


def _skip_until_top_level(src: str, pos: int, stop: str) -> int:
    depth_angle = 0
    depth_paren = 0
    depth_brace = 0
    while pos < len(src):
        ch = src[pos]
        if ch in "\"'":
            pos = _skip_rust_string(src, pos)
            continue
        if depth_angle == 0 and depth_paren == 0 and depth_brace == 0 and ch in stop:
            return pos
        if ch == "<":
            depth_angle += 1
        elif ch == ">" and depth_angle > 0:
            depth_angle -= 1
        elif ch == "(":
            depth_paren += 1
        elif ch == ")":
            depth_paren -= 1
        elif ch == "{":
            depth_brace += 1
        elif ch == "}":
            depth_brace -= 1
        pos += 1
    return len(src)


def _skip_function_decl(src: str, pos: int) -> int:
    pos = _skip_trivia(src, pos)
    if not src.startswith("fn ", pos):
        raise ShapeError("malformed function")
    pos += 3
    _, pos = _read_ident(src, pos)
    pos = _skip_trivia(src, pos)
    if pos < len(src) and src[pos] == "<":
        pos = _skip_balanced(src, pos, "<", ">")
    if pos < len(src) and src[pos] == "(":
        pos = _skip_balanced(src, pos, "(", ")")
    pos = _skip_trivia(src, pos)
    if pos + 1 < len(src) and src[pos : pos + 2] == "->":
        pos += 2
        pos = _skip_trivia(src, pos)
        pos = _skip_until_top_level(src, pos, "{;")
    pos = _skip_trivia(src, pos)
    if pos < len(src) and src[pos] == ";":
        return pos + 1
    if pos < len(src) and src[pos] == "{":
        return _skip_balanced(src, pos, "{", "}")
    semi = src.find(";", pos)
    return semi + 1 if semi >= 0 else len(src)


def _skip_const_item(src: str, pos: int) -> int:
    pos = _skip_trivia(src, pos)
    if not src.startswith("const ", pos):
        raise ShapeError("malformed const")
    pos += len("const ")
    _, pos = _read_ident(src, pos)
    pos = _skip_trivia(src, pos)
    if pos < len(src) and src[pos] == ":":
        pos += 1
        pos = _skip_trivia(src, pos)
        pos = _skip_until_top_level(src, pos, "=;")
    pos = _skip_trivia(src, pos)
    if pos < len(src) and src[pos] == "=":
        pos += 1
        pos = _skip_until_semicolon(src, pos)
    pos = _skip_trivia(src, pos)
    if pos < len(src) and src[pos] == ";":
        return pos + 1
    return len(src)


def _skip_type_decl(src: str, pos: int) -> int:
    pos = _skip_trivia(src, pos)
    if src.startswith("pub ", pos):
        pos += 4
        pos = _skip_trivia(src, pos)
    if src.startswith("struct ", pos):
        pos += len("struct ")
        _, pos = _read_ident(src, pos)
        pos = _skip_trivia(src, pos)
        if pos < len(src) and src[pos] == "<":
            pos = _skip_balanced(src, pos, "<", ">")
        pos = _skip_trivia(src, pos)
        if pos < len(src) and src[pos] == ";":
            return pos + 1
        if pos < len(src) and src[pos] == "{":
            return _skip_balanced(src, pos, "{", "}")
        return pos
    if src.startswith("enum ", pos):
        pos += len("enum ")
        _, pos = _read_ident(src, pos)
        pos = _skip_trivia(src, pos)
        if pos < len(src) and src[pos] == "<":
            pos = _skip_balanced(src, pos, "<", ">")
        pos = _skip_trivia(src, pos)
        if pos < len(src) and src[pos] == "{":
            return _skip_balanced(src, pos, "{", "}")
        semi = src.find(";", pos)
        return semi + 1 if semi >= 0 else len(src)
    raise ShapeError("malformed type declaration")


def _skip_declaration(src: str, pos: int) -> int:
    pos = _skip_trivia(src, pos)
    if src.startswith("pub ", pos):
        after_pub = pos + 4
        if src.startswith("const fn ", after_pub):
            return _skip_function_decl(src, after_pub + len("const "))
        if src.startswith("fn ", after_pub):
            return _skip_function_decl(src, after_pub)
        if src.startswith("const ", after_pub):
            return _skip_const_item(src, after_pub)
        if src.startswith("struct ", after_pub) or src.startswith("enum ", after_pub):
            return _skip_type_decl(src, pos)
    if src.startswith("const fn ", pos):
        return _skip_function_decl(src, pos + len("const "))
    if src.startswith("const ", pos):
        return _skip_const_item(src, pos)
    if src.startswith("fn ", pos):
        return _skip_function_decl(src, pos)
    if src.startswith("struct ", pos) or src.startswith("enum ", pos):
        return _skip_type_decl(src, pos)
    semi = src.find(";", pos)
    return semi + 1 if semi >= 0 else len(src)


def extract_shapes_for_units(
    resolved: ResolvedSchema,
    workspace: Path,
    units: list[Unit],
    *,
    eligible_only: bool = False,
    class_names: set[str] | None = None,
) -> list[tuple[Unit, Shape]]:
    out: list[tuple[Unit, Shape]] = []
    for unit in units:
        if class_names is not None and unit.cls.name not in class_names:
            continue
        path = hand_packet_path(workspace, unit)
        if not path.is_file():
            continue
        try:
            shape = extract_shape(
                path.read_text(encoding="utf-8"),
                unit.cls.name,
                module_path(unit),
                packet_path=path,
            )
        except ShapeError:
            if eligible_only:
                continue
            raise
        if eligible_only and mismatch(shape, bps_field_pairs(unit.pkt, resolved)):
            continue
        out.append((unit, shape))
    return out
