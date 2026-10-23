//! `emit typescript`: a `bmlgen.ts` index over one ES module per route
//! category (`runtime.ts`, `encoding.ts`, `play_clientbound.ts`, `shared.ts`,
//! ...), the same split as the other printers.
//!
//! Modules run under node 22 `--experimental-strip-types` and pass
//! `tsc --strict --noEmit` (plus `--erasableSyntaxOnly`) and `prettier --check`.
//! Every module exports its declarations; `bmlgen.ts` re-exports them all, so
//! the public surface is unchanged. Each module imports only the runtime
//! helpers and other modules' symbols its text names.
//!
//! Types: a record is an `interface` (fields camelCase in schema order,
//! all `readonly` — the codec only ever constructs decoded values, it never
//! writes a field after construction; optional fields `name?: T`, never
//! `name: T | undefined`, so the interface reads the same whether or not
//! `exactOptionalPropertyTypes` is on), a sum is a discriminated union over
//! `kind: "tag0" | ...` (scalar and named-record payloads ride in
//! `value: T`; inline-block arms spread flat and carry the discriminant in
//! their own interface), an open enum is `type X = number` plus
//! `const X = {...} as const`. Integers of 32 bits or fewer are `number`,
//! 64-bit classes and `i128` are `bigint`.
//!
//! Integers follow the typed program exactly: every [`Binary`](crate::ir::typed::Expr::Binary)
//! names its result type, so narrow operations print as plain operators and
//! only `wraps` operations mask back to 64 bits. Plain `& | ^ <<` on a
//! `u32`-range `number` wrap to int32 in JS, so every truncating store prints
//! its `Cast` helper (`toU8` ... `toU32`, `>>> 0` for the latter) and `>>`
//! prints as `>>>` for unsigned or provably non-negative operands
//! ([`Program::int_range`](crate::ir::typed::Program::int_range) decides).
//! `/` floors and `%` takes the divisor's sign (`floorDiv`/`intMod`,
//! `0` for a zero divisor), matching the interpreter.
//!
//! Errors are `BmlError` (codec, offset, kind, detail); every failure the
//! typed program documents maps to a `never`-returning builder so narrowing
//! and definite assignment hold past them.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use crate::ResolvedSchema;
use crate::errors::{BmlResult, emit};
use crate::ir::typed::{
    BinOp, Body, CodecIdx, Expr, Func, IntTy, Origin, ParamRole, Program, ReadOp, RepeatMode, Stmt,
    Ty, TypeIdx, WriteOp, byte_literal, escape, hex_literal,
};
use crate::vectors::{TestRun, Vector};
const RUNTIME: &str = r#"export type BmlErrorKind =
  | "shortBuffer"
  | "requireFailed"
  | "invalidUtf8"
  | "noMatchArm"
  | "missingField"
  | "wrongVariant"
  | "negativeLength"
  | "bytesTooLong"
  | "untilEndEncode";
export class BmlError extends Error {
  kind: BmlErrorKind;
  codec: string;
  offset: number;
  constructor(
    kind: BmlErrorKind,
    codec: string,
    offset: number,
    detail: string,
  ) {
    super(`${codec} @${offset}: ${kind}: ${detail}`);
    this.name = "BmlError";
    this.kind = kind;
    this.codec = codec;
    this.offset = offset;
  }
}
export function shortBuffer(
  codec: string,
  offset: number,
  detail: string,
): never {
  throw new BmlError("shortBuffer", codec, offset, detail);
}
export function requireFailed(
  codec: string,
  offset: number,
  text: string,
): never {
  throw new BmlError("requireFailed", codec, offset, text);
}
export function invalidUtf8(codec: string, offset: number): never {
  throw new BmlError("invalidUtf8", codec, offset, "invalid UTF-8");
}
export function noMatchArm(codec: string, offset: number, tag: number): never {
  throw new BmlError("noMatchArm", codec, offset, `no arm for tag ${tag}`);
}
export function missingField(
  codec: string,
  offset: number,
  field: string,
): never {
  throw new BmlError("missingField", codec, offset, field);
}
export function wrongVariant(
  codec: string,
  offset: number,
  want: string,
  have: string,
): never {
  throw new BmlError(
    "wrongVariant",
    codec,
    offset,
    `want ${want}, have ${have}`,
  );
}
export function negativeLength(
  codec: string,
  offset: number,
  count: number,
): never {
  throw new BmlError("negativeLength", codec, offset, `count ${count}`);
}
export function bytesTooLong(
  codec: string,
  offset: number,
  detail: string,
): never {
  throw new BmlError("bytesTooLong", codec, offset, detail);
}
export function untilEndEncode(codec: string, offset: number): never {
  throw new BmlError("untilEndEncode", codec, offset, "encode not supported");
}
export const utf8Encoder = new TextEncoder();
const utf8Decoder = new TextDecoder("utf-8", { fatal: true });
export class Reader {
  data: Uint8Array;
  pos = 0;
  constructor(data: Uint8Array) {
    this.data = data;
  }
  get remaining(): number {
    return this.data.length - this.pos;
  }
  readU8(codec: string): number {
    if (this.pos >= this.data.length) {
      throw new BmlError("shortBuffer", codec, this.pos, "need 1 byte");
    }
    return this.data[this.pos++];
  }
  readBytes(codec: string, n: number): Uint8Array {
    if (!Number.isInteger(n) || n < 0) {
      throw new BmlError("negativeLength", codec, this.pos, `length ${n}`);
    }
    if (this.pos + n > this.data.length) {
      throw new BmlError(
        "shortBuffer",
        codec,
        this.pos,
        `need ${n} bytes, have ${this.remaining}`,
      );
    }
    const b = this.data.slice(this.pos, this.pos + n);
    this.pos += n;
    return b;
  }
  readRest(): Uint8Array {
    const b = this.data.slice(this.pos);
    this.pos = this.data.length;
    return b;
  }
  readF32(codec: string): number {
    const b = this.readBytes(codec, 4);
    return new DataView(b.buffer, b.byteOffset, 4).getFloat32(0, false);
  }
  readF64(codec: string): number {
    const b = this.readBytes(codec, 8);
    return new DataView(b.buffer, b.byteOffset, 8).getFloat64(0, false);
  }
  readF32Le(codec: string): number {
    const b = this.readBytes(codec, 4);
    return new DataView(b.buffer, b.byteOffset, 4).getFloat32(0, true);
  }
  readF64Le(codec: string): number {
    const b = this.readBytes(codec, 8);
    return new DataView(b.buffer, b.byteOffset, 8).getFloat64(0, true);
  }
}
export class Writer {
  private bytes: number[] = [];
  get length(): number {
    return this.bytes.length;
  }
  writeU8(v: number): void {
    this.bytes.push(v & 0xff);
  }
  writeBytes(b: Uint8Array): void {
    for (const x of b) this.bytes.push(x);
  }
  writeBytesFixed(codec: string, b: Uint8Array, n: number): void {
    if (b.length > n) {
      throw new BmlError(
        "bytesTooLong",
        codec,
        this.length,
        `have ${b.length}, want ${n}`,
      );
    }
    this.writeBytes(b);
    for (let i = b.length; i < n; i++) this.bytes.push(0);
  }
  writeF32(v: number): void {
    const buf = new ArrayBuffer(4);
    new DataView(buf).setFloat32(0, v, false);
    this.writeBytes(new Uint8Array(buf));
  }
  writeF64(v: number): void {
    const buf = new ArrayBuffer(8);
    new DataView(buf).setFloat64(0, v, false);
    this.writeBytes(new Uint8Array(buf));
  }
  writeF32Le(v: number): void {
    const buf = new ArrayBuffer(4);
    new DataView(buf).setFloat32(0, v, true);
    this.writeBytes(new Uint8Array(buf));
  }
  writeF64Le(v: number): void {
    const buf = new ArrayBuffer(8);
    new DataView(buf).setFloat64(0, v, true);
    this.writeBytes(new Uint8Array(buf));
  }
  toBytes(): Uint8Array {
    return Uint8Array.from(this.bytes);
  }
}
export function toU8(v: number): number {
  return v & 0xff;
}
export function toI8(v: number): number {
  const u = v & 0xff;
  return u >= 0x80 ? u - 0x100 : u;
}
export function toU16(v: number): number {
  return v & 0xffff;
}
export function toI16(v: number): number {
  const u = v & 0xffff;
  return u >= 0x8000 ? u - 0x10000 : u;
}
export function toI32(v: number | bigint): number {
  return typeof v === "bigint" ? Number(BigInt.asIntN(32, v)) : v | 0;
}
export function toU32(v: number): number {
  return v >>> 0;
}
export function toU64(v: bigint): bigint {
  return v & 0xffffffffffffffffn;
}
export function toI64(v: bigint): bigint {
  const u = v & 0xffffffffffffffffn;
  return u >= 0x8000000000000000n ? u - 0x10000000000000000n : u;
}
export function toBigI(v: bigint, bits: number): bigint {
  const u = v & ((1n << BigInt(bits)) - 1n);
  const sign = 1n << BigInt(bits - 1);
  return (u & sign) !== 0n ? u - (sign << 1n) : u;
}
export function floorDiv(a: number, b: number): number {
  return b === 0 ? 0 : Math.floor(a / b);
}
export function intMod(a: number, b: number): number {
  if (b === 0) return 0;
  const r = a % b;
  return r !== 0 && r < 0 !== b < 0 ? r + b : r;
}
export function floorDivBigint(a: bigint, b: bigint): bigint {
  if (b === 0n) return 0n;
  const q = a / b;
  const r = a % b;
  return r !== 0n && a < 0n !== b < 0n ? q - 1n : q;
}
export function intModBigint(a: bigint, b: bigint): bigint {
  if (b === 0n) return 0n;
  const r = a % b;
  return r !== 0n && r < 0n !== b < 0n ? r + b : r;
}
export function signExtend(v: number, bits: number): number {
  if (bits >= 32) return toI32(v);
  const u = v % 2 ** bits;
  return u >= 2 ** (bits - 1) ? u - 2 ** bits : u;
}
export function signExtendBigint(v: bigint, bits: number): bigint {
  return toBigI(v, bits);
}
export function utf16Units(s: string): number {
  let n = 0;
  for (const c of s) {
    const cp = c.codePointAt(0);
    n += cp !== undefined && cp > 0xffff ? 2 : 1;
  }
  return n;
}
export function utf8Len(s: string): number {
  return utf8Encoder.encode(s).length;
}
export function decodeUtf8(
  bytes: Uint8Array,
  codec: string,
  offset: number,
): string {
  try {
    return utf8Decoder.decode(bytes);
  } catch {
    throw new BmlError("invalidUtf8", codec, offset, "invalid UTF-8");
  }
}
"#;

/// The exact TS keywords that gain a trailing `_` after case conversion.
const TS_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
    "await",
    "async",
    "as",
    "asserts",
    "satisfies",
    "type",
    "string",
    "number",
    "boolean",
    "bigint",
    "symbol",
    "object",
    "unknown",
    "never",
    "undefined",
    "arguments",
    "eval",
    "from",
    "of",
    "get",
    "set",
    "declare",
    "module",
    "namespace",
    "require",
];

/// The emitted decode function for a codec, e.g. `decodeHandshake`.
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("decode{}", program.codecs[codec].name.pascal())
}

/// The emitted encode function for a codec, e.g. `encodeHandshake`.
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    format!("encode{}", program.codecs[codec].name.pascal())
}

/// The emitted type name for a declared type, e.g. `Handshake`.
pub fn type_name(program: &Program, ty: TypeIdx) -> String {
    escape(program.types[ty].name.pascal(), TS_KEYWORDS)
}

pub fn emit_typescript(resolved: &ResolvedSchema, out_dir: &Path) -> BmlResult<()> {
    emit_typescript_with_vectors(resolved, out_dir, crate::TsTarget::default(), &[]).map(|_| ())
}

pub fn emit_typescript_with_target(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    target: crate::TsTarget,
) -> BmlResult<()> {
    emit_typescript_with_vectors(resolved, out_dir, target, &[]).map(|_| ())
}

pub fn emit_typescript_with_vectors(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    target: crate::TsTarget,
    vectors: &[Vector],
) -> BmlResult<Option<TestRun>> {
    let program = crate::ir::typed(resolved)?;
    let printer = Printer { program: &program };
    let mut parts: BTreeMap<String, String> = BTreeMap::new();
    parts.insert("runtime".to_string(), RUNTIME.to_string());
    let mut stem = String::new();
    let mut buf = String::new();
    let mut flush = |stem: &str, buf: &mut String| {
        if !buf.is_empty() {
            parts.entry(stem.to_string()).or_default().push_str(buf);
            buf.clear();
        }
    };
    for t in 0..program.types.len() {
        let here = program.output_stem(&program.types[t].origin);
        if here != stem {
            flush(&stem, &mut buf);
            stem = here;
        }
        buf.push_str(&printer.type_block(t));
    }
    for c in 0..program.codecs.len() {
        let here = program.output_stem(&program.codecs[c].origin);
        if here != stem {
            flush(&stem, &mut buf);
            stem = here;
        }
        buf.push_str(&printer.codec_block(c));
    }
    flush(&stem, &mut buf);

    let symbols = symbols(&program);
    let runtime = runtime_symbols();
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    let mut stems: Vec<String> = parts.keys().cloned().collect();
    // Runtime first, then the categories alphabetically.
    stems.sort_by_key(|stem| (stem != "runtime", stem.clone()));
    for (stem, body) in &parts {
        let body = format!("{}\n", body.trim_end_matches('\n'));
        let src = format!("{}{body}", preamble(stem, &body, &symbols, &runtime));
        fs::write(out_dir.join(format!("{stem}.ts")), src).map_err(|e| emit(e.to_string()))?;
    }
    let index: String = stems
        .iter()
        .map(|stem| format!("export * from \"./{stem}.ts\";\n"))
        .collect();
    fs::write(out_dir.join("bmlgen.ts"), index).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("tsconfig.json"), tsconfig(target)).map_err(|e| emit(e.to_string()))?;

    if vectors.is_empty() {
        return Ok(None);
    }

    let mut embedded: Vec<&Vector> = Vec::new();
    for v in vectors {
        if program.packet_codec(&v.packet).is_some() {
            embedded.push(v);
        }
    }
    if embedded.is_empty() {
        // No vector has a codec here: write no test rather than one that checks nothing.
        return Ok(None);
    }
    let test = vectors_test(&program, &embedded);
    fs::write(out_dir.join("vectors.test.ts"), test).map_err(|e| emit(e.to_string()))?;
    Ok(Some(TestRun {
        file: "vectors.test.ts".to_string(),
        command: "node --experimental-strip-types vectors.test.ts".to_string(),
        vectors: embedded.len(),
    }))
}

