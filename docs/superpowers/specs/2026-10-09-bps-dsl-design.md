# BPS — Binary Protocol Schema: design

**Status:** approved direction (user decisions: Python 3.11+ stdlib-only front-end; `bps`/`bml`
naming; full Minecraft 26.3 packet surface; Rust code generation).
**Date:** 2026-10-09

## 1. Goal

A schema language plus compiler that can describe *arbitrary* binary wire formats — varints,
bit-packed integers, length- and terminator-delimited sequences, type-tagged unions, and
state/version-dependent dispatch — which protobuf descriptors structurally cannot express.
Validated end-to-end against **Minecraft Java Edition 26.3, protocol 777**.

v1 deliverables:

1. **Language + Python compiler** (`bps` package, stdlib only): lexer, parser, semantic
   analysis, `schema.json` emitter.
2. **Reference interpreter** (Python): symmetric decode/encode over the resolved AST; powers the
   CLI. The interpreter is the semantic definition.
3. **Rust emitter** (`bml emit rust`): any `.bps` schema compiles to a dependency-free Rust
   crate with per-encoding read/write functions and a routing dispatcher.
4. **`schemas/minecraft_26_3.bps`**: the complete protocol-777 surface — every state, both
   directions, every packet, and the encodings they reference (NBT, slots, entity metadata,
   chunk data, particles, registry references, etc.).
5. **Verification**: golden vectors derived from the published wiki tables, round-trip tests per
   packet, and Python-vs-generated-Rust differential tests.

## 2. Non-goals (v1)

- Codegen targets other than Rust.
- Session-level stream decoding that auto-tracks connection state (phase 2; `bml decode` takes
  explicit `--state/--direction`).
- Version overlays/diffing (`@26.2`) — one schema file per version in v1.
- Crypto/compression codecs. The schema describes the logical stream on one side of encryption
  and outside the zlib-compressed envelope; framing options record where those boundaries sit.
- Registry content databases (Burger-generated registries). Registry IDs are opaque integers in
  v1.
- Codecs for *other* file formats (world files, region files) unless a packet references them.
- Performance targets; correctness and round-trip fidelity first.

## 3. Why `.proto` fails (problem analysis)

Facts from the 26.3 spec (minecraft.wiki, protocol 777):

| Protocol concept | `.proto` capability | Minecraft 26.3 reality |
| --- | --- | --- |
| Integer encoding | field number fixes wire type (varint/64/32) | same value sent as VarInt, `u16be`, bit-packed 26-bit, or fixed-point |
| Framing | no framing concept | per-packet `VarInt` byte-length prefix, then `VarInt` packet ID |
| Dispatch | one message per field type | packet ID scoped by **connection state × direction**; `play` and `configuration` share one ID table |
| Presence | proto3 lacks presence | `Boolean`-prefixed optional, ID-or-X `0` sentinel, flag-mask conditional |
| Termination | none | sentinel (`until 0xFF` in entity metadata), rest-of-payload, context count |
| Bit packing | impossible | Position 64-bit BE split `x:26 \| z:26 \| y:12`; FixedBitSet `ceil(n/8)` |
| Composition | messages by reference only | self-recursive NBT trees, type-tagged unions (metadata types 0–23) |
| Validation | none | length limits (String(255), UTF-16 unit budget), protocol version checks |

VarInt itself is LEB128 with two's-complement sign (negative values always max-length, ≤5 bytes;
VarLong ≤10) — similar to protobuf varints but not interchangeable. The descriptor model is the
limitation, not the tooling.

## 4. Language spec

Files: `*.bps`. Lexical: `//` line comments, `/* */` block comments; identifiers
`[a-z_][a-z0-9_]*` (case sensitive); integers `123`, `0x1F`; strings `"..."`; punctuation as
used below. One declaration per top-level item; declaration order is irrelevant (forward
references allowed).

### 4.1 Declarations

