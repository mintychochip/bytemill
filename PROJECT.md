# Bytemill — Project Status and Remaining Goals

**Status date:** 2026-10-19
**Scope:** the Rust compiler (`crates/bytemill`, binary `bml`), the shipped schemas, the nine
typed code generators, and the rstom/Minestom downstream.

This file is the project's work queue. A goal is *remaining* until its "done when" has
current-tree evidence (code, test, workflow, or committed report). Design rationale lives in
`docs/superpowers/specs/`; this file only records state and next work.

---

## What ships today

| Area | State |
|---|---|
| Schema language | `.bps` parser, resolver, analyzer, interpreter (`ir::exec`), typed program (`ir::typed`) |
| Typed printers | Rust (typed), Go, Python, Java, C#, C, C++, TypeScript, Zig — all lower `ir::typed::Program` |
| Language versions | C++98–26, Go 1.16–1.24, Python 3.8–3.14, C89–C23, Rust 2015–2024, TS ES2015–ESNext, Zig 0.11–0.14, Java 8–21, C# 8–12 via `--std`/`<target>:<version>` |
| Protocol coverage | 39 version roots / 51 releases (1.13 → 26.3), hand-written 26.3, Hytale connection packets |
| Binaries | `.github/workflows/release.yml` publishes `bml` for Linux (x86_64 glibc/musl, aarch64), macOS (arm64/x86_64), Windows x86_64, with `SHA256SUMS` |
| Generated inputs | `data/minecraft-data/pc/` is fetched (`scripts/fetch_minecraft_data.py`); `targets/rstom/shapes/` is extracted (`scripts/extract_rstom_shapes.sh`); both are git-ignored, as are `build/` and `target/` |
| Gates run locally | full `cargo test` suite green, `clippy -D warnings` clean, per-printer compile/roundtrip tests |

**Not** in CI today: tests, formatters, oracle, protocol roundtrips, or any emitted-language
gate (see A1–A4). CI runs clippy on pushes/PRs and builds release binaries.

---

## Remaining goals

### P0 — Verification that actually runs

| ID | Goal | Evidence (current tree) | Done when |
|---|---|---|---|
| A1 | Run the test suite in CI on every push/PR | `.github/workflows/clippy.yml` is the only test-adjacent workflow and runs only clippy; no `cargo test` anywhere under `.github/workflows/` | A required check runs the suite (`scripts/bytemill_cargo.sh test --no-fail-fast`, or plain cargo with the same memory cap) and a failure blocks merge |
| A2 | Run every printer's language gate in CI | `docs/superpowers/specs/2026-10-15-bps-idiomatic-printers-design.md` §5 lists gofmt/go vet, ruff/mypy, javac+roundtrip, dotnet build, cc/clang-format, g++/clang-format, node/tsc/prettier, zig fmt/test, rustfmt/clippy — no workflow installs or runs them | A matrix job installs the toolchains and runs `test_emit_*` with every gate enabled; once enforced, a missing toolchain fails instead of skipping |
| A3 | Enforce formatting of the compiler itself | No `.rustfmt.toml`, no fmt workflow, no fmt in `.githooks/pre-commit`; the baseline tree is not `cargo fmt --check` clean, and the only fmt gates are for *emitted* code | A pinned rustfmt config + normalized tree (or an explicitly scoped check) runs as a required check |
| A4 | Schedule the ProtoDef oracle | `scripts/verify_versions.py` is manual, needs `npm ci` in `tools/protodef-oracle` and `write_generated_vectors --ignored` | A scheduled (and manually dispatchable) job runs both directions, refreshes pinned vectors only via reviewed PRs, and reports drift |
| A5 | Publish diagnostic results instead of hiding them | Four `#[ignore]` diagnostics: `test_versions.rs:208` (round-trip failures), `:223` (generator coverage), `:246` (vector writer), `test_versions_cross.rs:77` (cross disagreements) | Each release attaches or commits the diagnostic output, or the test becomes an assertion with a bound |
| A6 | Broaden the release smoke test | `.github/workflows/release.yml` smoke-tests only `bml check schemas/std/varint.bps` | Release builds also `check` the 26.3 wiki root and Hytale root, and run `wire-check` on a committed vector file |

