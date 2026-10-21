//! Expression evaluation, porting `bps/interp/evalexpr.py`.

use crate::ast::Expr;
use crate::errors::{BmlError, BmlResult, decode, py_repr_str};
use crate::value::{Env, Value};

pub fn eval_expr(expr: &Expr, env: &Env) -> BmlResult<Value> {
    match expr {
        Expr::Int(v) => Ok(Value::Int(*v)),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Str(s) => Ok(Value::str(s.clone())),
        Expr::Ident(name) => env
            .get(name)
            .cloned()
            .ok_or_else(|| decode(format!("unknown identifier {}", py_repr_str(name)))),
        Expr::Path(dotted) => {
            let mut cur: Value = Value::Null;
            for (i, part) in dotted.split('.').enumerate() {
                if i == 0 {
                    cur = env.get(part).cloned().ok_or_else(|| {
                        decode(format!("unknown identifier {}", py_repr_str(part)))
                    })?;
                    continue;
                }
                match &cur {
                    Value::Map(m) => {
                        cur = m
                            .get(part)
                            .cloned()
                            .ok_or_else(|| decode(format!("bad path {}", py_repr_str(dotted))))?;
                    }
                    _ => return Err(decode(format!("bad path {}", py_repr_str(dotted)))),
                }
            }
            Ok(cur)
        }
        Expr::Unop(op, inner) => {
            if op == "!" {
                let v = eval_expr(inner, env)?;
                Ok(Value::Bool(!v.truthy()))
            } else {
                Err(decode(format!("unknown unop {op}")))
            }
        }
        Expr::Binop(op, left, right) => {
            // Python evaluates both operands before dispatching: no short circuit.
            let l = eval_expr(left, env)?;
            let r = eval_expr(right, env)?;
            match op.as_str() {
                "||" => Ok(Value::Bool(l.truthy() || r.truthy())),
                "&&" => Ok(Value::Bool(l.truthy() && r.truthy())),
                "==" => Ok(Value::Bool(l.py_eq(&r))),
                "!=" => Ok(Value::Bool(!l.py_eq(&r))),
                "<" => Ok(Value::Bool(cmp_op(&l, &r, |o| o.is_lt())?)),
                "<=" => Ok(Value::Bool(cmp_op(&l, &r, |o| o.is_le())?)),
                ">" => Ok(Value::Bool(cmp_op(&l, &r, |o| o.is_gt())?)),
                ">=" => Ok(Value::Bool(cmp_op(&l, &r, |o| o.is_ge())?)),
                "+" => Ok(Value::Int(int_of(&l)? + int_of(&r)?)),
                "-" => Ok(Value::Int(int_of(&l)? - int_of(&r)?)),
                "*" => Ok(Value::Int(int_of(&l)? * int_of(&r)?)),
                "/" => Ok(Value::Int(floor_div(int_of(&l)?, int_of(&r)?)?)),
                "%" => Ok(Value::Int(py_mod(int_of(&l)?, int_of(&r)?)?)),
                "<<" => Ok(Value::Int(py_shl(int_of(&l)?, int_of(&r)?))),
                ">>" => Ok(Value::Int(py_shr(int_of(&l)?, int_of(&r)?))),
                "&" => Ok(Value::Int(int_of(&l)? & int_of(&r)?)),
                "|" => Ok(Value::Int(int_of(&l)? | int_of(&r)?)),
                "^" => Ok(Value::Int(int_of(&l)? ^ int_of(&r)?)),
                _ => Err(decode(format!("unknown binop {op}"))),
            }
        }
        Expr::Ternary(cond, yes, no) => {
            let c = eval_expr(cond, env)?;
            if c.truthy() {
                eval_expr(yes, env)
            } else {
                eval_expr(no, env)
            }
        }
        Expr::Call(name, args) => {
            let vals: Vec<Value> = args
                .iter()
                .map(|a| eval_expr(a, env))
                .collect::<BmlResult<_>>()?;
            eval_call(name, &vals)
        }
    }
}

fn cmp_op(l: &Value, r: &Value, pick: fn(std::cmp::Ordering) -> bool) -> BmlResult<bool> {
    let a = int_of(l)?;
    let b = int_of(r)?;
    Ok(pick(a.cmp(&b)))
}

