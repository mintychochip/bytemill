"""Rust emitter for the `rstom-packets` crate shape (`../rstom`).

A resolved BPS schema is compiled into a crate whose payloads implement the same
contract `tools/packets_gen` emits:

* `packet NAME { field: type }` -> `struct NAME { pub field: Rust }`
  plus `<Name>Serializer` implementing `PacketSerializer` over `NetworkBuffer`
  and `impl Packet for NAME { const CLASS_NAME }`.
* Wire-type mapping matches `NetworkBuffer`'s type constants.
* `route state.direction id => packet` -> the positional id tables under
  `packet_vanilla/`, one module per (side, state).
* The interpreter and this emitter share the grammar, so a schema that
  round-trips in Python emits a serializer that round-trips in Rust.
* Each packet's `roundtrip_*.rs` pins the Rust encoder against the Python
  interpreter's bytes for the same synthetic value (`bps/synth.py`), except
  `debug_block_value`, whose block-state properties payload has no documented
  boundary and stays a bare `rest` marker; that one keeps the encode/decode
  consistency check and says so in the test.

Registry-keyed payloads (`slot`, `nbt`, `particle_data`, `id_or_payload`) are
carried as the opaque byte frames the schema declares, which keeps the crate
compilable without rstom's domain types; `tests/roundtrip.rs` covers every
emitted payload.
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

from bps import ast
from bps.ast import BlockItem, PacketDecl, ResolvedSchema, RouteDecl, TypeRef
from bps.interp.engine import encode_packet
from bps.synth import synthesize_packet

# BPS type -> (NetworkBuffer type constant, Rust type). Mirrors the constants the
# rstom crate re-exports from `rstom::network::types`.
WIRE_TYPES: dict[str, tuple[str, str]] = {
    "u8": ("BYTE", "i8"),
    "i8": ("BYTE", "i8"),
    "u16be": ("UNSIGNED_SHORT", "u16"),
    "i16be": ("SHORT", "i16"),
    "u32be": ("UNSIGNED_INT", "u32"),
    "i32be": ("INT", "i32"),
    "u64be": ("LONG", "i64"),
    "i64be": ("LONG", "i64"),
    "f32be": ("FLOAT", "f32"),
    "f64be": ("DOUBLE", "f64"),
    "varint": ("VAR_INT", "i32"),
    "varlong": ("VAR_LONG", "i64"),
    "bool": ("BOOLEAN", "bool"),
    "boolean": ("BOOLEAN", "bool"),
    "angle": ("BYTE", "i8"),
    "position": ("BLOCK_POSITION", "CoordVec"),
    "uuid": ("UUID", "Uuid"),
    "string": ("STRING", "String"),
    "identifier": ("STRING", "String"),
    "nbt_string": ("STRING", "String"),
    "rest": ("RAW_BYTES", "Vec<u8>"),
}

# Types the schema declares that this emitter cannot render as a Rust field yet:
# they are carried as opaque frames. Slot/Hashed Slot are declared in full and
# non-recursive, so they are *not* here — they flatten into their own fields.
OPAQUE = frozenset(
    {
        "nbt",
        "nbt_entry",
        "nbt_compound_body",
        "nbt_list_body",
        "nbt_payload",
        "nbt_nothing",
        "fixed_bitset",
        "light_data",
        "light_array",
        "entity_metadata",
        "meta_entry",
    }
)

# Serverbound packets whose official name collides with a clientbound one.
_SUFFIX = {"_serverbound"}


def _pascal(s: str) -> str:
    return "".join(p[:1].upper() + p[1:] for p in re.split(r"[^a-zA-Z0-9]+", s) if p)


def _snake(s: str) -> str:
    return re.sub(r"[^a-zA-Z0-9_]", "_", s)


def _opaque_len(
    ref: TypeRef | None,
    resolved: ResolvedSchema | None,
    params: dict[str, int] | None = None,
) -> str | None:
    """Byte length of a payload's frame, when the schema declares one.

    Two sources: the field's own `bytes(n)` argument, or — for a named encoding
    whose body is a single `bytes(n)` — that body, with the encoding's parameters
    bound (`fixed_bitset(20)` -> `bytes(3)`).
    """
    if ref is not None and ref.name == "bytes" and ref.args:
        val = _eval_len_expr(ref.args[0], params or {})
        if val is not None:
            return str(val)
        return None
    if resolved is None or ref is None:
        return None
    enc = resolved.encodings.get(ref.name)
    if enc is None or not isinstance(enc.body, ast.BlockType):
        return None
    bind = {p.name: (p.default if p.default is not None else 0) for p in enc.params}
    bind.update(params or {})
    for i, arg in enumerate(ref.args):
        if i < len(enc.params):
            v = arg.value if arg.kind == "int" else 0
            bind[enc.params[i].name] = v
    for item in enc.body.items:
        if item.kind != "field" or item.wire is None:
            continue
        inner = item.wire.type_ref
        if not isinstance(inner, TypeRef) or inner.name != "bytes":
            continue
        if not inner.args:
            continue
        arg = inner.args[0]
        if arg.kind == "int":
            return str(int(arg.value))
        if arg.kind == "ident" and arg.name in bind:
            return str(int(bind[arg.name]))
        # A small arithmetic expression over the bound params, e.g.
        # `bytes(1 + (bits - 1) / 8)` for Fixed BitSet.
        val = _eval_len_expr(arg, bind)
        if val is not None:
            return str(val)
    return None


def _eval_len_expr(expr, bind: dict) -> int | None:
    """Evaluate a small integer length expression against bound parameters."""
    if expr is None:
        return None
    if expr.kind == "int":
        return int(expr.value)
    if expr.kind == "ident":
        v = bind.get(expr.name)
        return int(v) if isinstance(v, int) else None
    if expr.kind == "binop":
        left = _eval_len_expr(expr.left, bind)
        right = _eval_len_expr(expr.right, bind)
        if left is None or right is None:
            return None
        op = expr.value
        if op == "+":
            return left + right
        if op == "-":
            return left - right
        if op == "*":
            return left * right
        if op == "/":
            return left // right if right else None
        if op == "%":
            return left % right if right else None
    return None


def _field_rust(ref: TypeRef) -> tuple[str, str]:
    """(wire constant, Rust type) for a wire field's declared type.

    An opaque, registry-keyed payload (NBT, slot, particle data) has no fixed
    Rust type, so it is carried as a raw byte frame. `RAW_BYTES` reads to end of
    buffer, which is only correct when the field is the last one; otherwise the
    payload must be length-prefixed by the schema (`bytes(n)`), which this
    emitter derives from the declared count field.
    """
    if ref.name in WIRE_TYPES:
        wire, rtype = WIRE_TYPES[ref.name]
        if ref.transforms and "utf8" in ref.transforms:
            return wire, "String"
        return wire, rtype
    if ref.name == "bytes":
        # `bytes(n)` is a fixed frame; RAW_BYTES alone would read to end of
        # buffer, so the emitter wraps it in `fixed_raw_bytes(n)`.
        return "FIXED_BYTES", "Vec<u8>"
    return "RAW_BYTES", "Vec<u8>"


def _expand_encoding(
    ref: TypeRef,
    resolved: ResolvedSchema,
    out: list["PacketShape"],
    seen: set[str],
    guard: ast.Expr | None = None,
    field_types: dict[str, str] | None = None,
    parent: str = "",
) -> bool:
    """Flatten a named encoding's own fields into the packet's field list.

    Returns True when the encoding was expanded, so the caller does not also
    emit an opaque frame for it. Prevents infinite recursion on self-referential
    encodings (NBT).
    """
    if resolved is None or ref.name in seen or ref.name in OPAQUE:
        return False
    enc = resolved.encodings.get(ref.name)
    if enc is None or not isinstance(enc.body, ast.BlockType):
        return False
    seen.add(ref.name)
    # Parameters bound at the reference site travel with the expanded fields, so
    # `fixed_bitset(20)` resolves its inner `bytes(1 + (bits - 1) / 8)` to 3.
    bind = {
        p.name: (p.default if p.default is not None else 0) for p in enc.params
    }
    for i, arg in enumerate(ref.args):
        if i < len(enc.params):
            v = arg.value if arg.kind == "int" else 0
            bind[enc.params[i].name] = v
    _flatten(enc.body.items, out, resolved, bind, guard, field_types or {}, parent)
    return True


# Payload types whose on-wire frame is self-delimiting: a reader scans the
# structure instead of consuming the buffer. Each maps to the scanner in
# `frames.rs` that measures one payload.
SELF_DELIMITING: dict[str, str] = {
    "text_component": "nbt_tag_len",
    "nbt": "nbt_compound_len",
    "slot": "slot_len",
    "hashed_slot": "hashed_slot_len",
    "id_set": "id_set_len",
    "id_or_payload": "id_or_payload_len",
    "slot_display": "id_or_payload_len",
    "recipe_display": "id_or_payload_len",
}

# The smallest self-delimiting payload of each kind, used as a field's default:
# an empty buffer has no payload to scan, and a zero-length frame is not a
# valid instance of any of these types.
SELF_DELIMITING_DEFAULT: dict[str, bytes] = {
    # TAG_END: an NBT body with no content.
    "text_component": b"\x00",
    "nbt": b"\x00",
    # item_count = 0.
    "slot": b"\x00",
    # has_item = false.
    "hashed_slot": b"\x00",
    # type = 0, then an empty name.
    "id_set": b"\x00\x00\x00",
    # a registry id (1) with no inline body.
    "id_or_payload": b"\x01",
    "slot_display": b"\x01",
    "recipe_display": b"\x01",
}


def _frame_walker(f: "PacketShape") -> str | None:
    """The scanner for a self-delimiting payload field, if it is one."""
    if f.ref is None or not f.opaque:
        return None
    return SELF_DELIMITING.get(f.ref.name)


def _elem_frame_mode(e: "PacketShape", resolved: ResolvedSchema) -> str:
    """How one repeat element's payload is framed: prefixed or self-delimiting."""
    walker = _frame_walker(e)
    if walker:
        return walker
    if _opaque_len(e.ref, resolved, e.params) is None and (
        e.opaque or e.wire == "RAW_BYTES"
    ):
        return "prefix"
    return "plain"


def _len_field(f: "PacketShape") -> str | None:
    """The field a `bytes(<ident>)` payload takes its length from."""
    ref = f.ref
    if ref is None or ref.name != "bytes" or not ref.args:
        return None
    arg = ref.args[0]
    return arg.name if arg.kind == "ident" else None


def _elem_needs_prefix(e: "PacketShape", resolved: ResolvedSchema) -> bool:
    """An opaque payload with no declared length is not self-delimiting.

    `RAW_BYTES` reads to end of buffer, so inside a repeat it must be framed.
    This is the same varint-length + fixed-frame convention the single-field
    path uses (`_emit_packet`'s `prefixed` rest mode).
    """
    return _elem_frame_mode(e, resolved) == "prefix"


def _prefix_write(ident: str, indent: str) -> list[str]:
    return [
        f"{indent}buffer.write(VAR_INT, {ident}.len() as i32);",
        f"{indent}if !{ident}.is_empty() {{",
        f"{indent}    buffer.write(fixed_raw_bytes({ident}.len() as i32), {ident}.clone());",
        f"{indent}}}",
    ]


