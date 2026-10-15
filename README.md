# BPS — Binary Protocol Schema

Describe arbitrary binary wire formats (varints, bit-packed fields, tagged unions, stateful
packet routing) that protobuf cannot express. Reference target: **Minecraft Java 26.3 /
protocol 777**; every release protocol from 1.13 (393) on is in `schemas/versions/`.

## DSL structure (three layers)

1. **`encoding`** — reusable byte layouts (`varint`, `string(n)`, `position`, `nbt`, …)
2. **`packet` / `enum`** — logical message payloads
3. **`protocol`** — framing (`length` + `id` + `payload`) and `route state.direction id => packet`

Encoder/decode constructs: `match` / `switch` (tagged unions, as a field type or a statement), `[] count = expr` / `[] until expr`
(repeats), `if cond { ... }` (conditional field groups), `name = expr` (computed values),
`require expr` (assertions), `...encoding(args)` (spread into a block).

Example (see `schemas/minecraft_core.bps`):

```bps
encoding position = {
    raw: u64be
    x = sign_extend(raw >> 38, 26)
    y = sign_extend(raw % 4096, 12)
    z = sign_extend((raw >> 12) % 67108864, 26)
}

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
```

## Where names come from

The compiler implements five types: `u8`, `bytes(n)`, `rest`, `f32be`, `f64be`. Every other
type (`i32be`, `boolean`, `varint`, `string`, `position`) is an `encoding` declared in a
`.bps` file, and a file uses another file's names only by importing it:

```bps
import "std/integers.bps"      // i8, u16be, i32be, boolean, ...
import "std/varint.bps"        // varint, varlong
import "minecraft_core.bps"    // string, uuid, position, nbt, slot, ...
```

An import path is relative to the file that contains it, and each file loads once. Imports are
**not transitive** (the `.proto` rule): the file that uses `varint` is the file that imports
`std/varint.bps`. Forget one and the error names the line to add:

```
schemas/minecraft_26_3_login.bps: 1 name not declared here or in an imported file
  add  import "std/varint.bps"  for varint
```

`schemas/std/varint.bps` explains the byte layout; `docs/AUTHORING_GUIDE.md` §2 lists where
each shared name lives and which tool behaviors still depend on a std name.

## Schemas (protocol 777)

| File | Contents |
| --- | --- |
| `schemas/std/integers.bps` | Fixed-width integers and `boolean`, built from `u8` |
| `schemas/std/varint.bps` | `varint` and `varlong`, built from `u8` |
| `schemas/minecraft_core.bps` | Imports `std/`. String, Identifier, UUID, Game Profile, Position, Angle, BitSet, network **NBT** (recursive), Slot/Hashed Slot frame, entity metadata |
| `schemas/minecraft_26_3.bps` | Handshake, status ping/pong, login start, keep-alive, legacy ping |
| `schemas/minecraft_26_3_play.bps` | Play + configuration **clientbound** (135 + 20 packets, transcribed field layouts) |
| `schemas/minecraft_26_3_play_serverbound.bps` | Play **serverbound** (68 packets) |
| `schemas/minecraft_26_3_login.bps` | Login/configuration shared layouts + remaining mechanical packets |
| `schemas/minecraft_26_3_remaining.bps` | Final transcription batch — remaining registry-independent packets, LpVec3 velocity (`add_entity`) |
| `schemas/minecraft_26_3_wiki.bps` | The **261 routes** (state × direction × packet id → packet) from minecraft.wiki. Imports every packet file, so it is the one root that loads the whole protocol |

Each packet file imports `std/integers.bps`, `std/varint.bps` and `minecraft_core.bps`.

**232 packets, 261 routes, 90 encodings, zero `body: rest` stubs.** Every packet carries its
transcribed field layout. Registries vanilla enumerates (recipe and slot displays, particles,
position sources, chat types, number formats, command argument types) are modeled in full;
open ones (data-component patches, chunk section data) stop at a self-delimiting boundary via
`slot`, `item_stack_template`, `item_cost`.

Regenerate the route table:

```bash
cargo run -q -p bps --bin scrape_wiki_routes -- -o schemas/minecraft_26_3_wiki.bps \
  --manual-replace schemas/minecraft_core.bps schemas/minecraft_26_3.bps \
  schemas/minecraft_26_3_login.bps schemas/minecraft_26_3_play.bps \
  schemas/minecraft_26_3_play_serverbound.bps schemas/minecraft_26_3_remaining.bps
```

The generated file imports `std/varint.bps` (its frame uses it) and every `--manual-replace`
file that declares packets.

## Protocol versions (1.13 → 26.3)

`schemas/versions/` has one root schema per Minecraft Java protocol number from 393 (1.13) to
777 (26.3): 39 protocols, 51 release versions, every state and direction, every packet decoded.
Declarations shared across versions live once in `schemas/versions/common/` (164 files, ~44% of
the single-file size); a root imports the files its version needs. They are generated from
PrismarineJS minecraft-data's ProtoDef descriptions (vendored in `data/minecraft-data/`) by
`bpsc import-versions`, corrected where the decompiled vanilla server disagrees (26.1 to 26.3,
`schemas/versions/patches/`), and checked against an independent ProtoDef implementation.
Versions before 26.1 are as the community data publishes them. See `schemas/versions/README.md`
for the version table, the value shape, the verification record and the known limits.

```bash
bpsc check schemas/versions/v1_20_1.bps            # protocol 763: 1.20 and 1.20.1
bpsc routes schemas/versions/v26_3.bps             # 261 routes
bpsc import-versions --check                       # the committed files are what the importer writes
scripts/verify_versions.py                         # ProtoDef oracle, both directions (needs Node)
scripts/python_parity_versions.py                  # Python reference vs Rust on every version
```

## Quick start

```bash
scripts/bps_cargo.sh test --no-fail-fast    # 281 tests, memory-capped (see below); the rstom tests read ../rstom-bps
scripts/bps_cargo.sh clippy --workspace --all-targets -- -D warnings   # the lint gate: prints nothing, exits 0
cargo build --release                       # once: target/release/bpsc (the examples below call it `bpsc`)
ROOT=schemas/minecraft_26_3_wiki.bps             # imports every other schema file
bpsc check $ROOT                                 # parse + resolve + check
bpsc routes $ROOT                                # routing table (261 routes)
bpsc dump-json $ROOT                             # schema.json
bpsc shapes report $ROOT --rstom ../rstom-bps    # eligible vs blocked packets (worklist)
bpsc emit rust $ROOT --out /tmp/bps_rust && cargo test --manifest-path /tmp/bps_rust/Cargo.toml
bpsc emit rstom $ROOT --out ../rstom/crates/bps_rstom  # drop-in for `rstom-packets`
bpsc emit rstom-tree $ROOT --rstom ../rstom-bps --shapes targets/rstom/shapes [--check]
cargo run -q -p bps --bin java_probe_samples     # packets whose layout disagrees with real Minestom bytes
```

Decode a handshake **frame** (hex):

```bash
echo '10008906096c6f63616c686f737463dd01' | bpsc decode $ROOT \
  --state handshaking --direction serverbound --hex
# {"id": 0, "packet": "handshake", "fields": {"intent": 1, "protocol_version": 777, ...}}
```

VarInt in both directions (300 is `ac 02`; see `schemas/std/varint.bps`):

```bash
echo 'ac0202' | bpsc decode schemas/minecraft_26_3_play.bps --packet animate --hex
# {"animation": 2, "entity_id": 300}
bpsc encode --packet animate schemas/minecraft_26_3_play.bps '{"entity_id": 300, "animation": 2}'
# ac0202
```

## Rust codegen

`bpsc emit rust` emits a dependency-free crate (`src/runtime.rs` cursor + VarInt, NBT,
Slot; `src/packets.rs` one decoder per packet; `src/lib.rs` dispatch by state × direction ×
id). `tests/golden.rs` carries the same vectors as the interpreter's tests, so the emitted
decoders and the interpreter must agree.

