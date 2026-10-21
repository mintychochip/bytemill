//! `emit go`: an idiomatic Go package over the typed program.
//!
//! The printer reads [`Program`](crate::ir::typed) only: every name is already
//! resolved, every value typed, every nested record or sum lifted into its own
//! codec. It chooses Go syntax and nothing else.
//!
//! # Files
//!
//! `go.mod` (`module bmlgen` plus the requested `go` directive), `runtime.go`
//! (reader, writer, errors, helpers), and one `{category}.go` per route
//! category (types and codecs, in program order). A packet routed only as
//! `play_clientbound.go`; a packet routed in several state/direction pairs is
//! `shared.go`; encodings, enums, frames, and unrouted packets are
//! `encoding.go`. Package `bmlgen`.
//!
//! # Names
//!
//! Exported PascalCase for types, fields, variants, enum constants, and
//! functions; camelCase for locals and parameters. Go initialisms (`ID`,
//! `UUID`, ...) in every position. Keywords and predeclared identifiers take a
//! trailing `_`.
//!
//! # Types
//!
//! Records are plain structs, sums a sealed interface with one struct per
//! variant (a block-arm record implements the interface itself when it is only
//! that variant's payload, otherwise the variant wraps it), enums a named
//! integer with constants and a `String` method, optionals pointers, lists
//! slices, bytes `[]byte`.
//!
//! # Codecs
//!
//! `func DecodeHandshake(r *Reader, ...) (Handshake, error)` and
//! `func EncodeHandshake(w *Writer, v Handshake, ...) error` per codec.
//! Failures are `*Error` values carrying the codec, the reader offset on
//! decode, a sentinel kind for `errors.Is`, and a detail message.
//! Integer operators are plain Go operators (fixed-width Go arithmetic wraps,
//! never traps); signed division and modulo use floor helpers when the
//! dividend may be negative; a `Cond` becomes a temporary declared in place
//! and set by an `if`. Locals are declared where first stored (`x, err :=`)
//! unless used outside that block; counted loops are `for range n` on Go >= 1.22
//! and `for i := 0; i < int(n); i++` before it; list loops are
//! `for _, item := range list`.
//!
//! Output is deterministic and gofmt-canonical (`gofmt -l` clean, `go vet`
//! clean).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::ResolvedSchema;
use crate::errors::{BmlResult, emit};
use crate::ir::typed::{
    BinOp, Body, CodecIdx, Expr, Failure, Func, IntTy, Local, Name, Origin, Program, ReadOp,
    RepeatMode, Stmt, Ty, TypeIdx, WriteOp, byte_literal, diverges, escape, hex_literal,
};

/// `emit go`: write `go.mod`, `runtime.go`, and one file per [`Program::output_stem`].
pub fn emit_go(resolved: &ResolvedSchema, out_dir: &Path) -> BmlResult<()> {
    emit_go_with_version(resolved, out_dir, crate::GoVersion::default())
}

pub fn emit_go_with_version(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    ver: crate::GoVersion,
) -> BmlResult<()> {
    let program = crate::ir::typed(resolved)?;
    let mut printer = Printer::new(&program, ver)?;
    let mut files = printer.packets_files()?;
    let mut runtime = printer.runtime_go();
    if !ver.supports_any() {
        runtime = downgrade_any(&runtime);
        for body in files.values_mut() {
            *body = downgrade_any(body);
        }
    }
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    fs::write(
        out_dir.join("go.mod"),
        format!("module bmlgen\n\ngo {}\n", ver.mod_str()),
    )
    .map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("runtime.go"), runtime).map_err(|e| emit(e.to_string()))?;
    for (stem, body) in files {
        fs::write(out_dir.join(format!("{stem}.go")), body).map_err(|e| emit(e.to_string()))?;
    }
    Ok(())
}

/// Rewrite the `any` predeclared identifier as `interface{}` for Go < 1.18.
/// Comments, string literals, raw strings, and rune literals pass through
/// untouched; only code tokens spelling exactly `any` are rewritten.
fn downgrade_any(src: &str) -> String {
    #[derive(PartialEq)]
    enum State {
        Code,
        LineComment,
        BlockComment,
        Interpreted,
        Raw,
        Rune,
    }
    fn is_ident(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || byte == b'_'
    }
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(src.len());
    let mut i = 0;
    let mut state = State::Code;
    while i < bytes.len() {
        let c = bytes[i];
        match state {
            State::Code => {
                if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                    out.extend_from_slice(b"//");
                    i += 2;
                    state = State::LineComment;
                } else if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                    out.extend_from_slice(b"/*");
                    i += 2;
                    state = State::BlockComment;
                } else if c == b'"' {
                    out.push(c);
                    i += 1;
                    state = State::Interpreted;
                } else if c == b'`' {
                    out.push(c);
                    i += 1;
                    state = State::Raw;
                } else if c == b'\'' {
                    out.push(c);
                    i += 1;
                    state = State::Rune;
                } else if c == b'a'
                    && bytes.get(i + 1) == Some(&b'n')
                    && bytes.get(i + 2) == Some(&b'y')
                    && (i == 0 || !is_ident(bytes[i - 1]))
                    && (i + 3 >= bytes.len() || !is_ident(bytes[i + 3]))
                {
                    out.extend_from_slice(b"interface{}");
                    i += 3;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            State::LineComment => {
                out.push(c);
                i += 1;
                if c == b'\n' {
                    state = State::Code;
                }
            }
            State::BlockComment => {
                out.push(c);
                i += 1;
                if c == b'*' && bytes.get(i) == Some(&b'/') {
                    out.push(b'/');
                    i += 1;
                    state = State::Code;
                }
            }
            State::Interpreted => {
                out.push(c);
                i += 1;
                if c == b'\\' && i < bytes.len() {
                    out.push(bytes[i]);
                    i += 1;
                } else if c == b'"' {
                    state = State::Code;
                }
            }
            State::Raw => {
                out.push(c);
                i += 1;
                if c == b'`' {
                    state = State::Code;
                }
            }
            State::Rune => {
                out.push(c);
                i += 1;
                if c == b'\\' && i < bytes.len() {
                    out.push(bytes[i]);
                    i += 1;
                } else if c == b'\'' {
                    state = State::Code;
                }
            }
        }
    }
    // Byte-preserving: the input was valid UTF-8 and every edit is ASCII.
    String::from_utf8(out).expect("emit go: downgrade_any broke UTF-8")
}

/// The emitted decode function for `codec`, e.g. `DecodeHandshake`.
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("Decode{}", pascal(&program.codecs[codec].name))
}

/// The emitted encode function for `codec`, e.g. `EncodeHandshake`.
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("Encode{}", pascal(&program.codecs[codec].name))
}

// ----------------------------------------------------------------------
// Names
// ----------------------------------------------------------------------

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
    "any",
    "bool",
    "byte",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "rune",
    "string",
    "true",
    "false",
    "iota",
    "nil",
    "append",
    "cap",
    "clear",
    "copy",
    "delete",
    "len",
    "make",
    "max",
    "min",
    "new",
    "panic",
    "print",
    "println",
    "real",
    "recover",
];

const GO_INITIALISMS: &[&str] = &[
    "ID", "UUID", "URL", "URI", "JSON", "HTTP", "HTTPS", "IP", "TCP", "UDP", "UI", "UTF8", "XML",
    "API", "ASCII", "CPU", "DNS", "EOF", "TTL", "RPC", "SQL", "SSH", "TLS",
];

fn cap_word(word: &str) -> String {
    if GO_INITIALISMS.contains(&word.to_ascii_uppercase().as_str()) {
        return word.to_ascii_uppercase();
    }
    // A plural of an initialism keeps the initialism intact: `ids` → `IDs`.
    if let Some(singular) = word.strip_suffix("s")
        && !singular.is_empty()
        && GO_INITIALISMS.contains(&singular.to_ascii_uppercase().as_str())
    {
        return format!("{}s", singular.to_ascii_uppercase());
    }
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Exported PascalCase with initialisms and keyword escaping.
fn pascal(name: &Name) -> String {
    escape(
        name.words().iter().map(|w| cap_word(w)).collect(),
        GO_KEYWORDS,
    )
}

/// Unexported camelCase with initialisms and keyword escaping.
fn camel(name: &Name) -> String {
    let mut out = String::new();
    for (i, word) in name.words().iter().enumerate() {
        if i == 0 {
            out.push_str(word);
        } else {
            out.push_str(&cap_word(word));
        }
    }
    escape(out, GO_KEYWORDS)
}

/// A Go double-quoted string literal.
fn go_quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ----------------------------------------------------------------------
// Shared data and flags
// ----------------------------------------------------------------------

/// Helper/harness usage discovered while emitting packets; decides the
/// contents of `runtime.go` and the imports of `packets.go`.
#[derive(Default)]
struct Flags {
    sign_extend: bool,
    utf16_units: bool,
    floor_div: bool,
    floor_mod: bool,
    b2i: bool,
    utf8_valid: bool,
    sprintf: bool,
}

impl Flags {
    fn absorb(&mut self, other: &Flags) {
        self.sign_extend |= other.sign_extend;
        self.utf16_units |= other.utf16_units;
        self.floor_div |= other.floor_div;
        self.floor_mod |= other.floor_mod;
        self.b2i |= other.b2i;
        self.utf8_valid |= other.utf8_valid;
        self.sprintf |= other.sprintf;
    }
}

/// Program-wide Go names: one type name per type, one wrapper per wrapped sum
/// variant, and the set of directly-implemented record payloads.
struct Data<'a> {
    prog: &'a Program,
    type_go: Vec<String>,
    /// Wrapper struct name for each wrapped `(sum, variant)`.
    wrappers: BTreeMap<(TypeIdx, usize), String>,
    /// `(sum, variant)` whose record payload implements the interface itself.
    direct: BTreeSet<(TypeIdx, usize)>,
}

