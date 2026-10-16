//! IR → [`Program`]. See the module docs in `typed/mod.rs` for the output's
//! meaning; this file only builds it.
//!
//! Two passes. The first finds which names every nested record or sum reads
//! from its parent (its captures), to a fixed point over decode and encode.
//! The second walks each root procedure, typing every expression and lifting
//! each nested block, `match` field, block arm, and list element into a codec
//! of its own, called with its captures.

use std::collections::{HashMap, HashSet};

use super::{
    BinOp, Bind, Body, Codec, CodecIdx, CodecKind, EnumMember, Expr, Failure, FieldDecl, FieldIdx,
    Func, IntTy, Local, LocalId, MatchArm, Name, Origin, Param, ParamRole, Program, ReadOp,
    RepeatMode, RouteDecl, RouteRef, Stmt, Ty, TypeDecl, TypeIdx, TypeKind, VariantDecl,
    VariantIdx, WriteOp,
};
use crate::ast::{Expr as AstExpr, ResolvedSchema};
use crate::errors::{BpsResult, emit};
use crate::ir::{IrArm, IrSchema, Stmt as IrStmt, Storage, Ty as IrTy, TypeDef, WireOp, lower};

const BUILTINS: [&str; 5] = ["u8", "f32be", "f64be", "bytes", "rest"];
const ELEM: &str = "__elem";

/// Lower `resolved` and build its typed program.
pub fn typed(resolved: &ResolvedSchema) -> BpsResult<Program> {
    let ir = lower(resolved)?;
    Builder::new(resolved, &ir)?.build()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RootKind {
    Packet(String),
    Encoding,
    Enum,
    Frame(String),
}

struct Builder<'a> {
    resolved: &'a ResolvedSchema,
    ir: &'a IrSchema,
    prog: Program,
    /// Root procedures that get a codec, in IR order.
    roots: Vec<(String, RootKind)>,
    /// IR type key → declared type (records, sums, enums, aliases).
    type_idx: HashMap<String, TypeIdx>,
    /// IR key → codec.
    codec_idx: HashMap<String, CodecIdx>,
    /// Nested type key → names it reads from its parent.
    captures: HashMap<String, Vec<String>>,
    /// Nested codecs whose encode body is built.
    encoded: HashSet<CodecIdx>,
    /// Codec names claimed so far.
    codec_names: Taken,
}

impl<'a> Builder<'a> {
    fn new(resolved: &'a ResolvedSchema, ir: &'a IrSchema) -> BpsResult<Self> {
        Ok(Builder {
            resolved,
            ir,
            prog: Program {
                types: Vec::new(),
                codecs: Vec::new(),
                routes: Vec::new(),
            },
            roots: Vec::new(),
            type_idx: HashMap::new(),
            codec_idx: HashMap::new(),
            captures: HashMap::new(),
            encoded: HashSet::new(),
            codec_names: Taken::default(),
        })
    }

    fn build(mut self) -> BpsResult<Program> {
        self.classify_roots()?;
        self.declare_types()?;
        self.find_captures()?;
        self.declare_root_codecs()?;
        for i in 0..self.roots.len() {
            let key = self.roots[i].0.clone();
            let codec = self.codec_idx[&key];
            self.build_root(codec)?;
        }
        self.finish()?;
        self.routes()?;
        Ok(self.prog)
    }

    // ------------------------------------------------------------------
    // Roots and types
    // ------------------------------------------------------------------

    fn classify_roots(&mut self) -> BpsResult<()> {
        for key in self.ir.decode.keys() {
            if BUILTINS.contains(&key) {
                continue;
            }
            let kind = if let Some(name) = key.strip_prefix("packet:") {
                RootKind::Packet(name.to_string())
            } else if self.resolved.enums.contains_key(key) {
                RootKind::Enum
            } else if self.resolved.encodings.contains_key(key) {
                RootKind::Encoding
            } else if self.resolved.packets.contains_key(key) {
                RootKind::Packet(key.to_string())
            } else if let Some(name) = key.strip_prefix("frame_") {
                RootKind::Frame(name.to_string())
            } else {
                return Err(emit(format!(
                    "typed: procedure {key} is not a packet, encoding, enum, or frame"
                )));
            };
            if self.is_fragment(key)? {
                continue;
            }
            self.roots.push((key.to_string(), kind));
        }
        Ok(())
    }

    /// An encoding that reads a name nothing in it binds (`nbt_payload` reads
    /// the `tag_type` of the encodings that spread it): only meaningful spread.
    fn is_fragment(&self, key: &str) -> BpsResult<bool> {
        let proc = get_proc(&self.ir.decode, key)?;
        let mut bound: HashSet<String> = proc.params.iter().cloned().collect();
        bound_names(&proc.body, &mut bound);
        let mut read = Vec::new();
        read_names(&proc.body, &mut read);
        if let Some(result) = &proc.result {
            expr_names(result, &mut read);
        }
        Ok(read.iter().any(|n| !bound.contains(n)))
    }

