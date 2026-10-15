//! BPS — Binary Protocol Schema compiler and interpreter.
//!
//! The Rust port of the Python `bps` package. Module layout mirrors the
//! original: `lexer`/`ast`/`parser` for the front end, `load`/`refs`/`analyze`
//! for resolution, `interp` for the decode/encode engine, `emit_*` for the
//! code generators, and `wiki_routes` for the scraper behind the route file.

pub mod analyze;
pub mod ast;
pub mod cli;
pub mod emit_json;
pub mod emit_rust;
pub mod emit_rstom;
pub mod emit_tree;
pub mod errors;
pub mod frames_gen;
pub mod interp;
pub mod json;
pub mod lexer;
pub mod load;
pub mod minestom;
pub mod ordmap;
pub mod parser;
pub mod pins;
pub mod primitives;
pub mod protodef;
pub mod refs;
pub mod shapes;
pub mod synth;
pub mod text;
pub mod tree_packet;
pub mod value;
pub mod wiki_routes;
pub mod wire;
pub mod wiregen;

pub use analyze::analyze;
pub use ast::{BlockItem, ResolvedSchema};
pub use errors::{BpsError, BpsResult, ErrorKind};
pub use load::load_schema;
pub use parser::parse;
pub use value::{Env, Value};
