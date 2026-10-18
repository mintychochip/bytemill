//! `emit cpp`: `bpsgen.hpp` + `bpsgen.cpp` whose types and codec functions are
//! the typed program (`bps::ir::typed::Program`).
//!
//! * Records are `struct`s (fields in schema order, every typed-program field
//!   including presence flags such as `has_x`), sums are `std::variant`s with
//!   one wrapper struct per arm, open enums are `enum class`es (decode keeps
//!   unknown values via `static_cast`), optionals are `std::optional`, lists
//!   are `std::vector`, strings are `std::string`, bytes are
//!   `std::vector<std::uint8_t>`, errors are `DecodeError`/`EncodeError`
//!   exceptions naming the codec (and the reader offset on decode).
//! * One `decode_<snake>` returning `T` and one
//!   `encode_<snake>(Writer&, const T&, ...)` per codec. A sum decode picks
//!   its arm by the tag parameter with an if/else chain; a sum encode picks
//!   its arm by the tag parameter too, then checks the variant
//!   (`ExpectVariant`), never a `holds_alternative` chain.
//! * Recursive types stay by value. `std::vector` tolerates an incomplete
//!   element type, so only a member or variant alternative that needs a
//!   complete type to declare (a direct or optional field, a wrapper payload,
//!   an alternative) and reaches its owner through such edges is stored in a
//!   small deep-copying `Box<T>`; locals, parameters, and list elements are
//!   never boxed.
//! * Integer arithmetic is two's complement. `wraps` additions, subtractions,
//!   multiplications, and shifts use `add_wrap`/`sub_wrap`/`mul_wrap`/
//!   `shl_wrap` (via `uint64_t`, so no undefined behaviour); `/` floors
//!   (`div_floor`) and `%` takes the divisor's sign (`mod_sign`) for signed
//!   types and prints plainly for unsigned ones; `>>` is arithmetic since
//!   C++20; narrowing stores are explicit `static_cast`s. `wraps` is the
//!   overflow signal: plain operators print only where it is unset.
//! * `until(t)` decodes by peeking for the terminator (consumed on match) and
//!   `until_end` decodes while bytes remain; encoding `until_end` throws.
//!   Computed fields decode into locals and encode from the struct, so
//!   `position.raw` is written from the field.
//! * Lines that would exceed 80 columns split into `const` temporaries, which
//!   `clang-format` (LLVM style) leaves stable. Short-circuiting operators
//!   and ternaries are never split. Overlong `using` lists break after `=`
//!   with the alternatives packed under the `<`, and overlong `Construct`
//!   returns become designated initializers, both exactly the way
//!   `clang-format` lays them out (no `off/on` guards).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use crate::ResolvedSchema;
use crate::errors::{BpsResult, emit};
use crate::ir::typed::{
    BinOp, Body, CodecIdx, Expr, Func, IntTy, LocalId, Name, Origin, Program, ReadOp, RepeatMode,
    Stmt, Ty, TypeIdx, escape, hex_literal, typed,
};

/// C++20 keywords and alternative tokens, plus POSIX/glibc and math macro
/// hazards from the idiom guide. An emitted snake_case identifier colliding
/// with one of these gets a trailing `_`.
const KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "const_cast",
    "continue",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "private",
    "protected",
    "public",
    "reinterpret_cast",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "xor",
    "xor_eq",
    "near",
    "far",
    "min",
    "max",
    "linux",
    "major",
    "minor",
    "sign",
    "errno",
    "y0",
    "y1",
    "j0",
    "j1",
];

/// Identifiers the runtime defines at namespace scope. A generated top-level
/// type with one of these names is a printer error, never a silent rename.
const RUNTIME_TOP: &[&str] = &["Reader", "Writer", "DecodeError", "EncodeError", "Box"];

/// `clang-format` (LLVM style) column limit.
const MAX_COL: usize = 80;

pub fn emit_cpp(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    let program = typed(resolved)?;
    let layout = Layout::build(&program);
    check_top_level(&program, &layout)?;
    let header = emit_header(&program, &layout)?;
    let sources = emit_sources(&program, &layout)?;
    std::fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    let header_path = out_dir.join("bpsgen.hpp");
    std::fs::write(&header_path, header).map_err(|e| emit(e.to_string()))?;
    clang_format(&header_path);
    for (stem, source) in sources {
        let path = out_dir.join(format!("{stem}.cpp"));
        std::fs::write(&path, source).map_err(|e| emit(e.to_string()))?;
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

/// The emitted decode function for a codec: `decode_<snake>`.
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("decode_{}", program.codecs[codec].name.snake())
}

/// The emitted encode function for a codec: `encode_<snake>`.
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("encode_{}", program.codecs[codec].name.snake())
}

fn check_top_level(program: &Program, layout: &Layout) -> BpsResult<()> {
    for t in 0..program.types.len() {
        let name = pascal(&program.types[t].name);
        if RUNTIME_TOP.contains(&name.as_str()) {
            return Err(emit(format!("cpp: type {name} collides with the runtime")));
        }
    }
    for w in &layout.wrappers {
        if RUNTIME_TOP.contains(&w.name.as_str()) {
            return Err(emit(format!(
                "cpp: type {} collides with the runtime",
                w.name
            )));
        }
    }
    Ok(())
}

fn snake(name: &Name) -> String {
    escape(name.snake(), KEYWORDS)
}

fn pascal(name: &Name) -> String {
    escape(name.pascal(), KEYWORDS)
}

fn int_cpp(ty: IntTy) -> &'static str {
    match ty {
        IntTy::I8 => "std::int8_t",
        IntTy::I16 => "std::int16_t",
        IntTy::I32 => "std::int32_t",
        IntTy::I64 => "std::int64_t",
        IntTy::I128 => "__int128",
        IntTy::U8 => "std::uint8_t",
        IntTy::U16 => "std::uint16_t",
        IntTy::U32 => "std::uint32_t",
        IntTy::U64 => "std::uint64_t",
    }
}

/// C++ spelling of a value type.
fn cpp_ty(program: &Program, ty: &Ty) -> String {
    match ty {
        Ty::Int(t) => int_cpp(*t).to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::F32 => "float".to_string(),
        Ty::F64 => "double".to_string(),
        Ty::Str => "std::string".to_string(),
        Ty::Bytes => "std::vector<std::uint8_t>".to_string(),
        Ty::List(inner) => format!("std::vector<{}>", cpp_ty(program, inner)),
        Ty::Named(t) => pascal(&program.types[*t].name),
    }
}

fn opt_cpp(program: &Program, ty: &Ty, optional: bool) -> String {
    if optional {
        format!("std::optional<{}>", cpp_ty(program, ty))
    } else {
        cpp_ty(program, ty)
    }
}

// ---------------------------------------------------------------------------
// Layout: synthetic wrappers, Box placement, definition order.
// ---------------------------------------------------------------------------

/// One synthetic arm wrapper: `struct <Sum><Variant> { <payload> value; };`.
/// An arm whose payload already is the dedicated arm record reuses it.
struct Wrapper {
    name: String,
    sum: TypeIdx,
    variant: usize,
    payload: Option<Ty>,
    /// The payload already is this record; no new struct is declared.
    existing: Option<TypeIdx>,
}

enum Item {
    Type(TypeIdx),
    Wrapper,
}

struct Layout {
    wrappers: Vec<Wrapper>,
    wrapper_of: HashMap<(TypeIdx, usize), usize>,
    /// Record fields stored as `Box<..>`.
    boxed_field: HashSet<(TypeIdx, usize)>,
    /// Synthetic wrapper payloads stored as `Box<..>`.
    boxed_payload: HashSet<usize>,
    /// Sum alternatives stored as `Box<..>`.
    boxed_alt: HashSet<(TypeIdx, usize)>,
    order: Vec<Item>,
    uses_i128: bool,
}

/// Named types a declaration needs complete, ignoring `std::vector` (which
/// tolerates an incomplete element type).
fn named_direct(ty: &Ty) -> Vec<TypeIdx> {
    match ty {
        Ty::Named(t) => vec![*t],
        Ty::List(_) => Vec::new(),
        _ => Vec::new(),
    }
}

impl Layout {
    fn build(program: &Program) -> Layout {
        let mut wrappers = Vec::new();
        let mut wrapper_of = HashMap::new();
        for (ti, decl) in program.types.iter().enumerate() {
            let crate::ir::typed::TypeKind::Sum { variants } = &decl.kind else {
                continue;
            };
            for (vi, variant) in variants.iter().enumerate() {
                let Some(payload) = variant.payload.clone() else {
                    let name = format!("{}{}", pascal(&decl.name), pascal(&variant.name));
                    wrapper_of.insert((ti, vi), wrappers.len());
                    wrappers.push(Wrapper {
                        name,
                        sum: ti,
                        variant: vi,
                        payload: None,
                        existing: None,
                    });
                    continue;
                };
                let name = format!("{}{}", pascal(&decl.name), pascal(&variant.name));
                let existing = match &payload {
                    Ty::Named(t) if pascal(&program.types[*t].name) == name => Some(*t),
                    _ => None,
                };
                wrapper_of.insert((ti, vi), wrappers.len());
                wrappers.push(Wrapper {
                    name,
                    sum: ti,
                    variant: vi,
                    payload: Some(payload),
                    existing,
                });
            }
        }
        let nodes = program.types.len() + wrappers.len();
        let wnode = |w: usize| program.types.len() + w;
        // Needs-complete edges: the target must be declared (Box) or defined
        // (by value) before the source is defined.
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); nodes];
        for (ti, decl) in program.types.iter().enumerate() {
            match &decl.kind {
                crate::ir::typed::TypeKind::Record { fields } => {
                    for f in fields {
                        for u in named_direct(&f.ty) {
                            edges[ti].push(u);
                        }
                    }
                }
                crate::ir::typed::TypeKind::Sum { variants } => {
                    for (vi, variant) in variants.iter().enumerate() {
                        let Some(payload) = &variant.payload else {
                            continue;
                        };
                        match payload {
                            Ty::Named(t)
                                if wrappers[wrapper_of[&(ti, vi)]].existing == Some(*t) =>
                            {
                                edges[ti].push(*t);
                            }
                            _ => edges[ti].push(wnode(wrapper_of[&(ti, vi)])),
                        }
                    }
                }
                crate::ir::typed::TypeKind::Alias { target } => edges[ti].push(*target),
                crate::ir::typed::TypeKind::Enum { .. } => {}
            }
        }
        for (w, wrap) in wrappers.iter().enumerate() {
            if let Some(payload) = &wrap.payload
                && wrap.existing.is_none()
            {
                for u in named_direct(payload) {
                    edges[wnode(w)].push(u);
                }
            }
        }
        let scc = strongly_connected(&edges);
        let mut boxed_field = HashSet::new();
        let mut boxed_payload = HashSet::new();
        let mut boxed_alt = HashSet::new();
        for (ti, decl) in program.types.iter().enumerate() {
            match &decl.kind {
                crate::ir::typed::TypeKind::Record { fields } => {
                    for (fi, f) in fields.iter().enumerate() {
                        for u in named_direct(&f.ty) {
                            if scc[ti] == scc[u] {
                                boxed_field.insert((ti, fi));
                            }
                        }
                    }
                }
                crate::ir::typed::TypeKind::Sum { variants } => {
                    for (vi, variant) in variants.iter().enumerate() {
                        let target = match &variant.payload {
                            Some(Ty::Named(t))
                                if program.types[*t].name.pascal()
                                    == wrappers[wrapper_of[&(ti, vi)]].name =>
                            {
                                *t
                            }
                            _ => wnode(wrapper_of[&(ti, vi)]),
                        };
                        if scc[ti] == scc[target] {
                            boxed_alt.insert((ti, vi));
                        }
                    }
                }
                crate::ir::typed::TypeKind::Alias { .. } => {}
                crate::ir::typed::TypeKind::Enum { .. } => {}
            }
        }
        for (w, wrap) in wrappers.iter().enumerate() {
            if wrap.existing.is_some() {
                continue;
            }
            if let Some(payload) = &wrap.payload {
                for u in named_direct(payload) {
                    if scc[wnode(w)] == scc[u] {
                        boxed_payload.insert(w);
                    }
                }
            }
        }
        // Definition order: a before-edge `b -> a` means "define b first".
        // Unboxed edges need the definition; boxed edges to sums and enums
        // need it too (they cannot be forward declared); boxed edges to
        // records and wrappers need only the forward declaration.
        let mut before: Vec<Vec<usize>> = vec![Vec::new(); nodes];
        let mut after_count = vec![0usize; nodes];
        let mut add = |b: usize, a: usize, seen: &mut HashSet<(usize, usize)>| {
            if b != a && seen.insert((b, a)) {
                before[b].push(a);
                after_count[a] += 1;
            }
        };
        let mut seen = HashSet::new();
        let is_sum_enum = |n: usize| {
            n < program.types.len()
                && matches!(
                    program.types[n].kind,
                    crate::ir::typed::TypeKind::Sum { .. }
                        | crate::ir::typed::TypeKind::Enum { .. }
                )
        };
        for (a, outs) in edges.iter().enumerate() {
            for &b in outs {
                let boxed = is_boxed_edge(
                    program,
                    &wrappers,
                    &wrapper_of,
                    &boxed_field,
                    &boxed_payload,
                    &boxed_alt,
                    a,
                    b,
                );
                if !boxed || is_sum_enum(b) {
                    add(b, a, &mut seen);
                }
            }
        }
        // Kahn's algorithm, ties broken by program order (synthetic wrappers
        // sort with their sum). The graph is acyclic by construction: every
        // cycle of needs-complete edges is boxed, and boxed edges constrain
        // the order only toward sums and enums.
        let key = |n: usize| {
            if n < program.types.len() {
                (n, 0usize)
            } else {
                let w = &wrappers[n - program.types.len()];
                (w.sum, w.variant + 1)
            }
        };
        let mut ready: Vec<usize> = (0..nodes).filter(|&n| after_count[n] == 0).collect();
        let mut order_nodes = Vec::with_capacity(nodes);
        while !ready.is_empty() {
            ready.sort_by_key(|&n| key(n));
            let n = ready.remove(0);
            order_nodes.push(n);
            for &a in &before[n] {
                after_count[a] -= 1;
                if after_count[a] == 0 {
                    ready.push(a);
                }
            }
        }
        let order = order_nodes
            .into_iter()
            .map(|n| {
                if n < program.types.len() {
                    Item::Type(n)
                } else {
                    Item::Wrapper
                }
            })
            .collect();
        Layout {
            wrappers,
            wrapper_of,
            boxed_field,
            boxed_payload,
            boxed_alt,
            order,
            uses_i128: uses_i128(program),
        }
    }

    fn wrapper(&self, sum: TypeIdx, variant: usize) -> &Wrapper {
        &self.wrappers[self.wrapper_of[&(sum, variant)]]
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "a box edge is the node pair plus each box set the layout already computed"
)]
fn is_boxed_edge(
    program: &Program,
    wrappers: &[Wrapper],
    wrapper_of: &HashMap<(TypeIdx, usize), usize>,
    boxed_field: &HashSet<(TypeIdx, usize)>,
    boxed_payload: &HashSet<usize>,
    boxed_alt: &HashSet<(TypeIdx, usize)>,
    a: usize,
    b: usize,
) -> bool {
    if a < program.types.len() {
        match &program.types[a].kind {
            crate::ir::typed::TypeKind::Record { fields } => {
                return fields.iter().enumerate().any(|(fi, f)| {
                    boxed_field.contains(&(a, fi)) && named_direct(&f.ty).contains(&b)
                });
            }
            crate::ir::typed::TypeKind::Sum { .. } => {
                // The edge leads to the wrapper node or the existing record.
                for ((ti, vi), &w) in wrapper_of {
                    if *ti != a {
                        continue;
                    }
                    let wnode = program.types.len() + w;
                    if wnode == b || wrappers[w].existing == Some(b) {
                        return boxed_alt.contains(&(*ti, *vi));
                    }
                }
                return false;
            }
            crate::ir::typed::TypeKind::Alias { .. } => return false,
            crate::ir::typed::TypeKind::Enum { .. } => return false,
        }
    }
    let w = a - program.types.len();
    boxed_payload.contains(&w)
}