    fn root_kind(&self, key: &str) -> Option<&RootKind> {
        self.roots
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, kind)| kind)
    }

    fn is_record_alias(&self, key: &str) -> Option<String> {
        let Some(TypeDef::Record { fields }) = self.ir.types.get(key) else {
            return None;
        };
        if !fields.is_empty() {
            return None;
        }
        let proc = self.ir.decode.get(key)?;
        let [IrStmt::CallDecode { dst, callee, .. }] = proc.body.as_slice() else {
            return None;
        };
        if dst != "value" || !matches!(&proc.result, Some(AstExpr::Ident(n)) if n == "value") {
            return None;
        }
        match self.ir.types.get(&callee.0) {
            Some(TypeDef::Record { .. }) => Some(self.alias_target(&callee.0)),
            _ => None,
        }
    }

    fn alias_target(&self, key: &str) -> String {
        match self.is_record_alias(key) {
            Some(target) => target,
            None => key.to_string(),
        }
    }

    /// Declares every type a root reaches, names them, and fills their kinds.
    fn declare_types(&mut self) -> BpsResult<()> {
        // Reachable IR type keys from the roots' value types.
        let mut reach: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = Vec::new();
        for (key, _) in &self.roots {
            if matches!(
                self.ir.types.get(key),
                Some(TypeDef::Record { .. } | TypeDef::Sum { .. } | TypeDef::Enum { .. })
            ) {
                queue.push(key.clone());
            }
        }
        while let Some(key) = queue.pop() {
            if !reach.insert(key.clone()) {
                continue;
            }
            let mut targets = Vec::new();
            match self.ir.types.get(&key) {
                Some(TypeDef::Record { fields }) => {
                    if let Some(target) = self.is_record_alias(&key) {
                        targets.push(target);
                    }
                    for f in fields {
                        ir_named(&f.ty, &mut targets);
                    }
                }
                Some(TypeDef::Sum { arms }) => {
                    for arm in arms {
                        ir_named(&arm.ty, &mut targets);
                    }
                }
                _ => {}
            }
            for t in targets {
                if matches!(
                    self.ir.types.get(&t),
                    Some(TypeDef::Record { .. } | TypeDef::Sum { .. } | TypeDef::Enum { .. })
                ) {
                    queue.push(t);
                } else if !self.ir.types.contains_key(&t) {
                    return Err(emit(format!("typed: {key} refers to undeclared type {t}")));
                }
            }
        }
        // Indices in IR order.
        for (key, _) in self.ir.types.iter() {
            if reach.contains(key) {
                self.type_idx.insert(key.to_string(), self.prog.types.len());
                self.prog.types.push(TypeDecl {
                    name: Name::new(key),
                    ir: key.to_string(),
                    origin: Origin::Encoding {
                        name: key.to_string(),
                    },
                    kind: TypeKind::Record { fields: Vec::new() },
                    codec: None,
                });
            }
        }
        self.name_types()?;
        // Kinds.
        for idx in 0..self.prog.types.len() {
            let key = self.prog.types[idx].ir.clone();
            let kind = match get_type(self.ir, &key)? {
                TypeDef::Record { fields } => {
                    if let Some(target) = self.is_record_alias(&key) {
                        TypeKind::Alias {
                            target: self.type_idx[&target],
                        }
                    } else {
                        let mut taken = Taken::default();
                        let mut out = Vec::new();
                        for f in fields {
                            let name = taken.claim(Name::new(&f.name));
                            out.push(FieldDecl {
                                name,
                                ir: f.name.clone(),
                                ty: self.ty(&f.ty)?,
                                optional: f.optional,
                            });
                        }
                        TypeKind::Record { fields: out }
                    }
                }
                TypeDef::Sum { arms } => {
                    let mut taken = Taken::default();
                    let mut variants = Vec::new();
                    for arm in arms {
                        let name = taken.claim(variant_name(arm.tag));
                        variants.push(VariantDecl {
                            name,
                            tags: arm.tag.into_iter().collect(),
                            payload: Some(self.ty(&arm.ty)?),
                        });
                    }
                    TypeKind::Sum { variants }
                }
                TypeDef::Enum { base, members } => {
                    let base = int_ty(*base)
                        .ok_or_else(|| emit(format!("typed: enum {key} has a non-integer base")))?;
                    let mut taken = Taken::default();
                    TypeKind::Enum {
                        base,
                        members: members
                            .iter()
                            .map(|(name, value)| EnumMember {
                                name: taken.claim(Name::new(name)),
                                value: *value,
                            })
                            .collect(),
                    }
                }
                other => {
                    return Err(emit(format!(
                        "typed: {key} is not a declared type: {other:?}"
                    )));
                }
            };
            self.prog.types[idx].kind = kind;
        }
        Ok(())
    }

    /// Roots by their schema names, nested types by their parent's name and
    /// field (or variant). Unique under every casing.
    fn name_types(&mut self) -> BpsResult<()> {
        let mut taken = Taken::default();
        let mut named: HashSet<TypeIdx> = HashSet::new();
        let mut queue: Vec<TypeIdx> = Vec::new();
        for idx in 0..self.prog.types.len() {
            let key = self.prog.types[idx].ir.clone();
            let Some(kind) = self.root_kind(&key).cloned() else {
                continue;
            };
            let (name, origin) = self.root_name(&key, &kind);
            self.prog.types[idx].name = taken.claim(name);
            self.prog.types[idx].origin = origin;
            named.insert(idx);
            queue.push(idx);
        }
        let mut next = 0;
        while next < queue.len() {
            let parent = queue[next];
            next += 1;
            let key = self.prog.types[parent].ir.clone();
            let parent_name = self.prog.types[parent].name.clone();
            let mut children: Vec<(TypeIdx, Name, Origin)> = Vec::new();
            match get_type(self.ir, &key)? {
                TypeDef::Record { fields } => {
                    for f in fields {
                        let mut targets = Vec::new();
                        ir_named(&f.ty, &mut targets);
                        for t in targets {
                            if let Some(&child) = self.type_idx.get(&t)
                                && self.root_kind(&t).is_none()
                            {
                                children.push((
                                    child,
                                    parent_name.with(&f.name),
                                    Origin::Field {
                                        parent,
                                        field: f.name.clone(),
                                    },
                                ));
                            }
                        }
                    }
                }
                TypeDef::Sum { arms } => {
                    for (i, arm) in arms.iter().enumerate() {
                        let mut targets = Vec::new();
                        ir_named(&arm.ty, &mut targets);
                        for t in targets {
                            if let Some(&child) = self.type_idx.get(&t)
                                && self.root_kind(&t).is_none()
                            {
                                children.push((
                                    child,
                                    parent_name.join(&variant_name(arm.tag)),
                                    Origin::Variant {
                                        sum: parent,
                                        variant: i,
                                    },
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
            for (child, name, origin) in children {
                if named.insert(child) {
                    self.prog.types[child].name = taken.claim(name);
                    self.prog.types[child].origin = origin;
                    queue.push(child);
                }
            }
        }
        for idx in 0..self.prog.types.len() {
            if !named.contains(&idx) {
                let name = Name::new(&self.prog.types[idx].ir);
                self.prog.types[idx].name = taken.claim(name);
            }
        }
        Ok(())
    }

    fn root_name(&self, key: &str, kind: &RootKind) -> (Name, Origin) {
        match kind {
            RootKind::Packet(name) => {
                let mut n = Name::new(name);
                if key.starts_with("packet:") {
                    n = n.with("packet");
                }
                let routes = self
                    .resolved
                    .routes
                    .iter()
                    .filter(|(_, p)| &p.name == name)
                    .map(|(r, _)| RouteRef {
                        state: r.state.clone(),
                        direction: r.direction.clone(),
                        id: r.packet_id,
                    })
                    .collect();
                (
                    n,
                    Origin::Packet {
                        name: name.clone(),
                        routes,
                    },
                )
            }
            RootKind::Encoding => (
                Name::new(key),
                Origin::Encoding {
                    name: key.to_string(),
                },
            ),
            RootKind::Enum => (
                Name::new(key),
                Origin::Enum {
                    name: key.to_string(),
                },
            ),
            RootKind::Frame(name) => (
                Name::new(name).with("frame"),
                Origin::Frame { name: name.clone() },
            ),
        }
    }

    /// IR type → typed-program type.
    fn ty(&self, ty: &IrTy) -> BpsResult<Ty> {
        match ty {
            IrTy::Scalar(s) => Ok(storage_ty(*s)),
            IrTy::List(inner) => Ok(Ty::List(Box::new(self.ty(inner)?))),
            IrTy::Optional(inner) => self.ty(inner),
            IrTy::Named(id) => self.named_ty(&id.0),
        }
    }

    fn named_ty(&self, key: &str) -> BpsResult<Ty> {
        match self.ir.types.get(key) {
            Some(TypeDef::Scalar(s)) => Ok(storage_ty(*s)),
            Some(TypeDef::String) => Ok(Ty::Str),
            Some(TypeDef::Record { .. }) => {
                let target = self.alias_target(key);
                self.type_idx
                    .get(&target)
                    .map(|i| Ty::Named(*i))
                    .ok_or_else(|| {
                        emit(format!("typed: record {key} is not reachable from a codec"))
                    })
            }
            Some(TypeDef::Sum { .. } | TypeDef::Enum { .. }) => self
                .type_idx
                .get(key)
                .map(|i| Ty::Named(*i))
                .ok_or_else(|| emit(format!("typed: type {key} is not reachable from a codec"))),
            None => Err(emit(format!("typed: unknown type {key}"))),
        }
    }

    // ------------------------------------------------------------------
    // Captures (pass one)
    // ------------------------------------------------------------------

    fn find_captures(&mut self) -> BpsResult<()> {
        loop {
            let before = self.captures.clone();
            for i in 0..self.roots.len() {
                let key = self.roots[i].0.clone();
                let rec = self.record_key(&key);
                let ir = self.ir;
                let dec = get_proc(&ir.decode, &key)?;
                let mut bound: HashSet<String> = dec.params.iter().cloned().collect();
                if self.is_scalar(&key) {
                    bound_names(&dec.body, &mut bound);
                }
                let mut free = Vec::new();
                self.free_dec(&dec.body, rec.as_deref(), &mut bound, &mut free)?;
                let enc = get_proc(&ir.encode, &key)?;
                let mut bound: HashSet<String> = enc.params.iter().cloned().collect();
                bound.insert("value".into());
                if self.is_scalar(&key) {
                    bound_names(&dec.body, &mut bound);
                    bound_names(&enc.body, &mut bound);
                }
                if let Some(rec) = &rec {
                    for f in self.ir_fields(rec) {
                        bound.insert(f);
                    }
                }
                let mut free = Vec::new();
                self.free_enc(&enc.body, rec.as_deref(), &mut bound, &mut free)?;
            }
            if before == self.captures {
                return Ok(());
            }
        }
    }

    fn record_key(&self, key: &str) -> Option<String> {
        match self.ir.types.get(key) {
            Some(TypeDef::Record { .. }) if self.is_record_alias(key).is_none() => {
                Some(key.to_string())
            }
            _ => None,
        }
    }

    fn is_scalar(&self, key: &str) -> bool {
        matches!(self.ir.types.get(key), Some(TypeDef::Scalar(_)))
    }

    fn ir_fields(&self, rec: &str) -> Vec<String> {
        match self.ir.types.get(rec) {
            Some(TypeDef::Record { fields }) => fields.iter().map(|f| f.name.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// The IR type key of field `field` of record `rec` (or its list element).
    fn field_key(&self, rec: Option<&str>, field: &str) -> Option<String> {
        let TypeDef::Record { fields } = self.ir.types.get(rec?)? else {
            return None;
        };
        let f = fields.iter().find(|f| f.name == field)?;
        let mut targets = Vec::new();
        ir_named(&f.ty, &mut targets);
        targets.into_iter().next()
    }

    fn is_sum(&self, key: &str) -> bool {
        matches!(self.ir.types.get(key), Some(TypeDef::Sum { .. }))
    }

    /// The arm record key of the sum arm selected by `arm` (or the default).
    fn arm_key(&self, sum: &str, arm: Option<&IrArm>) -> Option<String> {
        let TypeDef::Sum { arms } = self.ir.types.get(sum)? else {
            return None;
        };
        let tag = arm.and_then(|a| a.tags.first().copied());
        let v = arms.iter().find(|v| v.tag == tag)?;
        let mut targets = Vec::new();
        ir_named(&v.ty, &mut targets);
        targets.into_iter().next()
    }

    fn note_captures(&mut self, key: &str, names: &[String]) {
        let entry = self.captures.entry(key.to_string()).or_default();
        for n in names {
            if !entry.contains(n) {
                entry.push(n.clone());
            }
        }
    }

    /// Free names of decode statements, flow-sensitive. Records every nested
    /// type's own free names in `self.captures`.
    fn free_dec(
        &mut self,
        stmts: &[IrStmt],
        rec: Option<&str>,
        bound: &mut HashSet<String>,
        free: &mut Vec<String>,
    ) -> BpsResult<()> {
        for stmt in stmts {
            match stmt {
                IrStmt::Let { name, expr } => {
                    refs(expr, bound, free);
                    bound.insert(name.clone());
                }
                IrStmt::Require(e) => refs(e, bound, free),
                IrStmt::If {
                    cond,
                    then_body,
                    else_body,
                    ..
                } => {
                    refs(cond, bound, free);
                    self.free_dec(then_body, rec, bound, free)?;
                    self.free_dec(else_body, rec, bound, free)?;
                }
                IrStmt::Match {
                    scrutinee,
                    arms,
                    default,
                } => {
                    refs(scrutinee, bound, free);
                    match typed_match(stmt, true) {
                        Some(dst) if self.field_key(rec, &dst).is_some_and(|k| self.is_sum(&k)) => {
                            let sum = self.field_key(rec, &dst).unwrap_or_default();
                            let own = self.free_sum_dec(&sum, arms, default.as_deref())?;
                            pass_up(&own, bound, free);
                            bound.insert(dst);
                        }
                        _ => {
                            for arm in arms {
                                self.free_dec(&arm.body, rec, bound, free)?;
                            }
                            if let Some(body) = default {
                                self.free_dec(body, rec, bound, free)?;
                            }
                        }
                    }
                }
                IrStmt::RepeatCount { n: e, elem, dst }
                | IrStmt::RepeatUntil { term: e, elem, dst } => {
                    refs(e, bound, free);
                    let own = self.free_elem_dec(elem, rec, dst)?;
                    pass_up(&own, bound, free);
                    bound.insert(dst.clone());
                }
                IrStmt::RepeatUntilEnd { elem, dst } => {
                    let own = self.free_elem_dec(elem, rec, dst)?;
                    pass_up(&own, bound, free);
                    bound.insert(dst.clone());
                }
                IrStmt::Block { body, dst } => {
                    let key = self
                        .field_key(rec, dst)
                        .ok_or_else(|| emit(format!("typed: block {dst} has no record type")))?;
                    let own = self.free_record_dec(&key, body)?;
                    pass_up(&own, bound, free);
                    bound.insert(dst.clone());
                }
                IrStmt::Read { dst, op } => {
                    if let WireOp::Bytes { len } = op {
                        refs(len, bound, free);
                    }
                    bound.insert(dst.clone());
                }
                IrStmt::CallDecode { dst, args, .. } => {
                    for a in args {
                        refs(a, bound, free);
                    }
                    bound.insert(dst.clone());
                }
                IrStmt::Utf8 { dst, src } => {
                    refs(&AstExpr::Ident(src.clone()), bound, free);
                    bound.insert(dst.clone());
                }
                IrStmt::Write { .. } | IrStmt::CallEncode { .. } => {}
            }
        }
        Ok(())
    }

    fn free_record_dec(&mut self, key: &str, body: &[IrStmt]) -> BpsResult<Vec<String>> {
        let mut bound = HashSet::new();
        let mut own = Vec::new();
        self.free_dec(body, Some(key), &mut bound, &mut own)?;
        self.note_captures(key, &own);
        Ok(own)
    }

    fn free_sum_dec(
        &mut self,
        sum: &str,
        arms: &[IrArm],
        default: Option<&[IrStmt]>,
    ) -> BpsResult<Vec<String>> {
        let mut own = Vec::new();
        let all: Vec<(Option<&IrArm>, &[IrStmt])> = arms
            .iter()
            .map(|a| (Some(a), a.body.as_slice()))
            .chain(default.map(|d| (None, d)))
            .collect();
        for (arm, body) in all {
            let mut bound = HashSet::new();
            for stmt in body {
                match stmt {
                    IrStmt::Block { body, .. } => {
                        let key = self
                            .arm_key(sum, arm)
                            .ok_or_else(|| emit(format!("typed: sum {sum} arm has no record")))?;
                        let inner = self.free_record_dec(&key, body)?;
                        pass_up(&inner, &bound, &mut own);
                    }
                    other => {
                        self.free_dec(std::slice::from_ref(other), None, &mut bound, &mut own)?
                    }
                }
            }
        }
        self.note_captures(sum, &own);
        Ok(own)
    }

    fn free_elem_dec(
        &mut self,
        elem: &[IrStmt],
        rec: Option<&str>,
        list: &str,
    ) -> BpsResult<Vec<String>> {
        let mut own = Vec::new();
        let mut bound = HashSet::new();
        let elem_key = self.field_key(rec, list);
        for stmt in elem {
            match stmt {
                IrStmt::Block { body, .. } => {
                    let key = elem_key.clone().ok_or_else(|| {
                        emit(format!("typed: element of {list} has no record type"))
                    })?;
                    let inner = self.free_record_dec(&key, body)?;
                    pass_up(&inner, &bound, &mut own);
                }
                IrStmt::Match {
                    scrutinee,
                    arms,
                    default,
                } if typed_match(stmt, true).is_some() => {
                    refs(scrutinee, &bound, &mut own);
                    let key = elem_key
                        .clone()
                        .ok_or_else(|| emit(format!("typed: element of {list} has no sum type")))?;
                    let inner = self.free_sum_dec(&key, arms, default.as_deref())?;
                    pass_up(&inner, &bound, &mut own);
                }
                other => self.free_dec(std::slice::from_ref(other), None, &mut bound, &mut own)?,
            }
        }
        Ok(own)
    }

    /// Free names of encode statements; a record's own fields are bound from
    /// the start.
    fn free_enc(
        &mut self,
        stmts: &[IrStmt],
        rec: Option<&str>,
        bound: &mut HashSet<String>,
        free: &mut Vec<String>,
    ) -> BpsResult<()> {
        for stmt in stmts {
            match stmt {
                IrStmt::Let { name, expr } => {
                    refs(expr, bound, free);
                    bound.insert(name.clone());
                }
                IrStmt::CallEncode { args, value, .. } => {
                    for a in args {
                        refs(a, bound, free);
                    }
                    refs(value, bound, free);
                }
                IrStmt::Write { op, value } => {
                    if let WireOp::Bytes { len } = op {
                        refs(len, bound, free);
                    }
                    refs(value, bound, free);
                }
                IrStmt::Require(e) => refs(e, bound, free),
                IrStmt::If {
                    cond,
                    then_body,
                    else_body,
                    ..
                } => {
                    refs(cond, bound, free);
                    self.free_enc(then_body, rec, bound, free)?;
                    self.free_enc(else_body, rec, bound, free)?;
                }
                IrStmt::Match {
                    scrutinee,
                    arms,
                    default,
                } => {
                    refs(scrutinee, bound, free);
                    match typed_match(stmt, false) {
                        Some(dst) if self.field_key(rec, &dst).is_some_and(|k| self.is_sum(&k)) => {
                            refs(&AstExpr::Ident(dst.clone()), bound, free);
                            let sum = self.field_key(rec, &dst).unwrap_or_default();
                            let own = self.free_sum_enc(&sum, arms, default.as_deref(), &dst)?;
                            pass_up(&own, bound, free);
                        }
                        _ => {
                            for arm in arms {
                                self.free_enc(&arm.body, rec, bound, free)?;
                            }
                            if let Some(body) = default {
                                self.free_enc(body, rec, bound, free)?;
                            }
                        }
                    }
                }
                IrStmt::RepeatCount { n: e, elem, dst }
                | IrStmt::RepeatUntil { term: e, elem, dst } => {
                    refs(e, bound, free);
                    refs(&AstExpr::Ident(dst.clone()), bound, free);
                    let own = self.free_elem_enc(elem, rec, dst)?;
                    pass_up(&own, bound, free);
                }
                IrStmt::RepeatUntilEnd { elem, dst } => {
                    refs(&AstExpr::Ident(dst.clone()), bound, free);
                    let own = self.free_elem_enc(elem, rec, dst)?;
                    pass_up(&own, bound, free);
                }
                IrStmt::Block { body, dst } => {
                    refs(&AstExpr::Ident(dst.clone()), bound, free);
                    let key = self
                        .field_key(rec, dst)
                        .ok_or_else(|| emit(format!("typed: block {dst} has no record type")))?;
                    let own = self.free_record_enc(&key, body)?;
                    pass_up(&own, bound, free);
                }
                IrStmt::Read { .. } | IrStmt::CallDecode { .. } | IrStmt::Utf8 { .. } => {}
            }
        }
        Ok(())
    }

    fn free_record_enc(&mut self, key: &str, body: &[IrStmt]) -> BpsResult<Vec<String>> {
        let mut bound: HashSet<String> = self.ir_fields(key).into_iter().collect();
        let mut own = Vec::new();
        self.free_enc(body, Some(key), &mut bound, &mut own)?;
        self.note_captures(key, &own);
        Ok(own)
    }

    fn free_sum_enc(
        &mut self,
        sum: &str,
        arms: &[IrArm],
        default: Option<&[IrStmt]>,
        dst: &str,
    ) -> BpsResult<Vec<String>> {
        let mut own = Vec::new();
        let all: Vec<(Option<&IrArm>, &[IrStmt])> = arms
            .iter()
            .map(|a| (Some(a), a.body.as_slice()))
            .chain(default.map(|d| (None, d)))
            .collect();
        for (arm, body) in all {
            let mut bound: HashSet<String> = HashSet::from([dst.to_string()]);
            for stmt in body {
                match stmt {
                    IrStmt::Block { body, .. } => {
                        let key = self
                            .arm_key(sum, arm)
                            .ok_or_else(|| emit(format!("typed: sum {sum} arm has no record")))?;
                        let inner = self.free_record_enc(&key, body)?;
                        pass_up(&inner, &bound, &mut own);
                    }
                    other => {
                        self.free_enc(std::slice::from_ref(other), None, &mut bound, &mut own)?
                    }
                }
            }
        }
        self.note_captures(sum, &own);
        Ok(own)
    }

    fn free_elem_enc(
        &mut self,
        elem: &[IrStmt],
        rec: Option<&str>,
        list: &str,
    ) -> BpsResult<Vec<String>> {
        let mut own = Vec::new();
        let mut bound: HashSet<String> = HashSet::from([ELEM.to_string()]);
        let elem_key = self.field_key(rec, list);
        for stmt in elem {
            match stmt {
                IrStmt::Block { body, .. } => {
                    let key = elem_key.clone().ok_or_else(|| {
                        emit(format!("typed: element of {list} has no record type"))
                    })?;
                    let inner = self.free_record_enc(&key, body)?;
                    pass_up(&inner, &bound, &mut own);
                }
                IrStmt::Match {
                    scrutinee,
                    arms,
                    default,
                } if typed_match(stmt, false).is_some() => {
                    refs(scrutinee, &bound, &mut own);
                    let key = elem_key
                        .clone()
                        .ok_or_else(|| emit(format!("typed: element of {list} has no sum type")))?;
                    let inner = self.free_sum_enc(&key, arms, default.as_deref(), ELEM)?;
                    pass_up(&inner, &bound, &mut own);
                }
                other => self.free_enc(std::slice::from_ref(other), None, &mut bound, &mut own)?,
            }
        }
        Ok(own)
    }

    // ------------------------------------------------------------------
    // Codecs (pass two)
    // ------------------------------------------------------------------

    fn declare_root_codecs(&mut self) -> BpsResult<()> {
        let mut taken = Taken::default();
        for i in 0..self.roots.len() {
            let (key, kind) = self.roots[i].clone();
            let (codec_kind, ty) = match self.ir.types.get(&key) {
                Some(TypeDef::Scalar(_)) => (CodecKind::Scalar, None),
                Some(TypeDef::String) => (CodecKind::String, None),
                Some(TypeDef::Enum { .. }) => (CodecKind::Enum, Some(self.type_idx[&key])),
                Some(TypeDef::Record { .. }) if self.is_record_alias(&key).is_some() => {
                    (CodecKind::Alias, Some(self.type_idx[&key]))
                }
                Some(TypeDef::Record { .. }) => (CodecKind::Record, Some(self.type_idx[&key])),
                other => return Err(emit(format!("typed: root {key} has type {other:?}"))),
            };
            let (name, origin) = match ty {
                Some(t) => (
                    self.prog.types[t].name.clone(),
                    self.prog.types[t].origin.clone(),
                ),
                None => self.root_name(&key, &kind),
            };
            let name = taken.claim(name);
            let value = self.named_ty(&key)?;
            let params = get_proc(&self.ir.decode, &key)?
                .params
                .iter()
                .map(|p| Param {
                    name: Name::new(p),
                    ty: Ty::Int(IntTy::I64),
                    optional: false,
                    role: ParamRole::Arg,
                })
                .collect();
            let idx = self.prog.codecs.len();
            self.prog.codecs.push(Codec {
                name,
                ir: key.clone(),
                origin,
                kind: codec_kind,
                value,
                ty,
                params,
                decode: empty_body(),
                encode: empty_body(),
            });
            self.codec_idx.insert(key.clone(), idx);
            if let Some(t) = ty {
                self.prog.types[t].codec = Some(idx);
            }
        }
        self.codec_names = taken;
        Ok(())
    }

    fn build_root(&mut self, codec: CodecIdx) -> BpsResult<()> {
        let key = self.prog.codecs[codec].ir.clone();
        let ir = self.ir;
        let dec_proc = get_proc(&ir.decode, &key)?;
        let enc_proc = get_proc(&ir.encode, &key)?;
        let kind = self.prog.codecs[codec].kind;
        let rec = self.prog.codecs[codec]
            .ty
            .filter(|_| kind == CodecKind::Record);
        let scalar = kind == CodecKind::Scalar;

        // Decode.
        let mut f = Fun::new(codec, rec);
        for p in &dec_proc.params {
            let local = f.local(Name::new(p), Ty::Int(IntTy::I64), false);
            f.params.push(local);
            f.bind_root(p, Binding::Local(local));
        }
        let types = self.decl_types(&dec_proc.body)?;
        if scalar {
            self.prebind(&mut f, &dec_proc.body, &types)?;
        }
        if let Some(rec) = rec {
            self.declare_fields(&mut f, rec, &dec_proc.body)?;
        }
        self.dec_stmts(&mut f, &dec_proc.body)?;
        let value_ty = self.prog.codecs[codec].value.clone();
        match (&dec_proc.result, rec) {
            (_, Some(rec)) => {
                let ret = self.construct(&mut f, rec)?;
                f.emit(Stmt::Return(ret));
            }
            (Some(result), None) => {
                let te = self.expr(&mut f, result, Mode::Stmt)?;
                let ret = self.coerce(te, &value_ty)?;
                f.emit(Stmt::Return(ret));
            }
            (None, None) => return Err(emit(format!("typed: {key} decodes no value"))),
        }
        self.prog.codecs[codec].decode = f.finish();

        // Encode.
        let mut f = Fun::new(codec, rec);
        let value = f.local(self.input_name(rec, codec), value_ty.clone(), false);
        f.value = Some(value);
        for p in &enc_proc.params {
            let local = f.local(Name::new(p), Ty::Int(IntTy::I64), false);
            f.params.push(local);
            f.bind_root(p, Binding::Local(local));
        }
        if scalar {
            let mut all = dec_proc.body.clone();
            all.extend(enc_proc.body.iter().cloned());
            self.prebind(&mut f, &all, &types)?;
        }
        f.bind_root("value", Binding::Local(value));
        if let Some(rec) = rec {
            self.bind_fields(&mut f, rec, value);
        }
        f.decl = types;
        self.enc_stmts(&mut f, &enc_proc.body)?;
        self.prog.codecs[codec].encode = f.finish();
        Ok(())
    }

    /// Declared type of each name a decode body binds (first binding; a
    /// `utf8` rebinding makes it a string).
    fn decl_types(&self, stmts: &[IrStmt]) -> BpsResult<HashMap<String, Ty>> {
        let mut out = HashMap::new();
        self.decl_types_into(stmts, &mut out)?;
        Ok(out)
    }

    fn decl_types_into(&self, stmts: &[IrStmt], out: &mut HashMap<String, Ty>) -> BpsResult<()> {
        for stmt in stmts {
            match stmt {
                IrStmt::CallDecode { dst, callee, .. } => {
                    let ty = self.named_ty(&callee.0)?;
                    out.entry(dst.clone()).or_insert(ty);
                }
                IrStmt::Read { dst, op } => {
                    out.entry(dst.clone()).or_insert(read_ty(op));
                }
                IrStmt::Utf8 { dst, .. } => {
                    out.insert(dst.clone(), Ty::Str);
                }
                IrStmt::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    self.decl_types_into(then_body, out)?;
                    self.decl_types_into(else_body, out)?;
                }
                IrStmt::Match { arms, default, .. } => {
                    for a in arms {
                        self.decl_types_into(&a.body, out)?;
                    }
                    if let Some(d) = default {
                        self.decl_types_into(d, out)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Scalar procedures start every bound name at zero (`decode_scalar`).
    fn prebind(
        &mut self,
        f: &mut Fun,
        stmts: &[IrStmt],
        types: &HashMap<String, Ty>,
    ) -> BpsResult<()> {
        let mut names = Vec::new();
        prebind_names(stmts, &mut names);
        for name in names {
            if f.scopes[0].contains_key(&name) {
                continue;
            }
            let ty = types.get(&name).cloned().unwrap_or(Ty::Int(IntTy::I64));
            let local = f.local(Name::new(&name), ty, false);
            f.emit(Stmt::Declare { local, zero: true });
            f.bind_root(&name, Binding::Local(local));
            f.prebound.insert(name);
        }
        Ok(())
    }

    /// Decode: a record field bound inside an `if` or `match` arm, or optional,
    /// is declared before the statements; the rest at their binding.
    fn declare_fields(&mut self, f: &mut Fun, rec: TypeIdx, stmts: &[IrStmt]) -> BpsResult<()> {
        let mut top = Vec::new();
        let mut nested = Vec::new();
        top_and_nested(stmts, &mut top, &mut nested);
        let fields = self.prog.fields(rec).to_vec();
        for field in &fields {
            let upfront = field.optional
                || nested.contains(&field.ir)
                || top.iter().filter(|n| **n == field.ir).count() > 1;
            if upfront {
                let local = f.local(field.name.clone(), field.ty.clone(), field.optional);
                f.emit(Stmt::Declare { local, zero: false });
                f.field_locals.insert(field.ir.clone(), local);
            }
        }
        Ok(())
    }

    fn bind_fields(&self, f: &mut Fun, rec: TypeIdx, value: LocalId) {
        for (i, field) in self.prog.fields(rec).iter().enumerate() {
            f.bind_root(
                &field.ir,
                Binding::Field {
                    base: value,
                    ty: rec,
                    field: i,
                },
            );
        }
    }

    fn construct(&self, f: &mut Fun, rec: TypeIdx) -> BpsResult<Expr> {
        let mut fields = Vec::new();
        for field in self.prog.fields(rec) {
            let local = f
                .field_locals
                .get(&field.ir)
                .copied()
                .ok_or_else(|| emit(format!("typed: field {} is never decoded", field.ir)))?;
            fields.push(Expr::Local(local));
        }
        Ok(Expr::Construct { ty: rec, fields })
    }

    // ------------------------------------------------------------------
    // Decode statements
    // ------------------------------------------------------------------

    fn dec_stmts(&mut self, f: &mut Fun, stmts: &[IrStmt]) -> BpsResult<()> {
        for (i, stmt) in stmts.iter().enumerate() {
            f.rest.push(stmts[i + 1..].to_vec());
            let result = self.dec_stmt(f, stmt);
            f.rest.pop();
            result?;
        }
        Ok(())
    }

    fn dec_block(&mut self, f: &mut Fun, stmts: &[IrStmt]) -> BpsResult<Vec<Stmt>> {
        f.push_block();
        let result = self.dec_stmts(f, stmts);
        let out = f.pop_block();
        result?;
        Ok(out)
    }

    fn dec_stmt(&mut self, f: &mut Fun, stmt: &IrStmt) -> BpsResult<()> {
        match stmt {
            IrStmt::Let { name, expr } => {
                let te = self.expr(f, expr, Mode::Stmt)?;
                let ty = self.target_ty(f, name).unwrap_or_else(|| te.ty.clone());
                let value = self.coerce(te, &ty)?;
                let dst = self.bind(f, name, &ty)?;
                f.emit(Stmt::Let {
                    dst: dst.bind,
                    value,
                });
                self.after_bind(f, dst);
            }
            IrStmt::Require(e) => {
                let te = self.expr(f, e, Mode::Stmt)?;
                let cond = self.as_bool(te)?;
                f.emit(Stmt::Require {
                    cond,
                    text: source(e),
                });
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
                fallback,
            } => {
                let cond = if *fallback {
                    let (c, refs) = self.guard(f, cond)?;
                    let c = match all_present(&refs) {
                        Some(all) => or(not(all), c),
                        None => c,
                    };
                    and(c, gt0(Expr::Remaining))
                } else {
                    let te = self.expr(f, cond, Mode::Stmt)?;
                    self.as_bool(te)?
                };
                let then = self.dec_block(f, then_body)?;
                let els = self.dec_block(f, else_body)?;
                f.emit(Stmt::If { cond, then, els });
            }
            IrStmt::Match {
                scrutinee,
                arms,
                default,
            } => {
                if let Some(dst) = typed_match(stmt, true)
                    && let Some(sum) = self.field_sum(f, &dst)
                {
                    let tag = self.expr(f, scrutinee, Mode::Stmt)?;
                    let tag = self.as_i64(tag)?;
                    let codec = self.lift_sum(f, sum, arms, default.as_deref(), &dst, true)?;
                    let mut args = vec![tag];
                    args.extend(self.capture_args(f, codec)?);
                    let ty = Ty::Named(sum);
                    let dst = self.bind(f, &dst, &ty)?;
                    f.emit(Stmt::Decode {
                        dst: dst.bind,
                        codec,
                        args,
                    });
                    self.after_bind(f, dst);
                } else {
                    let te = self.expr(f, scrutinee, Mode::Stmt)?;
                    let scrutinee = self.as_i64(te)?;
                    let mut out = Vec::new();
                    for arm in arms {
                        out.push(MatchArm {
                            tags: arm.tags.clone(),
                            body: self.dec_block(f, &arm.body)?,
                        });
                    }
                    let other = match default {
                        Some(body) => Some(self.dec_block(f, body)?),
                        None => None,
                    };
                    f.emit(Stmt::Match {
                        scrutinee,
                        arms: out,
                        other,
                    });
                }
            }
            IrStmt::RepeatCount { n, elem, dst } => {
                let te = self.expr(f, n, Mode::Stmt)?;
                let count = self.as_i64(te)?;
                self.dec_repeat(f, RepeatMode::Count(count), elem, dst)?;
            }
            IrStmt::RepeatUntil { term, elem, dst } => {
                let te = self.expr(f, term, Mode::Stmt)?;
                let term = self.as_i64(te)?;
                self.dec_repeat(f, RepeatMode::Until(term), elem, dst)?;
            }
            IrStmt::RepeatUntilEnd { elem, dst } => {
                self.dec_repeat(f, RepeatMode::UntilEnd, elem, dst)?
            }
            IrStmt::Block { body, dst } => {
                let rec = self
                    .field_record(f, dst)
                    .ok_or_else(|| emit(format!("typed: block {dst} has no record type")))?;
                let codec = self.lift_record(f, rec, body, true)?;
                let args = self.capture_args(f, codec)?;
                let dst = self.bind(f, dst, &Ty::Named(rec))?;
                f.emit(Stmt::Decode {
                    dst: dst.bind,
                    codec,
                    args,
                });
                self.after_bind(f, dst);
            }
            IrStmt::Read { dst, op } => {
                let op = self.read_op(f, op)?;
                let ty = read_op_ty(&op);
                let dst = self.bind(f, dst, &ty)?;
                f.emit(Stmt::Read { dst: dst.bind, op });
                self.after_bind(f, dst);
            }
            IrStmt::CallDecode { dst, callee, args } => {
                let (stmt_fn, ty) = self.call_decode(f, &callee.0, args)?;
                let dst = self.bind(f, dst, &ty)?;
                f.emit(stmt_fn(dst.bind));
                self.after_bind(f, dst);
            }
            IrStmt::Utf8 { dst, src } => {
                let te = self.expr(f, &AstExpr::Ident(src.clone()), Mode::Stmt)?;
                if te.ty != Ty::Bytes {
                    return Err(emit(format!("typed: utf8 of {src}, which is not bytes")));
                }
                let dst = self.bind(f, dst, &Ty::Str)?;
                f.emit(Stmt::Utf8 {
                    dst: dst.bind,
                    src: te.e,
                });
                self.after_bind(f, dst);
            }
            IrStmt::Write { .. } | IrStmt::CallEncode { .. } => {
                return Err(emit("typed: a write in a decode procedure"));
            }
        }
        Ok(())
    }

    /// A decode call of a builtin (`Read`) or codec (`Decode`), finished once
    /// its destination is known.
    fn call_decode(
        &mut self,
        f: &mut Fun,
        callee: &str,
        args: &[AstExpr],
    ) -> BpsResult<(StmtFn, Ty)> {
        if BUILTINS.contains(&callee) {
            let op = match callee {
                "u8" => ReadOp::U8,
                "f32be" => ReadOp::F32,
                "f64be" => ReadOp::F64,
                "rest" => ReadOp::Rest,
                _ => {
                    let len = args
                        .first()
                        .ok_or_else(|| emit("typed: bytes without a length"))?;
                    let te = self.expr(f, len, Mode::Stmt)?;
                    ReadOp::Bytes(self.as_i64(te)?)
                }
            };
            let ty = read_op_ty(&op);
            return Ok((Box::new(move |dst| Stmt::Read { dst, op }), ty));
        }
        let codec = *self
            .codec_idx
            .get(callee)
            .ok_or_else(|| emit(format!("typed: call of {callee}, which has no codec")))?;
        let args = self.call_args(f, codec, args)?;
        let ty = self.prog.codecs[codec].value.clone();
        Ok((Box::new(move |dst| Stmt::Decode { dst, codec, args }), ty))
    }

    fn call_args(
        &mut self,
        f: &mut Fun,
        codec: CodecIdx,
        args: &[AstExpr],
    ) -> BpsResult<Vec<Expr>> {
        let n = self.prog.codecs[codec].params.len();
        if args.len() != n {
            return Err(emit(format!(
                "typed: {} takes {n} arguments, got {}",
                self.prog.codecs[codec].ir,
                args.len()
            )));
        }
        let mut out = Vec::new();
        for a in args {
            let te = self.expr(f, a, Mode::Stmt)?;
            out.push(self.as_i64(te)?);
        }
        Ok(out)
    }

    fn read_op(&mut self, f: &mut Fun, op: &WireOp) -> BpsResult<ReadOp> {
        Ok(match op {
            WireOp::U8 => ReadOp::U8,
            WireOp::F32be => ReadOp::F32,
            WireOp::F64be => ReadOp::F64,
            WireOp::Rest => ReadOp::Rest,
            WireOp::Bytes { len } => {
                let te = self.expr(f, len, Mode::Stmt)?;
                ReadOp::Bytes(self.as_i64(te)?)
            }
        })
    }

    fn dec_repeat(
        &mut self,
        f: &mut Fun,
        mode: RepeatMode,
        elem: &[IrStmt],
        dst: &str,
    ) -> BpsResult<()> {
        let list_ty = self
            .target_ty(f, dst)
            .ok_or_else(|| emit(format!("typed: list {dst} has no type")))?;
        let Ty::List(item_ty) = list_ty.clone() else {
            return Err(emit(format!("typed: {dst} repeats but is not a list")));
        };
        let dst_bind = self.bind(f, dst, &list_ty)?;
        let list = if dst_bind.bind.declare && !dst_bind.bind.some {
            dst_bind.bind.local
        } else {
            f.local(Name::new(dst).with("list"), list_ty.clone(), false)
        };
        let item = f.local(Name::new(dst).with("item"), (*item_ty).clone(), false);
        f.push_block();
        f.bind_here(ELEM, Binding::Local(item));
        let item_bind = Bind {
            local: item,
            declare: true,
            some: false,
        };
        let result = self.dec_elem(f, elem, dst, &item_ty, item_bind);
        let body = f.pop_block();
        result?;
        f.emit(Stmt::Repeat {
            mode,
            list,
            item,
            body,
        });
        if list != dst_bind.bind.local {
            f.emit(Stmt::Let {
                dst: dst_bind.bind,
                value: Expr::Local(list),
            });
        }
        self.after_bind(f, dst_bind);
        Ok(())
    }

    /// One list element, decoded into `item`.
    fn dec_elem(
        &mut self,
        f: &mut Fun,
        elem: &[IrStmt],
        list: &str,
        item_ty: &Ty,
        item: Bind,
    ) -> BpsResult<()> {
        match elem {
            [IrStmt::CallDecode { dst, callee, args }] if dst == ELEM => {
                let (stmt_fn, ty) = self.call_decode(f, &callee.0, args)?;
                check_ty(&ty, item_ty, list)?;
                f.emit(stmt_fn(item));
            }
            [
                IrStmt::CallDecode { dst, callee, args },
                IrStmt::Utf8 { dst: d2, src },
            ] if dst == ELEM && d2 == ELEM && src == ELEM => {
                let (stmt_fn, ty) = self.call_decode(f, &callee.0, args)?;
                let raw = f.local(Name::new(list).with("bytes"), ty, false);
                f.emit(stmt_fn(Bind {
                    local: raw,
                    declare: true,
                    some: false,
                }));
                f.emit(Stmt::Utf8 {
                    dst: item,
                    src: Expr::Local(raw),
                });
            }
            [IrStmt::Block { body, dst }] if dst == ELEM => {
                let Ty::Named(rec) = item_ty else {
                    return Err(emit(format!(
                        "typed: element of {list} is a block but not a record"
                    )));
                };
                let codec = self.lift_record(f, *rec, body, true)?;
                let args = self.capture_args(f, codec)?;
                f.emit(Stmt::Decode {
                    dst: item,
                    codec,
                    args,
                });
            }
            [
                IrStmt::Match {
                    scrutinee,
                    arms,
                    default,
                },
            ] if typed_match(&elem[0], true).as_deref() == Some(ELEM) => {
                let Ty::Named(sum) = item_ty else {
                    return Err(emit(format!(
                        "typed: element of {list} is a match but not a sum"
                    )));
                };
                let tag = self.expr(f, scrutinee, Mode::Stmt)?;
                let tag = self.as_i64(tag)?;
                let codec = self.lift_sum(f, *sum, arms, default.as_deref(), ELEM, true)?;
                let mut args = vec![tag];
                args.extend(self.capture_args(f, codec)?);
                f.emit(Stmt::Decode {
                    dst: item,
                    codec,
                    args,
                });
            }
            other => {
                return Err(emit(format!(
                    "typed: unsupported element of {list}: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// Where a decode binding of `name` goes. See `after_bind`.
    fn bind(&mut self, f: &mut Fun, name: &str, ty: &Ty) -> BpsResult<Bound> {
        if f.prebound.contains(name) {
            let local = match f.lookup(name) {
                Some(Binding::Local(l)) => l,
                _ => return Err(emit(format!("typed: prebound {name} is not a local"))),
            };
            return Ok(Bound {
                bind: Bind {
                    local,
                    declare: false,
                    some: false,
                },
                name: name.into(),
                temp: None,
                field: None,
            });
        }
        let field = f.rec.and_then(|rec| {
            self.prog
                .fields(rec)
                .iter()
                .position(|fd| fd.ir == name)
                .map(|i| (rec, i))
        });
        let Some((rec, idx)) = field else {
            // A local of a non-record body (string codecs) or a scalar's name.
            let local = f.local(Name::new(name), ty.clone(), false);
            return Ok(Bound {
                bind: Bind {
                    local,
                    declare: true,
                    some: false,
                },
                name: name.into(),
                temp: None,
                field: None,
            });
        };
        let decl = self.prog.fields(rec)[idx].clone();
        if &decl.ty != ty {
            // An intermediate value (bytes before `utf8`): a temporary.
            let local = f.local(Name::new(name).with("raw"), ty.clone(), false);
            return Ok(Bound {
                bind: Bind {
                    local,
                    declare: true,
                    some: false,
                },
                name: name.into(),
                temp: Some(local),
                field: None,
            });
        }
        if let Some(&upfront) = f.field_locals.get(name) {
            if decl.optional && f.read_later(name) {
                let temp = f.local(Name::new(name).with("value"), ty.clone(), false);
                return Ok(Bound {
                    bind: Bind {
                        local: temp,
                        declare: true,
                        some: false,
                    },
                    name: name.into(),
                    temp: Some(temp),
                    field: Some(upfront),
                });
            }
            return Ok(Bound {
                bind: Bind {
                    local: upfront,
                    declare: false,
                    some: decl.optional,
                },
                name: name.into(),
                temp: None,
                field: Some(upfront),
            });
        }
        let local = f.local(decl.name.clone(), ty.clone(), false);
        f.field_locals.insert(name.to_string(), local);
        Ok(Bound {
            bind: Bind {
                local,
                declare: true,
                some: false,
            },
            name: name.into(),
            temp: None,
            field: Some(local),
        })
    }

    fn after_bind(&mut self, f: &mut Fun, b: Bound) {
        match (b.temp, b.field) {
            (Some(temp), Some(field)) => {
                // Optional field read again in this block: keep the value.
                f.emit(Stmt::Let {
                    dst: Bind {
                        local: field,
                        declare: false,
                        some: true,
                    },
                    value: Expr::Local(temp),
                });
                f.bind_root(&b.name, Binding::Local(field));
                f.bind_here(&b.name, Binding::Local(temp));
            }
            (Some(temp), None) => f.bind_here(&b.name, Binding::Local(temp)),
            (None, Some(field)) => f.bind_root(&b.name, Binding::Local(field)),
            (None, None) => {
                if f.prebound.contains(&b.name) {
                    return;
                }
                f.bind_here(&b.name, Binding::Local(b.bind.local));
            }
        }
    }

    /// The declared type of a name in this body: its record field's, else the
    /// type it was decoded with.
    fn target_ty(&self, f: &Fun, name: &str) -> Option<Ty> {
        if let Some(rec) = f.rec
            && let Some(fd) = self.prog.fields(rec).iter().find(|fd| fd.ir == name)
        {
            return Some(fd.ty.clone());
        }
        if let Some(Binding::Local(l)) = f.lookup(name)
            && f.prebound.contains(name)
        {
            return Some(f.locals[l].ty.clone());
        }
        f.decl.get(name).cloned()
    }

    fn field_sum(&self, f: &Fun, name: &str) -> Option<TypeIdx> {
        match self.target_ty(f, name)? {
            Ty::Named(t) if matches!(self.prog.types[t].kind, TypeKind::Sum { .. }) => Some(t),
            _ => None,
        }
    }

    fn field_record(&self, f: &Fun, name: &str) -> Option<TypeIdx> {
        match self.target_ty(f, name)? {
            Ty::Named(t) if matches!(self.prog.types[t].kind, TypeKind::Record { .. }) => Some(t),
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Encode statements
    // ------------------------------------------------------------------

    fn enc_stmts(&mut self, f: &mut Fun, stmts: &[IrStmt]) -> BpsResult<()> {
        if f.rec.is_some() {
            self.hoist_enc_lets(f, stmts)?;
        }
        for (i, stmt) in stmts.iter().enumerate() {
            f.rest.push(stmts[i + 1..].to_vec());
            let result = self.enc_stmt(f, stmt);
            f.rest.pop();
            result?;
        }
        Ok(())
    }

    fn enc_block(&mut self, f: &mut Fun, stmts: &[IrStmt]) -> BpsResult<Vec<Stmt>> {
        f.push_block();
        let mut result = Ok(());
        for (i, stmt) in stmts.iter().enumerate() {
            f.rest.push(stmts[i + 1..].to_vec());
            result = self.enc_stmt(f, stmt);
            f.rest.pop();
            if result.is_err() {
                break;
            }
        }
        let out = f.pop_block();
        result?;
        Ok(out)
    }

    /// A computed value bound inside an `if` or `match` arm and read after it
    /// lives in a local declared first, starting at the field's own value.
    fn hoist_enc_lets(&mut self, f: &mut Fun, stmts: &[IrStmt]) -> BpsResult<()> {
        for (i, stmt) in stmts.iter().enumerate() {
            let mut inner = Vec::new();
            if let IrStmt::If {
                then_body,
                else_body,
                ..
            } = stmt
            {
                computed_lets(then_body, &mut inner);
                computed_lets(else_body, &mut inner);
            } else if let IrStmt::Match { arms, default, .. } = stmt {
                for a in arms {
                    computed_lets(&a.body, &mut inner);
                }
                if let Some(d) = default {
                    computed_lets(d, &mut inner);
                }
            }
            for name in inner {
                if f.hoisted.contains_key(&name) || !reads(&stmts[i + 1..], &name) {
                    continue;
                }
                let rec = f.rec.unwrap_or_default();
                let field = self.prog.fields(rec).iter().position(|fd| fd.ir == name);
                let (ty, optional, init) = match field {
                    Some(idx) => {
                        let fd = &self.prog.fields(rec)[idx];
                        let base = Expr::Local(f.value.unwrap_or_default());
                        (
                            fd.ty.clone(),
                            fd.optional,
                            Some(Expr::Field {
                                base: Box::new(base),
                                ty: rec,
                                field: idx,
                            }),
                        )
                    }
                    None => (
                        f.decl.get(&name).cloned().unwrap_or(Ty::Int(IntTy::I64)),
                        false,
                        None,
                    ),
                };
                let local = f.local(Name::new(&name), ty, optional);
                match init {
                    Some(value) => f.emit(Stmt::Let {
                        dst: Bind {
                            local,
                            declare: true,
                            some: false,
                        },
                        value,
                    }),
                    None => f.emit(Stmt::Declare { local, zero: true }),
                }
                f.bind_root(&name, Binding::Local(local));
                f.hoisted.insert(name, local);
            }
        }
        Ok(())
    }

    fn enc_stmt(&mut self, f: &mut Fun, stmt: &IrStmt) -> BpsResult<()> {
        match stmt {
            IrStmt::Let { name, expr } => {
                let identity = matches!(expr, AstExpr::Ident(n) if n == name);
                if identity && !f.prebound.contains(name) {
                    match f.lookup(name) {
                        Some(Binding::Field { base, ty, field })
                            if self.prog.fields(ty)[field].optional =>
                        {
                            let decl = self.prog.fields(ty)[field].clone();
                            let local = f.local(decl.name.clone(), decl.ty.clone(), false);
                            let src = Expr::Field {
                                base: Box::new(Expr::Local(base)),
                                ty,
                                field,
                            };
                            f.emit(Stmt::Unwrap {
                                dst: Bind {
                                    local,
                                    declare: true,
                                    some: false,
                                },
                                src,
                                field: name.clone(),
                            });
                            f.bind_here(name, Binding::Local(local));
                        }
                        Some(Binding::Local(l)) if f.locals[l].optional => {
                            let local =
                                f.local(f.locals[l].name.clone(), f.locals[l].ty.clone(), false);
                            f.emit(Stmt::Unwrap {
                                dst: Bind {
                                    local,
                                    declare: true,
                                    some: false,
                                },
                                src: Expr::Local(l),
                                field: name.clone(),
                            });
                            f.bind_here(name, Binding::Local(local));
                        }
                        Some(_) => {}
                        None => {
                            return Err(emit(format!(
                                "typed: encode reads {name}, which is not bound"
                            )));
                        }
                    }
                    return Ok(());
                }
                let te = self.expr(f, expr, Mode::Stmt)?;
                let ty = self.enc_target_ty(f, name).unwrap_or_else(|| te.ty.clone());
                let value = self.coerce(te, &ty)?;
                if f.prebound.contains(name) {
                    let Some(Binding::Local(local)) = f.lookup(name) else {
                        return Err(emit(format!("typed: prebound {name} is not a local")));
                    };
                    f.emit(Stmt::Let {
                        dst: Bind {
                            local,
                            declare: false,
                            some: false,
                        },
                        value,
                    });
                } else if let Some(&local) = f.hoisted.get(name) {
                    let some = f.locals[local].optional;
                    f.emit(Stmt::Let {
                        dst: Bind {
                            local,
                            declare: false,
                            some,
                        },
                        value,
                    });
                    if some {
                        let tmp = f.local(Name::new(name).with("value"), ty, false);
                        // Keep reads in this block on the non-optional value.
                        if let Some(Stmt::Let { value, .. }) = f.sink().pop() {
                            f.emit(Stmt::Let {
                                dst: Bind {
                                    local: tmp,
                                    declare: true,
                                    some: false,
                                },
                                value,
                            });
                            f.emit(Stmt::Let {
                                dst: Bind {
                                    local,
                                    declare: false,
                                    some: true,
                                },
                                value: Expr::Local(tmp),
                            });
                            f.bind_here(name, Binding::Local(tmp));
                        }
                    }
                } else {
                    let local = f.local(Name::new(name), ty, false);
                    f.emit(Stmt::Let {
                        dst: Bind {
                            local,
                            declare: true,
                            some: false,
                        },
                        value,
                    });
                    f.bind_here(name, Binding::Local(local));
                }
            }
            IrStmt::CallEncode {
                callee,
                args,
                value,
            } => {
                let te = self.expr(f, value, Mode::Stmt)?;
                let stmt = self.call_encode(f, &callee.0, args, te)?;
                f.emit(stmt);
            }
            IrStmt::Write { op, value } => {
                let te = self.expr(f, value, Mode::Stmt)?;
                let stmt = self.write(f, op, te)?;
                f.emit(stmt);
            }
            IrStmt::Require(e) => {
                let te = self.expr(f, e, Mode::Stmt)?;
                let cond = self.as_bool(te)?;
                f.emit(Stmt::Require {
                    cond,
                    text: source(e),
                });
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
                fallback,
            } => {
                let cond = if *fallback {
                    let (c, refs) = self.guard(f, cond)?;
                    match all_present(&refs) {
                        None => c,
                        Some(all) => match self.any_present(f, then_body) {
                            None => or(not(all), c),
                            Some(any) => or(and(all.clone(), c), and(not(all), any)),
                        },
                    }
                } else {
                    let te = self.expr(f, cond, Mode::Stmt)?;
                    self.as_bool(te)?
                };
                let then = self.enc_block(f, then_body)?;
                let els = self.enc_block(f, else_body)?;
                f.emit(Stmt::If { cond, then, els });
            }
            IrStmt::Match {
                scrutinee,
                arms,
                default,
            } => {
                if let Some(dst) = typed_match(stmt, false)
                    && let Some(sum) = self.enc_field_sum(f, &dst)
                {
                    let value = self.expr(f, &AstExpr::Ident(dst.clone()), Mode::Stmt)?;
                    let tag = self.expr(f, scrutinee, Mode::Stmt)?;
                    let tag = self.as_i64(tag)?;
                    let codec = self.lift_sum(f, sum, arms, default.as_deref(), &dst, false)?;
                    let mut args = vec![tag];
                    args.extend(self.capture_args(f, codec)?);
                    f.emit(Stmt::Encode {
                        codec,
                        args,
                        value: value.e,
                    });
                } else {
                    let te = self.expr(f, scrutinee, Mode::Stmt)?;
                    let scrutinee = self.as_i64(te)?;
                    let mut out = Vec::new();
                    for arm in arms {
                        out.push(MatchArm {
                            tags: arm.tags.clone(),
                            body: self.enc_block(f, &arm.body)?,
                        });
                    }
                    let other = match default {
                        Some(body) => Some(self.enc_block(f, body)?),
                        None => None,
                    };
                    f.emit(Stmt::Match {
                        scrutinee,
                        arms: out,
                        other,
                    });
                }
            }
            IrStmt::RepeatCount { elem, dst, .. } => self.enc_repeat(f, elem, dst, None)?,
            IrStmt::RepeatUntil { term, elem, dst } => self.enc_repeat(f, elem, dst, Some(term))?,
            IrStmt::RepeatUntilEnd { .. } => f.emit(Stmt::Fail(Failure::EncodeUntilEnd)),
            IrStmt::Block { body, dst } => {
                let rec = self
                    .enc_field_record(f, dst)
                    .ok_or_else(|| emit(format!("typed: block {dst} has no record type")))?;
                let value = self.expr(f, &AstExpr::Ident(dst.clone()), Mode::Stmt)?;
                let codec = self.lift_record(f, rec, body, false)?;
                let args = self.capture_args(f, codec)?;
                f.emit(Stmt::Encode {
                    codec,
                    args,
                    value: value.e,
                });
            }
            IrStmt::Read { .. } | IrStmt::CallDecode { .. } | IrStmt::Utf8 { .. } => {
                return Err(emit("typed: a read in an encode procedure"));
            }
        }
        Ok(())
    }

    fn enc_target_ty(&self, f: &Fun, name: &str) -> Option<Ty> {
        if let Some(rec) = f.rec
            && let Some(fd) = self.prog.fields(rec).iter().find(|fd| fd.ir == name)
        {
            return Some(fd.ty.clone());
        }
        if let Some(Binding::Local(l)) = f.lookup(name)
            && (f.prebound.contains(name) || f.hoisted.contains_key(name))
        {
            return Some(f.locals[l].ty.clone());
        }
        f.decl.get(name).cloned()
    }

    fn enc_field_sum(&self, f: &Fun, name: &str) -> Option<TypeIdx> {
        match self.enc_target_ty(f, name)? {
            Ty::Named(t) if matches!(self.prog.types[t].kind, TypeKind::Sum { .. }) => Some(t),
            _ => None,
        }
    }

    fn enc_field_record(&self, f: &Fun, name: &str) -> Option<TypeIdx> {
        match self.enc_target_ty(f, name)? {
            Ty::Named(t) if matches!(self.prog.types[t].kind, TypeKind::Record { .. }) => Some(t),
            _ => None,
        }
    }

    fn call_encode(
        &mut self,
        f: &mut Fun,
        callee: &str,
        args: &[AstExpr],
        value: TE,
    ) -> BpsResult<Stmt> {
        if BUILTINS.contains(&callee) {
            let op = match callee {
                "u8" => WireOp::U8,
                "f32be" => WireOp::F32be,
                "f64be" => WireOp::F64be,
                "rest" => WireOp::Rest,
                _ => WireOp::Bytes {
                    len: args
                        .first()
                        .cloned()
                        .ok_or_else(|| emit("typed: bytes without a length"))?,
                },
            };
            return self.write(f, &op, value);
        }
        let codec = *self
            .codec_idx
            .get(callee)
            .ok_or_else(|| emit(format!("typed: call of {callee}, which has no codec")))?;
        let args = self.call_args(f, codec, args)?;
        let ty = self.prog.codecs[codec].value.clone();
        let value = self.coerce(value, &ty)?;
        Ok(Stmt::Encode { codec, args, value })
    }

    fn write(&mut self, f: &mut Fun, op: &WireOp, value: TE) -> BpsResult<Stmt> {
        Ok(match op {
            WireOp::U8 => Stmt::Write {
                op: WriteOp::U8,
                value: self.coerce(value, &Ty::Int(IntTy::U8))?,
            },
            WireOp::F32be => Stmt::Write {
                op: WriteOp::F32,
                value: self.coerce(value, &Ty::F32)?,
            },
            WireOp::F64be => Stmt::Write {
                op: WriteOp::F64,
                value: self.coerce(value, &Ty::F64)?,
            },
            WireOp::Rest => Stmt::Write {
                op: WriteOp::Rest,
                value: self.coerce(value, &Ty::Bytes)?,
            },
            WireOp::Bytes { len } => {
                let te = self.expr(f, len, Mode::Stmt)?;
                let len = self.as_i64(te)?;
                Stmt::Write {
                    op: WriteOp::Bytes(len),
                    value: self.coerce(value, &Ty::Bytes)?,
                }
            }
        })
    }

    fn enc_repeat(
        &mut self,
        f: &mut Fun,
        elem: &[IrStmt],
        dst: &str,
        term: Option<&AstExpr>,
    ) -> BpsResult<()> {
        let list_ty = self
            .enc_target_ty(f, dst)
            .ok_or_else(|| emit(format!("typed: list {dst} has no type")))?;
        let Ty::List(item_ty) = list_ty else {
            return Err(emit(format!("typed: {dst} repeats but is not a list")));
        };
        let list = self.expr(f, &AstExpr::Ident(dst.to_string()), Mode::Stmt)?;
        let item = f.local(Name::new(dst).with("item"), (*item_ty).clone(), false);
        f.push_block();
        f.bind_here(ELEM, Binding::Local(item));
        let result = self.enc_elem(f, elem, dst, &item_ty, item);
        let body = f.pop_block();
        result?;
        f.emit(Stmt::ForEach {
            list: list.e,
            item,
            body,
        });
        if let Some(term) = term {
            let te = self.expr(f, term, Mode::Stmt)?;
            let value = self.coerce(te, &Ty::Int(IntTy::U8))?;
            f.emit(Stmt::Write {
                op: WriteOp::U8,
                value,
            });
        }
        Ok(())
    }

    fn enc_elem(
        &mut self,
        f: &mut Fun,
        elem: &[IrStmt],
        list: &str,
        item_ty: &Ty,
        item: LocalId,
    ) -> BpsResult<()> {
        match elem {
            [
                IrStmt::CallEncode {
                    callee,
                    args,
                    value: AstExpr::Ident(n),
                },
            ] if n == ELEM => {
                let te = self.local_te(f, item);
                let stmt = self.call_encode(f, &callee.0, args, te)?;
                f.emit(stmt);
            }
            [IrStmt::Block { body, dst }] if dst == ELEM => {
                let Ty::Named(rec) = item_ty else {
                    return Err(emit(format!(
                        "typed: element of {list} is a block but not a record"
                    )));
                };
                let codec = self.lift_record(f, *rec, body, false)?;
                let args = self.capture_args(f, codec)?;
                f.emit(Stmt::Encode {
                    codec,
                    args,
                    value: Expr::Local(item),
                });
            }
            [
                IrStmt::Match {
                    scrutinee,
                    arms,
                    default,
                },
            ] if typed_match(&elem[0], false).as_deref() == Some(ELEM) => {
                let Ty::Named(sum) = item_ty else {
                    return Err(emit(format!(
                        "typed: element of {list} is a match but not a sum"
                    )));
                };
                let tag = self.expr(f, scrutinee, Mode::Stmt)?;
                let tag = self.as_i64(tag)?;
                let codec = self.lift_sum(f, *sum, arms, default.as_deref(), ELEM, false)?;
                let mut args = vec![tag];
                args.extend(self.capture_args(f, codec)?);
                f.emit(Stmt::Encode {
                    codec,
                    args,
                    value: Expr::Local(item),
                });
            }
            other => {
                return Err(emit(format!(
                    "typed: unsupported element of {list}: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// `Present` of the optional fields an encode group binds, OR-ed; `None`
    /// when one of them is always present.
    fn any_present(&self, f: &Fun, stmts: &[IrStmt]) -> Option<Expr> {
        let mut names = Vec::new();
        enc_bound_names(stmts, &mut names);
        let mut terms = Vec::new();
        for name in names {
            match f.lookup(&name) {
                Some(Binding::Field { base, ty, field })
                    if self.prog.fields(ty)[field].optional =>
                {
                    terms.push(Expr::Present(Box::new(Expr::Field {
                        base: Box::new(Expr::Local(base)),
                        ty,
                        field,
                    })));
                }
                Some(Binding::Local(l)) if f.locals[l].optional => {
                    terms.push(Expr::Present(Box::new(Expr::Local(l))));
                }
                _ => return None,
            }
        }
        terms.into_iter().reduce(or).or(Some(Expr::Bool(false)))
    }

    // ------------------------------------------------------------------
    // Lifting nested records and sums
    // ------------------------------------------------------------------

    /// The codec of nested record `rec`, with this direction's body built.
    fn lift_record(
        &mut self,
        parent: &mut Fun,
        rec: TypeIdx,
        body: &[IrStmt],
        decode: bool,
    ) -> BpsResult<CodecIdx> {
        let codec = self.nested_codec(parent, rec, CodecKind::Record)?;
        if decode {
            let mut f = Fun::new(codec, Some(rec));
            self.bind_params(&mut f, codec);
            self.declare_fields(&mut f, rec, body)?;
            self.dec_stmts(&mut f, body)?;
            let ret = self.construct(&mut f, rec)?;
            f.emit(Stmt::Return(ret));
            self.prog.codecs[codec].decode = f.finish();
        } else {
            if !self.encoded.insert(codec) {
                return Err(emit(format!(
                    "typed: {} is encoded twice",
                    self.prog.codecs[codec].ir
                )));
            }
            let mut f = Fun::new(codec, Some(rec));
            let value = f.local(self.input_name(Some(rec), codec), Ty::Named(rec), false);
            f.value = Some(value);
            self.bind_params(&mut f, codec);
            f.bind_root("value", Binding::Local(value));
            self.bind_fields(&mut f, rec, value);
            self.enc_stmts(&mut f, body)?;
            self.prog.codecs[codec].encode = f.finish();
        }
        Ok(codec)
    }

    /// The codec of sum `sum` (chosen by its `tag` parameter).
    fn lift_sum(
        &mut self,
        parent: &mut Fun,
        sum: TypeIdx,
        arms: &[IrArm],
        default: Option<&[IrStmt]>,
        dst: &str,
        decode: bool,
    ) -> BpsResult<CodecIdx> {
        let codec = self.nested_codec(parent, sum, CodecKind::Sum)?;
        let mut f = Fun::new(codec, None);
        let value = if decode {
            None
        } else {
            if !self.encoded.insert(codec) {
                return Err(emit(format!(
                    "typed: {} is encoded twice",
                    self.prog.codecs[codec].ir
                )));
            }
            let v = f.local(self.input_name(None, codec), Ty::Named(sum), false);
            f.value = Some(v);
            Some(v)
        };
        self.bind_params(&mut f, codec);
        let tag = f.params[0];
        let mut out = Vec::new();
        for arm in arms {
            let variant = self.variant_of(sum, Some(&arm.tags))?;
            let body = self.sum_arm(&mut f, sum, variant, &arm.body, dst, value)?;
            out.push(MatchArm {
                tags: arm.tags.clone(),
                body,
            });
        }
        let other = match default {
            Some(body) => {
                let variant = self.variant_of(sum, None)?;
                Some(self.sum_arm(&mut f, sum, variant, body, dst, value)?)
            }
            None => None,
        };
        f.emit(Stmt::Match {
            scrutinee: Expr::Local(tag),
            arms: out,
            other,
        });
        let body = f.finish();
        if decode {
            self.prog.codecs[codec].decode = body;
        } else {
            self.prog.codecs[codec].encode = body;
        }
        Ok(codec)
    }

    fn variant_of(&mut self, sum: TypeIdx, tags: Option<&[i128]>) -> BpsResult<VariantIdx> {
        let first = tags.and_then(|t| t.first().copied());
        let key = self.prog.types[sum].ir.clone();
        let Some(TypeDef::Sum { arms }) = self.ir.types.get(&key) else {
            return Err(emit(format!("typed: {key} is not a sum")));
        };
        let v = arms
            .iter()
            .position(|a| a.tag == first)
            .ok_or_else(|| emit(format!("typed: sum {key} has no arm for {first:?}")))?;
        if let (Some(tags), TypeKind::Sum { variants }) = (tags, &mut self.prog.types[sum].kind) {
            variants[v].tags = tags.to_vec();
        }
        Ok(v)
    }

    /// One arm of a sum codec: decode the payload and return the variant, or
    /// check the variant and encode its payload.
    fn sum_arm(
        &mut self,
        f: &mut Fun,
        sum: TypeIdx,
        variant: VariantIdx,
        body: &[IrStmt],
        dst: &str,
        value: Option<LocalId>,
    ) -> BpsResult<Vec<Stmt>> {
        let payload_ty = self.prog.variants(sum)[variant].payload.clone();
        f.push_block();
        let result = (|| -> BpsResult<()> {
            if body.is_empty() {
                if let TypeKind::Sum { variants } = &mut self.prog.types[sum].kind {
                    variants[variant].payload = None;
                }
                match value {
                    None => f.emit(Stmt::Return(Expr::Variant {
                        ty: sum,
                        variant,
                        payload: None,
                    })),
                    Some(v) => f.emit(Stmt::ExpectVariant {
                        dst: None,
                        sum: Expr::Local(v),
                        ty: sum,
                        variant,
                    }),
                }
                return Ok(());
            }
            let payload_ty =
                payload_ty.ok_or_else(|| emit("typed: sum arm without a payload type"))?;
            let name = self.prog.variants(sum)[variant].name.clone();
            let payload = f.local(name, payload_ty.clone(), false);
            match value {
                None => {
                    let bind = Bind {
                        local: payload,
                        declare: true,
                        some: false,
                    };
                    self.sum_payload_dec(f, body, dst, &payload_ty, bind)?;
                    f.emit(Stmt::Return(Expr::Variant {
                        ty: sum,
                        variant,
                        payload: Some(Box::new(Expr::Local(payload))),
                    }));
                }
                Some(v) => {
                    f.emit(Stmt::ExpectVariant {
                        dst: Some(Bind {
                            local: payload,
                            declare: true,
                            some: false,
                        }),
                        sum: Expr::Local(v),
                        ty: sum,
                        variant,
                    });
                    f.bind_here(dst, Binding::Local(payload));
                    self.sum_payload_enc(f, body, dst, &payload_ty, payload)?;
                }
            }
            Ok(())
        })();
        let out = f.pop_block();
        result?;
        Ok(out)
    }

    fn sum_payload_dec(
        &mut self,
        f: &mut Fun,
        body: &[IrStmt],
        dst: &str,
        ty: &Ty,
        bind: Bind,
    ) -> BpsResult<()> {
        match body {
            [
                IrStmt::CallDecode {
                    dst: d,
                    callee,
                    args,
                },
            ] if d == dst => {
                let (stmt_fn, got) = self.call_decode(f, &callee.0, args)?;
                check_ty(&got, ty, dst)?;
                f.emit(stmt_fn(bind));
            }
            [
                IrStmt::CallDecode {
                    dst: d,
                    callee,
                    args,
                },
                IrStmt::Utf8 { .. },
            ] if d == dst => {
                let (stmt_fn, got) = self.call_decode(f, &callee.0, args)?;
                let raw = f.local(Name::new(dst).with("bytes"), got, false);
                f.emit(stmt_fn(Bind {
                    local: raw,
                    declare: true,
                    some: false,
                }));
                f.emit(Stmt::Utf8 {
                    dst: bind,
                    src: Expr::Local(raw),
                });
            }
            [IrStmt::Block { body, dst: d }] if d == dst => {
                let Ty::Named(rec) = ty else {
                    return Err(emit(format!("typed: block arm of {dst} is not a record")));
                };
                let codec = self.lift_record(f, *rec, body, true)?;
                let args = self.capture_args(f, codec)?;
                f.emit(Stmt::Decode {
                    dst: bind,
                    codec,
                    args,
                });
            }
            other => {
                return Err(emit(format!(
                    "typed: unsupported sum arm for {dst}: {other:?}"
                )));
            }
        }
        Ok(())
    }

    fn sum_payload_enc(
        &mut self,
        f: &mut Fun,
        body: &[IrStmt],
        dst: &str,
        ty: &Ty,
        payload: LocalId,
    ) -> BpsResult<()> {
        match body {
            [
                IrStmt::CallEncode {
                    callee,
                    args,
                    value: AstExpr::Ident(n),
                },
            ] if n == dst => {
                let te = self.local_te(f, payload);
                let stmt = self.call_encode(f, &callee.0, args, te)?;
                f.emit(stmt);
            }
            [IrStmt::Block { body, dst: d }] if d == dst => {
                let Ty::Named(rec) = ty else {
                    return Err(emit(format!("typed: block arm of {dst} is not a record")));
                };
                let codec = self.lift_record(f, *rec, body, false)?;
                let args = self.capture_args(f, codec)?;
                f.emit(Stmt::Encode {
                    codec,
                    args,
                    value: Expr::Local(payload),
                });
            }
            other => {
                return Err(emit(format!(
                    "typed: unsupported sum arm for {dst}: {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// The codec of nested type `ty`, created at its first use with one
    /// parameter per capture, typed as the parent binds it there.
    fn nested_codec(
        &mut self,
        parent: &mut Fun,
        ty: TypeIdx,
        kind: CodecKind,
    ) -> BpsResult<CodecIdx> {
        let key = self.prog.types[ty].ir.clone();
        if let Some(&c) = self.codec_idx.get(&key) {
            return Ok(c);
        }
        let mut params = Vec::new();
        if kind == CodecKind::Sum {
            params.push(Param {
                name: Name::new("tag"),
                ty: Ty::Int(IntTy::I64),
                optional: false,
                role: ParamRole::Tag,
            });
        }
        for name in self.captures.get(&key).cloned().unwrap_or_default() {
            let (ty, optional) = self.peek(parent, &name)?;
            params.push(Param {
                name: Name::new(&name),
                ty,
                optional,
                role: ParamRole::Capture { ir: name.clone() },
            });
        }
        let decl = &self.prog.types[ty];
        let name = self.codec_names.claim(decl.name.clone());
        let idx = self.prog.codecs.len();
        self.prog.codecs.push(Codec {
            name,
            ir: key.clone(),
            origin: decl.origin.clone(),
            kind,
            value: Ty::Named(ty),
            ty: Some(ty),
            params,
            decode: empty_body(),
            encode: empty_body(),
        });
        self.codec_idx.insert(key, idx);
        self.prog.types[ty].codec = Some(idx);
        Ok(idx)
    }

    fn bind_params(&self, f: &mut Fun, codec: CodecIdx) {
        for p in &self.prog.codecs[codec].params {
            let local = f.local(p.name.clone(), p.ty.clone(), p.optional);
            f.params.push(local);
            match &p.role {
                ParamRole::Capture { ir } => f.bind_root(ir, Binding::Local(local)),
                ParamRole::Arg => f.bind_root(&p.name.snake(), Binding::Local(local)),
                ParamRole::Tag => {}
            }
        }
    }

    /// The parent's values for a nested codec's captures (optional ones stay
    /// optional).
    fn capture_args(&mut self, f: &mut Fun, codec: CodecIdx) -> BpsResult<Vec<Expr>> {
        let params = self.prog.codecs[codec].params.clone();
        let mut out = Vec::new();
        for p in params {
            let ParamRole::Capture { ir } = &p.role else {
                continue;
            };
            let (ty, optional) = self.peek(f, ir)?;
            if ty != p.ty || optional != p.optional {
                return Err(emit(format!(
                    "typed: {} reads {ir} as {:?}{}, but the caller has {:?}{}",
                    self.prog.codecs[codec].ir,
                    p.ty,
                    if p.optional { "?" } else { "" },
                    ty,
                    if optional { "?" } else { "" },
                )));
            }
            out.push(
                f.binding_expr(
                    f.lookup(ir)
                        .ok_or_else(|| emit(format!("typed: {ir} is not bound")))?,
                ),
            );
        }
        Ok(out)
    }

    /// Type and optionality of a name, without unwrapping it.
    fn peek(&self, f: &Fun, name: &str) -> BpsResult<(Ty, bool)> {
        match f.lookup(name) {
            Some(Binding::Local(l)) => Ok((f.locals[l].ty.clone(), f.locals[l].optional)),
            Some(Binding::Field { ty, field, .. }) => {
                let fd = &self.prog.fields(ty)[field];
                Ok((fd.ty.clone(), fd.optional))
            }
            None => Err(emit(format!(
                "typed: {} reads {name}, which is not bound",
                self.prog.codecs[f.codec].ir
            ))),
        }
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    fn local_te(&self, f: &Fun, local: LocalId) -> TE {
        TE::leaf(Expr::Local(local), f.locals[local].ty.clone())
    }

    /// A guard: its condition and the optional values it reads (`Get`).
    fn guard(&mut self, f: &mut Fun, cond: &AstExpr) -> BpsResult<(Expr, Vec<Expr>)> {
        f.guard_refs = Some(Vec::new());
        let te = self.expr(f, cond, Mode::Guard);
        let refs = f.guard_refs.take().unwrap_or_default();
        let c = self.as_bool(te?)?;
        Ok((c, refs))
    }

    fn expr(&mut self, f: &mut Fun, e: &AstExpr, mode: Mode) -> BpsResult<TE> {
        match e {
            AstExpr::Int(v) => {
                let value = i64_literal(*v)?;
                Ok(TE {
                    e: Expr::Int {
                        value,
                        ty: IntTy::I64,
                    },
                    ty: Ty::Int(IntTy::I64),
                    lit: true,
                    lo: value.into(),
                    hi: value.into(),
                })
            }
            AstExpr::Bool(b) => Ok(TE::leaf(Expr::Bool(*b), Ty::Bool)),
            AstExpr::Str(s) => Ok(TE::leaf(Expr::Str(s.clone()), Ty::Str)),
            AstExpr::Ident(name) => self.read(f, name, mode),
            AstExpr::Path(p) => Err(emit(format!("typed: dotted path {p} is not lowered"))),
            AstExpr::Unop(op, inner) => {
                if op != "!" {
                    return Err(emit(format!("typed: unary {op} is not lowered")));
                }
                let te = self.expr(f, inner, mode)?;
                Ok(TE::leaf(not(self.as_bool(te)?), Ty::Bool))
            }
            AstExpr::Binop(op, l, r) => {
                let l = self.expr(f, l, mode)?;
                let r = self.expr(f, r, mode)?;
                self.binop(op, l, r)
            }
            AstExpr::Ternary(c, a, b) => {
                let c = self.expr(f, c, mode)?;
                let c = self.as_bool(c)?;
                let a = self.expr(f, a, mode)?;
                let b = self.expr(f, b, mode)?;
                self.ternary(c, a, b)
            }
            AstExpr::Call(name, args) => {
                let mut tes = Vec::new();
                for a in args {
                    tes.push(self.expr(f, a, mode)?);
                }
                self.call(name, tes)
            }
        }
    }

    fn read(&mut self, f: &mut Fun, name: &str, mode: Mode) -> BpsResult<TE> {
        let b = f.lookup(name).ok_or_else(|| {
            emit(format!(
                "typed: {} reads {name}, which is not bound",
                self.prog.codecs[f.codec].ir
            ))
        })?;
        let (ty, optional) = self.peek(f, name)?;
        let e = f.binding_expr(b);
        if !optional {
            return Ok(TE::leaf(e, ty));
        }
        match mode {
            Mode::Guard => {
                if let Some(refs) = &mut f.guard_refs
                    && !refs.contains(&e)
                {
                    refs.push(e.clone());
                }
                Ok(TE::leaf(Expr::Get(Box::new(e)), ty))
            }
            Mode::Stmt => {
                let base = match &b {
                    Binding::Local(l) => f.locals[*l].name.clone(),
                    Binding::Field { ty, field, .. } => self.prog.fields(*ty)[*field].name.clone(),
                };
                let local = f.local(base, ty.clone(), false);
                f.emit(Stmt::Unwrap {
                    dst: Bind {
                        local,
                        declare: true,
                        some: false,
                    },
                    src: e,
                    field: name.to_string(),
                });
                f.bind_here(name, Binding::Local(local));
                Ok(TE::leaf(Expr::Local(local), ty))
            }
        }
    }

    fn as_bool(&self, te: TE) -> BpsResult<Expr> {
        match te.ty {
            Ty::Bool => Ok(te.e),
            Ty::Int(t) => Ok(Expr::Binary {
                op: BinOp::Ne,
                lhs: Box::new(retype(te.e, t)),
                rhs: Box::new(Expr::Int { value: 0, ty: t }),
                ty: Ty::Bool,
                wraps: false,
            }),
            other => Err(emit(format!("typed: {other:?} used as a condition"))),
        }
    }

    fn as_i64(&self, te: TE) -> BpsResult<Expr> {
        Ok(self.as_i64_te(te)?.e)
    }

    fn as_i64_te(&self, te: TE) -> BpsResult<TE> {
        match &te.ty {
            Ty::Int(IntTy::I64) => Ok(te),
            Ty::Int(_) if te.lit => Ok(TE {
                e: retype(te.e, IntTy::I64),
                ty: Ty::Int(IntTy::I64),
                ..te
            }),
            Ty::Int(_) => {
                let (lo, hi) = if IntTy::I64.contains(te.lo) && IntTy::I64.contains(te.hi) {
                    (te.lo, te.hi)
                } else {
                    (IntTy::I64.min_value(), IntTy::I64.max_value())
                };
                Ok(TE {
                    e: cast(te.e, Ty::Int(IntTy::I64)),
                    ty: Ty::Int(IntTy::I64),
                    lit: false,
                    lo,
                    hi,
                })
            }
            Ty::Bool => Ok(TE {
                e: cast(te.e, Ty::Int(IntTy::I64)),
                ty: Ty::Int(IntTy::I64),
                lit: false,
                lo: 0,
                hi: 1,
            }),
            Ty::Named(t) if matches!(self.prog.types[*t].kind, TypeKind::Enum { .. }) => {
                let TypeKind::Enum { base, .. } = self.prog.types[*t].kind else {
                    unreachable!()
                };
                Ok(TE {
                    e: cast(te.e, Ty::Int(IntTy::I64)),
                    ty: Ty::Int(IntTy::I64),
                    lit: false,
                    lo: base.min_value(),
                    hi: base.max_value(),
                })
            }
            other => Err(emit(format!("typed: {other:?} used as an integer"))),
        }
    }

    fn binop(&self, op: &str, l: TE, r: TE) -> BpsResult<TE> {
        let op = match op {
            "+" => BinOp::Add,
            "-" => BinOp::Sub,
            "*" => BinOp::Mul,
            "/" => BinOp::Div,
            "%" => BinOp::Mod,
            "<<" => BinOp::Shl,
            ">>" => BinOp::Shr,
            "&" => BinOp::BitAnd,
            "|" => BinOp::BitOr,
            "^" => BinOp::BitXor,
            "==" => BinOp::Eq,
            "!=" => BinOp::Ne,
            "<" => BinOp::Lt,
            "<=" => BinOp::Le,
            ">" => BinOp::Gt,
            ">=" => BinOp::Ge,
            "&&" => BinOp::And,
            "||" => BinOp::Or,
            other => return Err(emit(format!("typed: operator {other} is not lowered"))),
        };
        if op.is_logical() {
            let lhs = self.as_bool(l)?;
            let rhs = self.as_bool(r)?;
            return Ok(TE::leaf(
                Expr::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    ty: Ty::Bool,
                    wraps: false,
                },
                Ty::Bool,
            ));
        }
        if op.is_comparison() {
            return self.compare(op, l, r);
        }
        self.arith(op, l, r)
    }

    fn compare(&self, op: BinOp, l: TE, r: TE) -> BpsResult<TE> {
        let out = |lhs: Expr, rhs: Expr| {
            TE::leaf(
                Expr::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                    ty: Ty::Bool,
                    wraps: false,
                },
                Ty::Bool,
            )
        };
        match (&l.ty, &r.ty) {
            (Ty::Str, Ty::Str) if matches!(op, BinOp::Eq | BinOp::Ne) => Ok(out(l.e, r.e)),
            (Ty::Bool, Ty::Bool) if matches!(op, BinOp::Eq | BinOp::Ne) => Ok(out(l.e, r.e)),
            (Ty::F32, Ty::F32) | (Ty::F64, Ty::F64) => Ok(out(l.e, r.e)),
            _ => {
                if let Some(t) = common(&l, &r) {
                    return Ok(out(retype(l.e, t), retype(r.e, t)));
                }
                let l = self.as_i64(l)?;
                let r = self.as_i64(r)?;
                Ok(out(l, r))
            }
        }
    }

    fn arith(&self, op: BinOp, l: TE, r: TE) -> BpsResult<TE> {
        if matches!(op, BinOp::Div | BinOp::Mod) && !(r.lit && r.lo != 0) {
            return Err(emit("typed: `/` and `%` need a non-zero literal divisor"));
        }
        if matches!(op, BinOp::Shl | BinOp::Shr) && !(r.lit && (0..64).contains(&r.lo)) {
            return Err(emit("typed: a shift amount must be a literal in 0..64"));
        }
        if !matches!(l.ty, Ty::Int(_) | Ty::Bool) || !matches!(r.ty, Ty::Int(_) | Ty::Bool) {
            return Err(emit(format!(
                "typed: arithmetic on {:?} and {:?}",
                l.ty, r.ty
            )));
        }
        // The peephole the i64 domain allows: `x & -1` is `x`.
        if op == BinOp::BitAnd && r.lit && r.lo == -1 {
            return self.as_i64_te(l);
        }
        let exact = op_range(op, (l.lo, l.hi), (r.lo, r.hi));
        if let (Some(t), Some((lo, hi))) = (common(&l, &r), exact)
            && t != IntTy::I128
            && t.contains(lo)
            && t.contains(hi)
        {
            let e = Expr::Binary {
                op,
                lhs: Box::new(retype(l.e, t)),
                rhs: Box::new(retype(r.e, t)),
                ty: Ty::Int(t),
                wraps: false,
            };
            return Ok(TE {
                e,
                ty: Ty::Int(t),
                lit: false,
                lo,
                hi,
            });
        }
        let l = self.as_i64_te(l)?;
        let r = self.as_i64_te(r)?;
        let exact = op_range(op, (l.lo, l.hi), (r.lo, r.hi));
        let (wraps, lo, hi) = match exact {
            Some((lo, hi)) if IntTy::I64.contains(lo) && IntTy::I64.contains(hi) => (false, lo, hi),
            _ => (true, IntTy::I64.min_value(), IntTy::I64.max_value()),
        };
        let e = Expr::Binary {
            op,
            lhs: Box::new(l.e),
            rhs: Box::new(r.e),
            ty: Ty::Int(IntTy::I64),
            wraps,
        };
        Ok(TE {
            e,
            ty: Ty::Int(IntTy::I64),
            lit: false,
            lo,
            hi,
        })
    }

    fn ternary(&self, c: Expr, a: TE, b: TE) -> BpsResult<TE> {
        let mk = |a: Expr, b: Expr, ty: Ty, lo: i128, hi: i128| TE {
            e: Expr::Cond {
                cond: Box::new(c.clone()),
                then: Box::new(a),
                els: Box::new(b),
                ty: ty.clone(),
            },
            ty,
            lit: false,
            lo,
            hi,
        };
        match (&a.ty, &b.ty) {
            (Ty::Bool, Ty::Bool) => Ok(mk(a.e, b.e, Ty::Bool, 0, 1)),
            (Ty::Str, Ty::Str) => Ok(mk(a.e, b.e, Ty::Str, 0, 0)),
            _ => {
                let (lo, hi) = (a.lo.min(b.lo), a.hi.max(b.hi));
                if let Some(t) = common(&a, &b).filter(|_| !(a.lit && b.lit)) {
                    return Ok(mk(retype(a.e, t), retype(b.e, t), Ty::Int(t), lo, hi));
                }
                let a = self.as_i64(a)?;
                let b = self.as_i64(b)?;
                Ok(mk(a, b, Ty::Int(IntTy::I64), lo, hi))
            }
        }
    }

    fn call(&self, name: &str, args: Vec<TE>) -> BpsResult<TE> {
        let i64_ty = Ty::Int(IntTy::I64);
        let n = args.len();
        let want = |k: usize| -> BpsResult<()> {
            if n == k {
                Ok(())
            } else {
                Err(emit(format!("typed: {name} takes {k} arguments, got {n}")))
            }
        };
        let (func, out, lo, hi) = match name {
            "sign_extend" => {
                want(2)?;
                let (lo, hi) = match &args[1] {
                    TE {
                        lit: true,
                        lo: bits,
                        ..
                    } if (1..=64).contains(bits) => {
                        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
                    }
                    _ => (IntTy::I64.min_value(), IntTy::I64.max_value()),
                };
                let mut out = Vec::new();
                for a in args {
                    out.push(self.as_i64(a)?);
                }
                (Func::SignExtend, out, lo, hi)
            }
            "utf16_units" | "utf8_len" => {
                want(1)?;
                if args[0].ty != Ty::Str {
                    return Err(emit(format!("typed: {name} of a non-string")));
                }
                let func = if name == "utf8_len" {
                    Func::Utf8Len
                } else {
                    Func::Utf16Units
                };
                (
                    func,
                    vec![
                        args.into_iter()
                            .next()
                            .map(|a| a.e)
                            .unwrap_or(Expr::Remaining),
                    ],
                    0,
                    IntTy::I64.max_value(),
                )
            }
            "len" => {
                want(1)?;
                let func = match &args[0].ty {
                    Ty::Str => Func::StrLen,
                    Ty::Bytes => Func::BytesLen,
                    Ty::List(_) => Func::ListLen,
                    other => return Err(emit(format!("typed: len of {other:?}"))),
                };
                (
                    func,
                    vec![
                        args.into_iter()
                            .next()
                            .map(|a| a.e)
                            .unwrap_or(Expr::Remaining),
                    ],
                    0,
                    IntTy::I64.max_value(),
                )
            }
            "min" | "max" => {
                want(2)?;
                let (lo, hi) = if name == "min" {
                    (args[0].lo.min(args[1].lo), args[0].hi.min(args[1].hi))
                } else {
                    (args[0].lo.max(args[1].lo), args[0].hi.max(args[1].hi))
                };
                let mut out = Vec::new();
                for a in args {
                    out.push(self.as_i64(a)?);
                }
                (
                    if name == "min" { Func::Min } else { Func::Max },
                    out,
                    lo,
                    hi,
                )
            }
            other => return Err(emit(format!("typed: call {other} is not lowered"))),
        };
        Ok(TE {
            e: Expr::Call { func, args: out },
            ty: i64_ty,
            lit: false,
            lo,
            hi,
        })
    }

    /// `te` as a value of `ty`: same type, a literal that fits, or a cast.
    fn coerce(&self, te: TE, ty: &Ty) -> BpsResult<Expr> {
        if &te.ty == ty {
            return Ok(te.e);
        }
        match (&te.ty, ty) {
            (Ty::Int(_), Ty::Int(t)) if te.lit && t.contains(te.lo) => Ok(retype(te.e, *t)),
            (Ty::Int(_) | Ty::Bool, Ty::Int(_)) => Ok(cast(te.e, ty.clone())),
            (Ty::Int(_), Ty::Bool) => self.as_bool(te),
            (Ty::F32, Ty::F64) | (Ty::F64, Ty::F32) => Ok(cast(te.e, ty.clone())),
            (Ty::Str, Ty::Bytes) => Ok(Expr::StrBytes(Box::new(te.e))),
            (Ty::Int(_), Ty::Named(t))
                if matches!(self.prog.types[*t].kind, TypeKind::Enum { .. }) =>
            {
                Ok(cast(te.e, ty.clone()))
            }
            (Ty::Named(t), Ty::Int(_))
                if matches!(self.prog.types[*t].kind, TypeKind::Enum { .. }) =>
            {
                Ok(cast(te.e, ty.clone()))
            }
            (from, to) => Err(emit(format!("typed: cannot store {from:?} as {to:?}"))),
        }
    }

    // ------------------------------------------------------------------
    // Finish
    // ------------------------------------------------------------------

    fn finish(&mut self) -> BpsResult<()> {
        for codec in &mut self.prog.codecs {
            let n = codec.params.len();
            for body in [&mut codec.decode, &mut codec.encode] {
                if body.params.len() != n {
                    return Err(emit(format!(
                        "typed: {} has {n} parameters but a body binds {}",
                        codec.ir,
                        body.params.len()
                    )));
                }
                prune(body);
            }
            if codec.decode.stmts.is_empty() {
                return Err(emit(format!("typed: {} has no decode body", codec.ir)));
            }
        }
        Ok(())
    }

    /// The encode input is `value`, or `v` when a field or parameter is
    /// already called `value`.
    fn input_name(&self, rec: Option<TypeIdx>, codec: CodecIdx) -> Name {
        let value = Name::new("value");
        let taken = rec.is_some_and(|r| self.prog.fields(r).iter().any(|f| f.name == value))
            || self.prog.codecs[codec]
                .params
                .iter()
                .any(|p| p.name == value);
        if taken { Name::new("v") } else { value }
    }

    fn routes(&mut self) -> BpsResult<()> {
        for route in &self.ir.routes {
            let Some(&packet) = self.codec_idx.get(&route.packet.0) else {
                return Err(emit(format!(
                    "typed: route to {} without a codec",
                    route.packet.0
                )));
            };
            let frame = self
                .codec_idx
                .get(&format!("frame_{}", route.frame))
                .copied();
            self.prog.routes.push(RouteDecl {
                state: route.state.clone(),
                direction: route.direction.clone(),
                id: route.packet_id,
                packet,
                frame,
            });
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Function state
// ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    Local(LocalId),
    Field {
        base: LocalId,
        ty: TypeIdx,
        field: FieldIdx,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Stmt,
    Guard,
}

/// A decode binding in progress: the statement's destination plus what
/// becomes visible afterwards.
struct Bound {
    bind: Bind,
    name: String,
    /// A non-optional local holding the value in this block.
    temp: Option<LocalId>,
    /// The record field's local.
    field: Option<LocalId>,
}

struct Fun {
    codec: CodecIdx,
    rec: Option<TypeIdx>,
    locals: Vec<Local>,
    names: Taken,
    params: Vec<LocalId>,
    value: Option<LocalId>,
    scopes: Vec<HashMap<String, Binding>>,
    sinks: Vec<Vec<Stmt>>,
    /// Remaining sibling statements at each open block, for "read later".
    rest: Vec<Vec<IrStmt>>,
    field_locals: HashMap<String, LocalId>,
    prebound: HashSet<String>,
    hoisted: HashMap<String, LocalId>,
    decl: HashMap<String, Ty>,
    guard_refs: Option<Vec<Expr>>,
}

/// A statement waiting for its destination.
type StmtFn = Box<dyn FnOnce(Bind) -> Stmt>;

impl Fun {
    fn new(codec: CodecIdx, rec: Option<TypeIdx>) -> Fun {
        Fun {
            codec,
            rec,
            locals: Vec::new(),
            names: Taken::default(),
            params: Vec::new(),
            value: None,
            scopes: vec![HashMap::new()],
            sinks: vec![Vec::new()],
            rest: Vec::new(),
            field_locals: HashMap::new(),
            prebound: HashSet::new(),
            hoisted: HashMap::new(),
            decl: HashMap::new(),
            guard_refs: None,
        }
    }

    fn local(&mut self, name: Name, ty: Ty, optional: bool) -> LocalId {
        let name = self.names.claim(name);
        self.locals.push(Local {
            name,
            ty,
            optional,
            uses: 0,
        });
        self.locals.len() - 1
    }

    fn emit(&mut self, stmt: Stmt) {
        self.sink().push(stmt);
    }

    fn sink(&mut self) -> &mut Vec<Stmt> {
        let n = self.sinks.len();
        &mut self.sinks[n - 1]
    }

    fn push_block(&mut self) {
        self.scopes.push(HashMap::new());
        self.sinks.push(Vec::new());
    }

    fn pop_block(&mut self) -> Vec<Stmt> {
        self.scopes.pop();
        self.sinks.pop().unwrap_or_default()
    }

    fn bind_root(&mut self, name: &str, b: Binding) {
        self.scopes[0].insert(name.to_string(), b);
    }

    fn bind_here(&mut self, name: &str, b: Binding) {
        let n = self.scopes.len();
        self.scopes[n - 1].insert(name.to_string(), b);
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes.iter().rev().find_map(|s| s.get(name).copied())
    }

    fn binding_expr(&self, b: Binding) -> Expr {
        match b {
            Binding::Local(l) => Expr::Local(l),
            Binding::Field { base, ty, field } => Expr::Field {
                base: Box::new(Expr::Local(base)),
                ty,
                field,
            },
        }
    }

    /// Is `name` read by a later statement of the innermost open block?
    fn read_later(&self, name: &str) -> bool {
        self.rest.last().is_some_and(|rest| reads(rest, name))
    }

    fn finish(mut self) -> Body {
        let stmts = self.sinks.swap_remove(0);
        Body {
            params: self.params,
            value: self.value,
            locals: self.locals,
            stmts,
        }
    }
}

/// A typed expression with its value range (integers) and whether it is a
/// bare literal (which adopts a sibling's type).
#[derive(Clone, Debug)]
struct TE {
    e: Expr,
    ty: Ty,
    lit: bool,
    lo: i128,
    hi: i128,
}

impl TE {
    fn leaf(e: Expr, ty: Ty) -> TE {
        let (lo, hi) = match &ty {
            Ty::Int(t) => (t.min_value(), t.max_value()),
            Ty::Bool => (0, 1),
            _ => (0, 0),
        };
        TE {
            e,
            ty,
            lit: false,
            lo,
            hi,
        }
    }
}

/// Unique names under every casing.
#[derive(Default)]
struct Taken {
    seen: HashSet<String>,
}

impl Taken {
    fn claim(&mut self, name: Name) -> Name {
        let mut candidate = name.clone();
        let mut n = 2;
        loop {
            let forms = [
                candidate.snake(),
                candidate.camel(),
                candidate.pascal().to_ascii_lowercase(),
            ];
            if forms.iter().all(|f| !self.seen.contains(f)) {
                for f in forms {
                    self.seen.insert(f);
                }
                return candidate;
            }
            candidate = name.with(&n.to_string());
            n += 1;
        }
    }
}

// ----------------------------------------------------------------------
// Helpers over IR statements and expressions
// ----------------------------------------------------------------------

fn get_proc<'b>(
    map: &'b crate::ordmap::OrderedMap<crate::ir::Proc>,
    key: &str,
) -> BpsResult<&'b crate::ir::Proc> {
    map.get(key)
        .ok_or_else(|| emit(format!("typed: no procedure {key}")))
}

fn get_type<'b>(ir: &'b IrSchema, key: &str) -> BpsResult<&'b TypeDef> {
    ir.types
        .get(key)
        .ok_or_else(|| emit(format!("typed: no type {key}")))
}

fn empty_body() -> Body {
    Body {
        params: Vec::new(),
        value: None,
        locals: Vec::new(),
        stmts: Vec::new(),
    }
}

fn storage_ty(s: Storage) -> Ty {
    match s {
        Storage::Bool => Ty::Bool,
        Storage::F32 => Ty::F32,
        Storage::F64 => Ty::F64,
        Storage::Str => Ty::Str,
        Storage::Bytes => Ty::Bytes,
        other => Ty::Int(int_ty(other).unwrap_or(IntTy::I128)),
    }
}

fn int_ty(s: Storage) -> Option<IntTy> {
    Some(match s {
        Storage::I8 => IntTy::I8,
        Storage::I16 => IntTy::I16,
        Storage::I32 => IntTy::I32,
        Storage::I64 => IntTy::I64,
        Storage::I128 => IntTy::I128,
        Storage::U8 => IntTy::U8,
        Storage::U16 => IntTy::U16,
        Storage::U32 => IntTy::U32,
        Storage::U64 => IntTy::U64,
        _ => return None,
    })
}

fn read_ty(op: &WireOp) -> Ty {
    match op {
        WireOp::U8 => Ty::Int(IntTy::U8),
        WireOp::F32be => Ty::F32,
        WireOp::F64be => Ty::F64,
        WireOp::Bytes { .. } | WireOp::Rest => Ty::Bytes,
    }
}

fn read_op_ty(op: &ReadOp) -> Ty {
    match op {
        ReadOp::U8 => Ty::Int(IntTy::U8),
        ReadOp::F32 => Ty::F32,
        ReadOp::F64 => Ty::F64,
        ReadOp::Bytes(_) | ReadOp::Rest => Ty::Bytes,
    }
}

fn check_ty(got: &Ty, want: &Ty, what: &str) -> BpsResult<()> {
    if got == want {
        Ok(())
    } else {
        Err(emit(format!(
            "typed: {what} decodes {got:?}, its type is {want:?}"
        )))
    }
}

fn variant_name(tag: Option<i128>) -> Name {
    match tag {
        None => Name::new("other"),
        Some(t) if t < 0 => Name::new("tag").with(&format!("neg{}", t.unsigned_abs())),
        Some(t) => Name::new(&format!("tag{t}")),
    }
}

fn ir_named(ty: &IrTy, out: &mut Vec<String>) {
    match ty {
        IrTy::Named(id) => out.push(id.0.clone()),
        IrTy::List(inner) | IrTy::Optional(inner) => ir_named(inner, out),
        IrTy::Scalar(_) => {}
    }
}

fn i64_literal(v: i128) -> BpsResult<i64> {
    if let Ok(x) = i64::try_from(v) {
        return Ok(x);
    }
    if let Ok(x) = u64::try_from(v) {
        return Ok(x as i64);
    }
    Err(emit(format!("typed: literal {v} does not fit 64 bits")))
}

fn retype(e: Expr, t: IntTy) -> Expr {
    match e {
        Expr::Int { value, .. } => Expr::Int { value, ty: t },
        other => other,
    }
}

fn cast(e: Expr, to: Ty) -> Expr {
    Expr::Cast {
        expr: Box::new(e),
        to,
    }
}

fn not(e: Expr) -> Expr {
    match e {
        Expr::Not(inner) => *inner,
        other => Expr::Not(Box::new(other)),
    }
}

fn and(a: Expr, b: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::And,
        lhs: Box::new(a),
        rhs: Box::new(b),
        ty: Ty::Bool,
        wraps: false,
    }
}

fn or(a: Expr, b: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Or,
        lhs: Box::new(a),
        rhs: Box::new(b),
        ty: Ty::Bool,
        wraps: false,
    }
}

fn gt0(e: Expr) -> Expr {
    Expr::Binary {
        op: BinOp::Gt,
        lhs: Box::new(e),
        rhs: Box::new(Expr::Int {
            value: 0,
            ty: IntTy::I64,
        }),
        ty: Ty::Bool,
        wraps: false,
    }
}

fn all_present(refs: &[Expr]) -> Option<Expr> {
    refs.iter()
        .cloned()
        .map(|r| Expr::Present(Box::new(r)))
        .reduce(and)
}

/// The integer type both operands can share without a cast: one type, or a
/// literal that fits the other's.
fn common(l: &TE, r: &TE) -> Option<IntTy> {
    match (&l.ty, &r.ty) {
        (Ty::Int(a), Ty::Int(b)) => match (l.lit, r.lit) {
            (true, true) => None,
            (true, false) => b.contains(l.lo).then_some(*b),
            (false, true) => a.contains(r.lo).then_some(*a),
            (false, false) => (a == b).then_some(*a),
        },
        _ => None,
    }
}

/// Exact `[lo, hi]` of `op` over operand ranges, `None` when unbounded.
pub(super) fn op_range(
    op: BinOp,
    (alo, ahi): (i128, i128),
    (blo, bhi): (i128, i128),
) -> Option<(i128, i128)> {
    let span = |vals: [Option<i128>; 4]| -> Option<(i128, i128)> {
        let mut lo = i128::MAX;
        let mut hi = i128::MIN;
        for v in vals {
            let v = v?;
            lo = lo.min(v);
            hi = hi.max(v);
        }
        Some((lo, hi))
    };
    match op {
        BinOp::Add => Some((alo.checked_add(blo)?, ahi.checked_add(bhi)?)),
        BinOp::Sub => Some((alo.checked_sub(bhi)?, ahi.checked_sub(blo)?)),
        BinOp::Mul => span([
            alo.checked_mul(blo),
            alo.checked_mul(bhi),
            ahi.checked_mul(blo),
            ahi.checked_mul(bhi),
        ]),
        BinOp::Div => {
            if blo != bhi || blo == 0 {
                return None;
            }
            let d = blo;
            let (x, y) = (floor_div(alo, d), floor_div(ahi, d));
            Some((x.min(y), x.max(y)))
        }
        BinOp::Mod => {
            if blo != bhi || blo == 0 {
                return None;
            }
            let d = blo;
            if d > 0 {
                if alo >= 0 && ahi < d {
                    Some((alo, ahi))
                } else {
                    Some((0, d - 1))
                }
            } else {
                Some((d + 1, 0))
            }
        }
        BinOp::Shl => {
            if blo != bhi || !(0..64).contains(&blo) {
                return None;
            }
            let k = blo as u32;
            let lo = alo.checked_mul(1i128 << k)?;
            let hi = ahi.checked_mul(1i128 << k)?;
            Some((lo, hi))
        }
        BinOp::Shr => {
            if blo != bhi || !(0..64).contains(&blo) {
                return None;
            }
            let k = blo as u32;
            Some((alo >> k, ahi >> k))
        }
        BinOp::BitAnd => {
            if alo >= 0 && blo >= 0 {
                Some((0, ahi.min(bhi)))
            } else if blo >= 0 {
                Some((0, bhi))
            } else if alo >= 0 {
                Some((0, ahi))
            } else {
                Some((alo.min(blo), ahi.max(bhi).max(0)))
            }
        }
        BinOp::BitOr | BinOp::BitXor => {
            if alo >= 0 && blo >= 0 {
                let lo = if op == BinOp::BitOr { alo.max(blo) } else { 0 };
                Some((lo, bit_fill(ahi.max(bhi))))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn floor_div(a: i128, b: i128) -> i128 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

fn bit_fill(v: i128) -> i128 {
    if v <= 0 {
        return 0;
    }
    let bits = 128 - (v as u128).leading_zeros();
    if bits >= 127 {
        i128::MAX
    } else {
        (1i128 << bits) - 1
    }
}

/// `[lo, hi]` of an integer expression in its own type (after wrapping).
pub(super) fn int_range(prog: &Program, body: &Body, e: &Expr) -> Option<(i128, i128)> {
    let full = |ty: &Ty| match ty {
        Ty::Int(t) => Some((t.min_value(), t.max_value())),
        Ty::Bool => Some((0, 1)),
        _ => None,
    };
    match e {
        Expr::Int { value, .. } => Some((i128::from(*value), i128::from(*value))),
        Expr::Bool(_) | Expr::Present(_) | Expr::Not(_) => Some((0, 1)),
        Expr::Local(_) | Expr::Field { .. } | Expr::Get(_) => full(&prog.expr_ty(body, e).0),
        Expr::Binary {
            op,
            lhs,
            rhs,
            ty,
            wraps,
        } => {
            if op.is_comparison() || op.is_logical() {
                return Some((0, 1));
            }
            if *wraps {
                return full(ty);
            }
            let l = int_range(prog, body, lhs)?;
            let r = int_range(prog, body, rhs)?;
            op_range(*op, l, r).or_else(|| full(ty))
        }
        Expr::Cond { then, els, ty, .. } => {
            match (int_range(prog, body, then), int_range(prog, body, els)) {
                (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
                _ => full(ty),
            }
        }
        Expr::Cast { expr, to } => {
            let target = full(to)?;
            match int_range(prog, body, expr) {
                Some((lo, hi)) if target.0 <= lo && hi <= target.1 => Some((lo, hi)),
                _ => Some(target),
            }
        }
        Expr::Call { func, args } => match func {
            Func::SignExtend => match args.get(1) {
                Some(Expr::Int { value, .. }) if (1..=64).contains(value) => {
                    Some((-(1i128 << (value - 1)), (1i128 << (value - 1)) - 1))
                }
                _ => full(&Ty::Int(IntTy::I64)),
            },
            Func::Min | Func::Max => {
                let a = int_range(prog, body, &args[0])?;
                let b = int_range(prog, body, &args[1])?;
                Some(if *func == Func::Min {
                    (a.0.min(b.0), a.1.min(b.1))
                } else {
                    (a.0.max(b.0), a.1.max(b.1))
                })
            }
            _ => Some((0, IntTy::I64.max_value())),
        },
        Expr::Remaining => Some((0, IntTy::I64.max_value())),
        Expr::Str(_) | Expr::StrBytes(_) | Expr::Construct { .. } | Expr::Variant { .. } => None,
    }
}

/// Names a statement list binds anywhere (scalar prebinding).
fn prebind_names(stmts: &[IrStmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            IrStmt::Read { dst, .. }
            | IrStmt::CallDecode { dst, .. }
            | IrStmt::Let { name: dst, .. } => {
                if !out.contains(dst) {
                    out.push(dst.clone());
                }
            }
            IrStmt::If {
                then_body,
                else_body,
                ..
            } => {
                prebind_names(then_body, out);
                prebind_names(else_body, out);
            }
            IrStmt::Match { arms, default, .. } => {
                for a in arms {
                    prebind_names(&a.body, out);
                }
                if let Some(d) = default {
                    prebind_names(d, out);
                }
            }
            _ => {}
        }
    }
}

/// Every name bound anywhere, nested scopes included.
fn bound_names(stmts: &[IrStmt], out: &mut HashSet<String>) {
    for stmt in stmts {
        match stmt {
            IrStmt::Let { name, .. } => {
                out.insert(name.clone());
            }
            IrStmt::Read { dst, .. }
            | IrStmt::CallDecode { dst, .. }
            | IrStmt::Utf8 { dst, .. } => {
                out.insert(dst.clone());
            }
            IrStmt::If {
                then_body,
                else_body,
                ..
            } => {
                bound_names(then_body, out);
                bound_names(else_body, out);
            }
            IrStmt::Match { arms, default, .. } => {
                for a in arms {
                    bound_names(&a.body, out);
                }
                if let Some(d) = default {
                    bound_names(d, out);
                }
            }
            IrStmt::RepeatCount { elem, dst, .. }
            | IrStmt::RepeatUntil { elem, dst, .. }
            | IrStmt::RepeatUntilEnd { elem, dst } => {
                out.insert(dst.clone());
                out.insert(ELEM.into());
                bound_names(elem, out);
            }
            IrStmt::Block { body, dst } => {
                out.insert(dst.clone());
                bound_names(body, out);
            }
            IrStmt::Require(_) | IrStmt::Write { .. } | IrStmt::CallEncode { .. } => {}
        }
    }
}

/// Every name read anywhere.
fn read_names(stmts: &[IrStmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            IrStmt::Let { expr, .. } | IrStmt::Require(expr) => expr_names(expr, out),
            IrStmt::If {
                cond,
                then_body,
                else_body,
                ..
            } => {
                expr_names(cond, out);
                read_names(then_body, out);
                read_names(else_body, out);
            }
            IrStmt::Match {
                scrutinee,
                arms,
                default,
            } => {
                expr_names(scrutinee, out);
                for a in arms {
                    read_names(&a.body, out);
                }
                if let Some(d) = default {
                    read_names(d, out);
                }
            }
            IrStmt::RepeatCount { n: e, elem, .. } | IrStmt::RepeatUntil { term: e, elem, .. } => {
                expr_names(e, out);
                read_names(elem, out);
            }
            IrStmt::RepeatUntilEnd { elem, .. } => read_names(elem, out),
            IrStmt::Block { body, .. } => read_names(body, out),
            IrStmt::Read { op, .. } | IrStmt::Write { op, .. } => {
                if let WireOp::Bytes { len } = op {
                    expr_names(len, out);
                }
                if let IrStmt::Write { value, .. } = stmt {
                    expr_names(value, out);
                }
            }
            IrStmt::CallDecode { args, .. } => args.iter().for_each(|a| expr_names(a, out)),
            IrStmt::CallEncode { args, value, .. } => {
                args.iter().for_each(|a| expr_names(a, out));
                expr_names(value, out);
            }
            IrStmt::Utf8 { src, .. } => out.push(src.clone()),
        }
    }
}

fn expr_names(e: &AstExpr, out: &mut Vec<String>) {
    match e {
        AstExpr::Ident(n) => out.push(n.clone()),
        AstExpr::Path(p) => out.push(p.split('.').next().unwrap_or_default().to_string()),
        AstExpr::Unop(_, a) => expr_names(a, out),
        AstExpr::Binop(_, a, b) => {
            expr_names(a, out);
            expr_names(b, out);
        }
        AstExpr::Ternary(a, b, c) => {
            expr_names(a, out);
            expr_names(b, out);
            expr_names(c, out);
        }
        AstExpr::Call(_, args) => args.iter().for_each(|a| expr_names(a, out)),
        AstExpr::Int(_) | AstExpr::Bool(_) | AstExpr::Str(_) => {}
    }
}

/// Does any of `stmts` read `name` (in any nested statement)?
fn reads(stmts: &[IrStmt], name: &str) -> bool {
    let mut names = Vec::new();
    read_names(stmts, &mut names);
    names.iter().any(|n| n == name)
}

fn refs(e: &AstExpr, bound: &HashSet<String>, free: &mut Vec<String>) {
    let mut names = Vec::new();
    expr_names(e, &mut names);
    for n in names {
        if !bound.contains(&n) && !free.contains(&n) {
            free.push(n);
        }
    }
}

fn pass_up(inner: &[String], bound: &HashSet<String>, free: &mut Vec<String>) {
    for n in inner {
        if !bound.contains(n) && !free.contains(n) {
            free.push(n.clone());
        }
    }
}

/// Names bound by a decode list's own top-level statements, and those bound
/// inside its `if`s and `match` arms.
fn top_and_nested(stmts: &[IrStmt], top: &mut Vec<String>, nested: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            IrStmt::Let { name: dst, .. }
            | IrStmt::Read { dst, .. }
            | IrStmt::CallDecode { dst, .. }
            | IrStmt::Utf8 { dst, .. }
            | IrStmt::Block { dst, .. }
            | IrStmt::RepeatCount { dst, .. }
            | IrStmt::RepeatUntil { dst, .. }
            | IrStmt::RepeatUntilEnd { dst, .. } => top.push(dst.clone()),
            IrStmt::If {
                then_body,
                else_body,
                ..
            } => {
                let mut ignore = Vec::new();
                top_and_nested(then_body, nested, &mut ignore);
                top_and_nested(else_body, nested, &mut ignore);
                nested.extend(ignore);
            }
            IrStmt::Match { arms, default, .. } => {
                if let Some(dst) = typed_match(stmt, true) {
                    top.push(dst);
                    continue;
                }
                let mut ignore = Vec::new();
                for a in arms {
                    top_and_nested(&a.body, nested, &mut ignore);
                }
                if let Some(d) = default {
                    top_and_nested(d, nested, &mut ignore);
                }
                nested.extend(ignore);
            }
            IrStmt::Require(_) | IrStmt::Write { .. } | IrStmt::CallEncode { .. } => {}
        }
    }
}

/// Non-identity `let`s (computed or `= expr` values) anywhere in `stmts`.
fn computed_lets(stmts: &[IrStmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            IrStmt::Let { name, expr } if !matches!(expr, AstExpr::Ident(n) if n == name) => {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            }
            IrStmt::If {
                then_body,
                else_body,
                ..
            } => {
                computed_lets(then_body, out);
                computed_lets(else_body, out);
            }
            IrStmt::Match { arms, default, .. } => {
                for a in arms {
                    computed_lets(&a.body, out);
                }
                if let Some(d) = default {
                    computed_lets(d, out);
                }
            }
            _ => {}
        }
    }
}

/// Names an encode group binds: `let` targets, lists, blocks, sums.
fn enc_bound_names(stmts: &[IrStmt], out: &mut Vec<String>) {
    for stmt in stmts {
        match stmt {
            IrStmt::Let { name: n, .. }
            | IrStmt::Block { dst: n, .. }
            | IrStmt::RepeatCount { dst: n, .. }
            | IrStmt::RepeatUntil { dst: n, .. }
            | IrStmt::RepeatUntilEnd { dst: n, .. } => {
                if !out.contains(n) {
                    out.push(n.clone());
                }
            }
            IrStmt::If {
                then_body,
                else_body,
                ..
            } => {
                enc_bound_names(then_body, out);
                enc_bound_names(else_body, out);
            }
            IrStmt::Match { arms, default, .. } => {
                if let Some(dst) = typed_match(stmt, false) {
                    if !out.contains(&dst) {
                        out.push(dst);
                    }
                    continue;
                }
                for a in arms {
                    enc_bound_names(&a.body, out);
                }
                if let Some(d) = default {
                    enc_bound_names(d, out);
                }
            }
            _ => {}
        }
    }
}

/// The field a `match` decodes or encodes as a sum, when every arm has the
/// shape of a typed arm (`decode_field` / `encode_element` of one name).
fn typed_match(stmt: &IrStmt, decode: bool) -> Option<String> {
    let IrStmt::Match { arms, default, .. } = stmt else {
        return None;
    };
    let mut dst: Option<String> = None;
    let bodies = arms
        .iter()
        .map(|a| a.body.as_slice())
        .chain(default.as_deref());
    for body in bodies {
        let d = if decode {
            match body {
                [] => continue,
                [IrStmt::CallDecode { dst, .. }] => dst,
                [
                    IrStmt::CallDecode { dst, .. },
                    IrStmt::Utf8 { dst: d2, src },
                ] if d2 == dst && src == dst => dst,
                [IrStmt::Block { dst, .. }] => dst,
                _ => return None,
            }
        } else {
            match body {
                [] => continue,
                [
                    IrStmt::CallEncode {
                        value: AstExpr::Ident(n),
                        ..
                    },
                ] => n,
                [IrStmt::Block { dst, .. }] => dst,
                _ => return None,
            }
        };
        match &dst {
            None => dst = Some(d.clone()),
            Some(prev) if prev == d => {}
            Some(_) => return None,
        }
    }
    dst
}

/// The schema text of a `require` condition.
fn source(e: &AstExpr) -> String {
    fn go(e: &AstExpr, top: bool) -> String {
        let s = match e {
            AstExpr::Int(v) => return v.to_string(),
            AstExpr::Bool(b) => return b.to_string(),
            AstExpr::Str(s) => return format!("{s:?}"),
            AstExpr::Ident(n) | AstExpr::Path(n) => return n.clone(),
            AstExpr::Unop(op, a) => return format!("{op}{}", go(a, false)),
            AstExpr::Call(name, args) => {
                return format!(
                    "{name}({})",
                    args.iter()
                        .map(|a| go(a, true))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            AstExpr::Binop(op, a, b) => format!("{} {op} {}", go(a, false), go(b, false)),
            AstExpr::Ternary(c, a, b) => {
                format!("{} ? {} : {}", go(c, false), go(a, false), go(b, false))
            }
        };
        if top { s } else { format!("({s})") }
    }
    go(e, true)
}

fn count_uses(body: &mut Body) {
    let mut uses = vec![0usize; body.locals.len()];
    fn expr(e: &Expr, uses: &mut [usize]) {
        match e {
            Expr::Local(l) => uses[*l] += 1,
            Expr::Field { base, .. }
            | Expr::Present(base)
            | Expr::Get(base)
            | Expr::Not(base)
            | Expr::StrBytes(base) => expr(base, uses),
            Expr::Cast { expr: inner, .. } => expr(inner, uses),
            Expr::Binary { lhs, rhs, .. } => {
                expr(lhs, uses);
                expr(rhs, uses);
            }
            Expr::Cond {
                cond, then, els, ..
            } => {
                expr(cond, uses);
                expr(then, uses);
                expr(els, uses);
            }
            Expr::Call { args, .. } | Expr::Construct { fields: args, .. } => {
                args.iter().for_each(|a| expr(a, uses))
            }
            Expr::Variant { payload, .. } => {
                if let Some(p) = payload {
                    expr(p, uses);
                }
            }
            Expr::Int { .. } | Expr::Bool(_) | Expr::Str(_) | Expr::Remaining => {}
        }
    }
    fn stmts(list: &[Stmt], uses: &mut [usize]) {
        for s in list {
            match s {
                Stmt::Declare { .. } | Stmt::Fail(_) => {}
                Stmt::Let { value, .. } => expr(value, uses),
                Stmt::Read { op, .. } => {
                    if let ReadOp::Bytes(n) = op {
                        expr(n, uses);
                    }
                }
                Stmt::Decode { args, .. } => args.iter().for_each(|a| expr(a, uses)),
                Stmt::Utf8 { src, .. } => expr(src, uses),
                Stmt::Write { op, value } => {
                    if let WriteOp::Bytes(n) = op {
                        expr(n, uses);
                    }
                    expr(value, uses);
                }
                Stmt::Encode { args, value, .. } => {
                    args.iter().for_each(|a| expr(a, uses));
                    expr(value, uses);
                }
                Stmt::Require { cond, .. } => expr(cond, uses),
                Stmt::Unwrap { src, .. } => expr(src, uses),
                Stmt::If { cond, then, els } => {
                    expr(cond, uses);
                    stmts(then, uses);
                    stmts(els, uses);
                }
                Stmt::Match {
                    scrutinee,
                    arms,
                    other,
                } => {
                    expr(scrutinee, uses);
                    for a in arms {
                        stmts(&a.body, uses);
                    }
                    if let Some(o) = other {
                        stmts(o, uses);
                    }
                }
                Stmt::Repeat {
                    mode, item, body, ..
                } => {
                    if let RepeatMode::Count(e) | RepeatMode::Until(e) = mode {
                        expr(e, uses);
                    }
                    // Appending the item reads it.
                    uses[*item] += 1;
                    stmts(body, uses);
                }
                Stmt::ForEach { list, body, .. } => {
                    expr(list, uses);
                    stmts(body, uses);
                }
                Stmt::ExpectVariant { sum, .. } => expr(sum, uses),
                Stmt::Return(e) => expr(e, uses),
            }
        }
    }
    stmts(&body.stmts, &mut uses);
    for (local, n) in body.locals.iter_mut().zip(uses) {
        local.uses = n;
    }
}

/// Drops `let`s and declarations of locals nothing reads (a computed field
/// recomputed on encode and never written); expressions cannot fail, so
/// dropping them changes nothing.
fn prune(body: &mut Body) {
    loop {
        count_uses(body);
        let keep: Vec<bool> = (0..body.locals.len())
            .map(|l| body.locals[l].uses > 0 || body.params.contains(&l) || body.value == Some(l))
            .collect();
        fn sweep(list: &mut Vec<Stmt>, keep: &[bool]) -> bool {
            let before = list.len();
            list.retain(|s| match s {
                Stmt::Let { dst, .. } => keep[dst.local],
                Stmt::Declare { local, .. } => keep[*local],
                _ => true,
            });
            let mut changed = list.len() != before;
            for s in list.iter_mut() {
                match s {
                    Stmt::If { then, els, .. } => {
                        changed |= sweep(then, keep);
                        changed |= sweep(els, keep);
                    }
                    Stmt::Match { arms, other, .. } => {
                        for a in arms.iter_mut() {
                            changed |= sweep(&mut a.body, keep);
                        }
                        if let Some(o) = other {
                            changed |= sweep(o, keep);
                        }
                    }
                    Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => {
                        changed |= sweep(body, keep)
                    }
                    _ => {}
                }
            }
            changed
        }
        if !sweep(&mut body.stmts, &keep) {
            return;
        }
    }
}
