//! `emit python`: a stdlib-only `bpsgen.py` printed from the typed program.
//!
//! Every codec becomes typed Python: records are `@dataclasses.dataclass`
//! classes with `decode`/`encode` methods, sums are variant dataclasses plus
//! a `TypeAlias` union decoded and encoded by module functions taking the tag,
//! open enums are `enum.IntEnum` with unknown-preserving decode, and scalar
//! or string encodings are `decode_*`/`encode_*` module functions. There is
//! no environment, no dynamic value, no reflection, and no global state:
//! locals of the typed program print as plain annotated variables.
//!
//! Errors carry the offset where they surface: reads and calls print
//! unwrapped, and each error raised by a codec body is tagged with that
//! codec at its raise (`raise X(...).in_codec("name")`). There is no
//! `try/except` anywhere in generated codec code. The emitted file passes
//! `python3 -m py_compile`, `ruff check`, `ruff format --check`, and
//! `mypy --strict`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::ResolvedSchema;
use crate::errors::{BpsResult, emit};
use crate::ir::typed::{
    BinOp, Bind, Body, Codec, CodecIdx, CodecKind, Expr, Failure, Func, IntTy, LocalId, ParamRole,
    Program, ReadOp, RepeatMode, Stmt, Ty, TypeIdx, WriteOp,
};
use crate::ir::typed::{Origin, TypeKind};

/// Python keywords plus the soft keywords the guide escapes with a trailing `_`.
const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "match", "case", "type",
];

/// Lines longer than this are split the way `ruff format` would split them.
const MAX_WIDTH: usize = 88;

const RUNTIME: &str = r#"class BpsError(Exception):
    """Base error for codec failures; ``codecs`` holds the inner-to-outer path."""

    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.codecs: list[str] = []

    def in_codec(self, codec: str) -> Self:
        """Record that the failure propagated through ``codec``."""
        if not self.codecs or self.codecs[-1] != codec:
            self.codecs.append(codec)
        return self

    def __str__(self) -> str:
        base = super().__str__()
        return f"{base} [in {' <- '.join(self.codecs)}]" if self.codecs else base


class ShortBuffer(BpsError):
    """Not enough bytes left: needed ``need``, had ``have``, at ``offset``."""

    def __init__(self, offset: int, need: int, have: int) -> None:
        super().__init__(f"short buffer at offset {offset}: need {need}, have {have}")


class RequireFailed(BpsError):
    """A ``require`` did not hold: schema text in ``condition``."""

    def __init__(self, offset: int, condition: str) -> None:
        super().__init__(f"require failed at offset {offset}: {condition}")


class InvalidUtf8(BpsError):
    """Bytes at ``offset`` are not valid UTF-8."""

    def __init__(self, offset: int) -> None:
        super().__init__(f"invalid UTF-8 at offset {offset}")


class NoMatchArm(BpsError):
    """A ``match`` had no arm for ``tag`` (decode) or no variant fit (encode)."""

    def __init__(self, offset: int, tag: int) -> None:
        super().__init__(f"no match arm for tag {tag} at offset {offset}")


class MissingField(BpsError):
    """An absent optional field was needed: schema field ``field``."""

    def __init__(self, offset: int, field: str) -> None:
        super().__init__(f"missing optional field at offset {offset}: {field}")


class WrongVariant(BpsError):
    """On encode, the value was not the variant the tag selects."""

    def __init__(self, offset: int, expected: str, tag: int) -> None:
        super().__init__(
            f"wrong sum variant at offset {offset}: tag {tag} is not {expected}"
        )


class NegativeLength(BpsError):
    """A decoded length/count prefix was negative."""

    def __init__(self, offset: int, length: int) -> None:
        super().__init__(f"negative length at offset {offset}: {length}")


class BytesTooLong(BpsError):
    """A ``bytes(n)`` value of ``length`` bytes does not fit in ``size``."""

    def __init__(self, offset: int, length: int, size: int) -> None:
        super().__init__(f"bytes too long at offset {offset}: {length} > {size}")


class EncodeUntilEnd(BpsError):
    """A repeat-until-end list cannot be encoded (no terminator to write)."""

    def __init__(self) -> None:
        super().__init__("cannot encode a repeat-until-end list")


class Reader:
    """Big-endian cursor over an immutable byte buffer."""

    __slots__ = ("data", "pos")

    def __init__(self, data: bytes) -> None:
        self.data = data
        self.pos = 0

    @property
    def remaining(self) -> int:
        """Bytes left unread."""
        return len(self.data) - self.pos

    def read_u8(self) -> int:
        """Read one unsigned byte."""
        if self.pos >= len(self.data):
            raise ShortBuffer(self.pos, 1, 0)
        byte = self.data[self.pos]
        self.pos += 1
        return byte

    def peek_u8(self) -> int:
        """Look at the next byte without consuming it (until-terminators)."""
        if self.pos >= len(self.data):
            raise ShortBuffer(self.pos, 1, 0)
        return self.data[self.pos]

    def read_bytes(self, n: int) -> bytes:
        """Read exactly ``n`` bytes."""
        if n < 0:
            raise NegativeLength(self.pos, n)
        if self.pos + n > len(self.data):
            raise ShortBuffer(self.pos, n, self.remaining)
        chunk = self.data[self.pos : self.pos + n]
        self.pos += n
        return chunk

    def read_rest(self) -> bytes:
        """Read all remaining bytes."""
        chunk = self.data[self.pos :]
        self.pos = len(self.data)
        return chunk

    def read_f32(self) -> float:
        """Read a big-endian 32-bit float."""
        return float(struct.unpack(">f", self.read_bytes(4))[0])

    def read_f64(self) -> float:
        """Read a big-endian 64-bit float."""
        return float(struct.unpack(">d", self.read_bytes(8))[0])


class Writer:
    """Byte buffer writer; ``get_bytes()`` freezes the output."""

    __slots__ = ("buf",)

    def __init__(self) -> None:
        self.buf = bytearray()

    @property
    def offset(self) -> int:
        """Bytes written so far (encode-side error offsets)."""
        return len(self.buf)

    def write_u8(self, value: int) -> None:
        """Write the low 8 bits of ``value`` (narrowing masks on the caller)."""
        self.buf.append(value & 0xFF)

    def write_bytes(self, data: bytes) -> None:
        """Append ``data`` verbatim."""
        self.buf.extend(data)

    def write_bytes_fixed(self, data: bytes, size: int) -> None:
        """Append ``data`` zero-padded to ``size``; fail when longer."""
        if len(data) > size:
            raise BytesTooLong(self.offset, len(data), size)
        self.buf.extend(data)
        self.buf.extend(b"\x00" * (size - len(data)))

    def write_f32(self, value: float) -> None:
        """Write ``value`` as a big-endian 32-bit float."""
        self.buf.extend(struct.pack(">f", value))

    def write_f64(self, value: float) -> None:
        """Write ``value`` as a big-endian 64-bit float."""
        self.buf.extend(struct.pack(">d", value))

    def get_bytes(self) -> bytes:
        """Return the written bytes."""
        return bytes(self.buf)


def sign_extend(value: int, bits: int) -> int:
    """Two's-complement sign extension of ``value`` from ``bits`` width."""
    return value - (1 << bits) if value & (1 << (bits - 1)) else value


def utf16_units(text: str) -> int:
    """UTF-16 code units in ``text`` (astral characters count 2)."""
    return sum(2 if ord(char) > 0xFFFF else 1 for char in text)


def decode_utf8(data: bytes, offset: int) -> str:
    """Decode UTF-8, raising ``InvalidUtf8`` on bad bytes."""
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise InvalidUtf8(offset) from exc
"#;

const MISSING: &str = r#"@classmethod
def _missing_(cls, value: object) -> Self | None:
    if isinstance(value, int):
        unknown = int.__new__(cls, value)
        unknown._name_ = f"UNKNOWN_{value}"
        return unknown
    return None
"#;

/// Emit `bpsgen.py` for `resolved` into `out_dir`.
pub fn emit_python(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    let program = crate::ir::typed(resolved)?;
    let src = emit_program(&program)?;
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("bpsgen.py"), src).map_err(|e| emit(e.to_string()))?;
    Ok(())
}

/// The emitted identifier of a codec's decode: `decode_x` for functions,
/// `Cls.decode` for record methods.
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    let c = &program.codecs[codec];
    if c.kind == CodecKind::Record {
        let ty = c.ty.expect("record codec has a type");
        format!("{}.decode", program.types[ty].name.pascal())
    } else {
        format!("decode_{}", c.name.snake())
    }
}

/// The emitted identifier of a codec's encode: `encode_x` for functions,
/// `Cls.encode` for record methods.
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    let c = &program.codecs[codec];
    if c.kind == CodecKind::Record {
        let ty = c.ty.expect("record codec has a type");
        format!("{}.encode", program.types[ty].name.pascal())
    } else {
        format!("encode_{}", c.name.snake())
    }
}

