//! `ResolvedSchema` → [`IrSchema`].
//!
//! Scalar encodings become a call to each field's type. Bodies are not inlined.

use std::collections::HashMap;

use super::schema::{
    Field, IrArm, IrSchema, Proc, Route, Stmt, Ty, TypeDef, TypeId, Variant, WireOp,
};
use crate::ast::{
    BlockItem, EncBody, EncodingDecl, Expr, FieldType, MatchArm as AstArm, RepeatMode,
    ResolvedSchema, Tag, TypeRef, WireField,
};
use crate::errors::{BmlResult, emit};
use crate::ordmap::OrderedMap;
use crate::primitives::is_bare_primitive;

use super::width::{Storage, storage_of};

const BUILTINS: &[&str] = &["u8", "f32be", "f64be", "bytes", "rest"];

pub fn lower(resolved: &ResolvedSchema) -> BmlResult<IrSchema> {
    let mut ir = IrSchema {
        types: OrderedMap::new(),
        decode: OrderedMap::new(),
        encode: OrderedMap::new(),
        routes: Vec::new(),
        frames: Vec::new(),
    };
    install_builtins(&mut ir);
    for enc in resolved.encodings.values() {
        if let Some(yield_expr) = &enc.yield_expr {
            let locals = u8_locals(&enc.body);
            let class = storage_of(yield_expr, &locals);
            ir.types.insert(&enc.name, TypeDef::Scalar(class));
        }
    }
    for enc in resolved.encodings.values() {
        if enc.yield_expr.is_some() {
            lower_scalar(enc, resolved, &mut ir)?;
        }
    }
    lower_strings(resolved, &mut ir)?;
    lower_enums(resolved, &mut ir)?;
    lower_records(resolved, &mut ir)?;
    for pkt in resolved.packets.values() {
        let key = if ir.decode.contains_key(&pkt.name) {
            format!("packet:{}", pkt.name)
        } else {
            pkt.name.clone()
        };
        install_record(&key, &[], &pkt.items, resolved, &mut ir)?;
    }
    lower_frames(resolved, &mut ir)?;
    Ok(ir)
}

fn install_builtins(ir: &mut IrSchema) {
    let scalar = |class| TypeDef::Scalar(class);
    ir.types.insert("u8", scalar(Storage::U8));
    ir.types.insert("f32be", scalar(Storage::F32));
    ir.types.insert("f64be", scalar(Storage::F64));
    ir.types.insert("bytes", TypeDef::Scalar(Storage::Bytes));
    ir.types.insert("rest", TypeDef::Scalar(Storage::Bytes));

    ir.decode.insert("u8", read_builtin(WireOp::U8, &[]));
    ir.encode
        .insert("u8", write_builtin(WireOp::U8, mask_low_byte(), &[]));

    ir.decode.insert("f32be", read_builtin(WireOp::F32be, &[]));
    ir.encode.insert(
        "f32be",
        write_builtin(WireOp::F32be, Expr::Ident("value".into()), &[]),
    );
    ir.decode.insert("f64be", read_builtin(WireOp::F64be, &[]));
    ir.encode.insert(
        "f64be",
        write_builtin(WireOp::F64be, Expr::Ident("value".into()), &[]),
    );

    let n = Expr::Ident("n".into());
    ir.decode.insert(
        "bytes",
        read_builtin(WireOp::Bytes { len: n.clone() }, &["n"]),
    );
    ir.encode.insert(
        "bytes",
        write_builtin(
            WireOp::Bytes { len: n },
            Expr::Ident("value".into()),
            &["n"],
        ),
    );
    ir.decode.insert("rest", read_builtin(WireOp::Rest, &[]));
    ir.encode.insert(
        "rest",
        write_builtin(WireOp::Rest, Expr::Ident("value".into()), &[]),
    );
}

fn read_builtin(op: WireOp, params: &[&str]) -> Proc {
    Proc {
        params: params.iter().map(|s| (*s).to_string()).collect(),
        body: vec![Stmt::Read {
            dst: "value".into(),
            op,
        }],
        result: Some(Expr::Ident("value".into())),
    }
}

fn write_builtin(op: WireOp, value: Expr, params: &[&str]) -> Proc {
    Proc {
        params: params.iter().map(|s| (*s).to_string()).collect(),
        body: vec![Stmt::Write { op, value }],
        result: None,
    }
}

fn mask_low_byte() -> Expr {
    Expr::Binop(
        "&".into(),
        Box::new(Expr::Ident("value".into())),
        Box::new(Expr::Int(0xFF)),
    )
}

fn u8_locals(body: &EncBody) -> HashMap<String, Storage> {
    let mut locals = HashMap::new();
    let EncBody::Block(block) = body else {
        return locals;
    };
    for item in &block.items {
        let BlockItem::Field(wf) = item else {
            continue;
        };
        let FieldType::Ref(r) = &wf.type_ref else {
            continue;
        };
        if r.name == "u8" {
            locals.insert(wf.name.clone(), Storage::U8);
        }
    }
    locals
}

fn lower_scalar(enc: &EncodingDecl, resolved: &ResolvedSchema, ir: &mut IrSchema) -> BmlResult<()> {
    let EncBody::Block(block) = &enc.body else {
        return Err(emit(format!(
            "encoding {}: a scalar alias is not lowered",
            enc.name
        )));
    };
    let params: Vec<String> = enc.params.iter().map(|p| p.name.clone()).collect();
    let mut decode_cx = RecCx::with_params(&params);
    let decode_body = lower_items(&block.items, true, resolved, &enc.name, ir, &mut decode_cx)?;
    if let Some(yield_expr) = &enc.yield_expr {
        decode_cx.check(yield_expr, &enc.name)?;
    }
    let mut encode_cx = RecCx::with_params(&params);
    let encode_body = lower_items(&block.items, false, resolved, &enc.name, ir, &mut encode_cx)?;
    ir.decode.insert(
        &enc.name,
        Proc {
            params: params.clone(),
            body: decode_body,
            result: enc.yield_expr.clone(),
        },
    );
    ir.encode.insert(
        &enc.name,
        Proc {
            params,
            body: encode_body,
            result: None,
        },
    );
    Ok(())
}