def _prefix_read(ident: str, indent: str) -> list[str]:
    return [
        f"{indent}let {ident} = {{",
        f"{indent}    let len = buffer.read(VAR_INT) as usize;",
        f"{indent}    if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len as i32)) }}",
        f"{indent}}};",
    ]


def _scan_read(ident: str, walker: str, indent: str) -> list[str]:
    """Read a self-delimiting payload: scan its length, then that many bytes."""
    return [
        f"{indent}let {ident} = {{",
        f"{indent}    let mut raw = vec![0u8; buffer.readable_bytes() as usize];",
        f"{indent}    buffer.copy_to(buffer.read_index(), &mut raw);",
        f"{indent}    let len = crate::frames::{walker}(&raw) as i32;",
        f"{indent}    if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len)) }}",
        f"{indent}}};",
    ]


def _expr_idents(expr) -> list[str]:
    """Every identifier an expression reads, innermost first."""
    if expr is None:
        return []
    found: list[str] = []
    if expr.kind == "ident":
        found.append(expr.name)
    if expr.left is not None:
        found.extend(_expr_idents(expr.left))
    if expr.right is not None:
        found.extend(_expr_idents(expr.right))
    for a in expr.args or ():
        found.extend(_expr_idents(a))
    return found


def _scope_types(
    f: "PacketShape", top_fields: list["PacketShape"] | None
) -> dict[str, str]:
    """Element scope type map extended with the enclosing packet's fields.

    An element guard can read a packet field (`player_info_update`'s `actions`),
    whose Rust type decides how the operand widens.
    """
    types = dict(getattr(f, "elem_types", {}))
    for outer in top_fields or []:
        base = f"Vec<{outer.rtype}>" if outer.repeat_mode is not None else outer.rtype
        mapped = f"Option<{base}>" if outer.guard is not None else base
        types.setdefault(outer.ident, mapped)
    return types


def _elem_writes(
    f: "PacketShape",
    resolved: ResolvedSchema,
    var: str,
    top_fields: list["PacketShape"] | None = None,
) -> list[str]:
    """Write each element field from a bound `var`."""
    types = _scope_types(f, top_fields)
    out = [
        f"            let {name} = "
        + _strip_outer_parens(_rust_operand(expr, _scope_types(f, top_fields), "read"))
        + ";"
        for name, expr in getattr(f, "elem_computed", [])
    ]
    for e in f.element:
        if e.repeat_mode is not None:
            inner = _repeat_write(
                e,
                resolved,
                indent="                ",
                var=var,
                nested=True,
                top_fields=top_fields,
                iterable="items" if e.guard is not None else None,
            )
            if e.guard is not None:
                cond = _rust_cond(e.guard, types, "read")
                out.append(
                    "            if "
                    + cond
                    + " {\n                if let Some(items) = &"
                    + e.ident
                    + " {\n"
                    + inner
                    + "\n                }\n            }"
                )
            else:
                out.append(inner)
            continue
        mode = _elem_frame_mode(e, resolved)
        # A guarded slot is an Option, so its write unwraps into `v`.
        write_var = "v" if e.guard is not None else e.ident
        if mode == "prefix":
            body = _prefix_write(write_var, "                ")
        elif mode != "plain":
            body = [f"                buffer.write(RAW_BYTES, {write_var}.clone());"]
        else:
            frame = _frame_expr(e, resolved) or e.wire
            # The element's fields are bound as locals by `_destructure`.
            body = [f"                buffer.write({frame}, {write_var}.clone());"]
        if e.guard is not None:
            cond = _rust_cond(e.guard, types, "read")
            out.append(
                "            if " + cond + " {\n"
                f"                if let Some(v) = &{e.ident} {{\n"
                + "".join(f"    {line}\n" for line in body)
                + "                }\n"
                + "            }"
            )
        else:
            out.extend(body)
    return out


def _elem_reads(
    f: "PacketShape",
    resolved: ResolvedSchema,
    var: str = "value",
    top_fields: list["PacketShape"] | None = None,
) -> list[str]:
    """Read each element field as a local, ready to build the element."""
    out: list[str] = []
    # A computed local derives from fields, so it binds after the last field it
    # reads (guards on later fields read it in turn).
    computed = [
        (name, expr)
        for name, expr in getattr(f, "elem_computed", [])
    ]
    bound: set[str] = set()
    types = _scope_types(f, top_fields)

    def emit_computed() -> None:
        for name, expr in list(computed):
            if set(_expr_idents(expr)) <= bound:
                out.append(
                    "            let "
                    + name
                    + " = "
                    + _strip_outer_parens(_rust_operand(expr, types, "read"))
                    + ";"
                )
                computed.remove((name, expr))

    for e in f.element:
        if e.repeat_mode is not None:
            inner = _repeat_read(
                e,
                resolved,
                indent="                ",
                var=var,
                nested=True,
                top_fields=top_fields,
                bind=e.guard is None,
                outer_scope=f.element,
            )
            if e.guard is not None:
                cond = _rust_cond(e.guard, types, "read")
                out.append(
                    "            let "
                    + e.ident
                    + ": "
                    + _elem_part(e)
                    + " = if "
                    + cond
                    + " {\n                let mut "
                    + e.ident
                    + ": Vec<"
                    + e.rtype
                    + "> = Vec::new();\n"
                    + inner
                    + "\n                Some("
                    + e.ident
                    + ")\n            } else { None };"
                )
            else:
                out.append(inner)
            continue
        mode = _elem_frame_mode(e, resolved)
        if mode == "prefix":
            body = _prefix_read(e.ident, "                ")
        elif mode != "plain":
            body = _scan_read(e.ident, mode, "                ")
        else:
            frame = _frame_expr(e, resolved) or e.wire
            body = [f"                let {e.ident} = buffer.read({frame});"]
        if e.guard is not None:
            cond = _rust_cond(e.guard, types, "read")
            out.append(
                "            let "
                + e.ident
                + " = if "
                + cond
                + " {\n"
                + "\n".join(body)
                + "\n                Some("
                + e.ident
                + ")\n            } else { None };"
            )
        else:
            out.extend(body)
        bound.add(e.ident)
        emit_computed()
    for name, expr in computed:
        out.append(
            "            let "
            + name
            + " = "
            + _strip_outer_parens(_rust_operand(expr, types, "read"))
            + ";"
        )
    return out


def _elem_names(f: "PacketShape") -> list[str]:
    return [e.ident for e in f.element]


def _outer_guard_refs(f: "PacketShape") -> list[str]:
    """Packet-scope names an element's guards read, bound before the loop.

    A guard inside a repeat element can read a field of the enclosing packet
    (`player_info_update`'s `actions` bitmask). The rendered guard uses the bare
    name, so the loop needs a local for it.
    """
    local = {e.ident for e in f.element} | {
        name for name, _ in getattr(f, "elem_computed", [])
    }
    refs: set[str] = set()

    def walk(elements: list["PacketShape"]) -> None:
        for e in elements:
            if e.guard is not None:
                refs.update(_expr_idents(e.guard))
            local.add(e.ident)
            if e.repeat_mode is not None:
                walk(e.element)
            local.update(name for name, _ in getattr(e, "elem_computed", []))

    walk(f.element)
    return sorted(refs - local)


def _outer_bindings(f: "PacketShape", indent: str) -> list[str]:
    return [
        f"{indent}let {name} = value.{name}.clone();"
        for name in _outer_guard_refs(f)
    ]


def _repeat_write(
    f: "PacketShape",
    resolved: ResolvedSchema,
    indent: str = "        ",
    var: str = "value",
    nested: bool = False,
    top_fields: list["PacketShape"] | None = None,
    optional_vec: bool = False,
    iterable: str | None = None,
) -> str:
    if f.repeat_mode == "count":
        # The count is a plain field of this packet and is already written.
        body = "\n".join(_elem_writes(f, resolved, "item", top_fields))
        if nested:
            loop_head = (
                "".join(f"{line}\n" for line in _outer_bindings(f, "") if not nested)
                + f"{indent}for {var}_item in "
                + (iterable or f"&{f.ident}.clone()")
                + " {\n"
            )
        elif optional_vec:
            loop_head = (
                "".join(f"{line}\n" for line in _outer_bindings(f, indent) if not nested)
                + f"{indent}if let Some(items) = &{var}.{f.ident} {{\n"
                f"{indent}    for {var}_item in items {{\n"
            )
            body = body.replace(indent, indent + "    ", 1)
            return (
                f"{loop_head}"
                f"{_destructure(f, var)}\n"
                f"{body}\n"
                f"{indent}    }}\n"
                f"{indent}}}"
            )
        else:
            loop_head = (
                "".join(f"{line}\n" for line in _outer_bindings(f, indent) if not nested)
                + f"{indent}for {var}_item in &{var}.{f.ident} {{\n"
            )
        return (
            f"{loop_head}"
            f"{_destructure(f, var)}\n"
            f"{body}\n"
            f"{indent}}}"
        )
    # until_end / until: the terminator is a schema detail; emit to the end.
    body = "\n".join(_elem_writes(f, resolved, var, top_fields))
    if nested:
        loop_head = f"{indent}for {var}_item in &{f.ident}.clone() {{\n"
    elif optional_vec:
        loop_head = (
            f"{indent}if let Some(items) = &{var}.{f.ident} {{\n"
            f"{indent}    for {var}_item in items {{\n"
        )
        return (
            f"{loop_head}"
            f"{_destructure(f, var)}\n"
            f"{body}\n"
            f"{indent}    }}\n"
            f"{indent}}}"
        )
    else:
        loop_head = f"{indent}for {var}_item in &{var}.{f.ident} {{\n"
    return (
        f"{loop_head}"
        f"{_destructure(f, var)}\n"
        f"{body}\n"
        f"{indent}}}"
    )


def _repeat_read(
    f: "PacketShape",
    resolved: ResolvedSchema,
    indent: str = "        ",
    var: str = "value",
    nested: bool = False,
    top_fields: list["PacketShape"] | None = None,
    bind: bool = True,
    outer_scope: list["PacketShape"] | None = None,
) -> str:
    reads = "\n".join(_elem_reads(f, resolved, var, top_fields))
    if f.repeat_mode == "count":
        # The count is a plain field, so it is already read above; it may be
        # optional, in which case the guard already established it is present.
        count = f.count or _ident(f.ident) + "_count"
        # An optional count is unwrapped; the guard already established presence.
        scope = list(f.element if nested else top_fields or [])
        scope += list(outer_scope or [])
        count = _unwrap_count(count, scope)
        loop = (
            f"{indent}for _ in 0..{count} {{\n"
            f"{reads}\n"
            f"{indent}    {f.ident}.push({_elem_struct(f)});\n"
            f"{indent}}}"
        )
        if not bind:
            return loop
        # The packet's own fields are locals in the read scope, so an element
        # guard reading one needs no binding.
        return f"{indent}let mut {f.ident}: Vec<{f.rtype}> = Vec::new();\n" + loop
    return (
        f"{indent}let mut {f.ident} = Vec::new();\n"
        f"{indent}while buffer.readable_bytes() > 0 {{\n"
        f"{reads}\n"
        f"{indent}    {f.ident}.push({_elem_struct(f)});\n"
        f"{indent}}}"
    )