/// Tarjan's strongly connected components, deterministic in node order.
fn strongly_connected(edges: &[Vec<usize>]) -> Vec<usize> {
    struct Walk<'e> {
        edges: &'e [Vec<usize>],
        index: Vec<Option<usize>>,
        low: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        next: usize,
        comp: Vec<usize>,
    }
    fn visit(w: &mut Walk<'_>, v: usize) {
        w.index[v] = Some(w.next);
        w.low[v] = w.next;
        w.next += 1;
        w.stack.push(v);
        w.on_stack[v] = true;
        for &to in &w.edges[v] {
            if w.index[to].is_none() {
                visit(w, to);
                w.low[v] = w.low[v].min(w.low[to]);
            } else if w.on_stack[to] {
                w.low[v] = w.low[v].min(w.index[to].unwrap_or(usize::MAX));
            }
        }
        if w.low[v] == w.index[v].unwrap_or(usize::MAX) {
            while let Some(u) = w.stack.pop() {
                w.on_stack[u] = false;
                w.comp[u] = v;
                if u == v {
                    break;
                }
            }
        }
    }
    let mut w = Walk {
        edges,
        index: vec![None; edges.len()],
        low: vec![0; edges.len()],
        on_stack: vec![false; edges.len()],
        stack: Vec::new(),
        next: 0,
        comp: vec![usize::MAX; edges.len()],
    };
    for v in 0..edges.len() {
        if w.index[v].is_none() {
            visit(&mut w, v);
        }
    }
    // Renumber components in first-appearance order for determinism.
    let mut ids = HashMap::new();
    let mut out = vec![0; edges.len()];
    for (v, c) in w.comp.iter().enumerate() {
        let n = ids.len();
        out[v] = *ids.entry(*c).or_insert(n);
    }
    out
}