fn install_record(
    name: &str,
    params: &[String],
    items: &[BlockItem],
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
) -> BmlResult<()> {
    let items = disambiguate_nested(&expand_spreads(items, resolved)?);
    // The collect walk and both procedure walks visit bindings in the same
    // order, so each fresh renamer assigns identical final names.
    let mut collecter = Renamer::default();
    let fields = collect_fields(name, &items, resolved, ir, &mut collecter)?;
    ir.types.insert(name, TypeDef::Record { fields });
    let decode_body = lower_items(
        &items,
        true,
        resolved,
        name,
        ir,
        &mut RecCx::with_params(params),
    )?;
    ir.decode.insert(
        name,
        Proc {
            params: params.to_vec(),
            body: decode_body,
            result: None,
        },
    );
    let encode_body = lower_items(
        &items,
        false,
        resolved,
        name,
        ir,
        &mut RecCx::with_params(params),
    )?;
    ir.encode.insert(
        name,
        Proc {
            params: params.to_vec(),
            body: encode_body,
            result: None,
        },
    );
    Ok(())
}

/// Gives a block or `match` field the name `<name>_<n>` when an earlier field
/// of its namespace already has its name, so the nested type keys
/// (`<owner>_<name>_block`, `<owner>_<name>`) stay distinct: sibling groups
/// may each declare their own `value: { ... }`. The renamer would rename them
/// anyway; renaming first keeps their types apart.
fn disambiguate_nested(items: &[BlockItem]) -> Vec<BlockItem> {
    disambiguate_in(items, &mut std::collections::HashSet::new())
}

fn disambiguate_in(
    items: &[BlockItem],
    seen: &mut std::collections::HashSet<String>,
) -> Vec<BlockItem> {
    items
        .iter()
        .map(|item| match item {
            BlockItem::Field(wf) => {
                let mut wf = (**wf).clone();
                if matches!(wf.type_ref, FieldType::Block(_) | FieldType::Match(_))
                    && seen.contains(&wf.name)
                {
                    let mut n = 2;
                    while seen.contains(&format!("{}_{n}", wf.name)) {
                        n += 1;
                    }
                    wf.name = format!("{}_{n}", wf.name);
                }
                seen.insert(wf.name.clone());
                wf.type_ref = disambiguate_type(&wf.type_ref);
                BlockItem::Field(Box::new(wf))
            }
            BlockItem::Computed { name, .. } => {
                seen.insert(name.clone());
                item.clone()
            }
            BlockItem::Group { cond, items } => BlockItem::Group {
                cond: cond.clone(),
                items: disambiguate_in(items, seen),
            },
            // Statement-match arms splice into this namespace.
            BlockItem::MatchStmt(mt) => {
                let mut mt = mt.clone();
                for arm in &mut mt.arms {
                    if let Some(FieldType::Block(block)) = &mut arm.type_ref {
                        block.items = disambiguate_in(&block.items, seen);
                    }
                }
                BlockItem::MatchStmt(mt)
            }
            BlockItem::Require(_) | BlockItem::Spread { .. } => item.clone(),
        })
        .collect()
}

/// Nested blocks and arm blocks are namespaces of their own.
fn disambiguate_type(ty: &FieldType) -> FieldType {
    match ty {
        FieldType::Ref(_) => ty.clone(),
        FieldType::Block(block) => {
            let mut block = block.clone();
            block.items = disambiguate_nested(&block.items);
            FieldType::Block(block)
        }
        FieldType::Match(mt) => {
            let mut mt = mt.clone();
            for arm in &mut mt.arms {
                if let Some(t) = &arm.type_ref {
                    arm.type_ref = Some(disambiguate_type(t));
                }
            }
            FieldType::Match(mt)
        }
    }
}

/// Threaded state for lowering one record namespace.
///
/// `renamer` gives every binding its final field name: the first binding of a
/// name keeps it, each later binding with a different type becomes `<name>_2`,
/// `<name>_3`, …. `scopes` is the lexical namespace stack for the
/// renamed-read check; group and match arms share the top scope (their
/// bindings leak into the interpreter's environment), nested blocks and
/// repeat elements push a fresh one (the interpreter clones there).
struct RecCx {
    renamer: Renamer,
    scopes: Vec<RecScope>,
}

impl RecCx {
    fn fresh() -> Self {
        Self {
            renamer: Renamer::default(),
            scopes: vec![RecScope::default()],
        }
    }

    /// Parameters (and the encode `value`) are bound, never renamed.
    fn with_params(params: &[String]) -> Self {
        let mut cx = Self::fresh();
        for param in params {
            cx.bind(param, param);
        }
        cx.bind("value", "value");
        cx
    }

    /// Storage classes of the names bound so far, for the interval rule.
    fn classes(&self, ir: &IrSchema) -> HashMap<String, Storage> {
        self.renamer.classes(ir)
    }

    fn bind(&mut self, name: &str, final_name: &str) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.bound.insert(name.to_string(), final_name.to_string());
        }
    }

    fn check(&self, expr: &Expr, owner: &str) -> BmlResult<()> {
        check_no_renamed_read(expr, &self.scopes, owner)
    }
}

/// First binding of a name keeps it; later bindings with a different type
/// are renamed `<name>_2`, `<name>_3`, …. Same-type rebindings keep the name.
#[derive(Default)]
struct Renamer {
    seen: HashMap<String, Ty>,
}

impl Renamer {
    fn dst_for(&mut self, name: &str, ty: &Ty) -> String {
        match self.seen.get(name) {
            None => {
                self.seen.insert(name.to_string(), ty.clone());
                name.to_string()
            }
            Some(prev) if prev == ty => name.to_string(),
            _ => {
                let mut n = 2u32;
                loop {
                    let cand = format!("{name}_{n}");
                    match self.seen.get(&cand) {
                        None => {
                            self.seen.insert(cand.clone(), ty.clone());
                            return cand;
                        }
                        Some(prev) if prev == ty => return cand,
                        _ => n += 1,
                    }
                }
            }
        }
    }

    fn classes(&self, ir: &IrSchema) -> HashMap<String, Storage> {
        let mut out = HashMap::new();
        for (name, ty) in &self.seen {
            if let Some(class) = ty_storage(ty, ir) {
                out.insert(name.clone(), class);
            }
        }
        out
    }
}