### P1 — Emitter and runtime completeness

| ID | Goal | Evidence (current tree) | Done when |
|---|---|---|---|
| B1 | Support 128-bit integers in every printer | Go `emit_go.rs:547`, Java `emit_java.rs:690`, C# `emit_csharp.rs:506,2242`, C `emit_c.rs:153,167,180,365` reject or panic on `IntTy::I128`; C++/Rust/Zig/TS/Python already print it | A schema with an `i128` field decodes and re-encodes in all nine printers, with tests per language |
| B2 | Encode `repeat until_end` | Decode works, encode fails in the interpreter (`interp/engine.rs:775`), IR (`ir/exec.rs:351`), typed program, and every printer runtime | A list decoded `until_end` re-encodes to the same bytes in the interpreter and all nine printers |
| B3 | Put the remaining emitters on the typed program, or stop claiming they are | `emit rust` (`emit_rust.rs`), `emit rstom` (`emit_rstom.rs`), `emit rstom-tree` (`emit_tree.rs`) take `ResolvedSchema` directly; `README.md` says “Every printer lowers schemas to a shared typed intermediate representation” | Either those three lower `ir::typed::Program`, or README/spec name them as AST-based exceptions with a tracked migration goal |
| B4 | Make version flags honest for non-typed targets | `--std` is parsed for `rust`/`rstom`/`rstom-tree` in `EmitOptions::for_target` but ignored by the CLI arms; Zig stores `ZigVersion` as a no-op (`emit_zig.rs:543-547`) | Unsupported targets reject `--std` with a clear error (or implement it); Zig output differs meaningfully per version or the flag is rejected |
| B5 | Support `emit <target> --check` | `cli.rs:505-509`: `--check` is an error except for `rstom-tree` | Every emit target can report drift (exit 3) without writing; generated-code CI can gate on it |
| B6 | Remove the `rust-typed` panic | `emit_rust_typed.rs:197` panics when a method codec has no named type | The case returns a `BpsError`, with a regression test |

### P2 — Schema and protocol fidelity

| ID | Goal | Evidence (current tree) | Done when |
|---|---|---|---|
| C1 | Extend Hytale coverage | `schemas/hytale/README.md`: covered packets are only `connect`, `disconnect`, `ping`, `pong` (4 packets / 5 routes); no gameplay/channel packets, no zstd-compressed framing, offset-table layouts limited to reading packed order | Additional packet families modeled and tested; compression behavior either modeled or explicitly out of scope for the schema |
| C2 | Decode the intentionally opaque payloads | `data: rest` in `minecraft_26_3_login.bps:35,44,58`, `versions/common/from_1_13.bps:148,153,202,210`, `from_1_20_2.bps:44,49`; light data `rest` in `1_14_to_1_15_2.bps:134` and `1_16_to_1_16_5.bps:94`; metadata kinds 34/41 at `minecraft_core.bps:92,99` | Each site is either decoded by the schema, or carries a dated comment naming the blocker and the tests that would change |
| C3 | Close or bound the hand-written 26.3 looseness | `test_versions_cross.rs` lists 13 packet types where the hand-written schema is deliberately looser (containers, recipes, advancements, explode, attributes); versions README records 171 packets that do not round-trip exactly | The list shrinks to a documented, test-pinned exception set, or the schema models those fields |
| C4 | Audit pre-26.1 schemas against vanilla | `schemas/versions/README.md`: only 26.1–26.3 were compared with decompiled servers; 11 known 26.1 misdescriptions remain in older descriptions; 1.14.2 reuses 1.14.1 data as inference; 26.2/26.3 data are open-PR heads | Older descriptions patched where vanilla disagrees; the open-PR data dependencies are replaced when upstream merges; the inference note is removed or confirmed |
| C5 | Exercise more than status/login against real servers | `game_probe` boots the dedicated server and speaks status + login only (`game_probe.rs:3-5`) | Configuration and at least one play flow are probed, or the README states the limit exactly |
| C6 | Reconcile registry claims | `README.md` says all vanilla registries are fully modeled; `schemas/versions/README.md` says registry-keyed values are opaque integers; `minecraft_26_3_remaining.bps:3-5` stops at opaque byte arrays | One claim is true: either name tables/registries are modeled, or both docs say ids are opaque and the README claim is narrowed |

