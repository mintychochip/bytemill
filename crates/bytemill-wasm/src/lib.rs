//! WebAssembly bridge for Bytemill.
//!
//! The browser sends one JSON request per call. Requests are framed in linear
//! memory: `bml_alloc` hands out a buffer, `bml_run` returns a buffer whose
//! first four bytes are the little-endian response length. The JSON protocol
//! is documented in `site/src/bytemill/wasm.ts` on the mintychochip.dev side.
//!
//! The bridge deliberately exposes the interpreter, not the file-writing
//! emitters: the page is a schema/decoder playground, so every operation is
//! pure and needs no filesystem.

use std::slice;

use bytemill::ast::{BlockItem, FieldType, MatchArm, Schema};
use bytemill::errors::{BmlError, resolve};
use bytemill::interp::{decode_frame_payload, decode_packet, encode_packet};
use bytemill::json::{dumps, loads};
use bytemill::ordmap::OrderedMap;
use bytemill::value::Value;
use bytemill::{BmlResult, ResolvedSchema, analyze, parse};

// ---------------------------------------------------------------------------
// Memory ABI
// ---------------------------------------------------------------------------

/// Allocate a request buffer of exactly `len` zeroed bytes.
#[unsafe(no_mangle)]
pub extern "C" fn bml_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    let mut buffer = vec![0u8; len].into_boxed_slice();
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// Free a buffer previously returned by [`bml_alloc`].
///
/// # Safety
///
/// `ptr` must come from `bml_alloc(len)` and not be used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bml_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

/// Run one JSON request and return a length-prefixed JSON response.
///
/// # Safety
///
/// `ptr` must point to `len` readable bytes (normally from `bml_alloc`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bml_run(ptr: *const u8, len: usize) -> *mut u8 {
    let response = if ptr.is_null() || len == 0 {
        error_json(BmlError::parse("empty request"))
    } else {
        let request = unsafe { slice::from_raw_parts(ptr, len) };
        handle(request)
    };

    let mut framed = Vec::with_capacity(4 + response.len());
    framed.extend_from_slice(&(response.len() as u32).to_le_bytes());
    framed.extend_from_slice(response.as_bytes());
    let buffer = framed.into_boxed_slice();
    let out = buffer.as_ptr() as *mut u8;
    std::mem::forget(buffer);
    out
}

/// Read the payload length from a buffer returned by [`bml_run`].
///
/// # Safety
///
/// `ptr` must be a live buffer returned by `bml_run`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bml_response_len(ptr: *const u8) -> usize {
    if ptr.is_null() {
        return 0;
    }
    let mut prefix = [0u8; 4];
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, prefix.as_mut_ptr(), 4);
    }
    u32::from_le_bytes(prefix) as usize
}

/// Free a buffer returned by [`bml_run`].
///
/// # Safety
///
/// `ptr` must be a live buffer returned by `bml_run`, freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bml_free_response(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let len = unsafe { bml_response_len(ptr) };
    let total = len + 4;
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            ptr, total,
        )));
    }
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

fn handle(request: &[u8]) -> String {
    let text = match std::str::from_utf8(request) {
        Ok(text) => text,
        Err(_) => return error_json(BmlError::parse("request is not UTF-8")),
    };
    let request = match loads(text) {
        Ok(value) => value,
        Err(err) => return error_json(err),
    };
    let Some(op) = string_field(&request, "op") else {
        return error_json(BmlError::parse("request needs an \"op\" string"));
    };

    match op {
        "version" => ok_json(map(vec![
            ("engine", Value::str("bytemill")),
            ("version", Value::str(env!("CARGO_PKG_VERSION"))),
        ])),
        "check" => match check(&request) {
            Ok(data) => ok_json(data),
            Err(err) => error_json(err),
        },
        "decode" => match decode(&request) {
            Ok(data) => ok_json(data),
            Err(err) => error_json(err),
        },
        "encode" => match encode(&request) {
            Ok(data) => ok_json(data),
            Err(err) => error_json(err),
        },
        "stages" => match stages(&request) {
            Ok(data) => ok_json(data),
            Err(err) => error_json(err),
        },
        "targets" => ok_json(bytemill::compat::targets_json()),
        other => error_json(BmlError::parse(format!(
            "unknown op {other:?}; expected check, decode, encode, stages, targets, or version"
        ))),
    }
}