def _elem_struct(f: "PacketShape") -> str:
    """Build an element from the locals the read loop bound."""
    names = _elem_names(f)
    if len(names) == 1:
        return names[0]
    return "(" + ", ".join(names) + ")"


def _optional_names(fields: list["PacketShape"], var: str) -> set[str]:
    """Names of optional count fields bound as `Option` in this scope."""
    return {
        f.ident
        for f in fields
        if f.guard is not None and f.repeat_mode is None
    }


def _unwrap_count(count: str, scope: list["PacketShape"]) -> str:
    """An optional count is bound as `Option`, so unwrap it for the loop bound.

    `scope` is the list the count was flattened into: the enclosing element for a
    nested repeat, the packet's own fields otherwise.
    """
    optional = {f.ident for f in scope if f.guard is not None and f.repeat_mode is None}
    return f"{count}.unwrap_or(0)" if count in optional else count


def _destructure(f: "PacketShape", var: str = "value") -> str:
    """Bind the element's fields from the loop variable (a tuple or a value)."""
    names = _elem_names(f)
    binding = f"{var}_item"
    if len(names) == 1:
        return f"            let {names[0]} = {binding}.clone();"
    pat = "(" + ", ".join(names) + ")"
    return f"            let {pat} = {binding}.clone();"


def _frame_expr(f: "PacketShape", resolved: ResolvedSchema) -> str | None:
    """The `Type` expression for a payload field, or None for a plain wire type.

    A `bytes(n)` or a named encoding whose body is `bytes(n)` is a fixed frame;
    `rest` and an unbounded payload read to end of buffer.
    """
    if f.wire == "FIXED_BYTES" or f.opaque:
        n = _opaque_len(f.ref, resolved, f.params)
        return f"fixed_raw_bytes({n})" if n is not None else "RAW_BYTES"
    return None


def _java_name(ident: str) -> str:
    """`protocol_version` -> `protocolVersion`, minus rust keywords."""
    parts = _snake(ident).split("_")
    return parts[0] + "".join(p[:1].upper() + p[1:] for p in parts[1:])


_RUST_KEYWORDS = frozenset(
    {
        "as", "break", "const", "continue", "crate", "else", "enum", "extern",
        "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
        "super", "trait", "true", "type", "unsafe", "use", "where", "while",
        "async", "await", "dyn", "abstract", "become", "box", "do", "final",
        "macro", "override", "priv", "typeof", "unsized", "virtual", "yield",
    }
)


def _default_value(rtype: str) -> str:
    """A zero value for a Rust type, for packets rstom's Uuid cannot Default."""
    if rtype == "bool":
        return "false"
    if rtype in ("f32", "f64"):
        return "0.0"
    if rtype in ("u8", "i8", "u16", "i16", "u32", "i32", "u64", "i64"):
        return "0"
    if rtype == "String":
        return "String::new()"
    if rtype.startswith("Vec<"):
        return "Vec::new()"
    if rtype == "Uuid":
        return "Uuid::from_bits(0, 0)"
    if rtype == "CoordVec":
        return "CoordVec::new(0.0, 0.0, 0.0)"
    return "Default::default()"


def _ident(name: str) -> str:
    """A safe Rust identifier for a schema field name."""
    if name in _RUST_KEYWORDS:
        return f"{name}_"
    return name


class PacketShape:
    """A flattened field: its wire type, Rust type, and optionality."""

    def __init__(
        self,
        name: str,
        wire: str,
        rtype: str,
        opaque: bool,
        ref: TypeRef | None = None,
        params: dict[str, int] | None = None,
        repeat_mode: str | None = None,
        count: str | None = None,
        element: list["PacketShape"] | None = None,
        guard: ast.Expr | None = None,
        parent: str = "",
    ) -> None:
        self.ident = _ident(name)
        self.java = _java_name(name)
        self.wire = wire
        self.rtype = rtype
        self.guard = guard
        # An opaque payload is not self-delimiting as a plain field, so the
        # serializer wraps it in a fixed-length frame.
        self.opaque = opaque
        self.ref = ref
        self.params = params or {}
        # `count` mode writes a length prefix; the count value comes from the
        # field named by `count`.
        self.repeat_mode = repeat_mode
        self.count = count
        self.element = element or []
        # Set by `_repeat_shape`: the element scope's name -> Rust type map, and
        # its computed locals (a guard can read a computed discriminator).
        self.elem_types: dict[str, str] = {}
        self.elem_computed: list = []
        # Dotted path of enclosing field names this field was flattened out of.
        # The reference value nests those encodings under the field name, so a
        # flattened field reads its value along this path, not at top level.
        self.parent = parent


def _repeat_shape(
    wf,
    resolved: ResolvedSchema | None,
    bind: dict[str, int] | None,
    guard: ast.Expr | None = None,
    field_types: dict[str, str] | None = None,
    parent: str = "",
) -> "PacketShape":
    """A repeated field: a Vec whose elements are the flattened element type."""
    count = None
    if wf.repeat is not None and wf.repeat.mode == "count":
        count = _count_field(wf.repeat.expr) or _count_expr(wf.repeat.expr)
    element: list[PacketShape] = []
    elem_types: dict[str, str] = {}
    if isinstance(wf.type_ref, ast.BlockType):
        _flatten(
            wf.type_ref.items,
            element,
            resolved,
            bind,
            None,
            elem_types,
            parent,
            expand_payloads=False,
        )
    elif isinstance(wf.type_ref, TypeRef):
        # An opaque, registry-keyed element (a repeated Slot, say) has no plain
        # wire type; carry its bytes so the element is written at all. Dropping
        # it here silently erased the field from the wire (`Vec<()>`).
        wire, rtype = _field_rust(wf.type_ref)
        element.append(
            PacketShape(
                "_item", wire, rtype, wire == "RAW_BYTES", wf.type_ref, bind
            )
        )
    computed = [
        (item.computed_name, item.computed_expr)
        for item in (wf.type_ref.items if isinstance(wf.type_ref, ast.BlockType) else ())
        if item.kind == "computed"
    ]
    shape = PacketShape(
        wf.name,
        "REPEAT",
        _elem_rtype(element),
        False,
        wf.type_ref if isinstance(wf.type_ref, TypeRef) else None,
        bind,
        wf.repeat.mode if wf.repeat else None,
        count,
        element,
        guard=guard,
        parent=parent,
    )
    # The element scope's own name -> Rust type map, for guards that read a
    # sibling subfield of the same element.
    shape.elem_types = elem_types
    shape.elem_computed = computed
    return shape


def _elem_rtype(element: list["PacketShape"]) -> str:
    """The Rust element type: the one field's type, or a tuple for a record."""
    if len(element) == 1 and element[0].repeat_mode is None:
        return _elem_part(element[0])
    if not element:
        return "()"
    return "(" + ", ".join(_elem_part(e) for e in element) + ")"


def _elem_part(e: "PacketShape") -> str:
    """One tuple slot's type: a guarded subfield is optional, like a packet field."""
    base = f"Vec<{e.rtype}>" if e.repeat_mode is not None else e.rtype
    return f"Option<{base}>" if e.guard is not None else base


def _count_expr(expr) -> str | None:
    """A Rust expression for a `count =` that is not a plain field name.

    The GUI schema's grammar is a subset of Rust's for these shapes
    (`ident`, `ident >> int`, `a * b`), so it is emitted as-is.
    """
    if expr is None:
        return None
    if expr.kind == "ident":
        return _ident(expr.name)
    if expr.kind == "binop":
        left = _count_expr(expr.left)
        right = _count_expr(expr.right)
        if left is None or right is None:
            return None
        return f"{left} {expr.value} {right}"
    if expr.kind == "int":
        return str(int(expr.value))
    return None


def _count_field(ref_expr) -> str | None:
    """The field a `count = <ident>` reads, when it is a plain identifier."""
    if ref_expr is not None and ref_expr.kind == "ident":
        return ref_expr.name
    return None


def _and_guard(
    left: ast.Expr | None, right: ast.Expr | None
) -> ast.Expr | None:
    if left is None:
        return right
    if right is None:
        return left
    return ast.Expr("binop", "&&", left, right)


def _or_guard(left: ast.Expr | None, right: ast.Expr | None) -> ast.Expr | None:
    if left is None:
        return right
    if right is None:
        return left
    return ast.Expr("binop", "||", left, right)


def _match_tag_name(expr: ast.Expr) -> str:
    if expr.kind == "ident" and expr.name:
        return expr.name
    raise ValueError(f"unsupported match tag expression: {expr}")


def _field_rust_type(name: str, field_types: dict[str, str]) -> tuple[str, bool]:
    """(inner type, is_option) for a schema field name."""
    key = name
    rtype = field_types.get(key, field_types.get(_ident(key), "i32"))
    if rtype.startswith("Option<") and rtype.endswith(">"):
        return rtype[7:-1], True
    return rtype, False


def _rust_field_access(name: str, field_types: dict[str, str], mode: str) -> str:
    prefix = "value." if mode == "write" else ""
    ident = _ident(name)
    inner, optional = _field_rust_type(name, field_types)
    access = f"{prefix}{ident}"
    if not optional:
        # A mask literal over a narrow wire type must fit the type, so the
        # operand widens: `actions & 128` over a `u8` is `(actions as i32) & 128`.
        if inner in ("i8", "u8", "i16", "u16"):
            return f"({access} as i32)"
        return access
    if inner == "bool":
        return f"{access}.unwrap_or(false)"
    if inner in ("i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64"):
        return f"{access}.unwrap_or(0)"
    return f"{access}.clone().unwrap_or_default()"


def _rust_operand(expr: ast.Expr, field_types: dict[str, str], mode: str) -> str:
    """Rust scalar expression for a guard operand."""
    if expr.kind == "int":
        return str(int(expr.value))
    if expr.kind == "bool":
        return "true" if expr.value else "false"
    if expr.kind == "ident" and expr.name:
        return _rust_field_access(expr.name, field_types, mode)
    if expr.kind == "binop":
        left = _rust_operand(expr.left, field_types, mode)
        right = _rust_operand(expr.right, field_types, mode)
        return f"({left} {expr.value} {right})"
    if expr.kind == "unop" and expr.value == "!":
        return f"!({_rust_operand(expr.left, field_types, mode)})"
    raise ValueError(f"unsupported guard operand: {expr}")


