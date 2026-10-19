# protodef-oracle

Differential oracle for the BPS protocol schemas. It compiles a
minecraft-data `protocol.json` exactly the way node-minecraft-protocol does
and exchanges raw packet bytes with the BPS Rust tools through JSON Lines.

## Install

Requires Node.js ≥ 18.

```sh
cd tools/protodef-oracle
npm ci
```

Dependencies are pinned exactly in `package.json` / `package-lock.json`:
`protodef 1.19.0`, `prismarine-nbt 2.8.0`, `uuid-1345 1.0.2`.

## Reference semantics

Per `(state, direction)` scope, the setup mirrors node-minecraft-protocol:

```js
const compiler = new ProtoDefCompiler()
compiler.addTypes(require('./vendor/compiler-minecraft'))
compiler.addProtocol(protocolJson, [state, direction])
nbt.addTypesToCompiler('big', compiler)
const proto = compiler.compileProtoDefSync()
// root type 'packet': a `varint id + payload` buffer
proto.createPacketBuffer('packet', { name, params })
proto.parsePacketBuffer('packet', buf)
```

`vendor/` holds the node-minecraft-protocol natives (master commit
`1c8318927`, MIT licence; see the header of each file):
`compiler-minecraft.js`, `minecraft.js`, `lpVec3.js`.

### Deliberate deviations from upstream

1. **varlong override.** Upstream implements `varlong` with the 32-bit
   `readVarInt`/`writeVarInt`/`sizeOfVarInt`, corrupting values outside the
   int32 range. The vendored `minecraft.js` instead implements Mojang's
   `FriendlyByteBuf.readVarLong`/`writeVarLong`: unsigned base-128 groups of
   the 64-bit two's-complement pattern (`value >>>= 7`), so a negative long
   always takes ten bytes, exactly as the VarInt of an int takes five. (An
   earlier version of this file used signed LEB128, which is not Minecraft's
   encoding.) Values are BigInts in [-2⁶³, 2⁶³-1] and the generator produces
   the full range.
2. **entityDelta addition.** Upstream has no such type; some 26.3 protocol
   data declares `"entityDelta": "native"`. The vendored natives implement
   it per vanilla 26.3 `VecDelta.java` + `ClientboundMoveEntityPacket.java`:
   wire is `varint properties`, `stepCount = properties >>> 1`; stepCount 0
   means three big-endian i16 (xa, ya, za), otherwise stepCount repetitions
   of `varint ticks` + three big-endian i16. Value shape is
   `{ properties, linear: { x, y, z } }` or
   `{ properties, steps: [{ ticks, x, y, z }] }`. Like vanilla, a step count
   that cannot fit in the remaining bytes is rejected with a short-read
   error. The generator produces both forms (`properties` always agrees
   with the payload: stepCount 0 → 0/1, k steps → `(k << 1) | onGround`).

Two codec quirks are mirrored faithfully rather than worked around:

