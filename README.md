# Bytemill

A schema language (BPS) and compiler for binary wire formats that Protobuf cannot express: variable-length integers, bit-packed fields, tagged unions, conditional layouts, and stateful packet routing.

Bytemill compiles schemas to idiomatic, zero-allocation decoders and encoders across nine languages: **Rust, Go, Python, Java, C#, C, C++, TypeScript, and Zig**.

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

## Use Cases

- **Game Network Protocols & Emulators**: Multiplayer protocols (e.g., Minecraft, Hytale) rely on variable-length integers (`varint`), bitfields, dynamic length prefixes, compression framing, and stateful direction tables that Protobuf cannot express. BPS specifies the entire packet hierarchy in one schema and emits typed codecs for clients, proxies, and servers.
- **Embedded, IoT, & Network Telemetry**: Binary telemetry streams, CAN bus frames, and network appliances use bit-packed fields, non-standard integer widths (12-bit signed coordinates, 3-byte ints), and explicit endianness. Bytemill maps these directly to native C, C++, Rust, or Zig structs without heap allocation or parsing overhead.
- **High-Throughput Polyglot Services**: When gRPC / Protobuf serialization overhead or dynamic reflection is a bottleneck, Bytemill produces zero-copy, zero-reflection decoders in nine languages (Go, Rust, C++, C, Python, Java, C#, TypeScript, Zig) from a single protocol definition, keeping cross-language services byte-compatible.
- **Protocol Reverse Engineering & Auditing**: Capture packet streams, formalize the wire format in a readable `.bps` specification, and immediately generate decoders and validation test suites across multiple languages, eliminating transcription bugs between reverse-engineered specs and working code.
---

## Quick Start

### Install `bml`

Every tagged release ships prebuilt binaries; the installer detects the platform, downloads
the matching archive, verifies `SHA256SUMS`, and installs to `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/mintychochip/bytemill/master/scripts/install.sh | bash
```

Upgrade later with `scripts/update.sh` (a no-op when already current):

```bash
curl -fsSL https://raw.githubusercontent.com/mintychochip/bytemill/master/scripts/update.sh | bash
```

Use `--dir <path>` to install elsewhere or `--version vX.Y.Z` to pin a release, e.g.
`... | bash -s -- --version v1.2.3 --dir /usr/local/bin`. The scripts need `curl` (or
`wget`), `tar`, and `sha256sum`/`shasum`.

Windows is not covered by the shell scripts: download `bml-x86_64-pc-windows-msvc.zip`
from Releases. That page also has the archives and `SHA256SUMS` for manual installs, and
`gh release download --repo mintychochip/bytemill` fetches them too.

### Build from source

Requires Rust 1.85 or newer (edition 2024):

```bash
cargo build --release   # target/release/bml
```

### Check and Inspect Schemas

```bash
ROOT=schemas/minecraft_26_3_wiki.bps

bml check $ROOT     # Parse, resolve, and type-check
bml routes $ROOT    # Print stateful packet routing table (261 routes)
bml stages $ROOT    # Syntax, semantics, lowered IR, and typed IR as JSON
bml targets         # Compatibility matrix: typed printers and accepted standards
bml dump-json $ROOT # Output the parsed AST as JSON
```

### CLI Decode / Encode

Decode a raw hex handshake frame:
```bash
echo '10008906096c6f63616c686f737463dd01' | bml decode $ROOT \
  --state handshaking --direction serverbound --hex
# {"id": 0, "packet": "handshake", "fields": {"intent": 1, "protocol_version": 777, ...}}
```

Round-trip VarInts:
```bash
echo 'ac0202' | bml decode schemas/minecraft_26_3_play.bps --packet animate --hex
# {"animation": 2, "entity_id": 300}

bml encode --packet animate schemas/minecraft_26_3_play.bps '{"entity_id": 300, "animation": 2}'
# ac0202
```

Minecraft's compression layer is a frame envelope, not a schema construct. `--compression zlib`
unwraps `packet_length, data_length, data` before decoding; `--compression zlib:<threshold>` adds
it on encode (packet data shorter than the threshold use `data_length = 0`; `zlib` alone
means threshold 0):

```bash
# Decode a compressed play frame (state/direction required).
bml decode $ROOT --state play --direction clientbound --compression zlib --hex

# Encode a compressed handshake frame.
bml encode $ROOT --packet handshake --state handshaking --direction serverbound \
  --compression zlib:0 '{"protocol_version":777,"server_address":"localhost","server_port":25565,"intent":1}'
# 1910789c63e864e3... (packet_length, data_length, zlib(id + payload))
```

With `--compression`, `decode` expects the full wire frame including the outer `packet_length`
(`packet_length, data_length, data`); `encode` emits that same shape. Only Minecraft-style frames
are accepted (`length: varint`, `id: varint`, `payload: rest`); other families such as Hytale's
`i32le` frame error out rather than silently reusing the varint envelope. Decode ignores the
threshold; the `data_length` field says whether a frame is raw or zlib. Hytale's zstd envelope is
not implemented.

---

## Code Generation

Emit typed, deterministic decoders and encoders for any target:

```bash
bml emit <target> <schema.bps> --out <out_dir> [--std <standard>]
# or specify standard directly on the target:
bml emit cpp:98 schemas/std/varint.bps --out ./bml_cpp
bml emit go:1.18 schemas/std/varint.bps --out ./bml_go
bml emit python:3.8 schemas/std/varint.bps --out ./bml_py
bml emit c:89 schemas/std/varint.bps --out ./bml_c
bml emit java:17 schemas/std/varint.bps --out ./bml_java
bml emit csharp:10 schemas/std/varint.bps --out ./bml_cs
```

| Target | Default Standard | Supported Versions | Output Layout | Idiomatic Types | Formatter & Lint Gate |
|---|---|---|---|---|---|
| `rust-typed` | 2024 | 2015, 2018, 2021, 2024 | Standalone crate (`src/packets/`) | Zero-clone borrow encode, `Option<T>`, `Result<T, Error>` | `cargo test`, `rustfmt`, `clippy -D warnings` |
| `go` | 1.23 | 1.16 → 1.24 | `bmlgen` package (`*.go`) | Structs, sealed interfaces for sums, preallocated slices | `go test`, `gofmt -l`, `go vet` |
| `python` | 3.11 | 3.8 → 3.14 | `bmlgen/` package | `@dataclass(slots=True)` (3.10+), `IntEnum`, union aliases | `compileall`, `ruff format/check`, `mypy --strict` |
| `java` | 21 | 8, 11, 17, 21 | `bmlgen/` package | Records + sealed interfaces (17+), classes (8/11), `Optional` | `java`/`javac` compile + roundtrip |
| `csharp` | 12 | 8, 9, 10, 11, 12 | `BmlGen/` package | `record struct` (10+), `record`/classes for recursive types, `List<T>` | `dotnet build` + roundtrip |
| `c` | C11 | C89, C90, C99, C11, C17, C23 | `bmlgen.h` + per-category `.c` | Structs + unions, `bml_status`, explicit allocators | `cc -Wall -Werror`, `clang-format` |
| `cpp` | C++20 | C++98, 03, 11, 14, 17, 20, 23, 26 | `bmlgen.hpp` + `.cpp` | `std::variant`, `std::optional`, `std::span` (polyfills on <17) | `g++ -std=c++20 -Werror`, `clang-format` |
| `typescript` | ES2022 | ES2015, ES2018, ES2020, ES2022, ESNext | ES modules (`.ts`) | `readonly` interfaces, discriminated unions, `Uint8Array` | `tsc --strict --erasableSyntaxOnly`, `prettier` |
| `zig` | 0.14 | 0.11, 0.12, 0.13, 0.14 | `bmlgen.zig` package | `union(enum)`, `?T`, `errdefer` unwinding, `@"keyword"` | `zig fmt --check`, `zig test` |

`bml targets` prints the target and standard matrix as JSON; `crates/bytemill/src/compat.rs` is the
source it reads, and `tests/test_compat.rs` pins every version against `EmitOptions::for_target`.
Every printer lowers schemas to a shared typed intermediate representation (`ir::typed::Program`) and outputs native types without dynamic reflection or string-keyed dictionaries. Generated codecs are verified byte-for-byte against the BPS reference interpreter (`ir::exec`).

---

## Shipped Schemas

- **Minecraft Java 26.3 (Protocol 777)**: Full reference target in `schemas/`. 232 packets, 261 routes, zero `body: rest` stubs. All vanilla registries (particles, slot displays, recipes, command argument types) fully modeled.
- **Protocol Versions (1.13 → 26.3)**: 39 protocols across 51 releases in `schemas/versions/`. Shared declarations deduplicated in `schemas/versions/common/`. Validated against an independent ProtoDef oracle; the oracle input (`data/minecraft-data/`) is fetched from pinned commits, not committed.
- **Hytale**: Connection and handshake packets in `schemas/hytale/`.
- **Standard Library**: Reusable fixed-width integers and variable-length integer layouts in `schemas/std/`.

---

## Downstream Targets: Minestom / rstom

Bytemill supports in-tree code generation for the Rust Minestom server (`rstom`):

```bash
# Standalone drop-in mirror crate for rstom-packets
bml emit rstom $ROOT --out ../rstom/crates/bytemill_rstom

# In-place generation within rstom workspace using AST shape ledgers
bml shapes extract $ROOT --rstom ../rstom-bps --eligible --out targets/rstom/shapes
bml emit rstom-tree $ROOT --rstom ../rstom-bps --shapes targets/rstom/shapes [--check]
```

See `docs/superpowers/specs/2026-10-11-bps-generates-rstom-packets-design.md` for shape ledgers and wire pin gates.

---

## Development

```bash
# Run test suite
scripts/bytemill_cargo.sh test --no-fail-fast

# Run workspace linter gate (hard gate in CI)
scripts/bytemill_cargo.sh clippy --workspace --all-targets -- -D warnings
```

`scripts/bytemill_cargo.sh` wraps `cargo` under a systemd user scope (`MemoryMax=8G`, 4 jobs, 4 test threads) to cap memory usage during large protocol oracle suites.

Generated inputs are not stored in the repository:

```bash
scripts/fetch_minecraft_data.py   # pinned ProtoDef protocol.json files (data/minecraft-data/pc)
scripts/extract_rstom_shapes.sh   # rstom shape ledger (targets/rstom/shapes)
```

Tests that need them skip with the matching command in the message when they are absent.
`build/` and `target/` are build output and stay untracked.

Pushing a `v*` tag runs `.github/workflows/release.yml`: it builds `bml` for every
supported platform, smoke-tests each binary, writes `SHA256SUMS`, and attaches the archives
to the GitHub Release. Manually dispatching the workflow builds the same artifacts without
creating a release.

---

## Documentation

- `PROJECT.md` — project status and the remaining goal queue
- `docs/DSL_STRUCTURE.md` — DSL syntax, operators, and framing rules
- `docs/AUTHORING_GUIDE.md` — Schema authoring and shared standard types
- `docs/TRANSCRIPTION_CHEATSHEET.md` — Wiki packet definitions to BPS transcription guide
- `docs/superpowers/specs/2026-10-15-bps-idiomatic-printers-design.md` — Codegen IR and typed printer design specification
