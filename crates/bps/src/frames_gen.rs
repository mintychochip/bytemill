//! Frame walker generation, a port of `bps/frames_gen.py`.
//!
//! Compiles, for each root encoding, a Rust fn that skips one value of that
//! encoding at the front of a byte slice and reports its length.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::{
    BlockItem, EncBody, EncodingDecl, Expr, FieldType, MatchArm, MatchType, RepeatMode,
    Repeat, ResolvedSchema, Tag, TypeRef, WireField,
};
use crate::errors::{emit, py_repr_str, BpsResult};
use crate::ordmap::OrderedMap;

/// Fixed-width scalars: byte size.
const FIXED: &[(&str, usize)] = &[
    ("u8", 1),
    ("i8", 1),
    ("boolean", 1),
    ("angle", 1),
    ("u16be", 2),
    ("i16be", 2),
    ("u32be", 4),
    ("i32be", 4),
    ("f32be", 4),
    ("u64be", 8),
    ("i64be", 8),
    ("f64be", 8),
    ("position", 8),
    ("uuid", 16),
];
const SIGNED: &[&str] = &["i8", "i16be", "i32be", "i64be"];
/// Variable-width integers: most bytes they may take.
const VAR: &[(&str, usize)] = &[("varint", 5), ("varlong", 10)];
/// A varint length, then that many bytes.
const STRINGS: &[&str] = &["string", "identifier"];

const PRELUDE: &str = r#"
fn walk_skip(bytes: &[u8], pos: &mut usize, n: usize) -> Option<()> {
    let end = pos.checked_add(n)?;
    if end > bytes.len() {
        return None;
    }
    *pos = end;
    Some(())
}

/// A count read from the wire can only be honest if that many bytes remain.
fn walk_count(n: i64, bytes: &[u8]) -> Option<usize> {
    usize::try_from(n).ok().filter(|n| *n <= bytes.len())
}

fn walk_be(bytes: &[u8], pos: &mut usize, n: usize, signed: bool) -> Option<i64> {
    let end = pos.checked_add(n)?;
    let chunk = bytes.get(*pos..end)?;
    let mut value: i64 = 0;
    for byte in chunk {
        value = (value << 8) | i64::from(*byte);
    }
    if signed && n < 8 && chunk[0] & 0x80 != 0 {
        value -= 1i64 << (8 * n);
    }
    *pos = end;
    Some(value)
}

/// A LEB128 integer of at most `max` bytes; a 5-byte one is a signed 32-bit VarInt.
fn walk_var(bytes: &[u8], pos: &mut usize, max: usize) -> Option<i64> {
    let mut acc: u64 = 0;
    for i in 0..max {
        let byte = *bytes.get(*pos + i)?;
        acc |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            *pos += i + 1;
            return Some(if max == 5 { i64::from(acc as u32 as i32) } else { acc as i64 });
        }
    }
    None
}

/// A payload another walker already measures.
fn walk_hand(bytes: &[u8], pos: &mut usize, len: fn(&[u8]) -> usize) -> Option<()> {
    let rest = bytes.get(*pos..)?;
    if rest.is_empty() {
        return None;
    }
    *pos += len(rest);
    Some(())
}

fn walk_div(a: i64, b: i64) -> i64 {
    if b == 0 { 0 } else { a.div_euclid(b) }
}

fn walk_rem(a: i64, b: i64) -> i64 {
    if b == 0 { 0 } else { a.rem_euclid(b) }
}

fn walk_shl(a: i64, b: i64) -> i64 {
    u32::try_from(b).ok().and_then(|b| a.checked_shl(b)).unwrap_or(0)
}

fn walk_shr(a: i64, b: i64) -> i64 {
    u32::try_from(b).ok().and_then(|b| a.checked_shr(b)).unwrap_or(0)
}
"#;

