//! `dump-json`, porting `bps/emit_json.py`.

use crate::ast::Schema;
use crate::json::{Value, dumps_pretty_sorted};
use crate::ordmap::OrderedMap;

pub fn schema_to_json(schema: &Schema) -> String {
    dumps_pretty_sorted(&schema_dict(schema))
}

fn schema_dict(schema: &Schema) -> Value {
    let mut root = OrderedMap::new();
    root.insert(
        "encodings",
        Value::list(
            schema
                .encodings
                .iter()
                .map(|e| Value::str(e.name.clone()))
                .collect(),
        ),
    );
    root.insert(
        "enums",
        Value::list(
            schema
                .enums
                .iter()
                .map(|e| Value::str(e.name.clone()))
                .collect(),
        ),
    );
    root.insert(
        "packets",
        Value::list(
            schema
                .packets
                .iter()
                .map(|p| Value::str(p.name.clone()))
                .collect(),
        ),
    );
    let mut protocols = Vec::new();
    for p in &schema.protocols {
        let mut entry = OrderedMap::new();
        entry.insert("name", Value::str(p.name.clone()));
        entry.insert("version", Value::str(p.version.clone()));
        entry.insert("id", Value::Int(p.protocol_id));
        entry.insert("routes", Value::Int(p.routes.len() as i128));
        protocols.push(Value::map(entry));
    }
    root.insert("protocols", Value::list(protocols));
    Value::map(root)
}