fn uses_i128(program: &Program) -> bool {
    fn has(ty: &Ty) -> bool {
        match ty {
            Ty::Int(IntTy::I128) => true,
            Ty::List(inner) => has(inner),
            _ => false,
        }
    }
    for decl in &program.types {
        match &decl.kind {
            crate::ir::typed::TypeKind::Record { fields } => {
                if fields.iter().any(|f| has(&f.ty)) {
                    return true;
                }
            }
            crate::ir::typed::TypeKind::Sum { variants }
                if variants.iter().any(|v| v.payload.as_ref().is_some_and(has)) =>
            {
                return true;
            }
            _ => {}
        }
    }
    for codec in &program.codecs {
        if codec.params.iter().any(|p| has(&p.ty)) || has(&codec.value) {
            return true;
        }
        for body in [&codec.decode, &codec.encode] {
            if body.locals.iter().any(|l| has(&l.ty)) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Header and source.
// ---------------------------------------------------------------------------

const RUNTIME_BASE: &str = r#"#pragma once
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <variant>
#include <vector>

namespace bps {
class DecodeError : public std::runtime_error {
public:
  DecodeError(const std::string &codec, std::size_t offset,
              const std::string &what)
      : std::runtime_error("decode " + codec + " at offset " +
                           std::to_string(offset) + ": " + what),
        codec(codec), offset(offset) {}
  std::string codec;
  std::size_t offset;
};
class EncodeError : public std::runtime_error {
public:
  EncodeError(const std::string &codec, const std::string &what)
      : std::runtime_error("encode " + codec + ": " + what), codec(codec) {}
  std::string codec;
};
inline bool is_utf8(const std::uint8_t *s, std::size_t n) {
  std::size_t i = 0;
  while (i < n) {
    std::uint8_t c = s[i];
    std::size_t need = 0;
    std::uint32_t cp = 0;
    if (c < 0x80) {
      need = 1;
      cp = c;
    } else if (c < 0xC2) {
      return false;
    } else if (c < 0xE0) {
      need = 2;
      cp = c & 0x1F;
    } else if (c < 0xF0) {
      need = 3;
      cp = c & 0x0F;
    } else if (c < 0xF5) {
      need = 4;
      cp = c & 0x07;
    } else {
      return false;
    }
    if (i + need > n) {
      return false;
    }
    for (std::size_t k = 1; k < need; ++k) {
      if ((s[i + k] & 0xC0) != 0x80) {
        return false;
      }
      cp = (cp << 6) | (s[i + k] & 0x3F);
    }
    if (cp > 0x10FFFF || (cp >= 0xD800 && cp <= 0xDFFF)) {
      return false;
    }
    if ((need == 2 && cp < 0x80) || (need == 3 && cp < 0x800) ||
        (need == 4 && cp < 0x10000)) {
      return false;
    }
    i += need;
  }
  return true;
}
class Reader {
public:
  Reader(const std::uint8_t *data, std::size_t len) : data_(data), len_(len) {}
  std::size_t pos() const { return pos_; }
  std::size_t remaining() const { return len_ - pos_; }
  std::uint8_t u8(const char *codec) {
    if (pos_ >= len_) {
      throw DecodeError(codec, pos_, "short buffer: need 1 byte");
    }
    return data_[pos_++];
  }
  std::uint8_t peek_u8(const char *codec) const {
    if (pos_ >= len_) {
      throw DecodeError(codec, pos_, "short buffer: need 1 byte");
    }
    return data_[pos_];
  }
  std::vector<std::uint8_t> bytes(std::int64_t n, const char *codec) {
    if (n < 0) {
      throw DecodeError(codec, pos_, "negative length " + std::to_string(n));
    }
    if (static_cast<std::uint64_t>(n) > remaining()) {
      throw DecodeError(codec, pos_,
                        "short buffer: need " + std::to_string(n) + " bytes");
    }
    std::vector<std::uint8_t> out(data_ + pos_, data_ + pos_ + n);
    pos_ += static_cast<std::size_t>(n);
    return out;
  }
  std::vector<std::uint8_t> rest() {
    std::vector<std::uint8_t> out(data_ + pos_, data_ + len_);
    pos_ = len_;
    return out;
  }
  float f32(const char *codec) {
    std::uint32_t u = 0;
    for (std::uint8_t b : bytes(4, codec)) {
      u = (u << 8) | b;
    }
    float f = 0;
    std::memcpy(&f, &u, 4);
    return f;
  }
  double f64(const char *codec) {
    std::uint64_t u = 0;
    for (std::uint8_t b : bytes(8, codec)) {
      u = (u << 8) | b;
    }
    double d = 0;
    std::memcpy(&d, &u, 8);
    return d;
  }

  std::string utf8(std::vector<std::uint8_t> raw, const char *codec) {
    if (!is_utf8(raw.data(), raw.size())) {
      throw DecodeError(codec, pos_, "invalid UTF-8");
    }
    return std::string(raw.begin(), raw.end());
  }

private:
  const std::uint8_t *data_;
  std::size_t len_;
  std::size_t pos_ = 0;
};
class Writer {
public:
  std::vector<std::uint8_t> buf;
  void u8(std::uint8_t v) { buf.push_back(v); }
  void bytes(const std::vector<std::uint8_t> &b) {
    buf.insert(buf.end(), b.begin(), b.end());
  }
  void bytes_sized(const std::vector<std::uint8_t> &b, std::int64_t n,
                   const char *codec) {
    if (n < 0 || b.size() > static_cast<std::size_t>(n)) {
      throw EncodeError(codec, "bytes too long");
    }
    bytes(b);
    buf.insert(buf.end(), static_cast<std::size_t>(n) - b.size(), 0);
  }
  void f32(float v) {
    std::uint32_t u = 0;
    std::memcpy(&u, &v, 4);
    buf.push_back(static_cast<std::uint8_t>(u >> 24));
    buf.push_back(static_cast<std::uint8_t>(u >> 16));
    buf.push_back(static_cast<std::uint8_t>(u >> 8));
    buf.push_back(static_cast<std::uint8_t>(u));
  }
  void f64(double v) {
    std::uint64_t u = 0;
    std::memcpy(&u, &v, 8);
    for (int i = 7; i >= 0; --i) {
      buf.push_back(static_cast<std::uint8_t>(u >> (8 * i)));
    }
  }
};
inline std::int64_t add_wrap(std::int64_t a, std::int64_t b) {
  return static_cast<std::int64_t>(static_cast<std::uint64_t>(a) +
                                   static_cast<std::uint64_t>(b));
}
inline std::int64_t sub_wrap(std::int64_t a, std::int64_t b) {
  return static_cast<std::int64_t>(static_cast<std::uint64_t>(a) -
                                   static_cast<std::uint64_t>(b));
}
inline std::int64_t mul_wrap(std::int64_t a, std::int64_t b) {
  return static_cast<std::int64_t>(static_cast<std::uint64_t>(a) *
                                   static_cast<std::uint64_t>(b));
}
inline std::int64_t shl_wrap(std::int64_t a, std::int64_t b) {
  return static_cast<std::int64_t>(static_cast<std::uint64_t>(a)
                                   << static_cast<unsigned>(b));
}
inline std::int64_t div_floor(std::int64_t a, std::int64_t b) {
  if (b == 0) {
    return 0;
  }
  if (a == INT64_MIN && b == -1) {
    return INT64_MIN;
  }
  std::int64_t q = a / b;
  std::int64_t r = a % b;
  if (r != 0 && ((a < 0) != (b < 0))) {
    --q;
  }
  return q;
}
inline std::int64_t mod_sign(std::int64_t a, std::int64_t b) {
  if (b == 0) {
    return 0;
  }
  if (a == INT64_MIN && b == -1) {
    return 0;
  }
  std::int64_t r = a % b;
  if (r != 0 && ((a < 0) != (b < 0))) {
    r += b;
  }
  return r;
}
inline std::int64_t sign_extend(std::int64_t v, std::int64_t bits) {
  if (bits <= 0 || bits >= 64) {
    return v;
  }
  std::uint64_t u = static_cast<std::uint64_t>(v);
  std::uint64_t sign = std::uint64_t{1} << (bits - 1);
  std::uint64_t mask = (std::uint64_t{1} << bits) - 1;
  u &= mask;
  if ((u & sign) != 0) {
    u |= ~mask;
  }
  return static_cast<std::int64_t>(u);
}
inline std::int64_t utf16_units(const std::string &s) {
  std::int64_t n = 0;
  std::size_t i = 0;
  while (i < s.size()) {
    std::uint8_t c = static_cast<std::uint8_t>(s[i]);
    std::uint32_t cp = 0;
    std::size_t need = 1;
    if (c < 0x80) {
      cp = c;
      need = 1;
    } else if (c < 0xE0) {
      cp = c & 0x1F;
      need = 2;
    } else if (c < 0xF0) {
      cp = c & 0x0F;
      need = 3;
    } else {
      cp = c & 0x07;
      need = 4;
    }
    if (i + need > s.size()) {
      break;
    }
    for (std::size_t j = 1; j < need; ++j) {
      cp = (cp << 6) | (static_cast<std::uint8_t>(s[i + j]) & 0x3F);
    }
    i += need;
    n += cp > 0xFFFF ? 2 : 1;
  }
  return n;
}
inline std::int64_t utf8_len(const std::string &s) {
  return static_cast<std::int64_t>(s.size());
}
inline std::int64_t str_len(const std::string &s) {
  std::int64_t n = 0;
  for (char c : s) {
    if ((static_cast<std::uint8_t>(c) & 0xC0) != 0x80) {
      ++n;
    }
  }
  return n;
}
inline std::int64_t bytes_len(const std::vector<std::uint8_t> &b) {
  return static_cast<std::int64_t>(b.size());
}
template <typename T> inline std::int64_t list_len(const std::vector<T> &v) {
  return static_cast<std::int64_t>(v.size());
}
inline std::vector<std::uint8_t> str_bytes(const std::string &s) {
  return std::vector<std::uint8_t>(s.begin(), s.end());
}
template <typename T> struct Box {
  std::unique_ptr<T> p;
  Box() : p(std::make_unique<T>()) {}
  Box(const T &v) : p(std::make_unique<T>(v)) {}
  Box(T &&v) : p(std::make_unique<T>(std::move(v))) {}
  Box(const Box &o) : p(o.p ? std::make_unique<T>(*o.p) : nullptr) {}
  Box(Box &&) noexcept = default;
  Box &operator=(const Box &o) {
    if (!p) {
      p = o.p ? std::make_unique<T>(*o.p) : nullptr;
    } else if (o.p) {
      *p = *o.p;
    } else {
      p.reset();
    }
    return *this;
  }
  Box &operator=(Box &&) noexcept = default;
  ~Box() = default;
  T &operator*() { return *p; }
  const T &operator*() const { return *p; }
  T *operator->() { return p.get(); }
  const T *operator->() const { return p.get(); }
  bool operator==(const Box &o) const { return *p == *o.p; }
};
"#;

/// `__int128` support, emitted only when the program uses `I128` storage
/// (none of the shipped schemas do after lowering): `__int128` would trip
/// `-Wpedantic` if it were always present.
const RUNTIME_128: &str = r#"inline __int128 add_wrap128(__int128 a, __int128 b) {
  return static_cast<__int128>(static_cast<unsigned __int128>(a) + static_cast<unsigned __int128>(b));
}
inline __int128 sub_wrap128(__int128 a, __int128 b) {
  return static_cast<__int128>(static_cast<unsigned __int128>(a) - static_cast<unsigned __int128>(b));
}
inline __int128 mul_wrap128(__int128 a, __int128 b) {
  return static_cast<__int128>(static_cast<unsigned __int128>(a) * static_cast<unsigned __int128>(b));
}
inline __int128 shl_wrap128(__int128 a, __int128 b) {
  return static_cast<__int128>(static_cast<unsigned __int128>(a) << static_cast<unsigned>(b));
}
inline __int128 div_floor128(__int128 a, __int128 b) {
  if (b == 0) {
    return 0;
  }
  __int128 q = a / b;
  __int128 r = a % b;
  if (r != 0 && ((a < 0) != (b < 0))) {
    --q;
  }
  return q;
}
inline __int128 mod_sign128(__int128 a, __int128 b) {
  if (b == 0) {
    return 0;
  }
  __int128 r = a % b;
  if (r != 0 && ((a < 0) != (b < 0))) {
    r += b;
  }
  return r;
}
inline std::string str128(__int128 v) {
  bool neg = v < 0;
  unsigned __int128 u =
      neg ? static_cast<unsigned __int128>(-(v + 1)) + 1 : static_cast<unsigned __int128>(v);
  std::string s;
  do {
    s.push_back(static_cast<char>('0' + u % 10));
    u /= 10;
  } while (u != 0);
  if (neg) {
    s.push_back('-');
  }
  std::reverse(s.begin(), s.end());
  return s;
}
"#;

// ---------------------------------------------------------------------------
// Header and source.
// ---------------------------------------------------------------------------

struct Src {
    out: String,
    ind: usize,
}

impl Src {
    fn new() -> Self {
        Src {
            out: String::new(),
            ind: 0,
        }
    }
    fn line(&mut self, s: &str) {
        if s.is_empty() {
            self.out.push('\n');
        } else {
            for _ in 0..self.ind {
                self.out.push_str("  ");
            }
            self.out.push_str(s);
            self.out.push('\n');
        }
    }
    fn raw(&mut self, s: &str) {
        self.out.push_str(s);
    }
    fn open(&mut self, s: &str) {
        self.line(&format!("{s} {{"));
        self.ind += 1;
    }
    fn close(&mut self, s: &str) {
        self.ind = self.ind.saturating_sub(1);
        self.line(s);
    }
    fn lines(&mut self, docs: &[String]) {
        for d in docs {
            self.line(d);
        }
    }
}

fn origin_doc(program: &Program, origin: &Origin) -> Vec<String> {
    match origin {
        Origin::Packet { name, routes } => {
            let mut docs = vec![format!("// Packet {name}.")];
            for r in routes {
                docs.push(format!("// Route {} {} {}.", r.state, r.direction, r.id));
            }
            docs
        }
        Origin::Encoding { name } => vec![format!("// Encoding {name}.")],
        Origin::Enum { name } => vec![format!("// Enum {name}.")],
        Origin::Frame { name } => vec![format!("// Frame {name}.")],
        Origin::Field { parent, field } => {
            vec![format!(
                "// Field {}.{field}.",
                pascal(&program.types[*parent].name)
            )]
        }
        Origin::Variant { sum, variant } => {
            let s = &program.types[*sum];
            let crate::ir::typed::TypeKind::Sum { variants } = &s.kind else {
                return vec![format!("// Variant {}.", pascal(&s.name))];
            };
            vec![format!(
                "// Variant {}.{}.",
                pascal(&s.name),
                pascal(&variants[*variant].name)
            )]
        }
    }
}

/// A struct member, boxing recursive back-edges.
fn member_ty(
    program: &Program,
    layout: &Layout,
    ti: TypeIdx,
    fi: usize,
    ty: &Ty,
    optional: bool,
) -> String {
    let inner = cpp_ty(program, ty);
    let stored = if optional {
        format!("std::optional<{inner}>")
    } else {
        inner
    };
    if layout.boxed_field.contains(&(ti, fi)) {
        format!("Box<{stored}>")
    } else {
        stored
    }
}

/// `bool operator==(const T &) const = default;`, preferring fewer lines:
/// whole, then `bool` plus the rest, then hanging `default`.
fn emit_defaulted_eq(src: &mut Src, name: &str) {
    let one = format!("bool operator==(const {name} &) const = default;");
    if src.ind * 2 + one.len() <= MAX_COL {
        src.line(&one);
        return;
    }
    let rest = format!("operator==(const {name} &) const = default;");
    if src.ind * 2 + rest.len() <= MAX_COL {
        src.line("bool");
        src.line(&rest);
        return;
    }
    let head = format!("bool operator==(const {name} &) const =");
    if src.ind * 2 + head.len() <= MAX_COL {
        src.line(&head);
        src.line("    default;");
        return;
    }
    src.line("bool");
    src.line(&format!("operator==(const {name} &) const ="));
    src.ind += 1;
    src.line("default;");
    src.ind -= 1;
}

fn emit_type(program: &Program, layout: &Layout, src: &mut Src, item: &Item) {
    match item {
        Item::Type(ti) => {
            let decl = &program.types[*ti];
            src.lines(&origin_doc(program, &decl.origin));
            match &decl.kind {
                crate::ir::typed::TypeKind::Record { fields } => {
                    src.open(&format!("struct {}", pascal(&decl.name)));
                    for (fi, f) in fields.iter().enumerate() {
                        src.line(&format!(
                            "{} {};",
                            member_ty(program, layout, *ti, fi, &f.ty, f.optional),
                            snake(&f.name)
                        ));
                    }
                    emit_defaulted_eq(src, &pascal(&decl.name));
                    src.close("};");
                }
                crate::ir::typed::TypeKind::Sum { variants } => {
                    for (vi, variant) in variants.iter().enumerate() {
                        let w = layout.wrapper(*ti, vi);
                        if w.existing.is_some() {
                            continue;
                        }
                        src.line(&format!(
                            "// Variant {}.{}.",
                            pascal(&decl.name),
                            pascal(&variant.name)
                        ));
                        src.open(&format!("struct {}", w.name));
                        if let Some(p) = &w.payload {
                            let pty = cpp_ty(program, p);
                            let wi = layout.wrapper_of[&(*ti, vi)];
                            let stored = if layout.boxed_payload.contains(&wi) {
                                format!("Box<{pty}>")
                            } else {
                                pty
                            };
                            src.line(&format!("{stored} value;"));
                        }
                        emit_defaulted_eq(src, &w.name);
                        src.close("};");
                    }
                    let mut alts = Vec::new();
                    for (vi, _) in variants.iter().enumerate() {
                        let w = layout.wrapper(*ti, vi);
                        let base = w
                            .existing
                            .map(|t| pascal(&program.types[t].name))
                            .unwrap_or_else(|| w.name.clone());
                        alts.push(if layout.boxed_alt.contains(&(*ti, vi)) {
                            format!("Box<{base}>")
                        } else {
                            base
                        });
                    }
                    emit_using(src, &pascal(&decl.name), &alts);
                }
                crate::ir::typed::TypeKind::Enum { base, members } => {
                    let head = format!("enum class {} : {}", pascal(&decl.name), int_cpp(*base));
                    let inits = members
                        .iter()
                        .map(|m| format!("{} = {}", pascal(&m.name), m.value))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let one = format!("{head} {{ {inits} }};");
                    if src.ind * 2 + one.len() <= MAX_COL {
                        src.line(&one);
                    } else {
                        src.open(&head);
                        for (i, m) in members.iter().enumerate() {
                            let comma = if i + 1 == members.len() { "" } else { "," };
                            src.line(&format!("{} = {}{}", pascal(&m.name), m.value, comma));
                        }
                        src.close("};");
                    }
                }
                crate::ir::typed::TypeKind::Alias { target } => {
                    src.line(&format!(
                        "using {} = {};",
                        pascal(&decl.name),
                        pascal(&program.types[*target].name)
                    ));
                }
            }
        }
        Item::Wrapper => {
            // Synthetic wrappers are defined with their sum.
        }
    }
}

/// `using Name = std::variant<...>;`, one line when it fits. An overlong
/// list breaks after `=` with the alternatives packed greedily under the
/// `<`: exactly the `clang-format` (LLVM) layout, so no `off/on` guards.
fn emit_using(src: &mut Src, name: &str, alts: &[String]) {
    let one = format!("using {name} = std::variant<{}>;", alts.join(", "));
    if src.ind * 2 + one.len() <= MAX_COL {
        src.line(&one);
        return;
    }
    let budget = MAX_COL - src.ind * 2;
    src.line(&format!("using {name} ="));
    let inner_first = format!("{}std::variant<", " ".repeat(src.ind * 2 + 4));
    let cont = " ".repeat(src.ind * 2 + 17);
    for l in pack_lines(&inner_first, &cont, alts, ">;", budget) {
        src.line(&l);
    }
}

/// Greedy comma packing after `first`, continuing at `cont`, ending the last
/// line with `end`. Returns the rendered lines.
fn pack_lines(first: &str, cont: &str, items: &[String], end: &str, budget: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = first.to_string();
    let mut started = first.is_empty();
    for (i, a) in items.iter().enumerate() {
        let last = i + 1 == items.len();
        let piece = if started { format!(", {a}") } else { a.clone() };
        let tail = if last {
            end.to_string()
        } else {
            ",".to_string()
        };
        if cur.len() + piece.len() + tail.len() <= budget {
            cur.push_str(&piece);
            started = true;
            if last {
                cur.push_str(end);
            }
        } else {
            if started {
                cur.push(',');
            }
            lines.push(cur);
            cur = format!("{cont}{a}");
            started = true;
            if last {
                cur.push_str(end);
            }
        }
    }
    lines.push(cur);
    lines
}
/// Byte index just past the quoted span starting at `s[i]` (`"` or `'`).
fn skip_quoted(s: &[u8], mut i: usize) -> usize {
    let quote = s[i];
    i = next_char(s, i);
    while i < s.len() {
        match s[i] {
            b'\\' => i = next_char(s, i),
            c if c == quote => return i + 1,
            c if c < 0x80 => i += 1,
            _ => i = next_char(s, i),
        }
    }
    i
}

/// Advance one whole character from a boundary index.
fn next_char(s: impl AsRef<[u8]>, mut i: usize) -> usize {
    let s = s.as_ref();
    i += 1;
    while i < s.len() && (s[i] & 0xC0) == 0x80 {
        i += 1;
    }
    i
}

/// Split `s` at top-level commas (nesting depth 0, outside literals).
fn split_top_commas(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = skip_quoted(b, i),
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' if depth == 0 => {
                out.push(s[start..i].trim().to_string());
                i += 1;
                start = i;
            }
            c if c < 0x80 => i += 1,
            _ => i = next_char(s, i),
        }
    }
    out.push(s[start..].trim().to_string());
    out
}

