# BPS DSL structure (quick reference)

BPS separates **how bytes are laid out** from **what messages exist** and **how they are routed**.

## 1. `encoding` — wire layouts

Reusable, parameterizable byte grammars. Primitives (`u16be`, `leb128`) compose with blocks,
`match`, repetition, and computed fields.

```bps
encoding varint  = leb128(32, signed)

encoding string(max_chars = 32767) {
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
```

## 2. `packet` / `enum` — logical messages

```bps
enum intent : varint { status = 1, login = 2, transfer = 3 }

packet handshake {
    protocol_version: varint
    server_address:   string(255)
    server_port:      u16be
    intent:           varint
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
}
```

## Why not `.proto`?

Protobuf fixes wire types per field number. Minecraft 26.3 uses VarInt packet IDs, state-scoped
dispatch, LEB128 signed integers (not ZigZag), bit-packed positions, `until 0xFF` metadata
streams, and NBT unions — none of which fit the descriptor model.

## Example in this repo

- `schemas/minecraft_core.bps` — VarInt, String, Position, Entity Metadata
- `schemas/minecraft_26_3.bps` — protocol **777** lifecycle packets (expand routes per wiki)

Run: `python -m bps.cli check schemas/minecraft_core.bps schemas/minecraft_26_3.bps`