/// The prelude's helper fns, by name: a generated crate keeps only those it calls.
/// Each is its contiguous `///` doc run plus the fn through the first line that is `}`.
fn helpers() -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    let mut docs_start: Option<usize> = None;
    let mut current: Option<(usize, &'static str)> = None;
    let mut pos = 0usize;
    for line in PRELUDE.split_inclusive('\n') {
        let start = pos;
        pos += line.len();
        let bare = line.trim_end_matches('\n');
        if let Some((s, name)) = current {
            if bare == "}" {
                out.push((name, &PRELUDE[s..pos]));
                current = None;
            }
            continue;
        }
        if bare.starts_with("///") {
            docs_start.get_or_insert(start);
            continue;
        }
        if let Some(rest) = bare.strip_prefix("fn ") {
            if let Some(end) = rest.find('(') {
                current = Some((docs_start.take().unwrap_or(start), &rest[..end]));
                continue;
            }
        }
        docs_start = None;
    }
    out
}

pub fn walker_name(encoding: &str) -> String {
    format!("{encoding}_len")
}

fn skip_name(encoding: &str) -> String {
    format!("skip_{encoding}")
}

fn fixed_size(name: &str) -> Option<usize> {
    FIXED.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

fn var_max(name: &str) -> Option<usize> {
    VAR.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

fn is_builtin(name: &str) -> bool {
    fixed_size(name).is_some() || var_max(name).is_some() || STRINGS.contains(&name)
}

/// Follow `encoding a = b` aliases to the encoding that has a body.
fn alias_target(resolved: &ResolvedSchema, name: &str) -> String {
    let mut name = name.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    while !seen.contains(&name) {
        seen.insert(name.clone());
        let Some(enc) = resolved.encodings.get(&name) else { break };
        let EncBody::Ref(tr) = &enc.body else { break };
        if !tr.args.is_empty() || is_builtin(&tr.name) {
            break;
        }
        name = tr.name.clone();
    }
    name
}

/// Every expression one block item evaluates, its own and its nested blocks'.
fn exprs_of_item<'a>(item: &'a BlockItem, out: &mut Vec<&'a Expr>) {
    match item {
        BlockItem::Computed { expr, .. } => out.push(expr),
        BlockItem::Group { cond, items } => {
            if let Some(c) = cond {
                out.push(c);
            }
            for sub in items {
                exprs_of_item(sub, out);
            }
        }
        BlockItem::MatchStmt(mt) => exprs_of_match(mt, out),
        BlockItem::Field(wf) => {
            if let Some(e) = &wf.if_expr {
                out.push(e);
            }
            if let Some(Repeat { expr: Some(e), .. }) = &wf.repeat {
                out.push(e);
            }
            exprs_of_ftype(&wf.type_ref, out);
        }
        BlockItem::Require(_) | BlockItem::Spread { .. } => {}
    }
}

fn exprs_of_match<'a>(mt: &'a MatchType, out: &mut Vec<&'a Expr>) {
    out.push(&mt.tag_expr);
    for arm in &mt.arms {
        if let Some(t) = &arm.type_ref {
            exprs_of_ftype(t, out);
        }
    }
}

fn exprs_of_ftype<'a>(t: &'a FieldType, out: &mut Vec<&'a Expr>) {
    match t {
        FieldType::Block(b) => {
            for sub in &b.items {
                exprs_of_item(sub, out);
            }
        }
        FieldType::Match(mt) => exprs_of_match(mt, out),
        FieldType::Ref(tr) => out.extend(tr.args.iter()),
    }
}

fn idents(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Ident(name) => {
            if !name.is_empty() {
                out.insert(name.clone());
            }
        }
        Expr::Unop(_, inner) => idents(inner, out),
        Expr::Binop(_, l, r) => {
            idents(l, out);
            idents(r, out);
        }
        Expr::Ternary(a, b, c) => {
            idents(a, out);
            idents(b, out);
            idents(c, out);
        }
        Expr::Call(_, args) => {
            for a in args {
                idents(a, out);
            }
        }
        Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Path(_) => {}
    }
}

