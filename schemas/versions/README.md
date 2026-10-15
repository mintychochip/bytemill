# Minecraft Java protocols 1.13 → 26.3

One BPS schema per protocol number, 393 (1.13) to 777 (26.3): 39 protocols covering all 51
release versions in that range. Each root file is thin: header, imports, and the protocol
block (routes); every packet and encoding declaration lives in a shared file under `common/`,
stored once per set of versions in which it is identical. Loading a root loads its shared
files, so every example works the same:

```bash
bpsc check  schemas/versions/v1_20_1.bps          # parse + resolve
bpsc routes schemas/versions/v1_20_1.bps          # state × direction × id → packet
echo '<hex payload>' | bpsc decode --packet spawn_entity schemas/versions/v1_20_1.bps --hex
bpsc wire-check schemas/versions/v1_20_1.bps --file vectors.jsonl   # decode + re-encode, in bulk
```

A root is named after the newest release that shares the protocol number (`v1_20_1.bps` is 763,
which 1.20 and 1.20.1 share). A shared file is named after the run of consecutive versions it
serves: `from_1_13.bps` (unchanged since 1.13), `1_20_6_to_1_21_11.bps`, `only_1_21_11.bps`,
`1_13_to_1_16_5+1_19_4_to_1_20_4.bps` (a `+` joins runs). `manifest.json` is the table; the
generated files are never edited. To find a declaration:

```bash
grep -l '^packet map_chunk' schemas/versions/common/*.bps
```

| Protocol | Releases | File | Routes | Source (`data/minecraft-data/`) |
| --- | --- | --- | --- | --- |
| 393 | 1.13 | `v1_13.bps` | 143 | `pc/1.13` |
| 401 | 1.13.1 | `v1_13_1.bps` | 143 | `pc/1.13.1` |
| 404 | 1.13.2 | `v1_13_2.bps` | 143 | `pc/1.13.2` |
| 477 | 1.14 | `v1_14.bps` | 152 | `pc/1.14` |
| 480 | 1.14.1 | `v1_14_1.bps` | 152 | `pc/1.14.1` |
| 485 | 1.14.2 | `v1_14_2.bps` | 152 | `pc/1.14.1` (see below) |
| 490 | 1.14.3 | `v1_14_3.bps` | 152 | `pc/1.14.3` |
| 498 | 1.14.4 | `v1_14_4.bps` | 153 | `pc/1.14.4` |
| 573 | 1.15 | `v1_15.bps` | 153 | `pc/1.15` |
| 575 | 1.15.1 | `v1_15_1.bps` | 153 | `pc/1.15.1` |
| 578 | 1.15.2 | `v1_15_2.bps` | 153 | `pc/1.15.2` |
| 735 | 1.16 | `v1_16.bps` | 153 | `pc/1.16` |
| 736 | 1.16.1 | `v1_16_1.bps` | 153 | `pc/1.16.1` |
| 751 | 1.16.2 | `v1_16_2.bps` | 154 | `pc/1.16.2` |
| 753 | 1.16.3 | `v1_16_3.bps` | 154 | `pc/1.16.2` |
| 754 | 1.16.4, 1.16.5 | `v1_16_5.bps` | 154 | `pc/1.16.2` |
| 755 | 1.17 | `v1_17.bps` | 165 | `pc/1.17` |
| 756 | 1.17.1 | `v1_17_1.bps` | 165 | `pc/1.17.1` |
| 757 | 1.18, 1.18.1 | `v1_18_1.bps` | 166 | `pc/1.18` |
| 758 | 1.18.2 | `v1_18_2.bps` | 166 | `pc/1.18.2` |
| 759 | 1.19 | `v1_19.bps` | 169 | `pc/1.19` |
| 760 | 1.19.1, 1.19.2 | `v1_19_2.bps` | 173 | `pc/1.19.2` |
| 761 | 1.19.3 | `v1_19_3.bps` | 172 | `pc/1.19.3` |
| 762 | 1.19.4 | `v1_19_4.bps` | 176 | `pc/1.19.4` |
| 763 | 1.20, 1.20.1 | `v1_20_1.bps` | 176 | `pc/1.20` |
| 764 | 1.20.2 | `v1_20_2.bps` | 197 | `pc/1.20.2` |
| 765 | 1.20.3, 1.20.4 | `v1_20_4.bps` | 203 | `pc/1.20.3` |
| 766 | 1.20.5, 1.20.6 | `v1_20_6.bps` | 220 | `pc/1.20.5` |
| 767 | 1.21, 1.21.1 | `v1_21_1.bps` | 226 | `pc/1.21.1` |
| 768 | 1.21.2, 1.21.3 | `v1_21_3.bps` | 235 | `pc/1.21.3` |
| 769 | 1.21.4 | `v1_21_4.bps` | 237 | `pc/1.21.4` |
| 770 | 1.21.5 | `v1_21_5.bps` | 239 | `pc/1.21.5` |
| 771 | 1.21.6 | `v1_21_6.bps` | 245 | `pc/1.21.6` |
| 772 | 1.21.7, 1.21.8 | `v1_21_8.bps` | 245 | `pc/1.21.8` |
| 773 | 1.21.9, 1.21.10 | `v1_21_10.bps` | 252 | `pc/1.21.9` |
| 774 | 1.21.11 | `v1_21_11.bps` | 252 | `pc/1.21.11` |
| 775 | 26.1, 26.1.1, 26.1.2 | `v26_1_2.bps` | 257 | `pc/26.1` + `patches/26.1.json` |
| 776 | 26.2 | `v26_2.bps` | 257 | `pc/26.2` + `patches/26.2.json` |
| 777 | 26.3 | `v26_3.bps` | 261 | `pc/26.3` + `patches/26.3.json` |

