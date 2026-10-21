//! Run an [`IrSchema`] procedure. Scalar procs only; records and repeats come later.

use crate::ast::Expr;
use crate::errors::{BmlResult, encode};
use crate::interp::cursor::{Cursor, WriteCursor};
use crate::interp::expr::int_of;
use crate::value::{Env, Value};

use super::schema::{IrSchema, Stmt, TypeDef, WireOp};

pub fn exec_decode(ir: &IrSchema, ty: &str, args: &[Value], bytes: &[u8]) -> BmlResult<Value> {
    let mut cur = Cursor::new(bytes);
    exec_decode_at(ir, ty, args, &mut cur)
}

pub fn exec_encode(ir: &IrSchema, ty: &str, args: &[Value], value: &Value) -> BmlResult<Vec<u8>> {
    let proc = ir
        .encode
        .get(ty)
        .ok_or_else(|| encode(format!("no encode proc for {ty}")))?;
    let mut env = Env::new();
    if matches!(ir.types.get(ty), Some(TypeDef::Scalar(_))) {
        prebind(&proc.body, &mut env);
    }
    bind_args(&proc.params, args, &mut env)?;
    env.insert("value", value.clone());
    if let Value::Map(fields) = value {
        for (k, v) in fields.iter() {
            env.insert(k, v.clone());
        }
    }
    let mut w = WriteCursor::new();
    run_encode(ir, &proc.body, &mut env, &mut w)?;
    Ok(w.result())
}

pub fn exec_decode_frame(
    ir: &IrSchema,
    state: &str,
    direction: &str,
    frame: &[u8],
) -> BmlResult<(i128, Value)> {
    let frame_name = ir
        .routes
        .iter()
        .find(|route| route.state == state && route.direction == direction)
        .map(|route| route.frame.as_str())
        .unwrap_or("default");
    let decoded = exec_decode(ir, &format!("frame_{frame_name}"), &[], frame)?;
    let Value::Map(fields) = decoded else {
        return Err(crate::errors::decode("frame did not decode to a record"));
    };
    let id = int_of(
        fields
            .get("id")
            .ok_or_else(|| crate::errors::decode("frame has no id"))?,
    )?;
    let payload = match fields.get("payload") {
        Some(Value::Bytes(bytes)) => bytes.as_ref().to_vec(),
        _ => return Err(crate::errors::decode("frame has no payload")),
    };
    let route = ir
        .routes
        .iter()
        .find(|route| route.state == state && route.direction == direction && route.packet_id == id)
        .ok_or_else(|| {
            crate::errors::decode(format!("no route for id {id} in {state}.{direction}"))
        })?;
    let value = exec_decode(ir, &route.packet.0, &[], &payload)?;
    Ok((id, value))
}

pub fn exec_encode_frame(
    ir: &IrSchema,
    state: &str,
    direction: &str,
    packet: &str,
    fields: &Env,
) -> BmlResult<Vec<u8>> {
    let route = ir
        .routes
        .iter()
        .find(|route| {
            route.state == state && route.direction == direction && route.packet.0 == packet
        })
        .ok_or_else(|| encode(format!("no route for {packet} in {state}.{direction}")))?;
    let payload = exec_encode(ir, packet, &[], &Value::map(fields.clone()))?;
    let id_bytes = exec_encode(ir, "varint", &[], &Value::Int(route.packet_id))?;
    let mut frame = Env::new();
    frame.insert(
        "length",
        Value::Int((id_bytes.len() + payload.len()) as i128),
    );
    frame.insert("id", Value::Int(route.packet_id));
    frame.insert("payload", Value::bytes(payload));
    exec_encode(
        ir,
        &format!("frame_{}", route.frame),
        &[],
        &Value::map(frame),
    )
}

fn exec_decode_at(
    ir: &IrSchema,
    ty: &str,
    args: &[Value],
    cur: &mut Cursor<'_>,
) -> BmlResult<Value> {
    let proc = ir
        .decode
        .get(ty)
        .ok_or_else(|| crate::errors::decode(format!("no decode proc for {ty}")))?;
    let mut env = Env::new();
    if matches!(ir.types.get(ty), Some(TypeDef::Scalar(_))) {
        prebind(&proc.body, &mut env);
    }
    bind_args(&proc.params, args, &mut env)?;
    let mut out = Env::new();
    run_decode(ir, &proc.body, &mut env, &mut out, cur)?;
    match &proc.result {
        Some(expr) => eval_expr(expr, &env),
        None => Ok(Value::map(out)),
    }
}

