# Bytemill

A schema language (BML) and compiler for binary wire formats that Protobuf cannot express: variable-length integers, bit-packed fields, tagged unions, conditional layouts, and stateful packet routing.

Bytemill compiles schemas to idiomatic, zero-allocation decoders and encoders across nine languages: **Rust, Go, Python, Java, C#, C, C++, TypeScript, and Zig**.

---

## The DSL

A `.bml` schema has three core layers:

1. **`encoding`** — byte-level layouts (`varint`, `string`, `position`, `nbt`, `f32le`, …)
2. **`packet` / `enum`** — logical message payloads
3. **`protocol`** — packet framing and stateful routing (`route state.direction id => packet`)

Constructs include tagged unions (`match kind { 0 => i8, 1 => varint, ... }`), repeats (`[] count = expr` or `[] until expr`), conditionals (`if cond { ... }`), computed fields (`name = expr`), assertions (`require expr`), and inlining (`...encoding(args)`).

```bml
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

The compiler implements seven built-in primitives: `u8`, `bytes(n)`, `rest`, `f32be`, `f64be`, `f32le`, and `f64le`. Every other type is declared in `.bml` schemas and imported via non-transitive imports (a schema may declare its own `encoding f32le`, which shadows the primitive, same as for `f32be`):

```bml
import "std/integers.bml"      // i8, u16be, i32be, boolean, ...
import "std/varint.bml"        // varint, varlong
import "minecraft_core.bml"    // string, uuid, position, nbt, slot
```

---

## Use Cases

- **Game Network Protocols & Emulators**: Multiplayer protocols (e.g., Minecraft, Hytale) rely on variable-length integers (`varint`), bitfields, dynamic length prefixes, compression framing, and stateful direction tables that Protobuf cannot express. A single BML schema specifies the entire packet hierarchy, and Bytemill emits typed codecs for clients, proxies, and servers.
- **Embedded, IoT, & Network Telemetry**: Binary telemetry streams, CAN bus frames, and network appliances use bit-packed fields, non-standard integer widths (12-bit signed coordinates, 3-byte ints), and explicit endianness. Bytemill maps these directly to native C, C++, Rust, or Zig structs without heap allocation or parsing overhead.
- **High-Throughput Polyglot Services**: When gRPC / Protobuf serialization overhead or dynamic reflection is a bottleneck, Bytemill produces zero-copy, zero-reflection decoders in nine languages (Go, Rust, C++, C, Python, Java, C#, TypeScript, Zig) from a single protocol definition, keeping cross-language services byte-compatible.
- **Protocol Reverse Engineering & Auditing**: Capture packet streams, formalize the wire format in a readable `.bml` specification, and immediately generate decoders and validation test suites across multiple languages, eliminating transcription bugs between reverse-engineered specs and working code.
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

Run `bml help` to list every command, and `bml help <command>` (or `bml <command> --help`) for
that command's options.

```bash
ROOT=schemas/minecraft_26_3_wiki.bml

bml check $ROOT     # Parse, resolve, and type-check
bml routes $ROOT    # Print stateful packet routing table (261 routes)
bml stages $ROOT    # Syntax, semantics, lowered IR, and typed IR as JSON
bml targets         # Compatibility matrix: typed printers and accepted standards
bml dump-json $ROOT # Output the parsed AST as JSON
```

Invalid schemas print rustc-style diagnostics (`error: …`, ` --> file:line:col`, source line, caret, and sometimes `help:`). Several errors appear in source order. Machine-readable output uses `bml check --format json` (one JSON object with sorted keys; `help` omitted when absent). A valid root still prints `ok` and exits 0:

```bash
bml check schemas/std/varint.bml
# ok

bml check --format json /path/to/bad.bml
# {"diagnostics": [{"column": 18, "end_column": 30, "end_line": 1, "file": "/path/to/bad.bml", "help": "'unknown_type' (used in packet test) is declared in no loaded or nearby .bml file", "line": 1, "message": "'unknown_type' is not declared here or in an imported file", "severity": "error"}], "ok": false}
```

The `bytemill::diag` module exposes the same information (`diagnose_source`, `diagnose_files`, `render`, `to_json`). Known limits: a missing import file is reported without a caret; a missing `}` at end of file points at the last field; the parser reports a missing expression operand as `expected a name`.

### CLI Decode / Encode

Decode a raw hex handshake frame:
```bash
echo '10008906096c6f63616c686f737463dd01' | bml decode $ROOT \
  --state handshaking --direction serverbound --hex