fn emit_program(prog: &Program) -> BpsResult<String> {
    let mut e = Emitter::new(prog);
    e.header();
    e.runtime();
    e.records_and_enums();
    e.aliases();
    e.sums();
    e.codecs()?;
    Ok(e.finish())
}

struct Emitter<'a> {
    prog: &'a Program,
    buf: String,
    indent: usize,
}

impl<'a> Emitter<'a> {
    fn new(prog: &'a Program) -> Self {
        Emitter {
            prog,
            buf: String::new(),
            indent: 0,
        }
    }

    fn finish(self) -> String {
        self.buf
    }

    fn pad(&self) -> String {
        "    ".repeat(self.indent)
    }

    fn line(&mut self, s: &str) {
        if s.is_empty() {
            self.buf.push('\n');
        } else {
            self.buf.push_str(&self.pad());
            self.buf.push_str(s);
            self.buf.push('\n');
        }
    }

    fn blank(&mut self) {
        self.buf.push('\n');
    }

    fn nested(&mut self, f: impl FnOnce(&mut Self)) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }

    /// The full line fits the width gate.
    fn fits(&self, s: &str) -> bool {
        self.pad().len() + s.len() <= MAX_WIDTH
    }

    // ---------------------------------------------------------------
    // Top level
    // ---------------------------------------------------------------

    fn header(&mut self) {
        let mut dataclasses = false;
        let mut enums = false;
        let mut aliases = false;
        let mut selves = false;
        for t in &self.prog.types {
            match &t.kind {
                TypeKind::Record { .. } => {
                    dataclasses = true;
                    selves = true;
                }
                TypeKind::Sum { variants } => {
                    aliases = true;
                    if variants
                        .iter()
                        .any(|v| !is_record_payload(self.prog, &v.payload))
                    {
                        dataclasses = true;
                    }
                }
                TypeKind::Enum { .. } => {
                    enums = true;
                    selves = true;
                }
                TypeKind::Alias { .. } => {}
            }
        }
        self.line("from __future__ import annotations");
        self.blank();
        if dataclasses {
            self.line("import dataclasses");
        }
        if enums {
            self.line("import enum");
        }
        self.line("import struct");
        if selves && aliases {
            self.line("from typing import Self, TypeAlias");
        } else if selves {
            self.line("from typing import Self");
        } else if aliases {
            self.line("from typing import TypeAlias");
        }
        self.blank();
        self.blank();
    }

    fn runtime(&mut self) {
        for line in RUNTIME.lines() {
            self.line(line.trim_end());
        }
        self.blank();
        self.blank();
    }

    fn records_and_enums(&mut self) {
        let mut first = true;
        for idx in 0..self.prog.types.len() {
            match &self.prog.types[idx].kind {
                TypeKind::Record { .. } => {
                    if !first {
                        self.blank();
                        self.blank();
                    }
                    first = false;
                    self.record(idx);
                }
                TypeKind::Enum { .. } => {
                    if !first {
                        self.blank();
                        self.blank();
                    }
                    first = false;
                    self.enum_type(idx);
                }
                TypeKind::Sum { .. } | TypeKind::Alias { .. } => {}
            }
        }
        if !first {
            self.blank();
            self.blank();
        }
    }

    fn type_doc(&self, idx: TypeIdx) -> String {
        let t = &self.prog.types[idx];
        match &t.origin {
            Origin::Packet { name, routes } => {
                if routes.is_empty() {
                    format!("Packet {name}.")
                } else {
                    let list = routes
                        .iter()
                        .map(|r| format!("{}/{} {}", r.state, r.direction, r.id))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("Packet {name} ({list}).")
                }
            }
            Origin::Encoding { name } => format!("Encoding {name}."),
            Origin::Enum { name } => format!("Open enum {name}."),
            Origin::Frame { name } => format!("Frame {name}."),
            Origin::Field { parent, field } => {
                format!(
                    "Block {field} of {}.",
                    self.prog.types[*parent].name.pascal()
                )
            }
            Origin::Variant { sum, variant } => match &self.prog.types[*sum].kind {
                TypeKind::Sum { variants } => format!(
                    "Arm {} of {}.",
                    variants[*variant].name.pascal(),
                    self.prog.types[*sum].name.pascal()
                ),
                _ => "Sum arm.".to_string(),
            },
        }
    }

    fn record(&mut self, idx: TypeIdx) {
        let cls = self.prog.types[idx].name.pascal();
        let doc = self.type_doc(idx);
        let fields = match &self.prog.types[idx].kind {
            TypeKind::Record { fields } => fields.clone(),
            _ => return,
        };
        self.line("@dataclasses.dataclass(slots=True, kw_only=True)");
        self.line(&format!("class {cls}:"));
        self.nested(|e| {
            e.line(&format!("\"\"\"{doc}\"\"\""));
            // `ruff format` keeps a blank line between a class docstring
            // and the body that follows it.
            if !fields.is_empty() {
                e.blank();
            }
            for f in &fields {
                let name = escape_name(&f.name.snake());
                let ann = annotation(self.prog, &f.ty, f.optional);
                if f.optional {
                    e.line(&format!("{name}: {ann} = None"));
                } else {
                    e.line(&format!("{name}: {ann}"));
                }
            }
        });
        let codec = self.prog.types[idx].codec;
        if let Some(codec) = codec {
            self.methods(codec);
        }
    }

    fn enum_type(&mut self, idx: TypeIdx) {
        let cls = self.prog.types[idx].name.pascal();
        let doc = self.type_doc(idx);
        let members = match &self.prog.types[idx].kind {
            TypeKind::Enum { members, .. } => members.clone(),
            _ => Vec::new(),
        };
        self.line(&format!("class {cls}(enum.IntEnum):"));
        self.nested(|e| {
            e.line(&format!("\"\"\"{doc}\"\"\""));
            e.blank();
            for m in &members {
                e.line(&format!("{} = {}", m.name.screaming(), m.value));
            }
            e.blank();
            for line in MISSING.lines() {
                e.line(line.trim_end());
            }
        });
    }

    /// `X = Y` record aliases, in IR order (targets are records: defined).
    fn aliases(&mut self) {
        let mut first = true;
        for idx in 0..self.prog.types.len() {
            let target = match self.prog.types[idx].kind {
                TypeKind::Alias { target } => target,
                _ => continue,
            };
            if !first {
                self.blank();
                self.blank();
            }
            first = false;
            let name = self.prog.types[idx].name.pascal();
            let target_name = self.prog.types[target].name.pascal();
            let doc = self.type_doc(idx);
            self.line(&format!("{name} = {target_name}"));
            self.line(&format!("\"\"\"{doc}\"\"\""));
        }
        if !first {
            self.blank();
            self.blank();
        }
    }

    /// Sum unions in dependency order: a sum whose members name another sum
    /// emits after it (the alias evaluates at runtime).
    fn sums(&mut self) {
        let order = self.sum_order();
        let mut first = true;
        for idx in order {
            if !first {
                self.blank();
                self.blank();
            }
            first = false;
            self.sum(idx);
        }
        if !first {
            self.blank();
            self.blank();
        }
    }

    fn sum_order(&self) -> Vec<TypeIdx> {
        let sums: Vec<TypeIdx> = (0..self.prog.types.len())
            .filter(|t| matches!(self.prog.types[*t].kind, TypeKind::Sum { .. }))
            .collect();
        let in_set: HashSet<TypeIdx> = sums.iter().copied().collect();
        let mut deps: HashMap<TypeIdx, Vec<TypeIdx>> = HashMap::new();
        for s in &sums {
            let mut ds = Vec::new();
            if let TypeKind::Sum { variants } = &self.prog.types[*s].kind {
                for v in variants {
                    if let Some(Ty::Named(t)) = &v.payload
                        && matches!(self.prog.types[*t].kind, TypeKind::Sum { .. })
                        && in_set.contains(t)
                        && *t != *s
                    {
                        ds.push(*t);
                    }
                }
            }
            deps.insert(*s, ds);
        }
        let mut order = Vec::new();
        let mut done: HashSet<TypeIdx> = HashSet::new();
        while order.len() < sums.len() {
            let mut moved = false;
            for s in &sums {
                if done.contains(s) {
                    continue;
                }
                if deps[s].iter().all(|d| done.contains(d)) {
                    done.insert(*s);
                    order.push(*s);
                    moved = true;
                }
            }
            if !moved {
                for s in &sums {
                    if done.insert(*s) {
                        order.push(*s);
                    }
                }
            }
        }
        order
    }

    fn sum(&mut self, idx: TypeIdx) {
        let name = self.prog.types[idx].name.pascal();
        let variants = match &self.prog.types[idx].kind {
            TypeKind::Sum { variants } => variants.clone(),
            _ => return,
        };
        for v in &variants {
            if is_record_payload(self.prog, &v.payload) {
                continue;
            }
            let cls = format!("{name}{}", v.name.pascal());
            self.line("@dataclasses.dataclass(slots=True, kw_only=True)");
            self.line(&format!("class {cls}:"));
            self.nested(|e| {
                e.line(&format!(
                    "\"\"\"Variant {} of {name}.\"\"\"",
                    v.name.pascal()
                ));
                if let Some(payload) = &v.payload {
                    e.blank();
                    e.line(&format!("value: {}", annotation(self.prog, payload, false)));
                }
            });
            self.blank();
            self.blank();
        }
        let mut members: Vec<String> = Vec::new();
        for v in &variants {
            let member = self.variant_class(idx, v);
            // Several tags can share one payload type (particle data reuses
            // block records); the union lists each type once, in arm order.
            if !members.contains(&member) {
                members.push(member);
            }
        }
        let single = format!("{name}: TypeAlias = {}", members.join(" | "));
        if self.fits(&single) {
            self.line(&single);
        } else {
            self.line(&format!("{name}: TypeAlias = ("));
            self.nested(|e| {
                for (i, m) in members.iter().enumerate() {
                    if i == 0 {
                        e.line(m);
                    } else {
                        e.line(&format!("| {m}"));
                    }
                }
            });
            self.line(")");
        }
        self.line(&format!("\"\"\"{name}: a union of its variants.\"\"\""));
    }

    /// The Python class holding one variant's value: the record itself for
    /// record payloads, a generated wrapper otherwise.
    fn variant_class(&self, sum: TypeIdx, v: &crate::ir::typed::VariantDecl) -> String {
        match &v.payload {
            Some(Ty::Named(t)) if matches!(self.prog.types[*t].kind, TypeKind::Record { .. }) => {
                self.prog.types[*t].name.pascal()
            }
            _ => format!("{}{}", self.prog.types[sum].name.pascal(), v.name.pascal()),
        }
    }

    // ---------------------------------------------------------------
    // Codecs
    // ---------------------------------------------------------------

    fn codecs(&mut self) -> BpsResult<()> {
        // Record codecs print as methods on their class; only module
        // functions emit here (and take separators).
        let mut first = true;
        for idx in 0..self.prog.codecs.len() {
            if self.prog.codecs[idx].kind == CodecKind::Record {
                continue;
            }
            if !first {
                self.blank();
                self.blank();
            }
            first = false;
            self.codec(idx)?;
        }
        Ok(())
    }

    fn ctx_for(&self, c: &Codec, body: &Body, decode: bool, method: bool) -> FCtx {
        let tag = if decode {
            None
        } else {
            c.params
                .iter()
                .position(|p| matches!(p.role, ParamRole::Tag))
                .and_then(|i| body.params.get(i).copied())
        };
        // The printer owns `r`/`w`/`cls`/`self` (and the value parameter), so
        // a schema local with one of those names prints suffixed instead.
        let fixed: HashSet<String> = if decode {
            let mut fixed = HashSet::from(["r".to_string()]);
            if method {
                fixed.insert("cls".to_string());
            }
            fixed
        } else if method {
            HashSet::from(["w".to_string(), "self".to_string()])
        } else {
            let value = body
                .value
                .map(|id| escape_name(&body.locals[id].name.snake()))
                .unwrap_or_default();
            HashSet::from(["w".to_string(), value])
        };
        let mut taken: HashSet<String> = fixed.clone();
        for l in &body.locals {
            taken.insert(escape_name(&l.name.snake()));
        }
        let mut names = HashMap::new();
        for (id, l) in body.locals.iter().enumerate() {
            let mut printed = escape_name(&l.name.snake());
            // The encode input owns the signature's value name; only other
            // locals yield to the printer-owned names.
            if fixed.contains(&printed) && Some(id) != body.value {
                printed = fresh(&taken, &printed);
            }
            taken.insert(printed.clone());
            names.insert(id, printed);
        }
        FCtx {
            cname: c.name.snake(),
            decode,
            method,
            body: Body {
                params: body.params.clone(),
                value: body.value,
                locals: body.locals.clone(),
                stmts: Vec::new(),
            },
            tag,
            names,
            taken,
        }
    }

    fn methods(&mut self, codec: CodecIdx) {
        let c = self.prog.codecs[codec].clone();
        let snake = c.name.snake();
        let spaced = snake.replace('_', " ");
        let f = self.ctx_for(&c, &c.decode, true, true);
        let decode_params = self.fn_params(&f, &c, true);
        let g = self.ctx_for(&c, &c.encode, false, true);
        let encode_params = self.fn_params(&g, &c, false);
        self.blank();
        self.indent += 1;
        self.line("@classmethod");
        self.def_line(
            "def decode",
            &format!("cls, r: Reader{decode_params}"),
            " -> Self:",
        );
        self.doc_and_body(
            &format!("\"\"\"Decode a {spaced}.\"\"\""),
            &f,
            &c.decode.stmts,
        );
        self.blank();
        self.def_line(
            "def encode",
            &format!("self, w: Writer{encode_params}"),
            " -> None:",
        );
        self.doc_and_body(
            &format!("\"\"\"Encode a {spaced}.\"\"\""),
            &g,
            &c.encode.stmts,
        );
        self.indent -= 1;
    }

    fn codec(&mut self, codec: CodecIdx) -> BpsResult<()> {
        let c = self.prog.codecs[codec].clone();
        if c.kind == CodecKind::Record {
            return Ok(());
        }
        let snake = c.name.snake();
        let spaced = snake.replace('_', " ");
        let ret = annotation(self.prog, &c.value, false);
        let f = self.ctx_for(&c, &c.decode, true, false);
        let decode_params = self.fn_params(&f, &c, true);
        self.def_line(
            &format!("def decode_{snake}"),
            &format!("r: Reader{decode_params}"),
            &format!(" -> {ret}:"),
        );
        self.doc_and_body(
            &format!("\"\"\"Decode a {spaced}.\"\"\""),
            &f,
            &c.decode.stmts,
        );
        self.blank();
        self.blank();
        let value_ann = annotation(self.prog, &c.value, false);
        let f = self.ctx_for(&c, &c.encode, false, false);
        let encode_params = self.fn_params(&f, &c, false);
        let value_name = c
            .encode
            .value
            .map(|id| self.local_name(&f, id))
            .unwrap_or_else(|| "value".to_string());
        self.def_line(
            &format!("def encode_{snake}"),
            &format!("w: Writer, {value_name}: {value_ann}{encode_params}"),
            " -> None:",
        );
        self.doc_and_body(
            &format!("\"\"\"Encode a {spaced}.\"\"\""),
            &f,
            &c.encode.stmts,
        );
        Ok(())
    }

    /// A `def` head: single line when it fits, else parameters packed on one
    /// indented line (or one per line), the way `ruff format` does.
    fn def_line(&mut self, head: &str, params: &str, tail: &str) {
        let single = format!("{head}({params}){tail}");
        if self.fits(&single) {
            self.line(&single);
            return;
        }
        self.line(&format!("{head}("));
        self.indent += 1;
        if self.fits(params) {
            self.line(params);
        } else {
            for p in params.split(", ") {
                self.line(&format!("{p},"));
            }
        }
        self.indent -= 1;
        self.line(&format!("){tail}"));
    }

    fn fn_params(&self, f: &FCtx, c: &Codec, decode: bool) -> String {
        let body = if decode { &c.decode } else { &c.encode };
        let mut out = String::new();
        for (p, local) in c.params.iter().zip(&body.params) {
            let name = f
                .names
                .get(local)
                .cloned()
                .unwrap_or_else(|| escape_name(&body.locals[*local].name.snake()));
            let ann = annotation(self.prog, &p.ty, p.optional);
            out.push_str(&format!(", {name}: {ann}"));
        }
        out
    }

    fn fn_body(&mut self, f: &FCtx, stmts: &[Stmt]) {
        self.indent += 1;
        if stmts.is_empty() {
            self.line("pass");
        } else {
            if f.decode
                && !stmts
                    .iter()
                    .any(|s| stmt_uses_reader(self.prog, &f.body, s))
            {
                self.line("_ = r");
            }
            if !f.decode && !stmts.iter().any(stmt_uses_writer) {
                self.line("_ = w");
            }
            for s in stmts {
                self.stmt_or_long(f, s);
            }
        }
        self.indent -= 1;
    }

    /// A docstring plus its body; an empty body needs no `pass` (ruff PIE790:
    /// the docstring alone is a complete body).
    fn doc_and_body(&mut self, doc: &str, f: &FCtx, stmts: &[Stmt]) {
        self.nested(|e| {
            e.line(doc);
        });
        if !stmts.is_empty() {
            self.fn_body(f, stmts);
        }
    }

    /// A statement that must split reports itself instead of failing.
    fn stmt_or_long(&mut self, f: &FCtx, s: &Stmt) {
        match self.stmt(f, s) {
            Ok(()) => {}
            Err(long) => self.line(&long.into_string()),
        }
    }

    // ---------------------------------------------------------------
    // Statements
    // ---------------------------------------------------------------

    fn block(&mut self, heads: &[String], f: &FCtx, stmts: &[Stmt]) {
        for h in heads {
            self.line(h);
        }
        self.indent += 1;
        if stmts.is_empty() {
            self.line("pass");
        } else {
            for s in stmts {
                self.stmt_or_long(f, s);
            }
        }
        self.indent -= 1;
    }

    fn stmt(&mut self, f: &FCtx, s: &Stmt) -> Result<(), Long> {
        match s {
            Stmt::Declare { local, zero } => {
                self.declare(f, *local, *zero);
                Ok(())
            }
            Stmt::Let { dst, value } => {
                let rhs = self.top(f, value);
                let Ok(rhs) = rhs else {
                    return Err(rhs.unwrap_err());
                };
                self.assign(f, dst, rhs, value)
            }
            Stmt::Read { dst, op } => {
                let rhs = self.read_rhs(f, op);
                let line = self.let_line(f, dst, rhs, &read_ty(op));
                self.site(&line)
            }
            Stmt::Decode { dst, codec, args } => {
                let call = self.decode_call(f, *codec, args);
                let ty = self.prog.codecs[*codec].value.clone();
                let line = self.let_line(f, dst, call, &ty);
                self.site(&line)
            }
            Stmt::Utf8 { dst, src } => {
                let data = self.nested_str(f, src);
                let Ok(data) = data else {
                    return Err(data.unwrap_err());
                };
                let line = self.let_line(
                    f,
                    dst,
                    format!("decode_utf8({data}, r.pos - len({data}))"),
                    &Ty::Str,
                );
                self.site(&line)
            }
            Stmt::Write { op, value } => {
                let line = self.write_line(f, op, value);
                self.site(&line)
            }
            Stmt::Encode { codec, args, value } => {
                let line = self.encode_line(f, *codec, args, value);
                self.site(&line)
            }
            Stmt::Require { cond, text } => {
                let off = if f.decode { "r.pos" } else { "w.offset" };
                // `not (a == b)` trips SIM201; the flipped comparison is exact.
                let cond_s = match cond {
                    Expr::Binary { op, lhs, rhs, .. } if op.is_comparison() => {
                        let flipped = Expr::Binary {
                            op: flip_cmp(*op),
                            lhs: lhs.clone(),
                            rhs: rhs.clone(),
                            ty: Ty::Bool,
                            wraps: false,
                        };
                        self.top_or_emit(f, &flipped)
                    }
                    _ => format!("not {}", self.nested_req(f, cond)),
                };
                self.line(&format!("if {cond_s}:"));
                self.indent += 1;
                self.raise_here(f, "RequireFailed", &format!("{off}, {}", py_str(text)));
                self.indent -= 1;
                Ok(())
            }
            Stmt::Unwrap { dst, src, field } => {
                let src_s = self.nested_or_long(f, src);
                let off = if f.decode { "r.pos" } else { "w.offset" };
                self.line(&format!("if {src_s} is None:"));
                self.indent += 1;
                self.raise_here(f, "MissingField", &format!("{off}, {}", py_str(field)));
                self.indent -= 1;
                let l = &f.body.locals[dst.local];
                let name = self.local_name(f, dst.local);
                let ann = annotation(self.prog, &l.ty, false);
                self.line(&format!("{name}: {ann} = {src_s}"));
                Ok(())
            }
            Stmt::If { cond, then, els } => {
                let cond_s = self.top_or_emit(f, cond);
                self.block(&[format!("if {cond_s}:")], f, then);
                if !els.is_empty() {
                    self.block(&["else:".to_string()], f, els);
                }
                Ok(())
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => {
                let scrut = self.top_or_emit(f, scrutinee);
                self.line(&format!("match {scrut}:"));
                self.indent += 1;
                for arm in arms {
                    let tags = arm
                        .tags
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(" | ");
                    let head = format!("case {tags}:");
                    if self.fits(&head) {
                        self.line(&head);
                    } else {
                        // A 60-arm tag list: one tag per line, `|`-leading.
                        self.line("case (");
                        self.indent += 1;
                        for (i, t) in arm.tags.iter().enumerate() {
                            if i == 0 {
                                self.line(&t.to_string());
                            } else {
                                self.line(&format!("| {t}"));
                            }
                        }
                        self.indent -= 1;
                        self.line("):");
                    }
                    self.indent += 1;
                    if arm.body.is_empty() {
                        self.line("pass");
                    } else {
                        for s in &arm.body {
                            self.stmt_or_long(f, s);
                        }
                    }
                    self.indent -= 1;
                }
                self.line("case _:");
                self.indent += 1;
                match other {
                    Some(body) => {
                        if body.is_empty() {
                            self.line("pass");
                        } else {
                            for s in body {
                                self.stmt_or_long(f, s);
                            }
                        }
                    }
                    None => {
                        let off = if f.decode { "r.pos" } else { "w.offset" };
                        self.raise_here(f, "NoMatchArm", &format!("{off}, {scrut}"));
                    }
                }
                self.indent -= 1;
                self.indent -= 1;
                Ok(())
            }
            Stmt::Repeat {
                mode,
                list,
                item,
                body,
            } => self.repeat(f, mode, *list, *item, body),
            Stmt::ForEach { list, item, body } => {
                let item_name = self.local_name(f, *item);
                let list_s = self.top_or_emit(f, list);
                self.line(&format!("for {item_name} in {list_s}:"));
                self.indent += 1;
                for s in body {
                    self.stmt_or_long(f, s);
                }
                self.indent -= 1;
                Ok(())
            }
            Stmt::ExpectVariant {
                dst,
                sum,
                ty,
                variant,
            } => self.expect_variant(f, dst, sum, *ty, *variant),
            Stmt::Return(e) => self.return_stmt(f, e),
            Stmt::Fail(failure) => {
                let cls = match failure {
                    Failure::EncodeUntilEnd => "EncodeUntilEnd",
                };
                self.raise_here(f, cls, "");
                Ok(())
            }
        }
    }

    fn nested_req(&self, f: &FCtx, cond: &Expr) -> String {
        match self.nested_str(f, cond) {
            Ok(s) => s,
            Err(long) => long.into_string(),
        }
    }

    /// An expression for a statement slot: long output stays inline and the
    /// width gate reports it.
    fn top_or_emit(&self, f: &FCtx, e: &Expr) -> String {
        match self.top(f, e) {
            Ok(s) => s,
            Err(long) => long.into_string(),
        }
    }

    fn declare(&mut self, f: &FCtx, local: LocalId, zero: bool) {
        let l = &f.body.locals[local];
        if l.uses == 0 {
            return;
        }
        let name = self.local_name(f, local);
        let ann = annotation(self.prog, &l.ty, l.optional);
        if l.optional {
            self.line(&format!("{name}: {ann} = None"));
        } else if matches!(l.ty, Ty::List(_)) {
            self.line(&format!("{name}: {ann} = []"));
        } else if zero {
            match zero_value(&l.ty) {
                Some(z) => self.line(&format!("{name}: {ann} = {z}")),
                None => self.line(&format!("{name}: {ann}")),
            }
        } else {
            self.line(&format!("{name}: {ann}"));
        }
    }

    /// An assignment to `dst`: annotated when it declares, `_` when dead.
    fn let_line(&self, f: &FCtx, dst: &Bind, rhs: String, ty: &Ty) -> String {
        let l = &f.body.locals[dst.local];
        if l.uses == 0 {
            return format!("_ = {rhs}");
        }
        let name = self.local_name(f, dst.local);
        if dst.declare {
            let ann = annotation(self.prog, ty, l.optional);
            format!("{name}: {ann} = {rhs}")
        } else {
            format!("{name} = {rhs}")
        }
    }

    fn assign_lhs(&self, f: &FCtx, dst: &Bind, ty: &Ty) -> String {
        let l = &f.body.locals[dst.local];
        let name = if l.uses == 0 {
            "_".to_string()
        } else {
            self.local_name(f, dst.local)
        };
        if dst.declare && l.uses > 0 {
            format!("{name}: {}", annotation(self.prog, ty, l.optional))
        } else {
            name
        }
    }

    fn assign(&mut self, f: &FCtx, dst: &Bind, rhs: String, value: &Expr) -> Result<(), Long> {
        let ty = self.prog.expr_ty(&f.body, value).0;
        let line = self.let_line(f, dst, rhs.clone(), &ty);
        if self.fits(&line) {
            self.line(&line);
            return Ok(());
        }
        let lhs = self.assign_lhs(f, dst, &ty);
        // Too long: split the right-hand side by shape, looking through
        // range-proven no-op casts at the rendered core.
        match self.peel_identity(f, value) {
            Expr::Binary { .. } => {
                let core = self.peel_identity(f, value);
                self.emit_chain(&format!("{lhs} = "), ")", f, core);
                Ok(())
            }
            Expr::Call {
                func: Func::SignExtend,
                args,
            } => {
                self.split_call_expr(&format!("{lhs} = sign_extend"), f, args);
                Ok(())
            }
            Expr::Call { .. } | Expr::Construct { .. } | Expr::Variant { .. } => {
                self.split_assign_rhs(&lhs, &rhs, &line)
            }
            _ => self.assign_masked(f, lhs, line, value),
        }
    }

    /// A narrowing mask over a binary chain: `lhs = (chain) & mask`.
    fn assign_masked(
        &mut self,
        f: &FCtx,
        lhs: String,
        line: String,
        value: &Expr,
    ) -> Result<(), Long> {
        if let Expr::Cast {
            expr: inner,
            to: Ty::Int(t),
        } = value
            && !t.signed()
            && let Some(n) = self.narrowing(f, inner, t)
            && matches!(self.peel_identity(f, n.inner), Expr::Binary { .. })
        {
            self.emit_chain(&format!("{lhs} = "), &format!(") & {}", n.mask), f, n.inner);
            return Ok(());
        }
        Err(Long::new(line))
    }

    /// One argument per line, with a magic trailing comma (`ruff format`
    /// keeps exploded calls that have one). `head` ends just before `(`.
    fn explode(&mut self, line: &str) -> BpsResult<()> {
        let mark = self.buf.len();
        let indent = self.indent;
        let out = self.explode_inner(line);
        if out.is_err() {
            self.buf.truncate(mark);
            self.indent = indent;
        }
        out
    }

    fn explode_inner(&mut self, line: &str) -> BpsResult<()> {
        let (head, args, trailer) =
            split_call(line).ok_or_else(|| emit("emit python: cannot split line".to_string()))?;
        self.line(&format!("{head}("));
        self.indent += 1;
        for a in &args {
            // An overlong argument stays on its own line, the way `ruff
            // format` leaves an unsplittable argument (the width gate may
            // still flag it, but the output stays valid and aligned).
            self.line(&format!("{a},"));
        }
        self.indent -= 1;
        self.line(&format!("){trailer}"));
        Ok(())
    }

    fn binary_parts(&self, f: &FCtx, e: &Expr) -> Vec<String> {
        let (op, parts) = self.rendered_parts(f, e);
        let sym = bin_sym(op);
        parts
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if i == 0 {
                    s.clone()
                } else {
                    format!("{sym} {s}")
                }
            })
            .collect()
    }

    /// The flattened operator and rendered operands of a chain.
    fn rendered_parts(&self, f: &FCtx, e: &Expr) -> (BinOp, Vec<String>) {
        let (op, exprs) = flatten(e);
        let parts = exprs.iter().map(|p| self.nested_or_long(f, p)).collect();
        (op, parts)
    }

    /// Whether `s` fits at indent `ind`.
    fn fits_at(&self, ind: usize, s: &str) -> bool {
        ind * 4 + s.len() <= MAX_WIDTH
    }

    /// Emit `head + chain + closer`: black's right-hand split (head chunk on
    /// the first line, parenthesized tail) when it fits, else a full
    /// parenthesized split. `head` ends with `= `/`return `, `closer` starts
    /// with `)`.
    fn emit_chain(&mut self, head: &str, closer: &str, f: &FCtx, e: &Expr) {
        let (op, exprs) = flatten(e);
        let sym = bin_sym(op);
        let sep = format!(" {sym} ");
        let parts: Vec<String> = exprs.iter().map(|p| self.nested_or_long(f, p)).collect();
        if parts.len() >= 2 {
            // The tail sits inside the grouping paren already, so it prints
            // bare (ruff UP034 flags redundant parens there).
            let tail_bare = self.top_or_emit(f, &exprs[exprs.len() - 1]);
            let (init, _) = parts.split_at(parts.len() - 1);
            let head_line = format!("{head}{} {} (", init.join(&sep), sym);
            if self.fits(&head_line) && self.fits_at(self.indent + 1, &tail_bare) {
                self.line(&head_line);
                self.indent += 1;
                self.line(&tail_bare);
                self.indent -= 1;
                self.line(closer);
                return;
            }
        }
        self.line(&format!("{head}("));
        self.indent += 1;
        for (i, p) in parts.iter().enumerate() {
            if i == 0 {
                self.line(p);
            } else {
                self.line(&format!("{sym} {p}"));
            }
        }
        self.indent -= 1;
        self.line(closer);
    }

    /// A call with a too-long argument list: one argument per line, splitting
    /// binary arguments operator-first like `ruff format` does.
    fn split_call_expr(&mut self, head: &str, f: &FCtx, args: &[Expr]) {
        self.line(&format!("{head}("));
        self.indent += 1;
        for a in args {
            let s = self.nested_or_long(f, a);
            if self.fits(&format!("{s},")) {
                self.line(&format!("{s},"));
            } else if matches!(a, Expr::Binary { .. }) {
                let mut parts = self.binary_parts(f, a);
                if let Some(last) = parts.pop() {
                    for part in parts {
                        self.line(&part);
                    }
                    self.line(&format!("{last},"));
                }
            } else {
                self.line(&format!("{s},"));
            }
        }
        self.indent -= 1;
        self.line(")");
    }

    fn nested_or_long(&self, f: &FCtx, e: &Expr) -> String {
        match self.nested_str(f, e) {
            Ok(s) => s,
            Err(long) => long.into_string(),
        }
    }

    fn read_rhs(&self, f: &FCtx, op: &ReadOp) -> String {
        match op {
            ReadOp::U8 => "r.read_u8()".to_string(),
            ReadOp::F32 => "r.read_f32()".to_string(),
            ReadOp::F64 => "r.read_f64()".to_string(),
            ReadOp::Bytes(n) => format!("r.read_bytes({})", self.top_or_emit(f, n)),
            ReadOp::Rest => "r.read_rest()".to_string(),
        }
    }

    fn decode_call(&self, f: &FCtx, codec: CodecIdx, args: &[Expr]) -> String {
        let c = &self.prog.codecs[codec];
        let mut parts = vec!["r".to_string()];
        for a in args {
            parts.push(self.top_or_emit(f, a));
        }
        let joined = parts.join(", ");
        if c.kind == CodecKind::Record {
            let ty = c.ty.expect("record call has a type");
            let cls = self.prog.types[ty].name.pascal();
            format!("{cls}.decode({joined})")
        } else {
            format!("decode_{}({joined})", c.name.snake())
        }
    }

    fn write_line(&self, f: &FCtx, op: &WriteOp, value: &Expr) -> String {
        let v = self.top_or_emit(f, value);
        match op {
            WriteOp::U8 => format!("w.write_u8({v})"),
            WriteOp::F32 => format!("w.write_f32({v})"),
            WriteOp::F64 => format!("w.write_f64({v})"),
            WriteOp::Bytes(n) => {
                format!("w.write_bytes_fixed({v}, {})", self.top_or_emit(f, n))
            }
            WriteOp::Rest => format!("w.write_bytes({v})"),
        }
    }

    fn encode_line(&self, f: &FCtx, codec: CodecIdx, args: &[Expr], value: &Expr) -> String {
        let c = &self.prog.codecs[codec];
        let v = self.top_or_emit(f, value);
        let mut rest = Vec::new();
        for a in args {
            rest.push(self.top_or_emit(f, a));
        }
        if c.kind == CodecKind::Record {
            if rest.is_empty() {
                format!("{v}.encode(w)")
            } else {
                format!("{v}.encode(w, {})", rest.join(", "))
            }
        } else {
            let mut parts = vec!["w".to_string(), v];
            parts.extend(rest);
            format!("encode_{}({})", c.name.snake(), parts.join(", "))
        }
    }

    fn repeat(
        &mut self,
        f: &FCtx,
        mode: &RepeatMode,
        list: LocalId,
        item: LocalId,
        body: &[Stmt],
    ) -> Result<(), Long> {
        let list_name = self.local_name(f, list);
        let item_name = self.local_name(f, item);
        let Ty::List(inner) = f.body.locals[list].ty.clone() else {
            return Ok(());
        };
        let item_ann = annotation(self.prog, &inner, false);
        match mode {
            RepeatMode::Count(n) => {
                let n_s = self.nested_or_long(f, n);
                let n_bare = self.top_or_emit(f, n);
                self.line(&format!("if {n_s} < 0:"));
                self.indent += 1;
                self.raise_here(f, "NegativeLength", &format!("r.pos, {n_bare}"));
                self.indent -= 1;
                if let [Stmt::Decode { codec, args, .. }] = body {
                    let call = self.decode_call(f, *codec, args);
                    let single = format!(
                        "{list_name}: list[{item_ann}] = [{call} for _ in range({n_bare})]"
                    );
                    if self.fits(&single) {
                        self.site(&single)?;
                        return Ok(());
                    }
                    // Too long for one line: an explicit loop instead.
                }
                self.line(&format!("{list_name}: list[{item_ann}] = []"));
                self.line(&format!("for _ in range({n_bare}):"));
                self.indent += 1;
                for s in body {
                    self.stmt_or_long(f, s);
                }
                self.line(&format!("{list_name}.append({item_name})"));
                self.indent -= 1;
                Ok(())
            }
            RepeatMode::Until(term) => {
                let t = self.nested_or_long(f, term);
                let head = fresh(&f.taken, "head");
                self.line(&format!("{list_name}: list[{item_ann}] = []"));
                self.line("while True:");
                self.indent += 1;
                self.line("if r.remaining == 0:");
                self.indent += 1;
                self.line("break");
                self.indent -= 1;
                self.line(&format!("{head} = r.peek_u8()"));
                self.line(&format!("if {head} == {t}:"));
                self.indent += 1;
                self.line("r.read_u8()");
                self.line("break");
                self.indent -= 1;
                for s in body {
                    self.stmt_or_long(f, s);
                }
                self.line(&format!("{list_name}.append({item_name})"));
                self.indent -= 1;
                Ok(())
            }
            RepeatMode::UntilEnd => {
                self.line(&format!("{list_name}: list[{item_ann}] = []"));
                self.line("while r.remaining > 0:");
                self.indent += 1;
                for s in body {
                    self.stmt_or_long(f, s);
                }
                self.line(&format!("{list_name}.append({item_name})"));
                self.indent -= 1;
                Ok(())
            }
        }
    }

    fn expect_variant(
        &mut self,
        f: &FCtx,
        dst: &Option<Bind>,
        sum: &Expr,
        ty: TypeIdx,
        variant: usize,
    ) -> Result<(), Long> {
        let sum_s = self.top_or_emit(f, sum);
        let decl = self.prog.variants(ty)[variant].clone();
        let cls = self.variant_class(ty, &decl);
        let expected = decl.name.snake();
        let Some(tag) = f.tag else {
            return Ok(());
        };
        let tag_s = self.local_name(f, tag);
        self.line(&format!("if not isinstance({sum_s}, {cls}):"));
        self.indent += 1;
        self.raise_here(
            f,
            "WrongVariant",
            &format!("w.offset, {}, {tag_s}", py_str(&expected)),
        );
        self.indent -= 1;
        if let Some(dst) = dst {
            let l = &f.body.locals[dst.local];
            let name = self.local_name(f, dst.local);
            let ann = annotation(self.prog, &l.ty, false);
            if is_record_payload(self.prog, &decl.payload) {
                self.line(&format!("{name}: {ann} = {sum_s}"));
            } else {
                self.line(&format!("{name}: {ann} = {sum_s}.value"));
            }
        }
        Ok(())
    }

    fn return_stmt(&mut self, f: &FCtx, e: &Expr) -> Result<(), Long> {
        match e {
            Expr::Construct { ty, fields } => {
                let cls = self.ctor(f, *ty);
                let parts: Vec<String> = self
                    .prog
                    .fields(*ty)
                    .iter()
                    .zip(fields.iter())
                    .map(|(fd, v)| {
                        format!(
                            "{}={}",
                            escape_name(&fd.name.snake()),
                            self.nested_or_long(f, v)
                        )
                    })
                    .collect();
                let single = format!("return {cls}({})", parts.join(", "));
                if self.fits(&single) {
                    self.line(&single);
                } else {
                    self.explode(&format!("return {cls}({})", parts.join(", ")))
                        .unwrap_or_else(|_| self.line(&single));
                }
                Ok(())
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                let decl = self.prog.variants(*ty)[*variant].clone();
                let cls = self.variant_class(*ty, &decl);
                let single = match payload {
                    None => format!("return {cls}()"),
                    Some(p) => {
                        if is_record_payload(self.prog, &decl.payload) {
                            format!("return {}", self.nested_or_long(f, p))
                        } else {
                            format!("return {cls}(value={})", self.nested_or_long(f, p))
                        }
                    }
                };
                if self.fits(&single) {
                    self.line(&single);
                    return Ok(());
                }
                match payload {
                    Some(p) if !is_record_payload(self.prog, &decl.payload) => {
                        let arg = format!("value={}", self.nested_or_long(f, p));
                        self.explode(&format!("return {cls}({arg})"))
                            .unwrap_or_else(|_| self.line(&single));
                    }
                    _ => self.line(&single),
                }
                Ok(())
            }
            _ => {
                let rhs = self.top(f, e)?;
                let single = format!("return {rhs}");
                if self.fits(&single) {
                    self.line(&single);
                    return Ok(());
                }
                match self.peel_identity(f, e) {
                    Expr::Binary { .. } => {
                        let core = self.peel_identity(f, e);
                        self.emit_chain("return ", ")", f, core);
                        Ok(())
                    }
                    Expr::Call {
                        func: Func::SignExtend,
                        args,
                    } => {
                        self.split_call_expr("return sign_extend", f, args);
                        Ok(())
                    }
                    Expr::Call { .. } => {
                        // `return call(...)`: explode the call directly.
                        self.explode(&single).map_err(|_| Long::new(single))?;
                        Ok(())
                    }
                    _ => self.return_masked(f, single, e),
                }
            }
        }
    }

    /// A narrowing mask over a binary chain: `return (chain) & mask`.
    fn return_masked(&mut self, f: &FCtx, single: String, e: &Expr) -> Result<(), Long> {
        if let Expr::Cast {
            expr: inner,
            to: Ty::Int(t),
        } = e
            && !t.signed()
            && let Some(n) = self.narrowing(f, inner, t)
            && matches!(self.peel_identity(f, n.inner), Expr::Binary { .. })
        {
            self.emit_chain("return ", &format!(") & {}", n.mask), f, n.inner);
            return Ok(());
        }
        Err(Long::new(single))
    }

    /// `cls(...)` in a record method, `Cls(...)` in a module function.
    fn ctor(&self, f: &FCtx, ty: TypeIdx) -> String {
        if f.method {
            "cls".to_string()
        } else {
            self.prog.types[ty].name.pascal()
        }
    }

    /// Emit one fallible line unwrapped: reader errors carry the offset, and
    /// only errors raised by a codec body name the codec (at their raise).
    fn site(&mut self, line: &str) -> Result<(), Long> {
        if self.fits(line) {
            self.line(line);
            return Ok(());
        }
        if let Some((lhs, rhs)) = split_assign(line)
            && split_call(rhs).is_some()
        {
            return self.split_assign_rhs(lhs, rhs, line);
        }
        let mark = self.buf.len();
        if self.explode(line).is_ok() && self.lines_fit(mark) {
            return Ok(());
        }
        self.buf.truncate(mark);
        Err(Long::new(line.to_string()))
    }

    /// Split a too-long `lhs = rhs`: bare explode when the head fits, else
    /// parenthesized, the way `ruff format` does.
    fn split_assign_rhs(&mut self, lhs: &str, rhs: &str, line: &str) -> Result<(), Long> {
        let mark = self.buf.len();
        if self.explode(line).is_ok() && self.lines_fit(mark) {
            return Ok(());
        }
        self.buf.truncate(mark);
        let mark = self.buf.len();
        self.line(&format!("{lhs} = ("));
        self.indent += 1;
        let out = self.explode(rhs);
        self.indent -= 1;
        if out.is_ok() && self.lines_fit(mark) {
            self.line(")");
            return Ok(());
        }
        self.buf.truncate(mark);
        Err(Long::new(line.to_string()))
    }

    /// Whether every line emitted since `mark` fits the width gate.
    fn lines_fit(&self, mark: usize) -> bool {
        self.buf[mark..].split('\n').all(|l| l.len() <= MAX_WIDTH)
    }

    /// Raise an error constructed here, tagged with the codec name.
    fn raise_here(&mut self, f: &FCtx, cls: &str, args: &str) {
        let single = if args.is_empty() {
            format!("raise {cls}().in_codec(\"{}\")", f.cname)
        } else {
            format!("raise {cls}({args}).in_codec(\"{}\")", f.cname)
        };
        if self.fits(&single) || self.explode(&single).is_err() {
            self.line(&single);
        }
    }

    // ---------------------------------------------------------------
    // Expressions
    // ---------------------------------------------------------------

    /// Top-level expression: no outer parentheses.
    fn top(&self, f: &FCtx, e: &Expr) -> Result<String, Long> {
        self.with_min(f, e, 0)
    }

    /// Nested expression: parentheses when looser than `min`.
    fn nested_str(&self, f: &FCtx, e: &Expr) -> Result<String, Long> {
        self.with_min(f, e, 99)
    }

    fn with_min(&self, f: &FCtx, e: &Expr, min: u8) -> Result<String, Long> {
        let (s, prec) = self.render(f, e)?;
        if prec < min {
            Ok(format!("({s})"))
        } else {
            Ok(s)
        }
    }

    /// Render an expression with its precedence (higher binds tighter).
    fn render(&self, f: &FCtx, e: &Expr) -> Result<(String, u8), Long> {
        match e {
            Expr::Int { value, .. } => Ok((value.to_string(), 99)),
            Expr::Bool(b) => Ok((b.to_string(), 99)),
            Expr::Str(s) => Ok((py_str(s), 99)),
            Expr::Local(id) => Ok((self.local_name(f, *id), 99)),
            Expr::Field { base, ty, field } => {
                let b = self.with_min(f, base, 99)?;
                let name = escape_name(&self.prog.fields(*ty)[*field].name.snake());
                Ok((format!("{b}.{name}"), 99))
            }
            Expr::Present(inner) => {
                let s = self.with_min(f, inner, 99)?;
                Ok((format!("{s} is not None"), 4))
            }
            Expr::Get(inner) => self.render(f, inner),
            Expr::Not(inner) => {
                let s = self.with_min(f, inner, 4)?;
                Ok((format!("not {s}"), 3))
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let p = bin_prec(*op);
                // Comparisons do not chain: both sides need parens.
                let left_min = if op.is_comparison() { p + 1 } else { p };
                let l = self.with_min(f, lhs, left_min)?;
                let r = self.with_min(f, rhs, p + 1)?;
                Ok((format!("{l} {} {r}", bin_sym(*op)), p))
            }
            Expr::Cond {
                cond, then, els, ..
            } => {
                let c = self.with_min(f, cond, 1)?;
                let t = self.with_min(f, then, 1)?;
                let el = self.with_min(f, els, 0)?;
                Ok((format!("{t} if {c} else {el}"), 0))
            }
            Expr::Cast { expr, to } => self.cast(f, expr, to),
            Expr::Call { func, args } => {
                let rendered: Vec<String> = args
                    .iter()
                    .map(|a| self.with_min(f, a, 1))
                    .collect::<Result<_, _>>()?;
                Ok((
                    match func {
                        Func::SignExtend => format!("sign_extend({})", rendered.join(", ")),
                        Func::Utf16Units => format!("utf16_units({})", rendered.join(", ")),
                        Func::Utf8Len => {
                            format!("len({}.encode(\"utf-8\"))", self.with_min(f, &args[0], 99)?)
                        }
                        Func::StrLen | Func::BytesLen | Func::ListLen => {
                            format!("len({})", rendered.join(", "))
                        }
                        Func::Min => format!("min({})", rendered.join(", ")),
                        Func::Max => format!("max({})", rendered.join(", ")),
                    },
                    99,
                ))
            }
            Expr::StrBytes(inner) => {
                let s = self.with_min(f, inner, 99)?;
                Ok((format!("{s}.encode(\"utf-8\")"), 99))
            }
            Expr::Remaining => {
                if !f.decode {
                    return Err(Long::new("remaining in encode".to_string()));
                }
                Ok(("r.remaining".to_string(), 99))
            }
            Expr::Construct { ty, fields } => {
                let cls = self.ctor(f, *ty);
                let parts: Vec<String> = self
                    .prog
                    .fields(*ty)
                    .iter()
                    .zip(fields.iter())
                    .map(|(fd, v)| {
                        Ok(format!(
                            "{}={}",
                            escape_name(&fd.name.snake()),
                            self.with_min(f, v, 1)?
                        ))
                    })
                    .collect::<Result<_, _>>()?;
                Ok((format!("{cls}({})", parts.join(", ")), 99))
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                let decl = self.prog.variants(*ty)[*variant].clone();
                let cls = self.variant_class(*ty, &decl);
                match payload {
                    None => Ok((format!("{cls}()"), 99)),
                    Some(p) => {
                        if is_record_payload(self.prog, &decl.payload) {
                            self.render(f, p)
                        } else {
                            Ok((format!("{cls}(value={})", self.with_min(f, p, 1)?), 99))
                        }
                    }
                }
            }
        }
    }

    /// A real (non-identity) integer narrowing: the inner expression, the
    /// mask text, and the target signedness. Range-proven no-ops are `None`.
    fn narrowing<'b>(&self, f: &FCtx, inner: &'b Expr, to: &IntTy) -> Option<Narrowing<'b>> {
        let (from, _) = self.prog.expr_ty(&f.body, inner);
        if !matches!(from, Ty::Int(_)) || from == Ty::Int(*to) {
            return None;
        }
        if let Some((lo, hi)) = self.prog.int_range(&f.body, inner)
            && to.contains(lo)
            && to.contains(hi)
        {
            return None;
        }
        Some(Narrowing {
            inner,
            mask: format!("0x{:X}", mask_of(*to)),
            signed: to.signed(),
        })
    }
    /// Strip casts that render identically (range-proven no-ops) to find the
    /// splittable core of a too-long line.
    fn peel_identity<'b>(&self, f: &FCtx, e: &'b Expr) -> &'b Expr {
        let mut cur = e;
        while let Expr::Cast { expr: inner, .. } = cur {
            let (Ok((full, _)), Ok((bare, _))) = (self.render(f, cur), self.render(f, inner))
            else {
                break;
            };
            if full != bare {
                break;
            }
            cur = inner;
        }
        cur
    }

    #[allow(clippy::only_used_in_recursion)]
    fn cast(&self, f: &FCtx, expr: &Expr, to: &Ty) -> Result<(String, u8), Long> {
        let (from, _) = self.prog.expr_ty(&f.body, expr);
        if &from == to {
            return self.render(f, expr);
        }
        match (from, to) {
            (Ty::Bool, Ty::Int(_)) => {
                let v = self.with_min(f, expr, 1)?;
                Ok((format!("1 if {v} else 0"), 0))
            }
            (Ty::Int(_), Ty::Bool) => {
                let v = self.with_min(f, expr, 5)?;
                Ok((format!("{v} != 0"), 4))
            }
            (Ty::Int(_), Ty::Int(t)) => {
                if let Some(n) = self.narrowing(f, expr, t) {
                    if n.signed {
                        let inner = self.with_min(f, n.inner, 7)?;
                        Ok((
                            format!("sign_extend({inner} & {}, {})", n.mask, t.bits()),
                            99,
                        ))
                    } else {
                        let inner = self.with_min(f, n.inner, 7)?;
                        Ok((format!("{inner} & {}", n.mask), 7))
                    }
                } else {
                    self.render(f, expr)
                }
            }
            (Ty::Int(_), Ty::Named(t)) if is_enum(self.prog, *t) => {
                let cls = self.prog.types[*t].name.pascal();
                let v = self.with_min(f, expr, 1)?;
                Ok((format!("{cls}({v})"), 99))
            }
            (Ty::Named(t), Ty::Int(_)) if is_enum(self.prog, t) => self.render(f, expr),
            (Ty::F32, Ty::F64) | (Ty::F64, Ty::F32) => self.render(f, expr),
            _ => Ok((format!("int({})", self.with_min(f, expr, 1)?), 99)),
        }
    }

    fn local_name(&self, f: &FCtx, id: LocalId) -> String {
        if Some(id) == f.body.value && f.method {
            return "self".to_string();
        }
        f.names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| escape_name(&f.body.locals[id].name.snake()))
    }
}

