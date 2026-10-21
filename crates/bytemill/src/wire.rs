//! Wire vectors: packet payloads as hex, checked in bulk against a schema.
//!
//! A vector is one JSON object per line, `{"state", "direction", "id", "hex"}` (`hex` is the
//! payload after the packet id; `direction` is `clientbound`/`serverbound` or ProtoDef's
//! `toClient`/`toServer`). The check is the round trip that proves the schema describes those
//! bytes: the packet decodes strictly (every byte consumed) and encoding the decoded value
//! gives the same bytes back.

use crate::ast::ResolvedSchema;
use crate::errors::BmlResult;
use crate::interp::{decode_packet, encode_packet};
use crate::json::{dumps_sorted, loads};
use crate::value::{Env, Value};

/// One vector that did not round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub line: usize,
    pub packet: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct WireReport {
    pub checked: usize,
    pub failures: Vec<Failure>,
    /// With `collect`: `{"line": n, "value": <decoded>}` for each vector that round-tripped.
    pub decoded: Vec<String>,
}

fn field<'a>(v: &'a Value, key: &str, line: usize) -> Result<&'a Value, String> {
    v.get(key)
        .ok_or_else(|| format!("line {line}: vector has no {key:?}"))
}

fn text<'a>(v: &'a Value, key: &str, line: usize) -> Result<&'a str, String> {
    match field(v, key, line)? {
        Value::Str(s) => Ok(s),
        _ => Err(format!("line {line}: {key:?} is not a string")),
    }
}

fn from_hex(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err("odd number of hex digits".to_string());
    }
    let digit = |c: u8| -> Result<u8, String> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err(format!("{:?} is not a hex digit", c as char)),
        }
    };
    hex.as_bytes()
        .chunks(2)
        .map(|p| Ok(digit(p[0])? << 4 | digit(p[1])?))
        .collect()
}

fn first_difference(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!(
            "first difference at byte {i} (0x{:02x} vs 0x{:02x})",
            a[i], b[i]
        ),
        None => format!("lengths {} and {}", a.len(), b.len()),
    }
}

/// Check every vector in `jsonl` against `resolved`. A malformed vector is an error for the whole
/// run; a vector the schema cannot round trip is recorded in the report.
pub fn check_vectors(
    resolved: &ResolvedSchema,
    jsonl: &str,
    collect: bool,
) -> Result<WireReport, String> {
    let mut report = WireReport::default();
    for (i, raw) in jsonl.lines().enumerate() {
        let line = i + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let v = loads(raw).map_err(|e| format!("line {line}: {e}"))?;
        let state = text(&v, "state", line)?;
        let direction = match text(&v, "direction", line)? {
            "toClient" | "clientbound" => "clientbound",
            "toServer" | "serverbound" => "serverbound",
            other => return Err(format!("line {line}: direction {other:?}")),
        };
        let id = match field(&v, "id", line)? {
            Value::Int(n) => *n,
            _ => return Err(format!("line {line}: \"id\" is not an integer")),
        };
        let bytes = from_hex(text(&v, "hex", line)?).map_err(|e| format!("line {line}: {e}"))?;
        let Some((_, pkt)) = resolved
            .routes
            .iter()
            .find(|(r, _)| r.state == state && r.direction == direction && r.packet_id == id)
        else {
            report.failures.push(Failure {
                line,
                packet: format!("{state}.{direction} 0x{id:02x}"),
                reason: "no route".to_string(),
            });
            continue;
        };
        report.checked += 1;
        match round_trip(resolved, &pkt.name, &bytes) {
            Ok(decoded) => {
                if collect {
                    report.decoded.push(format!(
                        "{{\"line\":{line},\"value\":{}}}",
                        dumps_sorted(&Value::map(decoded))
                    ));
                }
            }
            Err(reason) => report.failures.push(Failure {
                line,
                packet: pkt.name.clone(),
                reason,
            }),
        }
    }
    Ok(report)
}

fn step<T>(what: &str, r: BmlResult<T>) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}

fn round_trip(resolved: &ResolvedSchema, packet: &str, bytes: &[u8]) -> Result<Env, String> {
    let decoded = step("decode", decode_packet(resolved, packet, bytes, true))?;
    let again: Vec<u8> = step("encode", encode_packet(resolved, packet, &decoded))?;
    if again == bytes {
        Ok(decoded)
    } else {
        Err(format!(
            "re-encode differs: {}",
            first_difference(bytes, &again)
        ))
    }
}
