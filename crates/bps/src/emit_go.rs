//! `emit go`: an idiomatic Go package over the typed program.
//!
//! The printer reads [`Program`](crate::ir::typed) only: every name is already
//! resolved, every value typed, every nested record or sum lifted into its own
//! codec. It chooses Go syntax and nothing else.
//!
//! # Files
//!
//! `go.mod` (`module bpsgen`, `go 1.23`), `runtime.go` (reader, writer, errors,
//! helpers), `packets.go` (types and codecs, in program order). Package
//! `bpsgen`.
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
//! dividend may be negative; a `Cond` becomes a hoisted `if`/`else` over a
//! function-scoped temporary.
//!
//! Output is deterministic and gofmt-canonical (`gofmt -l` clean, `go vet`
//! clean).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::ResolvedSchema;
use crate::errors::{BpsResult, emit};
use crate::ir::typed::{
    BinOp, Body, CodecIdx, Expr, Failure, Func, IntTy, Name, Origin, Program, ReadOp, RepeatMode,
    Stmt, Ty, TypeIdx, WriteOp, escape,
};

/// `emit go`: write `go.mod`, `runtime.go`, `packets.go` for `resolved`.
pub fn emit_go(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    let program = crate::ir::typed(resolved)?;
    let mut printer = Printer::new(&program)?;
    let packets = printer.packets_go()?;
    let runtime = printer.runtime_go();
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("go.mod"), "module bpsgen\n\ngo 1.23\n")
        .map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("runtime.go"), runtime).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("packets.go"), packets).map_err(|e| emit(e.to_string()))?;
    Ok(())
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
    fn new(prog: &'a Program) -> BpsResult<Data<'a>> {
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
}

impl<'a> Printer<'a> {
    fn new(prog: &'a Program) -> BpsResult<Printer<'a>> {
        Ok(Printer {
            data: Data::new(prog)?,
            flags: Flags::default(),
        })
    }
}

// ----------------------------------------------------------------------
// Go types
// ----------------------------------------------------------------------

/// Go type for an integer class; `I128` never appears in shipped schemas.
fn int_go(ty: IntTy) -> BpsResult<&'static str> {
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
    fn go_ty(&self, ty: &Ty, optional: bool) -> BpsResult<String> {
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
    fn expr_go_ty(&self, body: &Body, expr: &Expr) -> BpsResult<(String, bool)> {
        let (ty, optional) = self.prog.expr_ty(body, expr);
        Ok((self.go_ty(&ty, optional)?, optional))
    }
}

// ----------------------------------------------------------------------
// runtime.go
// ----------------------------------------------------------------------

const RUNTIME_BASE: &str = r#"package bpsgen

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

func (k ErrorKind) Error() string {
	return "bpsgen: " + string(k)
}

// Error is a decode or encode failure. Decode errors carry the reader offset;
// encode errors use Offset -1.
type Error struct {
	Codec  string
	Offset int
	Kind   ErrorKind
	Detail string
}

func (e *Error) Error() string {
	if e.Offset >= 0 {
		return "bpsgen: decode " + e.Codec + " at offset " + strconv.Itoa(e.Offset) + ": " + e.Detail
	}
	return "bpsgen: encode " + e.Codec + ": " + e.Detail
}

func (e *Error) Is(target error) bool {
	k, ok := target.(ErrorKind)
	return ok && e.Kind == k
}

func (e *Error) Unwrap() error {
	return e.Kind
}

// failDecode attaches the calling codec to a reader or sub-codec error.
func failDecode(codec string, r *Reader, err error) *Error {
	var bpsErr *Error
	if errors.As(err, &bpsErr) {
		if bpsErr.Codec != "" {
			return bpsErr
		}
		return &Error{Codec: codec, Offset: r.Offset(), Kind: bpsErr.Kind, Detail: bpsErr.Detail}
	}
	return &Error{Codec: codec, Offset: r.Offset(), Kind: ErrShortBuffer, Detail: err.Error()}
}

// failEncode attaches the calling codec to a writer or sub-codec error.
func failEncode(codec string, err error) *Error {
	var bpsErr *Error
	if errors.As(err, &bpsErr) {
		if bpsErr.Codec != "" {
			return bpsErr
		}
		return &Error{Codec: codec, Offset: -1, Kind: bpsErr.Kind, Detail: bpsErr.Detail}
	}
	return &Error{Codec: codec, Offset: -1, Kind: ErrBytesTooLong, Detail: err.Error()}
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

impl Printer<'_> {
    fn packets_go(&mut self) -> BpsResult<String> {
        let mut out = String::from("package bpsgen\n");
        // Types first, in program order.
        let mut types_out = String::new();
        for t in 0..self.data.prog.types.len() {
            types_out.push_str(&self.type_go(t)?);
        }
        // Codecs next, in program order (records helper usage).
        let mut codecs_out = String::new();
        for c in 0..self.data.prog.codecs.len() {
            codecs_out.push_str(&self.codec_go(c)?);
        }
        let mut imports = Vec::new();
        if self.flags.sprintf {
            imports.push("\t\"fmt\"\n");
        }
        if self.flags.utf8_valid {
            imports.push("\t\"unicode/utf8\"\n");
        }
        if !imports.is_empty() {
            out.push_str("\nimport (\n");
            for import in imports {
                out.push_str(import);
            }
            out.push_str(")\n");
        }
        if !types_out.is_empty() {
            out.push('\n');
            out.push_str(&types_out);
        }
        out.push_str(&codecs_out);
        while out.ends_with("\n\n") {
            out.pop();
        }
        Ok(out)
    }

    fn type_go(&mut self, t: TypeIdx) -> BpsResult<String> {
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
                let mut out = format!("{doc}\ntype {name} {base_go}\n\nconst (\n");
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
    hoisted: Vec<(String, String)>,
    flags: Flags,
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
            hoisted: Vec::new(),
            flags: Flags::default(),
            out: String::new(),
            indent: 1,
        };
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
    }

    fn close(&mut self) {
        self.indent -= 1;
        self.line("}");
    }

    fn fail(&mut self, kind: &str, detail: &str) -> String {
        if self.is_decode {
            format!(
                "return {}, &Error{{Codec: {}, Offset: {}.Offset(), Kind: {kind}, Detail: {detail}}}",
                self.ret_zero,
                go_quote(&self.codec_snake),
                self.cursor,
            )
        } else {
            format!(
                "return &Error{{Codec: {}, Offset: -1, Kind: {kind}, Detail: {detail}}}",
                go_quote(&self.codec_snake),
            )
        }
    }

    /// `expr` converted to `int64`.
    fn as_i64(&mut self, expr: &Expr, depth: u8) -> BpsResult<String> {
        let text = self.expr_at(expr, depth)?;
        let (ty, optional) = self.data.prog.expr_ty(self.body, expr);
        if !optional && matches!(ty, Ty::Int(IntTy::I64)) {
            Ok(text)
        } else if optional {
            Ok(format!("int64(*({text}))"))
        } else {
            Ok(format!("int64({text})"))
        }
    }

    /// `expr` converted to an exact Go type.
    fn as_ty(&mut self, expr: &Expr, want: &str, depth: u8) -> BpsResult<String> {
        let text = self.expr_at(expr, depth)?;
        let (got, _) = self.data.expr_go_ty(self.body, expr)?;
        if got == want {
            Ok(text)
        } else {
            Ok(format!("{want}({text})"))
        }
    }
}

impl Printer<'_> {
    fn codec_go(&mut self, c: CodecIdx) -> BpsResult<String> {
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
            let mut bg = BodyGen::new(&self.data, &decode_body, &snake, true, &ret_zero);
            let mut sig = format!("// Decode{go_name} decodes {value_doc}.");
            sig.push_str(&format!("\nfunc {decode_name}({} *Reader", bg.cursor));
            for (param, local) in params.iter().zip(decode_body.params.iter()) {
                let ty = self.data.go_ty(&param.ty, param.optional)?;
                sig.push_str(&format!(", {} {ty}", bg.local_go[*local]));
            }
            sig.push_str(&format!(") ({ret_go}, error) {{\n"));
            let forced = forced_locals(&decode_body.stmts);
            let mut vars = String::new();
            for (i, local) in decode_body.locals.iter().enumerate() {
                if decode_body.params.contains(&i) {
                    continue;
                }
                if local.uses == 0 && !forced.contains(&i) {
                    continue;
                }
                vars.push_str(&format!(
                    "\tvar {} {}\n",
                    bg.local_go[i],
                    self.data.go_ty(&local.ty, local.optional)?
                ));
            }
            bg.stmts(&decode_body.stmts)?;
            if !diverges(&decode_body.stmts) {
                let fail = bg.fail("ErrNoMatchArm", &go_quote("unreachable"));
                bg.line(&fail);
            }
            let mut hoisted = String::new();
            for (name, ty) in &bg.hoisted {
                hoisted.push_str(&format!("\tvar {name} {ty}\n"));
            }
            let flags = std::mem::take(&mut bg.flags);
            let out = std::mem::take(&mut bg.out);
            self.flags.absorb(&flags);
            (format!("{sig}{hoisted}{vars}"), out)
        };
        // Encode.
        let (esig, eout) = {
            let mut bg = BodyGen::new(&self.data, &encode_body, &snake, false, "");
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
            let forced = forced_locals(&encode_body.stmts);
            let mut vars = String::new();
            for (i, local) in encode_body.locals.iter().enumerate() {
                if i == value_id || encode_body.params.contains(&i) {
                    continue;
                }
                if local.uses == 0 && !forced.contains(&i) {
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
            let mut hoisted = String::new();
            for (name, ty) in &bg.hoisted {
                hoisted.push_str(&format!("\tvar {name} {ty}\n"));
            }
            let flags = std::mem::take(&mut bg.flags);
            let out = std::mem::take(&mut bg.out);
            self.flags.absorb(&flags);
            (format!("{sig}{hoisted}{vars}"), out)
        };
        Ok(format!("{dsig}{dout}}}\n\n{esig}{eout}}}\n\n"))
    }
}

/// Locals the generated code references beyond the program's reads: repeat
/// lists and items are assigned and appended by the loop scaffolding, and
/// `for` items are assigned from the range variable, so they need a
/// declaration even when nothing reads them.
fn forced_locals(stmts: &[Stmt]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    forced_into(stmts, &mut out);
    out
}

fn forced_into(stmts: &[Stmt], out: &mut BTreeSet<usize>) {
    for stmt in stmts {
        match stmt {
            Stmt::Repeat {
                list, item, body, ..
            } => {
                out.insert(*list);
                out.insert(*item);
                forced_into(body, out);
            }
            Stmt::ForEach { item, body, .. } => {
                out.insert(*item);
                forced_into(body, out);
            }
            Stmt::If { then, els, .. } => {
                forced_into(then, out);
                forced_into(els, out);
            }
            Stmt::Match { arms, other, .. } => {
                for arm in arms {
                    forced_into(&arm.body, out);
                }
                if let Some(other) = other {
                    forced_into(other, out);
                }
            }
            _ => {}
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
    fn stmts(&mut self, stmts: &[Stmt]) -> BpsResult<()> {
        for stmt in stmts {
            self.stmt(stmt)?;
        }
        Ok(())
    }

    /// Wrap a fallible read in its error check, preserving the inner codec.
    fn decode_check(&mut self, err: &str) -> BpsResult<()> {
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

    fn stmt(&mut self, stmt: &Stmt) -> BpsResult<()> {
        match stmt {
            Stmt::Declare { .. } => Ok(()),
            Stmt::Let { dst, value } => {
                let uses = self.body.locals[dst.local].uses;
                if uses == 0 {
                    return Ok(());
                }
                let text = self.expr(value)?;
                let name = self.local_go[dst.local].clone();
                if dst.some {
                    let (ty, _) = self.data.prog.expr_ty(self.body, value);
                    let goty = self.data.go_ty(&ty, false)?;
                    let tmp = self.temp();
                    self.line(&format!("var {tmp} {goty} = {text}"));
                    self.line(&format!("{name} = &{tmp}"));
                } else {
                    self.line(&format!("{name} = {text}"));
                }
                Ok(())
            }
            Stmt::Read { dst, op } => {
                let uses = self.body.locals[dst.local].uses;
                let name = self.local_go[dst.local].clone();
                let cur = self.cursor.clone();
                if matches!(op, ReadOp::Rest) {
                    if uses == 0 {
                        self.line(&format!("{cur}.Rest()"));
                    } else if self.body.locals[dst.local].optional {
                        let tmp = self.temp();
                        self.line(&format!("{tmp} := {cur}.Rest()"));
                        self.line(&format!("{name} = &{tmp}"));
                    } else {
                        self.line(&format!("{name} = {cur}.Rest()"));
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
                let tmp = self.temp();
                let err = self.err_name.clone();
                self.line(&format!("{tmp}, {err} := {call}"));
                self.decode_check(&err)?;
                if uses == 0 {
                    self.line(&format!("_ = {tmp}"));
                } else if self.body.locals[dst.local].optional {
                    self.line(&format!("{name} = &{tmp}"));
                } else {
                    self.line(&format!("{name} = {tmp}"));
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
                let tmp = self.temp();
                let err = self.err_name.clone();
                self.line(&format!("{tmp}, {err} := {call}"));
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
                if uses == 0 {
                    self.line(&format!("_ = {tmp}"));
                } else if optional {
                    self.line(&format!("{name} = &{tmp}"));
                } else {
                    self.line(&format!("{name} = {tmp}"));
                }
                Ok(())
            }
            Stmt::Utf8 { dst, src } => {
                let uses = self.body.locals[dst.local].uses;
                let optional = self.body.locals[dst.local].optional;
                let name = self.local_go[dst.local].clone();
                let text = self.expr(src)?;
                let tmp = self.temp();
                self.line(&format!("{tmp} := string({text})"));
                self.flags.utf8_valid = true;
                let detail = go_quote(&format!("invalid utf-8 in {}", self.codec_snake));
                self.open(&format!("if !utf8.ValidString({tmp})"));
                let fail = self.fail("ErrInvalidUTF8", &detail);
                self.line(&fail);
                self.close();
                if uses == 0 {
                    self.line(&format!("_ = {tmp}"));
                } else if optional {
                    self.line(&format!("{name} = &{tmp}"));
                } else {
                    self.line(&format!("{name} = {tmp}"));
                }
                Ok(())
            }
            Stmt::Write { op, value } => {
                let cur = self.cursor.clone();
                match op {
                    WriteOp::U8 => {
                        let text = self.as_ty(value, "uint8", 1)?;
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
                let check = self.expr(cond)?;
                let detail = go_quote(&format!("require failed: {text}"));
                self.open(&format!("if !({check})"));
                let fail = self.fail("ErrRequire", &detail);
                self.line(&fail);
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
                    self.line(&format!("{name} = *({source})"));
                }
                Ok(())
            }
            Stmt::If { cond, then, els } => {
                let check = self.expr(cond)?;
                self.line(&format!("if {check} {{"));
                self.indent += 1;
                self.stmts(then)?;
                self.indent -= 1;
                if !els.is_empty() {
                    self.line("} else {");
                    self.indent += 1;
                    self.stmts(els)?;
                    self.indent -= 1;
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
                    self.indent += 1;
                    self.stmts(&arm.body)?;
                    self.indent -= 1;
                }
                if let Some(other) = other {
                    self.line("default:");
                    self.indent += 1;
                    self.stmts(other)?;
                    self.indent -= 1;
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
                let made = format!("make({}, 0)", self.data.go_ty(&list_ty, false)?);
                match mode {
                    RepeatMode::Count(n) => {
                        let count = self.as_i64(n, 1)?;
                        let total = self.temp();
                        let index = self.temp();
                        self.line(&format!("var {total} int64 = {count}"));
                        self.open(&format!("if {total} < 0"));
                        let fail =
                            self.fail("ErrNegativeLength", &go_quote("negative repeat count"));
                        self.line(&fail);
                        self.close();
                        self.line(&format!("{list_go} = {made}"));
                        self.open(&format!(
                            "for {index} := int64(0); {index} < {total}; {index}++"
                        ));
                        self.stmts(body)?;
                        self.line(&format!("{list_go} = append({list_go}, {item_go})"));
                        self.close();
                    }
                    RepeatMode::Until(term) => {
                        let (term_ty, _) = self.data.prog.expr_ty(self.body, term);
                        let term_go = self.data.go_ty(&term_ty, false)?;
                        let text = self.expr(term)?;
                        let want = self.temp();
                        let peek = self.temp();
                        let cur = self.cursor.clone();
                        self.line(&format!("var {want} {term_go} = {text}"));
                        self.open(&format!("for {cur}.Remaining() > 0"));
                        self.line(&format!("{peek}, _ := {cur}.peekU8()"));
                        self.open(&format!("if {term_go}({peek}) == {want}"));
                        self.line(&format!("{cur}.skip(1)"));
                        self.line("break");
                        self.close();
                        self.stmts(body)?;
                        self.line(&format!("{list_go} = append({list_go}, {item_go})"));
                        self.close();
                    }
                    RepeatMode::UntilEnd => {
                        let cur = self.cursor.clone();
                        self.line(&format!("{list_go} = {made}"));
                        self.open(&format!("for {cur}.Remaining() > 0"));
                        self.stmts(body)?;
                        self.line(&format!("{list_go} = append({list_go}, {item_go})"));
                        self.close();
                    }
                }
                Ok(())
            }
            Stmt::ForEach { list, item, body } => {
                if self.is_decode {
                    return Err(emit("emit go: ForEach in decode".to_string()));
                }
                let text = self.expr(list)?;
                let (_, optional) = self.data.prog.expr_ty(self.body, list);
                let range = if optional { format!("*({text})") } else { text };
                let item_go = self.local_go[*item].clone();
                let elem = self.temp();
                self.open(&format!("for _, {elem} := range {range}"));
                self.line(&format!("{item_go} = {elem}"));
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
                let subject = if optional { format!("*({text})") } else { text };
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
                let bind_needed = match dst {
                    Some(bind) => self.body.locals[bind.local].uses > 0,
                    None => false,
                };
                let payload = self.temp();
                let ok = self.temp();
                let target = if bind_needed {
                    payload.clone()
                } else {
                    "_".to_string()
                };
                self.line(&format!("{target}, {ok} := {subject}.({wrap})"));
                self.open(&format!("if !{ok}"));
                let fail = self.fail("ErrWrongVariant", &detail);
                self.line(&fail);
                self.close();
                if let Some(bind) = dst
                    && bind_needed
                {
                    let name = self.local_go[bind.local].clone();
                    let value = if direct {
                        payload.clone()
                    } else {
                        format!("{payload}.Value")
                    };
                    if self.body.locals[bind.local].optional {
                        self.line(&format!("{name} = &({value})"));
                    } else {
                        self.line(&format!("{name} = {value}"));
                    }
                }
                Ok(())
            }
            Stmt::Return(value) => {
                if !self.is_decode {
                    return Err(emit("emit go: Return in encode".to_string()));
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
    fn ret_expr(&mut self, expr: &Expr) -> BpsResult<String> {
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
    ) -> BpsResult<String> {
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

    fn construct_go(&mut self, ty: TypeIdx, fields: &[Expr]) -> BpsResult<String> {
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
    ) -> BpsResult<String> {
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
    ) -> BpsResult<String> {
        self.coerce_arg(expr, want, want_opt, depth)
    }

    fn expr(&mut self, expr: &Expr) -> BpsResult<String> {
        self.expr_at(expr, 1)
    }

    /// Print `expr` at gofmt nesting `depth` (statements start at 1; the
    /// right operand of a binary operator, a parenthesized operand, and
    /// the arguments of a multi-argument call go deeper). Spacing around
    /// `* / % << >> &` versus `+ - | ^` follows go/printer's cutoff rule,
    /// so the output is already gofmt-canonical.
    fn expr_at(&mut self, expr: &Expr, depth: u8) -> BpsResult<String> {
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
                let tmp_ty = self.data.go_ty(ty, false)?;
                let check = self.expr(cond)?;
                let tmp = self.temp();
                self.hoisted.push((tmp.clone(), tmp_ty));
                let yes = self.expr(then)?;
                let no = self.expr(els)?;
                self.line(&format!("if {check} {{"));
                self.indent += 1;
                self.line(&format!("{tmp} = {yes}"));
                self.indent -= 1;
                self.line("} else {");
                self.indent += 1;
                self.line(&format!("{tmp} = {no}"));
                self.indent -= 1;
                self.line("}");
                Ok(tmp)
            }
            Expr::Cast { expr, to } => {
                let want = self.data.go_ty(to, false)?;
                let (from, from_opt) = self.data.prog.expr_ty(self.body, expr);
                let text = self.expr_at(expr, depth)?;
                let base = if from_opt { format!("*({text})") } else { text };
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
                    let bits = self.expr_at(&args[1], depth + 1)?;
                    Ok(format!("signExtend({value}, int({bits}))"))
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
    ) -> BpsResult<String> {
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
        let left = self.operand_at(lhs, level, false, depth + diff_prec(lhs, level))?;
        let right = self.operand_at(rhs, level, true, depth + 1)?;
        Ok(format!("{left}{gap}{symbol}{gap}{right}"))
    }

    /// `printed` widened to `int64` for the floor helpers.
    fn widen(&mut self, printed: &str, side: &Expr) -> BpsResult<String> {
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
    ) -> BpsResult<String> {
        let need = match child {
            Expr::Binary { op: child_op, .. } => {
                let child_level = prec(*child_op);
                // The right side is parenthesized at equal precedence: `a - (b - c)`
                // must not collapse to `a - b - c`.
                child_level < level || (is_right && child_level == level)
            }
            Expr::Cond { .. } => true,
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

/// Whether control never falls through `stmts`: the last statement returns
/// or fails on every path, so no trailing return is needed (and `go vet`
/// forbids the unreachable one).
fn diverges(stmts: &[Stmt]) -> bool {
    match stmts.last() {
        Some(Stmt::Return(_) | Stmt::Fail(_)) => true,
        Some(Stmt::If { then, els, .. }) => !els.is_empty() && diverges(then) && diverges(els),
        Some(Stmt::Match { arms, other, .. }) => {
            // Without `other` the printer generates a default that returns.
            let other_diverges = match other {
                Some(body) => diverges(body),
                None => true,
            };
            other_diverges && arms.iter().all(|arm| diverges(&arm.body))
        }
        _ => false,
    }
}

fn zero_value(data: &Data<'_>, ty: &Ty) -> BpsResult<String> {
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

fn doc_value(data: &Data<'_>, ty: &Ty) -> BpsResult<String> {
    match ty {
        Ty::Int(t) => Ok(format!("an {}", int_go(*t)?)),
        Ty::Bool => Ok("a bool".into()),
        Ty::F32 => Ok("a float32".into()),
        Ty::F64 => Ok("a float64".into()),
        Ty::Str => Ok("a string".into()),
        Ty::Bytes => Ok("a byte slice".into()),
        Ty::List(inner) => Ok(format!(
            "a slice of {}",
            strip_article(&doc_value(data, inner)?)
        )),
        Ty::Named(t) => Ok(format!("a {}", data.type_go[*t])),
    }
}
