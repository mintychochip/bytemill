# BPS code generation: one IR, four backends

**Status:** draft, waiting for review.
**Date:** 2026-10-15

Decisions already made: typed encode and decode; the IR lives in the Rust compiler; Go, typed Rust, Python, and C are printers of that IR and do not re-decide semantics.

## 1. Goal

`bml` lowers a resolved schema to one language-neutral IR, then prints libraries that encode and decode with the same meaning as the existing interpreter.

In scope:

1. A lowerer: `ResolvedSchema` → IR.
2. An IR interpreter in Rust. It is the executable definition of the IR.
3. Four printers: `emit go`, `emit rust-typed`, `emit python`, `emit c`.
4. Differential tests: IR interpreter against `interp`, then each generated library against the IR interpreter, on the Minecraft 26.3 root and on protocol 763 (`schemas/versions/v1_20_1.bps`).

## 2. Non-goals

- Replacing `emit rust` (JSON decoders), `emit rstom`, or `emit rstom-tree`. Those stay on the AST. The typed Rust target is a new subcommand so it does not collide with `emit rust`.
- A second lowering in the Python package. Python stays the reference interpreter and becomes one printer.
- Session state machines, compression, encryption, registry name tables.
- Hand-written per-language varint, string, or NBT implementations. Those encodings lower from their schema bodies. The five compiler primitives are the only wire builtins: `u8`, `bytes(n)`, `rest`, `f32be`, `f64be`.
- Editing generated output by hand. A later run with the same schema is byte-identical.

## 3. Pipeline

```
.bps → lex / parse / load / analyze → ResolvedSchema
     → lower → IR (types + encode procs + decode procs + routes)
     → IR interpreter
     → printers: go | rust-typed | python | c
```

`analyze` does not change. Backends never see the AST. A backend is wrong when its bytes disagree with the IR interpreter. The IR interpreter is wrong when it disagrees with `interp` on the same logical value.

## 4. IR

The IR is data in `crates/bps/src/ir/`. It is not the ProtoDef import IR (`protodef/ir.rs`); that one only exists to produce `.bps`.

### 4.1 Schema

- **Types.** Named records, sums, enums, and scalars. Packets are records.
- **Procedures.** Every packet and every encoding that has a body gets a `decode` procedure and an `encode` procedure. Two procedures, not one bidirectional list: the schema already splits the directions (read the bytes, or write the field's `= expr` and the scalar's yield).
- **Routes.** `(state, direction, packet id) → packet type`, plus the frame that route names, or the protocol's `default` frame when it names none. The frame block is lowered like any other block. Today's `decode_frame_payload` does not read that block: it hardcodes a varint length, a varint id, and the remaining bytes as payload. Every committed schema declares `frame default` as that same layout, so the lowered frame matches the interpreter there. A protocol with no frame is a lowerer error. Printers do not hardcode a Minecraft frame.

### 4.2 Public types

| Schema shape | IR type |
| --- | --- |
| Scalar encoding (`= expr` after the block) | One value. Raw byte fields are locals, not struct fields. |
| Encoding with no yield | A record, unless the string-shaped rule below applies. |
| Field `match` | A sum. The scrutinee is the tag. An arm with no body is a unit variant. |
| `[] count` or `[] until` | A list. |
| Field with `if` | Optional. |
| `enum Name : base` | A named integer newtype over `base`, plus constants. |

Statement `match` does not create a sum. It stays a statement, and the enclosing record's fields are the union of the fields spliced in by the arms. A field that an arm does not set is optional.

Two arms that splice the same field name with different types are a lowerer error.

Spread (`...encoding(args)`) is inlined into the enclosing procedure with parameters bound, the way `expand_items` works today. It is not a field named `_spread`.

Enums are open. Decode does not reject an integer that is not a declared member. The handshake packet stores `intent` as a raw `varint`; the `enum intent` declaration is a constant set, not a closed check. A field whose type *is* the enum name uses `base` on the wire and the enum as the public type.

### 4.3 Integer and bool storage

Expressions evaluate as `i128`, with the interpreter's rules: both sides of `&&` and `||` run (no short circuit), `/` is floor division, `%` takes the sign of the divisor, shifts match `interp/expr.rs`. The storage class is the public type of a field or scalar. Printers use that class and no narrower one. Decode of a field classified by the rules below always fits the class. Encode runs the schema's own `= expr` masks (a `u8` write keeps the low 8 bits, as `interp` does). The printer does not add a second range check, and it does not drop bits the schema kept.

The lowerer assigns the class. Printers do not.

Primitive `u8` is `u8` (0..=255).