```
import "path.bps"
encoding NAME(param: default, ...) = encoding_body
enum NAME : primitive_ref { member = int, ... }
packet NAME { field* }
protocol NAME @ VERSION { id = int  frame* route* }
```

- `encoding`: a reusable, parameterizable byte layout. Bodies may reference parameters and
  earlier fields by name.
- `enum`: named integer domain; usable as a field type; `match` arms may use member names.
- `packet`: a top-level payload structure (fields only, no enum/encoding nesting).
- `protocol`: names the version (`26.3`), the protocol number (`id = 777`), frame templates, and
  the routing table.
- `import`: makes another `.bps` file's declarations visible in this file. The path is
  relative to the importing file and each file is loaded once. A file sees its own
  declarations and those of the files it imports directly; imports are not transitive
  (the `.proto` rule), so every name a file uses traces to an `import` in that file.
  A name is still declared in exactly one file.

### 4.2 Encoding bodies

```
encoding_body := primitive_ref ('as' transform)*
                | '{' (field | match_stmt | require_stmt)* '}' ('=' expression)?

field         := NAME ':' type modifiers? write_expr?   // wire field: consumes bytes
                | NAME '=' expression            // computed value: consumes nothing
type          := primitive_ref | NAME | block | match_ref
modifiers    := 'if' expression                  // conditional presence
                | '[' ']' ('count' '=' expression
                          | 'until_end'
                          | 'until' expression)  // repetition
match_ref     := ('match' | 'switch') expression '{' arm (',' arm)* '}'   // field type
match_stmt    := ('match' | 'switch') expression '{' (tags '=>' block)* '}'   // statement
arm           := tags '=>' type
tags          := '_' | tag ('|' tag)*
tag           := int | enum_member
write_expr    := '=' expression                  // scalar encodings only, see §4.2
transform     := 'utf8' | 'fixed' '(' expression ')' | 'mutf8'
require_stmt  := 'require' expression            // assertion, see §4.5
```

Blocks are also types: an encoding may be declared inline (`{ a: u8, b: varint }`) or referenced
by name; a named encoding with parameters is instantiated with `NAME(args)`; a parameterized
encoding can be spread into a block with `...NAME(args)`.

Primitives are the only types the compiler implements: `u8`, `f32be`, `f64be`, `bytes(expr)`, and
`rest`. Floats stay primitive because IEEE-754 is not integer arithmetic over bytes. Every other
fixed-width integer, `boolean`, `varint`, and `varlong` is an ordinary encoding in `schemas/std/`
(see *Scalar encodings*). `bytes(n)` reads exactly `n` bytes; `rest` reads to the end of the
enclosing length-bounded scope (packet payload or an enclosing `bytes(...)` scope — never silently
crosses it).

**Scalar encodings.** A block followed by `= expr` is an encoding whose value is `expr`, not the
field dictionary. Decode reads the block, then evaluates `expr` over its fields; an optional field
that was not read is `0`. Encode binds the scalar being written to `value`, and a wire field's own
`= expr` is the byte it writes. `schemas/std/` builds its integers and LEB128 this way:

```
encoding u16be = {
    b0: u8 = (value >> 8) & 0xFF
    b1: u8 = value & 0xFF
} = (b0 << 8) | b1

encoding i16be = {
    raw: u16be = value & 0xFFFF
} = sign_extend(raw, 16)
```

The Rust emitters lower the std scalars by name, to runtime helpers that the differential tests
compare against the interpreter, and refuse any other scalar encoding a packet reaches.

### 4.3 Worked example (abridged; the full tree is `schemas/minecraft_26_3.bps`)

File organization: version-stable encodings live in `schemas/minecraft_core.bps`; a version file
(`minecraft_26_3.bps`) imports it and adds version-specific packets/routes. The example below
mixes both for brevity. `match` arms are integer literals or members of an enum declared for the
tag's domain (here `enum metadata_kind : varint { byte = 0, varint = 1, … }`).

