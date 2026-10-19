//! Minestom classes for BPS packets, porting `bps/minestom.py`.
//!
//! `rstom-packets` names a packet by its Minestom class and files it under the
//! Minestom package; BPS names it by what vanilla calls it (`add_entity`). The
//! two meet at the id tables: the class at position N of a state's table is the
//! class of the route with id N.

use crate::ast::{PacketDecl, ResolvedSchema, RouteDecl};
use crate::errors::{emit, BpsResult};
use crate::json::loads;
use crate::ordmap::OrderedMap;
use crate::value::Value;
use std::path::Path;

/// BPS spells the first state `handshaking`; Minestom's packages say `handshake`.
const PACKAGE_HANDSHAKING: (&str, &str) = ("handshaking", "handshake");

/// Minestom keeps two wire forms on one class and picks the serializer per id
/// table. A BPS packet is one wire form and a Rust struct has one, so such a
/// class is one struct per BPS packet: the packet named here keeps the class.
pub const SHARED_CLASSES: &[(&str, &str)] = &[("ShowDialogPacket", "show_dialog")];

pub const PACKAGE_PREFIX: &str = "net.minestom.server.network.packet";

pub fn package_of(state: &str) -> &str {
    let (bps_state, package) = PACKAGE_HANDSHAKING;
    if state == bps_state {
        package
    } else {
        state
    }
}

/// `SpawnEntityPacket` -> `spawn_entity_packet`, as the generator names the file.
///
/// Python's lookaround regex inserts `_` (a) between a lowercase letter or digit
/// and an uppercase letter, and (b) between an uppercase letter and an
/// `Upper+lower` pair. Both are boundary rules before a character.
pub fn module_stem(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let boundary = (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && c.is_ascii_uppercase()
                || (prev.is_ascii_uppercase()
                    && c.is_ascii_uppercase()
                    && next.is_some_and(|n| n.is_ascii_lowercase()));
            if boundary {
                out.push('_');
            }
        }
        out.extend(c.to_lowercase());
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinestomClass {
    pub name: String,
    /// The fully qualified name, which becomes the struct's `CLASS_NAME`.
    pub java: String,
    pub group: String,
    pub package: String,
    pub stem: String,
}

/// The classes of `packets_gen`'s schema, and the id tables that order them.
#[derive(Debug, Clone)]
pub struct ClassTable {
    classes: OrderedMap<MinestomClass>,
    tables: Vec<((String, String), Vec<String>)>,
}

impl ClassTable {
    pub fn from_schema(schema: &Value) -> BpsResult<ClassTable> {
        let mut classes: OrderedMap<MinestomClass> = OrderedMap::new();
        let packets = schema_map_list(schema, "packets")?;
        for entry in &packets {
            let name = map_str(entry, "name")?.to_string();
            let java = map_str(entry, "java")?.to_string();
            let group = map_str(entry, "group")?.to_string();
            let package = map_str(entry, "state")?.to_string();
            let stem = module_stem(&name);
            if let Some(Value::Str(src)) = entry.get("source") {
                let src_stem = Path::new(src.as_ref())
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if src_stem != stem {
                    return Err(emit(format!(
                        "{name}: module {src_stem}, not the {stem} its name gives"
                    )));
                }
            }
            let cls = MinestomClass { name: name.clone(), java, group, package, stem };
            classes.insert(&name, cls);
        }
        let mut tables = Vec::new();
        let state_tables = schema_map_list(schema, "stateTables")?;
        for t in &state_tables {
            let state = map_str(t, "state")?.to_string();
            let side = map_str(t, "side")?.to_string();
            let names = match t.get("packets") {
                Some(Value::List(items)) => items
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => Ok(s.to_string()),
                        _ => Err(emit("stateTables packets entries must be strings")),
                    })
                    .collect::<BpsResult<Vec<_>>>()?,
                _ => Vec::new(),
            };
            for name in &names {
                if !classes.contains_key(name) {
                    return Err(emit(format!(
                        "{state} {side} table lists {name}, which the schema does not declare"
                    )));
                }
            }
            tables.push(((state, side), names));
        }
        Ok(ClassTable { classes, tables })
    }

    pub fn load(path: &Path) -> BpsResult<ClassTable> {
        let text = crate::text::read_text(path)
            .map_err(|e| emit(format!("cannot read {}: {e}", path.display())))?;
        let schema = loads(&text)?;
        ClassTable::from_schema(&schema)
    }

    /// The class Minestom registers at an id, or `None` past that table's end.
    pub fn at(&self, state: &str, direction: &str, packet_id: i128) -> Option<&MinestomClass> {
        let side = if direction == "serverbound" { "CLIENT" } else { "SERVER" };
        let key = (package_of(state).to_string(), side.to_string());
        let table = self
            .tables
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[]);
        if packet_id < 0 || packet_id as usize >= table.len() {
            return None;
        }
        self.classes.get(&table[packet_id as usize])
    }

    pub fn named(&self, name: &str) -> Option<&MinestomClass> {
        self.classes.get(name)
    }
}

fn schema_map_list<'a>(schema: &'a Value, key: &str) -> BpsResult<Vec<&'a Value>> {
    match schema.get(key) {
        Some(Value::List(items)) => Ok(items.iter().collect()),
        Some(Value::Null) | None => Ok(Vec::new()),
        _ => Err(emit(format!("packets.json: {key} must be a list"))),
    }
}

fn map_str<'a>(map: &'a Value, key: &str) -> BpsResult<&'a str> {
    match map.get(key) {
        Some(Value::Str(s)) => Ok(s),
        _ => Err(emit(format!("packets.json entry needs a string {key:?}"))),
    }
}

