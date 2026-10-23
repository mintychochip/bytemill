//! `emit csharp`: an idiomatic C# library over the typed program.
//!
//! The printer reads [`Program`](crate::ir::typed) only: every name is already
//! resolved, every value typed, every nested record or sum lifted into its own
//! codec. It chooses C# syntax and nothing else.
//!
//! # Files
//!
//! `BmlGen/Runtime.cs` (reader, writer, errors, helpers) and one
//! `BmlGen/{category}.cs` per route category (types and codecs, in program
//! order). A packet routed only as `play_clientbound` lands in
//! `play_clientbound.cs`; a packet routed in several state/direction pairs is
//! `shared.cs`; encodings, enums, frames, and unrouted packets are
//! `encoding.cs`/`unrouted.cs`. Everything lives in `namespace BmlGen`.
//!
//! # Names
//!
//! PascalCase for types, methods, and properties; camelCase for parameters and
//! locals. A trailing `_` escapes C# keywords.
//!
//! # Types
//!
//! Records are `readonly record struct`s on C# 10+ and plain `struct`s before
//! that, with PascalCase properties, a full constructor, a static
//! `Decode(ref Reader)` and an instance `Encode(Writer)`. A record that can
//! reach itself through direct or optional record fields is a reference
//! type instead — a struct holding itself, even through `Nullable<T>`, is
//! an infinite layout — so it is a `record` on C# 9+ and a `sealed class`
//! on C# 8. Sums are an `I{Name}` interface with one record struct per
//! variant (a block-arm record implements the interface itself when it is
//! only that variant's payload, otherwise the variant wraps it in a `Value`
//! property), plus a static `{Name}Codec` class. Enums are `enum Name :
//! base` with schema-named members, plus a static codec class. Scalar and
//! string encodings become static `{Codec}Codec` classes. Optionals are
//! `T?` for value types and bare `null`-able references otherwise (every
//! file carries `#nullable disable`), lists `List<T>`, bytes `byte[]`.
//!
//! # Codecs
//!
//! Every codec body lowers to plain C# statements over the runtime: failures
//! throw `BmlError` carrying the codec name and the reader offset on decode;
//! integer arithmetic is unchecked (wrapping); signed division and modulo use
//! `BmlInt` floor helpers when the dividend may be negative. All locals are
//! declared up front with their zero value, so every path compiles.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::CSharpVersion;
use crate::ResolvedSchema;
use crate::errors::{BmlResult, emit};
use crate::vectors::{TestRun, Vector};

use crate::ir::typed::{
    BinOp, Body, CodecIdx, Expr, Func, IntTy, LocalId, Name, Origin, Program, ReadOp, RepeatMode,
    Stmt, Ty, TypeIdx, VariantIdx, WriteOp, byte_literal, diverges, escape, hex_literal,
};

/// `emit csharp`: write `BmlGen/Runtime.cs` and one file per
/// [`Program::output_stem`].
pub fn emit_csharp(resolved: &ResolvedSchema, out_dir: &Path) -> BmlResult<()> {
    emit_csharp_with_vectors(resolved, out_dir, CSharpVersion::default(), &[]).map(|_| ())
}

pub fn emit_csharp_with_version(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    ver: CSharpVersion,
) -> BmlResult<()> {
    emit_csharp_with_vectors(resolved, out_dir, ver, &[]).map(|_| ())
}

pub fn emit_csharp_with_vectors(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    ver: CSharpVersion,
    vectors: &[Vector],
) -> BmlResult<Option<TestRun>> {
    let program = crate::ir::typed(resolved)?;
    let mut printer = Printer::new(&program, ver)?;
    let files = printer.files()?;
    let root = out_dir.join("BmlGen");
    fs::create_dir_all(&root).map_err(|e| emit(e.to_string()))?;
    fs::write(root.join("Runtime.cs"), printer.runtime_cs()).map_err(|e| emit(e.to_string()))?;
    for (stem, body) in files {
        fs::write(root.join(format!("{stem}.cs")), body).map_err(|e| emit(e.to_string()))?;
    }
    if vectors.is_empty() {
        return Ok(None);
    }
    // Emit native roundtrip test (Check.cs + csproj) embedding the vectors.
    // Only include vectors for packets that have a codec.
    let mut n = 0usize;
    let mut checks = String::new();
    for (i, v) in vectors.iter().enumerate() {
        let Some(c) = program.packet_codec(&v.packet) else {
            continue;
        };
        let dec = decode_fn(&program, c);
        let enc = encode_fn(&program, c);
        let is_record = codec_uses_record_method(&program, c);
        let bytes = v.bytes()?;
        let lit = bytes_literal(&bytes);
        checks.push_str(&format!(
            "        // vector {} for {}\n        byte[] raw{} = {};\n        Reader r{} = new Reader(raw{});\n        var val{} = {}(ref r{});\n        if (r{}.Remaining != 0) throw new Exception(\"{}: decode did not consume whole payload (at \" + r{}.Offset + \")\");\n        Writer w{} = new Writer();\n",
            i, v.packet, i, lit, i, i, i, dec, i, i, v.packet, i, i
        ));
        if is_record {
            checks.push_str(&format!("        val{}.{}(w{});\n", i, enc, i));
        } else {
            checks.push_str(&format!("        {}(w{}, val{});\n", enc, i, i));
        }
        checks.push_str(&format!(
            "        byte[] back{} = w{}.ToByteArray();\n        if (back{}.Length != raw{}.Length) throw new Exception(\"{}: length mismatch\");\n        for (int j = 0; j < raw{}.Length; j++) if (back{}[j] != raw{}[j]) throw new Exception(\"{}: bytes differ at \" + j);\n",
            i, i, i, i, v.packet, i, i, i, v.packet
        ));
        n += 1;
    }
    let lang = match ver {
        CSharpVersion::CSharp8 => "8",
        CSharpVersion::CSharp9 => "9",
        CSharpVersion::CSharp10 => "10",
        CSharpVersion::CSharp11 => "11",
        CSharpVersion::CSharp12 => "12",
    };
    let csproj = format!(
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <LangVersion>{}</LangVersion>\n    <OutputType>Exe</OutputType>\n    <Nullable>disable</Nullable>\n    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n    <StartupObject>Check</StartupObject>\n  </PropertyGroup>\n  <ItemGroup>\n    <Compile Include=\"BmlGen/**/*.cs\" />\n    <Compile Include=\"Check.cs\" />\n  </ItemGroup>\n</Project>\n",
        lang
    );
    let check_src = format!(
        "using System;\nusing BmlGen;\n\nstatic class Check\n{{\n    public static void Main()\n    {{\n{}    }}\n}}\n",
        checks
    );
    fs::write(out_dir.join("bmlgen.csproj"), csproj).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("Check.cs"), check_src).map_err(|e| emit(e.to_string()))?;
    Ok(Some(TestRun {
        file: "Check.cs".to_string(),
        command: "dotnet run -v q".to_string(),
        vectors: n,
    }))
}

fn bytes_literal(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "new byte[0]".to_string();
    }
    let mut s = String::from("new byte[] {\n            ");
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 {
            if i % 8 == 0 {
                s.push_str(",\n            ");
            } else {
                s.push_str(", ");
            }
        }
        s.push_str(&format!("0x{:02x}", b));
    }
    s.push_str("\n        }");
    s
}

/// Whether the codec is emitted as methods directly on its record type (vs *Codec class).
pub fn codec_uses_record_method(program: &Program, codec: CodecIdx) -> bool {
    Data::record_method(program, codec)
}

/// Decode entrypoint name for the codec (e.g. "Probe.Decode" or "VarintCodec.Decode").
pub fn decode_fn(program: &Program, codec: CodecIdx) -> String {
    let data = Data::new(program).expect("typed program is valid for Data");
    if Data::record_method(program, codec) {
        let t = program.codecs[codec].ty.unwrap();
        format!("{}.Decode", data.type_cs[t])
    } else {
        let cls = data.codec_class[codec]
            .clone()
            .unwrap_or_else(|| format!("{}Codec", pascal(&program.codecs[codec].name)));
        format!("{}.Decode", cls)
    }
}

/// Encode entrypoint: for record codecs returns "Encode" (called as val.Encode(w)), else "Name.Encode".
pub fn encode_fn(program: &Program, codec: CodecIdx) -> String {
    let data = Data::new(program).expect("typed program is valid for Data");
    if Data::record_method(program, codec) {
        "Encode".to_string()
    } else {
        let cls = data.codec_class[codec]
            .clone()
            .unwrap_or_else(|| format!("{}Codec", pascal(&program.codecs[codec].name)));
        format!("{}.Encode", cls)
    }
}

// ----------------------------------------------------------------------
// Names
// ----------------------------------------------------------------------

