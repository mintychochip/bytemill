// Shared emit-tree fixtures, ported from `tests/test_emit_tree.py`'s helpers.
// Included by `test_emit_tree` and `test_emit_tree_safety`; it deliberately carries
// no `use` statements so both includers keep their own imports.

pub fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bml-tree-{tag}-{}-{:?}",
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
                    .join(format!("{n}.bml"))
                    .display()
            )
        })
        .collect();
    format!("{std}{KEEP_ALIVE_SCHEMA}")
}

fn keep_alive_table_value() -> bytemill::Value {
    let entry = |group: &str, package: &str, name: &str| {
        let mut m = bytemill::ordmap::OrderedMap::new();
        m.insert("name", bytemill::Value::str(name));
        m.insert(
            "java",
            bytemill::Value::str(format!(
                "net.minestom.server.network.packet.{group}.{package}.{name}"
            )),
        );
        m.insert("group", bytemill::Value::str(group));
        m.insert("state", bytemill::Value::str(package));
        bytemill::Value::map(m)
    };
    let mut root = bytemill::ordmap::OrderedMap::new();
    root.insert(
        "packets",
        bytemill::Value::list(vec![entry("client", "common", "ClientKeepAlivePacket")]),
    );
    let mut table = bytemill::ordmap::OrderedMap::new();
    table.insert("state", bytemill::Value::str("play"));
    table.insert("side", bytemill::Value::str("CLIENT"));
    table.insert(
        "packets",
        bytemill::Value::list(vec![bytemill::Value::str("ClientKeepAlivePacket")]),
    );
    root.insert(
        "stateTables",
        bytemill::Value::list(vec![bytemill::Value::map(table)]),
    );
    bytemill::Value::map(root)
}

pub fn keep_alive_table() -> bytemill::minestom::ClassTable {
    bytemill::minestom::ClassTable::from_schema(&keep_alive_table_value()).unwrap()
}

pub fn keep_alive_table_json() -> String {
    bytemill::json::dumps_sorted(&keep_alive_table_value())
}

fn struct_docs_from_golden() -> Vec<String> {
    let path = crate::common::repo_root()
        .join("crates/bytemill/tests/fixtures/rstom_tree/client_keep_alive_packet.rs");
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
pub fn keep_alive_shape() -> bytemill::shapes::Shape {
    let hand = bytemill::shapes::packet_root(&crate::common::rstom_tables::rstom_dir())
        .join("client/common/client_keep_alive_packet.rs");
    if hand.is_file() {
        let text = std::fs::read_to_string(&hand).unwrap();
        if let Ok(s) = bytemill::shapes::extract_shape(
            &text,
            "ClientKeepAlivePacket",
            "client/common/client_keep_alive_packet",
            Some(&hand),
        ) {
            return s;
        }
    }
    bytemill::shapes::Shape {
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
        fields: vec![bytemill::shapes::FieldShape {
            name: "id".into(),
            type_: "i64".into(),
            docs: vec!["Java: `id()`.".into()],
            bind: Default::default(),
        }],
        new: bytemill::shapes::NewShape {
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
        serializer: bytemill::shapes::SerializerShape {
            style: "struct".into(),
            name: "ClientKeepAliveSerializer".into(),
            public: false,
            docs: vec![
                "Java: the `template(LONG, ClientKeepAlivePacket::id, …)` template.".into(),
            ],
            read_via_new: false,
        },
        extras: None,
        tests: Some("client_keep_alive_packet.tests.rs".into()),
        ..bytemill::shapes::Shape::default()
    }
}

pub fn analyze_keep_alive() -> (bytemill::ResolvedSchema, bytemill::minestom::Unit) {
    let dir = temp_dir("keepalive");
    let schema = dir.join("keep.bml");
    write(&schema, &keep_alive_schema());
    let resolved = bytemill::analyze(&bytemill::load_schema(&[schema]).unwrap()).unwrap();
    let units = bytemill::minestom::build_units(&resolved, &keep_alive_table()).unwrap();
    let unit = units
        .into_iter()
        .find(|u| u.pkt.name == "keep_alive")
        .unwrap();
    (resolved, unit)
}

pub fn keep_alive_body(
    unit: &bytemill::minestom::Unit,
    resolved: &bytemill::ResolvedSchema,
) -> bytemill::emit_rstom::PacketBody {
    bytemill::emit_rstom::packet_body(unit, resolved).unwrap()
}
