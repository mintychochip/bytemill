//! `emit c`: C11 `bpsgen.h` plus one `{category}.c` per route category.
//!
//! The emitted tree is `clang-format`-canonical (LLVM style, 2-space indent,
//! 80 columns): the writer runs its output through `clang-format -i` when the
//! tool is installed, and a `clang-format --dry-run --Werror` pass over the
//! result must find nothing to change. Missing the tool leaves the writer's
//! output as it stands.
//!
//! Every codec of [`Program`] becomes a `bps_decode_<snake>` /
//! `bps_encode_<snake>` pair over an explicit reader/writer; every record a
//! `typedef struct`, every sum a tag enum (numbered from 1, so the zero
//! state is invalid and freeing it is always safe) plus a struct with an
//! anonymous union, every open enum a `typedef` of its base with constants.
//! Optional fields and locals are `bool has_x` plus a value that is valid only when
//! set; lists are a pointer plus a `size_t <x>_len`, grown by doubling, and
//! allocation sizes are spelled where the pointer is (`sizeof *p`), never from
//! a repeated type name.
//!
//! There is no global state: every fallible call runs through `BPS_TRY` and
//! jumps to a shared `fail:` label, which reports the codec (`const char
//! *error_codec`) and a detail (`const char *error_detail`, the `require`
//! text, the missing field, or the status name) once, through the reader or
//! writer next to the [`bps_status`] return; the offset is the reader's `pos`.
//! Decode output is owned by the caller and released with the per-type
//! `bps_free_<snake>` (or `bps_free_str` / `bps_free_bytes`); the writer
//! buffer is released with `bps_free` on the same allocator. `NULL` (or NULL
//! callbacks) selects libc `malloc`/`free`.
//!
//! Records keep exactly the typed program's fields: `position` decodes
//! `raw`, `x`, `y`, `z` and encodes `raw` back, it never rebuilds `raw` from
//! the parts. Integer semantics follow the typed program: [`BinOp`]s that
//! cannot overflow print as plain C operators, wrapping ones go through
//! `uint64_t`, and `/`, `%`, `>>` on possibly-negative values use the
//! `bps_div_floor` / `bps_mod_sign` / `bps_shr_arith` helpers. A sum with no
//! arm for the tag fails with `BPS_ERR_NO_MATCH`; an encode whose value
//! holds another variant fails with `BPS_ERR_VARIANT`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::ResolvedSchema;
use crate::errors::{BpsResult, emit};
use crate::ir::typed::{
    BinOp, Bind, Body, CodecIdx, Expr, Failure, Func, IntTy, LocalId, Origin, Program, ReadOp,
    RepeatMode, Stmt, Ty, TypeIdx, TypeKind, WriteOp, byte_literal, diverges, escape, hex_literal,
};

/// C11 keywords: an identifier equal to one takes a trailing `_`.
const C_KEYWORDS: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "_Alignas",
    "_Alignof",
    "_Atomic",
    "_Bool",
    "_Complex",
    "_Generic",
    "_Imaginary",
    "_Noreturn",
    "_Static_assert",
    "_Thread_local",
];

/// The emitted identifier of a codec's decode function.
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("bps_decode_{}", program.codecs[codec].name.snake())
}

/// The emitted identifier of a codec's encode function.
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("bps_encode_{}", program.codecs[codec].name.snake())
}

/// The emitted identifier of a named type.
pub fn type_name(program: &Program, ty: TypeIdx) -> String {
    format!("bps_{}", escape(program.types[ty].name.snake(), C_KEYWORDS))
}

/// The emitted identifier of a named type's free function.
pub fn free_fn(program: &Program, ty: TypeIdx) -> String {
    format!(
        "bps_free_{}",
        escape(program.types[ty].name.snake(), C_KEYWORDS)
    )
}

/// The emitted tag-enum type of a sum.
fn tag_type(program: &Program, ty: TypeIdx) -> String {
    format!("{}_tag", type_name(program, ty))
}

/// The emitted constant of a sum variant (`other` becomes `OTHER`).
fn tag_const(program: &Program, ty: TypeIdx, variant: usize) -> String {
    let decl = &program.types[ty];
    let stem = escape(decl.name.snake(), C_KEYWORDS).to_ascii_uppercase();
    let arm = match &decl.kind {
        TypeKind::Sum { variants } => variants[variant].name.screaming(),
        _ => unreachable!("tag of a non-sum"),
    };
    format!("BPS_{stem}_{arm}")
}

/// The emitted constant of an enum member.
fn enum_const(program: &Program, ty: TypeIdx, member: usize) -> String {
    let decl = &program.types[ty];
    let stem = escape(decl.name.snake(), C_KEYWORDS).to_ascii_uppercase();
    let name = match &decl.kind {
        TypeKind::Enum { members, .. } => members[member].name.screaming(),
        _ => unreachable!("member of a non-enum"),
    };
    format!("BPS_{stem}_{name}")
}

fn int_c(ty: IntTy) -> &'static str {
    match ty {
        IntTy::I8 => "int8_t",
        IntTy::I16 => "int16_t",
        IntTy::I32 => "int32_t",
        IntTy::I64 => "int64_t",
        IntTy::I128 => unreachable!("i128 has no C spelling"),
        IntTy::U8 => "uint8_t",
        IntTy::U16 => "uint16_t",
        IntTy::U32 => "uint32_t",
        IntTy::U64 => "uint64_t",
    }
}

fn uint_of(ty: IntTy) -> &'static str {
    match ty {
        IntTy::I8 => "uint8_t",
        IntTy::I16 => "uint16_t",
        IntTy::I32 => "uint32_t",
        IntTy::I64 => "uint64_t",
        IntTy::I128 => unreachable!("i128 has no C spelling"),
        IntTy::U8 => "uint8_t",
        IntTy::U16 => "uint16_t",
        IntTy::U32 => "uint32_t",
        IntTy::U64 => "uint64_t",
    }
}

/// The C spelling of a plain (non-list) value type.
fn ctype(program: &Program, ty: &Ty) -> BpsResult<String> {
    match ty {
        Ty::Int(t) => {
            if *t == IntTy::I128 {
                return Err(emit("emit c: i128 has no C11 spelling"));
            }
            Ok(int_c(*t).to_string())
        }
        Ty::Bool => Ok("bool".to_string()),
        Ty::F32 => Ok("float".to_string()),
        Ty::F64 => Ok("double".to_string()),
        Ty::Str => Ok("bps_str".to_string()),
        Ty::Bytes => Ok("bps_bytes".to_string()),
        Ty::List(_) => Err(emit("emit c: lists need a pointer and a length")),
        Ty::Named(t) => Ok(resolved_name(program, *t)?),
    }
}

/// A named type through aliases to its definition.
fn resolved_name(program: &Program, ty: TypeIdx) -> BpsResult<String> {
    match &program.types[ty].kind {
        TypeKind::Alias { target } => resolved_name(program, *target),
        _ => Ok(type_name(program, ty)),
    }
}

/// Whether values of this type can own heap memory (and need freeing).
fn owns(program: &Program, ty: &Ty, seen: &mut HashSet<TypeIdx>) -> bool {
    match ty {
        Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => false,
        Ty::Str | Ty::Bytes | Ty::List(_) => true,
        Ty::Named(t) => {
            if !seen.insert(*t) {
                return false;
            }
            match &program.types[*t].kind {
                TypeKind::Record { fields } => fields.iter().any(|f| owns(program, &f.ty, seen)),
                TypeKind::Sum { variants } => variants
                    .iter()
                    .any(|v| v.payload.as_ref().is_some_and(|p| owns(program, p, seen))),
                TypeKind::Enum { .. } => false,
                TypeKind::Alias { target } => owns(program, &Ty::Named(*target), seen),
            }
        }
    }
}

fn owns_ty(program: &Program, ty: &Ty) -> bool {
    owns(program, ty, &mut HashSet::new())
}

/// The zero initializer of a plain value type.
fn zero_of(program: &Program, ty: &Ty) -> BpsResult<String> {
    match ty {
        Ty::Int(_) => Ok("0".to_string()),
        Ty::Bool => Ok("false".to_string()),
        Ty::F32 => Ok("0.0f".to_string()),
        Ty::F64 => Ok("0.0".to_string()),
        Ty::Str | Ty::Bytes => Ok("{0}".to_string()),
        Ty::List(_) => Err(emit("emit c: lists start as NULL plus zero")),
        Ty::Named(t) => {
            let _ = resolved_name(program, *t)?;
            Ok("{0}".to_string())
        }
    }
}

fn origin_doc(origin: &Origin, program: &Program) -> String {
    match origin {
        Origin::Packet { name, routes } => {
            let mut s = format!("packet {name}");
            if !routes.is_empty() {
                let list: Vec<String> = routes
                    .iter()
                    .map(|r| format!("{}/{} {}", r.state, r.direction, r.id))
                    .collect();
                s.push_str(&format!(" ({})", list.join(", ")));
            }
            s
        }
        Origin::Encoding { name } => format!("encoding {name}"),
        Origin::Enum { name } => format!("enum {name}"),
        Origin::Frame { name } => format!("frame {name}"),
        Origin::Field { parent, field } => {
            format!("nested field {field} of {}", program.types[*parent].name)
        }
        Origin::Variant { sum, variant } => {
            let name = match &program.types[*sum].kind {
                TypeKind::Sum { variants } => variants[*variant].name.snake(),
                _ => unreachable!("variant of a non-sum"),
            };
            format!("arm {name} of {}", program.types[*sum].name)
        }
    }
}

fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

pub fn emit_c(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    emit_c_with_standard(resolved, out_dir, crate::CStandard::default())
}

pub fn emit_c_with_standard(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    std: crate::CStandard,
) -> BpsResult<()> {
    let program = crate::ir::typed(resolved)?;
    let printer = Gen::new(&program, std)?;
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    let header_path = out_dir.join("bpsgen.h");
    fs::write(&header_path, printer.header()?).map_err(|e| emit(e.to_string()))?;
    clang_format(&header_path);
    for (stem, body) in printer.sources()? {
        let path = out_dir.join(format!("{stem}.c"));
        fs::write(&path, body).map_err(|e| emit(e.to_string()))?;
        clang_format(&path);
    }
    Ok(())
}

/// Format in place when `clang-format` is installed. The printer stays inside
/// 80 columns itself; this pass catches the lines it still misses. Missing
/// the tool leaves the writer's output as is, matching the test gate.
fn clang_format(path: &Path) {
    let _ = std::process::Command::new("clang-format")
        .args(["-i"])
        .arg(path)
        .status();
}

/// Shared precomputation: decode alloc needs and a dependency-safe type order.
struct Gen<'a> {
    program: &'a Program,
    /// Whether each codec's decode takes the allocator.
    dec_alloc: Vec<bool>,
    /// Named types in definition order (dependencies first, stable).
    type_order: Vec<TypeIdx>,
    /// Record fields that hold their value behind a pointer because the
    /// type refers to itself by value (directly or through other types).
    boxed: HashSet<(TypeIdx, usize)>,
    /// The requested C standard: pre-C99 output avoids `stdint.h`,
    /// `stdbool.h`, mixed declarations, and for-loop initial declarations.
    std: crate::CStandard,
}

impl<'a> Gen<'a> {
    fn new(program: &'a Program, std: crate::CStandard) -> BpsResult<Self> {
        // Reject types C cannot spell before emitting anything.
        no_nested_lists(program)?;
        for (i, decl) in program.types.iter().enumerate() {
            let _ = type_name(program, i);
            match &decl.kind {
                TypeKind::Record { fields } => {
                    for f in fields {
                        if matches!(f.ty, Ty::List(_)) {
                            let _ = ctype(program, inner(&f.ty))?;
                        } else {
                            let _ = ctype(program, &f.ty)?;
                        }
                        let _ = escape(f.name.snake(), C_KEYWORDS);
                    }
                }
                TypeKind::Sum { variants } => {
                    for v in variants {
                        if let Some(p) = &v.payload {
                            if matches!(p, Ty::List(_)) {
                                return Err(emit("emit c: a sum payload cannot be a bare list"));
                            }
                            let _ = ctype(program, p)?;
                        }
                    }
                }
                TypeKind::Enum { base, .. } => {
                    if *base == IntTy::I128 {
                        return Err(emit("emit c: i128 has no C11 spelling"));
                    }
                }
                TypeKind::Alias { .. } => {}
            }
            let _ = i;
        }
        for codec in &program.codecs {
            for param in &codec.params {
                if matches!(param.ty, Ty::List(_)) {
                    let _ = ctype(program, inner(&param.ty))?;
                } else {
                    let _ = ctype(program, &param.ty)?;
                }
            }
            if matches!(codec.value, Ty::List(_)) {
                return Err(emit("emit c: a codec value cannot be a bare list"));
            }
            let _ = ctype(program, &codec.value)?;
            check_body(program, &codec.decode)?;
            check_body(program, &codec.encode)?;
        }
        let boxed = compute_boxed(program);
        let dec_alloc = alloc_needs(program, &boxed);
        let type_order = order_types(program, &boxed);
        Ok(Gen {
            program,
            dec_alloc,
            type_order,
            boxed,
            std,
        })
    }
}

fn inner(ty: &Ty) -> &Ty {
    match ty {
        Ty::List(t) => inner(t),
        _ => ty,
    }
}

/// Every expression's integer casts must target spellable types; lists never
/// nest, so one `inner` step reaches the element type for declarations.
fn check_body(program: &Program, body: &Body) -> BpsResult<()> {
    for local in &body.locals {
        if matches!(local.ty, Ty::List(_)) {
            let _ = ctype(program, inner(&local.ty))?;
        } else {
            let _ = ctype(program, &local.ty)?;
        }
    }
    Ok(())
}

fn stmts_alloc(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Read {
            op: ReadOp::Bytes(_) | ReadOp::Rest,
            ..
        } => true,
        Stmt::Repeat { .. } => true,
        Stmt::Let { value, .. } => has_strbytes(value),
        Stmt::If { then, els, .. } => stmts_alloc(then) || stmts_alloc(els),
        Stmt::Match { arms, other, .. } => {
            arms.iter().any(|a| stmts_alloc(&a.body))
                || other.as_ref().is_some_and(|o| stmts_alloc(o))
        }
        _ => false,
    })
}

/// A decode takes the allocator when it reads bytes/rest, builds a list, boxes
/// a recursive field (the box allocates), or calls a decode that does.
fn alloc_needs(program: &Program, boxed: &HashSet<(TypeIdx, usize)>) -> Vec<bool> {
    let mut need = vec![false; program.codecs.len()];
    for (i, codec) in program.codecs.iter().enumerate() {
        need[i] = stmts_alloc(&codec.decode.stmts) || return_needs_box(boxed, &codec.decode.stmts);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (i, codec) in program.codecs.iter().enumerate() {
            if !need[i] && calls_alloc(&codec.decode.stmts, &need) {
                need[i] = true;
                changed = true;
            }
        }
    }
    need
}

fn calls_alloc(stmts: &[Stmt], need: &[bool]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Decode { codec, .. } => need[*codec],
        Stmt::If { then, els, .. } => calls_alloc(then, need) || calls_alloc(els, need),
        Stmt::Match { arms, other, .. } => {
            arms.iter().any(|a| calls_alloc(&a.body, need))
                || other.as_ref().is_some_and(|o| calls_alloc(o, need))
        }
        Stmt::Repeat { body, .. } => calls_alloc(body, need),
        _ => false,
    })
}

/// Named types with dependencies (by-value fields and payloads) first.
/// Lists and boxed (pointer) fields only need names, so both are order-free.
fn order_types(program: &Program, boxed: &HashSet<(TypeIdx, usize)>) -> Vec<TypeIdx> {
    let mut done = vec![false; program.types.len()];
    let mut order = Vec::with_capacity(program.types.len());
    while order.len() < program.types.len() {
        let mut progressed = false;
        for i in 0..program.types.len() {
            if done[i] {
                continue;
            }
            if deps_done(program, i, boxed, &done) {
                done[i] = true;
                order.push(i);
                progressed = true;
            }
        }
        if !progressed {
            // A leftover reference cycle only ever passes through pointers
            // (lists, or boxed recursive fields), which are order-free
            // behind the forward declarations; emit the rest in program order.
            for (i, d) in done.iter().enumerate() {
                if !d {
                    order.push(i);
                }
                let _ = d;
            }
            break;
        }
    }
    order
}

fn deps_done(
    program: &Program,
    ty: TypeIdx,
    boxed: &HashSet<(TypeIdx, usize)>,
    done: &[bool],
) -> bool {
    let mut deps = Vec::new();
    dep_tys(program, ty, boxed, &mut deps);
    deps.iter().all(|d| done[*d] || *d == ty)
}