/// A function body: codec name for errors, direction, record-method shape.
struct FCtx {
    cname: String,
    decode: bool,
    method: bool,
    body: Body,
    /// The tag parameter of a sum codec's encode (for `WrongVariant`).
    tag: Option<LocalId>,
    /// Printed name of every local (printer-fixed names win over schema names).
    names: HashMap<LocalId, String>,
    /// Every printed local name plus fixed and reserved names.
    taken: HashSet<String>,
}

/// A rendered line that does not fit: the caller prints it as-is and the
/// width gate reports it.
struct Long(String);

/// A real integer narrowing for splitting: the inner expression, mask text,
/// and target signedness.
struct Narrowing<'b> {
    inner: &'b Expr,
    mask: String,
    signed: bool,
}

impl Long {
    fn new(s: String) -> Self {
        Long(s)
    }

    fn into_string(self) -> String {
        self.0
    }
}

fn is_record_payload(prog: &Program, payload: &Option<Ty>) -> bool {
    matches!(payload, Some(Ty::Named(t)) if matches!(prog.types[*t].kind, TypeKind::Record { .. }))
}

fn is_enum(prog: &Program, t: TypeIdx) -> bool {
    matches!(prog.types[t].kind, TypeKind::Enum { .. })
}

fn escape_name(snake: &str) -> String {
    crate::ir::typed::escape(snake.to_string(), KEYWORDS)
}

