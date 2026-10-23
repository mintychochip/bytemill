//! `emit rust-typed`: an idiomatic Rust crate generated from the typed program.
//!
//! The entry point [`emit_rust_typed`] builds `bytemill::ir::typed(&resolved)` and
//! prints it as an `edition = "2024"` crate with no dependencies:
//! `Cargo.toml`, `src/lib.rs` (`forbid(unsafe_code)`, re-exports),
//! `src/runtime.rs` (the [`Error`], [`Reader`], [`Writer`] and integer
//! helpers below), `src/model.rs` (shared encodings, enums, and every nested
//! record or sum that is not a packet), and `src/packets/` (one module per
//! route category: `play_clientbound`, `shared`, `encoding`, ...).
//!
//! Every codec is printed from its typed body: scalar and string codecs
//! (`varint`, `string`, `u16be`, ...) become free `decode_*`/`encode_*`
//! functions like any other codec -- there is no runtime special case by
//! name. Fragments (`nbt_payload`) never reach the printer: the typed program
//! gives them no codec and inlines them at their spreaders. Computed fields
//! (`position.x`, `= expr` bindings) are stored fields; encode writes the
//! stored value back. The runtime therefore holds exactly the cursor and
//! integer operations the typed program's [`ReadOp`] and [`WriteOp`] need
//! (`read_u8`, `read_bytes`, `read_f32_be`, `read_f64_be`, `read_f32_le`, `read_f64_le`, `read_rest`,
//! `peek_u8` for `until` loops, and the matching writer operations), plus the
//! integer builtins every [`Func`] call maps to.
//!
//! Mapping notes (the guide `local://idiom-rust.md` decides the idioms, the
//! typed program decides the shapes):
//!
//! * Records are `struct`s, sums are `enum`s, open enums are `enum`s with an
//!   `Unknown(base)` variant kept verbatim. `Alias` is a `pub type` alias and
//!   its codec delegates to the target codec.
//! * Optional fields, locals, and parameters are `Option<T>`; lists are
//!   `Vec<T>`; strings are `String`; bytes are `Vec<u8>`.
//! * `Box<T>` breaks direct size cycles only (a field or variant payload that
//!   reaches its owner without passing through `Vec` or `Option`).
//!   `Copy` is derived when every field is `Copy`; `Eq` when no `f32`/`f64`
//!   occurs anywhere inside.
//! * `Reader` borrows, `Writer` owns, decoded values own (`to_vec`,
//!   `to_owned`). Parameters are passed by value (every schema parameter,
//!   capture, and tag in the shipped schemas is `Copy`).
//! * Encode never copies: an optional field is bound through
//!   `self.field.as_ref().ok_or(..)?` and a sum payload as the reference the
//!   match already has (`v`, or `&**v` when boxed), so a body that only
//!   borrows a value does not clone it. `.clone()` stays exactly where the
//!   body moves the value into an owned place.
//! * Integer arithmetic runs in `i64` two's-complement like the interpreter:
//!   `wraps` additions, subtractions, multiplications, and shifts use
//!   `bml_add`/`bml_sub`/`bml_mul` or `wrapping_*`; `/` floors (`bml_div`)
//!   and `%` takes the divisor's sign (`bml_mod`); plain operators print
//!   where the result cannot overflow. [`Program::int_range`] decides the
//!   `/` and `%` cases. `sign_extend` matches the builtin.
//! * Failures map to [`Error`], naming the failing codec (snake case, with
//!   the list name after a `.` for counted repeats), the reader offset for
//!   short buffers and bad UTF-8, the `require` text, the missing field, and
//!   the unmatched tag.
//! * `until(t)` loops peek: the terminator byte is consumed only when it
//!   matches, mirroring the interpreter.
//! * A trailing `return` is printed bare (`Ok(value)`) so
//!   `clippy::needless_return` stays quiet; guards and non-tail blocks keep
//!   `return`.
//! * Lines are broken rustfmt-canonically when they would run long: call
//!   arguments go one per line with a trailing comma, and long binary chains
//!   split before the top-level operator. Everything else fits on one line.
//!
//! [`Error`]: https://doc.rust-lang.org/std/result/ (emitted `runtime::Error`)
//! [`Reader`]: (emitted `runtime::Reader`)
//! [`Writer`]: (emitted `runtime::Writer`)
//! [`ReadOp`]: crate::ir::typed::ReadOp
//! [`WriteOp`]: crate::ir::typed::WriteOp
//! [`Func`]: crate::ir::typed::Func
//! [`Program::int_range`]: crate::ir::typed::Program::int_range

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::ast::ResolvedSchema;
use crate::errors::{BmlResult, emit};
use crate::ir::typed::{
    BinOp, Body, CodecIdx, CodecKind, Expr, Func, IntTy, LocalId, Origin, Program, ReadOp,
    RepeatMode, Stmt, Ty, TypeIdx, escape, hex_literal, typed,
};

use crate::vectors::{TestRun, Vector};

/// Rust 2024 keywords (lowercase) plus `Self`: any identifier spelling one of
/// these gets a trailing `_`, the one keyword escape every printer uses.
const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];

/// Emit the `rust-typed` crate for `resolved` into `out_dir`.
pub fn emit_rust_typed(resolved: &ResolvedSchema, out_dir: &Path) -> BmlResult<()> {
    emit_rust_typed_with_edition(resolved, out_dir, crate::RustEdition::default())
}

pub fn emit_rust_typed_with_vectors(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    edition: crate::RustEdition,
    vectors: &[Vector],
) -> BmlResult<Option<TestRun>> {
    let program = typed(resolved)?;
    let printer = Gen::new(&program);
    let src = out_dir.join("src");
    let packets = src.join("packets");
    fs::create_dir_all(&packets).map_err(|e| emit(e.to_string()))?;
    let _ = fs::remove_file(src.join("packets.rs"));
    fs::write(out_dir.join("Cargo.toml"), cargo_toml(edition)).map_err(|e| emit(e.to_string()))?;
    fs::write(src.join("lib.rs"), printer.lib_rs()).map_err(|e| emit(e.to_string()))?;
    fs::write(src.join("runtime.rs"), RUNTIME).map_err(|e| emit(e.to_string()))?;
    fs::write(src.join("model.rs"), printer.file_rs(false, None))
        .map_err(|e| emit(e.to_string()))?;
    let stems = printer.packet_stems();
    fs::write(packets.join("mod.rs"), printer.packets_index(&stems))
        .map_err(|e| emit(e.to_string()))?;
    let mut paths = vec![
        src.join("lib.rs"),
        src.join("runtime.rs"),
        src.join("model.rs"),
        packets.join("mod.rs"),
    ];
    for stem in &stems {
        let path = packets.join(format!("{stem}.rs"));
        fs::write(&path, printer.file_rs(true, Some(stem.as_str())))
            .map_err(|e| emit(e.to_string()))?;
        paths.push(path);
    }
    rustfmt_paths(&paths, edition);

    if vectors.is_empty() {
        return Ok(None);
    }

    // An edition 2015 integration test names its crate with `extern crate`; later editions
    // resolve it from the manifest.
    let mut test_src = if edition == crate::RustEdition::Edition2015 {
        String::from("extern crate bml_generated;\nuse bml_generated::*;\n")
    } else {
        String::from("use bml_generated::*;\n")
    };
    let mut n_embed = 0usize;
    for (i, v) in vectors.iter().enumerate() {
        let ci = match program
            .codecs
            .iter()
            .position(|c| matches!(&c.origin, Origin::Packet { name: n, .. } if n == &v.packet))
        {
            Some(c) => c,
            None => continue,
        };
        let dec = decode_fn(&program, ci);
        let enc = encode_fn(&program, ci);
        let bytes = v.bytes()?;
        let list = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let one = format!("    let raw: &[u8] = &[{list}];");
        let raw_stmt = if one.len() <= 100 {
            one
        } else {
            let mut out = String::from("    let raw: &[u8] = &[\n");
            let mut line = String::from("        ");
            for e in bytes.iter().map(|b| b.to_string()) {
                if line.len() + e.len() + 1 > 100 {
                    out.push_str(line.trim_end());
                    out.push('\n');
                    line = String::from("        ");
                }
                line.push_str(&e);
                line.push_str(", ");
            }
            out.push_str(line.trim_end());
            out.push_str("\n    ];");
            out
        };
        let id: String = v
            .packet
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        test_src.push_str(&format!(
            "#[test]\nfn vec_{i}_{id}() {{\n{raw_stmt}\n    let mut r = Reader::new(raw);\n    let v = {dec}(&mut r).unwrap();\n    assert_eq!(r.remaining(), 0);\n    let mut w = Writer::new();\n    v.{enc}(&mut w).unwrap();\n    assert_eq!(w.into_bytes(), raw);\n}}\n"
        ));
        n_embed += 1;
    }
    if n_embed == 0 {
        return Ok(None);
    }

    let tests = out_dir.join("tests");
    fs::create_dir_all(&tests).map_err(|e| emit(e.to_string()))?;
    let test_file = tests.join("vectors.rs");
    fs::write(&test_file, &test_src).map_err(|e| emit(e.to_string()))?;
    // Formatted like every other generated source: the writer's array wrapping only approximates
    // rustfmt's, so the shipped test goes through `rustfmt` when it is installed.
    rustfmt_paths(std::slice::from_ref(&test_file), edition);
    Ok(Some(TestRun {
        file: "tests/vectors.rs".to_string(),
        command: "cargo test --test vectors".to_string(),
        vectors: n_embed,
    }))
}

pub fn emit_rust_typed_with_edition(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    edition: crate::RustEdition,
) -> BmlResult<()> {
    let _ = emit_rust_typed_with_vectors(resolved, out_dir, edition, &[])?;
    Ok(())
}

/// Format the generated sources with `rustfmt` when it is installed, so the
/// output is canonically formatted even where the line writer's breaking
/// differs from `rustfmt`'s (long operator chains, nested calls). The pass
/// is deterministic; without `rustfmt` the writer's output stands as is.
fn rustfmt_paths(paths: &[std::path::PathBuf], edition: crate::RustEdition) {
    for path in paths {
        let Ok(status) = std::process::Command::new("rustfmt")
            .args(["--edition", edition.as_str()])
            .arg(path)
            .status()
        else {
            return;
        };
        if !status.success() {
            return;
        }
    }
}

/// The emitted identifier of a codec's decode function: `Type::decode` for
/// record, sum, and enum codecs, `decode_<codec>` otherwise. Tests resolve
/// names through this instead of grepping the output.
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    let c = &program.codecs[codec];
    if is_method(&c.kind) {
        let ty = codec_type(program, codec);
        format!("{}::decode", esc_type(&program.types[ty].name))
    } else {
        format!("decode_{}", esc_ident(c.name.snake()))
    }
}

/// The emitted identifier of a codec's encode function: `encode` for record,
/// sum, and enum codecs (a method on the value), `encode_<codec>` otherwise.
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    let c = &program.codecs[codec];
    if is_method(&c.kind) {
        "encode".to_string()
    } else {
        format!("encode_{}", esc_ident(c.name.snake()))
    }
}

/// The emitted name of a type.
pub fn type_name_of(program: &Program, ty: TypeIdx) -> String {
    esc_type(&program.types[ty].name)
}

/// Whether a codec prints as methods on its type (record, sum, enum) rather
/// than free functions.
pub fn codec_is_method(program: &Program, codec: CodecIdx) -> bool {
    is_method(&program.codecs[codec].kind)
}

fn is_method(kind: &CodecKind) -> bool {
    matches!(kind, CodecKind::Record | CodecKind::Sum | CodecKind::Enum)
}

/// The type a method codec reads and writes.
fn codec_type(program: &Program, codec: CodecIdx) -> TypeIdx {
    let c = &program.codecs[codec];
    if let Some(ty) = c.ty {
        return ty;
    }
    match &c.value {
        Ty::Named(ty) => *ty,
        _ => panic!("rust-typed: method codec {} has no named type", c.name),
    }
}

/// `PascalCase`, escaping the one name (`Self`) that is special in type
/// position.
fn esc_type(name: &crate::ir::typed::Name) -> String {
    escape(name.pascal(), &["Self"])
}

/// `snake_case` with the Rust keyword escape.
fn esc_ident(snake: String) -> String {
    escape(snake, KEYWORDS)
}

/// Escape `want`, appending `_` until it avoids every name in `taken`.
fn claim(want: String, taken: &mut HashSet<String>) -> String {
    let mut out = want;
    while taken.contains(&out) {
        out.push('_');
    }
    taken.insert(out.clone());
    out
}

fn cargo_toml(edition: crate::RustEdition) -> String {
    format!(
        "[package]\nname = \"bml_generated\"\nversion = \"0.1.0\"\nedition = \"{}\"\n\
         description = \"Generated BML encoders and decoders.\"\n",
        edition.as_str(),
    )
}
/// The emitted `runtime.rs`: [`Error`], [`Reader`], [`Writer`], and the
/// integer helpers. This is exactly the surface the typed program needs:
/// the [`ReadOp`]/[`WriteOp`] cursor operations, `peek_u8` for `until`
/// loops, and one function per integer [`Func`].
///
/// [`Error`]: (emitted `runtime::Error`)
/// [`Reader`]: (emitted `runtime::Reader`)
/// [`Writer`]: (emitted `runtime::Writer`)
/// [`ReadOp`]: crate::ir::typed::ReadOp
/// [`WriteOp`]: crate::ir::typed::WriteOp
/// [`Func`]: crate::ir::typed::Func
const RUNTIME: &str = r#"use std::fmt::{Display, Formatter};

/// Every decode and encode failure. Carries the failing codec and the offset
/// or condition the interpreter would report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Not enough bytes left; `offset` is the cursor when the read failed.
    UnexpectedEof {
        /// Cursor offset of the failed read.
        offset: usize,
    },
    /// A `require` did not hold.
    RequireFailed {
        /// Codec whose `require` failed.
        codec: &'static str,
        /// Condition text from the schema.
        condition: &'static str,
    },
    /// Bytes claimed as `utf8` are not valid UTF-8; `offset` is the start.
    InvalidUtf8 {
        /// Offset of the first byte of the bad slice.
        offset: usize,
    },
    /// A `match` tag has no arm.
    NoMatchArm {
        /// Codec whose `match` failed.
        codec: &'static str,
        /// The unmatched tag.
        tag: i64,
    },
    /// An absent optional field was read outside the block that binds it.
    MissingField {
        /// Schema field name.
        field: &'static str,
    },
    /// Encode was given the wrong sum variant for the arm being written.
    WrongVariant {
        /// Codec being encoded.
        codec: &'static str,
        /// Expected variant name.
        expected: &'static str,
    },
    /// A repeat count or `bytes(n)` length is negative.
    NegativeLength {
        /// Codec being decoded.
        codec: &'static str,
        /// The negative length.
        length: i64,
    },
    /// `bytes(n)` value is longer than `n` on encode.
    BytesTooLong {
        /// Codec being encoded.
        codec: &'static str,
        /// Length of the value.
        len: usize,
        /// Declared length.
        max: usize,
    },
    /// `until-end` cannot be encoded without the frame boundary.
    EncodeUntilEnd {
        /// Codec being encoded.
        codec: &'static str,
    },
    /// No packet is routed at `id` in this state and direction.
    NoRoute {
        /// Protocol state (`play`, `login`, ...).
        state: &'static str,
        /// `clientbound` or `serverbound`.
        direction: &'static str,
        /// Packet id that was not routed.
        id: i128,
    },
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedEof { offset } => {
                write!(f, "unexpected end of buffer at offset {offset}")
            }
            Self::RequireFailed { codec, condition } => {
                write!(f, "{codec}: require failed: {condition}")
            }
            Self::InvalidUtf8 { offset } => {
                write!(f, "invalid UTF-8 at offset {offset}")
            }
            Self::NoMatchArm { codec, tag } => {
                write!(f, "{codec}: no match arm for tag {tag}")
            }
            Self::MissingField { field } => write!(f, "missing optional field `{field}`"),
            Self::WrongVariant { codec, expected } => {
                write!(f, "{codec}: wrong sum variant, expected {expected}")
            }
            Self::NegativeLength { codec, length } => {
                write!(f, "{codec}: negative length {length}")
            }
            Self::BytesTooLong { codec, len, max } => {
                write!(f, "{codec}: bytes too long ({len} > {max})")
            }
            Self::EncodeUntilEnd { codec } => {
                write!(f, "{codec}: cannot encode `until-end` item")
            }
            Self::NoRoute {
                state,
                direction,
                id,
            } => write!(f, "no packet for id {id} in {state} {direction}"),
        }
    }
}

impl std::error::Error for Error {}

/// Byte cursor for decoding.
#[derive(Debug)]
pub struct Reader<'a> {
    /// Whole input buffer.
    buf: &'a [u8],
    /// Next unread index.
    offset: usize,
}

impl<'a> Reader<'a> {
    /// Wrap a buffer.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, offset: 0 }
    }
    /// Current cursor offset.
    pub fn offset(&self) -> usize {
        self.offset
    }
    /// Bytes left to read.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.offset
    }
    /// Read one byte.
    pub fn read_u8(&mut self) -> Result<u8, Error> {
        let b = *self.buf.get(self.offset).ok_or(Error::UnexpectedEof {
            offset: self.offset,
        })?;
        self.offset += 1;
        Ok(b)
    }
    /// Look at the next byte without consuming it.
    pub fn peek_u8(&self) -> Result<u8, Error> {
        self.buf.get(self.offset).copied().ok_or(Error::UnexpectedEof {
            offset: self.offset,
        })
    }
    /// Read exactly `len` bytes.
    pub fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], Error> {
        if self.remaining() < len {
            return Err(Error::UnexpectedEof {
                offset: self.offset,
            });
        }
        let s = &self.buf[self.offset..self.offset + len];
        self.offset += len;
        Ok(s)
    }
    /// Read a big-endian `f32`.
    pub fn read_f32_be(&mut self) -> Result<f32, Error> {
        let mut b = [0; 4];
        for v in &mut b {
            *v = self.read_u8()?;
        }
        Ok(f32::from_bits(u32::from_be_bytes(b)))
    }
    /// Read a big-endian `f64`.
    pub fn read_f64_be(&mut self) -> Result<f64, Error> {
        let mut b = [0; 8];
        for v in &mut b {
            *v = self.read_u8()?;
        }
        Ok(f64::from_bits(u64::from_be_bytes(b)))
    }
    /// Read a little-endian `f32`.
    pub fn read_f32_le(&mut self) -> Result<f32, Error> {
        let mut b = [0; 4];
        for v in &mut b {
            *v = self.read_u8()?;
        }
        Ok(f32::from_bits(u32::from_le_bytes(b)))
    }
    /// Read a little-endian `f64`.
    pub fn read_f64_le(&mut self) -> Result<f64, Error> {
        let mut b = [0; 8];
        for v in &mut b {
            *v = self.read_u8()?;
        }
        Ok(f64::from_bits(u64::from_le_bytes(b)))
    }
    /// Take all remaining bytes (`rest` never fails).
    pub fn read_rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.offset..];
        self.offset = self.buf.len();
        s
    }
}

/// Byte sink for encoding.
#[derive(Debug, Default)]
pub struct Writer {
    /// Bytes written so far.
    buf: Vec<u8>,
}