fn check(request: &Value) -> BmlResult<Value> {
    let resolved = resolve_source(request)?;
    let packets = resolved
        .packets
        .values()
        .map(|packet| {
            let mut fields = Vec::new();
            collect_fields(&packet.items, &mut fields);
            map(vec![
                ("name", Value::str(packet.name.clone())),
                ("fields", strings(fields)),
            ])
        })
        .collect();

    let encodings = resolved
        .encodings
        .values()
        .map(|encoding| Value::str(encoding.name.clone()))
        .collect();
    let enums = resolved
        .enums
        .values()
        .map(|decl| {
            map(vec![
                ("name", Value::str(decl.name.clone())),
                (
                    "members",
                    strings(decl.members.iter().map(|m| m.name.clone()).collect()),
                ),
            ])
        })
        .collect();

    let protocols = resolved
        .protocols
        .values()
        .map(|protocol| {
            map(vec![
                ("name", Value::str(protocol.name.clone())),
                ("version", Value::str(protocol.version.clone())),
                ("id", Value::Int(protocol.protocol_id)),
                (
                    "frames",
                    strings(protocol.frames.iter().map(|f| f.name.clone()).collect()),
                ),
            ])
        })
        .collect();

    let routes = resolved
        .routes
        .iter()
        .map(|(route, packet)| {
            map(vec![
                ("state", Value::str(route.state.clone())),
                ("direction", Value::str(route.direction.clone())),
                ("id", Value::Int(route.packet_id)),
                ("packet", Value::str(packet.name.clone())),
            ])
        })
        .collect();

    Ok(map(vec![
        ("packets", Value::list(packets)),
        ("encodings", Value::list(encodings)),
        ("enums", Value::list(enums)),
        ("protocols", Value::list(protocols)),
        ("routes", Value::list(routes)),
    ]))
}

fn decode(request: &Value) -> BmlResult<Value> {
    let resolved = resolve_source(request)?;
    let input = required_string(request, "hex")?;
    let bytes = decode_hex(input)?;
    let packet = string_field(request, "packet");

    if let Some(packet) = packet {
        let env = decode_packet(&resolved, packet, &bytes, true)?;
        let reencoded = encode_packet(&resolved, packet, &env)?;
        return Ok(map(vec![
            ("packet", Value::str(packet)),
            ("fields", Value::map(env)),
            ("bytes", Value::Int(bytes.len() as i128)),
            ("hex", Value::str(hex(&bytes))),
            ("reencoded", Value::str(hex(&reencoded))),
            ("roundtrip", Value::Bool(reencoded == bytes)),
        ]));
    }

    let state = required_string(request, "state")?;
    let direction = required_string(request, "direction")?;
    let (id, env) = decode_frame_payload(&resolved, state, direction, &bytes)?;
    let packet_name = match env.get("packet") {
        Some(Value::Str(name)) => name.to_string(),
        _ => String::new(),
    };
    let fields = match env.get("fields") {
        Some(Value::Map(fields)) => fields.clone(),
        _ => std::rc::Rc::new(OrderedMap::new()),
    };
    Ok(map(vec![
        ("id", Value::Int(id)),
        ("packet", Value::str(packet_name)),
        ("fields", Value::Map(fields)),
        ("bytes", Value::Int(bytes.len() as i128)),
        ("hex", Value::str(hex(&bytes))),
    ]))
}

fn encode(request: &Value) -> BmlResult<Value> {
    let resolved = resolve_source(request)?;
    let packet = required_string(request, "packet")?;
    let value = request
        .get("value")
        .ok_or_else(|| BmlError::encode("request needs a \"value\" object"))?;
    let Value::Map(fields) = value else {
        return Err(BmlError::encode("encode needs a JSON object"));
    };

    let bytes = encode_packet(&resolved, packet, fields)?;
    let decoded = decode_packet(&resolved, packet, &bytes, true)?;
    let reencoded = encode_packet(&resolved, packet, &decoded)?;
    Ok(map(vec![
        ("packet", Value::str(packet)),
        ("hex", Value::str(hex(&bytes))),
        ("bytes", Value::Int(bytes.len() as i128)),
        ("fields", Value::map(decoded)),
        ("roundtrip", Value::Bool(reencoded == bytes)),
    ]))
}