impl<'a> Data<'a> {
    fn new(prog: &'a Program) -> BmlResult<Data<'a>> {
        let mut taken = BTreeSet::new();
        // Package-scope runtime identifiers and generated function names are
        // taken before any type claims its name.
        for reserved in [
            "Error",
            "ErrorKind",
            "Reader",
            "Writer",
            "NewReader",
            "failDecode",
            "failEncode",
            "requireFailed",
            "signExtend",
            "utf16Units",
            "floorDiv",
            "floorMod",
            "b2i",
            "ErrShortBuffer",
            "ErrRequire",
            "ErrInvalidUTF8",
            "ErrNoMatchArm",
            "ErrMissingField",
            "ErrWrongVariant",
            "ErrNegativeLength",
            "ErrBytesTooLong",
            "ErrUntilEnd",
        ] {
            taken.insert(reserved.to_string());
        }
        for c in 0..prog.codecs.len() {
            taken.insert(decode_fn(prog, c));
            taken.insert(encode_fn(prog, c));
        }
        let mut type_go = Vec::with_capacity(prog.types.len());
        for t in &prog.types {
            let mut base = pascal(&t.name);
            let mut n = 2;
            while taken.contains(&base) {
                base = format!("{}{}", pascal(&t.name), n);
                n += 1;
            }
            taken.insert(base.clone());
            type_go.push(base);
        }
        // Count record payloads program-wide.
        let mut payload_uses: BTreeMap<TypeIdx, usize> = BTreeMap::new();
        for decl in prog.types.iter() {
            let crate::ir::typed::TypeKind::Sum { variants } = &decl.kind else {
                continue;
            };
            for v in variants {
                if let Some(Ty::Named(t)) = &v.payload
                    && matches!(
                        prog.types[*t].kind,
                        crate::ir::typed::TypeKind::Record { .. }
                    )
                {
                    *payload_uses.entry(*t).or_insert(0) += 1;
                }
            }
        }
        let mut wrappers = BTreeMap::new();
        let mut direct = BTreeSet::new();
        for (s, decl) in prog.types.iter().enumerate() {
            let crate::ir::typed::TypeKind::Sum { variants } = &decl.kind else {
                continue;
            };
            for (v, variant) in variants.iter().enumerate() {
                let Some(payload) = &variant.payload else {
                    wrappers.insert(
                        (s, v),
                        Self::claim(
                            &mut taken,
                            &format!("{}{}", type_go[s], pascal(&variant.name)),
                        ),
                    );
                    continue;
                };
                let is_direct = match payload {
                    Ty::Named(t) => {
                        let is_record = matches!(
                            prog.types[*t].kind,
                            crate::ir::typed::TypeKind::Record { .. }
                        );
                        is_record && payload_uses.get(t).copied().unwrap_or(0) == 1
                    }
                    _ => false,
                };
                if is_direct {
                    direct.insert((s, v));
                } else {
                    wrappers.insert(
                        (s, v),
                        Self::claim(
                            &mut taken,
                            &format!("{}{}", type_go[s], pascal(&variant.name)),
                        ),
                    );
                }
            }
        }
        // Enum constants share the package scope with types.
        for (t, decl) in prog.types.iter().enumerate() {
            let crate::ir::typed::TypeKind::Enum { members, .. } = &decl.kind else {
                continue;
            };
            for m in members {
                Self::claim(&mut taken, &format!("{}{}", type_go[t], pascal(&m.name)));
            }
        }
        Ok(Data {
            prog,
            type_go,
            wrappers,
            direct,
        })
    }

    fn claim(taken: &mut BTreeSet<String>, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while taken.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        taken.insert(name.clone());
        name
    }
}

struct Printer<'a> {
    data: Data<'a>,
    flags: Flags,
    ver: crate::GoVersion,
}

impl<'a> Printer<'a> {
    fn new(prog: &'a Program, ver: crate::GoVersion) -> BmlResult<Printer<'a>> {
        Ok(Printer {
            data: Data::new(prog)?,
            flags: Flags::default(),
            ver,
        })
    }
}

// ----------------------------------------------------------------------
// Go types
// ----------------------------------------------------------------------

/// Go type for an integer class; `I128` never appears in shipped schemas.
fn int_go(ty: IntTy) -> BmlResult<&'static str> {
    match ty {
        IntTy::I8 => Ok("int8"),
        IntTy::I16 => Ok("int16"),
        IntTy::I32 => Ok("int32"),
        IntTy::I64 => Ok("int64"),
        IntTy::U8 => Ok("uint8"),
        IntTy::U16 => Ok("uint16"),
        IntTy::U32 => Ok("uint32"),
        IntTy::U64 => Ok("uint64"),
        IntTy::I128 => Err(emit("emit go: i128 has no Go type".to_string())),
    }
}

impl Data<'_> {
    /// Go type for `ty`; `optional` adds a pointer.
    fn go_ty(&self, ty: &Ty, optional: bool) -> BmlResult<String> {
        let base = match ty {
            Ty::Int(t) => int_go(*t)?.to_string(),
            Ty::Bool => "bool".into(),
            Ty::F32 => "float32".into(),
            Ty::F64 => "float64".into(),
            Ty::Str => "string".into(),
            Ty::Bytes => "[]byte".into(),
            Ty::List(inner) => format!("[]{}", self.go_ty(inner, false)?),
            Ty::Named(t) => self.type_go[*t].clone(),
        };
        Ok(if optional { format!("*{base}") } else { base })
    }

    /// Go type of an expression via [`Program::expr_ty`].
    fn expr_go_ty(&self, body: &Body, expr: &Expr) -> BmlResult<(String, bool)> {
        let (ty, optional) = self.prog.expr_ty(body, expr);
        Ok((self.go_ty(&ty, optional)?, optional))
    }
}

// ----------------------------------------------------------------------
// runtime.go
// ----------------------------------------------------------------------

const RUNTIME_BASE: &str = r#"// Package bmlgen is generated by bml from a BML binary-protocol schema: one
// Go type per schema record, sum, enum, or alias, and a Decode/Encode pair per
// codec over the Reader and Writer wire primitives defined here.
//
// A failing Decode or Encode returns an *Error carrying the codec name, the
// reader offset on decode, and a kind that errors.Is matches against the Err*
// sentinels.
package bmlgen

import (
	"encoding/binary"
	"errors"
	"fmt"
	"math"
	"strconv"
)

// ErrorKind identifies how decoding or encoding failed. The constants double
// as sentinel errors for errors.Is.
type ErrorKind string

// ErrorKind values, one per failure class; errors.Is matches *Error against them.
const (
	ErrShortBuffer    ErrorKind = "short buffer"
	ErrRequire        ErrorKind = "require failed"
	ErrInvalidUTF8    ErrorKind = "invalid utf-8"
	ErrNoMatchArm     ErrorKind = "no match arm"
	ErrMissingField   ErrorKind = "missing field"
	ErrWrongVariant   ErrorKind = "wrong variant"
	ErrNegativeLength ErrorKind = "negative length"
	ErrBytesTooLong   ErrorKind = "bytes too long"
	ErrUntilEnd       ErrorKind = "encode until end"
)

// Error returns the kind's wire-independent text.
func (k ErrorKind) Error() string {
	return "bmlgen: " + string(k)
}

// Error is a decode or encode failure. Decode errors carry the reader offset;
// encode errors use Offset -1.
type Error struct {
	Codec  string
	Offset int
	Kind   ErrorKind
	Detail string
}

// Error renders the codec, offset, and detail.
func (e *Error) Error() string {
	if e.Offset >= 0 {
		return "bmlgen: decode " + e.Codec + " at offset " + strconv.Itoa(e.Offset) + ": " + e.Detail
	}
	return "bmlgen: encode " + e.Codec + ": " + e.Detail
}

// Is reports whether e's kind matches an ErrorKind target, so errors.Is(err, ErrShortBuffer)
// works through any layer of wrapping.
func (e *Error) Is(target error) bool {
	k, ok := target.(ErrorKind)
	return ok && e.Kind == k
}

// Unwrap returns the kind, so errors.Is finds error kinds without Is.
func (e *Error) Unwrap() error {
	return e.Kind
}

// decodeError reports a decode failure in codec at the reader's offset.
func decodeError(codec string, r *Reader, kind ErrorKind, detail string) *Error {
	return &Error{Codec: codec, Offset: r.Offset(), Kind: kind, Detail: detail}
}

// encodeError reports an encode failure in codec.
func encodeError(codec string, kind ErrorKind, detail string) *Error {
	return &Error{Codec: codec, Offset: -1, Kind: kind, Detail: detail}
}

// failDecode attaches the calling codec to a reader or sub-codec error.
func failDecode(codec string, r *Reader, err error) *Error {
	var bmlErr *Error
	if errors.As(err, &bmlErr) {
		if bmlErr.Codec != "" {
			return bmlErr
		}
		return decodeError(codec, r, bmlErr.Kind, bmlErr.Detail)
	}
	return decodeError(codec, r, ErrShortBuffer, err.Error())
}

// failEncode attaches the calling codec to a writer or sub-codec error.
func failEncode(codec string, err error) *Error {
	var bmlErr *Error
	if errors.As(err, &bmlErr) {
		if bmlErr.Codec != "" {
			return bmlErr
		}
		return encodeError(codec, bmlErr.Kind, bmlErr.Detail)
	}
	return encodeError(codec, ErrBytesTooLong, err.Error())
}

// requireFailed reports a failed require in codec.
func requireFailed(codec string, r *Reader, detail string) *Error {
	return decodeError(codec, r, ErrRequire, "require failed: "+detail)
}

// Reader reads big-endian wire primitives over a byte slice.
type Reader struct {
	data []byte
	off  int
}

// NewReader returns a Reader over b.
func NewReader(b []byte) *Reader {
	return &Reader{data: b}
}

// Offset is the current read position.
func (r *Reader) Offset() int {
	return r.off
}

// Remaining is the number of unread bytes.
func (r *Reader) Remaining() int64 {
	return int64(len(r.data) - r.off)
}

// ReadU8 reads one byte.
func (r *Reader) ReadU8() (uint8, error) {
	if r.off >= len(r.data) {
		return 0, &Error{Offset: r.off, Kind: ErrShortBuffer, Detail: "need 1 byte, have 0"}
	}
	b := r.data[r.off]
	r.off++
	return b, nil
}

// ReadF32 reads a big-endian float32.
func (r *Reader) ReadF32() (float32, error) {
	b, err := r.ReadBytes(4)
	if err != nil {
		return 0, err
	}
	return math.Float32frombits(binary.BigEndian.Uint32(b)), nil
}

// ReadF64 reads a big-endian float64.
func (r *Reader) ReadF64() (float64, error) {
	b, err := r.ReadBytes(8)
	if err != nil {
		return 0, err
	}
	return math.Float64frombits(binary.BigEndian.Uint64(b)), nil
}

// ReadBytes reads and copies exactly n bytes.
func (r *Reader) ReadBytes(n int64) ([]byte, error) {
	if n < 0 {
		return nil, &Error{Offset: r.off, Kind: ErrNegativeLength, Detail: fmt.Sprintf("negative length %d", n)}
	}
	if int64(len(r.data)-r.off) < n {
		return nil, &Error{Offset: r.off, Kind: ErrShortBuffer, Detail: fmt.Sprintf("need %d bytes, have %d", n, len(r.data)-r.off)}
	}
	b := append([]byte(nil), r.data[r.off:r.off+int(n)]...)
	r.off += int(n)
	return b, nil
}

// Rest reads and copies every remaining byte.
func (r *Reader) Rest() []byte {
	b := append([]byte(nil), r.data[r.off:]...)
	r.off = len(r.data)
	return b
}

// peekU8 reports the next byte without consuming it.
func (r *Reader) peekU8() (uint8, bool) {
	if r.off >= len(r.data) {
		return 0, false
	}
	return r.data[r.off], true
}

// skip advances past n bytes, clamping at the end.
func (r *Reader) skip(n int) {
	r.off += n
	if r.off > len(r.data) {
		r.off = len(r.data)
	}
}

// Writer appends big-endian wire primitives to a byte slice.
type Writer struct {
	buf []byte
}

// Bytes returns the written bytes.
func (w *Writer) Bytes() []byte {
	return w.buf
}

// WriteU8 writes one byte.
func (w *Writer) WriteU8(v uint8) {
	w.buf = append(w.buf, v)
}

// WriteF32 writes a big-endian float32.
func (w *Writer) WriteF32(v float32) {
	var b [4]byte
	binary.BigEndian.PutUint32(b[:], math.Float32bits(v))
	w.buf = append(w.buf, b[:]...)
}

// WriteF64 writes a big-endian float64.
func (w *Writer) WriteF64(v float64) {
	var b [8]byte
	binary.BigEndian.PutUint64(b[:], math.Float64bits(v))
	w.buf = append(w.buf, b[:]...)
}

