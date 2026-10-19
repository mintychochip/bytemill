# BPS — shared declarations across protocol versions (sub-project A): design

**Status:** design approved by the user on 2026-10-13; this spec awaits review. Not committed.
**Follows:** `2026-10-12-bps-protocol-versions-design.md` (the 39 generated schemas). §4.1 of that
spec ("each generated file is self-contained") is superseded by §4 below.

## 1. Roadmap

The request ("derive the common objects, make a common file in the DSL; composition of encodings;
data types first with encodings as functional types") is three sub-projects, each with its own spec,
plan and build, in this order:

| | Sub-project | Decision |
| --- | --- | --- |
| **A** | Derive the common objects of the 39 generated schemas into shared files | this spec; no language change |
| **B** | Composition: an encoding can take encodings as parameters (`string(Len = varint)`, `blob(Len)`, `range(T)`) | chosen by the assistant when the user said "pick the best one" for the LenV1/LenV2 sketch. Layering (a bigger encoding keeps a smaller one in a named field, fields decode in declaration order) already works and needs no feature. "V2 = V1 plus changes" is not a language feature: A shares what did not change, C binds a type to the encoding each version uses |
| **C** | One data model for all versions: data types defined once, each protocol version binds them to encodings (functions type ↔ bytes); packets shared across versions | the user's pick for what "data type first" should give; spec after B |

**Invariant for all three:** bytes and decoded JSON stay identical for all 39 versions, the
hand-written `schemas/minecraft_26_3*.bps`, and the emitted rstom Rust (the user's pick: "Bytes and
decoded JSON, everywhere").

## 2. Goal and non-goals of A

**Goal.** Replace the 39 self-contained generated files by thin roots plus shared files, so that each
distinct declaration (an `encoding` or a `packet`) is stored once per set of versions in which it, and
everything it references, is identical.

**Non-goals.**
- Any language, loader, interpreter or emitter change. Python needs no change: it already follows imports.
- Hoisting anonymous inline shapes into named encodings (measured: few repeat across different names;
  the repetition is across versions of the same name).
- The hand-written schemas (`minecraft_core.bps`, `minecraft_26_3*.bps`) stay as they are.
- A generic building-block library (`string(Len)`, arrays, options): needs B.
- Sharing a declaration across a change of something it references (a packet that mentions `slot`
  cannot be shared across a `slot` change): needs B or C. See §3.
- `bml flatten` (printing a version as one file). Declined for now; cheap to add later by concatenating
  the loaded files without their import lines.

## 3. Measurements

From the committed files (a prototype; the importer must reproduce these):

| | |
| --- | --- |
| generated files | 39, 2.32 MB (routes: 0.40 MB of that) |
| declarations | 8,881 (7,271 packets, 1,610 encodings) |
| distinct by text | 726 (559 packets, 167 encodings) |
| distinct once references count (a declaration's identity includes what it references, see §4.3) | **1,032 variants** |
| distinct sets of versions those variants occur in | **165** (11 sets, holding 14 variants, are not one contiguous range) |
| identical in all 39 versions | 51 declarations: 12 encodings (`uuid`, `string`, `tags`, nine NBT bodies) and 39 packets |
| change between neighbouring protocols | 0–38 packets changed, 0–28 new (of 143–261) |
| estimated result | 165 shared files, ≈1.10 MB (47%); each root imports 21–63 files |

Comments are part of a declaration's text (mapper names live in comments); counting them changes none
of the numbers above.

## 4. Design

### 4.1 Layout

```
schemas/versions/
  manifest.json  README.md  patches/        unchanged
  v1_13.bps … v26_3.bps                     roots: header, imports, the protocol block (routes)
  common/<set>.bps                          generated shared declarations
```

**Naming.** Order the manifest entries as the manifest does. A set of entries is split into maximal
runs of consecutive entries. A run becomes a piece (the first matching row wins):

| run | piece |
| --- | --- |
| reaches the last entry | `from_<first>` |
| one entry | `only_<first>` |
| otherwise | `<first>_to_<last>` |

`<id>` is the entry's `release` with `.` replaced by `_` (as in the root's file name). Pieces are joined
with `+`. Examples: `from_1_13.bps` (unchanged since 1.13: the common file), `1_13_to_1_13_2.bps`,
`only_1_21_11.bps`, `1_13_to_1_16_5+1_19_4_to_1_20_4.bps` (longest today: 35 characters). A run that
reaches the newest entry carries no end, so appending 26.4 renames nothing that continues unchanged;
declarations that change in 26.4 leave `from_…` for a closed range.

### 4.2 Declarations

The importer renders each version into the single-file text exactly as today (`render::import`): it
stays the definition of what a version means, still runs through the name-flow lint, and is still what
`bml import-protodef` prints. That text is then split into header, imports, declarations and protocol
block.

A **declaration** is the doc comment lines directly above an `encoding` or `packet` (divider comments
`// ---` excluded) plus the declaration through its closing brace, or its single line when it has none.
The splitter scans for column-0 keywords, counting braces outside comments and string literals. A safety
net runs on every version: each piece must parse (with the crate's own parser) as exactly one declaration
of the expected kind and name, and the pieces' names must be exactly the declarations the whole file
parses to. Otherwise the import fails.

The **references** of a declaration come from parsing it and calling `refs::references`, the function
the loader's import check uses, restricted to names declared in the same version. Names that are not
(the std encodings and the bare primitives) are external: they do not affect identity, and every file
imports both std files, as the current files do.

### 4.3 Identity

Per version, take the graph whose nodes are `(kind, name)` and whose edges are references, and its
strongly connected components (NBT is recursive). Visit components children first. An id is an integer
interned from a key; equal keys ⇔ equal ids, across all versions (exact string keys, no hashing, no
collisions):

- A declaration outside any cycle: key = `(kind, name, text, sorted ids of the declarations it references)`.
- A component with two or more members, or a self reference: key = `(sorted (kind, name, text) of its
  members, sorted ids of the outside declarations they reference)`; each member's key = `(id of the
  component, kind, name)`.

Equal ids mean the declaration and everything reachable from it are textually identical, so the
declaration means the same thing in every version that has that id.

### 4.4 Files

Group ids by the set of versions in which they occur; one file per distinct set.

- **Shared file.** Header `// Shared by: <runs>.` where a run reads, by the same first-match rule as the
  naming table, `<release> (protocol <n>) onwards`, `<release> (protocol <n>) only` or `<release>
  (protocol <n>) to <release> (protocol <n>)`, joined with `, `; then the two `GENERATED` lines used
  today; both std imports (`../../std/…`); one import
  per other shared file that defines something its declarations reference (widest set first, then file
  name); then the declarations, encodings before packets, each group sorted by name, separated by blank
  lines.
- **Root.** The header it has today (title, shared protocol number, `GENERATED`, patch list); both std
  imports (`../std/…`); one import per shared file whose set contains the version (widest set first, then
  name); the protocol block as today.

A declaration's references have ids in sets that contain its own, so the import graph between shared files
is acyclic (a mutual reference forces equal sets, hence one file). A root imports exactly the files that
define its declarations, so no name is declared twice in one load.

### 4.5 Importer and CLI

- `render` exposes the pieces; `import()` is their concatenation (one source of truth for the text).
- New `protodef/share.rs`: `share(&[(VersionEntry, Pieces)]) -> Layout`, a map from relative path to text.
  Pure and deterministic (ordered maps, no hash iteration in output).
- `versions::import_versions(manifest, data, out, check) -> Report` lowers all versions (lint as now),
  shares, then writes or checks `out/v*.bps` and `out/common/*.bps`. `Report` gains `orphans`: `.bps` files
  in `out/common/` that the layout no longer contains. They are removed when writing and listed with
  `--check` (exit 3, as for a stale file). The importer refuses to touch a file in `common/` whose first
  line is not its `GENERATED` marker.
- `--only N` is removed: the layout depends on every version.
- Output of a run: `wrote W, unchanged U, removed R: 39 roots, 165 shared files, 8881 declarations kept
  as 1032`.

### 4.6 Unchanged

The language, loader, interpreters, emitters, the manifest format, the patch mechanism, the hand-written
schemas, Python, and `bml import-protodef` (still one self-contained file).

## 5. Verification

**Invariant.** For every version, the merged schema loaded from the root equals the merged schema
loaded from the single-file rendering, declaration by declaration (AST `PartialEq`; declaration order is
not meaningful), protocol block included.

1. **Baseline, once, in scratch (not committed), before regenerating.** `bml wire-check --dump` on the
   current single-file schemas for all 39 versions, over (a) the 7,271 pinned oracle packets and (b)
   vectors drawn from each schema (`write_generated_vectors`, 12 seeds). After regenerating, the same
   vectors through the roots must give byte-identical dumps.
2. **Permanent tests** (`test_versions.rs`, `share.rs`):
   - `shared_layout_means_what_the_single_file_means`: per version, load the root and the single-file
     rendering (temporary directory beside a copy of `schemas/std`) and compare declaration by declaration.
   - `committed_schemas_are_what_the_importer_writes`, now over the whole tree, including orphans.
   - `every_shared_file_checks_alone`: `load_schema` + `analyze` on each file in `common/`.
   - Unit tests on synthetic versions: identical declarations share a file; a change in a referenced
     encoding splits its dependants; a recursive group shares as a unit; a non-contiguous set gets a `+`
     name; appending a version leaves open-ended names unchanged; orphans are detected; a malformed
     piece is rejected.
   - Unchanged and still required: load + protocol id, route tables, manifest coverage, the round trips,
     the pinned oracle vectors, the 26.3 cross-check.
3. **Gates.** Clippy with `-D warnings`; the full cargo suite; the Python suite; `scripts/python_parity_versions.py`;
   one pass of `scripts/verify_versions.py`.

## 6. Error handling

The importer reports, with the version and declaration name: a piece that does not parse as one declaration
of the expected kind and name; two declarations with the same kind and name in one version; a non-generated
file in `common/`; a name used but declared nowhere (the loader's import check, via `bml check`).
There is no fallback to the single-file layout.

## 7. Risks and limits

- Reading one version means reading 21–63 files. `grep -l '^packet map_chunk' schemas/versions/common/*.bps`
  finds a declaration; the README says so.
- A packet that merely mentions a changed encoding cannot be shared across that change (1,032 variants
  against 726 distinct by text). B and C remove that limit.
- A new release moves declarations between files only for objects that changed.
- Route tables (0.40 MB, unique per version) are the floor.
- The splitter relies on the importer's regular output; the safety net in §4.2 turns a surprise into an
  import failure, not a silent mis-split.

## 8. Success criteria

1. All 39 roots and all shared files pass `bml check`.
2. The equivalence test passes for all 39 versions; the baseline dumps are byte-identical.
3. The generated text is at most 55% of today's 2.32 MB (the estimate is 47%; the size is a consequence,
   the equivalence test is the criterion).
4. Clippy, the full cargo suite, the Python suite, the Python parity run and one oracle pass are green.
5. `schemas/versions/README.md` (layout, how to find a declaration, numbers), the root `README.md`,
   `docs/DSL_STRUCTURE.md` and the previous spec's §4.1 are updated; `--only` appears nowhere.
