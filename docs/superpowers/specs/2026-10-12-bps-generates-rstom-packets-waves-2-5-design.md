# BPS generates rstom's packet layer: design (waves 2-5)

**Status:** continues `2026-10-11-bps-generates-rstom-packets-design.md` (waves 0-1; landed on
the autoresearch branch as `470563e5`). The user asked to continue with the other waves; the
direction ("it should generate everything", vanilla wins, hand API kept) is unchanged. This spec
fixes the design shared by waves 2-5 and the detail of wave 2. Waves 3-5 get their own plan at
their start, from fresh evidence (section 8).
**Date:** 2026-10-12

## 1. State and evidence (measured 2026-10-12)

- 64 of 236 matched packets are generated in place; 172 are hand files. `shapes report` blocks
  them for these reasons: 57 hold other public items in the file, 49 differ in field names or
  order, 48 in field count, about 15 in field types, and a few are special.
- Every non-eligible hand packet was aligned to its BPS wire fields by script after eliding the
  wire fields that a count or a presence flag explains (`/tmp/pins/survey/align.py`, rerun by
  `shapes report` from wave 2 on):

| class | packets | meaning |
|---|---|---|
| direct | 46 | each hand field maps to one wire field; names differ or counts and flags are elided |
| enum / cast | 22 + 7 | the hand type is an enum, `i8`, `u32`, `f32` over a wire integer |
| byte arrays | 4 | `Vec<u8>` over a counted byte list |
| domain | 19 | `Component`, `ItemStack`, `DebugUpdate`, `MessageSignature`, ... over opaque wire bytes |
| record lists | 16 | `Vec<Entry>` over a list of tuples; `Entry` is a hand type in the same file |
| combined / other | 5 + 4 | two of the above, or a single odd conversion |
| arity | 47 | one hand field spans several wire fields (`Pos`, nested records), or a union |
| no hand file | 1 | `ShowDialogPacket`, configuration form |

  The cumulative ladder (a packet is counted when every capability it needs is present):
  direct 46, then enums 68, casts 75, byte arrays 79, domain codecs 100, record lists 117. The
  other 55 need groups, unions or transforms.
- The hand write order equals the struct declaration order in 142 of 171 packets. Five packets
  write in another order (`UnloadChunkPacket`, `EntityRotationPacket`, `EntityPositionPacket`,
  `EntityPositionAndRotationPacket`, `SpawnEntityPacket`). Aligning by position alone would swap
  `UnloadChunkPacket`'s two fields; the pin gate cannot see it (it decodes and re-encodes the
  pinned bytes, and every scalar pin is `1`).
- 110 files under the packet tree carry inline tests. In wave 1 all 35 dropped tests were wire
  round trips; later waves have 29 pure behaviour tests and 13 mixed ones (constructor
  validation, accessors, builders, `should_panic` limits).
- Behaviour that is not wire: `new` validation in about 15 packets, list and string limits in 32
  files (`read_size`, `.list(MAX)`, `write_string_list`, assertions), enum conversions that
  panic out of range (Java `VALUES[i]`), helpers and constants in about 35 files. The limits are
  inconsistent between packets: `DestroyEntitiesPacket::read` checks `0..=MAX`,
  `PostEffectsPacket::read` does not, `PlayerInfoRemovePacket` checks through `UUID.list(MAX)`.
- The hand tree has three packet-local `Component` stubs and one ported `Component` that no
  packet uses; three `ItemStack` types; two `ItemStackHash` types. A binding must name the type
  by import path.
- rstom has no `frames` module. The in-tree template refuses `crate::frames::` (48 packets).
  A hand domain codec removes the need for a walker: `Component::read(buffer)` frames itself.

## 2. Decisions

1. **Contract unchanged.** Names, module paths, field names and types, derives, `new()`
   signatures and bodies, serializer names and visibility, and docs of the hand file are kept.
   The wire is what the vanilla 26.3 client accepts.
2. **Bindings join the shape.** Each hand field may name the wire field it carries
   (`FieldShape.wire`). A hand field with no `wire` carries the wire field of the same name.
   Wire fields that no hand field carries must be derivable: the count of a bound list, or the
   presence flag of a bound `Option`. Anything else is an error that names the wire field.
3. **The wire body is projected, not rewritten.** `_packet_body` takes a `Projection`: for each
   wire identifier the expression that yields its value from the hand struct on write
   (default `value.<ident>`), and the expression that builds each hand field from the read
   locals (default the local of the same name). The default reproduces today's text, so the
   standalone `emit rstom` output stays byte-identical (checked on all 501 files).
