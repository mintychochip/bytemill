# BPS DSL structure (quick reference)

BPS separates **how bytes are laid out** from **what messages exist** and **how they are routed**.

## 1. `encoding` — wire layouts

Reusable, parameterizable byte grammars. The compiler's primitives are `u8`,
`bytes(n)`, `rest`, `f32be`, and `f64be`. Wider integers and LEB128 live in
`schemas/std/` and are pulled in with `import`.

```bps
import "std/integers.bps"
import "std/varint.bps"

encoding string(max_chars = 32767) = {
    byte_len: varint
    value:    bytes(byte_len) as utf8
    require   utf16_units(value) <= max_chars
}

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
        3 => f32be
        4 => string(32767)
        8 => boolean
        _ => varint
    }
}
```

### Constructs

| Construct | Example | Meaning |
| --- | --- | --- |
| primitive | `u8`, `bytes(n)`, `rest`, `f32be`, `f64be` | the only built-in types |
| scalar encoding | `encoding u16be = { ... } = expr` | block whose value is `expr`; `name: type = expr` is the encode side |
| import | `import "std/varint.bps"` | makes that file's names visible here; path relative to the importing file; not transitive |
| bytes | `bytes(n)`, `bytes(n) as utf8` | exactly `n` bytes, optional transform |
| remainder | `rest` | to end of the length-bounded scope |
| repeat count | `items: slot[] count = n` | `n` from an already-decoded field |
| repeat term. | `entries: meta_entry[] until 0xFF` | until a terminator byte, consumed |
| conditional group | `if has_item { item_id: varint }` | whole group present/absent |
| tagged union | `match (action) { 0 => { title: text_component } _ => { } }` | arm chosen by a field value; `switch` is the same statement; `0 \| 1 =>` shares an arm. A field can also be `value: match kind { 0 => i8, _ => varint }` |
| computed | `x = sign_extend(...)` | no wire bytes, value derives |
| assertion | `require protocol_version == 777` | decode fails if false |
| spread | `...string(32767)` | inline another encoding's block |
| parameter | `encoding string(max_chars = 32767)` | instantiate as `string(255)` |

## 2. `packet` / `enum` — logical messages

```bps
enum intent : varint { status = 1, login = 2, transfer = 3 }

packet boss_event {
    uuid:   uuid
    action: varint
    if action == 0 {
        title:    text_component
        health:   f32be
    }
}
```

## 3. `protocol` — framing + dispatch

```bps
protocol minecraft @ v26_3 {
    id = 777
    frame default {
        length: varint
        id: varint
        payload: rest
    }
    route handshaking.serverbound 0x00 => handshake
    route play.clientbound 0x2d => keep_alive
}
```

A packet's meaning depends on **state × direction × id**, which the descriptor model cannot
express.

The frame block is the framing: `bpsc decode --state` reads it, so a second
game is a second frame shape, not a code change. `length`, `id`, and
`payload` are ordinary typed fields with one rule: `payload: rest` pairs
with a length that counts everything after the length field (id bytes
included, the Minecraft shape above); `payload: bytes(n)` reads exactly `n`
bytes (Hytale: `length: i32le`, `id: i32le`, `payload: bytes(length)` — the
length counts the payload only).

## Why not `.proto`?

Protobuf fixes wire types per field number. Minecraft 26.3 uses VarInt packet IDs, state-scoped
dispatch, LEB128 signed integers (not ZigZag), bit-packed positions, `until 0xFF` metadata
streams, and recursive network NBT — none of which fit the descriptor model.

## Recursive types

Network NBT needs self-reference: `nbt_compound_body` spreads `nbt_entry`, whose `match` arms
include `nbt_compound_body` and `nbt_list_body` (which in turn references
`nbt_compound_body`). See `schemas/minecraft_core.bps`.

```bps
encoding nbt_entry = {
    tag_type: u8
    name:     nbt_string
    ...nbt_payload
}

encoding nbt_payload = {
    value: match tag_type {
        9  => nbt_list_body
        10 => nbt_compound_body
        0  => nbt_nothing
    }
}
```

## Example in this repo

- `schemas/std/` — integers, boolean, VarInt, and VarLong; a file imports them to use them
- `schemas/minecraft_core.bps` — String, UUID, Position, NBT (recursive),
  Slot, entity metadata (all 44 kinds), 22-particle data table, Light Data, ID Set, LpVec3
- `schemas/minecraft_26_3*.bps` — protocol **777**: 232 packets across 261 routes, all
  transcribed (no `body: rest` stubs). Each packet file imports `std/integers.bps`,
  `std/varint.bps` and `minecraft_core.bps`; `minecraft_26_3_wiki.bps` (the routes)
  imports every packet file, so it is the one root that loads the whole protocol

- `schemas/versions/` — protocols 393 (1.13) to 777 (26.3), one thin root each (header, imports,
  routes) plus shared declaration files under `common/`, generated from minecraft-data's ProtoDef
  descriptions by `bpsc import-versions` (see its README)

Authoring rules and the per-packet workflow live in `docs/AUTHORING_GUIDE.md`.

Run: `bpsc check schemas/minecraft_26_3_wiki.bps`
