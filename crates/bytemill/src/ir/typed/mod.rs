//! The typed program: the IR with every name resolved, every value typed, and
//! every nested record or sum lifted into a codec of its own. Printers read
//! this, never [`super::Stmt`] or the AST. See
//! `docs/superpowers/specs/2026-10-15-bps-idiomatic-printers-design.md`.
//!
//! Meaning is the IR interpreter's (`ir::exec`); this module changes the
//! representation only. A printer that prints each construct below as
//! documented agrees byte-for-byte with the interpreter.
//!
//! # Shape
//!
//! - [`Program::types`]: records, sums, enums, and record aliases. Scalar and
//!   string encodings are not types; their codecs return a [`Ty`] primitive.
//! - [`Program::codecs`]: one decode/encode pair per encoding, packet, frame,
//!   enum, and per nested record or sum (a block field, a `match` field, a
//!   block arm, a list element). A nested codec receives the parent values it
//!   reads as [`ParamRole::Capture`] parameters, and a sum codec receives the
//!   value its arm is chosen by as its [`ParamRole::Tag`] parameter.
//! - [`Body`]: a function. Locals have unique names across the whole body (no
//!   shadowing anywhere), a type, and an `optional` flag.
//!
//! # Values and integers
//!
//! Integer arithmetic is `i64`, two's complement. Every [`Expr::Binary`] names
//! its result type: when both operands already have one integer type `T` (a
//! literal adopts its sibling's type when it fits) and the exact result
//! always fits `T`, the operation stays in `T`; otherwise both operands are
//! [`Expr::Cast`] to `i64` and `wraps` says whether the exact result can
//! leave `i64` (then the printer must use wrapping arithmetic). `Div` floors,
//! `Mod` takes the divisor's sign (divisors are always non-zero literals),
//! `Shr` is arithmetic, shift amounts are literals in `0..64`. A store into a
//! narrower type is an explicit truncating [`Expr::Cast`]. A value read as a
//! condition is a bool; an integer condition is `x != 0` in its own type.
//! `And`/`Or` must short-circuit: [`Expr::Get`] relies on a preceding
//! [`Expr::Present`] test. Python and TypeScript may evaluate integers
//! exactly; the results agree on every value a field can hold.
//!
//! # Statements
//!
//! A [`Bind`] stores a value into a local: `declare` introduces the local at
//! that point, `some` stores into an optional local (present from then on).
//!
//! - `Declare { local, zero }`: introduce `local` before the statements that
//!   assign it. `zero`: start at the type's zero (`0`, `false`, `""`, empty,
//!   absent). Not `zero`: unset; every path assigns it before reading it, or
//!   fails. An optional local always starts absent.
//! - `Let`: evaluate and bind.
//! - `Read`: read a wire primitive. Fails: too few bytes left; `Bytes(n)` with
//!   `n < 0`.
//! - `Decode`: call another codec's decode with `args` (one per
//!   [`Codec::params`] entry, an optional parameter takes an optional value).
//! - `Utf8`: bytes to a string. Fails: not valid UTF-8.
//! - `Write`: write a wire primitive. `Bytes(n)` pads with zero bytes up to
//!   `n` and fails when the value is longer than `n`.
//! - `Encode`: call another codec's encode.
//! - `Require`: fails with "require failed: `text`" unless `cond`.
//! - `Unwrap`: bind the value of optional `src`; fails with "missing `field`"
//!   when it is absent.
//! - `If`, `Match`: `Match` runs the first arm listing the scrutinee, else
//!   `other`; with neither it fails with "no match arm for tag `t`".
//! - `Repeat` (decode): introduces `list` (empty), then runs `body` (which
//!   binds `item`) and appends `item`: `Count(n)` `n` times (fails when
//!   `n < 0`); `Until(t)` until no bytes remain or the next byte equals `t`
//!   (that byte is consumed); `UntilEnd` while bytes remain.
//! - `ForEach` (encode): runs `body` with `item` bound to each element.
//! - `ExpectVariant` (encode): binds the payload of `sum` when it holds
//!   `variant`; fails with "wrong variant" otherwise.
//! - `Return`: the decoded value (decode bodies end in one, or every path
//!   returns or fails).
//! - `Fail`: always fails.
//!
//! Failures carry the codec's name; reads also know the reader's offset.