/// Binary operator precedence level for line splitting (loosest first).
fn split_prec(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "<" | "<=" | ">" | ">=" => 7,
        "<<" | ">>" => 8,
        "+" | "-" => 9,
        "*" | "/" | "%" => 10,
        _ => return None,
    })
}

/// Split `s` at top-level binary operators of the loosest level present.
/// Operators require single spaces on both sides (our printer always spaces
/// them; this excludes templates, unaries, `->`, and `>>` inside types).
/// Ternaries never split. Returns the pieces and the operator, or None.
fn split_loosest(s: &str) -> Option<(Vec<String>, &'static str)> {
    let b = s.as_bytes();
    const OPS: &[&str] = &[
        "||", "&&", "==", "!=", "<=", ">=", "<<", ">>", "<", ">", "+", "-", "*", "/", "%", "|",
        "^", "&",
    ];
    let mut depth = 0u32;
    let mut found: Vec<(usize, &'static str, u8)> = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = skip_quoted(b, i),
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ if depth == 0 => {
                let mut matched = false;
                for op in OPS {
                    if s[i..].starts_with(op)
                        && i > 0
                        && s[..i].ends_with(' ')
                        && s[i + op.len()..].starts_with(' ')
                        && let Some(level) = split_prec(op)
                    {
                        found.push((i, op, level));
                        i += op.len();
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    i = next_char(s, i);
                }
            }
            c if c < 0x80 => i += 1,
            _ => i = next_char(s, i),
        }
    }
    let loosest = found.iter().map(|(_, _, l)| *l).min()?;
    let op = found.iter().find(|(_, _, l)| *l == loosest)?.1;
    let mut pieces = Vec::new();
    let mut start = 0usize;
    for (pos, o, level) in found {
        if level != loosest {
            continue;
        }
        pieces.push(s[start..pos].trim().to_string());
        start = pos + o.len();
    }
    pieces.push(s[start..].trim().to_string());
    if pieces.len() < 2 {
        return None;
    }
    Some((pieces, op))
}

/// Split call-shaped `s` into `(head, args, trailer)`. The head must look
/// like a call head (identifiers, `::`, `.`, `->`, one `<>` group); anything
/// else yields None.
fn split_call(s: &str) -> Option<(String, Vec<String>, String)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' => i = skip_quoted(b, i),
            b'(' => break,
            c if c < 0x80 => i += 1,
            _ => i = next_char(s, i),
        }
    }
    if i >= b.len() {
        return None;
    }
    let mut head = s[..i].trim().to_string();
    if head.ends_with('>') {
        let mut depth = 0u32;
        let mut j = head.len();
        let hb = head.as_bytes();
        let mut ok = false;
        while j > 0 {
            j -= 1;
            match hb[j] {
                b'>' => depth += 1,
                b'<' => {
                    depth -= 1;
                    if depth == 0 {
                        ok = true;
                        break;
                    }
                }
                b'(' | b')' => break,
                _ => {}
            }
        }
        if !ok {
            return None;
        }
        head.truncate(j);
        head = head.trim_end().to_string();
    }
    if head.is_empty()
        || !head
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_:.-~>*".contains(c))
    {
        return None;
    }
    let mut depth = 0u32;
    let mut j = i;
    while j < b.len() {
        match b[j] {
            b'"' | b'\'' => j = skip_quoted(b, j),
            b'(' => {
                depth += 1;
                j += 1;
            }
            b')' => {
                depth -= 1;
                j += 1;
                if depth == 0 {
                    break;
                }
            }
            c if c < 0x80 => j += 1,
            _ => j = next_char(s, j),
        }
    }
    if depth != 0 {
        return None;
    }
    let inner = s[i + 1..j - 1].to_string();
    let trailer = s[j..].to_string();
    Some((s[..i].trim().to_string(), split_top_commas(&inner), trailer))
}

/// Emit `head(arg, ...)end`: one line when it fits, otherwise packed.
/// Declarations (signatures) continue at one indent and end one argument per
/// line; statements (calls, throws) continue at the statement indent.
/// `is_decl` selects the two styles.
fn pack_call(src: &mut Src, head: &str, args: &[String], end: &str, is_decl: bool) {
    let one = format!("{head}({}{end}", args.join(", "));
    if src.ind * 2 + one.len() <= MAX_COL {
        src.line(&one);
        return;
    }
    let base = src.ind * 2;
    for l in break_call(head, args, end, base, is_decl) {
        src.out.push_str(&l);
        src.out.push('\n');
    }
}

/// Break `head(args)end` (absolute columns, LLVM style). `base` is the
/// statement's indent; every returned line carries absolute leading spaces.
fn break_call(head: &str, args: &[String], end: &str, base: usize, is_decl: bool) -> Vec<String> {
    let pad = |n: usize| " ".repeat(n);
    let one = format!("{head}({}{end}", args.join(", "));
    if base + one.len() <= MAX_COL {
        return vec![format!("{}{}", pad(base), one)];
    }
    let paren_col = base + head.len() + 1;
    // A lone binary argument breaks in place, continuations paren-aligned.
    if args.len() == 1
        && let Some(lines) = break_single_arg(head, &args[0], end, base, paren_col)
    {
        return lines;
    }
    // Paren-aligned greedy comma packing, moving whole overlong arguments.
    if let Some(lines) = pack_paren(head, args, end, base, paren_col) {
        return lines;
    }
    if is_decl {
        // Return type alone when `name(args)end` fits on its own line.
        if let Some((ret, name)) = head.rsplit_once(' ') {
            let alone = format!("{name}({}{end}", args.join(", "));
            if base + alone.len() <= MAX_COL {
                return vec![
                    format!("{}{}", pad(base), ret),
                    format!("{}{}", pad(base), alone),
                ];
            }
        }
    }
    // Break after the paren; pack arguments at one indent, breaking
    // overlong ones recursively.
    let mut out = vec![format!("{}{}(", pad(base), head)];
    out.extend(pack_args_at(args, end, base + 4));
    out
}

/// A lone binary argument breaks in place: it starts on the head line and
/// its pieces continue paren-aligned. None when anything overflows (the
/// general comma flow takes over instead).
fn break_single_arg(
    head: &str,
    arg: &str,
    end: &str,
    base: usize,
    paren_col: usize,
) -> Option<Vec<String>> {
    let (pieces, op) = split_loosest(arg)?;
    let mut out = Vec::new();
    // First piece stays on the head line with a trailing operator.
    let mut cur = format!("{}{}({}", " ".repeat(base), head, pieces[0]);
    cur.push(' ');
    cur.push_str(op);
    if cur.len() > MAX_COL {
        return None;
    }
    out.push(cur);
    // Remaining pieces each take a paren-aligned line; the last takes `end`.
    for (i, p) in pieces[1..].iter().enumerate() {
        let last = i + 1 == pieces[1..].len();
        let mut line = format!("{}{}", " ".repeat(paren_col), p);
        if last {
            line.push_str(end);
        } else {
            line.push(' ');
            line.push_str(op);
        }
        if line.len() > MAX_COL {
            return None;
        }
        out.push(line);
    }
    Some(out)
}

/// Greedy paren-aligned comma packing with atomic arguments: an argument
/// that fits nowhere aligned fails the whole attempt (None).
/// Greedy paren-aligned comma packing. An argument that fits nowhere aligned
/// moves whole and breaks once (binary pieces or one call level, `+4`
/// continuations); anything still overflowing fails the whole attempt (None),
/// sending the call to break-after-paren flow.
fn pack_paren(
    head: &str,
    args: &[String],
    end: &str,
    base: usize,
    paren_col: usize,
) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    let mut cur = format!("{}{}(", " ".repeat(base), head);
    let mut started = false;
    for (i, a) in args.iter().enumerate() {
        let last = i + 1 == args.len();
        let tail = if last {
            end.to_string()
        } else {
            ",".to_string()
        };
        let piece = if started { format!(", {a}") } else { a.clone() };
        if cur.len() + piece.len() + tail.len() <= MAX_COL {
            cur.push_str(&piece);
            started = true;
            if last {
                cur.push_str(end);
            }
            continue;
        }
        // Finish the current line (unless it is just the head) and move the
        // argument whole to the next aligned line.
        if started {
            cur.push(',');
            lines.push(cur);
        } else {
            lines.push(cur);
        }
        cur = format!("{}{}", " ".repeat(paren_col), a);
        started = true;
        if cur.len() + tail.len() <= MAX_COL {
            if last {
                cur.push_str(end);
            }
            continue;
        }
        // Break once, `+4` continuations (with `tail` on the last piece);
        // deeper nesting fails the attempt. Subsequent arguments start
        // fresh on the next aligned line.
        let broken = break_arg_shallow(a, paren_col, &tail)?;
        for b in broken {
            lines.push(b);
        }
        cur = " ".repeat(paren_col);
        started = false;
    }
    if !cur.trim().is_empty() {
        lines.push(cur);
    }
    Some(lines)
}

