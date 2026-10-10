# BPS Language Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the BPS compiler front-end, reference interpreter, CLI, golden tests, and Minecraft 26.3 schema skeleton validated against wiki byte fixtures.

**Architecture:** Python 3.11+ stdlib-only pipeline: lexer → parser → analyzer → `{interp, schema.json, rust emit}`. Semantic truth lives in the interpreter; Rust emitter follows in plan 2.

**Tech Stack:** Python 3.11+, unittest, no third-party deps; optional `rustc` for differential tests in plan 2.

**Spec:** `docs/superpowers/specs/2026-10-09-bps-dsl-design.md`

## Global Constraints

- Python 3.11+, stdlib only for compiler/runtime.
- Naming: language **BPS**, files `*.bps`, CLI **`bpsc`**.
- Minecraft reference: **26.3**, protocol **777**.
- Rust codegen: dependency-free crate (emitter in plan 2).
- Full protocol-777 packet surface in `schemas/minecraft_26_3.bps` (routes + encodings; fields transcribed per wiki).

---

### Task 1: Project skeleton and errors

**Files:**
- Create: `bps/__init__.py`, `bps/errors.py`, `bpsc.py`, `pyproject.toml`
- Test: `tests/test_errors.py`

**Interfaces:**
- Produces: `BpsError`, `ParseError`, `ResolveError`, `DecodeError`, `EncodeError` with `message`, `offset` optional.

- [ ] **Step 1:** Add `pyproject.toml` with `[project.scripts] bpsc = "bps.cli:main"`.
- [ ] **Step 2:** Implement error hierarchy in `bps/errors.py`.
- [ ] **Step 3:** `python -m unittest tests.test_errors -v` → PASS.
- [ ] **Step 4:** Commit.

---

### Task 2: AST and lexer

**Files:** `bps/ast.py`, `bps/lexer.py`, `tests/test_lexer.py`

**Interfaces:**
- Produces: `tokenize(source) -> list[Token]`, frozen AST nodes `Schema`, `EncodingDecl`, `Field`, `PacketDecl`, `ProtocolDecl`, `Route`, etc.

- [ ] Tokenizer covers identifiers, ints, hex, strings, `//` and `/* */` comments.
- [ ] Golden token stream test for a tiny `.bps` snippet.
- [ ] Commit.

---

### Task 3: Parser

**Files:** `bps/parser.py`, `tests/test_parser.py`

**Interfaces:**
- Produces: `parse(source: str) -> Schema`

- [ ] Parse encoding/packet/enum/protocol declarations from spec §4.
- [ ] Parse field modifiers: `if`, `[] count/until/until_end`, inline `match`.
- [ ] Commit.

---

### Task 4: Analyzer

**Files:** `bps/analyze.py`, `tests/test_analyze.py`

**Interfaces:**
- Produces: `analyze(schema: Schema) -> ResolvedSchema` with symbol tables and route→packet binding.

- [ ] Resolve encoding params, detect unknown types, bind routes.
- [ ] Commit.

---

### Task 5: Interpreter primitives and cursor

**Files:** `bps/interp/cursor.py`, `bps/interp/codecs.py`, `bps/interp/evalexpr.py`, `bps/interp/engine.py`, `tests/test_interp.py`, `tests/golden/varint.json`

**Interfaces:**
- Produces: `decode_packet(resolved, packet_name, data: bytes) -> dict`, `encode_packet(...) -> bytes`

- [ ] VarInt/VarLong LEB128 signed per wiki samples.
- [ ] `string(n)`, `u16be`, `i64be`, `uuid`, `position` computed fields.
- [ ] `until 0xFF` loops for metadata subset.
- [ ] Golden vectors: varint 25565, position example, handshake frame.
- [ ] Commit.

---

### Task 6: JSON emitter and CLI

**Files:** `bps/emit_json.py`, `bps/cli.py`, `tests/test_cli.py`

**Interfaces:**
- Produces: `bpsc check|dump-json|routes|decode|encode`

- [ ] CLI smoke tests with hex fixtures.
- [ ] Commit.

---

### Task 7: Minecraft schemas

**Files:** `schemas/minecraft_core.bps`, `schemas/minecraft_26_3.bps`, `tests/test_minecraft_golden.py`

- [ ] `minecraft_core.bps`: varint, string, position, entity_metadata, nbt stub, slot stub.
- [ ] `minecraft_26_3.bps`: protocol 777 routes for handshaking/status/login/play/configuration (full route table).
- [ ] Golden tests: handshake, login_start, keep_alive, status_response.
- [ ] Commit.

---

### Task 8: Rust emitter (plan 2 entry)

**Files:** `bps/emit_rust.py`, `tests/test_emit_rust.py`

- [ ] Emit minimal crate for `schemas/examples/handshake.bps`; differential test vs Python (requires `rustc`).

---

## Spec coverage self-review

| Spec § | Task |
| --- | --- |
| Language §4 | 2–4 |
| Interpreter §4.5 | 5 |
| CLI §8 | 6 |
| Minecraft scope §6 | 7 |
| Rust emitter §7 | 8 |
| Verification §9 | 5, 7 |
