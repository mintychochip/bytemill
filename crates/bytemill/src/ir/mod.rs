//! Language-neutral schema IR.
//!
//! Printers read this, not the AST. See
//! `docs/superpowers/specs/2026-10-15-bps-codegen-ir-design.md`.
mod exec;
mod lower;
mod schema;
pub mod stages;
pub mod typed;
mod width;

pub use typed::typed;

pub use exec::{exec_decode, exec_decode_frame, exec_encode, exec_encode_frame};
pub use lower::lower;
pub use schema::{
    Field, Frame, IrArm, IrSchema, Proc, Route, Stmt, Ty, TypeDef, TypeId, Variant, WireOp,
};
pub use stages::{Stages, stages, stages_json};
pub use width::{Storage, storage_of};
