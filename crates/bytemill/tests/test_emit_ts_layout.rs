//! Prettier width sweep around the 80-column boundary for TS statement layout.

use std::path::Path;
use std::process::Command;

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-ts-layout-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn has_prettier() -> bool {
    Command::new("prettier").arg("--version").output().is_ok()
}

fn prettier_check_ts(dir: &Path) {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ts"))
        .collect();
    files.sort();
    for file in files {
        let out = Command::new("prettier")
            .args(["--check"])
            .arg(&file)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "prettier --check {} failed\nstdout: {}\nstderr: {}",
            file.file_name().unwrap().to_string_lossy(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn std_core() -> String {
    std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/leb128.bml")).unwrap()
}

/// `toU8(...)` over a masked bit-or chain (varuint32-style first byte).
fn schema_u8_chain(pad: usize) -> String {
    let field = format!("b{}", "p".repeat(pad));
    format!(
        r#"
encoding sweep_u8_{pad} = {{
    {field}: u8 = ((value & 0xFFFFFFFF) & 0x7F) | ((((value & 0xFFFFFFFF) >> 7) != 0) ? 0x80 : 0)
}} = (value & 0xFFFFFFFF)
"#
    )
}

/// `toU32(Number(` around a zigzag raw field (varint32 encode).
fn schema_number_wrap(pad: usize) -> String {
    let field = format!("raw{}", "p".repeat(pad));
    format!(
        r#"
encoding sweep_num_{pad} = {{
    {field}: varuint32 = ((value << 1) ^ (value >> 31)) & 0xFFFFFFFF
}} = sign_extend(({field} >> 1) ^ (0 - ({field} & 1)), 32)
"#
    )
}

/// `BigInt(` inside a `number` chain (varint64 encode).
fn schema_bigint_wrap(pad: usize) -> String {
    let field = format!("raw{}", "p".repeat(pad));
    format!(
        r#"
encoding sweep_big_{pad} = {{
    {field}: varuint64 = ((value << 1) ^ (value >> 63)) & 0xFFFFFFFFFFFFFFFF
}} = sign_extend(({field} >> 1) ^ (0 - ({field} & 1)), 64)
"#
    )
}

/// Plain `const` assignment: a long rhs without a `Number(` / `BigInt(` wrapper (varint64 raw).
fn schema_plain_assign(pad: usize) -> String {
    let field = format!("raw{}", "p".repeat(pad));
    format!(
        r#"
encoding sweep_plain_{pad} = {{
    {field}: varuint64 = ((value << 1) ^ (value >> 63)) & 0xFFFFFFFFFFFFFFFF
}} = sign_extend(({field} >> 1) ^ (0 - ({field} & 1)), 64)
"#
    )
}

#[test]
fn ts_layout_prettier_width_sweep() {
    if !has_prettier() {
        eprintln!("skipping ts_layout_prettier_width_sweep: prettier not installed");
        return;
    }

    let core = std_core();
    let mut checked = 0usize;
    // Columns 74..=90: pad field / local names to sweep the 80/81 boundary.
    for pad in 0..=16 {
        for extra in [
            schema_u8_chain(pad),
            schema_number_wrap(pad),
            schema_bigint_wrap(pad),
            schema_plain_assign(pad),
        ] {
            let source = core.clone() + &extra;
            let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
            let dir = scratch(&format!("sweep-{pad}-{checked}"));
            bytemill::emit_ts::emit_typescript(&resolved, &dir).unwrap();
            prettier_check_ts(&dir);
            checked += 1;
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    assert!(
        checked >= 60,
        "expected a non-vacuous width sweep (got {checked} emissions)"
    );
}