/// Pack arguments greedily at absolute indent `at`, breaking overlong ones
/// recursively (binary chains and nested calls, `+4` continuations). The last
/// line ends with `end`. Unbreakable arguments are emitted overlong.
/// `need_sep` tracks whether the next piece needs a `", "` prefix (i.e. the
/// current line ends with unterminated argument content).
fn pack_args_at(args: &[String], end: &str, at: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = " ".repeat(at);
    let mut need_sep = false;
    for (i, a) in args.iter().enumerate() {
        let last = i + 1 == args.len();
        let piece = if need_sep {
            format!(", {a}")
        } else {
            a.clone()
        };
        let tail = if last {
            end.to_string()
        } else {
            ",".to_string()
        };
        if cur.len() + piece.len() + tail.len() <= MAX_COL {
            cur.push_str(&piece);
            need_sep = true;
            if last {
                cur.push_str(end);
            }
            continue;
        }
        if need_sep {
            // `cur` holds unterminated content: terminate it. (`cur` is never
            // bare here, so this always emits a content line.)
            cur.push(',');
            out.push(std::mem::take(&mut cur));
        }
        // `cur` is bare indent here (dropped, never emitted); break the
        // argument starting fresh at `at`, then continue with its last line
        // (which already ends with its separator).
        let broken = break_arg_at(a, at);
        for b in &broken[..broken.len().saturating_sub(1)] {
            out.push(b.clone());
        }
        cur = broken.into_iter().last().unwrap_or_else(|| " ".repeat(at));
        if last {
            cur.push_str(end);
        } else {
            cur.push(',');
        }
        need_sep = false;
    }
    out.push(cur);
    out
}

/// Break one argument starting fresh at absolute indent `at`. All returned
/// lines carry absolute indents and no trailers. Binary chains split after
/// each top-level operator (`+4` continuations); nested calls break after
/// their paren (`+4`, recursing); anything else stays whole.
fn break_arg_at(arg: &str, at: usize) -> Vec<String> {
    if let Some((pieces, op)) = split_loosest(arg) {
        // One piece per line: first at `at`, rest at `at + 4`, trailing
        // operator on every line but the last. Pieces are emitted whole
        // (gate-driven if one ever overflows).
        let mut out = Vec::new();
        for (i, p) in pieces.iter().enumerate() {
            let last = i + 1 == pieces.len();
            let indent = if i == 0 { at } else { at + 4 };
            let mut line = format!("{}{}", " ".repeat(indent), p);
            if !last {
                line.push(' ');
                line.push_str(op);
            }
            out.push(line);
        }
        return out;
    }
    if let Some((head, args, trailer)) = split_call(arg) {
        let mut out = vec![format!("{}{}(", " ".repeat(at), head)];
        out.extend(pack_args_at(&args, &format!("){trailer}"), at + 4));
        return out;
    }
    vec![format!("{}{}", " ".repeat(at), arg)]
}

/// Break one argument shallowly (a single level) for paren-flow: binary
/// pieces or one call level, `+4` continuations, `tail` on the last line.
/// None when anything still overflows.
fn break_arg_shallow(arg: &str, at: usize, tail: &str) -> Option<Vec<String>> {
    if let Some((pieces, op)) = split_loosest(arg) {
        let mut out = Vec::new();
        out.push(format!("{}{}", " ".repeat(at), pieces[0]));
        for (i, p) in pieces[1..].iter().enumerate() {
            let last = i + 1 == pieces[1..].len();
            let mut line = format!("{}{}", " ".repeat(at + 4), p);
            if last {
                line.push_str(tail);
            }
            if line.len() > MAX_COL {
                return None;
            }
            out.push(line);
        }
        // Trailing operators on every line but the last.
        let n = out.len();
        for (i, l) in out.iter_mut().enumerate() {
            if i + 1 < n {
                l.push(' ');
                l.push_str(op);
            }
        }
        // First line (at `at`) must also fit (it ends with `op` now).
        if out[0].len() > MAX_COL {
            return None;
        }
        return Some(out);
    }
    if let Some((head, args, trailer)) = split_call(arg) {
        let mut out = vec![format!("{}{}(", " ".repeat(at), head)];
        // One level: inner arguments atomic at `+4`.
        let mut cur: String = " ".repeat(at + 4);
        let mut started = false;
        for (i, a) in args.iter().enumerate() {
            let last = i + 1 == args.len();
            let inner_tail = if last {
                format!("){trailer}")
            } else {
                ",".to_string()
            };
            let piece = if started { format!(", {a}") } else { a.clone() };
            if cur.len() + piece.len() + inner_tail.len() <= MAX_COL {
                cur.push_str(&piece);
                started = true;
                if last {
                    cur.push_str(&inner_tail);
                }
            } else {
                return None;
            }
        }
        if !started {
            // No arguments placed (empty args): `head()` + trailer on one line.
            cur.push_str(&format!("){trailer}"));
        }
        out.push(cur);
        // Attach the outer `tail` to the very last line and verify.
        let n = out.len();
        out[n - 1].push_str(tail);
        if out.iter().any(|l| l.len() > MAX_COL) {
            return None;
        }
        return Some(out);
    }
    None
}

/// A signature: `Ret Name(args)` packed, breaking between the return type
/// and the name when even the head overflows. Return types never contain
/// spaces, so the split is exact.
fn pack_sig(src: &mut Src, head: &str, args: &[String], end: &str) {
    if src.ind * 2 + head.len() < MAX_COL {
        pack_call(src, head, args, end, true);
        return;
    }
    match head.rfind(' ') {
        Some(i) => {
            src.line(&head[..i]);
            pack_call(src, &head[i + 1..], args, end, true);
        }
        None => pack_call(src, head, args, end, true),
    }
}

fn emit_header(program: &Program, layout: &Layout) -> BpsResult<String> {
    let mut src = Src::new();
    src.raw(RUNTIME_BASE);
    if layout.uses_i128 {
        src.raw(RUNTIME_128);
    }
    for decl in &program.types {
        if matches!(decl.kind, crate::ir::typed::TypeKind::Record { .. }) {
            src.line(&format!("struct {};", pascal(&decl.name)));
        }
    }
    for w in &layout.wrappers {
        if w.existing.is_none() {
            src.line(&format!("struct {};", w.name));
        }
    }
    src.line("");
    let mut first = true;
    for item in &layout.order {
        if matches!(item, Item::Wrapper) {
            continue;
        }
        if !first {
            src.line("");
        }
        first = false;
        emit_type(program, layout, &mut src, item);
    }
    for (ci, codec) in program.codecs.iter().enumerate() {
        let (rname, wname) = cursor_names(&codec.decode);
        src.line("");
        src.lines(&origin_doc(program, &codec.origin));
        let decode = sig_string(program, ci, true, &rname, &wname)?;
        pack_sig(&mut src, &decode.0, &decode.1, ");");
        src.lines(&origin_doc(program, &codec.origin));
        let encode = sig_string(program, ci, false, &rname, &wname)?;
        pack_sig(&mut src, &encode.0, &encode.1, ");");
    }
    src.line("} // namespace bps");
    Ok(finish(src.out))
}

fn finish(mut out: String) -> String {
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    out
}

/// Cursor spellings for one body: `r`/`w` unless a local takes the name.
fn cursor_names(body: &Body) -> (String, String) {
    let taken: HashSet<String> = body.locals.iter().map(|l| l.name.snake()).collect();
    let mut r = "r".to_string();
    if taken.contains(r.as_str()) {
        r = "reader".to_string();
    }
    if taken.contains(r.as_str()) {
        r = "reader_".to_string();
    }
    let mut w = "w".to_string();
    if taken.contains(w.as_str()) {
        w = "writer".to_string();
    }
    if taken.contains(w.as_str()) {
        w = "writer_".to_string();
    }
    (r, w)
}

/// A function signature without the body or trailing semicolon.
fn sig_string(
    program: &Program,
    codec: CodecIdx,
    decode: bool,
    rname: &str,
    wname: &str,
) -> BpsResult<(String, Vec<String>)> {
    let c = &program.codecs[codec];
    let body = if decode { &c.decode } else { &c.encode };
    if decode {
        let head = format!(
            "{} {}",
            cpp_ty(program, &c.value),
            decode_fn(program, codec)
        );
        let mut args = Vec::new();
        args.push(if body_reads(body) {
            format!("Reader &{rname}")
        } else {
            "Reader &".to_string()
        });
        for (i, p) in c.params.iter().enumerate() {
            let local = body.params[i];
            let name = if body.locals[local].uses == 0 {
                None
            } else {
                Some(snake(&body.locals[local].name))
            };
            args.push(param_decl(program, &p.ty, p.optional, name.as_deref()));
        }
        Ok((head, args))
    } else {
        let Some(value) = body.value else {
            return Err(emit("cpp: encode body without a value"));
        };
        let name = if body.locals[value].uses == 0 {
            None
        } else {
            Some(snake(&body.locals[value].name))
        };
        let head = format!("void {}", encode_fn(program, codec));
        let mut args = Vec::new();
        args.push(if body_writes(body) {
            format!("Writer &{wname}")
        } else {
            "Writer &".to_string()
        });
        let l = &body.locals[value];
        args.push(param_decl(program, &l.ty, l.optional, name.as_deref()));
        for (i, p) in c.params.iter().enumerate() {
            let local = body.params[i];
            let name = if body.locals[local].uses == 0 {
                None
            } else {
                Some(snake(&body.locals[local].name))
            };
            args.push(param_decl(program, &p.ty, p.optional, name.as_deref()));
        }
        Ok((head, args))
    }
}

fn param_decl(program: &Program, ty: &Ty, optional: bool, name: Option<&str>) -> String {
    let t = opt_cpp(program, ty, optional);
    let by_value = !optional
        && (matches!(ty, Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64) || is_enum_ty(program, ty));
    match (by_value, name) {
        (true, Some(n)) => format!("{t} {n}"),
        (true, None) => t,
        (false, Some(n)) => format!("const {t} &{n}"),
        (false, None) => format!("const {t} &"),
    }
}

fn is_enum_ty(program: &Program, ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Named(t)
            if matches!(program.types[*t].kind, crate::ir::typed::TypeKind::Enum { .. })
    )
}

fn emit_sources(program: &Program, layout: &Layout) -> BpsResult<BTreeMap<String, String>> {
    let mut bodies: BTreeMap<String, Src> = BTreeMap::new();
    for ci in 0..program.codecs.len() {
        let stem = program.output_stem(&program.codecs[ci].origin);
        let src = bodies.entry(stem).or_insert_with(Src::new);
        for decode in [true, false] {
            if !src.out.is_empty() {
                src.line("");
            }
            src.raw(&emit_function(program, layout, ci, decode)?);
        }
    }
    let mut files = BTreeMap::new();
    for (stem, src) in bodies {
        let mut out = String::from("#include \"bpsgen.hpp\"\n\nnamespace bps {\n");
        out.push_str(&src.out);
        out.push_str("} // namespace bps\n");
        files.insert(stem, finish(out));
    }
    Ok(files)
}

// ---------------------------------------------------------------------------
// Functions: statements and expressions.
// ---------------------------------------------------------------------------

/// Whether a decode body reads from its `Reader` (including offsets for
/// errors and `Remaining` expressions).
fn body_reads(body: &Body) -> bool {
    body.stmts.iter().any(stmt_reads)
}

fn stmt_reads(s: &Stmt) -> bool {
    match s {
        Stmt::Read { .. }
        | Stmt::Decode { .. }
        | Stmt::Utf8 { .. }
        | Stmt::Repeat { .. }
        | Stmt::Require { .. } => true,
        Stmt::Write { value, .. } => expr_reads(value),
        Stmt::Encode { args, value, .. } => expr_reads(value) || args.iter().any(expr_reads),
        Stmt::ForEach { list, body, .. } => expr_reads(list) || body.iter().any(stmt_reads),
        Stmt::Match { arms, other, .. } => {
            other.is_none()
                || arms.iter().any(|a| a.body.iter().any(stmt_reads))
                || other.as_ref().is_some_and(|o| o.iter().any(stmt_reads))
        }
        Stmt::If { cond, then, els } => {
            expr_reads(cond) || then.iter().any(stmt_reads) || els.iter().any(stmt_reads)
        }
        Stmt::Let { value, .. } | Stmt::Unwrap { src: value, .. } => expr_reads(value),
        Stmt::ExpectVariant { sum, .. } => expr_reads(sum),
        Stmt::Return(e) => expr_reads(e),
        Stmt::Declare { .. } | Stmt::Fail(_) => false,
    }
}

fn body_writes(body: &Body) -> bool {
    body.stmts.iter().any(stmt_writes)
}

fn stmt_writes(s: &Stmt) -> bool {
    match s {
        Stmt::Write { .. } | Stmt::Encode { .. } | Stmt::ForEach { .. } => true,
        Stmt::If { then, els, .. } => then.iter().any(stmt_writes) || els.iter().any(stmt_writes),
        Stmt::Match { arms, other, .. } => {
            arms.iter().any(|a| a.body.iter().any(stmt_writes))
                || other.as_ref().is_some_and(|o| o.iter().any(stmt_writes))
        }
        _ => false,
    }
}