/// A printer-introduced name that no local, parameter, or fixed name uses.
fn fresh(taken: &HashSet<String>, base: &str) -> String {
    let mut name = base.to_string();
    while taken.contains(&name) {
        name.push('_');
    }
    name
}

fn annotation(prog: &Program, ty: &Ty, optional: bool) -> String {
    let base = match ty {
        Ty::Int(_) => "int".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::F32 | Ty::F64 => "float".to_string(),
        Ty::Str => "str".to_string(),
        Ty::Bytes => "bytes".to_string(),
        Ty::List(inner) => format!("list[{}]", annotation(prog, inner, false)),
        Ty::Named(t) => prog.types[*t].name.pascal(),
    };
    if optional {
        format!("{base} | None")
    } else {
        base
    }
}

fn zero_value(ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::Int(_) => Some("0"),
        Ty::Bool => Some("False"),
        Ty::F32 | Ty::F64 => Some("0.0"),
        Ty::Str => Some("\"\""),
        Ty::Bytes => Some("b\"\""),
        Ty::List(_) | Ty::Named(_) => None,
    }
}

fn read_ty(op: &ReadOp) -> Ty {
    match op {
        ReadOp::U8 => Ty::Int(IntTy::U8),
        ReadOp::F32 => Ty::F32,
        ReadOp::F64 => Ty::F64,
        ReadOp::Bytes(_) | ReadOp::Rest => Ty::Bytes,
    }
}

