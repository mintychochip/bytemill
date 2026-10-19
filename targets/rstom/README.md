# targets/rstom

Generated rstom outputs live here. They are **not** tracked by git:
recreate them on demand instead of committing them.

## shapes/

The shape ledger (`shapes/**/*.json`, 64 files) is extracted from an
rstom worktree with the `bml` CLI:

```sh
cargo build --bin bml
scripts/extract_rstom_shapes.sh --rstom <path-to-rstom-worktree>
```

`--packets-json` (or `BPS_CLASS_TABLE`) and `--schema` (default
`schemas/minecraft_26_3_wiki.bps`) are optional; see
`scripts/extract_rstom_shapes.sh --help`.

Tests that need the ledger skip until it exists:

```text
skipped: targets/rstom/shapes not generated; run scripts/extract_rstom_shapes.sh
```