impl Writer {
    /// Empty sink.
    pub fn new() -> Self {
        Self::default()
    }
    /// Finished bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
    /// Write one byte.
    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    /// Write raw bytes.
    pub fn write_bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
    /// Write `bytes(n)`: fails when longer, zero-pads when shorter.
    pub fn write_bytes_fixed(
        &mut self,
        b: &[u8],
        len: usize,
        codec: &'static str,
    ) -> Result<(), Error> {
        if b.len() > len {
            return Err(Error::BytesTooLong {
                codec,
                len: b.len(),
                max: len,
            });
        }
        self.buf.extend_from_slice(b);
        self.buf.resize(self.buf.len() + (len - b.len()), 0);
        Ok(())
    }
    /// Write a big-endian `f32`.
    pub fn write_f32_be(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    /// Write a big-endian `f64`.
    pub fn write_f64_be(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    /// Write a little-endian `f32`.
    pub fn write_f32_le(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    /// Write a little-endian `f64`.
    pub fn write_f64_le(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }
}

/// Sign-extend the low `bits` of `v`.
pub fn sign_extend(v: i64, bits: u32) -> i64 {
    (v << (64 - bits)) >> (64 - bits)
}

/// UTF-16 code units of a string (for `string(max_chars)` limits).
pub fn utf16_units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Wrapping `+`: every typed-program integer op runs in `i64` two's complement.
pub fn bml_add(a: i64, b: i64) -> i64 {
    a.wrapping_add(b)
}

/// Wrapping `-`.
pub fn bml_sub(a: i64, b: i64) -> i64 {
    a.wrapping_sub(b)
}

/// Wrapping `*`.
pub fn bml_mul(a: i64, b: i64) -> i64 {
    a.wrapping_mul(b)
}

/// Floor `/` (rounds toward negative infinity, unlike Rust `/`).
pub fn bml_div(a: i64, b: i64) -> i64 {
    let q = a.wrapping_div(b);
    let r = a.wrapping_rem(b);
    if r != 0 && (r < 0) != (b < 0) {
        q.wrapping_sub(1)
    } else {
        q
    }
}

/// `%` with the divisor's sign (unlike Rust `%`).
pub fn bml_mod(a: i64, b: i64) -> i64 {
    let r = a.wrapping_rem(b);
    if r != 0 && (r < 0) != (b < 0) {
        r.wrapping_add(b)
    } else {
        r
    }
}

/// Minimum of two `i64` values.
pub fn bml_min(a: i64, b: i64) -> i64 {
    a.min(b)
}

/// Maximum of two `i64` values.
pub fn bml_max(a: i64, b: i64) -> i64 {
    a.max(b)
}
"#;
/// Indented line writer: one `line` call per output line, four spaces per
/// level, so the text already looks like `rustfmt` left it.
#[derive(Default)]
struct W {
    /// Finished text.
    text: String,
    /// Current nesting depth.
    depth: usize,
}

impl W {
    /// Empty writer.
    fn new() -> Self {
        Self {
            text: String::new(),
            depth: 0,
        }
    }

    /// Push one line (empty for a blank line).
    fn line(&mut self, line: &str) {
        if line.is_empty() {
            self.text.push('\n');
            return;
        }
        for _ in 0..self.depth {
            self.text.push_str("    ");
        }
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// Push pre-indented text (a continuation line) verbatim.
    fn raw(&mut self, line: &str) {
        self.text.push_str(line);
        self.text.push('\n');
    }

    /// Indent one level.
    fn ind(&mut self) {
        self.depth += 1;
    }

    /// Outdent one level.
    fn out(&mut self) {
        self.depth -= 1;
    }
}

/// Which other modules an emitted file refers to.
#[derive(Default)]
struct Uses {
    /// Names from `model.rs`.
    model: bool,
    /// Names from the `packets` module (set when `model.rs` names a packet).
    packets: bool,
    /// Names from `runtime.rs`.
    runtime: bool,
    /// Category currently being written. `None` for `model.rs`.
    here_stem: Option<String>,
    /// Other packet-category modules this file names.
    stems: BTreeSet<String>,
}

/// Emission state for one function body: the line writer, module-use
/// tracker, file side, and name/flag tables. Bundling these keeps every
/// `emit_*` helper under the argument limit.
struct Cx<'w, 'u, 'x, 'b> {
    /// Output lines and indentation.
    w: &'w mut W,
    /// Cross-module references.
    uses: &'u mut Uses,
    /// Whether the `model.rs` (`false`) or `packets.rs` (`true`) side emits.
    here: bool,
    /// Names and per-local flags for the body.
    ctx: &'x Ctx<'b>,
}

/// The printer: precomputed names and layout facts over the program.
struct Gen<'a> {
    /// The typed program being printed.
    prog: &'a Program,
    /// Emitted type name per [`TypeIdx`].
    type_name: Vec<String>,
    /// Emitted field name per record type and field.
    field_names: Vec<Vec<String>>,
    /// Emitted variant name per sum type and variant.
    variant_names: Vec<Vec<String>>,
    /// Emitted member name per enum type and member.
    member_names: Vec<Vec<String>>,
    /// Record fields stored behind `Box`.
    boxed_field: HashSet<(TypeIdx, usize)>,
    /// Sum variant payloads stored behind `Box`.
    boxed_variant: HashSet<(TypeIdx, usize)>,
    /// Types that can derive `Copy`.
    is_copy: Vec<bool>,
    /// Types containing an `f32` or `f64` (no `Eq`).
    has_float: Vec<bool>,
    /// Free decode function name per codec (non-method codecs only).
    free_decode: Vec<String>,
    /// Free encode function name per codec (non-method codecs only).
    free_encode: Vec<String>,
}

impl<'a> Gen<'a> {
    /// Precompute every name and layout fact.
    fn new(prog: &'a Program) -> Self {
        let mut taken_types = HashSet::new();
        let type_name = prog
            .types
            .iter()
            .map(|t| claim(esc_type(&t.name), &mut taken_types))
            .collect::<Vec<_>>();
        let mut field_names = vec![Vec::new(); prog.types.len()];
        let mut variant_names = vec![Vec::new(); prog.types.len()];
        let mut member_names = vec![Vec::new(); prog.types.len()];
        for (ti, t) in prog.types.iter().enumerate() {
            match &t.kind {
                crate::ir::typed::TypeKind::Record { fields } => {
                    let mut taken = HashSet::new();
                    for f in fields {
                        field_names[ti].push(claim(esc_ident(f.name.snake()), &mut taken));
                    }
                }
                crate::ir::typed::TypeKind::Sum { variants } => {
                    let mut taken = HashSet::new();
                    for v in variants {
                        variant_names[ti]
                            .push(claim(escape(v.name.pascal(), &["Self"]), &mut taken));
                    }
                }
                crate::ir::typed::TypeKind::Enum { members, .. } => {
                    let mut taken = HashSet::new();
                    // Reserve the catch-all shape before member names.
                    taken.insert("Unknown".to_string());
                    for m in members {
                        member_names[ti]
                            .push(claim(escape(m.name.pascal(), &["Self"]), &mut taken));
                    }
                }
                crate::ir::typed::TypeKind::Alias { .. } => {}
            }
        }
        let edges = direct_edges(prog);
        let mut boxed_field = HashSet::new();
        let mut boxed_variant = HashSet::new();
        for (ti, t) in prog.types.iter().enumerate() {
            match &t.kind {
                crate::ir::typed::TypeKind::Record { fields } => {
                    for (fi, f) in fields.iter().enumerate() {
                        if let Ty::Named(target) = &f.ty
                            && reaches(&edges, *target, ti)
                        {
                            boxed_field.insert((ti, fi));
                        }
                    }
                }
                crate::ir::typed::TypeKind::Sum { variants } => {
                    for (vi, v) in variants.iter().enumerate() {
                        if let Some(Ty::Named(target)) = &v.payload
                            && reaches(&edges, *target, ti)
                        {
                            boxed_variant.insert((ti, vi));
                        }
                    }
                }
                _ => {}
            }
        }
        let is_copy = copy_fixpoint(prog, &boxed_field, &boxed_variant);
        let has_float = float_fixpoint(prog);
        let mut taken_fns = HashSet::new();
        let mut free_decode = vec![String::new(); prog.codecs.len()];
        let mut free_encode = vec![String::new(); prog.codecs.len()];
        for (ci, c) in prog.codecs.iter().enumerate() {
            if !is_method(&c.kind) {
                free_decode[ci] = claim(
                    format!("decode_{}", esc_ident(c.name.snake())),
                    &mut taken_fns,
                );
                free_encode[ci] = claim(
                    format!("encode_{}", esc_ident(c.name.snake())),
                    &mut taken_fns,
                );
            }
        }
        Self {
            prog,
            type_name,
            field_names,
            variant_names,
            member_names,
            boxed_field,
            boxed_variant,
            is_copy,
            has_float,
            free_decode,
            free_encode,
        }
    }

    /// `true` for the `packets.rs` side: packet and frame codecs.
    fn file_of_codec(&self, codec: CodecIdx) -> bool {
        matches!(
            self.prog.codecs[codec].origin,
            Origin::Packet { .. } | Origin::Frame { .. }
        )
    }

    /// The file holding a type follows its codec; orphan types use `model.rs`.
    fn file_of_ty(&self, ty: TypeIdx) -> bool {
        self.prog.types[ty]
            .codec
            .map(|c| self.file_of_codec(c))
            .unwrap_or(false)
    }

    /// Record a reference to the file holding `ty`.
    fn note_ty(&self, uses: &mut Uses, here_packets: bool, ty: TypeIdx) {
        let there = self.file_of_ty(ty);
        if there != here_packets {
            if there {
                uses.packets = true;
            } else {
                uses.model = true;
            }
            return;
        }
        if !here_packets {
            return;
        }
        let Some(here) = uses.here_stem.clone() else {
            return;
        };
        let stem = self
            .prog
            .output_stem(type_origin(self.prog, ty))
            .to_string();
        if stem != here {
            uses.stems.insert(stem);
        }
    }

    /// Record a reference to the file holding `codec`.
    fn note_codec(&self, uses: &mut Uses, here_packets: bool, codec: CodecIdx) {
        let there = self.file_of_codec(codec);
        if there != here_packets {
            if there {
                uses.packets = true;
            } else {
                uses.model = true;
            }
            return;
        }
        if !here_packets {
            return;
        }
        let Some(here) = uses.here_stem.clone() else {
            return;
        };
        let stem = self
            .prog
            .output_stem(&self.prog.codecs[codec].origin)
            .to_string();
        if stem != here {
            uses.stems.insert(stem);
        }
    }

    /// `src/lib.rs`.
    fn lib_rs(&self) -> String {
        let mut w = W::new();
        w.line("//! Generated BML encoders and decoders: typed records for every");
        w.line("//! packet, encoding, and nested block, with `decode`/`encode` pairs");
        w.line("//! that agree byte-for-byte with the interpreter.");
        w.line("#![forbid(unsafe_code)]");
        w.line("#![warn(missing_docs)]");
        w.line("/// Shared encodings, enums, and nested types.");
        w.line("pub mod model;");
        w.line("/// Packets and frames.");
        w.line("pub mod packets;");
        w.line("/// Cursor, error, and integer helpers.");
        w.line("pub mod runtime;");
        w.line("/// Shared encodings, enums, and nested types.");
        w.line("pub use model::*;");
        w.line("/// Packets and frames.");
        w.line("pub use packets::*;");
        w.line("/// Cursor, error, and integer helpers.");
        w.line("pub use runtime::*;");
        w.text
    }

    /// `src/model.rs` when `only_stem` is `None`, otherwise one category
    /// module under `src/packets/`.
    fn file_rs(&self, packets_side: bool, only_stem: Option<&str>) -> String {
        let mut uses = Uses {
            here_stem: only_stem.map(str::to_string),
            ..Uses::default()
        };
        let mut defs = W::new();
        let mut items = W::new();
        let mut first_def = true;
        for ti in 0..self.prog.types.len() {
            if self.file_of_ty(ti) == packets_side
                && stem_selected(self.prog, type_origin(self.prog, ti), only_stem)
            {
                if !first_def {
                    defs.line("");
                }
                first_def = false;
                self.emit_typedef(&mut defs, &mut uses, packets_side, ti);
            }
        }
        let mut first_item = true;
        for (ci, c) in self.prog.codecs.iter().enumerate() {
            if self.file_of_codec(ci) == packets_side
                && stem_selected(self.prog, &c.origin, only_stem)
            {
                if !first_item {
                    items.line("");
                }
                first_item = false;
                self.emit_codec(&mut items, &mut uses, packets_side, ci);
            }
        }
        if let Some(stem) = only_stem {
            self.emit_route_enums(&mut items, &mut uses, stem);
        }
        let up = if only_stem.is_some() {
            "super::super"
        } else {
            "super"
        };
        let mut w = W::new();
        let mut imported = false;
        if uses.model && packets_side {
            w.line(&format!("use {up}::model::*;"));
            imported = true;
        }
        if uses.packets && !packets_side {
            w.line("use super::packets::*;");
            imported = true;
        }
        for stem in &uses.stems {
            w.line(&format!("use super::{stem}::*;"));
            imported = true;
        }
        if uses.runtime {
            w.line(&format!("use {up}::runtime::*;"));
            imported = true;
        }
        if imported {
            w.line("");
        }
        w.text.push_str(&defs.text);
        if !defs.text.is_empty() && !items.text.is_empty() {
            w.line("");
        }
        w.text.push_str(&items.text);
        if w.text.is_empty() {
            w.line(if packets_side {
                "// This schema defines no packets."
            } else {
                "// This schema defines no shared encodings."
            });
        }
        w.text
    }

    /// A typed enum of every packet routed in this category. `decode` returns
    /// the packet struct or [`Error::NoRoute`]. Shared packets live in their
    /// own module; this enum names them and records the import.
    fn emit_route_enums(&self, w: &mut W, uses: &mut Uses, stem: &str) {
        let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for (i, route) in self.prog.routes.iter().enumerate() {
            let key = format!(
                "{}_{}",
                crate::ir::typed::file_token(&route.state),
                crate::ir::typed::file_token(&route.direction)
            );
            if key == stem {
                groups
                    .entry((route.state.clone(), route.direction.clone()))
                    .or_default()
                    .push(i);
            }
        }
        for ((state, direction), mut idxs) in groups {
            idxs.sort_by_key(|i| (self.prog.routes[*i].id, *i));
            let mut seen = BTreeSet::new();
            let mut arms: Vec<(i128, String, String, String)> = Vec::new();
            for i in idxs {
                let route = &self.prog.routes[i];
                let ci = route.packet;
                let codec = &self.prog.codecs[ci];
                if !codec.params.is_empty() || !is_method(&codec.kind) {
                    continue;
                }
                let Some(ty) = codec.ty else {
                    continue;
                };
                let type_name = self.type_name[ty].clone();
                let mut variant = type_name.clone();
                if !seen.insert(variant.clone()) {
                    variant = format!("{variant}_{}", route.id.unsigned_abs());
                    if !seen.insert(variant.clone()) {
                        variant = format!("{variant}_{ci}");
                        seen.insert(variant.clone());
                    }
                }
                self.note_ty(uses, true, ty);
                let schema = match &codec.origin {
                    Origin::Packet { name, .. } => name.clone(),
                    _ => codec.name.snake().to_string(),
                };
                arms.push((route.id, variant, type_name, schema));
            }
            if arms.is_empty() {
                continue;
            }
            uses.runtime = true;
            if !w.text.is_empty() {
                w.line("");
            }
            let enum_name = pascal_stem(stem);
            let state_lit = rust_str(&state);
            let dir_lit = rust_str(&direction);
            w.line(&format!(
                "/// `{state}` `{direction}` packets. [`{enum_name}::decode`] returns the packet or [`Error::NoRoute`]."
            ));
            w.line("#[allow(clippy::large_enum_variant, reason = \"packet payloads differ in size; boxing every decode would allocate\")]");
            w.line("#[derive(Debug, Clone, PartialEq)]");
            w.line(&format!("pub enum {enum_name} {{"));
            w.ind();
            for (id, variant, type_name, schema) in &arms {
                w.line(&format!("/// `{schema}`, id {id}."));
                w.line(&format!("{variant}({type_name}),"));
            }
            w.out();
            w.line("}");
            w.line("");
            w.line(&format!("impl {enum_name} {{"));
            w.ind();
            w.line("/// Decode the packet body for `id`.");
            w.line("///");
            w.line("/// Returns [`Error::NoRoute`] when nothing is routed at `id`.");
            w.line("pub fn decode(id: i128, r: &mut Reader<'_>) -> Result<Self, Error> {");
            w.ind();
            w.line("match id {");
            w.ind();
            for (id, variant, type_name, _) in &arms {
                w.line(&format!(
                    "{id} => Ok(Self::{variant}({type_name}::decode(r)?)),"
                ));
            }
            w.line(&format!(
                "_ => Err(Error::NoRoute {{ state: {state_lit}, direction: {dir_lit}, id }}),"
            ));
            w.out();
            w.line("}");
            w.out();
            w.line("}");
            w.line("");
            w.line("/// Packet id of this variant.");
            w.line("pub fn id(&self) -> i128 {");
            w.ind();
            w.line("match self {");
            w.ind();
            for (id, variant, _, _) in &arms {
                w.line(&format!("Self::{variant}(_) => {id},"));
            }
            w.out();
            w.line("}");
            w.out();
            w.line("}");
            w.line("");
            w.line("/// Encode this packet's body.");
            w.line("pub fn encode(&self, w: &mut Writer) -> Result<(), Error> {");
            w.ind();
            w.line("match self {");
            w.ind();
            for (_, variant, _, _) in &arms {
                w.line(&format!("Self::{variant}(v) => v.encode(w),"));
            }
            w.out();
            w.line("}");
            w.out();
            w.line("}");
            w.out();
            w.line("}");
        }
    }

    /// Route categories that hold at least one packet or frame.
    fn packet_stems(&self) -> Vec<String> {
        let mut stems = BTreeSet::new();
        for (ci, c) in self.prog.codecs.iter().enumerate() {
            if self.file_of_codec(ci) {
                stems.insert(self.prog.output_stem(&c.origin).to_string());
            }
        }
        for ti in 0..self.prog.types.len() {
            if self.file_of_ty(ti) {
                stems.insert(
                    self.prog
                        .output_stem(type_origin(self.prog, ti))
                        .to_string(),
                );
            }
        }
        for route in &self.prog.routes {
            stems.insert(format!(
                "{}_{}",
                crate::ir::typed::file_token(&route.state),
                crate::ir::typed::file_token(&route.direction)
            ));
        }
        stems.into_iter().collect()
    }

    /// `src/packets/mod.rs`: one `pub mod` and `pub use` per category.
    fn packets_index(&self, stems: &[String]) -> String {
        if stems.is_empty() {
            return "// This schema defines no packets.\n".to_string();
        }
        let mut w = W::new();
        w.line("//! Packets and frames, one module per route category.");
        for stem in stems {
            w.line(&format!("/// Packets and frames in the `{stem}` category."));
            w.line(&format!("pub mod {stem};"));
        }
        w.line("");
        for stem in stems {
            w.line(&format!("pub use {stem}::*;"));
        }
        w.text
    }
}

/// `true` when `only_stem` is unset or `origin` belongs to that category.
fn stem_selected(prog: &Program, origin: &Origin, only_stem: Option<&str>) -> bool {
    match only_stem {
        None => true,
        Some(stem) => prog.output_stem(origin) == stem,
    }
}

/// The origin that decides a type's category file: its codec when it has one
/// (the struct and its impl must land in the same module), otherwise its own.
fn type_origin(prog: &Program, ty: TypeIdx) -> &Origin {
    match prog.types[ty].codec {
        Some(codec) => &prog.codecs[codec].origin,
        None => &prog.types[ty].origin,
    }
}

/// `play_clientbound` to `PlayClientbound`.
fn pascal_stem(stem: &str) -> String {
    let mut out = String::new();
    let mut cap = true;
    for c in stem.chars() {
        if c == '_' {
            cap = true;
            continue;
        }
        if cap {
            out.extend(c.to_uppercase());
            cap = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push_str("Packets");
    }
    if KEYWORDS.contains(&out.as_str()) {
        out.push('_');
    }
    out
}

/// A Rust string literal.
fn rust_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Direct size edges `owner -> target`: record fields and sum payloads that
/// hold a named type without passing through `Vec` (`Option` is no size
/// indirection; only `Vec` and `Box` break a size cycle).
fn direct_edges(prog: &Program) -> Vec<Vec<TypeIdx>> {
    let mut edges = vec![Vec::new(); prog.types.len()];
    for (ti, t) in prog.types.iter().enumerate() {
        match &t.kind {
            crate::ir::typed::TypeKind::Record { fields } => {
                for f in fields {
                    if let Ty::Named(target) = &f.ty {
                        edges[ti].push(*target);
                    }
                }
            }
            crate::ir::typed::TypeKind::Sum { variants } => {
                for v in variants {
                    if let Some(Ty::Named(target)) = &v.payload {
                        edges[ti].push(*target);
                    }
                }
            }
            crate::ir::typed::TypeKind::Alias { target } => {
                edges[ti].push(*target);
            }
            crate::ir::typed::TypeKind::Enum { .. } => {}
        }
    }
    edges
}

/// Whether `from` reaches `to` along direct edges (`from == to` counts).
fn reaches(edges: &[Vec<TypeIdx>], from: TypeIdx, to: TypeIdx) -> bool {
    let mut seen = vec![false; edges.len()];
    let mut stack = vec![from];
    while let Some(cur) = stack.pop() {
        if cur == to {
            return true;
        }
        if seen[cur] {
            continue;
        }
        seen[cur] = true;
        stack.extend_from_slice(&edges[cur]);
    }
    false
}

/// Least fixpoint starting from "not `Copy`", so reference cycles stay
/// `false`. A named field counts only when it is not boxed.
fn copy_fixpoint(
    prog: &Program,
    boxed_field: &HashSet<(TypeIdx, usize)>,
    boxed_variant: &HashSet<(TypeIdx, usize)>,
) -> Vec<bool> {
    let mut copy = vec![false; prog.types.len()];
    loop {
        let mut changed = false;
        for (ti, t) in prog.types.iter().enumerate() {
            let yes = match &t.kind {
                crate::ir::typed::TypeKind::Record { fields } => fields
                    .iter()
                    .enumerate()
                    .all(|(fi, f)| !boxed_field.contains(&(ti, fi)) && copy_ty(&f.ty, &copy)),
                crate::ir::typed::TypeKind::Sum { variants } => {
                    variants.iter().enumerate().all(|(vi, v)| match &v.payload {
                        None => true,
                        Some(ty) => !boxed_variant.contains(&(ti, vi)) && copy_ty(ty, &copy),
                    })
                }
                crate::ir::typed::TypeKind::Enum { .. } => true,
                crate::ir::typed::TypeKind::Alias { target } => copy[*target],
            };
            if yes && !copy[ti] {
                copy[ti] = true;
                changed = true;
            }
        }
        if !changed {
            return copy;
        }
    }
}

/// Whether a value type is `Copy` under the current fixpoint.
fn copy_ty(ty: &Ty, copy: &[bool]) -> bool {
    match ty {
        Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => true,
        Ty::Str | Ty::Bytes | Ty::List(_) => false,
        Ty::Named(t) => copy[*t],
    }
}

/// Least fixpoint starting from "no float".
fn float_fixpoint(prog: &Program) -> Vec<bool> {
    let mut has = vec![false; prog.types.len()];
    loop {
        let mut changed = false;
        for (ti, t) in prog.types.iter().enumerate() {
            let yes = match &t.kind {
                crate::ir::typed::TypeKind::Record { fields } => {
                    fields.iter().any(|f| float_ty(&f.ty, &has))
                }
                crate::ir::typed::TypeKind::Sum { variants } => variants
                    .iter()
                    .any(|v| v.payload.as_ref().is_some_and(|t| float_ty(t, &has))),
                crate::ir::typed::TypeKind::Enum { .. } => false,
                crate::ir::typed::TypeKind::Alias { target } => has[*target],
            };
            if yes && !has[ti] {
                has[ti] = true;
                changed = true;
            }
        }
        if !changed {
            return has;
        }
    }
}

/// Whether a value type contains an `f32` or `f64`.
fn float_ty(ty: &Ty, has: &[bool]) -> bool {
    match ty {
        Ty::F32 | Ty::F64 => true,
        Ty::Int(_) | Ty::Bool | Ty::Str | Ty::Bytes => false,
        Ty::List(inner) => float_ty(inner, has),
        Ty::Named(t) => has[*t],
    }
}
impl<'a> Gen<'a> {
    /// Integer storage name.
    fn int_name(ty: IntTy) -> &'static str {
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

    /// Rust spelling of a value type (`opt` wraps `Option<..>`).
    fn rust_ty(&self, ty: &Ty, opt: bool) -> String {
        let base = match ty {
            Ty::Int(t) => Self::int_name(*t).to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::F32 => "f32".to_string(),
            Ty::F64 => "f64".to_string(),
            Ty::Str => "String".to_string(),
            Ty::Bytes => "Vec<u8>".to_string(),
            Ty::List(inner) => format!("Vec<{}>", self.rust_ty(inner, false)),
            Ty::Named(t) => self.type_name[*t].clone(),
        };
        if opt { format!("Option<{base}>") } else { base }
    }

    /// Record every named type `ty` mentions that lives in the other file.
    fn note_ty_tree(&self, uses: &mut Uses, here_packets: bool, ty: &Ty) {
        match ty {
            Ty::List(inner) => self.note_ty_tree(uses, here_packets, inner),
            Ty::Named(t) => self.note_ty(uses, here_packets, *t),
            _ => {}
        }
    }

    /// `#[derive(..)]` for a type: always `Debug, Clone, PartialEq`, plus
    /// `Copy` when everything inside is `Copy` and `Eq` when no float occurs.
    fn derive_of(&self, ty: TypeIdx) -> String {
        let mut out = String::from("#[derive(Debug, Clone");
        if self.is_copy[ty] {
            out.push_str(", Copy");
        }
        out.push_str(", PartialEq");
        if !self.has_float[ty] {
            out.push_str(", Eq");
        }
        out.push_str(")]");
        out
    }

    /// Doc comment for a type or codec origin.
    fn origin_doc(&self, origin: &Origin) -> String {
        match origin {
            Origin::Packet { name, routes } => {
                if routes.is_empty() {
                    format!("/// Packet `{name}`.")
                } else {
                    let list = routes
                        .iter()
                        .map(|r| format!("{}.{} 0x{:02x}", r.state, r.direction, r.id))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("/// Packet `{name}` ({list}).")
                }
            }
            Origin::Encoding { name } => format!("/// Encoding `{name}`."),
            Origin::Enum { name } => {
                format!("/// Open enum `{name}`: decode keeps unknown values.")
            }
            Origin::Frame { name } => format!("/// Frame `{name}`."),
            Origin::Field { parent, field } => {
                format!("/// Nested `{field}` of `{}`.", self.type_name[*parent])
            }
            Origin::Variant { sum, variant } => {
                format!(
                    "/// Block arm `{}` of sum `{}`.",
                    self.variant_names[*sum][*variant], self.type_name[*sum]
                )
            }
        }
    }

    /// Doc comment for a codec's `Decode`/`Encode` side.
    fn codec_doc(&self, codec: CodecIdx, what: &str) -> String {
        let c = &self.prog.codecs[codec];
        match &c.origin {
            Origin::Packet { name, .. } => format!("/// {what} packet `{name}`."),
            Origin::Encoding { name } => format!("/// {what} encoding `{name}`."),
            Origin::Enum { name } => format!("/// {what} open enum `{name}`."),
            Origin::Frame { name } => format!("/// {what} frame `{name}`."),
            Origin::Field { parent, field } => {
                format!(
                    "/// {what} nested `{field}` of `{}`.",
                    self.type_name[*parent]
                )
            }
            Origin::Variant { sum, variant } => {
                format!(
                    "/// {what} arm `{}` of sum `{}`.",
                    self.variant_names[*sum][*variant], self.type_name[*sum]
                )
            }
        }
    }

    /// One type definition.
    fn emit_typedef(&self, w: &mut W, uses: &mut Uses, here_packets: bool, ti: TypeIdx) {
        let t = &self.prog.types[ti];
        w.line(&self.origin_doc(&t.origin));
        match &t.kind {
            crate::ir::typed::TypeKind::Record { fields } => {
                w.line(&self.derive_of(ti));
                if fields.is_empty() {
                    w.line(&format!("pub struct {} {{}}", self.type_name[ti]));
                } else {
                    w.line(&format!("pub struct {} {{", self.type_name[ti]));
                    w.ind();
                    for (fi, f) in fields.iter().enumerate() {
                        let mut ty = self.rust_ty(&f.ty, false);
                        self.note_ty_tree(uses, here_packets, &f.ty);
                        if self.boxed_field.contains(&(ti, fi)) {
                            ty = format!("Box<{ty}>");
                        }
                        if f.optional {
                            ty = format!("Option<{ty}>");
                        }
                        if f.optional {
                            w.line(&format!(
                                "/// `{}` (optional; `None` when absent on the wire): `{ty}`.",
                                f.ir
                            ));
                        } else {
                            w.line(&format!("/// `{}`: `{ty}`.", f.ir));
                        }
                        w.line(&format!("pub {}: {ty},", self.field_names[ti][fi]));
                    }
                    w.out();
                    w.line("}");
                }
            }
            crate::ir::typed::TypeKind::Sum { variants } => {
                w.line(&self.derive_of(ti));
                w.line(&format!("pub enum {} {{", self.type_name[ti]));
                w.ind();
                for (vi, v) in variants.iter().enumerate() {
                    w.line(&self.variant_doc(ti, vi));
                    let name = &self.variant_names[ti][vi];
                    match &v.payload {
                        None => w.line(&format!("{name},")),
                        Some(payload) => {
                            let mut ty = self.rust_ty(payload, false);
                            self.note_ty_tree(uses, here_packets, payload);
                            if self.boxed_variant.contains(&(ti, vi)) {
                                ty = format!("Box<{ty}>");
                            }
                            w.line(&format!("{name}({ty}),"));
                        }
                    }
                }
                w.out();
                w.line("}");
            }
            crate::ir::typed::TypeKind::Enum { base, members } => {
                w.line(&self.derive_of(ti));
                w.line(&format!("pub enum {} {{", self.type_name[ti]));
                w.ind();
                for (mi, m) in members.iter().enumerate() {
                    w.line(&format!("/// `{}` ({}).", m.name, m.value));
                    w.line(&format!("{},", self.member_names[ti][mi]));
                }
                w.line("/// Any other value, preserved verbatim.");
                w.line(&format!("Unknown({}),", Self::int_name(*base)));
                w.out();
                w.line("}");
            }
            crate::ir::typed::TypeKind::Alias { target } => {
                self.note_ty(uses, here_packets, *target);
                w.line(&format!(
                    "pub type {} = {};",
                    self.type_name[ti], self.type_name[*target]
                ));
            }
        }
    }

    /// Doc comment for one sum variant: its tags and payload type.
    fn variant_doc(&self, ty: TypeIdx, vi: usize) -> String {
        let v = match &self.prog.types[ty].kind {
            crate::ir::typed::TypeKind::Sum { variants } => &variants[vi],
            _ => panic!("rust-typed: variant doc for a non-sum"),
        };
        if v.tags.is_empty() {
            return "/// Catch-all arm (`_`).".to_string();
        }
        let tags = v
            .tags
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        match &v.payload {
            None => format!("/// Arm `{tags}`."),
            Some(payload) => format!("/// `{tags}` => `{}`.", self.rust_ty(payload, false)),
        }
    }

    /// One codec: an `impl` block for method codecs, free functions otherwise.
    fn emit_codec(&self, w: &mut W, uses: &mut Uses, here_packets: bool, ci: CodecIdx) {
        let kind = self.prog.codecs[ci].kind;
        if is_method(&kind) {
            let ty = codec_type(self.prog, ci);
            w.line(&format!("impl {} {{", self.type_name[ty]));
            w.ind();
            if kind == CodecKind::Enum {
                self.emit_enum_conv(w, ci, ty);
                w.line("");
            }
            self.emit_decode(w, uses, here_packets, ci, Some(ty));
            w.line("");
            self.emit_encode(w, uses, here_packets, ci, Some(ty));
            w.out();
            w.line("}");
        } else {
            self.emit_decode(w, uses, here_packets, ci, None);
            w.line("");
            self.emit_encode(w, uses, here_packets, ci, None);
        }
    }

    /// `from_<base>`/`to_<base>` for an open enum.
    fn emit_enum_conv(&self, w: &mut W, ci: CodecIdx, ty: TypeIdx) {
        let (base, members) = match &self.prog.types[ty].kind {
            crate::ir::typed::TypeKind::Enum { base, members } => (*base, members.clone()),
            _ => panic!("rust-typed: enum conversion for a non-enum"),
        };
        let base_name = Self::int_name(base);
        let type_name = &self.type_name[ty];
        let _ = ci;
        w.line("/// Wrap a raw value.");
        w.line(&format!(
            "pub fn from_{base_name}(v: {base_name}) -> Self {{"
        ));
        w.ind();
        w.line("match v {");
        w.ind();
        for (mi, m) in members.iter().enumerate() {
            w.line(&format!(
                "{} => Self::{},",
                grouped_lit(m.value),
                self.member_names[ty][mi]
            ));
        }
        w.line("other => Self::Unknown(other),");
        w.out();
        w.line("}");
        w.out();
        w.line("}");
        w.line("/// Raw value.");
        w.line(&format!("pub fn to_{base_name}(self) -> {base_name} {{"));
        w.ind();
        w.line("match self {");
        w.ind();
        for (mi, m) in members.iter().enumerate() {
            w.line(&format!(
                "Self::{} => {},",
                self.member_names[ty][mi],
                grouped_lit(m.value)
            ));
        }
        w.line("Self::Unknown(v) => v,");
        w.out();
        w.line("}");
        w.out();
        w.line("}");
        let _ = type_name;
    }
}

/// Decimal literal with `_` every three digits once it runs long, so
/// `clippy::unreadable_literal` stays quiet. Unsuffixed: callers add the type.
fn grouped_lit(value: i128) -> String {
    let neg = value < 0;
    let mut digits = value.unsigned_abs().to_string();
    if digits.len() >= 5 {
        let mut out = String::new();
        while digits.len() > 3 {
            out = format!("_{}", &digits[digits.len() - 3..]) + &out;
            digits.truncate(digits.len() - 3);
        }
        digits.push_str(&out);
    }
    if neg { format!("-{digits}") } else { digits }
}
/// Decode or encode side of a codec body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Building a value from the reader.
    Decode,
    /// Writing a value to the writer.
    Encode,
}

/// Per-function printing state: printable local names, loop-introduced
/// locals (whose `Declare` is skipped), and locals needing `mut`.
struct Ctx<'b> {
    /// The body being printed.
    body: &'b Body,
    /// Codec snake name for error payloads.
    snake: String,
    /// Decode or encode.
    mode: Mode,
    /// Printable name per [`LocalId`].
    locals: Vec<String>,
    /// Locals introduced by loop constructs (no `Declare` emitted).
    intro: HashSet<LocalId>,
    /// Locals requiring `mut`.
    mutable: HashSet<LocalId>,
    /// Zero-initialized locals whose initial value can be observed.
    live: HashSet<LocalId>,
    /// Deferred locals with exactly one store: the store declares them.
    merge: HashSet<LocalId>,
    /// `for` loop items (borrowed unless their element is `Copy`).
    for_item: HashSet<LocalId>,
}

impl<'b> Ctx<'b> {
    /// Build the context for one codec side.
    fn new(prog: &'b Program, codec: CodecIdx, mode: Mode, method: bool) -> Self {
        let body = match mode {
            Mode::Decode => &prog.codecs[codec].decode,
            Mode::Encode => &prog.codecs[codec].encode,
        };
        let snake = prog.codecs[codec].name.snake();
        let mut taken = HashSet::new();
        // The cursor params are fixed (`r` on decode, `w` on encode); no
        // local may take those names.
        taken.insert("r".to_string());
        taken.insert("w".to_string());
        let mut locals = vec![String::new(); body.locals.len()];
        if mode == Mode::Encode
            && let Some(value) = body.value
        {
            if method {
                locals[value] = "self".to_string();
                taken.insert("self".to_string());
            } else {
                locals[value] = claim("value".to_string(), &mut taken);
            }
        }
        for (li, local) in body.locals.iter().enumerate() {
            if locals[li].is_empty() {
                locals[li] = claim(esc_ident(local.name.snake()), &mut taken);
            }
        }
        // Unread parameters and values take an underscore: no schema name
        // renders with a leading one, so this cannot collide.
        for &p in &body.params {
            if body.locals[p].uses == 0 {
                let name = std::mem::take(&mut locals[p]);
                locals[p] = format!("_{name}");
            }
        }
        if mode == Mode::Encode
            && !method
            && let Some(value) = body.value
            && body.locals[value].uses == 0
        {
            let name = std::mem::take(&mut locals[value]);
            locals[value] = format!("_{name}");
        }
        let intro = intro_locals(body);
        let (mutable, live, merge) = mut_locals(body, &intro);
        let mut for_item = HashSet::new();
        walk_stmts(&body.stmts, &mut |s| {
            if let Stmt::ForEach { item, .. } = s {
                for_item.insert(*item);
            }
        });
        Self {
            body,
            snake,
            mode,
            locals,
            intro,
            mutable,
            live,
            merge,
            for_item,
        }
    }

    /// Printable name of a local.
    fn local(&self, id: LocalId) -> &str {
        &self.locals[id]
    }

    /// Whether a store declares its destination: the builder flag, or a
    /// merged single-store deferred declaration.
    fn declares(&self, dst: &crate::ir::typed::Bind) -> bool {
        dst.declare || self.merge.contains(&dst.local)
    }

    /// `mut` for a declaring store. Merged singles never reassign.
    fn decl_mut(&self, dst: &crate::ir::typed::Bind) -> bool {
        dst.declare && self.mutable.contains(&dst.local)
    }
}

/// Locals introduced by loop constructs: every repeat list, every `for`
/// item, and every repeat item the body binds with `declare`.
fn intro_locals(body: &Body) -> HashSet<LocalId> {
    let mut intro = HashSet::new();
    walk_stmts(&body.stmts, &mut |s| {
        if let Stmt::Repeat {
            list, item, body, ..
        } = s
        {
            intro.insert(*list);
            if binds_declare(body, *item) {
                intro.insert(*item);
            }
        }
        if let Stmt::ForEach { item, .. } = s {
            intro.insert(*item);
        }
    });
    intro
}

/// Whether any binding of `local` in `stmts` declares it.
fn binds_declare(stmts: &[Stmt], local: LocalId) -> bool {
    let mut found = false;
    walk_stmts(stmts, &mut |s| {
        let (id, declare) = match s {
            Stmt::Let { dst, .. }
            | Stmt::Read { dst, .. }
            | Stmt::Decode { dst, .. }
            | Stmt::Utf8 { dst, .. }
            | Stmt::Unwrap { dst, .. } => (dst.local, dst.declare),
            Stmt::ExpectVariant { dst: Some(d), .. } => (d.local, d.declare),
            _ => return,
        };
        if id == local && declare {
            found = true;
        }
    });
    found
}

/// Every use of a local in a body, with `owned` true where the emitted shape
/// needs the local's value (a move, or a read that does not accept a
/// reference). Only the positions that are reference-transparent -- the
/// source of a further `Unwrap`, the scrutinee of an `ExpectVariant`, the
/// receiver of a method call, a `&`-argument, a field or method base -- pass
/// `false`, so a binding declared as a reference stays usable exactly there.
fn walk_uses(stmts: &[Stmt], f: &mut impl FnMut(LocalId, bool)) {
    for s in stmts {
        match s {
            Stmt::Declare { .. } | Stmt::Fail(_) => {}
            Stmt::Let { value, .. } | Stmt::Return(value) => use_expr(value, true, f),
            Stmt::Read { op, .. } => {
                if let ReadOp::Bytes(n) = op {
                    use_expr(n, true, f);
                }
            }
            Stmt::Decode { args, .. } => {
                for a in args {
                    use_expr(a, true, f);
                }
            }
            Stmt::Utf8 { src, .. } => use_expr(src, false, f),
            Stmt::Write { op, value } => {
                if let crate::ir::typed::WriteOp::Bytes(n) = op {
                    use_expr(n, true, f);
                }
                use_expr(value, false, f);
            }
            Stmt::Encode { args, value, .. } => {
                use_expr(value, false, f);
                for a in args {
                    use_expr(a, true, f);
                }
            }
            Stmt::Require { cond, .. } => use_expr(cond, true, f),
            // The unwrapped place is only ever read through `.as_ref()`.
            Stmt::Unwrap { src, .. } => use_expr(src, false, f),
            Stmt::Match {
                scrutinee,
                arms,
                other,
                ..
            } => {
                use_expr(scrutinee, true, f);
                for a in arms {
                    walk_uses(&a.body, f);
                }
                if let Some(o) = other {
                    walk_uses(o, f);
                }
            }
            Stmt::Repeat { mode, body, .. } => {
                match mode {
                    RepeatMode::Count(n) | RepeatMode::Until(n) => use_expr(n, true, f),
                    RepeatMode::UntilEnd => {}
                }
                walk_uses(body, f);
            }
            Stmt::ForEach { list, body, .. } => {
                use_expr(list, false, f);
                walk_uses(body, f);
            }
            Stmt::ExpectVariant { sum, .. } => use_expr(sum, false, f),
            Stmt::If {
                cond, then, els, ..
            } => {
                use_expr(cond, true, f);
                walk_uses(then, f);
                walk_uses(els, f);
            }
        }
    }
}

/// Local uses inside one expression; `owned` is the enclosing need, carried
/// down through the reference-transparent forms and forced to `true` by
/// everything whose value is consumed.
fn use_expr(e: &Expr, owned: bool, f: &mut impl FnMut(LocalId, bool)) {
    match e {
        Expr::Local(id) => f(*id, owned),
        // A place expression reads through any number of references.
        Expr::Field { base, .. } => use_expr(base, owned, f),
        // `(x).is_some()` and `(x).as_bytes()` borrow their operand.
        Expr::Present(x) | Expr::StrBytes(x) => use_expr(x, false, f),
        // `(x).clone().unwrap()` moves out of the source.
        Expr::Get(x) | Expr::Not(x) => use_expr(x, true, f),
        Expr::Cast { expr, .. } => use_expr(expr, true, f),
        Expr::Binary { lhs, rhs, .. } => {
            use_expr(lhs, true, f);
            use_expr(rhs, true, f);
        }
        Expr::Cond {
            cond, then, els, ..
        } => {
            use_expr(cond, true, f);
            use_expr(then, true, f);
            use_expr(els, true, f);
        }
        Expr::Call { args, .. } => {
            for a in args {
                use_expr(a, true, f);
            }
        }
        Expr::Construct { fields, .. } => {
            for a in fields {
                use_expr(a, true, f);
            }
        }
        Expr::Variant { payload, .. } => {
            if let Some(p) = payload {
                use_expr(p, true, f);
            }
        }
        Expr::Int { .. } | Expr::Bool(_) | Expr::Str(_) | Expr::Remaining => {}
    }
}

/// Every statement in a body, nested ones included.
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

/// Locals needing `mut`: zero-initialized then stored, stored twice, stored
/// from inside a loop that does not declare them, or repeat lists (pushed).
/// Also returns the live zero-inits whose initial value can be observed, and
/// the deferred single-store locals whose store declares them (merging
/// silences `needless_late_init`; the single store dominates every read
/// exactly when dropping the initializer compiles, so the merge is sound).
fn mut_locals(
    body: &Body,
    intro: &HashSet<LocalId>,
) -> (HashSet<LocalId>, HashSet<LocalId>, HashSet<LocalId>) {
    fn decl(stmts: &[Stmt], depth: u32, out: &mut HashMap<LocalId, u32>) {
        for s in stmts {
            match s {
                Stmt::Declare { local, .. } => {
                    out.entry(*local).or_insert(depth);
                }
                Stmt::Let { dst, .. }
                | Stmt::Read { dst, .. }
                | Stmt::Decode { dst, .. }
                | Stmt::Utf8 { dst, .. }
                | Stmt::Unwrap { dst, .. } => {
                    if dst.declare {
                        out.entry(dst.local).or_insert(depth);
                    }
                }
                Stmt::ExpectVariant { dst, .. } => {
                    if let Some(d) = dst
                        && d.declare
                    {
                        out.entry(d.local).or_insert(depth);
                    }
                }
                Stmt::If { then, els, .. } => {
                    decl(then, depth, out);
                    decl(els, depth, out);
                }
                Stmt::Match { arms, other, .. } => {
                    for a in arms {
                        decl(&a.body, depth, out);
                    }
                    if let Some(o) = other {
                        decl(o, depth, out);
                    }
                }
                Stmt::Repeat {
                    list, item, body, ..
                } => {
                    out.entry(*list).or_insert(depth);
                    out.entry(*item).or_insert(depth + 1);
                    decl(body, depth + 1, out);
                }
                Stmt::ForEach { item, body, .. } => {
                    out.entry(*item).or_insert(depth + 1);
                    decl(body, depth + 1, out);
                }
                _ => {}
            }
        }
    }
    fn stores(
        stmts: &[Stmt],
        depth: u32,
        decl: &HashMap<LocalId, u32>,
        inits: &mut HashSet<LocalId>,
        counts: &mut HashMap<LocalId, u32>,
        looping: &mut HashSet<LocalId>,
        nondecl: &mut HashSet<LocalId>,
    ) {
        for s in stmts {
            match s {
                Stmt::Declare { local, zero } => {
                    if *zero {
                        inits.insert(*local);
                    }
                }
                Stmt::Let { dst, .. }
                | Stmt::Read { dst, .. }
                | Stmt::Decode { dst, .. }
                | Stmt::Utf8 { dst, .. }
                | Stmt::Unwrap { dst, .. } => {
                    *counts.entry(dst.local).or_insert(0) += 1;
                    if !dst.declare {
                        nondecl.insert(dst.local);
                    }
                    if depth > decl.get(&dst.local).copied().unwrap_or(0) {
                        looping.insert(dst.local);
                    }
                }
                Stmt::ExpectVariant { dst: Some(d), .. } => {
                    *counts.entry(d.local).or_insert(0) += 1;
                    if depth > decl.get(&d.local).copied().unwrap_or(0) {
                        looping.insert(d.local);
                    }
                }
                Stmt::If { then, els, .. } => {
                    stores(then, depth, decl, inits, counts, looping, nondecl);
                    stores(els, depth, decl, inits, counts, looping, nondecl);
                }
                Stmt::Match { arms, other, .. } => {
                    for a in arms {
                        stores(&a.body, depth, decl, inits, counts, looping, nondecl);
                    }
                    if let Some(o) = other {
                        stores(o, depth, decl, inits, counts, looping, nondecl);
                    }
                }
                Stmt::Repeat { list, body, .. } => {
                    stores(body, depth + 1, decl, inits, counts, looping, nondecl);
                    looping.insert(*list);
                }
                Stmt::ForEach { body, .. } => {
                    stores(body, depth + 1, decl, inits, counts, looping, nondecl);
                }
                _ => {}
            }
        }
    }
    let mut decls = HashMap::new();
    decl(&body.stmts, 0, &mut decls);
    let mut inits = HashSet::new();
    // Optional locals print with `= None`: initialized at declaration.
    for (li, local) in body.locals.iter().enumerate() {
        if local.optional {
            inits.insert(li);
        }
    }
    let mut counts = HashMap::new();
    let mut looping = HashSet::new();
    let mut nondecl = HashSet::new();
    stores(
        &body.stmts,
        0,
        &decls,
        &mut inits,
        &mut counts,
        &mut looping,
        &mut nondecl,
    );
    let live = live_inits(body);
    // Dead zero initializers declare deferred; optional locals always keep
    // `= None`, and their reassignment still needs `mut`.
    inits.retain(|l| body.locals[*l].optional || live.contains(l));
    let mut out = HashSet::new();
    for (local, n) in &counts {
        // Two stores always need `mut`; so does one reassignment of an
        // initialized local (`let x = None; x = v;` cannot reuse an immutable
        // binding, while `let x: T; x = v;` is deferred initialization).
        if *n >= 2 || looping.contains(local) || (nondecl.contains(local) && inits.contains(local))
        {
            out.insert(*local);
        }
    }
    // Repeat lists are pushed; the push needs `mut` even with no stores.
    for l in &looping {
        out.insert(*l);
    }
    // Deferred single-store locals merge their declaration into the store.
    // A zero initializer that survives is load-bearing (its local is read
    // before any store on some path), so merged locals are exactly the
    // declared, non-optional, non-loop-introduced locals stored once whose
    // initializer is dropped.
    let mut declared = HashSet::new();
    let mut zeroed = HashSet::new();
    walk_stmts(&body.stmts, &mut |s| {
        if let Stmt::Declare { local, zero } = s {
            declared.insert(*local);
            if *zero {
                zeroed.insert(*local);
            }
        }
    });
    let merge: HashSet<LocalId> = declared
        .into_iter()
        .filter(|l| {
            !body.locals[*l].optional
                && !intro.contains(l)
                && counts.get(l).copied().unwrap_or(0) == 1
                && !(zeroed.contains(l) && live.contains(l))
        })
        .collect();
    (out, live, merge)
}

/// Whether encode statements always diverge: a trailing `Fail`, or an
/// `if`/`match` whose every path diverges (encode `match` without `other`
/// gains a diverging arm from the printer).
fn diverges_encode(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Fail(_)) => true,
        Some(Stmt::If { then, els, .. }) => diverges_encode(then) && diverges_encode(els),
        Some(Stmt::Match { arms, other, .. }) => {
            !arms.is_empty()
                && arms.iter().all(|a| diverges_encode(&a.body))
                && other.as_ref().is_some_and(|o| diverges_encode(o))
        }
        _ => false,
    }
}
impl<'a> Gen<'a> {
    /// Decode function or method for one codec.
    fn emit_decode(
        &self,
        w: &mut W,
        uses: &mut Uses,
        here_packets: bool,
        ci: CodecIdx,
        ty: Option<TypeIdx>,
    ) {
        let ctx = Ctx::new(self.prog, ci, Mode::Decode, ty.is_some());
        w.line(&self.codec_doc(ci, "Decode"));
        for l in self.decode_sig(&ctx, ci, ty, w.depth) {
            w.line(&l);
        }
        w.ind();
        uses.runtime = true;
        let c = &self.prog.codecs[ci];
        for (pi, p) in c.params.iter().enumerate() {
            self.note_ty_tree(uses, here_packets, &p.ty);
            let _ = pi;
        }
        let mut cx = Cx {
            w: &mut *w,
            uses: &mut *uses,
            here: here_packets,
            ctx: &ctx,
        };
        self.emit_body(&mut cx, &c.decode, true);
        w.out();
        w.line("}");
    }

