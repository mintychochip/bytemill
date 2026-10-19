# Wiki → BPS transcription cheat sheet

Starting point: `/tmp/mc_packets.txt` (Minecraft Java Edition protocol/Packets,
protocol 777). Each packet section is `==== Packet Name ====` with a wiki table of
`Field Name | Field Type | Notes`.

The wiki is not the authority: it is wrong in places (see "Where the wiki is wrong"
below). What real clients speak is vanilla's own codec, in the decompiled
`net/minecraft/network/protocol/**` (`StreamCodec.composite(...)` lists a packet's
fields in wire order). Check a packet there before trusting a wiki row, and
`cargo run -q -p bps --bin java_probe_samples -- <packet>` for the bytes Minestom writes.

Output: a file that starts with the imports below and then contains **only**
`packet NAME { ... }` declarations. A file can use only names it declares or
imports, so these three lines come first:

```
import "std/integers.bps"
import "std/varint.bps"
import "minecraft_core.bps"
```

## Type mapping

| Wiki type | BPS |
| --- | --- |
| `VarInt` | `varint` |
| `VarLong` | `varlong` |
| `Byte` | `i8` |
| `Unsigned Byte` | `u8` |
| `Short` | `i16be` |
| `Unsigned Short` | `u16be` |
| `Int` | `i32be` |
| `Long` | `i64be` |
| `Float` | `f32be` |
| `Double` | `f64be` |
| `Boolean` | `boolean` |
| `String (n)` | `string(n)` |
| `Identifier` | `identifier` |
| `UUID` | `uuid` |
| `Position` | `position` |
| `Angle` | `angle` (add if missing) |
| `NBT` | `nbt` (tag id byte, then the payload; a lone `00` is "absent") |
| `Text Component` | `text_component` |
| `JSON Text Component` | `string(262144)` |
| `Entity Metadata` | `entity_metadata` |
| `Slot` | `slot` |
| `Hashed Slot` | `hashed_slot` |
| `BitSet` | `bitset` |
| `Fixed BitSet (n)` | `fixed_bitset(n)` |
| `LpVec3` | `lpvec3` |

## Structure rules

- Fields keep wiki order and names, lowercased, spaces → `_`, non-alphanumerics dropped.
- `Prefixed Array of X` (or `Array of X` with a preceding length VarInt):
  ```
  count: varint
  items: X[] count = count
  ```
  where `count` is the name of the length field actually on the wire (often the
  preceding field named `Count`, `Length`, or `Number of ...`). Use that field's name.
- `Array of X` with a count known only from context (e.g. "one per bit set") and it runs
  to the end of the packet: `items: X[] until_end`.
- `Prefixed Optional X` / `Optional X` (vanilla `ByteBufCodecs.optional`, `writeNullable`):
  ```
  has_x: boolean          // always a boolean, never a varint
  if has_x {
      x: X
  }
  ```
  A `holder` (`Holder<T>`) is different: a varint of registry id + 1, where `0`
  means the value follows inline.
- Conditional field (`if ...`, "Only present if ..."):
  ```
  if <condition> {
      field: type
  }
  ```
  Only when the condition references an already-decoded field; otherwise `body: rest`.
- `match` / `switch` when several fields depend on an already-decoded value:
  ```
  action: varint
  match (action) {
      0 => { title: text_component  health: f32be }
      2 | 5 => { flags: u8 }
      _ => { }
  }
  ```
  `switch (action)` is the same statement.
- `match` as a field type for a tagged union (entity metadata kinds, sound event ID-or-inline):
  ```
  kind: varint
  value: match kind { 0 => i8, 1 => varint, _ => varint }
  ```
- No trailing prose. No `encoding`/`protocol`/`enum` declarations. No imports.

## Fallback rule

If **any** field of a packet cannot be transcribed with the rules above (unknown type,
registry-dependent layout, `Varies` payload, counts not derivable), the whole packet
**must** be:

```
packet NAME {
    body: rest
}
```

Never guess a layout. A wrong field order is worse than `rest`.

## Where the wiki is wrong

Found by checking packets against vanilla's source and Minestom's recorded bytes:

- Advancement tab coordinates `x`/`y` are not inside the display: they follow the
  whole advancement, once per mapping.
- Test Instance Block Status `size` is an optional `Vec3i` (three VarInts), not
  three Doubles.
- A chat filter mask's partially-filtered bit set is a length-prefixed byte array,
  not a list of longs; chunk light masks use the same form.
- A chat type is a holder (registry id + 1, or `0` and an inline definition), and
  `Delete Chat` carries a packed message signature (id + 1, then the 256 bytes only
  when that is `0`).
- `Teleport Entity` (position, delta, rotation, flags) and `Entity Position Sync`
  (a position path) are two different packets; do not merge their fields.

## Verification

```
bml check <your file>
```

must print `ok`. If it instead says a name is not declared, it prints the `import`
line to add.
