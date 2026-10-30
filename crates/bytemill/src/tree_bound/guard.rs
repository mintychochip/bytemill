//! Rust conditions for BML guards over a scope's wire locals.

use crate::ast::Expr;
use crate::emit_rstom::ident;
use crate::errors::{BmlResult, emit};
use crate::ordmap::OrderedMap;
use std::collections::BTreeSet;

/// Locals that would shadow something a generated body uses.
const RESERVED: &[&str] = &["value", "buffer", "v", "elem"];

fn numbered(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

/// The local a wire field's value is held in.
pub fn local(wire_ident: &str) -> String {
    if RESERVED.contains(&wire_ident) || numbered(wire_ident, "it") || numbered(wire_ident, "slot")
    {
        format!("{wire_ident}_w")
    } else {
        wire_ident.to_string()
    }
}

/// The type of each wire local in scope: its Rust type and whether the local is an `Option`.
#[derive(Debug, Clone, Default)]
pub struct Types(pub OrderedMap<(String, bool)>);

impl Types {
    pub fn insert(&mut self, wire_ident: &str, rtype: &str, optional: bool) {
        self.0.insert(wire_ident, (rtype.to_string(), optional));
    }

    fn get(&self, name: &str) -> (String, bool) {
        self.0
            .get(&ident(name))
            .cloned()
            .unwrap_or_else(|| ("i32".to_string(), false))
    }
}

fn access(name: &str, types: &Types) -> String {
    let (inner, optional) = types.get(name);
    let local = local(&ident(name));
    if !optional {
        // A mask literal over a narrow wire type must fit the type, so the operand widens.
        if matches!(inner.as_str(), "i8" | "u8" | "i16" | "u16") {
            return format!("({local} as i32)");
        }
        return local;
    }
    if inner == "bool" {
        return format!("{local}.unwrap_or(false)");
    }
    if matches!(
        inner.as_str(),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64"
    ) {
        return format!("{local}.unwrap_or(0)");
    }
    format!("{local}.clone().unwrap_or_default()")
}

fn operand(expr: &Expr, types: &Types) -> BmlResult<String> {
    match expr {
        Expr::Int(i) => Ok(i.to_string()),
        Expr::Bool(b) => Ok(b.to_string()),
        Expr::Ident(n) if !n.is_empty() => Ok(access(n, types)),
        Expr::Binop(op, l, r) => Ok(format!(
            "({} {op} {})",
            operand(l, types)?,
            operand(r, types)?
        )),
        Expr::Unop(op, inner) if op == "!" => Ok(format!("!({})", operand(inner, types)?)),
        _ => Err(emit(format!("unsupported guard operand: {expr:?}"))),
    }
}

fn guard(expr: &Expr, types: &Types) -> BmlResult<String> {
    let truthy = |name: &str| -> String {
        let (inner, _) = types.get(name);
        let a = access(name, types);
        if inner == "bool" {
            a
        } else {
            format!("({a} != 0)")
        }
    };
    match expr {
        Expr::Int(i) => Ok(i.to_string()),
        Expr::Bool(b) => Ok(b.to_string()),
        Expr::Ident(n) if !n.is_empty() => Ok(truthy(n)),
        Expr::Unop(op, inner) if op == "!" => Ok(format!("!({})", guard(inner, types)?)),
        Expr::Binop(op, l, r) => {
            if op == "&&" || op == "||" {
                return Ok(format!("({} {op} {})", guard(l, types)?, guard(r, types)?));
            }
            if matches!(op.as_str(), "==" | "!=")
                && matches!(&**r, Expr::Int(0))
                && let Expr::Ident(n) = &**l
                && types.get(n).0 == "bool"
            {
                let a = access(n, types);
                return Ok(if op == "!=" { a } else { format!("!{a}") });
            }
            Ok(format!(
                "({} {op} {})",
                operand(l, types)?,
                operand(r, types)?
            ))
        }
        _ => Err(emit(format!("unsupported guard expression: {expr:?}"))),
    }
}

/// A guard as an `if` condition, without the wrapping parentheses.
pub fn cond(expr: &Expr, types: &Types) -> BmlResult<String> {
    let text = guard(expr, types)?;
    Ok(strip_parens(&text))
}

fn strip_parens(text: &str) -> String {
    if !text.starts_with('(') {
        return text.to_string();
    }
    let mut depth = 0i32;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return if i == text.len() - 1 {
                        text[1..text.len() - 1].to_string()
                    } else {
                        text.to_string()
                    };
                }
            }
            _ => {}
        }
    }
    text.to_string()
}

/// The idents `expr` reads.
pub fn idents(expr: &Expr, out: &mut BTreeSet<String>) {
    match expr {
        Expr::Ident(n) => {
            out.insert(ident(n));
        }
        Expr::Unop(_, a) => idents(a, out),
        Expr::Binop(_, l, r) => {
            idents(l, out);
            idents(r, out);
        }
        Expr::Ternary(a, b, c) => {
            idents(a, out);
            idents(b, out);
            idents(c, out);
        }
        Expr::Call(_, args) => args.iter().for_each(|a| idents(a, out)),
        _ => {}
    }
}

/// A guard in infix text, for listings.
pub fn expr_text(expr: &Expr) -> String {
    match expr {
        Expr::Int(i) => i.to_string(),
        Expr::Bool(b) => b.to_string(),
        Expr::Str(s) => format!("{s:?}"),
        Expr::Ident(n) | Expr::Path(n) => n.clone(),
        Expr::Unop(op, a) => format!("{op}{}", expr_text(a)),
        Expr::Binop(op, l, r) => format!("({} {op} {})", expr_text(l), expr_text(r)),
        Expr::Ternary(a, b, c) => {
            format!("({} ? {} : {})", expr_text(a), expr_text(b), expr_text(c))
        }
        Expr::Call(n, args) => format!(
            "{n}({})",
            args.iter().map(expr_text).collect::<Vec<_>>().join(", ")
        ),
    }
}
