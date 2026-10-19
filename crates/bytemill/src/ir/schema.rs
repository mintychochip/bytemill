//! The IR printers and the IR interpreter share. Task 2 of the codegen plan.

use crate::ast::Expr;
use crate::ordmap::OrderedMap;

use super::Storage;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeId(pub String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Scalar(Storage),
    List(Box<Ty>),
    Optional(Box<Ty>),
    Named(TypeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    /// `None` is the `_` arm.
    pub tag: Option<i128>,
    pub ty: Ty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeDef {
    Scalar(Storage),
    String,
    Record {
        fields: Vec<Field>,
    },
    Sum {
        arms: Vec<Variant>,
    },
    Enum {
        base: Storage,
        members: Vec<(String, i128)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct IrArm {
    pub tags: Vec<i128>,
    pub body: Vec<Stmt>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum WireOp {
    U8,
    F32be,
    F64be,
    Bytes { len: Expr },
    Rest,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        expr: Expr,
    },
    Require(Expr),
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
        /// When the guard cannot be evaluated, keep the group if it is present.
        /// Field `if` is false. A `Group`'s `if` is true.
        fallback: bool,
    },
    /// Run `body` as a nested record and store the map in `dst`.
    Block {
        body: Vec<Stmt>,
        dst: String,
    },
    Match {
        scrutinee: Expr,
        arms: Vec<IrArm>,
        default: Option<Vec<Stmt>>,
    },
    RepeatCount {
        n: Expr,
        elem: Vec<Stmt>,
        dst: String,
    },
    RepeatUntil {
        term: Expr,
        elem: Vec<Stmt>,
        dst: String,
    },
    RepeatUntilEnd {
        elem: Vec<Stmt>,
        dst: String,
    },
    Read {
        dst: String,
        op: WireOp,
    },
    Write {
        op: WireOp,
        value: Expr,
    },
    Utf8 {
        dst: String,
        src: String,
    },
    CallDecode {
        dst: String,
        callee: TypeId,
        args: Vec<Expr>,
    },
    CallEncode {
        callee: TypeId,
        args: Vec<Expr>,
        value: Expr,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    /// Decode: the yielded value. Encode: `None`.
    pub result: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub state: String,
    pub direction: String,
    pub packet_id: i128,
    pub packet: TypeId,
    pub frame: String,
}

/// A protocol frame: the type it installs and the procedures that read and
/// write it. `name` is the schema's frame name; `type_id` is the IR key
/// (`frame_<name>`), which is also the procedure name in both directions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub name: String,
    pub type_id: TypeId,
    pub decode: String,
    pub encode: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct IrSchema {
    pub types: OrderedMap<TypeDef>,
    pub decode: OrderedMap<Proc>,
    pub encode: OrderedMap<Proc>,
    pub routes: Vec<Route>,
    pub frames: Vec<Frame>,
}