fn bind_args(params: &[String], args: &[Value], env: &mut Env) -> BmlResult<()> {
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
            Stmt::Block { body, .. } => prebind(body, env),
            Stmt::Require(_) | Stmt::Write { .. } | Stmt::Utf8 { .. } | Stmt::CallEncode { .. } => {
            }
        }
    }
}

fn bind_out(env: &mut Env, out: &mut Env, name: &str, v: Value) {
    env.insert(name, v.clone());
    out.insert(name, v);
}

fn run_decode(
    ir: &IrSchema,
    stmts: &[Stmt],
    env: &mut Env,
    out: &mut Env,
    cur: &mut Cursor<'_>,
) -> BmlResult<()> {
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, expr } => {
                let v = eval_expr(expr, env)?;
                bind_out(env, out, name, v);
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
                fallback,
            } => {
                let want = match eval_expr(cond, env) {
                    Ok(v) => v.truthy(),
                    Err(_) if *fallback => true,
                    Err(e) => return Err(e),
                };
                if *fallback && want && cur.remaining() == 0 {
                    continue;
                }
                let branch = if want { then_body } else { else_body };
                run_decode(ir, branch, env, out, cur)?;
            }

            Stmt::Read { dst, op } => {
                let v = read_op(op, env, cur)?;
                bind_out(env, out, dst, v);
            }
            Stmt::CallDecode { dst, callee, args } => {
                let vals = eval_args(args, env)?;
                let v = exec_decode_at(ir, &callee.0, &vals, cur)?;
                bind_out(env, out, dst, v);
            }
            Stmt::Utf8 { dst, src } => {
                let raw = match env.get(src) {
                    Some(Value::Bytes(b)) => b.to_vec(),
                    Some(Value::Str(s)) => s.as_bytes().to_vec(),
                    _ => return Err(crate::errors::decode(format!("utf8 needs bytes in {src}"))),
                };
                let s = std::str::from_utf8(&raw)
                    .map_err(|_| crate::errors::decode("bytes are not valid utf-8"))?;
                bind_out(env, out, dst, Value::str(s.to_string()));
            }
            Stmt::Match {
                scrutinee,
                arms,
                default,
            } => {
                let tag = int_of(&eval_expr(scrutinee, env)?)?;
                let body = arms
                    .iter()
                    .find(|arm| arm.tags.contains(&tag))
                    .map(|arm| &arm.body)
                    .or(default.as_ref())
                    .ok_or_else(|| crate::errors::decode(format!("no match arm for tag {tag}")))?;
                run_decode(ir, body, env, out, cur)?;
            }
            Stmt::RepeatCount { n, elem, dst } => {
                let count = usize_len(&eval_expr(n, env)?)?;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(decode_elem(ir, elem, env, cur)?);
                }
                bind_out(env, out, dst, Value::list(items));
            }
            Stmt::RepeatUntil { term, elem, dst } => {
                let term = int_of(&eval_expr(term, env)?)?;
                let mut items = Vec::new();
                loop {
                    if cur.remaining() == 0 {
                        break;
                    }
                    if i128::from(cur.peek_u8()?) == term {
                        cur.read(1)?;
                        break;
                    }
                    items.push(decode_elem(ir, elem, env, cur)?);
                }
                bind_out(env, out, dst, Value::list(items));
            }
            Stmt::RepeatUntilEnd { elem, dst } => {
                let mut items = Vec::new();
                while cur.remaining() > 0 {
                    items.push(decode_elem(ir, elem, env, cur)?);
                }
                bind_out(env, out, dst, Value::list(items));
            }
            Stmt::Block { body, dst } => {
                let mut child = env.clone();
                let mut produced = Env::new();
                run_decode(ir, body, &mut child, &mut produced, cur)?;
                bind_out(env, out, dst, Value::map(produced));
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

fn decode_elem(ir: &IrSchema, elem: &[Stmt], env: &Env, cur: &mut Cursor<'_>) -> BmlResult<Value> {
    let mut child = env.clone();
    let mut produced = Env::new();
    run_decode(ir, elem, &mut child, &mut produced, cur)?;
    if let Some(v) = produced.get("__elem") {
        return Ok(v.clone());
    }
    Ok(Value::map(produced))
}

fn run_encode(ir: &IrSchema, stmts: &[Stmt], env: &mut Env, w: &mut WriteCursor) -> BmlResult<()> {
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
                fallback,
            } => {
                let branch = match eval_expr(cond, env) {
                    Ok(v) if v.truthy() => then_body,
                    Ok(_) => else_body,
                    Err(_) if *fallback && stmt_names_present(then_body, env) => then_body,
                    Err(_) if *fallback => continue,
                    Err(e) => return Err(e),
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
            Stmt::Match {
                scrutinee,
                arms,
                default,
            } => {
                let tag = int_of(&eval_expr(scrutinee, env)?)?;
                let body = arms
                    .iter()
                    .find(|arm| arm.tags.contains(&tag))
                    .map(|arm| &arm.body)
                    .or(default.as_ref())
                    .ok_or_else(|| encode(format!("no match arm for tag {tag}")))?;
                run_encode(ir, body, env, w)?;
            }
            Stmt::RepeatCount { elem, dst, .. } => encode_list(ir, elem, env, w, dst)?,
            Stmt::RepeatUntil { term, elem, dst } => {
                encode_list(ir, elem, env, w, dst)?;
                let term = int_of(&eval_expr(term, env)?)?;
                w.emit(&[(term & 0xFF) as u8]);
            }
            Stmt::RepeatUntilEnd { .. } => {
                return Err(encode("encode not supported for repeat mode until_end"));
            }
            Stmt::Block { body, dst } => {
                let mut child = env.clone();
                if let Some(Value::Map(fields)) = env.get(dst) {
                    for (key, val) in fields.iter() {
                        child.insert(key, val.clone());
                    }
                }
                run_encode(ir, body, &mut child, w)?;
            }
            Stmt::Read { .. } | Stmt::Utf8 { .. } | Stmt::CallDecode { .. } => {
                return Err(encode(format!("encode cannot run {stmt:?}")));
            }
        }
    }
    Ok(())
}