// WriteBytes appends b.
func (w *Writer) WriteBytes(b []byte) {
	w.buf = append(w.buf, b...)
}

// WriteFixed appends b padded with zero bytes to length n.
func (w *Writer) WriteFixed(b []byte, n int64) error {
	if int64(len(b)) > n {
		return &Error{Offset: -1, Kind: ErrBytesTooLong, Detail: fmt.Sprintf("bytes length %d exceeds %d", len(b), n)}
	}
	w.buf = append(w.buf, b...)
	for i := int64(len(b)); i < n; i++ {
		w.buf = append(w.buf, 0)
	}
	return nil
}
"#;

const HELPER_SIGN_EXTEND: &str = r#"
// signExtend interprets bit bits-1 of v as the sign bit.
func signExtend(v int64, bits int) int64 {
	if bits <= 0 || bits >= 64 {
		return v
	}
	mask := int64(1) << (bits - 1)
	if v&mask != 0 {
		return v - (int64(1) << bits)
	}
	return v
}
"#;

const HELPER_UTF16_UNITS: &str = r#"
// utf16Units counts the UTF-16 code units of s.
func utf16Units(s string) int64 {
	var n int64
	for _, c := range s {
		if c > 0xFFFF {
			n += 2
		} else {
			n++
		}
	}
	return n
}
"#;

const HELPER_FLOOR_DIV: &str = r#"
// floorDiv divides and floors toward negative infinity.
func floorDiv(a, b int64) int64 {
	q := a / b
	if a%b != 0 && (a < 0) != (b < 0) {
		q--
	}
	return q
}
"#;

const HELPER_FLOOR_MOD: &str = r#"
// floorMod takes the remainder with the divisor's sign.
func floorMod(a, b int64) int64 {
	m := a % b
	if m != 0 && (m < 0) != (b < 0) {
		m += b
	}
	return m
}
"#;

const HELPER_B2I: &str = r#"
// b2i converts a bool to 1 or 0.
func b2i(b bool) int64 {
	if b {
		return 1
	}
	return 0
}
"#;

impl Printer<'_> {
    fn runtime_go(&self) -> String {
        let mut out = RUNTIME_BASE.to_string();
        if self.flags.sign_extend {
            out.push_str(HELPER_SIGN_EXTEND);
        }
        if self.flags.utf16_units {
            out.push_str(HELPER_UTF16_UNITS);
        }
        if self.flags.floor_div {
            out.push_str(HELPER_FLOOR_DIV);
        }
        if self.flags.floor_mod {
            out.push_str(HELPER_FLOOR_MOD);
        }
        if self.flags.b2i {
            out.push_str(HELPER_B2I);
        }
        out
    }
}

// ----------------------------------------------------------------------
// packets.go: types
// ----------------------------------------------------------------------

/// Pad `name` with spaces so every type in a block starts at the same column,
/// the way gofmt leaves it.
fn align_gap(name: &str, width: usize) -> String {
    " ".repeat(width - name.len() + 1)
}

/// `Key: value,` lines of a multi-line composite literal, aligned the way
/// gofmt (go/printer's `exprList`) aligns them: consecutive keys share a
/// value column unless a key is longer than 40 columns and its size breaks
/// from the running geometric mean of its section by 2.5x either way, which
/// starts a new section. A one-element literal is not aligned.
fn gofmt_key_values(pairs: &[(String, String)]) -> Vec<String> {
    if pairs.len() < 2 {
        return pairs.iter().map(|(k, v)| format!("{k}: {v},")).collect();
    }
    let mut sections: Vec<Vec<usize>> = vec![Vec::new()];
    let (mut lnsum, mut count) = (0.0f64, 0u32);
    for (i, (key, _)) in pairs.iter().enumerate() {
        let size = key.len();
        if i > 0 {
            let prev = pairs[i - 1].0.len();
            let new_section = if count == 0 || (prev <= 40 && size <= 40) {
                false
            } else {
                let ratio = size as f64 / (lnsum / f64::from(count)).exp();
                2.5 * ratio <= 1.0 || 2.5 <= ratio
            };
            if new_section {
                sections.push(Vec::new());
                lnsum = 0.0;
                count = 0;
            }
        }
        if let Some(section) = sections.last_mut() {
            section.push(i);
        }
        lnsum += (size as f64).ln();
        count += 1;
    }
    let mut out = Vec::with_capacity(pairs.len());
    for section in sections {
        let width = section.iter().map(|&i| pairs[i].0.len()).max().unwrap_or(0);
        for i in section {
            let (key, value) = &pairs[i];
            out.push(format!(
                "{key}:{}{value},",
                " ".repeat(width - key.len() + 1)
            ));
        }
    }
    out
}

/// `package bmlgen` plus the imports this file's text actually uses.
fn go_file(body: &str) -> String {
    let mut imports = Vec::new();
    if body.contains("fmt.") {
        imports.push("\"fmt\"");
    }
    if body.contains("utf8.") {
        imports.push("\"unicode/utf8\"");
    }
    let mut out = String::from("package bmlgen\n");
    match imports.as_slice() {
        [] => {}
        [one] => {
            out.push('\n');
            out.push_str(&format!("import {one}\n"));
        }
        many => {
            out.push_str("\nimport (\n");
            for import in many {
                out.push_str(&format!("\t{import}\n"));
            }
            out.push_str(")\n");
        }
    }
    if !body.is_empty() {
        out.push('\n');
        out.push_str(body);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
impl Printer<'_> {
    fn packets_files(&mut self) -> BmlResult<BTreeMap<String, String>> {
        let mut bodies: BTreeMap<String, String> = BTreeMap::new();
        for t in 0..self.data.prog.types.len() {
            let stem = self.data.prog.output_stem(&self.data.prog.types[t].origin);
            bodies.entry(stem).or_default().push_str(&self.type_go(t)?);
        }
        for c in 0..self.data.prog.codecs.len() {
            let stem = self.data.prog.output_stem(&self.data.prog.codecs[c].origin);
            bodies.entry(stem).or_default().push_str(&self.codec_go(c)?);
        }
        Ok(bodies
            .into_iter()
            .map(|(stem, body)| (stem, go_file(&body)))
            .collect())
    }

    fn type_go(&mut self, t: TypeIdx) -> BmlResult<String> {
        let doc = self.type_doc(t);
        let name = self.data.type_go[t].clone();
        match &self.data.prog.types[t].kind {
            crate::ir::typed::TypeKind::Record { fields } => {
                let mut out = format!("{doc}\ntype {name} struct {{\n");
                let width = fields
                    .iter()
                    .map(|f| pascal(&f.name).len())
                    .max()
                    .unwrap_or(0);
                for f in fields {
                    let goty = self.data.go_ty(&f.ty, f.optional)?;
                    out.push_str(&format!(
                        "\t{}{}{}\n",
                        pascal(&f.name),
                        align_gap(&pascal(&f.name), width),
                        goty
                    ));
                }
                out.push_str("}\n\n");
                Ok(out)
            }
            crate::ir::typed::TypeKind::Sum { variants } => {
                let mut out = format!("{doc}\ntype {name} interface {{\n\tis{name}()\n}}\n\n");
                for (v, variant) in variants.iter().enumerate() {
                    let variant_pascal = pascal(&variant.name);
                    if self.data.direct.contains(&(t, v)) {
                        let target = match &variant.payload {
                            Some(Ty::Named(target)) => *target,
                            _ => {
                                return Err(emit(format!(
                                    "emit go: {name}::{variant_pascal} is not a direct record"
                                )));
                            }
                        };
                        let target_go = self.data.type_go[target].clone();
                        out.push_str(&format!("func ({target_go}) is{name}() {{\n}}\n\n"));
                    } else {
                        let wrap = self.data.wrappers[&(t, v)].clone();
                        let wrap_doc = format!(
                            "// {wrap} is the {variant} variant of {name}.",
                            variant = variant.name.snake()
                        );
                        match &variant.payload {
                            None => {
                                out.push_str(&format!(
                                    "{wrap_doc}\ntype {wrap} struct{{}}\n\nfunc ({wrap}) is{name}() {{\n}}\n\n"
                                ));
                            }
                            Some(payload) => {
                                let payload_go = self.data.go_ty(payload, false)?;
                                out.push_str(&format!(
                                    "{wrap_doc}\ntype {wrap} struct {{\n\tValue {payload_go}\n}}\n\nfunc ({wrap}) is{name}() {{\n}}\n\n"
                                ));
                            }
                        }
                    }
                }
                Ok(out)
            }
            crate::ir::typed::TypeKind::Enum { base, members } => {
                let base_go = int_go(*base)?.to_string();
                let mut out = format!("{doc}\ntype {name} {base_go}\n\n");
                out.push_str(&format!(
                    "// {name} values, as named by the schema.\nconst (\n"
                ));
                let width = members
                    .iter()
                    .map(|m| format!("{name}{}", pascal(&m.name)).len())
                    .max()
                    .unwrap_or(0);
                for m in members {
                    let const_name = format!("{name}{}", pascal(&m.name));
                    out.push_str(&format!(
                        "\t{}{}{name} = {}\n",
                        const_name,
                        align_gap(&const_name, width),
                        m.value
                    ));
                }
                out.push_str(")\n\n");
                out.push_str(&format!(
                    "// String returns the schema name of v.\nfunc (v {name}) String() string {{\n\tswitch v {{\n"
                ));
                for m in members {
                    out.push_str(&format!(
                        "\tcase {name}{}:\n\t\treturn {}\n",
                        pascal(&m.name),
                        go_quote(&m.name.snake())
                    ));
                }
                self.flags.sprintf = true;
                out.push_str(&format!(
                    "\tdefault:\n\t\treturn fmt.Sprintf({}, {base_go}(v))\n\t}}\n}}\n\n",
                    go_quote(&format!("{name}(%d)"))
                ));
                Ok(out)
            }
            crate::ir::typed::TypeKind::Alias { target } => {
                let target_go = self.data.type_go[*target].clone();
                Ok(format!("{doc}\ntype {name} = {target_go}\n\n"))
            }
        }
    }

    fn type_doc(&self, t: TypeIdx) -> String {
        let decl = &self.data.prog.types[t];
        let name = &self.data.type_go[t];
        match &decl.origin {
            Origin::Packet {
                name: packet,
                routes,
            } => {
                if routes.is_empty() {
                    format!("// {name} is the {packet} packet.")
                } else {
                    let routes = routes
                        .iter()
                        .map(|r| format!("{} {} 0x{:02x}", r.state, r.direction, r.id))
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!("// {name} is the {packet} packet ({routes}).")
                }
            }
            Origin::Encoding { name: encoding } => {
                format!("// {name} is the {encoding} encoding.")
            }
            Origin::Enum { name: encoding } => format!("// {name} is the {encoding} enum."),
            Origin::Frame { name: frame } => format!("// {name} is the {frame} frame."),
            Origin::Field { parent, field } => {
                let parent_go = &self.data.type_go[*parent];
                format!("// {name} is the {field} of {parent_go}.")
            }
            Origin::Variant { sum, variant } => {
                let sum_go = &self.data.type_go[*sum];
                let variant_name = match &self.data.prog.types[*sum].kind {
                    crate::ir::typed::TypeKind::Sum { variants } => variants[*variant].name.snake(),
                    _ => "arm".to_string(),
                };
                format!("// {name} is the {variant_name} arm of {sum_go}.")
            }
        }
    }
}

// ----------------------------------------------------------------------
// packets.go: codecs
// ----------------------------------------------------------------------

/// One function body under emission.
struct BodyGen<'a> {
    data: &'a Data<'a>,
    body: &'a Body,
    codec_snake: String,
    is_decode: bool,
    ret_zero: String,
    cursor: String,
    err_name: String,
    local_go: Vec<String>,
    taken: BTreeSet<String>,
    tmp: usize,
    /// Locals needing an upfront `var`: first stored inside a nested block
    /// but used outside it (see [`prescan`]).
    predeclared: BTreeSet<usize>,
    /// Locals declared and in scope at this point of the output
    /// (`predeclared`, parameters, and every `:=` or `var` so far in an
    /// enclosing block). A store uses `:=` only when it introduces one.
    declared: BTreeSet<usize>,
    /// Whether `err` is in scope: an upfront `var` (see [`prescan`]), or the
    /// first `name, err := call` of this or an enclosing block.
    err_declared: bool,
    /// What each open block will restore when it closes: Go scopes `:=` and
    /// `var` to the block, so names declared inside are gone after it.
    scopes: Vec<(bool, BTreeSet<usize>, BTreeSet<String>)>,
    flags: Flags,
    ver: crate::GoVersion,
    out: String,
    indent: usize,
}