# {"id": 0, "packet": "handshake", "fields": {"intent": 1, "protocol_version": 777, ...}}
```

Round-trip VarInts:
```bash
echo 'ac0202' | bml decode schemas/minecraft_26_3_play.bml --packet animate --hex
# {"animation": 2, "entity_id": 300}

bml encode --packet animate schemas/minecraft_26_3_play.bml '{"entity_id": 300, "animation": 2}'
# ac0202
```

Field byte ranges and annotated dumps (`--spans` and `--hexdump` are mutually exclusive; exit 2 if both are set):

```bash
echo 'ac0202' | bml decode schemas/minecraft_26_3_play.bml --packet animate --hex --spans
# {"fields": {"animation": 2, "entity_id": 300}, "spans": {"animation": {"len": 1, "offset": 2}, "entity_id": {"len": 2, "offset": 0}}}

echo 'ac0202' | bml decode schemas/minecraft_26_3_play.bml --packet animate --hex --hexdump
# 0000    ac 02                  entity_id = 300
# 0002    02                     animation = 2
```

Frame mode adds `frame` (`length`, `id`) and span offsets for the header; offsets are absolute within the frame (after `--compression` unwraps the envelope). Plain `decode` JSON without these flags is unchanged.

```bash
echo '10008906096c6f63616c686f737463dd01' | bml decode $ROOT \
  --state handshaking --direction serverbound --hex --spans
# {"fields": {...}, "frame": {"id": 0, "length": 16}, "id": 0, "packet": "handshake", "spans": {...}}
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

### Trace a captured session

`bml trace` replays a JSONL capture (one object per line with `direction` and `hex`; `toServer` / `toClient` also work). Each line is one framed chunk on the wire (for Minecraft: `packet_length` varint, `id` varint, then payload). The tracer steps the protocol state machine (handshaking → status or login → configuration → play), learns the compression threshold, and optionally decrypts with `--secret <32 hex digits>` (AES-128-CFB8, key = IV). It prints one JSON event per line: packet lines carry `packet` (no `event` key); other events use `event` (`state`, `compression`, `encryption`, `error`, `incomplete`). Exit 3 if any `error` event was printed; `incomplete` at end of a truncated capture is informational (exit 0). Minecraft-style routed frames only — Hytale and other families error out; there is no pcap/pcapng reader (convert captures to JSONL first). Transitions key on packet **names** from the loaded schema; the hand-written 26.3 root requires protocol 777.

Build a minimal capture from `bml encode` plus a framed handshake:

```bash
# Body from encode; frame from README quick start (length 0x10, id 0x00, payload).
echo '{"direction":"serverbound","hex":"10008906096c6f63616c686f737463dd01"}' \
  | bml trace schemas/minecraft_26_3_wiki.bml
# {"direction": "serverbound", "fields": {"intent": 1, "protocol_version": 777, "server_address": "localhost", "server_port": 25565}, "id": 0, "packet": "handshake", "seq": 1, "state": "handshaking"}
# {"cause": "handshake", "event": "state", "from": "handshaking", "seq": 2, "to": "status"}
```

---

## Code Generation

Emit typed, deterministic decoders and encoders for any target:

```bash
bml emit <target> <schema.bml> --out <out_dir> [--std <standard>]
# or specify standard directly on the target:
bml emit cpp:98 schemas/std/varint.bml --out ./bml_cpp
bml emit go:1.18 schemas/std/varint.bml --out ./bml_go
bml emit python:3.8 schemas/std/varint.bml --out ./bml_py
bml emit c:89 schemas/std/varint.bml --out ./bml_c
bml emit java:17 schemas/std/varint.bml --out ./bml_java
bml emit csharp:10 schemas/std/varint.bml --out ./bml_cs
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
Every printer lowers schemas to a shared typed intermediate representation (`ir::typed::Program`) and outputs native types without dynamic reflection or string-keyed dictionaries. Generated codecs are verified byte-for-byte against Bytemill's reference interpreter (`ir::exec`).

### Wire vectors

`bml gen` draws JSONL wire vectors `{"direction","hex","id","packet","state"}` that round-trip in the reference interpreter (`skipped <packet>: <reason>` and a summary on stderr). Pipe them into `bml wire-check` to close the loop:

```bash
bml gen schemas/minecraft_26_3_wiki.bml 2>/dev/null | bml wire-check schemas/minecraft_26_3_wiki.bml
# checked 664, failed 0

