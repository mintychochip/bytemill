# BPS DSL structure (quick reference)

BPS separates **how bytes are laid out** from **what messages exist** and **how they are routed**.

## 1. `encoding` — wire layouts

Reusable, parameterizable byte grammars. Primitives (`u16be`, `leb128`) compose with blocks,
`match`, repetition, conditional groups, computed fields, and `require` assertions.

```bps
encoding varint  = leb128(32, signed)

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
| primitive | `u8`, `i16be`, `f32be`, `bool` | fixed-width big-endian |
| varint | `varint`, `varlong`, `leb128(bits, signed)` | LEB128 two's complement |
| bytes | `bytes(n)`, `bytes(n) as utf8` | exactly `n` bytes, optional transform |
| remainder | `rest` | to end of the length-bounded scope |
| repeat count | `items: slot[] count = n` | `n` from an already-decoded field |
| repeat term. | `entries: meta_entry[] until 0xFF` | until a terminator byte, consumed |
| conditional group | `if has_item { item_id: varint }` | whole group present/absent |
| tagged union | `match kind { 1 => varint, _ => varint }` | arm chosen by a field value |
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
        _  => nbt_nothing
    }
}
```

## Example in this repo

- `schemas/minecraft_core.bps` — VarInt, String, Position, NBT, Slot, entity metadata
- `schemas/minecraft_26_3*.bps` — protocol **777**: 231 packets, 261 routes

Run: `python -m bps.cli check schemas/minecraft_core.bps schemas/minecraft_26_3.bps`
