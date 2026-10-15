//! `emit c`: a `.h` and `.c` whose encode and decode functions are the IR.
//!
//! Scalar values are `int64_t`. Float scalars are the IEEE bits (f32 in the low
//! 32 bits, f64 in all 64). A `bps_alloc` is part of every signature; integer
//! and float scalars do not allocate.
use std::fs;
use std::path::Path;

use crate::ast::Expr;
use crate::errors::{emit, BpsResult};
use crate::ir::{lower, Stmt, TypeDef, WireOp};
use crate::ResolvedSchema;

const HEADER: &str = r#"#ifndef BPSGEN_H
#define BPSGEN_H
#include <stddef.h>
#include <stdint.h>
typedef struct { const uint8_t *data; size_t len; size_t pos; } bps_cursor;
typedef struct { uint8_t *data; size_t len; size_t cap; } bps_writer;
typedef struct {
    void *(*malloc)(size_t, void *);
    void (*free)(void *, void *);
    void *ctx;
} bps_alloc;
int bps_cursor_u8(bps_cursor *c, int64_t *out);
int bps_cursor_f32(bps_cursor *c, int64_t *out);
int bps_cursor_f64(bps_cursor *c, int64_t *out);
int bps_writer_u8(bps_writer *w, bps_alloc *a, int64_t v);
int bps_writer_f32(bps_writer *w, bps_alloc *a, int64_t v);
int bps_writer_f64(bps_writer *w, bps_alloc *a, int64_t v);
int64_t bps_sign_extend(int64_t v, int bits);
#endif
"#;

