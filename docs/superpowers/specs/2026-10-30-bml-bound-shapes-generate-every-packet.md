# Bound shapes: every rstom packet payload generated from BML

Status: implemented. `bml emit rstom-tree` writes all 236 packet payload modules under
`crates/netherstar/src/network/packet/` (235 classes; `ShowDialogPacket` has a play and a
configuration unit). `bml shapes report` lists every class as `generated`.

This follows `2026-10-12-bps-generates-rstom-packets-waves-2-5-design.md`, whose rule 8 stands:
vanilla wins where the hand port (Minestom) and the wire schema disagree.

## Why shapes had to be bound

A shape of the first generation mirrors the BML packet one to one: each hand field is a wire field
of the same name and type, and the emitter writes them in BML order. That covered 105 packets.
The other 131 hand packets keep a different structure than the wire has: a `Vec` over a count and
a repeat, an `Option` over a flag and a guarded field, a `Pos` over five scalars, an enum written as
its ordinal, a nested record, a codec that is a hand function (`Component`, `LightData`,
`ArgumentSignatures`). A bound shape says how the hand fields map onto the wire fields, and the
emitter writes the conversions. Nothing is guessed: a wire field no hand field carries, derives or
gets a constant for is an error.

## The binding layer

A shape with `"bound": true` renders through the bound emitter (`crates/bytemill/src/tree_bound/`). Each
hand field says which wire fields it carries and how the hand value converts to and from them: a codec
that is the hand type, `write` and `read` templates, or buffer functions. Lists bind their elements,
hand structs flatten over the wire fields of their scope, and a guarded field the read skips takes a
default. A count is derived from its list and a flag from its option unless the shape gives the wire
field a value. The keys and their exact semantics are documented where they are implemented: `FieldBind`
in `shapes/model.rs`, `tree_bound/mod.rs`, and `tree_bind.rs` for the shared table below.

Mistakes are errors at emit time, not guesses: a wire field no hand field carries, derives or gets a
constant for; a constant that names no uncarried wire field; an unknown hook; a field over several wire
fields without a conversion.

`alias` shapes add a serializer to a hand type another packet module defines, with no type of their own
(`ShowDialogPacket::inline_serializer`, the configuration form).

## The bindings table

`targets/rstom/bindings.json`, beside the shapes directory, holds the conversions that hold for a hand
type in every packet, so a shape does not repeat them (`PlayerHand` as its ordinal, `Pos` over five
wires, `Component` through its buffer functions, the `Copy` hand types). A shape's own keys win over it.

## What a swap keeps

Binding does not weaken the swap rules of the earlier waves. The audit still refuses a hand serializer
carrying a non-wire construct (assertion, panic, limit, conversion, clamp, helper call) that neither a
hook contains nor `accepted` records with a reason. Every hand item must land in the generated file, the
`.ext.rs` sibling or the dropped helpers. Doc and attribute lines may not disappear. A refused run
writes nothing. The siblings are hand-owned afterwards and are never overwritten.

`bml shapes wires` lists the flattened wire idents a class's `wire` keys name, `bml shapes render`
previews what a shape generates without the swap checks, and `bml shapes fmt` rewrites a ledger into the
canonical form after authoring by script.

## Wire policy

The pin gate reads BML-synthesized bytes through each registry serializer and requires the same bytes
back. Where the hand packet could not pass, the earlier waves listed it in `wire_pins_known.json`. With
every packet generated the list shrinks to the limits of the hand types:

- `DeleteChatPacket` (play 31): `MessageSignature` holds only a full signature, so a cached reference
  (`id + 1`) has no representation. `domain-limit`.
- `ShowDialogPacket` (play 143): the hand holder uses registry id 0 as its direct form, so a reference to
  registry entry 0 cannot be told from a direct dialog. `domain-limit`.

A `domain-limit` entry does not stop a swap: the limit is the hand type's, and the generated packet
keeps it.

The other known entries and one the pins cannot see are generated with vanilla's layout:

| Packet | Vanilla |
|---|---|
| `ClientChangeDifficultyPacket` | the difficulty alone; `locked` stays a hand field, read as `false` |
| `ClientSetGameRulesPacket`, `GameRuleValuesPacket` | the rule key as an identifier string |
| `ClientUpdateStructureBlockPacket` | the seed as a VarLong |
| `DeleteChatPacket` | id 0, then the 256 signature bytes |
| `MoveMinecartPacket` | yaw and pitch as `floor(angle * 256 / 360)` bytes |
| `DeclareRecipesPacket` | the namespaced property key |
| `InitializeWorldBorderPacket` | `warningBlocks` before `warningTime` (equal synthesized pin values hide it) |
| `ShowDialogPacket` (configuration) | the bare dialog tag, from the generated `inline_serializer` |

The fixtures recorded from Minestom do not describe these. rstom's `probe_support::VANILLA_WINS` names
them, the replays skip them, and each packet's own `.tests.rs` pins the vanilla bytes.

## Adding or changing a packet

1. Write the shape in the ledger: the hand fields, bound to the wire fields (`bml shapes wires`).
2. Preview it with `bml shapes render`; run `bml shapes fmt`.
3. Hooks and `accepted` entries for behaviour the wire layout does not carry.
4. `bml emit rstom-tree schemas/minecraft_26_3_wiki.bml --rstom ../rstom`, then `--check`.
5. In rstom: `cargo test -p netherstar --test bml_wire_pins`, the packet tests, the probes.

## Verification

- `bml emit rstom-tree ... --check` prints `ok`; `bml shapes report` lists 236 `generated`.
- `cargo test -p netherstar` passes: the pin gate, 2109 library tests and every probe suite.
- `cargo test -p bytemill` passes apart from the suites that need `mypy`, `zig` or the TypeScript
  toolchain, which are absent from this machine.