- `topBitSetTerminatedArray` read **mutates** its input (clears bit 7 of
  each element's first byte). Every parse in this tool runs on a copy.
- Compiled `mapper` read is lenient (`mappings[value] || value`), mapped
  values are sanitised (numeric-looking → number, `'true'`/`'false'` →
  boolean), and `switch` uses strict equality (numeric keys match numbers,
  `'true'`/`'false'` match booleans, other keys match strings). The
  generator reproduces all three, including switch-default selection for
  unmapped raw mapper values.

## Modes

Every mode takes `--patches <patch.json>`: a `schemas/versions/patches/*.json` file whose named types
replace or extend the description before it is compiled, exactly as `bml import-versions` applies
them, so the oracle and BPS read the same corrected description.

### `list` — route tables

```sh
node oracle.js list --protocol <protocol.json>
```

Prints `state direction 0xID name` lines, one per declared packet, ordered
by state (`handshaking, status, login, configuration, play`), direction
(`toClient, toServer`), then numeric id. Ids are lowercase hex (`0x0`,
`0x1a`). Compare against the BPS side's route tables.

### `gen` — random valid packets

```sh
node oracle.js gen --protocol <protocol.json> [--count N=20] [--seed S=1] \
    [--only <state>.<direction>.<packet>] [--depth D=3]
```

For every packet in the protocol's packet mapper, emits N JSON lines on
stdout:

```json
{"state":"play","direction":"toClient","name":"spawn_entity","id":1,"hex":"<payload bytes AFTER the packet-id varint>"}
```

The generator walks the ProtoDef tree (containers in order with a scope for
references; `count`-referenced fields kept consistent; `buffer{count: n}`
exact; countType arrays 0–3 elements; `option` present/absent; bitfields in
range; bitflags with `_value: 0`; `entityMetadataLoop` items never starting
with the end byte; `topBitSetTerminatedArray` with ≥ 1 element starting
below 128, both enforced by serialising candidate elements through probe
types; `registryEntryHolder`/`Set` both shapes; NBT bounded by `--depth`;
strings mix ASCII/BMP/astral; floats finite with f32 pre-rounded;
varint/int32 edges; varlong/i64/u64 full 64-bit as BigInt; switch arms
selected exactly as the compiled code would, with compare fields biased
~60% toward arm keys so arms (not just defaults) get exercised, and mapper
default arms exercised via unmapped raw values ~30%).

Determinism: every draw comes from one mulberry32 stream seeded by `--seed`.

Self-check: each candidate is parsed (on a copy) and re-serialised, iterated
to a fixpoint (covers lossy codecs such as lpVec3 quantisation). Candidates
with no fixpoint are NOT emitted; they are counted and reported on stderr:

```json
{"unstable": true, "state": "...", "direction": "...", "name": "...", "id": 0, "reason": "generate|encode|parse|fixpoint|id", "detail": "..."}
```

A final `gen: packets=G unstable=U` line goes to stderr. Exit code is 0
unless arguments are bad.

### `check` — verify someone else's bytes

```sh
node oracle.js check --protocol <protocol.json> [--stdin | --file vectors.jsonl]
```

Reads `{"state","direction","id"?,"name"?,"hex"}` lines (payload after the
id; the packet is identified by `id` or `name`). For each: prepends the
varint id, parses with the compiled proto **on a copy**, requires **all**
bytes to be consumed, re-serialises and requires byte equality.

Failure lines go to stdout (one JSON object each):

```json
{"ok":false,"state":"...","direction":"...","name":"...","id":0,"reason":"...","detail":"..."}
```

Reasons: `bad-json`, `bad-input`, `unknown-scope`, `unknown-packet`,
`id-name-mismatch`, `compile`, `parse`, `short` (truncated input),
`trailing` (bytes left unconsumed), `reserialise`, `mismatch`. A human
summary (`check: ok=O fail=F total=T reasons={...}`) goes to stderr, and
the exit code is 1 when any vector fails.

## How the BPS side uses it

```sh
# 1. Compare route tables.
node oracle.js list --protocol data/minecraft-data/pc/1.20/protocol.json
# 2. Produce differential vectors (20 per packet here).
node oracle.js gen --protocol data/minecraft-data/pc/1.20/protocol.json --count 20 --seed 1 > vectors.jsonl
# 3. Strict decode + re-encode check on the Rust side.
bml wire-check schemas/versions/v1_20_1.bps --file vectors.jsonl
#    prints `checked N, failed M`, failures on stderr, exit 3 on failure.
# 4. Sanity: the oracle must accept its own output.
node oracle.js check --protocol data/minecraft-data/pc/1.20/protocol.json --file vectors.jsonl
```

Compiled protos are cached per `(state, direction)` inside one process, so
one invocation covers a whole protocol file efficiently.

`scripts/verify_versions.py` runs this for every entry of
`schemas/versions/manifest.json`, with the entry's patch file, in both directions: JS → BPS is the
`gen` + `wire-check` pair above; BPS → JS feeds `check` with random packets drawn from the BPS
schema (`bytemill::wiregen`, via the `write_generated_vectors` cargo test). A packet the oracle decodes
with every byte consumed but cannot write back (four shapes ProtoDef's JS value model cannot
represent, named in the script) is counted as `js-lossy`, anything else is a failure.

## Layout

- `oracle.js` — CLI (`list` / `gen` / `check`), packet assembly,
  parse→re-serialise fixpoint, check-mode verification.
- `lib/proto.js` — protocol loading, per-scope compilation (the exact
  node-minecraft-protocol recipe), packet tables, first-byte probe
  registration.
- `lib/gen.js` — seeded type-directed value generator (`Generator`,
  lexical scopes, NBT depth bound, mapper/switch/count semantics above).
- `vendor/` — vendored node-minecraft-protocol natives plus the two
  documented deviations.
