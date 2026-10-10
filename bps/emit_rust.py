"""Rust code generator: resolved BPS schema -> dependency-free Rust crate.

The emitter walks the resolved AST and produces, per packet, a decoder that mirrors
the reference interpreter: primitives, VarInt/VarLong, strings, UUIDs, positions,
network NBT, slots, conditional groups, tagged unions and counted/terminated repeats.
"""

from __future__ import annotations

import re
from pathlib import Path

from bps import ast
from bps.ast import BlockItem, EncodingDecl, PacketDecl, ResolvedSchema, TypeRef, WireField

RUNTIME_RS = r'''//! Generated BPS runtime: cursor, Minecraft VarInt, and shared wire helpers.

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum BpsError {
    Msg(String),
    At(String, usize),
}

impl std::fmt::Display for BpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BpsError::Msg(m) => write!(f, "{m}"),
            BpsError::At(m, o) => write!(f, "{m} at offset {o}"),
        }
    }
}

impl std::error::Error for BpsError {}

pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn read(&mut self, n: usize) -> Result<&'a [u8], BpsError> {
        if self.pos + n > self.data.len() {
            return Err(BpsError::At("unexpected EOF".into(), self.pos));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn peek_u8(&self) -> Result<u8, BpsError> {
        if self.pos >= self.data.len() {
            return Err(BpsError::At("unexpected EOF".into(), self.pos));
        }
        Ok(self.data[self.pos])
    }
}

pub fn u8_at(cur: &mut Cursor) -> Result<u8, BpsError> {
    Ok(cur.read(1)?[0])
}

pub fn i8_at(cur: &mut Cursor) -> Result<i64, BpsError> {
    Ok(u8_at(cur)? as i8 as i64)
}

pub fn u16_be(cur: &mut Cursor) -> Result<u16, BpsError> {
    let b = cur.read(2)?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

pub fn u32_be(cur: &mut Cursor) -> Result<u32, BpsError> {
    let b = cur.read(4)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn i32_be(cur: &mut Cursor) -> Result<i64, BpsError> {
    Ok(u32_be(cur)? as i32 as i64)
}

pub fn u64_be(cur: &mut Cursor) -> Result<u64, BpsError> {
    let b = cur.read(8)?;
    Ok(u64::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

pub fn i64_be(cur: &mut Cursor) -> Result<i64, BpsError> {
    Ok(u64_be(cur)? as i64)
}

pub fn f32_be(cur: &mut Cursor) -> Result<f64, BpsError> {
    Ok(f32::from_bits(u32_be(cur)?) as f64)
}

pub fn f64_be(cur: &mut Cursor) -> Result<f64, BpsError> {
    Ok(f64::from_bits(u64_be(cur)?))
}

pub fn boolean(cur: &mut Cursor) -> Result<bool, BpsError> {
    Ok(u8_at(cur)? != 0)
}

pub fn sign_extend(v: i64, bits: u32) -> i64 {
    let mask = 1i64 << (bits - 1);
    if (v & mask) != 0 {
        v - (1i64 << bits)
    } else {
        v
    }
}

pub fn leb128(cur: &mut Cursor, bits: u32) -> Result<i64, BpsError> {
    let max_bytes = if bits == 64 { 10 } else { 5 };
    let mut shift = 0u32;
    let mut result: u64 = 0;
    for _ in 0..max_bytes {
        let b = u8_at(cur)?;
        result |= ((b & 0x7F) as u64) << shift;
        shift += 7;
        if (b & 0x80) == 0 {
            break;
        }
    }
    let mut v = result as i64;
    if bits == 32 && (result & (1u64 << 31)) != 0 {
        v -= 1i64 << 32;
    } else if bits == 64 && (result & (1u64 << 63)) != 0 {
        v = (result as i64) | i64::MIN;
    }
    Ok(v)
}

pub fn varint(cur: &mut Cursor) -> Result<i64, BpsError> {
    leb128(cur, 32)
}

pub fn varlong(cur: &mut Cursor) -> Result<i64, BpsError> {
    leb128(cur, 64)
}

pub fn utf16_units(s: &str) -> usize {
    s.chars().map(|c| if (c as u32) > 0xFFFF { 2 } else { 1 }).sum()
}

pub fn string_at(cur: &mut Cursor, max_chars: usize) -> Result<String, BpsError> {
    let len = varint(cur)? as usize;
    let raw = cur.read(len)?.to_vec();
    let s = String::from_utf8(raw).map_err(|e| BpsError::Msg(e.to_string()))?;
    if utf16_units(&s) > max_chars {
        return Err(BpsError::Msg(format!("string exceeds {max_chars} UTF-16 units")));
    }
    Ok(s)
}

pub fn uuid_at(cur: &mut Cursor) -> Result<Value, BpsError> {
    let msb = u64_be(cur)?;
    let lsb = u64_be(cur)?;
    Ok(json!({ "msb": msb, "lsb": lsb }))
}

pub fn position_at(cur: &mut Cursor) -> Result<Value, BpsError> {
    let raw = u64_be(cur)?;
    let x = sign_extend((raw >> 38) as i64, 26);
    let y = sign_extend((raw % 4096) as i64, 12);
    let z = sign_extend(((raw >> 12) % 67108864) as i64, 26);
    Ok(json!({ "raw": raw, "x": x, "y": y, "z": z }))
}

/// Network NBT (protocol >= 764): root compound payload, records until TAG_End.
pub fn nbt_at(cur: &mut Cursor) -> Result<Value, BpsError> {
    Ok(nbt_compound_body(cur)?)
}

pub fn nbt_tag_at(cur: &mut Cursor) -> Result<Value, BpsError> {
    let tag_type = u8_at(cur)?;
    Ok(json!({ "tag_type": tag_type, "value": nbt_payload(cur, tag_type)? }))
}

fn nbt_compound_body(cur: &mut Cursor) -> Result<Value, BpsError> {
    let mut entries = Vec::new();
    while cur.remaining() > 0 {
        if cur.peek_u8()? == 0x00 {
            cur.read(1)?;
            break;
        }
        let tag_type = u8_at(cur)?;
        let name = nbt_string_at(cur)?;
        let value = nbt_payload(cur, tag_type)?;
        entries.push(json!({ "tag_type": tag_type, "name": name, "value": value }));
    }
    Ok(json!({ "entries": entries }))
}

fn nbt_payload(cur: &mut Cursor, tag_type: u8) -> Result<Value, BpsError> {
    Ok(match tag_type {
        1 => json!(i8_at(cur)?),
        2 => json!(u16_be(cur)? as i16 as i64),
        3 => json!(i32_be(cur)?),
        4 => json!(i64_be(cur)?),
        5 => json!(f32_be(cur)?),
        6 => json!(f64_be(cur)?),
        7 => {
            let n = i32_be(cur)? as usize;
            json!({ "byte_len": n, "data": cur.read(n)?.to_vec() })
        }
        8 => json!(nbt_string_at(cur)?),
        9 => nbt_list_body(cur)?,
        10 => nbt_compound_body(cur)?,
        11 => {
            let n = i32_be(cur)? as usize;
            let mut vals = Vec::new();
            for _ in 0..n {
                vals.push(json!(i32_be(cur)?));
            }
            json!({ "byte_len": n, "data": vals })
        }
        12 => {
            let n = i32_be(cur)? as usize;
            let mut vals = Vec::new();
            for _ in 0..n {
                vals.push(json!(i64_be(cur)?));
            }
            json!({ "byte_len": n, "data": vals })
        }
        _ => Value::Null,
    })
}

fn nbt_list_body(cur: &mut Cursor) -> Result<Value, BpsError> {
    let elem_type = u8_at(cur)?;
    let count = i32_be(cur)? as usize;
    let mut items = Vec::new();
    for _ in 0..count {
        items.push(json!({ "v": nbt_payload(cur, elem_type)? }));
    }
    Ok(json!({ "elem_type": elem_type, "count": count, "items": items }))
}

fn nbt_string_at(cur: &mut Cursor) -> Result<String, BpsError> {
    let len = u16_be(cur)? as usize;
    let raw = cur.read(len)?.to_vec();
    String::from_utf8(raw).map_err(|e| BpsError::Msg(e.to_string()))
}

/// Slot frame: item count, then the registry-keyed component counts (payloads are
/// registry-dependent and not walked, matching the v1 schema).
pub fn slot_at(cur: &mut Cursor) -> Result<Value, BpsError> {
    let item_count = varint(cur)?;
    if item_count <= 0 {
        return Ok(json!({ "item_count": item_count }));
    }
    let item_id = varint(cur)?;
    let add_count = varint(cur)?;
    let remove_count = varint(cur)?;
    Ok(json!({
        "item_count": item_count,
        "item_id": item_id,
        "add_count": add_count,
        "remove_count": remove_count,
    }))
}

pub fn hashed_slot_at(cur: &mut Cursor) -> Result<Value, BpsError> {
    let has_item = boolean(cur)?;
    if !has_item {
        return Ok(json!({ "has_item": false }));
    }
    Ok(json!({
        "has_item": true,
        "item_id": varint(cur)?,
        "item_count": varint(cur)?,
        "add_count": varint(cur)?,
        "remove_count": varint(cur)?,
    }))
}
'''