fn mask_of(t: IntTy) -> u128 {
    if t.bits() >= 128 {
        u128::MAX
    } else {
        (1u128 << t.bits()) - 1
    }
}

/// A Python string literal in `ruff format`'s double-quote style.
fn py_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c < '\u{20}') || c == '\u{7f}' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn bin_sym(op: crate::ir::typed::BinOp) -> &'static str {
    use crate::ir::typed::BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "//",
        Mod => "%",
        Shl => "<<",
        Shr => ">>",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        And => "and",
        Or => "or",
    }
}

fn bin_prec(op: crate::ir::typed::BinOp) -> u8 {
    use crate::ir::typed::BinOp::*;
    match op {
        Or => 1,
        And => 2,
        Eq | Ne | Lt | Le | Gt | Ge => 4,
        BitOr => 5,
        BitXor => 6,
        BitAnd => 7,
        Shl | Shr => 8,
        Add | Sub => 9,
        Mul | Div | Mod => 10,
    }
}

/// The negation of a comparison, for `require` heads (`not (a == b)` trips
/// SIM201; the flipped form is exactly equivalent on every operand type).
fn flip_cmp(op: crate::ir::typed::BinOp) -> crate::ir::typed::BinOp {
    use crate::ir::typed::BinOp::*;
    match op {
        Eq => Ne,
        Ne => Eq,
        Lt => Ge,
        Le => Gt,
        Gt => Le,
        Ge => Lt,
        _ => Ne,
    }
}