impl<'a> BodyGen<'a> {
    fn new(
        data: &'a Data<'a>,
        body: &'a Body,
        codec_snake: &str,
        is_decode: bool,
        ret_zero: &str,
        ver: crate::GoVersion,
    ) -> BodyGen<'a> {
        let mut taken = BTreeSet::new();
        let local_go = body
            .locals
            .iter()
            .map(|l| camel(&l.name))
            .collect::<Vec<_>>();
        for name in &local_go {
            taken.insert(name.clone());
        }
        let mut bg = BodyGen {
            data,
            body,
            codec_snake: codec_snake.to_string(),
            is_decode,
            ret_zero: ret_zero.to_string(),
            cursor: String::new(),
            err_name: String::new(),
            local_go,
            taken,
            tmp: 0,
            scopes: Vec::new(),
            predeclared: BTreeSet::new(),
            declared: BTreeSet::new(),
            err_declared: false,
            flags: Flags::default(),
            ver,
            out: String::new(),
            indent: 1,
        };
        let (pre, nested_err) = prescan(&body.stmts, &body.locals);
        bg.predeclared.extend(pre);
        bg.declared.extend(&bg.predeclared);
        bg.declared.extend(body.params.iter().copied());
        if !is_decode && let Some(value) = body.value {
            bg.declared.insert(value);
        }
        bg.err_declared = nested_err;
        bg.cursor = bg.pick(if is_decode { "r" } else { "w" });
        bg.err_name = bg.pick("err");
        bg
    }

    /// An unused identifier based on `base`.
    fn pick(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while self.taken.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        self.taken.insert(name.clone());
        name
    }

    /// A fresh temporary.
    fn temp(&mut self) -> String {
        loop {
            let name = format!("t{}", self.tmp);
            self.tmp += 1;
            if !self.taken.contains(&name) {
                self.taken.insert(name.clone());
                return name;
            }
        }
    }

    /// `=` or `:=` for storing into local `id`: declare at first assignment.
    fn store_op(&mut self, id: usize) -> &'static str {
        if self.declared.contains(&id) {
            "="
        } else {
            self.declared.insert(id);
            ":="
        }
    }

    /// `=` or `:=` for `dst, err := call`: `:=` while either side is new, so
    /// at least one side is always introduced and the line always compiles.
    /// `dst` is `None` for a discarded value (`_, err := call`).
    fn call_op(&mut self, dst: Option<usize>) -> &'static str {
        let fresh = !self.err_declared || dst.is_some_and(|d| !self.declared.contains(&d));
        self.err_declared = true;
        if let Some(d) = dst {
            self.declared.insert(d);
        }
        if fresh { ":=" } else { "=" }
    }

    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push('\t');
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn open(&mut self, text: &str) {
        self.line(&format!("{text} {{"));
        self.indent += 1;
        self.enter();
    }

    fn close(&mut self) {
        self.leave();
        self.indent -= 1;
        self.line("}");
    }

    /// Start a Go block: what it declares goes out of scope at [`Self::leave`].
    fn enter(&mut self) {
        self.scopes
            .push((self.err_declared, self.declared.clone(), self.taken.clone()));
    }

    fn leave(&mut self) {
        if let Some((err_declared, declared, taken)) = self.scopes.pop() {
            self.err_declared = err_declared;
            self.declared = declared;
            self.taken = taken;
        }
    }

    /// `stmts` as the body of an `if`, `else`, or `case`: one indent, one scope.
    fn block(&mut self, stmts: &[Stmt]) -> BmlResult<()> {
        self.indent += 1;
        self.enter();
        let result = self.stmts(stmts);
        self.leave();
        self.indent -= 1;
        result
    }

    fn fail(&mut self, kind: &str, detail: &str) -> String {
        let codec = go_quote(&self.codec_snake);
        if self.is_decode {
            format!(
                "return {}, decodeError({codec}, {}, {kind}, {detail})",
                self.ret_zero, self.cursor,
            )
        } else {
            format!("return encodeError({codec}, {kind}, {detail})")
        }
    }

    /// `expr` converted to `int64`.
    fn as_i64(&mut self, expr: &Expr, depth: u8) -> BmlResult<String> {
        let text = self.expr_at(expr, depth)?;
        let (ty, optional) = self.data.prog.expr_ty(self.body, expr);
        if !optional && matches!(ty, Ty::Int(IntTy::I64)) {
            Ok(text)
        } else if optional {
            Ok(format!("int64({})", deref(&text)))
        } else {
            Ok(format!("int64({text})"))
        }
    }

    /// `expr` converted to an exact Go type.
    fn as_ty(&mut self, expr: &Expr, want: &str, depth: u8) -> BmlResult<String> {
        let text = self.expr_at(expr, depth)?;
        let (got, _) = self.data.expr_go_ty(self.body, expr)?;
        if got == want {
            Ok(text)
        } else {
            Ok(format!("{want}({text})"))
        }
    }

    /// A repeat count that is a plain integer local or field (possibly widened
    /// to `int64` by the program): `for range` takes any integer type, so the
    /// loop uses it as is. Returns its text and whether it is unsigned (and
    /// so needs no negative check).
    fn plain_count(&mut self, n: &Expr) -> BmlResult<Option<(String, bool)>> {
        let inner = match n {
            Expr::Cast {
                expr,
                to: Ty::Int(IntTy::I64),
            } => expr.as_ref(),
            other => other,
        };
        if !matches!(inner, Expr::Local(_) | Expr::Field { .. }) {
            return Ok(None);
        }
        let (ty, optional) = self.data.prog.expr_ty(self.body, inner);
        let Ty::Int(int) = ty else {
            return Ok(None);
        };
        if optional || int == IntTy::I128 {
            return Ok(None);
        }
        Ok(Some((self.expr(inner)?, !int.signed())))
    }
}

/// `*x`, with parentheses only when `x` is more than a name or selector.
fn deref(text: &str) -> String {
    let plain = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if plain {
        format!("*{text}")
    } else {
        format!("*({text})")
    }
}

impl Printer<'_> {
    fn codec_go(&mut self, c: CodecIdx) -> BmlResult<String> {
        let prog = self.data.prog;
        let codec = &prog.codecs[c];
        let go_name = pascal(&codec.name);
        let ret_go = self.data.go_ty(&codec.value, false)?;
        let ret_zero = zero_value(&self.data, &codec.value)?;
        let value_doc = doc_value(&self.data, &codec.value)?;
        let params = codec.params.clone();
        let decode_body = codec.decode.clone();
        let encode_body = codec.encode.clone();
        let snake = codec.name.snake();
        let decode_name = decode_fn(prog, c);
        let encode_name = encode_fn(prog, c);

        // Decode.
        let (dsig, dout) = {
            let mut bg = BodyGen::new(&self.data, &decode_body, &snake, true, &ret_zero, self.ver);
            let mut sig = format!("// Decode{go_name} decodes {value_doc}.");
            sig.push_str(&format!("\nfunc {decode_name}({} *Reader", bg.cursor));
            for (param, local) in params.iter().zip(decode_body.params.iter()) {
                let ty = self.data.go_ty(&param.ty, param.optional)?;
                sig.push_str(&format!(", {} {ty}", bg.local_go[*local]));
            }
            sig.push_str(&format!(") ({ret_go}, error) {{\n"));
            let mut vars = String::new();
            if bg.err_declared {
                vars.push_str(&format!("\tvar {} error\n", bg.err_name));
            }
            for (i, local) in decode_body.locals.iter().enumerate() {
                if decode_body.params.contains(&i) {
                    continue;
                }
                // Declared where first stored below; only locals used outside
                // the block of that store need an upfront `var`.
                if !bg.predeclared.contains(&i) {
                    continue;
                }
                vars.push_str(&format!(
                    "\tvar {} {}\n",
                    bg.local_go[i],
                    self.data.go_ty(&local.ty, local.optional)?
                ));
            }
            bg.stmts(&decode_body.stmts)?;
            // A `Cond` declares its temporary in place (see `Expr::Cond`).
            if !diverges(&decode_body.stmts) {
                let fail = bg.fail("ErrNoMatchArm", &go_quote("unreachable"));
                bg.line(&fail);
            }
            let flags = std::mem::take(&mut bg.flags);
            let out = std::mem::take(&mut bg.out);
            self.flags.absorb(&flags);
            (format!("{sig}{vars}"), out)
        };
        // Encode.
        let (esig, eout) = {
            let mut bg = BodyGen::new(&self.data, &encode_body, &snake, false, "", self.ver);
            let Some(value_id) = encode_body.value else {
                return Err(emit(format!("emit go: {snake} has no encode value")));
            };
            let value_go = bg.local_go[value_id].clone();
            let mut sig = format!("// Encode{go_name} encodes {value_doc}.");
            sig.push_str(&format!(
                "\nfunc {encode_name}({} *Writer, {} {}",
                bg.cursor,
                value_go,
                self.data.go_ty(&codec.value, false)?
            ));
            for (param, local) in params.iter().zip(encode_body.params.iter()) {
                let ty = self.data.go_ty(&param.ty, param.optional)?;
                sig.push_str(&format!(", {} {ty}", bg.local_go[*local]));
            }
            sig.push_str(") error {\n");
            let mut vars = String::new();
            if bg.err_declared {
                vars.push_str(&format!("\tvar {} error\n", bg.err_name));
            }
            for (i, local) in encode_body.locals.iter().enumerate() {
                if i == value_id || encode_body.params.contains(&i) {
                    continue;
                }
                // Declared where first stored below; only locals used outside
                // the block of that store need an upfront `var`.
                if !bg.predeclared.contains(&i) {
                    continue;
                }
                vars.push_str(&format!(
                    "\tvar {} {}\n",
                    bg.local_go[i],
                    self.data.go_ty(&local.ty, local.optional)?
                ));
            }
            bg.stmts(&encode_body.stmts)?;
            bg.line("return nil");
            let flags = std::mem::take(&mut bg.flags);
            let out = std::mem::take(&mut bg.out);
            self.flags.absorb(&flags);
            (format!("{sig}{vars}"), out)
        };
        Ok(format!("{dsig}{dout}}}\n\n{esig}{eout}}}\n\n"))
    }
}

