# Wiki → BPS transcription cheat sheet

Source of truth: `/tmp/mc_packets.txt` (Minecraft Java Edition protocol/Packets,
protocol 777). Each packet section is `==== Packet Name ====` with a wiki table of
`Field Name | Field Type | Notes`.

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
| `NBT` | `nbt` |
| `Text Component` | `text_component` |
| `JSON Text Component` | `string(262144)` |
| `Entity Metadata` | `entity_metadata` |
| `Slot` | `slot` |
| `Hashed Slot` | `hashed_slot` |
| `BitSet` | `bitset` |
| `Fixed BitSet (n)` | `fixed_bitset(n)` |
| `LpVec3` | **not supported** → whole packet becomes `body: rest` |

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
- `Prefixed Optional X` / `Optional X`:
  ```
  flag: varint          // presence flag actually on the wire
  if flag != 0 {
      value: X
  }
  ```
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

## Verification

```
python -m bps.cli check <your file>
```

must print `ok`. If it instead says a name is not declared, it prints the `import`
line to add.
