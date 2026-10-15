//! `ResolvedSchema` → [`IrSchema`].
//!
//! Scalar encodings become a call to each field's type. Bodies are not inlined.

use std::collections::HashMap;

use super::schema::{Field, IrArm, IrSchema, Proc, Stmt, Ty, TypeDef, TypeId, Variant, WireOp};
use crate::ast::{
    BlockItem, EncBody, EncodingDecl, Expr, FieldType, MatchArm as AstArm, RepeatMode,
    ResolvedSchema, Tag, TypeRef, WireField,
};
use crate::errors::{BpsResult, emit};
use crate::ordmap::OrderedMap;
use crate::primitives::is_bare_primitive;

use super::width::{Storage, storage_of};

const BUILTINS: &[&str] = &["u8", "f32be", "f64be", "bytes", "rest"];

pub fn lower(resolved: &ResolvedSchema) -> BpsResult<IrSchema> {
    let mut ir = IrSchema {
        types: OrderedMap::new(),
        decode: OrderedMap::new(),
        encode: OrderedMap::new(),
        routes: Vec::new(),
        frames: OrderedMap::new(),
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
        install_record(&pkt.name, &[], &pkt.items, resolved, &mut ir)?;
    }
    Ok(ir)
}

fn install_builtins(ir: &mut IrSchema) {
    let scalar = |class| TypeDef::Scalar(class);
    ir.types.insert("u8", scalar(Storage::U8));
    ir.types.insert("f32be", scalar(Storage::F64));
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

fn lower_scalar(enc: &EncodingDecl, resolved: &ResolvedSchema, ir: &mut IrSchema) -> BpsResult<()> {
    let EncBody::Block(block) = &enc.body else {
        return Err(emit(format!(
            "encoding {}: a scalar alias is not lowered",
            enc.name
        )));
    };
    let params: Vec<String> = enc.params.iter().map(|p| p.name.clone()).collect();
    let decode_body = lower_items(&block.items, true, resolved, &enc.name)?;
    let encode_body = lower_items(&block.items, false, resolved, &enc.name)?;
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
) -> BpsResult<()> {
    let items = expand_spreads(items, resolved)?;
    let fields = collect_fields(name, &items, resolved, ir)?;
    ir.types.insert(name, TypeDef::Record { fields });
    ir.decode.insert(
        name,
        Proc {
            params: params.to_vec(),
            body: lower_items(&items, true, resolved, name)?,
            result: None,
        },
    );
    ir.encode.insert(
        name,
        Proc {
            params: params.to_vec(),
            body: lower_items(&items, false, resolved, name)?,
            result: None,
        },
    );
    Ok(())
}

/// `decode` selects decode statements. Encode drops `require` and uses each field's `= expr`.
fn lower_items(
    items: &[BlockItem],
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
) -> BpsResult<Vec<Stmt>> {
    let mut out = Vec::new();
    for item in items {
        match item {
            BlockItem::Require(expr) => {
                if decode {
                    out.push(Stmt::Require(expr.clone()));
                }
            }
            BlockItem::Computed { name, expr } => {
                out.push(Stmt::Let {
                    name: name.clone(),
                    expr: expr.clone(),
                });
            }
            BlockItem::Field(wf) => {
                let inner = if wf.repeat.is_some() {
                    vec![repeat_stmt(wf, decode, resolved, owner)?]
                } else if decode {
                    decode_field(&wf.name, &wf.type_ref, resolved, owner)?
                } else {
                    let value = wf
                        .encode_expr
                        .clone()
                        .unwrap_or_else(|| Expr::Ident(wf.name.clone()));
                    encode_field(&wf.name, &wf.type_ref, value, resolved, owner)?
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
                let inner = lower_items(items, decode, resolved, owner)?;
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
            BlockItem::MatchStmt(m) => out.push(stmt_match(m, decode, resolved, owner)?),
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
) -> BpsResult<Vec<Stmt>> {
    match ty {
        FieldType::Ref(r) => ref_decode(dst, r, resolved, owner),
        FieldType::Match(mt) => Ok(vec![typed_match(dst, mt, true, resolved, owner)?]),
        FieldType::Block(block) => Ok(vec![Stmt::Block {
            body: lower_items(&block.items, true, resolved, owner)?,
            dst: dst.to_string(),
        }]),
    }
}

fn ref_decode(
    dst: &str,
    r: &TypeRef,
    resolved: &ResolvedSchema,
    owner: &str,
) -> BpsResult<Vec<Stmt>> {
    check_transform(r, owner)?;
    let mut stmts = vec![Stmt::CallDecode {
        dst: dst.to_string(),
        callee: TypeId(r.name.clone()),
        args: call_args(resolved, r, owner)?,
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
) -> BpsResult<Vec<Stmt>> {
    match ty {
        FieldType::Ref(r) => ref_encode(name, r, value, resolved, owner),
        FieldType::Match(mt) => Ok(vec![typed_match(name, mt, false, resolved, owner)?]),
        FieldType::Block(block) => Ok(vec![Stmt::Block {
            body: lower_items(&block.items, false, resolved, owner)?,
            dst: name.to_string(),
        }]),
    }
}

fn ref_encode(
    name: &str,
    r: &TypeRef,
    value: Expr,
    resolved: &ResolvedSchema,
    owner: &str,
) -> BpsResult<Vec<Stmt>> {
    check_transform(r, owner)?;
    Ok(vec![
        Stmt::Let {
            name: name.to_string(),
            expr: value,
        },
        Stmt::CallEncode {
            callee: TypeId(r.name.clone()),
            args: call_args(resolved, r, owner)?,
            value: Expr::Ident(name.to_string()),
        },
    ])
}

fn check_transform(r: &TypeRef, owner: &str) -> BpsResult<()> {
    for t in &r.transforms {
        if t != "utf8" {
            return Err(emit(format!("{owner}: transform {t} is not utf8")));
        }
    }
    Ok(())
}

fn call_args(resolved: &ResolvedSchema, r: &TypeRef, owner: &str) -> BpsResult<Vec<Expr>> {
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
fn lower_strings(resolved: &ResolvedSchema, ir: &mut IrSchema) -> BpsResult<()> {
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
) -> BpsResult<bool> {
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
) -> BpsResult<Option<(String, Vec<Expr>)>> {
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

fn lower_enums(resolved: &ResolvedSchema, ir: &mut IrSchema) -> BpsResult<()> {
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

fn lower_records(resolved: &ResolvedSchema, ir: &mut IrSchema) -> BpsResult<()> {
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

fn repeat_stmt(
    wf: &WireField,
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
) -> BpsResult<Stmt> {
    let rep = wf.repeat.as_ref().unwrap();
    let elem = if decode {
        decode_field("__elem", &wf.type_ref, resolved, owner)?
    } else {
        encode_element(&wf.type_ref, resolved, owner)?
    };
    let expr = rep.expr.clone().unwrap_or(Expr::Int(0));
    Ok(match rep.mode {
        RepeatMode::Count => Stmt::RepeatCount {
            n: expr,
            elem,
            dst: wf.name.clone(),
        },
        RepeatMode::Until => Stmt::RepeatUntil {
            term: expr,
            elem,
            dst: wf.name.clone(),
        },
        RepeatMode::UntilEnd => Stmt::RepeatUntilEnd {
            elem,
            dst: wf.name.clone(),
        },
    })
}

fn encode_element(ty: &FieldType, resolved: &ResolvedSchema, owner: &str) -> BpsResult<Vec<Stmt>> {
    match ty {
        FieldType::Ref(r) => Ok(vec![Stmt::CallEncode {
            callee: TypeId(r.name.clone()),
            args: call_args(resolved, r, owner)?,
            value: Expr::Ident("__elem".into()),
        }]),
        FieldType::Block(block) => Ok(vec![Stmt::Block {
            body: lower_items(&block.items, false, resolved, owner)?,
            dst: "__elem".into(),
        }]),
        FieldType::Match(mt) => Ok(vec![typed_match("__elem", mt, false, resolved, owner)?]),
    }
}

fn typed_match(
    dst: &str,
    mt: &crate::ast::MatchType,
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
) -> BpsResult<Stmt> {
    let mut arms = Vec::new();
    let mut default = None;
    for arm in &mt.arms {
        let body = match &arm.type_ref {
            None => Vec::new(),
            Some(ty) if decode => decode_field(dst, ty, resolved, owner)?,
            Some(ty) => encode_element(ty, resolved, owner)?,
        };
        // encode_element writes `__elem`, but a match field's value is the field itself.
        let body = if decode {
            body
        } else {
            rewrite_elem_value(&body, dst)
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

fn rewrite_elem_value(stmts: &[Stmt], dst: &str) -> Vec<Stmt> {
    stmts
        .iter()
        .map(|stmt| match stmt {
            Stmt::CallEncode {
                callee,
                args,
                value: Expr::Ident(n),
            } if *n == "__elem" => Stmt::CallEncode {
                callee: callee.clone(),
                args: args.clone(),
                value: Expr::Ident(dst.to_string()),
            },
            other => other.clone(),
        })
        .collect()
}

fn stmt_match(
    mt: &crate::ast::MatchType,
    decode: bool,
    resolved: &ResolvedSchema,
    owner: &str,
) -> BpsResult<Stmt> {
    let mut arms = Vec::new();
    let mut default = None;
    for arm in &mt.arms {
        let body = match &arm.type_ref {
            Some(FieldType::Block(block)) => lower_items(&block.items, decode, resolved, owner)?,
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

fn arm_tags(arm: &AstArm) -> BpsResult<Option<Vec<i128>>> {
    if arm.tag.is_none() && arm.tags.is_empty() {
        return Ok(None);
    }
    let mut tags = Vec::new();
    if let Some(tag) = &arm.tag {
        tags.push(tag_int(tag)?);
    }
    for tag in &arm.tags {
        tags.push(tag_int(tag)?);
    }
    Ok(Some(tags))
}

fn tag_int(tag: &Tag) -> BpsResult<i128> {
    match tag {
        Tag::Int(v) => Ok(*v),
        Tag::Str(s) => Err(emit(format!("string match tag {s} is not lowered"))),
    }
}

fn expand_spreads(items: &[BlockItem], resolved: &ResolvedSchema) -> BpsResult<Vec<BlockItem>> {
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
) -> BpsResult<Vec<Field>> {
    let mut fields = Vec::new();
    for item in items {
        match item {
            BlockItem::Require(_) | BlockItem::Spread { .. } => {}
            BlockItem::Computed { name, expr } => fields.push(Field {
                name: name.clone(),
                ty: Ty::Scalar(storage_of(expr, &HashMap::new())),
                optional: false,
            }),
            BlockItem::Field(wf) => fields.push(field_desc(owner, wf, false, resolved, ir)?),
            BlockItem::Group { cond, items } => {
                let mut inner = collect_fields(owner, items, resolved, ir)?;
                if cond.is_some() {
                    for field in &mut inner {
                        field.optional = true;
                    }
                }
                fields.extend(inner);
            }
            BlockItem::MatchStmt(mt) => fields.extend(match_stmt_fields(owner, mt, resolved, ir)?),
        }
    }
    Ok(fields)
}

fn field_desc(
    owner: &str,
    wf: &WireField,
    force_opt: bool,
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
) -> BpsResult<Field> {
    let ty = match &wf.type_ref {
        FieldType::Ref(r) => ty_of(&r.name, ir),
        FieldType::Match(mt) => {
            let id = format!("{owner}_{}", wf.name);
            let arms = sum_arms(&id, mt, resolved, ir)?;
            ir.types.insert(&id, TypeDef::Sum { arms });
            Ty::Named(TypeId(id))
        }
        FieldType::Block(block) => {
            let id = format!("{owner}_{}_block", wf.name);
            let fields = collect_fields(&id, &block.items, resolved, ir)?;
            ir.types.insert(&id, TypeDef::Record { fields });
            Ty::Named(TypeId(id))
        }
    };
    let ty = if wf.repeat.is_some() {
        Ty::List(Box::new(ty))
    } else {
        ty
    };
    Ok(Field {
        name: wf.name.clone(),
        ty,
        optional: force_opt || wf.if_expr.is_some(),
    })
}

fn ty_of(name: &str, ir: &IrSchema) -> Ty {
    match ir.types.get(name) {
        Some(TypeDef::Scalar(class)) => Ty::Scalar(*class),
        Some(TypeDef::String) => Ty::Scalar(Storage::Str),
        _ => Ty::Named(TypeId(name.to_string())),
    }
}

fn sum_arms(
    owner: &str,
    mt: &crate::ast::MatchType,
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
) -> BpsResult<Vec<Variant>> {
    let mut arms = Vec::new();
    for (i, arm) in mt.arms.iter().enumerate() {
        let ty = match &arm.type_ref {
            None => Ty::Scalar(Storage::U8),
            Some(FieldType::Ref(r)) => ty_of(&r.name, ir),
            Some(FieldType::Block(block)) => {
                let id = format!("{owner}_arm{i}");
                let fields = collect_fields(&id, &block.items, resolved, ir)?;
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
) -> BpsResult<Vec<Field>> {
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
        per_arm.push(collect_fields(owner, items, resolved, ir)?);
    }
    let mut order = Vec::new();
    let mut seen: HashMap<String, (Ty, usize, bool)> = HashMap::new();
    for arm in &per_arm {
        let mut in_arm = HashMap::new();
        for field in arm {
            if let Some((prev, _, _)) = seen.get(&field.name)
                && prev != &field.ty
            {
                return Err(emit(format!(
                    "{owner}: match arms disagree on {}",
                    field.name
                )));
            }
            in_arm.insert(field.name.clone(), field.optional);
            if !seen.contains_key(&field.name) {
                order.push(field.name.clone());
                seen.insert(field.name.clone(), (field.ty.clone(), 0, field.optional));
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
