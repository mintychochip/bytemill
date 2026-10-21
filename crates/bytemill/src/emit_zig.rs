//! `emit zig`: a `bmlgen.zig` index over one module per route category
//! (`runtime.zig`, `encoding.zig`, `play_clientbound.zig`, `shared.zig`, ...),
//! each of typed structs, unions, enums, and codecs. `bmlgen.zig` re-exports
//! every public declaration, so the public surface is unchanged. A module that
//! reaches into another imports it once at file scope (`const encoding =
//! @import("encoding.zig");`) and calls `encoding.decodeString(...)`, never an
//! inline `@import("encoding.zig").decodeString(...)`; every module stem of the
//! program is therefore reserved for locals, so a local can never shadow an
//! alias.
//!
//! Names follow Zig's own conventions: types `PascalCase`, functions
//! `camelCase`, fields, locals, and enum members `snake_case`. A reserved word
//! escapes the way the language does (`@"test"`); a primitive type or value
//! name (`type`, `u8`) takes a trailing `_` instead, because `zig fmt` strips
//! the quotes of a quoted primitive in field position.
//!
//! The printer reads the typed program (`bytemill::ir::typed`), never the IR or
//! the AST. Every encoding, packet, frame, and enum becomes either free
//! `decodeX`/`encodeX` functions (scalars and strings) or `decode`/`encode`
//!/`deinit` methods on a `struct`, `union(enum)`, or `enum`. Statements print
//! in the typed program's order: a sum encode matches on its tag parameter
//! and checks the active variant with `ExpectVariant`; `require`s stay where
//! the program puts them. The runtime is only `Reader`/`Writer` over the
//! typed program's wire ops (`u8`, `f32`, `f64`, `bytes`, `rest`); multibyte
//! integers are schema codecs like `u16be`.
//!
//! Memory: decode takes an allocator exactly when the decoded value owns
//! memory (strings, bytes, lists, or records/sums holding those); every owner
//! has `deinit`, decode unwinds partial values with `errdefer`, and encode
//! only borrows. Recursive types box the back edge (`*T` via
//! `allocator.create`), chosen greedily so one box breaks each by-value
//! cycle. Integers use plain operators, `@divFloor`/`@mod` for `/`/`%`, and
//! `@intCast`/`@truncate`/`@bitCast` for the program's explicit casts, so
//! nothing traps in Debug on a value the interpreter can produce.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::ResolvedSchema;
use crate::errors::{BpsResult, emit};
use crate::ir::typed::{
    BinOp, Bind, Body, CodecIdx, Expr, Func, IntTy, LocalId, Origin, Program, ReadOp, RepeatMode,
    Stmt, Ty, TypeIdx, WriteOp, byte_literal, hex_literal,
};

/// Emit `bmlgen.zig` and its modules for `resolved` into `out_dir`.
pub fn emit_zig(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    emit_zig_with_version(resolved, out_dir, crate::ZigVersion::default())
}

pub fn emit_zig_with_version(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    ver: crate::ZigVersion,
) -> BpsResult<()> {
    let program = crate::ir::typed(resolved)?;
    let mut printer = Printer::build(&program, ver)?;
    let files = printer.files()?;
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    for (stem, src) in &files {
        fs::write(out_dir.join(format!("{stem}.zig")), src).map_err(|e| emit(e.to_string()))?;
    }
    Ok(())
}

/// The emitted identifier of a codec's decode function: `decodeVarint` for a
/// scalar or string codec, `Handshake.decode` for a type codec (an alias maps
/// to its target's method). Tests call through this instead of grepping.
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    let target = decode_target(program, codec);
    match program.codecs[codec].ty {
        None => format!("decode{}", program.codecs[codec].name.pascal()),
        Some(_) => format!("{}.decode", program.types[target].name.pascal()),
    }
}

/// The emitted identifier of a codec's encode function; see [`decode_fn`].
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    let target = decode_target(program, codec);
    match program.codecs[codec].ty {
        None => format!("encode{}", program.codecs[codec].name.pascal()),
        Some(_) => format!("{}.encode", program.types[target].name.pascal()),
    }
}

/// Whether a codec's decode takes an allocator and its type has `deinit`.
pub fn has_deinit(program: &Program, codec: CodecIdx) -> bool {
    let (boxed_field, boxed_variant) = boxed_edges(program);
    let owns = owns_fixpoint(program, &boxed_field, &boxed_variant);
    owns_ty(&owns, &program.codecs[codec].value)
}

/// The type whose methods implement `codec`: aliases delegate to their target.
fn decode_target(program: &Program, codec: CodecIdx) -> TypeIdx {
    match program.codecs[codec].ty {
        Some(t) => match program.types[t].kind {
            crate::ir::typed::TypeKind::Alias { target } => target,
            _ => t,
        },
        None => 0,
    }
}

/// Whether decoded values of `ty` own memory (need an allocator and `deinit`).
/// Slices always own (reads dupe, lists allocate); boxed pointers always own
/// (the allocation itself).
fn owns_value(printer: &Printer<'_>, ty: &Ty) -> bool {
    match ty {
        Ty::Named(t) => printer.owns[*t],
        _ => owns_ty(&printer.owns, ty),
    }
}

/// Ownership per type, least fixpoint: a record owns when a field owns, a sum
/// when a payload owns. Boxed cycle edges own through the allocation itself.
fn owns_fixpoint(
    prog: &Program,
    boxed_field: &HashSet<(TypeIdx, usize)>,
    boxed_variant: &HashSet<(TypeIdx, usize)>,
) -> Vec<bool> {
    let mut owns = vec![false; prog.types.len()];
    loop {
        let mut changed = false;
        for t in 0..prog.types.len() {
            let v = match &prog.types[t].kind {
                crate::ir::typed::TypeKind::Record { fields } => fields
                    .iter()
                    .enumerate()
                    .any(|(f, d)| boxed_field.contains(&(t, f)) || owns_ty(&owns, &d.ty)),
                crate::ir::typed::TypeKind::Sum { variants } => {
                    variants.iter().enumerate().any(|(v, d)| {
                        boxed_variant.contains(&(t, v))
                            || d.payload.as_ref().is_some_and(|p| owns_ty(&owns, p))
                    })
                }
                crate::ir::typed::TypeKind::Enum { .. } => false,
                crate::ir::typed::TypeKind::Alias { target } => owns[*target],
            };
            if v && !owns[t] {
                owns[t] = true;
                changed = true;
            }
        }
        if !changed {
            return owns;
        }
    }
}