fn dep_tys(
    program: &Program,
    ty: TypeIdx,
    boxed: &HashSet<(TypeIdx, usize)>,
    out: &mut Vec<TypeIdx>,
) {
    match &program.types[ty].kind {
        TypeKind::Record { fields } => {
            for (i, f) in fields.iter().enumerate() {
                if matches!(f.ty, Ty::List(_)) || boxed.contains(&(ty, i)) {
                    continue;
                }
                named_in(&f.ty, out);
            }
        }
        TypeKind::Sum { variants } => {
            for v in variants {
                if let Some(p) = &v.payload
                    && !matches!(p, Ty::List(_))
                {
                    named_in(p, out);
                }
            }
        }
        TypeKind::Alias { target } => out.push(*target),
        TypeKind::Enum { .. } => {}
    }
}

fn named_in(ty: &Ty, out: &mut Vec<TypeIdx>) {
    match ty {
        Ty::Named(t) => out.push(*t),
        Ty::List(t) => named_in(t, out),
        _ => {}
    }
}
// ----------------------------------------------------------------------
// Runtime
// ----------------------------------------------------------------------

const RUNTIME_DECLS: &str = r#"/* A fallible call inside a codec body: stores the status in `s` and jumps
   to `fail:` on error. The `fail` label reports the codec once (only when
   no nested codec already did) and, for decode, frees partial output. */
#define BPS_TRY(call) do { s = (call); if (s != BPS_OK) goto fail; } while (0)
typedef enum {
    BPS_OK = 0,
    BPS_ERR_SHORT,
    BPS_ERR_REQUIRE,
    BPS_ERR_UTF8,
    BPS_ERR_NO_MATCH,
    BPS_ERR_MISSING,
    BPS_ERR_VARIANT,
    BPS_ERR_NEG_LEN,
    BPS_ERR_TOO_LONG,
    BPS_ERR_UNTIL_END,
    BPS_ERR_NOMEM
} bps_status;
const char *bps_status_str(bps_status s);
typedef struct {
    void *(*malloc_fn)(size_t n, void *ctx);
    void (*free_fn)(void *p, void *ctx);
    void *ctx;
} bps_alloc;
void *bps_malloc(const bps_alloc *a, size_t n);
void bps_free(const bps_alloc *a, void *p);
typedef struct {
    uint8_t *data;
    size_t len;
} bps_bytes;
typedef struct {
    char *data;
    size_t len;
} bps_str;
void bps_free_bytes(const bps_alloc *a, bps_bytes *b);
void bps_free_str(const bps_alloc *a, bps_str *s);
bool bps_str_eq(bps_str a, bps_str b);
typedef struct {
    const uint8_t *data;
    size_t len;
    size_t pos;
    const char *error_codec;
    const char *error_detail;
} bps_reader;
typedef struct {
    uint8_t *data;
    size_t len;
    size_t cap;
    const char *error_codec;
    const char *error_detail;
} bps_writer;
bps_status bps_read_u8(bps_reader *r, uint8_t *out);
bps_status bps_peek_u8(bps_reader *r, uint8_t *out);
bps_status bps_read_bytes(bps_reader *r, const bps_alloc *a, int64_t n, bps_bytes *out);
bps_status bps_read_rest(bps_reader *r, const bps_alloc *a, bps_bytes *out);
bps_status bps_read_f32(bps_reader *r, float *out);
bps_status bps_read_f64(bps_reader *r, double *out);
int64_t bps_remaining(const bps_reader *r);
bps_status bps_grow(bps_writer *w, const bps_alloc *a, size_t need);
bps_status bps_write_u8(bps_writer *w, const bps_alloc *a, uint8_t v);
bps_status bps_write_bytes(bps_writer *w, const bps_alloc *a, const uint8_t *p, size_t n);
bps_status bps_write_bytes_pad(bps_writer *w, const bps_alloc *a, const uint8_t *p, size_t n, int64_t total);
bps_status bps_write_f32(bps_writer *w, const bps_alloc *a, float v);
bps_status bps_write_f64(bps_writer *w, const bps_alloc *a, double v);
bool bps_utf8_ok(const uint8_t *s, size_t n);
int64_t bps_utf16_units(const uint8_t *s, size_t n);
int64_t bps_str_len(const uint8_t *s, size_t n);
int64_t bps_sign_extend(int64_t v, int64_t bits);
int64_t bps_div_floor(int64_t a, int64_t b);
int64_t bps_mod_sign(int64_t a, int64_t b);
int64_t bps_shr_arith(int64_t a, int64_t b);
int64_t bps_min_i64(int64_t a, int64_t b);
int64_t bps_max_i64(int64_t a, int64_t b);
"#;

const RUNTIME_IMPLS: &str = r#"const char *bps_status_str(bps_status s) {
    switch (s) {
    case BPS_OK:
        return "BPS_OK";
    case BPS_ERR_SHORT:
        return "BPS_ERR_SHORT";
    case BPS_ERR_REQUIRE:
        return "BPS_ERR_REQUIRE";
    case BPS_ERR_UTF8:
        return "BPS_ERR_UTF8";
    case BPS_ERR_NO_MATCH:
        return "BPS_ERR_NO_MATCH";
    case BPS_ERR_MISSING:
        return "BPS_ERR_MISSING";
    case BPS_ERR_VARIANT:
        return "BPS_ERR_VARIANT";
    case BPS_ERR_NEG_LEN:
        return "BPS_ERR_NEG_LEN";
    case BPS_ERR_TOO_LONG:
        return "BPS_ERR_TOO_LONG";
    case BPS_ERR_UNTIL_END:
        return "BPS_ERR_UNTIL_END";
    case BPS_ERR_NOMEM:
        return "BPS_ERR_NOMEM";
    }
    return "BPS_UNKNOWN";
}
void *bps_malloc(const bps_alloc *a, size_t n) {
    if (n == 0) {
        n = 1;
    }
    if (a != NULL && a->malloc_fn != NULL) {
        return a->malloc_fn(n, a->ctx);
    }
    return malloc(n);
}
void bps_free(const bps_alloc *a, void *p) {
    if (p == NULL) {
        return;
    }
    if (a != NULL && a->free_fn != NULL) {
        a->free_fn(p, a->ctx);
        return;
    }
    free(p);
}
void bps_free_bytes(const bps_alloc *a, bps_bytes *b) {
    bps_free(a, b->data);
    b->data = NULL;
    b->len = 0;
}
void bps_free_str(const bps_alloc *a, bps_str *s) {
    bps_free(a, s->data);
    s->data = NULL;
    s->len = 0;
}
bool bps_str_eq(bps_str a, bps_str b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.data, b.data, a.len) == 0);
}
bps_status bps_read_u8(bps_reader *r, uint8_t *out) {
    if (r->pos >= r->len) {
        return BPS_ERR_SHORT;
    }
    *out = r->data[r->pos];
    r->pos += 1;
    return BPS_OK;
}
bps_status bps_peek_u8(bps_reader *r, uint8_t *out) {
    if (r->pos >= r->len) {
        return BPS_ERR_SHORT;
    }
    *out = r->data[r->pos];
    return BPS_OK;
}
bps_status bps_read_bytes(bps_reader *r, const bps_alloc *a, int64_t n, bps_bytes *out) {
    uint8_t *p;
    if (n < 0) {
        return BPS_ERR_NEG_LEN;
    }
    if ((uint64_t)n > (uint64_t)(r->len - r->pos)) {
        return BPS_ERR_SHORT;
    }
    p = bps_malloc(a, (size_t)n + 1);
    if (p == NULL) {
        return BPS_ERR_NOMEM;
    }
    if (n != 0) {
        memcpy(p, r->data + r->pos, (size_t)n);
    }
    p[n] = 0;
    r->pos += (size_t)n;
    out->data = p;
    out->len = (size_t)n;
    return BPS_OK;
}
bps_status bps_read_rest(bps_reader *r, const bps_alloc *a, bps_bytes *out) {
    return bps_read_bytes(r, a, (int64_t)(r->len - r->pos), out);
}
bps_status bps_read_f32(bps_reader *r, float *out) {
    uint8_t tmp[4];
    uint32_t u;
    float f;
    if (r->pos + 4 > r->len) {
        return BPS_ERR_SHORT;
    }
    memcpy(tmp, r->data + r->pos, 4);
    r->pos += 4;
    u = ((uint32_t)tmp[0] << 24) | ((uint32_t)tmp[1] << 16) | ((uint32_t)tmp[2] << 8) | tmp[3];
    memcpy(&f, &u, 4);
    *out = f;
    return BPS_OK;
}
bps_status bps_read_f64(bps_reader *r, double *out) {
    uint64_t u = 0;
    size_t i;
    double d;
    if (r->pos + 8 > r->len) {
        return BPS_ERR_SHORT;
    }
    for (i = 0; i < 8; i += 1) {
        u = (u << 8) | r->data[r->pos];
        r->pos += 1;
    }
    memcpy(&d, &u, 8);
    *out = d;
    return BPS_OK;
}
int64_t bps_remaining(const bps_reader *r) {
    return (int64_t)(r->len - r->pos);
}
bps_status bps_grow(bps_writer *w, const bps_alloc *a, size_t need) {
    size_t cap;
    uint8_t *p;
    if (need > (size_t)-1 - w->len) {
        return BPS_ERR_NOMEM;
    }
    if (w->len + need <= w->cap) {
        return BPS_OK;
    }
    cap = w->cap != 0 ? w->cap : 64;
    while (w->len + need > cap) {
        if (cap > (size_t)-1 / 2) {
            return BPS_ERR_NOMEM;
        }
        cap *= 2;
    }
    p = bps_malloc(a, cap);
    if (p == NULL) {
        return BPS_ERR_NOMEM;
    }
    if (w->len != 0) {
        memcpy(p, w->data, w->len);
    }
    bps_free(a, w->data);
    w->data = p;
    w->cap = cap;
    return BPS_OK;
}
bps_status bps_write_u8(bps_writer *w, const bps_alloc *a, uint8_t v) {
    bps_status s = bps_grow(w, a, 1);
    if (s != BPS_OK) {
        return s;
    }
    w->data[w->len] = v;
    w->len += 1;
    return BPS_OK;
}
bps_status bps_write_bytes(bps_writer *w, const bps_alloc *a, const uint8_t *p, size_t n) {
    bps_status s = bps_grow(w, a, n);
    if (s != BPS_OK) {
        return s;
    }
    if (n != 0) {
        memcpy(w->data + w->len, p, n);
    }
    w->len += n;
    return BPS_OK;
}
bps_status bps_write_bytes_pad(bps_writer *w, const bps_alloc *a, const uint8_t *p, size_t n, int64_t total) {
    bps_status s;
    size_t i;
    if (total < 0) {
        return BPS_ERR_NEG_LEN;
    }
    if ((uint64_t)n > (uint64_t)total) {
        return BPS_ERR_TOO_LONG;
    }
    s = bps_write_bytes(w, a, p, n);
    if (s != BPS_OK) {
        return s;
    }
    for (i = n; (uint64_t)i < (uint64_t)total; i += 1) {
        s = bps_write_u8(w, a, 0);
        if (s != BPS_OK) {
            return s;
        }
    }
    return BPS_OK;
}
bps_status bps_write_f32(bps_writer *w, const bps_alloc *a, float v) {
    uint32_t u;
    uint8_t tmp[4];
    bps_status s;
    memcpy(&u, &v, 4);
    tmp[0] = (uint8_t)(u >> 24);
    tmp[1] = (uint8_t)(u >> 16);
    tmp[2] = (uint8_t)(u >> 8);
    tmp[3] = (uint8_t)u;
    s = bps_write_bytes(w, a, tmp, 4);
    return s;
}
bps_status bps_write_f64(bps_writer *w, const bps_alloc *a, double v) {
    uint64_t u;
    uint8_t tmp[8];
    size_t i;
    bps_status s;
    memcpy(&u, &v, 8);
    for (i = 0; i < 8; i += 1) {
        tmp[i] = (uint8_t)(u >> (8 * (7 - i)));
    }
    s = bps_write_bytes(w, a, tmp, 8);
    return s;
}
bool bps_utf8_ok(const uint8_t *s, size_t n) {
    size_t i = 0;
    while (i < n) {
        uint8_t c = s[i];
        uint32_t cp;
        size_t need;
        size_t k;
        if (c < 0x80) {
            i += 1;
            continue;
        } else if ((c & 0xE0) == 0xC0) {
            cp = c & 0x1F;
            need = 2;
        } else if ((c & 0xF0) == 0xE0) {
            cp = c & 0x0F;
            need = 3;
        } else if ((c & 0xF8) == 0xF0) {
            cp = c & 0x07;
            need = 4;
        } else {
            return false;
        }
        if (i + need > n) {
            return false;
        }
        for (k = 1; k < need; k += 1) {
            if ((s[i + k] & 0xC0) != 0x80) {
                return false;
            }
            cp = (cp << 6) | (s[i + k] & 0x3F);
        }
        if (cp > 0x10FFFF || (cp >= 0xD800 && cp <= 0xDFFF)) {
            return false;
        }
        if ((need == 2 && cp < 0x80) || (need == 3 && cp < 0x800) || (need == 4 && cp < 0x10000)) {
            return false;
        }
        i += need;
    }
    return true;
}
static int64_t bps_utf_units(const uint8_t *s, size_t n, bool utf16) {
    int64_t k = 0;
    size_t i = 0;
    while (i < n) {
        uint8_t c = s[i];
        uint32_t cp;
        size_t need;
        size_t k2;
        if (c < 0x80) {
            cp = c;
            need = 1;
        } else if ((c & 0xE0) == 0xC0) {
            cp = c & 0x1F;
            need = 2;
        } else if ((c & 0xF0) == 0xE0) {
            cp = c & 0x0F;
            need = 3;
        } else if ((c & 0xF8) == 0xF0) {
            cp = c & 0x07;
            need = 4;
        } else {
            k += 1;
            i += 1;
            continue;
        }
        if (need == 1) {
            k += 1;
            i += 1;
            continue;
        }
        if (i + need > n) {
            k += 1;
            break;
        }
        for (k2 = 1; k2 < need; k2 += 1) {
            cp = (cp << 6) | (s[i + k2] & 0x3F);
        }
        i += need;
        if (utf16) {
            k += cp > 0xFFFF ? 2 : 1;
        } else {
            k += 1;
        }
    }
    return k;
}
int64_t bps_utf16_units(const uint8_t *s, size_t n) {
    return bps_utf_units(s, n, true);
}
int64_t bps_str_len(const uint8_t *s, size_t n) {
    return bps_utf_units(s, n, false);
}
int64_t bps_sign_extend(int64_t v, int64_t bits) {
    uint64_t u;
    uint64_t sign;
    uint64_t mask;
    if (bits <= 0 || bits >= 64) {
        return v;
    }
    u = (uint64_t)v;
    sign = (uint64_t)1 << (uint64_t)(bits - 1);
    mask = ((uint64_t)1 << (uint64_t)bits) - 1;
    u &= mask;
    if ((u & sign) != 0) {
        u |= ~mask;
    }
    return (int64_t)u;
}
int64_t bps_div_floor(int64_t a, int64_t b) {
    int64_t q;
    int64_t rr;
    if (b == 0) {
        return 0;
    }
    if (b == -1 && a == INT64_MIN) {
        return INT64_MIN;
    }
    q = a / b;
    rr = a % b;
    if (rr != 0 && ((rr < 0) != (b < 0))) {
        q -= 1;
    }
    return q;
}
int64_t bps_mod_sign(int64_t a, int64_t b) {
    int64_t rr;
    if (b == 0) {
        return 0;
    }
    if (b == -1 && a == INT64_MIN) {
        return 0;
    }
    rr = a % b;
    if (rr != 0 && ((rr < 0) != (b < 0))) {
        rr += b;
    }
    return rr;
}
int64_t bps_shr_arith(int64_t a, int64_t b) {
    uint64_t u;
    if (b <= 0) {
        return a;
    }
    if (b >= 64) {
        return a < 0 ? -1 : 0;
    }
    u = (uint64_t)a;
    if (a >= 0) {
        return (int64_t)(u >> (uint64_t)b);
    }
    return (int64_t)(~(~u >> (uint64_t)b));
}
int64_t bps_min_i64(int64_t a, int64_t b) {
    return a < b ? a : b;
}
int64_t bps_max_i64(int64_t a, int64_t b) {
    return a > b ? a : b;
}
"#;

// ----------------------------------------------------------------------
// Header
// ----------------------------------------------------------------------

impl<'a> Gen<'a> {
    /// Pre-C99 C: declarations cannot mix with statements, so temporaries
    /// are hoisted to the top of the function.
    fn c89(&self) -> bool {
        !self.std.supports_mixed_decls()
    }

