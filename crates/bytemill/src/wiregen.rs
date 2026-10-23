//! Random wire bytes for a packet, generated from the schema alone.
//!
//! The walk mirrors the decoder (`interp::engine`): fields in order, `if` guards, `match` arms,
//! repeats, spreads, with an environment of the values chosen so far, so every expression the
//! decoder will evaluate is evaluated here on the same values. Instead of reading bytes it picks a
//! value and writes its bytes. What it picks is steered by what the schema later asks of a
//! field: a count or a `bytes(n)` length stays small, a `match` tag is drawn from the arms'
//! tags, a field compared with a number or a string takes those constants.
//!
//! The generator shares nothing with `encode`, so decoding its bytes strictly and encoding the
//! result back is a real round-trip test of a schema: one that needs no hand-made values.

use std::collections::{HashMap, HashSet};

use crate::ast::{
    BlockItem, EncBody, Expr, FieldType, RepeatMode, ResolvedSchema, Tag, TypeRef, WireField,
    arm_tag_list, select_match_arm,
};
use crate::interp::cursor::Cursor;
use crate::interp::{decode_type_ref, encode_bare, eval_expr};
use crate::value::{Env, Value};

type R<T> = Result<T, String>;

/// xorshift64*: deterministic and dependency free.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `0..n` (`n > 0`).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Default, Clone)]
struct Hints {
    /// Constants a field is compared with or switched on.
    consts: HashMap<String, Vec<i128>>,
    /// String literals a field is compared with.
    strings: HashMap<String, Vec<String>>,
    /// Fields that size something: counts and byte lengths.
    small: HashSet<String>,
    /// Fields switched on by a `match` with no `_` arm: only its tags are valid values.
    exact: HashMap<String, Vec<i128>>,
}

impl Hints {
    fn merged(&self, other: Hints) -> Hints {
        let mut out = self.clone();
        for (k, v) in other.consts {
            out.consts.entry(k).or_default().extend(v);
        }
        for (k, v) in other.strings {
            out.strings.entry(k).or_default().extend(v);
        }
        out.small.extend(other.small);
        for (k, v) in other.exact {
            out.exact.entry(k).or_default().extend(v);
        }
        out
    }
}

fn idents(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Ident(n) => out.push(n.clone()),
        Expr::Path(p) => out.push(p.split('.').next().unwrap_or(p).to_string()),
        Expr::Unop(_, x) => idents(x, out),
        Expr::Binop(_, l, r) => {
            idents(l, out);
            idents(r, out);
        }
        Expr::Ternary(c, a, b) => {
            idents(c, out);
            idents(a, out);
            idents(b, out);
        }
        Expr::Call(_, args) => args.iter().for_each(|a| idents(a, out)),
        Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) => {}
    }
}

/// What the expressions in `e` ask of the fields they name.
fn scan_expr(e: &Expr, h: &mut Hints) {
    match e {
        Expr::Binop(op, l, r) => {
            if let (Expr::Ident(n), Expr::Int(c)) | (Expr::Int(c), Expr::Ident(n)) = (&**l, &**r)
                && matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=")
            {
                h.consts
                    .entry(n.clone())
                    .or_default()
                    .extend([*c - 1, *c, *c + 1]);
            }
            if let (Expr::Ident(n), Expr::Str(s)) | (Expr::Str(s), Expr::Ident(n)) = (&**l, &**r)
                && matches!(op.as_str(), "==" | "!=")
            {
                h.strings.entry(n.clone()).or_default().push(s.clone());
            }
            scan_expr(l, h);
            scan_expr(r, h);
        }
        Expr::Unop(_, x) => scan_expr(x, h),
        Expr::Ternary(c, a, b) => {
            scan_expr(c, h);
            scan_expr(a, h);
            scan_expr(b, h);
        }
        Expr::Call(_, args) => args.iter().for_each(|a| scan_expr(a, h)),
        _ => {}
    }
}

fn scan_type(cx: &mut ScanCx<'_>, t: &FieldType, h: &mut Hints) {
    match t {
        FieldType::Ref(r) => {
            for a in &r.args {
                if r.name == "bytes" {
                    let mut ids = Vec::new();
                    idents(a, &mut ids);
                    h.small.extend(ids);
                }
                scan_expr(a, h);
            }
        }
        FieldType::Block(b) => scan_items(cx, &b.items, h),
        FieldType::Match(m) => scan_match(cx, m, h),
    }
}