def _rust_guard(
    expr: ast.Expr | None, field_types: dict[str, str], mode: str
) -> str:
    """Rust boolean expression for a schema guard (`write` uses `value.` prefix)."""
    if expr is None:
        return "true"
    prefix = "value." if mode == "write" else ""

    def truthy_ident(name: str) -> str:
        inner, _optional = _field_rust_type(name, field_types)
        access = _rust_field_access(name, field_types, mode)
        if inner == "bool":
            return access
        return f"({access} != 0)"

    k = expr.kind
    if k == "int":
        return str(int(expr.value))
    if k == "bool":
        return "true" if expr.value else "false"
    if k == "ident" and expr.name:
        return truthy_ident(expr.name)
    if k == "unop" and expr.value == "!":
        return f"!({_rust_guard(expr.left, field_types, mode)})"
    if k == "binop":
        op = expr.value
        if op in ("&&", "||"):
            left = _rust_guard(expr.left, field_types, mode)
            right = _rust_guard(expr.right, field_types, mode)
            return f"({left} {op} {right})"
        if op in ("==", "!=", "<", "<=", ">", ">="):
            if (
                op == "!="
                and expr.right is not None
                and expr.right.kind == "int"
                and int(expr.right.value) == 0
                and expr.left is not None
                and expr.left.kind == "ident"
                and expr.left.name
            ):
                inner, _ = _field_rust_type(expr.left.name, field_types)
                if inner == "bool":
                    return _rust_field_access(expr.left.name, field_types, mode)
            if (
                op == "=="
                and expr.right is not None
                and expr.right.kind == "int"
                and int(expr.right.value) == 0
                and expr.left is not None
                and expr.left.kind == "ident"
                and expr.left.name
            ):
                inner, _ = _field_rust_type(expr.left.name, field_types)
                if inner == "bool":
                    return f"!{_rust_field_access(expr.left.name, field_types, mode)}"
            left = _rust_operand(expr.left, field_types, mode)
            right = _rust_operand(expr.right, field_types, mode)
            return f"({left} {op} {right})"
        left = _rust_operand(expr.left, field_types, mode)
        right = _rust_operand(expr.right, field_types, mode)
        return f"({left} {op} {right})"
    raise ValueError(f"unsupported guard expression: {expr}")


def _strip_outer_parens(text: str) -> str:
    """Drop one wrap-around `(...)` pair when it encloses the whole expression."""
    if not text.startswith("("):
        return text
    depth = 0
    for i, ch in enumerate(text):
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return text[1:-1] if i == len(text) - 1 else text
    return text


def _rust_cond(
    expr: ast.Expr | None, field_types: dict[str, str], mode: str
) -> str:
    """`_rust_guard` as a top-level `if` condition, without the wrapper parens."""
    return _strip_outer_parens(_rust_guard(expr, field_types, mode))


def _guard_holds(
    expr: ast.Expr | None, env: dict[str, object], field_types: dict[str, str]
) -> bool:
    """Whether a guard is true for synthesized default values."""
    if expr is None:
        return True
    k = expr.kind
    if k == "int":
        return bool(expr.value)
    if k == "bool":
        return bool(expr.value)
    if k == "ident" and expr.name:
        key = _ident(expr.name)
        v = env.get(key, env.get(expr.name, 0))
        rtype = field_types.get(expr.name, field_types.get(key, "i32"))
        if rtype == "bool":
            return bool(v)
        return int(v) != 0
    if k == "unop" and expr.value == "!":
        return not _guard_holds(expr.left, env, field_types)
    if k == "binop":
        op = expr.value
        if op == "&&":
            return _guard_holds(expr.left, env, field_types) and _guard_holds(
                expr.right, env, field_types
            )
        if op == "||":
            return _guard_holds(expr.left, env, field_types) or _guard_holds(
                expr.right, env, field_types
            )
        left = _guard_holds_value(expr.left, env, field_types)
        right = _guard_holds_value(expr.right, env, field_types)
        if op == "==":
            return left == right
        if op == "!=":
            return left != right
        if op == "<":
            return int(left) < int(right)
        if op == "<=":
            return int(left) <= int(right)
        if op == ">":
            return int(left) > int(right)
        if op == ">=":
            return int(left) >= int(right)
    return False


def _guard_holds_value(
    expr: ast.Expr, env: dict[str, object], field_types: dict[str, str]
) -> object:
    if expr.kind == "int":
        return int(expr.value)
    if expr.kind == "ident" and expr.name:
        return env.get(expr.name, 0)
    if expr.kind == "binop":
        left = _guard_holds_value(expr.left, env, field_types)
        right = _guard_holds_value(expr.right, env, field_types)
        op = expr.value
        if op == "+":
            return int(left) + int(right)
        if op == "-":
            return int(left) - int(right)
        if op == "*":
            return int(left) * int(right)
        if op == "/":
            return int(left) // int(right)
    return 0



def _merge_declared(
    out: list["PacketShape"],
    existing: "PacketShape",
    guard: ast.Expr | None,
    parent: str,
) -> None:
    """Reconcile a name declared twice in the same flattened scope.

    A match arm and the packet's own body can both declare a field (a `trail`
    particle's `x`/`y`/`z` and `level_particles`' coordinates). One struct
    field serves both wire positions: it moves to the later declaration's
    position and loses the guard, because the packet's own field is always
    present. The arm's own position is then unrepresentable and simply not
    written, which is what the reference does for every other particle.
    """
    if guard is None and not parent and existing.guard is not None:
        out.remove(existing)
        existing.guard = None
        existing.parent = ""
        out.append(existing)
        return
    existing.guard = _or_guard(existing.guard, guard)


def _append_field(
    out: list[PacketShape],
    wf: ast.WireField,
    resolved: ResolvedSchema | None,
    bind: dict[str, int] | None,
    guard: ast.Expr | None,
    field_types: dict[str, str],
    parent: str = "",
    expand_payloads: bool = True,
) -> None:
    if wf.repeat is not None:
        out.append(_repeat_shape(wf, resolved, bind, guard, field_types, parent))
        return
    type_ref = wf.type_ref
    if isinstance(type_ref, ast.MatchType):
        _flatten_match(type_ref, out, resolved, bind, guard, field_types, parent)
        return
    if isinstance(type_ref, ast.BlockType):
        _flatten(type_ref.items, out, resolved, bind, guard, field_types, parent)
        return
    if not isinstance(type_ref, TypeRef):
        return
    if type_ref.name == "text_component":
        # An NBT text component, not a single byte: carry its encoded payload
        # like every other registry-keyed payload (`nbt`, `slot`).
        wire, rtype = "RAW_BYTES", "Vec<u8>"
        existing = next((f for f in out if f.ident == _ident(wf.name)), None)
        if existing is not None:
            _merge_declared(out, existing, guard, parent)
            return
        out.append(
            PacketShape(
                wf.name,
                wire,
                rtype,
                True,
                type_ref,
                bind,
                guard=guard,
                parent=parent,
            )
        )
        field_types[wf.name] = f"Option<{rtype}>" if guard is not None else rtype
        return
    framed = (
        type_ref.name in OPAQUE
        or SELF_DELIMITING.get(type_ref.name) is not None
        or (resolved is not None and _opaque_len(type_ref, resolved, bind) is not None)
    )
    if (
        expand_payloads
        and not framed
        and type_ref.name not in WIRE_TYPES
        and _expand_encoding(
            type_ref,
            resolved,
            out,
            set(),
            guard,
            field_types,
            f"{parent}.{wf.name}" if parent else wf.name,
        )
    ):
        return
    existing = next((f for f in out if f.ident == _ident(wf.name)), None)
    wire, rtype = _field_rust(type_ref)
    if existing is not None:
        _merge_declared(out, existing, guard, parent)
        return
    out.append(
        PacketShape(
            wf.name,
            wire,
            rtype,
            type_ref.name not in WIRE_TYPES,
            type_ref,
            bind,
            guard=guard,
            parent=parent,
        )
    )
    field_types[wf.name] = f"Option<{rtype}>" if guard is not None else rtype


def _flatten_match(
    mt: ast.MatchType,
    out: list[PacketShape],
    resolved: ResolvedSchema | None,
    bind: dict[str, int] | None,
    parent_guard: ast.Expr | None,
    field_types: dict[str, str],
    parent: str = "",
) -> None:
    tag = _match_tag_name(mt.tag_expr)
    explicit: list[int | str] = []
    for arm in mt.arms:
        if arm.tag is None or arm.tag == "_":
            continue
        explicit.append(arm.tag)
        arm_guard = _and_guard(
            parent_guard,
            ast.Expr(
                "binop",
                "==",
                ast.Expr("ident", name=tag),
                ast.Expr("int", arm.tag) if isinstance(arm.tag, int) else None,
            ),
        )
        if arm.type_ref is None:
            continue
        if isinstance(arm.type_ref, ast.BlockType):
            _flatten(arm.type_ref.items, out, resolved, bind, arm_guard, field_types, parent)
        elif isinstance(arm.type_ref, TypeRef):
            if arm.type_ref.name in WIRE_TYPES or arm.type_ref.name in OPAQUE:
                continue
            _expand_encoding(
                arm.type_ref, resolved, out, set(), arm_guard, field_types, parent
            )
    default = next((a for a in mt.arms if a.tag is None or a.tag == "_"), None)
    if default is None or default.type_ref is None:
        return
    if isinstance(default.type_ref, TypeRef) and default.type_ref.name == "nbt_nothing":
        return
    neg_parts = [
        ast.Expr(
            "binop",
            "!=",
            ast.Expr("ident", name=tag),
            ast.Expr("int", t) if isinstance(t, int) else None,
        )
        for t in explicit
        if isinstance(t, int)
    ]
    default_guard = parent_guard
    for part in neg_parts:
        default_guard = _and_guard(default_guard, part)
    if isinstance(default.type_ref, ast.BlockType):
        _flatten(default.type_ref.items, out, resolved, bind, default_guard, field_types)
    elif isinstance(default.type_ref, TypeRef):
        _expand_encoding(
            default.type_ref, resolved, out, set(), default_guard, field_types
        )


def _flatten(
    items: tuple[BlockItem, ...],
    out: list[PacketShape],
    resolved: ResolvedSchema | None = None,
    bind: dict[str, int] | None = None,
    parent_guard: ast.Expr | None = None,
    field_types: dict[str, str] | None = None,
    parent: str = "",
    expand_payloads: bool = True,
) -> None:
    """Flatten a packet body into wire-order fields with conditional guards.

    `expand_payloads` is False inside a repeat element, where a payload
    encoding stays one framed field: flattening two Slots in one element would
    collide their field names and merge their guards.
    """
    field_types = field_types if field_types is not None else {}
    for item in items:
        if item.kind == "group":
            guard = _and_guard(parent_guard, item.group_if)
            _flatten(
                item.group_items,
                out,
                resolved,
                bind,
                guard,
                field_types,
                parent,
                expand_payloads,
            )
            continue
        if item.kind == "spread" and resolved is not None:
            enc = resolved.encodings.get(item.spread_name)
            if enc is not None and isinstance(enc.body, ast.BlockType):
                _flatten(
                    enc.body.items,
                    out,
                    resolved,
                    bind,
                    parent_guard,
                    field_types,
                    parent,
                    expand_payloads,
                )
            continue
        if item.kind == "field" and item.wire is not None:
            wf = item.wire
            guard = _and_guard(parent_guard, wf.if_expr)
            _append_field(
                out, wf, resolved, bind, guard, field_types, parent, expand_payloads
            )
        elif item.kind == "computed":
            continue


def _struct_name(pkt: PacketDecl) -> str:
    base = _pascal(pkt.name)
    return base if base.endswith("Packet") else base + "Packet"


def _packet_group(direction: str) -> str:
    """`client` = serverbound (received from client), `server` = clientbound."""
    return "client" if direction == "serverbound" else "server"


def _connection_side_rust(direction: str) -> str:
    return "ConnectionSide::Client" if direction == "serverbound" else "ConnectionSide::Server"


