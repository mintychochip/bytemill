//! Storage class of a scalar yield. Spec section 4.3.
//!
//! The class is the public type. Expression evaluation stays `i128`.

use std::collections::HashMap;

use crate::ast::Expr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Storage {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    Bool,
    F64,
    Str,
    Bytes,
}
pub fn storage_of(expr: &Expr, locals: &HashMap<String, Storage>) -> Storage {
    if matches!(expr, Expr::Bool(_)) {
        return Storage::Bool;
    }
    if is_comparison(expr) {
        return Storage::Bool;
    }
    if let Some(bits) = sign_extend_bits(expr) {
        return signed_class(bits);
    }
    if let Some(width) = unsigned_width(expr, locals) {
        return unsigned_class(width);
    }
    Storage::I128
}

fn signed_class(bits: i128) -> Storage {
    match bits {
        ..=8 => Storage::I8,
        9..=16 => Storage::I16,
        17..=32 => Storage::I32,
        33..=64 => Storage::I64,
        _ => Storage::I128,
    }
}

fn unsigned_class(width: u32) -> Storage {
    match width {
        8 => Storage::U8,
        16 => Storage::U16,
        32 => Storage::U32,
        64 => Storage::U64,
        _ => Storage::I128,
    }
}

fn is_comparison(expr: &Expr) -> bool {
    match expr {
        Expr::Binop(op, _, _) => matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">="),
        _ => false,
    }
}

/// `sign_extend(_, N)` with `N` an integer literal. A non-constant `N` is not a match.
fn sign_extend_bits(expr: &Expr) -> Option<i128> {
    let Expr::Call(name, args) = expr else {
        return None;
    };
    if name != "sign_extend" || args.len() != 2 {
        return None;
    }
    match &args[1] {
        Expr::Int(n) => Some(*n),
        _ => None,
    }
}

/// Width in bits of an OR-tree of `u8` values shifted by a constant multiple of 8.
fn unsigned_width(expr: &Expr, locals: &HashMap<String, Storage>) -> Option<u32> {
    match expr {
        Expr::Binop(op, left, right) if op == "|" => {
            let a = unsigned_width(left, locals)?;
            let b = unsigned_width(right, locals)?;
            Some(a.max(b))
        }
        other => {
            let shift = assembly_shift(other, locals)?;
            Some(shift + 8)
        }
    }
}

/// Shift amount of one assembled byte, or `None` if `expr` is not one.
fn assembly_shift(expr: &Expr, locals: &HashMap<String, Storage>) -> Option<u32> {
    match expr {
        Expr::Ident(name) if locals.get(name) == Some(&Storage::U8) => Some(0),
        Expr::Binop(op, left, right) if op == "<<" => {
            let Expr::Int(n) = right.as_ref() else {
                return None;
            };
            if *n < 0 || n % 8 != 0 || *n > u32::MAX as i128 {
                return None;
            }
            if !is_u8(left, locals) {
                return None;
            }
            Some(*n as u32)
        }
        Expr::Binop(op, left, right) if op == "&" => {
            if matches!(right.as_ref(), Expr::Int(_)) {
                assembly_shift(left, locals)
            } else if matches!(left.as_ref(), Expr::Int(_)) {
                assembly_shift(right, locals)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_u8(expr: &Expr, locals: &HashMap<String, Storage>) -> bool {
    match expr {
        Expr::Ident(name) => locals.get(name) == Some(&Storage::U8),
        Expr::Binop(op, left, right) if op == "&" => {
            if matches!(right.as_ref(), Expr::Int(_)) {
                is_u8(left, locals)
            } else if matches!(left.as_ref(), Expr::Int(_)) {
                is_u8(right, locals)
            } else {
                false
            }
        }
        _ => false,
    }
}