fn scan_match(cx: &mut ScanCx<'_>, m: &crate::ast::MatchType, h: &mut Hints) {
    scan_expr(&m.tag_expr, h);
    let mut tags: Vec<i128> = Vec::new();
    for arm in &m.arms {
        for t in arm_tag_list(arm) {
            if let Tag::Int(v) = t {
                tags.push(*v);
            }
        }
        if let Some(body) = &arm.type_ref {
            scan_type(cx, body, h);
        }
    }
    let has_default = m.arms.iter().any(crate::ast::is_default_arm);
    let mut ids = Vec::new();
    idents(&m.tag_expr, &mut ids);
    if !has_default {
        for id in ids {
            h.exact.entry(id).or_default().extend(tags.iter().copied());
        }
        return;
    }
    // With a `_` arm, a tag outside the arms is valid too: draw one now and then.
    if let Some(max) = tags.iter().max().copied() {
        tags.push(max + 1);
    }
    for id in ids {
        h.consts.entry(id).or_default().extend(tags.iter().copied());
    }
}

fn scan_items(cx: &mut ScanCx<'_>, items: &[BlockItem], h: &mut Hints) {
    for item in items {
        match item {
            BlockItem::Field(wf) => {
                if let Some(c) = &wf.if_expr {
                    scan_expr(c, h);
                }
                if let Some(r) = &wf.repeat
                    && let Some(e) = &r.expr
                {
                    if r.mode == RepeatMode::Count {
                        let mut ids = Vec::new();
                        idents(e, &mut ids);
                        h.small.extend(ids);
                    }
                    scan_expr(e, h);
                }
                scan_type(cx, &wf.type_ref, h);
            }
            BlockItem::Computed { expr, .. } => scan_expr(expr, h),
            BlockItem::Group { cond, items } => {
                if let Some(c) = cond {
                    scan_expr(c, h);
                }
                scan_items(cx, items, h);
            }
            BlockItem::MatchStmt(m) => scan_match(cx, m, h),
            BlockItem::Spread { name, .. } => {
                // A spread's fields live in this block, so what its body asks of them counts.
                if cx.seen.insert(name.clone())
                    && let Some(enc) = cx.resolved.encodings.get(name)
                    && let EncBody::Block(b) = &enc.body
                {
                    scan_items(cx, &b.items, h);
                }
            }
            BlockItem::Require(_) => {}
        }
    }
}

/// The schema a hint scan reads spread bodies from, and the spreads it has entered (a recursive
/// spread would not terminate).
struct ScanCx<'a> {
    resolved: &'a ResolvedSchema,
    seen: HashSet<String>,
}

/// A std scalar: its range and how it is written.
#[derive(Clone, Copy)]
enum Scalar {
    Fixed { bytes: usize, signed: bool },
    VarInt,
    VarLong,
    Bool,
}

fn std_scalar(name: &str) -> Option<Scalar> {
    Some(match name {
        "u8" => Scalar::Fixed {
            bytes: 1,
            signed: false,
        },
        "i8" => Scalar::Fixed {
            bytes: 1,
            signed: true,
        },
        "u16be" => Scalar::Fixed {
            bytes: 2,
            signed: false,
        },
        "i16be" => Scalar::Fixed {
            bytes: 2,
            signed: true,
        },
        "u32be" => Scalar::Fixed {
            bytes: 4,
            signed: false,
        },
        "i32be" => Scalar::Fixed {
            bytes: 4,
            signed: true,
        },
        "u64be" => Scalar::Fixed {
            bytes: 8,
            signed: false,
        },
        "i64be" => Scalar::Fixed {
            bytes: 8,
            signed: true,
        },
        "varint" => Scalar::VarInt,
        "varlong" => Scalar::VarLong,
        "boolean" => Scalar::Bool,
        _ => return None,
    })
}

const MAX_DEPTH: usize = 48;

