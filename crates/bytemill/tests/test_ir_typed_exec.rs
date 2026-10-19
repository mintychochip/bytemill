//! The typed program means what the interpreter means: for every packet the
//! synthesizer can build, in every shipped schema, decoding the interpreter's
//! bytes with the typed program and encoding the result gives the same bytes.
//!
//! The evaluator below follows the module docs of `bytemill::ir::typed` and
//! nothing else, the way a printer must: `i64` wrapping arithmetic, explicit
//! casts, narrow operations that never leave their type.

use bytemill::interp::encode_packet;
use bytemill::ir::typed::{
    BinOp, Bind, Body, CodecIdx, Expr, Failure, Func, IntTy, Program, ReadOp, RepeatMode, Stmt, Ty,
    WriteOp, typed,
};
use bytemill::synth::synthesize_packet;

#[derive(Clone, Debug, PartialEq)]
enum V {
    Int(i128),
    Bool(bool),
    F32(f32),
    F64(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<V>),
    Rec(Vec<V>),
    Var(usize, Option<Box<V>>),
    /// An absent optional.
    Absent,
}

enum Flow {
    Next,
    Return(V),
}

struct Ctx<'a> {
    prog: &'a Program,
    data: &'a [u8],
    pos: usize,
    out: Vec<u8>,
}

type R<T> = Result<T, String>;

fn wrap(v: i128, t: IntTy) -> i128 {
    let bits = t.bits();
    if bits >= 128 {
        return v;
    }
    let m = v & ((1i128 << bits) - 1);
    if t.signed() && m >= 1i128 << (bits - 1) {
        m - (1i128 << bits)
    } else {
        m
    }
}

fn zero(ty: &Ty) -> V {
    match ty {
        Ty::Int(_) | Ty::Named(_) => V::Int(0),
        Ty::Bool => V::Bool(false),
        Ty::F32 => V::F32(0.0),
        Ty::F64 => V::F64(0.0),
        Ty::Str => V::Str(String::new()),
        Ty::Bytes => V::Bytes(Vec::new()),
        Ty::List(_) => V::List(Vec::new()),
    }
}

fn int(v: &V) -> R<i128> {
    match v {
        V::Int(i) => Ok(*i),
        other => Err(format!("expected an integer, got {other:?}")),
    }
}

fn boolean(v: &V) -> R<bool> {
    match v {
        V::Bool(b) => Ok(*b),
        other => Err(format!("expected a bool, got {other:?}")),
    }
}

