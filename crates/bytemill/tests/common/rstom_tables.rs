//! Ports tests/rstom_tables.py: the Minestom class table the emitter tests need.

use bytemill::minestom::ClassTable;
use bytemill::ordmap::OrderedMap;
use bytemill::value::Value;
use std::path::{Path, PathBuf};

/// `BPS_RSTOM_DIR`, else `<repo>/../rstom-bps`, else `<repo>/../rstom`.
pub fn rstom_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BPS_RSTOM_DIR") {
        return PathBuf::from(dir);
    }
    let worktree = super::repo_root().parent().unwrap().join("rstom-bps");
    if worktree.is_dir() {
        return worktree;
    }
    super::repo_root().parent().unwrap().join("rstom")
}

pub fn packets_gen_schema() -> PathBuf {
    rstom_dir().join("tools/packets_gen/schema/packets.json")
}

/// The table `rstom-packets` is generated from; `None` without rstom.
pub fn real_table() -> Option<ClassTable> {
    let path = packets_gen_schema();
    if !path.is_file() {
        return None;
    }
    Some(ClassTable::load(&path).unwrap())
}

fn class_entry(group: &str, package: &str, name: &str) -> Value {
    let mut m = OrderedMap::new();
    m.insert("name", Value::str(name));
    m.insert(
        "java",
        Value::str(format!(
            "net.minestom.server.network.packet.{group}.{package}.{name}"
        )),
    );
    m.insert("dir", Value::str(format!("{group}/{package}")));
    m.insert("group", Value::str(group));
    m.insert("state", Value::str(package));
    m.insert("fields", Value::list(vec![]));
    Value::map(m)
}

/// A table of `(state, side, [class names by id])`, every class a `server/play` one.
pub fn small_table(tables: &[(&str, &str, &[&str])]) -> ClassTable {
    let mut names: Vec<String> = Vec::new();
    for (_, _, packets) in tables {
        for n in *packets {
            if !names.iter().any(|x| x == n) {
                names.push((*n).to_string());
            }
        }
    }
    let packets: Vec<Value> =
        names.iter().map(|n| class_entry("server", "play", n)).collect();
    let state_tables: Vec<Value> = tables
        .iter()
        .map(|(state, side, ids)| {
            let mut m = OrderedMap::new();
            m.insert("state", Value::str(*state));
            m.insert("side", Value::str(*side));
            m.insert(
                "packets",
                Value::list(ids.iter().map(|n| Value::str(*n)).collect()),
            );
            Value::map(m)
        })
        .collect();
    let mut root = OrderedMap::new();
    root.insert("packets", Value::list(packets));
    root.insert("stateTables", Value::list(state_tables));
    ClassTable::from_schema(&Value::map(root)).unwrap()
}

/// The smallest protocol the emitter accepts: the seven packets that move a
/// connection between states, and one more to test with.
const PROTOCOL: &[(&str, &str, &str, &str, &str)] = &[
    ("handshake", "handshaking.serverbound 0x00", "client", "handshake", "ClientHandshakePacket"),
    ("login_acknowledged", "login.serverbound 0x00", "client", "login", "ClientLoginAcknowledgedPacket"),
    ("configuration_acknowledged", "play.serverbound 0x00", "client", "play", "ClientConfigurationAckPacket"),
    ("finish_configuration", "configuration.serverbound 0x00", "client", "configuration", "ClientFinishConfigurationPacket"),
    ("finish_configuration", "configuration.clientbound 0x00", "server", "configuration", "FinishConfigurationPacket"),
    ("login_finished", "login.clientbound 0x00", "server", "login", "LoginSuccessPacket"),
    ("start_configuration", "play.clientbound 0x00", "server", "play", "StartConfigurationPacket"),
    ("probe", "play.clientbound 0x01", "server", "play", "ProbePacket"),
];

fn table_data() -> Value {
    // (state, side) -> names, in first-seen order.
    let mut tables: Vec<((String, String), Vec<String>)> = Vec::new();
    for (_, route, group, _, name) in PROTOCOL {
        let state = route.split('.').next().unwrap().replace("handshaking", "handshake");
        let side = if *group == "client" { "CLIENT" } else { "SERVER" }.to_string();
        match tables.iter_mut().find(|((s, sd), _)| *s == state && *sd == side) {
            Some((_, v)) => v.push((*name).to_string()),
            None => tables.push(((state, side), vec![(*name).to_string()])),
        }
    }
    let state_tables: Vec<Value> = tables
        .into_iter()
        .map(|((state, side), names)| {
            let mut m = OrderedMap::new();
            m.insert("state", Value::str(state));
            m.insert("side", Value::str(side));
            m.insert("packets", Value::list(names.into_iter().map(Value::str).collect()));
            Value::map(m)
        })
        .collect();
    let packets: Vec<Value> = PROTOCOL
        .iter()
        .map(|(_, _, group, package, name)| class_entry(group, package, name))
        .collect();
    let mut root = OrderedMap::new();
    root.insert("packets", Value::list(packets));
    root.insert("stateTables", Value::list(state_tables));
    Value::map(root)
}

pub fn probe_schema_json() -> String {
    bytemill::json::dumps_sorted(&table_data())
}

pub fn probe_table() -> ClassTable {
    ClassTable::from_schema(&table_data()).unwrap()
}

/// A minimal protocol whose `probe` packet is `body`, importing the std scalars.
pub fn probe_source(root: &Path, body: &str) -> String {
    probe_source_inner(root, body)
}

/// `probe_source(root)` with the default probe body, like the Python default.
pub fn probe_source_default(root: &Path) -> String {
    probe_source_inner(root, "packet probe { a: u8 }")
}

fn probe_source_inner(root: &Path, body: &str) -> String {
    let std: String = ["integers", "varint"]
        .iter()
        .map(|n| format!("import \"{}\"\n", root.join("schemas/std").join(format!("{n}.bps")).display()))
        .collect();
    let mut names: Vec<&str> = Vec::new();
    for (packet, _, _, _, _) in PROTOCOL {
        if *packet == "probe" {
            continue;
        }
        if !names.contains(packet) {
            names.push(packet);
        }
    }
    let others: Vec<String> = names
        .iter()
        .map(|packet| {
            let fields = if *packet == "handshake" { "intent: varint" } else { "a: u8" };
            format!("packet {packet} {{ {fields} }}")
        })
        .collect();
    let routes: String = PROTOCOL
        .iter()
        .map(|(packet, route, _, _, _)| format!("    route {route} => {packet}\n"))
        .collect();
    format!(
        "{std}\n{body}\n{others}\nprotocol minecraft @ v1 {{\n    id = 1\n    frame default {{\n        length: varint\n        id: varint\n        payload: rest\n    }}\n{routes}}}\n",
        others = others.join("\n"),
    )
}