struct Gen<'a> {
    resolved: &'a ResolvedSchema,
    rng: Rng,
    buf: Vec<u8>,
    depth: usize,
    /// How many NBT names have been drawn: each is distinct.
    names: usize,
    /// Branch sites taken so far: `(address, 0)` for a `match` arm, `(address, 1 | 2)` for an `if`
    /// group that was entered or skipped.
    seen: &'a mut HashSet<(usize, u8)>,
}

/// The branch sites of a schema, as `generate_covering` names them: one per `match` arm and two per
/// `if` group. What a set of runs reached, as a fraction of this, is the generator's coverage.
pub fn branch_sites(resolved: &ResolvedSchema) -> usize {
    fn arms(m: &crate::ast::MatchType) -> usize {
        m.arms.len()
            + m.arms
                .iter()
                .map(|a| a.type_ref.as_ref().map_or(0, ty))
                .sum::<usize>()
    }
    fn ty(t: &FieldType) -> usize {
        match t {
            FieldType::Ref(_) => 0,
            FieldType::Block(b) => items(&b.items),
            FieldType::Match(m) => arms(m),
        }
    }
    fn items(list: &[BlockItem]) -> usize {
        list.iter()
            .map(|i| match i {
                BlockItem::Field(wf) => ty(&wf.type_ref),
                BlockItem::Group { items: inner, .. } => 2 + items(inner),
                BlockItem::MatchStmt(m) => arms(m),
                _ => 0,
            })
            .sum()
    }
    let enc: usize = resolved
        .encodings
        .values()
        .map(|e| match &e.body {
            EncBody::Block(b) => items(&b.items),
            EncBody::Ref(_) => 0,
        })
        .sum();
    enc + resolved
        .packets
        .values()
        .map(|p| items(&p.items))
        .sum::<usize>()
}

/// Random wire bytes (the payload, after the packet id) for `packet`.
pub fn generate(resolved: &ResolvedSchema, packet: &str, seed: u64) -> R<Vec<u8>> {
    generate_covering(resolved, packet, seed, &mut HashSet::new())
}

/// [`generate`], recording in `seen` the branch sites the run took.
pub fn generate_covering(
    resolved: &ResolvedSchema,
    packet: &str,
    seed: u64,
    seen: &mut HashSet<(usize, u8)>,
) -> R<Vec<u8>> {
    let pkt = resolved
        .packets
        .get(packet)
        .ok_or_else(|| format!("unknown packet {packet:?}"))?;
    let mut g = Gen {
        resolved,
        rng: Rng::new(seed),
        buf: Vec::new(),
        depth: 0,
        names: 0,
        seen,
    };
    let mut hints = Hints::default();
    scan_items(
        &mut ScanCx {
            resolved,
            seen: HashSet::new(),
        },
        &pkt.items,
        &mut hints,
    );
    g.items(&pkt.items, &mut Env::new(), &mut Env::new(), &hints)?;
    Ok(g.buf)
}