mod build;
mod dump;

pub use build::typed;

use std::fmt;

/// Index into [`Program::types`].
pub type TypeIdx = usize;
/// Index into [`Program::codecs`].
pub type CodecIdx = usize;
/// Index into [`Body::locals`].
pub type LocalId = usize;
/// Index into a record's fields.
pub type FieldIdx = usize;
/// Index into a sum's variants.
pub type VariantIdx = usize;

/// An identifier as lowercase ASCII words; printers choose the case.
///
/// Words come from schema names split on `_` and `:`. The first word always
/// starts with a letter. [`Program`] guarantees uniqueness per namespace under
/// every casing (types, codecs, fields of a record, variants of a sum, members
/// of an enum, locals of a body).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(Vec<String>);

impl Name {
    /// Words of a schema identifier (`packet:tags`, `max_chars`, `u16be`).
    pub fn new(ident: &str) -> Name {
        let mut words = split_words(ident);
        if words.is_empty() {
            words.push("x".into());
        }
        if words[0].starts_with(|c: char| c.is_ascii_digit()) {
            words.insert(0, "n".into());
        }
        Name(words)
    }

    /// The words, lowercase.
    pub fn words(&self) -> &[String] {
        &self.0
    }

    /// This name followed by `more`'s words.
    pub fn join(&self, more: &Name) -> Name {
        let mut words = self.0.clone();
        words.extend(more.0.iter().cloned());
        Name(words)
    }

    /// This name followed by the words of `ident` (`ident` may start with a
    /// digit: `value` + `2` is `value_2`).
    pub fn with(&self, ident: &str) -> Name {
        let mut words = self.0.clone();
        words.extend(split_words(ident));
        Name(words)
    }

    /// `foo_bar_baz`
    pub fn snake(&self) -> String {
        self.0.join("_")
    }

    /// `FOO_BAR_BAZ`
    pub fn screaming(&self) -> String {
        self.snake().to_ascii_uppercase()
    }

    /// `FooBarBaz`
    pub fn pascal(&self) -> String {
        self.0.iter().map(|w| capitalize(w)).collect()
    }

    /// `fooBarBaz`
    pub fn camel(&self) -> String {
        let mut out = self.0[0].clone();
        for w in &self.0[1..] {
            out.push_str(&capitalize(w));
        }
        out
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.snake())
    }
}

fn split_words(ident: &str) -> Vec<String> {
    ident
        .split(['_', ':', '-', '.', ' '])
        .map(|w| {
            w.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// `ident` with a trailing `_` when it is in `keywords`: the one keyword
/// escape every printer uses.
pub fn escape(ident: String, keywords: &[&str]) -> String {
    if keywords.contains(&ident.as_str()) {
        ident + "_"
    } else {
        ident
    }
}

/// An integer storage class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
}

impl IntTy {
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 => 64,
            IntTy::I128 => 128,
        }
    }

    pub fn signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128
        )
    }

    /// Smallest value of the class.
    pub fn min_value(self) -> i128 {
        match self {
            IntTy::I128 => i128::MIN,
            t if t.signed() => -(1i128 << (t.bits() - 1)),
            _ => 0,
        }
    }

    /// Largest value of the class.
    pub fn max_value(self) -> i128 {
        match self {
            IntTy::I128 => i128::MAX,
            t if t.signed() => (1i128 << (t.bits() - 1)) - 1,
            t => (1i128 << t.bits()) - 1,
        }
    }

    pub fn contains(self, value: i128) -> bool {
        self.min_value() <= value && value <= self.max_value()
    }
}

/// A value type. Optionality is a property of a field, local, or parameter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    Int(IntTy),
    Bool,
    F32,
    F64,
    Str,
    Bytes,
    List(Box<Ty>),
    /// A record, sum, or enum; never an alias.
    Named(TypeIdx),
}