bml gen schemas/minecraft_26_3_wiki.bml 2>&1 | tail -1
# 664 vectors for 232 packets, 0 packets skipped
```

`--count` sets vectors per packet (default 3); `require` fields pin values (e.g. `protocol_version == 777` fixes `handshake`). Vectors where the interpreter skips a conditional group at EOF although its condition holds are dropped (typed codecs disagree on that wire form; see `PROJECT.md` B8).

Typed printers accept `bml emit <target> <schema> --out <dir> --vectors` to embed the same vectors as native literals plus a round-trip test. Example for `schemas/hytale/2026_02_17.bml` (12 vectors, 4 packets, 0 skipped):

| Target | Run command printed by `bml emit … --vectors` |
| --- | --- |
| `rust-typed` | `cd <out> && cargo test --test vectors` |
| `go` | `cd <out> && go test -count=1 ./...` |
| `python` | `cd <out> && python3 -m unittest -q test_vectors` |
| `java` | `cd <out>/bmlgen && java Vectors.java` |
| `csharp` | `cd <out> && dotnet run -v q` |
| `c` | `cd <out> && cc -std=c11 -Wall -Wextra -Wpedantic -Werror -o vectors_test *.c && ./vectors_test` |
| `cpp` | `cd <out> && g++ -std=c++20 -Wall -Wextra -Werror -o vectors_test *.cpp && ./vectors_test` |
| `typescript` | `cd <out> && node --experimental-strip-types vectors.test.ts` |
| `zig` | `cd <out> && zig test vectors_test.zig` |

`--vectors` is for the nine typed targets only (not `rust`, `rstom`, or `rstom-tree`; `rstom` and `rstom-tree` also reject `f32le`/`f64le` schemas with an error, `rust` emits them). The TypeScript package sets `allowImportingTsExtensions` and `noEmit` in `tsconfig.json` and is meant to run under `node --experimental-strip-types`, not `tsc` compile (baseline emit without those flags fails `tsc` with TS5097). Python printers emit `True`/`False`, not JavaScript `true`/`false`.

The repository tests run these vectors for `schemas/hytale/2026_02_17.bml` and `schemas/minecraft_26_3_wiki.bml` in all nine printers. The generated packages for the version roots in `schemas/versions/` do not yet pass their own vectors in every printer (`PROJECT.md` B7), so treat `--vectors` on those roots as a conformance probe.

---

## Shipped Schemas

- **Minecraft Java 26.3 (Protocol 777)**: Full reference target in `schemas/`. 232 packets, 261 routes, zero `body: rest` stubs. All vanilla registries (particles, slot displays, recipes, command argument types) fully modeled.
- **Protocol Versions (1.13 → 26.3)**: 39 protocols across 51 releases in `schemas/versions/`. Shared declarations deduplicated in `schemas/versions/common/`. Validated against an independent ProtoDef oracle; the oracle input (`data/minecraft-data/`) is fetched from pinned commits, not committed.
- **Hytale**: Connection and handshake packets in `schemas/hytale/`.
- **Standard Library**: Reusable fixed-width integers and variable-length integer layouts in `schemas/std/`, including `schemas/std/leb128.bml` (`varuint32`, `varuint64`, zigzag `varint32`, `varint64`; canonical encode, non-canonical padded decode rejected at the sixth/eleventh continuation byte).

---

## Downstream Targets: Minestom / rstom

Bytemill supports in-tree code generation for the Rust Minestom server (`rstom`):

```bash
# Standalone drop-in mirror crate for rstom-packets (its package is named `bml_rstom`)
bml emit rstom $ROOT --out ../rstom/crates/bml_rstom

# In-place generation within rstom workspace using AST shape ledgers
bml shapes extract $ROOT --rstom ../rstom --eligible --out targets/rstom/shapes
bml emit rstom-tree $ROOT --rstom ../rstom --shapes targets/rstom/shapes [--check]
```

See the [rstom packet generation design](docs/superpowers/specs/2026-10-11-bps-generates-rstom-packets-design.md) for shape ledgers and wire pin gates.

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
- `docs/TRANSCRIPTION_CHEATSHEET.md` — Wiki packet definitions to BML transcription guide
- [Idiomatic printers design](docs/superpowers/specs/2026-10-15-bps-idiomatic-printers-design.md) — Codegen IR and typed printer design specification