For a scalar yield or a computed field, apply the first matching rule:

1. **Comparison.** The expression is `==`, `!=`, `<`, `<=`, `>`, or `>=`. Storage is `bool`. `boolean` is `raw != 0`, so it is `bool`.
2. **`sign_extend(expr, N)` with `N` a constant.** Storage is the smallest signed class that holds `N` bits: `N <= 8` → `i8`, `<= 16` → `i16`, `<= 32` → `i32`, `<= 64` → `i64`, otherwise `i128`. This covers `i8` (8), `i16be`/`i16le` (16), `varint` and `i32be`/`i32le` (32), `varlong` and `i64be`/`i64le` (64), and computed fields such as `position`'s 12-bit and 26-bit components (`i16` and `i32`). A non-constant `N` does not match.
3. **Unsigned byte assembly.** The expression is a tree of `|` whose leaves are `u8`-typed names (or a subexpression already classified `u8`), each shifted by a constant multiple of 8, optionally masked. Width is `max(shift) + 8`. `8` → `u8`, `16` → `u16`, `32` → `u32`, `64` → `u64`, otherwise `i128`. `u16be`'s yield `(b0 << 8) | b1` matches because `b0` and `b1` are `u8` fields. This covers `u16be`/`u16le`, `u32be`/`u32le`, `u64be`/`u64le`.
4. **Otherwise** `i128`.

`position` itself is not a scalar (it has no yield). It is a record whose `raw` field is `u64` and whose `x`, `y`, `z` fields use rule 2. `raw` stays public.

### 4.4 String-shaped encodings

This is a shape rule, not a name table. It replaces the interpreter's special case for the names `string`, `identifier`, and `nbt_string`.

An encoding with no yield is `string` when its wire fields are exactly:

- one integer field (the length),
- one `bytes(<that field>) as utf8` field,
- any number of `require`s,

and nothing else (no match, repeat, nested block, or computed field).

`identifier`, a spread of only a string-shaped encoding into an otherwise empty encoding, is also `string`.

Decode reads the length, reads that many bytes, requires valid UTF-8, runs the `require`s, and returns the string. The length is not a public field. Encode takes a string, writes its UTF-8 byte length with the length field's encoding, writes the bytes, and runs the `require`s. `utf16_units(value) <= max_chars` therefore still runs.

`as utf8` is the only transform. Any other transform is a lowerer error. It applies only to `bytes`.

### 4.5 Statements

Used by both procedures. Read operations appear only in `decode`. Write operations appear only in `encode`.

- `let`, `require`
- `if`, `match` (arms, plus a `_` default)
- `repeat count`, `repeat until`, `repeat until-end`
- `read` / `write` of `u8`, `f32be`, `f64be`, `bytes(n)`, `rest`
- `call` of a named procedure (recursion is a call; `nbt` calls itself)

`repeat until` on encode writes the items, then the terminator byte, as `interp` does. `repeat until-end` on encode is an error, as `interp` does today (`encode not supported for repeat mode until_end`). Decode of `until-end` reads elements while bytes remain.

Scalar decode prebinds every wire name in the body to `0`, then overlays the bytes actually read, then evaluates the yield. Scalar encode binds `value` to the scalar and prebinds missing wire names to `0`, then runs each field's `= expr`. Later fields see earlier bytes. This is `decode_scalar` / `encode_scalar`.

`if` on encode: evaluate the guard against the value. If the guard names a missing field and evaluation fails, the group is present when any of its field names exists in the value. That fallback is today in `encode_expanded`. It is part of the encode procedure, not something a printer re-derives.

### 4.6 Calls and values

Calls: `sign_extend`, `utf16_units`, `len`, `min`, `max`, `contains`. Anything else is a lowerer error.
`len` of a string is the number of Unicode scalars, not the number of bytes. `utf16_units` counts UTF-16 code units: a scalar above U+FFFF counts as two.

Value kinds inside the interpreter: `i128`, `f64`, `bool`, `string`, `bytes`, `list`, `record`, `null`. Public storage (section 4.3) is applied when a value enters or leaves a field. Arithmetic stays `i128`.

### 4.7 Name hacks that do not survive

`interp` special-cases the names `string`, `identifier`, `nbt_string`, `uuid`, and `fixed_bitset` on encode. The lowerer ignores the names and lowers the body.

- String-shaped encodings become `string` by section 4.4.
- `uuid` is a record `{ msb: u64, lsb: u64 }`. The range check is the `u64` storage class.
- `fixed_bitset(bits)` is a record `{ data: bytes }`. It is not raw bytes.

### 4.8 Lowerer errors

