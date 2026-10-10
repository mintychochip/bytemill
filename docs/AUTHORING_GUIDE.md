# BPS authoring guide

How to write a `.bps` schema from scratch: the declaration order that works, how to
shape packages, how to build packet structures, and how to verify what you wrote.
`docs/DSL_STRUCTURE.md` is the construct reference; this is the recipe.

Read both. `schemas/minecraft_*.bps` are the worked examples — every pattern below
appears there in context.

## 1. Declare in this order

Order is not semantically required (forward references resolve), but this order
keeps a file readable and lets `bpsc check` fail on the missing piece nearest the
mistake.

```
encoding   primitives and composites   (varint, string, position, nbt)
enum       named integer domains       (intent, chat_mode)
packet     payload structures          (handshake, block_update)
protocol   framing + route table       (minecraft @ v26_3)
```

Split a file when it passes ~200 lines of packets, along the same axes:

```
schemas/<family>_core.bps    shared wire layouts
schemas/<family>_<version>.bps lifecycle packets with hand-tuned layouts
schemas/<family>_<state>.bps  per-state batches (play, login, configuration)
schemas/<family>_routes.bps  generated `route` table + stubs
```

Multiple files merge: `bpsc` concatenates declarations from every argument before
resolving, so a name must be declared in exactly one file (duplicates are a
`ResolveError`).

## 2. Define the primitives first

Minecraft-family layouts that everything else leans on:

```bps
encoding varint  = leb128(32, signed)
encoding varlong = leb128(64, signed)

encoding string(max_chars = 32767) = {
    byte_len: varint
    value:    bytes(byte_len) as utf8
    require   utf16_units(value) <= max_chars
}

encoding identifier = { ...string(32767) }
encoding angle      = u8
```

Rules that bite:

- `bytes(n)` with `as utf8` yields a string; without it, raw bytes.
- `rest` reads to the end of the *length-bounded scope* (a packet payload or an
  enclosing `bytes(...)`), never past it. **A repeat element is not a bounded
  scope**, so a `rest` inside a repeated block consumes to the end of the packet
  and swallows later fields — avoid it; bound the element with `bytes(n)`/a count
  instead (this is exactly why `commands`' parser payload needs a registry).
- `require` is an assertion evaluated on decode; a failure is a `DecodeError`.

## 3. Model structure, not "fields"

Ask, in order:

1. **Is the length known before the data?** → a count field plus `[] count = n`.
2. **Does it end at a sentinel?** → `[] until 0xFF`.
3. **Does it run to the end of the packet?** → `[] until_end`.
4. **Is the shape selected by a value?** → `match tag { ... }`.
5. **Is the whole group present conditionally?** → `if cond { ... }`.
6. **Is the value derived, not sent?** → `name = expr`.

```bps
encoding meta_entry = {
    index: u8
    kind:  varint                       // (4) selects the shape
    value: match kind {
        0 => i8
        1 => varint
        2 => varlong
        3 => f32be
        4 => string(32767)
        _ => varint
    }
}

encoding entity_metadata = {
    entries: meta_entry[] until 0xFF     // (2) sentinel-terminated
}
```

Conditional group, with the count derived from the element list:

```bps
if item_count > 0 {
    item_id:      varint
    add_count:    varint
    remove_count: varint
}
```

Flag-masked optional blocks (Java sends the boolean, then the value):

```bps
has_uuid: varint
if has_uuid != 0 {
    uuid: uuid
}
```

Bit-masked variable layouts need computed fields, then guards on them:

```bps
actions: u8
if actions & 0x01 != 0 { name: string(16) }
if actions & 0x04 != 0 { game_mode: varint }
```

## 4. Packet structure rules

- One `packet` per wire message, named with the *official* name from the protocol's
  packet report (`see docs/TRANSCRIPTION_CHEATSHEET.md`).
- Serverbound/clientbound packets with the same official name get a suffix
  (`container_close` vs `container_close_serverbound`) so a single schema set can
  hold both.
- Only field-level constructs inside a `packet` — no nested `encoding`/`protocol`.
- Cross-state packets are declared once and routed from every state that uses them.

```bps
packet boss_event {
    uuid:   uuid
    action: varint
    if action == 0 {
        title:    text_component
        health:   f32be
        color:    varint
        division: varint
        flags:    u8
    }
    if action == 2 { health: f32be }
}
```

### Recursive structures

Self-reference is required for NBT trees and works through named encodings:

```bps
encoding nbt_entry = {
    tag_type: u8
    name:     nbt_string
    ...nbt_payload
}

encoding nbt_payload = {
    value: match tag_type {
        9  => nbt_list_body      // list of payloads
        10 => nbt_compound_body  // records until TAG_End — this recurses
        _  => nbt_nothing
    }
}
```

Keep the indirection in the `match` arms, not in the encoding that contains them;
that is what terminates.

## 5. The route table

A `protocol` block names the version, the protocol number, the frame shape, and
every route.

```bps
protocol minecraft @ v26_3 {
    id = 777
    frame default {
        length: varint
        id:     varint
        payload: rest
    }

    route handshaking.serverbound 0x00 => handshake
    route play.clientbound      0x2d => keep_alive
    route play.clientbound      0x49 => player_position
}
```

- `state.direction` selects the ID table; the same numeric ID in different states
  maps to different packets, which is the whole point.
- `0xfe` legacy IDs are fine; decimal also works.
- `encode`/`decode` on the CLI need `--state` and `--direction` for frames, or
  `--packet` for a bare payload.

## 6. Registry-dependent payloads

The schema stops at the registry boundary: the framing is transcribed, the
registry-keyed body stays opaque. This is the boundary between "described" and
"data", and it is where a v1 schema ends.

```bps
// Sound Event: 0 means inline; otherwise registry ID + 1.
encoding id_or_sound_event = {
    id: varint
    if id == 0 {
        value: sound_event
    }
}

// Item counts and component counts are on the wire; the component payloads
// are keyed by minecraft:data_component_type and are not walked.
encoding slot = {
    item_count: varint
    if item_count > 0 {
        item_id:      varint
        add_count:    varint
        remove_count: varint
    }
}
```

Within a `match` arm, use `rest` for the opaque tail so framing stays correct:

```bps
sound = { sound_event_id: varint, if sound_event_id == 0 { ... sound_event }, ... }
```

## 7. Verify as you write

```bash
python -m bps.cli check schemas/your.bps          # parse + resolve + type-check
python -m bps.cli routes schemas/your.bps         # the routing table
python -m bps.cli dump-json schemas/your.bps      # schema.json for other tools
```

Then prove a value round-trips (`--packet` precedes the schema list so argparse can
tell the flag from the trailing JSON):

```bash
python -m bps.cli encode --packet handshake schemas/your.bps \
  '{"protocol_version":777,"server_address":"localhost","server_port":25565,"intent":1}'
```

And decode a frame:

```bash
echo '10008906096c6f63616c686f737463dd01' | python -m bps.cli decode schemas/your.bps \
  --state handshaking --direction serverbound --hex
```

The gates that must stay green (see `tests/`):

| Gate | Asserts |
| --- | --- |
| `test_parser.ParserSmoke.test_parse_all_schemas` | every `.bps` file parses |
| `test_routes.test_every_route_resolves_to_its_packet` | every route binds to a declared packet |
| `test_routes.test_rest_fallback_set_is_explicit` | no packet silently regresses to `body: rest` |
| `test_routes.test_non_stub_packets_smoke_decode` | every packet decodes a zero buffer cleanly |
| `test_roundtrip_all.RoundTripAllTest` | every packet's synthetic value round-trips |
| `test_emit_rust` | the generated Rust crate compiles and passes goldens |

## 8. Adding a packet to an existing family

1. Find the packet's section in the protocol's packet report and note the official
   name and field table.
2. Transcribe with the type mapping in `docs/TRANSCRIPTION_CHEATSHEET.md`.
3. Put it in the file that owns its state; use `_serverbound`/`_clientbound`
   suffixes only for a name collision.
4. Add its `route` line to the routes file (or regenerate it if the set is
   machine-derived).
5. Run the gates above. A new `body: rest` packet fails `test_rest_fallback_set_is_explicit`
   on purpose — either transcribe it or update the list with a reason.