```
import "std/integers.bps"                 // i8, u16be, ..., boolean: scalars built from u8
import "std/varint.bps"                   // VarInt (<= 5 bytes), VarLong (<= 10): LEB128 from u8

encoding string(max_chars = 32767) {      // String(n)
    byte_len: varint
    value:    bytes(byte_len) as utf8
    require   utf16_units(value) <= max_chars
}

encoding position = {                     // 64-bit BE: x:26 | z:26 | y:12, all signed
    raw: u64be
    x = sign_extend(raw >> 38, 26)
    z = sign_extend(raw >> 12, 26)
    y = sign_extend(raw, 12)
}

encoding entity_metadata = { entries: meta_entry[] until 0xFF }

encoding meta_entry = {
    index: u8
    kind:  varint
    value: match kind {
        byte        => i8
        varint      => varint
        varlong     => varlong
        float       => f32be
        string      => string(32767)
        text        => nbt
        opt_text    => { present: boolean, text: nbt if present }
        slot        => slot
        boolean     => boolean
        rotations   => { rx: f32be, ry: f32be, rz: f32be }
        position    => position
        opt_position=> { present: boolean, pos: position if present }
        direction   => varint
        opt_owner   => { present: boolean, owner: uuid if present }
        block_state => varint
        opt_block   => optional_blockstate
        particle    => particle
        particles   => { n: varint, items: particle[] count = n }
        villager    => { a: varint, b: varint, c: varint }
        opt_varint  => optional_varint
        pose        => varint
        _           => varint           // 21..23 registry IDs
    }
}

packet handshake {
    protocol_version: varint
    server_address:   string(255)
    server_port:      u16be
    intent:           intent
    require           protocol_version == 777
}

protocol minecraft@26.3 {
    id = 777
    frame default    { length: varint; id: varint; payload: rest }
    frame no_length  { id: u8; payload: rest }          // legacy server list ping

    route handshaking.serverbound 0x00 => handshake
    route handshaking.serverbound 0xFE => legacy_slp frame no_length
    route status.clientbound      0x00 => status_response
    route status.serverbound      0x01 => ping_request
    route login.serverbound       0x00 => login_start
    route play.clientbound        0x2D => keep_alive
    ...
}
```

### 4.4 Expressions

Used in sizes, counts, conditions, computed values, and `require`. Whitelisted subset:

- literals: int, hex, string, `true`/`false`; field/member access `a.b` (nested paths allowed);
- arithmetic `+ - * / %`, comparison `== != < <= > >=`, logical `&& || !`,
  bitwise `& | ^ << >>`, ternary `c ? a : b`;
- functions: `sign_extend(v, bits)`, `utf16_units(s)`, `len(x)`, `min/max`, `contains(x, c)`
  (character membership in a string — the v1 substitute for regex checks, so constraints stay
  emittable to dependency-free Rust).

Integer semantics: arbitrary-precision on the Python side; emitted Rust uses `i64`/`u64` and
masking helpers so `26`-bit fields behave identically (see §7).

Integer division truncates toward zero in both implementations (documented; `/` on negatives is a
rejected input in v1 schemas — resolver warns).

### 4.5 Semantics

Decode, per scope, in order:

1. Wire fields consume bytes in declaration order.
2. `if` modifiers skip the field when the expression is false (value: absent).
3. Repetition: `count = e` reads `e` items; `until_end` consumes to scope end; `until e` reads
   items until the next value equals `e`, then consumes and drops that terminator byte/value.
4. `match` (synonym `switch`) dispatches the tag value to an arm; `A | B =>` shares one arm and
   `_` is the fallback wherever it sits; no arm and no `_` → `DecodeError`. As a statement
   (`match (tag) { 0 => { x: u8 } }`) an arm is a block whose fields splice into the enclosing
   value, like an `if` group. As a field type (`v: match tag { 0 => i8 }`) an arm is one type.