/// Flatten one chain of an associative operator for black-style splitting:
/// only same-operator spines merge; anything else stays a whole operand.
fn flatten(e: &Expr) -> (crate::ir::typed::BinOp, Vec<Expr>) {
    use crate::ir::typed::BinOp::*;
    if let Expr::Binary { op, lhs, rhs, .. } = e {
        if matches!(op, Add | Mul | BitAnd | BitOr | BitXor | And | Or) {
            let mut parts = Vec::new();
            push_flat(lhs, *op, &mut parts);
            push_flat(rhs, *op, &mut parts);
            return (*op, parts);
        }
        return (*op, vec![(**lhs).clone(), (**rhs).clone()]);
    }
    (Add, vec![e.clone()])
}

/// Push `e`, merging through same-operator nodes only.
fn push_flat(e: &Expr, op: crate::ir::typed::BinOp, out: &mut Vec<Expr>) {
    if let Expr::Binary {
        op: op2, lhs, rhs, ..
    } = e
        && *op2 == op
    {
        push_flat(lhs, op, out);
        push_flat(rhs, op, out);
        return;
    }
    out.push(e.clone());
}

/// Split `lhs = rhs` at a top-level ` = ` (string-aware; `==`, `!=`, `<=`,
/// `>=` never match thanks to their missing spaces). For parenthesizing the
/// right-hand side of a too-long assignment.
fn split_assign(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    let mut in_str: Option<u8> = None;
    while i + 3 <= bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_str = Some(b);
                i += 1;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'=' if depth == 0 && bytes[i.saturating_sub(1)] == b' ' && bytes[i + 1] == b' ' => {
                return Some((line[..i - 1].trim_end(), line[i + 2..].trim_start()));
            }
            _ => i += 1,
        }
    }
    None
}