def _connection_state_rust(state: str) -> str:
    return {
        "handshaking": "ConnectionState::Handshake",
        "handshake": "ConnectionState::Handshake",
        "status": "ConnectionState::Status",
        "login": "ConnectionState::Login",
        "configuration": "ConnectionState::Configuration",
        "play": "ConnectionState::Play",
    }.get(state, f"ConnectionState::{_pascal(state)}")


def _java_class_name(pkt: PacketDecl, group: str, state: str) -> str:
    return f"net.minestom.server.network.packet.{group}.{state}.{_struct_name(pkt)}"


def _primary_route(
    pkt_name: str, resolved: ResolvedSchema
) -> tuple[str, str, str]:
    routes = [(r, p) for r, p in resolved.routes if p.name == pkt_name]
    if not routes:
        return "client", "play", "serverbound"
    routes.sort(key=lambda e: (e[0].state, e[0].direction, e[0].packet_id))
    r = routes[0][0]
    return _packet_group(r.direction), r.state, r.direction


def _contiguous_registry_entries(
    entries: list[tuple[RouteDecl, PacketDecl]],
) -> list[tuple[RouteDecl, PacketDecl]]:
    """Keep routes whose ids are 0..n-1; `PacketRegistry` is positional.

    Wiki routes such as `legacy_server_list_ping` at `0xfe` are omitted here
    (same as `packets_gen` handshaking table).
    """
    entries.sort(key=lambda e: e[0].packet_id)
    out: list[tuple[RouteDecl, PacketDecl]] = []
    for i, (route, pkt) in enumerate(entries):
        if route.packet_id != i:
            break
        out.append((route, pkt))
    return out


def _routes_by_registry(
    resolved: ResolvedSchema,
) -> dict[str, list[tuple[RouteDecl, PacketDecl]]]:
    out: dict[str, list[tuple[RouteDecl, PacketDecl]]] = {}
    for route, pkt in resolved.routes:
        key = f"{_packet_group(route.direction)}_{route.state}"
        out.setdefault(key, []).append((route, pkt))
    return {key: _contiguous_registry_entries(entries) for key, entries in out.items()}


def _packet_fields(
    pkt: PacketDecl, resolved: ResolvedSchema
) -> tuple[list["PacketShape"], dict[str, str]]:
    """The packet's flattened fields, plus the Rust type each name maps to."""
    fields: list[PacketShape] = []
    flatten_types: dict[str, str] = {}
    _flatten(pkt.items, fields, resolved, None, None, flatten_types)
    field_types: dict[str, str] = dict(flatten_types)
    for f in fields:
        base = f"Vec<{f.rtype}>" if f.repeat_mode is not None else f.rtype
        mapped = f"Option<{base}>" if f.guard is not None else base
        field_types[f.ident] = mapped
        field_types[_snake(f.ident)] = mapped
    return fields, field_types


def _emit_packet(pkt: PacketDecl, resolved: ResolvedSchema) -> str:
    fields, field_types = _packet_fields(pkt, resolved)
    name = _struct_name(pkt)
    ser = name[: -len("Packet")] + "Serializer"

    struct_lines = []
    for f in fields:
        base = f"Vec<{f.rtype}>" if f.repeat_mode is not None else f.rtype
        rtype = f"Option<{base}>" if f.guard is not None else base
        struct_lines.append(f"    /// Java: `{f.java}`.")
        struct_lines.append(f"    pub {f.ident}: {rtype},")
    if struct_lines and struct_lines[-1].endswith(","):
        struct_lines[-1] = struct_lines[-1][:-1]

    def _payload_mode(f: PacketShape, index: int) -> str:
        """How an unframed payload field is carried on the wire.

        `scan` is a self-delimiting payload (NBT, Slot, an id set): it is
        written raw and read by scanning its length, exactly like the
        reference. Anything else unframed reads to end of buffer, which is
        only correct for the last field; a payload before it is
        length-prefixed, which the reference's concatenated payloads are not,
        so it keeps the consistency check but no byte-for-byte pin.
        """
        if _frame_walker(f):
            return "scan"
        if _len_field(f):
            return "len_field"
        unframed = (f.ref is not None and f.ref.name == "rest") or (
            f.opaque and _opaque_len(f.ref, resolved, f.params) is None
        )
        if not unframed:
            return "plain"
        return "to_end" if index == len(fields) - 1 else "prefixed"

    writes = []
    for i, f in enumerate(fields):
        if f.repeat_mode is not None:
            opt_vec = f.guard is not None
            if f.guard is not None:
                cond = _rust_cond(f.guard, field_types, "write")
                inner = _repeat_write(
                    f, resolved, top_fields=fields, optional_vec=opt_vec
                )
                writes.append(f"        if {cond} {{\n{inner}\n        }}")
            else:
                writes.append(
                    _repeat_write(f, resolved, top_fields=fields, optional_vec=False)
                )
            continue
        if _payload_mode(f, i) == "prefixed":
            body = (
                f"            let slice = value.{f.ident}.as_slice();\n"
                f"            buffer.write(VAR_INT, slice.len() as i32);\n"
                f"            if !slice.is_empty() {{\n"
                f"                buffer.write(fixed_raw_bytes(slice.len() as i32), slice);\n"
                f"            }}"
            )
            if f.guard is not None:
                cond = _rust_cond(f.guard, field_types, "write")
                writes.append(
                    f"        if {cond} {{\n"
                    f"            if let Some(v) = &value.{f.ident} {{\n"
                    f"                let slice = v.as_slice();\n"
                    f"                buffer.write(VAR_INT, slice.len() as i32);\n"
                    f"                if !slice.is_empty() {{\n"
                    f"                    buffer.write(fixed_raw_bytes(slice.len() as i32), slice);\n"
                    f"                }}\n"
                    f"            }}\n"
                    f"        }}"
                )
            else:
                writes.append(body)
            continue
        frame = _frame_expr(f, resolved) or f.wire
        if f.guard is not None:
            cond = _rust_cond(f.guard, field_types, "write")
            writes.append(
                f"        if {cond} {{\n"
                f"            if let Some(v) = &value.{f.ident} {{\n"
                f"                buffer.write({frame}, v.clone());\n"
                f"            }}\n"
                f"        }}"
            )
        else:
            writes.append(f"        buffer.write({frame}, value.{f.ident}.clone());")

    reads = []
    read_types = dict(field_types)
    for i, f in enumerate(fields):
        if f.repeat_mode is not None:
            if f.guard is not None:
                cond = _rust_cond(f.guard, read_types, "read")
                inner = _repeat_read(f, resolved, top_fields=fields).strip()
                reads.append(
                    f"        let {f.ident} = if {cond} {{\n"
                    f"{inner}\n"
                    f"            Some({f.ident})\n"
                    f"        }} else {{ None }};"
                )
            else:
                reads.append(_repeat_read(f, resolved, top_fields=fields))
            read_types[f.ident] = f"Vec<{f.rtype}>"
            continue
        if _payload_mode(f, i) == "prefixed":
            if f.guard is not None:
                cond = _rust_cond(f.guard, read_types, "read")
                reads.append(
                    f"        let {f.ident} = if {cond} {{\n"
                    f"            let len = buffer.read(VAR_INT) as usize;\n"
                    f"            Some(if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len as i32)) }})\n"
                    f"        }} else {{ None }};"
                )
            else:
                reads.append(
                    f"        let len = buffer.read(VAR_INT) as usize;\n"
                    f"        let {f.ident} = if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len as i32)) }};"
                )
            read_types[f.ident] = f"Option<{f.rtype}>" if f.guard else f.rtype
            continue
        length_field = _len_field(f)
        if length_field:
            body = (
                f"            let {f.ident} = {{\n"
                f"                let n = {_ident(length_field)} as usize;\n"
                f"                if n == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(n as i32)) }}\n"
                f"            }};"
            )
            if f.guard is not None:
                cond = _rust_cond(f.guard, read_types, "read")
                reads.append(
                    f"        let {f.ident} = if {cond} {{\n{body}\n"
                    f"            Some({f.ident})\n        }} else {{ None }};"
                )
                read_types[f.ident] = f"Option<{f.rtype}>"
            else:
                reads.append(body)
                read_types[f.ident] = f.rtype
            continue
        walker = _frame_walker(f)
        if walker and _payload_mode(f, i) == "scan":
            if f.guard is not None:
                cond = _rust_cond(f.guard, read_types, "read")
                inner = "\n".join(_scan_read(f.ident, walker, "            "))
                reads.append(
                    f"        let {f.ident} = if {cond} {{\n{inner}\n"
                    f"            Some({f.ident})\n        }} else {{ None }};"
                )
                read_types[f.ident] = f"Option<{f.rtype}>"
            else:
                reads.extend(_scan_read(f.ident, walker, "        "))
                read_types[f.ident] = f.rtype
            continue
        frame = _frame_expr(f, resolved) or f.wire
        if f.guard is not None:
            cond = _rust_cond(f.guard, read_types, "read")
            reads.append(
                f"        let {f.ident} = if {cond} {{ Some(buffer.read({frame})) }} else {{ None }};"
            )
            read_types[f.ident] = f"Option<{f.rtype}>"
            continue
        reads.append(f"        let {f.ident} = buffer.read({frame});")
        read_types[f.ident] = f.rtype

    def ctor_type(f: "PacketShape") -> str:
        base = f"Vec<{f.rtype}>" if f.repeat_mode is not None else f.rtype
        return f"Option<{base}>" if f.guard is not None else base

    ctor = (
        ",\n        ".join(f"{f.ident}: {ctor_type(f)}" for f in fields)
        if fields
        else ""
    )
    init = ",\n            ".join(f"{f.ident}" for f in fields) if fields else ""
    field_decls = "\n".join(struct_lines) or "    // no wire fields"

    writes = "\n".join(writes) or "        let _ = (buffer, value);"
    reads = "\n".join(reads) or "        let _ = buffer;"
    def collect(items: list["PacketShape"]) -> tuple[set[str], set[str]]:
        """(wire constants, domain types) needed by these fields and their elements."""
        wires: set[str] = set()
        rtypes: set[str] = set()
        for f in items:
            if f.repeat_mode is not None:
                sub_w, sub_r = collect(f.element)
                wires |= sub_w
                rtypes |= sub_r
                continue
            wires.add(f.wire)
            rtypes.add(f.rtype)
        return wires, rtypes

    collected, rtypes = collect(fields)
    # Import exactly the type constants the generated body references; a
    # heuristic over field shapes over-imports and trips `unused_imports`.
    body_text = writes + "\n" + reads
    used_wires = sorted(
        w
        for w in (collected | {"VAR_INT", "RAW_BYTES", "FIXED_BYTES"})
        if re.search(rf"\b{w}\b", body_text)
    )
    if any(_payload_mode(f, i) == "prefixed" for i, f in enumerate(fields)):
        used_wires = sorted(set(used_wires) | {"VAR_INT"})
    imports = ""
    if used_wires:
        imports += "use rstom::network::types::{" + ", ".join(used_wires) + "};\n"
    if "fixed_raw_bytes(" in body_text and "fixed_raw_bytes" not in imports:
        imports += "use rstom::network::types::fixed_raw_bytes;\n"
    # A hand-written Default is needed whenever a field's zero value is not the
    # derive's: rstom's Uuid has no Default, and an opaque byte frame must be
    # exactly its declared length or the write path panics.
    def needs_default(items: list["PacketShape"]) -> bool:
        return any(
            f.guard is not None
            or f.repeat_mode is not None
            or f.opaque
            or f.wire == "FIXED_BYTES"
            or f.rtype in ("Uuid", "CoordVec")
            or needs_default(f.element)
            for f in items
        )

    needs_default = needs_default(fields)
    default_impl = ""
    big_tuple = any(
        f.repeat_mode is not None
        and f.rtype.startswith("(")
        and f.rtype.count(",") >= 11
        for f in fields
    )
    if big_tuple:
        derive = (
            "#[derive(Clone, Default)]" if not needs_default else "#[derive(Clone)]"
        )
    else:
        derive = (
            "#[derive(Clone, Debug, Default, PartialEq)]"
            if not needs_default
            else "#[derive(Clone, Debug, PartialEq)]"
        )
    if "Uuid" in rtypes:
        imports += "use rstom::network::uuid::Uuid;\n"
    default_env: dict[str, object] = {}
    zero = []
    for f in fields:
        frame = _frame_expr(f, resolved)
        if f.repeat_mode is not None:
            v = "Vec::new()"
            py_v: object = []
        elif f.opaque or frame:
            declared = _opaque_len(f.ref, resolved, f.params)
            self_delim = (
                SELF_DELIMITING_DEFAULT.get(f.ref.name)
                if f.ref is not None and _frame_walker(f)
                else None
            )
            if declared:
                v = f"vec![0u8; {declared}]"
                py_v = b"\x00" * int(declared)
            elif self_delim is not None:
                lit = _rust_bytes(self_delim)
                v = lit
                py_v = self_delim
            else:
                v = "Vec::new()"
                py_v = b""
        else:
            v = _default_value(f.rtype)
            py_v = 0 if f.rtype in ("i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64") else (
                False if f.rtype == "bool" else 0.0 if f.rtype in ("f32", "f64") else ""
            )
            if f.ident == "id" and f.rtype == "i32":
                v, py_v = "1", 1
            elif f.ident == "particle_type" and f.rtype == "i32":
                v, py_v = "300", 300
        active = f.guard is None or _guard_holds(f.guard, default_env, field_types)
        if f.guard is not None and not active:
            zero.append("None")
        elif f.guard is not None:
            zero.append(f"Some({v})")
        else:
            zero.append(v)
        if active:
            default_env[f.ident] = py_v
            default_env[_snake(f.java)] = py_v
    body = (
        "        Self\n"
        if not fields
        else f"        Self::new({', '.join(zero)})\n"
    )
    if needs_default:
        default_impl = (
            f"impl Default for {name} {{\n"
            "    fn default() -> Self {\n"
            f"{body}"
            "    }\n"
            "}\n\n"
        )
    if "CoordVec" in rtypes:
        imports += "use rstom::coordinate::Vec as CoordVec;\n"

    group, state, _direction = _primary_route(pkt.name, resolved)
    java_class = _java_class_name(pkt, group, state)
    if group == "client":
        marker_import = (
            "use rstom::network::packet::client::client_packet::"
            "{ClientPacket, ClientPacketGroup};\n"
        )
        marker_impl = (
            f"\nimpl ClientPacket for {name} {{}}\n"
            f"impl ClientPacketGroup for {name} {{}}\n"
        )
    else:
        marker_import = (
            "use rstom::network::packet::server::server_packet::"
            "{ServerPacket, ServerPacketGroup};\n"
        )
        marker_impl = (
            f"\nimpl ServerPacket for {name} {{}}\n"
            f"impl ServerPacketGroup for {name} {{}}\n"
        )

    return f'''//! Generated by `bps emit rust` from a resolved BPS schema.
//!
//! Wire layout transcribed from `schemas/minecraft_26_3*.bps`.

use std::sync::Arc;

{imports}{marker_import}use rstom::network::buffer::NetworkBuffer;
use rstom::network::packet::packet::Packet;
use rstom::network::packet::serializer::{{PacketSerializer, PacketValue, downcast}};

/// Java: the record's own canonical constructor signature.
{derive}
pub struct {name} {{
{field_decls}
}}

{default_impl}impl {name} {{
    /// The record's canonical constructor.
    #[must_use]
    pub fn new(
        {ctor}
    ) -> Self {{
        Self {{
            {init}
        }}
    }}

    /// The schema's serializer.
    #[must_use]
    pub fn serializer() -> Arc<dyn PacketSerializer> {{
        Arc::new({ser})
    }}
}}

impl Packet for {name} {{
    const CLASS_NAME: &'static str = "{java_class}";
}}
{marker_impl}
/// Java: the hand-written `SERIALIZER` of `{name}`.
pub struct {ser};

impl PacketSerializer for {ser} {{
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {{
        let value = downcast::<{name}>(value);
{writes or "        let _ = value;"}
    }}

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {{
{reads}
        Box::new({name}::new(
            {init}
        ))
    }}
}}
'''