    /// Encode function or method for one codec.
    fn emit_encode(
        &self,
        w: &mut W,
        uses: &mut Uses,
        here_packets: bool,
        ci: CodecIdx,
        ty: Option<TypeIdx>,
    ) {
        let ctx = Ctx::new(self.prog, ci, Mode::Encode, ty.is_some());
        w.line(&self.codec_doc(ci, "Encode"));
        for l in self.encode_sig(&ctx, ci, ty, w.depth) {
            w.line(&l);
        }
        w.ind();
        uses.runtime = true;
        let c = &self.prog.codecs[ci];
        for p in &c.params {
            self.note_ty_tree(uses, here_packets, &p.ty);
        }
        let mut cx = Cx {
            w: &mut *w,
            uses: &mut *uses,
            here: here_packets,
            ctx: &ctx,
        };
        self.emit_body(&mut cx, &c.encode, false);
        // A diverging body already returns; another `Ok(())` would be
        // unreachable (`unused_must_use`/`unreachable_code`).
        if !diverges_encode(&c.encode.stmts) {
            w.line("Ok(())");
        }
        w.out();
        w.line("}");
    }

    /// Function header lines with the opening brace: one line when it fits,
    /// parameters broken one per line with a trailing comma otherwise.
    fn fn_head(&self, name: &str, parts: &[String], ret: &str, depth: usize) -> Vec<String> {
        let one = format!("pub fn {name}({}) -> {ret} {{", parts.join(", "));
        if depth * 4 + one.len() <= 100 {
            return vec![one];
        }
        let pad = "    ".repeat(depth + 1);
        let mut out = vec![format!("pub fn {name}(")];
        for p in parts {
            out.push(format!("{pad}{p},"));
        }
        out.push(format!("{}) -> {ret} {{", "    ".repeat(depth)));
        out
    }

