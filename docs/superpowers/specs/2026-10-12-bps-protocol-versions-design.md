# BPS — Minecraft Java protocols 1.13 → 26.3: design

**Status:** built (2026-10-12). Direction taken on the user's request ("add all the protocols from
1.13 → 26.3"); the choices below were made without a review round and are the ones to challenge.
§10 records where the build departed from the first design and why.

## 1. Goal

One BPS schema per Minecraft Java Edition **release protocol** from 1.13 (protocol 393) to 26.3
(protocol 777), every state and direction, every packet, every route, with decoded packet bodies
(no `rest` stubs except where the source declares an opaque tail). 39 protocol numbers cover the
51 release versions in that range (a protocol number is shared by e.g. 1.20 and 1.20.1).

Each schema is a single root file: `bml check schemas/versions/v1_20_1.bps`,
`bml routes …`, `bml decode …`, `bml encode …` work on it unchanged.

## 2. Non-goals

- Snapshots, pre-releases, release candidates (hundreds of protocol numbers).
- Versions before 1.13 and Bedrock.
- New language constructs. The importer lowers every protodef construct to BPS as it exists,
  so the Python reference and the Rust implementation stay interchangeable on the output.
- Rust codegen (`emit rust`, `emit rstom`) for the imported schemas.
- Registry content (block states, item ids). Ids stay opaque integers, as in `schemas/`.

## 3. Source of truth

Hand-transcribing 39 versions of ~150–260 packets from vanilla is not feasible; the standard
machine-readable description is PrismarineJS **minecraft-data** `data/pc/<version>/protocol.json`
(ProtoDef), the data behind mineflayer and node-minecraft-protocol.

| Protocol(s) | Source | Note |
| --- | --- | --- |
| 393 … 775 | minecraft-data `master` @ `dd01127457231e7da629d645b3c6738068018eb0` (2026-10-11) | 34 distinct files |
| 485 (1.14.2) | `pc/1.14.1` | no description of its own; `1.14.3` adds `can_restock` to `trade_list`, which appeared in the 1.14.3 pre-releases (486–490) **[inference]** |
| 776 (26.2) | head of pull request #1333 (`pc_26_2` resync) | not on `master`; chosen over #1298 after both were compared with the decompiled vanilla 26.2 server |
| 777 (26.3) | head of pull request #1301 | not on `master`; patched against the decompiled vanilla 26.3 server and checked against the hand-written schema |

The upstream files are **vendored byte for byte** under `data/minecraft-data/` (`sources.json`: repository,
commit, upstream path, SHA-256; `NOTICE.md`: the licence statement, upstream has no LICENSE file), so
imports are offline and reproducible.

The descriptions are wrong in places. For 26.1 to 26.3 vanilla server sources exist locally, so
corrections are **patches** (`schemas/versions/patches/<ver>.json`): a patch replaces one named
type of one scope with a corrected definition and carries the vanilla source file that decides it.
The vendored bytes are never edited. Older versions have no vanilla sources here and are used as
published; §9 says what that risks.

`schemas/minecraft_26_3*.bps` (hand-transcribed from vanilla 26.3, validated against Java probes)
stays the authority for 26.3; `v26_3.bps` exists so the version set is uniform and so the importer
can be graded against an independent schema.

## 4. Approach

A protodef → BPS **importer** in the existing crate (`crates/bps/src/protodef/`, zero
dependencies), exposed as `bml import-protodef` / `bml import-versions`. Output is committed
(`schemas/versions/`), and `import-versions --check` fails when a committed file is stale, the
way `emit rstom-tree --check` does.

Alternatives rejected:

- *Hand-written schemas per version*: ~9 000 packet definitions, no way to review them.
- *DSL "version overlays"* (`@26.2` diffs): a language change, and the Python/Rust pair would
  have to learn it together; the data does not come as diffs anyway.
- *Python importer*: the Rust crate is the maintained implementation and cargo is the test
  runner. The Python implementation still loads and checks every generated file.

### 4.1 Layout

```
data/minecraft-data/pc/<ver>/protocol.json   vendored upstream files; sources.json, NOTICE.md
schemas/versions/manifest.json               protocol → releases, data file, patch file, notes
schemas/versions/patches/<ver>.json          corrections with their vanilla evidence
schemas/versions/v1_13.bps … v26_3.bps       generated, one per protocol number, named after the
                                             newest release sharing it (v1_20_1 for 763)
schemas/versions/README.md                   table, value shape, verification record, known limits
crates/bps/src/protodef/                     model, scope, lower, ir, render, lint, prelude, versions
crates/bps/src/wire.rs, wiregen.rs           `wire-check` (vectors) and the schema-directed generator
tools/protodef-oracle/                       optional Node differential tool (§7)
scripts/verify_versions.py                   runs the oracle over every version, both directions
                                             (`--pin` refreshes the committed vectors)
scripts/python_parity_versions.py            Python reference vs Rust on every generated schema
scripts/patch_reach.py                       which older descriptions share what the 26.1 audit fixed
scripts/fetch_vanilla.py                     decompiles the vanilla servers (26.1+) the patches cite
crates/bps/tests/fixtures/protodef_vectors/  one oracle packet per packet type and version
```