/// Split `head(arg, ...)trailer` at its top-level commas, string-aware.
/// Returns the head (ending just before `(`), the argument strings, and the
/// trailer after the matching `)`. Splits the outermost call: the last
/// top-level `(` whose match ends the line (so `raise E(a).in_codec("c")`
/// splits `.in_codec`, the way `ruff format` does).
fn split_call(line: &str) -> Option<(String, Vec<String>, String)> {
    let open = last_call_open(line)?;
    let close = match_paren(line, open)?;
    if close + 1 != line.len() {
        return None;
    }
    let head = line[..open].to_string();
    let trailer = line[close + 1..].to_string();
    let inner = &line[open + 1..close];
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    let mut in_str: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_str = Some(b);
                i += 1;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' if depth == 0 => {
                args.push(inner[start..i].trim().to_string());
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    args.push(inner[start..].trim().to_string());
    Some((head, args, trailer))
}

/// The index of the `)` matching the `(` at `open`, string-aware.
fn match_paren(line: &str, open: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    let mut in_str: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_str = Some(b);
                i += 1;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return if line.as_bytes()[open] == b'(' {
                        Some(i)
                    } else {
                        None
                    };
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// The last top-level `(` in the line, string-aware: the outermost call to
/// split.
fn last_call_open(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut last = None;
    let mut i = 0usize;
    let mut in_str: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_str = Some(b);
                i += 1;
            }
            b'(' => {
                if depth == 0 {
                    last = Some(i);
                }
                depth += 1;
                i += 1;
            }
            b'[' | b'{' => {
                depth += 1;
                i += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ => i += 1,
        }
    }
    last
}