fn owns_ty(owns: &[bool], ty: &Ty) -> bool {
    match ty {
        Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => false,
        Ty::Str | Ty::Bytes => true,
        Ty::List(_) => true,
        Ty::Named(t) => owns[*t],
    }
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

/// Zig keywords plus the primitive type names. Reserved words escape as
/// `@"<word>"`, the language's own convention; primitive type names cannot be
/// escaped that way because `zig fmt` strips the quotes in field position, so
/// they take a trailing `_`. Real 26.3 hits are fields `type` and `test`.
const KEYWORDS: &[&str] = &[
    "addrspace",
    "align",
    "allowzero",
    "and",
    "anyframe",
    "anytype",
    "asm",
    "async",
    "await",
    "break",
    "catch",
    "comptime",
    "const",
    "continue",
    "defer",
    "else",
    "enum",
    "errdefer",
    "error",
    "export",
    "extern",
    "fn",
    "for",
    "if",
    "inline",
    "linksection",
    "noalias",
    "noinline",
    "nosuspend",
    "opaque",
    "or",
    "orelse",
    "packed",
    "pub",
    "resume",
    "return",
    "struct",
    "suspend",
    "switch",
    "test",
    "threadlocal",
    "try",
    "union",
    "unreachable",
    "usingnamespace",
    "var",
    "volatile",
    "while",
    "bool",
    "c_char",
    "c_int",
    "c_long",
    "c_longdouble",
    "c_longlong",
    "c_short",
    "c_uint",
    "c_ulong",
    "c_ulonglong",
    "c_ushort",
    "comptime_float",
    "comptime_int",
    "f16",
    "f32",
    "f64",
    "f128",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "void",
    "type",
    "anyerror",
    "noreturn",
    "null",
    "undefined",
    "true",
    "false",
    "self",
];

/// Primitive type and value names. These are the only entries of [`KEYWORDS`]
/// that cannot be quoted: `zig fmt` rewrites `@"u8"` to `u8` in field
/// position, which then shadows the primitive and fails to compile, so they
/// escape with a trailing `_`.
const PRIMITIVES: &[&str] = &[
    "bool",
    "c_char",
    "c_int",
    "c_long",
    "c_longdouble",
    "c_longlong",
    "c_short",
    "c_uint",
    "c_ulong",
    "c_ulonglong",
    "c_ushort",
    "comptime_float",
    "comptime_int",
    "f16",
    "f32",
    "f64",
    "f128",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "void",
    "type",
    "anyerror",
    "noreturn",
    "null",
    "undefined",
    "true",
    "false",
    "self",
];

/// Extra names reserved for locals and parameters: the function signatures
/// already bind `reader`, `writer`, and `allocator`.
const LOCAL_RESERVED: &[&str] = &["reader", "writer", "allocator", "bml_rt"];

/// Escape one identifier. `reserved` marks names bound elsewhere in the
/// emitted function (signature params, module imports), which cannot be
/// quoted and so take the trailing `_`.
fn escaped(snake: &str, reserved: bool) -> String {
    if reserved || PRIMITIVES.contains(&snake) {
        format!("{snake}_")
    } else if KEYWORDS.contains(&snake) {
        format!("@\"{snake}\"")
    } else {
        snake.to_string()
    }
}

/// A field, variant, or enum-member name: usable bare or quoted, never
/// shadowing, since a container member may share a name with a module import.
fn field_name(name: &crate::ir::typed::Name) -> String {
    let snake = name.snake();
    if PRIMITIVES.contains(&snake.as_str()) {
        format!("{snake}_")
    } else if KEYWORDS.contains(&snake.as_str()) {
        format!("@\"{snake}\"")
    } else {
        snake
    }
}

/// A local or parameter name: [`escaped`] with every module stem of the
/// program reserved, so a local can never shadow a file-scope
/// `const <stem> = @import("<stem>.zig");` alias (see [`hoist_imports`]).
/// Reserving all stems is what makes that rewrite safe without a second
/// generation pass.
fn local_name(name: &crate::ir::typed::Name, stems: &BTreeSet<String>) -> String {
    let snake = name.snake();
    escaped(
        &snake,
        LOCAL_RESERVED.contains(&snake.as_str()) || stems.contains(&snake),
    )
}

/// Replace `@import("x.zig").Name` with `x.Name` throughout a module body and
/// collect the imported stems, so the caller can emit one file-scope alias
/// each. `self_stem` is never imported.
fn hoist_imports(body: &str, self_stem: &str, aliases: &mut BTreeSet<String>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find("@import(\"") {
        out.push_str(&rest[..at]);
        let after = &rest[at + "@import(\"".len()..];
        let Some(close) = after.find("\")") else {
            out.push_str(&rest[at..]);
            return out;
        };
        let stem = &after[..close];
        let tail = &after[close + 2..];
        if let Some(stem) = stem.strip_suffix(".zig")
            && stem != self_stem
            && !stem.is_empty()
            && stem
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            aliases.insert(stem.to_string());
            out.push_str(stem);
            out.push('.');
            rest = tail.strip_prefix('.').unwrap_or(tail);
        } else {
            out.push_str(&rest[at..after.len() + "@import(\"".len() + close + 2 - after.len()]);
            rest = tail;
            // Keep the original text when it is not a hoistable import.
            out.truncate(
                out.len() - (close + 2) - "@import(\"".len() - (after.len() - after.len()),
            );
            out.push_str(&format!("@import(\"{stem}.zig\")"));
            out.push('.');
        }
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

const RUNTIME: &str = r#"const std = @import("std");
/// Decode/encode failures. Offset of a decode failure is left in `reader.pos`.
pub const Error = error{ Short, RequireFailed, InvalidUtf8, NoMatchArm, MissingField, WrongVariant, NegativeLength, BytesTooLong, UntilEnd, OutOfMemory };
/// Big-endian cursor over a borrowed buffer.
pub const Reader = struct {
    data: []const u8,
    pos: usize = 0,
    /// The codec that detected the failure and its detail; `pos` is the
    /// offset. Set by the innermost failing codec's `errdefer`, so the first
    /// (deepest) failure wins and nested failures propagate untouched.
    err_codec: []const u8 = "",
    err_detail: []const u8 = "",
    pub fn fail(self: *Reader, codec: []const u8, detail: []const u8) void {
        if (self.err_codec.len == 0) {
            self.err_codec = codec;
            self.err_detail = detail;
        }
    }
    pub fn remaining(self: *const Reader) usize {
        return self.data.len - self.pos;
    }
    pub fn readU8(self: *Reader) Error!u8 {
        if (self.pos >= self.data.len) return error.Short;
        const b = self.data[self.pos];
        self.pos += 1;
        return b;
    }
    pub fn readBytes(self: *Reader, allocator: std.mem.Allocator, n: usize) Error![]u8 {
        if (self.pos + n > self.data.len) return error.Short;
        const out = allocator.dupe(u8, self.data[self.pos..][0..n]) catch return error.OutOfMemory;
        self.pos += n;
        return out;
    }
    /// Lengths arrive signed; negative is its own error.
    pub fn readSized(self: *Reader, allocator: std.mem.Allocator, n: i64) Error![]u8 {
        if (n < 0) return error.NegativeLength;
        return self.readBytes(allocator, @intCast(n));
    }
    pub fn readF32Be(self: *Reader) Error!f32 {
        if (self.remaining() < 4) return error.Short;
        const u = std.mem.readInt(u32, self.data[self.pos..][0..4], .big);
        self.pos += 4;
        return @bitCast(u);
    }
    pub fn readF64Be(self: *Reader) Error!f64 {
        if (self.remaining() < 8) return error.Short;
        const u = std.mem.readInt(u64, self.data[self.pos..][0..8], .big);
        self.pos += 8;
        return @bitCast(u);
    }
};
pub const Writer = struct {
    buf: std.ArrayList(u8),
    /// Set by the innermost failing codec's `errdefer`; see `Reader`.
    err_codec: []const u8 = "",
    err_detail: []const u8 = "",
    pub fn init(allocator: std.mem.Allocator) Writer {
        return .{ .buf = std.ArrayList(u8).init(allocator) };
    }
    pub fn fail(self: *Writer, codec: []const u8, detail: []const u8) void {
        if (self.err_codec.len == 0) {
            self.err_codec = codec;
            self.err_detail = detail;
        }
    }
    /// Release the buffer without taking ownership; `toOwnedSlice` does this
    /// implicitly, and errors need it.
    pub fn deinit(self: *Writer) void {
        self.buf.deinit();
    }
    pub fn toOwnedSlice(self: *Writer) Error![]u8 {
        return self.buf.toOwnedSlice() catch return error.OutOfMemory;
    }
    pub fn writeU8(self: *Writer, v: u8) Error!void {
        self.buf.append(v) catch return error.OutOfMemory;
    }
    pub fn writeBytes(self: *Writer, b: []const u8) Error!void {
        self.buf.appendSlice(b) catch return error.OutOfMemory;
    }
    /// `bytes(n)`: overlong errors, short zero-pads.
    pub fn writeSized(self: *Writer, b: []const u8, n: usize) Error!void {
        if (b.len > n) return error.BytesTooLong;
        try self.writeBytes(b);
        var i = b.len;
        while (i < n) : (i += 1) try self.writeU8(0);
    }
    pub fn writeF32Be(self: *Writer, v: f32) Error!void {
        var tmp: [4]u8 = undefined;
        std.mem.writeInt(u32, &tmp, @bitCast(v), .big);
        try self.writeBytes(&tmp);
    }
    pub fn writeF64Be(self: *Writer, v: f64) Error!void {
        var tmp: [8]u8 = undefined;
        std.mem.writeInt(u64, &tmp, @bitCast(v), .big);
        try self.writeBytes(&tmp);
    }
};
/// Two's-complement sign extension of a `bits`-wide field (`varlong` uses the
/// full 64-bit width, which is the identity).
pub fn signExtend(raw: u64, bits: u7) i64 {
    if (bits == 0 or bits >= 64) return @bitCast(raw);
    const b: u6 = @truncate(bits);
    const sign: u64 = @as(u64, 1) << (b - 1);
    const mask: u64 = (@as(u64, 1) << b) - 1;
    var out = raw & mask;
    if (out & sign != 0) out |= ~mask;
    return @bitCast(out);
}
/// UTF-16 units of validated UTF-8 (0.14.1 std has no counter; validated
/// first, so every lead byte starts a well-formed sequence here).
pub fn utf16Units(s: []const u8) usize {
    var n: usize = 0;
    var i: usize = 0;
    while (i < s.len) {
        const c = s[i];
        var cp: u32 = 0;
        var len: usize = 1;
        if (c < 0x80) {
            cp = c;
        } else if (c < 0xe0) {
            cp = c & 0x1f;
            len = 2;
        } else if (c < 0xf0) {
            cp = c & 0x0f;
            len = 3;
        } else {
            cp = c & 0x07;
            len = 4;
        }
        var j: usize = 1;
        while (j < len) : (j += 1) cp = (cp << 6) | (s[i + j] & 0x3f);
        i += len;
        n += if (cp > 0xffff) 2 else 1;
    }
    return n;
}
"#;

/// `str_len` helper, emitted only when a schema uses it.
const STRLEN_HELPER: &str = r#"/// Unicode scalars of a validated UTF-8 string.
pub fn strLen(s: []const u8) i64 {
    var n: i64 = 0;
    for (s) |c| {
        if ((c & 0xc0) != 0x80) n += 1;
    }
    return n;
}
"#;

// ---------------------------------------------------------------------------
// Printer
// ---------------------------------------------------------------------------

struct Printer<'a> {
    prog: &'a Program,
    /// Target Zig version: the emitted constructs are stable across every
    /// supported version, so this is stored for future version-conditional
    /// formatting without changing today's output.
    ver: crate::ZigVersion,
    /// Fields stored as pointers to break by-value recursion cycles.
    boxed_field: HashSet<(TypeIdx, usize)>,
    /// Sum variant payloads stored as pointers, same reason.
    boxed_variant: HashSet<(TypeIdx, usize)>,
    owns: Vec<bool>,
    /// Pointer-marked locals per codec body: `(codec, is_decode)`.
    ptr: HashMap<(CodecIdx, bool), HashSet<LocalId>>,
    needs_strlen: bool,
    /// Every file stem the program emits, so no local shadows a module alias.
    stems: BTreeSet<String>,
}

impl<'a> Printer<'a> {
    fn build(prog: &'a Program, ver: crate::ZigVersion) -> BpsResult<Self> {
        let (boxed_field, boxed_variant) = boxed_edges(prog);
        let owns = owns_fixpoint(prog, &boxed_field, &boxed_variant);
        let needs_strlen = prog
            .codecs
            .iter()
            .any(|c| body_uses_strlen(&c.decode.stmts) || body_uses_strlen(&c.encode.stmts));
        let stems: BTreeSet<String> = prog
            .types
            .iter()
            .map(|t| prog.output_stem(&t.origin).to_string())
            .chain(
                prog.codecs
                    .iter()
                    .map(|c| prog.output_stem(&c.origin).to_string()),
            )
            .chain(["runtime".to_string(), "std".to_string()])
            .collect();
        let mut printer = Self {
            prog,
            ver,
            boxed_field,
            boxed_variant,
            owns,
            ptr: HashMap::new(),
            needs_strlen,
            stems,
        };
        for c in 0..prog.codecs.len() {
            printer
                .ptr
                .insert((c, true), ptr_marks(&printer, &prog.codecs[c].decode));
            printer
                .ptr
                .insert((c, false), ptr_marks(&printer, &prog.codecs[c].encode));
        }
        Ok(printer)
    }

    /// `stem` -> module source: `runtime.zig`, one module per route category,
    /// and the `bmlgen.zig` index that re-exports them all.
    fn files(&mut self) -> BpsResult<BTreeMap<String, String>> {
        // The constructs printed here are stable across every supported Zig
        // version, so the target only selects this shared formatting.
        let _ = self.ver;
        let mut runtime = RUNTIME.to_string();
        if self.needs_strlen {
            runtime.push_str(STRLEN_HELPER);
        }
        let mut bodies: BTreeMap<String, String> = BTreeMap::new();
        // Scalar and string codecs are free functions, in codec order.
        for c in 0..self.prog.codecs.len() {
            if self.prog.codecs[c].ty.is_none() {
                let here = self.prog.output_stem(&self.prog.codecs[c].origin);
                let body = self.free_codec(c, &here)?;
                bodies.entry(here).or_default().push_str(&body);
            }
        }
        // Declared types, in type order, with their codec's methods.
        for t in 0..self.prog.types.len() {
            let here = self.prog.output_stem(&self.prog.types[t].origin);
            let body = self.type_decl(t, &here)?;
            bodies.entry(here).or_default().push_str(&body);
        }
        let index = self.index(&runtime);
        let mut files = BTreeMap::new();
        for (stem, body) in bodies {
            // One `const <stem> = @import("<stem>.zig");` per module this
            // module reaches into, so call sites read `encoding.decodeString`
            // instead of repeating the path. Never import self.
            let mut aliases: BTreeSet<String> = BTreeSet::new();
            let body = hoist_imports(&body, &stem, &mut aliases);
            let mut src = String::new();
            if body.contains("std.") {
                src.push_str("const std = @import(\"std\");\n");
            }
            if body.contains("bml_rt.") {
                src.push_str("const bml_rt = @import(\"runtime.zig\");\n");
            }
            for alias in &aliases {
                src.push_str(&format!("const {alias} = @import(\"{alias}.zig\");\n"));
            }
            if !src.is_empty() {
                src.push('\n');
            }
            src.push_str(&body);
            files.insert(stem, src);
        }
        files.insert("bmlgen".to_string(), index);
        files.insert("runtime".to_string(), runtime);
        Ok(files)
    }

    /// `bmlgen.zig`: re-export every public declaration, so the public API is
    /// what the single-module emission was.
    fn index(&self, runtime: &str) -> String {
        let mut entries: BTreeSet<(String, String)> = BTreeSet::new();
        for line in runtime.lines() {
            let rest = line
                .strip_prefix("pub const ")
                .or_else(|| line.strip_prefix("pub fn "));
            let Some(rest) = rest else { continue };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                entries.insert((name.clone(), "runtime".to_string()));
            }
        }
        for c in 0..self.prog.codecs.len() {
            if self.prog.codecs[c].ty.is_some() {
                continue;
            }
            let stem = self.prog.output_stem(&self.prog.codecs[c].origin);
            for name in [decode_fn(self.prog, c), encode_fn(self.prog, c)] {
                entries.insert((name, stem.clone()));
            }
        }
        for t in 0..self.prog.types.len() {
            entries.insert((
                self.prog.types[t].name.pascal(),
                self.prog.output_stem(&self.prog.types[t].origin),
            ));
        }
        let mut out = String::from("//! Generated BPS codecs, one module per route category.\n\n");
        for (name, stem) in entries {
            out.push_str(&format!(
                "pub const {name} = @import(\"{stem}.zig\").{name};\n"
            ));
        }
        out
    }
}

/// By-value `Named` references that must be boxed (`*T`) so the emitted
/// structs and unions have finite size. Lists already break cycles (slices);
/// optional and plain references do not. Edges are boxed greedily in type
/// order: an edge is boxed when its target already reaches its container
/// through kept edges, so one box breaks each cycle.
/// Field/variant positions stored as pointers to break by-value cycles.
type EdgeSet = HashSet<(TypeIdx, usize)>;