# Primitives emitted inline (no helper function needed beyond runtime fns).
PRIMITIVES: dict[str, str] = {
    "u8": "u8_at(cur)? as i64",
    "i8": "i8_at(cur)?",
    "u16be": "u16_be(cur)? as i64",
    "i16be": "u16_be(cur)? as i16 as i64",
    "u32be": "u32_be(cur)? as i64",
    "i32be": "i32_be(cur)?",
    "u64be": "u64_be(cur)? as i64",
    "i64be": "i64_be(cur)?",
    "f32be": "f32_be(cur)?",
    "f64be": "f64_be(cur)?",
    "varint": "varint(cur)?",
    "varlong": "varlong(cur)?",
    "bool": "boolean(cur)?",
    "boolean": "boolean(cur)?",
    "angle": "u8_at(cur)? as i64",
}

INT_TYPES = {
    "u8", "i8", "u16be", "i16be", "u32be", "i32be", "u64be", "i64be",
    "varint", "varlong", "angle", "leb128",
}

# Encodings with dedicated runtime helpers.
HELPERS: dict[str, str] = {
    "position": "position_at(cur)?",
    "uuid": "uuid_at(cur)?",
    "nbt": "nbt_at(cur)?",
    "text_component": "nbt_tag_at(cur)?",
    "slot": "slot_at(cur)?",
    "hashed_slot": "hashed_slot_at(cur)?",
}