fn stmt_uses_reader(prog: &Program, body: &Body, s: &Stmt) -> bool {
    match s {
        Stmt::Read { .. } | Stmt::Decode { .. } | Stmt::Utf8 { .. } => true,
        Stmt::Repeat {
            mode, body: inner, ..
        } => {
            matches!(mode, RepeatMode::Until(_) | RepeatMode::UntilEnd)
                || inner.iter().any(|x| stmt_uses_reader(prog, body, x))
                || mode_expr_uses_reader(prog, body, mode)
        }
        Stmt::If { cond, then, els } => {
            expr_uses_reader(prog, body, cond)
                || then.iter().any(|x| stmt_uses_reader(prog, body, x))
                || els.iter().any(|x| stmt_uses_reader(prog, body, x))
        }
        Stmt::Match {
            scrutinee,
            arms,
            other,
        } => {
            expr_uses_reader(prog, body, scrutinee)
                || arms
                    .iter()
                    .flat_map(|a| &a.body)
                    .any(|x| stmt_uses_reader(prog, body, x))
                || other
                    .as_ref()
                    .is_some_and(|o| o.iter().any(|x| stmt_uses_reader(prog, body, x)))
        }
        Stmt::ForEach { body: inner, .. } => inner.iter().any(|x| stmt_uses_reader(prog, body, x)),
        Stmt::Require { cond, .. } => expr_uses_reader(prog, body, cond),
        _ => false,
    }
}

fn mode_expr_uses_reader(prog: &Program, body: &Body, mode: &RepeatMode) -> bool {
    match mode {
        RepeatMode::Count(e) | RepeatMode::Until(e) => expr_uses_reader(prog, body, e),
        RepeatMode::UntilEnd => false,
    }
}

fn expr_uses_reader(prog: &Program, body: &Body, e: &Expr) -> bool {
    match e {
        Expr::Remaining => true,
        Expr::Field { base, .. }
        | Expr::Present(base)
        | Expr::Get(base)
        | Expr::Not(base)
        | Expr::StrBytes(base) => expr_uses_reader(prog, body, base),
        Expr::Cast { expr: inner, .. } => expr_uses_reader(prog, body, inner),
        Expr::Binary { lhs, rhs, .. } => {
            expr_uses_reader(prog, body, lhs) || expr_uses_reader(prog, body, rhs)
        }
        Expr::Cond {
            cond, then, els, ..
        } => {
            expr_uses_reader(prog, body, cond)
                || expr_uses_reader(prog, body, then)
                || expr_uses_reader(prog, body, els)
        }
        Expr::Call { args, .. } | Expr::Construct { fields: args, .. } => {
            args.iter().any(|a| expr_uses_reader(prog, body, a))
        }
        Expr::Variant { payload, .. } => payload
            .as_ref()
            .is_some_and(|p| expr_uses_reader(prog, body, p)),
        _ => {
            let _ = (prog, body);
            false
        }
    }
}

fn stmt_uses_writer(s: &Stmt) -> bool {
    match s {
        Stmt::Write { .. } | Stmt::Encode { .. } => true,
        Stmt::If { then, els, .. } => {
            then.iter().any(stmt_uses_writer) || els.iter().any(stmt_uses_writer)
        }
        Stmt::Match { arms, other, .. } => {
            arms.iter().flat_map(|a| &a.body).any(stmt_uses_writer)
                || other
                    .as_ref()
                    .is_some_and(|o| o.iter().any(stmt_uses_writer))
        }
        Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => body.iter().any(stmt_uses_writer),
        _ => false,
    }
}