Each generated file is self-contained: `import "../std/integers.bps"`, `import "../std/varint.bps"`,
then a prelude on demand (UUID, NBT flavours, `lp_vec3`, `entity_delta`), the protocol's named
types, the packets, and the `protocol minecraft @ v… { id = N … route … }` block. (Superseded:
since the shared-declarations build, roots are thin and declarations live in
`schemas/versions/common/`.)
`minecraft_core.bps` is not used: it encodes 26.x semantics (anonymous NBT roots) and must not
couple to the versions.

### 4.2 Translation (protodef → BPS)

Decode semantics follow the **compiled** ProtoDef used by node-minecraft-protocol
(`compiler-structures.js`, `compiler-conditional.js`, `compiler-utils.js`), which is the
behaviour the community data is tested against.

| protodef | BPS |
| --- | --- |
| `i8…i64`, `u8…u64`, `f32`, `f64`, `bool`, `varint`, `varlong` | `std/` encodings (`i8`, `u16be`, `f32be`, `boolean`, …) |
| `pstring{countType:varint}` | the `string` encoding `{ byte_len: varint, value: bytes(byte_len) as utf8 }` (no length cap: protodef has none) |
| `buffer{countType}` / `buffer{count:n}` | `{ len, data: bytes(len) }` / `bytes(n)`; `restBuffer` → `rest` |
| `UUID` | `uuid` (`msb`, `lsb`) |
| `container` | packet body / `encoding` / inline `{ … }`; `anon` fields are flattened |
| `array{countType}` | `<name>_count: T` and `<name>: E[] count = <name>_count` |
| `array{count:ref}` | `E[] count = ref` |
| `option` | `has_<name>: boolean`, then the value under `if has_<name>` (or a `match` statement, see below) |
| `mapper` | the underlying integer; arms keyed by a mapper name are rewritten to the raw integers, with the compiled mapper's rule that an unmapped or falsy-mapped value stays the raw number |
| `bitfield` | one `uN` raw field plus a computed value per member (`sign_extend` when signed) |
| `bitflags` | raw field plus one computed boolean per flag (`<name>_<flag>`) |
| `switch` | `<name>: match tag { … }` (numeric, bool, mapper, bit members); a string-keyed switch becomes `if tag == "…"` groups; an option-valued tag becomes `has ? x : sentinel`; a switch whose arms are options and that a later switch reads stays visible as `has_<name>`/`<name>` |
| anon `switch` | a `match` statement (arms splice into the enclosing block) |
| `topBitSetTerminatedArray` | a recursive encoding: first byte, element, `if (byte & 0x80) != 0 { next: … }` |
| `entityMetadataLoop` | `entries: E[] until 0xFF` |
| `registryEntryHolder(Set)` | inline block over the leading varint (`0` → inline value, `n` → id `n-1`) |
| `nbt`, `optionalNbt`, `anonymousNbt`, `anonOptionalNbt` | prelude encodings (named vs anonymous root; a lone `00` is the absent form) |
| `lpVec3`, `entityDelta` | prelude encodings (`entityDelta`: a 26.3 native, spec from vanilla `VecDelta`) |
| parametrised user types (`particleData`, `entityMetadataItem`, `$compareTo`) | expanded at the use site with the path substituted |

BPS binds fields by name in a flat environment that nested blocks inherit; ProtoDef resolves
`../x`, `a/b` and lexical names to a specific scope. The importer resolves every reference to a
(scope, field) pair first, then assigns names so that no scope between the reference and its
target re-declares the target's name (the decoder inherits the environment; the encoder merges
a whole block's fields up front, so a later shadow would break an earlier reference).

The decoder skips an `if` group when no bytes are left (a convenience for hand-written trailing
optional groups). A group that is present but legitimately empty would then lose its fields, so
the importer writes presence as a `match` statement whenever the content can be empty.

Packet names: the mapper name (`spawn_entity`); a name used in several states or directions is
suffixed `_<state>_<direction>` for every occurrence. Field names are `snake_case`d; a name that
collides after conversion gets a numeric suffix.

### 4.3 Value shape

The decoded JSON is BPS's, not protodef's: counts and presence flags are explicit fields,
bitfields carry the raw integer next to the derived members, UUIDs are `{msb, lsb}`. Nothing is
compared value-by-value across the two systems, only bytes (§7).

## 5. Error handling

The importer refuses what it cannot translate faithfully, with the type path and the version in
the message: an unresolved `compareTo`, a `switch` arm that cannot be flattened, an unknown
constructor, a name it cannot make unique. Every generated file is parsed and run through a
name-flow lint (each identifier an expression uses must be bound by then) before it is written.
There is no silent fallback to `rest`.

## 6. Hermetic gates (cargo)

1. Importer tests on small protodef documents, one construct each, parsed and decoded by the real
   BPS parser and interpreter (`test_protodef_import.rs`).