    /// Decode signature (locals come from the context for parameter names).
    fn decode_sig(
        &self,
        ctx: &Ctx<'_>,
        ci: CodecIdx,
        ty: Option<TypeIdx>,
        depth: usize,
    ) -> Vec<String> {
        let c = &self.prog.codecs[ci];
        let r = if touches_cursor(&c.decode) { "r" } else { "_r" };
        let mut parts = vec![format!("{r}: &mut Reader<'_>")];
        for (pi, p) in c.params.iter().enumerate() {
            let local = c.decode.params[pi];
            parts.push(format!(
                "{}: {}",
                ctx.local(local),
                self.rust_ty(&p.ty, p.optional)
            ));
        }
        let name = match ty {
            Some(_) => "decode".to_string(),
            None => self.free_decode[ci].clone(),
        };
        let ret = match ty {
            Some(_) => "Self".to_string(),
            None => self.rust_ty(&c.value, false),
        };
        self.fn_head(&name, &parts, &format!("Result<{ret}, Error>"), depth)
    }

    /// Encode signature.
    fn encode_sig(
        &self,
        ctx: &Ctx<'_>,
        ci: CodecIdx,
        ty: Option<TypeIdx>,
        depth: usize,
    ) -> Vec<String> {
        let c = &self.prog.codecs[ci];
        let mut parts = Vec::new();
        if ty.is_some() {
            parts.push("&self".to_string());
        }
        let w = if touches_cursor(&c.encode) { "w" } else { "_w" };
        parts.push(format!("{w}: &mut Writer"));
        if ty.is_none() {
            let value = c.encode.value.expect("free encode takes a value");
            parts.push(format!(
                "{}: {}",
                ctx.local(value),
                self.encode_value_ty(&c.value)
            ));
        }
        for (pi, p) in c.params.iter().enumerate() {
            let local = c.encode.params[pi];
            parts.push(format!(
                "{}: {}",
                ctx.local(local),
                self.rust_ty(&p.ty, p.optional)
            ));
        }
        let name = match ty {
            Some(_) => "encode".to_string(),
            None => self.free_encode[ci].clone(),
        };
        self.fn_head(&name, &parts, "Result<(), Error>", depth)
    }

    /// Encode value parameter type: owned when `Copy`, borrowed otherwise.
    fn encode_value_ty(&self, ty: &Ty) -> String {
        match ty {
            Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => self.rust_ty(ty, false),
            Ty::Str => "&str".to_string(),
            Ty::Bytes => "&[u8]".to_string(),
            Ty::List(inner) => format!("&[{}]", self.rust_ty(inner, false)),
            Ty::Named(t) => {
                if self.is_copy[*t] {
                    self.type_name[*t].clone()
                } else {
                    format!("&{}", self.type_name[*t])
                }
            }
        }
    }

    /// All statements of a body; the last one is in tail position when `tail`.
    fn emit_body(&self, cx: &mut Cx<'_, '_, '_, '_>, body: &Body, tail: bool) {
        self.emit_stmts(cx, &body.stmts, tail);
    }

    /// Statements with tail threading for the final one.
    fn emit_stmts(&self, cx: &mut Cx<'_, '_, '_, '_>, stmts: &[Stmt], tail: bool) {
        let mut iter = stmts.iter().peekable();
        while let Some(s) = iter.next() {
            let last = iter.peek().is_none();
            self.emit_stmt(cx, s, tail && last);
        }
    }

    /// One statement.
    fn emit_stmt(&self, cx: &mut Cx<'_, '_, '_, '_>, s: &Stmt, tail: bool) {
        match s {
            Stmt::Declare { local, zero } => self.emit_declare(cx, *local, *zero),
            Stmt::Let { dst, value } => self.emit_let(cx, dst, value),
            Stmt::Read { dst, op } => self.emit_read(cx, dst, op),
            Stmt::Decode { dst, codec, args } => {
                self.emit_decode_call(cx, dst, *codec, args);
            }
            Stmt::Utf8 { dst, src } => self.emit_utf8(cx, dst, src),
            Stmt::Write { op, value } => self.emit_write(cx, op, value),
            Stmt::Encode { codec, args, value } => {
                self.emit_encode_call(cx, *codec, args, value);
            }
            Stmt::Require { cond, text } => self.emit_require(cx, cond, text),
            Stmt::Unwrap { dst, src, field } => self.emit_unwrap(cx, dst, src, field),
            Stmt::If { cond, then, els } => {
                self.emit_if(cx, cond, then, els, tail);
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => {
                self.emit_match(cx, scrutinee, arms, other, tail);
            }
            Stmt::Repeat {
                mode,
                list,
                item,
                body,
            } => {
                self.emit_repeat(cx, mode, *list, *item, body);
            }
            Stmt::ForEach { list, item, body } => {
                self.emit_foreach(cx, list, *item, body);
            }
            Stmt::ExpectVariant {
                dst,
                sum,
                ty,
                variant,
            } => {
                self.emit_expect(cx, dst, sum, *ty, *variant);
            }
            Stmt::Return(e) => self.emit_return(cx, e, tail),
            Stmt::Fail(f) => self.emit_fail(cx, *f, tail),
        }
    }

    /// `let name: Ty [= zero];`, skipped for loop-introduced and merged
    /// (single-store deferred) locals.
    fn emit_declare(&self, cx: &mut Cx<'_, '_, '_, '_>, local: LocalId, zero: bool) {
        if cx.ctx.intro.contains(&local) || cx.ctx.merge.contains(&local) {
            return;
        }
        let decl = &cx.ctx.body.locals[local];
        let name = cx.ctx.local(local);
        let mutable = if cx.ctx.mutable.contains(&local) {
            "mut "
        } else {
            ""
        };
        if decl.optional {
            cx.w.line(&format!(
                "let {mutable}{name}: {} = None;",
                self.rust_ty(&decl.ty, true)
            ));
        } else if zero && cx.ctx.live.contains(&local) {
            cx.w.line(&format!(
                "let {mutable}{name}: {} = {};",
                self.rust_ty(&decl.ty, false),
                zero_of(&decl.ty)
            ));
        } else {
            // The initial value is never observed: declare deferred. The
            // typed program assigns every path before any read.
            cx.w.line(&format!(
                "let {mutable}{name}: {};",
                self.rust_ty(&decl.ty, false)
            ));
        }
    }

    /// `let`/`=`/`= Some` with a width-aware right-hand side.
    fn emit_let(&self, cx: &mut Cx<'_, '_, '_, '_>, dst: &crate::ir::typed::Bind, value: &Expr) {
        let decl = &cx.ctx.body.locals[dst.local];
        let name = cx.ctx.local(dst.local).to_string();
        let mutable = if cx.ctx.decl_mut(dst) { "mut " } else { "" };
        let stem = if cx.ctx.declares(dst) {
            let inner = self.rust_ty(&decl.ty, false);
            let ty = if dst.some || decl.optional {
                format!("Option<{inner}>")
            } else {
                inner
            };
            format!("let {mutable}{name}: {ty} = ")
        } else {
            format!("{name} = ")
        };
        // `= Some(..)` wraps the value; plain stores do not.
        let open = if dst.some { "Some(" } else { "" };
        let close = if dst.some { ");" } else { ";" };
        let used = cx.w.depth * 4 + stem.len() + open.len() + close.len();
        let lines = self.expr_fit(cx, value, cx.w.depth, used);
        if lines.len() == 1 {
            cx.w.line(&format!("{stem}{open}{}{close}", lines[0]));
        } else {
            cx.w.raw(&format!("{stem}{open}{}", lines[0]));
            for l in &lines[1..lines.len() - 1] {
                cx.w.raw(l);
            }
            cx.w.raw(&format!("{}{close}", lines[lines.len() - 1]));
        }
    }
}

/// Zero value of a type for `Declare` (always under a `let …: Ty`
/// annotation, so integer zeros stay unsuffixed and infer).
fn zero_of(ty: &Ty) -> String {
    match ty {
        Ty::Int(_) => "0".to_string(),
        Ty::Bool => "false".to_string(),
        Ty::F32 => "0f32".to_string(),
        Ty::F64 => "0f64".to_string(),
        Ty::Str => "String::new()".to_string(),
        Ty::Bytes => "Vec::new()".to_string(),
        Ty::List(_) => "Vec::new()".to_string(),
        Ty::Named(_) => panic!("rust-typed: named zero value"),
    }
}
impl<'a> Gen<'a> {
    /// `Read`: cursor operations with conversions into the destination type.
    /// A negative `Bytes(n)` fails with `NegativeLength` like the interpreter.
    fn emit_read(&self, cx: &mut Cx<'_, '_, '_, '_>, dst: &crate::ir::typed::Bind, op: &ReadOp) {
        let decl = &cx.ctx.body.locals[dst.local];
        let name = cx.ctx.local(dst.local).to_string();
        match op {
            ReadOp::Bytes(n) => {
                let len_expr = self.expr(cx, n);
                let codec = cx.ctx.snake.clone();
                self.emit_guarded_len(&mut *cx.w, &len_expr, &codec);
                let rhs = format!(
                    "r.read_bytes({} as usize)?.to_vec()",
                    cast_operand(n, &len_expr)
                );
                self.emit_store(cx, dst, &decl.ty.clone(), decl.optional, &name, &rhs);
            }
            ReadOp::Rest => {
                let rhs = "r.read_rest().to_vec()".to_string();
                self.emit_store(cx, dst, &decl.ty.clone(), decl.optional, &name, &rhs);
            }
            ReadOp::U8 => {
                let rhs = match &decl.ty {
                    Ty::Bool => "r.read_u8()? != 0".to_string(),
                    Ty::Int(IntTy::U8) => "r.read_u8()?".to_string(),
                    Ty::Int(t) if lossless_widen(&Ty::Int(IntTy::U8), &Ty::Int(*t)) => {
                        format!("{}::from(r.read_u8()?)", Self::int_name(*t))
                    }
                    Ty::Int(t) => format!("r.read_u8()? as {}", Self::int_name(*t)),
                    _ => "r.read_u8()?".to_string(),
                };
                self.emit_store(cx, dst, &decl.ty.clone(), decl.optional, &name, &rhs);
            }
            ReadOp::F32 => {
                let rhs = match &decl.ty {
                    Ty::F32 => "r.read_f32_be()?".to_string(),
                    _ => "r.read_f32_be()? as f64".to_string(),
                };
                self.emit_store(cx, dst, &decl.ty.clone(), decl.optional, &name, &rhs);
            }
            ReadOp::F64 => {
                let rhs = match &decl.ty {
                    Ty::F64 => "r.read_f64_be()?".to_string(),
                    _ => "r.read_f64_be()? as f32".to_string(),
                };
                self.emit_store(cx, dst, &decl.ty.clone(), decl.optional, &name, &rhs);
            }
            ReadOp::F32Le => {
                let rhs = match &decl.ty {
                    Ty::F32 => "r.read_f32_le()?".to_string(),
                    _ => "r.read_f32_le()? as f64".to_string(),
                };
                self.emit_store(cx, dst, &decl.ty.clone(), decl.optional, &name, &rhs);
            }
            ReadOp::F64Le => {
                let rhs = match &decl.ty {
                    Ty::F64 => "r.read_f64_le()?".to_string(),
                    _ => "r.read_f64_le()? as f32".to_string(),
                };
                self.emit_store(cx, dst, &decl.ty.clone(), decl.optional, &name, &rhs);
            }
        }
    }