/// Names a block declares in its own scope: fields, computeds, and those of
/// the groups and match arms that share it.
fn declared(items: &[BlockItem]) -> Vec<String> {
    let mut names = Vec::new();
    for item in items {
        match item {
            BlockItem::Field(wf) => {
                names.push(wf.name.clone());
                if let FieldType::Match(mt) = &wf.type_ref {
                    for arm in &mt.arms {
                        if let Some(FieldType::Block(b)) = &arm.type_ref {
                            names.extend(declared(&b.items));
                        }
                    }
                }
            }
            BlockItem::Computed { name, .. } => {
                if !name.is_empty() {
                    names.push(name.clone());
                }
            }
            BlockItem::Group { items, .. } => names.extend(declared(items)),
            BlockItem::MatchStmt(mt) => {
                for arm in &mt.arms {
                    if let Some(FieldType::Block(b)) = &arm.type_ref {
                        names.extend(declared(&b.items));
                    }
                }
            }
            BlockItem::Require(_) | BlockItem::Spread { .. } => {}
        }
    }
    names
}

fn walk_field_type(t: &FieldType, out: &mut BTreeSet<String>) {
    match t {
        FieldType::Ref(tr) => {
            out.insert(tr.name.clone());
        }
        FieldType::Block(b) => walk_items(&b.items, out),
        FieldType::Match(mt) => walk_match(mt, out),
    }
}

fn walk_match(mt: &MatchType, out: &mut BTreeSet<String>) {
    for arm in &mt.arms {
        if let Some(t) = &arm.type_ref {
            walk_field_type(t, out);
        }
    }
}

fn walk_items(items: &[BlockItem], out: &mut BTreeSet<String>) {
    for item in items {
        match item {
            BlockItem::Field(wf) => walk_field_type(&wf.type_ref, out),
            BlockItem::Group { items, .. } => walk_items(items, out),
            BlockItem::MatchStmt(mt) => walk_match(mt, out),
            BlockItem::Spread { name, .. } => {
                if !name.is_empty() {
                    out.insert(name.clone());
                }
            }
            BlockItem::Computed { .. } | BlockItem::Require(_) => {}
        }
    }
}

/// Named block encodings `name`'s body refers to.
fn references(resolved: &ResolvedSchema, name: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(enc) = resolved.encodings.get(name) else {
        return BTreeSet::new();
    };
    match &enc.body {
        EncBody::Ref(tr) => {
            out.insert(tr.name.clone());
        }
        EncBody::Block(b) => walk_items(&b.items, &mut out),
    }
    out.into_iter().filter(|n| resolved.encodings.contains_key(n)).collect()
}

/// Encodings that can reach themselves: they cannot be flattened into fields.
pub fn recursive_encodings(resolved: &ResolvedSchema) -> Vec<String> {
    let graph: BTreeMap<String, BTreeSet<String>> = resolved
        .encodings
        .keys()
        .map(|n| (n.to_string(), references(resolved, n)))
        .collect();
    let mut out: BTreeSet<String> = BTreeSet::new();
    for start in graph.keys() {
        let mut stack: Vec<String> = graph[start].iter().cloned().collect();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(cur) = stack.pop() {
            if &cur == start {
                out.insert(start.clone());
                break;
            }
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(next) = graph.get(&cur) {
                stack.extend(next.iter().cloned());
            }
        }
    }
    out.into_iter().collect()
}

fn in_element_type(t: &FieldType, found: &mut BTreeSet<String>) {
    match t {
        FieldType::Ref(tr) => {
            found.insert(tr.name.clone());
        }
        FieldType::Block(b) => {
            for item in &b.items {
                in_element_item(item, found);
            }
        }
        FieldType::Match(mt) => in_element_match(mt, found),
    }
}

fn in_element_match(mt: &MatchType, found: &mut BTreeSet<String>) {
    for arm in &mt.arms {
        if let Some(t) = &arm.type_ref {
            in_element_type(t, found);
        }
    }
}

fn in_element_item(item: &BlockItem, found: &mut BTreeSet<String>) {
    match item {
        BlockItem::Field(wf) => in_element_type(&wf.type_ref, found),
        BlockItem::Group { items, .. } => {
            for sub in items {
                in_element_item(sub, found);
            }
        }
        BlockItem::MatchStmt(mt) => in_element_match(mt, found),
        BlockItem::Computed { .. } | BlockItem::Require(_) | BlockItem::Spread { .. } => {}
    }
}