/// The typed program. Built by [`typed`].
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub types: Vec<TypeDecl>,
    pub codecs: Vec<Codec>,
    pub routes: Vec<RouteDecl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeDecl {
    pub name: Name,
    /// The IR type key.
    pub ir: String,
    pub origin: Origin,
    pub kind: TypeKind,
    /// The codec that reads and writes this type.
    pub codec: Option<CodecIdx>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeKind {
    Record {
        fields: Vec<FieldDecl>,
    },
    Sum {
        variants: Vec<VariantDecl>,
    },
    /// Open: decode keeps a value that is not a member.
    Enum {
        base: IntTy,
        members: Vec<EnumMember>,
    },
    /// `encoding a = b` where `b` is a record: another name for `target`.
    Alias {
        target: TypeIdx,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldDecl {
    pub name: Name,
    /// The IR field name (the schema's, or `<name>_<n>` after a rename).
    pub ir: String,
    pub ty: Ty,
    pub optional: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantDecl {
    /// `tag0`, `tag12`, `tag_neg1`; the `_` arm is `other`.
    pub name: Name,
    /// Every tag of the arm; empty for `_`.
    pub tags: Vec<i128>,
    /// `None` for an arm with no body.
    pub payload: Option<Ty>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EnumMember {
    pub name: Name,
    pub value: i128,
}

/// Where a type or codec comes from in the schema, for doc comments.
#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    Packet {
        name: String,
        routes: Vec<RouteRef>,
    },
    Encoding {
        name: String,
    },
    Enum {
        name: String,
    },
    Frame {
        name: String,
    },
    /// A block or `match` field of `parent`, or its list element.
    Field {
        parent: TypeIdx,
        field: String,
    },
    /// The block arm `variant` of sum `sum`.
    Variant {
        sum: TypeIdx,
        variant: VariantIdx,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteRef {
    pub state: String,
    pub direction: String,
    pub id: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDecl {
    pub state: String,
    pub direction: String,
    pub id: i128,
    pub packet: CodecIdx,
    pub frame: Option<CodecIdx>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Codec {
    pub name: Name,
    /// The IR key of the encoding, packet, frame, or nested type.
    pub ir: String,
    pub origin: Origin,
    pub kind: CodecKind,
    /// The decoded value's type.
    pub value: Ty,
    /// The type this codec reads and writes, when it is a declared type.
    pub ty: Option<TypeIdx>,
    pub params: Vec<Param>,
    pub decode: Body,
    pub encode: Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecKind {
    /// A scalar encoding (`varint`, `u16be`, `boolean`).
    Scalar,
    /// A string-shaped encoding (`string`, `identifier`).
    String,
    Record,
    Sum,
    Enum,
    /// `encoding a = b`: delegates to `b`.
    Alias,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: Name,
    pub ty: Ty,
    pub optional: bool,
    pub role: ParamRole,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamRole {
    /// A schema parameter (`max_chars`, `n`), always `i64`.
    Arg,
    /// The value a sum's arm is chosen by.
    Tag,
    /// A value of the calling codec the nested schema text reads (`ir` is its
    /// name there).
    Capture { ir: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Body {
    /// One local per [`Codec::params`] entry, in order.
    pub params: Vec<LocalId>,
    /// Encode: the value being written. Decode: `None`.
    pub value: Option<LocalId>,
    pub locals: Vec<Local>,
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    pub name: Name,
    pub ty: Ty,
    pub optional: bool,
    /// How many expressions read this local (stores do not count).
    pub uses: usize,
}

/// Where a statement stores its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bind {
    pub local: LocalId,
    /// This statement introduces the local.
    pub declare: bool,
    /// Store `Some(value)` into an optional local.
    pub some: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Declare {
        local: LocalId,
        zero: bool,
    },
    Let {
        dst: Bind,
        value: Expr,
    },
    Read {
        dst: Bind,
        op: ReadOp,
    },
    Decode {
        dst: Bind,
        codec: CodecIdx,
        args: Vec<Expr>,
    },
    Utf8 {
        dst: Bind,
        src: Expr,
    },
    Write {
        op: WriteOp,
        value: Expr,
    },
    Encode {
        codec: CodecIdx,
        args: Vec<Expr>,
        value: Expr,
    },
    Require {
        cond: Expr,
        text: String,
    },
    Unwrap {
        dst: Bind,
        src: Expr,
        field: String,
    },
    If {
        cond: Expr,
        then: Vec<Stmt>,
        els: Vec<Stmt>,
    },
    Match {
        scrutinee: Expr,
        arms: Vec<MatchArm>,
        other: Option<Vec<Stmt>>,
    },
    Repeat {
        mode: RepeatMode,
        list: LocalId,
        item: LocalId,
        body: Vec<Stmt>,
    },
    ForEach {
        list: Expr,
        item: LocalId,
        body: Vec<Stmt>,
    },
    ExpectVariant {
        dst: Option<Bind>,
        sum: Expr,
        ty: TypeIdx,
        variant: VariantIdx,
    },
    Return(Expr),
    Fail(Failure),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MatchArm {
    pub tags: Vec<i128>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RepeatMode {
    Count(Expr),
    Until(Expr),
    UntilEnd,
}

/// Wire primitives. `F32`/`F64` are big-endian IEEE 754; `F32Le`/`F64Le` are
/// little-endian.
#[derive(Clone, Debug, PartialEq)]
pub enum ReadOp {
    /// `u8`
    U8,
    /// `f32be` (big-endian)
    F32,
    /// `f64be` (big-endian)
    F64,
    /// `f32le` (little-endian)
    F32Le,
    /// `f64le` (little-endian)
    F64Le,
    /// Exactly `n` bytes (`i64`).
    Bytes(Expr),
    /// Every remaining byte.
    Rest,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WriteOp {
    U8,
    /// `f32be` (big-endian)
    F32,
    /// `f64be` (big-endian)
    F64,
    /// `f32le` (little-endian)
    F32Le,
    /// `f64le` (little-endian)
    F64Le,
    /// The value padded with zero bytes to `n` (`i64`); longer fails.
    Bytes(Expr),
    Rest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// "encode not supported for repeat mode until_end"
    EncodeUntilEnd,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Int {
        value: i64,
        ty: IntTy,
    },
    Bool(bool),
    Str(String),
    Local(LocalId),
    /// Field `field` of record `ty`; optional when the field is.
    Field {
        base: Box<Expr>,
        ty: TypeIdx,
        field: FieldIdx,
    },
    /// The optional operand is present.
    Present(Box<Expr>),
    /// The value of an optional operand already known to be present.
    Get(Box<Expr>),
    Not(Box<Expr>),
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        ty: Ty,
        wraps: bool,
    },
    Cond {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
        ty: Ty,
    },
    /// Integer to integer (truncating or extending), bool to integer (1/0),
    /// enum to its base and back, `f32` to `f64` and back.
    Cast {
        expr: Box<Expr>,
        to: Ty,
    },
    Call {
        func: Func,
        args: Vec<Expr>,
    },
    /// The UTF-8 bytes of a string.
    StrBytes(Box<Expr>),
    /// Decode: bytes left in the reader (`i64`).
    Remaining,
    /// A record value; one expression per field in order (optional fields
    /// take an optional expression).
    Construct {
        ty: TypeIdx,
        fields: Vec<Expr>,
    },
    /// A sum value.
    Variant {
        ty: TypeIdx,
        variant: VariantIdx,
        payload: Option<Box<Expr>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }

    pub fn is_logical(self) -> bool {
        matches!(self, BinOp::And | BinOp::Or)
    }

    /// The operator's usual spelling (`+`, `&&`, `==`, ...).
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
}

/// Builtin functions; every argument and result is `i64` unless noted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    /// `sign_extend(v, bits)`: bit `bits - 1` of `v` is the sign.
    SignExtend,
    /// UTF-16 code units of a string (a scalar above U+FFFF counts two).
    Utf16Units,
    /// UTF-8 byte length of a string.
    Utf8Len,
    /// Unicode scalars of a string.
    StrLen,
    BytesLen,
    ListLen,
    Min,
    Max,
}

impl Program {
    /// The codec of an IR key (`varint`, `handshake`, `packet:tags`).
    pub fn codec_by_ir(&self, ir: &str) -> Option<CodecIdx> {
        self.codecs.iter().position(|c| c.ir == ir)
    }

    /// The codec of a schema packet name.
    pub fn packet_codec(&self, packet: &str) -> Option<CodecIdx> {
        self.codecs
            .iter()
            .position(|c| matches!(&c.origin, Origin::Packet { name, .. } if name == packet))
    }

    /// Predictable output-file stem for `origin`.
    ///
    /// Induced from routes, not a new schema annotation:
    /// - a packet routed in exactly one `state`/`direction` is `{state}_{direction}`
    ///   (`play_clientbound`);
    /// - a packet routed in several of those pairs is `shared`;
    /// - a packet with no route, and every encoding, enum, and frame, is `encoding`;
    /// - a nested field or variant uses its parent's stem.
    pub fn output_stem(&self, origin: &Origin) -> String {
        let mut origin = origin;
        let mut seen = Vec::new();
        loop {
            match origin {
                Origin::Field { parent, .. } => {
                    if !seen.iter().any(|t| t == parent) {
                        seen.push(*parent);
                        origin = &self.types[*parent].origin;
                        continue;
                    }
                    return "encoding".to_string();
                }
                Origin::Variant { sum, .. } => {
                    if !seen.iter().any(|t| t == sum) {
                        seen.push(*sum);
                        origin = &self.types[*sum].origin;
                        continue;
                    }
                    return "encoding".to_string();
                }
                Origin::Packet { routes, .. } => {
                    let mut keys: Vec<String> = routes
                        .iter()
                        .map(|r| format!("{}_{}", file_token(&r.state), file_token(&r.direction)))
                        .collect();
                    keys.sort();
                    keys.dedup();
                    return match keys.as_slice() {
                        [] => "unrouted".to_string(),
                        [one] => one.clone(),
                        _ => "shared".to_string(),
                    };
                }
                Origin::Encoding { .. } | Origin::Enum { .. } | Origin::Frame { .. } => {
                    return "encoding".to_string();
                }
            }
        }
    }
}

/// A path segment: lowercase ascii, anything else becomes `_`.
pub(crate) fn file_token(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

impl Program {
    /// The type of an IR key.
    pub fn type_by_ir(&self, ir: &str) -> Option<TypeIdx> {
        self.types.iter().position(|t| t.ir == ir)
    }

    pub fn fields(&self, ty: TypeIdx) -> &[FieldDecl] {
        match &self.types[ty].kind {
            TypeKind::Record { fields } => fields,
            _ => &[],
        }
    }

    pub fn variants(&self, ty: TypeIdx) -> &[VariantDecl] {
        match &self.types[ty].kind {
            TypeKind::Sum { variants } => variants,
            _ => &[],
        }
    }

    /// The type of `expr` in `body`, and whether it is optional.
    pub fn expr_ty(&self, body: &Body, expr: &Expr) -> (Ty, bool) {
        match expr {
            Expr::Int { ty, .. } => (Ty::Int(*ty), false),
            Expr::Bool(_) | Expr::Present(_) | Expr::Not(_) => (Ty::Bool, false),
            Expr::Str(_) => (Ty::Str, false),
            Expr::Local(id) => (body.locals[*id].ty.clone(), body.locals[*id].optional),
            Expr::Field { ty, field, .. } => {
                let decl = &self.fields(*ty)[*field];
                (decl.ty.clone(), decl.optional)
            }
            Expr::Get(inner) => (self.expr_ty(body, inner).0, false),
            Expr::Binary { ty, .. } | Expr::Cond { ty, .. } | Expr::Cast { to: ty, .. } => {
                (ty.clone(), false)
            }
            Expr::Call { .. } | Expr::Remaining => (Ty::Int(IntTy::I64), false),
            Expr::StrBytes(_) => (Ty::Bytes, false),
            Expr::Construct { ty, .. } | Expr::Variant { ty, .. } => (Ty::Named(*ty), false),
        }
    }

    /// The values an integer expression can take, as `[lo, hi]` in its own
    /// type (after any wrap). `None` for a non-integer expression.
    pub fn int_range(&self, body: &Body, expr: &Expr) -> Option<(i128, i128)> {
        build::int_range(self, body, expr)
    }

    /// The logical negation of the bool `cond`, as a reader would write it:
    /// a comparison of integers, bools, or strings flips its operator
    /// (`a == b` becomes `a != b`, `a < b` becomes `a >= b`), `!x` becomes `x`,
    /// and `&&`/`||` follow De Morgan. Anything else, including a float
    /// comparison (NaN makes `!(a < b)` differ from `a >= b`), is wrapped in
    /// `Expr::Not`. Printers use it for `Require` (fail when the negation
    /// holds) instead of printing `!(cond)`.
    pub fn negated(&self, body: &Body, cond: &Expr) -> Expr {
        match cond {
            Expr::Not(inner) => (**inner).clone(),
            Expr::Bool(b) => Expr::Bool(!b),
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } if op.is_comparison() => {
                let (operand, _) = self.expr_ty(body, lhs);
                if matches!(operand, Ty::F32 | Ty::F64) {
                    return Expr::Not(Box::new(cond.clone()));
                }
                let flipped = match op {
                    BinOp::Eq => BinOp::Ne,
                    BinOp::Ne => BinOp::Eq,
                    BinOp::Lt => BinOp::Ge,
                    BinOp::Le => BinOp::Gt,
                    BinOp::Gt => BinOp::Le,
                    _ => BinOp::Lt,
                };
                Expr::Binary {
                    op: flipped,
                    lhs: lhs.clone(),
                    rhs: rhs.clone(),
                    ty: ty.clone(),
                    wraps: *wraps,
                }
            }
            Expr::Binary {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
                ty,
                wraps,
            } => Expr::Binary {
                op: if *op == BinOp::And {
                    BinOp::Or
                } else {
                    BinOp::And
                },
                lhs: Box::new(self.negated(body, lhs)),
                rhs: Box::new(self.negated(body, rhs)),
                ty: ty.clone(),
                wraps: *wraps,
            },
            other => Expr::Not(Box::new(other.clone())),
        }
    }
}

/// Whether a printer writes the integer literal `value` in hexadecimal: when
/// it is a direct operand of `&`, `|`, or `^` (a mask or a flag) and greater
/// than 9. Every printer follows this rule, so `b0 & 0x80` and
/// `value & 0xffffffff` read the same in each language.
pub fn hex_literal(op: BinOp, value: i64) -> bool {
    matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) && value > 9
}

/// The spelling of a literal wire byte (a `Write` of `U8` or a repeat-until
/// terminator) when `e` is an integer literal in `0..=255`: hexadecimal above
/// 9 like masks (`0xFF`, or `0xff` when `upper` is false), decimal otherwise.
/// `None` for anything else.
pub fn byte_literal(e: &Expr, upper: bool) -> Option<String> {
    match e {
        Expr::Int { value, .. } if (0..=255).contains(value) => Some(match *value {
            0..=9 => value.to_string(),
            _ if upper => format!("0x{value:X}"),
            _ => format!("0x{value:x}"),
        }),
        _ => None,
    }
}

/// Whether control never falls off the end of `stmts`: they end in a
/// `Return` or a `Fail`, or in an `If` with an `else` or a `Match` whose every
/// branch diverges. A `Match` without `other` diverges when its arms do:
/// printers give it a default that fails.
pub fn diverges(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return(_) | Stmt::Fail(_)) => true,
        Some(Stmt::If { then, els, .. }) => !els.is_empty() && diverges(then) && diverges(els),
        Some(Stmt::Match { arms, other, .. }) => {
            other.as_deref().is_none_or(diverges) && arms.iter().all(|arm| diverges(&arm.body))
        }
        _ => false,
    }
}