fn encode_list(
    ir: &IrSchema,
    elem: &[Stmt],
    env: &mut Env,
    w: &mut WriteCursor,
    dst: &str,
) -> BmlResult<()> {
    let list = env
        .get(dst)
        .cloned()
        .unwrap_or_else(|| Value::list(Vec::new()));
    let Value::List(items) = list else {
        return Err(encode(format!("expected list for {dst}")));
    };
    for item in items.iter() {
        env.insert("__elem", item.clone());
        run_encode(ir, elem, env, w)?;
    }
    Ok(())
}

fn stmt_names_present(stmts: &[Stmt], env: &Env) -> bool {
    fn walk(stmts: &[Stmt], env: &Env) -> bool {
        for stmt in stmts {
            let name = match stmt {
                Stmt::Let { name, .. }
                | Stmt::Read { dst: name, .. }
                | Stmt::CallDecode { dst: name, .. }
                | Stmt::RepeatCount { dst: name, .. }
                | Stmt::RepeatUntil { dst: name, .. }
                | Stmt::RepeatUntilEnd { dst: name, .. } => Some(name.as_str()),
                Stmt::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    if walk(then_body, env) || walk(else_body, env) {
                        return true;
                    }
                    None
                }
                Stmt::Match { arms, default, .. } => {
                    if arms.iter().any(|arm| walk(&arm.body, env))
                        || default.as_ref().is_some_and(|body| walk(body, env))
                    {
                        return true;
                    }
                    None
                }
                _ => None,
            };
            if name.is_some_and(|n| env.contains_key(n)) {
                return true;
            }
        }
        false
    }
    walk(stmts, env)
}

fn eval_args(args: &[Expr], env: &Env) -> BmlResult<Vec<Value>> {
    args.iter().map(|a| eval_expr(a, env)).collect()
}

fn read_op(op: &WireOp, env: &Env, cur: &mut Cursor<'_>) -> BmlResult<Value> {
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

fn write_op(op: &WireOp, value: &Value, env: &Env, w: &mut WriteCursor) -> BmlResult<()> {
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

fn usize_len(v: &Value) -> BmlResult<usize> {
    let n = int_of(v)?;
    usize::try_from(n).map_err(|_| encode(format!("negative length {n}")))
}

fn as_f64(v: &Value) -> BmlResult<f64> {
    match v {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => Ok(*i as f64),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err(encode("need a number")),
    }
}

fn fit_bytes(value: &Value, n: usize) -> BmlResult<Vec<u8>> {
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
fn eval_expr(expr: &Expr, env: &Env) -> BmlResult<Value> {
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
