//! Run an [`IrSchema`] procedure. Scalar procs only; records and repeats come later.

use crate::ast::Expr;
use crate::errors::{BpsResult, encode};
use crate::interp::cursor::{Cursor, WriteCursor};
use crate::interp::expr::int_of;
use crate::value::{Env, Value};

use super::schema::{IrSchema, Stmt, WireOp};

pub fn exec_decode(ir: &IrSchema, ty: &str, args: &[Value], bytes: &[u8]) -> BpsResult<Value> {
    let mut cur = Cursor::new(bytes);
    exec_decode_at(ir, ty, args, &mut cur)
}

pub fn exec_encode(ir: &IrSchema, ty: &str, args: &[Value], value: &Value) -> BpsResult<Vec<u8>> {
    let proc = ir
        .encode
        .get(ty)
        .ok_or_else(|| encode(format!("no encode proc for {ty}")))?;
    let mut env = Env::new();
    prebind(&proc.body, &mut env);
    bind_args(&proc.params, args, &mut env)?;
    env.insert("value", value.clone());
    let mut w = WriteCursor::new();
    run_encode(ir, &proc.body, &mut env, &mut w)?;
    Ok(w.result())
}

fn exec_decode_at(
    ir: &IrSchema,
    ty: &str,
    args: &[Value],
    cur: &mut Cursor<'_>,
) -> BpsResult<Value> {
    let proc = ir
        .decode
        .get(ty)
        .ok_or_else(|| crate::errors::decode(format!("no decode proc for {ty}")))?;
    let mut env = Env::new();
    prebind(&proc.body, &mut env);
    bind_args(&proc.params, args, &mut env)?;
    run_decode(ir, &proc.body, &mut env, cur)?;
    match &proc.result {
        Some(expr) => eval_expr(expr, &env),
        None => Ok(Value::map(env)),
    }
}

fn bind_args(params: &[String], args: &[Value], env: &mut Env) -> BpsResult<()> {
    if args.len() < params.len() {
        return Err(encode(format!(
            "need {} arguments, got {}",
            params.len(),
            args.len()
        )));
    }
    for (name, value) in params.iter().zip(args.iter()) {
        env.insert(name, value.clone());
    }
    Ok(())
}

/// Optional wire bytes are 0 when the `if` does not read them, matching `decode_scalar`.
fn prebind(stmts: &[Stmt], env: &mut Env) {
    for stmt in stmts {
        match stmt {
            Stmt::Read { dst, .. } | Stmt::CallDecode { dst, .. } | Stmt::Let { name: dst, .. } => {
                if !env.contains_key(dst) {
                    env.insert(dst, Value::Int(0));
                }
            }
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                prebind(then_body, env);
                prebind(else_body, env);
            }
            Stmt::Match { arms, default, .. } => {
                for arm in arms {
                    prebind(&arm.body, env);
                }
                if let Some(body) = default {
                    prebind(body, env);
                }
            }
            Stmt::RepeatCount { elem, .. }
            | Stmt::RepeatUntil { elem, .. }
            | Stmt::RepeatUntilEnd { elem, .. } => prebind(elem, env),
            Stmt::Require(_) | Stmt::Write { .. } | Stmt::Utf8 { .. } | Stmt::CallEncode { .. } => {
            }
        }
    }
}

fn run_decode(ir: &IrSchema, stmts: &[Stmt], env: &mut Env, cur: &mut Cursor<'_>) -> BpsResult<()> {
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, expr } => {
                let v = eval_expr(expr, env)?;
                env.insert(name, v);
            }
            Stmt::Require(expr) => {
                if !eval_expr(expr, env)?.truthy() {
                    return Err(crate::errors::decode(format!(
                        "require failed: {}",
                        expr.py_repr()
                    )));
                }
            }
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let branch = if eval_expr(cond, env)?.truthy() {
                    then_body
                } else {
                    else_body
                };
                run_decode(ir, branch, env, cur)?;
            }
            Stmt::Read { dst, op } => {
                let v = read_op(op, env, cur)?;
                env.insert(dst, v);
            }
            Stmt::CallDecode { dst, callee, args } => {
                let vals = eval_args(args, env)?;
                let v = exec_decode_at(ir, &callee.0, &vals, cur)?;
                env.insert(dst, v);
            }
            Stmt::Utf8 { dst, src } => {
                let raw = match env.get(src) {
                    Some(Value::Bytes(b)) => b.to_vec(),
                    Some(Value::Str(s)) => s.as_bytes().to_vec(),
                    _ => {
                        return Err(crate::errors::decode(format!("utf8 needs bytes in {src}")));
                    }
                };
                let s = std::str::from_utf8(&raw)
                    .map_err(|_| crate::errors::decode("bytes are not valid utf-8"))?;
                env.insert(dst, Value::str(s.to_string()));
            }
            other => {
                return Err(crate::errors::decode(format!(
                    "decode cannot run {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn run_encode(ir: &IrSchema, stmts: &[Stmt], env: &mut Env, w: &mut WriteCursor) -> BpsResult<()> {
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, expr } => {
                let v = eval_expr(expr, env)?;
                env.insert(name, v);
            }
            Stmt::Require(expr) => {
                if !eval_expr(expr, env)?.truthy() {
                    return Err(encode(format!("require failed: {}", expr.py_repr())));
                }
            }
            Stmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let branch = if eval_expr(cond, env)?.truthy() {
                    then_body
                } else {
                    else_body
                };
                run_encode(ir, branch, env, w)?;
            }
            Stmt::Write { op, value } => {
                let v = eval_expr(value, env)?;
                write_op(op, &v, env, w)?;
            }
            Stmt::CallEncode {
                callee,
                args,
                value,
            } => {
                let vals = eval_args(args, env)?;
                let v = eval_expr(value, env)?;
                let bytes = exec_encode(ir, &callee.0, &vals, &v)?;
                w.emit(&bytes);
            }
            other => return Err(encode(format!("encode cannot run {other:?}"))),
        }
    }
    Ok(())
}