/// The emitted `tsconfig.json`: the requested `target`, `ESNext` modules, and
/// strict type checking.
fn tsconfig(target: crate::TsTarget) -> String {
    format!(
        "{{\n  \"compilerOptions\": {{\n    \"target\": \"{}\",\n    \"module\": \"ESNext\",\n    \"strict\": true,\n    \"allowImportingTsExtensions\": true,\n    \"noEmit\": true\n  }}\n}}\n",
        target.as_str(),
    )
}

/// Emit the vectors.test.ts source: embeds chosen vectors as literals, runs
/// decode (strict full-consume), re-encode, byte-compare for each. Matches
/// the check shape from tests/test_emit_ts.rs roundtrip_schema.
fn vectors_test(program: &Program, embedded: &[&Vector]) -> String {
    let mut body = String::from(concat!(
        "const fails: string[] = [];\n",
        "function check(\n",
        "  name: string,\n",
        "  raw: number[],\n",
        "  dec: (r: bml.Reader) => unknown,\n",
        "  enc: (w: bml.Writer, v: never) => void,\n",
        ") {\n",
        "  try {\n",
        "    const reader = new bml.Reader(Uint8Array.from(raw));\n",
        "    const v = dec(reader);\n",
        "    if (reader.remaining !== 0) {\n",
        "      fails.push(name + \" trailing \" + reader.remaining);\n",
        "      return;\n",
        "    }\n",
        "    const w = new bml.Writer();\n",
        "    enc(w, v as never);\n",
        "    const back = w.toBytes();\n",
        "    if (back.length !== raw.length || back.some((b, i) => b !== raw[i])) {\n",
        "      fails.push(name + \" bytes \" + back.length + \" \" + raw.length);\n",
        "    }\n",
        "  } catch (e) {\n",
        "    fails.push(name + \" \" + e);\n",
        "  }\n",
        "}\n",
    ));
    for v in embedded {
        let c = program.packet_codec(&v.packet).unwrap();
        let dec = decode_fn(program, c);
        let enc = encode_fn(program, c);
        body.push_str(&format_check_call(&v.packet, &v.hex, &dec, &enc));
    }
    body.push_str(concat!(
        "if (fails.length) {\n",
        "  console.log(fails.slice(0, 30).join(\"\\n\"));\n",
        "  throw new Error(\"fail \" + fails.length);\n",
        "}\n",
    ));
    body.push_str(&format!("console.log(\"vectors\", {});\n", embedded.len()));
    format!("import * as bml from \"./bmlgen.ts\";\n{body}")
}

/// Whether a full source line fits prettier's 80-column width.
fn ts_line_fits(text: &str) -> bool {
    text.len() <= 80
}

/// Whether a line at indent `ind` (two spaces each) fits with trailing text
/// appended to the last fragment on that line.
fn ts_fits_at(ind: usize, content_len: usize, trail: usize) -> bool {
    ind * 2 + content_len + trail <= 80
}

/// Text printed after a wrapped operand, before the wrapper's closing `)`.
fn wrap_mid_tail(mid: &str) -> (&str, &str) {
    if mid == ")" {
        return ("", ")");
    }
    if let Some(rest) = mid.strip_suffix(')') {
        return (rest, ")");
    }
    (mid, "")
}

fn hex_bytes(hex: &str) -> Vec<String> {
    if hex.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = hex.chars().collect();
    chars
        .chunks(2)
        .map(|chunk| {
            let h = chunk.first().and_then(|c| c.to_digit(16)).unwrap_or(0) as u8;
            let l = chunk.get(1).and_then(|c| c.to_digit(16)).unwrap_or(0) as u8;
            ((h << 4) | l).to_string()
        })
        .collect()
}