/// Where each local is declared, and whether `err` needs an upfront `var`.
///
/// A local is declared at its first store (`:=`, or an in-place `var` for a
/// `Declare`) when every later occurrence, store or read, lies inside the
/// block of that store: a sum arm's payload, a loop item, a group's own
/// temporaries. A local first stored in a nested block and used outside it
/// must be function-scoped, so it gets an upfront `var`; that set is
/// returned.
///
/// A nested fallible call that stores into a local declared in an enclosing
/// block prints `name, err = call`, so an `err` must already be in scope
/// there. It is when an earlier top-level call declared it (`x, err :=`);
/// otherwise `err` gets an upfront `var` too. An optional destination goes
/// through a fresh `tmp, err :=` and an unused one through `_, err :=`, which
/// need nothing.
fn prescan(stmts: &[Stmt], locals: &[Local]) -> (BTreeSet<usize>, bool) {
    let mut scan = Scan::new();
    scan.list(stmts);
    let upfront: BTreeSet<usize> = scan
        .first
        .iter()
        .filter(|(id, home)| home.len() > 1 && scan.escapes.contains(*id))
        .map(|(id, _)| *id)
        .collect();
    let nested_err = scan.nested.iter().any(|(id, path, top_err)| {
        let local = &locals[*id];
        let outer = upfront.contains(id)
            || scan
                .first
                .get(id)
                .is_some_and(|home| home.len() < path.len());
        !top_err && outer && local.uses > 0 && !local.optional
    });
    (upfront, nested_err)
}

/// [`prescan`]'s walk: block paths of first stores and of every occurrence.
struct Scan {
    /// The block path of each local's first store.
    first: BTreeMap<usize, Vec<usize>>,
    /// Locals with an occurrence outside the block of their first store.
    escapes: BTreeSet<usize>,
    /// Nested fallible stores: local, block path, and whether a top-level
    /// fallible call had already declared `err` at function scope.
    nested: Vec<(usize, Vec<usize>, bool)>,
    top_err: bool,
    path: Vec<usize>,
    blocks: usize,
}

impl Scan {
    fn new() -> Scan {
        Scan {
            first: BTreeMap::new(),
            escapes: BTreeSet::new(),
            nested: Vec::new(),
            top_err: false,
            path: vec![0],
            blocks: 1,
        }
    }

    fn touch(&mut self, id: usize) {
        match self.first.get(&id) {
            None => {
                self.first.insert(id, self.path.clone());
            }
            Some(home) => {
                if !self.path.starts_with(home) {
                    self.escapes.insert(id);
                }
            }
        }
    }

    fn fallible(&mut self, id: usize) {
        self.touch(id);
        if self.path.len() > 1 {
            self.nested.push((id, self.path.clone(), self.top_err));
        } else {
            self.top_err = true;
        }
    }

    fn enter(&mut self) {
        self.path.push(self.blocks);
        self.blocks += 1;
    }

    fn block(&mut self, stmts: &[Stmt]) {
        self.enter();
        self.list(stmts);
        self.path.pop();
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Local(id) => self.touch(*id),
            Expr::Field { base, .. }
            | Expr::Present(base)
            | Expr::Get(base)
            | Expr::Not(base)
            | Expr::StrBytes(base)
            | Expr::Cast { expr: base, .. } => self.expr(base),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Cond {
                cond, then, els, ..
            } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            Expr::Call { args, .. } | Expr::Construct { fields: args, .. } => {
                for arg in args {
                    self.expr(arg);
                }
            }
            Expr::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    self.expr(payload);
                }
            }
            Expr::Int { .. } | Expr::Bool(_) | Expr::Str(_) | Expr::Remaining => {}
        }
    }

    fn list(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::Declare { local, .. } => self.touch(*local),
                Stmt::Let { dst, value } => {
                    self.expr(value);
                    self.touch(dst.local);
                }
                Stmt::Read { dst, op } => match op {
                    ReadOp::Rest => self.touch(dst.local),
                    ReadOp::Bytes(n) => {
                        self.expr(n);
                        self.fallible(dst.local);
                    }
                    _ => self.fallible(dst.local),
                },
                Stmt::Decode { dst, args, .. } => {
                    for arg in args {
                        self.expr(arg);
                    }
                    self.fallible(dst.local);
                }
                Stmt::Utf8 { dst, src } => {
                    self.expr(src);
                    self.touch(dst.local);
                }
                Stmt::Write { op, value } => {
                    if let WriteOp::Bytes(n) = op {
                        self.expr(n);
                    }
                    self.expr(value);
                }
                Stmt::Encode { args, value, .. } => {
                    for arg in args {
                        self.expr(arg);
                    }
                    self.expr(value);
                }
                Stmt::Require { cond, .. } => self.expr(cond),
                Stmt::Unwrap { dst, src, .. } => {
                    self.expr(src);
                    self.touch(dst.local);
                }
                Stmt::If { cond, then, els } => {
                    self.expr(cond);
                    self.block(then);
                    self.block(els);
                }
                Stmt::Match {
                    scrutinee,
                    arms,
                    other,
                } => {
                    self.expr(scrutinee);
                    for arm in arms {
                        self.block(&arm.body);
                    }
                    if let Some(other) = other {
                        self.block(other);
                    }
                }
                Stmt::Repeat {
                    mode,
                    list,
                    item,
                    body,
                } => {
                    if let RepeatMode::Count(e) | RepeatMode::Until(e) = mode {
                        self.expr(e);
                    }
                    self.touch(*list);
                    // The body stores the item; the loop appends it there.
                    self.enter();
                    self.list(body);
                    self.touch(*item);
                    self.touch(*list);
                    self.path.pop();
                }
                Stmt::ForEach { list, item, body } => {
                    self.expr(list);
                    // The range clause declares the item in the loop body.
                    self.enter();
                    self.touch(*item);
                    self.list(body);
                    self.path.pop();
                }
                Stmt::ExpectVariant { dst, sum, .. } => {
                    self.expr(sum);
                    if let Some(dst) = dst {
                        self.touch(dst.local);
                    }
                }
                Stmt::Return(value) => self.expr(value),
                Stmt::Fail(_) => {}
            }
        }
    }
}

/// Whether an expression prints atomically (no parentheses needed as an
/// operand or dereference target).
fn is_atomic(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Int { .. }
            | Expr::Bool(_)
            | Expr::Str(_)
            | Expr::Local(_)
            | Expr::Field { .. }
            | Expr::Call { .. }
            | Expr::Cast { .. }
            | Expr::StrBytes(_)
            | Expr::Get(_)
            | Expr::Remaining
    )
}

/// Go precedence level: `||`, `&&`, comparisons, additive, multiplicative.
fn prec(op: BinOp) -> u8 {
    match op {
        BinOp::Or => 1,
        BinOp::And => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
        BinOp::Add | BinOp::Sub | BinOp::BitOr | BinOp::BitXor => 4,
        BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Shl | BinOp::Shr | BinOp::BitAnd => 5,
    }
}

/// go/printer's spacing cutoff for one binary tree: levels 1-3 always take
/// spaces; whether levels 4 (`+ - | ^`) and 5 (`* / % << >> &`) do depends on
/// the mix in the tree and the nesting depth.
fn cutoff(has4: bool, has5: bool, max_problem: u8, depth: u8) -> u8 {
    if max_problem > 0 {
        return max_problem + 1;
    }
    if has4 && has5 {
        if depth == 1 {
            return 5;
        }
        return 4;
    }
    if depth == 1 {
        return 6;
    }
    4
}

/// The `has4`/`has5` mix of the operators in a binary tree, stopping at
/// operands gofmt would parenthesize or treat as primary (calls), plus the
/// `maxProblem` clash level for a right-hand `*deref` under `/`.
fn walk_binary(op: BinOp, lhs: &Expr, rhs: &Expr) -> (bool, bool, u8) {
    let (mut has4, mut has5) = (false, false);
    let mut problem = 0;
    match prec(op) {
        4 => has4 = true,
        5 => has5 = true,
        _ => {}
    }
    if let Expr::Binary {
        op: child,
        lhs: l,
        rhs: r,
        ..
    } = lhs
        && prec(*child) >= prec(op)
    {
        let (a, b, m) = walk_binary(*child, l, r);
        has4 |= a;
        has5 |= b;
        problem = problem.max(m);
    }
    match rhs {
        Expr::Binary {
            op: child,
            lhs: l,
            rhs: r,
            ..
        } if prec(*child) > prec(op) => {
            let (a, b, m) = walk_binary(*child, l, r);
            has4 |= a;
            has5 |= b;
            problem = problem.max(m);
        }
        // `a / *b` would lex as a comment start without a space.
        Expr::Get(_) if op == BinOp::Div => {
            problem = problem.max(5);
        }
        _ => {}
    }
    (has4, has5, problem)
}

/// Depth bump for a left operand: same-precedence chains share the depth.
fn diff_prec(child: &Expr, parent_level: u8) -> u8 {
    match child {
        Expr::Binary { op: child_op, .. } if prec(*child_op) == parent_level => 0,
        _ => 1,
    }
}

/// Parentheses undo one level of gofmt nesting depth.
fn reduce_depth(depth: u8) -> u8 {
    depth.saturating_sub(1).max(1)
}

impl<'a> BodyGen<'a> {
    fn stmts(&mut self, stmts: &[Stmt]) -> BmlResult<()> {
        let mut i = 0;
        while let Some(stmt) = stmts.get(i) {
            if matches!(stmt, Stmt::Declare { .. }) {
                i = self.declare_run(stmts, i)?;
            } else {
                self.stmt(stmt)?;
                i += 1;
            }
        }
        Ok(())
    }

    /// The run of `Declare`s from `stmts[at]`: each type in a row shares one
    /// `var a, b T` line, and locals already in scope print nothing. Returns
    /// the index after the run.
    fn declare_run(&mut self, stmts: &[Stmt], at: usize) -> BmlResult<usize> {
        let mut names: Vec<String> = Vec::new();
        let mut run_ty = String::new();
        let mut end = at;
        while let Some(Stmt::Declare { local: id, .. }) = stmts.get(end) {
            end += 1;
            if self.declared.contains(id) {
                continue;
            }
            let local = &self.body.locals[*id];
            let ty = self.data.go_ty(&local.ty, local.optional)?;
            if ty != run_ty && !names.is_empty() {
                self.line(&format!("var {} {run_ty}", names.join(", ")));
                names.clear();
            }
            run_ty = ty;
            names.push(self.local_go[*id].clone());
            self.declared.insert(*id);
        }
        if !names.is_empty() {
            self.line(&format!("var {} {run_ty}", names.join(", ")));
        }
        Ok(end)
    }

    /// Wrap a fallible read in its error check, preserving the inner codec.
    fn decode_check(&mut self, err: &str) -> BmlResult<()> {
        let cur = self.cursor.clone();
        self.open(&format!("if {err} != nil"));
        if self.is_decode {
            let zero = self.ret_zero.clone();
            let snake = go_quote(&self.codec_snake);
            self.line(&format!("return {zero}, failDecode({snake}, {cur}, {err})"));
        } else {
            let snake = go_quote(&self.codec_snake);
            self.line(&format!("return failEncode({snake}, {err})"));
        }
        self.close();
        Ok(())
    }

