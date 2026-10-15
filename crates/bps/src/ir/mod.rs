//! Language-neutral schema IR.
//!
//! Printers read this, not the AST. See
//! `docs/superpowers/specs/2026-10-15-bps-codegen-ir-design.md`.
mod exec;
mod lower;
mod schema;
mod width;

pub use exec::{exec_decode, exec_encode};
pub use lower::lower;
pub use schema::{Field, IrArm, IrSchema, Proc, Route, Stmt, Ty, TypeDef, TypeId, Variant, WireOp};
pub use width::{Storage, storage_of};