/// Python `int(v)`.
pub fn int_of(v: &Value) -> BmlResult<i128> {
    match v {
        Value::Int(i) => Ok(*i),
        Value::Bool(b) => Ok(i128::from(*b)),
        Value::Float(f) => crate::value::float_to_int(*f).map_err(decode),
        _ => Err(decode(format!("cannot convert {} to int", v.type_name()))),
    }
}

pub fn truthy(v: &Value) -> bool {
    v.truthy()
}

/// Python `//`: floor division (rounds toward negative infinity).
fn floor_div(a: i128, b: i128) -> BmlResult<i128> {
    if b == 0 {
        return Err(decode("integer division or modulo by zero"));
    }
    let q = a / b;
    let r = a % b;
    Ok(if r != 0 && ((r < 0) != (b < 0)) {
        q - 1
    } else {
        q
    })
}

/// Python `%`: the result takes the divisor's sign.
fn py_mod(a: i128, b: i128) -> BmlResult<i128> {
    if b == 0 {
        return Err(decode("integer division or modulo by zero"));
    }
    let r = a % b;
    Ok(if r != 0 && ((r < 0) != (b < 0)) {
        r + b
    } else {
        r
    })
}

/// Python `<<` with unbounded integers; saturates beyond i128 (never reached by
/// the schemas, whose shifts are bounded by u64 field widths).
fn py_shl(a: i128, n: i128) -> i128 {
    if n <= 0 {
        return if n == 0 { a } else { py_shr(a, -n) };
    }
    if n >= 127 {
        return if a == 0 {
            0
        } else if a > 0 {
            i128::MAX
        } else {
            i128::MIN
        };
    }
    a.wrapping_shl(n as u32)
}

/// Python `>>` with unbounded integers.
fn py_shr(a: i128, n: i128) -> i128 {
    if n <= 0 {
        return if n == 0 { a } else { py_shl(a, -n) };
    }
    if n >= 127 {
        return if a < 0 { -1 } else { 0 };
    }
    a >> n
}

fn eval_call(name: &str, args: &[Value]) -> BmlResult<Value> {
    match name {
        "sign_extend" => {
            let v = int_of(&args[0])?;
            let bits = int_of(&args[1])?;
            if bits <= 0 || bits > 128 {
                return Err(decode(format!("sign_extend bits out of range: {bits}")));
            }
            let mask: i128 = 1i128 << (bits as u32 - 1);
            Ok(Value::Int(if v & mask != 0 {
                (v ^ mask) - mask
            } else {
                v
            }))
        }
        "utf16_units" => {
            let s = as_str(&args[0])?;
            let units: usize = s
                .chars()
                .map(|c| if (c as u32) > 0xffff { 2 } else { 1 })
                .sum();
            Ok(Value::Int(units as i128))
        }
        "len" => {
            let n = match &args[0] {
                Value::Bytes(b) => b.len(),
                Value::Str(s) => s.chars().count(),
                Value::List(l) => l.len(),
                Value::Map(m) => m.len(),
                _ => return Err(decode("len() on unsupported type")),
            };
            Ok(Value::Int(n as i128))
        }
        "min" => Ok(Value::Int(int_of(&args[0])?.min(int_of(&args[1])?))),
        "max" => Ok(Value::Int(int_of(&args[0])?.max(int_of(&args[1])?))),
        "contains" => {
            let hay = as_str(&args[0])?;
            let needle = as_str(&args[1])?;
            Ok(Value::Bool(hay.contains(&needle)))
        }
        _ => Err(decode(format!("unknown call {}", py_repr_str(name)))),
    }
}

/// Python `str(v)` for the builtins that coerce.
fn as_str(v: &Value) -> BmlResult<String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Bool(b) => Ok(if *b { "True".into() } else { "False".into() }),
        Value::Float(f) => Ok(crate::json::float_repr(*f)),
        Value::Null => Ok("None".into()),
        Value::Bytes(b) => Ok(String::from_utf8_lossy(b).into_owned()),
        Value::List(_) | Value::Map(_) => Err(BmlError::new(
            crate::errors::ErrorKind::Decode,
            "str() on container",
        )),
    }
}