def _encode_bare(
    ref, value: object, resolved: ResolvedSchema, env: dict[str, int] | None = None
) -> bytes | None:
    """Python-encoded bytes for one field value, or None if it cannot encode."""
    from bps.interp.cursor import WriteCursor
    from bps.interp.engine import _encode_type

    w = WriteCursor()
    try:
        _encode_type(ref, w, resolved, dict(env or {}), value)
    except Exception:
        return None
    return w.result()


def _rust_str(value: str) -> str:
    body = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{body}".to_string()'


def _rust_bytes(data: bytes) -> str:
    return 'b"' + "".join(f"\\x{b:02x}" for b in data) + '".to_vec()'


def _rust_int(value: object, bits: int, signed: bool) -> str | None:
    v = int(value)
    if signed:
        lo, hi = -(1 << (bits - 1)), (1 << (bits - 1)) - 1
        if not (lo <= v <= hi):
            v = ((v - lo) % (1 << bits)) + lo
    else:
        if not (0 <= v < (1 << bits)):
            return None
    return str(v)


def _position_components(raw: int) -> tuple[int, int, int]:
    def ext(v: int, bits: int) -> int:
        return (v + (1 << (bits - 1))) % (1 << bits) - (1 << (bits - 1))

    return (ext(raw >> 38, 26), ext(raw % 4096, 12), ext((raw >> 12) % 67108864, 26))


_PRIM_RTYPES = {
    "i8": (8, True),
    "u8": (8, True),
    "i16": (16, True),
    "u16": (16, False),
    "i32": (32, True),
    "u32": (32, False),
    "i64": (64, True),
    "u64": (64, False),
}


def _rust_element(
    e: "PacketShape",
    value: object,
    resolved: ResolvedSchema,
    env: dict | None = None,
) -> str | None:
    """A repeat element's literal: the element type is never an Option."""
    if e.guard is not None and value is None:
        return None
    return _rust_inner(e, value, resolved, env)


def _rust_literal(
    f: "PacketShape",
    value: object,
    resolved: ResolvedSchema,
    env: dict | None = None,
) -> str | None:
    """Rust source for one field value, or None when it cannot be rendered."""
    if f.guard is not None and value is None:
        return "None"
    lit = _rust_inner(f, value, resolved, env)
    if lit is None:
        return None
    return f"Some({lit})" if f.guard is not None else lit


def _rust_inner(
    f: "PacketShape",
    value: object,
    resolved: ResolvedSchema,
    env: dict | None = None,
) -> str | None:
    """The `T` in `Option<T>` / the bare value: no optionality wrapper."""
    if f.repeat_mode is not None:
        if not isinstance(value, list):
            return None
        if not value:
            return "Vec::new()"
        parts = []
        for item in value:
            if len(f.element) == 1 and f.element[0].ident == "_item":
                lit = _rust_element(f.element[0], item, resolved, env)
            elif isinstance(item, dict):
                lits = []
                for e in f.element:
                    ev = _py_field_value(e, item)
                    el = _rust_literal(e, ev, resolved)
                    if el is None:
                        return None
                    lits.append(el)
                lit = "(" + ", ".join(lits) + ")"
            else:
                lit = None
            if lit is None:
                return None
            parts.append(lit)
        return "vec![" + ", ".join(parts) + "]"

    frame = _frame_expr(f, resolved)
    if f.opaque or frame or f.rtype == "Vec<u8>":
        if f.ref is not None:
            # Through the interpreter: a `bytes(<ident>)` frame is zero-padded to
            # the length the sibling field declares, which a raw value cannot be.
            data = _encode_bare(f.ref, value, resolved, {**(env or {}), **f.params})
            if data is None:
                if isinstance(value, (bytes, bytearray)):
                    data = bytes(value)
                elif isinstance(value, str):
                    data = value.encode("utf-8")
                else:
                    return None
        elif isinstance(value, (bytes, bytearray)):
            data = bytes(value)
        elif isinstance(value, str):
            data = value.encode("utf-8")
        else:
            return None
        # A declared frame is exactly its length: the interpreter zero-pads a
        # short value and rejects a long one, and `fixed_raw_bytes` panics on
        # any other length, so a literal must match that contract.
        n = _opaque_len(f.ref, resolved, f.params)
        if n is not None:
            n = int(n)
            if len(data) < n:
                data = data + bytes(n - len(data))
            elif len(data) > n:
                return None
        return _rust_bytes(data)

    if isinstance(value, bool):
        return "true" if value else "false"
    if f.rtype == "String":
        return _rust_str(value) if isinstance(value, str) else None
    if f.rtype in ("f32", "f64"):
        return repr(value) if isinstance(value, float) else None
    if f.rtype == "Uuid":
        if not isinstance(value, dict) or "msb" not in value or "lsb" not in value:
            return None
        mask = (1 << 64) - 1
        return f"Uuid::from_bits({int(value['msb']) & mask}, {int(value['lsb']) & mask})"
    if f.rtype == "CoordVec":
        if not isinstance(value, dict) or "raw" not in value:
            return None
        x, y, z = _position_components(int(value["raw"]))
        return f"CoordVec::new({float(x)}, {float(y)}, {float(z)})"
    if f.rtype in _PRIM_RTYPES and isinstance(value, int) and not isinstance(value, bool):
        bits, signed = _PRIM_RTYPES[f.rtype]
        return _rust_int(value, bits, signed)
    return None


def _py_field_value(f: "PacketShape", value: dict) -> object:
    """The reference value for a field, following its flattened parent path."""
    if f.parent:
        current: object = value
        for key in f.parent.split("."):
            if not isinstance(current, dict) or key not in current:
                return None
            current = current[key]
        value = current if isinstance(current, dict) else {}
    for key in (f.ident, f.ident.rstrip("_")):
        if key in value:
            return value[key]
    return None


