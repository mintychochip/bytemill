//! BML — Binary Protocol Schema compiler and interpreter.
//!
//! The Bytemill implementation of the BML language. `lexer`/`ast`/`parser`
//! form the front end, `load`/`refs`/`analyze` resolve a schema, `interp` is
//! the decode/encode engine, `emit_*` generates code, and `wiki_routes` is the
//! scraper behind the route file.

pub mod analyze;
pub mod ast;
pub mod cli;
pub mod cli_help;
pub mod compat;
pub mod compress;
pub mod emit_c;
pub mod emit_cpp;
pub mod emit_csharp;
pub mod emit_go;
pub mod emit_java;
pub mod emit_json;
pub mod emit_python;
pub mod emit_rstom;
pub mod emit_rust;
pub mod emit_rust_typed;
pub mod emit_tree;
pub mod emit_ts;
pub mod emit_zig;
pub mod errors;
pub mod frames_gen;
pub mod interp;
pub mod ir;
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
pub mod target_version;
pub mod text;
pub mod tree_packet;
pub mod value;
pub mod wiki_routes;
pub mod wire;
pub mod wiregen;

pub use target_version::{
    CSharpVersion, CStandard, CppStandard, EmitOptions, GoVersion, JavaVersion, PythonVersion,
    RustEdition, TsTarget, ZigVersion,
};

pub use analyze::analyze;
pub use ast::{BlockItem, ResolvedSchema};
pub use errors::{BmlError, BmlResult, ErrorKind};
pub use load::load_schema;
pub use parser::parse;
pub use value::{Env, Value};
