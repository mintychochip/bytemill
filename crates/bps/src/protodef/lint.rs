//! A name-flow lint for generated schemas.
//!
//! `analyze` checks that type names resolve; an identifier in an expression (a `match` tag, an
//! `if` guard, a `count`) is only looked up when a packet is decoded, so a dangling one would
//! surface as a decode error on some packet in some version. The lint walks the parsed schema
//! the way the interpreter binds names (a nested block inherits the enclosing names; `if` and
//! `match` statement arms splice what they declare into the enclosing block; a named encoding
//! starts empty, but a `...spread` runs in the spreading block) and reports every identifier that
//! is not bound by then.

use std::collections::{HashMap, HashSet};

use crate::ast::{BlockItem, EncBody, EncodingDecl, Expr, FieldType, MatchType, Schema};

type Names = HashSet<String>;

struct Linter<'a> {
    encodings: HashMap<&'a str, &'a EncodingDecl>,
    errors: Vec<String>,
}

/// Every unbound identifier in `schema`, as `where: message` lines.
pub fn lint(schema: &Schema) -> Vec<String> {
    let mut l = Linter {
        encodings: schema
            .encodings
            .iter()
            .map(|e| (e.name.as_str(), e))
            .collect(),
        errors: Vec::new(),
    };
    let mut spread_targets: HashSet<&str> = HashSet::new();
    for e in &schema.encodings {
        if let EncBody::Block(b) = &e.body {
            collect_spreads(&b.items, &mut spread_targets);
        }
    }
    for p in &schema.packets {
        collect_spreads(&p.items, &mut spread_targets);
    }
    for e in &schema.encodings {
        // A spread target reads names its spreader binds; it is checked there.
        if spread_targets.contains(e.name.as_str()) {
            continue;
        }
        if let EncBody::Block(b) = &e.body {
            let mut names: Names = e.params.iter().map(|p| p.name.clone()).collect();
            l.items(&b.items, &mut names, &format!("encoding {}", e.name), 0);
        }
    }
    for p in &schema.packets {
        let mut names = Names::new();
        l.items(&p.items, &mut names, &format!("packet {}", p.name), 0);
    }
    l.errors
}

fn collect_spreads<'a>(items: &'a [BlockItem], out: &mut HashSet<&'a str>) {
    for item in items {
        match item {
            BlockItem::Spread { name, .. } => {
                out.insert(name.as_str());
            }
            BlockItem::Group { items, .. } => collect_spreads(items, out),
            BlockItem::MatchStmt(m) => {
                for arm in &m.arms {
                    if let Some(FieldType::Block(b)) = &arm.type_ref {
                        collect_spreads(&b.items, out);
                    }
                }
            }
            BlockItem::Field(wf) => {
                if let FieldType::Block(b) = &wf.type_ref {
                    collect_spreads(&b.items, out);
                }
            }
            BlockItem::Computed { .. } | BlockItem::Require(_) => {}
        }
    }
}

impl Linter<'_> {
    fn expr(&mut self, e: &Expr, names: &Names, at: &str) {
        match e {
            Expr::Ident(n) => self.bound(n, names, at),
            Expr::Path(p) => self.bound(p.split('.').next().unwrap_or(p), names, at),
            Expr::Unop(_, x) => self.expr(x, names, at),
            Expr::Binop(_, l, r) => {
                self.expr(l, names, at);
                self.expr(r, names, at);
            }
            Expr::Ternary(c, a, b) => {
                self.expr(c, names, at);
                self.expr(a, names, at);
                self.expr(b, names, at);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| self.expr(a, names, at)),
            Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) => {}
        }
    }

    fn bound(&mut self, name: &str, names: &Names, at: &str) {
        if !names.contains(name) {
            self.errors
                .push(format!("{at}: identifier {name:?} is not bound here"));
        }
    }

    fn items(&mut self, items: &[BlockItem], names: &mut Names, at: &str, depth: usize) {
        for item in items {
            match item {
                BlockItem::Field(wf) => {
                    if let Some(c) = &wf.if_expr {
                        self.expr(c, names, at);
                    }
                    if let Some(r) = &wf.repeat
                        && let Some(e) = &r.expr
                    {
                        self.expr(e, names, at);
                    }
                    self.field_type(&wf.type_ref, names, at, depth);
                    names.insert(wf.name.clone());
                }
                BlockItem::Computed { name, expr } => {
                    self.expr(expr, names, at);
                    names.insert(name.clone());
                }
                BlockItem::Require(e) => self.expr(e, names, at),
                BlockItem::Spread { name, args } => {
                    for a in args {
                        self.expr(a, names, at);
                    }
                    // A spread runs in the spreading block. Recursion through spreads would not
                    // terminate; the generated schemas have none, and the depth cap says so.
                    if depth > 16 {
                        self.errors.push(format!("{at}: spreads nest too deeply"));
                        continue;
                    }
                    if let Some(enc) = self.encodings.get(name.as_str()).copied()
                        && let EncBody::Block(b) = &enc.body
                    {
                        self.items(&b.items, names, &format!("{at} (spread {name})"), depth + 1);
                    }
                }
                BlockItem::Group { cond, items } => {
                    if let Some(c) = cond {
                        self.expr(c, names, at);
                    }
                    let mut inner = names.clone();
                    self.items(items, &mut inner, at, depth);
                    names.extend(inner);
                }
                BlockItem::MatchStmt(m) => self.match_stmt(m, names, at, depth),
            }
        }
    }

    fn match_stmt(&mut self, m: &MatchType, names: &mut Names, at: &str, depth: usize) {
        self.expr(&m.tag_expr, names, at);
        let mut declared = Names::new();
        for arm in &m.arms {
            if let Some(FieldType::Block(b)) = &arm.type_ref {
                let mut inner = names.clone();
                self.items(&b.items, &mut inner, at, depth);
                declared.extend(inner);
            }
        }
        names.extend(declared);
    }

    fn field_type(&mut self, t: &FieldType, names: &Names, at: &str, depth: usize) {
        match t {
            FieldType::Ref(r) => {
                for a in &r.args {
                    self.expr(a, names, at);
                }
            }
            FieldType::Block(b) => {
                let mut inner = names.clone();
                self.items(&b.items, &mut inner, at, depth);
            }
            FieldType::Match(m) => {
                self.expr(&m.tag_expr, names, at);
                for arm in &m.arms {
                    if let Some(body) = &arm.type_ref {
                        self.field_type(body, names, at, depth);
                    }
                }
            }
        }
    }
}