2. `import-versions --check` over the committed files.
3. Per version: the schema loads, declares its protocol number, and its route table equals the
   (patched) description's id table.
4. Per version: `bps::wiregen` draws random bytes for every packet, steered at the `match` arms and
   `if` groups; strict decode then encode must reproduce them (12 seeds in CI; 300 seeds reach
   94–99% of the branch sites with no failure).
5. Pinned vectors: one oracle-generated packet per packet type and version, committed under
   `crates/bps/tests/fixtures/protodef_vectors/`, decoded and re-encoded without Node.
6. 26.3: packets drawn from the hand-written schema decode and re-encode under `v26_3.bps`, except
   for an explicit list of packets where the hand-written schema is deliberately looser (it stops
   at registry boundaries), each with its reason; a stale entry fails.

## 7. Differential oracle (optional tool)

`tools/protodef-oracle/` compiles the *same* `protocol.json` (patches applied) with ProtoDef and the
node-minecraft-protocol natives (`varlong` replaced by Mojang's 64-bit VarLong, `entityDelta`
added), and compares bytes:

- JS → BPS: random packets generated type-directed from the protodef tree and serialised by JS; BPS
  strictly decodes and re-encodes; the bytes must be identical.
- BPS → JS: random packets from `bps::wiregen`; JS must decode every byte. Re-serialisation must also
  match except for four shapes the ProtoDef JS value model cannot represent (`js-lossy`).

`scripts/verify_versions.py` runs both directions over all 39 versions.

## 8. Cross-checks beyond the shared source

- **26.3**: `patches/26.3.json` holds 21 corrections, each citing the decompiled vanilla 26.3 source that
  decides it. `v26_3.bps` is also compared with the hand-written, Java-probe-validated
  `schemas/minecraft_26_3*.bps`: of 6 240 packets drawn from the hand-written schema, 6 069 decode and
  re-encode unchanged under the imported one, and the other 171 are 13 packet types where the
  hand-written schema is looser (it stops at registry boundaries: slot component counts, recipe displays,
  advancement icons); the test lists those with their reasons and fails if one stops disagreeing. The
  comparison found one bug in the hand-written schema (`commands_node` read a name for node type 3),
  fixed against vanilla `ClientboundCommandsPacket`.
- **26.1 and 26.2**: the descriptions were diffed against each other and against the patched 26.3; every
  difference was explained from the decompiled vanilla sources of the versions involved or patched
  (`patches/26.1.json`: 10 corrections, `patches/26.2.json`: 12). Real vanilla changes between the
  versions (for instance `entity_look`'s `onGround` moving, `update_sign`) are not patched: each
  description is right for its own version.
- **Python parity**: `scripts/python_parity_versions.py`: `dump-json` of every generated schema is
  byte-identical in the Rust and Python implementations and 43 626 generated packets decode to
  identical JSON in both.
- **Oracle record**: `scripts/verify_versions.py --count 100 --seeds 100`: 727 100 packets in each
  direction over 39 versions, 0 failures, 709 `js-lossy` packets (all in versions from 1.21.3).

## 9. Risks and known limits

- minecraft-data is community-maintained. Its 26.2 and 26.3 data came from open pull requests and needed
  12 and 21 corrections against vanilla; 26.1 (merged `master` data) needed 10, most of them
  long-standing errors that vanilla has not changed for many versions. Versions before 26.1 were not
  compared with a vanilla jar, so their descriptions can carry the same errors. The oracle proves fidelity
  to the data, not to Mojang's code. `schemas/versions/README.md` lists, per finding, the older versions
  that have the same wrong shape (`scripts/patch_reach.py`); three of them change which bytes are read.
- The data labels some VarLong fields `varint` from 1.19 on (border speeds, structure block seed, block
  change entries): the wire is identical until a value needs more than five bytes.
- ProtoDef-compiled and BPS differ on lenient reads (see §4.2); only well-formed packets are compared.
- Output size: ≈2.5 MB of generated schemas and patches, ≈7 MB vendored data, ≈1 MB pinned vectors.

## 10. As built: departures from the first design

- **Patches** (§3) were not in the first design. They exist because the 26.2 and 26.3 descriptions turned
  out to be wrong against the vanilla jars in many places; a patch layer keeps the vendored bytes pristine
  and each correction reviewable.
- **`bps::wiregen`** replaced the first plan of reusing `bps::synth` for round trips: `synth` draws values by
  encoding *name* (a `slot` or `nbt` it knows from the hand-written schema), which clashes with the imported
  encodings of the same names. The generator writes bytes by walking the schema and shares nothing with `encode`.
- **26.2 source**: #1333 rather than #1298 (the vanilla comparison: both have the right ids, #1298 only adds
  wire-neutral differences from 26.1).
- **`match` for possibly-empty groups** (§4.2) was found by the generated round trips, not designed.
- The oracle's first `varlong` was signed LEB128; Minecraft's VarLong is the unsigned-shift form (a negative
  value takes ten bytes). BPS was right and the oracle was fixed.