    fn header(&self) -> BpsResult<String> {
        let p = self.program;
        let mut h = String::new();
        h.push_str("#ifndef BPSGEN_H\n#define BPSGEN_H\n");
        h.push_str("/* Generated by bpsc emit c. Do not edit. */\n");
        h.push_str("#include <stddef.h>\n");
        if self.std.supports_stdint() {
            h.push_str("#include <stdint.h>\n");
        } else {
            h.push_str("/* Pre-C99 C has no stdint.h: exact widths from C89 types. */\n");
            h.push_str("typedef signed char int8_t;\n");
            h.push_str("typedef unsigned char uint8_t;\n");
            h.push_str("typedef short int16_t;\n");
            h.push_str("typedef unsigned short uint16_t;\n");
            h.push_str("typedef int int32_t;\n");
            h.push_str("typedef unsigned int uint32_t;\n");
            h.push_str("typedef long long int64_t;\n");
            h.push_str("typedef unsigned long long uint64_t;\n");
            h.push_str("#ifndef INT64_MIN\n");
            h.push_str("#define INT64_MIN (-9223372036854775807LL - 1)\n");
            h.push_str("#endif\n");
        }
        if self.std.supports_stdbool() {
            h.push_str("#include <stdbool.h>\n");
        } else {
            h.push_str("/* Pre-C99 C has no stdbool.h. */\n");
            h.push_str("typedef int bool;\n");
            h.push_str("#define false 0\n");
            h.push_str("#define true 1\n");
        }
        h.push_str("#include <string.h>\n#include <stdlib.h>\n\n");
        h.push_str("/* Runtime: cursors, owned values, errors, helpers. Decode output is\n");
        h.push_str("   owned by the caller and released with bps_free_<type>; the writer\n");
        h.push_str("   buffer is released with bps_free on the same allocator. A NULL\n");
        h.push_str("   allocator (or NULL callbacks) selects libc malloc/free. On failure\n");
        h.push_str("   the codec sets error_codec/error_detail (and the reader's pos is\n");
        h.push_str("   the error offset); nested failures propagate untouched. */\n");
        h.push_str(RUNTIME_DECLS);
        if self.c89() {
            h.push_str("/* Pre-C99 C has no compound literals: build strings by call. */\n");
            h.push_str("bps_str bps_make_str(const char *s, size_t n);\n");
        }
        h.push('\n');
        // Forward declarations for reference cycles.
        for ty in &self.type_order {
            match &p.types[*ty].kind {
                TypeKind::Record { .. } | TypeKind::Sum { .. } => {
                    h.push_str(&format!(
                        "typedef struct {} {};\n",
                        type_name(p, *ty),
                        type_name(p, *ty)
                    ));
                }
                _ => {}
            }
        }
        h.push('\n');
        for ty in &self.type_order {
            self.decl_type(&mut h, *ty)?;
        }
        // Codec and free prototypes.
        for (i, codec) in p.codecs.iter().enumerate() {
            let doc = origin_doc(&codec.origin, p);
            h.push_str(&format!("/** Decode {doc}. */\n{};\n", self.decode_sig(i)?));
            h.push_str(&format!("/** Encode {doc}. */\n{};\n", self.encode_sig(i)?));
        }
        for ty in &self.type_order {
            match &p.types[*ty].kind {
                TypeKind::Record { .. }
                | TypeKind::Sum { .. }
                | TypeKind::Enum { .. }
                | TypeKind::Alias { .. } => {
                    let doc = origin_doc(&p.types[*ty].origin, p);
                    h.push_str(&format!("/** Free {doc}. */\n{};\n", self.free_sig(*ty)?));
                }
            }
        }
        h.push_str("#endif\n");
        Ok(h)
    }

    fn decl_type(&self, h: &mut String, ty: TypeIdx) -> BpsResult<()> {
        let p = self.program;
        let decl = &p.types[ty];
        let name = type_name(p, ty);
        match &decl.kind {
            TypeKind::Record { fields } => {
                h.push_str(&format!(
                    "/** Record {}: {}. */\n",
                    decl.name,
                    origin_doc(&decl.origin, p)
                ));
                h.push_str(&format!("struct {name} {{\n"));
                if fields.is_empty() {
                    h.push_str("    uint8_t empty;\n");
                }
                let members = record_members(p, ty);
                for (i, f) in fields.iter().enumerate() {
                    let m = &members[i];
                    if let Some(flag) = &m.has {
                        h.push_str(&format!("    bool {flag};\n"));
                    }
                    h.push_str(&format!(
                        "    {};\n",
                        self.member_decl(
                            &f.ty,
                            &m.value,
                            m.len.as_deref(),
                            self.boxed.contains(&(ty, i))
                        )?
                    ));
                }
                h.push_str("};\n");
            }
            TypeKind::Sum { variants } => {
                h.push_str(&format!(
                    "/** Sum {}: {}. */\n",
                    decl.name,
                    origin_doc(&decl.origin, p)
                ));
                // Tags start at 1: the zero state is invalid, so freeing a
                // zero-initialized (never successfully decoded) sum is safe.
                h.push_str("typedef enum {\n");
                for (i, _) in variants.iter().enumerate() {
                    h.push_str(&format!("    {} = {},\n", tag_const(p, ty, i), i + 1));
                }
                h.push_str(&format!("}} {};\n", tag_type(p, ty)));
                h.push_str(&format!("struct {name} {{\n"));
                h.push_str(&format!("    {} tag;\n", tag_type(p, ty)));
                h.push_str("    union {\n");
                let mut any = false;
                for (i, v) in variants.iter().enumerate() {
                    if let Some(payload) = &v.payload {
                        let c = escape(v.name.snake(), C_KEYWORDS);
                        h.push_str(&format!(
                            "        {};\n",
                            self.member_decl(payload, &c, None, false)?
                        ));
                        let _ = i;
                        any = true;
                    }
                }
                if !any {
                    h.push_str("        uint8_t unit;\n");
                }
                h.push_str("    } value;\n};\n");
            }
            TypeKind::Enum { base, members } => {
                h.push_str(&format!(
                    "/** Enum {}: {}. */\n",
                    decl.name,
                    origin_doc(&decl.origin, p)
                ));
                h.push_str(&format!("typedef {} {name};\n", int_c(*base)));
                h.push_str("enum {\n");
                for (i, m) in members.iter().enumerate() {
                    h.push_str(&format!("    {} = {},\n", enum_const(p, ty, i), m.value));
                    let _ = m;
                }
                h.push_str("};\n");
            }
            TypeKind::Alias { target } => {
                h.push_str(&format!(
                    "/** Alias {} of {}: {}. */\n",
                    decl.name,
                    p.types[*target].name,
                    origin_doc(&decl.origin, p)
                ));
                h.push_str(&format!("typedef {} {name};\n", resolved_name(p, *target)?));
            }
        }
        Ok(())
    }

    /// `int32_t name;`, `bps_foo *items; size_t items_len;` for lists (the
    /// length is the resolved member name, which may have bumped), and
    /// `bps_foo *next;` for boxed recursive fields (no length).
    fn member_decl(&self, ty: &Ty, c: &str, len: Option<&str>, boxed: bool) -> BpsResult<String> {
        match ty {
            Ty::List(_) => {
                let ln = len.ok_or_else(|| emit("emit c: a list member without a length"))?;
                Ok(format!(
                    "{} *{c};\n    size_t {ln}",
                    ctype(self.program, inner(ty))?
                ))
            }
            _ if boxed => Ok(format!("{} *{c}", ctype(self.program, ty)?)),
            _ => Ok(format!("{} {c}", ctype(self.program, ty)?)),
        }
    }

    /// One codec parameter (optional scalars take a presence flag first).
    /// Centralized C names for one function body (see [`name_map`]).
    fn param_decls(&self, codec: CodecIdx, decode: bool) -> BpsResult<Vec<String>> {
        let p = self.program;
        let body = if decode {
            &p.codecs[codec].decode
        } else {
            &p.codecs[codec].encode
        };
        let nm = name_map(p, codec, decode);
        let mut out = Vec::new();
        for (i, param) in p.codecs[codec].params.iter().enumerate() {
            let l = body.params[i];
            let c = &nm.local[l];
            if param.optional {
                out.push(format!("bool {}", nm.has[&l]));
            }
            match &param.ty {
                Ty::List(_) => {
                    out.push(format!("const {} *{c}", ctype(p, inner(&param.ty))?));
                    out.push(format!("size_t {}", nm.len[&l]));
                }
                t if is_scalar(t) || is_enum_ty(p, t) => out.push(format!("{} {c}", ctype(p, t)?)),
                _ => out.push(format!("const {} *{c}", ctype(p, &param.ty)?)),
            }
        }
        Ok(out)
    }

    fn decode_sig(&self, codec: CodecIdx) -> BpsResult<String> {
        let p = self.program;
        let mut parts = vec!["bps_reader *r".to_string()];
        if self.dec_alloc[codec] {
            parts.push("const bps_alloc *a".to_string());
        }
        parts.extend(self.param_decls(codec, true)?);
        parts.push(format!("{} *out", ctype(p, &p.codecs[codec].value)?));
        Ok(format!(
            "bps_status {}({})",
            decode_fn(p, codec),
            parts.join(", ")
        ))
    }

    fn encode_sig(&self, codec: CodecIdx) -> BpsResult<String> {
        let p = self.program;
        let body = &p.codecs[codec].encode;
        let nm = name_map(p, codec, false);
        let mut parts = vec![
            "bps_writer *w".to_string(),
            "const bps_alloc *a".to_string(),
        ];
        parts.extend(self.param_decls(codec, false)?);
        let v = body.value.expect("encode bodies take an input value");
        let vn = &nm.local[v];
        if nm.ptr.contains(&v) {
            parts.push(format!("const {} *{vn}", ctype(p, &p.codecs[codec].value)?));
        } else {
            parts.push(format!("{} {vn}", ctype(p, &p.codecs[codec].value)?));
        }
        Ok(format!(
            "bps_status {}({})",
            encode_fn(p, codec),
            parts.join(", ")
        ))
    }

    fn free_sig(&self, ty: TypeIdx) -> BpsResult<String> {
        let _ = resolved_name(self.program, ty)?;
        Ok(format!(
            "void {}(const bps_alloc *a, {} *v)",
            free_fn(self.program, ty),
            type_name(self.program, ty)
        ))
    }
}
// ----------------------------------------------------------------------
// Source: free functions and codecs
// ----------------------------------------------------------------------

fn c_preamble() -> String {
    "/* Generated by bpsc emit c. Do not edit. */\n#include \"bpsgen.h\"\n#include <string.h>\n\n"
        .to_string()
}

impl<'a> Gen<'a> {
    fn sources(&self) -> BpsResult<BTreeMap<String, String>> {
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        let mut runtime = format!(
            "/* Generated by bpsc emit c. Do not edit. */\n#include \"bpsgen.h\"\n#include <string.h>\n\n{RUNTIME_IMPLS}\n"
        );
        if self.c89() {
            runtime.push_str(
                "bps_str bps_make_str(const char *s, size_t n) {\n    bps_str b;\n    b.data = (char *)s;\n    b.len = n;\n    return b;\n}\n",
            );
        }
        files.insert("runtime".to_string(), runtime);
        for ty in &self.type_order {
            let stem = self.program.output_stem(&self.program.types[*ty].origin);
            let file = files.entry(stem).or_insert_with(c_preamble);
            self.emit_free(file, *ty)?;
        }
        for (i, codec) in self.program.codecs.iter().enumerate() {
            let stem = self.program.output_stem(&codec.origin);
            let file = files.entry(stem).or_insert_with(c_preamble);
            self.emit_decode(file, i)?;
            self.emit_encode(file, i)?;
        }
        Ok(files)
    }

    fn emit_free(&self, s: &mut String, ty: TypeIdx) -> BpsResult<()> {
        let p = self.program;
        let decl = &p.types[ty];
        let doc = origin_doc(&decl.origin, p);
        s.push_str(&format!("/** Free {doc}. */\n{} {{\n", self.free_sig(ty)?));
        let hoist_at = s.len();
        let mut hoist: Vec<String> = Vec::new();
        match &decl.kind {
            TypeKind::Record { fields } => {
                let mut any = false;
                let members = record_members(p, ty);
                for (i, f) in fields.iter().enumerate() {
                    let m = &members[i];
                    let target = format!("v->{}", m.value);
                    if self.boxed.contains(&(ty, i)) {
                        let inner = self.free_boxed(&f.ty, &target, 2)?;
                        if inner.is_empty() {
                            continue;
                        }
                        any = true;
                        match &m.has {
                            Some(h) => {
                                s.push_str(&format!("    if (v->{h}) {{\n"));
                                s.push_str(&inner);
                                s.push_str("    }\n");
                            }
                            None => {
                                s.push_str(&format!("    if ({target} != NULL) {{\n"));
                                s.push_str(&inner);
                                s.push_str("    }\n");
                            }
                        }
                        continue;
                    }
                    let len = m.len.as_deref().map(|l| format!("v->{l}"));
                    let body = self.free_value(&f.ty, &target, len.as_deref(), 1, &mut hoist)?;
                    if body.is_empty() {
                        continue;
                    }
                    any = true;
                    if let Some(h) = &m.has {
                        s.push_str(&format!("    if (v->{h}) {{\n"));
                        s.push_str(&body);
                        s.push_str("    }\n");
                    } else {
                        s.push_str(&body);
                    }
                }
                if !any {
                    s.push_str("    (void)a;\n    (void)v;\n");
                }
            }
            TypeKind::Sum { variants } => {
                // Only variants that own memory get a case; the rest fall to
                // `default`.
                let mut cases = String::new();
                for (i, v) in variants.iter().enumerate() {
                    let Some(payload) = &v.payload else {
                        continue;
                    };
                    let target = format!("v->value.{}", escape(v.name.snake(), C_KEYWORDS));
                    let body = self.free_value(payload, &target, None, 2, &mut hoist)?;
                    if !body.is_empty() {
                        cases.push_str(&format!("    case {}:\n", tag_const(p, ty, i)));
                        cases.push_str(&body);
                        cases.push_str("        break;\n");
                    }
                }
                if cases.is_empty() {
                    s.push_str("    (void)a;\n    (void)v;\n");
                } else {
                    s.push_str("    switch (v->tag) {\n");
                    s.push_str(&cases);
                    s.push_str("    default:\n        break;\n    }\n");
                }
            }
            TypeKind::Enum { .. } => {
                s.push_str("    (void)a;\n    (void)v;\n");
            }
            TypeKind::Alias { target } => match &p.types[*target].kind {
                TypeKind::Enum { .. } => {
                    s.push_str("    (void)a;\n    (void)v;\n");
                }
                _ => {
                    s.push_str(&format!("    {}(a, v);\n", free_fn(p, *target)));
                }
            },
        }
        if !hoist.is_empty() {
            let decls: String = hoist.iter().map(|d| format!("    {d}\n")).collect();
            s.insert_str(hoist_at, &decls);
        }
        s.push_str("}\n");
        Ok(())
    }

    /// Free a boxed (pointer) record member `target`: the pointee, then the
    /// pointer itself. Mirrors [`free_value`]'s `Named` arm without the `&`.
    fn free_boxed(&self, ty: &Ty, target: &str, depth: usize) -> BpsResult<String> {
        let p = self.program;
        let pad: String = "    ".repeat(depth);
        let Ty::Named(t) = ty else {
            return Err(emit("emit c: a boxed field is always a named type"));
        };
        let f = match &p.types[*t].kind {
            TypeKind::Enum { .. } => return Ok(String::new()),
            TypeKind::Alias { target } => match &p.types[*target].kind {
                TypeKind::Enum { .. } => return Ok(String::new()),
                _ => free_fn(p, *target),
            },
            _ => free_fn(p, *t),
        };
        Ok(format!(
            "{pad}{f}(a, {target});\n{pad}bps_free(a, (void *)({target}));\n"
        ))
    }

