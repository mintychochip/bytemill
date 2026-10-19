# BPS codegen IR implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lower a resolved BPS schema into one language-neutral IR and execute that IR so its bytes match the existing interpreter.

**Architecture:** `lower` walks `ResolvedSchema` once and produces `IrSchema` (types, decode procs, encode procs, routes, frames). `exec` runs those procs. Printers bind to the signatures Task 2 freezes. Go, typed Rust, Python, and C are Task 9, after the differential in Task 8 is green.

**Tech Stack:** Rust edition 2024, crate `bytemill`, no new dependencies. Tests are `cargo test -p bytemill`.

**Spec:** `docs/superpowers/specs/2026-10-15-bps-codegen-ir-design.md`

## Global Constraints

- Semantics live in the IR. No name special-cases for `varint`, `string`, `nbt`, `uuid`, or `fixed_bitset`.
- Encode and decode are separate procedures.
- Integer storage follows spec section 4.3, including 8-bit and 16-bit `sign_extend` and unsigned assembly of `u8` fields. Anything else is `i128`.
- Expressions stay `crate::ast::Expr` and evaluate with `interp::eval_expr` (`i128`, floor `/`, Python `%`, both sides of `&&` and `||`).
- The only wire builtins are `u8`, `bytes(n)`, `rest`, `f32be`, `f64be`. The only transform is `utf8`. The only calls are `sign_extend`, `utf16_units`, `len`, `min`, `max`, `contains`.
- `len` of a string is Unicode scalars. `utf16_units` counts UTF-16 code units.
- `repeat until-end` encode is an error. `repeat until` encode writes the terminator after the items.
- Scalar decode and encode prebind missing wire names to `0`. Encode of a scalar binds `value`.
- A protocol with no `frame` is a lowerer error. Frame bytes come from the lowered frame, not a hardcoded varint pair.
- `emit rust`, `emit rstom`, and `emit rstom-tree` stay untouched.
- Lowerer failures are `ErrorKind::Emit` and name the packet or encoding.

---

### Task 1: Storage classifier

**Files:**
- Create: `crates/bytemill/src/ir/width.rs`
- Create: `crates/bytemill/src/ir/mod.rs`
- Modify: `crates/bytemill/src/lib.rs` (add `pub mod ir;`)
- Test: `crates/bytemill/tests/test_ir_width.rs`

**Interfaces:**
- Consumes: `crate::ast::Expr`, `analyze`, `parse`, `load_schema`
- Produces: `ir::Storage`, `ir::storage_of(expr, locals) -> Storage`

`Storage` is `I8 I16 I32 I64 I128 U8 U16 U32 U64 Bool F64 Str Bytes`. `locals` maps a name already classified (`b0` to `U8`) so unsigned assembly can see byte fields.

- [ ] **Step 1: Write the failing test** in `crates/bytemill/tests/test_ir_width.rs`

```rust
use bytemill::ast::Expr;
use bytemill::ir::{storage_of, Storage};
use std::collections::HashMap;

fn locals(pairs: &[(&str, Storage)]) -> HashMap<String, Storage> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
}

fn parse_expr(src: &str) -> Expr {
    let text = format!("encoding probe = {{ x: u8 }} = {src}\n");
    let schema = bytemill::parse(&text).unwrap();
    schema.encodings[0].yield_expr.clone().unwrap()
}

#[test]
fn sign_extend_picks_the_smallest_signed_class() {
    let env = locals(&[("raw", Storage::U8)]);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 8)"), &env), Storage::I8);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 12)"), &env), Storage::I16);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 26)"), &env), Storage::I32);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 32)"), &env), Storage::I32);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 64)"), &env), Storage::I64);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 65)"), &env), Storage::I128);
}

#[test]
fn comparison_is_bool_and_unsigned_or_is_u16() {
    let env = locals(&[("b0", Storage::U8), ("b1", Storage::U8), ("raw", Storage::U8)]);
    assert_eq!(storage_of(&parse_expr("raw != 0"), &env), Storage::Bool);
    assert_eq!(storage_of(&parse_expr("(b0 << 8) | b1"), &env), Storage::U16);
}
```

- [ ] **Step 2:** Run `cargo test -p bytemill --test test_ir_width -- --test-threads=8`. Expected: compile failure, `storage_of` missing.

- [ ] **Step 3:** Implement `storage_of` with the four rules in spec section 4.3. A `sign_extend` whose bit count is not an `Expr::Int` returns `I128`. An `|` tree returns `U8`/`U16`/`U32`/`U64` only when every leaf is a local of class `U8` or a shift of one by a constant multiple of 8. Any other shape returns `I128`. Add `pub mod ir;` to `lib.rs`.