    /// `let` or assignment of an already-rendered right-hand side.
    fn emit_store(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        dst: &crate::ir::typed::Bind,
        ty: &Ty,
        opt: bool,
        name: &str,
        rhs: &str,
    ) {
        let mutable = if cx.ctx.decl_mut(dst) { "mut " } else { "" };
        if cx.ctx.declares(dst) {
            let ascribe = self.rust_ty(ty, opt || dst.some);
            if dst.some {
                cx.w.line(&format!("let {mutable}{name}: {ascribe} = Some({rhs});"));
            } else {
                cx.w.line(&format!("let {mutable}{name}: {ascribe} = {rhs};"));
            }
        } else if dst.some {
            cx.w.line(&format!("{name} = Some({rhs});"));
        } else {
            cx.w.line(&format!("{name} = {rhs};"));
        }
    }

    /// Negative-length guard naming `Error::NegativeLength` for `codec`.
    fn emit_guarded_len(&self, w: &mut W, len_expr: &str, codec: &str) {
        w.line(&format!("if ({len_expr}) < 0 {{"));
        w.ind();
        w.line("return Err(Error::NegativeLength {");
        w.ind();
        w.line(&format!("codec: \"{codec}\","));
        if len_expr == "length" {
            w.line("length,");
        } else {
            w.line(&format!("length: {len_expr},"));
        }
        w.out();
        w.line("});");
        w.out();
        w.line("}");
    }

    /// `Decode`: call another codec's decode function or method.
    fn emit_decode_call(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        dst: &crate::ir::typed::Bind,
        codec: CodecIdx,
        args: &[Expr],
    ) {
        self.note_codec(&mut *cx.uses, cx.here, codec);
        let callee = &self.prog.codecs[codec];
        let base = if is_method(&callee.kind) {
            let ty = codec_type(self.prog, codec);
            self.note_ty(&mut *cx.uses, cx.here, ty);
            format!("{}::decode(", self.type_name[ty])
        } else {
            format!("{}(", self.free_decode[codec])
        };
        let mut arg_strs = vec!["r".to_string()];
        for a in args {
            arg_strs.push(self.call_arg(cx, a));
        }
        let decl = &cx.ctx.body.locals[dst.local];
        let name = cx.ctx.local(dst.local).to_string();
        let (ty, opt) = (decl.ty.clone(), decl.optional);
        if cx.ctx.declares(dst) {
            let mutable = if cx.ctx.mutable.contains(&dst.local) {
                "mut "
            } else {
                ""
            };
            let ascribe = self.rust_ty(&ty, opt || dst.some);
            if dst.some {
                let stem = format!("let {mutable}{name}: {ascribe} = Some({base}");
                self.stmt_call(&mut *cx.w, &stem, &arg_strs, ")?);");
            } else {
                let stem = format!("let {mutable}{name}: {ascribe} = {base}");
                self.stmt_call(&mut *cx.w, &stem, &arg_strs, ")?;");
            }
        } else if dst.some {
            self.stmt_call(
                &mut *cx.w,
                &format!("{name} = Some({base}"),
                &arg_strs,
                ")?);",
            );
        } else {
            self.stmt_call(&mut *cx.w, &format!("{name} = {base}"), &arg_strs, ")?;");
        }
    }

    /// By-value call argument: bare when `Copy`, cloned otherwise (the
    /// callee takes ownership, so the clone is never redundant).
    fn call_arg(&self, cx: &mut Cx<'_, '_, '_, '_>, e: &Expr) -> String {
        let s = self.expr(cx, e);
        if self.copy_of(cx.ctx, e) {
            s
        } else {
            format!("{s}.clone()")
        }
    }

    /// Whether a non-`Copy` bind can be declared as a reference: the body
    /// still has it declared, and every use of it is a borrow (a method
    /// receiver, a `&`-argument, a further `.as_ref()`, or a place read).
    fn borrowed_binding(&self, ctx: &Ctx<'_>, local: LocalId) -> bool {
        if !binds_declare(&ctx.body.stmts, local) {
            return false;
        }
        let mut owned = false;
        walk_uses(&ctx.body.stmts, &mut |id, need| {
            if id == local && need {
                owned = true;
            }
        });
        !owned
    }

    /// Whether an expression's value is `Copy`.
    fn copy_of(&self, ctx: &Ctx<'_>, e: &Expr) -> bool {
        let (ty, _) = self.prog.expr_ty(ctx.body, e);
        self.copy_ty_in(&ty)
    }

    /// Whether a type is `Copy` (standalone, using the fixpoint for names).
    fn copy_ty_in(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => true,
            Ty::Str | Ty::Bytes | Ty::List(_) => false,
            Ty::Named(t) => self.is_copy[*t],
        }
    }

    /// A call-shaped statement `stem(arg, ..)<tail>`: one line when it fits,
    /// arguments broken one per line with a trailing comma otherwise.
    fn stmt_call(&self, w: &mut W, stem: &str, args: &[String], tail: &str) {
        let one = format!("{stem}{}{tail}", args.join(", "));
        if w.depth * 4 + one.len() <= 100 {
            w.line(&one);
            return;
        }
        w.line(stem);
        w.ind();
        for a in args {
            w.line(&format!("{a},"));
        }
        w.out();
        w.line(tail);
    }

    /// `Utf8`: validate bytes as UTF-8, reporting the slice start.
    fn emit_utf8(&self, cx: &mut Cx<'_, '_, '_, '_>, dst: &crate::ir::typed::Bind, src: &Expr) {
        let src_str = self.expr(cx, src);
        let borrowed = borrow_of(src, &src_str);
        let rhs = format!(
            "std::str::from_utf8({borrowed}).map_err(|_| Error::InvalidUtf8 {{ offset: r.offset().saturating_sub(({src_str}).len()) }})?.to_owned()"
        );
        let decl = &cx.ctx.body.locals[dst.local];
        let name = cx.ctx.local(dst.local).to_string();
        let (ty, opt) = (decl.ty.clone(), decl.optional);
        self.emit_store(cx, dst, &ty, opt, &name, &rhs);
    }

    /// `Write`: writer operations; negative `Bytes(n)` fails like the
    /// interpreter instead of reserving a huge buffer.
    fn emit_write(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        op: &crate::ir::typed::WriteOp,
        value: &Expr,
    ) {
        match op {
            crate::ir::typed::WriteOp::U8 => {
                let v = self.expr(cx, value);
                let (ty, _) = self.prog.expr_ty(cx.ctx.body, value);
                let arg = if let Some(byte) = crate::ir::typed::byte_literal(value, true) {
                    byte
                } else if matches!(ty, Ty::Int(IntTy::U8)) {
                    v
                } else {
                    format!("{} as u8", cast_operand(value, &v))
                };
                self.stmt_call(&mut *cx.w, "w.write_u8(", std::slice::from_ref(&arg), ");");
            }
            crate::ir::typed::WriteOp::F32 => {
                let v = self.expr(cx, value);
                let (ty, _) = self.prog.expr_ty(cx.ctx.body, value);
                let arg = if matches!(ty, Ty::F32) {
                    v
                } else {
                    format!("({v}) as f32")
                };
                self.stmt_call(
                    &mut *cx.w,
                    "w.write_f32_be(",
                    std::slice::from_ref(&arg),
                    ");",
                );
            }
            crate::ir::typed::WriteOp::F64 => {
                let v = self.expr(cx, value);
                let (ty, _) = self.prog.expr_ty(cx.ctx.body, value);
                let arg = if matches!(ty, Ty::F64) {
                    v
                } else {
                    format!("({v}) as f64")
                };
                self.stmt_call(
                    &mut *cx.w,
                    "w.write_f64_be(",
                    std::slice::from_ref(&arg),
                    ");",
                );
            }
            crate::ir::typed::WriteOp::F32Le => {
                let v = self.expr(cx, value);
                let (ty, _) = self.prog.expr_ty(cx.ctx.body, value);
                let arg = if matches!(ty, Ty::F32) {
                    v
                } else {
                    format!("({v}) as f32")
                };
                self.stmt_call(
                    &mut *cx.w,
                    "w.write_f32_le(",
                    std::slice::from_ref(&arg),
                    ");",
                );
            }
            crate::ir::typed::WriteOp::F64Le => {
                let v = self.expr(cx, value);
                let (ty, _) = self.prog.expr_ty(cx.ctx.body, value);
                let arg = if matches!(ty, Ty::F64) {
                    v
                } else {
                    format!("({v}) as f64")
                };
                self.stmt_call(
                    &mut *cx.w,
                    "w.write_f64_le(",
                    std::slice::from_ref(&arg),
                    ");",
                );
            }
            crate::ir::typed::WriteOp::Bytes(n) => {
                let n_str = self.expr(cx, n);
                let codec = cx.ctx.snake.clone();
                self.emit_guarded_len(&mut *cx.w, &format!("({n_str})"), &codec);
                let v = self.expr(cx, value);
                let bytes = match value {
                    Expr::StrBytes(_) => v,
                    _ if self.is_ref_local(cx.ctx, value) => v,
                    _ => borrow_of(value, &v),
                };
                let args = [
                    bytes,
                    format!("{} as usize", cast_operand(n, &n_str)),
                    format!("\"{}\"", cx.ctx.snake),
                ];
                self.stmt_call(&mut *cx.w, "w.write_bytes_fixed(", &args, ")?;");
            }
            crate::ir::typed::WriteOp::Rest => {
                let v = self.expr(cx, value);
                let bytes = match value {
                    Expr::StrBytes(_) => v,
                    _ if self.is_ref_local(cx.ctx, value) => v,
                    _ => borrow_of(value, &v),
                };
                self.stmt_call(
                    &mut *cx.w,
                    "w.write_bytes(",
                    std::slice::from_ref(&bytes),
                    ");",
                );
            }
        }
    }

    /// `Encode`: call another codec's encode function or method.
    fn emit_encode_call(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        codec: CodecIdx,
        args: &[Expr],
        value: &Expr,
    ) {
        self.note_codec(&mut *cx.uses, cx.here, codec);
        let callee = &self.prog.codecs[codec];
        let mut arg_strs = Vec::new();
        for a in args {
            arg_strs.push(self.call_arg(cx, a));
        }
        if is_method(&callee.kind) {
            let ty = codec_type(self.prog, codec);
            self.note_ty(&mut *cx.uses, cx.here, ty);
            let recv = self.expr(cx, value);
            let mut full_args = vec!["w".to_string()];
            full_args.extend(arg_strs);
            self.stmt_call(&mut *cx.w, &format!("({recv}).encode("), &full_args, ")?;");
        } else {
            let (ty, _) = self.prog.expr_ty(cx.ctx.body, value);
            let v = self.expr(cx, value);
            let varg = if self.copy_ty_in(&ty) || self.is_ref_local(cx.ctx, value) {
                v
            } else {
                borrow_of(value, &v)
            };
            let mut full_args = vec!["w".to_string(), varg];
            full_args.extend(arg_strs);
            self.stmt_call(
                &mut *cx.w,
                &format!("{}(", self.free_encode[codec]),
                &full_args,
                ")?;",
            );
        }
    }

    /// `Require`: fail with the codec and condition text when the negation
    /// holds (`if x != y`, never `if !(x == y)`).
    fn emit_require(&self, cx: &mut Cx<'_, '_, '_, '_>, cond: &Expr, text: &str) {
        let neg = self.prog.negated(cx.ctx.body, cond);
        let cond_str = self.expr(cx, &neg);
        cx.w.line(&format!("if {cond_str} {{"));
        cx.w.ind();
        cx.w.line("return Err(Error::RequireFailed {");
        cx.w.ind();
        cx.w.line(&format!("codec: \"{}\",", cx.ctx.snake));
        cx.w.line(&format!("condition: {text:?},"));
        cx.w.out();
        cx.w.line("});");
        cx.w.out();
        cx.w.line("}");
    }

    /// `Unwrap`: bind an optional's value or fail with the missing field.
    /// The destination is left un-ascribed so boxed payloads infer.
    ///
    /// A non-`Copy` source is bound by reference (`.as_ref()`) whenever the
    /// rest of the body only ever borrows the destination's value: the
    /// source is usually a field of borrowed `self` on encode, which cannot
    /// be moved at all. A source the body still moves keeps its clone.
    fn emit_unwrap(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        dst: &crate::ir::typed::Bind,
        src: &Expr,
        field: &str,
    ) {
        let mut src_str = self.expr(cx, src);
        let (ty, opt) = self.prog.expr_ty(cx.ctx.body, src);
        debug_assert!(opt, "rust-typed: unwrap of a non-optional");
        let copy = self.copy_ty_in(&ty);
        if !copy && self.borrowed_binding(cx.ctx, dst.local) {
            src_str = format!("{src_str}.as_ref()");
        } else if !copy {
            src_str = format!("{src_str}.clone()");
        }
        let rhs = format!("{src_str}.ok_or(Error::MissingField {{ field: {field:?} }})?");
        let name = cx.ctx.local(dst.local).to_string();
        let mutable = if cx.ctx.decl_mut(dst) { "mut " } else { "" };
        if cx.ctx.declares(dst) {
            cx.w.line(&format!("let {mutable}{name} = {rhs};"));
        } else {
            cx.w.line(&format!("{name} = {rhs};"));
        }
    }
}

impl<'a> Gen<'a> {
    /// `If`: branches share the tail flag; a lone `if` in `else` becomes
    /// `else if` (which is also what `rustfmt` prints).
    fn emit_if(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        cond: &Expr,
        then: &[Stmt],
        els: &[Stmt],
        tail: bool,
    ) {
        let mut cond_str = self.expr(cx, cond);
        let mut then = then;
        let mut els = els;
        cx.w.line(&format!("if {cond_str} {{"));
        loop {
            cx.w.ind();
            self.emit_stmts(cx, then, tail);
            cx.w.out();
            if els.is_empty() {
                cx.w.line("}");
                break;
            }
            if els.len() == 1
                && let Stmt::If {
                    cond: cond2,
                    then: then2,
                    els: els2,
                } = &els[0]
            {
                cond_str = self.expr(cx, cond2);
                cx.w.line(&format!("}} else if {cond_str} {{"));
                then = then2;
                els = els2;
                continue;
            }
            cx.w.line("} else {");
            cx.w.ind();
            self.emit_stmts(cx, els, tail);
            cx.w.out();
            cx.w.line("}");
            break;
        }
    }

    /// `Match`: arms in schema order; without `other`, a trailing error arm
    /// (which also makes the match exhaustive) unless the arms already cover
    /// a boolean scrutinee.
    fn emit_match(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        scrutinee: &Expr,
        arms: &[crate::ir::typed::MatchArm],
        other: &Option<Vec<Stmt>>,
        tail: bool,
    ) {
        let (sty, _) = self.prog.expr_ty(cx.ctx.body, scrutinee);
        let is_bool = matches!(sty, Ty::Bool);
        // A plain local matches directly; anything else binds a temporary so
        // the `NoMatchArm` error can still name the value.
        let scrut = match scrutinee {
            Expr::Local(id) => cx.ctx.local(*id).to_string(),
            _ => {
                let mut tmp = "match_tag".to_string();
                while cx.ctx.locals.iter().any(|l| l == &tmp) {
                    tmp.push('_');
                }
                let s = self.expr(cx, scrutinee);
                cx.w.line(&format!("let {tmp} = {s};"));
                tmp
            }
        };
        let tag = if matches!(sty, Ty::Int(IntTy::I64)) {
            scrut.clone()
        } else if is_bool || lossless_widen(&sty, &Ty::Int(IntTy::I64)) {
            format!("i64::from({scrut})")
        } else {
            format!("{scrut} as i64")
        };
        cx.w.line(&format!("match {scrut} {{"));
        cx.w.ind();
        for arm in arms {
            let pat = if is_bool {
                // Several tags can map to one boolean; duplicates would be
                // unreachable patterns.
                let mut seen = [false, false];
                let mut out = Vec::new();
                for t in &arm.tags {
                    let word = if *t == 0 { "false" } else { "true" };
                    let slot = usize::from(*t != 0);
                    if !seen[slot] {
                        seen[slot] = true;
                        out.push(word);
                    }
                }
                out.join(" | ")
            } else {
                int_pattern(&arm.tags)
            };
            self.emit_arm(cx, &pat, &arm.body, tail);
        }
        match other {
            Some(body) => self.emit_arm(cx, "_", body, tail),
            None => {
                let exhaustive = is_bool
                    && arms.iter().flat_map(|a| a.tags.iter()).any(|t| *t == 0)
                    && arms.iter().flat_map(|a| a.tags.iter()).any(|t| *t != 0);
                if !exhaustive {
                    let tag_field = if tag == "tag" {
                        "tag".to_string()
                    } else {
                        format!("tag: {tag}")
                    };
                    let err = format!(
                        "Err(Error::NoMatchArm {{ codec: \"{}\", {tag_field} }})",
                        cx.ctx.snake
                    );
                    self.emit_arm_raw(&mut *cx.w, "_", &err, tail, cx.ctx.mode == Mode::Encode);
                }
            }
        }
        cx.w.out();
        cx.w.line("}");
    }

