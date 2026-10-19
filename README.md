# Wirecraft

A schema language (BPS) and compiler for binary wire formats that Protobuf cannot express: variable-length integers, bit-packed fields, tagged unions, conditional layouts, and stateful packet routing.

Wirecraft compiles schemas to idiomatic, zero-allocation decoders and encoders across seven languages: **Rust, Go, Python, C, C++, TypeScript, and Zig**.

---

## The DSL

A `.bps` schema has three core layers:

1. **`encoding`** — byte-level layouts (`varint`, `string`, `position`, `nbt`)
2. **`packet` / `enum`** — logical message payloads
3. **`protocol`** — packet framing and stateful routing (`route state.direction id => packet`)

Constructs include tagged unions (`match kind { 0 => i8, 1 => varint, ... }`), repeats (`[] count = expr` or `[] until expr`), conditionals (`if cond { ... }`), computed fields (`name = expr`), assertions (`require expr`), and inlining (`...encoding(args)`).

```bps
// Bit-packed 64-bit coordinate with sign-extended fields
encoding position = {
    raw: u64be
    x = sign_extend(raw >> 38, 26)
    y = sign_extend(raw % 4096, 12)
    z = sign_extend((raw >> 12) % 67108864, 26)
}

// Tagged union
encoding meta_entry = {
    index: u8
    kind:  varint
    value: match kind {
        0 => i8
        1 => varint
        2 => varlong
        3 => f32be
        4 => string(32767)
        8 => boolean
        _ => varint
    }
}

// Packet with conditional fields and sub-encodings
packet player_chat {
    global_index:  varint
    sender:        uuid
    index:         varint
    has_signature: boolean
    signature:     bytes(256) if has_signature
    message:       string(256)
    timestamp:     i64be
}
```

The compiler implements five built-in primitives: `u8`, `bytes(n)`, `rest`, `f32be`, `f64be`. Every other type is declared in `.bps` schemas and imported via non-transitive imports:

```bps
import "std/integers.bps"      // i8, u16be, i32be, boolean, ...
import "std/varint.bps"        // varint, varlong
import "minecraft_core.bps"    // string, uuid, position, nbt, slot
```

---

## Quick Start

### Build the CLI

```bash
cargo build --release
# binary produced at target/release/bpsc
```

### Check and Inspect Schemas

```bash
ROOT=schemas/minecraft_26_3_wiki.bps

bpsc check $ROOT     # Parse, resolve, and type-check
bpsc routes $ROOT    # Print stateful packet routing table (261 routes)
bpsc dump-json $ROOT # Output AST / IR as JSON
```

### CLI Decode / Encode

Decode a raw hex handshake frame:
```bash
echo '10008906096c6f63616c686f737463dd01' | bpsc decode $ROOT \
  --state handshaking --direction serverbound --hex
# {"id": 0, "packet": "handshake", "fields": {"intent": 1, "protocol_version": 777, ...}}
```

Round-trip VarInts:
```bash
echo 'ac0202' | bpsc decode schemas/minecraft_26_3_play.bps --packet animate --hex
# {"animation": 2, "entity_id": 300}

bpsc encode --packet animate schemas/minecraft_26_3_play.bps '{"entity_id": 300, "animation": 2}'
# ac0202
```

---

## Code Generation

Emit typed, deterministic decoders and encoders for any target:

```bash
bpsc emit <target> <schema.bps> --out <out_dir>
```

| Target | Language | Output Layout | Idiomatic Types | Formatter & Lint Gate |
|---|---|---|---|---|
| `rust-typed` | Rust (2024) | Standalone crate (`src/packets/`) | Zero-clone borrow encode, `Option<T>`, `Result<T, Error>` | `cargo test`, `rustfmt`, `clippy -D warnings` |
| `go` | Go (1.23) | `bpsgen` package (`*.go`) | Structs, sealed interfaces for sums, preallocated slices | `go test`, `gofmt -l`, `go vet` |
| `python` | Python (3.11+) | `bpsgen/` package | `@dataclass(slots=True)`, `IntEnum`, union aliases, `__all__` | `compileall`, `ruff format/check`, `mypy --strict` |
| `c` | C11 | `bpsgen.h` + per-category `.c` | Structs + anonymous unions, `bps_status`, explicit allocators | `cc -Wall -Werror`, `clang-format` |
| `cpp` | C++20 | `bpsgen.hpp` + `.cpp` | `std::variant`, `std::optional`, `std::span`, `[[nodiscard]]` | `g++ -std=c++20 -Werror`, `clang-format` |
| `typescript` | TypeScript | ES modules (`.ts`) | `readonly` interfaces, discriminated unions, `Uint8Array` | `tsc --strict --erasableSyntaxOnly`, `prettier` |
| `zig` | Zig (0.14) | `bpsgen.zig` package | `union(enum)`, `?T`, `errdefer` unwinding, `@"keyword"` | `zig fmt --check`, `zig test` |

Every printer lowers schemas to a shared typed intermediate representation (`ir::typed::Program`) and outputs native types without dynamic reflection or string-keyed dictionaries. Generated codecs are verified byte-for-byte against the BPS reference interpreter (`ir::exec`).

---

## Shipped Schemas

- **Minecraft Java 26.3 (Protocol 777)**: Full reference target in `schemas/`. 232 packets, 261 routes, zero `body: rest` stubs. All vanilla registries (particles, slot displays, recipes, command argument types) fully modeled.
- **Protocol Versions (1.13 → 26.3)**: 39 protocols across 51 releases in `schemas/versions/`. Shared declarations deduplicated in `schemas/versions/common/`. Validated against independent ProtoDef oracle.
- **Hytale**: Connection and handshake packets in `schemas/hytale/`.
- **Standard Library**: Reusable fixed-width integers and variable-length integer layouts in `schemas/std/`.

---

## Downstream Targets: Minestom / rstom

Wirecraft supports in-tree code generation for the Rust Minestom server (`rstom`):

```bash
# Standalone drop-in mirror crate for rstom-packets
bpsc emit rstom $ROOT --out ../rstom/crates/bps_rstom

# In-place generation within rstom workspace using AST shape ledgers
bpsc shapes extract $ROOT --rstom ../rstom-bps --eligible --out targets/rstom/shapes
bpsc emit rstom-tree $ROOT --rstom ../rstom-bps --shapes targets/rstom/shapes [--check]
```

See `docs/superpowers/specs/2026-10-11-bps-generates-rstom-packets-design.md` for shape ledgers and wire pin gates.

---

## Development

```bash
# Run test suite
scripts/bps_cargo.sh test --no-fail-fast

# Run workspace linter gate (hard gate in CI)
scripts/bps_cargo.sh clippy --workspace --all-targets -- -D warnings
```

`scripts/bps_cargo.sh` wraps `cargo` under a systemd user scope (`MemoryMax=8G`, 4 jobs, 4 test threads) to cap memory usage during large protocol oracle suites.

---

## Documentation

- `docs/DSL_STRUCTURE.md` — DSL syntax, operators, and framing rules
- `docs/AUTHORING_GUIDE.md` — Schema authoring and shared standard types
- `docs/TRANSCRIPTION_CHEATSHEET.md` — Wiki packet definitions to BPS transcription guide
- `docs/superpowers/specs/2026-10-15-bps-idiomatic-printers-design.md` — Codegen IR and typed printer design specification
