// Shared emit-tree fixtures, ported from `tests/test_emit_tree.py`'s helpers.
// Included by `test_emit_tree` and `test_emit_tree_safety`; it deliberately carries
// no `use` statements so both includers keep their own imports.

pub fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bps-tree-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn write(path: &std::path::Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

pub fn probe_source(body: &str) -> String {
    crate::common::probe_source_for(body)
}

const KEEP_ALIVE_SCHEMA: &str = "packet keep_alive { id: i64be }\nprotocol minecraft @ v1 {\n    id = 1\n    frame default { length: varint id: varint payload: rest }\n    route play.serverbound 0x00 => keep_alive\n}\n";

fn keep_alive_schema() -> String {
    let root = crate::common::repo_root();
    let std: String = ["integers", "varint"]
        .iter()
        .map(|n| {
            format!(
                "import \"{}\"\n",
                root.join("schemas/std")
                    .join(format!("{n}.bps"))
                    .display()
            )
        })
        .collect();
    format!("{std}{KEEP_ALIVE_SCHEMA}")
}

fn keep_alive_table_value() -> bps::Value {
    let entry = |group: &str, package: &str, name: &str| {
        let mut m = bps::ordmap::OrderedMap::new();
        m.insert("name", bps::Value::str(name));
        m.insert(
            "java",
            bps::Value::str(format!(
                "net.minestom.server.network.packet.{group}.{package}.{name}"
            )),
        );
        m.insert("group", bps::Value::str(group));
        m.insert("state", bps::Value::str(package));
        bps::Value::map(m)
    };
    let mut root = bps::ordmap::OrderedMap::new();
    root.insert(
        "packets",
        bps::Value::list(vec![entry("client", "common", "ClientKeepAlivePacket")]),
    );
    let mut table = bps::ordmap::OrderedMap::new();
    table.insert("state", bps::Value::str("play"));
    table.insert("side", bps::Value::str("CLIENT"));
    table.insert(
        "packets",
        bps::Value::list(vec![bps::Value::str("ClientKeepAlivePacket")]),
    );
    root.insert(
        "stateTables",
        bps::Value::list(vec![bps::Value::map(table)]),
    );
    bps::Value::map(root)
}

pub fn keep_alive_table() -> bps::minestom::ClassTable {
    bps::minestom::ClassTable::from_schema(&keep_alive_table_value()).unwrap()
}

pub fn keep_alive_table_json() -> String {
    bps::json::dumps_sorted(&keep_alive_table_value())
}

fn struct_docs_from_golden() -> Vec<String> {
    let path = crate::common::repo_root()
        .join("crates/bps/tests/fixtures/rstom_tree/client_keep_alive_packet.rs");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut docs = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("/// ") {
            docs.push(rest.to_string());
        } else if line.starts_with("#[derive") {
            break;
        }
    }
    docs
}

/// The shape of the hand keep-alive packet, extracted when the worktree has it.
pub fn keep_alive_shape() -> bps::shapes::Shape {
    let hand = crate::common::rstom_tables::rstom_dir()
        .join("crates/rstom/src/network/packet/client/common/client_keep_alive_packet.rs");
    if hand.is_file() {
        let text = std::fs::read_to_string(&hand).unwrap();
        if let Ok(s) = bps::shapes::extract_shape(
            &text,
            "ClientKeepAlivePacket",
            "client/common/client_keep_alive_packet",
            Some(&hand),
        ) {
            return s;
        }
    }
    bps::shapes::Shape {
        cls: "ClientKeepAlivePacket".into(),
        module: "client/common/client_keep_alive_packet".into(),
        module_docs: vec![
            "Port of `net.minestom.server.network.packet.client.common.ClientKeepAlivePacket`."
                .into(),
        ],
        struct_docs: struct_docs_from_golden(),
        derives: vec![
            "Copy".into(),
            "Clone".into(),
            "Debug".into(),
            "PartialEq".into(),
            "Eq".into(),
        ],
        form: "braced".into(),
        fields: vec![bps::shapes::FieldShape {
            name: "id".into(),
            type_: "i64".into(),
            docs: vec!["Java: `id()`.".into()],
            wire: None,
        }],
        new: bps::shapes::NewShape {
            present: true,
            const_: false,
            must_use: true,
            attrs: Vec::new(),
            docs: vec!["Java: `new ClientKeepAlivePacket(long)`.".into()],
            body: Vec::new(),
            params: None,
        },
        serializer_fn_docs: vec!["Java: `ClientKeepAlivePacket.SERIALIZER`.".into()],
        serializer_fn_must_use: true,
        serializer: bps::shapes::SerializerShape {
            style: "struct".into(),
            name: "ClientKeepAliveSerializer".into(),
            public: false,
            docs: vec![
                "Java: the `template(LONG, ClientKeepAlivePacket::id, …)` template.".into(),
            ],
            read_via_new: false,
        },
        extras: None,
        ..bps::shapes::Shape::default()
    }
}

pub fn analyze_keep_alive() -> (bps::ResolvedSchema, bps::minestom::Unit) {
    let dir = temp_dir("keepalive");
    let schema = dir.join("keep.bps");
    write(&schema, &keep_alive_schema());
    let resolved = bps::analyze(&bps::load_schema(&[schema]).unwrap()).unwrap();
    let units = bps::minestom::build_units(&resolved, &keep_alive_table()).unwrap();
    let unit = units
        .into_iter()
        .find(|u| u.pkt.name == "keep_alive")
        .unwrap();
    (resolved, unit)
}

pub fn keep_alive_body(
    unit: &bps::minestom::Unit,
    resolved: &bps::ResolvedSchema,
) -> bps::emit_rstom::PacketBody {
    bps::emit_rstom::packet_body(unit, resolved).unwrap()
}