fn boxed_edges(prog: &Program) -> (EdgeSet, EdgeSet) {
    /// Direct by-value named references of a type, omitting list elements.
    fn direct(ty: &Ty, out: &mut Vec<TypeIdx>) {
        match ty {
            Ty::Named(t) => out.push(*t),
            Ty::List(inner) => {
                let _ = inner;
            }
            _ => {}
        }
    }
    // Reachability over kept edges.
    fn reaches(kept: &[Vec<TypeIdx>], from: TypeIdx, to: TypeIdx) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if seen.insert(n) {
                stack.extend(kept[n].iter().copied());
            }
        }
        false
    }
    let mut kept: Vec<Vec<TypeIdx>> = vec![Vec::new(); prog.types.len()];
    let mut boxed_field = HashSet::new();
    let mut boxed_variant = HashSet::new();
    for t in 0..prog.types.len() {
        match &prog.types[t].kind {
            crate::ir::typed::TypeKind::Record { fields } => {
                for (f, decl) in fields.iter().enumerate() {
                    let mut refs = Vec::new();
                    direct(&decl.ty, &mut refs);
                    for target in refs {
                        if target == t || reaches(&kept, target, t) {
                            boxed_field.insert((t, f));
                        } else {
                            kept[t].push(target);
                        }
                    }
                }
            }
            crate::ir::typed::TypeKind::Sum { variants } => {
                for (v, decl) in variants.iter().enumerate() {
                    let mut refs = Vec::new();
                    if let Some(p) = &decl.payload {
                        direct(p, &mut refs);
                    }
                    for target in refs {
                        if target == t || reaches(&kept, target, t) {
                            boxed_variant.insert((t, v));
                        } else {
                            kept[t].push(target);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (boxed_field, boxed_variant)
}

impl Printer<'_> {
    // -- types ----------------------------------------------------------

    fn type_decl(&mut self, t: TypeIdx, here: &str) -> BpsResult<String> {
        let mut out = String::new();
        out.push_str(&self.doc(t));
        let name = self.prog.types[t].name.pascal();
        match &self.prog.types[t].kind {
            crate::ir::typed::TypeKind::Record { .. } => {
                out.push_str(&format!("pub const {name} = struct {{\n"));
                let fields = match &self.prog.types[t].kind {
                    crate::ir::typed::TypeKind::Record { fields } => fields.clone(),
                    _ => unreachable!(),
                };
                for (f, decl) in fields.iter().enumerate() {
                    let fname = field_name(&decl.name);
                    out.push_str(&format!(
                        "    {fname}: {},\n",
                        self.field_ty(t, f, decl, here)?
                    ));
                }
                if let Some(codec) = self.prog.types[t].codec {
                    if self.prog.codecs[codec].kind == crate::ir::typed::CodecKind::Alias {
                        // Aliases emit no methods here; the const below aliases.
                    } else {
                        out.push_str(&self.decode_method(codec, &name, here)?);
                        out.push_str(&self.encode_method(codec, &name, here)?);
                        out.push_str(&self.deinit_record(t, &name));
                    }
                }
                out.push_str("};\n");
            }
            crate::ir::typed::TypeKind::Sum { .. } => {
                out.push_str(&format!("pub const {name} = union(enum) {{\n"));
                let variants = match &self.prog.types[t].kind {
                    crate::ir::typed::TypeKind::Sum { variants } => variants.clone(),
                    _ => unreachable!(),
                };
                for (v, decl) in variants.iter().enumerate() {
                    let vname = field_name(&decl.name);
                    match &decl.payload {
                        Some(p) => out.push_str(&format!(
                            "    {vname}: {},\n",
                            self.payload_ty(t, v, p, here)?
                        )),
                        None => out.push_str(&format!("    {vname}: void,\n")),
                    }
                }
                if let Some(codec) = self.prog.types[t].codec {
                    out.push_str(&self.decode_method(codec, &name, here)?);
                    out.push_str(&self.encode_method(codec, &name, here)?);
                    out.push_str(&self.deinit_sum(t, &name));
                }
                out.push_str("};\n");
            }
            crate::ir::typed::TypeKind::Enum { base, members } => {
                let base = *base;
                let members = members.clone();
                out.push_str(&format!("pub const {name} = enum({}) {{\n", int_ty(base)));
                for m in &members {
                    out.push_str(&format!("    {} = {},\n", field_name(&m.name), m.value));
                }
                out.push_str("    _,\n");
                if let Some(codec) = self.prog.types[t].codec {
                    out.push_str(&self.decode_method(codec, &name, here)?);
                    out.push_str(&self.encode_method(codec, &name, here)?);
                }
                out.push_str("};\n");
            }
            crate::ir::typed::TypeKind::Alias { target } => {
                let target = *target;
                let tname = self.type_ref(target, here);
                out.push_str(&format!("pub const {name} = {tname};\n"));
            }
        }
        Ok(out)
    }

    fn doc(&self, t: TypeIdx) -> String {
        format!("/// {}\n", self.origin_doc(&self.prog.types[t].origin))
    }

    fn origin_doc(&self, origin: &Origin) -> String {
        match origin {
            Origin::Packet { name, routes } => {
                let r = routes
                    .iter()
                    .map(|r| format!("{} {} {}", r.state, r.direction, r.id))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Packet `{name}` ({r}).")
            }
            Origin::Encoding { name } => format!("Encoding `{name}`."),
            Origin::Enum { name } => format!("Enum `{name}`."),
            Origin::Frame { name } => format!("Frame `{name}`."),
            Origin::Field { parent, field } => {
                format!(
                    "Field `{field}` of `{}`.",
                    self.prog.types[*parent].name.pascal()
                )
            }
            Origin::Variant { sum, variant } => {
                let vname = match &self.prog.types[*sum].kind {
                    crate::ir::typed::TypeKind::Sum { variants } => variants[*variant].name.snake(),
                    _ => "?".into(),
                };
                format!(
                    "Arm `{vname}` of sum `{}`.",
                    self.prog.types[*sum].name.pascal()
                )
            }
        }
    }

    /// Zig type of a record field, boxing cycle edges.
    fn field_ty(
        &self,
        t: TypeIdx,
        f: usize,
        decl: &crate::ir::typed::FieldDecl,
        here: &str,
    ) -> BpsResult<String> {
        let inner = self.zig_ty(&decl.ty, here)?;
        let boxed = self.boxed_field.contains(&(t, f));
        Ok(wrap_optional(wrap_boxed(inner, boxed), decl.optional))
    }

    /// Zig type of a sum variant payload.
    fn payload_ty(&self, t: TypeIdx, v: usize, payload: &Ty, here: &str) -> BpsResult<String> {
        Ok(wrap_boxed(
            self.zig_ty(payload, here)?,
            self.boxed_variant.contains(&(t, v)),
        ))
    }

    /// A named type as this module spells it: `@import("x.zig").Foo` when the
    /// type belongs to another module, the bare name inside its own. Zig
    /// declarations are module-private without `pub`, so nothing else works.
    fn type_ref(&self, ty: TypeIdx, here: &str) -> String {
        let name = self.prog.types[ty].name.pascal();
        let stem = self.prog.output_stem(&self.prog.types[ty].origin);
        if stem == here {
            name
        } else {
            format!("@import(\"{stem}.zig\").{name}")
        }
    }

    fn zig_ty(&self, ty: &Ty, here: &str) -> BpsResult<String> {
        match ty {
            Ty::Int(t) => Ok(int_ty(*t).into()),
            Ty::Bool => Ok("bool".into()),
            Ty::F32 => Ok("f32".into()),
            Ty::F64 => Ok("f64".into()),
            Ty::Str | Ty::Bytes => Ok("[]u8".into()),
            Ty::List(inner) => Ok(format!("[]{}", self.zig_ty(inner, here)?)),
            Ty::Named(t) => Ok(self.type_ref(*t, here)),
        }
    }
}

/// `*T` when the position is boxed.
fn wrap_boxed(inner: String, boxed: bool) -> String {
    if boxed { format!("*{inner}") } else { inner }
}

/// `?T` for optional fields and locals.
fn wrap_optional(inner: String, optional: bool) -> String {
    if optional { format!("?{inner}") } else { inner }
}

fn int_ty(ty: IntTy) -> &'static str {
    match ty {
        IntTy::I8 => "i8",
        IntTy::I16 => "i16",
        IntTy::I32 => "i32",
        IntTy::I64 => "i64",
        IntTy::I128 => "i128",
        IntTy::U8 => "u8",
        IntTy::U16 => "u16",
        IntTy::U32 => "u32",
        IntTy::U64 => "u64",
    }
}
impl Printer<'_> {
    // -- codecs ---------------------------------------------------------

    fn free_codec(&self, c: CodecIdx, here: &str) -> BpsResult<String> {
        let mut out = String::new();
        out.push_str(&format!(
            "/// {}\n",
            self.origin_doc(&self.prog.codecs[c].origin)
        ));
        let name = self.prog.codecs[c].name.pascal();
        let ret = self.zig_ty(&self.prog.codecs[c].value, here)?;
        out.push_str(&self.function(c, None, &format!("decode{name}"), &ret, true, here)?);
        let value_ty = self.borrow_ty(&self.prog.codecs[c].value.clone(), here)?;
        out.push_str(&self.function(c, None, &format!("encode{name}"), &value_ty, false, here)?);
        Ok(out)
    }

    /// A decode or encode function. `recv` is `Some(type)` for methods and
    /// `None` for free functions (named `fn_name`); `extra` is the encode
    /// input type for free encode functions (borrowed slices).
    fn function(
        &self,
        c: CodecIdx,
        recv: Option<&str>,
        fn_name: &str,
        extra: &str,
        decode: bool,
        here: &str,
    ) -> BpsResult<String> {
        let codec = &self.prog.codecs[c];
        let body = if decode { &codec.decode } else { &codec.encode };
        let info = analyze_body(self, body);
        let mut sig = String::new();
        sig.push_str(&format!("pub fn {fn_name}("));
        let mut first = true;
        let mut param = |sig: &mut String, s: &str| {
            if !first {
                sig.push_str(", ");
            }
            first = false;
            sig.push_str(s);
        };
        if decode && owns_value(self, &codec.value) {
            param(&mut sig, "allocator: std.mem.Allocator");
        }
        match recv {
            Some(t) if !decode => param(&mut sig, &format!("self: {t}")),
            _ => {}
        }
        if decode {
            param(&mut sig, "reader: *bml_rt.Reader");
        } else if recv.is_none() {
            param(&mut sig, "writer: *bml_rt.Writer");
            param(&mut sig, &format!("value: {extra}"));
        } else {
            param(&mut sig, "writer: *bml_rt.Writer");
        }
        for pid in &body.params {
            let p = &body.locals[*pid];
            param(
                &mut sig,
                &format!(
                    "{}: {}",
                    local_name(&p.name, &self.stems),
                    self.param_ty(p, here)?
                ),
            );
        }
        let ret = if decode {
            match recv {
                Some(t) => t.to_string(),
                None => self.zig_ty(&codec.value, here)?,
            }
        } else {
            "void".to_string()
        };
        let mut fgen = FnGen::new(self, body, c, decode, info, here);
        fgen.depth += if recv.is_some() { 1 } else { 0 };
        fgen.stmts(&body.stmts)?;
        let pad = if recv.is_some() { "    " } else { "" };
        let mut out = format!("{pad}{sig}) bml_rt.Error!{ret} {{\n");
        // Report this codec at the deepest failure: `errdefer` runs inner to
        // outer, and `fail` keeps the first codec seen.
        let diag = match recv {
            Some(t) => format!("{t}.{fn_name}"),
            None => fn_name.to_string(),
        };
        let cursor = if decode { "reader" } else { "writer" };
        if decode {
            fgen.uses_reader = true;
        } else {
            fgen.uses_writer = true;
        }
        out.push_str(&format!(
            "{pad}    errdefer |err| {cursor}.fail(\"{diag}\", @errorName(err));\n"
        ));
        // `_ = x;` for signature names the body never reads.
        if decode {
            if !fgen.uses_reader {
                out.push_str(&format!("{pad}    _ = reader;\n"));
            }
            if owns_value(self, &codec.value) && !fgen.uses_alloc {
                out.push_str(&format!("{pad}    _ = allocator;\n"));
            }
        } else {
            if !fgen.uses_writer {
                out.push_str(&format!("{pad}    _ = writer;\n"));
            }
            if let Some(v) = body.value
                && body.locals[v].uses == 0
            {
                out.push_str(&format!("{pad}    _ = {};\n", self.value_name(c, body, v)));
            }
        }
        for pid in &body.params {
            if body.locals[*pid].uses == 0 {
                out.push_str(&format!(
                    "{pad}    _ = {};\n",
                    local_name(&body.locals[*pid].name, &self.stems)
                ));
            }
        }
        out.push_str(&fgen.out);
        out.push_str(&format!("{pad}}}\n"));
        Ok(out)
    }

    /// How body `value` prints in an encode signature: `self` in methods.
    fn value_name(&self, c: CodecIdx, body: &Body, v: LocalId) -> String {
        if self.prog.codecs[c].ty.is_some() {
            return "self".into();
        }
        local_name(&body.locals[v].name, &self.stems)
    }

    fn decode_method(&self, c: CodecIdx, recv: &str, here: &str) -> BpsResult<String> {
        self.function(c, Some(recv), "decode", "", true, here)
    }

    fn encode_method(&self, c: CodecIdx, recv: &str, here: &str) -> BpsResult<String> {
        self.function(c, Some(recv), "encode", "", false, here)
    }

    /// Borrowed form of a type for free encode inputs: slices become const.
    fn borrow_ty(&self, ty: &Ty, here: &str) -> BpsResult<String> {
        match ty {
            Ty::Str | Ty::Bytes => Ok("[]const u8".into()),
            Ty::List(inner) => Ok(format!("[]const {}", self.zig_ty(inner, here)?)),
            _ => self.zig_ty(ty, here),
        }
    }

    /// Zig type of a codec parameter (never a boxed pointer in practice).
    fn param_ty(&self, p: &crate::ir::typed::Local, here: &str) -> BpsResult<String> {
        Ok(wrap_optional(self.zig_ty(&p.ty, here)?, p.optional))
    }
}
impl Printer<'_> {
    // -- deinit ---------------------------------------------------------

    /// `deinit` for a record, if any field owns memory.
    fn deinit_record(&self, t: TypeIdx, recv: &str) -> String {
        let fields = match &self.prog.types[t].kind {
            crate::ir::typed::TypeKind::Record { fields } => fields.clone(),
            _ => return String::new(),
        };
        let mut body = String::new();
        for (f, decl) in fields.iter().enumerate() {
            let boxed = self.boxed_field.contains(&(t, f));
            for s in cleanup(
                &self.owns,
                &format!("self.{}", field_name(&decl.name)),
                &decl.ty,
                boxed,
                decl.optional,
            ) {
                body.push_str(&format!("        {s}\n"));
            }
        }
        if body.is_empty() {
            return String::new();
        }
        format!(
            "    pub fn deinit(self: {recv}, allocator: std.mem.Allocator) void {{\n{body}    }}\n"
        )
    }

    /// `deinit` for a sum, if any payload owns memory.
    fn deinit_sum(&self, t: TypeIdx, recv: &str) -> String {
        let variants = match &self.prog.types[t].kind {
            crate::ir::typed::TypeKind::Sum { variants } => variants.clone(),
            _ => return String::new(),
        };
        let mut arms = String::new();
        for (v, decl) in variants.iter().enumerate() {
            let vname = field_name(&decl.name);
            let boxed = self.boxed_variant.contains(&(t, v));
            match &decl.payload {
                None => arms.push_str(&format!("            .{vname} => {{}},\n")),
                Some(p) => {
                    let steps = cleanup(&self.owns, "bml_v", p, boxed, false);
                    if steps.is_empty() {
                        arms.push_str(&format!("            .{vname} => {{}},\n"));
                    } else if steps.len() == 1 {
                        let step = steps[0].trim_end_matches(';');
                        arms.push_str(&format!("            .{vname} => |bml_v| {step},\n"));
                    } else {
                        arms.push_str(&format!("            .{vname} => |bml_v| {{\n"));
                        for s in &steps {
                            arms.push_str(&format!("                {s}\n"));
                        }
                        arms.push_str("            },\n");
                    }
                }
            }
        }
        if !arms.contains("bml_v") {
            return String::new();
        }
        format!(
            "    pub fn deinit(self: {recv}, allocator: std.mem.Allocator) void {{\n        switch (self) {{\n{arms}        }}\n    }}\n"
        )
    }
}

/// Statements freeing the value `x` of type `ty` (`boxed`: `x` is a `*T`
/// made by this printer). Optionals unwrap first. Returns no statements
/// when nothing owns memory.
fn cleanup(owns: &[bool], x: &str, ty: &Ty, boxed: bool, optional: bool) -> Vec<String> {
    if !optional {
        return cleanup_inner(owns, x, ty, boxed);
    }
    let inner = cleanup_inner(owns, "bml_v", ty, boxed);
    if inner.is_empty() {
        return Vec::new();
    }
    if inner.len() == 1 {
        return vec![format!("if ({x}) |bml_v| {}", inner[0])];
    }
    let mut out = vec![format!("if ({x}) |bml_v| {{")];
    out.extend(inner.into_iter().map(|s| format!("    {s}")));
    out.push("}".into());
    out
}

fn cleanup_inner(owns: &[bool], x: &str, ty: &Ty, boxed: bool) -> Vec<String> {
    if boxed {
        // Boxed positions hold named types. `deinit` exists exactly when the
        // target owns; the allocation itself always needs destroying.
        if matches!(ty, Ty::Named(t) if owns[*t]) {
            return vec![
                format!("{x}.*.deinit(allocator);"),
                format!("allocator.destroy({x});"),
            ];
        }
        return vec![format!("allocator.destroy({x});")];
    }
    match ty {
        Ty::Str | Ty::Bytes => vec![format!("allocator.free({x});")],
        Ty::List(elem) => {
            let mut out = Vec::new();
            let steps = elem_cleanup(owns, "bml_e", elem);
            if steps.is_empty() {
                out.push(format!("allocator.free({x});"));
            } else if steps.len() == 1 {
                out.push(format!("for ({x}) |*bml_e| {}", steps[0]));
                out.push(format!("allocator.free({x});"));
            } else {
                out.push(format!("for ({x}) |*bml_e| {{"));
                for s in steps {
                    out.push(format!("    {s}"));
                }
                out.push("}".into());
                out.push(format!("allocator.free({x});"));
            }
            out
        }
        Ty::Named(t) if owns[*t] => vec![format!("{x}.deinit(allocator);")],
        Ty::Named(_) => Vec::new(),
        _ => Vec::new(),
    }
}

/// Cleanup of a list element behind `|*bml_e|`: `bml_e.*` is the element.
fn elem_cleanup(owns: &[bool], e: &str, elem: &Ty) -> Vec<String> {
    match elem {
        Ty::Str | Ty::Bytes => vec![format!("allocator.free({e}.*);")],
        Ty::List(inner) => {
            let mut out = Vec::new();
            let steps = elem_cleanup(owns, "bml_f", inner);
            if steps.is_empty() {
                out.push(format!("allocator.free({e}.*);"));
            } else {
                out.push(format!("for ({e}.*) |*bml_f| {{"));
                for s in steps {
                    out.push(format!("    {s}"));
                }
                out.push("}".into());
                out.push(format!("allocator.free({e}.*);"));
            }
            out
        }
        Ty::Named(t) if owns[*t] => vec![format!("{e}.deinit(allocator);")],
        _ => Vec::new(),
    }
}
// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// Per-body facts the statement printer needs.
struct BodyInfo {
    /// Locals held as `*T`: used in a boxed field or variant position, or
    /// unwrapped/expected from one.
    ptr: HashSet<LocalId>,
    /// `bytes` locals consumed once by `Utf8`: no `errdefer` at the read,
    /// ownership moves into the string.
    donors: HashSet<LocalId>,
    /// `Utf8` results that are their donor under the donor's name (both are
    /// `[]u8`): the statement only validates, with no second binding.
    alias: HashMap<LocalId, LocalId>,
}

/// Collect [`BodyInfo`]: pointer marks, single-use `Utf8` donors, and the
/// results that can keep the donor's name.
fn analyze_body(printer: &Printer<'_>, body: &Body) -> BodyInfo {
    let ptr = ptr_marks(printer, body);
    let mut donors = HashSet::new();
    let mut items = HashSet::new();
    walk_stmts(&body.stmts, &mut |s| match s {
        Stmt::Utf8 {
            src: Expr::Local(id),
            ..
        } if body.locals[*id].uses == 1 && is_fresh_bytes(body, &body.stmts, *id) => {
            donors.insert(*id);
        }
        Stmt::Repeat { item, .. } => {
            items.insert(*item);
        }
        _ => {}
    });
    let mut alias = HashMap::new();
    walk_stmts(&body.stmts, &mut |s| {
        if let Stmt::Utf8 {
            dst,
            src: Expr::Local(id),
        } = s
            && donors.contains(id)
            && dst.declare
            && !dst.some
            && !ptr.contains(&dst.local)
            && !items.contains(&dst.local)
        {
            alias.insert(dst.local, *id);
        }
    });
    BodyInfo { ptr, donors, alias }
}

/// Pointer-marked locals: boxed-position uses, then `Unwrap`/`ExpectVariant`
/// destinations inheriting pointer-ness, to a fixpoint.
fn ptr_marks(printer: &Printer<'_>, body: &Body) -> HashSet<LocalId> {
    let mut ptr = HashSet::new();
    mark_boxed_uses(printer, body, &body.stmts, &mut ptr);
    loop {
        let mut changed = false;
        walk_stmts(&body.stmts, &mut |s| match s {
            Stmt::Unwrap { dst, src, .. } => {
                if expr_ptr(printer, body, &ptr, src) && ptr.insert(dst.local) {
                    changed = true;
                }
            }
            Stmt::ExpectVariant {
                dst: Some(dst),
                ty,
                variant,
                ..
            } if printer.boxed_variant.contains(&(*ty, *variant)) && ptr.insert(dst.local) => {
                changed = true;
            }
            _ => {}
        });
        if !changed {
            break;
        }
    }
    ptr
}

/// Whether `id` is bound once by a `bytes`/`rest` read (a transferable buffer).
fn is_fresh_bytes(body: &Body, stmts: &[Stmt], id: LocalId) -> bool {
    let mut found = false;
    walk_stmts(stmts, &mut |s| match s {
        Stmt::Read {
            dst,
            op: ReadOp::Bytes(_) | ReadOp::Rest,
        } if dst.local == id && dst.declare => {
            found = true;
        }
        _ => {}
    });
    let _ = body;
    found
}

/// Mark locals appearing in boxed field or variant-payload positions.
fn mark_boxed_uses(printer: &Printer<'_>, body: &Body, stmts: &[Stmt], ptr: &mut HashSet<LocalId>) {
    walk_stmts(stmts, &mut |s| match s {
        Stmt::Let { value, .. } | Stmt::Return(value) => {
            walk_expr(value, &mut |e| match e {
                Expr::Construct { ty, fields } => {
                    for (f, e) in fields.iter().enumerate() {
                        if printer.boxed_field.contains(&(*ty, f))
                            && let Expr::Local(id) = e
                        {
                            ptr.insert(*id);
                        }
                    }
                }
                Expr::Variant {
                    ty,
                    variant,
                    payload: Some(payload),
                } if printer.boxed_variant.contains(&(*ty, *variant))
                    && let Expr::Local(id) = payload.as_ref() =>
                {
                    ptr.insert(*id);
                }
                _ => {}
            });
        }
        _ => {}
    });
    let _ = body;
}

/// Whether an expression already has pointer type (`*T`).
fn expr_ptr(printer: &Printer<'_>, _body: &Body, ptr: &HashSet<LocalId>, e: &Expr) -> bool {
    match e {
        Expr::Local(id) => ptr.contains(id),
        Expr::Field { ty, field, .. } => printer.boxed_field.contains(&(*ty, *field)),
        Expr::Get(inner) => expr_ptr(printer, _body, ptr, inner),
        _ => false,
    }
}

fn walk_stmts(stmts: &[Stmt], f: &mut impl FnMut(&Stmt)) {
    for s in stmts {
        f(s);
        match s {
            Stmt::If { then, els, .. } => {
                walk_stmts(then, f);
                walk_stmts(els, f);
            }
            Stmt::Match { arms, other, .. } => {
                for a in arms {
                    walk_stmts(&a.body, f);
                }
                if let Some(o) = other {
                    walk_stmts(o, f);
                }
            }
            Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => walk_stmts(body, f),
            _ => {}
        }
    }
}

fn walk_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Field { base, .. }
        | Expr::Present(base)
        | Expr::Get(base)
        | Expr::Not(base)
        | Expr::StrBytes(base) => walk_expr(base, f),
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        Expr::Cond {
            cond, then, els, ..
        } => {
            walk_expr(cond, f);
            walk_expr(then, f);
            walk_expr(els, f);
        }
        Expr::Cast { expr, .. } => walk_expr(expr, f),
        Expr::Call { args, .. } => {
            for a in args {
                walk_expr(a, f);
            }
        }
        Expr::Construct { fields, .. } => {
            for x in fields {
                walk_expr(x, f);
            }
        }
        Expr::Variant {
            payload: Some(p), ..
        } => {
            walk_expr(p, f);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Statement printer
// ---------------------------------------------------------------------------

/// How a repeat body binds its item: into a preallocated slot (`Count`) or a
/// fresh const for `ArrayList::append` (`Push`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemMode {
    Count,
    Push,
}

struct FnGen<'a, 'p> {
    printer: &'a Printer<'p>,
    body: &'p Body,
    /// Module being printed; references into other modules qualify.
    here: String,
    decode: bool,
    /// Encode bodies of type codecs run as methods on `self`.
    method: bool,
    info: BodyInfo,
    /// The repeat item being bound, if inside a repeat body.
    item: Option<(LocalId, ItemMode)>,
    /// Locals whose partial value already unwinds (optional vars).
    covered: HashSet<LocalId>,
    fresh: usize,
    taken: HashSet<String>,
    uses_reader: bool,
    uses_writer: bool,
    uses_alloc: bool,
    out: String,
    depth: usize,
    rep_tmp: Option<String>,
    rep_filled: Option<String>,
}

impl<'a, 'p> FnGen<'a, 'p> {
    fn new(
        printer: &'a Printer<'p>,
        body: &'p Body,
        codec: CodecIdx,
        decode: bool,
        info: BodyInfo,
        here: &str,
    ) -> Self {
        let mut taken = HashSet::new();
        for l in &body.locals {
            taken.insert(local_name(&l.name, &printer.stems));
        }
        let mut covered = HashSet::new();
        for (id, l) in body.locals.iter().enumerate() {
            if l.optional {
                covered.insert(id);
            }
        }
        Self {
            printer,
            body,
            here: here.to_string(),
            decode,
            method: printer.prog.codecs[codec].ty.is_some() && !decode,
            info,
            item: None,
            covered,
            fresh: 0,
            taken,
            uses_reader: false,
            uses_writer: false,
            uses_alloc: false,
            out: String::new(),
            depth: 1,
            rep_tmp: None,
            rep_filled: None,
        }
    }

    /// A collision-free temporary base name.
    fn temp(&mut self, base: &str) -> String {
        loop {
            let n = format!("{base}{}", self.fresh);
            self.fresh += 1;
            if self.taken.insert(n.clone()) {
                return n;
            }
        }
    }

    fn ln(&mut self, s: &str) {
        // Expressions broken over lines (long `|`-chains) carry their own
        // continuation indent; only the first line is indented here.
        for (i, line) in s.split('\n').enumerate() {
            if i > 0 {
                self.out.push_str(line);
                self.out.push('\n');
                continue;
            }
            for _ in 0..self.depth {
                self.out.push_str("    ");
            }
            self.out.push_str(line);
            self.out.push('\n');
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) -> BpsResult<()> {
        for s in stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    /// Zig type of a local, pointer-aware.
    fn local_ty(&self, id: LocalId) -> BpsResult<String> {
        let l = &self.body.locals[id];
        Ok(wrap_optional(
            wrap_boxed(
                self.printer.zig_ty(&l.ty, &self.here)?,
                self.info.ptr.contains(&id),
            ),
            l.optional,
        ))
    }

    /// How a local prints: the encode input is `self` in methods.
    fn value_ref(&self, id: LocalId) -> String {
        if self.method && Some(id) == self.body.value {
            return "self".into();
        }
        self.name(id)
    }

    /// A local's Zig name; an aliased `Utf8` result takes its donor's.
    fn name(&self, id: LocalId) -> String {
        let id = self.info.alias.get(&id).copied().unwrap_or(id);
        local_name(&self.body.locals[id].name, &self.printer.stems)
    }

    /// `errdefer` freeing `name` (called right after its binding). A
    /// multi-statement cleanup goes in one block so the statements keep
    /// their order (`errdefer`s otherwise unwind in reverse).
    fn errdefer(&mut self, name: &str, id: LocalId) {
        let l = &self.body.locals[id];
        let boxed = self.info.ptr.contains(&id);
        let steps = cleanup(&self.printer.owns, name, &l.ty.clone(), boxed, l.optional);
        self.errdefer_steps(&steps);
    }

    fn errdefer_steps(&mut self, steps: &[String]) {
        if steps.is_empty() {
            return;
        }
        if steps.len() == 1 && !steps[0].starts_with("if (") && !steps[0].starts_with("for (") {
            self.ln(&format!("errdefer {}", steps[0]));
            return;
        }
        self.ln("errdefer {");
        self.depth += 1;
        for s in steps {
            self.ln(s);
        }
        self.depth -= 1;
        self.ln("}");
    }

    /// Unwind the decoded prefix of a list being built: `slice` is the built
    /// part (`tmp[0..filled]` or `arr.items`), `elem` the element type.
    fn partial_deinit(&mut self, slice: &str, elem: &Ty) {
        let steps = elem_cleanup(&self.printer.owns, "bml_e", elem);
        if steps.is_empty() {
            return;
        }
        if steps.len() == 1 {
            self.ln("errdefer {");
            self.depth += 1;
            self.ln(&format!("for ({slice}) |*bml_e| {}", steps[0]));
            self.depth -= 1;
            self.ln("}");
            return;
        }
        self.ln("errdefer {");
        self.depth += 1;
        self.ln(&format!("for ({slice}) |*bml_e| {{"));
        self.depth += 1;
        for s in &steps {
            self.ln(s);
        }
        self.depth -= 1;
        self.ln("}");
        self.depth -= 1;
        self.ln("}");
    }

    /// Bind `rhs` into `dst`: repeat slots, optional stores, declarations
    /// with `errdefer` for fresh owned values, or plain stores.
    /// `fresh_owned`: `rhs` newly allocates (read bytes, owning decode,
    /// validated string). `rhs_ptr`: `rhs` already has pointer type.
    fn bind(&mut self, dst: &Bind, rhs: String, fresh_owned: bool, rhs_ptr: bool) -> BpsResult<()> {
        let id = dst.local;
        let name = self.name(id);
        // Repeat items go to the array, never to a named local.
        if let Some((item, mode)) = self.item
            && item == id
        {
            match mode {
                ItemMode::Count => {
                    let slot = format!(
                        "{}[{}]",
                        self.rep_tmp.clone().unwrap_or_default(),
                        self.rep_filled.clone().unwrap_or_default()
                    );
                    self.ln(&format!("{slot} = {rhs};"));
                }
                ItemMode::Push => {
                    let ty = self.local_ty(id)?;
                    self.ln(&format!("const {name}: {ty} = {rhs};"));
                }
            }
            return Ok(());
        }
        let marked = self.info.ptr.contains(&id);
        // Pointer-marked destinations materialize through `create`.
        let (rhs, fresh_owned) = if marked && !rhs_ptr {
            (self.boxed(id, rhs)?, true)
        } else {
            (rhs, fresh_owned)
        };
        if dst.some {
            self.ln(&format!("{name} = {rhs};"));
            return Ok(());
        }
        if dst.declare {
            let ty = self.local_ty(id)?;
            self.ln(&format!("const {name}: {ty} = {rhs};"));
            if fresh_owned && self.decode && !self.info.donors.contains(&id) {
                self.errdefer(&name, id);
            }
            return Ok(());
        }
        self.ln(&format!("{name} = {rhs};"));
        if fresh_owned && self.decode && !self.covered.contains(&id) {
            self.errdefer(&name, id);
            self.covered.insert(id);
        }
        Ok(())
    }

    /// `rhs` (a `T` value) boxed into a `*T` allocation expression for local
    /// `id`, always expanded: `zig fmt` keeps the block form at any width.
    fn boxed(&mut self, id: LocalId, rhs: String) -> BpsResult<String> {
        let target = self
            .printer
            .zig_ty(&self.body.locals[id].ty.clone(), &self.here)?;
        let blk = self.temp("bml_blk");
        let ptr = self.temp("bml_p");
        self.uses_alloc = true;
        let pad0 = "    ".repeat(self.depth);
        let pad1 = "    ".repeat(self.depth + 1);
        Ok(format!(
            "{blk}: {{\n{pad1}const {ptr} = allocator.create({target}) catch return error.OutOfMemory;\n{pad1}errdefer allocator.destroy({ptr});\n{pad1}{ptr}.* = {rhs};\n{pad1}break :{blk} {ptr};\n{pad0}}}"
        ))
    }
}
impl<'a, 'p> FnGen<'a, 'p> {
    // -- statements ---------------------------------------------------

    fn stmt(&mut self, s: &Stmt) -> BpsResult<()> {
        match s {
            Stmt::Declare { local, zero } => self.declare(*local, *zero),
            Stmt::Let { dst, value } => {
                let rhs = self.expr(value)?;
                self.bind(dst, rhs, false, false)
            }
            Stmt::Read { dst, op } => {
                let rhs = self.read(op)?;
                let fresh = matches!(op, ReadOp::Bytes(_) | ReadOp::Rest);
                self.bind(dst, rhs, fresh, false)
            }
            Stmt::Decode { dst, codec, args } => {
                let rhs = self.decode_call(*codec, args)?;
                let fresh = owns_value(self.printer, &self.printer.prog.codecs[*codec].value);
                // A decode call returns a value; a pointer-marked destination
                // boxes it in `bind`.
                self.bind(dst, rhs, fresh, false)
            }
            Stmt::Utf8 { dst, src } => self.utf8(dst, src),
            Stmt::Write { op, value } => {
                let v = self.expr(value)?;
                self.uses_writer = true;
                match op {
                    WriteOp::U8 => {
                        let byte = byte_literal(value, false).unwrap_or(v);
                        self.ln(&format!("try writer.writeU8({byte});"));
                    }
                    WriteOp::F32 => self.ln(&format!("try writer.writeF32Be({v});")),
                    WriteOp::F64 => self.ln(&format!("try writer.writeF64Be({v});")),
                    WriteOp::Bytes(n) => {
                        let n = self.usize_arg(n)?;
                        self.ln(&format!("try writer.writeSized({v}, {n});"));
                    }
                    WriteOp::Rest => self.ln(&format!("try writer.writeBytes({v});")),
                }
                Ok(())
            }
            Stmt::Encode { codec, args, value } => {
                let call = self.encode_call(*codec, args, value)?;
                self.ln(&format!("try {call};"));
                Ok(())
            }
            Stmt::Require { cond, text } => {
                // Fail when the negation holds; the comment keeps the
                // program's original condition text.
                let neg = self.printer.prog.negated(self.body, cond);
                let c = self.expr(&neg)?;
                self.ln(&format!(
                    "if ({c}) return error.RequireFailed; // require {text}"
                ));
                Ok(())
            }
            Stmt::Unwrap { dst, src, .. } => {
                let s = self.expr(src)?;
                let rhs = format!("({s}) orelse return error.MissingField");
                let ptr = expr_ptr(self.printer, self.body, &self.info.ptr, src);
                self.bind(dst, rhs, false, ptr)
            }
            Stmt::If { cond, then, els } => {
                let c = self.cond(cond)?;
                self.ln(&format!("if ({c}) {{"));
                self.depth += 1;
                self.stmts(then)?;
                self.depth -= 1;
                if els.is_empty() {
                    self.ln("}");
                } else {
                    self.ln("} else {");
                    self.depth += 1;
                    self.stmts(els)?;
                    self.depth -= 1;
                    self.ln("}");
                }
                Ok(())
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => self.match_stmt(scrutinee, arms, other),
            Stmt::Repeat {
                mode,
                list,
                item,
                body,
            } => self.repeat(mode, *list, *item, body),
            Stmt::ForEach { list, item, body } => self.for_each(list, *item, body),
            Stmt::ExpectVariant {
                dst,
                sum,
                ty,
                variant,
            } => self.expect_variant(dst, sum, *ty, *variant),
            Stmt::Return(e) => {
                // A long struct literal goes one field per line.
                if let Expr::Construct { ty, fields } = e {
                    let parts = self.construct_parts(*ty, fields)?;
                    if !parts.is_empty() {
                        let single = format!(".{{ {} }}", parts.join(", "));
                        if self.depth * 4 + "return ".len() + single.len() < MAX_WIDTH {
                            self.ln(&format!("return {single};"));
                        } else {
                            self.ln("return .{");
                            self.depth += 1;
                            for part in &parts {
                                self.ln(&format!("{part},"));
                            }
                            self.depth -= 1;
                            self.ln("};");
                        }
                        return Ok(());
                    }
                }
                let r = self.return_expr(e)?;
                self.ln(&format!("return {r};"));
                Ok(())
            }
            Stmt::Fail(_) => {
                self.ln("return error.UntilEnd;");
                Ok(())
            }
        }
    }

    fn declare(&mut self, local: LocalId, zero: bool) -> BpsResult<()> {
        let l = &self.body.locals[local];
        let name = local_name(&l.name, &self.printer.stems);
        if l.optional {
            let ty = self.local_ty(local)?;
            self.ln(&format!("var {name}: {ty} = null;"));
            if self.decode && owns_value(self.printer, &l.ty.clone()) {
                self.errdefer(&name, local);
            }
            return Ok(());
        }
        let init = match &l.ty {
            Ty::Int(_) => "0",
            Ty::Bool => "false",
            Ty::F32 | Ty::F64 => "0",
            _ => "undefined",
        };
        let _ = zero;
        let ty = self.local_ty(local)?;
        self.ln(&format!("var {name}: {ty} = {init};"));
        Ok(())
    }

    fn read(&mut self, op: &ReadOp) -> BpsResult<String> {
        self.uses_reader = true;
        match op {
            ReadOp::U8 => Ok("try reader.readU8()".into()),
            ReadOp::F32 => Ok("try reader.readF32Be()".into()),
            ReadOp::F64 => Ok("try reader.readF64Be()".into()),
            ReadOp::Bytes(n) => {
                self.uses_alloc = true;
                let n = self.expr(n)?;
                Ok(format!("try reader.readSized(allocator, {n})"))
            }
            ReadOp::Rest => {
                self.uses_alloc = true;
                Ok("try reader.readBytes(allocator, reader.remaining())".into())
            }
        }
    }
    /// A length cast to `usize`: a widening hop through `i64` is covered by
    /// the outer cast, so it is skipped (the negativity guard, if any, still
    /// reads the original expression).
    fn usize_arg(&mut self, e: &Expr) -> BpsResult<String> {
        // A cast widening to `i64` is covered by the outer cast to `usize`.
        let mut core = e;
        if let Expr::Cast { expr, to } = e
            && *to == Ty::Int(IntTy::I64)
        {
            let (cty, _) = self.printer.prog.expr_ty(self.body, expr);
            if let Ty::Int(f) = cty
                && (f == IntTy::I64 || f.bits() < 64)
            {
                core = expr;
            }
        }
        Ok(format!("@as(usize, @intCast({}))", self.expr(core)?))
    }

    fn utf8(&mut self, dst: &Bind, src: &Expr) -> BpsResult<()> {
        self.uses_alloc = true;
        let s = self.expr(src)?;
        let id = dst.local;
        let name = self.name(id);
        let donor = matches!(src, Expr::Local(l) if self.info.donors.contains(l));
        // Repeat items bind into the array; validate first, then move or dupe.
        if let Some((item, _)) = self.item
            && item == id
        {
            return self.utf8_item(src, &s, donor);
        }
        // The donor becomes the string under its own name: own it and
        // validate it in place.
        if self.info.alias.contains_key(&id) {
            if self.decode && !self.covered.contains(&id) {
                self.errdefer(&name, id);
                self.covered.insert(id);
            }
            self.ln(&format!(
                "if (!std.unicode.utf8ValidateSlice({name})) return error.InvalidUtf8;"
            ));
            return Ok(());
        }
        let rhs = if donor {
            s.clone()
        } else {
            format!("allocator.dupe(u8, {s}) catch return error.OutOfMemory")
        };
        let marked = self.info.ptr.contains(&id);
        let (rhs, fresh) = if marked {
            (self.boxed(id, rhs)?, true)
        } else {
            (rhs, true)
        };
        if dst.some {
            self.ln(&format!("{name} = {rhs};"));
        } else if dst.declare {
            let ty = self.local_ty(id)?;
            self.ln(&format!("const {name}: {ty} = {rhs};"));
        } else {
            self.ln(&format!("{name} = {rhs};"));
        }
        if fresh && self.decode && !self.covered.contains(&id) {
            self.errdefer(&name, id);
            self.covered.insert(id);
        }
        self.ln(&format!(
            "if (!std.unicode.utf8ValidateSlice({name})) return error.InvalidUtf8;"
        ));
        Ok(())
    }

    /// `Utf8` whose destination is a repeat item.
    fn utf8_item(&mut self, src: &Expr, s: &str, donor: bool) -> BpsResult<()> {
        let (_, mode) = self.item.unwrap_or((0, ItemMode::Push));
        self.ln(&format!(
            "if (!std.unicode.utf8ValidateSlice({s})) return error.InvalidUtf8;"
        ));
        match mode {
            ItemMode::Count => {
                let slot = format!(
                    "{}[{}]",
                    self.rep_tmp.clone().unwrap_or_default(),
                    self.rep_filled.clone().unwrap_or_default()
                );
                if donor {
                    self.ln(&format!("{slot} = {s};"));
                } else {
                    self.ln(&format!(
                        "{slot} = allocator.dupe(u8, {s}) catch return error.OutOfMemory;"
                    ));
                }
            }
            ItemMode::Push => {
                let _ = src;
                let name = self.item_name();
                if donor {
                    self.ln(&format!("const {name} = {s};"));
                } else {
                    self.ln(&format!(
                        "const {name} = allocator.dupe(u8, {s}) catch return error.OutOfMemory;"
                    ));
                }
            }
        }
        Ok(())
    }

    fn item_name(&self) -> String {
        match self.item {
            Some((id, _)) => self.name(id),
            None => "bml_item".into(),
        }
    }
}
impl<'a, 'p> FnGen<'a, 'p> {
    fn match_stmt(
        &mut self,
        scrutinee: &Expr,
        arms: &[crate::ir::typed::MatchArm],
        other: &Option<Vec<Stmt>>,
    ) -> BpsResult<()> {
        let s = self.expr(scrutinee)?;
        self.ln(&format!("switch ({s}) {{"));
        self.depth += 1;
        for arm in arms {
            let tags = arm
                .tags
                .iter()
                .map(|t| format!("{t}"))
                .collect::<Vec<_>>()
                .join(", ");
            self.ln(&format!("{tags} => {{"));
            self.depth += 1;
            self.stmts(&arm.body)?;
            self.depth -= 1;
            self.ln("},");
        }
        match other {
            Some(body) => {
                self.ln("else => {");
                self.depth += 1;
                self.stmts(body)?;
                self.depth -= 1;
                self.ln("},");
            }
            None => {
                self.ln("else => return error.NoMatchArm,");
            }
        }
        self.depth -= 1;
        self.ln("}");
        Ok(())
    }

    fn for_each(&mut self, list: &Expr, item: LocalId, body: &[Stmt]) -> BpsResult<()> {
        let l = self.expr(list)?;
        self.uses_writer = true;
        if self.body.locals[item].uses == 0 {
            self.ln(&format!("for ({l}) |_| {{"));
        } else {
            let name = self.name(item);
            self.ln(&format!("for ({l}) |{name}| {{"));
        }
        self.depth += 1;
        self.stmts(body)?;
        self.depth -= 1;
        self.ln("}");
        Ok(())
    }

    fn expect_variant(
        &mut self,
        dst: &Option<Bind>,
        sum: &Expr,
        ty: TypeIdx,
        variant: usize,
    ) -> BpsResult<()> {
        let s = self.expr(sum)?;
        let vname = match &self.printer.prog.types[ty].kind {
            crate::ir::typed::TypeKind::Sum { variants } => field_name(&variants[variant].name),
            _ => return Err(emit("zig: expect on non-sum".to_string())),
        };
        match dst {
            Some(b) => {
                let name = self.name(b.local);
                let head = if b.declare {
                    let ty = self.local_ty(b.local)?;
                    format!("const {name}: {ty} = switch ({s}) {{")
                } else {
                    format!("{name} = switch ({s}) {{")
                };
                self.ln(&head);
                self.depth += 1;
                self.ln(&format!(".{vname} => |{name}| {name},"));
                self.ln("else => return error.WrongVariant,");
                self.depth -= 1;
                self.ln("};");
            }
            None => {
                self.ln(&format!("if ({s} != .{vname}) return error.WrongVariant;"));
            }
        }
        Ok(())
    }

    /// `.name = value` parts of a record construction, in field order.
    fn construct_parts(&mut self, ty: TypeIdx, fields: &[Expr]) -> BpsResult<Vec<String>> {
        let names = match &self.printer.prog.types[ty].kind {
            crate::ir::typed::TypeKind::Record { fields } => fields
                .iter()
                .map(|f| field_name(&f.name))
                .collect::<Vec<_>>(),
            _ => return Err(emit("zig: construct of non-record".to_string())),
        };
        let mut parts = Vec::new();
        for (n, v) in names.iter().zip(fields.iter()) {
            parts.push(format!(".{n} = {}", self.expr(v)?));
        }
        Ok(parts)
    }

    fn return_expr(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Construct { ty, fields } => {
                let parts = self.construct_parts(*ty, fields)?;
                if parts.is_empty() {
                    return Ok(".{}".into());
                }
                Ok(format!(".{{ {} }}", parts.join(", ")))
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                let vname = match &self.printer.prog.types[*ty].kind {
                    crate::ir::typed::TypeKind::Sum { variants } => {
                        field_name(&variants[*variant].name)
                    }
                    _ => return Err(emit("zig: variant of non-sum".to_string())),
                };
                match payload {
                    Some(p) => Ok(format!(".{{ .{vname} = {} }}", self.expr(p)?)),
                    None => Ok(format!(".{{ .{vname} = {{}} }}")),
                }
            }
            _ => self.expr(e),
        }
    }

    /// How this module calls a free codec (`decodeX`), qualified across
    /// modules, or a method codec (`Type.decode`) on its type.
    fn codec_path(&self, codec: CodecIdx, decode: bool) -> String {
        let prog = self.printer.prog;
        match prog.codecs[codec].ty {
            None => {
                let stem = prog.output_stem(&prog.codecs[codec].origin);
                let name = format!(
                    "{}{}",
                    if decode { "decode" } else { "encode" },
                    prog.codecs[codec].name.pascal()
                );
                if stem == self.here {
                    name
                } else {
                    format!("@import(\"{stem}.zig\").{name}")
                }
            }
            Some(_) => {
                let target = decode_target(prog, codec);
                let method = if decode { "decode" } else { "encode" };
                format!("{}.{method}", self.printer.type_ref(target, &self.here))
            }
        }
    }

    /// Call another codec's decode: `Type.decode(allocator?, reader, args)`.
    fn decode_call(&mut self, codec: CodecIdx, args: &[Expr]) -> BpsResult<String> {
        self.uses_reader = true;
        let path = self.codec_path(codec, true);
        let mut parts = Vec::new();
        if owns_value(self.printer, &self.printer.prog.codecs[codec].value) {
            parts.push("allocator".to_string());
            self.uses_alloc = true;
        }
        parts.push("reader".to_string());
        let params = self.printer.prog.codecs[codec].decode.params.clone();
        let callee_ptr = self
            .printer
            .ptr
            .get(&(codec, true))
            .cloned()
            .unwrap_or_default();
        for (a, p) in args.iter().zip(params.iter()) {
            parts.push(self.call_arg(a, *p, &callee_ptr)?);
        }
        Ok(format!("try {path}({})", parts.join(", ")))
    }

    /// Call another codec's encode: free `encodeX(writer, value, args)` or
    /// `value.encode(writer, args)`.
    fn encode_call(&mut self, codec: CodecIdx, args: &[Expr], value: &Expr) -> BpsResult<String> {
        self.uses_writer = true;
        let ty = self.printer.prog.codecs[codec].ty;
        let params = self.printer.prog.codecs[codec].encode.params.clone();
        let callee_ptr = self
            .printer
            .ptr
            .get(&(codec, false))
            .cloned()
            .unwrap_or_default();
        let mut rest = Vec::new();
        for (a, p) in args.iter().zip(params.iter()) {
            rest.push(self.call_arg(a, *p, &callee_ptr)?);
        }
        match ty {
            None => {
                let path = self.codec_path(codec, false);
                let v = self.encode_value(value)?;
                if rest.is_empty() {
                    Ok(format!("{path}(writer, {v})"))
                } else {
                    Ok(format!("{path}(writer, {v}, {})", rest.join(", ")))
                }
            }
            Some(_) => {
                let v = self.encode_value(value)?;
                let method = "encode";
                if rest.is_empty() {
                    Ok(format!("{v}.{method}(writer)"))
                } else {
                    Ok(format!("{v}.{method}(writer, {})", rest.join(", ")))
                }
            }
        }
    }

    /// An encode input or call argument, dereferenced when the Zig value is a
    /// boxed pointer but the callee takes it by value.
    fn encode_value(&mut self, value: &Expr) -> BpsResult<String> {
        let v = self.expr(value)?;
        if expr_ptr(self.printer, self.body, &self.info.ptr, value) {
            Ok(format!("{v}.*"))
        } else {
            Ok(v)
        }
    }

    fn call_arg(
        &mut self,
        arg: &Expr,
        param: LocalId,
        callee_ptr: &HashSet<LocalId>,
    ) -> BpsResult<String> {
        let a = self.expr(arg)?;
        let want_ptr = callee_ptr.contains(&param);
        if expr_ptr(self.printer, self.body, &self.info.ptr, arg) && !want_ptr {
            Ok(format!("{a}.*"))
        } else {
            Ok(a)
        }
    }
}
impl<'a, 'p> FnGen<'a, 'p> {
    fn repeat(
        &mut self,
        mode: &RepeatMode,
        list: LocalId,
        item: LocalId,
        body: &[Stmt],
    ) -> BpsResult<()> {
        if !self.decode {
            return self.repeat_encode(mode, list, item, body);
        }
        self.uses_reader = true;
        self.uses_alloc = true;
        let list_name = self.name(list);
        let list_ty = self.local_ty(list)?;
        let elem_ty = match &self.body.locals[list].ty.clone() {
            Ty::List(e) => self.printer.zig_ty(e, &self.here)?,
            _ => return Err(emit("zig: repeat list is not a list".to_string())),
        };
        let rep = self.temp("bml_rep");
        match mode {
            RepeatMode::Count(n) => {
                let count = self.cond(n)?;
                let alloc_n = self.usize_arg(n)?;
                let tmp = self.temp("bml_tmp");
                let filled = self.temp("bml_filled");
                self.ln(&format!("const {list_name}: {list_ty} = {rep}: {{"));
                self.depth += 1;
                self.ln(&format!("if ({count} < 0) return error.NegativeLength;"));
                self.ln(&format!(
                    "const {tmp} = allocator.alloc({elem_ty}, {alloc_n}) catch return error.OutOfMemory;"
                ));
                self.ln(&format!("errdefer allocator.free({tmp});"));
                self.ln(&format!("var {filled}: usize = 0;"));
                self.partial_deinit(
                    &format!("{tmp}[0..{filled}]"),
                    &self.body.locals[item].ty.clone(),
                );
                self.ln(&format!(
                    "while ({filled} < {tmp}.len) : ({filled} += 1) {{"
                ));
                self.depth += 1;
                let outer = self.item.replace((item, ItemMode::Count));
                let otmp = self.rep_tmp.replace(tmp.clone());
                let ofill = self.rep_filled.replace(filled.clone());
                self.stmts(body)?;
                self.item = outer;
                self.rep_tmp = otmp;
                self.rep_filled = ofill;
                self.depth -= 1;
                self.ln("}");
                self.ln(&format!("break :{rep} {tmp};"));
                self.depth -= 1;
                self.ln("};");
            }
            _ => {
                let term = match mode {
                    RepeatMode::Until(t) => Some(t),
                    _ => None,
                };
                let arr = self.temp("bml_arr");
                self.ln(&format!("const {list_name}: {list_ty} = {rep}: {{"));
                self.depth += 1;
                if let Some(t) = term {
                    // A literal byte compares directly; anything else is
                    // evaluated once, before the loop.
                    let want = match byte_literal(t, false) {
                        Some(byte) => byte,
                        None => {
                            let term_n = self.temp("bml_term");
                            let printed = self.expr(t)?;
                            let (tty, _) = self.printer.prog.expr_ty(self.body, t);
                            let cast = match tty {
                                Ty::Int(f) => self.int_cast(f, IntTy::U8, t, printed)?,
                                _ => printed,
                            };
                            self.ln(&format!("const {term_n}: u8 = {cast};"));
                            term_n
                        }
                    };
                    self.ln(&format!(
                        "var {arr} = std.ArrayList({elem_ty}).init(allocator);"
                    ));
                    self.ln(&format!("errdefer {arr}.deinit();"));
                    self.partial_deinit(
                        &format!("{arr}.items"),
                        &self.body.locals[item].ty.clone(),
                    );
                    self.ln("while (reader.remaining() > 0) {");
                    self.depth += 1;
                    self.ln(&format!("if (reader.data[reader.pos] == {want}) {{"));
                    self.depth += 1;
                    self.ln("reader.pos += 1;");
                    self.ln("break;");
                    self.depth -= 1;
                    self.ln("}");
                    let outer = self.item.replace((item, ItemMode::Push));
                    self.stmts(body)?;
                    self.item = outer;
                    self.ln(&format!(
                        "{arr}.append({}) catch return error.OutOfMemory;",
                        self.name(item)
                    ));
                    self.depth -= 1;
                    self.ln("}");
                } else {
                    self.ln(&format!(
                        "var {arr} = std.ArrayList({elem_ty}).init(allocator);"
                    ));
                    self.ln(&format!("errdefer {arr}.deinit();"));
                    self.partial_deinit(
                        &format!("{arr}.items"),
                        &self.body.locals[item].ty.clone(),
                    );
                    self.ln("while (reader.remaining() > 0) {");
                    self.depth += 1;
                    let outer = self.item.replace((item, ItemMode::Push));
                    self.stmts(body)?;
                    self.item = outer;
                    self.ln(&format!(
                        "{arr}.append({}) catch return error.OutOfMemory;",
                        self.name(item)
                    ));
                    self.depth -= 1;
                    self.ln("}");
                }
                self.ln(&format!(
                    "break :{rep} {arr}.toOwnedSlice() catch return error.OutOfMemory;"
                ));
                self.depth -= 1;
                self.ln("};");
            }
        }
        // The owned slice now belongs to the caller-side value; unwind it on
        // any later failure.
        let ty = self.body.locals[list].ty.clone();
        let boxed = self.info.ptr.contains(&list);
        let steps = cleanup(
            &self.printer.owns,
            &list_name,
            &ty,
            boxed,
            self.body.locals[list].optional,
        );
        self.errdefer_steps(&steps);
        Ok(())
    }

    /// `Repeat` in an encode body (not produced by the builder, but harmless):
    /// iterate the bound list, then write the terminator.
    fn repeat_encode(
        &mut self,
        mode: &RepeatMode,
        list: LocalId,
        item: LocalId,
        body: &[Stmt],
    ) -> BpsResult<()> {
        self.uses_writer = true;
        let name = self.name(list);
        if self.body.locals[item].uses == 0 {
            self.ln(&format!("for ({name}) |_| {{"));
        } else {
            let item_name = self.name(item);
            self.ln(&format!("for ({name}) |{item_name}| {{"));
        }
        self.depth += 1;
        self.stmts(body)?;
        self.depth -= 1;
        self.ln("}");
        match mode {
            RepeatMode::Until(t) => {
                let byte = match byte_literal(t, false) {
                    Some(byte) => byte,
                    None => {
                        let printed = self.expr(t)?;
                        let (tty, _) = self.printer.prog.expr_ty(self.body, t);
                        match tty {
                            Ty::Int(f) => self.int_cast(f, IntTy::U8, t, printed)?,
                            _ => printed,
                        }
                    }
                };
                self.ln(&format!("try writer.writeU8({byte});"));
            }
            RepeatMode::UntilEnd => {
                self.ln("return error.UntilEnd;");
            }
            RepeatMode::Count(_) => {}
        }
        Ok(())
    }
}
impl<'a, 'p> FnGen<'a, 'p> {
    // -- expressions --------------------------------------------------

    fn expr(&mut self, e: &Expr) -> BpsResult<String> {
        match e {
            Expr::Int { value, .. } => Ok(format!("{value}")),
            Expr::Bool(b) => Ok(format!("{b}")),
            Expr::Str(s) => Ok(format!("{s:?}")),
            Expr::Local(id) => Ok(self.value_ref(*id)),
            Expr::Field { base, ty, field } => {
                let b = self.expr(base)?;
                let (f, optional) = match &self.printer.prog.types[*ty].kind {
                    crate::ir::typed::TypeKind::Record { fields } => {
                        (field_name(&fields[*field].name), fields[*field].optional)
                    }
                    _ => return Err(emit("zig: field of non-record".to_string())),
                };
                // A boxed optional unwraps (`orelse`, `.?`) before the
                // dereference; only dereference non-optional fields here.
                if self.printer.boxed_field.contains(&(*ty, *field)) && !optional {
                    Ok(format!("{b}.{f}.*"))
                } else {
                    Ok(format!("{b}.{f}"))
                }
            }
            Expr::Present(inner) => {
                let s = self.expr(inner)?;
                // `!=` needs no outer pair; the inner is a local or field.
                match **inner {
                    Expr::Binary { .. } | Expr::Not(_) | Expr::Cond { .. } | Expr::Present(_) => {
                        Ok(format!("({s}) != null"))
                    }
                    _ => Ok(format!("{s} != null")),
                }
            }
            Expr::Get(inner) => {
                let s = self.expr(inner)?;
                // Postfix `.?` binds tightest; wrap only a loose inner.
                match **inner {
                    Expr::Binary { .. } | Expr::Not(_) | Expr::Cond { .. } | Expr::Present(_) => {
                        Ok(format!("({s}).?"))
                    }
                    _ => Ok(format!("{s}.?")),
                }
            }
            Expr::Not(inner) => {
                let s = self.expr(inner)?;
                // Calls, locals, and fields need no pair after `!`.
                match **inner {
                    Expr::Local(_)
                    | Expr::Int { .. }
                    | Expr::Bool(_)
                    | Expr::Str(_)
                    | Expr::Field { .. }
                    | Expr::Get(_)
                    | Expr::Call { .. } => Ok(format!("!{s}")),
                    _ => Ok(format!("!({s})")),
                }
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
            } => Ok(format!(
                "@as({}, if ({}) {} else {})",
                self.printer.zig_ty(ty, &self.here)?,
                self.cond(cond)?,
                self.expr(then)?,
                self.expr(els)?
            )),
            Expr::Cast { expr, to } => {
                let inner = self.expr(expr)?;
                self.cast_expr(expr, inner, to)
            }
            Expr::Call { func, args } => self.call(*func, args),
            Expr::StrBytes(inner) => self.expr(inner),
            Expr::Remaining => {
                self.uses_reader = true;
                Ok("@as(i64, @intCast(reader.remaining()))".into())
            }
            Expr::Construct { .. } | Expr::Variant { .. } => self.return_expr(e),
        }
    }

    /// A condition: wrapped only when too loose (`and`/`or`/if-expression)
    /// to sit directly in its statement.
    fn cond(&mut self, e: &Expr) -> BpsResult<String> {
        let s = self.expr(e)?;
        match e {
            Expr::Binary {
                op: BinOp::And | BinOp::Or,
                ..
            }
            | Expr::Cond { .. } => Ok(format!("({s})")),
            _ => Ok(s),
        }
    }

    /// An operand of `parent`: wrapped only when precedence needs it.
    fn operand(&mut self, e: &Expr, parent: BinOp, side: Side) -> BpsResult<String> {
        let s = self.expr(e)?;
        if need_parens(e, parent, side) {
            Ok(format!("({s})"))
        } else {
            Ok(s)
        }
    }

    /// An operand of `&`, `|`, or `^`: integer literals print in hex, as do
    /// the branches of a ternary continuation flag (`if (c) 0x80 else 0`).
    fn mask_operand(&mut self, e: &Expr, parent: BinOp, side: Side) -> BpsResult<String> {
        match e {
            Expr::Int { value, .. } if hex_literal(parent, *value) => Ok(zig_hex(*value)),
            Expr::Cond {
                cond,
                then,
                els,
                ty,
            } => Ok(format!(
                "@as({}, if ({}) {} else {})",
                self.printer.zig_ty(ty, &self.here)?,
                self.cond(cond)?,
                self.mask_operand(then, parent, side)?,
                self.mask_operand(els, parent, side)?,
            )),
            _ => self.operand(e, parent, side),
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
        use BinOp::*;
        // String equality is a library call.
        if matches!(op, Eq | Ne) {
            let (lt, _) = self.printer.prog.expr_ty(self.body, lhs);
            if matches!(lt, Ty::Str) {
                let l = self.expr(lhs)?;
                let r = self.expr(rhs)?;
                if matches!(op, Eq) {
                    return Ok(format!("std.mem.eql(u8, {l}, {r})"));
                }
                return Ok(format!("!std.mem.eql(u8, {l}, {r})"));
            }
        }
        let _ = ty;
        // `|`, `&`, `^` chains break over lines when they run long.
        if matches!(op, BitOr | BitAnd | BitXor) {
            return self.bit_chain(op, lhs, rhs);
        }
        // `@divFloor`/`@mod` take their operands as call arguments.
        if matches!(op, Div | Mod) {
            let l = self.expr(lhs)?;
            let r = self.expr(rhs)?;
            return Ok(match op {
                Div => format!("@divFloor({l}, {r})"),
                _ => format!("@mod({l}, {r})"),
            });
        }
        let l = self.operand(lhs, op, Side::Left)?;
        let r = self.operand(rhs, op, Side::Right)?;
        Ok(match op {
            Add if wraps => format!("{l} +% {r}"),
            Sub if wraps => format!("{l} -% {r}"),
            Mul if wraps => format!("{l} *% {r}"),
            Add => format!("{l} + {r}"),
            Sub => format!("{l} - {r}"),
            Mul => format!("{l} * {r}"),
            Shl => format!("{l} << {r}"),
            Shr => format!("{l} >> {r}"),
            Eq => format!("{l} == {r}"),
            Ne => format!("{l} != {r}"),
            Lt => format!("{l} < {r}"),
            Le => format!("{l} <= {r}"),
            Gt => format!("{l} > {r}"),
            Ge => format!("{l} >= {r}"),
            And => format!("{l} and {r}"),
            Or => format!("{l} or {r}"),
            _ => unreachable!("zig: bit/div/mod print above"),
        })
    }

    /// A `|`/`&`/`^` spine, one operand per line when the single line would
    /// run past the width limit (`zig fmt` keeps breaks after operators).
    fn bit_chain(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr) -> BpsResult<String> {
        fn spine<'e>(e: &'e Expr, op: BinOp, out: &mut Vec<&'e Expr>) {
            match e {
                Expr::Binary {
                    op: c, lhs, rhs, ..
                } if *c == op => {
                    spine(lhs, op, out);
                    spine(rhs, op, out);
                }
                _ => out.push(e),
            }
        }
        let mut parts = Vec::new();
        spine(lhs, op, &mut parts);
        spine(rhs, op, &mut parts);
        let symbol = op.symbol();
        let mut flat = Vec::with_capacity(parts.len());
        for (i, p) in parts.iter().enumerate() {
            let side = if i == 0 { Side::Left } else { Side::Right };
            flat.push(self.mask_operand(p, op, side)?);
        }
        let single = flat.join(&format!(" {symbol} "));
        if self.depth * 4 + single.len() <= MAX_WIDTH {
            return Ok(single);
        }
        let pad = "    ".repeat(self.depth + 1);
        Ok(flat.join(&format!(" {symbol}\n{pad}")))
    }

    fn cast_expr(&mut self, inner: &Expr, printed: String, to: &Ty) -> BpsResult<String> {
        let (from, _) = self.printer.prog.expr_ty(self.body, inner);
        if from == *to {
            return Ok(printed);
        }
        match (&from, to) {
            (_, Ty::Named(_)) => Ok(format!("@enumFromInt({printed})")),
            (Ty::Named(_), _) => Ok(format!("@intFromEnum({printed})")),
            (Ty::Bool, Ty::Int(_)) => Ok(format!("@intFromBool({printed})")),
            (Ty::F32, Ty::F64) | (Ty::F64, Ty::F32) => Ok(format!(
                "{}(@floatCast({printed}))",
                self.printer.zig_ty(to, &self.here)?
            )),
            (Ty::Int(_), Ty::F32 | Ty::F64) => Ok(format!(
                "{}(@floatFromInt({printed}))",
                self.printer.zig_ty(to, &self.here)?
            )),
            (Ty::F32 | Ty::F64, Ty::Int(_)) => Ok(format!(
                "{}(@intFromFloat({printed}))",
                self.printer.zig_ty(to, &self.here)?
            )),
            (Ty::Int(f), Ty::Int(t)) => self.int_cast(*f, *t, inner, printed),
            _ => Err(emit(format!("zig: cannot cast {from:?} to {to:?}"))),
        }
    }

    /// A minimal integer cast. The outer `@as` always names the type (never
    /// rely on inference); the operand keeps its own annotation only when it
    /// does not already have the source type. A lossless round trip through
    /// a wider or same-width type (`u64` → `i64` → `u64`) is the value itself.
    fn int_cast(&mut self, from: IntTy, to: IntTy, inner: &Expr, x: String) -> BpsResult<String> {
        if from == to {
            return Ok(x);
        }
        if let Expr::Cast {
            expr: core,
            to: mid,
        } = inner
            && let Ty::Int(mid_ty) = mid
        {
            let (core_ty, _) = self.printer.prog.expr_ty(self.body, core);
            if let Ty::Int(core_from) = core_ty
                && core_from == to
                && mid_ty.bits() >= core_from.bits()
            {
                return self.expr(core);
            }
        }
        // Literals that fit the target need only the target's annotation.
        if let Expr::Int { value, .. } = inner
            && to.contains(*value as i128)
        {
            return Ok(format!("@as({}, {value})", int_ty(to)));
        }
        let need_inner = match inner {
            Expr::Int { .. } => true,
            _ => {
                let (ity, _) = self.printer.prog.expr_ty(self.body, inner);
                ity != Ty::Int(from)
            }
        };
        let x = if need_inner {
            format!("@as({}, {x})", int_ty(from))
        } else {
            x
        };
        let target = int_ty(to);
        // A target that holds every value of the source type coerces; one
        // that holds every value this operand can take (by range) is a
        // value-preserving `@intCast`; anything else wraps.
        if to.contains(from.min_value()) && to.contains(from.max_value()) {
            return Ok(format!("@as({target}, {x})"));
        }
        let (lo, hi) = self
            .printer
            .prog
            .int_range(self.body, inner)
            .unwrap_or((from.min_value(), from.max_value()));
        if to.contains(lo) && to.contains(hi) {
            return Ok(format!("@as({target}, @intCast({x}))"));
        }
        if to.bits() == from.bits() {
            return Ok(format!("@as({target}, @bitCast({x}))"));
        }
        if to.bits() > from.bits() {
            // A signed source into a wider unsigned target: widen with the
            // sign, then reinterpret.
            return Ok(format!(
                "@as({target}, @bitCast(@as({}, {x})))",
                signed_ty(to)
            ));
        }
        // `@truncate` keeps the signedness, so only a sign change goes
        // through the unsigned types.
        if from.signed() == to.signed() {
            return Ok(format!("@as({target}, @truncate({x}))"));
        }
        let base = if from.signed() {
            format!("@as({}, @bitCast({x}))", unsigned_ty(from))
        } else {
            x
        };
        let mid = format!("@as({}, @truncate({base}))", unsigned_ty(to));
        if to.signed() {
            Ok(format!("@as({target}, @bitCast({mid}))"))
        } else {
            Ok(mid)
        }
    }

    fn call(&mut self, func: Func, args: &[Expr]) -> BpsResult<String> {
        match func {
            Func::SignExtend => {
                if args.len() != 2 {
                    return Err(emit("zig: sign_extend arity".to_string()));
                }
                // A cast whose operand is already `u64` is skipped: the
                // value feeds the `u64` parameter directly.
                let mut val = &args[0];
                if let Expr::Cast { expr: core, .. } = val {
                    let (cty, _) = self.printer.prog.expr_ty(self.body, core);
                    if cty == Ty::Int(IntTy::U64) {
                        val = core;
                    }
                }
                let a = self.expr(val)?;
                let b = self.expr(&args[1])?;
                // The `u64` input reinterprets bits (`@bitCast`, never a
                // trapping `@intCast`); the width is a plain literal.
                let (aty, _) = self.printer.prog.expr_ty(self.body, val);
                let av = match aty {
                    Ty::Int(IntTy::U64) => a,
                    Ty::Int(t) if !t.signed() => format!("@intCast({a})"),
                    Ty::Int(IntTy::I64) => format!("@bitCast({a})"),
                    Ty::Int(_) => format!("@bitCast(@as(i64, @intCast({a})))"),
                    _ if matches!(val, Expr::Int { .. }) => a,
                    _ => format!("@intCast({a})"),
                };
                let bv = match &args[1] {
                    Expr::Int { .. } => b,
                    _ => format!("@intCast({b})"),
                };
                Ok(format!("bml_rt.signExtend({av}, {bv})"))
            }
            Func::Utf16Units => {
                if args.len() != 1 {
                    return Err(emit("zig: utf16_units arity".to_string()));
                }
                let s = self.expr(&args[0])?;
                Ok(format!("@as(i64, @intCast(bml_rt.utf16Units({s})))"))
            }
            Func::Utf8Len => {
                if args.len() != 1 {
                    return Err(emit("zig: utf8_len arity".to_string()));
                }
                let s = self.expr(&args[0])?;
                Ok(format!("@as(i64, @intCast(({s}).len))"))
            }
            Func::StrLen => {
                if args.len() != 1 {
                    return Err(emit("zig: str_len arity".to_string()));
                }
                let s = self.expr(&args[0])?;
                Ok(format!("bml_rt.strLen({s})"))
            }
            Func::BytesLen | Func::ListLen => {
                if args.len() != 1 {
                    return Err(emit("zig: len arity".to_string()));
                }
                let s = self.expr(&args[0])?;
                Ok(format!("@as(i64, @intCast(({s}).len))"))
            }
            Func::Min => {
                if args.len() != 2 {
                    return Err(emit("zig: min arity".to_string()));
                }
                Ok(format!(
                    "@min({}, {})",
                    self.expr(&args[0])?,
                    self.expr(&args[1])?
                ))
            }
            Func::Max => {
                if args.len() != 2 {
                    return Err(emit("zig: max arity".to_string()));
                }
                Ok(format!(
                    "@max({}, {})",
                    self.expr(&args[0])?,
                    self.expr(&args[1])?
                ))
            }
        }
    }
}

/// Break `|`/`&`/`^` chains past this width (statement indent included).
const MAX_WIDTH: usize = 100;
/// Lowercase hex with `_` every four digits (`0x80`, `0x0f`, `0xffff_ffff`).
fn zig_hex(value: i64) -> String {
    debug_assert!(value > 9);
    let mut digits = format!("{value:x}");
    // Whole bytes read best (`0x0f`, not `0xf`).
    if digits.len() % 2 == 1 {
        digits.insert(0, '0');
    }
    if digits.len() <= 4 {
        return format!("0x{digits}");
    }
    let mut out = String::from("0x");
    let first = digits.len() % 4;
    let (head, tail) = digits.split_at(first);
    out.push_str(head);
    for (i, chunk) in tail.as_bytes().chunks(4).enumerate() {
        if i > 0 || !head.is_empty() {
            out.push('_');
        }
        out.push_str(std::str::from_utf8(chunk).unwrap_or("?"));
    }
    out
}

/// Which side of its parent an operand sits on (for non-associative ops).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// Zig precedence, tightest last (`&` binds tighter than `!=`, shifts sit
/// between addition and `&`; `@divFloor`/`@mod` are calls, hence atomic).
fn bin_prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        BinOp::BitOr => 4,
        BinOp::BitXor => 5,
        BinOp::BitAnd => 6,
        BinOp::Shl | BinOp::Shr => 7,
        BinOp::Add | BinOp::Sub => 8,
        BinOp::Mul => 9,
        BinOp::Div | BinOp::Mod => 10,
    }
}

/// Whether a binary child needs a pair of parentheses inside `parent`.
fn need_parens(child: &Expr, parent: BinOp, side: Side) -> bool {
    use BinOp::*;
    if matches!(parent, Div | Mod) {
        return false;
    }
    let Expr::Binary { op: c, .. } = child else {
        return false;
    };
    let (cp, pp) = (bin_prec(*c), bin_prec(parent));
    if cp != pp {
        return cp < pp;
    }
    // Same precedence: associative ops (`+`, `*`, `&`, `|`, `^`, `and`,
    // `or`) need none; the rest regroup on the right.
    match parent {
        Add | Mul | BitAnd | BitOr | BitXor | And | Or => false,
        Sub | Shl | Shr => side == Side::Right,
        _ => true,
    }
}

fn unsigned_ty(ty: IntTy) -> &'static str {
    match ty {
        IntTy::I8 | IntTy::U8 => "u8",
        IntTy::I16 | IntTy::U16 => "u16",
        IntTy::I32 | IntTy::U32 => "u32",
        IntTy::I64 | IntTy::U64 => "u64",
        IntTy::I128 => "u128",
    }
}

fn signed_ty(ty: IntTy) -> &'static str {
    match ty {
        IntTy::I8 | IntTy::U8 => "i8",
        IntTy::I16 | IntTy::U16 => "i16",
        IntTy::I32 | IntTy::U32 => "i32",
        IntTy::I64 | IntTy::U64 => "i64",
        IntTy::I128 => "i128",
    }
}

/// Whether any statement calls `str_len` (decides the helper's emission).
fn body_uses_strlen(stmts: &[Stmt]) -> bool {
    let mut found = false;
    walk_stmts(stmts, &mut |s| {
        let exprs: Vec<&Expr> = match s {
            Stmt::Let { value, .. } | Stmt::Return(value) => vec![value],
            Stmt::Require { cond, .. } => vec![cond],
            Stmt::If { cond, .. } => vec![cond],
            Stmt::Match { scrutinee, .. } => vec![scrutinee],
            _ => Vec::new(),
        };
        for e in exprs {
            walk_expr(e, &mut |x| {
                if matches!(
                    x,
                    Expr::Call {
                        func: Func::StrLen,
                        ..
                    }
                ) {
                    found = true;
                }
            });
        }
    });
    found
}
