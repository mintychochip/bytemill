# Wirecraft

A schema language (BPS) and compiler for binary wire formats that Protobuf cannot express: variable-length integers, bit-packed fields, tagged unions, conditional layouts, and stateful packet routing.

Wirecraft compiles schemas to idiomatic, zero-allocation decoders and encoders across nine languages: **Rust, Go, Python, Java, C#, C, C++, TypeScript, and Zig**.

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
- **Embedded, IoT, & Network Telemetry**: Binary telemetry streams, CAN bus frames, and network appliances use bit-packed fields, non-standard integer widths (12-bit signed coordinates, 3-byte ints), and explicit endianness. Wirecraft maps these directly to native C, C++, Rust, or Zig structs without heap allocation or parsing overhead.
- **High-Throughput Polyglot Services**: When gRPC / Protobuf serialization overhead or dynamic reflection is a bottleneck, Wirecraft produces zero-copy, zero-reflection decoders in nine languages (Go, Rust, C++, C, Python, Java, C#, TypeScript, Zig) from a single protocol definition, keeping cross-language services byte-compatible.
- **Protocol Reverse Engineering & Auditing**: Capture packet streams, formalize the wire format in a readable `.bps` specification, and immediately generate decoders and validation test suites across multiple languages, eliminating transcription bugs between reverse-engineered specs and working code.
---

## Quick Start

### Install `bpsc`

Tagged releases ship prebuilt binaries, so using Wirecraft does not require a Rust
toolchain. Each release attaches stable-named archives plus `SHA256SUMS`:

| Platform | Asset |
|---|---|
| Linux x86_64 (glibc) | `bpsc-x86_64-unknown-linux-gnu.tar.gz` |
| Linux x86_64 (static) | `bpsc-x86_64-unknown-linux-musl.tar.gz` |
| Linux aarch64 (glibc) | `bpsc-aarch64-unknown-linux-gnu.tar.gz` |
| macOS aarch64 | `bpsc-aarch64-apple-darwin.tar.gz` |
| macOS x86_64 | `bpsc-x86_64-apple-darwin.tar.gz` |
| Windows x86_64 | `bpsc-x86_64-pc-windows-msvc.zip` |

Linux (swap the asset name for another platform):

```bash
base=https://github.com/mintychochip/wirecraft/releases/latest/download
curl -fsSLO "$base/bpsc-x86_64-unknown-linux-gnu.tar.gz"
curl -fsSLO "$base/SHA256SUMS"
grep bpsc-x86_64-unknown-linux-gnu.tar.gz SHA256SUMS | sha256sum -c -
tar xzf bpsc-x86_64-unknown-linux-gnu.tar.gz
sudo install -m 0755 bpsc /usr/local/bin/bpsc
```

macOS uses `shasum` instead of `sha256sum`:

```bash
base=https://github.com/mintychochip/wirecraft/releases/latest/download
curl -fsSLO "$base/bpsc-aarch64-apple-darwin.tar.gz"
curl -fsSLO "$base/SHA256SUMS"
grep bpsc-aarch64-apple-darwin.tar.gz SHA256SUMS | shasum -a 256 -c -
tar xzf bpsc-aarch64-apple-darwin.tar.gz
sudo install -m 0755 bpsc /usr/local/bin/bpsc
```

Windows (PowerShell):

```powershell
$base = "https://github.com/mintychochip/wirecraft/releases/latest/download"
Invoke-WebRequest "$base/bpsc-x86_64-pc-windows-msvc.zip" -OutFile bpsc.zip
Invoke-WebRequest "$base/SHA256SUMS" -OutFile SHA256SUMS
(Get-FileHash bpsc.zip -Algorithm SHA256).Hash  # must match the zip's line in SHA256SUMS
Expand-Archive bpsc.zip -DestinationPath "$env:USERPROFILE\bin" -Force
# add %USERPROFILE%\bin to PATH, then in a new terminal:
bpsc check schemas\std\varint.bps
```

GitHub CLI users can fetch the same assets with
`gh release download --repo mintychochip/wirecraft --pattern 'bpsc-*'`.

### Build from source

Requires Rust 1.85 or newer (the crate uses edition 2024):

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
bpsc emit <target> <schema.bps> --out <out_dir> [--std <standard>]
# or specify standard directly on the target:
bpsc emit cpp:98 schemas/std/varint.bps --out ./bps_cpp
bpsc emit go:1.18 schemas/std/varint.bps --out ./bps_go
bpsc emit python:3.8 schemas/std/varint.bps --out ./bps_py
bpsc emit c:89 schemas/std/varint.bps --out ./bps_c
bpsc emit java:17 schemas/std/varint.bps --out ./bps_java
bpsc emit csharp:10 schemas/std/varint.bps --out ./bps_cs
```

| Target | Default Standard | Supported Versions | Output Layout | Idiomatic Types | Formatter & Lint Gate |
|---|---|---|---|---|---|
| `rust-typed` | 2024 | 2015, 2018, 2021, 2024 | Standalone crate (`src/packets/`) | Zero-clone borrow encode, `Option<T>`, `Result<T, Error>` | `cargo test`, `rustfmt`, `clippy -D warnings` |
| `go` | 1.23 | 1.16 → 1.24 | `bpsgen` package (`*.go`) | Structs, sealed interfaces for sums, preallocated slices | `go test`, `gofmt -l`, `go vet` |
| `python` | 3.11 | 3.8 → 3.14 | `bpsgen/` package | `@dataclass(slots=True)` (3.10+), `IntEnum`, union aliases | `compileall`, `ruff format/check`, `mypy --strict` |
| `java` | 21 | 8, 11, 17, 21 | `bpsgen/` package | Records + sealed interfaces (17+), classes (8/11), `Optional` | `java`/`javac` compile + roundtrip |
| `csharp` | 12 | 8, 9, 10, 11, 12 | `BpsGen/` package | `record struct` (10+), `record`/classes for recursive types, `List<T>` | `dotnet build` + roundtrip |
| `c` | C11 | C89, C90, C99, C11, C17, C23 | `bpsgen.h` + per-category `.c` | Structs + unions, `bps_status`, explicit allocators | `cc -Wall -Werror`, `clang-format` |
| `cpp` | C++20 | C++98, 03, 11, 14, 17, 20, 23, 26 | `bpsgen.hpp` + `.cpp` | `std::variant`, `std::optional`, `std::span` (polyfills on <17) | `g++ -std=c++20 -Werror`, `clang-format` |
| `typescript` | ES2022 | ES2015, ES2018, ES2020, ES2022, ESNext | ES modules (`.ts`) | `readonly` interfaces, discriminated unions, `Uint8Array` | `tsc --strict --erasableSyntaxOnly`, `prettier` |
| `zig` | 0.14 | 0.11, 0.12, 0.13, 0.14 | `bpsgen.zig` package | `union(enum)`, `?T`, `errdefer` unwinding, `@"keyword"` | `zig fmt --check`, `zig test` |
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

Pushing a `v*` tag runs `.github/workflows/release.yml`: it builds `bpsc` for Linux
(x86_64 glibc/musl, aarch64), macOS (arm64/x86_64), and Windows (x86_64), smoke-tests each
binary, writes `SHA256SUMS`, and attaches the archives to the GitHub Release. Manually
dispatching the workflow builds the same artifacts without creating a release.

---

## Documentation

- `PROJECT.md` — project status and the remaining goal queue
- `docs/DSL_STRUCTURE.md` — DSL syntax, operators, and framing rules
- `docs/AUTHORING_GUIDE.md` — Schema authoring and shared standard types
- `docs/TRANSCRIPTION_CHEATSHEET.md` — Wiki packet definitions to BPS transcription guide
- `docs/superpowers/specs/2026-10-15-bps-idiomatic-printers-design.md` — Codegen IR and typed printer design specification
