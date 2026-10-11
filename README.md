# BPS — Binary Protocol Schema

Describe arbitrary binary wire formats (varints, bit-packed fields, tagged unions, stateful
packet routing) that protobuf cannot express. Reference target: **Minecraft Java 26.3 /
protocol 777**.

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
python scripts/scrape_wiki_routes.py -o schemas/minecraft_26_3_wiki.bps \
  --manual-replace schemas/minecraft_core.bps schemas/minecraft_26_3.bps \
  schemas/minecraft_26_3_login.bps schemas/minecraft_26_3_play.bps \
  schemas/minecraft_26_3_play_serverbound.bps schemas/minecraft_26_3_remaining.bps
```

The generated file imports `std/varint.bps` (its frame uses it) and every `--manual-replace`
file that declares packets.

## Quick start

```bash
python -m unittest discover -s tests -t . -v     # 94 tests, includes the cargo build of the rstom crate
ROOT=schemas/minecraft_26_3_wiki.bps             # imports every other schema file
python -m bps.cli check $ROOT                    # parse + resolve + check
python -m bps.cli routes $ROOT                   # routing table (261 routes)
python -m bps.cli dump-json $ROOT                # schema.json
python -m bps.cli emit rust $ROOT --out /tmp/bps_rust && cargo test --manifest-path /tmp/bps_rust/Cargo.toml
python -m bps.cli emit rstom $ROOT --out ../rstom/crates/bps_rstom  # drop-in for `rstom-packets`
python scripts/java_probe_samples.py             # packets whose layout disagrees with real Minestom bytes
```

Decode a handshake **frame** (hex):

```bash
echo '10008906096c6f63616c686f737463dd01' | python -m bps.cli decode $ROOT \
  --state handshaking --direction serverbound --hex
# {"id": 0, "packet": "handshake", "fields": {"intent": 1, "protocol_version": 777, ...}}
```

VarInt in both directions (300 is `ac 02`; see `schemas/std/varint.bps`):

```bash
echo 'ac0202' | python -m bps.cli decode schemas/minecraft_26_3_play.bps --packet animate --hex
# {"animation": 2, "entity_id": 300}
python -m bps.cli encode --packet animate schemas/minecraft_26_3_play.bps '{"entity_id": 300, "animation": 2}'
# ac0202
```

## Rust codegen

`bpsc emit rust` emits a dependency-free crate (`src/runtime.rs` cursor + VarInt, NBT,
Slot; `src/packets.rs` one decoder per packet; `src/lib.rs` dispatch by state × direction ×
id). `tests/golden.rs` carries the same vectors as the Python side, so Python and Rust must
agree.

`bpsc emit rstom` writes a drop-in for rstom's `rstom-packets` crate: the same module tree
(`client/` is what the client sends, `server/` what the server sends; below each, the
Minestom packages `common`, `configuration`, `handshake`, `login`, `play`, `status`), a
struct for each Minestom class under its Minestom name and `CLASS_NAME`, the ten
`packet_vanilla::*_registry` tables, `ported::all()`, and the same re-exports at the crate
root. The class of a route is the class at that id in rstom's
`tools/packets_gen/schema/packets.json`, which the emit finds in the rstom workspace it
writes into (`--packets-json` names another); `tests/test_minestom.py` checks that mapping
against the id tables recorded from Minestom itself.

What is not drop-in: a struct's fields are the schema's wire fields, not the Java record's,
so constructors differ (the handshake `intent` is an `i32`, not an enum) and the shared
hand-written types (`ChunkData`, `component`, ...) do not exist. A packet that travels both
ways is two structs (`ClientKeepAlivePacket`, `KeepAlivePacket`). Minestom keeps two wire
forms on `ShowDialogPacket`; BPS has a packet for each, so the configuration form is one
extra module, `server/configuration/show_dialog_packet`.

The output is rustfmt-canonical, and each emit drops the generated files the current schema
no longer declares (a file without the emitter marker is left alone), so the tree always
matches its `lib.rs`. Every struct's `roundtrip_*.rs` pins the Rust encoder against the
Python interpreter's bytes for the same synthetic value (`bps/synth.py`), and
`tests/test_emit_rstom.py` fails if any packet stops pinning. `tests/registry_tables.rs`
builds all ten id tables and checks their class names and order.

A payload with no length prefix (a recipe display, a particle, an advancement) is carried
as raw bytes, so a reader has to scan the structure to find its end. `src/frames.rs` holds
those scanners: NBT and slots are hand-written, every other one is compiled from its own
encoding (`bps/frames_gen.py`) and checked against the interpreter on random valid payloads
(`tests/test_frames_gen.py`). The generated struct has one namespace per packet, so using
the same encoding twice in one packet is an `EmitError`; give the fields distinct names.

## Docs

- `docs/DSL_STRUCTURE.md` — DSL quick reference
- `docs/TRANSCRIPTION_CHEATSHEET.md` — wiki `.proto`-style types → BPS mapping rules
- `docs/superpowers/specs/2026-10-09-bps-dsl-design.md` — design
- `docs/superpowers/plans/2026-10-09-bps-language-core.md` — implementation plan
