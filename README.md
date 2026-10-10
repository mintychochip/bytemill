# BPS — Binary Protocol Schema

Describe arbitrary binary wire formats (varints, bit-packed fields, tagged unions, stateful
packet routing) that protobuf cannot express. Reference target: **Minecraft Java 26.3 /
protocol 777**.

## DSL structure (three layers)

1. **`encoding`** — reusable byte layouts (`varint`, `string(n)`, `position`, `nbt`, …)
2. **`packet` / `enum`** — logical message payloads
3. **`protocol`** — framing (`length` + `id` + `payload`) and `route state.direction id => packet`

Encoder/decode constructs: `match` (tagged unions), `[] count = expr` / `[] until expr`
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

## Schemas (protocol 777)

| File | Contents |
| --- | --- |
| `schemas/minecraft_core.bps` | VarInt/VarLong, String, Identifier, UUID, Game Profile, Position, Angle, BitSet, network **NBT** (recursive), Slot/Hashed Slot frame, entity metadata |
| `schemas/minecraft_26_3.bps` | Handshake, status ping/pong, login start, keep-alive, legacy ping |
| `schemas/minecraft_26_3_play.bps` | Play + configuration **clientbound** (135 + 20 packets, transcribed field layouts) |
| `schemas/minecraft_26_3_play_serverbound.bps` | Play **serverbound** (68 packets) |
| `schemas/minecraft_26_3_login.bps` | Login/configuration shared layouts + remaining mechanical packets |
| `schemas/minecraft_26_3_wiki.bps` | The **261 routes** (state × direction × packet id → packet) from minecraft.wiki |

231 packets are declared; 29 remain `body: rest` because the wiki layout is registry- or
payload-dependent (chunk data, particles, recipes, sound events, …). The exact set is pinned
by `tests/test_routes.py::REST_FALLBACKS`.

Regenerate the route table:

```bash
python scripts/scrape_wiki_routes.py -o schemas/minecraft_26_3_wiki.bps \
  --manual-replace schemas/minecraft_core.bps schemas/minecraft_26_3.bps \
  schemas/minecraft_26_3_play.bps schemas/minecraft_26_3_play_serverbound.bps \
  schemas/minecraft_26_3_login.bps
```

## Quick start

```bash
python -m unittest discover -s tests -t . -v     # 18 tests, includes cargo differential
SCHEMAS="schemas/minecraft_core.bps schemas/minecraft_26_3.bps schemas/minecraft_26_3_login.bps \
schemas/minecraft_26_3_play.bps schemas/minecraft_26_3_play_serverbound.bps \
schemas/minecraft_26_3_wiki.bps"
python -m bps.cli check $SCHEMAS                # parse + resolve + check
python -m bps.cli routes $SCHEMAS               # routing table (261 routes)
python -m bps.cli dump-json $SCHEMAS            # schema.json
python -m bps.cli emit rust $SCHEMAS --out /tmp/bps_rust && cargo test --manifest-path /tmp/bps_rust/Cargo.toml
```

Decode a handshake **frame** (hex):

```bash
echo '10008906096c6f63616c686f737463dd01' | python -m bps.cli decode $SCHEMAS \
  --state handshaking --direction serverbound --hex
# {"id": 0, "packet": "handshake", "fields": {"intent": 1, "protocol_version": 777, ...}}
```

Encode a VarInt probe round-trip:

```bash
echo 'ddc701' | python -m bps.cli decode schemas/minecraft_core.bps --packet varint_probe --hex
```

## Rust codegen

`bpsc emit rust` emits a dependency-free crate (`src/runtime.rs` cursor + VarInt, NBT,
Slot; `src/packets.rs` one decoder per packet; `src/lib.rs` dispatch by state × direction ×
id). `tests/golden.rs` carries the same vectors as the Python side, so Python and Rust must
agree.

## Docs

- `docs/DSL_STRUCTURE.md` — DSL quick reference
- `docs/TRANSCRIPTION_CHEATSHEET.md` — wiki `.proto`-style types → BPS mapping rules
- `docs/superpowers/specs/2026-10-09-bps-dsl-design.md` — design
- `docs/superpowers/plans/2026-10-09-bps-language-core.md` — implementation plan