### P3 — rstom / Minestom downstream

| ID | Goal | Evidence (current tree) | Done when |
|---|---|---|---|
| D1 | Shape the remaining hand-written packets | `targets/rstom/shapes` holds 64 shapes; specs describe 172 remaining (direct 46, enum/cast 29, byte arrays 4, domain 19, record lists 16, combined/other 9, arity 47, no hand file 1) | Waves 2–5 land with per-wave tests, and `test_emit_tree_real_tree` asserts the new eligible count |
| D2 | Add the `frames` module or a scoped fallback | 48 packets render `crate::frames::<walker>(…)`, but rstom has no `frames` module; `tree_packet::guard_frames_walkers` refuses to emit them | Those 48 packets emit and compile in rstom, or the refusal lists a dated, owned fallback |
| D3 | Complete the cutover | `targets/rstom` holds only its generated-ledger README (shapes are git-ignored); no `bindings.json`; class table still external (`tools/packets_gen/schema/packets.json`); `bps_rstom`, `rstom-packets`, and `packets_gen` remain load-bearing | `ported.rs`/`mod.rs`/id tables are generated; bindings and class table live in-tree; the three legacy packages are retired with the workspace green |
| D4 | Make `--check` and the pin gate complete | `--check` covers generated files only, not callers/tests/plugins; pin gate has 260 id-table rows vs 261 routes — `legacy_server_list_ping` (`0xfe`) is omitted (`emit_rstom.rs:1611-1612`) | `--check` reports drift outside generated files too, and every route is either pinned or documented as unroutable |
| D5 | Document the downstream surface | No workspace registry of targets; `README.md` Downstream cites only the waves-0-1 spec; Code Generation table omits `rstom`/`rstom-tree` | `targets/` has a README/registry; README links the waves 2–5 spec and lists both rstom targets with their gates |

### P4 — Docs and tooling hygiene

| ID | Goal | Evidence (current tree) | Done when |
|---|---|---|---|
| E1 | Remove or restore the Python parity reference | `schemas/versions/README.md` cites `scripts/python_parity_versions.py`, which does not exist (the prototype was removed in `779d185`) | The README reference matches the tree, or the script is restored and runnable |
| E2 | Fix stale schema comments and missing root docs | `schemas/minecraft_26_3_play.bps:3` claims `body: rest` fallbacks that no longer exist; `minecraft_26_3_play_serverbound.bps`/`remaining.bps` are unlisted in READMEs; `schemas/hytale/README.md` documents only the root, not `connection.bps`/`core.bps` | Comments match the schema; every root is listed where a reader looks |
| E3 | Keep the nine-language claims exact | README language/registry claims must match C6/B3 outcomes | README statements are covered by a test or a named spec section |
| E4 | Maintain this file | — | Every merged change that closes a goal updates its row in the same PR; new deferrals add a row instead of a comment |

---

## Explicit non-goals (current design)

From `docs/superpowers/specs/2026-10-15-bps-codegen-ir-design.md` §2, unless a product
decision changes it:

- Session state machines, compression, and encryption *inside the nine printers*. Codecs
  operate on bodies; framing/transport belongs to the consumer. (Hytale compressed framing
  is a schema/model question tracked as C1, not a printer feature.)
- Registry name tables in the typed code generators (registry ids stay opaque integers until
  C6 says otherwise).
- Replacing `emit rust`/`emit rstom`/`emit rstom-tree` *as part of* the typed-printer work.
  B3 tracks whether to migrate or exempt them.

## Decision log / open questions

- **i128 (B1):** choose representations per language: C++/Rust/Zig/TS/Python already have
  one; Go needs a pair struct, C a struct, Java a pair or `BigInteger` (allocation), C#
  `System.Int128` on .NET 7+ but the C# 8/9 modes need a pair. Record the choice in the
  printers spec before implementing.
- **`until_end` encode (B2):** define whether encoding a decoded `until_end` list is a
  concatenation by definition (no terminator) or remains an error; update interpreter, IR,
  typed program, and all printers together.
- **rstom cutover ownership (D3):** decide whether this repository owns the class table and
  bindings, or an external workspace does; `--check` and the release process depend on it.