Routes include the legacy server-list ping (`handshaking.serverbound 0xfe`) that the description
lists.

## Where the packets come from

The packet layouts are PrismarineJS [minecraft-data](https://github.com/PrismarineJS/minecraft-data)'s
ProtoDef `protocol.json` files, the data behind mineflayer and node-minecraft-protocol, **vendored
byte for byte** in `data/minecraft-data/` (`sources.json` names the commit, upstream path and
SHA-256 of each file; `NOTICE.md` has the licence). `bpsc import-versions` turns them into the
schemas here: `crates/bps/src/protodef/` is the importer, `docs/superpowers/specs/2026-10-12-bps-protocol-versions-design.md`
the design, the translation table and the reasoning.

- **26.1 and earlier** are minecraft-data `master` at commit `dd01127`. Where one file describes
  several protocol numbers (`pc/1.16.2` for 751, 753 and 754) the schemas are identical apart
  from the protocol number. 1.14.2 (485) has no description of its own: `pc/1.14.1` is used, since
  the next one, `pc/1.14.3`, adds only `can_restock` to `trade_list`, a field that arrived with the
  1.14.3 pre-releases (486–490) *[inferred, not checked against a 1.14.2 jar]*.
- **26.2 and 26.3** are not on minecraft-data `master`: the vendored files are the heads of open pull
  requests (#1333 and #1301).
- **26.1, 26.2 and 26.3 have patch files** (`patches/26.1.json`, `patches/26.2.json`,
  `patches/26.3.json`: 10, 12 and 21 corrections). A patch replaces one named type of one scope
  with a corrected definition and carries the vanilla source file that decides it; every patch is
  generated into the file's header comment. The decompiled vanilla 26.1, 26.2 and 26.3 servers were
  compared with the descriptions packet by packet where they differ between versions
  (`scripts/fetch_vanilla.py 26.1 26.2 26.3` rebuilds those trees from Mojang's server jars), and the
  hand-written, vanilla-derived `schemas/minecraft_26_3_*.bps` remain the authority for 26.3
  (`v26_3.bps` is checked against them, see below). Nothing earlier than 26.1 was compared with a
  vanilla jar: see *Known limits*.

The importer follows the *compiled* ProtoDef that node-minecraft-protocol uses, the behaviour the data
is tested against. Where the description is looser than vanilla it is looser here (see
*Known limits*).

## Decoded values

The decoded JSON is BPS's shape, not ProtoDef's, because BPS binds fields by name in a flat
environment:

- an array's length is a field of its own: `{"items_count": 2, "items": [...]}`;
- an `option` is a presence flag plus, when present, the value: `{"has_x": true, "x": ...}`; an
  absent value has no key;
- a `bitfield`/`bitflags` field keeps the raw integer and adds one derived value per member
  (`position`: `raw`, `x`, `y`, `z`; flags: `flags`, `flags_<name>`); derived values are not encoded;
- a `switch` is `match` on the tag field. A switch whose compared field is a string becomes `if tag == "..."`
  groups; negative keys are literal; an anonymous switch's fields sit beside its siblings;
- a mapper is its raw integer (the names are in the generated comments, not in the value);
- strings decode to strings; UUIDs to `{"msb", "lsb"}`; byte buffers to bytes.

A packet whose name exists in several states or directions is suffixed `_<state>_<direction>`
(`keep_alive_play_clientbound`); a unique name is plain (`spawn_entity`).

## Regenerate and check

```bash
bpsc import-versions            # rewrite schemas/versions/*.bps + common/ from manifest.json + data/
bpsc import-versions --check    # exit 3 listing any committed file that differs (also a cargo test)
bpsc import-protodef data/minecraft-data/pc/1.20/protocol.json --protocol 763 --release 1.20.1 | less
```

Every generated file is parsed and run through a name-flow lint at import time: each identifier an
expression uses must be bound by an earlier field, or the import fails with the packet and the name.

The sharing: 39 roots and 164 shared files, 8 881 declarations (declaration + everything it
references) kept as 1 011 — 44% of the 2.32 MB the self-contained files took. For every version,
the merged schema from the root is proven equal, declaration by declaration, to the single-file
rendering (`shared_layout_means_what_the_single_file_means` in `test_versions.rs`), and the decoded
dumps of 7 271 pinned oracle packets are byte-identical before and after.

## Verification

All of this is repeatable. The cargo tests are hermetic (no Node, no network); the oracle and the
Python parity run need Node and Python.

| Check | What it shows | Where |
| --- | --- | --- |
| Importer, one construct at a time | each protodef construct decodes and re-encodes known bytes, including the scoping hazards (an inner field that shadows a name read from outside, an option read by a later switch) | `crates/bps/tests/test_protodef_import.rs` |
| Route tables | every version's routes equal the (patched) description's id table | `test_versions.rs` |
| Up to date | `import-versions --check` over the committed files | `test_versions.rs` |
| Random packets from the schema itself | `bps::wiregen` writes random bytes for every packet of every version, steered to reach the `match` arms and `if` groups; strict decode then encode must give the same bytes (12 seeds per packet in CI; at 300 seeds: 0 failures, 94–99% of all branch sites reached) | `test_versions.rs` |
| Pinned oracle packets | one packet per packet type and version drawn by the ProtoDef oracle (7 271 packets) decodes strictly and re-encodes byte for byte, with no Node | `test_versions.rs`, `crates/bps/tests/fixtures/protodef_vectors/`; refresh with `scripts/verify_versions.py --pin` |
| **ProtoDef oracle** | the same `protocol.json` (patches applied) is compiled with ProtoDef and node-minecraft-protocol's natives; JS-generated random packets decode strictly in BPS and re-encode byte for byte (JS → BPS), and BPS-generated packets decode in ProtoDef with every byte consumed (BPS → JS) | `scripts/verify_versions.py`, `tools/protodef-oracle/` |
| Rust ↔ Python | `dump-json` of every generated schema is byte-identical in the Rust and Python implementations, and the same random packets decode to identical JSON in both | `scripts/python_parity_versions.py` |
| 26.3 vs the hand-written schema | 6 240 packets drawn from the hand-written, Java-probe-validated `schemas/minecraft_26_3*.bps`: 6 069 decode and re-encode unchanged under `v26_3.bps`; the other 171 are 13 packet types where the hand-written schema is deliberately looser (it stops at registry boundaries), each listed with its reason in the test. The comparison found one bug in the hand-written schema (`commands_node`), now fixed | `test_versions_cross.rs` |
| Vanilla, 26.1 to 26.3 | the corrections in `patches/`, each citing the decompiled vanilla source that decides it | `patches/*.json` |

The last full oracle run, `scripts/verify_versions.py --count 100 --seeds 100`:

```
39 versions: 727 100 packets JS → BPS, failed 0; 727 100 packets BPS → JS, failed 0
js-lossy (decoded fully, JS cannot write the value back): 709 packets, all in versions from 1.21.3
  480 u32 bitflags with bit 31 set      135 option flag over a void payload
   90 non-canonical lpVec3                4 registryEntryHolder with a falsy inline value
```

## Known limits

- **The data is community-maintained.** The oracle proves the schemas say what the data says, not what
  Mojang's code does. 26.1, 26.2 and 26.3 were compared with the decompiled vanilla servers (and 26.3
  with the hand-written schema); **nothing earlier was compared with a vanilla jar**. That audit found
  errors in the 26.1 description that vanilla has not changed for many versions, so the older files can
  carry the same ones. `scripts/patch_reach.py` prints this table; these are unpatched in the schemas:

  | Wrong in the 26.1 description (fixed there by a patch) | Same in the descriptions of |
  | --- | --- |
  | `move_minecart` steps use floats (`vec3f`, `f32` rotations); vanilla writes Vec3 doubles and rotation bytes | 1.21.3..1.21.11 |
  | `custom_click_action` has a presence flag; vanilla length-prefixes the payload | 1.21.6..1.21.11 |
  | `ItemBlockProperty` ranges are mandatory strings; vanilla has optional strings | 1.20.6..1.21.11 |
  | `initialize_world_border.speed` is a `varint`; vanilla writes a VarLong | 1.19..1.21.11 |
  | `world_border_lerp_size.speed` is a `varint`; vanilla writes a VarLong | 1.19..1.21.11 |
  | `multi_block_change.records` are `varint`s; vanilla writes VarLongs | 1.19..1.21.11 |
  | `update_structure_block.seed` is a `varint`; vanilla writes a VarLong | 1.19..1.21.11 |
  | `update_structure_block.flags` is an enum mapper; vanilla writes a bitmask byte | 1.21.5..1.21.11 |
  | `player_info` flag bits 6 and 7 are swapped (vanilla: list order 6, hat 7) | 1.21.4..1.21.11 |
  | `login`/`respawn` `previousGamemode` is `u8`; vanilla writes a signed byte (-1 for none) | 1.20.6..1.21.11 |
  | `ChatTypeParameterType` is rotated (vanilla: sender 0, target 1, content 2) | 1.20.6..1.21.11 |

  Only the first three change how many bytes are read. A VarLong labelled `varint` reads the same
  bytes until a value needs more than five (the 1.17 to 1.18.2 descriptions have these fields right).
  The structure block flags, the two `player_info` bits (for a list order below 128) and the gamemode
  byte are one byte either way, and a mapper is its raw integer in the generated schema, so those
  rows mislabel or swap values without changing the layout. Mapper names survive only as comments,
  and some are mislabelled in the data (entity metadata type 13 is `optional_uuid`, vanilla
  `OPTIONAL_LIVING_ENTITY_REFERENCE`); the wire is unaffected.
- **Registry-keyed values are opaque integers**: block states, item ids, particle ids carry no names.
- **BPS reads trailing optional groups leniently**: an `if` group at the very end of a packet is
  skipped when no bytes are left. Generated schemas write presence as a `match` statement whenever the
  content can be empty, so a present-but-empty value is not lost.
- ProtoDef's JS value model is lossy for four shapes (u32 `bitflags` with bit 31, non-canonical
  `lpVec3`, an option flag over a void payload, a `registryEntryHolder` whose inline value is falsy),
  which is why the BPS → JS direction reports some packets as `js-lossy`: they must still decode
  fully; only the JS re-serialisation differs. A BPS-side re-encode of the same bytes is exact.