    /// Free `target` (an addressable lvalue) of type `ty` at `depth` indent.
    /// `len` is the length expression for a top-level list (a record's
    /// `<x>_len` member); nested elements have none. Empty when the type
    /// owns nothing.
    fn free_value(
        &self,
        ty: &Ty,
        target: &str,
        len: Option<&str>,
        depth: usize,
        hoist: &mut Vec<String>,
    ) -> BpsResult<String> {
        let p = self.program;
        let pad: String = "    ".repeat(depth);
        match ty {
            Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => Ok(String::new()),
            Ty::Str => Ok(format!("{pad}bps_free_str(a, {});\n", addr_of(target))),
            Ty::Bytes => Ok(format!("{pad}bps_free_bytes(a, {});\n", addr_of(target))),
            Ty::List(elem) => {
                let e = ctype(p, inner(ty))?;
                let bound = len
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("({target})_len"));
                let mut out = String::new();
                if owns_ty(p, elem) {
                    let loop_var = format!("k{depth}");
                    if self.c89() {
                        // Pre-C99 C forbids declarations in `for` initializers:
                        // hoist the counter and assign it in the header.
                        let decl = format!("size_t {loop_var};");
                        if !hoist.contains(&decl) {
                            hoist.push(decl);
                        }
                        out.push_str(&format!(
                            "{pad}for ({loop_var} = 0; {loop_var} < {bound}; {loop_var} += 1) {{\n"
                        ));
                    } else {
                        out.push_str(&format!(
                            "{pad}for (size_t {loop_var} = 0; {loop_var} < {bound}; {loop_var} += 1) {{\n"
                        ));
                    }
                    out.push_str(&self.free_value(
                        elem,
                        &format!("({target})[{loop_var}]"),
                        None,
                        depth + 1,
                        hoist,
                    )?);
                    out.push_str(&format!("{pad}}}\n"));
                    let _ = e;
                }
                out.push_str(&format!("{pad}bps_free(a, (void *)({target}));\n"));
                Ok(out)
            }
            Ty::Named(t) => match &p.types[*t].kind {
                TypeKind::Enum { .. } => Ok(String::new()),
                TypeKind::Alias { target: aliased } => match &p.types[*aliased].kind {
                    TypeKind::Enum { .. } => Ok(String::new()),
                    _ => Ok(format!(
                        "{pad}{}(a, {});\n",
                        free_fn(p, *aliased),
                        addr_of(target)
                    )),
                },
                _ => Ok(format!(
                    "{pad}{}(a, {});\n",
                    free_fn(p, *t),
                    addr_of(target)
                )),
            },
        }
    }
}
// ----------------------------------------------------------------------
// Per-function C names
// ----------------------------------------------------------------------

/// Every C name of one function body: locals plus the presence, length, and
/// capacity companions the printer derives. Signatures and bodies share
/// these, so they always agree.
struct NameMap {
    local: Vec<String>,
    has: HashMap<LocalId, String>,
    len: HashMap<LocalId, String>,
    cap: HashMap<LocalId, String>,
    /// Locals held as const pointers (owned parameters, encode input).
    ptr: HashSet<LocalId>,
}

fn is_scalar(ty: &Ty) -> bool {
    matches!(ty, Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64)
}

/// C precedence of a binary operator (higher binds tighter).
fn prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::BitOr => 3,
        BinOp::BitXor => 4,
        BinOp::BitAnd => 5,
        BinOp::Eq | BinOp::Ne => 6,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 7,
        BinOp::Shl | BinOp::Shr => 8,
        BinOp::Add | BinOp::Sub => 9,
        BinOp::Mul | BinOp::Div | BinOp::Mod => 10,
    }
}

fn is_bitwise(op: BinOp) -> bool {
    matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
}

/// An integer literal that is a direct operand of `&`, `|` (or `^`) prints
/// in hex when it is a mask or flag bit (`typed::hex_literal`).
fn hex_mask(op: BinOp, e: &Expr, s: String) -> String {
    if let Expr::Int { value, .. } = e
        && hex_literal(op, *value)
    {
        format!("0x{value:X}")
    } else {
        s
    }
}

/// Open enums are plain integers at the ABI: they pass by value, unlike
/// every other named type.
fn is_enum_ty(program: &Program, ty: &Ty) -> bool {
    matches!(ty, Ty::Named(t) if matches!(program.types[*t].kind, TypeKind::Enum { .. }))
}

fn name_map(program: &Program, codec: CodecIdx, decode: bool) -> NameMap {
    let body = if decode {
        &program.codecs[codec].decode
    } else {
        &program.codecs[codec].encode
    };
    let mut taken = HashSet::new();
    // The parameters and the status variable are fixed names a codec body
    // always binds (decode `r`/`a`/`out`, encode `w`/`a`/`value`, plus `s`):
    // seed them so a schema local of the same name takes the `_2` suffix and
    // never collides with them.
    for fixed in if decode {
        ["r", "a", "out", "s"]
    } else {
        ["w", "a", "value", "s"]
    } {
        taken.insert(fixed.to_string());
    }
    let mut local = Vec::with_capacity(body.locals.len());
    for l in &body.locals {
        let base = escape(l.name.snake(), C_KEYWORDS);
        let mut c = base.clone();
        let mut n = 2;
        while taken.contains(&c) {
            c = format!("{base}_{n}");
            n += 1;
        }
        taken.insert(c.clone());
        local.push(c);
    }
    let fresh = |taken: &mut HashSet<String>, base: String| {
        let mut c = base.clone();
        let mut n = 2;
        while taken.contains(&c) {
            c = format!("{base}_{n}");
            n += 1;
        }
        taken.insert(c.clone());
        c
    };
    let mut has = HashMap::new();
    for (i, l) in body.locals.iter().enumerate() {
        if l.optional {
            has.insert(i, fresh(&mut taken, format!("has_{}", local[i])));
        }
    }
    let mut len = HashMap::new();
    for (i, l) in body.locals.iter().enumerate() {
        if matches!(l.ty, Ty::List(_)) {
            len.insert(i, fresh(&mut taken, format!("{}_len", local[i])));
        }
    }
    let mut repeats = HashSet::new();
    if decode {
        collect_repeats(&body.stmts, &mut repeats);
    }
    let mut repeats: Vec<LocalId> = repeats.into_iter().collect();
    repeats.sort_unstable();
    let mut cap = HashMap::new();
    for l in &repeats {
        cap.insert(*l, fresh(&mut taken, format!("{}_cap", local[*l])));
    }
    let mut ptr = HashSet::new();
    for (i, l) in body.locals.iter().enumerate() {
        if is_enum_ty(program, &l.ty) {
            continue;
        }
        if body.params.contains(&i) && !is_scalar(&l.ty) {
            ptr.insert(i);
        }
    }
    if !decode
        && let Some(v) = body.value
        && !is_scalar(&body.locals[v].ty)
        && !is_enum_ty(program, &body.locals[v].ty)
    {
        ptr.insert(v);
    }
    NameMap {
        local,
        has,
        len,
        cap,
        ptr,
    }
}

fn collect_repeats(stmts: &[Stmt], out: &mut HashSet<LocalId>) {
    for s in stmts {
        match s {
            Stmt::Repeat { list, body, .. } => {
                out.insert(*list);
                collect_repeats(body, out);
            }
            Stmt::If { then, els, .. } => {
                collect_repeats(then, out);
                collect_repeats(els, out);
            }
            Stmt::Match { arms, other, .. } => {
                for a in arms {
                    collect_repeats(&a.body, out);
                }
                if let Some(o) = other {
                    collect_repeats(o, out);
                }
            }
            _ => {}
        }
    }
}

// ----------------------------------------------------------------------
// Codec functions
// ----------------------------------------------------------------------

impl<'a> Gen<'a> {
    fn emit_decode(&self, s: &mut String, codec: CodecIdx) -> BpsResult<()> {
        let p = self.program;
        let doc = origin_doc(&p.codecs[codec].origin, p);
        let mut f = FnEmit::new(p, &self.dec_alloc, &self.boxed, codec, true, self.std);
        f.function()?;
        s.push_str(&format!(
            "/** Decode {doc}. */\n{} {{\n{}}}\n",
            self.decode_sig(codec)?,
            f.code
        ));
        Ok(())
    }

    fn emit_encode(&self, s: &mut String, codec: CodecIdx) -> BpsResult<()> {
        let p = self.program;
        let doc = origin_doc(&p.codecs[codec].origin, p);
        let mut f = FnEmit::new(p, &self.dec_alloc, &self.boxed, codec, false, self.std);
        f.function()?;
        s.push_str(&format!(
            "/** Encode {doc}. */\n{} {{\n{}}}\n",
            self.encode_sig(codec)?,
            f.code
        ));
        Ok(())
    }
}

/// One decode or encode function.
struct FnEmit<'p> {
    p: &'p Program,
    dec_alloc: &'p [bool],
    boxed: &'p HashSet<(TypeIdx, usize)>,
    decode: bool,
    fn_name: String,
    body: &'p Body,
    nm: NameMap,
    taken: HashSet<String>,
    ind: usize,
    code: String,
    voids: Vec<String>,
    live: Vec<LocalId>,
    /// Locals some path to the current statement may have stored: a return
    /// frees only these (the rest still hold their empty initial value).
    stored: HashSet<LocalId>,
    tmp: usize,
    need_s: bool,
    std: crate::CStandard,
    /// Pre-C99 declarations, hoisted to the top of the function.
    hoist: Vec<String>,
}

impl<'p> FnEmit<'p> {
    fn new(
        p: &'p Program,
        dec_alloc: &'p [bool],
        boxed: &'p HashSet<(TypeIdx, usize)>,
        codec: CodecIdx,
        decode: bool,
        std: crate::CStandard,
    ) -> Self {
        let body = if decode {
            &p.codecs[codec].decode
        } else {
            &p.codecs[codec].encode
        };
        let fn_name = if decode {
            decode_fn(p, codec)
        } else {
            encode_fn(p, codec)
        };
        let nm = name_map(p, codec, decode);
        let mut taken = HashSet::new();
        for c in nm
            .local
            .iter()
            .chain(nm.has.values())
            .chain(nm.len.values())
            .chain(nm.cap.values())
        {
            taken.insert(c.clone());
        }
        // Boxed (pointer) stores allocate at return, so a failing malloc needs
        // the status variable and fail label even in an otherwise infallible body.
        let need_s =
            need_s(&body.stmts, decode) || (decode && return_needs_box(boxed, &body.stmts));
        FnEmit {
            p,
            dec_alloc,
            boxed,
            decode,
            fn_name,
            body,
            nm,
            taken,
            ind: 1,
            code: String::new(),
            voids: Vec::new(),
            live: Vec::new(),
            stored: HashSet::new(),
            tmp: 0,
            need_s,
            std,
            hoist: Vec::new(),
        }
    }

    fn line(&mut self, t: &str) {
        if !t.is_empty() {
            self.code.push_str(&"    ".repeat(self.ind));
        }
        self.code.push_str(t);
        self.code.push('\n');
    }

    fn temp(&mut self, base: &str) -> String {
        let mut c = base.to_string();
        while self.taken.contains(&c) {
            self.tmp += 1;
            c = format!("{base}_{}", self.tmp);
        }
        self.taken.insert(c.clone());
        c
    }
    fn c89(&self) -> bool {
        !self.std.supports_mixed_decls()
    }

    /// Declare a mid-body temporary `name` of type `prefix` (a plain type
    /// like `int64_t`, or a pointer prefix like `uint8_t *`) initialized
    /// with `init`. Pre-C99 C hoists the declaration to the top of the
    /// function and assigns in place; newer standards declare inline.
    fn decl_temp(&mut self, prefix: &str, name: &str, init: &str) {
        if self.c89() {
            self.hoist.push(format!("{prefix} {name};"));
            self.line(&format!("{name} = {init};"));
        } else {
            self.line(&format!("{prefix} {name} = {init};"));
        }
    }

    /// Open a `for` loop over a temporary counter `name` of type `ty` up
    /// to `bound`. Pre-C99 C forbids declarations in the initializer, so
    /// the counter is hoisted and assigned in the header instead.
    fn for_temp(&mut self, ty: &str, name: &str, bound: &str) {
        if self.c89() {
            self.hoist.push(format!("{ty} {name};"));
            self.line(&format!(
                "for ({name} = 0; {name} < {bound}; {name} += 1) {{"
            ));
        } else {
            self.line(&format!(
                "for ({ty} {name} = 0; {name} < {bound}; {name} += 1) {{"
            ));
        }
    }

    fn rw(&self) -> &'static str {
        if self.decode { "r" } else { "w" }
    }

    fn function(&mut self) -> BpsResult<()> {
        let uses = local_uses(self.body);
        let addr = addr_taken(&self.body.stmts);
        for l in 0..self.body.locals.len() {
            if self.body.params.contains(&l) || self.body.value == Some(l) {
                continue;
            }
            self.decl_local(l, &uses, &addr)?;
        }
        for l in self.body.params.clone() {
            if !uses.contains(&l) {
                self.voids.push(format!("(void){};", self.nm.local[l]));
            }
            if let Some(h) = self.nm.has.get(&l) {
                self.voids.push(format!("(void){h};"));
            }
            if let Some(ln) = self.nm.len.get(&l) {
                self.voids.push(format!("(void){ln};"));
            }
        }
        if let Some(v) = self.body.value
            && !uses.contains(&v)
        {
            self.voids.push(format!("(void){};", self.nm.local[v]));
        }
        let r_unused = self.decode && !self.need_s && !stmts_has_remaining(&self.body.stmts);
        let w_unused = !self.decode && !touches_w(&self.body.stmts);
        let out_unused = self.decode && !writes_out(&self.body.stmts);
        if r_unused {
            self.voids.push("(void)r;".to_string());
        }
        if w_unused {
            self.voids.push("(void)w;".to_string());
            self.voids.push("(void)a;".to_string());
        }
        if out_unused {
            self.voids.push("(void)out;".to_string());
        }
        let voids = std::mem::take(&mut self.voids);
        // Pre-C99 declarations land here, ahead of the `(void)` statements.
        let hoist_at = self.code.len();
        for v in &voids {
            self.line(v);
        }
        if self.need_s {
            if self.c89() {
                self.hoist.push("bps_status s;".to_string());
                self.line("s = BPS_OK;");
            } else {
                self.line("bps_status s = BPS_OK;");
            }
        }
        let stmts = self.body.stmts.clone();
        if self.decode {
            self.dec_stmts(&stmts)?;
        } else {
            self.enc_stmts(&stmts)?;
        }
        // Falling off the end is unreachable when every path returns or
        // fails; otherwise C needs a terminal, and failing loudly beats
        // returning garbage.
        let falls_through = !diverges(&stmts);
        if self.decode {
            if falls_through {
                self.line(&format!("r->error_codec = \"{}\";", self.fn_name));
                self.line("r->error_detail = \"unreachable\";");
                self.line("return BPS_ERR_NO_MATCH;");
            }
            if self.need_s {
                self.label("fail");
                self.claim();
                let live = self.live.clone();
                for l in &live {
                    self.free_local(*l)?;
                }
                self.line("return s;");
            }
        } else {
            if falls_through {
                self.line("return BPS_OK;");
            }
            if self.need_s {
                self.label("fail");
                self.claim();
                self.line("return s;");
            }
        }
        if !self.hoist.is_empty() {
            let decls: String = self.hoist.iter().map(|d| format!("    {d}\n")).collect();
            self.code.insert_str(hoist_at, &decls);
        }
        Ok(())
    }

    /// A label, one level out from the statements around it.
    fn label(&mut self, name: &str) {
        self.ind -= 1;
        self.line(&format!("{name}:"));
        self.ind += 1;
    }

    fn decl_local(
        &mut self,
        l: LocalId,
        uses: &HashSet<LocalId>,
        addr: &HashSet<LocalId>,
    ) -> BpsResult<()> {
        let ty = self.body.locals[l].ty.clone();
        let c = self.nm.local[l].clone();
        if self.decode && owns_ty(self.p, &ty) {
            self.live.push(l);
        }
        match &ty {
            Ty::List(_) => {
                if let Some(h) = self.nm.has.get(&l).cloned() {
                    self.line(&format!("bool {h} = false;"));
                    self.voids.push(format!("(void){h};"));
                }
                let e = ctype(self.p, inner(&ty))?;
                self.line(&format!("{e} *{c} = NULL;"));
                let ln = self.nm.len[&l].clone();
                self.line(&format!("size_t {ln} = 0;"));
                self.voids.push(format!("(void){ln};"));
                if !uses.contains(&l) && !addr.contains(&l) {
                    self.voids.push(format!("(void){c};"));
                }
                if let Some(cp) = self.nm.cap.get(&l) {
                    self.line(&format!("size_t {cp} = 0;"));
                }
            }
            _ => {
                if self.body.locals[l].optional {
                    let h = self.nm.has[&l].clone();
                    self.line(&format!("bool {h} = false;"));
                    self.voids.push(format!("(void){h};"));
                }
                self.line(&format!(
                    "{} {c} = {};",
                    ctype(self.p, &ty)?,
                    zero_of(self.p, &ty)?
                ));
                if !uses.contains(&l) && !addr.contains(&l) {
                    self.voids.push(format!("(void){c};"));
                }
            }
        }
        Ok(())
    }

    /// A fallible call: `BPS_TRY` stores the status in `s` and jumps to the
    /// shared `fail:` label, which reports the codec once (a nested codec
    /// already did; helpers leave the fields alone for it to claim).
    fn try_call(&mut self, call: &str) {
        self.line(&format!("BPS_TRY({call});"));
    }

    /// The shared `fail:` label body: report this codec, but only when no
    /// nested codec already did (helpers leave the fields alone).
    fn claim(&mut self) {
        let rw = self.rw();
        self.line(&format!("if ({rw}->error_codec == NULL) {{"));
        self.ind += 1;
        self.line(&format!("{rw}->error_codec = \"{}\";", self.fn_name));
        self.line(&format!("if ({rw}->error_detail == NULL) {{"));
        self.ind += 1;
        self.line(&format!("{rw}->error_detail = bps_status_str(s);"));
        self.ind -= 1;
        self.line("}");
        self.ind -= 1;
        self.line("}");
    }

    /// A direct failure: set the status and the detail, then jump to the
    /// shared `fail:` label, which reports the codec.
    fn fail_direct(&mut self, status: &str, detail: &str) {
        let rw = self.rw();
        self.line(&format!("s = {status};"));
        self.line(&format!("{rw}->error_detail = {detail};"));
        self.line("goto fail;");
    }
}
// ----------------------------------------------------------------------
// Statements
// ----------------------------------------------------------------------