const CS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "record",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "add",
    "alias",
    "and",
    "args",
    "async",
    "await",
    "dynamic",
    "from",
    "get",
    "global",
    "init",
    "join",
    "let",
    "managed",
    "nameof",
    "nint",
    "not",
    "notnull",
    "nuint",
    "on",
    "or",
    "orderby",
    "partial",
    "remove",
    "required",
    "scoped",
    "select",
    "set",
    "unmanaged",
    "value",
    "var",
    "when",
    "where",
    "with",
    "yield",
    // Method names the generated types declare; a property must not take them.
    "Decode",
    "Encode",
];

/// PascalCase with keyword escaping.
fn pascal(name: &Name) -> String {
    escape(
        name.words()
            .iter()
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect(),
        CS_KEYWORDS,
    )
}

/// camelCase with keyword escaping.
fn camel(name: &Name) -> String {
    let mut out = String::new();
    for (i, word) in name.words().iter().enumerate() {
        if i == 0 {
            out.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                out.push_str(&(first.to_ascii_uppercase().to_string() + chars.as_str()));
            }
        }
    }
    escape(out, CS_KEYWORDS)
}

/// A C# double-quoted string literal.
fn cs_quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ----------------------------------------------------------------------
// Shared data
// ----------------------------------------------------------------------

/// Variant, and the set of directly-implemented record payloads.
struct Data<'a> {
    prog: &'a Program,
    type_cs: Vec<String>,
    /// Records that must be C# reference types: reachable from themselves
    /// through value-layout edges only (see [`Data::reference_records`]).
    ref_records: BTreeSet<TypeIdx>,
    /// Variant wrapper class names, for variants whose payload is not a
    /// record that implements the sum interface directly.
    wrappers: BTreeMap<(TypeIdx, VariantIdx), String>,
    /// Sum variants whose payload record implements the interface itself.
    direct: BTreeSet<(TypeIdx, VariantIdx)>,
    /// The codec class per codec; `None` when the codec's methods live on a
    /// record type.
    codec_class: Vec<Option<String>>,
}

impl<'a> Data<'a> {
    fn new(prog: &'a Program) -> BmlResult<Data<'a>> {
        let mut taken = BTreeSet::new();
        // Runtime identifiers are taken before any type claims its name.
        for reserved in [
            "BmlError", "Reader", "Writer", "BmlInt", "BmlText", "Runtime",
        ] {
            taken.insert(reserved.to_string());
        }
        let mut type_cs = Vec::with_capacity(prog.types.len());
        for t in &prog.types {
            let base = pascal(&t.name);
            // The first spelling wins; later duplicates are numbered.
            let mut name = base.clone();
            let mut n = 2;
            while taken.contains(&name) || type_cs.contains(&name) {
                name = format!("{base}{n}");
                n += 1;
            }
            taken.insert(name.clone());
            type_cs.push(name);
        }
        // Sum interfaces share the namespace scope with types.
        for (t, decl) in prog.types.iter().enumerate() {
            if matches!(decl.kind, crate::ir::typed::TypeKind::Sum { .. }) {
                taken.insert(format!("I{}", type_cs[t]));
            }
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
                            &format!("{}{}", type_cs[s], pascal(&variant.name)),
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
                            &format!("{}{}", type_cs[s], pascal(&variant.name)),
                        ),
                    );
                }
            }
        }
        let mut codec_class = Vec::with_capacity(prog.codecs.len());
        for (c, codec) in prog.codecs.iter().enumerate() {
            if Self::record_method(prog, c) {
                codec_class.push(None);
            } else {
                codec_class.push(Some(Self::claim(
                    &mut taken,
                    &format!("{}Codec", pascal(&codec.name)),
                )));
            }
        }
        Ok(Data {
            prog,
            type_cs,
            ref_records: Self::reference_records(prog),
            wrappers,
            direct,
            codec_class,
        })
    }

    /// Whether codec `c` is emitted as methods on its record type.
    fn record_method(prog: &Program, c: CodecIdx) -> bool {
        match prog.codecs[c].ty {
            Some(t) => {
                matches!(
                    prog.types[t].kind,
                    crate::ir::typed::TypeKind::Record { .. }
                ) && prog.types[t].codec == Some(c)
            }
            None => false,
        }
    }

    /// Records that must be C# reference types: those reachable from
    /// themselves through value-layout edges only — a direct record field,
    /// or an optional field whose type is a record emitted as a value type
    /// (`T?` on a struct is `Nullable<T>`, still a value type, so such a
    /// cycle would be an infinite struct layout, CS0523). Edges through
    /// `List<T>`, `byte[]`, `string`, or sums are references and never form
    /// a layout cycle.
    fn reference_records(prog: &Program) -> BTreeSet<TypeIdx> {
        let n = prog.types.len();
        let mut edges: Vec<Vec<TypeIdx>> = vec![Vec::new(); n];
        for (t, decl) in prog.types.iter().enumerate() {
            let crate::ir::typed::TypeKind::Record { fields } = &decl.kind else {
                continue;
            };
            for f in fields {
                let Ty::Named(mut u) = f.ty.clone() else {
                    continue;
                };
                loop {
                    match &prog.types[u].kind {
                        crate::ir::typed::TypeKind::Alias { target } => u = *target,
                        crate::ir::typed::TypeKind::Record { .. } => {
                            edges[t].push(u);
                            break;
                        }
                        _ => break,
                    }
                }
            }
        }
        let mut out = BTreeSet::new();
        for t in 0..n {
            if !matches!(
                prog.types[t].kind,
                crate::ir::typed::TypeKind::Record { .. }
            ) {
                continue;
            }
            // Depth-first search from `t`; reaching `t` again is a cycle.
            let mut seen = BTreeSet::from([t]);
            let mut stack = vec![t];
            let mut cyclic = false;
            while let Some(cur) = stack.pop() {
                for next in &edges[cur] {
                    if *next == t {
                        cyclic = true;
                        break;
                    }
                    if seen.insert(*next) {
                        stack.push(*next);
                    }
                }
                if cyclic {
                    break;
                }
            }
            if cyclic {
                out.insert(t);
            }
        }
        out
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

    /// The interface of sum type `t`.
    fn sum_iface(&self, t: TypeIdx) -> String {
        format!("I{}", self.type_cs[t])
    }
}

struct Printer<'a> {
    data: Data<'a>,
    ver: CSharpVersion,
}

impl<'a> Printer<'a> {
    fn new(prog: &'a Program, ver: CSharpVersion) -> BmlResult<Printer<'a>> {
        Ok(Printer {
            data: Data::new(prog)?,
            ver,
        })
    }
}

// ----------------------------------------------------------------------
// C# types
// ----------------------------------------------------------------------

/// C# type for an integer class; `I128` never appears in shipped schemas.
fn int_cs(ty: IntTy) -> BmlResult<&'static str> {
    match ty {
        IntTy::I8 => Ok("sbyte"),
        IntTy::I16 => Ok("short"),
        IntTy::I32 => Ok("int"),
        IntTy::I64 => Ok("long"),
        IntTy::U8 => Ok("byte"),
        IntTy::U16 => Ok("ushort"),
        IntTy::U32 => Ok("uint"),
        IntTy::U64 => Ok("ulong"),
        IntTy::I128 => Err(emit("emit csharp: i128 has no C# type".to_string())),
    }
}

impl Data<'_> {
    /// C# type for `ty`; `optional` adds `?` for value types only
    /// (`Nullable<T>`). Reference types (`string`, `byte[]`, `List<T>`,
    /// sums, reference records) stay bare; absent is `null`.
    fn cs_ty(&self, ty: &Ty, optional: bool) -> BmlResult<String> {
        let base = match ty {
            Ty::Int(t) => int_cs(*t)?.to_string(),
            Ty::Bool => "bool".into(),
            Ty::F32 => "float".into(),
            Ty::F64 => "double".into(),
            Ty::Str => "string".into(),
            Ty::Bytes => "byte[]".into(),
            Ty::List(inner) => format!("List<{}>", self.cs_ty(inner, false)?),
            Ty::Named(t) => match &self.prog.types[*t].kind {
                crate::ir::typed::TypeKind::Alias { target } => {
                    return self.cs_ty(&Ty::Named(*target), optional);
                }
                crate::ir::typed::TypeKind::Sum { .. } => self.sum_iface(*t),
                _ => self.type_cs[*t].clone(),
            },
        };
        Ok(if optional && self.is_value_type(ty)? {
            format!("{base}?")
        } else {
            base
        })
    }

    /// Whether values of `ty` are C# value types (`Nullable<T>` when optional).
    /// Records in [`Data::ref_records`] are reference types.
    fn is_value_type(&self, ty: &Ty) -> BmlResult<bool> {
        match ty {
            Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => Ok(true),
            Ty::Str | Ty::Bytes | Ty::List(_) => Ok(false),
            Ty::Named(t) => match &self.prog.types[*t].kind {
                crate::ir::typed::TypeKind::Record { .. } => Ok(!self.ref_records.contains(t)),
                crate::ir::typed::TypeKind::Enum { .. } => Ok(true),
                crate::ir::typed::TypeKind::Sum { .. } => Ok(false),
                crate::ir::typed::TypeKind::Alias { target } => {
                    self.is_value_type(&Ty::Named(*target))
                }
            },
        }
    }

    /// The zero value starting a local of `ty` (`null` when optional).
    fn zero_cs(&self, ty: &Ty, optional: bool) -> BmlResult<String> {
        if optional {
            return Ok("null".into());
        }
        match ty {
            Ty::Int(_) | Ty::Bool | Ty::F32 | Ty::F64 => Ok(if matches!(ty, Ty::Bool) {
                "false".into()
            } else {
                "0".into()
            }),
            Ty::Str => Ok("\"\"".into()),
            Ty::Bytes => Ok("Array.Empty<byte>()".into()),
            Ty::List(inner) => Ok(format!("new List<{}>()", self.cs_ty(inner, false)?)),
            Ty::Named(t) => match &self.prog.types[*t].kind {
                crate::ir::typed::TypeKind::Record { .. }
                | crate::ir::typed::TypeKind::Sum { .. }
                | crate::ir::typed::TypeKind::Enum { .. } => Ok("default".into()),
                crate::ir::typed::TypeKind::Alias { target } => {
                    self.zero_cs(&Ty::Named(*target), false)
                }
            },
        }
    }

    /// The property for field `field` of record `ty`.
    fn prop(&self, ty: TypeIdx, field: usize) -> String {
        let fields = self.prog.fields(ty);
        let mut p = pascal(&fields[field].name);
        if p == self.type_cs[ty] {
            // A member may not have the same name as its enclosing type.
            p.push('_');
        }
        p
    }
}