/// Greedy-wrap comma-separated tokens into lines that fit at `line_indent`.
fn wrap_csv(nums: &[String], line_indent: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    for n in nums {
        let trial = if cur.is_empty() {
            n.clone()
        } else {
            format!("{cur}, {n}")
        };
        let line = format!("{}{},", "  ".repeat(line_indent), trial);
        if cur.is_empty() || ts_line_fits(&line) {
            cur = trial;
        } else {
            rows.push(cur);
            cur = n.clone();
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    rows
}

/// The second argument to `check` (a byte array), including indent and trailing comma.
fn format_check_array_arg(nums: &[String], arg_indent: usize) -> String {
    let pad = "  ".repeat(arg_indent);
    if nums.is_empty() {
        return format!("{pad}[],");
    }
    let inner = nums.join(", ");
    let single = format!("{pad}[{inner}],");
    if ts_line_fits(&single) {
        return single;
    }
    let rows = wrap_csv(nums, arg_indent + 1);
    let pad_c = "  ".repeat(arg_indent + 1);
    let mut out = format!("{pad}[\n");
    for row in &rows {
        out.push_str(&format!("{pad_c}{row},\n"));
    }
    out.push_str(&format!("{pad}],"));
    out
}

/// One `check(...);` statement in prettier's one-line or broken-call shape.
fn format_check_call(packet: &str, hex: &str, dec: &str, enc: &str) -> String {
    let nums = hex_bytes(hex);
    let array = if nums.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", nums.join(", "))
    };
    let single = format!("check({packet:?}, {array}, bml.{dec}, bml.{enc});");
    if ts_line_fits(&single) {
        return format!("{single}\n");
    }
    let array = format_check_array_arg(&nums, 1);
    format!("check(\n  {packet:?},\n{array}\n  bml.{dec},\n  bml.{enc},\n);\n")
}
/// A generated export and the module that defines it, plus whether it is a
/// value (a `const`/`class`/`function`) or a type only.
struct Symbol {
    stem: String,
    value: bool,
}

/// Every name `bmlgen.ts` re-exports, keyed by emitted identifier.
fn symbols(program: &Program) -> BTreeMap<String, Symbol> {
    let mut out = BTreeMap::new();
    for t in 0..program.types.len() {
        let value = matches!(
            program.types[t].kind,
            crate::ir::typed::TypeKind::Enum { .. }
        );
        out.insert(
            type_name(program, t),
            Symbol {
                stem: program.output_stem(&program.types[t].origin),
                value,
            },
        );
    }
    for c in 0..program.codecs.len() {
        let stem = program.output_stem(&program.codecs[c].origin);
        for name in [decode_fn(program, c), encode_fn(program, c)] {
            out.insert(
                name,
                Symbol {
                    stem: stem.clone(),
                    value: true,
                },
            );
        }
    }
    out
}

/// The exports of [`RUNTIME`]: `(name, is_value)` in source order.
fn runtime_symbols() -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for line in RUNTIME.lines() {
        let (rest, value) = if let Some(rest) = line.strip_prefix("export type ") {
            (rest, false)
        } else if let Some(rest) = line
            .strip_prefix("export function ")
            .or_else(|| line.strip_prefix("export class "))
            .or_else(|| line.strip_prefix("export const "))
        {
            (rest, true)
        } else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

/// Identifiers in `text`, skipping comments, string literals, and object
/// literal property keys. Generated names are unique, so a named binding
/// means a real reference; schema text inside strings and docs never adds
/// one, and an open enum's `const X = { Member: 1 }` keys are not
/// references to a type named `Member`.
fn identifiers(text: &str) -> HashSet<String> {
    let bytes = text.as_bytes();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'"' | b'\'' | b'`' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        c if c == quote => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' => {
                let start = i;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
                {
                    i += 1;
                }
                if !is_property_key(text, start, i) {
                    out.insert(text[start..i].to_string());
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Whether the identifier `text[start..end]` is an object literal or type
/// member key: the previous non-space character opens a member (`{` or `,`)
/// and the next non-space character is `:` (or `?:`). A ternary's true arm
/// follows `?`, never `{`/`,`, so it is unaffected.
fn is_property_key(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].trim_end().chars().next_back();
    if !matches!(before, Some('{') | Some(',')) {
        return false;
    }
    let mut rest = text[end..].chars();
    let mut next = rest.next();
    if next == Some('?') {
        next = rest.next();
    }
    next == Some(':')
}

/// The import block of one module: the runtime helpers and the symbols of
/// other categories its text names, each in prettier's one-or-many-line shape.
fn preamble(
    stem: &str,
    body: &str,
    symbols: &BTreeMap<String, Symbol>,
    runtime: &[(String, bool)],
) -> String {
    let names = identifiers(body);
    let mut runtime_values = BTreeSet::new();
    let mut runtime_types = BTreeSet::new();
    for (name, value) in runtime {
        if stem == "runtime" || !names.contains(name) {
            continue;
        }
        if *value {
            runtime_values.insert(name.clone());
        } else {
            runtime_types.insert(name.clone());
        }
    }
    let mut deps: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for name in &names {
        let Some(symbol) = symbols.get(name) else {
            continue;
        };
        if symbol.stem == stem {
            continue;
        }
        let slot = deps.entry(symbol.stem.clone()).or_default();
        if symbol.value {
            slot.0.insert(name.clone());
        } else {
            slot.1.insert(name.clone());
        }
    }
    let mut out = String::new();
    if !runtime_values.is_empty() {
        out.push_str(&import_stmt("import", &runtime_values, "./runtime.ts"));
    }
    if !runtime_types.is_empty() {
        out.push_str(&import_stmt("import type", &runtime_types, "./runtime.ts"));
    }
    for (dep, (values, types)) in deps {
        if !values.is_empty() {
            out.push_str(&import_stmt("import", &values, &format!("./{dep}.ts")));
        }
        if !types.is_empty() {
            out.push_str(&import_stmt("import type", &types, &format!("./{dep}.ts")));
        }
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// One import declaration, broken the way prettier breaks it at 80 columns.
fn import_stmt(keyword: &str, names: &BTreeSet<String>, module: &str) -> String {
    let joined: Vec<&str> = names.iter().map(String::as_str).collect();
    let single = format!("{keyword} {{ {} }} from \"{module}\";", joined.join(", "));
    if single.len() <= 80 {
        return format!("{single}\n");
    }
    let mut out = format!("{keyword} {{\n");
    for name in joined {
        out.push_str(&format!("  {name},\n"));
    }
    out.push_str(&format!("}} from \"{module}\";\n"));
    out
}

struct Printer<'a> {
    program: &'a Program,
}

/// A `camelCase` identifier, keyword-escaped.
fn camel(name: &crate::ir::typed::Name) -> String {
    escape(name.camel(), TS_KEYWORDS)
}

/// A `PascalCase` identifier, keyword-escaped.
fn pascal(name: &crate::ir::typed::Name) -> String {
    escape(name.pascal(), TS_KEYWORDS)
}

/// A double-quoted TS string literal with JSON-style escapes.
fn ts_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
/// `{ a, b }`, or `{}` when there are no parts.
fn braced(parts: &[String]) -> String {
    if parts.is_empty() {
        "{}".to_string()
    } else {
        format!("{{ {} }}", parts.join(", "))
    }
}
/// The TS type of a value type: 32-bit-or-narrower integers are `number`,
/// 64-bit classes and `i128` are `bigint`.
fn ts_ty(program: &Program, ty: &Ty) -> String {
    match ty {
        Ty::Int(t) => {
            if t.bits() <= 32 {
                "number".to_string()
            } else {
                "bigint".to_string()
            }
        }
        Ty::Bool => "boolean".to_string(),
        Ty::F32 | Ty::F64 => "number".to_string(),
        Ty::Str => "string".to_string(),
        Ty::Bytes => "Uint8Array".to_string(),
        Ty::List(t) => format!("{}[]", ts_ty(program, t)),
        Ty::Named(t) => type_name(program, *t),
    }
}

/// Whether an integer type is signed (enums resolve to their base).
fn int_signed(program: &Program, ty: &Ty) -> bool {
    match ty {
        Ty::Int(t) => t.signed(),
        Ty::Bool => false,
        Ty::Named(t) => match &program.types[*t].kind {
            crate::ir::typed::TypeKind::Enum { base, .. } => base.signed(),
            _ => true,
        },
        _ => true,
    }
}

/// A record field name; a block-arm record's own `kind` field yields to the
/// printer-owned sum discriminant.
fn field_ident(program: &Program, ty: TypeIdx, field: usize) -> String {
    let decl = &program.fields(ty)[field];
    let mut name = camel(&decl.name);
    if name == "kind" && matches!(program.types[ty].origin, Origin::Variant { .. }) {
        name.push('_');
    }
    name
}

/// A doc comment naming the origin of a type or codec.
fn origin_doc(program: &Program, origin: &Origin) -> String {
    match origin {
        Origin::Packet { name, .. } => format!("/** Packet `{name}`. */"),
        Origin::Encoding { name } => format!("/** Encoding `{name}`. */"),
        Origin::Enum { name } => format!("/** Enum `{name}`. */"),
        Origin::Frame { name } => format!("/** Frame `{name}`. */"),
        Origin::Field { parent, field } => {
            format!(
                "/** Field `{field}` of `{}`. */",
                type_name(program, *parent)
            )
        }
        Origin::Variant { sum, variant } => {
            let name = match &program.types[*sum].kind {
                crate::ir::typed::TypeKind::Sum { variants } => variants[*variant].name.snake(),
                _ => "?".to_string(),
            };
            format!("/** Variant `{name}` of `{}`. */", type_name(program, *sum))
        }
    }
}

impl<'a> Printer<'a> {
    /// One declared type: origin doc, declaration, and the blank line after it.
    fn type_block(&self, t: TypeIdx) -> String {
        let mut out = String::new();
        let decl = &self.program.types[t];
        out.push_str(&origin_doc(self.program, &decl.origin));
        out.push('\n');
        match &decl.kind {
            crate::ir::typed::TypeKind::Record { .. } => self.record(t, &mut out),
            crate::ir::typed::TypeKind::Sum { .. } => self.sum(t, &mut out),
            crate::ir::typed::TypeKind::Enum { base, members } => {
                self.enumer(t, *base, members, &mut out)
            }
            crate::ir::typed::TypeKind::Alias { target } => {
                out.push_str(&format!(
                    "export type {} = {};\n",
                    type_name(self.program, t),
                    ts_ty(self.program, &Ty::Named(*target))
                ));
            }
        }
        out.push('\n');
        out
    }

    /// A record interface; a block arm carries its variant discriminant first.
    fn record(&self, ty: TypeIdx, out: &mut String) {
        let is_arm = matches!(self.program.types[ty].origin, Origin::Variant { .. });
        if !is_arm && self.program.fields(ty).is_empty() {
            out.push_str(&format!(
                "export interface {} {{}}\n",
                type_name(self.program, ty)
            ));
            return;
        }
        out.push_str(&format!(
            "export interface {} {{\n",
            type_name(self.program, ty)
        ));
        if is_arm {
            let (sum, variant) = match &self.program.types[ty].origin {
                Origin::Variant { sum, variant } => (*sum, *variant),
                _ => unreachable!(),
            };
            let kind = match &self.program.types[sum].kind {
                crate::ir::typed::TypeKind::Sum { variants } => variants[variant].name.snake(),
                _ => "other".to_string(),
            };
            out.push_str(&format!("  readonly kind: \"{kind}\";\n"));
        }
        for f in 0..self.program.fields(ty).len() {
            let decl = &self.program.fields(ty)[f];
            let opt = if decl.optional { "?" } else { "" };
            out.push_str(&format!(
                "  readonly {}{}: {};\n",
                field_ident(self.program, ty, f),
                opt,
                ts_ty(self.program, &decl.ty)
            ));
        }
        out.push_str("}\n");
    }

    /// One variant interface per arm plus the union alias, in schema order.
    fn sum(&self, ty: TypeIdx, out: &mut String) {
        let name = type_name(self.program, ty);
        let variants = self.program.variants(ty).to_vec();
        for (v, decl) in variants.iter().enumerate() {
            out.push_str(&self.variant_interface(ty, v, decl));
            out.push('\n');
        }
        let members: Vec<String> = variants
            .iter()
            .enumerate()
            .map(|(v, _)| self.variant_name(ty, v))
            .collect();
        let single = format!("export type {name} = {};\n", members.join(" | "));
        if single.len() <= 80 {
            out.push_str(&single);
            return;
        }
        let flat = format!("  {};\n", members.join(" | "));
        if flat.len() <= 80 {
            out.push_str(&format!("export type {name} =\n"));
            out.push_str(&flat);
            return;
        }
        out.push_str(&format!("export type {name} =\n"));
        let last = members.len() - 1;
        for (i, m) in members.iter().enumerate() {
            if i == last {
                out.push_str(&format!("  | {m};\n"));
            } else {
                out.push_str(&format!("  | {m}\n"));
            }
        }
    }

    fn variant_name(&self, ty: TypeIdx, variant: usize) -> String {
        let decl = &self.program.variants(ty)[variant];
        if decl.tags.is_empty() {
            format!("{}Other", type_name(self.program, ty))
        } else {
            format!("{}{}", type_name(self.program, ty), pascal(&decl.name))
        }
    }

    /// A variant interface: `{ kind: "tagN"; value: T }` for scalar and
    /// named-record payloads, the arm record's fields for inline blocks,
    /// `{ kind: "other"; tag: number; value: T }` for `_` (the payload is
    /// always decoded, so the field is required).
    fn variant_interface(
        &self,
        ty: TypeIdx,
        variant: usize,
        decl: &crate::ir::typed::VariantDecl,
    ) -> String {
        let name = self.variant_name(ty, variant);
        let mut out = format!("export interface {name} {{\n");
        if decl.tags.is_empty() {
            out.push_str("  readonly kind: \"other\";\n  readonly tag: number;\n");
            if let Some(payload) = &decl.payload {
                // Decode always constructs the payload, so the field is
                // required: encode reads it without a `!` assertion and with
                // no `missingField` branch to write.
                out.push_str(&format!(
                    "  readonly value: {};\n",
                    ts_ty(self.program, payload)
                ));
            }
            out.push_str("}\n");
            return out;
        }
        out.push_str(&format!("  readonly kind: \"{}\";\n", decl.name.snake()));
        match &decl.payload {
            None => {}
            Some(Ty::Named(t)) if self.is_block_arm(*t) => {
                for f in 0..self.program.fields(*t).len() {
                    let field = &self.program.fields(*t)[f];
                    let opt = if field.optional { "?" } else { "" };
                    out.push_str(&format!(
                        "  readonly {}{}: {};\n",
                        field_ident(self.program, *t, f),
                        opt,
                        ts_ty(self.program, &field.ty)
                    ));
                }
            }
            Some(payload) => {
                out.push_str(&format!(
                    "  readonly value: {};\n",
                    ts_ty(self.program, payload)
                ));
            }
        }
        out.push_str("}\n");
        out
    }

    /// Whether a record type is an inline block arm (decodes flat into its sum).
    fn is_block_arm(&self, ty: TypeIdx) -> bool {
        matches!(
            (&self.program.types[ty].kind, &self.program.types[ty].origin),
            (
                crate::ir::typed::TypeKind::Record { .. },
                Origin::Variant { .. }
            )
        )
    }

    fn enumer(
        &self,
        ty: TypeIdx,
        base: IntTy,
        members: &[crate::ir::typed::EnumMember],
        out: &mut String,
    ) {
        let name = type_name(self.program, ty);
        // The open value type (`decode` returns it directly); the `const`
        // below only carries the known members.
        out.push_str(&format!(
            "export type {name} = {};\n",
            ts_ty(self.program, &Ty::Int(base))
        ));
        let entries: Vec<String> = members
            .iter()
            .map(|m| format!("{}: {}", pascal(&m.name), m.value))
            .collect();
        let single = format!(
            "export const {name} = {{ {} }} as const;\n",
            entries.join(", ")
        );
        if single.len() <= 80 {
            out.push_str(&single);
        } else {
            out.push_str(&format!("export const {name} = {{\n"));
            for e in &entries {
                out.push_str(&format!("  {e},\n"));
            }
            out.push_str("} as const;\n");
        }
    }
}
/// The TS domain of an integer expression.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dom {
    Num,
    Big,
}

/// ±2^53: every integer in this range is exactly representable as an f64.
const SAFE53: i128 = 1 << 53;

/// `int_range` proves every value fits exactly in a `number`.
fn fits53(range: Option<(i128, i128)>) -> bool {
    match range {
        Some((lo, hi)) => lo >= -SAFE53 && hi <= SAFE53,
        None => false,
    }
}

/// One call argument for prettier-style breaking: an expression with wrap
/// text around it, or short raw text that never breaks.
enum CallArg<'b> {
    E {
        pre: String,
        e: &'b Expr,
        mid: String,
    },
    Raw(String),
}

/// One codec function's body printer.
struct FnGen<'a> {
    program: &'a Program,
    codec: String,
    decode: bool,
    /// Per local: print as `number` (schema `Arg`/`Tag` parameters are `i64`
    /// in the typed program but `number` in TS).
    num_like: Vec<bool>,
    /// Per local: introduce with `const` (stored exactly once, never via
    /// `Declare`; see `const_locals`).
    const_local: Vec<bool>,
    /// Per local: targeted by any `Bind` store (list pre-declarations
    /// without one print `const`, mutated only through `.push`).
    stored_local: Vec<bool>,
    /// The sum tag parameter's name, for `other` variant literals.
    tag_param: Option<String>,
}
impl<'a> FnGen<'a> {
    fn offset(&self) -> &'static str {
        if self.decode { "r.pos" } else { "w.length" }
    }

    fn local_name(&self, body: &Body, id: usize) -> String {
        camel(&body.locals[id].name)
    }

    /// The TS domain of an expression: `number` whenever `int_range` proves
    /// every value fits exactly in an f64. Named storage (locals, fields)
    /// keeps its declared type's domain — the interface owns that — as do
    /// `num_like` parameters; lengths, counts, and `remaining` are
    /// memory-bounded, hence always `number`.
    fn dom(&self, body: &Body, e: &Expr) -> Dom {
        if let Expr::Local(id) = e
            && self.num_like[*id]
        {
            return Dom::Num;
        }
        match e {
            Expr::Local(_) | Expr::Field { .. } | Expr::Get(_) => {
                let (ty, _) = self.program.expr_ty(body, e);
                match ty {
                    Ty::Int(t) if t.bits() > 32 => Dom::Big,
                    _ => Dom::Num,
                }
            }
            Expr::Call {
                func:
                    Func::Utf8Len | Func::Utf16Units | Func::StrLen | Func::BytesLen | Func::ListLen,
                ..
            }
            | Expr::Remaining => Dom::Num,
            _ => self.dom_computed(body, e),
        }
    }

    /// The domain of a computed expression. `Binary` delegates to
    /// `number_op` so the decision here and in `binary()` agree by
    /// construction; anything printing `bigint` but fitting exactly still
    /// converts with `Number`.
    fn dom_computed(&self, body: &Body, e: &Expr) -> Dom {
        if let Expr::Binary {
            op,
            lhs,
            rhs,
            ty,
            wraps,
        } = e
        {
            if op.is_comparison() || op.is_logical() {
                return Dom::Num;
            }
            if self.number_op(body, e, *op, lhs, rhs, ty, *wraps) {
                return Dom::Num;
            }
            return Dom::Big;
        }
        // Casts print their target's helper, so their domain is the
        // target's; everything else computed goes by range.
        if let Expr::Cast { to, .. } = e {
            match to {
                Ty::Int(t) if t.bits() > 32 => return Dom::Big,
                _ => return Dom::Num,
            }
        }
        let (ty, _) = self.program.expr_ty(body, e);
        match ty {
            Ty::Int(t) if t.bits() > 32 => {
                if fits53(self.program.int_range(body, e)) {
                    Dom::Num
                } else {
                    Dom::Big
                }
            }
            _ => Dom::Num,
        }
    }
    /// The integer class of an arithmetic result type.
    fn result_int(ty: &Ty) -> IntTy {
        match ty {
            Ty::Int(t) => *t,
            _ => IntTy::I32,
        }
    }

    /// Whether an integer expression is signed (for `>>` versus `>>>`):
    /// leaves from declarations, non-negative literals unsigned, `& |`
    /// unsigned iff both sides are, `<< >>` inheriting the left, `+ - * / %`
    /// signed iff either side is, `sign_extend` signed, casts by target.
    fn sign(&self, body: &Body, e: &Expr) -> bool {
        match e {
            Expr::Int { value, .. } => *value < 0,
            Expr::Bool(_) | Expr::Present(_) | Expr::Not(_) => false,
            Expr::Local(_) | Expr::Field { .. } | Expr::Get(_) => {
                let (ty, _) = self.program.expr_ty(body, e);
                int_signed(self.program, &ty)
            }
            Expr::Binary { op, lhs, rhs, .. } => match op {
                BinOp::BitAnd | BinOp::BitOr => self.sign(body, lhs) || self.sign(body, rhs),
                BinOp::Shl | BinOp::Shr => self.sign(body, lhs),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    self.sign(body, lhs) || self.sign(body, rhs)
                }
                _ => false,
            },
            Expr::Cond { then, .. } => self.sign(body, then),
            Expr::Cast { to, .. } => int_signed(self.program, to),
            Expr::Call { func, args } => match func {
                Func::SignExtend => true,
                Func::Min | Func::Max => self.sign(body, &args[0]) || self.sign(body, &args[1]),
                _ => false,
            },
            Expr::Remaining => true,
            Expr::Str(_) | Expr::StrBytes(_) | Expr::Construct { .. } | Expr::Variant { .. } => {
                false
            }
        }
    }

    fn expr(&self, body: &Body, e: &Expr) -> String {
        match e {
            Expr::Int { value, ty } => {
                if ty.bits() <= 32 {
                    format!("{value}")
                } else {
                    format!("{value}n")
                }
            }
            Expr::Bool(b) => b.to_string(),
            Expr::Str(s) => ts_string(s),
            Expr::Local(id) => self.local_name(body, *id),
            Expr::Field { base, ty, field } => {
                format!(
                    "{}.{}",
                    self.expr(body, base),
                    field_ident(self.program, *ty, *field)
                )
            }
            Expr::Present(inner) => format!("({} !== undefined)", self.expr(body, inner)),
            Expr::Get(inner) => self.expr(body, inner),
            Expr::Not(inner) => format!("!{}", self.atom(body, inner)),
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } => self.binary(body, e, *op, lhs, rhs, ty, *wraps, true),
            Expr::Cond {
                cond, then, els, ..
            } => {
                // `?:` binds loosest: only a nested condition needs parens.
                let c = if matches!(cond.as_ref(), Expr::Cond { .. }) {
                    format!("({})", self.expr(body, cond))
                } else {
                    self.expr(body, cond)
                };
                let big = self.dom(body, e) == Dom::Big;
                format!(
                    "{c} ? {} : {}",
                    self.cond_branch(body, then, big),
                    self.cond_branch(body, els, big)
                )
            }
            Expr::Cast { expr, to } => self.cast(body, expr, to),
            Expr::Call { func, args } => self.call(body, *func, args),
            Expr::StrBytes(inner) => format!("utf8Encoder.encode({})", self.expr(body, inner)),
            Expr::Remaining => "r.remaining".to_string(),
            Expr::Construct { .. } | Expr::Variant { .. } => self.value(body, e),
        }
    }

    /// `expr` without this node's `>>> 0` normalize, for an operand whose
    /// parent already coerces (`& | ^ << >> >>>` see only the value mod
    /// 2^32 through ToInt32/ToUint32). One level only: deeper operands
    /// print through `expr`, keeping their own normalizes.
    fn expr_nowrap(&self, body: &Body, e: &Expr) -> String {
        if let Expr::Binary {
            op,
            lhs,
            rhs,
            ty,
            wraps,
        } = e
        {
            return self.binary(body, e, *op, lhs, rhs, ty, *wraps, false);
        }
        self.expr(body, e)
    }

    /// An operand: parenthesized when it is a binary operation, a ternary,
    /// or a presence test (all bind looser than `!`).
    fn atom(&self, body: &Body, e: &Expr) -> String {
        match e {
            Expr::Binary { .. } | Expr::Cond { .. } | Expr::Present(_) => {
                format!("({})", self.expr(body, e))
            }
            _ => self.expr(body, e),
        }
    }

    /// Operator precedence for minimal parentheses (JS levels).
    fn prec(op: BinOp) -> u8 {
        match op {
            BinOp::Mul | BinOp::Div | BinOp::Mod => 14,
            BinOp::Add | BinOp::Sub => 13,
            BinOp::Shl | BinOp::Shr => 12,
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 11,
            BinOp::Eq | BinOp::Ne => 10,
            BinOp::BitAnd => 9,
            BinOp::BitXor => 8,
            BinOp::BitOr => 7,
            BinOp::And => 6,
            BinOp::Or => 5,
        }
    }

    /// A binary operand with prettier's parentheses: minimal ones plus its
    /// clarifying set (`& | ^ <<` group tighter operands, shifts group
    /// everything, `%` and `==` chains group left-nested pairs, `&&` groups
    /// under `||`, right-nested `+ *` group). Two refinements keep
    /// prettier-clean output: a `>>> 0` normalize is skipped under a
    /// coercing parent (see `expr_nowrap`), and a call-shaped operand
    /// (`toI64(…)`, `floorDiv(…)`) never takes clarifying parens.
    fn operand(&self, body: &Body, e: &Expr, parent: BinOp, right: bool) -> String {
        if let Expr::Binary {
            op,
            lhs,
            rhs,
            ty,
            wraps,
        } = e
        {
            // A call-shaped operand prints as a call, so it never needs
            // clarifying parens (prettier strips them).
            if self.binary_is_call(body, e, *op, lhs, rhs, ty, *wraps) {
                return self.expr(body, e);
            }
            use BinOp::{BitAnd, BitOr, BitXor, Shl, Shr};
            // Under a coercing parent the `>>> 0` normalize is a no-op, so
            // the operand prints without it (a remaining `wraps` mask still
            // prints as a call, handled above).
            if matches!(
                parent,
                BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | Shl | Shr
            ) && matches!(op, Shl | BitAnd | BitOr | BitXor)
            {
                let s = self.expr_nowrap(body, e);
                return if child_paren(parent, *op, right) {
                    format!("({s})")
                } else {
                    s
                };
            }
            if child_paren(parent, *op, right) {
                return format!("({})", self.expr(body, e));
            }
        }
        if matches!(e, Expr::Cond { .. }) {
            return format!("({})", self.expr(body, e));
        }
        self.expr(body, e)
    }

    /// Whether `binary()` prints this node as a call rather than an
    /// operator: a `wraps` mask (`toI64(…)`/`toU64(…)`/`toBigI(…)`, or a
    /// narrow helper) wraps the operator text, and `Div`/`Mod` always print
    /// `floorDiv(…)`/`intMod(…)` (and the `bigint` pair). Mirrors
    /// `binary()` exactly, so the paren decision sees the printed shape.
    #[allow(clippy::too_many_arguments)]
    fn binary_is_call(
        &self,
        body: &Body,
        whole: &Expr,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ty: &Ty,
        wraps: bool,
    ) -> bool {
        use BinOp::{Add, Div, Mod, Mul, Sub};
        if matches!(op, Div | Mod) {
            return true;
        }
        if !wraps {
            return false;
        }
        if self.number_op(body, whole, op, lhs, rhs, ty, wraps) {
            matches!(op, Add | Sub | Mul)
        } else {
            true
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn binary(
        &self,
        body: &Body,
        whole: &Expr,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ty: &Ty,
        wraps: bool,
        allow_wrap: bool,
    ) -> String {
        let l = self.operand(body, lhs, op, false);
        let r = self.operand(body, rhs, op, true);
        match op {
            BinOp::Eq | BinOp::Ne => {
                // Domains are unified by `cmp_operands`, so strict equality
                // is sound for every operand kind (numbers, bigints,
                // strings, booleans).
                let eq = match op {
                    BinOp::Eq => "===",
                    _ => "!==",
                };
                let (l, r) = self.cmp_operands(body, lhs, rhs, op);
                return format!("{l} {eq} {r}");
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let (l, r) = self.cmp_operands(body, lhs, rhs, op);
                return format!("{l} {} {r}", op.symbol());
            }
            BinOp::And | BinOp::Or => {
                return format!("{l} {} {r}", op.symbol());
            }
            _ => {}
        }
        // Arithmetic and bitwise domains come from `int_range`, not the
        // result class: `number` whenever every intermediate is exact.
        let big = !self.number_op(body, whole, op, lhs, rhs, ty, wraps);
        let l = self.coerced_operand(body, lhs, op, false, big);
        let r = self.coerced_operand(body, rhs, op, true, big);
        if big {
            let raw = match op {
                BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Shl
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor => format!("{l} {} {r}", op.symbol()),
                BinOp::Shr => format!("{l} >> {r}"),
                BinOp::Div => return format!("floorDivBigint({l}, {r})"),
                BinOp::Mod => return format!("intModBigint({l}, {r})"),
                _ => format!("{l} {} {r}", op.symbol()),
            };
            if !wraps {
                return raw;
            }
            let mask = match ty {
                Ty::Int(IntTy::I128) => return format!("toBigI({raw}, 128)"),
                Ty::Int(t) if t.signed() => "toI64",
                _ => "toU64",
            };
            return format!("{mask}({raw})");
        }
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let raw = format!("{l} {} {r}", op.symbol());
                // `wraps` with a narrow target cannot occur (the builder
                // widens instead), but mask defensively all the same.
                if wraps {
                    let helper = narrow_name(Self::result_int(ty));
                    return format!("{helper}({raw})");
                }
                raw
            }
            BinOp::Div => format!("floorDiv({l}, {r})"),
            BinOp::Mod => format!("intMod({l}, {r})"),
            BinOp::Shr => {
                if self.sign(body, lhs) {
                    format!("{l} >> {r}")
                } else {
                    format!("{l} >>> {r}")
                }
            }
            BinOp::Shl | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let raw = format!("{l} {} {r}", op.symbol());
                // Skipped under a coercing parent (`expr_nowrap`): `& | ^`
                // `<<` `>>` `>>>` all funnel operands through ToInt32 or
                // ToUint32, which see only the value mod 2^32, so the
                // normalize is a no-op there and only obscures the chain.
                if allow_wrap && self.needs_unsigned_wrap(body, whole, lhs, rhs) {
                    format!("({raw}) >>> 0")
                } else {
                    raw
                }
            }
            _ => format!("{l} {} {r}", op.symbol()),
        }
    }

    /// Comparison operands in one domain: literals defer to the sibling,
    /// otherwise a remaining `number` side converts via `BigInt` (exact
    /// for integers), so `tsc --strict` never sees mixed domains.
    fn cmp_operands(&self, body: &Body, lhs: &Expr, rhs: &Expr, op: BinOp) -> (String, String) {
        let lits = (
            matches!(lhs, Expr::Int { .. }),
            matches!(rhs, Expr::Int { .. }),
        );
        let (mut lbig, mut rbig) = (self.prints_big(body, lhs), self.prints_big(body, rhs));
        match lits {
            (true, false) => lbig = rbig,
            (false, true) => rbig = lbig,
            _ => {}
        }
        let l = self.cmp_text(body, lhs, op, false, lbig);
        let r = self.cmp_text(body, rhs, op, true, rbig);
        let lp = if lits.0 {
            lbig
        } else {
            self.prints_big(body, lhs)
        };
        let rp = if lits.1 {
            rbig
        } else {
            self.prints_big(body, rhs)
        };
        let ints = (
            matches!(self.program.expr_ty(body, lhs).0, Ty::Int(_)),
            matches!(self.program.expr_ty(body, rhs).0, Ty::Int(_)),
        );
        match (lp, rp, ints) {
            (true, false, (_, true)) => (l, format!("BigInt({r})")),
            (false, true, (true, _)) => (format!("BigInt({l})"), r),
            _ => (l, r),
        }
    }

    /// One comparison side: literals take the decided domain, everything
    /// else keeps precedence parens.
    fn cmp_text(&self, body: &Body, e: &Expr, op: BinOp, right: bool, big: bool) -> String {
        if let Expr::Int { value, .. } = e {
            if big {
                return format!("{value}n");
            }
            return format!("{value}");
        }
        self.operand(body, e, op, right)
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "a binary op plus both sides, their type, and the wrap flag"
    )]
    fn number_op(
        &self,
        body: &Body,
        whole: &Expr,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        ty: &Ty,
        _wraps: bool,
    ) -> bool {
        if !matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Shl
                | BinOp::Shr
                | BinOp::BitAnd
                | BinOp::BitOr
                | BinOp::BitXor
        ) {
            return false;
        }
        if matches!(ty, Ty::Int(t) if t.bits() <= 32) {
            return true;
        }
        if !fits53(self.program.int_range(body, whole)) {
            return false;
        }
        if self.big_blocker(body, lhs) || self.big_blocker(body, rhs) {
            return false;
        }
        match op {
            BinOp::Shl | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                match self.program.int_range(body, whole) {
                    Some((lo, hi)) => {
                        lo >= i128::from(i32::MIN)
                            && hi <= i128::from(u32::MAX)
                            && !(lo < 0 && hi > i128::from(i32::MAX))
                    }
                    None => false,
                }
            }
            _ => true,
        }
    }

    /// An integer operand that forces `bigint`: it prints as `bigint` and
    /// its range does not fit exactly in a `number`.
    fn big_blocker(&self, body: &Body, e: &Expr) -> bool {
        if !matches!(self.program.expr_ty(body, e).0, Ty::Int(_)) {
            return false;
        }
        self.dom(body, e) == Dom::Big && !fits53(self.program.int_range(body, e))
    }

    /// An operand printed in the operation's domain: literals take the
    /// domain's suffix, crossings convert (`Number` is exact exactly when
    /// the operand fits, which `number_op` guarantees for `number`).
    fn coerced_operand(
        &self,
        body: &Body,
        e: &Expr,
        parent: BinOp,
        right: bool,
        big: bool,
    ) -> String {
        if let Expr::Int { value, .. } = e {
            // A mask or flag operand prints in hex (`0x7f`, not `127`).
            if matches!(parent, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
                return bit_int(parent, i128::from(*value), big);
            }
            if big {
                return format!("{value}n");
            }
            return format!("{value}");
        }
        if big {
            return self.big_text(body, e, parent, right);
        }
        self.num_text(body, e, Some((parent, right)))
    }

    /// `e` printed as `number`: literals plain, value-preserving widening
    /// casts unwrapped, `bigint` values via `Number` (callers guarantee the
    /// value fits). With precedence context, same-domain text keeps minimal
    /// parens; conversions are calls and need none.
    fn num_text(&self, body: &Body, e: &Expr, prec: Option<(BinOp, bool)>) -> String {
        if let Expr::Int { value, .. } = e {
            return format!("{value}");
        }
        if let Expr::Cast { expr, to } = e
            && self.widening_elide(body, expr, to)
        {
            return self.num_text(body, expr, prec);
        }
        match self.dom(body, e) {
            Dom::Num => match prec {
                Some((parent, right)) => self.operand(body, e, parent, right),
                None => self.expr(body, e),
            },
            Dom::Big => format!("Number({})", self.expr(body, e)),
        }
    }

    /// `e` printed as `bigint`: literals suffixed, `number` values via
    /// `BigInt` (always exact for integers).
    fn big_text(&self, body: &Body, e: &Expr, parent: BinOp, right: bool) -> String {
        if let Expr::Int { value, .. } = e {
            return format!("{value}n");
        }
        match self.dom(body, e) {
            Dom::Big => self.operand(body, e, parent, right),
            Dom::Num => format!("BigInt({})", self.expr(body, e)),
        }
    }

    /// A widening cast that changes no value (`int_range` of the operand
    /// fits the target) of a `number` operand: `number` contexts print the
    /// operand itself.
    fn widening_elide(&self, body: &Body, inner: &Expr, to: &Ty) -> bool {
        let Ty::Int(t) = to else {
            return false;
        };
        if self.dom(body, inner) != Dom::Num {
            return false;
        }
        match self.program.int_range(body, inner) {
            Some((lo, hi)) => t.min_value() <= lo && hi <= t.max_value(),
            None => false,
        }
    }
    /// Whether a `& | ^ <<` normalizes with `>>> 0`: its own range can
    /// exceed int32 (staying uint32), and both operands are int32-or-
    /// narrower — intermediate chain results skip it (int32-wrapping is
    /// homomorphic there, and the eventual truncating store agrees).
    fn needs_unsigned_wrap(&self, body: &Body, whole: &Expr, lhs: &Expr, rhs: &Expr) -> bool {
        if !self.int32_or_narrower(body, lhs) || !self.int32_or_narrower(body, rhs) {
            return false;
        }
        match self.program.int_range(body, whole) {
            Some((lo, hi)) => hi > i128::from(i32::MAX) && lo >= 0,
            None => false,
        }
    }

    /// An operand's range fits in int32 (so this node's wrap, if any,
    /// comes from its own operator, not a wide input).
    fn int32_or_narrower(&self, body: &Body, e: &Expr) -> bool {
        if !matches!(self.program.expr_ty(body, e).0, Ty::Int(_)) {
            return true;
        }
        match self.program.int_range(body, e) {
            Some((lo, hi)) => lo >= i128::from(i32::MIN) && hi <= i128::from(i32::MAX),
            None => false,
        }
    }

    /// Every truncating store prints its cast; domain crossings convert.
    /// The operand prints in its own domain (`Number` only ever wraps a
    /// value `int_range` proves exact), so helpers always receive the type
    /// they declare.
    fn cast(&self, body: &Body, inner: &Expr, to: &Ty) -> String {
        let (from, _) = self.program.expr_ty(body, inner);
        if Self::is_enum(self.program, &from) || Self::is_enum(self.program, to) {
            return self.expr(body, inner);
        }
        match (&from, to) {
            (Ty::Int(t1), Ty::Int(t2)) => {
                let to_big = t2.bits() > 32;
                let (v, big) = match inner {
                    Expr::Int { value, .. } => {
                        if to_big {
                            (format!("{value}n"), true)
                        } else {
                            (format!("{value}"), false)
                        }
                    }
                    _ => {
                        let big = self.dom(body, inner) == Dom::Big;
                        (self.expr(body, inner), big)
                    }
                };
                int_cast_dom(*t1, *t2, self.program, body, inner, big, &v)
            }
            (Ty::Bool, Ty::Int(t2)) => {
                let v = self.expr(body, inner);
                if t2.bits() <= 32 {
                    format!("({v} ? 1 : 0)")
                } else {
                    format!("({v} ? 1n : 0n)")
                }
            }
            (Ty::Int(_), Ty::Bool) => {
                let (v, big) = match inner {
                    Expr::Int { value, .. } => (format!("{value}"), false),
                    _ => {
                        let big = self.dom(body, inner) == Dom::Big;
                        (self.expr(body, inner), big)
                    }
                };
                if big {
                    format!("({v} !== 0n)")
                } else {
                    format!("({v} !== 0)")
                }
            }
            (Ty::Bool, Ty::Bool) => self.expr(body, inner),
            (Ty::F32 | Ty::F64, Ty::F32 | Ty::F64) => self.expr(body, inner),
            _ => self.expr(body, inner),
        }
    }

    fn is_enum(program: &Program, ty: &Ty) -> bool {
        matches!(ty, Ty::Named(t) if matches!(program.types[*t].kind, crate::ir::typed::TypeKind::Enum { .. }))
    }

    fn cast_big(&self, body: &Body, inner: &Expr, to: &Ty) -> bool {
        match inner {
            Expr::Int { .. } => matches!(to, Ty::Int(t) if t.bits() > 32),
            _ => self.dom(body, inner) == Dom::Big,
        }
    }

    /// A cast helper's name for call-style breaking.
    fn cast_head(to: IntTy) -> &'static str {
        if to.bits() <= 32 {
            return narrow_name(to);
        }
        if to == IntTy::I128 {
            return "toBigI";
        }
        if to.signed() { "toI64" } else { "toU64" }
    }

    /// A cast operand as a breakable call argument, mirroring
    /// `cast_wrap_dom` (masks, grouping, `Number` exactness).
    fn cast_operand_arg<'x>(
        &self,
        body: &'x Body,
        expr: &'x Expr,
        from: IntTy,
        to: IntTy,
        big: bool,
    ) -> CallArg<'x> {
        if from == to {
            return CallArg::E {
                pre: String::new(),
                e: expr,
                mid: String::new(),
            };
        }
        if to.bits() <= 32 {
            if !big {
                return CallArg::E {
                    pre: String::new(),
                    e: expr,
                    mid: String::new(),
                };
            }
            if fits53(self.program.int_range(body, expr)) {
                return CallArg::E {
                    pre: "Number(".to_string(),
                    e: expr,
                    mid: ")".to_string(),
                };
            }
            let mask = match to.bits() {
                8 => "0xffn",
                16 => "0xffffn",
                _ => "0xffffffffn",
            };
            if needs_group(expr, 9) {
                return CallArg::E {
                    pre: "Number((".to_string(),
                    e: expr,
                    mid: format!(") & {mask})"),
                };
            }
            return CallArg::E {
                pre: "Number(".to_string(),
                e: expr,
                mid: format!(" & {mask})"),
            };
        }
        if to == IntTy::I128 {
            if !big {
                return CallArg::E {
                    pre: "BigInt(".to_string(),
                    e: expr,
                    mid: ")".to_string(),
                };
            }
            return CallArg::E {
                pre: String::new(),
                e: expr,
                mid: String::new(),
            };
        }
        if !big {
            return CallArg::E {
                pre: "BigInt(".to_string(),
                e: expr,
                mid: ")".to_string(),
            };
        }
        CallArg::E {
            pre: String::new(),
            e: expr,
            mid: String::new(),
        }
    }

    fn call(&self, body: &Body, func: Func, args: &[Expr]) -> String {
        match func {
            Func::SignExtend => self.sign_extend(body, args),
            Func::Utf16Units => format!("utf16Units({})", self.expr(body, &args[0])),
            Func::Utf8Len => format!("utf8Len({})", self.expr(body, &args[0])),
            Func::StrLen => format!("Array.from({}).length", self.expr(body, &args[0])),
            Func::BytesLen | Func::ListLen => {
                format!("{}.length", self.expr(body, &args[0]))
            }
            Func::Min | Func::Max => {
                let l = self.expr(body, &args[0]);
                let r = self.expr(body, &args[1]);
                if self.dom(body, &args[0]) == Dom::Big {
                    let cmp = if func == Func::Min { "<" } else { ">" };
                    format!("({l} {cmp} {r} ? {l} : {r})")
                } else {
                    let name = if func == Func::Min {
                        "Math.min"
                    } else {
                        "Math.max"
                    };
                    format!("{name}({l}, {r})")
                }
            }
        }
    }

    /// `sign_extend` in `number` when the result fits (small literal width
    /// and an exactly-convertible operand), else the `bigint` version.
    fn sign_extend(&self, body: &Body, args: &[Expr]) -> String {
        let bits = self.expr_num(body, &args[1]);
        if self.sign_extend_num(body, args) {
            return format!("signExtend({}, {bits})", self.num_operand(body, &args[0]));
        }
        format!(
            "signExtendBigint({}, {bits})",
            self.big_operand(body, &args[0])
        )
    }
    /// Whether `sign_extend` prints in `number`: small literal width and an
    /// exactly-convertible operand.
    fn sign_extend_num(&self, body: &Body, args: &[Expr]) -> bool {
        matches!(&args[1], Expr::Int { value, .. } if (1..=53).contains(value))
            && !self.big_blocker(body, &args[0])
    }
    /// A ternary branch in the condition's domain; matching domains print
    /// exactly as before, crossings convert (`Number` is exact because a
    /// branch of a `number` condition fits its proven range).
    fn cond_branch(&self, body: &Body, e: &Expr, big: bool) -> String {
        if !matches!(self.program.expr_ty(body, e).0, Ty::Int(_)) {
            return self.expr(body, e);
        }
        if let Expr::Int { value, .. } = e {
            if big {
                return format!("{value}n");
            }
            return format!("{value}");
        }
        match (big, self.dom(body, e)) {
            (true, Dom::Num) => format!("BigInt({})", self.expr(body, e)),
            (false, Dom::Big) => self.num_text(body, e, None),
            _ => self.expr(body, e),
        }
    }

    /// Whether an expression prints as `bigint`: literals by class,
    /// everything else by domain.
    fn prints_big(&self, body: &Body, e: &Expr) -> bool {
        if let Expr::Int { ty, .. } = e {
            return ty.bits() > 32;
        }
        self.dom(body, e) == Dom::Big
    }

    /// An operand printed as `number` in a call: `num_text` without
    /// precedence context.
    fn num_operand(&self, body: &Body, e: &Expr) -> String {
        self.num_text(body, e, None)
    }

    /// An operand printed as `bigint`: literals suffixed, `number` values
    /// via `BigInt` (always exact for integers).
    fn big_operand(&self, body: &Body, e: &Expr) -> String {
        if let Expr::Int { value, .. } = e {
            return format!("{value}n");
        }
        match self.dom(body, e) {
            Dom::Big => self.expr(body, e),
            Dom::Num => format!("BigInt({})", self.expr(body, e)),
        }
    }
    /// An expression in a `number` position (counts, byte lengths, tag and
    /// integer-parameter arguments): widening casts unwrap, other `bigint`
    /// values convert with `Number`.
    fn expr_num(&self, body: &Body, e: &Expr) -> String {
        if let Expr::Int { value, .. } = e {
            return format!("{value}");
        }
        if self.dom(body, e) == Dom::Num {
            return self.expr(body, e);
        }
        if let Expr::Cast {
            expr,
            to: Ty::Int(IntTy::I64 | IntTy::U64),
        } = e
            && self.dom(body, expr) == Dom::Num
        {
            return self.expr(body, expr);
        }
        format!("Number({})", self.expr(body, e))
    }

    /// A `number`-position expression as a breakable call argument,
    /// mirroring `expr_num` (literals plain, widenings unwrapped,
    /// `bigint` via `Number`).
    fn num_call_arg<'x>(&self, body: &'x Body, e: &'x Expr) -> CallArg<'x> {
        if let Expr::Int { value, .. } = e {
            return CallArg::Raw(format!("{value}"));
        }
        if self.dom(body, e) == Dom::Num {
            return CallArg::E {
                pre: String::new(),
                e,
                mid: String::new(),
            };
        }
        if let Expr::Cast {
            expr,
            to: Ty::Int(IntTy::I64 | IntTy::U64),
        } = e
            && self.dom(body, expr) == Dom::Num
        {
            return CallArg::E {
                pre: String::new(),
                e: expr,
                mid: String::new(),
            };
        }
        CallArg::E {
            pre: "Number(".to_string(),
            e,
            mid: ")".to_string(),
        }
    }

    /// A call argument converted to its parameter's domain.
    fn arg(&self, body: &Body, num_target: bool, e: &Expr) -> String {
        if let Expr::Int { value, .. } = e
            && num_target
        {
            return format!("{value}");
        }
        match (num_target, self.dom(body, e)) {
            (true, Dom::Num) | (false, Dom::Big) => self.expr(body, e),
            (true, Dom::Big) => self.expr_num(body, e),
            (false, Dom::Num) => format!("BigInt({})", self.expr(body, e)),
        }
    }

    /// A record or sum value literal (constructs and variants print at
    /// `Return` and anywhere an expression needs them).
    fn value(&self, body: &Body, e: &Expr) -> String {
        braced(&self.value_parts(body, e))
    }

    /// The comma-separated parts of a value literal, for width-checked layout.
    fn value_parts(&self, body: &Body, e: &Expr) -> Vec<String> {
        match e {
            Expr::Construct { ty, fields } => {
                let mut parts = Vec::new();
                if let Origin::Variant { sum, variant } = &self.program.types[*ty].origin {
                    let kind = match &self.program.types[*sum].kind {
                        crate::ir::typed::TypeKind::Sum { variants } => {
                            variants[*variant].name.snake()
                        }
                        _ => "other".to_string(),
                    };
                    parts.push(format!("kind: \"{kind}\""));
                }
                for (f, e) in fields.iter().enumerate() {
                    // `{ protocolVersion }`, not
                    // `{ protocolVersion: protocolVersion }`: both names go
                    // through the same case transform, so equal text means
                    // the value is that local.
                    let name = field_ident(self.program, *ty, f);
                    let v = self.expr(body, e);
                    if v == name {
                        parts.push(name);
                    } else {
                        parts.push(format!("{name}: {v}"));
                    }
                }
                parts
            }
            Expr::Variant {
                ty,
                variant,
                payload,
            } => {
                let decl = &self.program.variants(*ty)[*variant];
                if decl.tags.is_empty() {
                    let tag = self.tag_param.clone().unwrap_or_else(|| "tag".to_string());
                    let mut parts = vec!["kind: \"other\"".to_string(), format!("tag: {tag}")];
                    if let Some(p) = payload {
                        parts.push(format!("value: {}", self.expr(body, p)));
                    }
                    return parts;
                }
                let kind = decl.name.snake();
                match payload {
                    None => vec![format!("kind: \"{kind}\"")],
                    Some(p) => {
                        if self.is_flat_payload(*ty, *variant) {
                            // The spread arm record carries its own
                            // `kind` discriminant; repeating it errors
                            // under `tsc --strict` (TS2783).
                            vec![format!("...{}", self.expr(body, p))]
                        } else {
                            vec![
                                format!("kind: \"{kind}\""),
                                format!("value: {}", self.expr(body, p)),
                            ]
                        }
                    }
                }
            }
            _ => vec![self.expr(body, e)],
        }
    }

    fn is_flat_payload(&self, ty: TypeIdx, variant: usize) -> bool {
        match &self.program.variants(ty)[variant].payload {
            Some(Ty::Named(t)) => {
                matches!(
                    (&self.program.types[*t].kind, &self.program.types[*t].origin),
                    (
                        crate::ir::typed::TypeKind::Record { .. },
                        Origin::Variant { .. }
                    )
                )
            }
            _ => false,
        }
    }
}