5. Computed `=` fields evaluate after the wire fields they reference, in declaration order.
6. `require` evaluates immediately after the enclosing scope is read; failure → `DecodeError`
   with byte offset.
7. A scalar encoding yields its trailing `= expr`, evaluated after its block and its `require`s.

Encode: wire fields serialize in declaration order; `if`/repetition driven by the same
expressions evaluated on the *value being encoded*; `require` validates before writing;
computed fields are **not** serialized (they are read-side derivations; to encode a `position`,
set `raw`). This is a deliberate, documented simplification — the alternative (automatic inverse
solving) is deferred. Scalar encodings are the exception: they carry their own inverse, the
per-field `= expr` writes.

Errors: `BpsError` hierarchy (`ParseError`, `ResolveError`, `DecodeError`, `EncodeError`) with
source positions and byte offsets. Decode never raises `KeyError`/`IndexError` uncaught; any malformed
input yields `DecodeError`.

## 5. Standard library

Built-in primitives and transforms (§4.2), helper functions (§4.4), the std scalars in
`schemas/std/` (`integers.bps`: `i8`, `u16be`, …, `boolean`; `varint.bps`: `varint`, `varlong`),
plus the Minecraft encoding library shipped as a `.bps` file (`schemas/minecraft_26_3.bps` and a
shared `schemas/minecraft_core.bps` for encodings reusable across versions): `string`, `identifier`,
`uuid`, `angle`, `position`, `lp_vec3`, `bitset`, `fixed_bitset`, `id_or`,
`entity_metadata`, `slot`, `hashed_slot`, `nbt`, `particle`, `optional_varint`,
`optional_blockstate`, `game_profile`, `resolvable_profile`, `chat_type`, `sound_event`,
`teleport_flags`, `recipe_display`, `slot_display`, `light_data`, `lp_vec3`, `debug_subscription_*`
as needed by the packet surface.

## 6. Minecraft 26.3 schema scope

- States: `handshaking`, `status`, `login`, `configuration`, `play`. Both directions; every
  packet in the wiki's protocol-777 tables, including the legacy server-list-ping framing
  exception (`no_length`, `u8` ID, no length prefix).
- Play packets 0x00–0x44 clientbound and the full serverbound table per the wiki.
- Complex payloads transcribed from their dedicated pages: Slot data, Entity metadata, NBT
  (network variant; file/name-tagged NBT only where a packet needs it), Chunk format, Particles,
  Registry data (as opaque ID references), Light data, Debug subscriptions.
- Protocol number `777`; handshake `require protocol_version == 777` documents the version.

Transcription risk is the dominant risk of v1 (see §9).

## 7. Architecture

```
bps/
  __init__.py      version/exports
  errors.py        BpsError hierarchy
  ast.py           frozen dataclasses: Schema, Encoding, Enum, Packet, Protocol, Route, Field...
  lexer.py         hand-written tokenizer
  parser.py        recursive descent -> AST
  load.py          import resolution: files -> one Schema, each file included once
  primitives.py    the five compiler primitives (u8, f32be, f64be, bytes, rest)
  analyze.py       symbol tables, parameter binding, cycle detection, reachability
  emit_json.py     schema.json (stable, versioned)
  interp/
    cursor.py      byte cursor + CursorError
    codecs.py      primitive/composite decode+encode
    evalexpr.py    expression evaluator (decode + encode directions)
  emit_rust.py     AST -> dependency-free Rust crate
  cli.py           bml entry point
schemas/
  std/                      # integers.bps, varint.bps: scalars built from u8
  minecraft_core.bps        # shared encodings
  minecraft_26_3.bps        # imports core; full protocol 777
tests/
  test_lexer_parser.py  test_analyze.py  test_interp.py  test_roundtrip.py
  golden/               # wiki-derived vectors (hex + expected JSON)
  test_emit_rust.py     # differential: interp vs generated Rust
```