impl<'p> FnEmit<'p> {
    fn dec_stmts(&mut self, stmts: &[Stmt]) -> BpsResult<()> {
        for s in stmts {
            self.dec_stmt(s)?;
            note_stores(s, &mut self.stored);
        }
        Ok(())
    }

    fn dec_stmt(&mut self, s: &Stmt) -> BpsResult<()> {
        match s {
            Stmt::Declare { .. } => Ok(()),
            Stmt::Let { dst, value } => self.dec_let(dst, value),
            Stmt::Read { dst, op } => self.dec_read(dst, op),
            Stmt::Decode { dst, codec, args } => self.dec_decode(dst, *codec, args),
            Stmt::Utf8 { dst, src } => self.dec_utf8(dst, src),
            Stmt::Write { .. } => Err(emit("emit c: a write in a decode body")),
            Stmt::Encode { .. } => Err(emit("emit c: an encode in a decode body")),
            Stmt::Require { cond, text } => {
                let flipped = self.p.negated(self.body, cond);
                let c = self.expr(&flipped)?;
                self.line(&format!("if ({c}) {{"));
                self.ind += 1;
                self.fail_direct(
                    "BPS_ERR_REQUIRE",
                    &format!("\"require failed: {}\"", esc(text)),
                );
                self.ind -= 1;
                self.line("}");
                Ok(())
            }
            Stmt::Unwrap { dst, src, field } => self.dec_unwrap(dst, src, field),
            Stmt::If { cond, then, els } => {
                let c = self.expr(cond)?;
                // Each branch starts from what was stored before the `if`;
                // `dec_stmts` adds both branches' stores after it.
                let before = self.stored.clone();
                self.line(&format!("if ({c}) {{"));
                self.ind += 1;
                self.dec_stmts(then)?;
                self.ind -= 1;
                self.stored = before.clone();
                if els.is_empty() {
                    self.line("}");
                } else {
                    self.line("} else {");
                    self.ind += 1;
                    self.dec_stmts(els)?;
                    self.ind -= 1;
                    self.line("}");
                }
                self.stored = before;
                Ok(())
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => self.stmt_match(scrutinee, arms, other),
            Stmt::Repeat {
                mode,
                list,
                item,
                body,
            } => self.dec_repeat(mode, *list, *item, body),
            Stmt::ForEach { .. } => Err(emit("emit c: a for-each in a decode body")),
            Stmt::ExpectVariant { .. } => Err(emit("emit c: an expect-variant in a decode body")),
            Stmt::Return(e) => self.dec_return(e),
            Stmt::Fail(Failure::EncodeUntilEnd) => {
                self.fail_direct(
                    "BPS_ERR_UNTIL_END",
                    "\"encode not supported for repeat mode until_end\"",
                );
                Ok(())
            }
        }
    }

    fn dec_let(&mut self, dst: &Bind, value: &Expr) -> BpsResult<()> {
        let Bind { local, some, .. } = *dst;
        let lhs = self.nm.local[local].clone();
        if some {
            self.line(&format!("{} = true;", self.nm.has[&local]));
        }
        let ty = self.body.locals[local].ty.clone();
        if matches!(ty, Ty::Bytes)
            && let Expr::StrBytes(s) = value
        {
            let (d, n) = self.str_parts(s)?;
            self.line(&format!("{lhs}.data = bps_malloc(a, (size_t)({n}) + 1);"));
            self.line(&format!("if ({lhs}.data == NULL) {{"));
            self.ind += 1;
            self.fail_direct("BPS_ERR_NOMEM", "\"out of memory\"");
            self.ind -= 1;
            self.line("}");
            self.line(&format!("memcpy({lhs}.data, {d}, (size_t)({n}));"));
            self.line(&format!("{lhs}.data[({n})] = 0;"));
            self.line(&format!("{lhs}.len = (size_t)({n});"));
            return Ok(());
        }
        let rhs = self.expr(value)?;
        self.line(&format!("{lhs} = {rhs};"));
        if owns_ty(self.p, &ty)
            && let Expr::Local(l2) = value
            && owns_ty(self.p, &self.body.locals[*l2].ty)
            && *l2 != local
        {
            // A moved list carries its length to the destination before the
            // source is neutralized.
            if matches!(ty, Ty::List(_)) {
                let dl = self.len_name(&local)?;
                let sl = self.len_name(l2)?;
                self.line(&format!("{dl} = {sl};"));
            }
            let c2 = self.nm.local[*l2].clone();
            self.line(&format!("memset(&{c2}, 0, sizeof({c2}));"));
            // A moved list must also lose its length: the end-of-function
            // cleanup frees any live temp by its length.
            if matches!(self.body.locals[*l2].ty, Ty::List(_)) {
                self.line(&format!("{} = 0;", self.nm.len[l2]));
                if let Some(cp) = self.nm.cap.get(l2) {
                    self.line(&format!("{cp} = 0;"));
                }
            }
        }
        Ok(())
    }

    fn dec_read(&mut self, dst: &Bind, op: &ReadOp) -> BpsResult<()> {
        let Bind { local, some, .. } = *dst;
        let lhs = self.nm.local[local].clone();
        let call = match op {
            ReadOp::U8 => format!("bps_read_u8(r, &{lhs})"),
            ReadOp::F32 => format!("bps_read_f32(r, &{lhs})"),
            ReadOp::F64 => format!("bps_read_f64(r, &{lhs})"),
            ReadOp::Bytes(n) => format!("bps_read_bytes(r, a, {}, &{lhs})", self.expr(n)?),
            ReadOp::Rest => format!("bps_read_rest(r, a, &{lhs})"),
        };
        self.try_call(&call);
        if some {
            self.line(&format!("{} = true;", self.nm.has[&local]));
        }
        Ok(())
    }

    fn dec_decode(&mut self, dst: &Bind, codec: CodecIdx, args: &[Expr]) -> BpsResult<()> {
        let Bind { local, some, .. } = *dst;
        let lhs = self.nm.local[local].clone();
        let mut argv = vec!["r".to_string()];
        if self.dec_alloc[codec] {
            argv.push("a".to_string());
        }
        argv.extend(self.call_args(codec, args)?);
        argv.push(format!("&{lhs}"));
        self.try_call(&format!(
            "{}({})",
            decode_fn(self.p, codec),
            argv.join(", ")
        ));
        if some {
            self.line(&format!("{} = true;", self.nm.has[&local]));
        }
        Ok(())
    }

    fn dec_utf8(&mut self, dst: &Bind, src: &Expr) -> BpsResult<()> {
        let Expr::Local(l) = src else {
            return Err(emit("emit c: utf8 of a non-local"));
        };
        let Bind { local, some, .. } = *dst;
        let sc = self.nm.local[*l].clone();
        let dc = self.nm.local[local].clone();
        self.line(&format!("if (!bps_utf8_ok({sc}.data, {sc}.len)) {{"));
        self.ind += 1;
        self.fail_direct("BPS_ERR_UTF8", "\"invalid utf-8\"");
        self.ind -= 1;
        self.line("}");
        self.line(&format!("{dc}.data = (char *){sc}.data;"));
        self.line(&format!("{dc}.len = {sc}.len;"));
        self.line(&format!("{sc}.data = NULL;"));
        self.line(&format!("{sc}.len = 0;"));
        if some {
            self.line(&format!("{} = true;", self.nm.has[&local]));
        }
        Ok(())
    }

    fn dec_unwrap(&mut self, dst: &Bind, src: &Expr, field: &str) -> BpsResult<()> {
        let Expr::Local(l) = src else {
            return Err(emit("emit c: unwrap of a non-local in decode"));
        };
        let Bind { local, .. } = *dst;
        let h = self
            .nm
            .has
            .get(l)
            .cloned()
            .ok_or_else(|| emit("emit c: unwrap of a required value"))?;
        let sc = self.nm.local[*l].clone();
        let dc = self.nm.local[local].clone();
        self.line(&format!("if (!{h}) {{"));
        self.ind += 1;
        self.fail_direct("BPS_ERR_MISSING", &format!("\"missing `{}`\"", esc(field)));
        self.ind -= 1;
        self.line("}");
        self.line(&format!("{dc} = {sc};"));
        if matches!(self.body.locals[local].ty, Ty::List(_)) {
            let dl = self.len_name(&local)?;
            let sl = self.len_name(l)?;
            self.line(&format!("{dl} = {sl};"));
            self.line(&format!("{sl} = 0;"));
        }
        if owns_ty(self.p, &self.body.locals[local].ty) {
            self.line(&format!("memset(&{sc}, 0, sizeof({sc}));"));
            self.line(&format!("{h} = false;"));
        }
        Ok(())
    }

    fn stmt_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[crate::ir::typed::MatchArm],
        other: &Option<Vec<Stmt>>,
    ) -> BpsResult<()> {
        let s = self.expr(scrutinee)?;
        // Each arm starts from what was stored before the `switch`; the
        // statement loop adds every arm's stores after it.
        let before = self.stored.clone();
        self.line(&format!("switch ({s}) {{"));
        for arm in arms {
            let Some((last, rest)) = arm.tags.split_last() else {
                return Err(emit("emit c: a match arm without tags"));
            };
            for t in rest {
                self.line(&format!("case {t}:"));
            }
            self.line(&format!("case {last}: {{"));
            self.ind += 1;
            self.stored = before.clone();
            self.arm_body(&arm.body)?;
            self.ind -= 1;
            self.line("}");
        }
        self.stored = before.clone();
        match other {
            Some(o) => {
                self.line("default: {");
                self.ind += 1;
                self.arm_body(o)?;
                self.ind -= 1;
                self.line("}");
            }
            None => {
                self.line("default:");
                self.ind += 1;
                self.fail_direct("BPS_ERR_NO_MATCH", "\"no match arm for tag\"");
                self.ind -= 1;
            }
        }
        self.line("}");
        self.stored = before;
        Ok(())
    }

    /// A `case` body, with the `break` only when control can reach it.
    fn arm_body(&mut self, body: &[Stmt]) -> BpsResult<()> {
        if self.decode {
            self.dec_stmts(body)?;
        } else {
            self.enc_stmts(body)?;
        }
        if !diverges(body) {
            self.line("break;");
        }
        Ok(())
    }

    fn dec_repeat(
        &mut self,
        mode: &RepeatMode,
        list: LocalId,
        item: LocalId,
        body: &[Stmt],
    ) -> BpsResult<()> {
        match mode {
            RepeatMode::Count(n) => {
                let cnt = self.temp("count");
                let k = self.temp("k");
                let nv = self.expr(n)?;
                self.decl_temp("int64_t", &cnt, &nv);
                self.line(&format!("if ({cnt} < 0) {{"));
                self.ind += 1;
                self.fail_direct("BPS_ERR_NEG_LEN", "\"negative count\"");
                self.ind -= 1;
                self.line("}");
                self.for_temp("int64_t", &k, &cnt);
                self.ind += 1;
                self.dec_stmts(body)?;
                self.append(list, item)?;
                self.ind -= 1;
                self.line("}");
            }
            RepeatMode::Until(t) => {
                let pk = self.temp("peek");
                // A literal byte compares to the peeked `uint8_t` directly.
                let check = match byte_literal(t, true) {
                    Some(byte) => format!("{pk} == {byte}"),
                    None => {
                        let term = self.expr(t)?;
                        format!("(int64_t){pk} == {}", self.atom(t, term))
                    }
                };
                self.line("while (bps_remaining(r) > 0) {");
                self.ind += 1;
                self.decl_temp("uint8_t", &pk, "0");
                self.try_call(&format!("bps_peek_u8(r, &{pk})"));
                self.line(&format!("if ({check}) {{"));
                self.ind += 1;
                self.line("r->pos += 1;");
                self.line("break;");
                self.ind -= 1;
                self.line("}");
                self.dec_stmts(body)?;
                self.append(list, item)?;
                self.ind -= 1;
                self.line("}");
            }
            RepeatMode::UntilEnd => {
                self.line("while (bps_remaining(r) > 0) {");
                self.ind += 1;
                self.dec_stmts(body)?;
                self.append(list, item)?;
                self.ind -= 1;
                self.line("}");
            }
        }
        Ok(())
    }

    /// Grow `list` if full, copy `item` in, clear `item`.
    fn append(&mut self, list: LocalId, item: LocalId) -> BpsResult<()> {
        let lc = self.nm.local[list].clone();
        let ln = self.nm.len[&list].clone();
        let cp = self
            .nm
            .cap
            .get(&list)
            .cloned()
            .ok_or_else(|| emit("emit c: a list without capacity"))?;
        let ic = self.nm.local[item].clone();
        let elem = ctype(self.p, inner(&self.body.locals[list].ty.clone()))?;
        self.line(&format!("if ({ln} == {cp}) {{"));
        self.ind += 1;
        let ncap = self.temp("ncap");
        let np = self.temp("np");
        self.decl_temp("size_t", &ncap, &format!("{cp} != 0 ? {cp} * 2 : 4"));
        self.decl_temp(
            &format!("{elem} *"),
            &np,
            &format!("bps_malloc(a, {ncap} * sizeof *{np})"),
        );
        self.line(&format!("if ({np} == NULL) {{"));
        self.ind += 1;
        self.fail_direct("BPS_ERR_NOMEM", "\"out of memory\"");
        self.ind -= 1;
        self.line("}");
        self.line(&format!("if ({ln} != 0) {{"));
        self.ind += 1;
        self.line(&format!("memcpy({np}, {lc}, {ln} * sizeof *{np});"));
        self.ind -= 1;
        self.line("}");
        self.line(&format!("bps_free(a, {lc});"));
        self.line(&format!("{lc} = {np};"));
        self.line(&format!("{cp} = {ncap};"));
        self.ind -= 1;
        self.line("}");
        self.line(&format!("{lc}[{ln}] = {ic};"));
        self.line(&format!("{ln} += 1;"));
        self.line(&format!("memset(&{ic}, 0, sizeof({ic}));"));
        Ok(())
    }