fn eval_args(args: &[Expr], env: &Env) -> BpsResult<Vec<Value>> {
    args.iter().map(|a| eval_expr(a, env)).collect()
}

fn read_op(op: &WireOp, env: &Env, cur: &mut Cursor<'_>) -> BpsResult<Value> {
    match op {
        WireOp::U8 => Ok(Value::Int(i128::from(cur.read(1)?[0]))),
        WireOp::F32be => {
            let b = cur.read(4)?;
            let f = f32::from_be_bytes([b[0], b[1], b[2], b[3]]);
            Ok(Value::Float(f64::from(f)))
        }
        WireOp::F64be => {
            let b = cur.read(8)?;
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            Ok(Value::Float(f64::from_be_bytes(a)))
        }
        WireOp::Bytes { len } => {
            let n = usize_len(&eval_expr(len, env)?)?;
            Ok(Value::bytes(cur.read(n)?.to_vec()))
        }
        WireOp::Rest => Ok(Value::bytes(cur.read(cur.remaining())?.to_vec())),
    }
}

fn write_op(op: &WireOp, value: &Value, env: &Env, w: &mut WriteCursor) -> BpsResult<()> {
    match op {
        WireOp::U8 => {
            w.emit(&[(int_of(value)? & 0xFF) as u8]);
            Ok(())
        }
        WireOp::F32be => {
            let f = as_f64(value)?;
            w.emit(&(f as f32).to_be_bytes());
            Ok(())
        }
        WireOp::F64be => {
            w.emit(&as_f64(value)?.to_be_bytes());
            Ok(())
        }
        WireOp::Bytes { len } => {
            let n = usize_len(&eval_expr(len, env)?)?;
            w.emit(&fit_bytes(value, n)?);
            Ok(())
        }
        WireOp::Rest => {
            w.emit(&raw_bytes(value).ok_or_else(|| encode("rest needs bytes"))?);
            Ok(())
        }
    }
}

fn usize_len(v: &Value) -> BpsResult<usize> {
    let n = int_of(v)?;
    usize::try_from(n).map_err(|_| encode(format!("negative length {n}")))
}

fn as_f64(v: &Value) -> BpsResult<f64> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => Ok(*i as f64),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err(encode("need a number")),
    }
}

fn fit_bytes(value: &Value, n: usize) -> BpsResult<Vec<u8>> {
    let mut raw = raw_bytes(value).ok_or_else(|| encode("bytes need a byte string"))?;
    if raw.len() > n {
        return Err(encode(format!("bytes({n}) got {} bytes", raw.len())));
    }
    raw.resize(n, 0);
    Ok(raw)
}

fn raw_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Bytes(b) => Some(b.to_vec()),
        Value::Str(s) => Some(s.as_bytes().to_vec()),
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items.iter() {
                match it {
                    Value::Int(i) if (0..=255).contains(i) => out.push(*i as u8),
                    Value::Bool(b) => out.push(u8::from(*b)),
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Schema calls go to the interpreter. `utf8_len` is emitted by the lowerer for
/// string-shaped encodings: UTF-8 byte length, not Unicode scalars (`len`).
fn eval_expr(expr: &Expr, env: &Env) -> BpsResult<Value> {
    if let Expr::Call(name, args) = expr
        && name == "utf8_len"
        && args.len() == 1
    {
        let v = eval_expr(&args[0], env)?;
        let n = match &v {
            Value::Str(s) => s.len(),
            Value::Bytes(b) => b.len(),
            _ => return Err(encode("utf8_len needs a string")),
        };
        return Ok(Value::Int(n as i128));
    }
    crate::interp::expr::eval_expr(expr, env)
}