impl Ctx<'_> {
    fn decode(&mut self, codec: CodecIdx, args: Vec<V>) -> R<V> {
        let body = &self.prog.codecs[codec].decode;
        let mut locals = vec![None; body.locals.len()];
        for (p, a) in body.params.iter().zip(args) {
            locals[*p] = Some(a);
        }
        match self.stmts(codec, body, &body.stmts, &mut locals)? {
            Flow::Return(v) => Ok(v),
            Flow::Next => Err(format!(
                "{}: decode fell off the end",
                self.prog.codecs[codec].ir
            )),
        }
    }

    fn encode(&mut self, codec: CodecIdx, args: Vec<V>, value: V) -> R<()> {
        let body = &self.prog.codecs[codec].encode;
        let mut locals = vec![None; body.locals.len()];
        for (p, a) in body.params.iter().zip(args) {
            locals[*p] = Some(a);
        }
        locals[body.value.ok_or("encode without a value")?] = Some(value);
        self.stmts(codec, body, &body.stmts, &mut locals)?;
        Ok(())
    }

    fn store(&self, body: &Body, b: &Bind, v: V, locals: &mut [Option<V>]) {
        let _ = body;
        locals[b.local] = Some(v);
    }

    fn read(&mut self, n: usize) -> R<&[u8]> {
        if self.data.len() - self.pos < n {
            return Err(format!("short buffer at {}", self.pos));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn stmts(
        &mut self,
        codec: CodecIdx,
        body: &Body,
        stmts: &[Stmt],
        locals: &mut Vec<Option<V>>,
    ) -> R<Flow> {
        let name = &self.prog.codecs[codec].ir;
        for s in stmts {
            match s {
                Stmt::Declare { local, zero: z } => {
                    let l = &body.locals[*local];
                    locals[*local] = if l.optional {
                        Some(V::Absent)
                    } else if *z {
                        Some(zero(&l.ty))
                    } else {
                        None
                    };
                }
                Stmt::Let { dst, value } => {
                    let v = self.expr(body, value, locals)?;
                    self.store(body, dst, v, locals);
                }
                Stmt::Read { dst, op } => {
                    let v = match op {
                        ReadOp::U8 => V::Int(i128::from(self.read(1)?[0])),
                        ReadOp::F32 => {
                            let b = self.read(4)?;
                            V::F32(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
                        }
                        ReadOp::F64 => {
                            let b = self.read(8)?;
                            V::F64(f64::from_be_bytes(b.try_into().map_err(|_| "f64")?))
                        }
                        ReadOp::Bytes(n) => {
                            let n = int(&self.expr(body, n, locals)?)?;
                            if n < 0 {
                                return Err(format!("{name}: negative length {n}"));
                            }
                            V::Bytes(self.read(n as usize)?.to_vec())
                        }
                        ReadOp::Rest => {
                            let n = self.data.len() - self.pos;
                            V::Bytes(self.read(n)?.to_vec())
                        }
                    };
                    self.store(body, dst, v, locals);
                }
                Stmt::Decode {
                    dst,
                    codec: c,
                    args,
                } => {
                    let args = args
                        .iter()
                        .map(|a| self.expr(body, a, locals))
                        .collect::<R<Vec<_>>>()?;
                    let v = self.decode(*c, args)?;
                    self.store(body, dst, v, locals);
                }
                Stmt::Utf8 { dst, src } => {
                    let V::Bytes(b) = self.expr(body, src, locals)? else {
                        return Err("utf8 of non-bytes".into());
                    };
                    let s = String::from_utf8(b).map_err(|_| format!("{name}: invalid utf-8"))?;
                    self.store(body, dst, V::Str(s), locals);
                }
                Stmt::Write { op, value } => {
                    let v = self.expr(body, value, locals)?;
                    match (op, v) {
                        (WriteOp::U8, V::Int(i)) => {
                            if !(0..=255).contains(&i) {
                                return Err(format!("{name}: u8 write of {i}"));
                            }
                            self.out.push(i as u8);
                        }
                        (WriteOp::F32, V::F32(f)) => self.out.extend(f.to_be_bytes()),
                        (WriteOp::F64, V::F64(f)) => self.out.extend(f.to_be_bytes()),
                        (WriteOp::Bytes(n), V::Bytes(mut b)) => {
                            let n = int(&self.expr(body, n, locals)?)?;
                            if n < 0 || b.len() as i128 > n {
                                return Err(format!("{name}: bytes({n}) got {}", b.len()));
                            }
                            b.resize(n as usize, 0);
                            self.out.extend(b);
                        }
                        (WriteOp::Rest, V::Bytes(b)) => self.out.extend(b),
                        (op, v) => return Err(format!("{name}: write {op:?} of {v:?}")),
                    }
                }
                Stmt::Encode {
                    codec: c,
                    args,
                    value,
                } => {
                    let args = args
                        .iter()
                        .map(|a| self.expr(body, a, locals))
                        .collect::<R<Vec<_>>>()?;
                    let v = self.expr(body, value, locals)?;
                    self.encode(*c, args, v)?;
                }
                Stmt::Require { cond, text } => {
                    if !boolean(&self.expr(body, cond, locals)?)? {
                        return Err(format!("{name}: require failed: {text}"));
                    }
                }
                Stmt::Unwrap { dst, src, field } => {
                    let v = self.expr(body, src, locals)?;
                    if v == V::Absent {
                        return Err(format!("{name}: missing {field}"));
                    }
                    self.store(body, dst, v, locals);
                }
                Stmt::If { cond, then, els } => {
                    let branch = if boolean(&self.expr(body, cond, locals)?)? {
                        then
                    } else {
                        els
                    };
                    if let Flow::Return(v) = self.stmts(codec, body, branch, locals)? {
                        return Ok(Flow::Return(v));
                    }
                }
                Stmt::Match {
                    scrutinee,
                    arms,
                    other,
                } => {
                    let tag = int(&self.expr(body, scrutinee, locals)?)?;
                    let branch = arms
                        .iter()
                        .find(|a| a.tags.contains(&tag))
                        .map(|a| &a.body)
                        .or(other.as_ref())
                        .ok_or_else(|| format!("{name}: no match arm for tag {tag}"))?;
                    if let Flow::Return(v) = self.stmts(codec, body, branch, locals)? {
                        return Ok(Flow::Return(v));
                    }
                }
                Stmt::Repeat {
                    mode,
                    list,
                    item,
                    body: inner,
                } => {
                    let mut items = Vec::new();
                    match mode {
                        RepeatMode::Count(n) => {
                            let n = int(&self.expr(body, n, locals)?)?;
                            if n < 0 {
                                return Err(format!("{name}: negative count {n}"));
                            }
                            for _ in 0..n {
                                self.stmts(codec, body, inner, locals)?;
                                items.push(locals[*item].clone().ok_or("item unset")?);
                            }
                        }
                        RepeatMode::Until(t) => {
                            let t = int(&self.expr(body, t, locals)?)?;
                            loop {
                                if self.pos == self.data.len() {
                                    break;
                                }
                                if i128::from(self.data[self.pos]) == t {
                                    self.pos += 1;
                                    break;
                                }
                                self.stmts(codec, body, inner, locals)?;
                                items.push(locals[*item].clone().ok_or("item unset")?);
                            }
                        }
                        RepeatMode::UntilEnd => {
                            while self.pos < self.data.len() {
                                self.stmts(codec, body, inner, locals)?;
                                items.push(locals[*item].clone().ok_or("item unset")?);
                            }
                        }
                    }
                    locals[*list] = Some(V::List(items));
                }
                Stmt::ForEach {
                    list,
                    item,
                    body: inner,
                } => {
                    let V::List(items) = self.expr(body, list, locals)? else {
                        return Err("for over non-list".into());
                    };
                    for it in items {
                        locals[*item] = Some(it);
                        self.stmts(codec, body, inner, locals)?;
                    }
                }
                Stmt::ExpectVariant {
                    dst, sum, variant, ..
                } => {
                    let V::Var(v, payload) = self.expr(body, sum, locals)? else {
                        return Err("expect on non-sum".into());
                    };
                    if v != *variant {
                        return Err(format!("{name}: wrong variant {v}, expected {variant}"));
                    }
                    if let Some(d) = dst {
                        let p = payload.ok_or("variant without payload")?;
                        self.store(body, d, *p, locals);
                    }
                }
                Stmt::Return(e) => return Ok(Flow::Return(self.expr(body, e, locals)?)),
                Stmt::Fail(Failure::EncodeUntilEnd) => {
                    return Err(format!("{name}: encode of until-end"));
                }
            }
        }
        Ok(Flow::Next)
    }

    fn expr(&self, body: &Body, e: &Expr, locals: &[Option<V>]) -> R<V> {
        Ok(match e {
            Expr::Int { value, ty } => {
                if !ty.contains(i128::from(*value)) {
                    return Err(format!("literal {value} does not fit {ty:?}"));
                }
                V::Int(i128::from(*value))
            }
            Expr::Bool(b) => V::Bool(*b),
            Expr::Str(s) => V::Str(s.clone()),
            Expr::Local(l) => locals[*l]
                .clone()
                .ok_or_else(|| format!("read of unset {}", body.locals[*l].name))?,
            Expr::Field { base, field, .. } => match self.expr(body, base, locals)? {
                V::Rec(fields) => fields[*field].clone(),
                other => return Err(format!("field of {other:?}")),
            },
            Expr::Present(inner) => V::Bool(self.expr(body, inner, locals)? != V::Absent),
            Expr::Get(inner) => match self.expr(body, inner, locals)? {
                V::Absent => return Err("get of an absent optional".into()),
                v => v,
            },
            Expr::Not(inner) => V::Bool(!boolean(&self.expr(body, inner, locals)?)?),
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } => {
                if *op == BinOp::And || *op == BinOp::Or {
                    let l = boolean(&self.expr(body, lhs, locals)?)?;
                    if (*op == BinOp::And && !l) || (*op == BinOp::Or && l) {
                        return Ok(V::Bool(l));
                    }
                    return Ok(V::Bool(boolean(&self.expr(body, rhs, locals)?)?));
                }
                let l = self.expr(body, lhs, locals)?;
                let r = self.expr(body, rhs, locals)?;
                if op.is_comparison() {
                    let ord = match (&l, &r) {
                        (V::Int(a), V::Int(b)) => a.cmp(b),
                        (V::Bool(a), V::Bool(b)) => a.cmp(b),
                        (V::Str(a), V::Str(b)) => a.cmp(b),
                        (V::F32(a), V::F32(b)) => a.partial_cmp(b).ok_or("nan")?,
                        (V::F64(a), V::F64(b)) => a.partial_cmp(b).ok_or("nan")?,
                        _ => return Err(format!("compare {l:?} {r:?}")),
                    };
                    use std::cmp::Ordering::*;
                    return Ok(V::Bool(match op {
                        BinOp::Eq => ord == Equal,
                        BinOp::Ne => ord != Equal,
                        BinOp::Lt => ord == Less,
                        BinOp::Le => ord != Greater,
                        BinOp::Gt => ord == Greater,
                        _ => ord != Less,
                    }));
                }
                let (a, b) = (int(&l)?, int(&r)?);
                let exact = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => {
                        let q = a / b;
                        if a % b != 0 && ((a < 0) != (b < 0)) {
                            q - 1
                        } else {
                            q
                        }
                    }
                    BinOp::Mod => {
                        let r = a % b;
                        if r != 0 && ((r < 0) != (b < 0)) {
                            r + b
                        } else {
                            r
                        }
                    }
                    BinOp::Shl => a << b,
                    BinOp::Shr => a >> b,
                    BinOp::BitAnd => a & b,
                    BinOp::BitOr => a | b,
                    BinOp::BitXor => a ^ b,
                    _ => unreachable!(),
                };
                let Ty::Int(t) = ty else {
                    return Err(format!("arithmetic typed {ty:?}"));
                };
                if !wraps && !t.contains(exact) {
                    return Err(format!(
                        "{op:?} of {a} and {b} leaves {t:?} without `wraps`"
                    ));
                }
                V::Int(wrap(exact, *t))
            }
            Expr::Cond {
                cond, then, els, ..
            } => {
                if boolean(&self.expr(body, cond, locals)?)? {
                    self.expr(body, then, locals)?
                } else {
                    self.expr(body, els, locals)?
                }
            }
            Expr::Cast { expr, to } => {
                let v = self.expr(body, expr, locals)?;
                match (v, to) {
                    (V::Int(i), Ty::Int(t)) => V::Int(wrap(i, *t)),
                    (V::Bool(b), Ty::Int(_)) => V::Int(i128::from(b)),
                    (V::Int(i), Ty::Named(_)) => V::Int(i),
                    (V::F32(f), Ty::F64) => V::F64(f64::from(f)),
                    (V::F64(f), Ty::F32) => V::F32(f as f32),
                    (v, to) => return Err(format!("cast {v:?} to {to:?}")),
                }
            }
            Expr::Call { func, args } => {
                let vs = args
                    .iter()
                    .map(|a| self.expr(body, a, locals))
                    .collect::<R<Vec<_>>>()?;
                match func {
                    Func::SignExtend => {
                        let (v, bits) = (int(&vs[0])?, int(&vs[1])?);
                        let mask = 1i128 << (bits - 1);
                        V::Int(wrap(
                            if v & mask != 0 { (v ^ mask) - mask } else { v },
                            IntTy::I64,
                        ))
                    }
                    Func::Utf16Units => match &vs[0] {
                        V::Str(s) => V::Int(s.encode_utf16().count() as i128),
                        v => return Err(format!("utf16 of {v:?}")),
                    },
                    Func::Utf8Len => match &vs[0] {
                        V::Str(s) => V::Int(s.len() as i128),
                        v => return Err(format!("utf8_len of {v:?}")),
                    },
                    Func::StrLen => match &vs[0] {
                        V::Str(s) => V::Int(s.chars().count() as i128),
                        v => return Err(format!("len of {v:?}")),
                    },
                    Func::BytesLen => match &vs[0] {
                        V::Bytes(b) => V::Int(b.len() as i128),
                        v => return Err(format!("len of {v:?}")),
                    },
                    Func::ListLen => match &vs[0] {
                        V::List(l) => V::Int(l.len() as i128),
                        v => return Err(format!("len of {v:?}")),
                    },
                    Func::Min => V::Int(int(&vs[0])?.min(int(&vs[1])?)),
                    Func::Max => V::Int(int(&vs[0])?.max(int(&vs[1])?)),
                }
            }
            Expr::StrBytes(inner) => match self.expr(body, inner, locals)? {
                V::Str(s) => V::Bytes(s.into_bytes()),
                v => return Err(format!("bytes of {v:?}")),
            },
            Expr::Remaining => V::Int((self.data.len() - self.pos) as i128),
            Expr::Construct { fields, .. } => V::Rec(
                fields
                    .iter()
                    .map(|f| self.expr(body, f, locals))
                    .collect::<R<Vec<_>>>()?,
            ),
            Expr::Variant {
                variant, payload, ..
            } => V::Var(
                *variant,
                match payload {
                    Some(p) => Some(Box::new(self.expr(body, p, locals)?)),
                    None => None,
                },
            ),
        })
    }
}

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn first_diff(a: &[u8], b: &[u8]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(i) => format!(
            "byte {i}: {:02x} vs {:02x} (lens {} vs {})",
            a[i],
            b[i],
            a.len(),
            b.len()
        ),
        None => format!("lens {} vs {}", a.len(), b.len()),
    }
}