/// An integer-to-integer cast: `v` prints in domain `big`. Narrowing a
/// `bigint` masks in `bigint` first unless `int_range` proves `Number()`
/// exact; widening a `number` extends through `BigInt`.
fn int_cast_dom(
    from: IntTy,
    to: IntTy,
    program: &Program,
    body: &Body,
    inner: &Expr,
    big: bool,
    v: &str,
) -> String {
    if from == to {
        return v.to_string();
    }
    let (pre, suf) = cast_wrap_dom(from, to, program, body, inner, big);
    format!("{pre}{v}{suf}")
}

/// A narrowing helper name for a 32-bit-or-narrower class.
fn narrow_name(to: IntTy) -> &'static str {
    match to {
        IntTy::I8 => "toI8",
        IntTy::I16 => "toI16",
        IntTy::I32 => "toI32",
        IntTy::U8 => "toU8",
        IntTy::U16 => "toU16",
        IntTy::U32 => "toU32",
        _ => "toI32",
    }
}
/// Whether an operand needs parentheses inside an operator of JS precedence
/// `parent`: a lower-precedence binary or any ternary does.
fn needs_group(e: &Expr, parent: u8) -> bool {
    match e {
        Expr::Binary { op, .. } => FnGen::prec(*op) < parent,
        Expr::Cond { .. } => true,
        _ => false,
    }
}
/// An integer literal that is a direct operand of `& | ^`: hexadecimal when
/// [`hex_literal`] says so (a mask or flag reads as `0x7f`, not `127`).
/// Wider-than-64 values, which `hex_literal` cannot take, follow the same
/// `> 9` rule; `big` selects the `n` suffix.
fn bit_int(op: BinOp, value: i128, big: bool) -> String {
    let hex = match i64::try_from(value) {
        Ok(v) => hex_literal(op, v),
        Err(_) => value > 9,
    };
    if hex {
        if big {
            format!("0x{value:x}n")
        } else {
            format!("0x{value:x}")
        }
    } else if big {
        format!("{value}n")
    } else {
        format!("{value}")
    }
}
/// Whether a binary child needs parentheses under `parent` (`right` selects
/// the right-hand side), matching prettier exactly: minimal precedence
/// parens plus its clarifying set, verified operator-pair by operator-pair.
fn child_paren(parent: BinOp, child: BinOp, right: bool) -> bool {
    use BinOp::{
        Add, And, BitAnd, BitOr, BitXor, Div, Eq, Ge, Gt, Le, Lt, Mod, Mul, Ne, Or, Shl, Shr, Sub,
    };
    if child == parent {
        return match parent {
            BitAnd | BitOr | BitXor | And | Or => false,
            Add | Mul | Sub | Div | Lt | Le | Gt | Ge => right,
            Mod | Shl | Shr | Eq | Ne => true,
        };
    }
    let (pp, cp) = (FnGen::prec(parent), FnGen::prec(child));
    if cp < pp {
        return true;
    }
    if cp > pp {
        return matches!(parent, BitOr | BitXor | BitAnd | Shl | Shr)
            || (parent == Or && child == And);
    }
    match parent {
        Add | Sub => false,
        Mul | Div | Mod | Shl | Shr | Eq | Ne | BitAnd | BitOr | BitXor => true,
        Lt | Le | Gt | Ge => right,
        And | Or => true,
    }
}
/// Split an integer cast into the text around its operand, so long operands
/// can break across lines inside the wrapper. `big` says the operand prints
/// as `bigint` (literals already take the helper's domain before this).
fn cast_wrap_dom(
    from: IntTy,
    to: IntTy,
    program: &Program,
    body: &Body,
    inner: &Expr,
    big: bool,
) -> (String, String) {
    if from == to {
        return (String::new(), String::new());
    }
    if to.bits() <= 32 {
        let f = narrow_name(to);
        if big {
            if fits53(program.int_range(body, inner)) {
                return (format!("{f}(Number("), "))".to_string());
            }
            let mask = match to.bits() {
                8 => "0xffn",
                16 => "0xffffn",
                _ => "0xffffffffn",
            };
            // `&` binds looser than arithmetic: group a compound operand.
            if needs_group(inner, 9) {
                return (format!("{f}(Number(("), format!(") & {mask}))"));
            }
            return (format!("{f}(Number("), format!(" & {mask}))"));
        }
        return (format!("{f}("), ")".to_string());
    }
    if to == IntTy::I128 {
        if !big {
            return ("toBigI(BigInt(".to_string(), "), 128)".to_string());
        }
        return ("toBigI(".to_string(), ", 128)".to_string());
    }
    let f = if to.signed() { "toI64" } else { "toU64" };
    if !big {
        (format!("{f}(BigInt("), "))".to_string())
    } else {
        (format!("{f}("), ")".to_string())
    }
}
impl<'a> Printer<'a> {
    /// The decode and encode functions of one codec.
    fn codec_block(&self, c: CodecIdx) -> String {
        let mut out = String::new();
        let codec = &self.program.codecs[c];
        let doc = origin_doc(self.program, &codec.origin);
        let ret = ts_ty(self.program, &codec.value);
        for decode in [true, false] {
            let body = if decode { &codec.decode } else { &codec.encode };
            let mut num_like = vec![false; body.locals.len()];
            for (i, lid) in body.params.iter().enumerate() {
                if matches!(codec.params[i].role, ParamRole::Arg | ParamRole::Tag) {
                    num_like[*lid] = true;
                }
            }
            let tag_param = codec
                .params
                .iter()
                .zip(body.params.iter())
                .find(|(p, _)| matches!(p.role, ParamRole::Tag))
                .map(|(_, lid)| camel(&body.locals[*lid].name));
            let (const_local, stored_local) = const_locals(body);
            let cg = FnGen {
                program: self.program,
                codec: codec.name.snake(),
                decode,
                num_like,
                const_local,
                stored_local,
                tag_param,
            };
            let name = if decode {
                decode_fn(self.program, c)
            } else {
                encode_fn(self.program, c)
            };
            out.push_str(&doc);
            out.push('\n');
            // Signature parameters: the reader/writer (and the value under
            // encode) first, then schema, tag, and capture parameters.
            let mut params = Vec::new();
            if decode {
                params.push("r: Reader".to_string());
            } else {
                params.push("w: Writer".to_string());
                let v = body
                    .value
                    .map(|id| camel(&body.locals[id].name))
                    .unwrap_or_else(|| "v".to_string());
                params.push(format!("{v}: {ret}"));
            }
            for (i, lid) in body.params.iter().enumerate() {
                let p = &codec.params[i];
                let ty = if matches!(p.role, ParamRole::Arg | ParamRole::Tag) {
                    "number".to_string()
                } else {
                    ts_ty(self.program, &p.ty)
                };
                let opt = if p.optional { " | undefined" } else { "" };
                params.push(format!("{}: {ty}{opt}", camel(&body.locals[*lid].name)));
            }
            let empty = (if decode {
                &codec.decode.stmts
            } else {
                &codec.encode.stmts
            })
            .is_empty();
            // Empty bodies stay on one line, matching prettier.
            let (head, tail) = if decode {
                (
                    format!("export function {name}"),
                    if empty {
                        format!(": {ret} {{}}")
                    } else {
                        format!(": {ret} {{")
                    },
                )
            } else if empty {
                (format!("export function {name}"), ": void {}".to_string())
            } else {
                (format!("export function {name}"), ": void {".to_string())
            };
            cg.signature(&head, &params, &tail, &mut out);
            if empty {
                out.push('\n');
                continue;
            }
            cg.stmts(
                body,
                if decode {
                    &codec.decode.stmts
                } else {
                    &codec.encode.stmts
                },
                1,
                &mut out,
            );
            out.push_str("}\n\n");
        }
        out
    }
}

