//! Explicit compiler phases above the lowered IR.
//!
//! The compiler used to hand tooling only its endpoints: the parsed [`Schema`]
//! (syntax) or the lowered [`IrSchema`] (semantics after lowering). This module
//! keeps the phases as data so a caller can inspect what each one knows:
//!
//! ```text
//! syntax    parsed declarations and imports, before any name is resolved
//! semantic  analyzed declarations, symbol tables, and checked routes
//! lowered   the language-neutral IrSchema: types, procedures, frames, routes
//! typed     the Program printers consume: resolved names, types, codecs, locals
//! ```
//!
//! The phases are views over the existing compiler passes; running [`stages`]
//! does not change how the CLI or the printers compile a schema.

use crate::analyze::analyze;
use crate::ast::{ResolvedSchema, Schema};
use crate::errors::BpsResult;
use crate::json::Value;
use crate::ordmap::OrderedMap;

use super::typed::{self, CodecKind, Program, TypeKind};
use super::{IrSchema, Proc, TypeDef, lower};

/// A declaration before resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub kind: &'static str,
    pub name: String,
}

/// The parsed schema: what the parser saw, with no names resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxPhase {
    pub imports: Vec<String>,
    pub declarations: Vec<Declaration>,
}

/// A route after the analyzer checked its target packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteSummary {
    pub state: String,
    pub direction: String,
    pub packet_id: i128,
    pub packet: String,
    pub frame: Option<String>,
}

/// The analyzed schema: declaration tables and checked routes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticPhase {
    pub encodings: Vec<String>,
    pub enums: Vec<String>,
    pub packets: Vec<String>,
    pub protocols: Vec<String>,
    pub routes: Vec<RouteSummary>,
}

/// A type as the lowered IR knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweredType {
    pub name: String,
    pub kind: &'static str,
}

/// A lowered decode or encode procedure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Procedure {
    pub name: String,
    pub params: Vec<String>,
    pub returns: bool,
}

/// A lowered frame and the procedures that read and write it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweredFrame {
    pub name: String,
    pub type_id: String,
    pub decode: String,
    pub encode: String,
}

/// The language-neutral IR: types, procedures, frames, and routes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoweredPhase {
    pub types: Vec<LoweredType>,
    pub decode: Vec<Procedure>,
    pub encode: Vec<Procedure>,
    pub frames: Vec<LoweredFrame>,
    pub routes: Vec<RouteSummary>,
}

/// A type as the typed program knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedType {
    pub name: String,
    pub ir: String,
    pub kind: &'static str,
    pub codec: Option<String>,
}

/// One typed codec and its body shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedCodec {
    pub name: String,
    pub ir: String,
    pub kind: &'static str,
    pub value: String,
    pub params: Vec<String>,
    pub locals: usize,
    pub statements: usize,
}

/// The typed program printers consume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedPhase {
    pub types: Vec<TypedType>,
    pub codecs: Vec<TypedCodec>,
    pub routes: Vec<RouteSummary>,
}

/// Every phase, in compiler order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stages {
    pub syntax: SyntaxPhase,
    pub semantic: SemanticPhase,
    pub lowered: LoweredPhase,
    pub typed: TypedPhase,
}

/// Run the compiler front end and return each phase's view.
///
/// This is the same pass order as `bml` itself: parse (done by the caller),
/// analyze, lower, then type. A failure stops at the phase that raised it.
pub fn stages(schema: &Schema) -> BpsResult<Stages> {
    let syntax = syntax_phase(schema);
    let resolved = analyze(schema)?;
    let semantic = semantic_phase(&resolved);
    let lowered_ir = lower(&resolved)?;
    let lowered = lowered_phase(&lowered_ir);
    let program = typed::typed(&resolved)?;
    let typed = typed_phase(&program);
    Ok(Stages {
        syntax,
        semantic,
        lowered,
        typed,
    })
}

fn syntax_phase(schema: &Schema) -> SyntaxPhase {
    let mut declarations = Vec::new();
    for encoding in &schema.encodings {
        declarations.push(Declaration {
            kind: "encoding",
            name: encoding.name.clone(),
        });
    }
    for decl in &schema.enums {
        declarations.push(Declaration {
            kind: "enum",
            name: decl.name.clone(),
        });
    }
    for packet in &schema.packets {
        declarations.push(Declaration {
            kind: "packet",
            name: packet.name.clone(),
        });
    }
    for protocol in &schema.protocols {
        declarations.push(Declaration {
            kind: "protocol",
            name: protocol.name.clone(),
        });
    }
    SyntaxPhase {
        imports: schema.imports.clone(),
        declarations,
    }
}