fn scan_items(items: &[BlockItem], found: &mut BTreeSet<String>) {
    for item in items {
        match item {
            BlockItem::Field(wf) => {
                if wf.repeat.is_some() {
                    in_element_type(&wf.type_ref, found);
                }
                match &wf.type_ref {
                    FieldType::Block(b) => scan_items(&b.items, found),
                    FieldType::Match(mt) => scan_match(mt, found),
                    FieldType::Ref(_) => {}
                }
            }
            BlockItem::Group { items, .. } => scan_items(items, found),
            BlockItem::MatchStmt(mt) => scan_match(mt, found),
            BlockItem::Computed { .. } | BlockItem::Require(_) | BlockItem::Spread { .. } => {}
        }
    }
}

fn scan_match(mt: &MatchType, found: &mut BTreeSet<String>) {
    for arm in &mt.arms {
        if let Some(FieldType::Block(b)) = &arm.type_ref {
            scan_items(&b.items, found);
        }
    }
}

/// Named encodings used inside a repeat element, directly or through others.
///
/// A repeat element is not flattened into fields, so a payload there is one
/// raw frame whose end only a walker can find.
pub fn element_encodings(resolved: &ResolvedSchema) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    for pkt in resolved.packets.values() {
        scan_items(&pkt.items, &mut found);
    }
    for enc in resolved.encodings.values() {
        if let EncBody::Block(b) = &enc.body {
            scan_items(&b.items, &mut found);
        }
    }
    found.into_iter().filter(|n| resolved.encodings.contains_key(n)).collect()
}

/// The compiled walkers: Rust source, encoding -> public fn, and what failed.
#[derive(Debug, Clone, Default)]
pub struct Walkers {
    pub source: String,
    /// Root encoding -> its public walker fn name.
    pub names: OrderedMap<String>,
    /// Encoding -> reason it has no walker.
    pub unsupported: OrderedMap<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Bool,
}

struct Compiler<'a> {
    resolved: &'a ResolvedSchema,
    known: &'a OrderedMap<String>,
    pending: Vec<String>,
    done: BTreeSet<String>,
}

impl<'a> Compiler<'a> {
    /// The fn that skips `name`, queueing its compilation.
    fn want(&mut self, name: &str) -> String {
        let target = alias_target(self.resolved, name);
        if !self.done.contains(&target) && !self.pending.contains(&target) {
            self.pending.push(target.clone());
        }
        skip_name(&target)
    }

    /// Compile one encoding's skip fn; `Err` is the reason it has no walker.
    fn compile(&mut self, name: &str, enc: &EncodingDecl) -> Result<String, String> {
        let EncBody::Block(block) = &enc.body else {
            return Err(format!("{name}: not a block encoding"));
        };
        let consts: HashMap<String, i128> = enc
            .params
            .iter()
            .map(|p| (p.name.clone(), p.default.unwrap_or(0)))
            .collect();
        let mut referenced: HashSet<String> = HashSet::new();
        let mut exprs: Vec<&Expr> = Vec::new();
        for item in &block.items {
            exprs_of_item(item, &mut exprs);
        }
        for e in exprs {
            idents(e, &mut referenced);
        }
        let mut body = Body {
            c: self,
            owner: name.to_string(),
            consts,
            referenced,
            lines: Vec::new(),
            scopes: Vec::new(),
            uid: 0,
        };
        body.block(&block.items, 1)?;
        let mut lines = vec![
            "#[allow(unused_assignments, unused_mut, unused_variables)]".to_string(),
            format!("fn {}(bytes: &[u8], pos: &mut usize, depth: u32) -> Option<()> {{", skip_name(name)),
            "    if depth > 256 {".to_string(),
            "        return None;".to_string(),
            "    }".to_string(),
        ];
        lines.extend(body.lines);
        lines.push("    Some(())".to_string());
        lines.push("}".to_string());
        Ok(lines.join("\n"))
    }
}