- [ ] **Step 4:** Add `std_encodings_match_the_table`. Load `schemas/std/integers.bps` and `schemas/std/varint.bps` via `load_schema` + `analyze`. Assert: `i8` is `I8`; `i16be` and `i16le` are `I16`; `i32be`, `i32le`, and `varint` are `I32`; `i64be`, `i64le`, and `varlong` are `I64`; `u16be` and `u16le` are `U16`; `u32be` and `u32le` are `U32`; `u64be` and `u64le` are `U64`; `boolean` is `Bool`.

- [ ] **Step 5:** Re-run the test. Expected: pass.

- [ ] **Step 6:** Commit `feat(ir): classify scalar storage from the yield expression`.

### Task 2: IR schema types

**Files:**
- Create: `crates/bytemill/src/ir/schema.rs`
- Modify: `crates/bytemill/src/ir/mod.rs`

**Interfaces:**
- Consumes: `Storage`, `crate::ast::Expr`
- Produces: the types below. Later tasks construct these and do not rename them.

```rust
pub struct TypeId(pub String);

pub enum Ty {
    Scalar(Storage),
    List(Box<Ty>),
    Optional(Box<Ty>),
    Named(TypeId),
}

pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub optional: bool,
}

pub enum TypeDef {
    Scalar(Storage),
    String,
    Record { fields: Vec<Field> },
    Sum { arms: Vec<Variant> },
    Enum { base: Storage, members: Vec<(String, i128)> },
}

pub struct Variant {
    pub tag: Option<i128>,
    pub ty: Ty,
}

pub enum Stmt {
    Let { name: String, expr: Expr },
    Require(Expr),
    If { cond: Expr, then_body: Vec<Stmt>, else_body: Vec<Stmt> },
    Match { scrutinee: Expr, arms: Vec<IrArm>, default: Option<Vec<Stmt>> },
    RepeatCount { n: Expr, elem: Vec<Stmt>, dst: String },
    RepeatUntil { term: Expr, elem: Vec<Stmt>, dst: String },
    RepeatUntilEnd { elem: Vec<Stmt>, dst: String },
    Read { dst: String, op: WireOp },
    Write { op: WireOp, value: Expr },
    Utf8 { dst: String, src: String },
    CallDecode { dst: String, callee: TypeId, args: Vec<Expr> },
    CallEncode { callee: TypeId, args: Vec<Expr>, value: Expr },
}

pub struct IrArm {
    pub tags: Vec<i128>,
    pub body: Vec<Stmt>,
}

pub enum WireOp {
    U8,
    F32be,
    F64be,
    Bytes { len: Expr },
    Rest,
}

pub struct Proc {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub result: Option<Expr>,
}

pub struct Route {
    pub state: String,
    pub direction: String,
    pub packet_id: i128,
    pub packet: TypeId,
    pub frame: String,
}

pub struct IrSchema {
    pub types: crate::ordmap::OrderedMap<TypeDef>,
    pub decode: crate::ordmap::OrderedMap<Proc>,
    pub encode: crate::ordmap::OrderedMap<Proc>,
    pub routes: Vec<Route>,
    pub frames: crate::ordmap::OrderedMap<Proc>,
}
```

`Utf8` is a `Stmt`, not a `WireOp`. A printer must not treat it as a cursor read.

- [ ] **Step 1:** Add the types. Re-export them from `ir/mod.rs`.
- [ ] **Step 2:** `cargo check -p bytemill` passes.
- [ ] **Step 3:** Commit `feat(ir): define the schema IR`.

### Task 3: Lower scalars and primitives

**Files:**
- Create: `crates/bytemill/src/ir/lower.rs`
- Test: `crates/bytemill/tests/test_ir_lower.rs`

**Interfaces:**
- Consumes: `ResolvedSchema`, Task 2 types, `storage_of`
- Produces: `pub fn lower(resolved: &ResolvedSchema) -> BpsResult<IrSchema>`

- [ ] **Step 1: Failing test.** Parse `schemas/std/integers.bps` plus `packet probe { n: i32be }`. `lower` yields type `i32be` as `TypeDef::Scalar(Storage::I32)`. Decode of `i32be` calls `u32be` (or reads through it); it does not inline another scalar's body. `u8` decode is one `Read` of `WireOp::U8`. `u8` encode is one `Write` of `value & 0xFF`.

- [ ] **Step 2:** Run `cargo test -p bytemill --test test_ir_lower -- --test-threads=8`. Expected: failure.

- [ ] **Step 3: Implement `lower`.**
  - Unknown calls and transforms other than `utf8` are `BpsError::emit`, naming the encoding.
  - Builtin procs for `u8`, `f32be`, `f64be`, `bytes`, `rest`. `bytes` takes param `n`.
  - Each scalar encoding becomes `TypeDef::Scalar(storage_of(yield))` plus decode and encode procs. Decode result is the yield expression. A field whose type is another encoding is `CallDecode` / `CallEncode`.
  - `b1: u8 if (b0 & 0x80) != 0` is `Stmt::If`. The else branch does not read. Prebind of absent names to 0 is `exec`'s job (Task 4), not extra lets.