fn expr_reads(e: &Expr) -> bool {
    match e {
        Expr::Remaining => true,
        Expr::Local(_) | Expr::Int { .. } | Expr::Bool(_) | Expr::Str(_) => false,
        Expr::Field { base, .. }
        | Expr::Present(base)
        | Expr::Get(base)
        | Expr::Not(base)
        | Expr::StrBytes(base) => expr_reads(base),
        Expr::Binary { lhs, rhs, .. } => expr_reads(lhs) || expr_reads(rhs),
        Expr::Cond {
            cond, then, els, ..
        } => expr_reads(cond) || expr_reads(then) || expr_reads(els),
        Expr::Cast { expr, .. } => expr_reads(expr),
        Expr::Call { args, .. } => args.iter().any(expr_reads),
        Expr::Construct { fields, .. } => fields.iter().any(expr_reads),
        Expr::Variant { payload, .. } => payload.as_ref().is_some_and(|p| expr_reads(p)),
    }
}

struct Emitter<'a> {
    program: &'a Program,
    layout: &'a Layout,
    snake: String,
    body: &'a Body,
    is_encode: bool,
    rname: String,
    wname: String,
    armc: usize,
    loop_depth: usize,
    declared: HashSet<LocalId>,
    src: Src,
}

struct EVal {
    s: String,
    boxed: bool,
}

fn emit_function(
    program: &Program,
    layout: &Layout,
    codec: CodecIdx,
    decode: bool,
) -> BpsResult<String> {
    let c = &program.codecs[codec];
    let body: &Body = if decode { &c.decode } else { &c.encode };
    let (rname, wname) = cursor_names(body);
    let mk = |src: Src| Emitter {
        program,
        layout,
        snake: c.name.snake(),
        body,
        is_encode: !decode,
        rname: rname.clone(),
        wname: wname.clone(),
        armc: 0,
        loop_depth: 0,
        declared: HashSet::new(),
        src,
    };
    let (head, args) = sig_string(program, codec, decode, &rname, &wname)?;
    let sig_one = format!("{head}({})", args.join(", "));
    // Render the body aside at function-level indent, so width checks are
    // exact; short bodies join onto the signature line.
    let mut probe_src = Src::new();
    probe_src.ind = 1;
    let mut probe = mk(probe_src);
    let stmts = body.stmts.clone();
    for s in &stmts {
        probe.stmt(s);
    }
    let mut out = Src::new();
    out.lines(&origin_doc(program, &c.origin));
    let mut probe_lines: Vec<&str> = probe.src.out.lines().collect();
    while probe_lines.last().is_some_and(|l| l.is_empty()) {
        probe_lines.pop();
    }
    if sig_one.len() <= MAX_COL {
        if probe_lines.is_empty() {
            out.line(&format!("{sig_one} {{}}"));
            return Ok(out.out);
        }
        if probe_lines.len() == 1 {
            let joined = format!("{sig_one} {{ {} }}", probe_lines[0].trim_start());
            if joined.len() <= MAX_COL {
                out.line(&joined);
                return Ok(out.out);
            }
        }
    }
    pack_sig(&mut out, &head, &args, ") {");
    for l in &probe_lines {
        out.line(l);
    }
    out.line("}");
    Ok(out.out)
}

impl<'a> Emitter<'a> {
    fn fits(&self, s: &str) -> bool {
        self.src.ind * 2 + s.len() <= MAX_COL
    }

    fn budget(&self) -> usize {
        MAX_COL - self.src.ind * 2
    }

    fn local_name(&self, id: LocalId) -> String {
        snake(&self.body.locals[id].name)
    }

    fn bind_decl(&self, dst: &crate::ir::typed::Bind) -> String {
        let l = &self.body.locals[dst.local];
        let name = snake(&l.name);
        if !dst.declare {
            return name;
        }
        let ty = opt_cpp(self.program, &l.ty, l.optional);
        if l.uses == 0 {
            format!("[[maybe_unused]] {ty} {name}")
        } else {
            format!("{ty} {name}")
        }
    }

    fn throw_msg(&mut self, kind: &str) {
        let q = format!("\"{}\"", self.snake);
        if self.is_encode {
            pack_call(
                &mut self.src,
                "throw EncodeError",
                &[q, format!("\"{kind}\"")],
                ");",
                false,
            );
        } else {
            pack_call(
                &mut self.src,
                "throw DecodeError",
                &[q, format!("{}.pos()", self.rname), format!("\"{kind}\"")],
                ");",
                false,
            );
        }
    }

    /// `head(args...);` as a statement, packed when long.
    fn stmt_call(&mut self, head: &str, args: &[String]) {
        pack_call(&mut self.src, head, args, ");", false);
    }

    /// `decl = init;` as one C++ expression (the typed program's tree). A
    /// call-shaped initializer that overflows breaks after `=` with the call
    /// packed at one deeper indent; anything else stays on one line, the way
    /// clang-format leaves long arithmetic, concats, and braced inits alone.
    fn assign_expr(&mut self, decl: &str, e: &Expr) {
        let s = self.val(e);
        let one = format!("{decl} = {s};");
        if self.fits(&one) {
            self.src.line(&one);
            return;
        }
        if let Some((head, args)) = self.call_parts(e) {
            self.src.line(&format!("{decl} ="));
            self.src.ind += 1;
            pack_call(&mut self.src, &head, &args, ");", false);
            self.src.ind -= 1;
            return;
        }
        self.src.line(&one);
    }

    /// Split a call-shaped expression into its head and argument strings.
    fn call_parts(&mut self, e: &Expr) -> Option<(String, Vec<String>)> {
        match e {
            Expr::Call { func, args } => {
                let vals = args.iter().map(|a| self.val(a)).collect();
                Some((call_head(*func).to_string(), vals))
            }
            Expr::Cast { expr, to } => Some((
                format!("static_cast<{}>", cpp_ty(self.program, to)),
                vec![self.val(expr)],
            )),
            Expr::StrBytes(inner) => Some(("str_bytes".to_string(), vec![self.val(inner)])),
            _ => None,
        }
    }

    /// `decl = init;`, hanging the initializer at +4 columns when the line
    /// runs long.
    fn decl_init(&mut self, decl: &str, init: &str) {
        let one = format!("{decl} = {init};");
        if self.fits(&one) {
            self.src.line(&one);
            return;
        }
        self.src.line(&format!("{decl} ="));
        self.src.line(&format!("    {init};"));
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Declare { local, .. } => {
                let d = self.bind_decl(&crate::ir::typed::Bind {
                    local: *local,
                    declare: true,
                    some: false,
                });
                self.src.line(&format!("{d}{{}};"));
            }
            Stmt::Let { dst, value } => {
                let d = self.bind_decl(dst);
                let v = value.clone();
                self.assign_expr(&d, &v);
            }
            Stmt::Read { dst, op } => {
                let d = self.bind_decl(dst);
                let r = self.rname.clone();
                let q = format!("\"{}\"", self.snake);
                match op {
                    ReadOp::U8 => self.assign_call(&d, &format!("{r}.u8"), &[q]),
                    ReadOp::F32 => self.assign_call(&d, &format!("{r}.f32"), &[q]),
                    ReadOp::F64 => self.assign_call(&d, &format!("{r}.f64"), &[q]),
                    ReadOp::Bytes(n) => {
                        let n = n.clone();
                        let nv = self.val(&n);
                        self.assign_call(&d, &format!("{r}.bytes"), &[nv, q]);
                    }
                    ReadOp::Rest => self.assign_call(&d, &format!("{r}.rest"), &[]),
                }
            }
            Stmt::Decode { dst, codec, args } => {
                let d = self.bind_decl(dst);
                let f = decode_fn(self.program, *codec);
                let r = self.rname.clone();
                let mut call = vec![r];
                for a in args {
                    call.push(self.val(a));
                }
                self.assign_call(&d, &f, &call);
            }
            Stmt::Utf8 { dst, src } => {
                let d = self.bind_decl(dst);
                let r = self.rname.clone();
                let q = format!("\"{}\"", self.snake);
                let sv = match src {
                    Expr::Local(id)
                        if self.body.locals[*id].uses == 1 && !self.body.locals[*id].optional =>
                    {
                        format!("std::move({})", self.local_name(*id))
                    }
                    _ => self.val(src),
                };
                self.assign_call(&d, &format!("{r}.utf8"), &[sv, q]);
            }
            Stmt::Write { op, value } => {
                let w = self.wname.clone();
                let v = value.clone();
                let vv = self.val(&v);
                let q = format!("\"{}\"", self.snake);
                match op {
                    crate::ir::typed::WriteOp::U8 => {
                        let byte = crate::ir::typed::byte_literal(&v, true);
                        self.stmt_call(&format!("{w}.u8"), &[byte.unwrap_or(vv)])
                    }
                    crate::ir::typed::WriteOp::F32 => self.stmt_call(&format!("{w}.f32"), &[vv]),
                    crate::ir::typed::WriteOp::F64 => self.stmt_call(&format!("{w}.f64"), &[vv]),
                    crate::ir::typed::WriteOp::Bytes(n) => {
                        let n = n.clone();
                        let nv = self.val(&n);
                        self.stmt_call(&format!("{w}.bytes_sized"), &[vv, nv, q]);
                    }
                    crate::ir::typed::WriteOp::Rest => self.stmt_call(&format!("{w}.bytes"), &[vv]),
                }
            }
            Stmt::Encode { codec, args, value } => {
                let f = encode_fn(self.program, *codec);
                let w = self.wname.clone();
                let v = value.clone();
                let mut call = vec![w, self.val(&v)];
                for a in args {
                    call.push(self.val(a));
                }
                self.stmt_call(&f, &call);
            }
            Stmt::Require { cond, text } => {
                let c = cond.clone();
                let neg = self.program.negated(self.body, &c);
                let cv = self.val(&neg);
                self.src.open(&format!("if ({cv})"));
                self.throw_msg(&format!("require failed: {text}"));
                self.src.close("}");
            }
            Stmt::Unwrap { dst, src, field } => {
                let r = self.render(src);
                let base = if is_postfix(src) {
                    r.s.clone()
                } else {
                    format!("({})", r.s)
                };
                let dot = if r.boxed { "->" } else { "." };
                self.src.open(&format!("if (!{base}{dot}has_value())"));
                self.throw_msg(&format!("missing field {field}"));
                self.src.close("}");
                let access = format!("{base}{dot}value()");
                if dst.declare {
                    let l = &self.body.locals[dst.local];
                    let ty = cpp_ty(self.program, &l.ty);
                    let name = snake(&l.name);
                    let decl = if l.uses == 0 {
                        format!("[[maybe_unused]] const {ty} &{name}")
                    } else {
                        format!("const {ty} &{name}")
                    };
                    self.decl_init(&decl, &access);
                    self.declared.insert(dst.local);
                } else {
                    let name = self.local_name(dst.local);
                    self.src.line(&format!("{name} = {access};"));
                }
            }
            Stmt::If { cond, then, els } => {
                let c = cond.clone();
                let cv = self.val(&c);
                self.src.open(&format!("if ({cv})"));
                let then = then.clone();
                for s in &then {
                    self.stmt(s);
                }
                if els.is_empty() {
                    self.src.close("}");
                } else {
                    self.src.close("} else {");
                    self.src.ind += 1;
                    let els = els.clone();
                    for s in &els {
                        self.stmt(s);
                    }
                    self.src.ind -= 1;
                    self.src.line("}");
                }
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => {
                let s = scrutinee.clone();
                let a = arms.clone();
                let o = other.clone();
                self.stmt_match(&s, &a, &o);
            }
            Stmt::Repeat {
                mode,
                list,
                item,
                body,
            } => {
                let (m, l, i, b) = (mode.clone(), *list, *item, body.clone());
                self.stmt_repeat(&m, l, i, &b);
            }
            Stmt::ForEach { list, item, body } => {
                let l = list.clone();
                let lv = self.val(&l);
                let iname = self.local_name(*item);
                self.src.open(&format!("for (const auto &{iname} : {lv})"));
                self.loop_depth += 1;
                let body = body.clone();
                for s in &body {
                    self.stmt(s);
                }
                self.loop_depth -= 1;
                self.src.close("}");
            }
            Stmt::ExpectVariant {
                dst,
                sum,
                ty,
                variant,
            } => {
                let (d, s, t, v) = (*dst, sum.clone(), *ty, *variant);
                self.stmt_expect(&d, &s, t, v);
            }
            Stmt::Return(e) => {
                let v = e.clone();
                self.stmt_return(&v);
            }
            Stmt::Fail(_) => {
                self.throw_msg("cannot encode until-end");
            }
        }
    }