4. **Everything that is not API or wire moves to siblings.** `emit rstom-tree` creates
   `<stem>.ext.rs` (other items, other impl items, constants, helper functions, `use` lines they
   need) and `<stem>.tests.rs` (the inline `#[cfg(test)]` module body) from the hand file when
   they do not exist, and a conservation check proves that every item of the hand file reappears
   in the generated file, the ext file or the tests file. The generated tail declares them:
   `#[path = "<stem>.ext.rs"] mod ext;` with `#[allow(unused_imports)] pub use ext::*;`, and
   `#[cfg(test)] #[path = "<stem>.tests.rs"] mod tests;`. **This amends the wave-1 rule:** inline
   tests are no longer dropped. They move, and they run against the generated serializer, which
   is a differential check that the pin gate cannot give (hand assertions on distinct values).
5. **Serializer behaviour is audited, fail closed.** `shapes audit` lists every non-wire
   construct in a hand `write` or `read` body (`panic!`, `assert!`, `expect`, limits, clamps,
   `read_size`, `.list(`, conditionals that are not presence guards). Each construct is either
   reproduced by a **hook** (verbatim Rust lines the shape inserts at a named point:
   `write.start`, `read.after:<wire ident>`, `read.end`) or recorded under `accepted` in the shape
   with a reason. A packet with an unresolved construct is not swappable. Wire-valid behaviour
   (bytes for valid values, round trips) and the API are the contract; the text of a panic on
   invalid input is not, unless a carried test asserts it.
6. **Domain types stay hand-written and are bound.** Waves 3-4 add a binding table
   (`targets/rstom/bindings.json`) keyed by the hand type's import path: its wire encoding,
   read and write expressions, imports and `Copy`-ness. A domain codec replaces the walker, so
   no `crate::frames` appears in rstom. Hot-path packets (chunk data, light, entity metadata,
   entity position) call their hand codec by borrow; the generated write never clones them.
7. **Types are normalised once.** The extractor reads the hand file's imports: a bare `Vec`
   imported from `crate::coordinate` becomes `CoordVec`, and `std::vec::Vec<..>` becomes
   `Vec<..>`; the generated file imports `crate::coordinate::Vec as CoordVec`. `CoordVec`
   and `Pos` are `Copy` and are not cloned.
8. **Vanilla wins on the 9 known deviations**, last, one decision each (wave 4). Where the hand
   API has a field that the vanilla wire lacks (`ClientChangeDifficultyPacket.locked`), the
   field stays in the struct as a non-wire field (default on read, ignored on write) unless a
   ruling says otherwise.
9. **Waves merge on the user's word.** Work goes to the worktree branch
   `bps-packets-waves-2-5` (from the live tip `470563e5`) and to protoc `master`; the live
   branch is not touched again until the user asks, because every merge recompiles the loop.

## 3. Architecture (what changes)

```text
protoc
  bps/shapes/            (was bps/shapes.py, split first, API unchanged)
    model.py             Shape, FieldShape, NewShape, SerializerShape, Hooks, JSON
    extract.py           extract_shape, items/impl/test spans
    split.py             split_hand_file -> (api part, ext part, tests part), conservation
    infer.py             infer_bindings (name first, then position), audit
    ledger.py            load_ledger, worklist report
  bps/bindings.py        Binding table (wave 3), projection builder from a shape
  bps/emit_rstom.py      _packet_body(unit, resolved, projection=None)
  bps/tree_packet.py     renders struct/new/serializer from shape + projection + hooks
  bps/emit_tree.py       plans, splits, conserves, writes (all or nothing)
  targets/rstom/shapes/  ledger (64 now; wave 2 adds ~46)
  targets/rstom/bindings.json   (wave 3)
```

### 3.1 Shape v2 (JSON additions; v1 files stay valid)

```json
{
  "imports": ["use crate::coordinate::Vec as CoordVec;"],
  "fields": [{"name": "target_id", "type": "i32", "docs": [], "wire": "entity_id"}],
  "new": {"params": "target_id: i32"},
  "tests": "client_attack_packet.tests.rs",
  "hooks": {"read.after:entity_ids_count": ["assert!(...)"]},
  "accepted": [{"construct": "...", "reason": "..."}]
}
```

`new.params` is verbatim when it differs from the field list (`impl Into<String>` parameters).
`imports` holds only the hand `use` lines the shape's types, parameters and bodies need, and no
name that the generator imports itself. `extras` and `tests` are file names; when absent from the
workspace, `emit rstom-tree` creates them from the hand file.

### 3.2 Projection