    /// Check the error of a sub-codec call, preserving the inner codec.
    fn check_call_err(&mut self, err: &str) -> BmlResult<()> {
        if self.is_decode {
            let zero = self.ret_zero.clone();
            self.open(&format!("if {err} != nil"));
            self.line(&format!("return {zero}, {err}"));
            self.close();
        } else {
            self.open(&format!("if {err} != nil"));
            self.line(&format!("return {err}"));
            self.close();
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> BmlResult<()> {
        match stmt {
            // Printed by `declare_run`, which sees the whole run.
            Stmt::Declare { .. } => Ok(()),
            Stmt::Let { dst, value } => {
                let uses = self.body.locals[dst.local].uses;
                if uses == 0 {
                    return Ok(());
                }
                let text = self.expr(value)?;
                let name = self.local_go[dst.local].clone();
                if dst.some {
                    // `tmp := x`, converted when `x` is an untyped constant
                    // or prints as another Go type.
                    let (ty, _) = self.data.prog.expr_ty(self.body, value);
                    let goty = self.data.go_ty(&ty, false)?;
                    let (got, _) = self.data.expr_go_ty(self.body, value)?;
                    let typed = if got == goty && !matches!(value, Expr::Int { .. }) {
                        text
                    } else {
                        format!("{goty}({text})")
                    };
                    let tmp = self.temp();
                    self.line(&format!("{tmp} := {typed}"));
                    let op = self.store_op(dst.local);
                    self.line(&format!("{name} {op} &{tmp}"));
                } else {
                    let op = self.store_op(dst.local);
                    self.line(&format!("{name} {op} {text}"));
                }
                Ok(())
            }
            Stmt::Read { dst, op } => {
                let uses = self.body.locals[dst.local].uses;
                let optional = self.body.locals[dst.local].optional;
                let name = self.local_go[dst.local].clone();
                let cur = self.cursor.clone();
                if matches!(op, ReadOp::Rest) {
                    if uses == 0 {
                        self.line(&format!("{cur}.Rest()"));
                    } else if optional {
                        let tmp = self.temp();
                        self.line(&format!("{tmp} := {cur}.Rest()"));
                        let assign = self.store_op(dst.local);
                        self.line(&format!("{name} {assign} &{tmp}"));
                    } else {
                        let assign = self.store_op(dst.local);
                        self.line(&format!("{name} {assign} {cur}.Rest()"));
                    }
                    return Ok(());
                }
                let call = match op {
                    ReadOp::U8 => format!("{cur}.ReadU8()"),
                    ReadOp::F32 => format!("{cur}.ReadF32()"),
                    ReadOp::F64 => format!("{cur}.ReadF64()"),
                    ReadOp::Bytes(n) => format!("{cur}.ReadBytes({})", self.as_i64(n, 1)?),
                    ReadOp::Rest => unreachable!(),
                };
                let err = self.err_name.clone();
                if uses == 0 {
                    let assign = self.call_op(None);
                    self.line(&format!("_, {err} {assign} {call}"));
                    self.decode_check(&err)?;
                } else if optional {
                    // A fresh temporary; `:=` always introduces it. In a
                    // nested block this shadows the outer `err`, which is
                    // still correct: it is checked immediately below.
                    let tmp = self.temp();
                    self.line(&format!("{tmp}, {err} := {call}"));
                    self.err_declared = true;
                    self.decode_check(&err)?;
                    let assign = self.store_op(dst.local);
                    self.line(&format!("{name} {assign} &{tmp}"));
                } else {
                    let assign = self.call_op(Some(dst.local));
                    self.line(&format!("{name}, {err} {assign} {call}"));
                    self.decode_check(&err)?;
                }
                Ok(())
            }
            Stmt::Decode { dst, codec, args } => {
                let uses = self.body.locals[dst.local].uses;
                let optional = self.body.locals[dst.local].optional;
                let name = self.local_go[dst.local].clone();
                let prog = self.data.prog;
                let params = prog.codecs[*codec].params.clone();
                let cur = self.cursor.clone();
                let mut call = format!("{}({cur}", decode_fn(prog, *codec));
                let arg_depth = if params.is_empty() { 1 } else { 2 };
                for (arg, param) in args.iter().zip(params.iter()) {
                    call.push_str(&format!(
                        ", {}",
                        self.coerce_arg(arg, &param.ty, param.optional, arg_depth)?
                    ));
                }
                call.push(')');
                let err = self.err_name.clone();
                if uses == 0 {
                    let assign = self.call_op(None);
                    self.line(&format!("_, {err} {assign} {call}"));
                    self.check_call_err(&err)?;
                } else if optional {
                    // A fresh temporary; `:=` always introduces it. In a
                    // nested block this shadows the outer `err`, which is
                    // still correct: it is checked immediately below.
                    let tmp = self.temp();
                    self.line(&format!("{tmp}, {err} := {call}"));
                    self.err_declared = true;
                    self.check_call_err(&err)?;
                    let assign = self.store_op(dst.local);
                    self.line(&format!("{name} {assign} &{tmp}"));
                } else {
                    let assign = self.call_op(Some(dst.local));
                    self.line(&format!("{name}, {err} {assign} {call}"));
                    self.check_call_err(&err)?;
                }
                Ok(())
            }
            Stmt::Utf8 { dst, src } => {
                let uses = self.body.locals[dst.local].uses;
                let optional = self.body.locals[dst.local].optional;
                let name = self.local_go[dst.local].clone();
                let text = self.expr(src)?;
                self.flags.utf8_valid = true;
                let detail = go_quote(&format!("invalid utf-8 in {}", self.codec_snake));
                if uses == 0 || optional {
                    let tmp = self.temp();
                    self.line(&format!("{tmp} := string({text})"));
                    self.open(&format!("if !utf8.ValidString({tmp})"));
                    let fail = self.fail("ErrInvalidUTF8", &detail);
                    self.line(&fail);
                    self.close();
                    if uses > 0 {
                        let assign = self.store_op(dst.local);
                        self.line(&format!("{name} {assign} &{tmp}"));
                    }
                } else {
                    let assign = self.store_op(dst.local);
                    self.line(&format!("{name} {assign} string({text})"));
                    self.open(&format!("if !utf8.ValidString({name})"));
                    let fail = self.fail("ErrInvalidUTF8", &detail);
                    self.line(&fail);
                    self.close();
                }
                Ok(())
            }
            Stmt::Write { op, value } => {
                let cur = self.cursor.clone();
                match op {
                    WriteOp::U8 => {
                        let text = match byte_literal(value, true) {
                            Some(byte) => byte,
                            None => self.as_ty(value, "uint8", 1)?,
                        };
                        self.line(&format!("{cur}.WriteU8({text})"));
                    }
                    WriteOp::F32 => {
                        let text = self.as_ty(value, "float32", 1)?;
                        self.line(&format!("{cur}.WriteF32({text})"));
                    }
                    WriteOp::F64 => {
                        let text = self.as_ty(value, "float64", 1)?;
                        self.line(&format!("{cur}.WriteF64({text})"));
                    }
                    WriteOp::Bytes(n) => {
                        let bytes = self.as_ty(value, "[]byte", 2)?;
                        let count = self.as_i64(n, 2)?;
                        let err = self.err_name.clone();
                        self.open(&format!(
                            "if {err} := {cur}.WriteFixed({bytes}, {count}); {err} != nil"
                        ));
                        if self.is_decode {
                            let fail = self.fail("ErrBytesTooLong", &go_quote("bytes too long"));
                            self.line(&fail);
                        } else {
                            let snake = go_quote(&self.codec_snake);
                            self.line(&format!("return failEncode({snake}, {err})"));
                        }
                        self.close();
                    }
                    WriteOp::Rest => {
                        let bytes = self.as_ty(value, "[]byte", 1)?;
                        self.line(&format!("{cur}.WriteBytes({bytes})"));
                    }
                }
                Ok(())
            }
            Stmt::Encode { codec, args, value } => {
                let prog = self.data.prog;
                let params = prog.codecs[*codec].params.clone();
                let target = prog.codecs[*codec].value.clone();
                let arg_depth = if params.is_empty() { 1 } else { 2 };
                let text = self.coerce_to(value, &target, false, arg_depth)?;
                let cur = self.cursor.clone();
                let mut call = format!("{}({cur}, {text}", encode_fn(prog, *codec));
                for (arg, param) in args.iter().zip(params.iter()) {
                    call.push_str(&format!(
                        ", {}",
                        self.coerce_arg(arg, &param.ty, param.optional, arg_depth)?
                    ));
                }
                call.push(')');
                let err = self.err_name.clone();
                self.open(&format!("if {err} := {call}; {err} != nil"));
                if self.is_decode {
                    let zero = self.ret_zero.clone();
                    self.line(&format!("return {zero}, {err}"));
                } else {
                    self.line(&format!("return {err}"));
                }
                self.close();
                Ok(())
            }
            Stmt::Require { cond, text } => {
                let neg = self.data.prog.negated(self.body, cond);
                let check = self.expr(&neg)?;
                self.open(&format!("if {check}"));
                if self.is_decode {
                    let zero = self.ret_zero.clone();
                    let cur = self.cursor.clone();
                    let snake = go_quote(&self.codec_snake);
                    let detail = go_quote(text);
                    self.line(&format!(
                        "return {zero}, requireFailed({snake}, {cur}, {detail})"
                    ));
                } else {
                    let detail = go_quote(&format!("require failed: {text}"));
                    let fail = self.fail("ErrRequire", &detail);
                    self.line(&fail);
                }
                self.close();
                Ok(())
            }
            Stmt::Unwrap { dst, src, field } => {
                let uses = self.body.locals[dst.local].uses;
                let name = self.local_go[dst.local].clone();
                let source = self.expr(src)?;
                let detail = go_quote(&format!("missing {field}"));
                self.open(&format!("if {source} == nil"));
                let fail = self.fail("ErrMissingField", &detail);
                self.line(&fail);
                self.close();
                if uses > 0 {
                    let assign = self.store_op(dst.local);
                    self.line(&format!("{name} {assign} {}", deref(&source)));
                }
                Ok(())
            }
            Stmt::If { cond, then, els } => {
                let check = self.expr(cond)?;
                self.line(&format!("if {check} {{"));
                self.block(then)?;
                if !els.is_empty() {
                    self.line("} else {");
                    self.block(els)?;
                }
                self.line("}");
                Ok(())
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => {
                let subject = self.expr(scrutinee)?;
                self.line(&format!("switch {subject} {{"));
                for arm in arms {
                    let tags = arm
                        .tags
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.line(&format!("case {tags}:"));
                    self.block(&arm.body)?;
                }
                if let Some(other) = other {
                    self.line("default:");
                    self.block(other)?;
                } else {
                    self.flags.sprintf = true;
                    self.line("default:");
                    self.indent += 1;
                    let detail = format!(
                        "fmt.Sprintf(\"no match arm for tag %d in {}\", {subject})",
                        self.codec_snake
                    );
                    let fail = self.fail("ErrNoMatchArm", &detail);
                    self.line(&fail);
                    self.indent -= 1;
                }
                self.line("}");
                Ok(())
            }
            Stmt::Repeat {
                mode,
                list,
                item,
                body,
            } => {
                if !self.is_decode {
                    return Err(emit("emit go: Repeat in encode".to_string()));
                }
                let list_go = self.local_go[*list].clone();
                let item_go = self.local_go[*item].clone();
                let list_ty = self.body.locals[*list].ty.clone();
                let Ty::List(_) = &list_ty else {
                    return Err(emit("emit go: Repeat list is not a list".to_string()));
                };
                let list_go_ty = self.data.go_ty(&list_ty, false)?;
                let made = format!("make({list_go_ty}, 0)");
                let cur = self.cursor.clone();
                match mode {
                    RepeatMode::Count(n) => {
                        let (total, unsigned) = match self.plain_count(n)? {
                            Some(plain) => plain,
                            None => {
                                let count = self.as_i64(n, 1)?;
                                let total = self.pick("n");
                                self.line(&format!("{total} := {count}"));
                                (total, false)
                            }
                        };
                        if !unsigned {
                            self.open(&format!("if {total} < 0"));
                            let fail =
                                self.fail("ErrNegativeLength", &go_quote("negative repeat count"));
                            self.line(&fail);
                            self.close();
                        }
                        let list_assign = self.store_op(*list);
                        self.line(&format!(
                            "{list_go} {list_assign} make({list_go_ty}, 0, int({total}))"
                        ));
                        if self.ver.supports_range_int() {
                            self.open(&format!("for range {total}"));
                        } else {
                            // Ranging over an integer needs Go 1.22; older
                            // toolchains reject `for range total`.
                            let iv = self.pick("i");
                            self.open(&format!("for {iv} := 0; {iv} < int({total}); {iv}++"));
                        }
                    }
                    RepeatMode::Until(term) => {
                        // A byte literal compares directly; anything else is
                        // evaluated once, before the loop.
                        let byte = byte_literal(term, true);
                        let want = match &byte {
                            Some(byte) => byte.clone(),
                            None => {
                                let text = self.as_i64(term, 1)?;
                                let want = self.pick("want");
                                self.line(&format!("{want} := {text}"));
                                want
                            }
                        };
                        let list_assign = self.store_op(*list);
                        self.line(&format!("{list_go} {list_assign} {made}"));
                        self.open(&format!("for {cur}.Remaining() > 0"));
                        let next = self.pick("next");
                        let check = if byte.is_some() {
                            format!("{next} == {want}")
                        } else {
                            format!("int64({next}) == {want}")
                        };
                        self.open(&format!("if {next}, _ := {cur}.peekU8(); {check}"));
                        self.line(&format!("{cur}.skip(1)"));
                        self.line("break");
                        self.close();
                    }
                    RepeatMode::UntilEnd => {
                        let list_assign = self.store_op(*list);
                        self.line(&format!("{list_go} {list_assign} {made}"));
                        self.open(&format!("for {cur}.Remaining() > 0"));
                    }
                }
                self.stmts(body)?;
                self.line(&format!("{list_go} = append({list_go}, {item_go})"));
                self.close();
                Ok(())
            }
            Stmt::ForEach { list, item, body } => {
                if self.is_decode {
                    return Err(emit("emit go: ForEach in decode".to_string()));
                }
                let text = self.expr(list)?;
                let (_, optional) = self.data.prog.expr_ty(self.body, list);
                let range = if optional { deref(&text) } else { text };
                if self.body.locals[*item].uses == 0 {
                    self.open(&format!("for range {range}"));
                } else {
                    let item_go = self.local_go[*item].clone();
                    self.open(&format!("for _, {item_go} := range {range}"));
                    self.declared.insert(*item);
                }
                self.stmts(body)?;
                self.close();
                Ok(())
            }
            Stmt::ExpectVariant {
                dst,
                sum,
                ty,
                variant,
            } => {
                let prog = self.data.prog;
                let text = self.expr(sum)?;
                let (_, optional) = prog.expr_ty(self.body, sum);
                let subject = if optional { deref(&text) } else { text };
                let variant_decl = match &prog.types[*ty].kind {
                    crate::ir::typed::TypeKind::Sum { variants } => variants[*variant].clone(),
                    _ => {
                        return Err(emit("emit go: ExpectVariant of a non-sum".to_string()));
                    }
                };
                let variant_name = pascal(&variant_decl.name);
                let direct = self.data.direct.contains(&(*ty, *variant));
                let wrap = if direct {
                    match &variant_decl.payload {
                        Some(Ty::Named(target)) => self.data.type_go[*target].clone(),
                        _ => {
                            return Err(emit(
                                "emit go: direct variant without record payload".to_string(),
                            ));
                        }
                    }
                } else {
                    self.data.wrappers[&(*ty, *variant)].clone()
                };
                let detail = go_quote(&format!(
                    "wrong variant for {}: expected {}",
                    self.codec_snake, variant_name
                ));
                let ok = self.pick("ok");
                let bind = dst.filter(|b| self.body.locals[b.local].uses > 0);
                let Some(bind) = bind else {
                    self.open(&format!("if _, {ok} := {subject}.({wrap}); !{ok}"));
                    let fail = self.fail("ErrWrongVariant", &detail);
                    self.line(&fail);
                    self.close();
                    return Ok(());
                };
                let local = bind.local;
                let fresh = !self.declared.contains(&local) && !self.body.locals[local].optional;
                let holder = if fresh && direct {
                    // The asserted record is the payload: bind it by name.
                    self.declared.insert(local);
                    self.local_go[local].clone()
                } else {
                    self.pick("v")
                };
                self.line(&format!("{holder}, {ok} := {subject}.({wrap})"));
                self.open(&format!("if !{ok}"));
                let fail = self.fail("ErrWrongVariant", &detail);
                self.line(&fail);
                self.close();
                let value = if direct {
                    holder.clone()
                } else {
                    format!("{holder}.Value")
                };
                if fresh && !direct {
                    // Read the wrapped payload in place: `v.Value`.
                    self.local_go[local] = value;
                    self.declared.insert(local);
                } else if !(fresh && direct) {
                    let name = self.local_go[local].clone();
                    let assign = self.store_op(local);
                    if self.body.locals[local].optional {
                        self.line(&format!("{name} {assign} &{value}"));
                    } else {
                        self.line(&format!("{name} {assign} {value}"));
                    }
                }
                Ok(())
            }
            Stmt::Return(value) => {
                if !self.is_decode {
                    return Err(emit("emit go: Return in encode".to_string()));
                }
                // Long struct literals go multi-line; anything else stays put.
                if let Expr::Construct { ty, fields } = value {
                    let text = self.ret_expr(value)?;
                    if format!("return {text}, nil").len() <= 120 {
                        self.line(&format!("return {text}, nil"));
                        return Ok(());
                    }
                    let prog = self.data.prog;
                    let names = match &prog.types[*ty].kind {
                        crate::ir::typed::TypeKind::Record { fields } => {
                            fields.iter().map(|f| pascal(&f.name)).collect::<Vec<_>>()
                        }
                        _ => {
                            return Err(emit("emit go: Construct of a non-record".to_string()));
                        }
                    };
                    let mut pairs = Vec::with_capacity(names.len());
                    for (field_name, field_value) in names.iter().zip(fields.iter()) {
                        pairs.push((field_name.clone(), self.expr(field_value)?));
                    }
                    let type_go = self.data.type_go[*ty].clone();
                    self.line(&format!("return {type_go}{{"));
                    self.indent += 1;
                    for line in gofmt_key_values(&pairs) {
                        self.line(&line);
                    }
                    self.indent -= 1;
                    self.line("}, nil");
                    return Ok(());
                }
                let text = self.ret_expr(value)?;
                self.line(&format!("return {text}, nil"));
                Ok(())
            }
            Stmt::Fail(failure) => match failure {
                Failure::EncodeUntilEnd => {
                    let fail = self.fail(
                        "ErrUntilEnd",
                        &go_quote("encode not supported for repeat mode until_end"),
                    );
                    self.line(&fail);
                    Ok(())
                }
            },
        }
    }

    /// A value in return position: struct and sum literals print directly.
    fn ret_expr(&mut self, expr: &Expr) -> BmlResult<String> {
        match expr {
            Expr::Variant {
                ty,
                variant,
                payload,
            } => self.variant_value(*ty, *variant, payload.as_deref()),
            _ => self.expr(expr),
        }
    }

    fn variant_value(
        &mut self,
        ty: TypeIdx,
        variant: usize,
        payload: Option<&Expr>,
    ) -> BmlResult<String> {
        if self.data.direct.contains(&(ty, variant)) {
            match payload {
                Some(value) => self.expr(value),
                None => Err(emit("emit go: direct variant without payload".to_string())),
            }
        } else {
            let wrap = self.data.wrappers[&(ty, variant)].clone();
            match payload {
                None => Ok(format!("{wrap}{{}}")),
                Some(value) => {
                    let text = self.expr(value)?;
                    Ok(format!("{wrap}{{Value: {text}}}"))
                }
            }
        }
    }

    fn construct_go(&mut self, ty: TypeIdx, fields: &[Expr]) -> BmlResult<String> {
        let prog = self.data.prog;
        let names = match &prog.types[ty].kind {
            crate::ir::typed::TypeKind::Record { fields } => {
                fields.iter().map(|f| pascal(&f.name)).collect::<Vec<_>>()
            }
            _ => return Err(emit("emit go: Construct of a non-record".to_string())),
        };
        let type_go = self.data.type_go[ty].clone();
        let mut parts = Vec::with_capacity(fields.len());
        for (name, value) in names.iter().zip(fields.iter()) {
            parts.push(format!("{name}: {}", self.expr(value)?));
        }
        Ok(format!("{type_go}{{{}}}", parts.join(", ")))
    }

    /// `expr` coerced to an argument of type `want`.
    fn coerce_arg(
        &mut self,
        expr: &Expr,
        want: &Ty,
        want_opt: bool,
        depth: u8,
    ) -> BmlResult<String> {
        let text = self.expr_at(expr, depth)?;
        let (got_ty, got_opt) = self.data.prog.expr_ty(self.body, expr);
        if got_opt != want_opt {
            return Ok(text);
        }
        match (&got_ty, want) {
            (Ty::Int(_), Ty::Int(_)) => {
                let got = self.data.go_ty(&got_ty, false)?;
                let want_go = self.data.go_ty(want, false)?;
                if got == want_go {
                    Ok(text)
                } else {
                    Ok(format!("{want_go}({text})"))
                }
            }
            _ => Ok(text),
        }
    }

    /// `expr` coerced to a value of type `want`.
    fn coerce_to(
        &mut self,
        expr: &Expr,
        want: &Ty,
        want_opt: bool,
        depth: u8,
    ) -> BmlResult<String> {
        self.coerce_arg(expr, want, want_opt, depth)
    }

    fn expr(&mut self, expr: &Expr) -> BmlResult<String> {
        self.expr_at(expr, 1)
    }

    /// Print `expr` at gofmt nesting `depth` (statements start at 1; the
    /// right operand of a binary operator, a parenthesized operand, and
    /// the arguments of a multi-argument call go deeper). Spacing around
    /// `* / % << >> &` versus `+ - | ^` follows go/printer's cutoff rule,
    /// so the output is already gofmt-canonical.
    fn expr_at(&mut self, expr: &Expr, depth: u8) -> BmlResult<String> {
        match expr {
            Expr::Int { value, ty } => {
                int_go(*ty)?;
                Ok(value.to_string())
            }
            Expr::Bool(value) => Ok(value.to_string()),
            Expr::Str(value) => Ok(go_quote(value)),
            Expr::Local(id) => Ok(self.local_go[*id].clone()),
            Expr::Field { base, ty, field } => {
                let name = match &self.data.prog.types[*ty].kind {
                    crate::ir::typed::TypeKind::Record { fields } => pascal(&fields[*field].name),
                    _ => return Err(emit("emit go: Field of a non-record".to_string())),
                };
                match base.as_ref() {
                    Expr::Get(inner) => {
                        let text = self.expr(inner)?;
                        Ok(format!("(*{text}).{name}"))
                    }
                    _ => {
                        let text = self.expr(base)?;
                        Ok(format!("{text}.{name}"))
                    }
                }
            }
            Expr::Present(inner) => {
                let text = self.expr(inner)?;
                Ok(format!("{text} != nil"))
            }
            Expr::Get(inner) => {
                let text = self.expr(inner)?;
                if is_atomic(inner) {
                    Ok(format!("*{text}"))
                } else {
                    Ok(format!("*({text})"))
                }
            }
            Expr::Not(inner) => {
                if is_atomic(inner) {
                    let text = self.expr(inner)?;
                    Ok(format!("!{text}"))
                } else {
                    let text = self.expr_at(inner, reduce_depth(depth))?;
                    Ok(format!("!({text})"))
                }
            }
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } => self.binary(*op, lhs, rhs, ty, *wraps, depth),
            Expr::Cond {
                cond,
                then,
                els,
                ty,
            } => {
                // Go has no conditional expression: a temporary declared right
                // here and set by an `if` (no `else` when the other value is
                // the zero value the declaration already gives it).
                let tmp_ty = self.data.go_ty(ty, false)?;
                let check = self.expr(cond)?;
                let yes = self.expr(then)?;
                let no = self.expr(els)?;
                let zero = zero_value(self.data, ty)?;
                let tmp = self.temp();
                self.line(&format!("var {tmp} {tmp_ty}"));
                self.line(&format!("if {check} {{"));
                self.indent += 1;
                self.line(&format!("{tmp} = {yes}"));
                self.indent -= 1;
                if no != zero {
                    self.line("} else {");
                    self.indent += 1;
                    self.line(&format!("{tmp} = {no}"));
                    self.indent -= 1;
                }
                self.line("}");
                Ok(tmp)
            }
            Expr::Cast { expr, to } => {
                let want = self.data.go_ty(to, false)?;
                let (from, from_opt) = self.data.prog.expr_ty(self.body, expr);
                let text = self.expr_at(expr, depth)?;
                let base = if from_opt { deref(&text) } else { text };
                let got = self.data.go_ty(&from, false)?;
                if got == want {
                    return Ok(base);
                }
                match (&from, to) {
                    (Ty::Bool, Ty::Int(_)) => {
                        self.flags.b2i = true;
                        let call = format!("b2i({base})");
                        if want == "int64" {
                            Ok(call)
                        } else {
                            Ok(format!("{want}({call})"))
                        }
                    }
                    (Ty::Int(_), Ty::Int(_))
                    | (Ty::F32, Ty::F64)
                    | (Ty::F64, Ty::F32)
                    | (Ty::Named(_), Ty::Int(_))
                    | (Ty::Int(_), Ty::Named(_))
                    | (Ty::Named(_), Ty::Named(_)) => Ok(format!("{want}({base})")),
                    _ => Err(emit(format!(
                        "emit go: unsupported cast {from:?} to {to:?}"
                    ))),
                }
            }
            Expr::Call { func, args } => match func {
                Func::SignExtend => {
                    self.flags.sign_extend = true;
                    let value = self.expr_at(&args[0], depth + 1)?;
                    // A literal width converts implicitly; anything else is
                    // an `int64` expression that needs `int(...)`.
                    let bits = match &args[1] {
                        Expr::Int { value, .. } => value.to_string(),
                        other => format!("int({})", self.expr_at(other, depth + 1)?),
                    };
                    Ok(format!("signExtend({value}, {bits})"))
                }
                Func::Utf16Units => {
                    self.flags.utf16_units = true;
                    let text = self.expr_at(&args[0], depth)?;
                    Ok(format!("utf16Units({text})"))
                }
                Func::Utf8Len => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("int64(len({text}))"))
                }
                Func::StrLen => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("int64(len([]rune({text})))"))
                }
                Func::BytesLen => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("int64(len({text}))"))
                }
                Func::ListLen => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("int64(len({text}))"))
                }
                Func::Min => {
                    let low = self.expr_at(&args[0], depth + 1)?;
                    let high = self.expr_at(&args[1], depth + 1)?;
                    Ok(format!("min({low}, {high})"))
                }
                Func::Max => {
                    let low = self.expr_at(&args[0], depth + 1)?;
                    let high = self.expr_at(&args[1], depth + 1)?;
                    Ok(format!("max({low}, {high})"))
                }
            },
            Expr::StrBytes(inner) => {
                let text = self.expr(inner)?;
                Ok(format!("[]byte({text})"))
            }
            Expr::Remaining => {
                if !self.is_decode {
                    return Err(emit("emit go: Remaining in encode".to_string()));
                }
                Ok(format!("{}.Remaining()", self.cursor.clone()))
            }
            Expr::Construct { ty, fields } => self.construct_go(*ty, fields),
            Expr::Variant {
                ty,
                variant,
                payload,
            } => self.variant_value(*ty, *variant, payload.as_deref()),
        }
    }

    fn binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ty: &Ty,
        _wraps: bool,
        depth: u8,
    ) -> BmlResult<String> {
        // Signed division and modulo floor; Go truncates. Use a helper when
        // the dividend may be negative.
        let negative = match ty {
            Ty::Int(t) if t.signed() => match self.data.prog.int_range(self.body, lhs) {
                Some((lo, _)) => lo < 0,
                None => true,
            },
            _ => false,
        };
        if negative && matches!(op, BinOp::Div | BinOp::Mod) {
            if op == BinOp::Div {
                self.flags.floor_div = true;
            } else {
                self.flags.floor_mod = true;
            }
            let helper = if op == BinOp::Div {
                "floorDiv"
            } else {
                "floorMod"
            };
            // The helper call takes two arguments, so its operands print one
            // level deeper; the `int64` conversions keep that depth.
            let left = self.expr_at(lhs, depth + 1)?;
            let right = self.expr_at(rhs, depth + 1)?;
            let narrow = self.data.go_ty(ty, false)?;
            let left_wide = self.widen(&left, lhs)?;
            let right_wide = self.widen(&right, rhs)?;
            let call = format!("{helper}({left_wide}, {right_wide})");
            if narrow == "int64" {
                return Ok(call);
            }
            return Ok(format!("{narrow}({call})"));
        }
        let level = prec(op);
        let (has4, has5, problem) = walk_binary(op, lhs, rhs);
        let gap = if level < cutoff(has4, has5, problem, depth) {
            " "
        } else {
            ""
        };
        let symbol = op.symbol();
        let left = match lhs {
            Expr::Int { value, ty } if hex_literal(op, *value) => {
                int_go(*ty)?;
                format!("0x{value:X}")
            }
            _ => self.operand_at(lhs, level, false, depth + diff_prec(lhs, level))?,
        };
        let right = match rhs {
            Expr::Int { value, ty } if hex_literal(op, *value) => {
                int_go(*ty)?;
                format!("0x{value:X}")
            }
            _ => self.operand_at(rhs, level, true, depth + 1)?,
        };
        Ok(format!("{left}{gap}{symbol}{gap}{right}"))
    }

    /// `printed` widened to `int64` for the floor helpers.
    fn widen(&mut self, printed: &str, side: &Expr) -> BmlResult<String> {
        let (side_ty, side_opt) = self.data.prog.expr_ty(self.body, side);
        if !side_opt && matches!(side_ty, Ty::Int(IntTy::I64)) {
            Ok(printed.to_string())
        } else if side_opt {
            Ok(format!("int64(*({printed}))"))
        } else {
            Ok(format!("int64({printed})"))
        }
    }

    /// Print a binary operand, parenthesizing when Go would parse it
    /// differently. Parentheses undo one level of gofmt nesting depth.
    fn operand_at(
        &mut self,
        child: &Expr,
        level: u8,
        is_right: bool,
        pos_depth: u8,
    ) -> BmlResult<String> {
        let need = match child {
            Expr::Binary { op: child_op, .. } => {
                let child_level = prec(*child_op);
                // The right side is parenthesized at equal precedence: `a - (b - c)`
                // must not collapse to `a - b - c`.
                child_level < level || (is_right && child_level == level)
            }
            // A conditional prints as the name of its temporary.
            Expr::Cond { .. } => false,
            _ => false,
        };
        if need {
            Ok(format!(
                "({})",
                self.expr_at(child, reduce_depth(pos_depth))?
            ))
        } else {
            self.expr_at(child, pos_depth)
        }
    }
}