def _snake(s: str) -> str:
    return re.sub(r"[^a-zA-Z0-9_]", "_", s)


def _is_rest_only(pkt: PacketDecl) -> bool:
    if len(pkt.items) != 1:
        return False
    it = pkt.items[0]
    return (
        it.kind == "field"
        and it.wire is not None
        and isinstance(it.wire.type_ref, TypeRef)
        and it.wire.type_ref.name == "rest"
    )


class Walker:
    """Emits Rust statements that decode a block into `out` (JSON) and `env` (ints)."""

    def __init__(self, resolved: ResolvedSchema) -> None:
        self.resolved = resolved
        self.counter = 0

    def tmp(self) -> str:
        self.counter += 1
        return f"t{self.counter}"

    # --- expressions -------------------------------------------------------

    def expr(self, e: ast.Expr) -> str:
        k = e.kind
        if k == "int":
            return str(int(e.value))
        if k == "bool":
            return "true" if e.value else "false"
        if k == "ident":
            return f'env.get("{e.name}").copied().unwrap_or(0)'
        if k == "unop":
            if e.value == "!":
                return f"(!({self.expr(e.left)} != 0))"
            return f"(-{self.expr(e.left)})"
        if k == "binop":
            left, right = self.expr(e.left), self.expr(e.right)
            op = e.value
            if op == "&&":
                return f"(if ({left} != 0) && ({right} != 0) {{ 1 }} else {{ 0 }})"
            if op == "||":
                return f"(if ({left} != 0) || ({right} != 0) {{ 1 }} else {{ 0 }})"
            if op in ("==", "!=", "<", "<=", ">", ">="):
                return f"(if {left} {op} {right} {{ 1 }} else {{ 0 }})"
            return f"({left} {op} {right})"
        if k == "call":
            args = ", ".join(self.expr(a) for a in e.args)
            name = e.name
            if name == "sign_extend":
                return f"runtime::sign_extend({args})"
            if name == "utf16_units":
                return f"runtime::utf16_units(&{args}) as i64"
            if name == "len":
                return f"(({args}) as i64)"
            if name == "min":
                return f"std::cmp::min({args})"
            if name == "max":
                return f"std::cmp::max({args})"
            if name == "contains":
                return f"({args}).contains(&({args}))"
        if k == "ternary":
            return f"(if {self.expr(e.left)} != 0 {{ {self.expr(e.right)} }} else {{ {self.expr(e.args[0])} }})"
        raise ValueError(f"unsupported expression in Rust emitter: {e}")

    # --- decode of a type into a Rust expression ---------------------------

    def decode_expr(self, t, indent: str) -> str:
        """Return a Rust expression evaluating to a serde_json::Value."""
        if isinstance(t, ast.BlockType):
            return self.block_expr(t.items, indent)
        if isinstance(t, ast.MatchType):
            tag = self.expr(t.tag_expr)
            arms = []
            for arm in t.arms:
                if arm.type_ref is None:
                    continue
                body = self.decode_expr(arm.type_ref, indent + "        ")
                pat = "_" if arm.tag is None else str(int(arm.tag))
                arms.append(f"{indent}        {pat} => {body},")
            joined = "\n".join(arms)
            return (
                "{\n"
                f"{indent}    let tag = {tag};\n"
                f"{indent}    match tag {{\n{joined}\n{indent}        _ => Value::Null,\n"
                f"{indent}    }}\n{indent}}}"
            )
        assert isinstance(t, TypeRef)
        return self.ref_expr(t, indent)

    def ref_expr(self, ref: TypeRef, indent: str) -> str:
        name = ref.name
        if name in PRIMITIVES:
            return f"json!({PRIMITIVES[name]})"
        if name in HELPERS:
            return HELPERS[name]
        if name == "string":
            maxc = self.expr(ref.args[0]) if ref.args else "32767"
            return f"json!(runtime::string_at(cur, {maxc} as usize)?)"
        if name == "nbt_string":
            return "json!({ \"byte_len\": 0 })"
        if name == "bytes":
            n = self.expr(ref.args[0])
            return f"json!(cur.read(({n}) as usize)?.to_vec())"
        if name == "rest":
            return "json!(cur.read(cur.remaining())?.to_vec())"
        if name == "leb128":
            bits = self.expr(ref.args[0]) if ref.args else "32"
            return f"json!(runtime::leb128(cur, ({bits}) as u32)?)"
        enc = self.resolved.encodings.get(name)
        if enc is not None:
            if name == "varint":
                return "json!(runtime::varint(cur)?)"
            if name == "varlong":
                return "json!(runtime::varlong(cur)?)"
            if name in ("string", "identifier"):
                return "json!(runtime::string_at(cur, 32767)?)"
            if name == "nbt_string":
                return (
                    "json!({ \"byte_len\": runtime::u16_be(cur)?, "
                    "\"value\": String::from_utf8(cur.read(runtime::u16_be(cur)? as usize)?.to_vec())"
                    ".map_err(|e| runtime::BpsError::Msg(e.to_string()))? })"
                )
            if isinstance(enc.body, TypeRef):
                return self.ref_expr(enc.body, indent)
            if isinstance(enc.body, ast.BlockType):
                return self.block_expr(enc.body.items, indent)
        raise ValueError(f"unsupported type in Rust emitter: {name}")

    def block_expr(self, items: tuple[BlockItem, ...], indent: str) -> str:
        body = self.block_stmts(items, indent + "    ", nested=True)
        return (
            "{\n"
            f"{indent}    let mut out: serde_json::Map<String, Value> = serde_json::Map::new();\n"
            f"{indent}    let mut env: std::collections::HashMap<String, i64> = env.clone();\n"
            f"{body}\n"
            f"{indent}    Value::Object(out)\n{indent}}}"
        )

    def block_stmts(self, items: tuple[BlockItem, ...], indent: str, nested: bool = False) -> str:
        lines: list[str] = []
        for item in items:
            if item.kind == "require":
                continue
            if item.kind == "computed":
                e = self.expr(item.computed_expr)
                lines.append(f'{indent}env.insert("{item.computed_name}".into(), {e});')
                lines.append(
                    f'{indent}out.insert("{item.computed_name}".into(), json!({e}));'
                )
                continue
            if item.kind == "group":
                cond = self.expr(item.group_if)
                inner = self.block_stmts(item.group_items, indent + "    ")
                lines.append(f"{indent}if {cond} != 0 {{")
                lines.append(inner)
                lines.append(f"{indent}}}")
                continue
            if item.kind == "spread":
                enc = self.resolved.encodings[item.spread_name]
                if isinstance(enc.body, ast.BlockType):
                    lines.append(self.block_stmts(enc.body.items, indent))
                continue
            if item.kind != "field" or item.wire is None:
                continue
            lines.append(self.field_stmts(item.wire, indent))
        return "\n".join(x for x in lines if x)

    def field_stmts(self, wf: WireField, indent: str) -> str:
        if wf.repeat is not None:
            return self.repeat_stmts(wf, indent)
        value = self.decode_expr(wf.type_ref, indent)
        is_int = isinstance(wf.type_ref, TypeRef) and wf.type_ref.name in INT_TYPES
        lines = [f'{indent}let v = {value};']
        if is_int:
            lines.append(
                f'{indent}env.insert("{wf.name}".into(), v.as_i64().unwrap_or(0));'
            )
        lines.append(f'{indent}out.insert("{wf.name}".into(), v);')
        stmt = "\n".join(lines)
        if wf.if_expr is not None:
            cond = self.expr(wf.if_expr)
            return f"{indent}if {cond} != 0 {{\n{stmt}\n{indent}}}"
        return stmt

    def repeat_stmts(self, wf: WireField, indent: str) -> str:
        rep = wf.repeat
        elem = self.decode_expr(wf.type_ref, indent + "        ")
        lines = [f"{indent}{{"]
        lines.append(f"{indent}    let mut items: Vec<Value> = Vec::new();")
        if rep.mode == "count":
            n = self.expr(rep.expr)
            lines.append(f"{indent}    for _ in 0..({n}) {{")
            lines.append(f"{indent}        items.push({elem});")
            lines.append(f"{indent}    }}")
        elif rep.mode == "until_end":
            lines.append(f"{indent}    while cur.remaining() > 0 {{")
            lines.append(f"{indent}        items.push({elem});")
            lines.append(f"{indent}    }}")
        elif rep.mode == "until":
            term = self.expr(rep.expr)
            lines.append(f"{indent}    while cur.remaining() > 0 {{")
            lines.append(f"{indent}        if cur.peek_u8()? as i64 == {term} {{")
            lines.append(f"{indent}            cur.read(1)?;")
            lines.append(f"{indent}            break;")
            lines.append(f"{indent}        }}")
            lines.append(f"{indent}        items.push({elem});")
            lines.append(f"{indent}    }}")
        else:
            raise ValueError(f"unknown repeat mode {rep.mode}")
        lines.append(f'{indent}    out.insert("{wf.name}".into(), json!(items));')
        lines.append(f"{indent}}}")
        return "\n".join(lines)

    # --- packets -----------------------------------------------------------

    def packet_fn(self, pkt: PacketDecl) -> str:
        name = _snake(pkt.name)
        body = self.block_stmts(pkt.items, "    ")
        return f'''
pub fn decode_{name}(cur: &mut Cursor) -> Result<Value, BpsError> {{
    let mut out: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut env: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
{body}
    Ok(Value::Object(out))
}}
'''