    fn dec_return(&mut self, e: &Expr) -> BpsResult<()> {
        let mut consumed = HashSet::new();
        match e {
            Expr::Construct { ty, fields } => {
                // Boxed (pointer) members allocate first: a later NOMEM must
                // not strand ownership already moved into `out`.
                for (i, f) in fields.iter().enumerate() {
                    if !self.is_boxed(*ty, i) {
                        continue;
                    }
                    let Expr::Local(l) = f else {
                        return Err(emit("emit c: a record field built from a non-local"));
                    };
                    let m = self.mem(*ty, i);
                    let alloc = format!(
                        "out->{} = bps_malloc(a, sizeof(*out->{}));",
                        m.value, m.value
                    );
                    if self.p.fields(*ty)[i].optional {
                        let hl = self.nm.has[l].clone();
                        self.line(&format!("if ({hl}) {{"));
                        self.ind += 1;
                        self.line(&alloc);
                        self.line(&format!("if (out->{} == NULL) {{", m.value));
                        self.ind += 1;
                        self.fail_direct("BPS_ERR_NOMEM", "\"out of memory\"");
                        self.ind -= 1;
                        self.line("}");
                        self.ind -= 1;
                        self.line("} else {");
                        self.ind += 1;
                        self.line(&format!("out->{} = NULL;", m.value));
                        self.ind -= 1;
                        self.line("}");
                    } else {
                        self.line(&alloc);
                        self.line(&format!("if (out->{} == NULL) {{", m.value));
                        self.ind += 1;
                        self.fail_direct("BPS_ERR_NOMEM", "\"out of memory\"");
                        self.ind -= 1;
                        self.line("}");
                    }
                }
                for (i, f) in fields.iter().enumerate() {
                    let Expr::Local(l) = f else {
                        return Err(emit("emit c: a record field built from a non-local"));
                    };
                    let m = self.mem(*ty, i);
                    let lc = self.nm.local[*l].clone();
                    let opt = self.p.fields(*ty)[i].optional;
                    if opt {
                        let h = m
                            .has
                            .ok_or_else(|| emit("emit c: a field without presence"))?;
                        self.line(&format!("out->{h} = {};", self.nm.has[l]));
                    }
                    if self.is_boxed(*ty, i) {
                        if opt {
                            self.line(&format!("if ({}) {{", self.nm.has[l]));
                            self.ind += 1;
                            self.line(&format!("*out->{} = {lc};", m.value));
                            self.ind -= 1;
                            self.line("}");
                        } else {
                            self.line(&format!("*out->{} = {lc};", m.value));
                        }
                    } else {
                        self.line(&format!("out->{} = {lc};", m.value));
                        if matches!(self.p.fields(*ty)[i].ty, Ty::List(_)) {
                            let lm = m.len.ok_or_else(|| emit("emit c: a list without length"))?;
                            let ll = self.len_name(l)?;
                            self.line(&format!("out->{lm} = {ll};"));
                        }
                    }
                    consumed.insert(*l);
                }
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                self.line(&format!("out->tag = {};", tag_const(self.p, *ty, *variant)));
                if let Some(p) = payload {
                    let Expr::Local(l) = &**p else {
                        return Err(emit("emit c: a sum payload built from a non-local"));
                    };
                    let mem = escape(self.p.variants(*ty)[*variant].name.snake(), C_KEYWORDS);
                    self.line(&format!("out->value.{mem} = {};", self.nm.local[*l]));
                    consumed.insert(*l);
                }
            }
            _ => {
                let v = self.expr(e)?;
                self.line(&format!("*out = {v};"));
                locals_in(e, &mut consumed);
            }
        }
        // Only what some path here stored holds anything to free.
        let live = self.live.clone();
        for l in &live {
            if self.stored.contains(l) && !consumed.contains(l) {
                self.free_local(*l)?;
            }
        }
        self.line("return BPS_OK;");
        Ok(())
    }

    fn free_local(&mut self, l: LocalId) -> BpsResult<()> {
        let ty = self.body.locals[l].ty.clone();
        let c = self.nm.local[l].clone();
        let opt = self.body.locals[l].optional;
        if opt {
            self.line(&format!("if ({}) {{", self.nm.has[&l]));
            self.ind += 1;
        }
        match &ty {
            Ty::Str => self.line(&format!("bps_free_str(a, &{c});")),
            Ty::Bytes => self.line(&format!("bps_free_bytes(a, &{c});")),
            Ty::List(elem) => {
                if owns_ty(self.p, elem) {
                    let k = self.temp("k");
                    let ln = self.nm.len[&l].clone();
                    self.for_temp("size_t", &k, &ln);
                    self.ind += 1;
                    self.free_elem(elem, &format!("{c}[{k}]"))?;
                    self.ind -= 1;
                    self.line("}");
                }
                self.line(&format!("bps_free(a, {c});"));
            }
            Ty::Named(t) => {
                let r = match &self.p.types[*t].kind {
                    TypeKind::Alias { target } => *target,
                    _ => *t,
                };
                if !matches!(self.p.types[r].kind, TypeKind::Enum { .. }) {
                    self.line(&format!("{}(a, &{c});", free_fn(self.p, r)));
                }
            }
            _ => {}
        }
        if opt {
            self.ind -= 1;
            self.line("}");
        }
        Ok(())
    }

    fn free_elem(&mut self, ty: &Ty, target: &str) -> BpsResult<()> {
        match ty {
            Ty::Str => self.line(&format!("bps_free_str(a, {});", addr_of(target))),
            Ty::Bytes => self.line(&format!("bps_free_bytes(a, {});", addr_of(target))),
            Ty::Named(t) => {
                let r = match &self.p.types[*t].kind {
                    TypeKind::Alias { target } => *target,
                    _ => *t,
                };
                if !matches!(self.p.types[r].kind, TypeKind::Enum { .. }) {
                    self.line(&format!("{}(a, {});", free_fn(self.p, r), addr_of(target)));
                }
            }
            Ty::List(_) => return Err(emit("emit c: a nested list")),
            _ => {}
        }
        Ok(())
    }
}
impl<'p> FnEmit<'p> {
    fn enc_stmts(&mut self, stmts: &[Stmt]) -> BpsResult<()> {
        for s in stmts {
            self.enc_stmt(s)?;
        }
        Ok(())
    }