impl<'a> FnGen<'a> {
    fn line(&self, ind: usize, text: &str, out: &mut String) {
        out.push_str(&"  ".repeat(ind));
        out.push_str(text);
        out.push('\n');
    }

    fn signature(&self, head: &str, params: &[String], tail: &str, out: &mut String) {
        let single = format!("{head}({}){tail}", params.join(", "));
        if single.len() <= 80 {
            out.push_str(&single);
            out.push('\n');
            return;
        }
        out.push_str(&format!("{head}(\n"));
        for p in params {
            out.push_str(&format!("  {p},\n"));
        }
        out.push_str(&format!("){tail}\n"));
    }

    fn stmts(&self, body: &Body, stmts: &[Stmt], ind: usize, out: &mut String) {
        for s in stmts {
            self.stmt(body, s, ind, out);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn stmt(&self, body: &Body, s: &Stmt, ind: usize, out: &mut String) {
        match s {
            Stmt::Declare { local, zero } => {
                let loc = &body.locals[*local];
                let n = self.local_name(body, *local);
                if loc.optional {
                    self.line(
                        ind,
                        &format!("let {n}: {} | undefined;", ts_ty(self.program, &loc.ty)),
                        out,
                    );
                } else if let Ty::List(t) = &loc.ty {
                    // Only `.push` mutates the list, so `const` — unless a
                    // `Bind` store targets it (then `let`, defensively).
                    let kw = if self.stored_local[*local] {
                        "let"
                    } else {
                        "const"
                    };
                    self.line(
                        ind,
                        &format!("{kw} {n}: {}[] = [];", ts_ty(self.program, t)),
                        out,
                    );
                } else if *zero && !matches!(loc.ty, Ty::Named(_)) {
                    self.line(ind, &format!("let {n} = {};", zero_str(&loc.ty)), out);
                } else {
                    self.line(
                        ind,
                        &format!("let {n}: {};", ts_ty(self.program, &loc.ty)),
                        out,
                    );
                }
            }
            Stmt::Let { dst, value } => {
                let head = self.decl_head(body, dst);
                self.assign(body, &head, value, ind, out);
            }
            Stmt::Read { dst, op } => {
                let call = match op {
                    ReadOp::U8 => format!("r.readU8(\"{}\")", self.codec),
                    ReadOp::F32 => format!("r.readF32(\"{}\")", self.codec),
                    ReadOp::F64 => format!("r.readF64(\"{}\")", self.codec),
                    ReadOp::F32Le => format!("r.readF32Le(\"{}\")", self.codec),
                    ReadOp::F64Le => format!("r.readF64Le(\"{}\")", self.codec),
                    ReadOp::Bytes(n) => {
                        let bind = self.bind_head(body, dst);
                        let single = format!(
                            "{bind}r.readBytes(\"{}\", {});",
                            self.codec,
                            self.expr_num(body, n)
                        );
                        self.stmt_call(
                            body,
                            &format!("{bind}r.readBytes"),
                            &[
                                CallArg::Raw(format!("\"{}\"", self.codec)),
                                self.num_call_arg(body, n),
                            ],
                            ";",
                            ind,
                            out,
                            &single,
                        );
                        return;
                    }
                    ReadOp::Rest => "r.readRest()".to_string(),
                };
                self.line(ind, &format!("{}{call};", self.bind_head(body, dst)), out);
            }
            Stmt::Decode { dst, codec, args } => {
                let target = &self.program.codecs[*codec];
                let call = format!(
                    "{}({}{})",
                    decode_fn(self.program, *codec),
                    "r",
                    self.call_args(body, target, args)
                );
                self.assign_break(&self.bind_head(body, dst), &call, ind, out);
            }
            Stmt::Utf8 { dst, src } => {
                let b = self.expr(body, src);
                let call = format!("decodeUtf8({b}, \"{}\", r.pos - {b}.length)", self.codec);
                self.assign_break(&self.bind_head(body, dst), &call, ind, out);
            }
            Stmt::Write { op, value } => {
                if let WriteOp::Bytes(n) = op {
                    let v = self.expr(body, value);
                    let single = format!(
                        "w.writeBytesFixed(\"{}\", {v}, {});",
                        self.codec,
                        self.expr_num(body, n)
                    );
                    self.stmt_call(
                        body,
                        "w.writeBytesFixed",
                        &[
                            CallArg::Raw(format!("\"{}\"", self.codec)),
                            CallArg::E {
                                pre: String::new(),
                                e: value,
                                mid: String::new(),
                            },
                            self.num_call_arg(body, n),
                        ],
                        ";",
                        ind,
                        out,
                        &single,
                    );
                    return;
                }
                if let (WriteOp::U8, Some(byte)) = (op, byte_literal(value, false)) {
                    self.line(ind, &format!("w.writeU8({byte});"), out);
                    return;
                }
                let (open, close) = match op {
                    WriteOp::U8 => ("w.writeU8(", ");"),
                    WriteOp::F32 => ("w.writeF32(", ");"),
                    WriteOp::F64 => ("w.writeF64(", ");"),
                    WriteOp::F32Le => ("w.writeF32Le(", ");"),
                    WriteOp::F64Le => ("w.writeF64Le(", ");"),
                    WriteOp::Bytes(_) => unreachable!("handled above"),
                    WriteOp::Rest => ("w.writeBytes(", ");"),
                };
                self.lay(body, open, value, close, ind, out);
            }
            Stmt::Encode { codec, args, value } => {
                let target = &self.program.codecs[*codec];
                let name = encode_fn(self.program, *codec);
                let single = format!(
                    "{name}(w, {}{});",
                    self.expr(body, value),
                    self.call_args(body, target, args)
                );
                let mut cargs = vec![
                    CallArg::Raw("w".to_string()),
                    CallArg::E {
                        pre: String::new(),
                        e: value,
                        mid: String::new(),
                    },
                ];
                for (i, a) in args.iter().enumerate() {
                    cargs.push(CallArg::Raw(self.param_arg(body, &target.params[i], a)));
                }
                self.stmt_call(body, &name, &cargs, ";", ind, out, &single);
            }
            Stmt::Require { cond, text } => {
                self.lay_require(body, cond, text, ind, out);
            }
            Stmt::Unwrap { dst, src, field } => {
                let n = self.local_name(body, dst.local);
                let head = self.decl_head(body, dst);
                self.line(ind, &format!("{head}{};", self.expr(body, src)), out);
                let guard = format!("if ({n} === undefined)");
                let fail = format!(
                    "missingField(\"{}\", {}, \"{field}\");",
                    self.codec,
                    self.offset()
                );
                if ind * 2 + guard.len() + 1 + fail.len() <= 80 {
                    self.line(ind, &format!("{guard} {fail}"), out);
                } else {
                    self.line(ind, &guard, out);
                    self.stmt_call(
                        body,
                        "missingField",
                        &[
                            CallArg::Raw(format!("\"{}\"", self.codec)),
                            CallArg::Raw(self.offset().to_string()),
                            CallArg::Raw(format!("\"{field}\"")),
                        ],
                        ";",
                        ind + 1,
                        out,
                        &fail,
                    );
                }
            }
            Stmt::If { cond, then, els } => {
                let single = format!("if ({}) {{", self.expr(body, cond));
                if ind * 2 + single.len() <= 80 {
                    self.line(ind, &single, out);
                } else {
                    // Prettier breaks the condition group: `if (`, one
                    // operand per line, `) {`.
                    self.line(ind, "if (", out);
                    let mut parts: Vec<(usize, String)> = Vec::new();
                    self.lay_parts(body, "", cond, ind + 1, 0, &mut parts);
                    for (li, l) in parts {
                        self.line(li, &l, out);
                    }
                    self.line(ind, ") {", out);
                }
                self.stmts(body, then, ind + 1, out);
                if els.is_empty() {
                    self.line(ind, "}", out);
                } else {
                    self.line(ind, "} else {", out);
                    self.stmts(body, els, ind + 1, out);
                    self.line(ind, "}", out);
                }
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => {
                self.match_stmt(body, scrutinee, arms, other, ind, out);
            }
            Stmt::Repeat {
                mode,
                list,
                item,
                body: inner,
            } => {
                let names = (self.local_name(body, *list), self.local_name(body, *item));
                self.repeat(body, mode, *list, names, inner, ind, out);
            }
            Stmt::ForEach {
                list,
                item,
                body: inner,
            } => {
                let item = self.local_name(body, *item);
                self.line(
                    ind,
                    &format!("for (const {item} of {}) {{", self.expr(body, list)),
                    out,
                );
                self.stmts(body, inner, ind + 1, out);
                self.line(ind, "}", out);
            }
            Stmt::ExpectVariant {
                dst,
                sum,
                ty,
                variant,
            } => {
                self.expect_variant(body, dst, sum, (*ty, *variant), ind, out);
            }
            Stmt::Return(e) => match e {
                Expr::Construct { .. } | Expr::Variant { .. } => {
                    let parts = self.value_parts(body, e);
                    let single = format!("return {};", braced(&parts));
                    if ind * 2 + single.len() <= 80 {
                        self.line(ind, &single, out);
                    } else {
                        self.line(ind, "return {", out);
                        for p in &parts {
                            self.line(ind + 1, &format!("{p},"), out);
                        }
                        self.line(ind, "};", out);
                    }
                }
                _ => self.lay(body, "return ", e, ";", ind, out),
            },
            Stmt::Fail(_) => {
                self.line(
                    ind,
                    &format!("untilEndEncode(\"{}\", {});", self.codec, self.offset()),
                    out,
                );
            }
        }
    }

    fn bind_head(&self, body: &Body, dst: &crate::ir::typed::Bind) -> String {
        self.decl_head(body, dst)
    }

    /// `let` or `const` for a store: `const` when the binding introduces a
    /// local that is never stored again (see `const_locals`).
    fn decl_head(&self, body: &Body, dst: &crate::ir::typed::Bind) -> String {
        let n = self.local_name(body, dst.local);
        if dst.declare && self.const_local[dst.local] {
            format!("const {n} = ")
        } else if dst.declare {
            format!("let {n} = ")
        } else {
            format!("{n} = ")
        }
    }

    fn call_args(&self, body: &Body, target: &crate::ir::typed::Codec, args: &[Expr]) -> String {
        let mut out = String::new();
        for (i, a) in args.iter().enumerate() {
            out.push_str(&format!(", {}", self.param_arg(body, &target.params[i], a)));
        }
        out
    }

    fn param_arg(&self, body: &Body, param: &crate::ir::typed::Param, e: &Expr) -> String {
        let num_target = matches!(param.role, ParamRole::Arg | ParamRole::Tag)
            || matches!(&param.ty, Ty::Int(t) if t.bits() <= 32);
        self.arg(body, num_target, e)
    }

    fn assign(&self, body: &Body, head: &str, e: &Expr, ind: usize, out: &mut String) {
        match e {
            Expr::Construct { .. } | Expr::Variant { .. } => {
                let parts = self.value_parts(body, e);
                let single = format!("{head}{};", braced(&parts));
                if ind * 2 + single.len() <= 80 {
                    self.line(ind, &single, out);
                } else {
                    self.line(ind, &format!("{head}{{",), out);
                    for p in &parts {
                        self.line(ind + 1, &format!("{p},"), out);
                    }
                    self.line(ind, "};", out);
                }
            }
            _ => self.lay(body, head, e, ";", ind, out),
        }
    }

    fn lay_require(&self, body: &Body, cond: &Expr, text: &str, ind: usize, out: &mut String) {
        // Fail when the negation holds (`protocolVersion !== 777`, not
        // `!(protocolVersion == 777)`); the detail keeps the schema text.
        let neg = self.program.negated(body, cond);
        let detail = ts_string(text);
        let single = format!(
            "if ({}) requireFailed(\"{}\", {}, {detail});",
            self.expr(body, &neg),
            self.codec,
            self.offset(),
        );
        if ind * 2 + single.len() <= 80 {
            self.line(ind, &single, out);
            return;
        }
        // Split the statement; only a pathological condition breaks further.
        let head = format!("if ({})", self.expr(body, &neg));
        let fail = format!(
            "requireFailed(\"{}\", {}, {detail});",
            self.codec,
            self.offset(),
        );
        // `single` did not fit, so split the statement.
        if ind * 2 + head.len() <= 80 {
            self.line(ind, &head, out);
            self.stmt_call(
                body,
                "requireFailed",
                &[
                    CallArg::Raw(format!("\"{}\"", self.codec)),
                    CallArg::Raw(self.offset().to_string()),
                    CallArg::Raw(detail.clone()),
                ],
                ";",
                ind + 1,
                out,
                &fail,
            );
            return;
        }
        // Break the condition, keeping the `requireFailed` on the last line.
        let mut lines: Vec<(usize, String)> = Vec::new();
        let trail = format!(
            ") requireFailed(\"{}\", {}, {detail});",
            self.codec,
            self.offset(),
        );
        self.lay_parts(body, "if (", &neg, ind, trail.len(), &mut lines);
        let (li, last) = lines.pop().unwrap_or((ind, String::new()));
        for (li, l) in lines {
            self.line(li, &l, out);
        }
        self.line(
            li,
            &format!(
                "{last}) requireFailed(\"{}\", {}, {detail});",
                self.codec,
                self.offset(),
            ),
            out,
        );
    }
}

/// Per local: introduce with `const`. True when the local is stored exactly
/// once, by an introducing store (`Bind.declare`), with no `Declare` and no
/// second store anywhere in the body (including `if`/`switch`/loop nests).
/// Reassigned byte cursors (`let b0 = 0` + later stores), multi-arm
/// destinations, and optional pre-declarations stay `let`. The second
/// vector marks locals with any `Bind` store at all, so list
/// pre-declarations (mutated only through `.push`) can still print `const`.
fn const_locals(body: &Body) -> (Vec<bool>, Vec<bool>) {
    fn store(stores: &mut [(usize, usize)], dst: &crate::ir::typed::Bind) {
        let e = &mut stores[dst.local];
        if dst.declare {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }
    fn stmts(list: &[Stmt], stores: &mut [(usize, usize)], declared: &mut [bool]) {
        for s in list {
            match s {
                Stmt::Declare { local, .. } => declared[*local] = true,
                Stmt::Let { dst, .. }
                | Stmt::Read { dst, .. }
                | Stmt::Decode { dst, .. }
                | Stmt::Utf8 { dst, .. }
                | Stmt::Unwrap { dst, .. } => store(stores, dst),
                Stmt::ExpectVariant { dst: Some(dst), .. } => store(stores, dst),
                Stmt::If { then, els, .. } => {
                    stmts(then, stores, declared);
                    stmts(els, stores, declared);
                }
                Stmt::Match { arms, other, .. } => {
                    for a in arms {
                        stmts(&a.body, stores, declared);
                    }
                    if let Some(other) = other {
                        stmts(other, stores, declared);
                    }
                }
                Stmt::Repeat { body: inner, .. } | Stmt::ForEach { body: inner, .. } => {
                    stmts(inner, stores, declared);
                }
                _ => {}
            }
        }
    }
    let mut stores = vec![(0usize, 0usize); body.locals.len()];
    let mut declared = vec![false; body.locals.len()];
    stmts(&body.stmts, &mut stores, &mut declared);
    let is_const = stores
        .iter()
        .zip(declared.iter())
        .map(|((intro, again), decl)| *intro == 1 && *again == 0 && !decl)
        .collect::<Vec<_>>();
    let stored = stores
        .iter()
        .map(|(intro, again)| *intro + *again > 0)
        .collect::<Vec<_>>();
    (is_const, stored)
}

/// The zero value of a type for `Declare`.
fn zero_str(ty: &Ty) -> &'static str {
    match ty {
        Ty::Int(t) => {
            if t.bits() <= 32 {
                "0"
            } else {
                "0n"
            }
        }
        Ty::Bool => "false",
        Ty::F32 | Ty::F64 => "0",
        Ty::Str => "\"\"",
        Ty::Bytes => "new Uint8Array(0)",
        Ty::List(_) | Ty::Named(_) => "0",
    }
}
/// Associative operators whose chains can break across lines.
fn is_assoc(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Add
            | BinOp::Mul
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::And
            | BinOp::Or
    )
}

/// Flatten `p1 op p2 op ...` (same associative operator, either spine).
fn split_assoc<'b>(e: &'b Expr, op: BinOp, out: &mut Vec<&'b Expr>) {
    if let Expr::Binary {
        op: op2, lhs, rhs, ..
    } = e
        && *op2 == op
    {
        split_assoc(lhs, op, out);
        split_assoc(rhs, op, out);
        return;
    }
    out.push(e);
}

impl<'a> FnGen<'a> {
    fn chain_operand<'x>(&self, body: &'x Body, p: &'x Expr, chain_big: bool) -> &'x Expr {
        match p {
            Expr::Cast { expr: inner, to }
                if !chain_big && self.widening_elide(body, inner, to) =>
            {
                inner
            }
            _ => p,
        }
    }

