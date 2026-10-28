//! Wire-vector generation: valid payloads per packet, checked against the reference interpreter.
//!
//! [`generate`] draws payloads from the schema ([`crate::wiregen`]) and keeps one only when the
//! reference interpreter decodes it strictly and re-encodes the same bytes. A packet that yields
//! nothing — the generator cannot draw it, or every candidate fails the round trip — is listed in
//! [`VectorSet::skipped`] instead of aborting the run.

use std::collections::HashSet;

use crate::ast::ResolvedSchema;
use crate::errors::{self, BmlResult};
use crate::json::dumps_sorted;
use crate::ordmap::OrderedMap;
use crate::value::Value;
use crate::wire;
use crate::wiregen;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorSpec {
    pub per_packet: usize,
    pub seed: u64,
}

impl Default for VectorSpec {
    fn default() -> Self {
        Self {
            per_packet: 3,
            seed: 1,
        }
    }
}

/// The packet's first route, in `resolved.routes` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorRoute {
    pub state: String,
    pub direction: String,
    pub id: i128,
}

/// One payload that round-tripped. `hex` is the lowercase payload after the packet id; empty
/// when the packet has no fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vector {
    pub packet: String,
    pub route: Option<VectorRoute>,
    pub hex: String,
}

impl Vector {
    /// The payload as bytes. `hex` is a public field, so it is validated here.
    pub fn bytes(&self) -> BmlResult<Vec<u8>> {
        let bad = || {
            crate::errors::emit(format!(
                "vector for packet {}: {:?} is not hex bytes",
                self.packet, self.hex
            ))
        };
        let nibble = |b: u8| char::from(b).to_digit(16).map(|d| d as u8);
        self.hex
            .as_bytes()
            .chunks(2)
            .map(|pair| match pair {
                [hi, lo] => Some((nibble(*hi)? << 4) | nibble(*lo)?),
                _ => None,
            })
            .map(|byte| byte.ok_or_else(bad))
            .collect()
    }
}

/// A packet that produced no surviving payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub packet: String,
    pub reason: String,
}

/// Vectors that round-tripped, and the packets that produced none.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VectorSet {
    pub vectors: Vec<Vector>,
    pub skipped: Vec<Skipped>,
}

/// A vector for every packet, in schema declaration order.
pub fn generate(resolved: &ResolvedSchema, spec: &VectorSpec) -> BmlResult<VectorSet> {
    let names: Vec<String> = resolved
        .schema
        .packets
        .iter()
        .map(|packet| packet.name.clone())
        .collect();
    generate_for(resolved, &names, spec)
}

/// Vectors for `packets`, in the order given. An unknown name is a resolve error that names it.
///
/// Each packet draws from `spec.seed`, `spec.seed + 1`, … on its own, so a subset matches what
/// [`generate`] would have emitted for those packets.
pub fn generate_for(
    resolved: &ResolvedSchema,
    packets: &[String],
    spec: &VectorSpec,
) -> BmlResult<VectorSet> {
    for name in packets {
        if resolved.packets.get(name.as_str()).is_none() {
            return Err(errors::resolve(format!(
                "unknown packet {}",
                errors::py_repr_str(name)
            )));
        }
    }
    let mut set = VectorSet::default();
    if spec.per_packet == 0 {
        return Ok(set);
    }
    for name in packets {
        fill(resolved, name, spec, &mut set);
    }
    Ok(set)
}

/// One JSON object per vector. Keys are sorted; route keys are omitted when the packet has no
/// route. Every line, including the last, ends with `\n`.
pub fn to_jsonl(vectors: &[Vector]) -> String {
    let mut out = String::new();
    for vector in vectors {
        let mut obj = OrderedMap::new();
        obj.insert("hex", Value::str(vector.hex.as_str()));
        obj.insert("packet", Value::str(vector.packet.as_str()));
        if let Some(route) = &vector.route {
            obj.insert("direction", Value::str(route.direction.as_str()));
            obj.insert("id", Value::Int(route.id));
            obj.insert("state", Value::str(route.state.as_str()));
        }
        out.push_str(&dumps_sorted(&Value::map(obj)));
        out.push('\n');
    }
    out
}

/// Up to `per_packet` distinct payloads. Attempts are `seed`, `seed + 1`, … and stop at eight
/// times `per_packet`. A draw is kept only when its hex is new for this packet and the reference
/// interpreter round-trips it. The first failure's text is the skip reason when none survive.
fn fill(resolved: &ResolvedSchema, name: &str, spec: &VectorSpec, set: &mut VectorSet) {
    let attempts = spec.per_packet.saturating_mul(8);
    let mut kept = Vec::with_capacity(spec.per_packet);
    let mut seen = HashSet::with_capacity(attempts);
    let mut reason = None;
    for i in 0..attempts {
        if kept.len() == spec.per_packet {
            break;
        }
        let seed = spec.seed.wrapping_add(i as u64);
        let bytes = match wiregen::generate(resolved, name, seed) {
            Ok(bytes) => bytes,
            Err(err) => {
                reason.get_or_insert(err);
                continue;
            }
        };
        let hex = hex_of(&bytes);
        if seen.contains(&hex) {
            continue;
        }
        match wire::round_trip(resolved, name, &bytes) {
            Ok(_) => {
                seen.insert(hex.clone());
                kept.push(hex);
            }
            Err(err) => {
                seen.insert(hex);
                reason.get_or_insert(err);
            }
        }
    }
    if kept.is_empty() {
        set.skipped.push(Skipped {
            packet: name.to_string(),
            reason: reason.unwrap_or_else(|| "no candidate round-tripped".to_string()),
        });
        return;
    }
    let route = route_of(resolved, name);
    for hex in kept {
        set.vectors.push(Vector {
            packet: name.to_string(),
            route: route.clone(),
            hex,
        });
    }
}

fn route_of(resolved: &ResolvedSchema, packet: &str) -> Option<VectorRoute> {
    resolved
        .routes
        .iter()
        .find(|(_, pkt)| pkt.name == packet)
        .map(|(route, _)| VectorRoute {
            state: route.state.clone(),
            direction: route.direction.clone(),
            id: route.packet_id,
        })
}

fn hex_of(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    out
}

/// How to run the round-trip test a printer wrote next to its package: the test file's path
/// relative to the output directory, the shell command that runs it from that directory, and how
/// many vectors it embeds (vectors for packets the printer has no codec for are left out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRun {
    pub file: String,
    pub command: String,
    pub vectors: usize,
}