fn parse_source(request: &Value) -> BmlResult<Schema> {
    let source = required_string(request, "source")?;
    if source.trim().is_empty() {
        return Err(BmlError::parse("schema source is empty"));
    }
    let schema = parse(source)?;
    if !schema.imports.is_empty() {
        return Err(resolve(
            "imports are not available in the browser playground; paste a self-contained schema",
        ));
    }
    Ok(schema)
}

fn resolve_source(request: &Value) -> BmlResult<ResolvedSchema> {
    let schema = parse_source(request)?;
    analyze(&schema)
}

fn stages(request: &Value) -> BmlResult<Value> {
    let schema = parse_source(request)?;
    let report = bytemill::ir::stages(&schema)?;
    Ok(bytemill::ir::stages_json(&report))
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn map(pairs: Vec<(&str, Value)>) -> Value {
    let mut out = OrderedMap::new();
    for (key, value) in pairs {
        out.insert(key, value);
    }
    Value::map(out)
}

fn strings(items: Vec<String>) -> Value {
    Value::list(items.into_iter().map(Value::str).collect())
}

fn ok_json(data: Value) -> String {
    dumps(&map(vec![("ok", Value::Bool(true)), ("data", data)]))
}

fn error_json(error: BmlError) -> String {
    dumps(&map(vec![
        ("ok", Value::Bool(false)),
        (
            "error",
            map(vec![
                ("kind", Value::str(error.kind.as_str())),
                (
                    "offset",
                    match error.offset {
                        Some(offset) => Value::Int(offset as i128),
                        None => Value::Null,
                    },
                ),
                ("message", Value::str(error.message)),
            ]),
        ),
    ]))
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    match value.get(key) {
        Some(Value::Str(text)) => Some(text),
        _ => None,
    }
}

fn required_string<'a>(value: &'a Value, key: &str) -> BmlResult<&'a str> {
    string_field(value, key)
        .ok_or_else(|| BmlError::parse(format!("request needs a {key:?} string")))
}

fn collect_fields(items: &[BlockItem], out: &mut Vec<String>) {
    for item in items {
        match item {
            BlockItem::Field(field) => push_unique(out, &field.name),
            BlockItem::Group { items, .. } => collect_fields(items, out),
            BlockItem::MatchStmt(match_type) => {
                for arm in &match_type.arms {
                    collect_arm(arm, out);
                }
            }
            BlockItem::Computed { .. } | BlockItem::Require(_) | BlockItem::Spread { .. } => {}
        }
    }
}

fn collect_arm(arm: &MatchArm, out: &mut Vec<String>) {
    match &arm.type_ref {
        Some(FieldType::Block(block)) => collect_fields(&block.items, out),
        Some(FieldType::Match(match_type)) => {
            for arm in &match_type.arms {
                collect_arm(arm, out);
            }
        }
        Some(FieldType::Ref(_)) | None => {}
    }
}

fn push_unique(out: &mut Vec<String>, name: &str) {
    if !out.iter().any(|item| item == name) {
        out.push(name.to_string());
    }
}

// ---------------------------------------------------------------------------
// Hex
// ---------------------------------------------------------------------------

fn decode_hex(text: &str) -> BmlResult<Vec<u8>> {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_whitespace() || c == ',' || c == '_' || c == ':' || c == '-' {
            continue;
        }
        if c == '0' && matches!(chars.peek(), Some('x' | 'X')) {
            chars.next();
            continue;
        }
        clean.push(c);
    }
    if !clean.len().is_multiple_of(2) {
        return Err(BmlError::decode("hex needs an even number of digits"));
    }
    let mut out = Vec::with_capacity(clean.len() / 2);
    let bytes = clean.as_bytes();
    for pair in bytes.as_chunks::<2>().0 {
        let high = hex_digit(pair[0])?;
        let low = hex_digit(pair[1])?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> BmlResult<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(BmlError::decode(format!(
            "invalid hex digit {:?}",
            other as char
        ))),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}
