//! AST, a faithful port of `bps/ast.py`.
//!
//! Python's `Expr`/`BlockItem` are tag-and-fields dataclasses; here they are
//! enums, which is the same information with exhaustive matching.

use crate::errors::py_repr_str;
use crate::ordmap::OrderedMap;

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub default: Option<i128>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i128),
    Bool(bool),
    Str(String),
    Ident(String),
    /// A dotted path `a.b.c`; `eval` walks the env one segment at a time.
    Path(String),
    Unop(String, Box<Expr>),
    Binop(String, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

impl Expr {
    /// Python's `repr()` of the corresponding `bps.ast.Expr` dataclass, which is what the
    /// interpreter's `require failed: ...` message shows:
    /// `Expr(kind='binop', value='==', left=Expr(...), right=Expr(...), args=(), name=None)`.
    pub fn py_repr(&self) -> String {
        const NONE: &str = "None";
        fn node(kind: &str, value: &str, left: &str, right: &str, args: &str, name: &str) -> String {
            format!(
                "Expr(kind='{kind}', value={value}, left={left}, right={right}, args={args}, name={name})"
            )
        }
        // A tuple's repr: `()`, `(x,)`, `(x, y)`.
        fn tuple(items: &[String]) -> String {
            match items {
                [] => "()".to_string(),
                [only] => format!("({only},)"),
                many => format!("({})", many.join(", ")),
            }
        }
        match self {
            Expr::Int(v) => node("int", &v.to_string(), NONE, NONE, "()", NONE),
            Expr::Bool(b) => node("bool", if *b { "True" } else { "False" }, NONE, NONE, "()", NONE),
            Expr::Str(s) => node("string", &py_repr_str(s), NONE, NONE, "()", NONE),
            Expr::Ident(name) => node("ident", NONE, NONE, NONE, "()", &py_repr_str(name)),
            Expr::Path(path) => node("path", &py_repr_str(path), NONE, NONE, "()", NONE),
            Expr::Unop(op, operand) => {
                node("unop", &py_repr_str(op), &operand.py_repr(), NONE, "()", NONE)
            }
            Expr::Binop(op, left, right) => node(
                "binop",
                &py_repr_str(op),
                &left.py_repr(),
                &right.py_repr(),
                "()",
                NONE,
            ),
            Expr::Ternary(cond, yes, no) => node(
                "ternary",
                NONE,
                &cond.py_repr(),
                &yes.py_repr(),
                &tuple(&[no.py_repr()]),
                NONE,
            ),
            Expr::Call(name, args) => {
                let args: Vec<String> = args.iter().map(Expr::py_repr).collect();
                node("call", NONE, NONE, NONE, &tuple(&args), &py_repr_str(name))
            }
        }
    }
}

/// A match-arm tag: an int or a (never-matched) string label.
#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Int(i128),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeRef {
    pub name: String,
    pub args: Vec<Expr>,
    pub transforms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldType {
    Ref(TypeRef),
    Block(BlockType),
    Match(MatchType),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    /// `None` is the `_` default arm; the first tag when `tags` is set.
    pub tag: Option<Tag>,
    pub type_ref: Option<FieldType>,
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockType {
    pub items: Vec<BlockItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatMode {
    Count,
    Until,
    UntilEnd,
}

impl RepeatMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RepeatMode::Count => "count",
            RepeatMode::Until => "until",
            RepeatMode::UntilEnd => "until_end",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Repeat {
    pub mode: RepeatMode,
    pub expr: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WireField {
    pub name: String,
    pub type_ref: FieldType,
    pub if_expr: Option<Expr>,
    pub repeat: Option<Repeat>,
    pub encode_expr: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchType {
    pub tag_expr: Expr,
    pub arms: Vec<MatchArm>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockItem {
    /// Boxed: a wire field is several times larger than any other item.
    Field(Box<WireField>),
    Computed { name: String, expr: Expr },
    Require(Expr),
    Spread { name: String, args: Vec<Expr> },
    Group { cond: Option<Expr>, items: Vec<BlockItem> },
    /// Statement `match`/`switch`: arms are blocks whose fields splice in.
    MatchStmt(MatchType),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EncBody {
    Ref(TypeRef),
    Block(BlockType),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncodingDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub body: EncBody,
    pub yield_expr: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumMember {
    pub name: String,
    pub value: i128,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    pub name: String,
    pub base: String,
    pub members: Vec<EnumMember>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PacketDecl {
    pub name: String,
    pub items: Vec<BlockItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameDecl {
    pub name: String,
    pub items: Vec<BlockItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteDecl {
    pub state: String,
    pub direction: String,
    pub packet_id: i128,
    pub packet_name: String,
    pub frame: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolDecl {
    pub name: String,
    pub version: String,
    pub protocol_id: i128,
    pub frames: Vec<FrameDecl>,
    pub routes: Vec<RouteDecl>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Schema {
    pub encodings: Vec<EncodingDecl>,
    pub enums: Vec<EnumDecl>,
    pub packets: Vec<PacketDecl>,
    pub protocols: Vec<ProtocolDecl>,
    pub imports: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedSchema {
    pub schema: Schema,
    pub encodings: OrderedMap<EncodingDecl>,
    pub enums: OrderedMap<EnumDecl>,
    pub packets: OrderedMap<PacketDecl>,
    pub protocols: OrderedMap<ProtocolDecl>,
    pub routes: Vec<(RouteDecl, PacketDecl)>,
    /// The emit_rstom port's cached frame plan. Python stashes the same thing on
    /// the object as `resolved._bps_frames`; the field lives here because the
    /// resolved schema is the only value threaded through every emitter helper.
    pub frames_cache: std::cell::RefCell<
        Option<std::rc::Rc<crate::emit_rstom::FramesPlan>>,
    >,
}

impl ResolvedSchema {
    pub fn encoding(&self, name: &str) -> Option<&EncodingDecl> {
        self.encodings.get(name)
    }
}

/// Python's `arm.tag is None and not arm.tags` — the `_` fallback arm.
pub fn is_default_arm(arm: &MatchArm) -> bool {
    arm.tag.is_none() && arm.tags.is_empty()
}

/// The int tags an arm matches; string labels never match a numeric tag.
pub fn arm_tag_list(arm: &MatchArm) -> &[Tag] {
    if !arm.tags.is_empty() {
        &arm.tags
    } else if arm.tag.is_none() {
        &[]
    } else {
        std::slice::from_ref(arm.tag.as_ref().unwrap())
    }
}

/// First arm whose int tags include `tag`; `_` is the fallback wherever it sits.
pub fn select_match_arm(mt: &MatchType, tag: i128) -> Option<&MatchArm> {
    let mut default: Option<&MatchArm> = None;
    for arm in &mt.arms {
        if is_default_arm(arm) {
            default = Some(arm);
            continue;
        }
        for arm_tag in arm_tag_list(arm) {
            if let Tag::Int(v) = arm_tag
                && *v == tag
            {
                return Some(arm);
            }
        }
    }
    default
}