def _parity_args(
    pkt: PacketDecl, fields: list["PacketShape"], resolved: ResolvedSchema
) -> tuple[list[str], bytes] | None:
    """Constructor literals + the Python-encoded reference bytes for `pkt`.

    The interpreter is the reference encoder (its output is pinned by the
    golden frames in `tests/test_play_packets.py`), so a Rust serializer that
    agrees with it is wire-correct — a self-roundtrip alone only proves the
    encoder and decoder are mutually inverse. Returns None when a packet's
    value cannot be rendered; the caller then falls back to a default-value
    self-roundtrip rather than failing the emit.
    """
    fields, field_types = _packet_fields(pkt, resolved)

    def _payload_mode(f, index):
        if _frame_walker(f):
            return "scan"
        if _len_field(f):
            return "len_field"
        unframed = (f.ref is not None and f.ref.name == "rest") or (
            f.opaque and _opaque_len(f.ref, resolved, f.params) is None
        )
        if not unframed:
            return "plain"
        return "to_end" if index == len(fields) - 1 else "prefixed"

    try:
        # A payload this emitter must length-prefix (an unframed opaque repeat
        # element, a non-final unframed payload) diverges from the reference's
        # concatenated payloads, so those packets keep the consistency check
        # but no byte-for-byte pin. Self-delimiting payloads are written raw
        # and read by scanning their length, exactly like the reference, so
        # they still pin.
        for i, f in enumerate(fields):
            if f.repeat_mode is not None and any(
                _elem_needs_prefix(e, resolved) for e in f.element
            ):
                return None
            if _payload_mode(f, i) == "prefixed":
                return None
        py = synthesize_packet(resolved, pkt)
        expected = encode_packet(resolved, pkt.name, py)
        lits = []
        for f in fields:
            lit = _rust_literal(f, _py_field_value(f, py), resolved, py)
            if lit is None:
                return None
            lits.append(lit)
    except Exception:
        return None
    return lits, expected


def _emit_roundtrip_test(
    stem: str, name: str, ser: str, pkt: PacketDecl, resolved: ResolvedSchema
) -> str:
    """One test per packet, matching rstom's `*.tests.rs` contract."""
    fields, _types = _packet_fields(pkt, resolved)
    parity = _parity_args(pkt, fields, resolved)
    if parity:
        literals, pin_bytes = parity
        ctor = f"{name}::new({', '.join(literals)})"
        pin = (
            "    // Pinned by the Python interpreter's encoding of the same\n"
            "    // synthetic value (`bps/synth.py`); this asserts the Rust\n"
            "    // serializer produces identical bytes, not just inverse ones.\n"
            f"    assert_eq!(bytes, {_rust_bytes(pin_bytes)});\n"
        )
        extra = [
            use
            for use, marker in (
                ("use rstom::coordinate::Vec as CoordVec;", "CoordVec::new"),
                ("use rstom::network::uuid::Uuid;", "Uuid::from_bits"),
            )
            if marker in ctor
        ]
        use_block = "".join(f"{use}\n" for use in extra) + "\n"
    else:
        ctor = f"{name}::default()"
        pin = (
            "    // No pinned reference for this packet's synthetic value; the\n"
            "    // encode/decode pair is only checked for consistency.\n"
        )
        use_block = ""
    return f'''//! Generated by `bps emit rust`: round-trip for `{name}`.

use bps_rstom::packets::{stem}::{name};

use rstom::network::buffer::NetworkBuffer;
use rstom::network::packet::serializer::PacketValue;
{use_block}
#[test]
fn round_trips() {{
    let packet = {ctor};
    let bytes = NetworkBuffer::make_array(|buffer| {{
        {name}::serializer().write(buffer, &(Box::new(packet.clone()) as PacketValue))
    }});
{pin}    let mut buffer = NetworkBuffer::wrap(bytes.clone(), 0, bytes.len() as i64);
    let decoded = {name}::serializer().read(&mut buffer);
    let bytes2 = NetworkBuffer::make_array(|buffer| {{
        {name}::serializer().write(buffer, &decoded)
    }});
    assert_eq!(bytes, bytes2);
}}
'''


def _emit_packets_list(resolved: ResolvedSchema) -> str:
    imports = []
    lines = []
    for pkt in sorted(resolved.packets.values(), key=lambda p: p.name):
        stem = _snake(pkt.name)
        name = _struct_name(pkt)
        imports.append(f"use super::{stem}::{name};")
        lines.append(f"        ({name}::CLASS_NAME, {name}::serializer()),")
    body = "\n".join(lines) or "        // no packets"
    imp = "\n".join(imports)
    return f'''//! Generated by `bps emit rust`: serializer registry for `ported::all()`.

use rstom::network::packet::packet::Packet;
use rstom::network::packet::serializer::Serializer;

{imp}

#[must_use]
pub fn list() -> Vec<(&'static str, Serializer)> {{
    vec![
{body}
    ]
}}
'''


def _emit_registry_fn(
    key: str, entries: list[tuple[RouteDecl, PacketDecl]], resolved: ResolvedSchema
) -> str:
    """`key` is e.g. `client_play` (group_state)."""
    group, state = key.split("_", 1)
    direction = "serverbound" if group == "client" else "clientbound"
    fn = f"{group}_{state}_registry"
    static_name = f"{group.upper()}_{state.upper()}"
    conn_state = _connection_state_rust(state)
    conn_side = _connection_side_rust(direction)
    uses = sorted(
        {
            f"use crate::packets::{_snake(pkt.name)}::{_struct_name(pkt)};"
            for _, pkt in entries
        }
    )
    use_block = "\n".join(uses)
    entry_lines = []
    for _, pkt in entries:
        name = _struct_name(pkt)
        g, st, _ = _primary_route(pkt.name, resolved)
        java = _java_class_name(pkt, g, st)
        entry_lines.append(
            f'                PacketEntry::ported::<{name}>("{java}", {name}::serializer()),'
        )
    entries_body = "\n".join(entry_lines)
    return f'''//! Generated by `bps emit rust`: `{fn}`.

use std::sync::OnceLock;

use super::resolved_registry;
use rstom::network::connection_state::ConnectionState;
use rstom::network::packet::packet_registry::{{ConnectionSide, PacketRegistry}};
use rstom::network::packet::serializer::PacketEntry;

{use_block}

static {static_name}: OnceLock<PacketRegistry> = OnceLock::new();

#[must_use]
pub fn {fn}() -> &'static PacketRegistry {{
    {static_name}.get_or_init(|| {{
        resolved_registry(
            {conn_state},
            {conn_side},
            vec![
{entries_body}
            ],
        )
    }})
}}
'''


_VANILLA_BODY = '''use rstom::network::connection_state::ConnectionState;
use rstom::network::packet::packet_error::PacketError;
use rstom::network::packet::packet_parser::PacketParser;
use rstom::network::packet::packet_registry::{ConnectionSide, PacketRegistry};
use rstom::network::packet::serializer::{PacketEntry, PacketValue};

use crate::packets::{
    configuration_acknowledged::ConfigurationAcknowledgedPacket,
    finish_configuration::FinishConfigurationPacket,
    handshake::HandshakePacket,
    login_acknowledged::LoginAcknowledgedPacket,
    login_finished::LoginFinishedPacket,
    start_configuration::StartConfigurationPacket,
};

pub(crate) fn resolved_registry(
    state: ConnectionState,
    side: ConnectionSide,
    entries: Vec<PacketEntry>,
) -> PacketRegistry {
    let ported = crate::ported::all();
    PacketRegistry::registry(
        state,
        side,
        entries
            .into_iter()
            .map(|entry| entry.resolve(&ported))
            .collect(),
    )
}

#[must_use]
pub fn next_client_state(
    packet: &PacketValue,
    current_state: ConnectionState,
) -> Result<ConnectionState, PacketError> {
    if let Some(handshake) = packet.downcast_ref::<HandshakePacket>() {
        return Ok(match handshake.intent {
            1 => ConnectionState::Status,
            2 | 3 => ConnectionState::Login,
            _ => current_state,
        });
    }
    if packet.downcast_ref::<LoginAcknowledgedPacket>().is_some()
        || packet.downcast_ref::<ConfigurationAcknowledgedPacket>().is_some()
    {
        return Ok(ConnectionState::Configuration);
    }
    if packet.downcast_ref::<FinishConfigurationPacket>().is_some() {
        return Ok(ConnectionState::Play);
    }
    Ok(current_state)
}

#[must_use]
pub fn next_server_state(
    packet: &PacketValue,
    current_state: ConnectionState,
) -> Result<ConnectionState, PacketError> {
    if current_state == ConnectionState::Handshake {
        return Err(PacketError::IllegalState(
            "No server Handshake packet exists".to_string(),
        ));
    }
    if packet.downcast_ref::<LoginFinishedPacket>().is_some()
        || packet.downcast_ref::<StartConfigurationPacket>().is_some()
    {
        return Ok(ConnectionState::Configuration);
    }
    if packet.downcast_ref::<FinishConfigurationPacket>().is_some() {
        return Ok(ConnectionState::Play);
    }
    Ok(current_state)
}

pub struct ClientPacketParser;

pub struct ServerPacketParser;

impl PacketParser for ClientPacketParser {
    fn handshake(&self) -> &PacketRegistry {
        client_handshake_registry()
    }
    fn status(&self) -> &PacketRegistry {
        client_status_registry()
    }
    fn login(&self) -> &PacketRegistry {
        client_login_registry()
    }
    fn configuration(&self) -> &PacketRegistry {
        client_configuration_registry()
    }
    fn play(&self) -> &PacketRegistry {
        client_play_registry()
    }
}

impl PacketParser for ServerPacketParser {
    fn handshake(&self) -> &PacketRegistry {
        server_handshake_registry()
    }
    fn status(&self) -> &PacketRegistry {
        server_status_registry()
    }
    fn login(&self) -> &PacketRegistry {
        server_login_registry()
    }
    fn configuration(&self) -> &PacketRegistry {
        server_configuration_registry()
    }
    fn play(&self) -> &PacketRegistry {
        server_play_registry()
    }
}

pub static CLIENT_PACKET_PARSER: ClientPacketParser = ClientPacketParser;
pub static SERVER_PACKET_PARSER: ServerPacketParser = ServerPacketParser;
'''

_PORTED_RS = '''//! Generated by `bps emit rust`: ported serializer table.

use rstom::network::packet::serializer::Serializer;

#[must_use]
pub fn all() -> Vec<(&'static str, Serializer)> {
    crate::packets::list::list()
}
'''