`bpsc emit rstom` writes a drop-in for rstom's `rstom-packets` crate: the same module tree
(`client/` is what the client sends, `server/` what the server sends; below each, the
Minestom packages `common`, `configuration`, `handshake`, `login`, `play`, `status`), a
struct for each Minestom class under its Minestom name and `CLASS_NAME`, the ten
`packet_vanilla::*_registry` tables, `ported::all()`, and the same re-exports at the crate
root. The class of a route is the class at that id in rstom's
`tools/packets_gen/schema/packets.json`, which the emit finds in the rstom workspace it
writes into (`--packets-json` names another); `crates/bps/tests/test_minestom.rs` checks that
mapping against the id tables recorded from Minestom itself.

What is not drop-in: a struct's fields are the schema's wire fields, not the Java record's,
so constructors differ (the handshake `intent` is an `i32`, not an enum) and the shared
hand-written types (`ChunkData`, `component`, ...) do not exist. A packet that travels both
ways is two structs (`ClientKeepAlivePacket`, `KeepAlivePacket`). Minestom keeps two wire
forms on `ShowDialogPacket`; BPS has a packet for each, so the configuration form is one
extra module, `server/configuration/show_dialog_packet`.

The output is rustfmt-canonical, and each emit drops the generated files the current schema
no longer declares (a file without the emitter marker is left alone), so the tree always
matches its `lib.rs`. Every struct's `roundtrip_*.rs` pins the Rust encoder against the
interpreter's bytes for the same synthetic value (`crates/bps/src/synth.rs`; the generated
comment still names the Python interpreter and `bps/synth.py`, text the emitter keeps
byte-for-byte), and `crates/bps/tests/test_emit_rstom.rs` fails if any packet stops pinning.
`tests/registry_tables.rs` builds all ten id tables and checks their class names and order.

A payload with no length prefix (a recipe display, a particle, an advancement) is carried
as raw bytes, so a reader has to scan the structure to find its end. `src/frames.rs` holds
those scanners: NBT and slots are hand-written, every other one is compiled from its own
encoding (`crates/bps/src/frames_gen.rs`) and checked against the interpreter on random valid
payloads (`crates/bps/tests/test_frames_gen.rs`). The generated struct has one namespace per
packet, so using the same encoding twice in one packet is an `EmitError`; give the fields
distinct names.

## Generating rstom's packets in place

`emit rstom` writes a mirror crate; `emit rstom-tree` writes **inside** an `rstom` workspace at the
hand packet paths. A **shape** is the hand file's public API (type name, derives, field names and
types, `new`, serializer layout, module docs) extracted once and kept in the ledger; BPS supplies
the wire layout from the schemas. Where Minestom or the hand port disagrees with vanilla, BPS wins.

```bash
bpsc shapes extract $ROOT --rstom ../rstom-bps --eligible --out targets/rstom/shapes
bpsc shapes report $ROOT --rstom ../rstom-bps
bpsc emit rstom-tree $ROOT --rstom ../rstom-bps --shapes targets/rstom/shapes
bpsc emit rstom-tree $ROOT --rstom ../rstom-bps --shapes targets/rstom/shapes --check
```

The ledger is `targets/rstom/shapes/` (**64** JSON files at
`<group>/<package>/<stem>.json`). Each shape records module and struct docs, derives, fields
(names, types, docs), `new` and serializer metadata, and optionally an `extras` filename. When
`extras` is set, add `<stem>.ext.rs` beside the packet (helpers the generator does not emit);
the packet re-exports it via `#[path = "<stem>.ext.rs"] mod ext` and
`#[allow(unused_imports)] pub use ext::*` so module-level constants keep their paths without
`-D warnings` on an empty ext module. Five wave-1 shapes name extras today.

Every emit also refreshes the **wire pin gate** in the worktree:
`crates/rstom/tests/bps_wire_pins.rs`, `crates/rstom/tests/bps/wire_pins.json` (**260** rows,
one per id-table slot, bytes from `crates/bps/src/synth.rs`), and `wire_pins_known.json` (triaged failures;
**9** entries in three categories: `minestom-deviation`, `hand-bug`, `domain-limit` — the list
only shrinks). Swapped packets must not appear in the known file.