    /// `decl = head(args...);`, packing long calls.
    fn assign_call(&mut self, decl: &str, head: &str, args: &[String]) {
        let one = format!("{decl} = {head}({});", args.join(", "));
        if self.fits(&one) {
            self.src.line(&one);
            return;
        }
        let call = format!("{head}({})", args.join(", "));
        if self.src.ind * 2 + 4 + call.len() < MAX_COL {
            self.decl_init(decl, &call);
            return;
        }
        pack_call(&mut self.src, &format!("{decl} = {head}"), args, ");", true);
    }

    fn stmt_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[crate::ir::typed::MatchArm],
        other: &Option<Vec<Stmt>>,
    ) {
        if arms.is_empty() {
            if let Some(o) = other {
                let o = o.clone();
                for s in &o {
                    self.stmt(s);
                }
            } else {
                let s = scrutinee.clone();
                self.match_throw(&s);
            }
            return;
        }
        let mut units = Vec::new();
        let mut bodies = Vec::new();
        for a in arms {
            let mut tags = Vec::new();
            for t in &a.tags {
                let s = scrutinee.clone();
                tags.push(format!("{} == {t}", self.paren_child(&s, BinOp::Eq, true)));
            }
            units.push(tags.join(" || "));
            bodies.push(a.body.clone());
        }
        let n = arms.len();
        for i in 0..n {
            let prefix = if i == 0 { "if (" } else { "} else if (" };
            let parts: Vec<&str> = units[i].split(" || ").collect();
            let lines = cond_lines(prefix, &parts, ") {", self.budget());
            for l in &lines {
                self.src.line(l);
            }
            self.src.ind += 1;
            for s in &bodies[i] {
                self.stmt(s);
            }
            self.src.ind -= 1;
            if i + 1 == n {
                if let Some(o) = other {
                    self.src.line("} else {");
                    self.src.ind += 1;
                    let o = o.clone();
                    for s in &o {
                        self.stmt(s);
                    }
                    self.src.ind -= 1;
                    self.src.line("}");
                } else {
                    self.src.line("} else {");
                    self.src.ind += 1;
                    let s = scrutinee.clone();
                    self.match_throw(&s);
                    self.src.ind -= 1;
                    self.src.line("}");
                }
            }
        }
    }

    /// The `std::to_string` operand of a dynamic error message, printed inline
    /// as one expression; long throw lines break around it canonically.
    fn stringify_operand(&mut self, e: &Expr) -> String {
        let (ty, _) = self.program.expr_ty(self.body, e);
        let stringify = if matches!(ty, Ty::Int(IntTy::I128)) {
            "str128"
        } else {
            "std::to_string"
        };
        let v = e.clone();
        let s = self.val(&v);
        format!("{stringify}({s})")
    }

    fn match_throw(&mut self, scrutinee: &Expr) {
        let s = scrutinee.clone();
        let sv = self.stringify_operand(&s);
        let msg = format!("no match arm for tag \" + {sv}");
        if self.is_encode {
            pack_call(
                &mut self.src,
                "throw EncodeError",
                &[format!("\"{}\"", self.snake), format!("\"{msg}")],
                ");",
                false,
            );
        } else {
            pack_call(
                &mut self.src,
                "throw DecodeError",
                &[
                    format!("\"{}\"", self.snake),
                    format!("{}.pos()", self.rname),
                    format!("\"{msg}"),
                ],
                ");",
                false,
            );
        }
    }

    fn stmt_repeat(&mut self, mode: &RepeatMode, list: LocalId, item: LocalId, body: &[Stmt]) {
        let (lname, lty, luses) = {
            let l = &self.body.locals[list];
            (snake(&l.name), l.ty.clone(), l.uses)
        };
        if !self.declared.contains(&list) {
            let item_ty = match &lty {
                Ty::List(inner) => cpp_ty(self.program, inner),
                other => cpp_ty(self.program, other),
            };
            let decl = if luses == 0 {
                format!("[[maybe_unused]] std::vector<{item_ty}> {lname}")
            } else {
                format!("std::vector<{item_ty}> {lname}")
            };
            self.src.line(&format!("{decl};"));
            self.declared.insert(list);
        }
        match mode {
            RepeatMode::Count(n) => {
                let n = n.clone();
                let nv = self.paren_child(&n, BinOp::Lt, true);
                self.src.open(&format!("if ({nv} < 0)"));
                let nn = n.clone();
                let msg = format!("negative length \" + {}", self.stringify_operand(&nn));
                if self.is_encode {
                    pack_call(
                        &mut self.src,
                        "throw EncodeError",
                        &[format!("\"{}\"", self.snake), format!("\"{msg}")],
                        ");",
                        false,
                    );
                } else {
                    pack_call(
                        &mut self.src,
                        "throw DecodeError",
                        &[
                            format!("\"{}\"", self.snake),
                            format!("{}.pos()", self.rname),
                            format!("\"{msg}"),
                        ],
                        ");",
                        false,
                    );
                }
                self.src.close("}");
                let d = self.loop_depth;
                let init = format!("std::int64_t _i{d} = 0");
                let cond = format!("_i{d} < {nv}");
                let incr = format!("++_i{d}");
                let header = format!("for ({init}; {cond}; {incr})");
                if self.fits(&header) {
                    self.src.open(&header);
                } else if self.fits(&format!("for ({init}; {cond};")) {
                    // Break after the condition.
                    self.src.line(&format!("for ({init}; {cond};"));
                    self.src.line(&format!("     {incr}) {{"));
                    self.src.ind += 1;
                } else {
                    // Break after the initializer, packing the rest aligned.
                    self.src.line(&format!("for ({init};"));
                    let rest = format!("{cond}; {incr}) {{");
                    if self.src.ind * 2 + 5 + rest.len() <= MAX_COL {
                        self.src.line(&format!("     {rest}"));
                    } else {
                        self.src.line(&format!("     {cond};"));
                        self.src.line(&format!("     {incr}) {{"));
                    }
                    self.src.ind += 1;
                }
            }
            RepeatMode::Until(t) => {
                let r = self.rname.clone();
                let q = format!("\"{}\"", self.snake);
                self.src.open(&format!("while ({r}.remaining() > 0)"));
                let t = t.clone();
                let peek = match crate::ir::typed::byte_literal(&t, true) {
                    Some(byte) => format!("{r}.peek_u8({q}) == {byte}"),
                    None => {
                        let tv = self.paren_child(&t, BinOp::Eq, false);
                        format!("static_cast<std::int64_t>({r}.peek_u8({q})) == {tv}")
                    }
                };
                self.src.open(&format!("if ({peek})"));
                self.src.line(&format!("{r}.u8({q});"));
                self.src.line("break;");
                self.src.close("}");
            }
            RepeatMode::UntilEnd => {
                let r = self.rname.clone();
                self.src.open(&format!("while ({r}.remaining() > 0)"));
            }
        }
        self.loop_depth += 1;
        let body = body.to_vec();
        for s in &body {
            self.stmt(s);
        }
        self.loop_depth -= 1;
        let iname = self.local_name(item);
        self.src
            .line(&format!("{lname}.push_back(std::move({iname}));"));
        self.src.close("}");
    }

    fn stmt_expect(
        &mut self,
        dst: &Option<crate::ir::typed::Bind>,
        sum: &Expr,
        ty: TypeIdx,
        variant: usize,
    ) {
        let altbase = {
            let w = self.layout.wrapper(ty, variant);
            w.existing
                .map(|t| pascal(&self.program.types[t].name))
                .unwrap_or_else(|| w.name.clone())
        };
        let boxed = self.layout.boxed_alt.contains(&(ty, variant));
        let alt = if boxed {
            format!("Box<{altbase}>")
        } else {
            altbase
        };
        let vname = match &self.program.types[ty].kind {
            crate::ir::typed::TypeKind::Sum { variants } => snake(&variants[variant].name),
            _ => format!("tag{variant}"),
        };
        let s = sum.clone();
        let r = self.render(&s);
        let sumptr = if r.boxed {
            format!("&(*{})", r.s)
        } else {
            format!("&{}", r.s)
        };
        let arm = format!("_arm{}", self.armc);
        self.armc += 1;
        self.decl_init(
            &format!("const {alt} *{arm}"),
            &format!("std::get_if<{alt}>({sumptr})"),
        );
        self.src.open(&format!("if ({arm} == nullptr)"));
        self.throw_msg(&format!("wrong variant: expected {vname}"));
        self.src.close("}");
        let Some(d) = dst else { return };
        let payload = match &self.program.types[ty].kind {
            crate::ir::typed::TypeKind::Sum { variants } => variants[variant].payload.clone(),
            _ => None,
        };
        let Some(p) = payload else { return };
        let (pty, pname, puses, is_whole) = {
            let w = self.layout.wrapper(ty, variant);
            let l = &self.body.locals[d.local];
            let whole = matches!(&p, Ty::Named(t) if Some(*t) == w.existing);
            (cpp_ty(self.program, &p), snake(&l.name), l.uses, whole)
        };
        let init = if is_whole {
            if boxed {
                format!("**{arm}")
            } else {
                format!("*{arm}")
            }
        } else if boxed {
            format!("(*{arm}).value")
        } else {
            format!("{arm}->value")
        };
        if !d.declare {
            self.src.line(&format!("{pname} = {init};"));
            return;
        }
        let decl = if puses == 0 {
            format!("[[maybe_unused]] const {pty} &{pname}")
        } else {
            format!("const {pty} &{pname}")
        };
        self.decl_init(&decl, &init);
        self.declared.insert(d.local);
    }

    fn stmt_return(&mut self, e: &Expr) {
        if let Expr::Construct { ty, fields } = e {
            let v = e.clone();
            let one = format!("return {};", self.val(&v));
            if self.fits(&one) {
                self.src.line(&one);
                return;
            }
            // Overlong: one designated initializer per line at a `+4`
            // continuation, trailing comma, `};` on its own line — the
            // `clang-format` (LLVM) layout, byte-stable without guards.
            let tname = pascal(&self.program.types[*ty].name);
            self.src.line(&format!("return {tname}{{"));
            self.src.ind += 2;
            let decls = self.program.fields(*ty).to_vec();
            for (i, init) in fields.iter().enumerate() {
                let r = self.render(init);
                let boxed = self.layout.boxed_field.contains(&(*ty, i));
                let s = if r.boxed && !boxed {
                    format!("(*{})", r.s)
                } else {
                    r.s
                };
                self.src.line(&format!(".{} = {s},", snake(&decls[i].name)));
            }
            self.src.ind -= 2;
            self.src.line("};");
            return;
        }
        let s = self.val(e);
        self.src.line(&format!("return {s};"));
    }

    /// The value of an expression, dereferencing `Box` when a plain value
    /// is needed.
    fn val(&mut self, e: &Expr) -> String {
        let r = self.render(e);
        if r.boxed { format!("(*{})", r.s) } else { r.s }
    }

    fn render(&mut self, e: &Expr) -> EVal {
        match e {
            Expr::Int { value, ty } => EVal {
                s: int_lit(*value, *ty),
                boxed: false,
            },
            Expr::Bool(b) => EVal {
                s: b.to_string(),
                boxed: false,
            },
            Expr::Str(s) => EVal {
                s: quoted(s),
                boxed: false,
            },
            Expr::Local(id) => EVal {
                s: self.local_name(*id),
                boxed: false,
            },
            Expr::Field { base, ty, field } => {
                let b = base.clone();
                let r = self.render(&b);
                let join = if r.boxed { "->" } else { "." };
                let boxed = self.layout.boxed_field.contains(&(*ty, *field));
                let fname = snake(&self.program.fields(*ty)[*field].name);
                EVal {
                    s: format!("{}{}{fname}", r.s, join),
                    boxed,
                }
            }
            Expr::Present(inner) => {
                let ic = inner.clone();
                let r = self.render(&ic);
                let base = postfix_str(inner, &r.s);
                let dot = if r.boxed { "->" } else { "." };
                EVal {
                    s: format!("{base}{dot}has_value()"),
                    boxed: false,
                }
            }
            Expr::Get(inner) => {
                let ic = inner.clone();
                let r = self.render(&ic);
                let base = postfix_str(inner, &r.s);
                let dot = if r.boxed { "->" } else { "." };
                EVal {
                    s: format!("{base}{dot}value()"),
                    boxed: false,
                }
            }
            Expr::Not(inner) => {
                let ic = inner.clone();
                let s = self.val(&ic);
                let out = match **inner {
                    Expr::Local(_)
                    | Expr::Field { .. }
                    | Expr::Get(_)
                    | Expr::Present(_)
                    | Expr::Call { .. }
                    | Expr::Cast { .. } => format!("!{s}"),
                    _ => format!("!({s})"),
                };
                EVal {
                    s: out,
                    boxed: false,
                }
            }
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } => {
                let (o, l, r, t, w) = (*op, lhs.clone(), rhs.clone(), ty.clone(), *wraps);
                self.binary(o, &l, &r, &t, w)
            }
            Expr::Cond {
                cond, then, els, ..
            } => {
                let (c, t, n) = (cond.clone(), then.clone(), els.clone());
                EVal {
                    s: format!("({} ? {} : {})", self.val(&c), self.val(&t), self.val(&n)),
                    boxed: false,
                }
            }
            Expr::Cast { expr, to } => {
                let ic = expr.clone();
                let tc = to.clone();
                EVal {
                    s: format!(
                        "static_cast<{}>({})",
                        cpp_ty(self.program, &tc),
                        self.val(&ic)
                    ),
                    boxed: false,
                }
            }
            Expr::Call { func, args } => {
                let ac = args.clone();
                EVal {
                    s: self.call(*func, &ac),
                    boxed: false,
                }
            }
            Expr::StrBytes(inner) => {
                let ic = inner.clone();
                EVal {
                    s: format!("str_bytes({})", self.val(&ic)),
                    boxed: false,
                }
            }
            Expr::Remaining => EVal {
                s: format!("static_cast<std::int64_t>({}.remaining())", self.rname),
                boxed: false,
            },
            Expr::Construct { ty, fields } => {
                let tname = pascal(&self.program.types[*ty].name);
                let mut parts = Vec::new();
                for (i, init) in fields.iter().enumerate() {
                    let r = self.render(init);
                    let boxed = self.layout.boxed_field.contains(&(*ty, i));
                    parts.push(if r.boxed && !boxed {
                        format!("(*{})", r.s)
                    } else {
                        r.s
                    });
                }
                EVal {
                    s: format!("{tname}{{{}}}", parts.join(", ")),
                    boxed: false,
                }
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                let (altbase, existing) = {
                    let w = self.layout.wrapper(*ty, *variant);
                    (
                        w.existing
                            .map(|t| pascal(&self.program.types[t].name))
                            .unwrap_or_else(|| w.name.clone()),
                        w.existing.is_some(),
                    )
                };
                let sname = pascal(&self.program.types[*ty].name);
                match payload {
                    None => EVal {
                        s: format!("{sname}{{{altbase}{{}}}}"),
                        boxed: false,
                    },
                    Some(p) => {
                        let pc = p.clone();
                        let ps = self.val(&pc);
                        if existing {
                            EVal {
                                s: format!("{sname}{{{ps}}}"),
                                boxed: false,
                            }
                        } else {
                            EVal {
                                s: format!("{sname}{{{altbase}{{{ps}}}}}",),
                                boxed: false,
                            }
                        }
                    }
                }
            }
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, ty: &Ty, wraps: bool) -> EVal {
        if op.is_logical() {
            let l = self.paren_child(lhs, op, true);
            let r = self.paren_child(rhs, op, false);
            return EVal {
                s: format!("{l} {} {r}", op.symbol()),
                boxed: false,
            };
        }
        if op.is_comparison() {
            let l = self.cmp_operand(lhs, rhs, op, true);
            let r = self.cmp_operand(rhs, lhs, op, false);
            return EVal {
                s: format!("{l} {} {r}", op.symbol()),
                boxed: false,
            };
        }
        if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
            let l = self.mask_operand(lhs, op, true);
            let r = self.mask_operand(rhs, op, false);
            return EVal {
                s: self.binop_compose(op, &l, &r, ty, wraps),
                boxed: false,
            };
        }
        if helper_printed(op, ty, wraps) {
            let l = lhs.clone();
            let r = rhs.clone();
            let ls = self.val(&l);
            let rs = self.val(&r);
            return EVal {
                s: self.binop_compose(op, &ls, &rs, ty, wraps),
                boxed: false,
            };
        }
        let l = self.paren_child(lhs, op, true);
        let r = self.paren_child(rhs, op, false);
        EVal {
            s: self.binop_compose(op, &l, &r, ty, wraps),
            boxed: false,
        }
    }

    /// A bitwise operand: an integer literal that is a mask prints in hex
    /// (`typed::hex_literal`); anything else parenthesizes as usual.
    fn mask_operand(&mut self, e: &Expr, parent: BinOp, is_left: bool) -> String {
        if let Expr::Int { value, ty } = e
            && hex_literal(parent, *value)
        {
            return int_lit_hex(*value, *ty);
        }
        self.paren_child(e, parent, is_left)
    }

    fn paren_child(&mut self, e: &Expr, parent: BinOp, is_left: bool) -> String {
        let cp = child_prec(e);
        let pp = bin_prec(parent);
        let s = self.val(e);
        // g++ -Wparentheses wants explicit parens when `&`, `^`, and `|`
        // mix, even though precedence already decides it.
        let mixed_bits = matches!(parent, BinOp::BitOr | BinOp::BitXor | BinOp::BitAnd)
            && matches!(
                e,
                Expr::Binary { op, .. }
                    if matches!(op, BinOp::BitOr | BinOp::BitXor | BinOp::BitAnd) && *op != parent
            );
        if cp < pp
            || mixed_bits
            || (cp == pp
                && !is_left
                && matches!(
                    parent,
                    BinOp::Sub
                        | BinOp::Div
                        | BinOp::Mod
                        | BinOp::Shl
                        | BinOp::Shr
                        | BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                ))
        {
            format!("({s})")
        } else {
            s
        }
    }

    /// A comparison operand, casting an enum side to its base type and
    /// parenthesizing as needed.
    fn cmp_operand(&mut self, e: &Expr, other: &Expr, parent: BinOp, is_left: bool) -> String {
        let (lt, _) = self.program.expr_ty(self.body, e);
        let (rt, _) = self.program.expr_ty(self.body, other);
        if let Ty::Named(t) = &lt
            && matches!(
                self.program.types[*t].kind,
                crate::ir::typed::TypeKind::Enum { .. }
            )
            && !matches!(rt, Ty::Named(u) if u == *t)
        {
            let crate::ir::typed::TypeKind::Enum { base, .. } = &self.program.types[*t].kind else {
                return self.paren_child(e, parent, is_left);
            };
            let s = self.val(e);
            return format!("static_cast<{}>({s})", int_cpp(*base));
        }
        self.paren_child(e, parent, is_left)
    }

    fn binop_compose(&self, op: BinOp, l: &str, r: &str, ty: &Ty, wraps: bool) -> String {
        let wide128 = matches!(ty, Ty::Int(IntTy::I128));
        let helper = |name: &str| {
            if wide128 {
                format!("{name}128({l}, {r})")
            } else {
                format!("{name}({l}, {r})")
            }
        };
        match op {
            BinOp::Add if wraps => helper("add_wrap"),
            BinOp::Sub if wraps => helper("sub_wrap"),
            BinOp::Mul if wraps => helper("mul_wrap"),
            BinOp::Shl if wraps => helper("shl_wrap"),
            BinOp::Div if matches!(ty, Ty::Int(t) if t.signed()) => {
                if wide128 {
                    format!("div_floor128({l}, {r})")
                } else {
                    format!("div_floor({l}, {r})")
                }
            }
            BinOp::Mod if matches!(ty, Ty::Int(t) if t.signed()) => {
                if wide128 {
                    format!("mod_sign128({l}, {r})")
                } else {
                    format!("mod_sign({l}, {r})")
                }
            }
            _ => format!("{l} {} {r}", op.symbol()),
        }
    }

    fn call(&mut self, func: Func, args: &[Expr]) -> String {
        let vals: Vec<String> = args.iter().map(|a| self.val(a)).collect();
        self.call_compose(func, &vals)
    }

    fn call_compose(&self, func: Func, vals: &[String]) -> String {
        format!("{}({})", call_head(func), vals.join(", "))
    }
}