const PRELUDE: &str = r#"#include "bpsgen.h"
#include <string.h>
int bps_cursor_u8(bps_cursor *c, int64_t *out) {
    if (c->pos >= c->len) return 1;
    *out = c->data[c->pos++];
    return 0;
}
int bps_writer_u8(bps_writer *w, bps_alloc *a, int64_t v) {
    (void)a;
    if (w->len >= w->cap) return 1;
    w->data[w->len++] = (uint8_t)(v & 0xFF);
    return 0;
}
int bps_cursor_f32(bps_cursor *c, int64_t *out) {
    if (c->pos + 4 > c->len) return 1;
    uint32_t u = ((uint32_t)c->data[c->pos] << 24) | ((uint32_t)c->data[c->pos + 1] << 16)
        | ((uint32_t)c->data[c->pos + 2] << 8) | (uint32_t)c->data[c->pos + 3];
    c->pos += 4;
    *out = (int64_t)u;
    return 0;
}
int bps_cursor_f64(bps_cursor *c, int64_t *out) {
    if (c->pos + 8 > c->len) return 1;
    uint64_t u = 0;
    for (int i = 0; i < 8; i++) u = (u << 8) | c->data[c->pos++];
    memcpy(out, &u, sizeof u);
    return 0;
}
int bps_writer_f32(bps_writer *w, bps_alloc *a, int64_t v) {
    (void)a;
    if (w->len + 4 > w->cap) return 1;
    uint32_t u = (uint32_t)v;
    w->data[w->len++] = (uint8_t)(u >> 24);
    w->data[w->len++] = (uint8_t)(u >> 16);
    w->data[w->len++] = (uint8_t)(u >> 8);
    w->data[w->len++] = (uint8_t)u;
    return 0;
}
int bps_writer_f64(bps_writer *w, bps_alloc *a, int64_t v) {
    (void)a;
    if (w->len + 8 > w->cap) return 1;
    uint64_t u = 0;
    memcpy(&u, &v, sizeof u);
    for (int i = 7; i >= 0; i--) w->data[w->len++] = (uint8_t)(u >> (8 * i));
    return 0;
}
int64_t bps_sign_extend(int64_t v, int bits) {
    if (bits <= 0 || bits >= 64) return v;
    int64_t mask = (int64_t)1 << (bits - 1);
    if (v & mask) return v - ((int64_t)1 << bits);
    return v;
}
"#;
pub fn emit_c(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    let ir = lower(resolved)?;
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    let mut header = String::from(HEADER);
    let mut src = String::from(PRELUDE);
    for (name, _) in ir.decode.iter() {
        let id = c_name(name);
        header.push_str(&format!(
            "int bps_decode_{id}(bps_cursor *c, bps_alloc *a, int64_t *out);\n"
        ));
        header.push_str(&format!(
            "int bps_encode_{id}(bps_writer *w, bps_alloc *a, int64_t value);\n"
        ));
    }
    for (name, proc) in ir.decode.iter() {
        let id = c_name(name);
        src.push_str(&format!(
            "int bps_decode_{id}(bps_cursor *c, bps_alloc *a, int64_t *out) {{\n"
        ));
        src.push_str("    (void)a;\n");
        let locals = prebind_dests(&proc.body);
        for local in &locals {
            src.push_str(&format!("    int64_t v_{} = 0;\n", c_name(local)));
        }
        for local in &locals {
            src.push_str(&format!("    (void)v_{};\n", c_name(local)));
        }
        src.push_str(&stmts_c(&proc.body, true)?);
        match &proc.result {
            Some(expr) => src.push_str(&format!("    *out = {};\n    return 0;\n}}\n", c_expr(expr))),
            None => src.push_str("    *out = 0;\n    return 0;\n}\n"),
        }
        let _ = proc;
    }
    for (name, proc) in ir.encode.iter() {
        let id = c_name(name);
        src.push_str(&format!(
            "int bps_encode_{id}(bps_writer *w, bps_alloc *a, int64_t value) {{\n"
        ));
        src.push_str("    (void)a;\n");
        let mut locals = prebind_dests(&proc.body);
        if !locals.iter().any(|n| n == "value") {
            locals.push("value".into());
        }
        for local in &locals {
            if local == "value" {
                src.push_str("    int64_t v_value = value;\n");
            } else {
                src.push_str(&format!("    int64_t v_{} = 0;\n", c_name(local)));
            }
        }
        for local in &locals {
            src.push_str(&format!("    (void)v_{};\n", c_name(local)));
        }
        src.push_str(&stmts_c(&proc.body, false)?);
        src.push_str("    return 0;\n}\n");
        let _ = proc;
    }
    // Records are not given a C struct in this first cut; integer scalars are.
    let _ = TypeDef::Scalar(crate::ir::Storage::I32);
    fs::write(out_dir.join("bpsgen.h"), header).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("bpsgen.c"), src).map_err(|e| emit(e.to_string()))?;
    Ok(())
}

fn stmts_c(body: &[Stmt], decode: bool) -> BpsResult<String> {
    let mut out = String::new();
    for stmt in body {
        out.push_str(&stmt_c(stmt, decode)?);
    }
    Ok(out)
}