- [ ] **Step 4:** Test passes.
- [ ] **Step 5:** Commit `feat(ir): lower scalar encodings to call-based procs`.

### Task 4: Execute scalars

**Files:**
- Create: `crates/bytemill/src/ir/exec.rs`
- Test: extend `crates/bytemill/tests/test_ir_lower.rs`

**Interfaces:**
- Consumes: `IrSchema`, `Proc`, `Value`, `interp::eval_expr`
- Produces:

```rust
pub fn exec_decode(ir: &IrSchema, ty: &str, args: &[Value], bytes: &[u8]) -> BpsResult<Value>;
pub fn exec_encode(ir: &IrSchema, ty: &str, args: &[Value], value: &Value) -> BpsResult<Vec<u8>>;
```

- [ ] **Step 1: Failing test.** Load integers and varint. `i8` value `-1` encodes to `[0xff]` and decodes back to `Value::Int(-1)`. `varint` 300 encodes to `ac 02`. `varint` `-1` encodes to `ff ff ff ff 0f`. `boolean` `true` encodes to `[1]` and decodes to `Value::Bool(true)`.

- [ ] **Step 2:** Run the test. Expected: failure.
- [ ] **Step 3: Implement `exec`.** Scalar decode prebinds every `Read` destination to `Value::Int(0)`, overlays params, runs the body, evaluates `result`. Scalar encode inserts `value` and the same zeros. `Require` failure uses `ErrorKind::Decode` or `Encode`. Do not reimplement `&&` or `||`; `eval_expr` is already eager.
- [ ] **Step 4:** Test passes.
- [ ] **Step 5:** Commit `feat(ir): execute scalar encode and decode`.

### Task 5: String-shaped encodings

**Files:**
- Modify: `crates/bytemill/src/ir/lower.rs`, `crates/bytemill/src/ir/exec.rs`
- Test: `crates/bytemill/tests/test_ir_lower.rs`

- [ ] **Step 1: Failing test.** Schema imports std integers and varint, then:

```bps
encoding string(max_chars = 32767) {
    byte_len: varint
    value: bytes(byte_len) as utf8
    require utf16_units(value) <= max_chars
}
encoding identifier = { ...string(32767) }
packet probe { s: string(16)  id: identifier }
```

`lower` types both as `TypeDef::String`. `exec_encode(ir, "string", &[Value::Int(16)], &Value::str("hi"))` equals the interpreter bytes for field `s` = `"hi"` (varint 2, then `68 69`). Decode returns `Value::Str`, not a map. A string whose UTF-16 length exceeds `max_chars` fails. Do not match on the name `string`.

- [ ] **Step 2:** Run, expect failure.
- [ ] **Step 3:** Implement spec section 4.4. A spread of only a string-shaped encoding into an empty encoding is `TypeDef::String` whose procs call the inner procs with the spread arguments.
- [ ] **Step 4:** Also assert the string-shaped names in `schemas/minecraft_core.bps` are exactly `string`, `identifier`, and `nbt_string`. A new name fails the test until the expected list is updated.
- [ ] **Step 5:** Commit `feat(ir): lower string-shaped encodings to a single string`.

### Task 6: Records, optionals, repeats, matches, spreads

**Files:**
- Modify: `lower.rs`, `exec.rs`
- Test: `crates/bytemill/tests/test_ir_lower.rs`

- [ ] **Step 1: Failing tests.**
  - `position` from `schemas/minecraft_core.bps` is a `Record` with `raw: U64`, `x` and `z` at `I32`, `y` at `I16`. Eight zero bytes decode to a map containing those four names.
  - Packet field `extra: u8 if flag` is omitted on encode when `flag` is false.
  - `items: u8[] count = n` with `n: u8` encodes a count byte plus the items.
  - `entries: u8[] until 0x00` encodes the items then `00`.
  - `until-end` decodes one element per remaining byte. Encode returns an error whose message contains `until_end`.
  - Field `match` builds `TypeDef::Sum`. Tag 1 selects the `u8` arm.
  - Statement `match` keeps `Stmt::Match`. Fields spliced only by one arm are optional. Two arms that splice the same name as `u8` and as `u16be` make `lower` return `EmitError`.
  - A spread of a non-string record inlines its fields. No field is named `_spread`.
  - `uuid` is a record of two `u64` fields. `fixed_bitset` is a record with one `bytes` field.
  - `enum intent : varint { status = 1 login = 2 }` lowers to `TypeDef::Enum` with base `I32` and those members. A field typed `intent` decodes through `varint` and does not reject `0`.
  - Group-encode fallback: when the guard expression fails because it names a missing field, the group is still encoded if the value contains any of the group's field names.