fn semantic_phase(resolved: &ResolvedSchema) -> SemanticPhase {
    SemanticPhase {
        encodings: resolved.encodings.keys().map(str::to_string).collect(),
        enums: resolved.enums.keys().map(str::to_string).collect(),
        packets: resolved.packets.keys().map(str::to_string).collect(),
        protocols: resolved.protocols.keys().map(str::to_string).collect(),
        routes: resolved
            .routes
            .iter()
            .map(|(route, packet)| RouteSummary {
                state: route.state.clone(),
                direction: route.direction.clone(),
                packet_id: route.packet_id,
                packet: packet.name.clone(),
                frame: route.frame.clone(),
            })
            .collect(),
    }
}

fn lowered_phase(ir: &IrSchema) -> LoweredPhase {
    LoweredPhase {
        types: ir
            .types
            .iter()
            .map(|(name, def)| LoweredType {
                name: name.to_string(),
                kind: match def {
                    TypeDef::Scalar(_) => "scalar",
                    TypeDef::String => "string",
                    TypeDef::Record { .. } => "record",
                    TypeDef::Sum { .. } => "sum",
                    TypeDef::Enum { .. } => "enum",
                },
            })
            .collect(),
        decode: procedures(&ir.decode),
        encode: procedures(&ir.encode),
        frames: ir
            .frames
            .iter()
            .map(|frame| LoweredFrame {
                name: frame.name.clone(),
                type_id: frame.type_id.0.clone(),
                decode: frame.decode.clone(),
                encode: frame.encode.clone(),
            })
            .collect(),
        routes: ir
            .routes
            .iter()
            .map(|route| RouteSummary {
                state: route.state.clone(),
                direction: route.direction.clone(),
                packet_id: route.packet_id,
                packet: route.packet.0.clone(),
                frame: Some(route.frame.clone()),
            })
            .collect(),
    }
}

fn procedures(table: &OrderedMap<Proc>) -> Vec<Procedure> {
    table
        .iter()
        .map(|(name, proc_)| Procedure {
            name: name.to_string(),
            params: proc_.params.clone(),
            returns: proc_.result.is_some(),
        })
        .collect()
}

fn typed_phase(program: &Program) -> TypedPhase {
    TypedPhase {
        types: program
            .types
            .iter()
            .map(|decl| TypedType {
                name: decl.name.snake(),
                ir: decl.ir.clone(),
                kind: match &decl.kind {
                    TypeKind::Record { .. } => "record",
                    TypeKind::Sum { .. } => "sum",
                    TypeKind::Enum { .. } => "enum",
                    TypeKind::Alias { .. } => "alias",
                },
                codec: decl.codec.map(|idx| program.codecs[idx].name.snake()),
            })
            .collect(),
        codecs: program
            .codecs
            .iter()
            .map(|codec| TypedCodec {
                name: codec.name.snake(),
                ir: codec.ir.clone(),
                kind: match codec.kind {
                    CodecKind::Scalar => "scalar",
                    CodecKind::String => "string",
                    CodecKind::Record => "record",
                    CodecKind::Sum => "sum",
                    CodecKind::Enum => "enum",
                    CodecKind::Alias => "alias",
                },
                value: program.ty_name(&codec.value),
                params: codec
                    .params
                    .iter()
                    .map(|param| {
                        format!(
                            "{}: {}{}",
                            param.name.snake(),
                            program.ty_name(&param.ty),
                            if param.optional { "?" } else { "" }
                        )
                    })
                    .collect(),
                locals: codec.decode.locals.len(),
                statements: codec.decode.stmts.len() + codec.encode.stmts.len(),
            })
            .collect(),
        routes: program
            .routes
            .iter()
            .map(|route| RouteSummary {
                state: route.state.clone(),
                direction: route.direction.clone(),
                packet_id: route.id,
                packet: program.codecs[route.packet].name.snake(),
                frame: route.frame.map(|idx| program.codecs[idx].name.snake()),
            })
            .collect(),
    }
}