fn stmt_c(stmt: &Stmt, decode: bool) -> BpsResult<String> {
    match stmt {
        Stmt::Let { name, expr } => Ok(format!("    v_{} = {};\n", c_name(name), c_expr(expr))),
        Stmt::Require(expr) => Ok(format!(
            "    if (!({})) return 2;\n",
            c_expr(expr)
        )),
        Stmt::If { cond, then_body, else_body, .. } => {
            let mut s = format!("    if ({}) {{\n", c_expr(cond));
            s.push_str(&stmts_c(then_body, decode)?);
            if !else_body.is_empty() {
                s.push_str("    } else {\n");
                s.push_str(&stmts_c(else_body, decode)?);
            }
            s.push_str("    }\n");
            Ok(s)
        }
        Stmt::Read { dst, op: WireOp::U8 } => Ok(format!(
            "    if (bps_cursor_u8(c, &v_{})) return 1;\n",
            c_name(dst)
        )),
        Stmt::Read { dst, op: WireOp::F32be } => Ok(format!(
            "    if (bps_cursor_f32(c, &v_{})) return 1;\n",
            c_name(dst)
        )),
        Stmt::Read { dst, op: WireOp::F64be } => Ok(format!(
            "    if (bps_cursor_f64(c, &v_{})) return 1;\n",
            c_name(dst)
        )),
        Stmt::Write { op: WireOp::U8, value } => Ok(format!(
            "    if (bps_writer_u8(w, a, {})) return 1;\n",
            c_expr(value)
        )),
        Stmt::Write { op: WireOp::F32be, value } => Ok(format!(
            "    if (bps_writer_f32(w, a, {})) return 1;\n",
            c_expr(value)
        )),
        Stmt::Write { op: WireOp::F64be, value } => Ok(format!(
            "    if (bps_writer_f64(w, a, {})) return 1;\n",
            c_expr(value)
        )),
        Stmt::CallDecode { dst, callee, args } if args.is_empty() => Ok(format!(
            "    if (bps_decode_{}(c, a, &v_{})) return 1;\n",
            c_name(&callee.0),
            c_name(dst)
        )),
        Stmt::CallEncode { callee, args, value } if args.is_empty() => Ok(format!(
            "    if (bps_encode_{}(w, a, {})) return 1;\n",
            c_name(&callee.0),
            c_expr(value)
        )),
        Stmt::Read { .. }
        | Stmt::Write { .. }
        | Stmt::CallDecode { .. }
        | Stmt::CallEncode { .. }
        | Stmt::Utf8 { .. }
        | Stmt::RepeatCount { .. }
        | Stmt::RepeatUntil { .. }
        | Stmt::RepeatUntilEnd { .. }
        | Stmt::Match { .. }
        | Stmt::Block { .. } => Ok("    return 3;\n".into()),
    }
}

fn c_expr(expr: &Expr) -> String {
    match expr {
        Expr::Int(v) if *v > i64::MAX as i128 || *v < i64::MIN as i128 => {
            format!("{}", (*v as u64) as i64)
        }
        Expr::Int(v) => v.to_string(),
        Expr::Bool(true) => "1".into(),
        Expr::Bool(false) => "0".into(),
        Expr::Ident(name) => format!("v_{}", c_name(name)),
        Expr::Unop(op, inner) if op == "!" => format!("(!{})", c_expr(inner)),
        Expr::Binop(op, left, right) if op == "&&" => format!("(({}) && ({}))", c_expr(left), c_expr(right)),
        Expr::Binop(op, left, right) if op == "||" => format!("(({}) || ({}))", c_expr(left), c_expr(right)),
        Expr::Binop(op, left, right) if op == "/" => format!("(({}) / ({}))", c_expr(left), c_expr(right)),
        Expr::Binop(op, left, right) => format!("(({}) {} ({}))", c_expr(left), op, c_expr(right)),
        Expr::Ternary(cond, yes, no) => format!("(({}) ? ({}) : ({}))", c_expr(cond), c_expr(yes), c_expr(no)),
        Expr::Call(name, args) if name == "sign_extend" => {
            format!("bps_sign_extend({}, (int)({}))", c_expr(&args[0]), c_expr(&args[1]))
        }
        other => format!("(/* {other:?} */ 0)"),
    }
}

fn c_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if ["int", "return", "if", "else", "for", "while"].contains(&s.as_str()) {
        s.push('_');
    }
    s
}

fn prebind_dests(stmts: &[Stmt]) -> Vec<String> {
    let mut names = Vec::new();
    fn walk(stmts: &[Stmt], names: &mut Vec<String>) {
        for stmt in stmts {
            match stmt {
                Stmt::Read { dst, .. } | Stmt::CallDecode { dst, .. } | Stmt::Let { name: dst, .. } => {
                    if !names.iter().any(|n| n == dst) {
                        names.push(dst.clone());
                    }
                }
                Stmt::If { then_body, else_body, .. } => {
                    walk(then_body, names);
                    walk(else_body, names);
                }
                _ => {}
            }
        }
    }
    walk(stmts, &mut names);
    names
}