// ----------------------------------------------------------------------
// Runtime.cs
// ----------------------------------------------------------------------

const RUNTIME_CS: &str = r#"// <auto-generated>
// This file was generated by bml from a BML binary-protocol schema.
// Do not edit: regenerate with `bml emit csharp`.
#nullable disable
using System;
using System.Collections.Generic;
using System.Text;

namespace BmlGen
{
    /// <summary>Thrown when decoding or encoding fails.</summary>
    public class BmlError : Exception
    {
        public BmlError(string message)
            : base(message)
        {
        }

        public BmlError(string codec, string message)
            : base(codec + ": " + message)
        {
        }

        public BmlError(string codec, int offset, string message)
            : base(codec + " at offset " + offset + ": " + message)
        {
        }
    }

    /// <summary>Reads BML wire primitives from a byte buffer.</summary>
    public ref struct Reader
    {
        private ReadOnlySpan<byte> _span;
        private int _pos;

        public Reader(byte[] buffer)
        {
            _span = new ReadOnlySpan<byte>(buffer);
            _pos = 0;
        }

        public int Offset
        {
            get { return _pos; }
        }

        public long Remaining
        {
            get { return (long)(_span.Length - _pos); }
        }

        public byte PeekU8()
        {
            if (_pos >= _span.Length)
            {
                throw new BmlError("short buffer");
            }
            return _span[_pos];
        }

        public void Skip(int n)
        {
            if (n < 0 || _pos + n > _span.Length)
            {
                throw new BmlError("short buffer");
            }
            _pos += n;
        }

        public byte ReadU8()
        {
            if (_pos >= _span.Length)
            {
                throw new BmlError("short buffer");
            }
            return _span[_pos++];
        }

        public byte[] ReadBytes(int n)
        {
            if (n < 0 || _pos + n > _span.Length)
            {
                throw new BmlError("short buffer");
            }
            byte[] result = _span.Slice(_pos, n).ToArray();
            _pos += n;
            return result;
        }

        public byte[] ReadRest()
        {
            byte[] result = _span.Slice(_pos).ToArray();
            _pos = _span.Length;
            return result;
        }

        public float ReadF32()
        {
            byte[] raw = ReadBytes(4);
            if (BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            return BitConverter.ToSingle(raw, 0);
        }

        public float ReadF32Le()
        {
            byte[] raw = ReadBytes(4);
            if (!BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            return BitConverter.ToSingle(raw, 0);
        }

        public double ReadF64()
        {
            byte[] raw = ReadBytes(8);
            if (BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            return BitConverter.ToDouble(raw, 0);
        }
        public double ReadF64Le()
        {
            byte[] raw = ReadBytes(8);
            if (!BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            return BitConverter.ToDouble(raw, 0);
        }

    }

    /// <summary>Writes BML wire primitives into a buffer.</summary>
    public sealed class Writer
    {
        private readonly List<byte> _buf = new List<byte>();

        public void WriteU8(byte value)
        {
            _buf.Add(value);
        }

        public void WriteBytes(byte[] value)
        {
            _buf.AddRange(value);
        }

        public void WriteBytes(byte[] value, long n)
        {
            if (n < 0 || value.Length > n)
            {
                throw new BmlError("bytes longer than fixed length");
            }
            _buf.AddRange(value);
            for (long i = value.Length; i < n; i++)
            {
                _buf.Add(0);
            }
        }

        public void WriteF32(float value)
        {
            byte[] raw = BitConverter.GetBytes(value);
            if (BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            _buf.AddRange(raw);
        }

        public void WriteF32Le(float value)
        {
            byte[] raw = BitConverter.GetBytes(value);
            if (!BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            _buf.AddRange(raw);
        }

        public void WriteF64(double value)
        {
            byte[] raw = BitConverter.GetBytes(value);
            if (BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            _buf.AddRange(raw);
        }

        public void WriteF64Le(double value)
        {
            byte[] raw = BitConverter.GetBytes(value);
            if (!BitConverter.IsLittleEndian)
            {
                Array.Reverse(raw);
            }
            _buf.AddRange(raw);
        }

        public byte[] ToByteArray()
        {
            return _buf.ToArray();
        }
    }

    /// <summary>Integer arithmetic with BML semantics (floor division).</summary>
    public static class BmlInt
    {
        public static long Div(long a, long b)
        {
            long q = a / b;
            long r = a % b;
            if (r != 0 && ((r < 0) != (b < 0)))
            {
                q -= 1;
            }
            return q;
        }

        public static long Mod(long a, long b)
        {
            long r = a % b;
            if (r != 0 && ((r < 0) != (b < 0)))
            {
                r += b;
            }
            return r;
        }

        public static long SignExtend(long value, int bits)
        {
            if (bits >= 64)
            {
                return value;
            }
            long mask = 1L << (bits - 1);
            long low = value & ((1L << bits) - 1);
            return (low ^ mask) - mask;
        }
    }

    /// <summary>UTF-8 and string helpers.</summary>
    public static class BmlText
    {
        private static readonly Encoding StrictUtf8 = new UTF8Encoding(false, true);

        public static string DecodeUtf8(byte[] value)
        {
            try
            {
                return StrictUtf8.GetString(value);
            }
            catch (DecoderFallbackException e)
            {
                throw new BmlError("invalid utf-8: " + e.Message);
            }
            catch (ArgumentException e)
            {
                throw new BmlError("invalid utf-8: " + e.Message);
            }
        }

        public static string DecodeUtf8(byte[] value, string codec, int offset)
        {
            try
            {
                return StrictUtf8.GetString(value);
            }
            catch (DecoderFallbackException)
            {
                throw new BmlError(codec, offset, "invalid utf-8");
            }
            catch (ArgumentException)
            {
                throw new BmlError(codec, offset, "invalid utf-8");
            }
        }

        public static byte[] EncodeUtf8(string value)
        {
            return Encoding.UTF8.GetBytes(value);
        }

        public static long Utf8Len(string value)
        {
            return (long)Encoding.UTF8.GetByteCount(value);
        }

        public static long StrLen(string value)
        {
            long n = 0;
            for (int i = 0; i < value.Length; i++)
            {
                if (char.IsHighSurrogate(value[i]) && i + 1 < value.Length && char.IsLowSurrogate(value[i + 1]))
                {
                    i++;
                }
                n++;
            }
            return n;
        }

        public static long Utf16Units(string value)
        {
            return (long)value.Length;
        }
    }
}
"#;

impl Printer<'_> {
    fn runtime_cs(&self) -> String {
        RUNTIME_CS.to_string()
    }
}

// ----------------------------------------------------------------------
// BmlGen/{stem}.cs
// ----------------------------------------------------------------------

fn cs_file(body: &str) -> String {
    let mut out = String::from(
        "// <auto-generated>\n// This file was generated by bml from a BML binary-protocol schema.\n// Do not edit: regenerate with `bml emit csharp`.\n#nullable disable\nusing System;\nusing System.Collections.Generic;\nusing System.Text;\n\nnamespace BmlGen\n{\n",
    );
    for line in body.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("}\n");
    out
}

impl Printer<'_> {
    fn files(&mut self) -> BmlResult<BTreeMap<String, String>> {
        let mut bodies: BTreeMap<String, String> = BTreeMap::new();
        let mut aliases: BTreeMap<String, String> = BTreeMap::new();
        for t in 0..self.data.prog.types.len() {
            if matches!(
                self.data.prog.types[t].kind,
                crate::ir::typed::TypeKind::Alias { .. }
            ) {
                let stem = self.data.prog.output_stem(&self.data.prog.types[t].origin);
                aliases
                    .entry(stem)
                    .or_default()
                    .push_str(&self.alias_using(t)?);
                continue;
            }
            let stem = self.data.prog.output_stem(&self.data.prog.types[t].origin);
            bodies
                .entry(stem)
                .or_default()
                .push_str(&self.type_cs_decl(t)?);
        }
        for c in 0..self.data.prog.codecs.len() {
            if self.data.codec_class[c].is_none() {
                continue;
            }
            let stem = self.data.prog.output_stem(&self.data.prog.codecs[c].origin);
            bodies
                .entry(stem)
                .or_default()
                .push_str(&self.codec_class(c)?);
        }
        // A stem holding only aliases still gets a file.
        for (stem, using) in &aliases {
            bodies.entry(stem.clone()).or_default().insert_str(0, using);
        }
        Ok(bodies
            .into_iter()
            .map(|(stem, body)| (stem, cs_file(&body)))
            .collect())
    }

    fn alias_using(&self, t: TypeIdx) -> BmlResult<String> {
        // Resolve through chained aliases to the final target.
        let mut target = match &self.data.prog.types[t].kind {
            crate::ir::typed::TypeKind::Alias { target } => *target,
            _ => return Err(emit("emit csharp: alias_using of a non-alias".to_string())),
        };
        let mut seen = vec![t];
        while let crate::ir::typed::TypeKind::Alias { target: next } =
            &self.data.prog.types[target].kind
        {
            if seen.contains(&target) {
                return Err(emit("emit csharp: alias cycle".to_string()));
            }
            seen.push(target);
            target = *next;
        }
        let target_cs = match &self.data.prog.types[target].kind {
            crate::ir::typed::TypeKind::Sum { .. } => self.data.sum_iface(target),
            _ => self.data.type_cs[target].clone(),
        };
        Ok(format!("using {} = {};\n", self.data.type_cs[t], target_cs))
    }

    fn type_doc(&self, t: TypeIdx) -> String {
        let decl = &self.data.prog.types[t];
        let _name = &self.data.type_cs[t];
        match &decl.origin {
            Origin::Packet {
                name: packet,
                routes,
            } => {
                if routes.is_empty() {
                    format!("/// <summary>The {packet} packet.</summary>")
                } else {
                    let routes = routes
                        .iter()
                        .map(|r| format!("{} {} 0x{:02x}", r.state, r.direction, r.id))
                        .collect::<Vec<_>>()
                        .join("; ");
                    format!("/// <summary>The {packet} packet ({routes}).</summary>")
                }
            }
            Origin::Encoding { name: encoding } => {
                format!("/// <summary>The {encoding} encoding.</summary>")
            }
            Origin::Enum { name: encoding } => {
                format!("/// <summary>The {encoding} enum.</summary>")
            }
            Origin::Frame { name: frame } => format!("/// <summary>The {frame} frame.</summary>"),
            Origin::Field { parent, field } => {
                let parent_cs = &self.data.type_cs[*parent];
                format!("/// <summary>The {field} of {parent_cs}.</summary>")
            }
            Origin::Variant { sum, variant } => {
                let sum_cs = &self.data.type_cs[*sum];
                let variant_name = match &self.data.prog.types[*sum].kind {
                    crate::ir::typed::TypeKind::Sum { variants } => variants[*variant].name.snake(),
                    _ => "arm".to_string(),
                };
                format!("/// <summary>The {variant_name} arm of {sum_cs}.</summary>")
            }
        }
    }

    fn type_cs_decl(&mut self, t: TypeIdx) -> BmlResult<String> {
        match self.data.prog.types[t].kind.clone() {
            crate::ir::typed::TypeKind::Record { .. } => self.record_decl(t),
            crate::ir::typed::TypeKind::Sum { .. } => self.sum_decl(t),
            crate::ir::typed::TypeKind::Enum { .. } => self.enum_decl(t),
            crate::ir::typed::TypeKind::Alias { .. } => {
                Err(emit("emit csharp: alias in type_cs_decl".to_string()))
            }
        }
    }

    fn record_decl(&mut self, t: TypeIdx) -> BmlResult<String> {
        let doc = self.type_doc(t);
        let name = self.data.type_cs[t].clone();
        let fields = match &self.data.prog.types[t].kind {
            crate::ir::typed::TypeKind::Record { fields } => fields.clone(),
            _ => return Err(emit("emit csharp: record_decl of a non-record".to_string())),
        };
        // Sums this record directly implements.
        let mut ifaces: Vec<String> = Vec::new();
        for (s, v) in self.data.direct.iter() {
            if let Some(Ty::Named(target)) = self
                .data
                .prog
                .variants(*s)
                .get(*v)
                .and_then(|vd| vd.payload.clone())
                && target == t
            {
                ifaces.push(self.data.sum_iface(*s));
            }
        }
        let impls = if ifaces.is_empty() {
            String::new()
        } else {
            format!(" : {}", ifaces.join(", "))
        };
        let mut props: Vec<(String, String, String)> = Vec::new();
        for (f, field) in fields.iter().enumerate() {
            let cs_ty = self.data.cs_ty(&field.ty, field.optional)?;
            props.push((cs_ty, self.data.prop(t, f), camel(&field.name)));
        }
        let mut out = String::new();
        // Records in a value-layout cycle are reference types: a struct
        // holding itself (even through `Nullable<T>`) is an infinite layout.
        if self.data.ref_records.contains(&t) {
            if self.ver.supports_record() {
                out.push_str(&format!("{doc}\npublic record {name}{impls}\n{{\n"));
                for (ty, prop, _) in &props {
                    out.push_str(&format!("    public {ty} {prop} {{ get; init; }}\n"));
                }
            } else {
                out.push_str(&format!("{doc}\npublic sealed class {name}{impls}\n{{\n"));
                for (ty, prop, _) in &props {
                    out.push_str(&format!("    public {ty} {prop} {{ get; set; }}\n"));
                }
            }
            if !props.is_empty() {
                out.push('\n');
                let params = props
                    .iter()
                    .map(|(ty, _, param)| format!("{ty} {param}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("    public {name}({params})\n    {{\n"));
                for (_, prop, param) in &props {
                    out.push_str(&format!("        {prop} = {param};\n"));
                }
                out.push_str("    }\n");
            }
        } else if self.ver.supports_record_struct() {
            out.push_str(&format!(
                "{doc}\npublic readonly record struct {name}{impls}\n{{\n"
            ));
            for (ty, prop, _) in &props {
                out.push_str(&format!("    public {ty} {prop} {{ get; init; }}\n"));
            }
            if !props.is_empty() {
                out.push('\n');
                let params = props
                    .iter()
                    .map(|(ty, _, param)| format!("{ty} {param}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("    public {name}({params})\n    {{\n"));
                for (_, prop, param) in &props {
                    out.push_str(&format!("        {prop} = {param};\n"));
                }
                out.push_str("    }\n");
            }
        } else {
            out.push_str(&format!("{doc}\npublic struct {name}{impls}\n{{\n"));
            for (ty, prop, _) in &props {
                out.push_str(&format!("    public {ty} {prop} {{ get; set; }}\n"));
            }
            if !props.is_empty() {
                out.push('\n');
                let params = props
                    .iter()
                    .map(|(ty, _, param)| format!("{ty} {param}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!("    public {name}({params})\n    {{\n"));
                for (_, prop, param) in &props {
                    out.push_str(&format!("        {prop} = {param};\n"));
                }
                out.push_str("    }\n");
            }
        }
        if let Some(c) = self.data.prog.types[t].codec
            && Data::record_method(self.data.prog, c)
        {
            out.push('\n');
            out.push_str(&indent(&self.decode_method(c, true)?, 1));
            out.push('\n');
            out.push_str(&indent(&self.encode_method(c, true)?, 1));
        }
        out.push_str("}\n\n");
        Ok(out)
    }

    fn sum_decl(&mut self, t: TypeIdx) -> BmlResult<String> {
        let doc = self.type_doc(t);
        let name = self.data.type_cs[t].clone();
        let iface = self.data.sum_iface(t);
        let variants = match &self.data.prog.types[t].kind {
            crate::ir::typed::TypeKind::Sum { variants } => variants.clone(),
            _ => return Err(emit("emit csharp: sum_decl of a non-sum".to_string())),
        };
        let mut out = format!("{doc}\npublic interface {iface}\n{{\n}}\n\n");
        for (v, variant) in variants.iter().enumerate() {
            let wrap = self.data.wrappers.get(&(t, v)).cloned();
            let direct = self.data.direct.contains(&(t, v));
            let vdoc = format!(
                "/// <summary>The {} variant of {}.</summary>",
                variant.name.snake(),
                name
            );
            match (&variant.payload, direct, wrap) {
                (_, true, _) => {
                    // The record payload implements the interface itself; its
                    // declaration carries the `: IFace` clause.
                }
                (None, false, Some(wrap)) => {
                    if self.ver.supports_record_struct() {
                        out.push_str(&format!(
                            "{vdoc}\npublic readonly record struct {wrap} : {iface}\n{{\n}}\n\n"
                        ));
                    } else {
                        out.push_str(&format!(
                            "{vdoc}\npublic struct {wrap} : {iface}\n{{\n}}\n\n"
                        ));
                    }
                }
                (Some(payload), false, Some(wrap)) => {
                    let payload_cs = self.data.cs_ty(payload, false)?;
                    if self.ver.supports_record_struct() {
                        out.push_str(&format!(
                            "{vdoc}\npublic readonly record struct {wrap} : {iface}\n{{\n    public {payload_cs} Value {{ get; init; }}\n\n    public {wrap}({payload_cs} value)\n    {{\n        Value = value;\n    }}\n}}\n\n"
                        ));
                    } else {
                        out.push_str(&format!(
                            "{vdoc}\npublic struct {wrap} : {iface}\n{{\n    public {payload_cs} Value {{ get; set; }}\n\n    public {wrap}({payload_cs} value)\n    {{\n        Value = value;\n    }}\n}}\n\n"
                        ));
                    }
                }
                _ => {
                    return Err(emit(format!(
                        "emit csharp: {name} variant {} has no shape",
                        variant.name.snake()
                    )));
                }
            }
        }
        Ok(out)
    }

    fn enum_decl(&mut self, t: TypeIdx) -> BmlResult<String> {
        let doc = self.type_doc(t);
        let name = self.data.type_cs[t].clone();
        let (base, members) = match &self.data.prog.types[t].kind {
            crate::ir::typed::TypeKind::Enum { base, members } => (*base, members.clone()),
            _ => return Err(emit("emit csharp: enum_decl of a non-enum".to_string())),
        };
        let base_cs = int_cs(base)?;
        let mut out = format!("{doc}\npublic enum {name} : {base_cs}\n{{\n");
        for m in &members {
            out.push_str(&format!("    {} = {},\n", pascal(&m.name), m.value));
        }
        out.push_str("}\n\n");
        Ok(out)
    }

    /// `Decode` method of codec `c`; on a record when `on_record`.
    fn decode_method(&mut self, c: CodecIdx, _on_record: bool) -> BmlResult<String> {
        let prog = self.data.prog;
        let codec = &prog.codecs[c];
        let ret_cs = self.data.cs_ty(&codec.value, false)?;
        let params = codec.params.clone();
        let decode_body = codec.decode.clone();
        let ret_doc = doc_value(&self.data, &codec.value)?;
        let mut sig = format!(
            "/// <summary>Decodes {ret_doc}.</summary>\npublic static {ret_cs} Decode(ref Reader r"
        );
        let mut bg = BodyGen::new(
            &self.data,
            &decode_body,
            &codec.name,
            true,
            "r",
            String::new(),
        );
        for (param, local) in params.iter().zip(decode_body.params.iter()) {
            let ty = self.data.cs_ty(&param.ty, param.optional)?;
            sig.push_str(&format!(", {ty} {}", bg.local_cs[*local]));
        }
        sig.push_str(")\n{\n");
        let mut body = String::new();
        for (i, local) in decode_body.locals.iter().enumerate() {
            if decode_body.params.contains(&i) {
                continue;
            }
            body.push_str(&format!(
                "    {} {} = {};\n",
                self.data.cs_ty(&local.ty, local.optional)?,
                bg.local_cs[i],
                self.data.zero_cs(&local.ty, local.optional)?
            ));
        }
        bg.indent = 1;
        bg.stmts(&decode_body.stmts)?;
        if !diverges(&decode_body.stmts) {
            bg.line(&format!(
                "throw new BmlError({}, r.Offset, \"unreachable\");",
                cs_quote(&bg.codec)
            ));
        }
        body.push_str(&bg.out);
        Ok(format!("{sig}{body}}}\n"))
    }

    /// `Encode` method of codec `c`; instance form on a record when `on_record`.
    fn encode_method(&mut self, c: CodecIdx, on_record: bool) -> BmlResult<String> {
        let prog = self.data.prog;
        let codec = &prog.codecs[c];
        let ret_cs = self.data.cs_ty(&codec.value, false)?;
        let params = codec.params.clone();
        let encode_body = codec.encode.clone();
        let ret_doc = doc_value(&self.data, &codec.value)?;
        let Some(value_id) = encode_body.value else {
            return Err(emit(format!(
                "emit csharp: {} has no encode value",
                codec.name.snake()
            )));
        };
        let value_name = if on_record {
            "this".to_string()
        } else {
            "value".to_string()
        };
        let mut bg = BodyGen::new(
            &self.data,
            &encode_body,
            &codec.name,
            false,
            "w",
            value_name.clone(),
        );
        bg.value = Some(value_id);
        let mut sig = if on_record {
            format!("/// <summary>Encodes {ret_doc}.</summary>\npublic void Encode(Writer w")
        } else {
            format!(
                "/// <summary>Encodes {ret_doc}.</summary>\npublic static void Encode(Writer w, {ret_cs} value"
            )
        };
        for (param, local) in params.iter().zip(encode_body.params.iter()) {
            let ty = self.data.cs_ty(&param.ty, param.optional)?;
            sig.push_str(&format!(", {ty} {}", bg.local_cs[*local]));
        }
        sig.push_str(")\n{\n");
        let mut body = String::new();
        for (i, local) in encode_body.locals.iter().enumerate() {
            if i == value_id || encode_body.params.contains(&i) {
                continue;
            }
            body.push_str(&format!(
                "    {} {} = {};\n",
                self.data.cs_ty(&local.ty, local.optional)?,
                bg.local_cs[i],
                self.data.zero_cs(&local.ty, local.optional)?
            ));
        }
        bg.indent = 1;
        bg.stmts(&encode_body.stmts)?;
        body.push_str(&bg.out);
        Ok(format!("{sig}{body}}}\n"))
    }

    fn codec_class(&mut self, c: CodecIdx) -> BmlResult<String> {
        let class = self.data.codec_class[c]
            .clone()
            .unwrap_or_else(|| format!("{}Codec", pascal(&self.data.prog.codecs[c].name)));
        let doc = match &self.data.prog.codecs[c].origin {
            Origin::Packet { name, .. } => {
                format!("/// <summary>Codecs for the {name} packet.</summary>")
            }
            Origin::Encoding { name } => {
                format!("/// <summary>Codecs for the {name} encoding.</summary>")
            }
            Origin::Enum { name } => format!("/// <summary>Codecs for the {name} enum.</summary>"),
            Origin::Frame { name } => {
                format!("/// <summary>Codecs for the {name} frame.</summary>")
            }
            Origin::Field { parent, field } => format!(
                "/// <summary>Codecs for the {field} of {}.</summary>",
                self.data.type_cs[*parent]
            ),
            Origin::Variant { sum, .. } => format!(
                "/// <summary>Codecs for an arm of {}.</summary>",
                self.data.type_cs[*sum]
            ),
        };
        let decode = indent(&self.decode_method(c, false)?, 1);
        let encode = indent(&self.encode_method(c, false)?, 1);
        Ok(format!(
            "{doc}\npublic static class {class}\n{{\n{decode}\n{encode}}}\n\n"
        ))
    }
}

/// Indent every non-blank line of `text` by `levels` of four spaces.
fn indent(text: &str, levels: usize) -> String {
    let pad = "    ".repeat(levels);
    let mut out = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else {
            out.push_str(&pad);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn doc_value(data: &Data<'_>, ty: &Ty) -> BmlResult<String> {
    match ty {
        Ty::Int(t) => Ok(with_article(int_cs(*t)?)),
        Ty::Bool => Ok("a bool".into()),
        Ty::F32 => Ok("a float".into()),
        Ty::F64 => Ok("a double".into()),
        Ty::Str => Ok("a string".into()),
        Ty::Bytes => Ok("a byte array".into()),
        Ty::List(inner) => Ok(format!(
            "a list of {}",
            strip_article(&doc_value(data, inner)?)
        )),
        Ty::Named(t) => Ok(with_article(&data.type_cs[*t])),
    }
}

fn strip_article(text: &str) -> &str {
    text.strip_prefix("an ")
        .or_else(|| text.strip_prefix("a "))
        .unwrap_or(text)
}

fn with_article(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let you = ["uint", "uu", "uni", "use", "usu", "uti"]
        .iter()
        .any(|p| lower.starts_with(p));
    let vowel = lower.starts_with(['a', 'e', 'i', 'o', 'u']) && !you;
    format!("{} {word}", if vowel { "an" } else { "a" })
}

// ----------------------------------------------------------------------
// Bodies: statements
// ----------------------------------------------------------------------

/// One method body under emission.
struct BodyGen<'a> {
    data: &'a Data<'a>,
    body: Body,
    is_decode: bool,
    codec: String,
    cursor: &'static str,
    value: Option<LocalId>,
    value_name: String,
    local_cs: Vec<String>,
    used: BTreeSet<String>,
    tmp: usize,
    indent: usize,
    out: String,
}

impl<'a> BodyGen<'a> {
    fn new(
        data: &'a Data<'a>,
        body: &Body,
        codec: &Name,
        is_decode: bool,
        cursor: &'static str,
        value_name: String,
    ) -> BodyGen<'a> {
        let mut used = BTreeSet::new();
        used.insert(cursor.to_string());
        used.insert("value".to_string());
        let mut local_cs = Vec::with_capacity(body.locals.len());
        for (i, local) in body.locals.iter().enumerate() {
            if body.params.contains(&i) {
                let name = claim(&mut used, &camel(&local.name));
                local_cs.push(name);
            } else {
                local_cs.push(String::new());
            }
        }
        for (i, local) in body.locals.iter().enumerate() {
            if body.params.contains(&i) {
                continue;
            }
            let name = claim(&mut used, &camel(&local.name));
            local_cs[i] = name;
        }
        BodyGen {
            data,
            body: body.clone(),
            is_decode,
            codec: pascal(codec),
            cursor,
            value: None,
            value_name,
            local_cs,
            used,
            tmp: 0,
            indent: 1,
            out: String::new(),
        }
    }

    fn local(&self, id: LocalId) -> String {
        if Some(id) == self.value {
            return self.value_name.clone();
        }
        self.local_cs[id].clone()
    }

    fn pick(&mut self, base: &str) -> String {
        self.tmp += 1;
        let mut name = format!("{base}{}", self.tmp);
        let mut n = self.tmp;
        while self.used.contains(&name) {
            n += 1;
            name = format!("{base}{n}");
        }
        self.tmp = n;
        self.used.insert(name.clone());
        name
    }

    fn line(&mut self, text: &str) {
        self.out.push_str(&"    ".repeat(self.indent));
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn open(&mut self, text: &str) {
        self.line(text);
        self.line("{");
        self.indent += 1;
    }

    fn close(&mut self) {
        if self.indent > 0 {
            self.indent -= 1;
        }
        self.line("}");
    }

    fn fail(&self, message: &str) -> String {
        if self.is_decode {
            format!(
                "throw new BmlError({}, {}.Offset, {});",
                cs_quote(&self.codec),
                self.cursor,
                cs_quote(message)
            )
        } else {
            format!(
                "throw new BmlError({}, {});",
                cs_quote(&self.codec),
                cs_quote(message)
            )
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) -> BmlResult<()> {
        for stmt in stmts {
            self.stmt(&stmt.clone())?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> BmlResult<()> {
        match stmt {
            // Locals are declared up front with their zero value.
            Stmt::Declare { .. } => Ok(()),
            Stmt::Let { dst, value } => {
                let rhs = self.expr(value)?;
                self.line(&format!("{} = {};", self.local(dst.local), rhs));
                Ok(())
            }
            Stmt::Read { dst, op } => {
                let d = self.local(dst.local);
                match op {
                    ReadOp::U8 => self.line(&format!("{d} = {}.ReadU8();", self.cursor)),
                    ReadOp::F32 => self.line(&format!("{d} = {}.ReadF32();", self.cursor)),
                    ReadOp::F64 => self.line(&format!("{d} = {}.ReadF64();", self.cursor)),
                    ReadOp::F32Le => self.line(&format!("{d} = {}.ReadF32Le();", self.cursor)),
                    ReadOp::F64Le => self.line(&format!("{d} = {}.ReadF64Le();", self.cursor)),

                    ReadOp::Bytes(n) => {
                        let count = self.expr(n)?;
                        self.line(&format!("{d} = {}.ReadBytes((int)({count}));", self.cursor));
                    }
                    ReadOp::Rest => self.line(&format!("{d} = {}.ReadRest();", self.cursor)),
                }
                Ok(())
            }
            Stmt::Decode { dst, codec, args } => {
                let call = self.decode_call(*codec, args)?;
                self.line(&format!("{} = {call};", self.local(dst.local)));
                Ok(())
            }
            Stmt::Utf8 { dst, src } => {
                let text = self.expr(src)?;
                let d = self.local(dst.local);
                if self.is_decode {
                    self.line(&format!(
                        "{d} = BmlText.DecodeUtf8({text}, {}, {}.Offset);",
                        cs_quote(&self.codec),
                        self.cursor
                    ));
                } else {
                    self.line(&format!("{d} = BmlText.DecodeUtf8({text});"));
                }
                Ok(())
            }
            Stmt::Write { op, value } => {
                let text = self.expr(value)?;
                match op {
                    WriteOp::U8 => self.line(&format!("{}.WriteU8((byte)({text}));", self.cursor)),
                    WriteOp::F32 => {
                        self.line(&format!("{}.WriteF32((float)({text}));", self.cursor));
                    }
                    WriteOp::F64 => {
                        self.line(&format!("{}.WriteF64((double)({text}));", self.cursor));
                    }
                    WriteOp::F32Le => {
                        self.line(&format!("{}.WriteF32Le((float)({text}));", self.cursor));
                    }
                    WriteOp::F64Le => {
                        self.line(&format!("{}.WriteF64Le((double)({text}));", self.cursor));
                    }

                    WriteOp::Bytes(n) => {
                        let count = self.expr(n)?;
                        self.line(&format!("{}.WriteBytes({text}, {count});", self.cursor));
                    }
                    WriteOp::Rest => self.line(&format!("{}.WriteBytes({text});", self.cursor)),
                }
                Ok(())
            }
            Stmt::Encode { codec, args, value } => {
                let call = self.encode_call(*codec, args, value)?;
                self.line(&format!("{call};"));
                Ok(())
            }
            Stmt::Require { cond, text } => {
                let negated = self.data.prog.negated(&self.body.clone(), cond);
                let check = self.expr(&negated)?;
                let message = format!("require failed: `{text}`");
                self.line(&format!("if ({check}) {}", self.fail(&message)));
                Ok(())
            }
            Stmt::Unwrap { dst, src, field } => {
                let text = self.expr(src)?;
                let d = self.local(dst.local);
                let message = format!("missing `{field}`");
                self.line(&format!("{d} = {text} ?? {};", self.fail_expr(&message)));
                Ok(())
            }
            Stmt::If { cond, then, els } => {
                let check = self.expr(cond)?;
                self.open(&format!("if ({check})"));
                self.stmts(then)?;
                self.close();
                if !els.is_empty() {
                    self.open("else");
                    self.stmts(els)?;
                    self.close();
                }
                Ok(())
            }
            Stmt::Match {
                scrutinee,
                arms,
                other,
            } => {
                let text = self.expr(scrutinee)?;
                self.open(&format!("switch ((long)({text}))"));
                for arm in arms {
                    for tag in &arm.tags {
                        self.line(&format!("case {tag}:"));
                    }
                    self.indent += 1;
                    self.stmts(&arm.body)?;
                    if !diverges(&arm.body) {
                        self.line("break;");
                    }
                    self.indent -= 1;
                }
                self.line("default:");
                self.indent += 1;
                match other {
                    Some(stmts) => {
                        self.stmts(stmts)?;
                        if !diverges(stmts) {
                            self.line("break;");
                        }
                    }
                    None => {
                        let message = format!("no match arm for tag `{{{text}}}`");
                        self.line(&format!("throw new {};", self.fail_call(&message)));
                    }
                }
                self.indent -= 1;
                self.close();
                Ok(())
            }
            Stmt::Repeat {
                mode,
                list,
                item,
                body,
            } => {
                let list_go = self.local(*list);
                let item_go = self.local(*item);
                let elem = match &self.body.locals[*list].ty {
                    Ty::List(inner) => self.data.cs_ty(inner, false)?,
                    other => {
                        return Err(emit(format!(
                            "emit csharp: repeat list is not a list: {other:?}"
                        )));
                    }
                };
                match mode {
                    RepeatMode::Count(n) => {
                        let total = self.expr(n)?;
                        self.line(&format!(
                            "if (({total}) < 0) {}",
                            self.fail("negative repeat count")
                        ));
                        self.line(&format!("{list_go} = new List<{elem}>();"));
                        let iv = self.pick("i");
                        self.open(&format!("for (long {iv} = 0; {iv} < ({total}); {iv}++)"));
                        self.stmts(body)?;
                        self.line(&format!("{list_go}.Add({item_go});"));
                        self.close();
                    }
                    RepeatMode::Until(term) => {
                        self.line(&format!("{list_go} = new List<{elem}>();"));
                        self.open(&format!("while ({}.Remaining > 0)", self.cursor));
                        match byte_literal(term, true) {
                            Some(byte) => {
                                self.open(&format!("if ({}.PeekU8() == {byte})", self.cursor));
                                self.line(&format!("{}.Skip(1);", self.cursor));
                                self.line("break;");
                                self.close();
                            }
                            None => {
                                let want = self.as_i64(term)?;
                                self.open(&format!(
                                    "if (((long){}.PeekU8()) == ({want}))",
                                    self.cursor
                                ));
                                self.line(&format!("{}.Skip(1);", self.cursor));
                                self.line("break;");
                                self.close();
                            }
                        }
                        self.stmts(body)?;
                        self.line(&format!("{list_go}.Add({item_go});"));
                        self.close();
                    }
                    RepeatMode::UntilEnd => {
                        self.line(&format!("{list_go} = new List<{elem}>();"));
                        self.open(&format!("while ({}.Remaining > 0)", self.cursor));
                        self.stmts(body)?;
                        self.line(&format!("{list_go}.Add({item_go});"));
                        self.close();
                    }
                }
                Ok(())
            }
            Stmt::ForEach { list, item, body } => {
                if self.is_decode {
                    return Err(emit("emit csharp: ForEach in decode".to_string()));
                }
                let range = self.expr(list)?;
                let item_go = self.local(*item);
                let each = self.pick("each");
                self.open(&format!("foreach (var {each} in {range})"));
                self.line(&format!("{item_go} = {each};"));
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
                let text = self.expr(sum)?;
                let wrap = if self.data.direct.contains(&(*ty, *variant)) {
                    match self.expecting_payload(*ty, *variant)? {
                        Some(target) => self.data.type_cs[target].clone(),
                        None => {
                            return Err(emit(
                                "emit csharp: direct variant without record".to_string(),
                            ));
                        }
                    }
                } else {
                    self.data
                        .wrappers
                        .get(&(*ty, *variant))
                        .cloned()
                        .ok_or_else(|| emit("emit csharp: variant without wrapper".to_string()))?
                };
                let matched = self.pick("v");
                self.line(&format!(
                    "if (!({text} is {wrap} {matched})) {}",
                    self.fail("wrong variant")
                ));
                if let Some(dst) = dst {
                    let d = self.local(dst.local);
                    if self.data.direct.contains(&(*ty, *variant)) {
                        self.line(&format!("{d} = {matched};"));
                    } else {
                        self.line(&format!("{d} = {matched}.Value;"));
                    }
                }
                Ok(())
            }
            Stmt::Return(expr) => {
                let text = self.expr(expr)?;
                self.line(&format!("return {text};"));
                Ok(())
            }
            Stmt::Fail(failure) => {
                let message = match failure {
                    crate::ir::typed::Failure::EncodeUntilEnd => {
                        "encode not supported for repeat mode until_end"
                    }
                };
                self.line(&self.fail(message));
                Ok(())
            }
        }
    }

    /// `new BmlError(...)` as an expression (for `?? throw`).
    fn fail_expr(&self, message: &str) -> String {
        if self.is_decode {
            format!(
                "throw new BmlError({}, {}.Offset, {})",
                cs_quote(&self.codec),
                self.cursor,
                cs_quote(message)
            )
        } else {
            format!(
                "throw new BmlError({}, {})",
                cs_quote(&self.codec),
                cs_quote(message)
            )
        }
    }

    /// `BmlError(...)` constructor arguments for an interpolated message.
    fn fail_call(&self, message: &str) -> String {
        if self.is_decode {
            format!(
                "BmlError({}, {}.Offset, ${})",
                cs_quote(&self.codec),
                self.cursor,
                cs_quote(message)
            )
        } else {
            format!(
                "BmlError({}, ${})",
                cs_quote(&self.codec),
                cs_quote(message)
            )
        }
    }

    fn expecting_payload(&self, ty: TypeIdx, variant: usize) -> BmlResult<Option<TypeIdx>> {
        match self.data.prog.variants(ty).get(variant) {
            Some(vd) => match &vd.payload {
                Some(Ty::Named(t)) => Ok(Some(*t)),
                _ => Ok(None),
            },
            None => Err(emit("emit csharp: variant out of range".to_string())),
        }
    }

    fn decode_call(&mut self, codec: CodecIdx, args: &[Expr]) -> BmlResult<String> {
        let recv = if Data::record_method(self.data.prog, codec) {
            let t = self.data.prog.codecs[codec].ty.unwrap();
            // A record property can share a type's PascalCase name; the
            // qualified name still resolves as a type inside the body.
            format!("global::BmlGen.{}.Decode", self.data.type_cs[t])
        } else {
            format!(
                "{}.Decode",
                self.data.codec_class[codec]
                    .clone()
                    .unwrap_or_else(|| format!(
                        "{}Codec",
                        pascal(&self.data.prog.codecs[codec].name)
                    ))
            )
        };
        let mut s = format!("{recv}(ref {}", self.cursor);
        let params = self.data.prog.codecs[codec].params.clone();
        for (param, arg) in params.iter().zip(args.iter()) {
            let want = self.data.cs_ty(&param.ty, param.optional)?;
            s.push_str(", ");
            s.push_str(&self.coerced(arg, &want)?);
        }
        s.push(')');
        Ok(s)
    }

    fn encode_call(&mut self, codec: CodecIdx, args: &[Expr], value: &Expr) -> BmlResult<String> {
        let params = self.data.prog.codecs[codec].params.clone();
        let mut arg_texts = Vec::new();
        for (param, arg) in params.iter().zip(args.iter()) {
            let want = self.data.cs_ty(&param.ty, param.optional)?;
            arg_texts.push(self.coerced(arg, &want)?);
        }
        if Data::record_method(self.data.prog, codec) {
            let ret = self
                .data
                .cs_ty(&self.data.prog.codecs[codec].value, false)?;
            let v = self.coerced(value, &ret)?;
            let mut s = format!("({v}).Encode({}", self.cursor);
            for a in arg_texts {
                s.push_str(", ");
                s.push_str(&a);
            }
            s.push(')');
            Ok(s)
        } else {
            let class = self.data.codec_class[codec]
                .clone()
                .unwrap_or_else(|| format!("{}Codec", pascal(&self.data.prog.codecs[codec].name)));
            let ret = self
                .data
                .cs_ty(&self.data.prog.codecs[codec].value, false)?;
            let v = self.coerced(value, &ret)?;
            let mut s = format!("{class}.Encode({}, {v}", self.cursor);
            for a in arg_texts {
                s.push_str(", ");
                s.push_str(&a);
            }
            s.push(')');
            Ok(s)
        }
    }

    /// Print `arg` converted to the C# type `want`.
    fn coerced(&mut self, arg: &Expr, want: &str) -> BmlResult<String> {
        let (ty, optional) = self.data.prog.expr_ty(&self.body.clone(), arg);
        let text = self.expr(arg)?;
        let got = self.data.cs_ty(&ty, optional)?;
        if got == *want {
            return Ok(text);
        }
        if optional {
            let inner = self.data.cs_ty(&ty, false)?;
            let base = if self.data.is_value_type(&ty)? {
                format!("{text}.Value")
            } else {
                text
            };
            if inner == *want {
                return Ok(base);
            }
            return Ok(format!("({want})({base})"));
        }
        Ok(format!("({want})({text})"))
    }

    /// Print `expr` widened to `long`.
    fn as_i64(&mut self, expr: &Expr) -> BmlResult<String> {
        let (ty, optional) = self.data.prog.expr_ty(&self.body.clone(), expr);
        let text = self.expr(expr)?;
        let base = if optional {
            if self.data.is_value_type(&ty)? {
                format!("{text}.Value")
            } else {
                text
            }
        } else {
            text
        };
        if !optional && matches!(ty, Ty::Int(IntTy::I64)) {
            Ok(base)
        } else {
            Ok(format!("(long)({base})"))
        }
    }
}

fn claim(taken: &mut BTreeSet<String>, base: &str) -> String {
    let mut name = base.to_string();
    if name.is_empty() {
        name = "v".to_string();
    }
    let stem = name.clone();
    let mut n = 2;
    while taken.contains(&name) {
        name = format!("{stem}{n}");
        n += 1;
    }
    taken.insert(name.clone());
    name
}

// ----------------------------------------------------------------------
// Bodies: expressions
// ----------------------------------------------------------------------

impl BodyGen<'_> {
    fn expr(&mut self, expr: &Expr) -> BmlResult<String> {
        match expr {
            Expr::Int { value, ty } => int_lit(*value, *ty, false),
            Expr::Bool(value) => Ok(value.to_string()),
            Expr::Str(value) => Ok(cs_quote(value)),
            Expr::Local(id) => Ok(self.local(*id)),
            Expr::Field { base, ty, field } => {
                let name = match &self.data.prog.types[*ty].kind {
                    crate::ir::typed::TypeKind::Record { .. } => self.data.prop(*ty, *field),
                    _ => return Err(emit("emit csharp: Field of a non-record".to_string())),
                };
                let text = self.expr(base)?;
                Ok(format!("{text}.{name}"))
            }
            Expr::Present(inner) => {
                let text = self.expr(inner)?;
                let (ty, _) = self.data.prog.expr_ty(&self.body.clone(), inner);
                if self.data.is_value_type(&ty)? {
                    Ok(format!("{text}.HasValue"))
                } else {
                    Ok(format!("{text} != null"))
                }
            }
            Expr::Get(inner) => {
                let text = self.expr(inner)?;
                let (ty, _) = self.data.prog.expr_ty(&self.body.clone(), inner);
                if self.data.is_value_type(&ty)? {
                    Ok(format!("{text}.Value"))
                } else {
                    Ok(text)
                }
            }
            Expr::Not(inner) => {
                if is_atomic(inner) {
                    let text = self.expr(inner)?;
                    Ok(format!("!{text}"))
                } else {
                    let text = self.expr(inner)?;
                    Ok(format!("!({text})"))
                }
            }
            Expr::Binary {
                op,
                lhs,
                rhs,
                ty,
                wraps: _,
            } => self.binary(*op, lhs, rhs, ty),
            Expr::Cond {
                cond, then, els, ..
            } => {
                let check = self.expr(cond)?;
                let yes = self.expr(then)?;
                let no = self.expr(els)?;
                Ok(format!("({check} ? {yes} : {no})"))
            }
            Expr::Cast { expr, to } => {
                let (from, from_opt) = self.data.prog.expr_ty(&self.body.clone(), expr);
                let mut text = self.expr(expr)?;
                if from_opt && self.data.is_value_type(&from)? {
                    text = format!("{text}.Value");
                }
                let want = self.data.cs_ty(to, false)?;
                let got = self.data.cs_ty(&from, false)?;
                if got == want {
                    return Ok(text);
                }
                match (&from, to) {
                    (Ty::Bool, Ty::Int(_)) => Ok(format!("({want})(({text}) ? 1 : 0)")),
                    (Ty::Int(_), Ty::Int(_))
                    | (Ty::Int(_), Ty::F32)
                    | (Ty::Int(_), Ty::F64)
                    | (Ty::F32, Ty::F64)
                    | (Ty::F64, Ty::F32)
                    | (Ty::F32, Ty::Int(_))
                    | (Ty::F64, Ty::Int(_))
                    | (Ty::Named(_), Ty::Int(_))
                    | (Ty::Int(_), Ty::Named(_))
                    | (Ty::Named(_), Ty::Named(_)) => Ok(format!("({want})({text})")),
                    _ => Err(emit(format!(
                        "emit csharp: unsupported cast {from:?} to {to:?}"
                    ))),
                }
            }
            Expr::Call { func, args } => match func {
                Func::SignExtend => {
                    let value = self.as_i64(&args[0])?;
                    let bits = match &args[1] {
                        Expr::Int { value, .. } => value.to_string(),
                        other => format!("(int)({})", self.as_i64(other)?),
                    };
                    Ok(format!("BmlInt.SignExtend({value}, {bits})"))
                }
                Func::Utf16Units => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("BmlText.Utf16Units({text})"))
                }
                Func::Utf8Len => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("BmlText.Utf8Len({text})"))
                }
                Func::StrLen => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("BmlText.StrLen({text})"))
                }
                Func::BytesLen => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("(long)({text}).Length"))
                }
                Func::ListLen => {
                    let text = self.expr(&args[0])?;
                    Ok(format!("(long)({text}).Count"))
                }
                Func::Min => {
                    let low = self.expr(&args[0])?;
                    let high = self.expr(&args[1])?;
                    Ok(format!("Math.Min({low}, {high})"))
                }
                Func::Max => {
                    let low = self.expr(&args[0])?;
                    let high = self.expr(&args[1])?;
                    Ok(format!("Math.Max({low}, {high})"))
                }
            },
            Expr::StrBytes(inner) => {
                let text = self.expr(inner)?;
                Ok(format!("BmlText.EncodeUtf8({text})"))
            }
            Expr::Remaining => {
                if !self.is_decode {
                    return Err(emit("emit csharp: Remaining in encode".to_string()));
                }
                Ok(format!("{}.Remaining", self.cursor))
            }
            Expr::Construct { ty, fields } => self.construct_cs(*ty, fields),
            Expr::Variant {
                ty,
                variant,
                payload,
            } => self.variant_value(*ty, *variant, payload.as_deref()),
        }
    }

    fn construct_cs(&mut self, ty: TypeIdx, fields: &[Expr]) -> BmlResult<String> {
        let name = match &self.data.prog.types[ty].kind {
            crate::ir::typed::TypeKind::Record { .. } => self.data.type_cs[ty].clone(),
            _ => return Err(emit("emit csharp: Construct of a non-record".to_string())),
        };
        let mut args = Vec::new();
        for (f, e) in fields.iter().enumerate() {
            let decl = &self.data.prog.fields(ty)[f];
            let want = self.data.cs_ty(&decl.ty, decl.optional)?;
            args.push(self.coerced(e, &want)?);
        }
        Ok(format!("new {name}({})", args.join(", ")))
    }

    fn variant_value(
        &mut self,
        ty: TypeIdx,
        variant: usize,
        payload: Option<&Expr>,
    ) -> BmlResult<String> {
        if self.data.direct.contains(&(ty, variant)) {
            let Some(payload) = payload else {
                return Err(emit(
                    "emit csharp: direct variant without payload".to_string(),
                ));
            };
            return self.expr(payload);
        }
        let wrap = self
            .data
            .wrappers
            .get(&(ty, variant))
            .cloned()
            .ok_or_else(|| emit("emit csharp: variant without wrapper".to_string()))?;
        match payload {
            None => Ok(format!("new {wrap}()")),
            Some(payload) => {
                let vd = &self.data.prog.variants(ty)[variant];
                let want = match &vd.payload {
                    Some(p) => self.data.cs_ty(p, false)?,
                    None => return Err(emit("emit csharp: payload without type".to_string())),
                };
                let text = self.coerced(payload, &want)?;
                Ok(format!("new {wrap}({text})"))
            }
        }
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, ty: &Ty) -> BmlResult<String> {
        // Signed division and modulo floor; C# truncates. Use a helper when
        // the dividend may be negative.
        let negative = match ty {
            Ty::Int(t) if t.signed() => match self.data.prog.int_range(&self.body.clone(), lhs) {
                Some((lo, _)) => lo < 0,
                None => true,
            },
            _ => false,
        };
        if negative && matches!(op, BinOp::Div | BinOp::Mod) {
            let helper = if op == BinOp::Div {
                "BmlInt.Div"
            } else {
                "BmlInt.Mod"
            };
            let left = self.as_i64(lhs)?;
            let right = self.as_i64(rhs)?;
            let call = format!("{helper}({left}, {right})");
            let narrow = self.data.cs_ty(ty, false)?;
            if narrow == "long" {
                return Ok(call);
            }
            return Ok(format!("({narrow})({call})"));
        }
        let level = prec(op);
        let symbol = op.symbol();
        let left = match lhs {
            Expr::Int { value, ty } if hex_literal(op, *value) => int_lit(*value, *ty, true)?,
            _ => self.operand(lhs, level, false)?,
        };
        let right = match rhs {
            Expr::Int { value, ty } if hex_literal(op, *value) => int_lit(*value, *ty, true)?,
            _ => self.operand(rhs, level, true)?,
        };
        // C# shift operands must be `int`; the IR types the count as i64.
        let right = if matches!(op, BinOp::Shl | BinOp::Shr) {
            format!("(int)({right})")
        } else {
            right
        };
        let s = format!("{left} {symbol} {right}");
        // Small integer operands promote to `int`; narrow back to the result.
        if matches!(ty, Ty::Int(IntTy::I8 | IntTy::I16 | IntTy::U8 | IntTy::U16)) {
            let narrow = self.data.cs_ty(ty, false)?;
            return Ok(format!("({narrow})({s})"));
        }
        Ok(format!("({s})"))
    }

    /// Print a binary operand, parenthesizing when C# would parse it
    /// differently.
    fn operand(&mut self, child: &Expr, level: u8, is_right: bool) -> BmlResult<String> {
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
            Ok(format!("({})", self.expr(child)?))
        } else {
            self.expr(child)
        }
    }
}

fn is_atomic(expr: &Expr) -> bool {
    !matches!(expr, Expr::Binary { .. } | Expr::Cond { .. } | Expr::Not(_))
}

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

/// A C# integer literal of class `ty`.
fn int_lit(value: i64, ty: IntTy, hex: bool) -> BmlResult<String> {
    let digits = if hex {
        format!("0x{value:X}")
    } else {
        value.to_string()
    };
    match ty {
        IntTy::I8 => Ok(format!("(sbyte){digits}")),
        IntTy::I16 => Ok(format!("(short){digits}")),
        IntTy::I32 => Ok(digits),
        IntTy::I64 => Ok(format!("{digits}L")),
        IntTy::U8 => Ok(format!("(byte){digits}")),
        IntTy::U16 => Ok(format!("(ushort){digits}")),
        IntTy::U32 => Ok(format!("{digits}U")),
        IntTy::U64 => Ok(format!("{digits}UL")),
        IntTy::I128 => Err(emit("emit csharp: i128 has no C# literal".to_string())),
    }
}