/// The phases as one JSON object, for `bml stages` and the browser bridge.
pub fn stages_json(stages: &Stages) -> Value {
    obj(vec![
        (
            "syntax",
            obj(vec![
                ("imports", strings(&stages.syntax.imports)),
                (
                    "declarations",
                    Value::list(
                        stages
                            .syntax
                            .declarations
                            .iter()
                            .map(|decl| {
                                obj(vec![
                                    ("kind", Value::str(decl.kind)),
                                    ("name", Value::str(decl.name.clone())),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
        ),
        (
            "semantic",
            obj(vec![
                ("encodings", strings(&stages.semantic.encodings)),
                ("enums", strings(&stages.semantic.enums)),
                ("packets", strings(&stages.semantic.packets)),
                ("protocols", strings(&stages.semantic.protocols)),
                ("routes", routes_json(&stages.semantic.routes)),
            ]),
        ),
        (
            "lowered",
            obj(vec![
                (
                    "types",
                    Value::list(
                        stages
                            .lowered
                            .types
                            .iter()
                            .map(|ty| {
                                obj(vec![
                                    ("name", Value::str(ty.name.clone())),
                                    ("kind", Value::str(ty.kind)),
                                ])
                            })
                            .collect(),
                    ),
                ),
                ("decode", procedures_json(&stages.lowered.decode)),
                ("encode", procedures_json(&stages.lowered.encode)),
                ("frames", frames_json(&stages.lowered.frames)),
                ("routes", routes_json(&stages.lowered.routes)),
            ]),
        ),
        (
            "typed",
            obj(vec![
                (
                    "types",
                    Value::list(
                        stages
                            .typed
                            .types
                            .iter()
                            .map(|ty| {
                                obj(vec![
                                    ("name", Value::str(ty.name.clone())),
                                    ("ir", Value::str(ty.ir.clone())),
                                    ("kind", Value::str(ty.kind)),
                                    ("codec", option_string(&ty.codec)),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "codecs",
                    Value::list(
                        stages
                            .typed
                            .codecs
                            .iter()
                            .map(|codec| {
                                obj(vec![
                                    ("name", Value::str(codec.name.clone())),
                                    ("ir", Value::str(codec.ir.clone())),
                                    ("kind", Value::str(codec.kind)),
                                    ("value", Value::str(codec.value.clone())),
                                    ("params", strings(&codec.params)),
                                    ("locals", Value::Int(codec.locals as i128)),
                                    ("statements", Value::Int(codec.statements as i128)),
                                ])
                            })
                            .collect(),
                    ),
                ),
                ("routes", routes_json(&stages.typed.routes)),
            ]),
        ),
    ])
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    let mut out = OrderedMap::new();
    for (key, value) in pairs {
        out.insert(key, value);
    }
    Value::map(out)
}

fn strings(items: &[String]) -> Value {
    Value::list(items.iter().cloned().map(Value::str).collect())
}

fn option_string(value: &Option<String>) -> Value {
    match value {
        Some(value) => Value::str(value.clone()),
        None => Value::Null,
    }
}

fn routes_json(routes: &[RouteSummary]) -> Value {
    Value::list(
        routes
            .iter()
            .map(|route| {
                obj(vec![
                    ("state", Value::str(route.state.clone())),
                    ("direction", Value::str(route.direction.clone())),
                    ("id", Value::Int(route.packet_id)),
                    ("packet", Value::str(route.packet.clone())),
                    ("frame", option_string(&route.frame)),
                ])
            })
            .collect(),
    )
}

fn frames_json(frames: &[LoweredFrame]) -> Value {
    Value::list(
        frames
            .iter()
            .map(|frame| {
                obj(vec![
                    ("name", Value::str(frame.name.clone())),
                    ("type_id", Value::str(frame.type_id.clone())),
                    ("decode", Value::str(frame.decode.clone())),
                    ("encode", Value::str(frame.encode.clone())),
                ])
            })
            .collect(),
    )
}

fn procedures_json(procedures: &[Procedure]) -> Value {
    Value::list(
        procedures
            .iter()
            .map(|proc_| {
                obj(vec![
                    ("name", Value::str(proc_.name.clone())),
                    ("params", strings(&proc_.params)),
                    ("returns", Value::Bool(proc_.returns)),
                ])
            })
            .collect(),
    )
}