    /// One associative chain part, matching the per-part loop's `plain` /
    /// `Number(` / `BigInt(` rendering.
    fn chain_part_text(
        &self,
        body: &Body,
        q: &Expr,
        op: BinOp,
        chain_big: bool,
        nowrap: bool,
        right: bool,
    ) -> String {
        let part_big = self.prints_big(body, q);
        let (open, close, plain) = match (chain_big, part_big) {
            (true, false) => ("BigInt(", ")", false),
            (false, true) if fits53(self.program.int_range(body, q)) => ("Number(", ")", false),
            _ => ("", "", true),
        };
        let t = if nowrap {
            self.expr_nowrap(body, q)
        } else {
            self.expr(body, q)
        };
        if plain {
            let (po, pc) = if self.chain_paren(body, q, op, right) {
                ("(", ")")
            } else {
                ("", "")
            };
            format!("{po}{}{pc}", t)
        } else {
            format!("{open}{}{close}", t)
        }
    }

    /// Elided associative chain text when every part prints like the per-part
    /// loop (parentheses and wrappers). Returns `None` for mask-literal chains.
    fn assoc_flatten(
        &self,
        body: &Body,
        parts: &[&Expr],
        op: BinOp,
        chain_big: bool,
        nowrap: bool,
    ) -> Option<String> {
        if matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor) {
            for p in parts {
                let q = self.chain_operand(body, p, chain_big);
                if matches!(q, Expr::Int { .. }) {
                    return None;
                }
            }
        }
        let sym = op.symbol();
        let mut flat = String::new();
        for (i, p) in parts.iter().enumerate() {
            let q = self.chain_operand(body, p, chain_big);
            if i > 0 {
                flat.push(' ');
                flat.push_str(sym);
                flat.push(' ');
            }
            flat.push_str(&self.chain_part_text(body, q, op, chain_big, nowrap, i > 0));
        }
        Some(flat)
    }