- [ ] **Step 2:** Run, expect failure.
- [ ] **Step 3:** Implement. Calls stay calls.
- [ ] **Step 4:** Tests pass.
- [ ] **Step 5:** Commit `feat(ir): lower records, matches, and repeats`.

### Task 7: Frames, routes, packets

**Files:**
- Modify: `lower.rs`, `exec.rs`
- Test: `crates/bytemill/tests/test_ir_lower.rs`

**Interfaces:**
- Produces:

```rust
pub fn exec_decode_frame(ir: &IrSchema, state: &str, direction: &str, frame: &[u8]) -> BpsResult<(i128, Value)>;
pub fn exec_encode_frame(ir: &IrSchema, state: &str, direction: &str, packet: &str, fields: &crate::value::Env) -> BpsResult<Vec<u8>>;
```

- [ ] **Step 1: Failing test.** `schemas/minecraft_26_3_wiki.bps` lowers. Route count equals `resolved.routes.len()`. Frame `10008906096c6f63616c686f737463dd01` decodes as handshake id 0 with `protocol_version` 777, `server_address` `localhost`, `server_port` 25565, `intent` 1. A protocol with no `frame` fails `lower` with `EmitError`. Encoding those fields yields the same bytes.

- [ ] **Step 2:** Run, expect failure.
- [ ] **Step 3:** Packet procs are records. Frame procs are the frame block. `exec_decode_frame` runs the route's frame (or `default`), then the packet decode on the payload. The length varint counts the bytes after itself, matching `decode_frame_payload`.
- [ ] **Step 4:** Test passes.
- [ ] **Step 5:** Commit `feat(ir): lower frames and dispatch routes`.

### Task 8: Differential against the interpreter

**Files:**
- Create: `crates/bytemill/tests/test_ir_diff.rs`

- [ ] **Step 1:** For `schemas/minecraft_26_3_wiki.bps` and `schemas/versions/v1_20_1.bps`, `lower`, then for every packet `synth::synthesize_packet`, `encode_packet`, `exec_decode`, `exec_encode`. Bytes match. Normalize an interpreter string map `{byte_len, value}` to the string. Compare `uuid` and `fixed_bitset` as records. On mismatch print the packet name and the first differing byte.

- [ ] **Step 2:** `cargo test -p bytemill --test test_ir_diff -- --test-threads=8` passes. Fix the lowerer or exec until it does. Do not weaken the oracle.

- [ ] **Step 3:** `cargo test -p bytemill --test test_std_types --test test_emit_rust --test test_emit_rstom -- --test-threads=8` still passes.

- [ ] **Step 4:** Commit `test(ir): round-trip the IR against the interpreter`.

### Task 9: Printers

Start only after Task 8 is green. Each printer reads `IrSchema` and nothing else. The type map is spec section 5. Commands require `--out`.

**Files:**
- Create: `crates/bytemill/src/emit_go.rs`, `emit_rust_typed.rs`, `emit_python.rs`, `emit_c.rs`
- Modify: `crates/bytemill/src/lib.rs`, `crates/bytemill/src/cli.rs` (the `emit` target allow-list)
- Test: `crates/bytemill/tests/test_emit_go.rs`, `test_emit_rust_typed.rs`, `test_emit_python.rs`, `test_emit_c.rs`

- [ ] **Step 1:** One failing test per language. Emit std varint plus `packet probe { n: varint }` and assert the output contains encode and decode of `probe`. A driver (`go test`, `cargo test`, `python`, `cc`) round-trips 300 (`ac 02`) and `-1`.

- [ ] **Step 2:** Implement each printer as one match on `Stmt` and one match on `Ty`. The runtime file supplies the cursor, `sign_extend`, and `utf16_units` only. C decode takes `bps_alloc` (malloc and free function pointers plus a context). The caller frees the top object. Go uses `int32` for `I32`, never `int`.

- [ ] **Step 3:** Run Task 8's vectors through each generated library for those two schemas. If `cc` or `python` is missing, the test still asserts the generated source contains one function per packet and prints the skip; Go and Rust must compile.

- [ ] **Step 4:** One commit per language: `feat(emit): go backend`, `feat(emit): typed rust backend`, `feat(emit): python backend`, `feat(emit): c backend`.

- [ ] **Step 5:** Document the four commands in `README.md`. Commit `docs: document the IR language backends`.

## Self-review

- Spec section 4 (widths, string shape, frames, open enums, until-end, spreads) is Tasks 1-7.
- The differential is Task 8. The four printers are Task 9 and do not start before it.
- `Utf8` is a `Stmt`. Task 2 does not also put it on `WireOp`.
- `emit rust` and the rstom emitters are not rewritten.