/// Round-trips every synthesizable packet; returns (checked, failures).
fn check(rel: &str) -> (usize, Vec<String>) {
    let resolved = bytemill::analyze(&bytemill::load_schema(&[repo(rel)]).unwrap()).unwrap();
    let prog = typed(&resolved).unwrap();
    let mut failed = Vec::new();
    let mut n = 0;
    for packet in resolved.packets.values() {
        let Ok(value) = synthesize_packet(&resolved, packet) else {
            continue;
        };
        let Ok(bytes) = encode_packet(&resolved, &packet.name, &value) else {
            continue;
        };
        let codec = prog
            .packet_codec(&packet.name)
            .unwrap_or_else(|| panic!("{rel}: no codec for {}", packet.name));
        n += 1;
        let mut dec = Ctx {
            prog: &prog,
            data: &bytes,
            pos: 0,
            out: Vec::new(),
        };
        let decoded = match dec.decode(codec, Vec::new()) {
            Ok(v) => v,
            Err(e) => {
                failed.push(format!("{}: decode: {e}", packet.name));
                continue;
            }
        };
        let mut enc = Ctx {
            prog: &prog,
            data: &[],
            pos: 0,
            out: Vec::new(),
        };
        match enc.encode(codec, Vec::new(), decoded) {
            Ok(()) if enc.out == bytes => {}
            Ok(()) => failed.push(format!("{}: {}", packet.name, first_diff(&bytes, &enc.out))),
            Err(e) => failed.push(format!("{}: encode: {e}", packet.name)),
        }
    }
    (n, failed)
}

#[test]
fn every_schema_round_trips_through_the_typed_program() {
    let mut roots = vec![
        "schemas/minecraft_26_3_wiki.bps".to_string(),
        "schemas/hytale/2026_02_17.bps".to_string(),
    ];
    let mut versions: Vec<String> = std::fs::read_dir(repo("schemas/versions"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('v') && n.ends_with(".bps"))
        .map(|n| format!("schemas/versions/{n}"))
        .collect();
    versions.sort();
    roots.extend(versions);
    let mut report = Vec::new();
    for root in &roots {
        let (n, failed) = check(root);
        assert!(n > 0, "{root}: nothing to check");
        if !failed.is_empty() {
            report.push(format!(
                "{root}: {} of {n} disagree\n  {}",
                failed.len(),
                failed
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
        }
    }
    assert!(report.is_empty(), "{}", report.join("\n"));
}