/// One lexical namespace: original names bound so far and their final names.
/// A read of a name whose final differs from the original would mean
/// "whichever binding ran last", so it is a lowering error.
#[derive(Default)]
struct RecScope {
    bound: HashMap<String, String>,
}

fn check_no_renamed_read(expr: &Expr, scopes: &[RecScope], owner: &str) -> BmlResult<()> {
    let mut bad: Option<(String, String)> = None;
    read_idents(expr, &mut |name| {
        if bad.is_some() {
            return;
        }
        for scope in scopes.iter().rev() {
            if let Some(final_name) = scope.bound.get(name) {
                if final_name != name {
                    bad = Some((name.to_string(), final_name.clone()));
                }
                break;
            }
        }
    });
    if let Some((name, final_name)) = bad {
        return Err(emit(format!(
            "{owner}: `{name}` was renamed to `{final_name}`; reading `{name}` would mean whichever binding ran last"
        )));
    }
    Ok(())
}

fn read_idents(expr: &Expr, f: &mut impl FnMut(&str)) {
    match expr {
        Expr::Ident(name) => f(name),
        Expr::Path(dotted) => {
            if let Some(first) = dotted.split('.').next() {
                f(first);
            }
        }
        Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) => {}
        Expr::Unop(_, inner) => read_idents(inner, f),
        Expr::Binop(_, left, right) => {
            read_idents(left, f);
            read_idents(right, f);
        }
        Expr::Ternary(cond, yes, no) => {
            read_idents(cond, f);
            read_idents(yes, f);
            read_idents(no, f);
        }
        Expr::Call(_, args) => {
            for arg in args {
                read_idents(arg, f);
            }
        }
    }
}

/// `decode` selects decode statements. Encode drops `require` and uses each field's `= expr`.
fn lower_items(
    items: &[BlockItem],
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
    ir: &IrSchema,
    cx: &mut RecCx,
) -> BmlResult<Vec<Stmt>> {
    let mut out = Vec::new();
    for item in items {
        match item {
            BlockItem::Require(expr) => {
                if decode {
                    cx.check(expr, owner)?;
                    out.push(Stmt::Require(expr.clone()));
                }
            }
            BlockItem::Computed { name, expr } => {
                cx.check(expr, owner)?;
                let ty = Ty::Scalar(storage_of(expr, &cx.classes(ir)));
                let final_name = cx.renamer.dst_for(name, &ty);
                cx.bind(name, &final_name);
                out.push(Stmt::Let {
                    name: final_name,
                    expr: expr.clone(),
                });
            }
            BlockItem::Field(wf) => {
                if let Some(cond) = &wf.if_expr {
                    cx.check(cond, owner)?;
                }
                let ty = field_ty(owner, wf, ir);
                let final_name = cx.renamer.dst_for(&wf.name, &ty);
                cx.bind(&wf.name, &final_name);
                let inner = if wf.repeat.is_some() {
                    vec![repeat_stmt(
                        wf,
                        &final_name,
                        decode,
                        resolved,
                        owner,
                        ir,
                        cx,
                    )?]
                } else if decode {
                    decode_field(&final_name, &wf.type_ref, resolved, owner, ir, cx)?
                } else {
                    let value = match &wf.encode_expr {
                        Some(e) => {
                            cx.check(e, owner)?;
                            e.clone()
                        }
                        None => Expr::Ident(final_name.clone()),
                    };
                    encode_field(&final_name, &wf.type_ref, value, resolved, owner, ir, cx)?
                };
                if let Some(cond) = &wf.if_expr {
                    out.push(Stmt::If {
                        cond: cond.clone(),
                        then_body: inner,
                        else_body: Vec::new(),
                        fallback: false,
                    });
                } else {
                    out.extend(inner);
                }
            }
            BlockItem::Group { cond, items } => {
                if let Some(cond) = cond {
                    cx.check(cond, owner)?;
                }
                let inner = lower_items(items, decode, resolved, owner, ir, cx)?;
                if let Some(cond) = cond {
                    out.push(Stmt::If {
                        cond: cond.clone(),
                        then_body: inner,
                        else_body: Vec::new(),
                        fallback: true,
                    });
                } else {
                    out.extend(inner);
                }
            }
            BlockItem::MatchStmt(m) => {
                cx.check(&m.tag_expr, owner)?;
                out.push(stmt_match(m, decode, resolved, owner, ir, cx)?);
            }
            BlockItem::Spread { .. } => {
                return Err(emit(format!("{owner}: spread survived expansion")));
            }
        }
    }
    Ok(out)
}

fn decode_field(
    dst: &str,
    ty: &FieldType,
    resolved: &ResolvedSchema,
    owner: &str,
    ir: &IrSchema,
    cx: &mut RecCx,
) -> BmlResult<Vec<Stmt>> {
    match ty {
        FieldType::Ref(r) => ref_decode(dst, r, resolved, owner, cx),
        FieldType::Match(mt) => {
            cx.check(&mt.tag_expr, owner)?;
            Ok(vec![typed_match(dst, mt, true, resolved, owner, ir, cx)?])
        }
        FieldType::Block(block) => {
            // A nested record: its own renamer, but parent scopes stay
            // visible (nested bodies read parent fields).
            cx.scopes.push(RecScope::default());
            let mut parent = Renamer::default();
            std::mem::swap(&mut cx.renamer, &mut parent);
            let body = lower_items(&block.items, true, resolved, owner, ir, cx);
            std::mem::swap(&mut cx.renamer, &mut parent);
            cx.scopes.pop();
            Ok(vec![Stmt::Block {
                body: body?,
                dst: dst.to_string(),
            }])
        }
    }
}

fn ref_decode(
    dst: &str,
    r: &TypeRef,
    resolved: &ResolvedSchema,
    owner: &str,
    cx: &RecCx,
) -> BmlResult<Vec<Stmt>> {
    check_transform(r, owner)?;
    let args = call_args(resolved, r, owner)?;
    for arg in &args {
        cx.check(arg, owner)?;
    }
    let mut stmts = vec![Stmt::CallDecode {
        dst: dst.to_string(),
        callee: TypeId(r.name.clone()),
        args,
    }];
    if r.transforms.iter().any(|t| t == "utf8") {
        stmts.push(Stmt::Utf8 {
            dst: dst.to_string(),
            src: dst.to_string(),
        });
    }
    Ok(stmts)
}

