//! Language-neutral schema IR.
//!
//! Printers read this, not the AST. See
//! `docs/superpowers/specs/2026-10-15-bps-codegen-ir-design.md`.

mod width;

pub use width::{storage_of, Storage};