    /// Whether a chain part needs parentheses inside a chain of `op`: a
    /// call-shaped part (`toI64(…)`, `floorDiv(…)`) never does, matching
    /// prettier, which strips such parens.
    fn chain_paren(&self, body: &Body, p: &Expr, op: BinOp, right: bool) -> bool {
        match p {
            Expr::Binary {
                op: inner,
                lhs,
                rhs,
                ty,
                wraps,
            } => {
                if self.binary_is_call(body, p, *inner, lhs, rhs, ty, *wraps) {
                    return false;
                }
                child_paren(op, *inner, right)
            }
            Expr::Cond { .. } => true,
            _ => false,
        }
    }

    /// Lay out `prefix + E + suffix` at indent `ind`, breaking long
    /// associative chains and call arguments across lines.
    fn lay(&self, body: &Body, prefix: &str, e: &Expr, suffix: &str, ind: usize, out: &mut String) {
        let mut lines: Vec<(usize, String)> = Vec::new();
        self.lay_parts(body, prefix, e, ind, suffix.len(), &mut lines);
        let last = lines.len() - 1;
        for (i, (li, l)) in lines.iter().enumerate() {
            if i == last {
                self.line(*li, &format!("{l}{suffix}"), out);
            } else {
                self.line(*li, l, out);
            }
        }
    }

    /// The lines of `prefix + E` (the caller appends any suffix to the last).
    fn lay_parts(
        &self,
        body: &Body,
        prefix: &str,
        e: &Expr,
        ind: usize,
        last_trail: usize,
        lines: &mut Vec<(usize, String)>,
    ) {
        self.lay_inner(body, prefix, e, ind, last_trail, lines, false);
    }

    /// `lay_parts` for an operand whose parent already coerces: this node's
    /// `>>> 0` normalize prints as a no-op-free operator (one level only).
    fn lay_parts_nowrap(
        &self,
        body: &Body,
        prefix: &str,
        e: &Expr,
        ind: usize,
        last_trail: usize,
        lines: &mut Vec<(usize, String)>,
    ) {
        self.lay_inner(body, prefix, e, ind, last_trail, lines, true);
    }