# Self-delimiting payload frames. A registry-keyed payload (NBT, Slot, an id
# set) has no length prefix on the wire, so a reader must scan the structure to
# know where it ends. These walkers measure one payload at the front of a byte
# slice; the generated serializers write such a payload as raw bytes and read
# back exactly the scanned length, which is what the reference encoder does.
_FRAMES_RS = '''//! Generated by `bps emit rust`: self-delimiting payload scanners.

/// Bytes of one non-zigzag LEB128 VarInt at the front of `bytes`.
fn varint_len(bytes: &[u8]) -> Option<usize> {
    for (i, byte) in bytes.iter().enumerate().take(5) {
        if byte & 0x80 == 0 {
            return Some(i + 1);
        }
    }
    None
}

/// Value of one VarInt, for the payloads whose shape depends on it.
fn varint_value(bytes: &[u8]) -> Option<i32> {
    let mut result: i32 = 0;
    for (i, byte) in bytes.iter().enumerate().take(5) {
        result |= i32::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some(result);
        }
    }
    None
}

fn skip_u16(bytes: &[u8], pos: &mut usize) -> Option<()> {
    let lo = *bytes.get(*pos)? as usize;
    let hi = *bytes.get(*pos + 1)? as usize;
    *pos += 2 + ((hi << 8) | lo);
    Some(())
}

fn skip_i32(bytes: &[u8], pos: &mut usize) -> Option<i32> {
    let value = i32::from_be_bytes([
        *bytes.get(*pos)?,
        *bytes.get(*pos + 1)?,
        *bytes.get(*pos + 2)?,
        *bytes.get(*pos + 3)?,
    ]);
    *pos += 4;
    Some(value)
}

/// One NBT payload body, addressed by its tag id (compound entries, list
/// items, ...), the shape Java's `readPayload` switches on.
fn skip_nbt_body(tag: u8, bytes: &[u8], pos: &mut usize, depth: u32) -> Option<()> {
    if depth > 512 {
        return None;
    }
    match tag {
        0 => {}
        1 => *pos += 1,
        2 => *pos += 2,
        3 | 5 => *pos += 4,
        4 | 6 => *pos += 8,
        7 => *pos += usize::try_from(skip_i32(bytes, pos)?).ok()?,
        8 => skip_u16(bytes, pos)?,
        9 => {
            let element = *bytes.get(*pos)?;
            *pos += 1;
            for _ in 0..skip_i32(bytes, pos)? {
                skip_nbt_body(element, bytes, pos, depth + 1)?;
            }
        }
        10 => loop {
            let entry_type = *bytes.get(*pos)?;
            *pos += 1;
            if entry_type == 0 {
                break;
            }
            skip_u16(bytes, pos)?;
            skip_nbt_body(entry_type, bytes, pos, depth + 1)?;
        },
        11 => *pos += 4 * usize::try_from(skip_i32(bytes, pos)?).ok()?,
        12 => *pos += 8 * usize::try_from(skip_i32(bytes, pos)?).ok()?,
        _ => return None,
    }
    (*pos <= bytes.len()).then_some(())
}

/// Length of one tag-id-prefixed NBT payload (`text_component`).
#[must_use]
pub fn nbt_tag_len(bytes: &[u8]) -> usize {
    let tag = *bytes.first().unwrap_or(&0);
    let mut pos = 1usize;
    if skip_nbt_body(tag, bytes, &mut pos, 0).is_none() {
        return bytes.len();
    }
    pos.min(bytes.len())
}

/// Length of one nameless NBT compound (`nbt`): entries until TAG_END.
#[must_use]
pub fn nbt_compound_len(bytes: &[u8]) -> usize {
    let mut pos = 0usize;
    loop {
        let entry_type = *bytes.get(pos).unwrap_or(&0);
        pos += 1;
        if entry_type == 0 {
            break;
        }
        if skip_u16(bytes, &mut pos).is_none() || skip_nbt_body(entry_type, bytes, &mut pos, 0).is_none() {
            return bytes.len();
        }
    }
    pos.min(bytes.len())
}

/// Length of one Slot payload.
#[must_use]
pub fn slot_len(bytes: &[u8]) -> usize {
    let mut pos = varint_len(bytes).unwrap_or(bytes.len());
    let count = varint_value(bytes).unwrap_or(0);
    if count > 0 {
        for _ in 0..3 {
            let n = varint_len(bytes.get(pos..).unwrap_or(&[])).unwrap_or(0);
            pos += n;
        }
    }
    pos.min(bytes.len())
}

/// Length of one Hashed Slot payload.
#[must_use]
pub fn hashed_slot_len(bytes: &[u8]) -> usize {
    let mut pos = 1usize;
    if bytes.first().copied().unwrap_or(0) != 0 {
        for _ in 0..3 {
            let n = varint_len(bytes.get(pos..).unwrap_or(&[])).unwrap_or(0);
            pos += n;
        }
    }
    pos.min(bytes.len())
}

/// Length of one registry id or inline payload: the id VarInt, plus the
/// inline body (which reads to end of buffer) when the id is 0.
#[must_use]
pub fn id_or_payload_len(bytes: &[u8]) -> usize {
    let id_len = varint_len(bytes).unwrap_or(bytes.len());
    if varint_value(bytes).unwrap_or(1) == 0 {
        return bytes.len();
    }
    id_len.min(bytes.len())
}

/// Length of one registry id set payload.
#[must_use]
pub fn id_set_len(bytes: &[u8]) -> usize {
    let mut pos = varint_len(bytes).unwrap_or(bytes.len());
    let tag_type = varint_value(bytes).unwrap_or(0);
    if tag_type == 0 {
        if skip_u16(bytes, &mut pos).is_none() {
            return bytes.len();
        }
    } else {
        for _ in 0..tag_type.saturating_sub(1) {
            let n = varint_len(bytes.get(pos..).unwrap_or(&[])).unwrap_or(0);
            pos += n;
        }
    }
    pos.min(bytes.len())
}
'''


def _parser_methods(by_reg: dict[str, list], group: str) -> str:
    """`PacketParser` methods for one side (`client` or `server`)."""
    state_to_key = {
        "handshake": f"{group}_handshaking",
        "status": f"{group}_status",
        "login": f"{group}_login",
        "configuration": f"{group}_configuration",
        "play": f"{group}_play",
    }
    lines: list[str] = []
    for method, key in state_to_key.items():
        if key not in by_reg:
            lines.append(
                f"    fn {method}(&self) -> &PacketRegistry {{\n"
                f"        panic!(\"no registry for {key}\")\n"
                f"    }}"
            )
        else:
            lines.append(
                f"    fn {method}(&self) -> &PacketRegistry {{\n"
                f"        {key}_registry()\n"
                f"    }}"
            )
    return "\n\n".join(lines)


def _vanilla_mod_rs(by_reg: dict[str, list]) -> str:
    reexport = "\n".join(
        f"mod {k};\npub use {k}::{k}_registry;" for k in sorted(by_reg)
    )
    client_parser = _parser_methods(by_reg, "client")
    server_parser = _parser_methods(by_reg, "server")
    tail = _VANILLA_BODY.split("impl PacketParser for ClientPacketParser {", 1)[0]
    parsers = (
        f"impl PacketParser for ClientPacketParser {{\n{client_parser}\n}}\n\n"
        f"impl PacketParser for ServerPacketParser {{\n{server_parser}\n}}\n\n"
        "pub static CLIENT_PACKET_PARSER: ClientPacketParser = ClientPacketParser;\n"
        "pub static SERVER_PACKET_PARSER: ServerPacketParser = ServerPacketParser;\n"
    )
    return (
        "//! Generated by `bps emit rust`: `PacketVanilla` id tables and parsers.\n\n"
        + reexport
        + "\n\n"
        + tail
        + parsers
    )


def _emit_packet_vanilla(resolved: ResolvedSchema, vanilla_dir: Path) -> list[Path]:
    written: list[Path] = []
    by_reg = _routes_by_registry(resolved)
    vanilla_dir.mkdir(parents=True, exist_ok=True)
    for key, entries in sorted(by_reg.items()):
        body = _emit_registry_fn(key, entries, resolved)
        (vanilla_dir / f"{key}.rs").write_text(body, encoding="utf-8")
        written.append(vanilla_dir / f"{key}.rs")
    (vanilla_dir / "mod.rs").write_text(_vanilla_mod_rs(by_reg), encoding="utf-8")
    written.append(vanilla_dir / "mod.rs")
    return written


def emit_rstom(
    resolved: ResolvedSchema,
    out_dir: Path,
    rstom_path: str = "../rstom",
) -> list[Path]:
    """Emit an rstom-compatible packet crate. Returns the emitted files.

    `rstom_path` is the `path` dependency written into the crate's manifest;
    the deployment target (`../rstom/crates/bps_rstom`) resolves its sibling
    crate relatively, while a scratch emit can point at an absolute path.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    src = out_dir / "src"
    written: list[Path] = []

    mod_lines = []
    for pkt in sorted(resolved.packets.values(), key=lambda p: p.name):
        stem = _snake(pkt.name)
        (src / "packets").mkdir(parents=True, exist_ok=True)
        body = _emit_packet(pkt, resolved)
        (src / "packets" / f"{stem}.rs").write_text(body, encoding="utf-8")
        mod_lines.append(f"pub mod {stem};")
        written.append(src / "packets" / f"{stem}.rs")

    list_path = src / "packets" / "list.rs"
    list_path.write_text(_emit_packets_list(resolved), encoding="utf-8")
    written.append(list_path)
    mod_lines.append("pub mod list;")

    (src / "packets" / "mod.rs").write_text(
        "//! Generated by `bps emit rust`: one module per declared packet.\n\n"
        + "\n".join(mod_lines)
        + "\n",
        encoding="utf-8",
    )
    written.append(src / "packets" / "mod.rs")

    for old in src.glob("packet_*.rs"):
        old.unlink(missing_ok=True)

    ported_path = src / "ported.rs"
    ported_path.write_text(_PORTED_RS, encoding="utf-8")
    written.append(ported_path)

    vanilla_written = _emit_packet_vanilla(resolved, src / "packet_vanilla")
    written.extend(vanilla_written)

    tests = out_dir / "tests"
    tests.mkdir(parents=True, exist_ok=True)
    for pkt in sorted(resolved.packets.values(), key=lambda p: p.name):
        name = _struct_name(pkt)
        stem = _snake(pkt.name)
        ser = name[: -len("Packet")] + "Serializer"
        (tests / f"roundtrip_{stem}.rs").write_text(
            _emit_roundtrip_test(stem, name, ser, pkt, resolved),
            encoding="utf-8",
        )
        written.append(tests / f"roundtrip_{stem}.rs")

    (src / "lib.rs").write_text(
        "//! Generated by `bps emit rust`: packet payloads, `ported`, and `packet_vanilla`.\n\n"
        "pub mod frames;\n"
        "pub mod packet_vanilla;\n"
        "pub mod packets;\n"
        "pub mod ported;\n",
        encoding="utf-8",
    )
    written.append(src / "lib.rs")
    frames_path = src / "frames.rs"
    frames_path.write_text(_FRAMES_RS, encoding="utf-8")
    written.append(frames_path)

    (out_dir / "Cargo.toml").write_text(
        '[package]\nname = "bps_rstom"\nversion = "0.1.0"\nedition = "2024"\n\n'
        "[dependencies]\n"
        f'rstom = {{ path = "{rstom_path}" }}\n\n'
        "[dev-dependencies]\n"
        f'rstom = {{ path = "{rstom_path}" }}\n',
        encoding="utf-8",
    )
    written.append(out_dir / "Cargo.toml")

    # The crate is fully generated: drop files an earlier emit left behind that
    # the current schema no longer declares, so the tree matches `lib.rs`.
    for d, pattern in ((src / "packets", "*.rs"), (src / "packet_vanilla", "*.rs")):
        for stale in d.glob(pattern):
            if stale not in written:
                stale.unlink()
    for stale in tests.glob("roundtrip_*.rs"):
        if stale not in written:
            stale.unlink()

    # Emitted code is stable but not canonical by construction (hand-rolled
    # string templates); rustfmt is the single source of truth for layout.
    rustfmt = shutil.which("rustfmt")
    if rustfmt:
        for f in written:
            if f.suffix == ".rs":
                subprocess.run(
                    [rustfmt, "--edition", "2024", str(f)],
                    check=False,
                    capture_output=True,
                )
    return written