impl Gen<'_> {
    fn int_of(&self, e: &Expr, env: &Env) -> R<i128> {
        eval_expr(e, env).map_err(|e| e.to_string())?.as_int()
    }

    fn items(&mut self, items: &[BlockItem], env: &mut Env, out: &mut Env, hints: &Hints) -> R<()> {
        for item in items {
            match item {
                BlockItem::Require(_) => {}
                BlockItem::Computed { name, expr } => {
                    let v = eval_expr(expr, env).map_err(|e| e.to_string())?;
                    env.insert(name, v.clone());
                    out.insert(name, v);
                }
                BlockItem::Group { cond, items } => {
                    let on = match cond {
                        Some(c) => eval_expr(c, env).map_or(true, |v| v.truthy()),
                        None => true,
                    };
                    self.seen
                        .insert((std::ptr::from_ref(item) as usize, if on { 1 } else { 2 }));
                    if on {
                        self.items(items, env, out, hints)?;
                    }
                }
                BlockItem::MatchStmt(m) => {
                    let tag = self.int_of(&m.tag_expr, env)?;
                    let arm = select_match_arm(m, tag)
                        .ok_or_else(|| format!("no match arm for tag {tag}"))?;
                    self.seen.insert((std::ptr::from_ref(arm) as usize, 0));
                    if let Some(FieldType::Block(b)) = &arm.type_ref {
                        let inner = hints.merged(scan_block(self.resolved, &b.items));
                        self.items(&b.items, env, out, &inner)?;
                    }
                }
                BlockItem::Spread { name, args } => {
                    let enc = self
                        .resolved
                        .encodings
                        .get(name)
                        .ok_or_else(|| format!("unknown spread {name}"))?;
                    for (i, p) in enc.params.iter().enumerate() {
                        let v = match args.get(i) {
                            Some(a) => eval_expr(a, env).map_err(|e| e.to_string())?,
                            None => Value::Int(p.default.ok_or("missing parameter")?),
                        };
                        env.insert(&p.name, v);
                    }
                    match &enc.body {
                        EncBody::Block(b) => self.items(&b.items, env, out, hints)?,
                        EncBody::Ref(t) => {
                            self.type_ref(t, env, None, hints)?;
                        }
                    }
                }
                BlockItem::Field(wf) => self.field(wf, env, out, hints)?,
            }
        }
        Ok(())
    }

    fn field(&mut self, wf: &WireField, env: &mut Env, out: &mut Env, hints: &Hints) -> R<()> {
        if let Some(c) = &wf.if_expr
            && !eval_expr(c, env).map_err(|e| e.to_string())?.truthy()
        {
            return Ok(());
        }
        let value = match &wf.repeat {
            Some(rep) => {
                let n = match rep.mode {
                    RepeatMode::Count => {
                        let n = self.int_of(
                            rep.expr.as_ref().ok_or("count repeat without a count")?,
                            env,
                        )?;
                        if n > 2048 {
                            return Err(format!("{}: count {n} is out of range", wf.name));
                        }
                        // A negative count reads as none, as the decoder does.
                        n.max(0) as usize
                    }
                    RepeatMode::UntilEnd | RepeatMode::Until => self.rng.below(3) as usize,
                };
                let term = match (rep.mode, &rep.expr) {
                    (RepeatMode::Until, Some(e)) => Some(self.int_of(e, env)?),
                    _ => None,
                };
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.element(wf, env, hints, term)?);
                }
                if let Some(t) = term {
                    self.buf.push(t as u8);
                }
                Value::list(items)
            }
            None => self.type_of(&wf.type_ref, env, Some(&wf.name), hints)?,
        };
        env.insert(&wf.name, value.clone());
        out.insert(&wf.name, value);
        Ok(())
    }

    /// One repeated element. Under `until`, an element must not start with the terminator byte.
    fn element(
        &mut self,
        wf: &WireField,
        env: &Env,
        hints: &Hints,
        term: Option<i128>,
    ) -> R<Value> {
        for _ in 0..32 {
            let start = self.buf.len();
            let v = self.type_of(&wf.type_ref, env, None, hints)?;
            let clash =
                term.is_some_and(|t| self.buf.get(start).is_some_and(|b| i128::from(*b) == t));
            if !clash {
                return Ok(v);
            }
            self.buf.truncate(start);
        }
        Err(format!(
            "{}: could not draw an element that avoids the terminator",
            wf.name
        ))
    }

    fn type_of(&mut self, t: &FieldType, env: &Env, name: Option<&str>, hints: &Hints) -> R<Value> {
        match t {
            FieldType::Block(b) => {
                let inner = hints.merged(scan_block(self.resolved, &b.items));
                let mut scope = env.clone();
                let mut out = Env::new();
                self.items(&b.items, &mut scope, &mut out, &inner)?;
                Ok(Value::map(out))
            }
            FieldType::Match(m) => {
                let tag = self.int_of(&m.tag_expr, env)?;
                let arm = select_match_arm(m, tag)
                    .ok_or_else(|| format!("no match arm for tag {tag}"))?;
                self.seen.insert((std::ptr::from_ref(arm) as usize, 0));
                match &arm.type_ref {
                    None => Ok(Value::Null),
                    Some(body) => self.type_of(body, env, name, hints),
                }
            }
            FieldType::Ref(r) => self.type_ref(r, env, name, hints),
        }
    }

    fn type_ref(&mut self, r: &TypeRef, env: &Env, name: Option<&str>, hints: &Hints) -> R<Value> {
        let want = |n: &str| name.unwrap_or(n).to_string();
        if let Some(sc) = std_scalar(&r.name) {
            return Ok(self.scalar(sc, &want(""), hints));
        }
        match r.name.as_str() {
            "f32be" => {
                let f = self.float();
                self.buf.extend_from_slice(&(f as f32).to_be_bytes());
                return Ok(Value::Float(f64::from(f as f32)));
            }
            "f64be" => {
                let f = self.float();
                self.buf.extend_from_slice(&f.to_be_bytes());
                return Ok(Value::Float(f));
            }
            "rest" => {
                let n = self.rng.below(4) as usize;
                let bytes: Vec<u8> = (0..n).map(|_| self.rng.below(256) as u8).collect();
                self.buf.extend_from_slice(&bytes);
                return Ok(Value::bytes(bytes));
            }
            "bytes" => {
                let n = self.int_of(r.args.first().ok_or("bytes() needs a size")?, env)?;
                let n = usize::try_from(n).map_err(|_| format!("bytes({n})"))?;
                if n > 4096 {
                    return Err(format!("bytes({n}) is too long to draw"));
                }
                if r.transforms.iter().any(|t| t == "utf8") {
                    let s: String = (0..n)
                        .map(|_| (b'a' + self.rng.below(26) as u8) as char)
                        .collect();
                    self.buf.extend_from_slice(s.as_bytes());
                    return Ok(Value::str(s));
                }
                let bytes: Vec<u8> = (0..n).map(|_| self.rng.below(256) as u8).collect();
                self.buf.extend_from_slice(&bytes);
                return Ok(Value::bytes(bytes));
            }
            "string" | "identifier" | "nbt_string" => {
                return Ok(self.string(&r.name, &want(""), hints));
            }
            _ => {}
        }
        let resolved = self.resolved;
        let enc = resolved
            .encodings
            .get(&r.name)
            .ok_or_else(|| format!("unknown type {:?}", r.name))?;
        if self.depth >= MAX_DEPTH {
            return Err("recursion too deep".to_string());
        }
        let mut scope = if enc.params.is_empty() {
            Env::new()
        } else {
            env.clone()
        };
        for (i, p) in enc.params.iter().enumerate() {
            let v = match r.args.get(i) {
                Some(a) => eval_expr(a, env).map_err(|e| e.to_string())?,
                None => Value::Int(p.default.ok_or("missing parameter")?),
            };
            scope.insert(&p.name, v);
        }
        self.depth += 1;
        let result = match &enc.body {
            EncBody::Ref(inner) => self.type_ref(inner, &scope, name, hints),
            EncBody::Block(b) => {
                if enc.yield_expr.is_some() {
                    let field = want("");
                    let mut chosen = None;
                    for _attempt in 0..32u32 {
                        let v = if let Some(c) = hints.exact.get(&field)
                            && !c.is_empty()
                        {
                            c[self.rng.below(c.len() as u64) as usize]
                        } else if hints.small.contains(&field) {
                            i128::from(self.rng.below(4))
                        } else if let Some(c) = hints.consts.get(&field)
                            && !c.is_empty()
                            && self.rng.below(10) < 8
                        {
                            c[self.rng.below(c.len() as u64) as usize]
                        } else {
                            match self.rng.below(10) {
                                0..=3 => i128::from(self.rng.below(6)),
                                4 => 0,
                                5 => -1,
                                6 => 1,
                                _ => i128::from((self.rng.below(200) as i32) - 50),
                            }
                        };
                        let tr = TypeRef {
                            name: r.name.clone(),
                            args: r.args.clone(),
                            transforms: r.transforms.clone(),
                        };
                        let enc_res = encode_bare(&tr, &Value::Int(v), self.resolved, &scope)
                            .map_err(|e| e.to_string());
                        if let Ok(Some(wire)) = enc_res {
                            let mut cur = Cursor::new(&wire);
                            let dec_res = decode_type_ref(&tr, &mut cur, self.resolved, &scope)
                                .map_err(|e| e.to_string());
                            if let Ok(decoded) = dec_res
                                && cur.remaining() == 0
                            {
                                self.buf.extend_from_slice(&wire);
                                chosen = Some(decoded);
                                break;
                            }
                        }
                    }
                    match chosen {
                        Some(val) => Ok(val),
                        None => Err(format!("scalar encoding {:?} has no generator", enc.name)),
                    }
                } else {
                    let inner = hints.merged(scan_block(self.resolved, &b.items));
                    let mut out = Env::new();
                    self.items(&b.items, &mut scope, &mut out, &inner)
                        .map(|()| Value::map(out))
                }
            }
        };
        self.depth -= 1;
        result
    }

    fn scalar(&mut self, sc: Scalar, field: &str, hints: &Hints) -> Value {
        let (lo, hi): (i128, i128) = match sc {
            Scalar::Bool => {
                let b = self.rng.below(2) == 1;
                self.buf.push(u8::from(b));
                return Value::Bool(b);
            }
            Scalar::Fixed { bytes, signed } => {
                let bits = bytes as u32 * 8;
                if signed {
                    (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
                } else {
                    (0, (1i128 << bits) - 1)
                }
            }
            Scalar::VarInt => (i128::from(i32::MIN), i128::from(i32::MAX)),
            Scalar::VarLong => (i128::from(i64::MIN), i128::from(i64::MAX)),
        };
        let v = self.pick_int(lo, hi, field, hints);
        match sc {
            Scalar::Fixed { bytes, .. } => {
                let be = (v as u64).to_be_bytes();
                self.buf.extend_from_slice(&be[8 - bytes..]);
            }
            Scalar::VarInt => self.leb128(u64::from(v as i32 as u32)),
            Scalar::VarLong => self.leb128(v as i64 as u64),
            Scalar::Bool => {}
        }
        Value::Int(v)
    }

    fn leb128(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.buf.push(byte);
                return;
            }
            self.buf.push(byte | 0x80);
        }
    }

    fn pick_int(&mut self, lo: i128, hi: i128, field: &str, hints: &Hints) -> i128 {
        let clamp = |v: i128| v.clamp(lo, hi);
        if let Some(c) = hints.exact.get(field)
            && !c.is_empty()
        {
            return clamp(c[self.rng.below(c.len() as u64) as usize]);
        }
        if hints.small.contains(field) {
            // Counts and lengths: 0..=3, or a constant the schema compares them with.
            return clamp(i128::from(self.rng.below(4)));
        }
        if let Some(c) = hints.consts.get(field)
            && !c.is_empty()
            && self.rng.below(10) < 8
        {
            return clamp(c[self.rng.below(c.len() as u64) as usize]);
        }
        match self.rng.below(10) {
            0..=3 => clamp(i128::from(self.rng.below(6))),
            4 => lo,
            5 => hi,
            6 => clamp(-1),
            _ => {
                let span = (hi - lo + 1) as u128;
                let r = (u128::from(self.rng.next()) << 64 | u128::from(self.rng.next())) % span;
                lo + r as i128
            }
        }
    }

    fn float(&mut self) -> f64 {
        const POOL: [f64; 8] = [0.0, 1.0, -1.5, 0.25, 100.0, -3.75, 1.0e6, 0.001];
        POOL[self.rng.below(POOL.len() as u64) as usize]
    }

    /// The special strings: written whole, with the length prefix the encoding names.
    fn string(&mut self, enc: &str, field: &str, hints: &Hints) -> Value {
        // NBT names within a compound are unique in real data, and a reader that keys a compound
        // by name (ProtoDef's) cannot represent a repeat: draw distinct ones.
        let s: String = if enc == "nbt_string" {
            self.names += 1;
            format!("k{}", self.names)
        } else {
            match hints.strings.get(field) {
                Some(c) if !c.is_empty() && self.rng.below(10) < 8 => {
                    c[self.rng.below(c.len() as u64) as usize].clone()
                }
                _ => {
                    let n = self.rng.below(6) as usize;
                    (0..n)
                        .map(|_| (b'a' + self.rng.below(26) as u8) as char)
                        .collect()
                }
            }
        };
        let len = s.len();
        if enc == "nbt_string" {
            self.buf
                .extend_from_slice(&u16::try_from(len).unwrap_or(0).to_be_bytes());
        } else {
            self.leb128(len as u64);
        }
        self.buf.extend_from_slice(s.as_bytes());
        Value::str(s)
    }
}

fn scan_block(resolved: &ResolvedSchema, items: &[BlockItem]) -> Hints {
    let mut h = Hints::default();
    scan_items(
        &mut ScanCx {
            resolved,
            seen: HashSet::new(),
        },
        items,
        &mut h,
    );
    h
}
