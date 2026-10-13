//! Interpreter/JSON values, the Rust counterpart of the Python `Any` that flows
//! through `bps/interp` and `json`.
//!
//! `Int` is `i128`: varint/varlong decoding and the `u64be`/`u64` positions in
//! the schemas exceed `i64::MAX`, and Python's expression arithmetic is
//! arbitrary-precision.

use crate::ordmap::OrderedMap;
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    List(Rc<Vec<Value>>),
    Map(Rc<OrderedMap<Value>>),
    Null,
}

/// Python `int(f)` for a float: truncates toward zero.
///
/// NaN and the infinities raise in Python; the saturating `as` cast would turn them into `0` and
/// `i128::MAX`, which would then reach the wire as valid-looking bytes. Python's `int` is also
/// unbounded, and a double at or above 2^127 does not fit `i128`: saturating there would give
/// `i128::MAX` (low bits all ones) where Python's integer, a multiple of 2^75, has them all zero.
/// Such a value keeps its low 128 bits instead, which is all a width-masked encode can see.
pub fn float_to_int(f: f64) -> Result<i128, String> {
    const I128_LIMIT: f64 = (1u128 << 127) as f64;
    if f.is_nan() {
        return Err("cannot convert float NaN to integer".to_string());
    }
    if f.is_infinite() {
        return Err("cannot convert float infinity to integer".to_string());
    }
    let truncated = f.trunc();
    if truncated.abs() < I128_LIMIT {
        return Ok(truncated as i128); // exact: every double below 2^127 is an integer that fits
    }
    let bits = truncated.abs().to_bits();
    let mantissa = u128::from((bits & ((1u64 << 52) - 1)) | (1u64 << 52));
    let shift = ((bits >> 52) & 0x7ff) as i32 - 1075; // value = mantissa * 2^shift, shift >= 75
    let low = if shift >= 128 { 0 } else { mantissa << shift };
    let value = low as i128;
    Ok(if truncated < 0.0 { value.wrapping_neg() } else { value })
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::from(s.into()))
    }

    pub fn bytes(b: Vec<u8>) -> Value {
        Value::Bytes(Rc::from(b.into_boxed_slice()))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }

    pub fn map(m: OrderedMap<Value>) -> Value {
        Value::Map(Rc::new(m))
    }

    /// Truthiness: Python `bool(v)`.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Bool(b) => *b,
            Value::Str(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::List(l) => !l.is_empty(),
            Value::Map(m) => !m.is_empty(),
            Value::Null => false,
        }
    }

    /// Python `int(v)`: bools map to 0/1, floats truncate toward zero.
    pub fn as_int(&self) -> Result<i128, String> {
        match self {
            Value::Int(i) => Ok(*i),
            Value::Bool(b) => Ok(i128::from(*b)),
            Value::Float(f) => float_to_int(*f),
            _ => Err(format!("cannot convert {} to int", self.type_name())),
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(i128::from(*b) as f64),
            _ => None,
        }
    }

    /// JSON-object style access: the field of a map, else `None`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Map(m) => m.get(key),
            _ => None,
        }
    }

    /// Python's type name for the value, used in error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::Bytes(_) => "bytes",
            Value::List(_) => "list",
            Value::Map(_) => "dict",
            Value::Null => "NoneType",
        }
    }

    /// Python `==` across the numeric tower: `1 == 1.0` and `True == 1`.
    pub fn py_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::Null, Value::Null) => true,
            (Value::List(a), Value::List(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.py_eq(y))
            }
            (Value::Map(a), Value::Map(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.py_eq(w)))
            }
            (
                Value::Int(_) | Value::Float(_) | Value::Bool(_),
                Value::Int(_) | Value::Float(_) | Value::Bool(_),
            ) => {
                // Python compares int and float exactly (1 == 1.0, and a huge int
                // never equals a float it merely rounds to).
                let a = self.as_int();
                let b = other.as_int();
                match (a, b) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => match (self.as_f64(), other.as_f64()) {
                        (Some(a), Some(b)) => a == b,
                        _ => false,
                    },
                }
            }
            _ => false,
        }
    }

    /// Python evalexpr compares with `int(left) < int(right)` — both sides
    /// truncate toward zero first.
    pub fn py_cmp(&self, other: &Value) -> Option<std::cmp::Ordering> {
        let a = self.as_int().ok()?;
        let b = other.as_int().ok()?;
        Some(a.cmp(&b))
    }
}

/// The decode/encode environment: names bound while walking the wire.
pub type Env = OrderedMap<Value>;

/// `{**env, name: value}` without cloning when the key is already last.
pub fn env_with(env: &Env, name: &str, value: Value) -> Env {
    let mut out = env.clone();
    out.insert(name, value);
    out
}
