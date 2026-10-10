# BPS — Binary Protocol Schema

Describe arbitrary binary wire formats (varints, bit-packed fields, tagged unions, stateful
packet routing) that protobuf cannot express. Reference target: **Minecraft Java 26.3 /
protocol 777**.

## DSL structure (three layers)

1. **`encoding`** — reusable byte layouts (`varint`, `string(n)`, `position`, …)
2. **`packet` / `enum`** — logical message payloads
3. **`protocol`** — framing (`length` + `id` + `payload`) and `route state.direction id => packet`

Example: `schemas/minecraft_core.bps` + `schemas/minecraft_26_3.bps`.

## Quick start

```bash
python -m unittest discover -s tests -v
python -m bps.cli check schemas/minecraft_core.bps schemas/minecraft_26_3.bps
python -m bps.cli routes schemas/minecraft_core.bps schemas/minecraft_26_3.bps
```

Decode a handshake **frame** (hex):

```bash
echo '10008906096c6f63616c686f737463dd01' | python -m bps.cli decode \
  schemas/minecraft_core.bps schemas/minecraft_26_3.bps \
  --state handshaking --direction serverbound --hex
```

## Docs

- Design: `docs/superpowers/specs/2026-10-09-bps-dsl-design.md`
- Plan: `docs/superpowers/plans/2026-10-09-bps-language-core.md`
