//! `ResolvedSchema` → [`IrSchema`].
//!
//! Scalar encodings become a call to each field's type. Bodies are not inlined.

use std::collections::HashMap;

use crate::ast::{BlockItem, EncBody, EncodingDecl, Expr, FieldType, ResolvedSchema, TypeRef};
use crate::errors::{BpsResult, emit};
use crate::ordmap::OrderedMap;
use crate::primitives::is_bare_primitive;

use super::schema::{Field, IrSchema, Proc, Stmt, Ty, TypeDef, TypeId, WireOp};
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
    for pkt in resolved.packets.values() {
        lower_packet(&pkt.name, &pkt.items, resolved, &mut ir)?;
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

fn lower_packet(
    name: &str,
    items: &[BlockItem],
    resolved: &ResolvedSchema,
    ir: &mut IrSchema,
) -> BpsResult<()> {
    let mut fields = Vec::new();
    for item in items {
        let BlockItem::Field(wf) = item else {
            return Err(emit(format!(
                "packet {name}: only plain fields are lowered so far"
            )));
        };
        let FieldType::Ref(r) = &wf.type_ref else {
            return Err(emit(format!(
                "packet {name} field {}: not a named type",
                wf.name
            )));
        };
        fields.push(Field {
            name: wf.name.clone(),
            ty: Ty::Named(TypeId(r.name.clone())),
            optional: wf.if_expr.is_some(),
        });
    }
    ir.types.insert(name, TypeDef::Record { fields });
    ir.decode.insert(
        name,
        Proc {
            params: Vec::new(),
            body: lower_items(items, true, resolved, name)?,
            result: None,
        },
    );
    ir.encode.insert(
        name,
        Proc {
            params: Vec::new(),
            body: lower_items(items, false, resolved, name)?,
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
                let inner = if decode {
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
                    });
                } else {
                    out.extend(inner);
                }
            }
            BlockItem::Spread { .. } | BlockItem::Group { .. } | BlockItem::MatchStmt(_) => {
                return Err(emit(format!(
                    "{owner}: spread, group, and statement match are not lowered yet"
                )));
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
    let FieldType::Ref(r) = ty else {
        return Err(emit(format!(
            "{owner} field {dst}: inline type is not lowered yet"
        )));
    };
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
    let FieldType::Ref(r) = ty else {
        return Err(emit(format!(
            "{owner} field {name}: inline type is not lowered yet"
        )));
    };
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
    if is_builtin(&r.name) || is_bare_primitive(&r.name) {
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
