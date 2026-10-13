//! Ports tests/test_emit_rust.py.

mod common;

/// Deletes the generated crate and its `target/` (about 60 MB) however the test ends, like the
/// Python test's `TemporaryDirectory`.
struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        common::cleanup(&self.0);
    }
}

/// `emit rust` on the whole protocol, then the generated crate's own tests.
#[test]
fn emit_and_cargo_test() {
    if std::process::Command::new("cargo").arg("--version").output().is_err() {
        eprintln!("cargo not installed; skipping");
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let out = common::temp_tree("emit-rust");
    let _cleanup = Cleanup(out.clone());
    let crate_dir = out.join("crate");
    bps::emit_rust::emit_rust(&resolved, &crate_dir).unwrap();
    let proc = std::process::Command::new("cargo")
        .args(["test", "--quiet"])
        .current_dir(&crate_dir)
        .output()
        .unwrap();
    assert_eq!(
        proc.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&proc.stdout),
        String::from_utf8_lossy(&proc.stderr)
    );
}
