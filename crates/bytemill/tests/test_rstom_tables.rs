//! Ports tests/test_rstom_tables.py: `RSTOM` resolves from env, then the
//! rstom-bps worktree, then a live checkout.
//!
//! Deviation: the Python test reloads the module to re-read the env var into
//! the `RSTOM` constant; the Rust helper reads the env on every call, so the
//! tests call `rstom_dir()` directly under a `Mutex`-guarded env swap.

mod common;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

/// Serializes the env swaps: `set_var`/`remove_var` are `unsafe` in edition
/// 2024, and another test must never observe a half-swapped environment.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Restores `BML_RSTOM_DIR` on drop so a failure cannot leak into other tests.
struct RestoreEnv {
    old: Option<OsString>,
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        unsafe {
            match &self.old {
                Some(v) => std::env::set_var("BML_RSTOM_DIR", v),
                None => std::env::remove_var("BML_RSTOM_DIR"),
            }
        }
    }
}

fn hold_env() -> (MutexGuard<'static, ()>, RestoreEnv) {
    let guard = ENV_LOCK.lock().unwrap();
    let old = std::env::var_os("BML_RSTOM_DIR");
    (guard, RestoreEnv { old })
}

#[test]
fn bml_rstom_dir_overrides_default() {
    let (_lock, _restore) = hold_env();
    unsafe {
        std::env::set_var("BML_RSTOM_DIR", "/tmp/custom");
    }
    assert_eq!(common::rstom_tables::rstom_dir(), PathBuf::from("/tmp/custom"));
}

#[test]
fn prefers_rstom_bps_sibling_when_present() {
    let (_lock, _restore) = hold_env();
    unsafe {
        std::env::remove_var("BML_RSTOM_DIR");
    }
    let parent = common::repo_root().parent().unwrap().to_path_buf();
    let expected = if parent.join("rstom-bps").is_dir() {
        parent.join("rstom-bps")
    } else {
        parent.join("rstom")
    };
    assert_eq!(common::rstom_tables::rstom_dir(), expected);
}
