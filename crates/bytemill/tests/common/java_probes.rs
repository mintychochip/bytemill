//! Ports tests/java_probes.py: Java ground truth from rstom's probe fixtures.

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::json::loads;
use bytemill::ResolvedSchema;
use bytemill::Value;
use std::path::PathBuf;

pub fn probes_dir() -> PathBuf {
    super::rstom_tables::rstom_dir().join("probes/expected")
}

pub fn available() -> bool {
    probes_dir().is_dir()
}

const STATE: &[(&str, &str)] =
    &[("HANDSHAKE", "handshaking"), ("STATUS", "status"), ("LOGIN", "login"), ("CONFIGURATION", "configuration"), ("PLAY", "play")];

fn state_name(s: &str) -> &'static str {
    STATE.iter().find(|(k, _)| *k == s).map(|(_, v)| *v).expect("state")
}

fn direction_name(side: &str) -> &'static str {
    // Minestom names a packet by who sends it: a CLIENT packet is serverbound.
    if side == "CLIENT" {
        "serverbound"
    } else {
        "clientbound"
    }
}

const KIND_FIXTURES: &[&str] = &[
    "serverplaybpacketprobe",
    "serverplayspacketprobe",
    "serverplayscoreprobe",
    "serverplayworldprobe",
    "serveradvancementprobe",
];

const KIND_CLASS: &[(&str, &str)] = &[("selectTab", "SelectAdvancementTabPacket")];

const UNREGISTERED: &[(&str, &str)] = &[("LegacyServerListPingPacket", "legacy_server_list_ping")];

#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub state: String,
    pub direction: String,
    pub packet_id: i128,
    pub java_class: String,
}

#[derive(Debug, Clone)]
pub struct Sample {
    pub fixture: String,
    pub index: usize,
    pub java_class: String,
    pub data: Vec<u8>,
    /// The field values Java built the packet from, when recorded.
    pub value: Option<Value>,
}

impl Sample {
    pub fn label(&self) -> String {
        format!("{}[{}]", self.fixture, self.index)
    }
}

fn cases(fixture: &str) -> Value {
    let path = probes_dir().join(format!("{fixture}.json"));
    let text = std::fs::read_to_string(&path).unwrap();
    let v = loads(&text).unwrap();
    v.get("cases").cloned().unwrap_or(Value::Null)
}

fn case_list(fixture: &str) -> Vec<Value> {
    match cases(fixture) {
        Value::List(items) => (*items).clone(),
        _ => Vec::new(),
    }
}