fn zero_value(data: &Data<'_>, ty: &Ty) -> BmlResult<String> {
    match ty {
        Ty::Int(_) | Ty::F32 | Ty::F64 => Ok("0".into()),
        Ty::Bool => Ok("false".into()),
        Ty::Str => Ok("\"\"".into()),
        Ty::Bytes | Ty::List(_) => Ok("nil".into()),
        Ty::Named(t) => match &data.prog.types[*t].kind {
            crate::ir::typed::TypeKind::Record { .. } => Ok(format!("{}{{}}", data.type_go[*t])),
            crate::ir::typed::TypeKind::Sum { .. } => Ok("nil".into()),
            crate::ir::typed::TypeKind::Enum { .. } => Ok(format!("{}(0)", data.type_go[*t])),
            crate::ir::typed::TypeKind::Alias { target } => zero_value(data, &Ty::Named(*target)),
        },
    }
}

/// `doc_value` without its leading article.
fn strip_article(text: &str) -> &str {
    text.strip_prefix("an ")
        .or_else(|| text.strip_prefix("a "))
        .unwrap_or(text)
}

fn doc_value(data: &Data<'_>, ty: &Ty) -> BmlResult<String> {
    match ty {
        Ty::Int(t) => Ok(with_article(int_go(*t)?)),
        Ty::Bool => Ok("a bool".into()),
        Ty::F32 => Ok("a float32".into()),
        Ty::F64 => Ok("a float64".into()),
        Ty::Str => Ok("a string".into()),
        Ty::Bytes => Ok("a byte slice".into()),
        Ty::List(inner) => Ok(format!(
            "a slice of {}",
            strip_article(&doc_value(data, inner)?)
        )),
        Ty::Named(t) => Ok(with_article(&data.type_go[*t])),
    }
}

/// `word` after "a" or "an", by how it sounds: "an int32", "an Intent",
/// "a uint8", "a UUID", "a Handshake".
fn with_article(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let you = ["uint", "uu", "uni", "use", "usu", "uti"]
        .iter()
        .any(|p| lower.starts_with(p));
    let vowel = lower.starts_with(['a', 'e', 'i', 'o', 'u']) && !you;
    format!("{} {word}", if vowel { "an" } else { "a" })
}