    fn enc_stmt(&mut self, s: &Stmt) -> BpsResult<()> {
        match s {
            Stmt::Declare { .. } => Ok(()),
            Stmt::Let { dst, value } => self.enc_let(dst, value),
            Stmt::Read { .. } | Stmt::Decode { .. } | Stmt::Utf8 { .. } => {
                Err(emit("emit c: a read in an encode body"))
            }
            Stmt::Write { op, value } => self.enc_write(op, value),
            Stmt::Encode { codec, args, value } => self.enc_encode(*codec, args, value),
            Stmt::Require { cond, text } => {
                let flipped = self.p.negated(self.body, cond);
                let c = self.expr(&flipped)?;
                self.line(&format!("if ({c}) {{"));
                self.ind += 1;
                self.fail_direct(
                    "BPS_ERR_REQUIRE",
                    &format!("\"require failed: {}\"", esc(text)),
                );
                self.ind -= 1;
                self.line("}");
                Ok(())
            }
            Stmt::Unwrap { dst, src, field } => self.enc_unwrap(dst, src, field),
            Stmt::If { cond, then, els } => {
                let c = self.expr(cond)?;
                self.line(&format!("if ({c}) {{"));
                self.ind += 1;
                self.enc_stmts(then)?;
                self.ind -= 1;
                if els.is_empty() {
                    self.line("}");
                } else {
                    self.line("} else {");
                    self.ind += 1;
                    self.enc_stmts(els)?;
                    self.ind -= 1;
                    self.line("}");
                }
                Ok(())
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => self.stmt_match(scrutinee, arms, other),
            Stmt::Repeat { .. } => Err(emit("emit c: a repeat in an encode body")),
            Stmt::ForEach { list, item, body } => self.enc_foreach(list, *item, body),
            Stmt::ExpectVariant {
                dst,
                sum,
                ty,
                variant,
            } => self.enc_expect(dst, sum, *ty, *variant),
            Stmt::Return(_) => {
                self.line("return BPS_OK;");
                Ok(())
            }
            Stmt::Fail(Failure::EncodeUntilEnd) => {
                self.fail_direct(
                    "BPS_ERR_UNTIL_END",
                    "\"encode not supported for repeat mode until_end\"",
                );
                Ok(())
            }
        }
    }

    /// Encode binds borrow: values are read through the caller's memory and
    /// never freed here.
    fn enc_let(&mut self, dst: &Bind, value: &Expr) -> BpsResult<()> {
        let Bind { local, some, .. } = *dst;
        let lhs = self.nm.local[local].clone();
        if some {
            self.line(&format!("{} = true;", self.nm.has[&local]));
        }
        let ty = self.body.locals[local].ty.clone();
        if matches!(ty, Ty::Bytes)
            && let Expr::StrBytes(s) = value
        {
            let (d, n) = self.str_parts(s)?;
            self.line(&format!("{lhs}.data = (uint8_t *){d};"));
            self.line(&format!("{lhs}.len = {n};"));
            return Ok(());
        }
        let rhs = self.expr(value)?;
        self.line(&format!("{lhs} = {rhs};"));
        if matches!(ty, Ty::List(_))
            && matches!(value, Expr::Local(_) | Expr::Field { .. } | Expr::Get(_))
        {
            let dl = self.nm.len[&local].clone();
            let sl = self.len_of(value)?;
            self.line(&format!("{dl} = {sl};"));
        }
        Ok(())
    }

    fn enc_write(&mut self, op: &WriteOp, value: &Expr) -> BpsResult<()> {
        let call = match op {
            WriteOp::U8 => {
                let v = match byte_literal(value, true) {
                    Some(byte) => byte,
                    None => self.expr(value)?,
                };
                format!("bps_write_u8(w, a, {v})")
            }
            WriteOp::F32 => {
                let v = self.expr(value)?;
                format!("bps_write_f32(w, a, {v})")
            }
            WriteOp::F64 => {
                let v = self.expr(value)?;
                format!("bps_write_f64(w, a, {v})")
            }
            WriteOp::Bytes(n) => {
                let total = self.expr(n)?;
                let (d, ln) = self.bytes_parts(value)?;
                format!("bps_write_bytes_pad(w, a, {d}, {ln}, {total})")
            }
            WriteOp::Rest => {
                let (d, ln) = self.bytes_parts(value)?;
                format!("bps_write_bytes(w, a, {d}, {ln})")
            }
        };
        self.try_call(&call);
        Ok(())
    }

    fn enc_encode(&mut self, codec: CodecIdx, args: &[Expr], value: &Expr) -> BpsResult<()> {
        let mut argv = vec!["w".to_string(), "a".to_string()];
        argv.extend(self.call_args(codec, args)?);
        argv.push(self.encode_ref(value)?);
        self.try_call(&format!(
            "{}({})",
            encode_fn(self.p, codec),
            argv.join(", ")
        ));
        Ok(())
    }

    /// How the encode input passes to a nested call: scalars and enums by
    /// value, owned values by const pointer.
    fn encode_ref(&mut self, value: &Expr) -> BpsResult<String> {
        let (ty, _) = self.p.expr_ty(self.body, value);
        match value {
            Expr::Local(l) if self.nm.ptr.contains(l) => {
                if is_enum_ty(self.p, &ty) {
                    Ok(format!("(*{})", self.nm.local[*l]))
                } else {
                    Ok(self.nm.local[*l].clone())
                }
            }
            // A boxed member is already a `T *`; pass it straight through.
            Expr::Field { ty, field, .. } if self.is_boxed(*ty, *field) => self.field_full(value),
            _ if is_scalar(&ty) || is_enum_ty(self.p, &ty) => self.expr(value),
            _ => Ok(addr_of(&self.place(value)?)),
        }
    }

    fn enc_unwrap(&mut self, dst: &Bind, src: &Expr, field: &str) -> BpsResult<()> {
        let Bind { local, .. } = *dst;
        let h = self.has_of(src)?;
        let v = self.val_of(src)?;
        let dc = self.nm.local[local].clone();
        self.line(&format!("if (!{h}) {{"));
        self.ind += 1;
        self.fail_direct("BPS_ERR_MISSING", &format!("\"missing `{}`\"", esc(field)));
        self.ind -= 1;
        self.line("}");
        self.line(&format!("{dc} = {v};"));
        // Unwrapping a list must also carry its length.
        if matches!(self.body.locals[local].ty, Ty::List(_)) {
            let dl = self.len_name(&local)?;
            let sl = self.len_of(src)?;
            self.line(&format!("{dl} = {sl};"));
        }
        Ok(())
    }

    fn enc_foreach(&mut self, list: &Expr, item: LocalId, body: &[Stmt]) -> BpsResult<()> {
        let lv = self.list_ptr(list)?;
        let ln = self.len_of(list)?;
        let ic = self.nm.local[item].clone();
        let k = self.temp("k");
        self.for_temp("size_t", &k, &ln);
        self.ind += 1;
        self.line(&format!("{ic} = {lv}[{k}];"));
        self.enc_stmts(body)?;
        self.ind -= 1;
        self.line("}");
        Ok(())
    }

    fn enc_expect(
        &mut self,
        dst: &Option<Bind>,
        sum: &Expr,
        ty: TypeIdx,
        variant: usize,
    ) -> BpsResult<()> {
        let Expr::Local(l) = sum else {
            return Err(emit("emit c: expect-variant of a non-local"));
        };
        let c = self.nm.local[*l].clone();
        let (tag, pay) = if self.nm.ptr.contains(l) {
            (format!("{c}->tag"), format!("{c}->value"))
        } else {
            (format!("{c}.tag"), format!("{c}.value"))
        };
        self.line(&format!(
            "if ({tag} != {}) {{",
            tag_const(self.p, ty, variant)
        ));
        self.ind += 1;
        self.fail_direct("BPS_ERR_VARIANT", "\"wrong variant\"");
        self.ind -= 1;
        self.line("}");
        if let Some(d) = dst {
            let mem = escape(self.p.variants(ty)[variant].name.snake(), C_KEYWORDS);
            self.line(&format!("{} = {pay}.{mem};", self.nm.local[d.local]));
        }
        Ok(())
    }
}

// ----------------------------------------------------------------------
// Expressions
// ----------------------------------------------------------------------

impl<'p> FnEmit<'p> {
    fn expr(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Int { value, .. } => Ok(value.to_string()),
            Expr::Bool(b) => Ok(b.to_string()),
            Expr::Str(_) => Err(emit("emit c: a bare string literal")),
            Expr::Local(l) => Ok(self.nm.local[*l].clone()),
            Expr::Field { .. } => self.field_full(e),
            Expr::Present(p) => self.has_of(p),
            Expr::Get(g) => self.val_of(g),
            Expr::Not(x) => {
                let s = self.expr(x)?;
                Ok(match **x {
                    Expr::Binary { .. } | Expr::Cond { .. } => format!("!({s})"),
                    _ => format!("!{s}"),
                })
            }
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } => self.binary(*op, lhs, rhs, ty, *wraps),
            Expr::Cond {
                cond,
                then,
                els,
                ty,
            } => {
                let c = self.expr(cond)?;
                if matches!(ty, Ty::Str) {
                    Ok(format!(
                        "({c} ? {} : {})",
                        self.str_val(then)?,
                        self.str_val(els)?
                    ))
                } else {
                    Ok(format!(
                        "({c} ? {} : {})",
                        self.expr(then)?,
                        self.expr(els)?
                    ))
                }
            }
            Expr::Cast { expr: inner, to } => self.cast(inner, to),
            Expr::Call { func, args } => self.call(*func, args),
            Expr::StrBytes(_) => Err(emit("emit c: string bytes outside a byte write")),
            Expr::Remaining => {
                if self.decode {
                    Ok("bps_remaining(r)".to_string())
                } else {
                    Err(emit("emit c: bytes remaining in an encode body"))
                }
            }
            Expr::Construct { .. } | Expr::Variant { .. } => {
                Err(emit("emit c: a value outside return"))
            }
        }
    }

    fn binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ty: &Ty,
        wraps: bool,
    ) -> BpsResult<String> {
        if op.is_logical() {
            return Ok(format!(
                "{} {} {}",
                self.operand(lhs, op, false)?,
                op.symbol(),
                self.operand(rhs, op, true)?
            ));
        }
        if op.is_comparison() {
            let (lt, _) = self.p.expr_ty(self.body, lhs);
            let (rt, _) = self.p.expr_ty(self.body, rhs);
            if lt == Ty::Str && rt == Ty::Str {
                let a = self.str_val(lhs)?;
                let b = self.str_val(rhs)?;
                if op == BinOp::Eq {
                    return Ok(format!("bps_str_eq({a}, {b})"));
                }
                return Ok(format!("!bps_str_eq({a}, {b})"));
            }
            return Ok(format!(
                "{} {} {}",
                self.operand(lhs, op, false)?,
                op.symbol(),
                self.operand(rhs, op, true)?
            ));
        }
        if is_bitwise(op) {
            let l = self.bitwise_operand(lhs, op, false)?;
            let r = self.bitwise_operand(rhs, op, true)?;
            return Ok(format!("{l} {} {r}", op.symbol()));
        }
        let l = self.operand(lhs, op, false)?;
        let r = self.operand(rhs, op, true)?;
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                if wraps {
                    Ok(format!(
                        "(int64_t)((uint64_t)({l}) {} (uint64_t)({r}))",
                        op.symbol()
                    ))
                } else {
                    Ok(format!("{l} {} {r}", op.symbol()))
                }
            }
            BinOp::Div => Ok(format!("bps_div_floor((int64_t)({l}), (int64_t)({r}))")),
            BinOp::Mod => Ok(format!("bps_mod_sign((int64_t)({l}), (int64_t)({r}))")),
            BinOp::Shl => {
                if wraps {
                    Ok(format!("(int64_t)((uint64_t)({l}) << (uint64_t)({r}))"))
                } else {
                    Ok(format!("{l} << {r}"))
                }
            }
            BinOp::Shr => {
                let signed = matches!(ty, Ty::Int(t) if t.signed());
                let neg = self
                    .p
                    .int_range(self.body, lhs)
                    .is_none_or(|(lo, _)| lo < 0);
                if signed && neg {
                    Ok(format!("bps_shr_arith((int64_t)({l}), (int64_t)({r}))"))
                } else {
                    Ok(format!("{l} >> {r}"))
                }
            }
            _ => Ok(format!("{l} {} {r}", op.symbol())),
        }
    }

    /// Render `child` as an operand of binary `parent`: parentheses only
    /// where C precedence needs them. Mixed `&`/`|`/`^` and `&&` inside
    /// `||` keep theirs: the strict gate (`-Wparentheses`) rejects the bare
    /// mix even though it parses unambiguously.
    fn operand(&mut self, child: &Expr, parent: BinOp, right: bool) -> BpsResult<String> {
        let s = self.expr(child)?;
        let Expr::Binary { op: cop, .. } = child else {
            // Calls, casts, and atoms bind tighter than every binary
            // operator; ternaries bind looser than all of them.
            return Ok(if matches!(child, Expr::Cond { .. }) {
                format!("({s})")
            } else {
                s
            });
        };
        let mixed_bitwise = is_bitwise(*cop) && is_bitwise(parent) && *cop != parent;
        let and_in_or = parent == BinOp::Or && *cop == BinOp::And;
        let need = if mixed_bitwise || and_in_or {
            true
        } else if right {
            prec(*cop) <= prec(parent)
        } else {
            prec(*cop) < prec(parent)
        };
        Ok(if need { format!("({s})") } else { s })
    }

    /// A hand-written operand in a comparison the printer builds itself
    /// (the repeat-until terminator): parenthesize only a nested binary or
    /// ternary.
    fn atom(&self, e: &Expr, s: String) -> String {
        match e {
            Expr::Binary { .. } | Expr::Cond { .. } => format!("({s})"),
            _ => s,
        }
    }

    /// An operand of `&`, `|`, `^`: masks print hex, and a flag ternary
    /// (`cond ? 128 : 0`) prints its set-bit branch hex too, as in the
    /// guide's `((v >> 7) != 0 ? 0x80 : 0)`.
    fn bitwise_operand(&mut self, child: &Expr, parent: BinOp, right: bool) -> BpsResult<String> {
        if let Expr::Cond {
            cond,
            then,
            els,
            ty,
        } = child
            && !matches!(ty, Ty::Str)
        {
            let c = self.expr(cond)?;
            let t = self.flag_branch(then)?;
            let e = self.flag_branch(els)?;
            return Ok(format!("({c} ? {t} : {e})"));
        }
        Ok(hex_mask(parent, child, self.operand(child, parent, right)?))
    }

    /// A flag-bit ternary branch: a set-bit constant prints hex.
    fn flag_branch(&mut self, e: &Expr) -> BpsResult<String> {
        if let Expr::Int { value, .. } = e
            && *value > 9
        {
            return Ok(format!("0x{value:X}"));
        }
        self.expr(e)
    }

    fn cast(&mut self, inner: &Expr, to: &Ty) -> BpsResult<String> {
        let (from, _) = self.p.expr_ty(self.body, inner);
        let v = self.expr(inner)?;
        // A cast binds tighter than every binary operator: only binary and
        // conditional operands need parentheses.
        let v = if matches!(inner, Expr::Binary { .. } | Expr::Cond { .. }) {
            format!("({v})")
        } else {
            v
        };
        let range = self.p.int_range(self.body, inner);
        // A plain cast when the target holds every value the operand can
        // take; a signed target that may not converts through its unsigned
        // twin, where C defines the wrap.
        let conv = |s: IntTy, t: IntTy, v: String| {
            let (lo, hi) = range.unwrap_or((s.min_value(), s.max_value()));
            if s == t {
                v
            } else if t.signed() && !(t.contains(lo) && t.contains(hi)) {
                format!("({})({}){v}", int_c(t), uint_of(t))
            } else {
                format!("({}){v}", int_c(t))
            }
        };
        match (&from, to) {
            (Ty::Int(s), Ty::Int(t)) => Ok(conv(*s, *t, v)),
            (Ty::Bool, Ty::Int(t)) => Ok(format!("({}){v}", int_c(*t))),
            (Ty::Named(a), Ty::Int(t)) => match enum_base(self.p, *a) {
                Some(b) => Ok(conv(b, *t, v)),
                None => Err(emit("emit c: cannot cast a record to an integer")),
            },
            (Ty::Int(s), Ty::Named(b)) => match enum_base(self.p, *b) {
                Some(t) => Ok(conv(*s, t, v)),
                None => Err(emit("emit c: cannot cast an integer to a record")),
            },
            (Ty::Named(a), Ty::Named(b)) => match (enum_base(self.p, *a), enum_base(self.p, *b)) {
                (Some(x), Some(y)) => Ok(conv(x, y, v)),
                _ => Err(emit("emit c: cannot cast between records")),
            },
            (Ty::F32, Ty::F64) => Ok(format!("(double){v}")),
            (Ty::F64, Ty::F32) => Ok(format!("(float){v}")),
            _ => Err(emit("emit c: cannot cast here")),
        }
    }

    fn call(&mut self, func: Func, args: &[Expr]) -> BpsResult<String> {
        match func {
            Func::SignExtend => Ok(format!(
                "bps_sign_extend({}, {})",
                self.expr(&args[0])?,
                self.expr(&args[1])?
            )),
            Func::Utf16Units => {
                let (d, n) = self.str_parts(&args[0])?;
                Ok(format!("bps_utf16_units({d}, {n})"))
            }
            Func::Utf8Len => Ok(self.str_parts(&args[0])?.1),
            Func::StrLen => {
                let (d, n) = self.str_parts(&args[0])?;
                Ok(format!("bps_str_len({d}, {n})"))
            }
            Func::BytesLen => Ok(self.bytes_parts(&args[0])?.1),
            Func::ListLen => self.len_of(&args[0]),
            Func::Min => Ok(format!(
                "bps_min_i64({}, {})",
                self.expr(&args[0])?,
                self.expr(&args[1])?
            )),
            Func::Max => Ok(format!(
                "bps_max_i64({}, {})",
                self.expr(&args[0])?,
                self.expr(&args[1])?
            )),
        }
    }

    /// Field access with `->` past a pointer base, `.` otherwise.
    fn acc(&mut self, e: &Expr) -> BpsResult<(String, bool)> {
        match e {
            Expr::Local(l) => Ok((self.nm.local[*l].clone(), self.nm.ptr.contains(l))),
            Expr::Get(x) => {
                let (s, p) = self.acc(x)?;
                if p {
                    Ok((format!("(*{s})"), false))
                } else {
                    Ok((s, false))
                }
            }
            Expr::Field { ty, field, .. } => {
                let f = self.field_full(e)?;
                // A boxed member is a pointer, so outer access uses `->`.
                Ok((f, self.is_boxed(*ty, *field)))
            }
            _ => Ok((self.expr(e)?, false)),
        }
    }

    fn field_full(&mut self, e: &Expr) -> BpsResult<String> {
        let Expr::Field { base, ty, field } = e else {
            return Err(emit("emit c: not a field"));
        };
        let (b, p) = self.acc(base)?;
        let f = self.mem(*ty, *field).value;
        Ok(if p {
            format!("{b}->{f}")
        } else {
            format!("{b}.{f}")
        })
    }

    fn has_of(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Local(l) => self
                .nm
                .has
                .get(l)
                .cloned()
                .ok_or_else(|| emit("emit c: presence of a required value")),
            Expr::Field { base, ty, field } => {
                let (b, p) = self.acc(base)?;
                let h = self
                    .mem(*ty, *field)
                    .has
                    .ok_or_else(|| emit("emit c: presence of a required field"))?;
                Ok(if p {
                    format!("{b}->{h}")
                } else {
                    format!("{b}.{h}")
                })
            }
            Expr::Get(x) => self.has_of(x),
            _ => Err(emit("emit c: presence of this value")),
        }
    }

    /// The value of an optional already known to be present.
    fn val_of(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Local(l) => {
                let c = self.nm.local[*l].clone();
                Ok(if self.nm.ptr.contains(l) {
                    format!("(*{c})")
                } else {
                    c
                })
            }
            Expr::Field { ty, field, .. } => {
                let f = self.field_full(e)?;
                // A boxed member is a pointer; the value is what it points to.
                Ok(if self.is_boxed(*ty, *field) {
                    format!("(*{f})")
                } else {
                    f
                })
            }
            Expr::Get(x) => self.val_of(x),
            _ => self.expr(e),
        }
    }

    /// An addressable place for `&`.
    fn place(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Local(l) => {
                if self.nm.ptr.contains(l) {
                    return Err(emit("emit c: address of a pointer"));
                }
                Ok(self.nm.local[*l].clone())
            }
            Expr::Field { ty, field, .. } => {
                let f = self.field_full(e)?;
                // A boxed member is a pointer; `(*p)` stays addressable, and
                // `&(*p)` is just `p`, so callers keep working unchanged.
                Ok(if self.is_boxed(*ty, *field) {
                    format!("(*{f})")
                } else {
                    f
                })
            }
            Expr::Get(x) => self.place(x),
            _ => Err(emit("emit c: address of a temporary")),
        }
    }

    fn list_ptr(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Local(l) => Ok(self.nm.local[*l].clone()),
            Expr::Field { .. } => self.field_full(e),
            Expr::Get(x) => self.list_ptr(x),
            _ => Err(emit("emit c: a list from a temporary")),
        }
    }

    /// The length variable of a list local.
    fn len_name(&self, id: &LocalId) -> BpsResult<String> {
        self.nm
            .len
            .get(id)
            .cloned()
            .ok_or_else(|| emit("emit c: a list without length"))
    }

    fn len_of(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Local(l) => self
                .nm
                .len
                .get(l)
                .cloned()
                .ok_or_else(|| emit("emit c: length of a non-list")),
            Expr::Field { base, ty, field } => {
                let (b, p) = self.acc(base)?;
                let ln = self
                    .mem(*ty, *field)
                    .len
                    .ok_or_else(|| emit("emit c: length of a non-list field"))?;
                Ok(if p {
                    format!("{b}->{ln}")
                } else {
                    format!("{b}.{ln}")
                })
            }
            Expr::Get(x) => self.len_of(x),
            _ => Err(emit("emit c: length of this value")),
        }
    }

    /// The `(data, len)` of a string: literals decay, pointers dereference.
    fn str_parts(&mut self, e: &Expr) -> BpsResult<(String, String)> {
        match e {
            Expr::Local(l) => {
                let c = self.nm.local[*l].clone();
                if self.nm.ptr.contains(l) {
                    Ok((format!("(const uint8_t *){c}->data"), format!("{c}->len")))
                } else {
                    Ok((format!("(const uint8_t *){c}.data"), format!("{c}.len")))
                }
            }
            Expr::Field { .. } => {
                let f = self.field_full(e)?;
                Ok((format!("(const uint8_t *){f}.data"), format!("{f}.len")))
            }
            Expr::Get(x) => self.str_parts(x),
            Expr::Str(s) => Ok((
                format!("(const uint8_t *)\"{}\"", esc(s)),
                s.len().to_string(),
            )),
            _ => Err(emit("emit c: string bytes of this value")),
        }
    }

    /// The `(data, len)` of bytes.
    fn bytes_parts(&mut self, e: &Expr) -> BpsResult<(String, String)> {
        match e {
            Expr::Local(l) => {
                let c = self.nm.local[*l].clone();
                if self.nm.ptr.contains(l) {
                    Ok((format!("{c}->data"), format!("{c}->len")))
                } else {
                    Ok((format!("{c}.data"), format!("{c}.len")))
                }
            }
            Expr::Field { .. } => {
                let f = self.field_full(e)?;
                Ok((format!("{f}.data"), format!("{f}.len")))
            }
            Expr::Get(x) => self.bytes_parts(x),
            Expr::StrBytes(s) => self.str_parts(s),
            _ => Err(emit("emit c: bytes of this value")),
        }
    }

    /// A string as a value: locals and fields pass through, literals become
    /// compound literals.
    fn str_val(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Local(l) => {
                let c = self.nm.local[*l].clone();
                Ok(if self.nm.ptr.contains(l) {
                    format!("(*{c})")
                } else {
                    c
                })
            }
            Expr::Field { .. } => self.field_full(e),
            Expr::Get(x) => self.str_val(x),
            Expr::Str(s) => {
                if self.c89() {
                    Ok(format!("bps_make_str(\"{}\", {})", esc(s), s.len()))
                } else {
                    Ok(format!("(bps_str){{\"{}\", {}}}", esc(s), s.len()))
                }
            }
            _ => Err(emit("emit c: a string value here")),
        }
    }
}

// ----------------------------------------------------------------------
// Static analysis over bodies
// ----------------------------------------------------------------------

fn enum_base(program: &Program, ty: TypeIdx) -> Option<IntTy> {
    match &program.types[ty].kind {
        TypeKind::Enum { base, .. } => Some(*base),
        TypeKind::Alias { target } => enum_base(program, *target),
        _ => None,
    }
}

fn locals_in(e: &Expr, out: &mut HashSet<LocalId>) {
    expr_uses(e, out);
}

fn expr_uses(e: &Expr, out: &mut HashSet<LocalId>) {
    match e {
        Expr::Local(l) => {
            out.insert(*l);
        }
        Expr::Field { base, .. }
        | Expr::Present(base)
        | Expr::Get(base)
        | Expr::Not(base)
        | Expr::StrBytes(base) => expr_uses(base, out),
        Expr::Cast { expr: x, .. } => expr_uses(x, out),
        Expr::Binary { lhs, rhs, .. } => {
            expr_uses(lhs, out);
            expr_uses(rhs, out);
        }
        Expr::Cond {
            cond, then, els, ..
        } => {
            expr_uses(cond, out);
            expr_uses(then, out);
            expr_uses(els, out);
        }
        Expr::Call { args, .. } => {
            for a in args {
                expr_uses(a, out);
            }
        }
        Expr::Construct { fields, .. } => {
            for f in fields {
                expr_uses(f, out);
            }
        }
        Expr::Variant {
            payload: Some(p), ..
        } => {
            expr_uses(p, out);
        }
        _ => {}
    }
}

fn stmts_uses(stmts: &[Stmt], out: &mut HashSet<LocalId>) {
    for s in stmts {
        match s {
            Stmt::Declare { .. } | Stmt::Fail(_) => {}
            Stmt::Let { value, .. } => expr_uses(value, out),
            Stmt::Read { op, .. } => {
                if let ReadOp::Bytes(n) = op {
                    expr_uses(n, out);
                }
            }
            Stmt::Decode { args, .. } => {
                for a in args {
                    expr_uses(a, out);
                }
            }
            Stmt::Utf8 { src, .. } => expr_uses(src, out),
            Stmt::Write { op, value } => {
                if let WriteOp::Bytes(n) = op {
                    expr_uses(n, out);
                }
                expr_uses(value, out);
            }
            Stmt::Encode { args, value, .. } => {
                for a in args {
                    expr_uses(a, out);
                }
                expr_uses(value, out);
            }
            Stmt::Require { cond, .. } => expr_uses(cond, out),
            Stmt::Unwrap { src, .. } => expr_uses(src, out),
            Stmt::If { cond, then, els } => {
                expr_uses(cond, out);
                stmts_uses(then, out);
                stmts_uses(els, out);
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => {
                expr_uses(scrutinee, out);
                for a in arms {
                    stmts_uses(&a.body, out);
                }
                if let Some(o) = other {
                    stmts_uses(o, out);
                }
            }
            Stmt::Repeat {
                mode, item, body, ..
            } => {
                if let RepeatMode::Count(e) | RepeatMode::Until(e) = mode {
                    expr_uses(e, out);
                }
                out.insert(*item);
                stmts_uses(body, out);
            }
            Stmt::ForEach { list, body, .. } => {
                expr_uses(list, out);
                stmts_uses(body, out);
            }
            Stmt::ExpectVariant { sum, .. } => expr_uses(sum, out),
            Stmt::Return(e) => expr_uses(e, out),
        }
    }
}

fn local_uses(body: &Body) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    stmts_uses(&body.stmts, &mut out);
    out
}

/// Locals stored through a pointer (decode outputs land here).
fn addr_taken(stmts: &[Stmt]) -> HashSet<LocalId> {
    let mut out = HashSet::new();
    addr_in(stmts, &mut out);
    out
}

fn addr_in(stmts: &[Stmt], out: &mut HashSet<LocalId>) {
    for s in stmts {
        match s {
            Stmt::Read { dst, .. } | Stmt::Decode { dst, .. } => {
                out.insert(dst.local);
            }
            Stmt::If { then, els, .. } => {
                addr_in(then, out);
                addr_in(els, out);
            }
            Stmt::Match { arms, other, .. } => {
                for a in arms {
                    addr_in(&a.body, out);
                }
                if let Some(o) = other {
                    addr_in(o, out);
                }
            }
            Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => addr_in(body, out),
            _ => {}
        }
    }
}