/// Every `(state, direction, id)` Java registers, with the class at that id.
pub fn slots() -> Vec<Slot> {
    let mut out = Vec::new();
    for case in case_list("packetprobe") {
        if case.get("op").and_then(|v| match v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }) != Some("registryTable".to_string())
        {
            continue;
        }
        let state = state_name(case.get("state").and_then(|v| match v {
            Value::Str(s) => Some(s.as_ref()),
            _ => None,
        }).unwrap());
        let direction = direction_name(case.get("side").and_then(|v| match v {
            Value::Str(s) => Some(s.as_ref()),
            _ => None,
        }).unwrap());
        let Value::List(packets) = case.get("packets").cloned().unwrap_or(Value::Null) else {
            continue;
        };
        for (packet_id, java_class) in packets.iter().enumerate() {
            if let Value::Str(c) = java_class {
                out.push(Slot {
                    state: state.to_string(),
                    direction: direction.to_string(),
                    packet_id: packet_id as i128,
                    java_class: c.to_string(),
                });
            }
        }
    }
    out
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn str_field(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// The recorded packet bytes. Cases where Java itself rejected the input are skipped.
pub fn samples() -> Vec<Sample> {
    let mut out = Vec::new();
    for (index, case) in case_list("corepacketsprobe").iter().enumerate() {
        let Some(hex) = str_field(case.get("write")) else { continue };
        let java_class = str_field(case.get("name"))
            .map(|n| n.rsplit('.').next().unwrap().to_string())
            .unwrap_or_default();
        out.push(Sample {
            fixture: "corepacketsprobe".into(),
            index,
            java_class,
            data: hex_bytes(&hex),
            value: None,
        });
    }
    for (index, case) in case_list("playernetworkprobe").iter().enumerate() {
        let op = str_field(case.get("op"));
        if op.as_deref() == Some("clientSettings")
            && let Some(hex) = str_field(case.get("hex"))
        {
            out.push(Sample {
                fixture: "playernetworkprobe".into(),
                index,
                java_class: "ClientSettingsPacket".into(),
                data: hex_bytes(&hex),
                value: case.get("input").cloned().filter(|v| !matches!(v, Value::Null)),
            });
        }
    }
    for fixture in KIND_FIXTURES {
        for (index, case) in case_list(fixture).iter().enumerate() {
            let Some(hex) = str_field(case.get("bytes")) else { continue };
            if case.get("errorClass").is_some() {
                continue;
            }
            let value = case.get("value").cloned();
            let java_class = value
                .as_ref()
                .and_then(|v| str_field(v.get("packetClass")))
                .or_else(|| {
                    let kind = str_field(case.get("kind"))?;
                    KIND_CLASS
                        .iter()
                        .find(|(k, _)| *k == kind)
                        .map(|(_, c)| (*c).to_string())
                        .or_else(|| {
                            let mut chars = kind.chars();
                            let first = chars.next()?.to_uppercase();
                            Some(format!("{first}{}Packet", chars.as_str()))
                        })
                })
                .unwrap_or_default();
            out.push(Sample {
                fixture: (*fixture).into(),
                index,
                java_class,
                data: hex_bytes(&hex),
                value: value.filter(|v| !matches!(v, Value::Null)),
            });
        }
    }
    out
}

pub fn route_packets(resolved: &ResolvedSchema) -> Vec<(String, String, i128, String)> {
    resolved
        .routes
        .iter()
        .map(|(r, pkt)| (r.state.clone(), r.direction.clone(), r.packet_id, pkt.name.clone()))
        .collect()
}

/// The BPS packet(s) at the ids where Java registers each class.
pub fn packets_by_class(resolved: &ResolvedSchema) -> Vec<(String, Vec<String>)> {
    let route = route_packets(resolved);
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for slot in slots() {
        let key = (slot.state.clone(), slot.direction.clone(), slot.packet_id);
        if let Some((_, _, _, packet)) = route.iter().find(|(s, d, id, _)| {
            *s == key.0 && *d == key.1 && *id == key.2
        }) {
            match out.iter_mut().find(|(c, _)| *c == slot.java_class) {
                Some((_, v)) => {
                    if !v.iter().any(|p| p == packet) {
                        v.push(packet.clone());
                    }
                }
                None => out.push((slot.java_class.clone(), vec![packet.clone()])),
            }
        }
    }
    for (java_class, packet) in UNREGISTERED {
        match out.iter_mut().find(|(c, _)| c == java_class) {
            Some((_, v)) => {
                if !v.iter().any(|p| p == packet) {
                    v.push((*packet).to_string());
                }
            }
            None => out.push(((*java_class).to_string(), vec![(*packet).to_string()])),
        }
    }
    for (_, packets) in out.iter_mut() {
        packets.sort();
    }
    out
}

/// Why the schema cannot reproduce `data` for `packet`, or `None` when it does.
pub fn check(resolved: &ResolvedSchema, packet: &str, data: &[u8]) -> Option<String> {
    let value = match decode_packet(resolved, packet, data, true) {
        Ok(v) => v,
        Err(e) => return Some(format!("decode: {e}")),
    };
    match encode_packet(resolved, packet, &value) {
        Err(e) => Some(format!("re-encode: {}: {e}", e.kind.as_str())),
        Ok(again) => {
            if again != data {
                Some(format!("re-encode differs: {}", hex(&again)))
            } else {
                None
            }
        }
    }
}

/// Samples the schema cannot reproduce, by BPS packet.
pub fn failures(resolved: &ResolvedSchema) -> Vec<(String, Vec<(Sample, String)>)> {
    let by_class = packets_by_class(resolved);
    let mut out: Vec<(String, Vec<(Sample, String)>)> = Vec::new();
    for sample in samples() {
        let Some((_, packets)) = by_class.iter().find(|(c, _)| *c == sample.java_class) else {
            continue;
        };
        for packet in packets {
            if let Some(reason) = check(resolved, packet, &sample.data) {
                match out.iter_mut().find(|(p, _)| p == packet) {
                    Some((_, v)) => v.push((sample.clone(), reason)),
                    None => out.push((packet.clone(), vec![(sample.clone(), reason)])),
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
