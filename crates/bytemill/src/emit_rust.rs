//! `emit rust`: a port of `bps/emit_rust.py`.
//!
//! Emits a dependency-free generated crate: a runtime, one decoder per packet,
//! a route table, and a golden test. Decoders mirror the reference interpreter.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::analyze::reject_unlowered_scalars;
use crate::ast::{
    BlockItem, EncBody, Expr, FieldType, MatchArm, MatchType, PacketDecl, Repeat, RepeatMode,
    ResolvedSchema, Tag, TypeRef, WireField,
};
use crate::errors::{BmlResult, emit, py_repr_str};
use crate::frames_gen;

const RUNTIME_RS: &str = r#"//! Generated BML runtime: cursor, Minecraft VarInt, and shared wire helpers.

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum BmlError {
    Msg(String),
    At(String, usize),
}

impl std::fmt::Display for BmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BmlError::Msg(m) => write!(f, "{m}"),
            BmlError::At(m, o) => write!(f, "{m} at offset {o}"),
        }
    }
}

impl std::error::Error for BmlError {}

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
    pub fn read(&mut self, n: usize) -> Result<&'a [u8], BmlError> {
        if self.pos + n > self.data.len() {
            return Err(BmlError::At("unexpected EOF".into(), self.pos));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn peek_u8(&self) -> Result<u8, BmlError> {
        if self.pos >= self.data.len() {
            return Err(BmlError::At("unexpected EOF".into(), self.pos));
        }
        Ok(self.data[self.pos])
    }
}

pub fn u8_at(cur: &mut Cursor) -> Result<u8, BmlError> {
    Ok(cur.read(1)?[0])
}

pub fn i8_at(cur: &mut Cursor) -> Result<i64, BmlError> {
    Ok(u8_at(cur)? as i8 as i64)
}

pub fn u16_be(cur: &mut Cursor) -> Result<u16, BmlError> {
    let b = cur.read(2)?;
    Ok(u16::from_be_bytes([b[0], b[1]]))
}

