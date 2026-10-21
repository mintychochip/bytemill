//! Import PrismarineJS minecraft-data `protocol.json` (ProtoDef) descriptions as BPS schemas.
//!
//! `model` parses the JSON into a typed tree, `lower` translates packets and named types into
//! the `ir` tree (resolving every ProtoDef path through `scope`), `names` and `scope` give
//! every field its final name, and `render` prints one self-contained `.bps` file per protocol.
//! `versions` drives it over the manifest in `schemas/versions/`.

pub mod ir;
pub mod lint;
pub mod lower;
pub mod model;
pub mod names;
pub mod prelude;
pub mod render;
pub mod scope;
pub mod share;
pub mod versions;

pub use model::Protocol;
pub use render::{FileSpec, import};
pub use versions::{VersionEntry, import_versions};