/// One generated struct: a BPS packet as the Minestom class of one direction.
#[derive(Debug, Clone)]
pub struct Unit {
    pub pkt: PacketDecl,
    pub direction: String,
    pub cls: MinestomClass,
    pub routes: Vec<RouteDecl>,
}

/// `add_entity` -> `AddEntityPacket`: split on non-alphanumerics, upper-case the
/// first character of each part, append `Packet`.
pub fn class_name(packet: &str) -> String {
    let mut out = String::new();
    for part in packet.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out.push_str("Packet");
    out
}

/// The class an extra wire form of a shared class becomes: same name, its own
/// state's package.
fn extra_form(cls: &MinestomClass, packet: &str, routes: &[RouteDecl]) -> BpsResult<MinestomClass> {
    let mut states: Vec<String> =
        routes.iter().map(|r| package_of(&r.state).to_string()).collect();
    states.sort();
    states.dedup();
    if states.len() != 1 {
        return Err(emit(format!(
            "{packet:?} shares {} but routes in [{}], not one state",
            cls.name,
            states
                .iter()
                .map(|s| format!("'{s}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let package = states.into_iter().next().unwrap();
    Ok(MinestomClass {
        java: format!("{PACKAGE_PREFIX}.{}.{}.{}", cls.group, package, cls.name),
        package,
        ..cls.clone()
    })
}

/// A struct for each direction each packet travels, ordered by module path.
pub fn build_units(resolved: &ResolvedSchema, classes: &ClassTable) -> BpsResult<Vec<Unit>> {
    // Insertion-ordered, like the Python dicts that back these tables.
    let mut found: Vec<((String, String), MinestomClass)> = Vec::new();
    let mut routed: Vec<((String, String), Vec<RouteDecl>)> = Vec::new();

    for (route, pkt) in &resolved.routes {
        // A route past its table's end is one Minestom does not register (the
        // pre-1.7 ping); the class can only be found by the packet's own name.
        let cls = classes
            .at(&route.state, &route.direction, route.packet_id)
            .or_else(|| classes.named(&class_name(&pkt.name)))
            .ok_or_else(|| {
                emit(format!(
                    "route {}.{} {:#04x} => {}: no Minestom class at that id, and none named {}",
                    route.state,
                    route.direction,
                    route.packet_id,
                    pkt.name,
                    class_name(&pkt.name)
                ))
            })?
            .clone();
        let key = (pkt.name.clone(), route.direction.clone());
        match found.iter_mut().find(|(k, _)| *k == key) {
            Some((_, existing)) => {
                if *existing != cls {
                    return Err(emit(format!(
                        "packet {:?} is routed {} to two Minestom classes: {}, {}",
                        pkt.name, route.direction, existing.name, cls.name
                    )));
                }
            }
            None => found.push((key.clone(), cls)),
        }
        match routed.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(route.clone()),
            None => routed.push((key, vec![route.clone()])),
        }
    }

    for name in resolved.packets.keys() {
        if !found.iter().any(|((n, _), _)| n == name) {
            return Err(emit(format!("packet {} has no route, so no Minestom class", pyq(name))));
        }
    }

    // Which packets claim each (group, class).
    let mut claims: Vec<((String, String), Vec<String>)> = Vec::new();
    for ((name, _), cls) in &found {
        let key = (cls.group.clone(), cls.name.clone());
        match claims.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => {
                if !v.contains(name) {
                    v.push(name.clone());
                }
            }
            None => claims.push((key, vec![name.clone()])),
        }
    }
    for ((_, class_name), packets) in &claims {
        if packets.len() > 1 && !SHARED_CLASSES.iter().any(|(c, k)| c == class_name && packets.iter().any(|p| p == k)) {
            let joined = packets
                .iter()
                .map(|p| pyq(p))
                .collect::<Vec<_>>()
                .join(" and ");
            return Err(emit(format!(
                "class {class_name} is claimed by {joined}; name the one that keeps it in SHARED_CLASSES"
            )));
        }
    }

    let mut units = Vec::new();
    for ((name, direction), cls) in &found {
        let routes = routed
            .iter()
            .find(|(k, _)| k == &(name.clone(), direction.clone()))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let claim_count = claims
            .iter()
            .find(|((g, c), _)| g == &cls.group && c == &cls.name)
            .map(|(_, v)| v.len())
            .unwrap_or(0);
        let cls = if claim_count > 1
            && !SHARED_CLASSES.iter().any(|(c, k)| c == &cls.name && k == name)
        {
            extra_form(cls, name, &routes)?
        } else {
            cls.clone()
        };
        let pkt = resolved.packets.get(name).cloned().ok_or_else(|| {
            emit(format!("packet {} has no declaration", pyq(name)))
        })?;
        units.push(Unit { pkt, direction: direction.clone(), cls, routes });
    }
    units.sort_by(|a, b| {
        (a.cls.group.as_str(), a.cls.package.as_str(), a.cls.stem.as_str())
            .cmp(&(b.cls.group.as_str(), b.cls.package.as_str(), b.cls.stem.as_str()))
    });
    for pair in units.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if (a.cls.group.as_str(), a.cls.package.as_str(), a.cls.stem.as_str())
            == (b.cls.group.as_str(), b.cls.package.as_str(), b.cls.stem.as_str())
        {
            return Err(emit(format!(
                "{} and {} are both {}",
                pyq(&a.pkt.name),
                pyq(&b.pkt.name),
                a.cls.java
            )));
        }
    }
    Ok(units)
}

/// Python `repr` of the bare string, for error text.
fn pyq(s: &str) -> String {
    crate::errors::py_repr_str(s)
}
