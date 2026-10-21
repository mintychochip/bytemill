//! Shared helpers for the ported pytest suite.
//!
//! Every test crate compiles this module and uses a subset of it, so unused helpers are expected.
#![allow(dead_code)]

pub mod java_probes;
pub mod rstom_tables;

use bytemill::ast::Schema;
use bytemill::errors::BmlResult;
use bytemill::{analyze, load_schema, parse};
use std::path::{Path, PathBuf};

/// The repository root (the crate lives at <root>/crates/bytemill).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate is two levels under the repo root")
        .to_path_buf()
}

pub fn schema_path(rel: &str) -> PathBuf {
    repo_root().join("schemas").join(rel)
}

/// The whole protocol, loaded from the one file that imports the rest.
pub fn load_minecraft() -> BmlResult<bytemill::ResolvedSchema> {
    analyze(&load_schema(&[schema_path("minecraft_26_3_wiki.bml")])?)
}

/// The Minecraft corpus plus extra declarations parsed from `source`.
pub fn load_with_extra(source: &str) -> BmlResult<bytemill::ResolvedSchema> {
    let base = load_minecraft()?;
    let extra = parse(source)?;
    let schema = Schema {
        encodings: base.schema.encodings.iter().cloned().chain(extra.encodings).collect(),
        enums: base.schema.enums.iter().cloned().chain(extra.enums).collect(),
        packets: base.schema.packets.iter().cloned().chain(extra.packets).collect(),
        protocols: base.schema.protocols.iter().cloned().chain(extra.protocols).collect(),
        imports: Vec::new(),
    };
    analyze(&schema)
}

/// A unique throwaway directory of `.bml` files.
pub fn temp_tree(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bml-test-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn write_file(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, text).unwrap();
    path
}

/// Clean up a temp tree (best effort).
pub fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// The decoded environment as a comparable structure.
pub fn env_of(v: &bytemill::Value) -> &bytemill::Env {
    match v {
        bytemill::Value::Map(m) => m,
        _ => panic!("expected a map value"),
    }
}

/// The generated shape ledger (`targets/rstom/shapes`): git-ignored and
/// possibly absent on a fresh clone. True when the directory holds at least
/// one `*.json` file; tests that need the ledger skip otherwise.
pub fn committed_shapes_present() -> bool {
    let dir = repo_root().join("targets/rstom/shapes");
    if !dir.is_dir() {
        return false;
    }
    let mut stack = vec![dir];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "json") {
                return true;
            }
        }
    }
    false
}

/// Message printed when a test skips for lack of the generated ledger.
pub const MISSING_SHAPES_MSG: &str =
    "skipped: targets/rstom/shapes not generated; run scripts/extract_rstom_shapes.sh";

/// `probe_source` for the shared emit-tree fixtures (they cannot import this
/// module because they are `include!`d into two different test crates).
pub fn probe_source_for(body: &str) -> String {
    rstom_tables::probe_source(&repo_root(), body)
}