pub fn u32_be(cur: &mut Cursor) -> Result<u32, BmlError> {
    let b = cur.read(4)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

pub fn i32_be(cur: &mut Cursor) -> Result<i64, BmlError> {
    Ok(u32_be(cur)? as i32 as i64)
}

pub fn u64_be(cur: &mut Cursor) -> Result<u64, BmlError> {
    let b = cur.read(8)?;
    Ok(u64::from_be_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

pub fn i64_be(cur: &mut Cursor) -> Result<i64, BmlError> {
    Ok(u64_be(cur)? as i64)
}

pub fn f32_be(cur: &mut Cursor) -> Result<f64, BmlError> {
    Ok(f32::from_bits(u32_be(cur)?) as f64)
}

pub fn f64_be(cur: &mut Cursor) -> Result<f64, BmlError> {
    Ok(f64::from_bits(u64_be(cur)?))
}

pub fn boolean(cur: &mut Cursor) -> Result<bool, BmlError> {
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

pub fn leb128(cur: &mut Cursor, bits: u32) -> Result<i64, BmlError> {
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

pub fn varint(cur: &mut Cursor) -> Result<i64, BmlError> {
    leb128(cur, 32)
}

pub fn varlong(cur: &mut Cursor) -> Result<i64, BmlError> {
    leb128(cur, 64)
}

pub fn utf16_units(s: &str) -> usize {
    s.chars().map(|c| if (c as u32) > 0xFFFF { 2 } else { 1 }).sum()
}

pub fn string_at(cur: &mut Cursor, max_chars: usize) -> Result<String, BmlError> {
    let len = varint(cur)? as usize;
    let raw = cur.read(len)?.to_vec();
    let s = String::from_utf8(raw).map_err(|e| BmlError::Msg(e.to_string()))?;
    if utf16_units(&s) > max_chars {
        return Err(BmlError::Msg(format!("string exceeds {max_chars} UTF-16 units")));
    }
    Ok(s)
}

pub fn uuid_at(cur: &mut Cursor) -> Result<Value, BmlError> {
    let msb = u64_be(cur)?;
    let lsb = u64_be(cur)?;
    Ok(json!({ "msb": msb, "lsb": lsb }))
}

pub fn position_at(cur: &mut Cursor) -> Result<Value, BmlError> {
    let raw = u64_be(cur)?;
    let x = sign_extend((raw >> 38) as i64, 26);
    let y = sign_extend((raw % 4096) as i64, 12);
    let z = sign_extend(((raw >> 12) % 67108864) as i64, 26);
    Ok(json!({ "raw": raw, "x": x, "y": y, "z": z }))
}

/// Network NBT (protocol >= 764): the root tag id, then its payload (no root name).
pub fn nbt_tag_at(cur: &mut Cursor) -> Result<Value, BmlError> {
    let tag_type = u8_at(cur)?;
    Ok(json!({ "tag_type": tag_type, "value": nbt_payload(cur, tag_type)? }))
}

fn nbt_compound_body(cur: &mut Cursor) -> Result<Value, BmlError> {
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

fn nbt_payload(cur: &mut Cursor, tag_type: u8) -> Result<Value, BmlError> {
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

fn nbt_list_body(cur: &mut Cursor) -> Result<Value, BmlError> {
    let elem_type = u8_at(cur)?;
    let count = i32_be(cur)? as usize;
    let mut items = Vec::new();
    for _ in 0..count {
        items.push(json!({ "v": nbt_payload(cur, elem_type)? }));
    }
    Ok(json!({ "elem_type": elem_type, "count": count, "items": items }))
}

fn nbt_string_at(cur: &mut Cursor) -> Result<String, BmlError> {
    let len = u16_be(cur)? as usize;
    let raw = cur.read(len)?.to_vec();
    String::from_utf8(raw).map_err(|e| BmlError::Msg(e.to_string()))
}

/// Slot frame: item count, then the registry-keyed component counts (payloads are
/// registry-dependent and not walked, matching the v1 schema).
pub fn slot_at(cur: &mut Cursor) -> Result<Value, BmlError> {
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

pub fn hashed_slot_at(cur: &mut Cursor) -> Result<Value, BmlError> {
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
"#;

/// Names lowered to runtime helpers. `u8`/`f32be`/`f64be` are real primitives.
/// The integer and varint names are schemas in `schemas/std/`; the helpers must
/// agree with those schemas, which the differential tests check.
const PRIMITIVES: &[(&str, &str)] = &[
    ("u8", "u8_at(cur)? as i64"),
    ("i8", "i8_at(cur)?"),
    ("u16be", "u16_be(cur)? as i64"),
    ("i16be", "u16_be(cur)? as i16 as i64"),
    ("u32be", "u32_be(cur)? as i64"),
    ("i32be", "i32_be(cur)?"),
    ("u64be", "u64_be(cur)? as i64"),
    ("i64be", "i64_be(cur)?"),
    ("f32be", "f32_be(cur)?"),
    ("f64be", "f64_be(cur)?"),
    ("varint", "varint(cur)?"),
    ("varlong", "varlong(cur)?"),
    ("boolean", "boolean(cur)?"),
    ("angle", "u8_at(cur)? as i64"),
];

const INT_TYPES: &[&str] = &[
    "u8", "i8", "u16be", "i16be", "u32be", "i32be", "u64be", "i64be", "varint", "varlong", "angle",
];

/// Encodings with dedicated runtime helpers.
const HELPERS: &[(&str, &str)] = &[
    ("position", "position_at(cur)?"),
    ("uuid", "uuid_at(cur)?"),
    ("nbt", "nbt_tag_at(cur)?"),
    ("text_component", "nbt_tag_at(cur)?"),
    ("slot", "slot_at(cur)?"),
    ("hashed_slot", "hashed_slot_at(cur)?"),
];

const GOLDEN_RS: &str = r#"//! Differential golden vectors, mirroring tests/test_varint.py.

use bml_generated::decode_frame;

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
"#;

fn lookup(table: &[(&'static str, &'static str)], name: &str) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn snake(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The field name of a packet that is a single `rest` field, if it is one.
fn rest_only(pkt: &PacketDecl) -> Option<&str> {
    match pkt.items.as_slice() {
        // A box cannot be destructured in a pattern, so the `rest` type is checked in the guard.
        [BlockItem::Field(wf)] if matches!(&wf.type_ref, FieldType::Ref(tr) if tr.name == "rest") => {
            Some(wf.name.as_str())
        }
        _ => None,
    }
}

fn required(e: &Option<Expr>) -> BmlResult<&Expr> {
    e.as_ref()
        .ok_or_else(|| emit("'NoneType' object has no attribute 'kind'"))
}

/// Emits Rust statements that decode a block into `out` (JSON) and `env` (ints).
struct Walker<'a> {
    resolved: &'a ResolvedSchema,
    /// An encoding that contains itself cannot be expanded inline: it becomes a
    /// function that calls itself. `fns` fills in as packets reach them.
    recursive: Vec<String>,
    fns: Vec<(String, String)>,
}

impl<'a> Walker<'a> {
    fn new(resolved: &'a ResolvedSchema) -> Self {
        Walker {
            resolved,
            recursive: frames_gen::recursive_encodings(resolved),
            fns: Vec::new(),
        }
    }

    /// Rust pattern for an arm. `None` is an unresolved enum-member tag.
    fn match_pat(arm: &MatchArm) -> Option<String> {
        let no_tag = match &arm.tag {
            None => true,
            Some(Tag::Str(s)) => s == "_",
            Some(Tag::Int(_)) => false,
        };
        if no_tag && arm.tags.is_empty() {
            return Some("_".to_string());
        }
        let raw: Vec<&Tag> = if !arm.tags.is_empty() {
            arm.tags.iter().collect()
        } else {
            arm.tag.iter().collect()
        };
        let ints: Vec<String> = raw
            .into_iter()
            .filter_map(|t| match t {
                Tag::Int(v) => Some(v.to_string()),
                Tag::Str(_) => None,
            })
            .collect();
        if ints.is_empty() {
            None
        } else {
            Some(ints.join(" | "))
        }
    }

    // --- expressions -------------------------------------------------------

    fn expr(&self, e: &Expr) -> BmlResult<String> {
        match e {
            Expr::Int(v) => Ok(v.to_string()),
            Expr::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
            Expr::Ident(name) => Ok(format!("env.get(\"{name}\").copied().unwrap_or(0)")),
            Expr::Unop(op, inner) => {
                let x = self.expr(inner)?;
                if op == "!" {
                    Ok(format!("(if {x} == 0 {{ 1 }} else {{ 0 }})"))
                } else {
                    Ok(format!("(-{x})"))
                }
            }
            Expr::Binop(op, l, r) => {
                let left = self.expr(l)?;
                let right = self.expr(r)?;
                match op.as_str() {
                    "&&" => Ok(format!(
                        "(if ({left} != 0) && ({right} != 0) {{ 1 }} else {{ 0 }})"
                    )),
                    "||" => Ok(format!(
                        "(if ({left} != 0) || ({right} != 0) {{ 1 }} else {{ 0 }})"
                    )),
                    "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                        Ok(format!("(if {left} {op} {right} {{ 1 }} else {{ 0 }})"))
                    }
                    _ => Ok(format!("({left} {op} {right})")),
                }
            }
            Expr::Call(name, args) => {
                let joined = args
                    .iter()
                    .map(|a| self.expr(a))
                    .collect::<BmlResult<Vec<_>>>()?
                    .join(", ");
                match name.as_str() {
                    "sign_extend" => Ok(format!("runtime::sign_extend({joined})")),
                    "utf16_units" => Ok(format!("runtime::utf16_units(&{joined}) as i64")),
                    "len" => Ok(format!("(({joined}) as i64)")),
                    "min" => Ok(format!("std::cmp::min({joined})")),
                    "max" => Ok(format!("std::cmp::max({joined})")),
                    "contains" => Ok(format!("({joined}).contains(&({joined}))")),
                    _ => Err(emit(format!(
                        "unsupported expression in Rust emitter: {e:?}"
                    ))),
                }
            }
            Expr::Ternary(cond, yes, no) => Ok(format!(
                "(if {} != 0 {{ {} }} else {{ {} }})",
                self.expr(cond)?,
                self.expr(yes)?,
                self.expr(no)?
            )),
            Expr::Str(_) | Expr::Path(_) => Err(emit(format!(
                "unsupported expression in Rust emitter: {e:?}"
            ))),
        }
    }

    // --- decode of a type into a Rust expression ---------------------------

    /// A Rust expression evaluating to a serde_json::Value.
    fn decode_expr(&mut self, t: &FieldType, indent: &str) -> BmlResult<String> {
        match t {
            FieldType::Block(b) => self.block_expr(&b.items, indent),
            FieldType::Match(mt) => self.match_expr(mt, indent),
            FieldType::Ref(tr) => self.ref_expr(tr, indent),
        }
    }

    fn match_expr(&mut self, mt: &MatchType, indent: &str) -> BmlResult<String> {
        let tag = self.expr(&mt.tag_expr)?;
        let mut arms = Vec::new();
        for arm in &mt.arms {
            let Some(ftype) = &arm.type_ref else { continue };
            let Some(pat) = Self::match_pat(arm) else {
                continue;
            };
            let body = self.decode_expr(ftype, &format!("{indent}        "))?;
            arms.push(format!("{indent}        {pat} => {body},"));
        }
        let joined = arms.join("\n");
        Ok(format!(
            "{{\n{indent}    let tag = {tag};\n{indent}    match tag {{\n{joined}\n{indent}        _ => Value::Null,\n{indent}    }}\n{indent}}}"
        ))
    }

    fn ref_expr(&mut self, r: &TypeRef, indent: &str) -> BmlResult<String> {
        let name = r.name.as_str();
        if let Some(p) = lookup(PRIMITIVES, name) {
            return Ok(format!("json!({p})"));
        }
        if let Some(h) = lookup(HELPERS, name) {
            return Ok(h.to_string());
        }
        if name == "string" {
            let maxc = match r.args.first() {
                Some(a) => self.expr(a)?,
                None => "32767".to_string(),
            };
            return Ok(format!("json!(runtime::string_at(cur, {maxc} as usize)?)"));
        }
        if name == "nbt_string" {
            return Ok(r#"json!({ "byte_len": 0 })"#.to_string());
        }
        if name == "bytes" {
            let Some(arg) = r.args.first() else {
                return Err(emit("list index out of range"));
            };
            let n = self.expr(arg)?;
            return Ok(format!("json!(cur.read(({n}) as usize)?.to_vec())"));
        }
        if name == "rest" {
            return Ok("json!(cur.read(cur.remaining())?.to_vec())".to_string());
        }
        let Some(enc) = self.resolved.encodings.get(name) else {
            return Err(emit(format!("unsupported type in Rust emitter: {name}")));
        };
        if name == "identifier" {
            return Ok("json!(runtime::string_at(cur, 32767)?)".to_string());
        }
        match &enc.body {
            EncBody::Ref(tr) => self.ref_expr(tr, indent),
            EncBody::Block(b) => {
                if self.recursive.iter().any(|n| n == name) {
                    self.recursive_fn(name, &b.items)?;
                    Ok(format!("{}_at(cur, &env)?", snake(name)))
                } else {
                    self.block_expr(&b.items, indent)
                }
            }
        }
    }

    /// Queue the function that decodes a recursive encoding (once).
    fn recursive_fn(&mut self, name: &str, items: &[BlockItem]) -> BmlResult<()> {
        if self.fns.iter().any(|(n, _)| n == name) {
            return Ok(());
        }
        // Claimed: the body below calls back into this name.
        self.fns.push((name.to_string(), String::new()));
        let body = self.block_expr(items, "    ")?;
        let text = format!(
            "\npub(crate) fn {}_at(\n    cur: &mut Cursor,\n    env: &std::collections::HashMap<String, i64>,\n) -> Result<Value, BmlError> {{\n    Ok({body})\n}}\n",
            snake(name)
        );
        if let Some(slot) = self.fns.iter_mut().find(|(n, _)| n == name) {
            slot.1 = text;
        }
        Ok(())
    }

    fn block_expr(&mut self, items: &[BlockItem], indent: &str) -> BmlResult<String> {
        let body = self.block_stmts(items, &format!("{indent}    "))?;
        Ok(format!(
            "{{\n{indent}    let mut out: serde_json::Map<String, Value> = serde_json::Map::new();\n{indent}    let mut env: std::collections::HashMap<String, i64> = env.clone();\n{body}\n{indent}    Value::Object(out)\n{indent}}}"
        ))
    }

    fn block_stmts(&mut self, items: &[BlockItem], indent: &str) -> BmlResult<String> {
        let mut lines: Vec<String> = Vec::new();
        for item in items {
            match item {
                BlockItem::Require(_) => {}
                BlockItem::Computed { name, expr } => {
                    let e = self.expr(expr)?;
                    lines.push(format!("{indent}env.insert(\"{name}\".into(), {e});"));
                    lines.push(format!(
                        "{indent}out.insert(\"{name}\".into(), json!({e}));"
                    ));
                }
                BlockItem::Group { cond, items } => {
                    let cond = self.expr(required(cond)?)?;
                    let inner = self.block_stmts(items, &format!("{indent}    "))?;
                    lines.push(format!("{indent}if {cond} != 0 {{"));
                    lines.push(inner);
                    lines.push(format!("{indent}}}"));
                }
                BlockItem::MatchStmt(mt) => {
                    lines.push(format!("{indent}match {} {{", self.expr(&mt.tag_expr)?));
                    let mut saw_default = false;
                    for arm in &mt.arms {
                        let Some(pat) = Self::match_pat(arm) else {
                            continue;
                        };
                        if pat == "_" {
                            saw_default = true;
                        }
                        let body = match &arm.type_ref {
                            Some(FieldType::Block(b)) => {
                                self.block_stmts(&b.items, &format!("{indent}        "))?
                            }
                            _ => String::new(),
                        };
                        lines.push(format!("{indent}    {pat} => {{"));
                        if !body.is_empty() {
                            lines.push(body);
                        }
                        lines.push(format!("{indent}    }}"));
                    }
                    if !saw_default {
                        lines.push(format!("{indent}    _ => {{}}"));
                    }
                    lines.push(format!("{indent}}}"));
                }
                BlockItem::Spread { name, .. } => {
                    let enc = self
                        .resolved
                        .encodings
                        .get(name)
                        .ok_or_else(|| emit(format!("KeyError: {}", py_repr_str(name))))?;
                    if let EncBody::Block(b) = &enc.body {
                        lines.push(self.block_stmts(&b.items, indent)?);
                    }
                }
                BlockItem::Field(wf) => lines.push(self.field_stmts(wf, indent)?),
            }
        }
        Ok(lines
            .iter()
            .filter(|x| !x.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn field_stmts(&mut self, wf: &WireField, indent: &str) -> BmlResult<String> {
        if let Some(rep) = &wf.repeat {
            return self.repeat_stmts(wf, rep, indent);
        }
        let value = self.decode_expr(&wf.type_ref, indent)?;
        let is_int =
            matches!(&wf.type_ref, FieldType::Ref(tr) if INT_TYPES.contains(&tr.name.as_str()));
        let mut lines = vec![format!("{indent}let v = {value};")];
        if is_int {
            lines.push(format!(
                "{indent}env.insert(\"{}\".into(), v.as_i64().unwrap_or(0));",
                wf.name
            ));
        }
        lines.push(format!("{indent}out.insert(\"{}\".into(), v);", wf.name));
        let stmt = lines.join("\n");
        match &wf.if_expr {
            Some(c) => {
                let cond = self.expr(c)?;
                Ok(format!("{indent}if {cond} != 0 {{\n{stmt}\n{indent}}}"))
            }
            None => Ok(stmt),
        }
    }

    fn repeat_stmts(&mut self, wf: &WireField, rep: &Repeat, indent: &str) -> BmlResult<String> {
        let elem = self.decode_expr(&wf.type_ref, &format!("{indent}        "))?;
        let mut lines = vec![
            format!("{indent}{{"),
            format!("{indent}    let mut items: Vec<Value> = Vec::new();"),
        ];
        match rep.mode {
            RepeatMode::Count => {
                let n = self.expr(required(&rep.expr)?)?;
                lines.push(format!("{indent}    for _ in 0..({n}) {{"));
                lines.push(format!("{indent}        items.push({elem});"));
                lines.push(format!("{indent}    }}"));
            }
            RepeatMode::UntilEnd => {
                lines.push(format!("{indent}    while cur.remaining() > 0 {{"));
                lines.push(format!("{indent}        items.push({elem});"));
                lines.push(format!("{indent}    }}"));
            }
            RepeatMode::Until => {
                let term = self.expr(required(&rep.expr)?)?;
                lines.push(format!("{indent}    while cur.remaining() > 0 {{"));
                lines.push(format!(
                    "{indent}        if cur.peek_u8()? as i64 == {term} {{"
                ));
                lines.push(format!("{indent}            cur.read(1)?;"));
                lines.push(format!("{indent}            break;"));
                lines.push(format!("{indent}        }}"));
                lines.push(format!("{indent}        items.push({elem});"));
                lines.push(format!("{indent}    }}"));
            }
        }
        lines.push(format!(
            "{indent}    out.insert(\"{}\".into(), json!(items));",
            wf.name
        ));
        lines.push(format!("{indent}}}"));
        Ok(lines.join("\n"))
    }

    // --- packets -----------------------------------------------------------

    fn packet_fn(&mut self, pkt: &PacketDecl) -> BmlResult<String> {
        let body = self.block_stmts(&pkt.items, "    ")?;
        Ok(format!(
            "\npub fn decode_{}(cur: &mut Cursor) -> Result<Value, BmlError> {{\n    let mut out: serde_json::Map<String, Value> = serde_json::Map::new();\n    let mut env: std::collections::HashMap<String, i64> = std::collections::HashMap::new();\n{body}\n    Ok(Value::Object(out))\n}}\n",
            snake(&pkt.name)
        ))
    }
}

/// A path segment: ASCII letters and digits, lowercased, other characters
/// collapsed to a single `_`.
fn file_token(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "item".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Route category for one packet: `{state}_{direction}`, `shared` when the
/// packet is routed more than one way, `unrouted` when it has no route.
fn packet_stem(resolved: &ResolvedSchema, name: &str) -> String {
    let mut keys: Vec<String> = resolved
        .routes
        .iter()
        .filter(|(_, pkt)| pkt.name == name)
        .map(|(route, _)| {
            format!(
                "{}_{}",
                file_token(&route.state),
                file_token(&route.direction)
            )
        })
        .collect();
    keys.sort();
    keys.dedup();
    match keys.as_slice() {
        [] => "unrouted".to_string(),
        [one] => one.clone(),
        _ => "shared".to_string(),
    }
}

fn write_file(path: &Path, text: &str) -> BmlResult<()> {
    std::fs::write(path, text).map_err(|e| emit(e.to_string()))
}

pub fn emit_rust(resolved: &ResolvedSchema, out_dir: &Path) -> BmlResult<()> {
    let lowered: HashSet<String> = PRIMITIVES
        .iter()
        .chain(HELPERS.iter())
        .map(|(k, _)| k.to_string())
        .collect();
    reject_unlowered_scalars(resolved, &lowered, "Rust")?;
    let src = out_dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| emit(e.to_string()))?;

    let proto_id = resolved
        .protocols
        .values()
        .next()
        .map(|p| p.protocol_id)
        .unwrap_or(0);

    let mut walker = Walker::new(resolved);
    let mut by_stem: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pkt in resolved.packets.values() {
        let text = match rest_only(pkt) {
            Some(field) => format!(
                "\npub fn decode_{}(cur: &mut Cursor) -> Result<Value, BmlError> {{\n    Ok(json!({{ \"{field}\": cur.read(cur.remaining())?.to_vec() }}))\n}}\n",
                snake(&pkt.name)
            ),
            None => walker.packet_fn(pkt)?,
        };
        by_stem
            .entry(packet_stem(resolved, &pkt.name))
            .or_default()
            .push(text);
    }
    let helpers: Vec<String> = walker.fns.iter().map(|(_, text)| text.clone()).collect();
    let packets_dir = src.join("packets");
    std::fs::create_dir_all(&packets_dir).map_err(|e| emit(e.to_string()))?;
    let header = "//! Generated packet decoders.\n#![allow(unused_mut, unused_variables, unused_imports, clippy::all)]\n\nuse super::super::runtime::{self, *};\nuse serde_json::{json, Value};\n";
    if helpers.is_empty() {
        let _ = std::fs::remove_file(packets_dir.join("codecs.rs"));
    } else {
        write_file(
            &packets_dir.join("codecs.rs"),
            &format!("{header}\n{}\n", helpers.join("\n")),
        )?;
    }
    let mut mods = String::from("//! Generated packet decoders, one module per route category.\n");
    if !helpers.is_empty() {
        mods.push_str("mod codecs;\n");
    }
    for (stem, bodies) in &by_stem {
        let mut file = String::from(header);
        if !helpers.is_empty() {
            file.push_str("use super::codecs::*;\n");
        }
        file.push('\n');
        file.push_str(&bodies.join("\n"));
        file.push('\n');
        write_file(&packets_dir.join(format!("{stem}.rs")), &file)?;
        mods.push_str(&format!("mod {stem};\npub use {stem}::*;\n"));
    }
    if by_stem.is_empty() {
        mods.push_str("// This schema defines no packets.\n");
    }
    write_file(&src.join("packets.rs"), &mods)?;
    let route_arms = resolved
        .routes
        .iter()
        .map(|(route, pkt)| {
            format!(
                "        ({}, \"{}\", \"{}\") => decode_{}(&mut cur),",
                route.packet_id,
                route.state,
                route.direction,
                snake(&pkt.name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    write_file(&src.join("runtime.rs"), RUNTIME_RS)?;

    write_file(
        &src.join("lib.rs"),
        &format!(
            "pub mod runtime;\nmod packets;\n\nuse packets::*;\nuse runtime::{{BmlError, Cursor, varint}};\n\npub const PROTOCOL_ID: i32 = {proto_id};\n\npub fn decode_packet(\n    state: &str,\n    direction: &str,\n    packet_id: i32,\n    payload: &[u8],\n) -> Result<serde_json::Value, BmlError> {{\n    let mut cur = Cursor::new(payload);\n    match (packet_id, state, direction) {{\n{route_arms}\n        _ => Err(BmlError::Msg(format!(\n            \"no route for id {{}} in {{}}.{{}}\", packet_id, state, direction\n        ))),\n    }}\n}}\n\npub fn decode_frame(\n    state: &str,\n    direction: &str,\n    frame: &[u8],\n) -> Result<(i32, serde_json::Value), BmlError> {{\n    let mut cur = Cursor::new(frame);\n    let _len = varint(&mut cur)?;\n    let pid = varint(&mut cur)? as i32;\n    let rest = cur.read(cur.remaining())?.to_vec();\n    let val = decode_packet(state, direction, pid, &rest)?;\n    Ok((pid, val))\n}}\n"
        ),
    )?;
    write_file(
        &out_dir.join("Cargo.toml"),
        "[package]\nname = \"bml_generated\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde_json = \"1\"\n",
    )?;

    let tests = out_dir.join("tests");
    std::fs::create_dir_all(&tests).map_err(|e| emit(e.to_string()))?;
    write_file(&tests.join("golden.rs"), GOLDEN_RS)?;
    Ok(())
}