fn encode_field(
    name: &str,
    ty: &FieldType,
    value: Expr,
    resolved: &ResolvedSchema,
    owner: &str,
    ir: &IrSchema,
    cx: &mut RecCx,
) -> BmlResult<Vec<Stmt>> {
    match ty {
        FieldType::Ref(r) => ref_encode(name, r, value, resolved, owner, cx),
        FieldType::Match(mt) => {
            cx.check(&mt.tag_expr, owner)?;
            Ok(vec![typed_match(name, mt, false, resolved, owner, ir, cx)?])
        }
        FieldType::Block(block) => {
            // A nested record: its own renamer, but parent scopes stay
            // visible (nested bodies read parent fields).
            cx.scopes.push(RecScope::default());
            let mut parent = Renamer::default();
            std::mem::swap(&mut cx.renamer, &mut parent);
            let body = lower_items(&block.items, false, resolved, owner, ir, cx);
            std::mem::swap(&mut cx.renamer, &mut parent);
            cx.scopes.pop();
            Ok(vec![Stmt::Block {
                body: body?,
                dst: name.to_string(),
            }])
        }
    }
}

fn ref_encode(
    name: &str,
    r: &TypeRef,
    value: Expr,
    resolved: &ResolvedSchema,
    owner: &str,
    cx: &RecCx,
) -> BmlResult<Vec<Stmt>> {
    check_transform(r, owner)?;
    // `value` was already checked by the caller (explicit `= expr`); the
    // default `Ident(field)` it substitutes needs no check.
    let args = call_args(resolved, r, owner)?;
    for arg in &args {
        cx.check(arg, owner)?;
    }
    Ok(vec![
        Stmt::Let {
            name: name.to_string(),
            expr: value,
        },
        Stmt::CallEncode {
            callee: TypeId(r.name.clone()),
            args,
            value: Expr::Ident(name.to_string()),
        },
    ])
}

fn check_transform(r: &TypeRef, owner: &str) -> BmlResult<()> {
    for t in &r.transforms {
        if t != "utf8" {
            return Err(emit(format!("{owner}: transform {t} is not utf8")));
        }
    }
    Ok(())
}

fn call_args(resolved: &ResolvedSchema, r: &TypeRef, owner: &str) -> BmlResult<Vec<Expr>> {
    if is_builtin(&r.name) || is_bare_primitive(&r.name) || resolved.enums.contains_key(&r.name) {
        return Ok(r.args.clone());
    }
    let Some(enc) = resolved.encodings.get(&r.name) else {
        return Err(emit(format!("{owner}: unknown type {}", r.name)));
    };
    let mut args = r.args.clone();
    for (i, p) in enc.params.iter().enumerate() {
        if i < args.len() {
            continue;
        }
        match p.default {
            Some(v) => args.push(Expr::Int(v)),
            None => {
                return Err(emit(format!(
                    "{owner}: call {} is missing argument {}",
                    r.name, p.name
                )));
            }
        }
    }
    Ok(args)
}

fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

/// Encodings whose wire is a length plus `bytes(length) as utf8`, and spreads of those.
fn lower_strings(resolved: &ResolvedSchema, ir: &mut IrSchema) -> BmlResult<()> {
    let mut progressed = true;
    while progressed {
        progressed = false;
        for enc in resolved.encodings.values() {
            if enc.yield_expr.is_some() || ir.types.contains_key(&enc.name) {
                continue;
            }
            if install_string(enc, resolved, ir)? {
                progressed = true;
            }
        }
    }
    Ok(())
}

fn install_string(
    enc: &EncodingDecl,
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
) -> BmlResult<bool> {
    let EncBody::Block(block) = &enc.body else {
        return Ok(false);
    };
    let params: Vec<String> = enc.params.iter().map(|p| p.name.clone()).collect();
    if let Some(shape) = direct_string(&block.items, ir) {
        let (decode, encode) = string_procs(&shape, &params);
        ir.types.insert(&enc.name, TypeDef::String);
        ir.decode.insert(&enc.name, decode);
        ir.encode.insert(&enc.name, encode);
        return Ok(true);
    }
    if let Some((callee, args)) = string_spread(&block.items, resolved, ir, &enc.name)? {
        let decode = Proc {
            params: params.clone(),
            body: vec![Stmt::CallDecode {
                dst: "value".into(),
                callee: TypeId(callee.clone()),
                args: args.clone(),
            }],
            result: Some(Expr::Ident("value".into())),
        };
        let encode = Proc {
            params,
            body: vec![Stmt::CallEncode {
                callee: TypeId(callee),
                args,
                value: Expr::Ident("value".into()),
            }],
            result: None,
        };
        ir.types.insert(&enc.name, TypeDef::String);
        ir.decode.insert(&enc.name, decode);
        ir.encode.insert(&enc.name, encode);
        return Ok(true);
    }
    Ok(false)
}

struct StringShape {
    len_name: String,
    len_ty: String,
    val_name: String,
    requires: Vec<Expr>,
}

