//! A readable pseudo-code rendering of codecs, for tests and for printer
//! authors inspecting what a printer must emit.

use std::fmt::Write;

use super::{Bind, Body, Codec, Expr, Program, ReadOp, RepeatMode, Stmt, Ty, TypeKind, WriteOp};

impl Program {
    /// Every type and codec, in order.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for t in 0..self.types.len() {
            out.push_str(&self.dump_type(t));
        }
        for c in 0..self.codecs.len() {
            out.push_str(&self.dump_codec(c));
        }
        out
    }

    /// One type declaration.
    pub fn dump_type(&self, idx: usize) -> String {
        let t = &self.types[idx];
        let mut out = String::new();
        match &t.kind {
            TypeKind::Record { fields } => {
                let _ = writeln!(out, "record {} {{", t.name.pascal());
                for f in fields {
                    let _ = writeln!(
                        out,
                        "    {}: {}{}",
                        f.name.snake(),
                        self.ty_name(&f.ty),
                        if f.optional { "?" } else { "" }
                    );
                }
                out.push_str("}\n");
            }
            TypeKind::Sum { variants } => {
                let _ = writeln!(out, "sum {} {{", t.name.pascal());
                for v in variants {
                    let payload = v
                        .payload
                        .as_ref()
                        .map(|p| format!("({})", self.ty_name(p)))
                        .unwrap_or_default();
                    let _ = writeln!(out, "    {}{payload} = {:?}", v.name.pascal(), v.tags);
                }
                out.push_str("}\n");
            }
            TypeKind::Enum { base, members } => {
                let _ = writeln!(out, "enum {}: {base:?} {{", t.name.pascal());
                for m in members {
                    let _ = writeln!(out, "    {} = {}", m.name.pascal(), m.value);
                }
                out.push_str("}\n");
            }
            TypeKind::Alias { target } => {
                let _ = writeln!(
                    out,
                    "alias {} = {}",
                    t.name.pascal(),
                    self.types[*target].name.pascal()
                );
            }
        }
        out
    }

    /// One codec's decode and encode functions.
    pub fn dump_codec(&self, idx: usize) -> String {
        let c = &self.codecs[idx];
        let mut out = String::new();
        let _ = writeln!(
            out,
            "decode_{}({}) -> {} {{",
            c.name.snake(),
            self.params(c, &c.decode),
            self.ty_name(&c.value)
        );
        self.stmts(&c.decode, &c.decode.stmts, 1, &mut out);
        out.push_str("}\n");
        let value = c
            .encode
            .value
            .map(|v| self.local(&c.encode, v))
            .unwrap_or_default();
        let sep = if c.params.is_empty() { "" } else { ", " };
        let _ = writeln!(
            out,
            "encode_{}({value}: {}{sep}{}) {{",
            c.name.snake(),
            self.ty_name(&c.value),
            self.params(c, &c.encode)
        );
        self.stmts(&c.encode, &c.encode.stmts, 1, &mut out);
        out.push_str("}\n");
        out
    }

    fn params(&self, c: &Codec, body: &Body) -> String {
        c.params
            .iter()
            .zip(&body.params)
            .map(|(p, l)| {
                format!(
                    "{}: {}{}",
                    self.local(body, *l),
                    self.ty_name(&p.ty),
                    if p.optional { "?" } else { "" }
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn ty_name(&self, ty: &Ty) -> String {
        match ty {
            Ty::Int(t) => format!("{t:?}").to_ascii_lowercase(),
            Ty::Bool => "bool".into(),
            Ty::F32 => "f32".into(),
            Ty::F64 => "f64".into(),
            Ty::Str => "str".into(),
            Ty::Bytes => "bytes".into(),
            Ty::List(inner) => format!("[{}]", self.ty_name(inner)),
            Ty::Named(t) => self.types[*t].name.pascal(),
        }
    }

    fn local(&self, body: &Body, id: usize) -> String {
        body.locals[id].name.snake()
    }

    fn bind(&self, body: &Body, b: &Bind) -> String {
        let l = &body.locals[b.local];
        let name = l.name.snake();
        let decl = if b.declare {
            format!(
                "let {name}: {}{}",
                self.ty_name(&l.ty),
                if l.optional { "?" } else { "" }
            )
        } else {
            name
        };
        if b.some {
            format!("{decl} = some")
        } else {
            format!("{decl} =")
        }
    }

    fn stmts(&self, body: &Body, stmts: &[Stmt], depth: usize, out: &mut String) {
        let pad = "    ".repeat(depth);
        for s in stmts {
            match s {
                Stmt::Declare { local, zero } => {
                    let l = &body.locals[*local];
                    let _ = writeln!(
                        out,
                        "{pad}let {}: {}{}{}",
                        l.name.snake(),
                        self.ty_name(&l.ty),
                        if l.optional { "?" } else { "" },
                        if *zero { " = zero" } else { "" }
                    );
                }
                Stmt::Let { dst, value } => {
                    let _ = writeln!(
                        out,
                        "{pad}{} {}",
                        self.bind(body, dst),
                        self.expr(body, value)
                    );
                }
                Stmt::Read { dst, op } => {
                    let op = match op {
                        ReadOp::U8 => "u8".to_string(),
                        ReadOp::F32 => "f32".into(),
                        ReadOp::F64 => "f64".into(),
                        ReadOp::Bytes(n) => format!("bytes({})", self.expr(body, n)),
                        ReadOp::Rest => "rest".into(),
                    };
                    let _ = writeln!(out, "{pad}{} read {op}", self.bind(body, dst));
                }
                Stmt::Decode { dst, codec, args } => {
                    let _ = writeln!(
                        out,
                        "{pad}{} decode_{}({})",
                        self.bind(body, dst),
                        self.codecs[*codec].name.snake(),
                        self.args(body, args)
                    );
                }
                Stmt::Utf8 { dst, src } => {
                    let _ = writeln!(
                        out,
                        "{pad}{} utf8({})",
                        self.bind(body, dst),
                        self.expr(body, src)
                    );
                }
                Stmt::Write { op, value } => {
                    let op = match op {
                        WriteOp::U8 => "u8".to_string(),
                        WriteOp::F32 => "f32".into(),
                        WriteOp::F64 => "f64".into(),
                        WriteOp::Bytes(n) => format!("bytes({})", self.expr(body, n)),
                        WriteOp::Rest => "rest".into(),
                    };
                    let _ = writeln!(out, "{pad}write {op} {}", self.expr(body, value));
                }
                Stmt::Encode { codec, args, value } => {
                    let sep = if args.is_empty() { "" } else { ", " };
                    let _ = writeln!(
                        out,
                        "{pad}encode_{}({}{sep}{})",
                        self.codecs[*codec].name.snake(),
                        self.expr(body, value),
                        self.args(body, args)
                    );
                }
                Stmt::Require { cond, text } => {
                    let _ = writeln!(out, "{pad}require {}  # {text}", self.expr(body, cond));
                }
                Stmt::Unwrap { dst, src, field } => {
                    let _ = writeln!(
                        out,
                        "{pad}{} unwrap {}  # missing {field}",
                        self.bind(body, dst),
                        self.expr(body, src)
                    );
                }
                Stmt::If { cond, then, els } => {
                    let _ = writeln!(out, "{pad}if {} {{", self.expr(body, cond));
                    self.stmts(body, then, depth + 1, out);
                    if !els.is_empty() {
                        let _ = writeln!(out, "{pad}}} else {{");
                        self.stmts(body, els, depth + 1, out);
                    }
                    let _ = writeln!(out, "{pad}}}");
                }
                Stmt::Match {
                    scrutinee,
                    arms,
                    other,
                } => {
                    let _ = writeln!(out, "{pad}match {} {{", self.expr(body, scrutinee));
                    for a in arms {
                        let _ = writeln!(out, "{pad}    {:?} => {{", a.tags);
                        self.stmts(body, &a.body, depth + 2, out);
                        let _ = writeln!(out, "{pad}    }}");
                    }
                    if let Some(o) = other {
                        let _ = writeln!(out, "{pad}    _ => {{");
                        self.stmts(body, o, depth + 2, out);
                        let _ = writeln!(out, "{pad}    }}");
                    }
                    let _ = writeln!(out, "{pad}}}");
                }
                Stmt::Repeat {
                    mode,
                    list,
                    item,
                    body: inner,
                } => {
                    let mode = match mode {
                        RepeatMode::Count(n) => format!("count {}", self.expr(body, n)),
                        RepeatMode::Until(t) => format!("until {}", self.expr(body, t)),
                        RepeatMode::UntilEnd => "until end".into(),
                    };
                    let _ = writeln!(
                        out,
                        "{pad}let {}: {} = repeat {mode} {{ {} }}",
                        self.local(body, *list),
                        self.ty_name(&body.locals[*list].ty),
                        self.local(body, *item)
                    );
                    self.stmts(body, inner, depth + 1, out);
                    let _ = writeln!(out, "{pad}}}");
                }
                Stmt::ForEach {
                    list,
                    item,
                    body: inner,
                } => {
                    let _ = writeln!(
                        out,
                        "{pad}for {} in {} {{",
                        self.local(body, *item),
                        self.expr(body, list)
                    );
                    self.stmts(body, inner, depth + 1, out);
                    let _ = writeln!(out, "{pad}}}");
                }
                Stmt::ExpectVariant {
                    dst,
                    sum,
                    ty,
                    variant,
                } => {
                    let name = self.variants(*ty)[*variant].name.pascal();
                    match dst {
                        Some(d) => {
                            let _ = writeln!(
                                out,
                                "{pad}{} expect {name}({})",
                                self.bind(body, d),
                                self.expr(body, sum)
                            );
                        }
                        None => {
                            let _ = writeln!(out, "{pad}expect {name}({})", self.expr(body, sum));
                        }
                    }
                }
                Stmt::Return(e) => {
                    let _ = writeln!(out, "{pad}return {}", self.expr(body, e));
                }
                Stmt::Fail(f) => {
                    let _ = writeln!(out, "{pad}fail {f:?}");
                }
            }
        }
    }

    fn args(&self, body: &Body, args: &[Expr]) -> String {
        args.iter()
            .map(|a| self.expr(body, a))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn expr(&self, body: &Body, e: &Expr) -> String {
        match e {
            Expr::Int { value, ty } => format!("{value}{}", format!("{ty:?}").to_ascii_lowercase()),
            Expr::Bool(b) => b.to_string(),
            Expr::Str(s) => format!("{s:?}"),
            Expr::Local(l) => self.local(body, *l),
            Expr::Field { base, ty, field } => {
                format!(
                    "{}.{}",
                    self.expr(body, base),
                    self.fields(*ty)[*field].name.snake()
                )
            }
            Expr::Present(inner) => format!("present({})", self.expr(body, inner)),
            Expr::Get(inner) => format!("get({})", self.expr(body, inner)),
            Expr::Not(inner) => format!("!{}", self.expr(body, inner)),
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } => format!(
                "({} {}{} {})",
                self.expr(body, lhs),
                op.symbol(),
                if *wraps {
                    format!("~{}", self.ty_name(ty))
                } else {
                    String::new()
                },
                self.expr(body, rhs)
            ),
            Expr::Cond {
                cond, then, els, ..
            } => {
                format!(
                    "({} ? {} : {})",
                    self.expr(body, cond),
                    self.expr(body, then),
                    self.expr(body, els)
                )
            }
            Expr::Cast { expr, to } => format!("{}({})", self.ty_name(to), self.expr(body, expr)),
            Expr::Call { func, args } => format!("{func:?}({})", self.args(body, args)),
            Expr::StrBytes(inner) => format!("bytes_of({})", self.expr(body, inner)),
            Expr::Remaining => "remaining".into(),
            Expr::Construct { ty, fields } => {
                let parts = self
                    .fields(*ty)
                    .iter()
                    .zip(fields)
                    .map(|(f, e)| format!("{}: {}", f.name.snake(), self.expr(body, e)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{ {parts} }}", self.types[*ty].name.pascal())
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                let name = format!(
                    "{}::{}",
                    self.types[*ty].name.pascal(),
                    self.variants(*ty)[*variant].name.pascal()
                );
                match payload {
                    Some(p) => format!("{name}({})", self.expr(body, p)),
                    None => name,
                }
            }
        }
    }
}