/// The head name of a builtin call, without its arguments.
fn call_head(func: Func) -> &'static str {
    match func {
        Func::SignExtend => "sign_extend",
        Func::Utf16Units => "utf16_units",
        Func::Utf8Len => "utf8_len",
        Func::StrLen => "str_len",
        Func::BytesLen => "bytes_len",
        Func::ListLen => "list_len",
        Func::Min => "std::min",
        Func::Max => "std::max",
    }
}

/// One `if`/`else if` condition line set: flat `||` units packed with
/// trailing-`||` continuations, exactly as `clang-format` packs them.
fn cond_lines(first: &str, units: &[&str], end: &str, budget: usize) -> Vec<String> {
    let one = format!("{first}{}{end}", units.join(" || "));
    if one.len() <= budget {
        return vec![one];
    }
    let mut lines = Vec::new();
    let mut cur = first.to_string();
    for (i, u) in units.iter().enumerate() {
        let last = i + 1 == units.len();
        let piece = if cur.ends_with('(') {
            (*u).to_string()
        } else {
            format!(" || {u}")
        };
        let tail = if last {
            end.to_string()
        } else {
            " ||".to_string()
        };
        if cur.len() + piece.len() + tail.len() <= budget {
            cur.push_str(&piece);
            if last {
                cur.push_str(end);
            }
        } else {
            cur.push_str(" ||");
            lines.push(cur);
            cur = format!("    {u}");
            if last {
                cur.push_str(end);
            }
        }
    }
    lines.push(cur);
    lines
}

fn is_postfix(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Local(_)
            | Expr::Field { .. }
            | Expr::Get(_)
            | Expr::Present(_)
            | Expr::Call { .. }
            | Expr::Cast { .. }
    )
}

fn postfix_str(e: &Expr, s: &str) -> String {
    if is_postfix(e) {
        s.to_string()
    } else {
        format!("({s})")
    }
}

fn bin_prec(op: BinOp) -> u8 {
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

fn child_prec(e: &Expr) -> u8 {
    match e {
        Expr::Binary { op, ty, wraps, .. } => {
            if helper_printed(*op, ty, *wraps) {
                99
            } else {
                bin_prec(*op)
            }
        }
        Expr::Cond { .. } => 0,
        Expr::Not(_) => 12,
        _ => 99,
    }
}

/// Whether a binary operation prints as a helper call (atomic) rather than
/// a plain operator.
fn helper_printed(op: BinOp, ty: &Ty, wraps: bool) -> bool {
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Shl => {
            wraps && matches!(ty, Ty::Int(IntTy::I64 | IntTy::I128))
        }
        BinOp::Div | BinOp::Mod => matches!(ty, Ty::Int(t) if t.signed()),
        _ => false,
    }
}

fn int_lit(value: i64, ty: IntTy) -> String {
    int_spelling(value, ty, false)
}

/// A mask operand of `&`, `|`, `^` in hex (`0x{:X}`, as in the runtime).
fn int_lit_hex(value: i64, ty: IntTy) -> String {
    int_spelling(value, ty, true)
}

/// An integer literal with a suffix only where the value needs it: a value
/// that fits in `int` converts value-preserving into every 64-bit operand
/// and parameter in the emitted code, so `<< 7LL`, `32LL`, `255ULL`, and
/// `? 128LL : 0LL` print plain. Nothing in any shipped schema passes a
/// literal to a deduced template argument (`std::min`/`std::max` never take
/// one), so the narrower type changes no overload.
fn int_spelling(value: i64, ty: IntTy, hex: bool) -> String {
    let digits = if hex {
        format!("0x{value:X}")
    } else {
        format!("{value}")
    };
    let fits_int = (i32::MIN as i64..=i32::MAX as i64).contains(&value);
    match ty {
        IntTy::I8 | IntTy::I16 | IntTy::I32 => digits,
        IntTy::I64 => {
            if fits_int {
                digits
            } else {
                format!("{digits}LL")
            }
        }
        IntTy::U8 | IntTy::U16 => digits,
        IntTy::U32 => {
            if fits_int {
                digits
            } else {
                format!("{digits}U")
            }
        }
        IntTy::U64 => {
            if fits_int {
                digits
            } else {
                format!("{digits}ULL")
            }
        }
        IntTy::I128 => format!("static_cast<__int128>({digits}LL)"),
    }
}

fn quoted(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7F => {
                o.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