fn direct_string(items: &[BlockItem], ir: &IrSchema) -> Option<StringShape> {
    let mut len_name = None;
    let mut len_ty = None;
    let mut val_name = None;
    let mut bytes_of = None;
    let mut requires = Vec::new();
    for item in items {
        match item {
            BlockItem::Require(expr) => requires.push(expr.clone()),
            BlockItem::Field(wf) if wf.if_expr.is_none() && wf.repeat.is_none() => {
                let FieldType::Ref(r) = &wf.type_ref else {
                    return None;
                };
                let int_len =
                    r.transforms.is_empty() && r.args.is_empty() && is_int_scalar(ir, &r.name);
                let utf8_bytes =
                    r.name == "bytes" && r.transforms == ["utf8".to_string()] && r.args.len() == 1;
                if int_len {
                    if len_name.is_some() {
                        return None;
                    }
                    len_name = Some(wf.name.clone());
                    len_ty = Some(r.name.clone());
                } else if utf8_bytes {
                    if val_name.is_some() {
                        return None;
                    }
                    let Expr::Ident(arg) = &r.args[0] else {
                        return None;
                    };
                    val_name = Some(wf.name.clone());
                    bytes_of = Some(arg.clone());
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if bytes_of.as_ref() != len_name.as_ref() {
        return None;
    }
    Some(StringShape {
        len_name: len_name?,
        len_ty: len_ty?,
        val_name: val_name?,
        requires,
    })
}

fn is_int_scalar(ir: &IrSchema, name: &str) -> bool {
    match ir.types.get(name) {
        Some(TypeDef::Scalar(class)) => matches!(
            class,
            Storage::I8
                | Storage::I16
                | Storage::I32
                | Storage::I64
                | Storage::I128
                | Storage::U8
                | Storage::U16
                | Storage::U32
                | Storage::U64
        ),
        _ => false,
    }
}

fn string_spread(
    items: &[BlockItem],
    resolved: &ResolvedSchema,
    ir: &IrSchema,
    owner: &str,
) -> BmlResult<Option<(String, Vec<Expr>)>> {
    if items.len() != 1 {
        return Ok(None);
    }
    let BlockItem::Spread { name, args } = &items[0] else {
        return Ok(None);
    };
    if !matches!(ir.types.get(name), Some(TypeDef::String)) {
        return Ok(None);
    }
    let r = TypeRef {
        name: name.clone(),
        args: args.clone(),
        transforms: Vec::new(),
    };
    Ok(Some((name.clone(), call_args(resolved, &r, owner)?)))
}

fn string_procs(shape: &StringShape, params: &[String]) -> (Proc, Proc) {
    let mut decode = vec![
        Stmt::CallDecode {
            dst: shape.len_name.clone(),
            callee: TypeId(shape.len_ty.clone()),
            args: Vec::new(),
        },
        Stmt::CallDecode {
            dst: shape.val_name.clone(),
            callee: TypeId("bytes".into()),
            args: vec![Expr::Ident(shape.len_name.clone())],
        },
        Stmt::Utf8 {
            dst: shape.val_name.clone(),
            src: shape.val_name.clone(),
        },
    ];
    let mut encode = Vec::new();
    if shape.val_name != "value" {
        encode.push(Stmt::Let {
            name: shape.val_name.clone(),
            expr: Expr::Ident("value".into()),
        });
    }
    encode.push(Stmt::Let {
        name: shape.len_name.clone(),
        expr: Expr::Call("utf8_len".into(), vec![Expr::Ident("value".into())]),
    });
    encode.push(Stmt::CallEncode {
        callee: TypeId(shape.len_ty.clone()),
        args: Vec::new(),
        value: Expr::Ident(shape.len_name.clone()),
    });
    encode.push(Stmt::CallEncode {
        callee: TypeId("bytes".into()),
        args: vec![Expr::Ident(shape.len_name.clone())],
        value: Expr::Ident("value".into()),
    });
    for req in &shape.requires {
        decode.push(Stmt::Require(req.clone()));
        encode.push(Stmt::Require(req.clone()));
    }
    let decode = Proc {
        params: params.to_vec(),
        body: decode,
        result: Some(Expr::Ident(shape.val_name.clone())),
    };
    let encode = Proc {
        params: params.to_vec(),
        body: encode,
        result: None,
    };
    (decode, encode)
}

fn lower_enums(resolved: &ResolvedSchema, ir: &mut IrSchema) -> BmlResult<()> {
    for en in resolved.enums.values() {
        let base = match ir.types.get(&en.base) {
            Some(TypeDef::Scalar(class)) => *class,
            _ => Storage::I128,
        };
        let members = en
            .members
            .iter()
            .map(|m| (m.name.clone(), m.value))
            .collect();
        ir.types.insert(&en.name, TypeDef::Enum { base, members });
        ir.decode.insert(
            &en.name,
            Proc {
                params: Vec::new(),
                body: vec![Stmt::CallDecode {
                    dst: "value".into(),
                    callee: TypeId(en.base.clone()),
                    args: Vec::new(),
                }],
                result: Some(Expr::Ident("value".into())),
            },
        );
        ir.encode.insert(
            &en.name,
            Proc {
                params: Vec::new(),
                body: vec![Stmt::CallEncode {
                    callee: TypeId(en.base.clone()),
                    args: Vec::new(),
                    value: Expr::Ident("value".into()),
                }],
                result: None,
            },
        );
    }
    Ok(())
}

fn lower_records(resolved: &ResolvedSchema, ir: &mut IrSchema) -> BmlResult<()> {
    let mut progress = true;
    while progress {
        progress = false;
        for enc in resolved.encodings.values() {
            if enc.yield_expr.is_some() || ir.types.contains_key(&enc.name) {
                continue;
            }
            match &enc.body {
                EncBody::Ref(inner) => {
                    if ir.types.contains_key(&inner.name) || is_builtin(&inner.name) {
                        install_alias(enc, inner, ir);
                        progress = true;
                    }
                }
                EncBody::Block(block) => {
                    let params: Vec<String> = enc.params.iter().map(|p| p.name.clone()).collect();
                    ir.types
                        .insert(&enc.name, TypeDef::Record { fields: Vec::new() });
                    install_record(&enc.name, &params, &block.items, resolved, ir)?;
                    progress = true;
                }
            }
        }
    }
    Ok(())
}

fn install_alias(enc: &EncodingDecl, inner: &TypeRef, ir: &mut IrSchema) {
    let def = match ir.types.get(&inner.name) {
        Some(TypeDef::Scalar(class)) => TypeDef::Scalar(*class),
        Some(TypeDef::String) => TypeDef::String,
        _ => TypeDef::Record { fields: Vec::new() },
    };
    ir.types.insert(&enc.name, def);
    ir.decode.insert(
        &enc.name,
        Proc {
            params: Vec::new(),
            body: vec![Stmt::CallDecode {
                dst: "value".into(),
                callee: TypeId(inner.name.clone()),
                args: inner.args.clone(),
            }],
            result: Some(Expr::Ident("value".into())),
        },
    );
    ir.encode.insert(
        &enc.name,
        Proc {
            params: Vec::new(),
            body: vec![Stmt::CallEncode {
                callee: TypeId(inner.name.clone()),
                args: inner.args.clone(),
                value: Expr::Ident("value".into()),
            }],
            result: None,
        },
    );
}

fn lower_frames(resolved: &ResolvedSchema, ir: &mut IrSchema) -> BmlResult<()> {
    for proto in resolved.protocols.values() {
        if proto.frames.is_empty() && !proto.routes.is_empty() {
            return Err(emit(format!("protocol {}: no frame", proto.name)));
        }
        for frame in &proto.frames {
            let type_id = TypeId(format!("frame_{}", frame.name));
            install_record(&type_id.0, &[], &frame.items, resolved, ir)?;
            ir.frames.push(super::schema::Frame {
                name: frame.name.clone(),
                type_id: type_id.clone(),
                decode: type_id.0.clone(),
                encode: type_id.0,
            });
        }
    }
    for (route, packet) in &resolved.routes {
        let prefixed = format!("packet:{}", packet.name);
        let key = if ir.decode.contains_key(&prefixed) {
            prefixed
        } else {
            packet.name.clone()
        };
        ir.routes.push(Route {
            state: route.state.clone(),
            direction: route.direction.clone(),
            packet_id: route.packet_id,
            packet: TypeId(key),
            frame: route.frame.clone().unwrap_or_else(|| "default".into()),
        });
    }
    Ok(())
}
fn repeat_stmt(
    wf: &WireField,
    dst: &str,
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
    ir: &IrSchema,
    cx: &mut RecCx,
) -> BmlResult<Stmt> {
    let rep = wf.repeat.as_ref().unwrap();
    let elem = if decode {
        decode_field("__elem", &wf.type_ref, resolved, owner, ir, cx)?
    } else {
        encode_element(&wf.type_ref, "__elem", resolved, owner, ir, cx)?
    };
    let expr = rep.expr.clone().unwrap_or(Expr::Int(0));
    cx.check(&expr, owner)?;
    Ok(match rep.mode {
        RepeatMode::Count => Stmt::RepeatCount {
            n: expr,
            elem,
            dst: dst.to_string(),
        },
        RepeatMode::Until => Stmt::RepeatUntil {
            term: expr,
            elem,
            dst: dst.to_string(),
        },
        RepeatMode::UntilEnd => Stmt::RepeatUntilEnd {
            elem,
            dst: dst.to_string(),
        },
    })
}

fn encode_element(
    ty: &FieldType,
    dst: &str,
    resolved: &ResolvedSchema,
    owner: &str,
    ir: &IrSchema,
    cx: &mut RecCx,
) -> BmlResult<Vec<Stmt>> {
    match ty {
        FieldType::Ref(r) => {
            let args = call_args(resolved, r, owner)?;
            for arg in &args {
                cx.check(arg, owner)?;
            }
            Ok(vec![Stmt::CallEncode {
                callee: TypeId(r.name.clone()),
                args,
                value: Expr::Ident(dst.to_string()),
            }])
        }
        FieldType::Block(block) => {
            cx.scopes.push(RecScope::default());
            let mut parent = Renamer::default();
            std::mem::swap(&mut cx.renamer, &mut parent);
            let body = lower_items(&block.items, false, resolved, owner, ir, cx);
            std::mem::swap(&mut cx.renamer, &mut parent);
            cx.scopes.pop();
            Ok(vec![Stmt::Block {
                body: body?,
                dst: dst.to_string(),
            }])
        }
        FieldType::Match(mt) => {
            cx.check(&mt.tag_expr, owner)?;
            Ok(vec![typed_match(dst, mt, false, resolved, owner, ir, cx)?])
        }
    }
}

fn typed_match(
    dst: &str,
    mt: &crate::ast::MatchType,
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
    ir: &IrSchema,
    cx: &mut RecCx,
) -> BmlResult<Stmt> {
    let mut arms = Vec::new();
    let mut default = None;
    for arm in &mt.arms {
        let body = match &arm.type_ref {
            None => Vec::new(),
            Some(ty) if decode => decode_field(dst, ty, resolved, owner, ir, cx)?,
            Some(ty) => encode_element(ty, dst, resolved, owner, ir, cx)?,
        };
        match arm_tags(arm)? {
            None => default = Some(body),
            Some(tags) => arms.push(IrArm { tags, body }),
        }
    }
    Ok(Stmt::Match {
        scrutinee: mt.tag_expr.clone(),
        arms,
        default,
    })
}

fn stmt_match(
    mt: &crate::ast::MatchType,
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
    ir: &IrSchema,
    cx: &mut RecCx,
) -> BmlResult<Stmt> {
    let mut arms = Vec::new();
    let mut default = None;
    for arm in &mt.arms {
        let body = match &arm.type_ref {
            // Arms share the record namespace: their bindings leak into the
            // interpreter's environment like group bindings do.
            Some(FieldType::Block(block)) => {
                lower_items(&block.items, decode, resolved, owner, ir, cx)?
            }
            None => Vec::new(),
            Some(_) => {
                return Err(emit(format!(
                    "{owner}: statement match arm must be a block"
                )));
            }
        };
        match arm_tags(arm)? {
            None => default = Some(body),
            Some(tags) => arms.push(IrArm { tags, body }),
        }
    }
    Ok(Stmt::Match {
        scrutinee: mt.tag_expr.clone(),
        arms,
        default,
    })
}

/// Every tag of an arm, once each: `tag` is the first of `tags` when both
/// are set.
fn arm_tags(arm: &AstArm) -> BmlResult<Option<Vec<i128>>> {
    if arm.tag.is_none() && arm.tags.is_empty() {
        return Ok(None);
    }
    let mut tags = Vec::new();
    for tag in arm.tag.iter().chain(&arm.tags) {
        let v = tag_int(tag)?;
        if !tags.contains(&v) {
            tags.push(v);
        }
    }
    Ok(Some(tags))
}

fn tag_int(tag: &Tag) -> BmlResult<i128> {
    match tag {
        Tag::Int(v) => Ok(*v),
        Tag::Str(s) => Err(emit(format!("string match tag {s} is not lowered"))),
    }
}

fn expand_spreads(items: &[BlockItem], resolved: &ResolvedSchema) -> BmlResult<Vec<BlockItem>> {
    let mut out = Vec::new();
    for item in items {
        if let BlockItem::Spread { name, args } = item {
            let Some(enc) = resolved.encodings.get(name) else {
                return Err(emit(format!("unknown spread {name}")));
            };
            let EncBody::Block(block) = &enc.body else {
                return Err(emit(format!("spread {name}: alias body is not lowered")));
            };
            let mut map = HashMap::new();
            for (i, param) in enc.params.iter().enumerate() {
                if let Some(arg) = args.get(i) {
                    map.insert(param.name.clone(), arg.clone());
                } else if let Some(default) = param.default {
                    map.insert(param.name.clone(), Expr::Int(default));
                }
            }
            let substituted = subst_items(&block.items, &map);
            out.extend(expand_spreads(&substituted, resolved)?);
        } else {
            out.push(item.clone());
        }
    }
    Ok(out)
}

fn subst_items(items: &[BlockItem], map: &HashMap<String, Expr>) -> Vec<BlockItem> {
    items.iter().map(|item| subst_item(item, map)).collect()
}

fn subst_item(item: &BlockItem, map: &HashMap<String, Expr>) -> BlockItem {
    match item {
        BlockItem::Require(expr) => BlockItem::Require(subst_expr(expr, map)),
        BlockItem::Computed { name, expr } => BlockItem::Computed {
            name: name.clone(),
            expr: subst_expr(expr, map),
        },
        BlockItem::Field(wf) => BlockItem::Field(Box::new(WireField {
            name: wf.name.clone(),
            type_ref: subst_type(&wf.type_ref, map),
            if_expr: wf.if_expr.as_ref().map(|expr| subst_expr(expr, map)),
            repeat: wf.repeat.clone().map(|mut rep| {
                rep.expr = rep.expr.map(|expr| subst_expr(&expr, map));
                rep
            }),
            encode_expr: wf.encode_expr.as_ref().map(|expr| subst_expr(expr, map)),
        })),
        BlockItem::Spread { name, args } => BlockItem::Spread {
            name: name.clone(),
            args: args.iter().map(|arg| subst_expr(arg, map)).collect(),
        },
        BlockItem::Group { cond, items } => BlockItem::Group {
            cond: cond.as_ref().map(|expr| subst_expr(expr, map)),
            items: subst_items(items, map),
        },
        BlockItem::MatchStmt(mt) => BlockItem::MatchStmt(crate::ast::MatchType {
            tag_expr: subst_expr(&mt.tag_expr, map),
            arms: mt
                .arms
                .iter()
                .map(|arm| AstArm {
                    tag: arm.tag.clone(),
                    tags: arm.tags.clone(),
                    type_ref: arm.type_ref.as_ref().map(|ty| subst_type(ty, map)),
                })
                .collect(),
        }),
    }
}

fn subst_type(ty: &FieldType, map: &HashMap<String, Expr>) -> FieldType {
    match ty {
        FieldType::Ref(r) => FieldType::Ref(TypeRef {
            name: r.name.clone(),
            args: r.args.iter().map(|arg| subst_expr(arg, map)).collect(),
            transforms: r.transforms.clone(),
        }),
        FieldType::Block(block) => FieldType::Block(crate::ast::BlockType {
            items: subst_items(&block.items, map),
        }),
        FieldType::Match(mt) => FieldType::Match(crate::ast::MatchType {
            tag_expr: subst_expr(&mt.tag_expr, map),
            arms: mt
                .arms
                .iter()
                .map(|arm| AstArm {
                    tag: arm.tag.clone(),
                    tags: arm.tags.clone(),
                    type_ref: arm.type_ref.as_ref().map(|ty| subst_type(ty, map)),
                })
                .collect(),
        }),
    }
}

fn subst_expr(expr: &Expr, map: &HashMap<String, Expr>) -> Expr {
    match expr {
        Expr::Ident(name) => map.get(name).cloned().unwrap_or_else(|| expr.clone()),
        Expr::Unop(op, inner) => Expr::Unop(op.clone(), Box::new(subst_expr(inner, map))),
        Expr::Binop(op, left, right) => Expr::Binop(
            op.clone(),
            Box::new(subst_expr(left, map)),
            Box::new(subst_expr(right, map)),
        ),
        Expr::Ternary(cond, yes, no) => Expr::Ternary(
            Box::new(subst_expr(cond, map)),
            Box::new(subst_expr(yes, map)),
            Box::new(subst_expr(no, map)),
        ),
        Expr::Call(name, args) => Expr::Call(
            name.clone(),
            args.iter().map(|arg| subst_expr(arg, map)).collect(),
        ),
        other => other.clone(),
    }
}

fn collect_fields(
    owner: &str,
    items: &[BlockItem],
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
    renamer: &mut Renamer,
) -> BmlResult<Vec<Field>> {
    let mut fields = Vec::new();
    for item in items {
        match item {
            BlockItem::Require(_) | BlockItem::Spread { .. } => {}
            BlockItem::Computed { name, expr } => {
                let ty = Ty::Scalar(storage_of(expr, &renamer.classes(ir)));
                add_field(
                    owner,
                    &mut fields,
                    renamer,
                    Field {
                        name: name.clone(),
                        ty,
                        optional: false,
                    },
                )?;
            }
            BlockItem::Field(wf) => {
                add_field(
                    owner,
                    &mut fields,
                    renamer,
                    field_desc(owner, wf, false, resolved, ir)?,
                )?;
            }
            BlockItem::Group { cond, items } => {
                let mut inner = collect_fields(owner, items, resolved, ir, renamer)?;
                if cond.is_some() {
                    for field in &mut inner {
                        field.optional = true;
                    }
                }
                for field in inner {
                    add_field(owner, &mut fields, renamer, field)?;
                }
            }
            BlockItem::MatchStmt(mt) => {
                for field in match_stmt_fields(owner, mt, resolved, ir, renamer)? {
                    add_field(owner, &mut fields, renamer, field)?;
                }
            }
        }
    }
    Ok(fields)
}

/// Pushes `field`, renaming it to `<name>_<n>` when an earlier binding of
/// `field.name` has a different type. Same-type rebindings keep the name and
/// merge optionality. There is no `__any`: every field keeps one type.
fn add_field(
    owner: &str,
    fields: &mut Vec<Field>,
    renamer: &mut Renamer,
    field: Field,
) -> BmlResult<()> {
    let _ = owner;
    let final_name = renamer.dst_for(&field.name, &field.ty);
    if let Some(existing) = fields.iter_mut().find(|have| have.name == final_name) {
        existing.optional = existing.optional && field.optional;
        return Ok(());
    }
    fields.push(Field {
        name: final_name,
        ty: field.ty,
        optional: field.optional,
    });
    Ok(())
}

fn field_desc(
    owner: &str,
    wf: &WireField,
    force_opt: bool,
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
) -> BmlResult<Field> {
    match &wf.type_ref {
        FieldType::Ref(_) => {}
        FieldType::Match(mt) => {
            let id = format!("{owner}_{}", wf.name);
            let arms = sum_arms(&id, mt, resolved, ir)?;
            ir.types.insert(&id, TypeDef::Sum { arms });
        }
        FieldType::Block(block) => {
            let id = format!("{owner}_{}_block", wf.name);
            // A nested record is its own namespace.
            let mut nested = Renamer::default();
            let fields = collect_fields(&id, &block.items, resolved, ir, &mut nested)?;
            ir.types.insert(&id, TypeDef::Record { fields });
        }
    };
    let ty = field_ty(owner, wf, ir);
    Ok(Field {
        name: wf.name.clone(),
        ty,
        optional: force_opt || wf.if_expr.is_some(),
    })
}

/// The record-field type of `wf`, without creating nested types. Must agree
/// with `field_desc`; the procedure walks use it to replay the same renames.
fn field_ty(owner: &str, wf: &WireField, ir: &IrSchema) -> Ty {
    let ty = match &wf.type_ref {
        FieldType::Ref(r) => ty_of(&r.name, ir),
        FieldType::Match(_) => Ty::Named(TypeId(format!("{owner}_{}", wf.name))),
        FieldType::Block(_) => Ty::Named(TypeId(format!("{owner}_{}_block", wf.name))),
    };
    if wf.repeat.is_some() {
        Ty::List(Box::new(ty))
    } else {
        ty
    }
}

fn ty_of(name: &str, ir: &IrSchema) -> Ty {
    match ir.types.get(name) {
        Some(TypeDef::Scalar(class)) => Ty::Scalar(*class),
        Some(TypeDef::String) => Ty::Scalar(Storage::Str),
        _ => Ty::Named(TypeId(name.to_string())),
    }
}

/// Storage class of a field type, for the interval rule.
fn ty_storage(ty: &Ty, ir: &IrSchema) -> Option<Storage> {
    match ty {
        Ty::Scalar(class) => Some(*class),
        Ty::Named(id) => match ir.types.get(&id.0) {
            Some(TypeDef::Scalar(class)) => Some(*class),
            Some(TypeDef::Enum { base, .. }) => Some(*base),
            _ => None,
        },
        Ty::List(_) | Ty::Optional(_) => None,
    }
}

fn sum_arms(
    owner: &str,
    mt: &crate::ast::MatchType,
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
) -> BmlResult<Vec<Variant>> {
    let mut arms = Vec::new();
    for (i, arm) in mt.arms.iter().enumerate() {
        let ty = match &arm.type_ref {
            None => Ty::Scalar(Storage::U8),
            Some(FieldType::Ref(r)) => ty_of(&r.name, ir),
            Some(FieldType::Block(block)) => {
                let id = format!("{owner}_arm{i}");
                let mut nested = Renamer::default();
                let fields = collect_fields(&id, &block.items, resolved, ir, &mut nested)?;
                ir.types.insert(&id, TypeDef::Record { fields });
                Ty::Named(TypeId(id))
            }
            Some(FieldType::Match(_)) => Ty::Named(TypeId(format!("{owner}_arm{i}_match"))),
        };
        let tag = match &arm.tag {
            None if arm.tags.is_empty() => None,
            Some(Tag::Int(v)) => Some(*v),
            Some(Tag::Str(s)) => return Err(emit(format!("string match tag {s}"))),
            None => arm.tags.iter().find_map(|tag| match tag {
                Tag::Int(v) => Some(*v),
                Tag::Str(_) => None,
            }),
        };
        arms.push(Variant { tag, ty });
    }
    Ok(arms)
}

fn match_stmt_fields(
    owner: &str,
    mt: &crate::ast::MatchType,
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
    renamer: &mut Renamer,
) -> BmlResult<Vec<Field>> {
    let mut per_arm = Vec::new();
    for arm in &mt.arms {
        let items = match &arm.type_ref {
            Some(FieldType::Block(block)) => &block.items,
            None => {
                per_arm.push(Vec::new());
                continue;
            }
            Some(_) => {
                return Err(emit(format!(
                    "{owner}: statement match arm must be a block"
                )));
            }
        };
        let mut arm_renamer = Renamer::default();
        per_arm.push(collect_fields(
            owner,
            items,
            resolved,
            ir,
            &mut arm_renamer,
        )?);
    }
    let mut order = Vec::new();
    let mut seen: HashMap<String, (Ty, usize, bool)> = HashMap::new();
    for arm in &per_arm {
        let mut in_arm = HashMap::new();
        for field in arm {
            // Same-name bindings with different types rename, as in groups.
            let final_name = renamer.dst_for(&field.name, &field.ty);
            in_arm.insert(final_name.clone(), field.optional);
            if let std::collections::hash_map::Entry::Vacant(slot) = seen.entry(final_name.clone())
            {
                order.push(final_name);
                slot.insert((field.ty.clone(), 0, field.optional));
            }
        }
        for (name, optional) in in_arm {
            let entry = seen.get_mut(&name).unwrap();
            entry.1 += 1;
            entry.2 = entry.2 || optional;
        }
    }
    let arms = per_arm.len();
    Ok(order
        .into_iter()
        .map(|name| {
            let (ty, count, optional) = seen.remove(&name).unwrap();
            Field {
                name,
                ty,
                optional: optional || count < arms,
            }
        })
        .collect())
}