Unknown type, unknown call, transform other than `utf8`, a `bytes` without a size, a statement-match arm that is not a block. The error names the encoding or packet and the construct.

## 5. Backend contract

Each printer reads only the IR. Mapping:

| IR | Go | Rust (`rust-typed`) | Python | C |
| --- | --- | --- | --- | --- |
| `i8` `i16` `i32` `i64` | `int8` `int16` `int32` `int64` | `i8` `i16` `i32` `i64` | `int` | `int8_t` `int16_t` `int32_t` `int64_t` |
| `u8` `u16` `u32` `u64` | `uint8` `uint16` `uint32` `uint64` | `u8` `u16` `u32` `u64` | `int` | `uint8_t` `uint16_t` `uint32_t` `uint64_t` |
| `i128` | `type Int` two's-complement pair of `uint64` | `i128` | `int` | `struct bps_i128 { uint64_t lo, hi; }` |
| `f64` | `float64` | `f64` | `float` | `double` |
| `bool` | `bool` | `bool` | `bool` | `bool` |
| `string` | `string` | `String` | `str` | pointer + length |
| `bytes` | `[]byte` | `Vec<u8>` | `bytes` | pointer + length |
| list | slice | `Vec` | `list` | pointer + length |
| optional | pointer, `nil` if absent | `Option` | the value or `None` | pointer, `NULL` if absent |
| record | struct | struct | class with fields | struct |
| sum | struct with a tag and a payload | enum | class with a tag | struct with a tag |
| enum | named integer + consts | tuple struct + consts | `int` subclass with consts | integer typedef + consts |

Go `int` is not used. Its width depends on the architecture.

Commands, each requiring `--out <dir>`:

- `bml emit go`
- `bml emit rust-typed`
- `bml emit python`
- `bml emit c`

Output:

- **Go:** one module. Runtime file (cursor, `sign_extend`, `utf16_units`, errors) plus generated types. `Encode` / `Decode` methods. `DecodeFrame(state, direction, frame)` and `EncodeFrame` from the route table.
- **Rust:** a crate that does not depend on `bps`. Same surface. `i128` is the language primitive.
- **Python:** a package, stdlib only, that does not import the `bps` interpreter. Same surface.
- **C:** a `.h` and a `.c`. Every decode takes a `bps_alloc` (malloc and free function pointers, plus a context). The caller frees the top object. No global allocator and no hidden ownership.

Generated names are stable sanitizations of schema names. A keyword in the target language gets a single documented suffix, the same one every time. The expression evaluator in each runtime implements section 4.6, including non-short-circuit `&&` / `||` and floor division.

## 6. Errors

Lowerer errors are `EmitError` and name the schema construct. Generated decode and encode errors name the type and, when the runtime has a cursor, the byte offset. They do not have to reproduce the interpreter's Python `repr` strings. They do have to fail on the same inputs: a failed `require`, invalid UTF-8, a missing match arm, a short buffer, a negative `bytes` length, and `until-end` on encode.

## 7. Tests

1. **Storage classes.** Every encoding in `schemas/std/integers.bps` and `schemas/std/varint.bps` gets the class in section 4.3. `boolean` is `bool`. A test lists every encoding the string-shaped rule selects, so a surprise classification fails by name.
2. **IR against `interp`.** For `schemas/minecraft_26_3_wiki.bps` and `schemas/versions/v1_20_1.bps`: build a synthetic value, encode with `interp`, decode with the IR interpreter, encode again, and compare bytes. The harness normalizes string-shaped values (the interpreter's decoded map `{byte_len, value}` corresponds to the IR string). `uuid` and `fixed_bitset` are compared as records, not through the interpreter's name hacks.
3. **Printers.** Emit each language, compile it (`go test`, `cargo test`, `python`, `cc`), and round-trip the same vectors. The first differing byte and the packet name are the failure.
4. The existing suite stays green. `emit rust` and `emit rstom` tests are unchanged.

## 8. Implementation order

One spec, built in this order. Later steps are not a follow-up project.

1. IR types, lowerer, storage rules, string-shaped rule.
2. IR interpreter and the differential against `interp`.
3. Go printer.
4. Typed Rust printer.
5. Python printer.
6. C printer.

## 9. Locked decisions

- Semantics live in the IR and its interpreter. Printers do not special-case `varint`, `string`, or `nbt`.
- Encode and decode are separate procedures.
- Integer width is the table in section 4.3, including 8-bit and 16-bit sign extension and unsigned assembly. Not "only 32 and 64, else i128".
- `emit rust` and the rstom emitters are out of this change.
- All four printers ship in this change.