    /// A value arm, `pat => expr,`, braced when the line would run long.
    /// A diverging arm keeps its `return` outside tail position.
    fn emit_arm_raw(&self, w: &mut W, pat: &str, expr: &str, tail: bool, ret: bool) {
        let one = format!("{pat} => {expr},");
        if !ret && w.depth * 4 + one.len() <= 100 {
            w.line(&one);
            return;
        }
        w.line(&format!("{pat} => {{"));
        w.ind();
        if tail && !ret {
            w.line(&format!("{expr},"));
        } else {
            w.line(&format!("return {expr};"));
        }
        w.out();
        w.line("}");
    }

    /// One match arm: a lone trailing `Return`/`Fail` prints as a value arm
    /// when it fits on one line, anything else as a braced block.
    fn emit_arm(&self, cx: &mut Cx<'_, '_, '_, '_>, pat: &str, body: &[Stmt], tail: bool) {
        if body.len() == 1 {
            match &body[0] {
                Stmt::Return(e) if cx.ctx.mode == Mode::Decode => {
                    // `pat => Ok(value),` when the value fits on one line.
                    let used = cx.w.depth * 4 + pat.len() + " => Ok(".len() + "),".len();
                    let lines = self.expr_fit(cx, e, cx.w.depth, used);
                    if lines.len() == 1
                        && cx.w.depth * 4 + pat.len() + 4 + 3 + lines[0].len() + 2 <= 100
                    {
                        cx.w.line(&format!("{pat} => Ok({}),", lines[0]));
                        return;
                    }
                }
                Stmt::Fail(f) => {
                    let err = self.fail_expr(cx.ctx, *f);
                    self.emit_arm_raw(&mut *cx.w, pat, &err, tail, cx.ctx.mode == Mode::Encode);
                    return;
                }
                _ => {}
            }
        }
        cx.w.line(&format!("{pat} => {{"));
        cx.w.ind();
        self.emit_stmts(cx, body, tail);
        cx.w.out();
        cx.w.line("}");
    }

    /// `Repeat` (decode): counted, `until` (peeked), and `until-end` loops.
    /// The list starts empty and each iteration pushes the bound item.
    fn emit_repeat(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        mode: &RepeatMode,
        list: LocalId,
        item: LocalId,
        body: &[Stmt],
    ) {
        let list_name = cx.ctx.local(list).to_string();
        let item_ty = cx.ctx.body.locals[item].ty.clone();
        let codec = format!("{}.{}", cx.ctx.snake, cx.ctx.local(list));
        cx.w.line(&format!(
            "let mut {list_name}: Vec<{}> = Vec::new();",
            self.rust_ty(&item_ty, false)
        ));
        match mode {
            RepeatMode::Count(n) => {
                let n_str = self.expr(cx, n);
                self.emit_guarded_len(&mut *cx.w, &n_str, &codec);
                cx.w.line(&format!("for _ in 0..{n_str} {{"));
                cx.w.ind();
                self.emit_stmts(cx, body, false);
                self.emit_push(cx, &list_name, item);
                cx.w.out();
                cx.w.line("}");
            }
            RepeatMode::Until(term) => {
                // A literal byte compares to the peeked `u8` directly.
                let check = match crate::ir::typed::byte_literal(term, true) {
                    Some(byte) => format!("r.peek_u8()? == {byte}"),
                    None => {
                        let t = self.expr(cx, term);
                        let t = if matches!(term, Expr::Cond { .. }) {
                            format!("({t})")
                        } else {
                            t
                        };
                        format!("i64::from(r.peek_u8()?) == {t}")
                    }
                };
                cx.w.line("while r.remaining() > 0 {");
                cx.w.ind();
                cx.w.line(&format!("if {check} {{"));
                cx.w.ind();
                cx.w.line("r.read_u8()?;");
                cx.w.line("break;");
                cx.w.out();
                cx.w.line("}");
                self.emit_stmts(cx, body, false);
                self.emit_push(cx, &list_name, item);
                cx.w.out();
                cx.w.line("}");
            }
            RepeatMode::UntilEnd => {
                cx.w.line("while r.remaining() > 0 {");
                cx.w.ind();
                self.emit_stmts(cx, body, false);
                self.emit_push(cx, &list_name, item);
                cx.w.out();
                cx.w.line("}");
            }
        }
    }

    /// `list.push(item);` for a repeat iteration.
    fn emit_push(&self, cx: &mut Cx<'_, '_, '_, '_>, list_name: &str, item: LocalId) {
        cx.w.line(&format!("{list_name}.push({});", cx.ctx.local(item)));
    }

    /// `ForEach` (encode): borrow the list, copying `Copy` items.
    fn emit_foreach(&self, cx: &mut Cx<'_, '_, '_, '_>, list: &Expr, item: LocalId, body: &[Stmt]) {
        let list_str = self.expr(cx, list);
        let item_ty = cx.ctx.body.locals[item].ty.clone();
        let iter = if self.copy_ty_in(&item_ty) && self.foreach_moves(body, item) {
            "iter().copied()"
        } else {
            "iter()"
        };
        cx.w.line(&format!(
            "for {} in ({list_str}).{iter} {{",
            cx.ctx.local(item)
        ));
        cx.w.ind();
        self.emit_stmts(cx, body, false);
        cx.w.out();
        cx.w.line("}");
    }

    /// `ExpectVariant` (encode): bind the sum payload or fail.
    fn emit_expect(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        dst: &Option<crate::ir::typed::Bind>,
        sum: &Expr,
        ty: TypeIdx,
        variant: usize,
    ) {
        let sum_str = self.expr(cx, sum);
        let sum_ref = borrow_of(sum, &sum_str);
        let type_name = &self.type_name[ty];
        self.note_ty(&mut *cx.uses, cx.here, ty);
        let variant_name = &self.variant_names[ty][variant];
        let err = format!(
            "return Err(Error::WrongVariant {{ codec: \"{}\", expected: \"{variant_name}\" }})",
            cx.ctx.snake
        );
        match dst {
            None => {
                cx.w.line(&format!("match {sum_ref} {{"));
                cx.w.ind();
                cx.w.line(&format!("{type_name}::{variant_name} => {{}}"));
                // Unit or payload: a bare `_` arm would also match; bind the
                // payload when there is one so the shape stays total.
                cx.w.line("_ => {");
                cx.w.ind();
                cx.w.line(&(err.clone() + ";"));
                cx.w.out();
                cx.w.line("}");
                cx.w.out();
                cx.w.line("}");
            }
            Some(d) => {
                let name = cx.ctx.local(d.local).to_string();
                let mutable = if cx.ctx.decl_mut(d) { "mut " } else { "" };
                let payload = &self.prog.types[ty];
                let unit = match &payload.kind {
                    crate::ir::typed::TypeKind::Sum { variants } => {
                        variants[variant].payload.is_none()
                    }
                    _ => false,
                };
                // A unit check binds nothing; the destination (if declared
                // elsewhere) stays unused by construction of the program.
                let _ = (name.clone(), mutable);
                if unit {
                    cx.w.line(&format!("match {sum_ref} {{"));
                    cx.w.ind();
                    cx.w.line(&format!("{type_name}::{variant_name} => {{}}"));
                    cx.w.line("_ => {");
                    cx.w.ind();
                    cx.w.line(&(err.clone() + ";"));
                    cx.w.out();
                    cx.w.line("}");
                    cx.w.out();
                    cx.w.line("}");
                } else {
                    let boxed = self.boxed_variant.contains(&(ty, variant));
                    let payload_ty = match &self.prog.types[ty].kind {
                        crate::ir::typed::TypeKind::Sum { variants } => {
                            variants[variant].payload.clone()
                        }
                        _ => panic!("rust-typed: payload of a non-sum"),
                    };
                    // The scrutinee is borrowed (`&self` on encode), so
                    // match a reference: `v` is `&Payload` (`&Box<Payload>`
                    // when boxed) in every case. `Copy` payloads copy out of
                    // it, a boxed one derefs, and a payload the body only
                    // borrows binds as the reference itself.
                    let by_ref = d.declare
                        && dst
                            .as_ref()
                            .is_some_and(|d| self.borrowed_binding(cx.ctx, d.local));
                    let binding = if boxed {
                        if by_ref {
                            "&**v".to_string()
                        } else {
                            "(**v).clone()".to_string()
                        }
                    } else if payload_ty.as_ref().is_some_and(|t| self.copy_ty_in(t)) {
                        "*v".to_string()
                    } else if by_ref {
                        "v".to_string()
                    } else {
                        "(*v).clone()".to_string()
                    };
                    // The destination infers (boxed payloads included).
                    let head = if cx.ctx.declares(d) {
                        format!("let {mutable}{name} = match {sum_ref} {{")
                    } else {
                        format!("{name} = match {sum_ref} {{")
                    };
                    let arm = format!("{type_name}::{variant_name}(v) => {binding},");
                    let one = format!("{head} {arm} _ => {{ {err}; }} }};");
                    if cx.w.depth * 4 + one.len() <= 100 {
                        cx.w.line(&one);
                    } else {
                        cx.w.line(&head);
                        cx.w.ind();
                        cx.w.line(&arm);
                        cx.w.line("_ => {");
                        cx.w.ind();
                        cx.w.line(&(err.clone() + ";"));
                        cx.w.out();
                        cx.w.line("}");
                        cx.w.out();
                        cx.w.line("};");
                    }
                }
            }
        }
    }

    /// `Return`: bare `Ok(..)` in tail position (which `clippy` requires),
    /// `return` otherwise. Encode bodies never return (verified on 26.3 and
    /// Hytale); a stray one still yields `()`.
    fn emit_return(&self, cx: &mut Cx<'_, '_, '_, '_>, e: &Expr, tail: bool) {
        if cx.ctx.mode == Mode::Encode {
            cx.w.line(if tail { "Ok(())" } else { "return Ok(());" });
            return;
        }
        let (open, close) = if tail {
            ("Ok(", ")")
        } else {
            ("return Ok(", ");")
        };
        let used = cx.w.depth * 4 + open.len() + close.len();
        let lines = self.expr_fit(cx, e, cx.w.depth, used);
        if lines.len() == 1 {
            cx.w.line(&format!("{open}{}{close}", lines[0]));
        } else {
            cx.w.raw(&format!("{open}{}", lines[0]));
            for l in &lines[1..lines.len() - 1] {
                cx.w.raw(l);
            }
            cx.w.raw(&format!("{}{close}", lines[lines.len() - 1]));
        }
    }

    /// `Fail`: always fails, bare in tail position on decode; `return`
    /// elsewhere and always on encode (a bare `Err` would not match the
    /// `()` arms of an encode `match` or `if`).
    fn emit_fail(&self, cx: &mut Cx<'_, '_, '_, '_>, fail: crate::ir::typed::Failure, tail: bool) {
        let expr = self.fail_expr(cx.ctx, fail);
        if tail && cx.ctx.mode == Mode::Decode {
            cx.w.line(&expr);
        } else {
            cx.w.line(&format!("return {expr};"));
        }
    }

    /// The error expression of a `Fail`.
    fn fail_expr(&self, ctx: &Ctx<'_>, fail: crate::ir::typed::Failure) -> String {
        match fail {
            crate::ir::typed::Failure::EncodeUntilEnd => {
                format!("Err(Error::EncodeUntilEnd {{ codec: \"{}\" }})", ctx.snake)
            }
        }
    }
}

