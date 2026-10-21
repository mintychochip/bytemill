# Hytale dedicated server, build 2026.02.17-255364b8e

Transcribed from the decompiled `com.hypixel.hytale.protocol` package on
[soksx/hytale-protocol](https://github.com/soksx/hytale-protocol) branch
`version/2026.02.17-255364b8e`. The Java is not vendored. The bytes in
`crates/bytemill/tests/test_hytale.rs` are what those `serialize` methods write.

```bash
bml check  schemas/hytale/2026_02_17.bml
bml routes schemas/hytale/2026_02_17.bml
bml decode schemas/hytale/2026_02_17.bml --packet disconnect --hex <<< '0100026869'
```

Covered so far: `connect` (0, serverbound), `disconnect` (1, both directions),
`ping` (2, clientbound), `pong` (3, serverbound). Channel is
`NetworkChannel.Default`, spelled `default` in the route state. `clientbound`
is server to client.

## Wire, from the decompile

- Frame: `i32le` payload length, `i32le` packet id, then that many payload
  bytes. The length does **not** include the id (`PacketIO.writeFramedPacket`
  stores `serializedSize` after writing the id).
- Integers are little-endian, except UUID, which is two big-endian longs.
- Strings are an unsigned VarInt byte count plus UTF-8. Fixed ASCII fields are
  null-padded bytes (`client_version` is 20).
- A leading null-bit byte marks optional fields. Ping and Pong still write the
  12-byte instant when the bit is clear.
- Connect's variable fields are an offset table. Java seeks. Canonical
  `serialize` packs present fields in order and writes `-1` for an absent
  offset. The schema reads that packed order, so a payload that is only
  reachable by seeking does not decode here.
- Zstd applies only when `IS_COMPRESSED` is set. These four packets are not.

`bml decode --state` reads the frame block: `payload: bytes(length)` takes
exactly `length` bytes, so the length here counts the payload only. A
`payload: rest` frame instead counts everything after the length field (id
bytes included), which is the Minecraft shape.