    /// The lines of `prefix + E` with optional `>>> 0` suppression (the
    /// caller appends any suffix to the last).
    #[allow(clippy::too_many_arguments)]
    fn lay_inner(
        &self,
        body: &Body,
        prefix: &str,
        e: &Expr,
        ind: usize,
        last_trail: usize,
        lines: &mut Vec<(usize, String)>,
        nowrap: bool,
    ) {
        let single = if nowrap {
            format!("{prefix}{}", self.expr_nowrap(body, e))
        } else {
            format!("{prefix}{}", self.expr(body, e))
        };
        if ts_fits_at(ind, single.len(), last_trail) {
            lines.push((ind, single));
            return;
        }
        match e {
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps,
            } if is_assoc(*op) => {
                let chain_big = !self.number_op(body, e, *op, lhs, rhs, ty, *wraps);
                let mut parts = Vec::new();
                split_assoc(e, *op, &mut parts);
                let sym = op.symbol();
                let n = parts.len();
                if let Some(flat) = self.assoc_flatten(body, &parts, *op, chain_big, nowrap)
                    && ts_fits_at(ind, prefix.len() + flat.len(), last_trail)
                {
                    lines.push((ind, format!("{prefix}{flat}")));
                    return;
                }
                for (i, p) in parts.into_iter().enumerate() {
                    let q = self.chain_operand(body, p, chain_big);
                    let pre = if i == 0 {
                        prefix.to_string()
                    } else {
                        String::new()
                    };
                    let li = if i == 0 { ind } else { ind + 1 };
                    // Parts print in the chain's domain: wrap crossings
                    // (`Number` is exact exactly when the part fits, which a
                    // `number` chain guarantees for every part but a huge
                    // one — those never occur and are left for `tsc`).
                    let part_big = self.prints_big(body, q);
                    let (open, close, plain) = match (chain_big, part_big) {
                        (true, false) => ("BigInt(".to_string(), ")".to_string(), false),
                        (false, true) => {
                            if fits53(self.program.int_range(body, q)) {
                                ("Number(".to_string(), ")".to_string(), false)
                            } else {
                                (String::new(), String::new(), true)
                            }
                        }
                        _ => (String::new(), String::new(), true),
                    };
                    if plain {
                        // A ternary part binds loosest, so it stays
                        // parenthesized wherever it lands in the chain.
                        let (po, pc) = if self.chain_paren(body, q, *op, i > 0) {
                            ("(".to_string(), ")".to_string())
                        } else {
                            (String::new(), String::new())
                        };
                        // A bare mask part prints in hex; parts are short, so
                        // no breaking applies.
                        if let Expr::Int { value, .. } = q
                            && matches!(*op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
                        {
                            let t = bit_int(*op, i128::from(*value), chain_big);
                            if i + 1 == n {
                                lines.push((li, format!("{pre}{po}{t}{pc}")));
                            } else {
                                lines.push((li, format!("{pre}{po}{t}{pc} {sym}")));
                            }
                        } else {
                            // Under `& | ^` a `>>> 0` normalize is a no-op,
                            // so a `& | ^ <<` part prints without it.
                            let suppress =
                                matches!(*op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
                                    && matches!(
                                        q,
                                        Expr::Binary {
                                            op: BinOp::Shl
                                                | BinOp::BitAnd
                                                | BinOp::BitOr
                                                | BinOp::BitXor,
                                            ..
                                        }
                                    );
                            let trail = if i + 1 == n {
                                pc.len()
                            } else {
                                pc.len() + 1 + sym.len()
                            };
                            if suppress {
                                self.lay_parts_nowrap(body, &(pre + &po), q, li, trail, lines);
                            } else {
                                self.lay_parts(body, &(pre + &po), q, li, trail, lines);
                            }
                            let end = lines.len() - 1;
                            if i + 1 == n {
                                lines[end].1.push_str(&pc);
                            } else {
                                lines[end].1.push_str(&format!("{pc} {sym}"));
                            }
                        }
                    } else {
                        let after = if i + 1 == n {
                            String::new()
                        } else {
                            format!(" {sym}")
                        };
                        self.lay_wrapped_chain(body, &pre, &open, q, &close, &after, li, lines);
                    }
                }
            }
            Expr::Cast { expr, to } => {
                let (from, _) = self.program.expr_ty(body, expr);
                if Self::is_enum(self.program, &from) || Self::is_enum(self.program, to) {
                    self.lay_parts(body, prefix, expr, ind, 0, lines);
                    return;
                }
                match (&from, to) {
                    (Ty::Int(t1), Ty::Int(t2)) => {
                        let big = self.cast_big(body, expr, to);
                        let t2c = *t2;
                        let mut args = vec![self.cast_operand_arg(body, expr, *t1, t2c, big)];
                        if t2c == IntTy::I128 {
                            args.push(CallArg::Raw("128".to_string()));
                        }
                        self.lay_call_open(
                            body,
                            &format!("{prefix}{}", Self::cast_head(t2c)),
                            &args,
                            "",
                            ind,
                            last_trail,
                            lines,
                            &single,
                        );
                    }
                    (Ty::Int(_), Ty::Bool) => {
                        let big = self.cast_big(body, expr, &Ty::Bool);
                        let zero = if big { "0n" } else { "0" };
                        let tail = format!(" !== {zero})");
                        self.lay_parts(
                            body,
                            &(prefix.to_string() + "("),
                            expr,
                            ind,
                            tail.len(),
                            lines,
                        );
                        let end = lines.len() - 1;
                        lines[end].1.push_str(&tail);
                    }
                    _ => lines.push((ind, single)),
                }
            }
            Expr::Call { func, args } => {
                self.lay_call(body, prefix, *func, args, ind, last_trail, lines, &single)
            }
            _ => lines.push((ind, single)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn lay_call(
        &self,
        body: &Body,
        prefix: &str,
        func: Func,
        args: &[Expr],
        ind: usize,
        last_trail: usize,
        lines: &mut Vec<(usize, String)>,
        single: &str,
    ) {
        match func {
            Func::SignExtend => {
                let (name, pre, mid) =
                    match (self.sign_extend_num(body, args), self.dom(body, &args[0])) {
                        (true, Dom::Num) => ("signExtend", "", ""),
                        (true, Dom::Big) => ("signExtend", "Number(", ")"),
                        (false, Dom::Big) => ("signExtendBigint", "", ""),
                        (false, Dom::Num) => ("signExtendBigint", "BigInt(", ")"),
                    };
                // Wraps convert crossings (`Number` is exact: a `number`
                // choice guarantees the fit, `BigInt` of an integer always
                // is); `lay_parts` prints the argument in its own domain.
                self.lay_call_open(
                    body,
                    &format!("{prefix}{name}"),
                    &[
                        CallArg::E {
                            pre: pre.to_string(),
                            e: &args[0],
                            mid: mid.to_string(),
                        },
                        CallArg::Raw(self.expr_num(body, &args[1])),
                    ],
                    "",
                    ind,
                    last_trail,
                    lines,
                    single,
                );
            }
            Func::Utf16Units => self.lay_call_open(
                body,
                &format!("{prefix}utf16Units"),
                &[CallArg::E {
                    pre: String::new(),
                    e: &args[0],
                    mid: String::new(),
                }],
                "",
                ind,
                last_trail,
                lines,
                single,
            ),
            Func::Utf8Len => self.lay_call_open(
                body,
                &format!("{prefix}utf8Len"),
                &[CallArg::E {
                    pre: String::new(),
                    e: &args[0],
                    mid: String::new(),
                }],
                "",
                ind,
                last_trail,
                lines,
                single,
            ),
            Func::StrLen => self.lay_call_open(
                body,
                &format!("{prefix}Array.from"),
                &[CallArg::E {
                    pre: String::new(),
                    e: &args[0],
                    mid: String::new(),
                }],
                ".length",
                ind,
                last_trail,
                lines,
                single,
            ),
            Func::BytesLen | Func::ListLen => {
                self.lay_parts(body, prefix, &args[0], ind, ".length".len(), lines);
                let end = lines.len() - 1;
                lines[end].1.push_str(".length");
            }
            Func::Min | Func::Max if self.dom(body, &args[0]) == Dom::Num => {
                let head = if func == Func::Min {
                    "Math.min"
                } else {
                    "Math.max"
                };
                self.lay_call_open(
                    body,
                    &format!("{prefix}{head}"),
                    &[
                        CallArg::E {
                            pre: String::new(),
                            e: &args[0],
                            mid: String::new(),
                        },
                        CallArg::E {
                            pre: String::new(),
                            e: &args[1],
                            mid: String::new(),
                        },
                    ],
                    "",
                    ind,
                    last_trail,
                    lines,
                    single,
                );
            }
            _ => lines.push((ind, single.to_string())),
        }
    }

    /// `head + rhs + ";"` with prettier's assignment layout: one line when
    /// it fits, else the head alone and the whole right-hand side at +1
    /// indent (a call's callee and arguments stay together, exactly as
    /// prettier breaks `let x = decodeFoo(r);`).
    fn assign_break(&self, head: &str, rhs: &str, ind: usize, out: &mut String) {
        let single = format!("{head}{rhs};");
        if ts_fits_at(ind, single.len(), 0) {
            self.line(ind, &single, out);
            return;
        }
        self.line(ind, head.trim_end(), out);
        self.line(ind + 1, &format!("{rhs};"), out);
    }

    /// One call argument for prettier-style breaking.
    /// (The enum itself lives at module level, after this `impl`.)
    /// A call statement with prettier breaking: `single` (byte-identical
    /// to the legacy shape) when it fits, else the head, comma-terminated
    /// arguments, and the closer.
    #[allow(
        clippy::too_many_arguments,
        reason = "a broken call is the head, the arguments, the closer, and the single-line text"
    )]
    fn stmt_call(
        &self,
        body: &Body,
        head: &str,
        args: &[CallArg],
        closer: &str,
        ind: usize,
        out: &mut String,
        single: &str,
    ) {
        if ts_fits_at(ind, single.len(), 0) {
            self.line(ind, single, out);
            return;
        }
        let mut lines = Vec::new();
        self.lay_call_open(body, head, args, closer, ind, 0, &mut lines, single);
        for (li, l) in lines {
            self.line(li, &l, out);
        }
    }

    /// `Number(` / `BigInt(` wrapper in an associative chain: one line when
    /// it fits, else break like a one-argument call and append `close` and
    /// any following operator on the last line.
    #[allow(clippy::too_many_arguments)]
    fn lay_wrapped_chain(
        &self,
        body: &Body,
        prefix: &str,
        open: &str,
        e: &Expr,
        close: &str,
        after: &str,
        ind: usize,
        lines: &mut Vec<(usize, String)>,
    ) {
        let inline = format!("{prefix}{open}{}{close}{after}", self.expr(body, e));
        if ts_fits_at(ind, inline.len(), 0) {
            lines.push((ind, inline));
            return;
        }
        if !open.ends_with('(') {
            self.lay_parts(
                body,
                &(prefix.to_string() + open),
                e,
                ind,
                close.len() + after.len(),
                lines,
            );
            let end = lines.len() - 1;
            lines[end].1.push_str(&(close.to_string() + after));
            return;
        }
        let wrap_head = format!("{prefix}{}", open.trim_end_matches('('));
        lines.push((ind, format!("{wrap_head}(")));
        self.lay_parts(body, "", e, ind + 1, close.len() + after.len(), lines);
        let end = lines.len() - 1;
        lines[end].1.push_str(&(close.to_string() + after));
    }

    /// A call argument with optional `pre`/`mid` wrappers (`Number(…)`, etc.).
    fn lay_wrapped_arg(
        &self,
        body: &Body,
        pre: &str,
        e: &Expr,
        mid: &str,
        ind: usize,
        lines: &mut Vec<(usize, String)>,
    ) {
        let inline = format!("{pre}{}{mid}", self.expr(body, e));
        if ts_fits_at(ind, inline.len(), 1) {
            lines.push((ind, format!("{inline},")));
            return;
        }
        if !pre.ends_with('(') {
            self.lay_inner(body, pre, e, ind, 1 + mid.len(), lines, false);
            let end = lines.len() - 1;
            lines[end].1.push_str(&format!("{mid},"));
            return;
        }
        let wrap_head = pre.trim_end_matches('(');
        let (tail, _) = wrap_mid_tail(mid);
        lines.push((ind, format!("{wrap_head}(")));
        self.lay_parts(body, "", e, ind + 1, tail.len() + 1, lines);
        let end = lines.len() - 1;
        lines[end].1.push_str(&format!("{tail},"));
        lines.push((ind, "),".to_string()));
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a broken call is the head, the arguments, the closer, and the single-line text"
    )]
    fn lay_call_open(
        &self,
        body: &Body,
        head: &str,
        args: &[CallArg],
        closer: &str,
        ind: usize,
        last_trail: usize,
        lines: &mut Vec<(usize, String)>,
        single: &str,
    ) {
        if ts_fits_at(ind, single.len(), last_trail) {
            lines.push((ind, single.to_string()));
            return;
        }
        lines.push((ind, format!("{head}(")));
        for arg in args {
            match arg {
                CallArg::E { pre, e, mid } => {
                    self.lay_wrapped_arg(body, pre, e, mid, ind + 1, lines);
                }
                CallArg::Raw(s) => lines.push((ind + 1, format!("{s},"))),
            }
        }
        lines.push((ind, format!("){closer}")));
    }

    #[allow(clippy::too_many_arguments)]
    fn match_stmt(
        &self,
        body: &Body,
        scrutinee: &Expr,
        arms: &[crate::ir::typed::MatchArm],
        other: &Option<Vec<Stmt>>,
        ind: usize,
        out: &mut String,
    ) {
        // A 32-bit-or-narrower tag switches as `number`, otherwise `bigint`.
        // The domain decides first: `Tag` parameters are `i64` in the typed
        // program but `number` in TS.
        let num = self.dom(body, scrutinee) == Dom::Num
            || match self.program.int_range(body, scrutinee) {
                Some((lo, hi)) => lo >= i128::from(i32::MIN) && hi <= i128::from(i32::MAX),
                None => false,
            };
        let scr = if num {
            self.expr_num(body, scrutinee)
        } else {
            self.expr(body, scrutinee)
        };
        self.line(ind, &format!("switch ({scr}) {{"), out);
        for arm in arms {
            for (i, t) in arm.tags.iter().enumerate() {
                let lit = if num { format!("{t}") } else { format!("{t}n") };
                if i + 1 == arm.tags.len() {
                    self.line(ind + 1, &format!("case {lit}: {{"), out);
                } else {
                    self.line(ind + 1, &format!("case {lit}:"), out);
                }
            }
            self.stmts(body, &arm.body, ind + 2, out);
            if !diverges(&arm.body) {
                self.line(ind + 2, "break;", out);
            }
            self.line(ind + 1, "}", out);
        }
        self.line(ind + 1, "default: {", out);
        match other {
            Some(stmts) => self.stmts(body, stmts, ind + 2, out),
            None => {
                let tag = if num {
                    scr.clone()
                } else {
                    format!("Number({scr})")
                };
                let single = format!("noMatchArm(\"{}\", {}, {tag});", self.codec, self.offset());
                self.stmt_call(
                    body,
                    "noMatchArm",
                    &[
                        CallArg::Raw(format!("\"{}\"", self.codec)),
                        CallArg::Raw(self.offset().to_string()),
                        CallArg::Raw(tag),
                    ],
                    ";",
                    ind + 2,
                    out,
                    &single,
                );
            }
        }
        self.line(ind + 1, "}", out);
        self.line(ind, "}", out);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a repeat is the mode, the list, both names, the body, and the indent"
    )]
    fn repeat(
        &self,
        body: &Body,
        mode: &RepeatMode,
        list: usize,
        names: (String, String),
        inner: &[Stmt],
        ind: usize,
        out: &mut String,
    ) {
        let (list_n, item_n) = names;
        // `Repeat` introduces its list; declare it here (there is no
        // separate `Declare` for it in the typed program).
        self.line(
            ind,
            &format!("const {list_n}: {}[] = [];", self.list_elem(body, list)),
            out,
        );
        match mode {
            RepeatMode::Count(n) => {
                let n = self.expr_num(body, n);
                let check = format!(
                    "if ({n} < 0) negativeLength(\"{}\", {}, {n});",
                    self.codec,
                    self.offset()
                );
                if ind * 2 + check.len() <= 80 {
                    self.line(ind, &check, out);
                } else {
                    self.line(ind, &format!("if ({n} < 0)"), out);
                    let fail = format!(
                        "negativeLength(\"{}\", {}, {n});",
                        self.codec,
                        self.offset()
                    );
                    self.stmt_call(
                        body,
                        "negativeLength",
                        &[
                            CallArg::Raw(format!("\"{}\"", self.codec)),
                            CallArg::Raw(self.offset().to_string()),
                            CallArg::Raw(n.clone()),
                        ],
                        ";",
                        ind + 1,
                        out,
                        &fail,
                    );
                }
                self.line(ind, &format!("for (let i = 0; i < {n}; i++) {{"), out);
            }
            RepeatMode::Until(t) => {
                self.line(ind, "while (r.remaining > 0) {", out);
                let term = byte_literal(t, false).unwrap_or_else(|| self.expr_num(body, t));
                self.line(ind + 1, &format!("if (r.data[r.pos] === {term}) {{"), out);
                self.line(ind + 2, "r.pos++;", out);
                self.line(ind + 2, "break;", out);
                self.line(ind + 1, "}", out);
            }
            RepeatMode::UntilEnd => {
                self.line(ind, "while (r.remaining > 0) {", out);
            }
        }
        self.stmts(body, inner, ind + 1, out);
        self.line(ind + 1, &format!("{list_n}.push({item_n});"), out);
        self.line(ind, "}", out);
    }

    /// The element type name for a repeat list declaration.
    fn list_elem(&self, body: &Body, list: usize) -> String {
        match &body.locals[list].ty {
            Ty::List(elem) => ts_ty(self.program, elem),
            _ => "unknown".to_string(),
        }
    }

    fn expect_variant(
        &self,
        body: &Body,
        dst: &Option<crate::ir::typed::Bind>,
        sum: &Expr,
        arm: (TypeIdx, usize),
        ind: usize,
        out: &mut String,
    ) {
        let (ty, variant) = arm;
        let decl = &self.program.variants(ty)[variant];
        let kind = if decl.tags.is_empty() {
            "other".to_string()
        } else {
            decl.name.snake()
        };
        let s = self.expr(body, sum);
        let guard = format!("if ({s}.kind !== \"{kind}\")");
        let fail = format!(
            "wrongVariant(\"{}\", {}, \"{kind}\", {s}.kind);",
            self.codec,
            self.offset()
        );
        if ind * 2 + guard.len() + 1 + fail.len() <= 80 {
            self.line(ind, &format!("{guard} {fail}"), out);
        } else {
            self.line(ind, &guard, out);
            self.stmt_call(
                body,
                "wrongVariant",
                &[
                    CallArg::Raw(format!("\"{}\"", self.codec)),
                    CallArg::Raw(self.offset().to_string()),
                    CallArg::Raw(format!("\"{kind}\"")),
                    CallArg::Raw(format!("{s}.kind")),
                ],
                ";",
                ind + 1,
                out,
                &fail,
            );
        }
        if let Some(dst) = dst {
            let head = self.decl_head(body, dst);
            // A flat block arm carries the discriminant itself; every other
            // arm (including the catch-all, whose payload field is required)
            // reads `value`.
            let src = if self.is_flat_payload(ty, variant) {
                s.clone()
            } else {
                format!("{s}.value")
            };
            self.line(ind, &format!("{head}{src};"), out);
        }
    }
}

/// Whether a statement list diverges (ends in `return` or a `never` call).
fn diverges(stmts: &[Stmt]) -> bool {
    matches!(stmts.last(), Some(Stmt::Return(_)) | Some(Stmt::Fail(_)))
}