Pipeline: source → tokens → AST → resolved AST → {interpreter | schema.json | Rust crate}.
`schema.json` is the interchange artifact so external tools (fuzzers, generators, docs) consume a
frozen schema instead of forking the language.

### Rust emitter

Generated crate layout:

```
minecraft_26_3/
  src/lib.rs        # State enum, Direction, Packet enum, decode_frame/dispatch
  src/runtime.rs    # Cursor, varint read/write, sign_extend, utf16_units, masks
  src/encodings.rs  # one struct/enum + read/write fn per encoding
  src/packets.rs    # packet structs
  Cargo.toml        # no dependencies
  tests/golden.rs   # same vectors as the Python side
```

Mapping rules: encoding block → struct with `read`/`write` methods; `match` → enum with named
variants; repetition → `Vec<T>`; computed fields → struct fields populated at read, skipped at
write; `require` → `Result::Err` on both paths; expressions → Rust expressions over `i64`/`u64`
with explicit masks.

## 8. CLI (`bml`)

```
bml check schema.bps ...                 # parse + resolve + report
bml dump-json schema.bps                 # emit schema.json
bml routes schema.bps                    # print routing table
bml decode schema.bps --state play --direction clientbound < packet.hex
bml encode schema.bps --state play --direction clientbound < packet.json
bml emit rust schema.bps --out out/      # generated crate
```

Exit codes: 0 ok, 1 usage, 2 schema errors, 3 decode/encode errors. `decode`/`encode` accept
`--hex` or stdin; output JSON is stable-key-sorted for diffing.

## 9. Verification strategy

1. **Golden vectors**, each with source citation in a comment:
   - VarInt samples (`25565 → dd c7 01`, `-1 → ff ff ff ff 0f`, …) and VarLong samples from the
     wiki table.
   - Position example (`x=18357644, z=-20882616, y=831`) from the wiki's 64-bit layout.
   - Handshake frame for protocol 777: `10 00 89 06 09 6c6f63616c686f7374 63dd01`
     (len=16, id=0, 777, "localhost", port 25565, intent 1) — computed, not hand-copied.
   - Status/ping/pong 8-byte timestamp frames; login start; keep-alive.
2. **Round-trip**: for every route in the protocol block, a synthetic value is encoded then
   re-decoded and compared; plus `encode(decode(bytes)) == bytes` for fixtures.
3. **Differential**: every golden vector runs through the Python interpreter *and* the generated
   Rust crate; both must produce identical JSON-shaped values.
4. **Coverage check**: test asserts every `route` has at least one round-trip fixture; a packet
   without a fixture fails CI, which forces the schema to stay complete as packets are added.
5. **CLI smoke**: decode a multi-packet hex stream end-to-end and show decoded output.

## 10. Risks

| Risk | Mitigation |
| --- | --- |
| Wiki transcription errors for ~70 play packets | Two-pass transcription (per-state batches), golden vectors pinned from wiki sample tables, coverage test blocks unrouted packets |
| Emitter complexity for `match`/computed/expressions | Emitter owns a small, mechanical mapping; expressions restricted to whitelisted ops evaluated identically in interp and Rust |
| Integer width divergence (Python bignum vs Rust i64) | All wire math in `i64`/`u64` with explicit masks in the Rust runtime; the std `varint`/`varlong` schemas mask the scalar to its width before writing |
| Chunk-data per-version instability | Encode structure now (bit-unpacked long arrays via computed fields); palette semantics can refine without language changes |
| Scope creep in expressions | Whitelist + resolver rejects unknown functions |

## 11. Roadmap after v1

Session-level stream decoding (state machine driven by `handshake.intent`); version overlays and
diffing (`@26.2` route tables); additional codegen targets; registry tables from Burger exports;
fuzz + differential harness against vanilla captures; a second protocol family (Hytale) as the
language-generality proof.