/// The statements of one skip fn, with the locals later expressions read.
struct Body<'c, 'a> {
    c: &'c mut Compiler<'a>,
    owner: String,
    consts: HashMap<String, i128>,
    /// Only the fields a later expression reads become numeric locals.
    referenced: HashSet<String>,
    lines: Vec<String>,
    scopes: Vec<HashSet<String>>,
    uid: usize,
}

fn unparen(code: &str) -> &str {
    if !(code.starts_with('(') && code.ends_with(')')) {
        return code;
    }
    let mut depth: i64 = 0;
    for (i, ch) in code.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
        }
        if depth == 0 && i < code.len() - 1 {
            return code;
        }
    }
    &code[1..code.len() - 1]
}

fn kind_name(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_) => "int",
        Expr::Bool(_) => "bool",
        Expr::Str(_) => "str",
        Expr::Ident(_) => "ident",
        Expr::Path(_) => "path",
        Expr::Unop(_, _) => "unop",
        Expr::Binop(_, _, _) => "binop",
        Expr::Ternary(_, _, _) => "ternary",
        Expr::Call(_, _) => "call",
    }
}

impl<'c, 'a> Body<'c, 'a> {
    // -- helpers ----------------------------------------------------------------

    fn emit(&mut self, indent: usize, text: &str) {
        self.lines.push(format!("{}{}", "    ".repeat(indent), text));
    }

    fn fail(&self, why: &str) -> String {
        format!("{}: {}", self.owner, why)
    }

    fn local(name: &str) -> String {
        format!("v_{name}")
    }

