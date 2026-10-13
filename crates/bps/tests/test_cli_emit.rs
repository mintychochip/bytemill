//! Ports tests/test_cli_emit.py.

mod common;

use bps::Value;
use std::path::{Path, PathBuf};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "bps-cliemit-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn write_table(path: &Path) -> PathBuf {
    write(path, &common::rstom_tables::probe_schema_json());
    path.to_path_buf()
}

#[test]
fn the_table_is_read_from_the_workspace_the_crate_is_written_into() {
    let dir = temp_dir("ws");
    let schema = dir.join("probe.bps");
    write(&schema, &common::rstom_tables::probe_source_default(&common::repo_root()));
    let workspace = dir.join("rstom");
    write_table(&workspace.join("tools/packets_gen/schema/packets.json"));
    let out = workspace.join("crates/bps_rstom");
    let code = bps::cli::main(&[
        "emit".into(),
        "rstom".into(),
        schema.to_string_lossy().into_owned(),
        "--out".into(),
        out.to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert_eq!(code, 0);
    assert!(out.join("src/server/play/probe_packet.rs").is_file());
}

#[test]
fn a_flag_names_the_table_for_a_crate_outside_a_workspace() {
    let dir = temp_dir("flag");
    let schema = dir.join("probe.bps");
    write(&schema, &common::rstom_tables::probe_source_default(&common::repo_root()));
    let table = write_table(&dir.join("elsewhere/packets.json"));
    let out = dir.join("scratch");
    let code = bps::cli::main(&[
        "emit".into(),
        "rstom".into(),
        schema.to_string_lossy().into_owned(),
        "--out".into(),
        out.to_string_lossy().into_owned(),
        "--packets-json".into(),
        table.to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert_eq!(code, 0);
    assert!(out.join("src/server/play/probe_packet.rs").is_file());
}

#[test]
fn without_a_table_the_emit_says_where_it_looked() {
    let dir = temp_dir("notable");
    let schema = dir.join("probe.bps");
    write(&schema, &common::rstom_tables::probe_source_default(&common::repo_root()));
    let out = dir.join("scratch");
    let code = bps::cli::main(&[
        "emit".into(),
        "rstom".into(),
        schema.to_string_lossy().into_owned(),
        "--out".into(),
        out.to_string_lossy().into_owned(),
    ])
    .unwrap();
    assert_eq!(code, 3);
    assert!(!out.exists(), "nothing is written when the table is missing");
}

/// The error text is on stderr; capture it by running the binary.
#[test]
fn without_a_table_the_error_names_the_flag_and_file() {
    let dir = temp_dir("notable2");
    let schema = dir.join("probe.bps");
    write(&schema, &common::rstom_tables::probe_source_default(&common::repo_root()));
    let out = dir.join("scratch");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_bpsc"))
        .args([
            "emit",
            "rstom",
            &schema.to_string_lossy(),
            "--out",
            &out.to_string_lossy(),
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(3));
    let err = String::from_utf8_lossy(&result.stderr);
    assert!(err.contains("--packets-json"), "{err}");
    assert!(err.contains("packets.json"), "{err}");
    let _ = Value::Null;
}