impl<'a> Gen<'a> {
    /// Single-line rendering of an expression. Wide results are broken by
    /// `expr_fit`; purely structural forms (records, `if`) are multiline
    /// there and single-line (possibly long) here.
    fn expr(&self, cx: &mut Cx<'_, '_, '_, '_>, e: &Expr) -> String {
        match e {
            Expr::Int { value, ty } => int_lit(*value, *ty),
            Expr::Bool(b) => b.to_string(),
            Expr::Str(s) => format!("{s:?}"),
            Expr::Local(id) => cx.ctx.local(*id).to_string(),
            Expr::Field { base, ty, field } => {
                let b = self.expr(cx, base);
                format!("{b}.{}", self.field_names[*ty][*field])
            }
            Expr::Present(inner) => {
                let x = self.expr(cx, inner);
                format!("({x}).is_some()")
            }
            Expr::Get(inner) => {
                let x = self.expr(cx, inner);
                let (ty, _) = self.prog.expr_ty(cx.ctx.body, inner);
                if self.copy_ty_in(&ty) {
                    format!("({x}).as_ref().copied().unwrap()")
                } else {
                    format!("({x}).clone().unwrap()")
                }
            }
            Expr::Not(inner) => self.not_str(cx, inner),
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } => self.binary_str(cx, *op, lhs, rhs, ty, *wraps),
            Expr::Cond {
                cond, then, els, ..
            } => self.cond_str(cx, cond, then, els),
            Expr::Cast { expr, to } => self.cast_str(cx, expr, to),
            Expr::Call { func, args } => self.call_str(cx, *func, args),
            Expr::StrBytes(inner) => {
                let x = self.expr(cx, inner);
                format!("({x}).as_bytes()")
            }
            Expr::Remaining => "r.remaining() as i64".to_string(),
            Expr::Construct { ty, fields } => {
                let t = &self.prog.types[*ty];
                let names: Vec<String> = match &t.kind {
                    crate::ir::typed::TypeKind::Record { fields: decls } => decls
                        .iter()
                        .enumerate()
                        .map(|(fi, f)| {
                            self.note_ty_tree(&mut *cx.uses, cx.here, &f.ty);
                            (self.field_names[*ty][fi].clone(), &f.ty, fi)
                        })
                        .map(|(n, _, _)| n)
                        .collect(),
                    _ => panic!("rust-typed: construct of a non-record"),
                };
                let parts = fields
                    .iter()
                    .enumerate()
                    .map(|(fi, f)| {
                        let v = self.expr(cx, f);
                        let boxed = self.boxed_field.contains(&(*ty, fi));
                        let short = !boxed
                            && matches!(f, Expr::Local(id) if cx.ctx.local(*id) == names[fi]);
                        if short {
                            names[fi].clone()
                        } else if boxed {
                            let field_opt = match &self.prog.types[*ty].kind {
                                crate::ir::typed::TypeKind::Record { fields } => {
                                    fields[fi].optional
                                }
                                _ => false,
                            };
                            self.boxed_arg(cx.ctx, &names[fi], f, &v, field_opt)
                        } else {
                            format!("{}: {v}", names[fi])
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if parts.is_empty() {
                    format!("{} {{}}", self.type_name[*ty])
                } else {
                    format!("{} {{ {parts} }}", self.type_name[*ty])
                }
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                self.note_ty(&mut *cx.uses, cx.here, *ty);
                let name = format!(
                    "{}::{}",
                    self.type_name[*ty], self.variant_names[*ty][*variant]
                );
                match payload {
                    None => name,
                    Some(p) => {
                        let v = self.expr(cx, p);
                        if self.boxed_variant.contains(&(*ty, *variant)) {
                            format!("{name}(Box::new({v}))")
                        } else {
                            format!("{name}({v})")
                        }
                    }
                }
            }
        }
    }
    /// Box-wrapped struct-literal field: `Box::new(v)`, mapped over an
    /// optional value, wrapped in `Some` for an optional field holding a
    /// plain value.
    fn boxed_arg(&self, ctx: &Ctx<'_>, name: &str, f: &Expr, v: &str, field_opt: bool) -> String {
        let (_, vopt) = self.prog.expr_ty(ctx.body, f);
        let inner = if vopt {
            format!("({v}).map(Box::new)")
        } else {
            format!("Box::new({v})")
        };
        if field_opt && !vopt {
            format!("{name}: Some({inner})")
        } else {
            format!("{name}: {inner}")
        }
    }

    /// Width-aware expression lines: the first continues the caller's stem,
    /// continuations carry absolute indentation. `indent` is the statement
    /// depth, `used` the columns already consumed on the first line.
    fn expr_fit(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        e: &Expr,
        indent: usize,
        used: usize,
    ) -> Vec<String> {
        let one = self.expr(cx, e);
        if used + one.len() <= 100 {
            return vec![one];
        }
        match e {
            Expr::Binary { .. } => self.binary_fit(cx, e, indent, used),
            Expr::Call { func, args } => self.call_fit(cx, *func, args, indent),
            Expr::Cond {
                cond,
                then,
                els,
                ty,
            } => {
                let _ = ty;
                let c = self.expr(cx, cond);
                let pad = "    ".repeat(indent + 1);
                let mut out = vec![format!("if {c} {{")];
                for l in self.expr_fit(cx, then, indent + 1, (indent + 1) * 4) {
                    out.push(format!("{pad}{l}"));
                }
                out.push(format!("{}}} else {{", "    ".repeat(indent)));
                for l in self.expr_fit(cx, els, indent + 1, (indent + 1) * 4) {
                    out.push(format!("{pad}{l}"));
                }
                out.push(format!("{}}}", "    ".repeat(indent)));
                out
            }
            Expr::Construct { ty, fields } => {
                let tname = self.type_name[*ty].clone();
                let mut out = vec![format!("{tname} {{")];
                let pad = "    ".repeat(indent + 1);
                let decls = match &self.prog.types[*ty].kind {
                    crate::ir::typed::TypeKind::Record { fields } => fields.clone(),
                    _ => panic!("rust-typed: construct of a non-record"),
                };
                for (fi, f) in fields.iter().enumerate() {
                    let v = self.expr(cx, f);
                    self.note_ty_tree(&mut *cx.uses, cx.here, &decls[fi].ty);
                    let boxed = self.boxed_field.contains(&(*ty, fi));
                    let name = &self.field_names[*ty][fi];
                    let short =
                        !boxed && matches!(f, Expr::Local(id) if cx.ctx.local(*id) == *name);
                    if short {
                        out.push(format!("{pad}{name},"));
                    } else if boxed {
                        let arg = self.boxed_arg(cx.ctx, name, f, &v, decls[fi].optional);
                        out.push(format!("{pad}{arg},"));
                    } else {
                        out.push(format!("{pad}{name}: {v},"));
                    }
                }
                out.push(format!("{}}}", "    ".repeat(indent)));
                out
            }
            Expr::Cast { expr, to } => {
                // A call inside breaks vertically and takes the suffix; any
                // other long inner keeps its parentheses.
                if let Expr::Call { func, args } = expr.as_ref() {
                    let mut lines = self.call_fit(cx, *func, args, indent);
                    let last = lines.pop().unwrap_or_default();
                    lines.push(format!("{last} as {}", self.rust_ty(to, false)));
                    return lines;
                }
                let mut lines = self.expr_fit(cx, expr, indent, used + 1);
                lines[0].insert(0, '(');
                let last = lines.len() - 1;
                lines[last] = format!("{}) as {}", lines[last], self.rust_ty(to, false));
                lines
            }
            Expr::Not(inner) => {
                if let Expr::Call { func, args } = inner.as_ref() {
                    let mut lines = self.call_fit(cx, *func, args, indent);
                    lines[0].insert(0, '!');
                    return lines;
                }
                let mut lines = self.expr_fit(cx, inner, indent, used + 2);
                lines[0].insert(0, '(');
                lines[0].insert(0, '!');
                let last = lines.len() - 1;
                lines[last].push(')');
                lines
            }
            _ => vec![one],
        }
    }

    /// One operand of a binary operator with its operator known: a mask
    /// literal prints in hex on one line, anything else like `operand_fit`.
    fn operand_fit_op(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        op: BinOp,
        e: &Expr,
        indent: usize,
        used: usize,
    ) -> Vec<String> {
        if let Expr::Int { value, .. } = e
            && hex_literal(op, *value)
        {
            let _ = (indent, used);
            return vec![hex_lit(*value)];
        }
        self.operand_fit(cx, e, indent, used)
    }

    fn operand_fit(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        e: &Expr,
        indent: usize,
        used: usize,
    ) -> Vec<String> {
        if atomic(e) || self.call_cast(cx.ctx, e) {
            return self.expr_fit(cx, e, indent, used);
        }
        let mut lines = self.expr_fit(cx, e, indent, used + 1);
        lines[0].insert(0, '(');
        let last = lines.len() - 1;
        lines[last].push(')');
        lines
    }

    /// `!x` with double-negation cancelling and flippable comparisons
    /// negated (`!(a == b)` prints as `a != b`); the rest keeps `!(..)`.
    fn not_str(&self, cx: &mut Cx<'_, '_, '_, '_>, inner: &Expr) -> String {
        match inner {
            Expr::Bool(b) => (!b).to_string(),
            Expr::Not(x) => self.expr(cx, x),
            Expr::Binary { op, lhs, rhs, .. } => {
                // `==`/`!=` are exact negations (NaN-safe); ordered
                // comparisons flip only for non-float operands, so NaN agrees.
                let floaty = [lhs, rhs].iter().any(|e| {
                    matches!(
                        self.prog.expr_ty(cx.ctx.body, e).0,
                        Ty::F32 | Ty::F64 | Ty::Named(_)
                    )
                });
                let flip = match op {
                    BinOp::Eq => Some(BinOp::Ne),
                    BinOp::Ne => Some(BinOp::Eq),
                    BinOp::Lt if !floaty => Some(BinOp::Ge),
                    BinOp::Le if !floaty => Some(BinOp::Gt),
                    BinOp::Gt if !floaty => Some(BinOp::Le),
                    BinOp::Ge if !floaty => Some(BinOp::Lt),
                    _ => None,
                };
                match flip {
                    Some(f) => {
                        let l = self.operand_str(cx, f, lhs);
                        let r = self.operand_str(cx, f, rhs);
                        format!("{l} {} {r}", f.symbol())
                    }
                    None => format!("!({})", self.expr(cx, inner)),
                }
            }
            Expr::Local(_) | Expr::Field { .. } | Expr::Call { .. } => {
                format!("!{}", self.expr(cx, inner))
            }
            _ => format!("!({})", self.expr(cx, inner)),
        }
    }

    /// One operand in single-line rendering: a mask literal in hex,
    /// otherwise parenthesized unless atomic. Single-level clarifying
    /// parentheses never trip `clippy`.
    fn operand_str(&self, cx: &mut Cx<'_, '_, '_, '_>, op: BinOp, e: &Expr) -> String {
        if let Expr::Int { value, .. } = e
            && hex_literal(op, *value)
        {
            return hex_lit(*value);
        }
        let s = self.expr(cx, e);
        if atomic(e) || self.call_cast(cx.ctx, e) {
            s
        } else {
            format!("({s})")
        }
    }

    /// Single-line binary rendering: plain operators, `bml_*` helpers where
    /// the result can overflow, and wrapping methods off `i64`.
    fn binary_str(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ty: &Ty,
        wraps: bool,
    ) -> String {
        match self.binary_form(cx.ctx, op, ty, wraps, lhs, rhs) {
            BinForm::Plain => {
                let l = self.operand_str(cx, op, lhs);
                let r = self.operand_str(cx, op, rhs);
                if matches!(op, BinOp::Shl | BinOp::Shr) {
                    format!("{l} {} {}", op.symbol(), self.shift_amount(cx, rhs))
                } else {
                    format!("{l} {} {r}", op.symbol())
                }
            }
            BinForm::Helper(callee) => {
                let l = self.expr(cx, lhs);
                let r = self.expr(cx, rhs);
                let (lt, _) = self.prog.expr_ty(cx.ctx.body, lhs);
                let (rt, _) = self.prog.expr_ty(cx.ctx.body, rhs);
                let l = wide_i64(&l, &lt, lhs);
                let r = wide_i64(&r, &rt, rhs);
                let call = format!("{callee}({l}, {r})");
                match ty {
                    Ty::Int(IntTy::I64) => call,
                    _ => format!("{call} as {}", self.rust_ty(ty, false)),
                }
            }
            BinForm::Method(callee) => {
                let l = self.expr(cx, lhs);
                if matches!(op, BinOp::Shl | BinOp::Shr) {
                    let r = self.shift_amount(cx, rhs);
                    format!("({l}).{callee}({r})")
                } else {
                    let r = self.expr(cx, rhs);
                    format!("({l}).{callee}({r})")
                }
            }
        }
    }

    /// Width-aware binary rendering: helpers break their arguments, plain
    /// operators split before the top-level operator.
    fn binary_fit(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        e: &Expr,
        indent: usize,
        used: usize,
    ) -> Vec<String> {
        let Expr::Binary {
            op,
            lhs,
            rhs,
            ty,
            wraps,
        } = e
        else {
            return vec![self.expr(cx, e)];
        };
        match self.binary_form(cx.ctx, *op, ty, *wraps, lhs, rhs) {
            BinForm::Helper(callee) => {
                let suffix = match ty {
                    Ty::Int(IntTy::I64) => String::new(),
                    _ => format!(" as {}", self.rust_ty(ty, false)),
                };
                let pad = "    ".repeat(indent + 1);
                let mut out = vec![format!("{callee}(")];
                for side in [lhs, rhs] {
                    let (st, _) = self.prog.expr_ty(cx.ctx.body, side);
                    let mut lines = self.expr_fit(cx, side, indent + 1, (indent + 1) * 4);
                    if !matches!(st, Ty::Int(IntTy::I64)) {
                        let last = lines.len() - 1;
                        if let Expr::Int { value, ty: lty } = side.as_ref() {
                            lines = vec![format!(
                                "{}_{} as i64",
                                grouped_lit(i128::from(*value)),
                                Self::int_name(*lty)
                            )];
                        } else if lossless_widen(&st, &Ty::Int(IntTy::I64)) {
                            lines[0].insert_str(0, "i64::from(");
                            lines[last].push(')');
                        } else {
                            lines[0].insert(0, '(');
                            lines[last] = format!("{}) as i64", lines[last]);
                        }
                    }
                    let last = lines.len() - 1;
                    for (i, l) in lines.iter().enumerate() {
                        if i == 0 {
                            let comma = if last == 0 { "," } else { "" };
                            out.push(format!("{pad}{l}{comma}"));
                        } else if i == last {
                            out.push(format!("{l},"));
                        } else {
                            out.push(l.clone());
                        }
                    }
                }
                if suffix.is_empty() {
                    out.push(format!("{})", "    ".repeat(indent)));
                } else {
                    out.push(format!("{}){suffix}", "    ".repeat(indent)));
                }
                let _ = used;
                out
            }
            BinForm::Method(callee) => {
                // Receiver splits inside one wrapping pair; the (short)
                // amount stays on the last line.
                let inner = self.expr_fit(cx, lhs, indent, used + 1);
                let r = if matches!(*op, BinOp::Shl | BinOp::Shr) {
                    self.shift_amount(cx, rhs)
                } else {
                    self.expr(cx, rhs)
                };
                if inner.len() == 1 {
                    return vec![format!("({}).{callee}({r})", inner[0])];
                }
                let mut out = Vec::with_capacity(inner.len());
                out.push(format!("({}", inner[0]));
                for l in &inner[1..inner.len() - 1] {
                    out.push((*l).to_string());
                }
                out.push(format!("{}).{callee}({r})", inner[inner.len() - 1]));
                out
            }
            BinForm::Plain => {
                let mut out = self.operand_fit_op(cx, *op, lhs, indent, used);
                let sym = op.symbol().to_string();
                let cont = indent + 1;
                let pad = "    ".repeat(cont);
                let r = if matches!(*op, BinOp::Shl | BinOp::Shr) {
                    self.shift_amount(cx, rhs)
                } else {
                    let rlines = self.operand_fit_op(cx, *op, rhs, cont, pad.len() + sym.len() + 1);
                    if rlines.len() == 1 {
                        rlines.into_iter().next().unwrap_or_default()
                    } else {
                        // A multiline right side keeps its own shape on the
                        // continuation line.
                        let mut first = true;
                        let mut joined = String::new();
                        for l in rlines {
                            if first {
                                joined = l;
                                first = false;
                            } else {
                                joined.push('\n');
                                joined.push_str(&l);
                            }
                        }
                        // `out` gains several lines: push them all.
                        out.push(format!("{pad}{sym} {joined}"));
                        return out;
                    }
                };
                out.push(format!("{pad}{sym} {r}"));
                out
            }
        }
    }

    /// Plain operator or runtime helper for a binary op.
    fn binary_form(
        &self,
        ctx: &Ctx<'_>,
        op: BinOp,
        ty: &Ty,
        wraps: bool,
        lhs: &Expr,
        rhs: &Expr,
    ) -> BinForm {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                if !wraps {
                    return BinForm::Plain;
                }
                if matches!(ty, Ty::Int(IntTy::I64)) {
                    BinForm::Helper(match op {
                        BinOp::Add => "bml_add",
                        BinOp::Sub => "bml_sub",
                        _ => "bml_mul",
                    })
                } else {
                    BinForm::Method(match op {
                        BinOp::Add => "wrapping_add",
                        BinOp::Sub => "wrapping_sub",
                        _ => "wrapping_mul",
                    })
                }
            }
            BinOp::Div | BinOp::Mod => {
                if self.plain_divmod(ctx, lhs, rhs) {
                    BinForm::Plain
                } else {
                    BinForm::Helper(match op {
                        BinOp::Div => "bml_div",
                        _ => "bml_mod",
                    })
                }
            }
            BinOp::Shl | BinOp::Shr => {
                if wraps {
                    BinForm::Method(match op {
                        BinOp::Shl => "wrapping_shl",
                        _ => "wrapping_shr",
                    })
                } else {
                    BinForm::Plain
                }
            }
            _ => BinForm::Plain,
        }
    }

    /// Plain `/` and `%` agree with floor/divisor-sign when the dividend
    /// cannot be negative and the divisor is positive.
    fn plain_divmod(&self, ctx: &Ctx<'_>, lhs: &Expr, rhs: &Expr) -> bool {
        let body = ctx.body;
        let l = self.prog.int_range(body, lhs);
        let r = self.prog.int_range(body, rhs);
        match (l, r) {
            (Some((llo, _)), Some((rlo, _))) => llo >= 0 && rlo > 0,
            _ => false,
        }
    }

    /// Shift amount: bare when already `u32` or a literal, else `as u32`
    /// (`wrapping_shl`/`wrapping_shr` take `u32`).
    fn shift_amount(&self, cx: &mut Cx<'_, '_, '_, '_>, rhs: &Expr) -> String {
        let (ty, _) = self.prog.expr_ty(cx.ctx.body, rhs);
        if matches!(ty, Ty::Int(IntTy::U32)) {
            return self.expr(cx, rhs);
        }
        if let Expr::Int { value, .. } = rhs {
            return grouped_lit(i128::from(*value));
        }
        let r = self.expr(cx, rhs);
        format!("{} as u32", cast_operand(rhs, &r))
    }

    /// Single-line builtin call.
    fn call_str(&self, cx: &mut Cx<'_, '_, '_, '_>, func: Func, args: &[Expr]) -> String {
        match func {
            Func::SignExtend => {
                let a = self.arg(cx, args, 0);
                let b = self.u32_arg(cx, &args[1]);
                format!("sign_extend({a}, {b})")
            }
            Func::Utf16Units => {
                let s = self.arg(cx, args, 0);
                if self.is_ref_local(cx.ctx, &args[0]) {
                    format!("utf16_units({s}) as i64")
                } else {
                    format!("utf16_units({}) as i64", borrow_of(&args[0], &s))
                }
            }
            Func::Utf8Len => format!("({}).len() as i64", self.arg(cx, args, 0)),
            Func::StrLen => {
                format!("({}).chars().count() as i64", self.arg(cx, args, 0))
            }
            Func::BytesLen => format!("({}).len() as i64", self.arg(cx, args, 0)),
            Func::ListLen => format!("({}).len() as i64", self.arg(cx, args, 0)),
            Func::Min => {
                let a = self.arg(cx, args, 0);
                let b = self.arg(cx, args, 1);
                format!("bml_min({a}, {b})")
            }
            Func::Max => {
                let a = self.arg(cx, args, 0);
                let b = self.arg(cx, args, 1);
                format!("bml_max({a}, {b})")
            }
        }
    }

    /// Width-aware builtin call: real calls break arguments one per line;
    /// inlined length helpers wrap a split argument in parentheses.
    fn call_fit(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        func: Func,
        args: &[Expr],
        indent: usize,
    ) -> Vec<String> {
        match func {
            Func::Utf8Len => self.len_fit(cx, &args[0], indent, ".len() as i64"),
            Func::StrLen => self.len_fit(cx, &args[0], indent, ".chars().count() as i64"),
            Func::BytesLen | Func::ListLen => self.len_fit(cx, &args[0], indent, ".len() as i64"),
            _ => {
                let callee = match func {
                    Func::SignExtend => "sign_extend",
                    Func::Utf16Units => "utf16_units",
                    Func::Min => "bml_min",
                    Func::Max => "bml_max",
                    _ => panic!("rust-typed: length helper in call position"),
                };
                // `utf16_units(..) as i64` keeps its suffix outside the break.
                let close = match func {
                    Func::Utf16Units => ") as i64",
                    _ => ")",
                };
                // One fitted line-block per argument in source order.
                let mut bodies: Vec<Vec<String>> = Vec::new();
                match func {
                    Func::SignExtend => {
                        bodies.push(self.fitted_arg(cx, &args[0], indent));
                        bodies.push(vec![self.u32_arg(cx, &args[1])]);
                    }
                    Func::Utf16Units => {
                        let s = self.expr(cx, &args[0]);
                        bodies.push(vec![if self.is_ref_local(cx.ctx, &args[0]) {
                            s
                        } else {
                            borrow_of(&args[0], &s)
                        }]);
                    }
                    Func::Min | Func::Max => {
                        bodies.push(self.fitted_arg(cx, &args[0], indent));
                        bodies.push(self.fitted_arg(cx, &args[1], indent));
                    }
                    _ => panic!("rust-typed: length helper in call position"),
                }
                let pad = "    ".repeat(indent + 1);
                let mut out = vec![format!("{callee}(")];
                for lines in &bodies {
                    let last = lines.len() - 1;
                    out.push(format!("{pad}{}", lines[0]));
                    for l in &lines[1..last.max(1)] {
                        out.push((*l).to_string());
                    }
                    if last > 0 {
                        out.push(format!("{},", lines[last]));
                    } else {
                        let back = out.len() - 1;
                        out[back].push(',');
                    }
                }
                out.push(format!("{}{close}", "    ".repeat(indent)));
                out
            }
        }
    }

    /// Width-aware inlined length: `(arg).len() as i64`-style with a split
    /// argument, or the single line when nothing helps.
    fn len_fit(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        arg: &Expr,
        indent: usize,
        suffix: &str,
    ) -> Vec<String> {
        let mut lines = self.operand_fit(cx, arg, indent, indent * 4 + 1);
        let last = lines.len() - 1;
        lines[last] = format!("{}{suffix}", lines[last]);
        lines
    }

    /// One argument fitted at the next indent level.
    fn fitted_arg(&self, cx: &mut Cx<'_, '_, '_, '_>, arg: &Expr, indent: usize) -> Vec<String> {
        self.expr_fit(cx, arg, indent + 1, (indent + 1) * 4)
    }

    /// `u32` argument: bare for literals and `u32` values, `as u32` else.
    fn u32_arg(&self, cx: &mut Cx<'_, '_, '_, '_>, e: &Expr) -> String {
        let (ty, _) = self.prog.expr_ty(cx.ctx.body, e);
        if matches!(ty, Ty::Int(IntTy::U32)) {
            return self.expr(cx, e);
        }
        if let Expr::Int { value, .. } = e {
            return grouped_lit(i128::from(*value));
        }
        let s = self.expr(cx, e);
        format!("{} as u32", cast_operand(e, &s))
    }
    /// One call argument as a single-line expression.
    fn arg(&self, cx: &mut Cx<'_, '_, '_, '_>, args: &[Expr], i: usize) -> String {
        self.expr(cx, &args[i])
    }

    /// Ternary with boolean simplification (`needless_bool` insurance).
    fn cond_str(
        &self,
        cx: &mut Cx<'_, '_, '_, '_>,
        cond: &Expr,
        then: &Expr,
        els: &Expr,
    ) -> String {
        if let (Expr::Bool(t), Expr::Bool(f)) = (then, els) {
            if *t && !f {
                return self.expr(cx, cond);
            }
            if !t && *f {
                return self.not_str(cx, cond);
            }
            if t == f {
                return t.to_string();
            }
        }
        let c = self.expr(cx, cond);
        let t = self.expr(cx, then);
        let f = self.expr(cx, els);
        format!("if {c} {{ {t} }} else {{ {f} }}")
    }

    /// Whether a cast renders as a call (`T::from(..)`, `from_x(..)`,
    /// `..to_x()`): a call binds tighter than any operator, so it never
    /// needs clarifying parentheses as an operand. Mirrors `cast_str`.
    fn call_cast(&self, ctx: &Ctx<'_>, e: &Expr) -> bool {
        let Expr::Cast { expr: inner, to } = e else {
            return false;
        };
        let (from, _) = self.prog.expr_ty(ctx.body, inner);
        let numeric = |t: &Ty| matches!(t, Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64);
        if numeric(&from) && numeric(to) {
            if matches!(to, Ty::Bool) {
                return false;
            }
            if let Ty::Int(_) = to
                && let Expr::Cond { then, els, .. } = inner.as_ref()
                && let Expr::Int { value: 1, .. } = then.as_ref()
                && let Expr::Int { value: 0, .. } = els.as_ref()
            {
                return true;
            }
            if !matches!(inner.as_ref(), Expr::Int { .. }) && lossless_widen(&from, to) {
                return true;
            }
            return false;
        }
        if let Ty::Named(t) = to
            && let crate::ir::typed::TypeKind::Enum { .. } = &self.prog.types[*t].kind
        {
            return true;
        }
        if let Ty::Named(t) = &from
            && let crate::ir::typed::TypeKind::Enum { .. } = &self.prog.types[*t].kind
            && numeric(to)
        {
            return true;
        }
        false
    }

    /// Casts: `T::from(..)` for lossless widening (including a `1`-vs-`0`
    /// boolean ternary), truncating `as` otherwise, `from_`/`to_` across
    /// enums. Cast operands skip their clarifying pair when they are a
    /// plain path, call, or literal (`!x` always keeps its parens: `!flag
    /// as u8` would reparse as `!(flag as u8)`).
    fn cast_str(&self, cx: &mut Cx<'_, '_, '_, '_>, e: &Expr, to: &Ty) -> String {
        let (from, _) = self.prog.expr_ty(cx.ctx.body, e);
        if &from == to {
            return self.expr(cx, e);
        }
        // Boolean ternary to an integer: `u8::from(value)`, not
        // `(if value { 1 } else { 0 }) as u8`.
        if let Ty::Int(_) = to
            && let Expr::Cond {
                cond, then, els, ..
            } = e
            && let Expr::Int { value: 1, .. } = then.as_ref()
            && let Expr::Int { value: 0, .. } = els.as_ref()
        {
            let c = self.expr(cx, cond);
            return format!("{}::from({c})", self.rust_ty(to, false));
        }
        let s = self.expr(cx, e);
        let numeric = |t: &Ty| matches!(t, Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64);
        if numeric(&from) && numeric(to) {
            // Bool conditions arrive explicit (`x != 0`); a stray int-to-bool
            // cast compares against zero.
            if matches!(to, Ty::Bool) {
                return format!("({s}) != 0");
            }
            // Literals keep their source suffix through `cast_operand`
            // (`20_i64 as usize`); `T::from(5)` would leave the literal's
            // type to inference.
            if !matches!(e, Expr::Int { .. }) && lossless_widen(&from, to) {
                return format!("{}::from({s})", self.rust_ty(to, false));
            }
            let o = cast_operand(e, &s);
            return format!("{o} as {}", self.rust_ty(to, false));
        }
        if let Ty::Named(t) = to
            && let crate::ir::typed::TypeKind::Enum { base, .. } = &self.prog.types[*t].kind
        {
            self.note_ty(&mut *cx.uses, cx.here, *t);
            return format!(
                "{}::from_{}({s})",
                self.type_name[*t],
                Self::int_name(*base)
            );
        }
        if let Ty::Named(t) = &from
            && let crate::ir::typed::TypeKind::Enum { base, .. } = &self.prog.types[*t].kind
            && numeric(to)
        {
            return format!("({s}).to_{}()", Self::int_name(*base));
        }
        s
    }
}