/// `&target` for an lvalue, parenthesized only when `target` has an operator
/// at its top level (the printer spaces binary operators): member access,
/// subscripts, and `*` all bind tighter than `&`.
fn addr_of(target: &str) -> String {
    let mut depth = 0i32;
    let compound = target.chars().any(|c| {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            _ => {}
        }
        c == ' ' && depth == 0
    });
    if compound {
        format!("&({target})")
    } else {
        format!("&{target}")
    }
}

/// Add every local `s` stores, in any branch or loop body, to `out`.
fn note_stores(s: &Stmt, out: &mut HashSet<LocalId>) {
    match s {
        Stmt::Let { dst, .. }
        | Stmt::Read { dst, .. }
        | Stmt::Decode { dst, .. }
        | Stmt::Utf8 { dst, .. }
        | Stmt::Unwrap { dst, .. }
        | Stmt::ExpectVariant { dst: Some(dst), .. } => {
            out.insert(dst.local);
        }
        Stmt::If { then, els, .. } => {
            then.iter().chain(els).for_each(|s| note_stores(s, out));
        }
        Stmt::Match { arms, other, .. } => {
            arms.iter()
                .flat_map(|a| &a.body)
                .chain(other.iter().flatten())
                .for_each(|s| note_stores(s, out));
        }
        Stmt::Repeat {
            list, item, body, ..
        } => {
            out.insert(*list);
            out.insert(*item);
            body.iter().for_each(|s| note_stores(s, out));
        }
        Stmt::ForEach { item, body, .. } => {
            out.insert(*item);
            body.iter().for_each(|s| note_stores(s, out));
        }
        Stmt::Declare { .. }
        | Stmt::Write { .. }
        | Stmt::Encode { .. }
        | Stmt::Require { .. }
        | Stmt::ExpectVariant { dst: None, .. }
        | Stmt::Return(_)
        | Stmt::Fail(_) => {}
    }
}

/// Whether the body needs a status variable and a `fail:` label. Every
/// counted statement emits a use of `s` paired with `goto fail`, so a
/// counted body always jumps to the label at least once.
fn need_s(stmts: &[Stmt], decode: bool) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Declare { .. } | Stmt::Let { .. } | Stmt::Return(_) => false,
        Stmt::Read { .. }
        | Stmt::Decode { .. }
        | Stmt::Utf8 { .. }
        | Stmt::Write { .. }
        | Stmt::Encode { .. }
        | Stmt::Require { .. }
        | Stmt::Unwrap { .. }
        | Stmt::ExpectVariant { .. }
        | Stmt::Fail(_) => true,
        Stmt::Repeat { .. } => decode,
        Stmt::If { then, els, .. } => need_s(then, decode) || need_s(els, decode),
        Stmt::Match { arms, other, .. } => {
            arms.iter().any(|a| need_s(&a.body, decode))
                || other.as_ref().is_none_or(|o| need_s(o, decode))
        }
        Stmt::ForEach { body, .. } => need_s(body, decode),
    })
}
/// Whether a codec's decode function takes the allocator (for tests building
/// calls through [`decode_fn`]).
pub fn decode_takes_alloc(program: &Program, codec: CodecIdx) -> bool {
    alloc_needs(program, &compute_boxed(program))[codec]
}

/// Lists never nest: every list is a pointer plus a length, one level only.
fn no_nested_lists(program: &Program) -> BpsResult<()> {
    fn bad(ty: &Ty) -> bool {
        match ty {
            Ty::List(t) => matches!(**t, Ty::List(_)),
            _ => false,
        }
    }
    for decl in &program.types {
        match &decl.kind {
            TypeKind::Record { fields } => {
                for f in fields {
                    if bad(&f.ty) {
                        return Err(emit("emit c: a nested list"));
                    }
                }
            }
            TypeKind::Sum { variants } => {
                for v in variants {
                    if v.payload.as_ref().is_some_and(bad) {
                        return Err(emit("emit c: a nested list"));
                    }
                }
            }
            _ => {}
        }
    }
    for codec in &program.codecs {
        for param in &codec.params {
            if bad(&param.ty) {
                return Err(emit("emit c: a nested list"));
            }
        }
        for body in [&codec.decode, &codec.encode] {
            for l in &body.locals {
                if bad(&l.ty) {
                    return Err(emit("emit c: a nested list"));
                }
            }
        }
    }
    Ok(())
}

fn has_strbytes(e: &Expr) -> bool {
    match e {
        Expr::StrBytes(_) => true,
        Expr::Field { base, .. } | Expr::Present(base) | Expr::Get(base) | Expr::Not(base) => {
            has_strbytes(base)
        }
        Expr::Cast { expr: x, .. } => has_strbytes(x),
        Expr::Binary { lhs, rhs, .. } => has_strbytes(lhs) || has_strbytes(rhs),
        Expr::Cond {
            cond, then, els, ..
        } => has_strbytes(cond) || has_strbytes(then) || has_strbytes(els),
        Expr::Call { args, .. } => args.iter().any(has_strbytes),
        Expr::Construct { fields, .. } => fields.iter().any(has_strbytes),
        Expr::Variant { payload, .. } => payload.as_ref().is_some_and(|p| has_strbytes(p)),
        _ => false,
    }
}
/// Whether an encode body touches the writer or allocator at all; if not,
/// both stay named but voided.
fn touches_w(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Declare { .. } | Stmt::Let { .. } | Stmt::ExpectVariant { .. } | Stmt::Return(_) => {
            false
        }
        Stmt::Write { .. }
        | Stmt::Encode { .. }
        | Stmt::Require { .. }
        | Stmt::Unwrap { .. }
        | Stmt::Fail(_) => true,
        Stmt::Read { .. } | Stmt::Decode { .. } | Stmt::Utf8 { .. } | Stmt::Repeat { .. } => false,
        Stmt::If { then, els, .. } => touches_w(then) || touches_w(els),
        Stmt::Match { arms, other, .. } => {
            arms.iter().any(|a| touches_w(&a.body)) || other.as_ref().is_none_or(|o| touches_w(o))
        }
        Stmt::ForEach { body, .. } => touches_w(body),
    })
}

/// Whether any expression reads the bytes remaining in the reader.
fn stmts_has_remaining(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Declare { .. } | Stmt::Fail(_) => false,
        Stmt::Let { value, .. } => expr_has_remaining(value),
        Stmt::Read { op, .. } => matches!(op, ReadOp::Bytes(n) if expr_has_remaining(n)),
        Stmt::Decode { args, .. } => args.iter().any(expr_has_remaining),
        Stmt::Utf8 { src, .. } => expr_has_remaining(src),
        Stmt::Write { op, value } => {
            matches!(op, WriteOp::Bytes(n) if expr_has_remaining(n)) || expr_has_remaining(value)
        }
        Stmt::Encode { args, value, .. } => {
            args.iter().any(expr_has_remaining) || expr_has_remaining(value)
        }
        Stmt::Require { cond, .. } => expr_has_remaining(cond),
        Stmt::Unwrap { src, .. } => expr_has_remaining(src),
        Stmt::If { cond, then, els } => {
            expr_has_remaining(cond) || stmts_has_remaining(then) || stmts_has_remaining(els)
        }
        Stmt::Match {
            scrutinee,
            arms,
            other,
        } => {
            expr_has_remaining(scrutinee)
                || arms.iter().any(|a| stmts_has_remaining(&a.body))
                || other.as_ref().is_some_and(|o| stmts_has_remaining(o))
        }
        Stmt::Repeat { mode, body, .. } => {
            matches!(mode, RepeatMode::Count(e) | RepeatMode::Until(e) if expr_has_remaining(e))
                || stmts_has_remaining(body)
        }
        Stmt::ForEach { list, body, .. } => expr_has_remaining(list) || stmts_has_remaining(body),
        Stmt::ExpectVariant { sum, .. } => expr_has_remaining(sum),
        Stmt::Return(e) => expr_has_remaining(e),
    })
}

fn expr_has_remaining(e: &Expr) -> bool {
    match e {
        Expr::Remaining => true,
        Expr::Local(_) | Expr::Int { .. } | Expr::Bool(_) | Expr::Str(_) => false,
        Expr::Field { base, .. }
        | Expr::Present(base)
        | Expr::Get(base)
        | Expr::Not(base)
        | Expr::StrBytes(base) => expr_has_remaining(base),
        Expr::Cast { expr: x, .. } => expr_has_remaining(x),
        Expr::Binary { lhs, rhs, .. } => expr_has_remaining(lhs) || expr_has_remaining(rhs),
        Expr::Cond {
            cond, then, els, ..
        } => expr_has_remaining(cond) || expr_has_remaining(then) || expr_has_remaining(els),
        Expr::Call { args, .. } => args.iter().any(expr_has_remaining),
        Expr::Construct { fields, .. } => fields.iter().any(expr_has_remaining),
        Expr::Variant { payload, .. } => payload.as_ref().is_some_and(|p| expr_has_remaining(p)),
    }
}

impl<'p> FnEmit<'p> {
    /// Call arguments: presence flags for optional parameters, pointers (and
    /// lengths) for owned ones. Arguments are present values unless the
    /// callee takes an optional parameter.
    fn call_args(&mut self, codec: CodecIdx, args: &[Expr]) -> BpsResult<Vec<String>> {
        let cparams = self.p.codecs[codec].params.clone();
        let mut argv = Vec::new();
        for (a, prm) in args.iter().zip(cparams.iter()) {
            let (_, aopt) = self.p.expr_ty(self.body, a);
            if prm.optional {
                if aopt {
                    argv.push(self.has_of(a)?);
                } else {
                    argv.push("true".to_string());
                }
            }
            match &prm.ty {
                t if is_scalar(t) => {
                    if aopt && prm.optional {
                        argv.push(self.val_of(a)?);
                    } else {
                        argv.push(self.expr(a)?);
                    }
                }
                Ty::List(_) => {
                    let ptr = self.list_ptr(a)?;
                    let ln = self.len_of(a)?;
                    if aopt && prm.optional {
                        let h = self.has_of(a)?;
                        argv.push(format!("(({h}) ? ({ptr}) : NULL)"));
                        argv.push(format!("(({h}) ? ({ln}) : 0)"));
                    } else {
                        argv.push(ptr);
                        argv.push(ln);
                    }
                }
                _ => {
                    // A boxed member is already a `T *`; pass it straight through.
                    if let Expr::Field { ty, field, .. } = a
                        && self.is_boxed(*ty, *field)
                    {
                        let f = self.field_full(a)?;
                        if aopt && prm.optional {
                            let h = self.has_of(a)?;
                            argv.push(format!("(({h}) ? ({f}) : NULL)"));
                        } else {
                            argv.push(f);
                        }
                    } else {
                        let pl = self.place(a)?;
                        if aopt && prm.optional {
                            let h = self.has_of(a)?;
                            argv.push(format!("({h} ? {} : NULL)", addr_of(&pl)));
                        } else {
                            argv.push(addr_of(&pl));
                        }
                    }
                }
            }
        }
        Ok(argv)
    }
}
/// Whether any decode Return writes through `out` (empty records don't).
fn writes_out(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Return(e) => !matches!(e, Expr::Construct { fields, .. } if fields.is_empty()),
        Stmt::If { then, els, .. } => writes_out(then) || writes_out(els),
        Stmt::Match { arms, other, .. } => {
            arms.iter().any(|a| writes_out(&a.body))
                || other.as_ref().is_some_and(|o| writes_out(o))
        }
        Stmt::Repeat { body, .. } => writes_out(body),
        _ => false,
    })
}

/// Whether any decode Return stores a boxed (pointer) field, which allocates
/// and so needs the status variable and fail label.
fn return_needs_box(boxed: &HashSet<(TypeIdx, usize)>, stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Return(Expr::Construct { ty, fields }) => {
            (0..fields.len()).any(|i| boxed.contains(&(*ty, i)))
        }
        Stmt::If { then, els, .. } => return_needs_box(boxed, then) || return_needs_box(boxed, els),
        Stmt::Match { arms, other, .. } => {
            arms.iter().any(|a| return_needs_box(boxed, &a.body))
                || other.as_ref().is_some_and(|o| return_needs_box(boxed, o))
        }
        Stmt::Repeat { body, .. } => return_needs_box(boxed, body),
        _ => false,
    })
}
/// Resolved struct member names of a record. Values keep source names;
/// presence and length members bump on collision with real fields.
struct FieldNames {
    value: String,
    has: Option<String>,
    len: Option<String>,
}

fn record_members(program: &Program, ty: TypeIdx) -> Vec<FieldNames> {
    let mut taken = HashSet::new();
    let fields = program.fields(ty);
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        out.push(FieldNames {
            value: fresh_name(&mut taken, escape(f.name.snake(), C_KEYWORDS)),
            has: None,
            len: None,
        });
    }
    for (i, f) in fields.iter().enumerate() {
        if f.optional {
            let base = format!("has_{}", out[i].value);
            out[i].has = Some(fresh_name(&mut taken, base));
        }
    }
    for (i, f) in fields.iter().enumerate() {
        if matches!(f.ty, Ty::List(_)) {
            let base = format!("{}_len", out[i].value);
            out[i].len = Some(fresh_name(&mut taken, base));
        }
    }
    out
}

fn fresh_name(taken: &mut HashSet<String>, base: String) -> String {
    let mut c = base.clone();
    let mut n = 2;
    while taken.contains(&c) {
        c = format!("{base}_{n}");
        n += 1;
    }
    taken.insert(c.clone());
    c
}

impl<'p> FnEmit<'p> {
    /// Resolved member names of a record field.
    fn mem(&self, ty: TypeIdx, field: usize) -> FieldNames {
        record_members(self.p, ty).remove(field)
    }

    /// Whether a record field lives behind a pointer (recursive types are
    /// boxed so the struct stays finite).
    fn is_boxed(&self, ty: TypeIdx, field: usize) -> bool {
        self.boxed.contains(&(resolve_ty(self.p, ty), field))
    }
}
/// Follow record aliases to the defined type.
fn resolve_ty(program: &Program, mut ty: TypeIdx) -> TypeIdx {
    loop {
        match &program.types[ty].kind {
            TypeKind::Alias { target } => ty = *target,
            _ => return ty,
        }
    }
}

/// By-value references of a record or sum: field and payload types with
/// aliases resolved, lists excluded (they are already pointers).
fn value_refs(program: &Program, ty: TypeIdx) -> Vec<TypeIdx> {
    let mut out = Vec::new();
    let push = |t: &Ty, out: &mut Vec<TypeIdx>| {
        let mut stack = vec![t];
        while let Some(x) = stack.pop() {
            match x {
                Ty::Named(u) => out.push(resolve_ty(program, *u)),
                Ty::List(inner) => stack.push(inner),
                _ => {}
            }
        }
    };
    match &program.types[ty].kind {
        TypeKind::Record { fields } => {
            for f in fields {
                if !matches!(f.ty, Ty::List(_)) {
                    push(&f.ty, &mut out);
                }
            }
        }
        TypeKind::Sum { variants } => {
            for v in variants {
                if let Some(pl) = &v.payload
                    && !matches!(pl, Ty::List(_))
                {
                    push(pl, &mut out);
                }
            }
        }
        _ => {}
    }
    out
}

/// Whether `to` is reachable from `from` by a non-trivial by-value path.
fn reaches(program: &Program, from: TypeIdx, to: TypeIdx) -> bool {
    let mut seen = HashSet::new();
    let mut stack = value_refs(program, from);
    while let Some(x) = stack.pop() {
        if x == to {
            return true;
        }
        if seen.insert(x) {
            stack.extend(value_refs(program, x));
        }
    }
    false
}

/// Record fields whose value must live behind a pointer: their type reaches
/// back to the record, so a by-value member would be infinite size.
fn compute_boxed(program: &Program) -> HashSet<(TypeIdx, usize)> {
    let mut out = HashSet::new();
    for (ty, decl) in program.types.iter().enumerate() {
        let TypeKind::Record { fields } = &decl.kind else {
            continue;
        };
        for (i, f) in fields.iter().enumerate() {
            // Lists are already a pointer plus a length, so they can never
            // make a struct infinite; only by-value fields need boxing.
            if matches!(f.ty, Ty::List(_)) {
                continue;
            }
            let mut targets = Vec::new();
            let mut stack = vec![&f.ty];
            while let Some(x) = stack.pop() {
                match x {
                    Ty::Named(u) => targets.push(resolve_ty(program, *u)),
                    Ty::List(inner) => stack.push(inner),
                    _ => {}
                }
            }
            if targets.iter().any(|t| {
                matches!(
                    program.types[*t].kind,
                    TypeKind::Record { .. } | TypeKind::Sum { .. }
                ) && reaches(program, *t, ty)
            }) {
                out.insert((ty, i));
            }
        }
    }
    out
}