**Safety on an in-place swap:** the emitter refuses to overwrite a hand `.rs` unless a fresh
extract matches the ledger shape; it refuses when doc or attribute lines before `#[cfg(test)]`
would be lost (they may live in `.ext.rs` instead). Any other hand packet path without the
generated marker aborts the whole run (all-or-nothing). Re-emit with the same schema and ledger
is byte-identical. `--check` regenerates in memory and reports drift, missing files, hand-shaped
ledger entries, and orphan generated packets; it never writes. Inline `#[cfg(test)]` modules in
hand packets are not copied — the pin gate holds wire-byte coverage instead. Packets with
`extras` need **nightly** `rustfmt` (`--unstable-features --skip-children`) on emit.

Rstom-side rules: `docs/constraints/MAIN/BPS_GENERATED_PACKETS.md` in the worktree (62 constraint
rows). Wave **0** landed the extractor, in-tree emitter, and pin gate; wave **1** swaps the **64**
ledger packets whose BPS fields already match (`shapes report` marks them `eligible`; the rest are
later waves). See `docs/superpowers/specs/2026-10-11-bps-generates-rstom-packets-design.md` and
`docs/superpowers/plans/2026-10-11-bps-rstom-tree-waves-0-1.md`.

### Building the rstom workspace politely

`scripts/rstom_cargo.sh` runs `cargo` under `nice -n 19` / `ionice -c3`, `CARGO_BUILD_JOBS=4`, and
a private target dir (`/dev/shm/rstom-bps-target` by default). Point it at the worktree with
`BPS_RSTOM_DIR` and override the target with `BPS_CARGO_TARGET_DIR`; never share one target dir
between two checkouts (the suite asserts it never writes into `rstom`'s own `target/`). For a full
`rstom` test run use `cargo test --no-fail-fast` so every failure is reported.

### Running this crate's cargo politely

`scripts/bps_cargo.sh <cargo args>` is the same idea for `crates/bps`, plus a hard memory cap: cargo
runs in a transient systemd user scope (`MemoryMax`, default 8G via `BPS_MEM_MAX`; swap off;
`CPUQuota` 400%; 4 jobs and 4 test threads), with scratch files under `~/.cache/bps-scratch` rather
than a RAM-backed `/tmp`, and prints the scope's peak memory when cargo exits. A runaway test is then
killed by the kernel inside its own scope instead of pushing the desktop into reclaim until
`systemd-oomd` kills the terminal, which is what an unbounded test did once. Run the suite and any
one-off build through it (`BPS_MEM_MAX=3G scripts/bps_cargo.sh test` catches a runaway sooner).

### Clippy is a hard gate

`[workspace.lints.clippy] all = "deny"` in `Cargo.toml` makes any clippy finding a compile error, so
plain `cargo clippy` fails with no flags. The gate adds `-D warnings`, so a rustc warning fails too:

```bash
scripts/bps_cargo.sh clippy --workspace --all-targets -- -D warnings   # prints nothing, exits 0
```

It runs in three places:

- **CI**: `.github/workflows/clippy.yml`, on every push to `master` and every pull request. The
  toolchain is pinned there to the nightly the tree was cleaned with, so a lint a newer clippy adds
  cannot fail code nobody touched; bump it with the local default and fix what the newer clippy
  finds in the same change.
- **Pre-commit hook**: `.githooks/pre-commit` rejects a commit that touches `*.rs` or `Cargo.*`
  while the gate fails. Enable it once per clone with `git config core.hooksPath .githooks`. It
  checks the working tree, so stage whole files; `git commit --no-verify` skips it, CI does not.
- **By hand**, before handing work back.

Fix a finding instead of silencing it. One that cannot be fixed gets the narrowest
`#[allow(clippy::lint, reason = "...")]`, never a crate-level allow. The tree has eight such
suppressions: `too_many_arguments` on six `emit_rstom.rs` functions (ports of Python helpers) and
on one test helper, and `excessive_precision` in `test_json_floats.rs`.

## Docs

- `docs/DSL_STRUCTURE.md` — DSL quick reference
- `docs/TRANSCRIPTION_CHEATSHEET.md` — wiki `.proto`-style types → BPS mapping rules
- `docs/superpowers/specs/2026-10-09-bps-dsl-design.md` — design
- `docs/superpowers/plans/2026-10-09-bps-language-core.md` — implementation plan