`Projection.write: dict[wire ident, Access]` where `Access(expr, place, copy)`: `place` says
the expression is an lvalue (clone it unless `copy`), otherwise it is used as is.
`Projection.read: dict[hand field, str]` gives the construction expression. Derived wire
fields: `x_count` is `<bound vec>.len() as i32`; a flag `has_x` is `<bound option>.is_some()`.
Guards that read a derived field use the same accessor. `read_via_new` builds the value with
`Cls::new(<args in parameter order>)`.

### 3.3 Binding inference

For each hand field, in order: an exact name match on a wire field; else the first unbound
wire field of an equal type that follows the previous binding; else unresolved. The write order
of the hand serializer (the order in which `write` mentions the fields) overrides declaration
order when the two differ. Every binding with `wire != name` where two or more fields share a
wire type is listed for review in the task report. Unbound hand fields and underived wire fields
are reported by reason; they define the worklist.

### 3.4 Split engine

A rustfmt-formatted hand file is split by top-level item: an item starts at its first doc or
attribute line in column 0 and ends at the closing brace or semicolon in column 0. The inherent
impl of the packet struct splits by function: `new` and `serializer` are generated; every other
function, constant and associated item moves to an ext impl block. `impl Packet` and the marker
traits are generated; every other impl moves. The serializer struct and its impl are generated.
Names the moved text uses come from the hand file's `use` lines.

## 4. Behaviour and test policy

- `new.body` is verbatim (wave 1). `read_via_new` follows the hand read (wave 1, ruling R16).
- Hooks are verbatim hand lines. The conservation check proves each hook line occurs in the hand
  file. A hook may name only wire identifiers and hand fields.
- Carried tests run against the generated serializer. A carried test that fails is a finding:
  fix the binding or hook, or record an accepted deviation with the reviewer's agreement. A test
  is never edited to pass.
- The pin gate stays the wire authority (260 rows, known list unchanged until wave 4).
- Hot-path packets are not touched before wave 4, and then only by borrowed codec calls.

## 5. Waves

| wave | what BPS learns | packets | notes |
|---|---|---|---|
| 2 | rename and reorder, count and flag elision, coordinate-`Vec` alias, hooks, split of items and tests, shape v2, inference and audit | direct 46 | detailed plan now |
| 3 | enum, cast and array conversions, domain codecs (`Component` x3, `ItemStack`, ...), options with conversions; binding table | about 54 (to 100) | plan at wave start |
| 4 | record lists and nested hand types, groups (`Pos`, `WorldPos`, `GameProfile`), transforms (angle, lp-vector, fixed point), unions (boss bar, teams, player info, scoreboard, particles, sound), special impls, the 9 deviations | about 72 (to 172) | plan at wave start; unions may stay a hand codec behind one codec binding |
| 5 | id tables, `ported`, `mod.rs` trees from BPS; retire `rstom-packets`, `packets_gen`, `bps_rstom`; class table moves into protoc; AGENTS.md, PORTING.md, constraint docs | cutover | plan at wave start |

A packet that no capability can reach stays a hand file; the final report names each with its
reason. The end state "no hand payload files" is the aim, not a promise the data has already
made.

## 6. Gates

The wave-1 gates, plus: `shapes audit` clean for every swapped packet; carried tests green; the
three-way conservation (generated + ext + tests = hand file); ledger fixed point (re-extracting
the swapped tree reproduces the ledger); standalone emit byte-identical after every change to
`_packet_body`; `cargo test -p rstom --no-fail-fast` with the same 6 baseline failures; clippy
shows 0 diagnostics in files the wave changed; e2e smoke login to play.

## 7. Risks

- A binding that is positionally wrong round-trips and passes the pin gate. Mitigation: name
  first, write order next, review list, parity probes and carried tests in `cargo test -p rstom`.
- Moved tests may reach private items. Mitigation: ext and tests are child modules; `use super::*`
  sees private parent items (verified in wave 1 for `PluginMessageSerializer`).
- Hooks are arbitrary Rust in a ledger. Mitigation: every hook line is checked against the hand
  file and the audit shows what hooks stand for.
- The loop's machine load. Mitigation: all cargo through `scripts/rstom_cargo.sh`, one at a time.
- Item splitting by text. Mitigation: it relies on rustfmt layout (hand files are formatted), a
  conservation check, and a ledger fixed-point test over all 236 files.

## 8. Acceptance (wave 2)

`shapes report` lists the direct packets as bindable and every other reason as before. The
swapped packets have the generated marker, no hand serializer code and no entry in
`wire_pins_known.json`; `git diff` outside generated files, siblings, the pin gate and docs is
empty (no caller or plugin edit); every gate in section 6 passes; the ledger fixed point holds
for all swapped packets; the standalone `emit rstom` output is byte-identical to before.
