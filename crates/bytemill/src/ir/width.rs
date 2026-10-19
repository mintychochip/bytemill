//! Storage class of a scalar yield. Spec section 4.3.
//!
//! The class is the public type. Expression evaluation stays `i128`.
//!
//! Besides `sign_extend(_, N)` and byte-assembly trees, an interval rule
//! covers computed fields like `flags & 3`: `&` with a non-negative constant
//! mask, `>>` by a constant, `%` by a positive constant, `|` of bounded
//! operands, and literals, using the operand classes from `locals`. The
//! result is the smallest unsigned class when the interval is non-negative,
//! else the smallest signed class, else `i128`. Unknown names (a field bound
//! later, a parameter) make the interval unbounded, so the rule only ever
//! shrinks what it can prove.

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
    F32,
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
    if let Some((lo, hi)) = interval(expr, locals) {
        return class_for_interval(lo, hi);
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

/// Smallest class holding `[lo, hi]`: unsigned when non-negative, else signed.
fn class_for_interval(lo: i128, hi: i128) -> Storage {
    if lo >= 0 {
        if hi <= 0xFF {
            return Storage::U8;
        }
        if hi <= 0xFFFF {
            return Storage::U16;
        }
        if hi <= 0xFFFF_FFFF {
            return Storage::U32;
        }
        if hi <= 0xFFFF_FFFF_FFFF_FFFF {
            return Storage::U64;
        }
        return Storage::I128;
    }
    if lo >= -128 && hi <= 127 {
        return Storage::I8;
    }
    if lo >= -32_768 && hi <= 32_767 {
        return Storage::I16;
    }
    if lo >= -2_147_483_648 && hi <= 2_147_483_647 {
        return Storage::I32;
    }
    if lo >= -9_223_372_036_854_775_808 && hi <= 9_223_372_036_854_775_807 {
        return Storage::I64;
    }
    Storage::I128
}

/// Exact range of a storage class: what an already-typed local can hold.
fn class_range(class: Storage) -> Option<(i128, i128)> {
    match class {
        Storage::I8 => Some((-128, 127)),
        Storage::I16 => Some((-32_768, 32_767)),
        Storage::I32 => Some((-2_147_483_648, 2_147_483_647)),
        Storage::I64 => Some((-9_223_372_036_854_775_808, 9_223_372_036_854_775_807)),
        Storage::I128 => None,
        Storage::U8 => Some((0, 0xFF)),
        Storage::U16 => Some((0, 0xFFFF)),
        Storage::U32 => Some((0, 0xFFFF_FFFF)),
        Storage::U64 => Some((0, 0xFFFF_FFFF_FFFF_FFFF)),
        Storage::Bool => Some((0, 1)),
        Storage::F32 | Storage::F64 | Storage::Str | Storage::Bytes => None,
    }
}

/// Sound `[lo, hi]` bound for integer expressions, or `None` when unbounded.
/// Arithmetic is the interpreter's: `/` floors, `%` takes the divisor's sign,
/// `>>` is arithmetic, shifts saturate like the interpreter's `py_shl`.
fn interval(expr: &Expr, locals: &HashMap<String, Storage>) -> Option<(i128, i128)> {
    match expr {
        Expr::Int(v) => Some((*v, *v)),
        Expr::Bool(_) => Some((0, 1)),
        Expr::Str(_) => None,
        Expr::Ident(name) => locals.get(name).copied().and_then(class_range),
        Expr::Path(_) => None,
        Expr::Unop(_, _) => None,
        Expr::Ternary(_, _, _) => None,
        Expr::Call(_, _) => None,
        Expr::Binop(op, left, right) => interval_binop(op, left, right, locals),
    }
}

fn interval_binop(
    op: &str,
    left: &Expr,
    right: &Expr,
    locals: &HashMap<String, Storage>,
) -> Option<(i128, i128)> {
    // Mask and remainder rules need only the constant; the other side may be
    // any integer (even an unknown name).
    if op == "&" {
        if let Expr::Int(m) = right
            && *m >= 0
        {
            return Some((0, *m));
        }
        if let Expr::Int(m) = left
            && *m >= 0
        {
            return Some((0, *m));
        }
    }
    if op == "%"
        && let Expr::Int(p) = right
    {
        if *p > 0 {
            return Some((0, p - 1));
        }
        if *p < 0 {
            return Some((p + 1, 0));
        }
        return None;
    }
    if op == ">>"
        && let Expr::Int(k) = right
        && *k >= 0
        && *k < 128
    {
        let (lo, hi) = interval(left, locals)?;
        let k = *k as u32;
        return Some((lo >> k, hi >> k));
    }
    let (alo, ahi) = interval(left, locals)?;
    let (blo, bhi) = interval(right, locals)?;
    match op {
        "|" => {
            if alo < 0 || blo < 0 {
                return None;
            }
            Some((0, bit_fill(ahi.max(bhi))))
        }
        "<<" => {
            if blo != bhi || !(0..128).contains(&blo) {
                return None;
            }
            let k = blo as u32;
            // Shifting a non-negative value left stays non-negative; a
            // negative one can only be bounded when it stays in range.
            let lo = shl_sat(alo, k)?;
            let hi = shl_sat(ahi, k)?;
            if alo < 0 && lo >= 0 {
                return None;
            }
            Some((lo.min(hi), lo.max(hi)))
        }
        "+" => Some((alo.checked_add(blo)?, ahi.checked_add(bhi)?)),
        "-" => Some((alo.checked_sub(bhi)?, ahi.checked_sub(blo)?)),
        "*" => {
            let ps = [
                alo.checked_mul(blo)?,
                alo.checked_mul(bhi)?,
                ahi.checked_mul(blo)?,
                ahi.checked_mul(bhi)?,
            ];
            Some((ps.into_iter().min()?, ps.into_iter().max()?))
        }
        "/" => {
            if blo <= 0 && bhi >= 0 {
                return None;
            }
            let ps = [
                floor_div(alo, blo)?,
                floor_div(alo, bhi)?,
                floor_div(ahi, blo)?,
                floor_div(ahi, bhi)?,
            ];
            Some((ps.into_iter().min()?, ps.into_iter().max()?))
        }
        _ => None,
    }
}

/// Smallest `2^n - 1` covering `v` (`v >= 0`); saturates at `i128::MAX`.
fn bit_fill(v: i128) -> i128 {
    if v <= 0 {
        return 0;
    }
    let bits = 128 - (v as u128).leading_zeros();
    if bits >= 127 {
        return i128::MAX;
    }
    (1i128 << bits) - 1
}

fn shl_sat(v: i128, k: u32) -> Option<i128> {
    if k >= 128 {
        return if v == 0 { Some(0) } else { None };
    }
    // The interpreter saturates beyond i128; saturation is unbounded here.
    v.checked_shl(k)
}

fn floor_div(a: i128, b: i128) -> Option<i128> {
    if b == 0 {
        return None;
    }
    let q = a / b;
    let r = a % b;
    if r != 0 && (r < 0) != (b < 0) {
        Some(q - 1)
    } else {
        Some(q)
    }
}
