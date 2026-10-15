//! The importer's output tree and its BPS text printer.
//!
//! Fields are identified by [`FieldId`], not by name: names are assigned after a whole packet
//! or encoding is lowered (see `naming`), because the right name of a field depends on which
//! other fields the nested scopes below it refer to.

pub type FieldId = usize;

#[derive(Debug, Clone, PartialEq)]
pub enum IExpr {
    Int(i128),
    Bool(bool),
    Str(String),
    Field(FieldId),
    Not(Box<IExpr>),
    Bin(&'static str, Box<IExpr>, Box<IExpr>),
    Tern(Box<IExpr>, Box<IExpr>, Box<IExpr>),
    Call(&'static str, Vec<IExpr>),
}

impl IExpr {
    pub fn bin(op: &'static str, l: IExpr, r: IExpr) -> IExpr {
        IExpr::Bin(op, Box::new(l), Box::new(r))
    }

    /// Every field this expression reads.
    pub fn fields(&self, out: &mut Vec<FieldId>) {
        match self {
            IExpr::Field(f) => out.push(*f),
            IExpr::Not(e) => e.fields(out),
            IExpr::Bin(_, l, r) => {
                l.fields(out);
                r.fields(out);
            }
            IExpr::Tern(c, a, b) => {
                c.fields(out);
                a.fields(out);
                b.fields(out);
            }
            IExpr::Call(_, args) => args.iter().for_each(|a| a.fields(out)),
            IExpr::Int(_) | IExpr::Bool(_) | IExpr::Str(_) => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IType {
    Name(String),
    /// `bytes(n)`
    Bytes(IExpr),
    /// `bytes(n) as utf8`
    Utf8(IExpr),
    Block(IBlock),
    Match(Box<IMatch>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IBlock {
    pub items: Vec<IItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IRepeat {
    Count(IExpr),
    Until(i128),
}

#[derive(Debug, Clone, PartialEq)]
pub enum IItem {
    Field {
        fid: FieldId,
        ty: IType,
        repeat: Option<IRepeat>,
    },
    Computed {
        fid: FieldId,
        expr: IExpr,
    },
    If {
        cond: IExpr,
        items: Vec<IItem>,
    },
    MatchStmt(IMatch),
    Spread(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct IMatch {
    pub tag: IExpr,
    pub arms: Vec<IArm>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IArm {
    /// Empty means the `_` arm.
    pub tags: Vec<i128>,
    pub body: IType,
    /// Printed as a trailing comment (the protodef name of the arm).
    pub note: Option<String>,
}

/// A named encoding: an alias of a type or a block of items.
#[derive(Debug, Clone, PartialEq)]
pub enum EncBody {
    Alias(String),
    Block(IBlock),
}

pub struct Printer<'a> {
    names: &'a [String],
    out: String,
}

impl<'a> Printer<'a> {
    pub fn new(names: &'a [String]) -> Printer<'a> {
        Printer {
            names,
            out: String::new(),
        }
    }

    pub fn finish(self) -> String {
        self.out
    }

    pub fn raw(&mut self, text: &str) {
        self.out.push_str(text);
    }

    fn pad(&mut self, depth: usize) {
        for _ in 0..depth {
            self.out.push_str("    ");
        }
    }

    pub fn expr(&self, e: &IExpr) -> String {
        match e {
            IExpr::Int(n) => n.to_string(),
            IExpr::Bool(b) => b.to_string(),
            IExpr::Str(s) => quote(s),
            IExpr::Field(f) => self.names[*f].clone(),
            IExpr::Not(inner) => format!("!{}", self.operand(inner)),
            IExpr::Bin(op, l, r) => format!("{} {op} {}", self.operand(l), self.operand(r)),
            IExpr::Tern(c, a, b) => {
                format!(
                    "{} ? {} : {}",
                    self.operand(c),
                    self.operand(a),
                    self.operand(b)
                )
            }
            IExpr::Call(name, args) => {
                let args: Vec<String> = args.iter().map(|a| self.expr(a)).collect();
                format!("{name}({})", args.join(", "))
            }
        }
    }

    /// An operand: compound expressions are parenthesised so the printed text never depends on
    /// BPS precedence (shifts bind looser than `&`, unlike C).
    fn operand(&self, e: &IExpr) -> String {
        match e {
            IExpr::Bin(..) | IExpr::Tern(..) => format!("({})", self.expr(e)),
            _ => self.expr(e),
        }
    }

    pub fn items(&mut self, items: &[IItem], depth: usize) {
        for item in items {
            self.item(item, depth);
        }
    }

    fn item(&mut self, item: &IItem, depth: usize) {
        match item {
            IItem::Field { fid, ty, repeat } => {
                self.pad(depth);
                self.out.push_str(&self.names[*fid].clone());
                self.out.push_str(": ");
                self.ty(ty, depth);
                match repeat {
                    Some(IRepeat::Count(e)) => {
                        let e = self.expr(e);
                        self.out.push_str(&format!("[] count = {e}"));
                    }
                    Some(IRepeat::Until(t)) => self.out.push_str(&format!("[] until {t:#04x}")),
                    None => {}
                }
                self.out.push('\n');
            }
            IItem::Computed { fid, expr } => {
                self.pad(depth);
                let e = self.expr(expr);
                self.out.push_str(&format!("{} = {e}\n", self.names[*fid]));
            }
            IItem::If { cond, items } => {
                self.pad(depth);
                let c = self.expr(cond);
                self.out.push_str(&format!("if {c} {{\n"));
                self.items(items, depth + 1);
                self.pad(depth);
                self.out.push_str("}\n");
            }
            IItem::MatchStmt(m) => {
                self.pad(depth);
                self.match_body(m, depth);
                self.out.push('\n');
            }
            IItem::Spread(name) => {
                self.pad(depth);
                self.out.push_str(&format!("...{name}\n"));
            }
        }
    }

    fn match_body(&mut self, m: &IMatch, depth: usize) {
        let tag = self.expr(&m.tag);
        self.out.push_str(&format!("match {tag} {{\n"));
        for arm in &m.arms {
            self.pad(depth + 1);
            if arm.tags.is_empty() {
                self.out.push('_');
            } else {
                let tags: Vec<String> = arm.tags.iter().map(i128::to_string).collect();
                self.out.push_str(&tags.join(" | "));
            }
            self.out.push_str(" => ");
            self.ty(&arm.body, depth + 1);
            if let Some(note) = &arm.note {
                self.out
                    .push_str(&format!("    // {}", note.replace('\n', " ")));
            }
            self.out.push('\n');
        }
        self.pad(depth);
        self.out.push('}');
    }

    pub fn ty(&mut self, ty: &IType, depth: usize) {
        match ty {
            IType::Name(n) => self.out.push_str(n),
            IType::Bytes(e) => {
                let e = self.expr(e);
                self.out.push_str(&format!("bytes({e})"));
            }
            IType::Utf8(e) => {
                let e = self.expr(e);
                self.out.push_str(&format!("bytes({e}) as utf8"));
            }
            IType::Block(b) => {
                if b.items.is_empty() {
                    self.out.push_str("{ }");
                } else {
                    self.out.push_str("{\n");
                    self.items(&b.items, depth + 1);
                    self.pad(depth);
                    self.out.push('}');
                }
            }
            IType::Match(m) => self.match_body(m, depth),
        }
    }
}

/// A BPS string literal: the lexer takes `\x` as the character `x`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}