/// Plain operator or runtime helper call for one binary operation.
enum BinForm {
    /// `l op r` with operand parentheses.
    Plain,
    /// `bml_add(l, r)`-style helper (narrow results cast back).
    Helper(&'static str),
    /// `(l).wrapping_add(r)`-style method.
    Method(&'static str),
}

/// Borrow of an already-rendered expression: `&path` for a plain path,
/// `&(…)` otherwise (`&self.x`, never `&(self.x)`).
fn borrow_of(e: &Expr, s: &str) -> String {
    match e {
        Expr::Local(_) | Expr::Field { .. } => format!("&{s}"),
        _ => format!("&({s})"),
    }
}

/// Truncating-cast operand: a literal keeps its source suffix (`20_i64 as
/// usize` — an unsuffixed literal would infer the target and trip
/// `clippy::unnecessary_cast`), a plain path, call, or bool is bare,
/// anything else parenthesized (`!x` always keeps its parens).
fn cast_operand(e: &Expr, s: &str) -> String {
    if let Expr::Int { value, ty } = e {
        if *value == i64::MIN {
            return format!("({s})");
        }
        return format!("{}_{}", grouped_lit(i128::from(*value)), Gen::int_name(*ty));
    }
    match e {
        Expr::Local(_) | Expr::Field { .. } | Expr::Call { .. } | Expr::Bool(_) => s.to_string(),
        _ => format!("({s})"),
    }
}

fn atomic(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Int { .. }
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Local(_)
            | Expr::Field { .. }
            | Expr::Remaining
            | Expr::Call { .. }
            | Expr::StrBytes(_)
            | Expr::Present(_)
            | Expr::Not(_)
            | Expr::Get(_)
    )
}

/// `expr` widened to `i64`: bare when it already is one, a suffixed literal
/// plus `as` for literals (unsuffixed would infer `i64`), `i64::from(..)`
/// for lossless widening, truncating `as` otherwise (`u64` reinterpretation
/// and float casts keep `as`).
fn wide_i64(s: &str, ty: &Ty, e: &Expr) -> String {
    if matches!(ty, Ty::Int(IntTy::I64)) {
        s.to_string()
    } else if let Expr::Int { value, ty: lty } = e {
        format!(
            "{}_{} as i64",
            grouped_lit(i128::from(*value)),
            Gen::int_name(*lty)
        )
    } else if lossless_widen(ty, &Ty::Int(IntTy::I64)) {
        format!("i64::from({s})")
    } else {
        format!("({s}) as i64")
    }
}

/// Value range of an integer-valued type for the lossless-widening check.
fn int_bounds(ty: &Ty) -> Option<(i128, i128)> {
    match ty {
        Ty::Bool => Some((0, 1)),
        Ty::Int(IntTy::U8) => Some((0, u8::MAX as i128)),
        Ty::Int(IntTy::U16) => Some((0, u16::MAX as i128)),
        Ty::Int(IntTy::U32) => Some((0, u32::MAX as i128)),
        Ty::Int(IntTy::U64) => Some((0, u64::MAX as i128)),
        Ty::Int(IntTy::I8) => Some((i8::MIN as i128, i8::MAX as i128)),
        Ty::Int(IntTy::I16) => Some((i16::MIN as i128, i16::MAX as i128)),
        Ty::Int(IntTy::I32) => Some((i32::MIN as i128, i32::MAX as i128)),
        Ty::Int(IntTy::I64) => Some((i64::MIN as i128, i64::MAX as i128)),
        Ty::Int(IntTy::I128) => Some((i128::MIN, i128::MAX)),
        _ => None,
    }
}

/// True when every `from` value survives the trip to `to`: emit
/// `To::from(v)` there and keep `as` for the truncating rest.
fn lossless_widen(from: &Ty, to: &Ty) -> bool {
    match (int_bounds(from), int_bounds(to)) {
        (Some((flo, fhi)), Some((tlo, thi))) => flo >= tlo && fhi <= thi,
        _ => false,
    }
}

/// Hexadecimal mask literal in Rust style (`0xFF`, `0xFFFF_FFFF`): uppercase,
/// `_` every four digits once it runs long.
fn hex_lit(value: i64) -> String {
    let mut digits = format!("{:X}", value);
    if digits.len() > 4 {
        let mut out = String::new();
        while digits.len() > 4 {
            out = format!("_{}", &digits[digits.len() - 4..]) + &out;
            digits.truncate(digits.len() - 4);
        }
        digits.push_str(&out);
    }
    format!("0x{digits}")
}

/// Integer literal: unsuffixed throughout (every `let` is ascribed and every
/// other position infers from its operator or callee), decimal except via
/// [`hex_lit`] at mask operands.
fn int_lit(value: i64, ty: IntTy) -> String {
    if value == i64::MIN {
        if ty == IntTy::I128 {
            return "(i64::MIN as i128)".to_string();
        }
        return "i64::MIN".to_string();
    }
    grouped_lit(i128::from(value))
}

/// Whether a statement list diverges (no fallthrough): a trailing `Return`
/// or `Fail`, or an `If`/`Match` whose every reachable branch diverges. A
/// `Match` without `other` gains a diverging error arm from the printer
/// unless its arms already cover a boolean scrutinee.
fn diverges(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return(_)) | Some(Stmt::Fail(_)) => true,
        Some(Stmt::If { then, els, .. }) => !els.is_empty() && diverges(then) && diverges(els),
        Some(Stmt::Match { arms, other, .. }) => {
            arms.iter().all(|a| diverges(&a.body)) && other.as_ref().is_some_and(|o| diverges(o))
        }
        _ => false,
    }
}

/// Zero-initialized locals whose initial value can be observed: a read
/// reached without a dominating store (must-stored forward analysis with
/// intersection joins; loop bodies keep the entry state since they may run
/// zero times). Every other zero-init declares deferred (`let l: T;`),
/// which silences `unused_assignments` without risking `possibly-uninitialized`.
fn live_inits(body: &Body) -> HashSet<LocalId> {
    struct Walk {
        stored: HashSet<LocalId>,
        live: HashSet<LocalId>,
    }
    impl Walk {
        fn use_expr(&mut self, e: &Expr) {
            match e {
                Expr::Local(id) => {
                    if !self.stored.contains(id) {
                        self.live.insert(*id);
                    }
                }
                Expr::Field { base, .. } => self.use_expr(base),
                Expr::Present(x)
                | Expr::Get(x)
                | Expr::Not(x)
                | Expr::StrBytes(x)
                | Expr::Cast { expr: x, .. } => self.use_expr(x),
                Expr::Binary { lhs, rhs, .. } => {
                    self.use_expr(lhs);
                    self.use_expr(rhs);
                }
                Expr::Cond {
                    cond, then, els, ..
                } => {
                    self.use_expr(cond);
                    self.use_expr(then);
                    self.use_expr(els);
                }
                Expr::Call { args, .. } => {
                    for a in args {
                        self.use_expr(a);
                    }
                }
                Expr::Construct { fields, .. } => {
                    for f in fields {
                        self.use_expr(f);
                    }
                }
                Expr::Variant {
                    payload: Some(p), ..
                } => {
                    self.use_expr(p);
                }
                _ => {}
            }
        }
        fn use_stmts(&mut self, stmts: &[Stmt]) {
            for s in stmts {
                match s {
                    Stmt::Declare { local, .. } => {
                        self.stored.remove(local);
                    }
                    Stmt::Let { dst, value } => {
                        self.use_expr(value);
                        self.stored.insert(dst.local);
                    }
                    Stmt::Read { dst, op } => {
                        if let ReadOp::Bytes(n) = op {
                            self.use_expr(n);
                        }
                        self.stored.insert(dst.local);
                    }
                    Stmt::Decode { dst, args, .. } => {
                        for a in args {
                            self.use_expr(a);
                        }
                        self.stored.insert(dst.local);
                    }
                    Stmt::Utf8 { dst, src } => {
                        self.use_expr(src);
                        self.stored.insert(dst.local);
                    }
                    Stmt::Write { op, value } => {
                        if let crate::ir::typed::WriteOp::Bytes(n) = op {
                            self.use_expr(n);
                        }
                        self.use_expr(value);
                    }
                    Stmt::Encode { args, value, .. } => {
                        self.use_expr(value);
                        for a in args {
                            self.use_expr(a);
                        }
                    }
                    Stmt::Require { cond, .. } => self.use_expr(cond),
                    Stmt::Unwrap { dst, src, .. } => {
                        self.use_expr(src);
                        self.stored.insert(dst.local);
                    }
                    Stmt::If { cond, then, els } => {
                        self.use_expr(cond);
                        let entry = self.stored.clone();
                        self.use_stmts(then);
                        let then_out = std::mem::replace(&mut self.stored, entry.clone());
                        self.use_stmts(els);
                        let els_out = std::mem::replace(&mut self.stored, entry);
                        self.stored = then_out.intersection(&els_out).copied().collect();
                    }
                    Stmt::Match {
                        scrutinee,
                        arms,
                        other,
                    } => {
                        self.use_expr(scrutinee);
                        let entry = self.stored.clone();
                        let mut exits = Vec::new();
                        for a in arms {
                            self.stored = entry.clone();
                            self.use_stmts(&a.body);
                            if !diverges(&a.body) {
                                exits.push(std::mem::take(&mut self.stored));
                            }
                        }
                        if let Some(o) = other {
                            self.stored = entry.clone();
                            self.use_stmts(o);
                            if !diverges(o) {
                                exits.push(std::mem::take(&mut self.stored));
                            }
                        }
                        // Without `other` the printer's trailing error arm
                        // diverges (or the boolean arms already cover every
                        // value), so only named fall-through arms continue.
                        self.stored = match exits.split_first() {
                            None => entry,
                            Some((first, rest)) => rest.iter().fold(first.clone(), |acc, s| {
                                acc.intersection(s).copied().collect()
                            }),
                        };
                    }
                    Stmt::Repeat { mode, body, .. } => {
                        match mode {
                            RepeatMode::Count(n) | RepeatMode::Until(n) => self.use_expr(n),
                            RepeatMode::UntilEnd => {}
                        }
                        let entry = self.stored.clone();
                        self.use_stmts(body);
                        self.stored = entry;
                    }
                    Stmt::ForEach { list, body, .. } => {
                        self.use_expr(list);
                        let entry = self.stored.clone();
                        self.use_stmts(body);
                        self.stored = entry;
                    }
                    Stmt::ExpectVariant { dst, sum, .. } => {
                        self.use_expr(sum);
                        if let Some(d) = dst {
                            self.stored.insert(d.local);
                        }
                    }
                    Stmt::Return(e) => self.use_expr(e),
                    Stmt::Fail(_) => {}
                }
            }
        }
    }
    let mut walk = Walk {
        stored: HashSet::new(),
        live: HashSet::new(),
    };
    walk.use_stmts(&body.stmts);
    walk.live
}

/// Whether an expression reads `Remaining` (the cursor) anywhere inside.
fn has_remaining(e: &Expr) -> bool {
    match e {
        Expr::Remaining => true,
        Expr::Field { base, .. } => has_remaining(base),
        Expr::Present(x)
        | Expr::Get(x)
        | Expr::Not(x)
        | Expr::StrBytes(x)
        | Expr::Cast { expr: x, .. } => has_remaining(x),
        Expr::Binary { lhs, rhs, .. } => has_remaining(lhs) || has_remaining(rhs),
        Expr::Cond {
            cond, then, els, ..
        } => has_remaining(cond) || has_remaining(then) || has_remaining(els),
        Expr::Call { args, .. } => args.iter().any(has_remaining),
        Expr::Construct { fields, .. } => fields.iter().any(has_remaining),
        Expr::Variant { payload, .. } => payload.as_ref().is_some_and(|p| has_remaining(p)),
        _ => false,
    }
}

/// Whether a body touches the cursor: any wire or codec op, or `Remaining`.
/// Bodies that do not (empty records, pure computations) take `_r`/`_w`.
fn touches_cursor(body: &Body) -> bool {
    let mut hit = false;
    walk_stmts(&body.stmts, &mut |s| {
        if hit {
            return;
        }
        match s {
            Stmt::Read { .. }
            | Stmt::Decode { .. }
            | Stmt::Utf8 { .. }
            | Stmt::Write { .. }
            | Stmt::Encode { .. } => hit = true,
            Stmt::Repeat { mode, .. } => match mode {
                // `until` loops always peek; counted loops touch the
                // cursor only through their bound items.
                RepeatMode::Count(n) | RepeatMode::Until(n) => {
                    if has_remaining(n) {
                        hit = true;
                    }
                }
                RepeatMode::UntilEnd => hit = true,
            },
            Stmt::ForEach { list, .. } => {
                if has_remaining(list) {
                    hit = true;
                }
            }
            Stmt::Require { cond, .. } | Stmt::Unwrap { src: cond, .. } => {
                if has_remaining(cond) {
                    hit = true;
                }
            }
            Stmt::Let { value, .. } | Stmt::Return(value) => {
                if has_remaining(value) {
                    hit = true;
                }
            }
            Stmt::If { cond, .. } => {
                if has_remaining(cond) {
                    hit = true;
                }
            }
            Stmt::Match { scrutinee, .. } if has_remaining(scrutinee) => {
                hit = true;
            }
            _ => {}
        }
    });
    hit
}

impl<'a> Gen<'a> {
    /// Whether the rendered expression already has reference type: the
    /// method receiver, a borrowed free-encode value, or a borrowed
    /// `for` item. No schema name renders as bare `self`, so that test
    /// only matches the value local of methods.
    fn is_ref_local(&self, ctx: &Ctx<'_>, e: &Expr) -> bool {
        let Expr::Local(id) = e else {
            return false;
        };
        if ctx.mode == Mode::Encode && Some(*id) == ctx.body.value && ctx.local(*id) == "self" {
            return true;
        }
        let (ty, _) = self.prog.expr_ty(ctx.body, e);
        if self.copy_ty_in(&ty) {
            return false;
        }
        if ctx.mode == Mode::Encode && Some(*id) == ctx.body.value {
            return true;
        }
        ctx.for_item.contains(id) || self.bound_by_ref(ctx, *id)
    }

    /// Whether `local` is a non-`Copy` destination the printer bound as a
    /// reference: an `Unwrap` through `.as_ref()`, or an `ExpectVariant`
    /// payload it bound as `v`. Uses must not take another `&`.
    fn bound_by_ref(&self, ctx: &Ctx<'_>, local: LocalId) -> bool {
        let mut hit = false;
        walk_stmts(&ctx.body.stmts, &mut |s| match s {
            Stmt::Unwrap { dst, .. } => hit |= dst.local == local,
            Stmt::ExpectVariant { dst: Some(d), .. } => hit |= d.local == local,
            _ => {}
        });
        hit && self.borrowed_binding(ctx, local)
    }

    /// Whether a `for` item is used where ownership is needed, forcing
    /// `.iter().copied()`; pure borrows stay with `.iter()`
    /// (`unnecessary_to_owned` requires the distinction).
    fn foreach_moves(&self, stmts: &[Stmt], item: LocalId) -> bool {
        let mut hit = false;
        walk_stmts(stmts, &mut |s| {
            if hit {
                return;
            }
            match s {
                Stmt::Let { value, .. } | Stmt::Return(value) => {
                    hit |= expr_moves_item(value, item);
                }
                Stmt::Read {
                    op: ReadOp::Bytes(n),
                    ..
                } => {
                    hit |= expr_moves_item(n, item);
                }
                Stmt::Decode { args, .. } => {
                    hit |= args.iter().any(|a| expr_moves_item(a, item));
                }
                Stmt::Utf8 { src, .. } => {
                    hit |= expr_moves_item(src, item);
                }
                Stmt::Write { op, value } => {
                    if let crate::ir::typed::WriteOp::Bytes(n) = op {
                        hit |= expr_moves_item(n, item);
                    }
                    hit |= expr_moves_item(value, item);
                }
                Stmt::Encode { codec, args, value } => {
                    let callee = &self.prog.codecs[*codec];
                    if is_method(&callee.kind) {
                        hit |= moves_nonbare(value, item);
                    } else {
                        hit |= expr_moves_item(value, item);
                    }
                    hit |= args.iter().any(|a| expr_moves_item(a, item));
                }
                Stmt::Require { cond, .. }
                | Stmt::Unwrap { src: cond, .. }
                | Stmt::If { cond, .. }
                | Stmt::Match {
                    scrutinee: cond, ..
                } => {
                    hit |= expr_moves_item(cond, item);
                }
                Stmt::ExpectVariant { sum, .. } => {
                    hit |= expr_moves_item(sum, item);
                }
                Stmt::Repeat { mode, .. } => match mode {
                    RepeatMode::Count(n) | RepeatMode::Until(n) => {
                        hit |= expr_moves_item(n, item);
                    }
                    RepeatMode::UntilEnd => {}
                },
                Stmt::ForEach { list, .. } => {
                    hit |= expr_moves_item(list, item);
                }
                _ => {}
            }
        });
        hit
    }
}

/// Owned-position scan: true if `item` is used where ownership is needed.
fn expr_moves_item(e: &Expr, item: LocalId) -> bool {
    match e {
        Expr::Local(id) => *id == item,
        // Borrowed whole: comparisons, presence, and method receivers only
        // borrow a bare item; deeper expressions still move.
        Expr::Present(x) | Expr::Get(x) => {
            let _ = x;
            false
        }
        Expr::Binary { op, lhs, rhs, .. } if op.is_comparison() => {
            moves_nonbare(lhs, item) || moves_nonbare(rhs, item)
        }
        Expr::Binary { lhs, rhs, .. } => expr_moves_item(lhs, item) || expr_moves_item(rhs, item),
        Expr::Cond {
            cond, then, els, ..
        } => {
            expr_moves_item(cond, item) || expr_moves_item(then, item) || expr_moves_item(els, item)
        }
        Expr::Not(x)
        | Expr::Cast { expr: x, .. }
        | Expr::StrBytes(x)
        | Expr::Field { base: x, .. } => expr_moves_item(x, item),
        Expr::Call { args, .. } => args.iter().any(|a| expr_moves_item(a, item)),
        Expr::Construct { fields, .. } => fields.iter().any(|f| expr_moves_item(f, item)),
        Expr::Variant { payload, .. } => payload.as_ref().is_some_and(|p| expr_moves_item(p, item)),
        _ => false,
    }
}

/// Like [`expr_moves_item`], but a bare `item` is borrowed (method
/// receivers, `&` arguments, write values, comparison operands).
fn moves_nonbare(e: &Expr, item: LocalId) -> bool {
    if matches!(e, Expr::Local(id) if *id == item) {
        return false;
    }
    expr_moves_item(e, item)
}

/// Match pattern for integer tags: consecutive runs collapse to `lo..=hi`
/// (`manual_range_patterns` requires it); anything else joins with ` | `.
fn int_pattern(tags: &[i128]) -> String {
    let mut sorted = tags.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = Vec::new();
    let mut run: Option<(i128, i128)> = None;
    let flush = |run: Option<(i128, i128)>, parts: &mut Vec<String>| {
        if let Some((lo, hi)) = run {
            if lo == hi {
                parts.push(lo.to_string());
            } else {
                parts.push(format!("{lo}..={hi}"));
            }
        }
    };
    for t in sorted {
        match run {
            Some((lo, hi)) if t == hi + 1 => run = Some((lo, t)),
            _ => {
                flush(run, &mut parts);
                run = Some((t, t));
            }
        }
    }
    flush(run, &mut parts);
    if parts.is_empty() {
        return "_".to_string();
    }
    parts.join(" | ")
}