    fn in_scope(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    // -- expressions -------------------------------------------------------------

    fn expr(&mut self, e: &Expr) -> Result<(String, Kind), String> {
        match e {
            Expr::Int(v) => Ok((format!("{v}i64"), Kind::Int)),
            Expr::Bool(b) => Ok((if *b { "true" } else { "false" }.to_string(), Kind::Bool)),
            Expr::Ident(name) => {
                if let Some(v) = self.consts.get(name) {
                    if !self.in_scope(name) {
                        return Ok((format!("{v}i64"), Kind::Int));
                    }
                }
                if !self.in_scope(name) {
                    return Err(self.fail(&format!(
                        "expression reads {}, which is not a value in scope",
                        py_repr_str(name)
                    )));
                }
                Ok((Self::local(name), Kind::Int))
            }
            Expr::Unop(op, inner) if op == "!" => {
                let pair = self.expr(inner)?;
                Ok((format!("(!{})", self.as_bool(pair)), Kind::Bool))
            }
            Expr::Binop(op, l, r) => self.binop(op, l, r),
            other => Err(self.fail(&format!(
                "unsupported expression kind {}",
                py_repr_str(kind_name(other))
            ))),
        }
    }

    fn as_bool(&self, pair: (String, Kind)) -> String {
        match pair {
            (code, Kind::Bool) => code,
            (code, Kind::Int) => format!("({code} != 0)"),
        }
    }

    fn as_int(&self, pair: (String, Kind)) -> String {
        match pair {
            (code, Kind::Bool) => format!("({code} as i64)"),
            (code, Kind::Int) => code,
        }
    }

    fn binop(&mut self, op: &str, l: &Expr, r: &Expr) -> Result<(String, Kind), String> {
        let left = self.expr(l)?;
        let right = self.expr(r)?;
        if op == "&&" || op == "||" {
            let (a, b) = (self.as_bool(left), self.as_bool(right));
            return Ok((format!("({a} {op} {b})"), Kind::Bool));
        }
        let a = self.as_int(left);
        let b = self.as_int(right);
        match op {
            "==" | "!=" | "<" | "<=" | ">" | ">=" => Ok((format!("({a} {op} {b})"), Kind::Bool)),
            "&" | "|" | "^" => Ok((format!("({a} {op} {b})"), Kind::Int)),
            "+" => Ok((format!("{a}.wrapping_add({b})"), Kind::Int)),
            "-" => Ok((format!("{a}.wrapping_sub({b})"), Kind::Int)),
            "*" => Ok((format!("{a}.wrapping_mul({b})"), Kind::Int)),
            "/" => Ok((format!("walk_div({a}, {b})"), Kind::Int)),
            "%" => Ok((format!("walk_rem({a}, {b})"), Kind::Int)),
            "<<" => Ok((format!("walk_shl({a}, {b})"), Kind::Int)),
            ">>" => Ok((format!("walk_shr({a}, {b})"), Kind::Int)),
            _ => Err(self.fail(&format!("unsupported operator {}", py_repr_str(op)))),
        }
    }

    // -- structure ---------------------------------------------------------------

    fn block(&mut self, items: &[BlockItem], indent: usize) -> Result<(), String> {
        let names = declared(items);
        self.scopes.push(HashSet::new());
        for name in names {
            let declared_here = self.scopes.last().is_some_and(|s| s.contains(&name));
            if self.referenced.contains(&name) && !declared_here {
                if let Some(s) = self.scopes.last_mut() {
                    s.insert(name.clone());
                }
                let line = format!("let mut {}: i64 = 0;", Self::local(&name));
                self.emit(indent, &line);
            }
        }
        for item in items {
            self.item(item, indent)?;
        }
        self.scopes.pop();
        Ok(())
    }

    fn item(&mut self, item: &BlockItem, indent: usize) -> Result<(), String> {
        match item {
            BlockItem::Require(_) => Ok(()),
            BlockItem::Spread { name, .. } => {
                Err(self.fail(&format!("spread of {} is not supported", py_repr_str(name))))
            }
            BlockItem::Computed { name, expr } => {
                let pair = self.expr(expr)?;
                let code = self.as_int(pair);
                let line = format!("{} = {};", Self::local(name), unparen(&code));
                self.emit(indent, &line);
                Ok(())
            }
            BlockItem::Group { cond, items } => {
                let Some(cond) = cond else {
                    return Err(self.fail("group without a condition"));
                };
                let pair = self.expr(cond)?;
                let line = format!("if {} {{", unparen(&self.as_bool(pair)));
                self.emit(indent, &line);
                // A conditional group shares its block's scope: its locals are already declared.
                for sub in items {
                    self.item(sub, indent + 1)?;
                }
                self.emit(indent, "}");
                Ok(())
            }
            BlockItem::MatchStmt(mt) => self.match_(mt, indent, None),
            BlockItem::Field(wf) => self.field(wf, indent),
        }
    }

    fn field(&mut self, wf: &WireField, indent: usize) -> Result<(), String> {
        let mut inner = indent;
        if let Some(cond) = &wf.if_expr {
            let pair = self.expr(cond)?;
            let line = format!("if {} {{", unparen(&self.as_bool(pair)));
            self.emit(indent, &line);
            inner += 1;
        }
        if let Some(rep) = &wf.repeat {
            self.repeat(wf, rep, inner)?;
        } else {
            self.type_(&wf.type_ref, Some(&wf.name), inner)?;
        }
        if wf.if_expr.is_some() {
            self.emit(indent, "}");
        }
        Ok(())
    }

    fn repeat(&mut self, wf: &WireField, rep: &Repeat, indent: usize) -> Result<(), String> {
        let expr = match &rep.expr {
            Some(e) if rep.mode == RepeatMode::Count => e,
            _ => {
                return Err(self.fail(&format!(
                    "repeat mode {} is not supported",
                    py_repr_str(rep.mode.as_str())
                )))
            }
        };
        let pair = self.expr(expr)?;
        let n = self.as_int(pair);
        self.uid += 1;
        let lp = format!("n{}", self.uid);
        self.emit(indent, &format!("let {lp} = walk_count({n}, bytes)?;"));
        self.emit(indent, &format!("for _ in 0..{lp} {{"));
        match &wf.type_ref {
            FieldType::Block(b) => self.block(&b.items, indent + 1)?,
            other => self.type_(other, None, indent + 1)?,
        }
        self.emit(indent, "}");
        Ok(())
    }

    fn match_(&mut self, mt: &MatchType, indent: usize, bind: Option<&str>) -> Result<(), String> {
        let pair = self.expr(&mt.tag_expr)?;
        let tag = self.as_int(pair);
        self.emit(indent, &format!("match {} {{", unparen(&tag)));
        let mut default: Option<&MatchArm> = None;
        for arm in &mt.arms {
            let tags: Vec<&Tag> = if !arm.tags.is_empty() {
                arm.tags.iter().collect()
            } else {
                arm.tag.iter().collect()
            };
            if tags.is_empty() {
                default = Some(arm);
                continue;
            }
            let mut ints = Vec::new();
            for t in tags {
                match t {
                    Tag::Int(v) => ints.push(v.to_string()),
                    Tag::Str(_) => return Err(self.fail("match arm tags must be integers")),
                }
            }
            self.emit(indent + 1, &format!("{} => {{", ints.join(" | ")));
            self.arm(arm, indent + 2, bind)?;
            self.emit(indent + 1, "}");
        }
        if let Some(d) = default {
            self.emit(indent + 1, "_ => {");
            self.arm(d, indent + 2, bind)?;
            self.emit(indent + 1, "}");
        } else {
            self.emit(indent + 1, "_ => return None,");
        }
        self.emit(indent, "}");
        Ok(())
    }

    fn arm(&mut self, arm: &MatchArm, indent: usize, bind: Option<&str>) -> Result<(), String> {
        match &arm.type_ref {
            None => Ok(()),
            // An arm block shares the enclosing scope, so its locals are declared there.
            Some(FieldType::Block(b)) => {
                for item in &b.items {
                    self.item(item, indent)?;
                }
                Ok(())
            }
            Some(t) => self.type_(t, bind, indent),
        }
    }

    /// Skip one value of type `t`; keep it in the local `bind` when one is named.
    fn type_(&mut self, t: &FieldType, bind: Option<&str>, indent: usize) -> Result<(), String> {
        match t {
            FieldType::Block(b) => {
                for item in &b.items {
                    self.item(item, indent)?;
                }
                Ok(())
            }
            FieldType::Match(mt) => self.match_(mt, indent, bind),
            FieldType::Ref(tr) => self.type_ref(tr, bind, indent),
        }
    }

    fn type_ref(&mut self, tr: &TypeRef, bind: Option<&str>, indent: usize) -> Result<(), String> {
        let name = tr.name.as_str();
        let wanted = bind.is_some_and(|b| self.in_scope(b));
        let dest = match bind {
            Some(b) if wanted => format!("{} = ", Self::local(b)),
            _ => String::new(),
        };
        if name == "rest" {
            self.emit(indent, "*pos = bytes.len();");
        } else if name == "bytes" {
            let Some(arg) = tr.args.first() else {
                return Err(self.fail("bytes() without a length"));
            };
            let pair = self.expr(arg)?;
            let n = self.as_int(pair);
            self.emit(indent, &format!("walk_skip(bytes, pos, walk_count({n}, bytes)?)?;"));
        } else if let Some(size) = fixed_size(name) {
            if wanted {
                let signed = SIGNED.contains(&name);
                self.emit(indent, &format!("{dest}walk_be(bytes, pos, {size}, {signed})?;"));
            } else {
                self.emit(indent, &format!("walk_skip(bytes, pos, {size})?;"));
            }
        } else if let Some(max) = var_max(name) {
            self.emit(indent, &format!("{dest}walk_var(bytes, pos, {max})?;"));
        } else if STRINGS.contains(&name) {
            self.emit(indent, "let len = walk_var(bytes, pos, 5)?;");
            self.emit(indent, "walk_skip(bytes, pos, walk_count(len, bytes)?)?;");
        } else if name == "nbt_nothing" {
            // Emits nothing: the value is consumed elsewhere.
        } else if let Some(f) = self.c.known.get(name).cloned() {
            self.emit(indent, &format!("walk_hand(bytes, pos, {f})?;"));
        } else if self.c.resolved.encodings.contains_key(name) {
            if !tr.args.is_empty() {
                let target = alias_target(self.c.resolved, name);
                if let Some(enc) = self.c.resolved.encodings.get(&target) {
                    if !enc.params.is_empty() {
                        return Err(self.fail(&format!(
                            "{name} is used with arguments, which is not supported"
                        )));
                    }
                }
            }
            if wanted {
                return Err(self.fail(&format!("{name} is read as a number but is a composite")));
            }
            let f = self.c.want(name);
            self.emit(indent, &format!("{f}(bytes, pos, depth + 1)?;"));
        } else {
            return Err(self.fail(&format!("unknown type {}", py_repr_str(name))));
        }
        Ok(())
    }
}

/// Compile walkers for `roots` and everything they reach.
///
/// `known` maps encodings that already have a hand-written walker (NBT, slots)
/// to its Rust fn; they are called, not recompiled.
pub fn generate(
    resolved: &ResolvedSchema,
    roots: &[String],
    known: &OrderedMap<String>,
) -> BpsResult<Walkers> {
    let mut comp = Compiler {
        resolved,
        known,
        pending: Vec::new(),
        done: BTreeSet::new(),
    };
    let mut result = Walkers::default();
    let mut bodies: BTreeMap<String, String> = BTreeMap::new();
    for root in roots {
        comp.want(root);
    }
    while !comp.pending.is_empty() {
        let name = comp.pending.remove(0);
        let Some(enc) = resolved.encodings.get(&name) else {
            return Err(emit(format!("KeyError: {}", py_repr_str(&name))));
        };
        match comp.compile(&name, enc) {
            Ok(src) => {
                bodies.insert(name.clone(), src);
            }
            Err(msg) => result.unsupported.insert(&name, msg),
        }
        comp.done.insert(name);
    }
    let broken: BTreeSet<String> = result.unsupported.keys().map(str::to_string).collect();
    // A walker that calls a broken one is itself broken.
    let ok: Vec<String> = bodies
        .keys()
        .filter(|n| {
            !broken.contains(*n) && !closure(resolved, n, known).iter().any(|c| broken.contains(c))
        })
        .cloned()
        .collect();
    for root in roots {
        let target = alias_target(resolved, root);
        if ok.contains(&target) {
            result.names.insert(root, walker_name(&target));
        } else if !result.unsupported.contains_key(root) {
            result
                .unsupported
                .insert(root, format!("{root}: reaches an encoding without a walker"));
        }
    }
    if !ok.is_empty() {
        let text = ok
            .iter()
            .map(|n| bodies[n].as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let helpers: Vec<&str> = helpers()
            .into_iter()
            .filter(|(name, _)| text.contains(&format!("{name}(")))
            .map(|(_, h)| h)
            .collect();
        let publics: Vec<String> = ok
            .iter()
            .map(|n| {
                format!(
                    "/// Length of one `{n}` payload at the front of `bytes`.\n\
                     #[must_use]\n\
                     pub fn {}(bytes: &[u8]) -> usize {{\n\
                     \x20   let mut pos = 0usize;\n\
                     \x20   if {}(bytes, &mut pos, 0).is_none() {{\n\
                     \x20       return bytes.len();\n\
                     \x20   }}\n\
                     \x20   pos.min(bytes.len())\n\
                     }}",
                    walker_name(n),
                    skip_name(n)
                )
            })
            .collect();
        result.source = format!(
            "\n{}\n{}\n\n{}\n",
            helpers.join("\n"),
            text,
            publics.join("\n\n")
        );
    }
    Ok(result)
}

/// Encodings reachable from `name`'s body that need their own walker.
fn closure(resolved: &ResolvedSchema, name: &str, known: &OrderedMap<String>) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![name.to_string()];
    while let Some(cur) = stack.pop() {
        for r in references(resolved, &cur) {
            let target = alias_target(resolved, &r);
            if known.contains_key(&target) || is_builtin(&target) {
                continue;
            }
            if !out.contains(&target) && target != name {
                out.insert(target.clone());
                stack.push(target);
            }
        }
    }
    out
}