def emit_rust(resolved: ResolvedSchema, out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    src = out_dir / "src"
    src.mkdir(parents=True, exist_ok=True)

    proto_id = 0
    if resolved.protocols:
        proto_id = next(iter(resolved.protocols.values())).protocol_id

    walker = Walker(resolved)
    decoders: list[str] = []
    for pkt in resolved.packets.values():
        if _is_rest_only(pkt):
            field = pkt.items[0].wire.name  # type: ignore[union-attr]
            decoders.append(
                f'''
pub fn decode_{_snake(pkt.name)}(cur: &mut Cursor) -> Result<Value, BpsError> {{
    Ok(json!({{ "{field}": cur.read(cur.remaining())?.to_vec() }}))
}}
'''
            )
        else:
            decoders.append(walker.packet_fn(pkt))

    route_arms = "\n".join(
        f'        ({route.packet_id}, "{route.state}", "{route.direction}") => '
        f"decode_{_snake(pkt.name)}(&mut cur),"
        for route, pkt in resolved.routes
    )

    (src / "runtime.rs").write_text(RUNTIME_RS, encoding="utf-8")
    (src / "packets.rs").write_text(
        "//! Generated packet decoders.\n"
        "#![allow(unused_mut, unused_variables, unused_imports, clippy::all)]\n\n"
        "use super::runtime::{self, *};\n"
        "use serde_json::{json, Value};\n\n"
        + "\n".join(decoders)
        + "\n",
        encoding="utf-8",
    )
    (src / "lib.rs").write_text(
        f'''pub mod runtime;
mod packets;

use packets::*;
use runtime::{{BpsError, Cursor, varint}};

pub const PROTOCOL_ID: i32 = {proto_id};

pub fn decode_packet(
    state: &str,
    direction: &str,
    packet_id: i32,
    payload: &[u8],
) -> Result<serde_json::Value, BpsError> {{
    let mut cur = Cursor::new(payload);
    match (packet_id, state, direction) {{
{route_arms}
        _ => Err(BpsError::Msg(format!(
            "no route for id {{}} in {{}}.{{}}", packet_id, state, direction
        ))),
    }}
}}

pub fn decode_frame(
    state: &str,
    direction: &str,
    frame: &[u8],
) -> Result<(i32, serde_json::Value), BpsError> {{
    let mut cur = Cursor::new(frame);
    let _len = varint(&mut cur)?;
    let pid = varint(&mut cur)? as i32;
    let rest = cur.read(cur.remaining())?.to_vec();
    let val = decode_packet(state, direction, pid, &rest)?;
    Ok((pid, val))
}}
''',
        encoding="utf-8",
    )
    (out_dir / "Cargo.toml").write_text(
        '[package]\nname = "bps_generated"\nversion = "0.1.0"\nedition = "2021"\n\n'
        '[dependencies]\nserde_json = "1"\n',
        encoding="utf-8",
    )

    tests_dir = out_dir / "tests"
    tests_dir.mkdir(exist_ok=True)
    (tests_dir / "golden.rs").write_text(
        r'''//! Differential golden vectors, mirroring tests/test_varint.py.

use bps_generated::decode_frame;

#[test]
fn handshake_frame() {
    let frame = [
        0x10, 0x00, 0x89, 0x06, 0x09, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x68, 0x6f, 0x73, 0x74, 0x63,
        0xdd, 0x01,
    ];
    let (pid, val) = decode_frame("handshaking", "serverbound", &frame).unwrap();
    assert_eq!(pid, 0);
    let obj = val.as_object().expect("object");
    assert_eq!(obj.get("protocol_version").and_then(|v| v.as_i64()), Some(777));
    assert_eq!(obj.get("server_address").and_then(|v| v.as_str()), Some("localhost"));
    assert_eq!(obj.get("server_port").and_then(|v| v.as_i64()), Some(25565));
    assert_eq!(obj.get("intent").and_then(|v| v.as_i64()), Some(1));
}

#[test]
fn status_ping_frame() {
    // len=9, id=1, timestamp=0x0102030405060708
    let frame = [0x09, 0x01, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let (pid, val) = decode_frame("status", "serverbound", &frame).unwrap();
    assert_eq!(pid, 1);
    assert_eq!(
        val.get("timestamp").and_then(|v| v.as_i64()),
        Some(0x0102030405060708)
    );
}

#[test]
fn login_start_frame() {
    // len=0x17, id=0, "Notch", UUID 00112233-4455-6677-8899-aabbccddeeff
    let frame = [
        0x17, 0x00, 0x05, 0x4e, 0x6f, 0x74, 0x63, 0x68, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
        0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    ];
    let (pid, val) = decode_frame("login", "serverbound", &frame).unwrap();
    assert_eq!(pid, 0);
    assert_eq!(val.get("name").and_then(|v| v.as_str()), Some("Notch"));
    assert_eq!(
        val.pointer("/player_uuid/msb").and_then(|v| v.as_u64()),
        Some(0x0011223344556677)
    );
}

#[test]
fn play_game_event_frame() {
    let frame = [0x06, 0x27, 0x01, 0x3f, 0xc0, 0x00, 0x00];
    let (pid, val) = decode_frame("play", "clientbound", &frame).unwrap();
    assert_eq!(pid, 0x27);
    assert_eq!(val.get("event").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(val.get("value").and_then(|v| v.as_f64()), Some(1.5));
}

#[test]
fn play_animate_frame() {
    let frame = [0x04, 0x02, 0xac, 0x02, 0x02];
    let (pid, val) = decode_frame("play", "clientbound", &frame).unwrap();
    assert_eq!(pid, 0x02);
    assert_eq!(val.get("entity_id").and_then(|v| v.as_i64()), Some(300));
    assert_eq!(val.get("animation").and_then(|v| v.as_i64()), Some(2));
}
''',
        encoding="utf-8",
    )
