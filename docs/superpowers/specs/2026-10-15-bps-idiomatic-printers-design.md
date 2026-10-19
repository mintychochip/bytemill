# BPS code generation: idiomatic printers over a typed program

**Status:** implemented. All seven printers read the typed program; the per-language gates
and the 26.3 and Hytale round trips run on the emitted trees. The per-language idiom review
(steps 3-4 of section 7) landed 2026-10-19; the verification record is section 8.
**Date:** 2026-10-15
**Builds on:** `2026-10-15-bps-codegen-ir-design.md` (the IR, its interpreter, the printers).

## 1. Problem

The six IR printers (`emit go`, `python`, `c`, `cpp`, `typescript`, `zig`) all print the same
thing: a dynamic interpreter in the target language's clothing. Every procedure keeps a
string-keyed environment (`env := map[string]any{}`, `bps_env_set(env, a, "b0", …)`,
`std::map<std::string, Val>`, a Zig `Val` with `keys`/`vals` slices), every read goes through a
coercion helper (`ival`, `bps_i`, `Vi`, `I`), and the public API is either untyped (`any`, `Val`,
dicts) or typed only at the edges and rebuilt from the map by reflection (`fieldsToEnv`,
`derefX`, `as_x(map)`). Names are raw schema keys (`Decode_handshake`, `Game_profile`,
`Meta_entry_value_arm6`), nothing is formatted by the language's own conventions, and C keeps a
global `bps_err`.

The cause is shared: the IR's statements name values by string and carry untyped AST
expressions. Each printer would have to re-derive scopes and types to print typed code, so each
printed the environment instead.

## 2. Decision

Add one shared pass, **IR → typed program**, and rewrite every printer on top of it.

```
ResolvedSchema → lower → IrSchema → typed → Program → printers: go | python | c | cpp | typescript | zig | rust-typed
                               └→ IR interpreter (unchanged definition of meaning)
```

- The typed program resolves every name to a typed local, parameter, or record field, gives
  every local a unique name in its function, makes every conversion explicit, and names every
  type, field, variant, and codec once. Printers only choose syntax.
- Semantics stay those of `ir::exec` (the executable definition). The typed program changes
  representation, not meaning. A printer is wrong when its bytes differ from the interpreter's.
- `emit rust-typed` (planned in the codegen IR spec, never shipped) is added as the seventh
  printer. `emit rust` (JSON decoders, a port of `bps/emit_rust.py`) and the rstom emitters stay
  as they are.

## 3. Lowerer changes (shared, before the typed pass)

1. **Computed-field storage uses the record's field classes.** `collect_fields` passes the
   classes of the fields already collected as `locals` to `storage_of`, and `storage_of` gains a
   last integer rule before `i128`: an interval rule over `&` with a non-negative constant mask,
   `>>` by a constant, `%` by a positive constant, `|` of bounded operands, and literals, using
   the operands' classes. Result: the smallest unsigned class when the interval is non-negative,
   else the smallest signed class, else `i128`. Today `commands_node.node_type = flags & 3` and
   every `flags_*` bit field in the versions are `i128`; after this they are `u8`.
2. **`f32be` is `f32` storage.** New `Storage::F32`. A wire `f32` is a `float32`/`f32` field,
   not a widened `f64`.
3. **No `__any` fields.** When groups or statement-match arms bind the same field name with
   different types, the first binding keeps the name and each later conflicting binding is
   renamed `<name>_<n>` (n = 2, 3, …) in both the record's fields and the procedures. A later
   expression that refers to a renamed field is a lowerer error (it would mean "whichever ran
   last"). Today only the versions schemas hit this (`particle_data_arm5_destination_block.value`
   is a `position` in one group and a `varint` in the other).
4. **Distinct nested types.** A block or `match` field whose name an earlier field of the same
   namespace already uses is renamed `<name>_<n>` before lowering, so two sibling groups that
   each declare their own `value: { … }` get two record types instead of one key overwritten by
   the other (versions 1.13–1.18 `command_node` parser properties).
5. **Arm tags once each.** A multi-tag arm (`1 | 2 =>`) lists each tag once.

The IR interpreter's differential tests (`test_ir_diff.rs`) and the storage tests
(`test_ir_width.rs`) stay green; the width tests gain the interval rule and `f32`.

## 4. The typed program (`crates/bytemill/src/ir/typed/`)

`bytemill::ir::typed(&ResolvedSchema) -> BpsResult<Program>` lowers and types. The authoritative
description of every construct, its meaning, and its failures is the module documentation of
`crates/bytemill/src/ir/typed/mod.rs`; `Program::dump_codec` renders a codec as pseudo-code for
inspection. In short:

- **Names.** `Name` holds lowercase words (`snake`, `camel`, `pascal`, `screaming`); printers
  add casing rules and the one keyword escape (`escape`: trailing `_`). Types and codecs are
  unique under every casing across the program; fields per record, variants per sum, members
  per enum, locals per function body (no local shadows another anywhere in a body). A packet
  is its name (`tags_packet` when an encoding is also `tags`), a frame `default` is
  `default_frame`, a nested block or `match` field is `<parent>_<field>`, a block arm is
  `<sum>_<variant>`; variants are `tag<n>`, `tag_neg<n>`, and `other` for `_`.
- **Types.** `Ty` is `Int(IntTy)`, `Bool`, `F32`, `F64`, `Str`, `Bytes`, `List`, `Named`
  (record, sum, or enum; aliases are resolved). Optionality belongs to fields, locals, and
  parameters. `TypeKind` is `Record`, `Sum` (variant payload `None` for an empty arm),
  `Enum` (open), `Alias`. `Origin` says where a type or codec comes from, for doc comments.
- **Codecs, lifted.** Every encoding, packet, frame, and enum has a codec, and so does every
  nested record or sum: a block field, a `match` field, a block arm, a list element. A nested
  codec takes the parent values its schema text reads as `ParamRole::Capture` parameters (the
  `nbt_list_body` items read `elem_type`; `player_info_update` players read `actions`), and a
  sum codec takes the value its arm is chosen by as its `ParamRole::Tag` parameter. So one
  function decodes one record, and nothing nests deeper than its `if`s, `match`es and loops.
  A codec of an encoding that reads names only its spreaders bind (`nbt_payload`) is a
  fragment: no codec, and its private types are dropped.
- **Bodies.** `Body { params, value, locals, stmts }`. Decode builds a record in locals and
  ends with `Return(Expr::Construct)`; a sum codec returns an `Expr::Variant` from each arm.
  Encode reads `Expr::Field`s of its `value`; it binds a local only for a computed or
  `= expr` value, an unwrapped optional, a sum payload (`ExpectVariant`), or a loop item.
  Statements: `Declare`, `Let`, `Read`, `Decode`, `Utf8`, `Write`, `Encode`, `Require`,
  `Unwrap`, `If`, `Match`, `Repeat`, `ForEach`, `ExpectVariant`, `Return`, `Fail`; a `Bind`
  says whether a store declares its local and whether it stores into an optional.
  `Local::uses` counts reads (dead `let`s are already removed).
- **Integers.** Every `Expr::Binary` carries its result type and `wraps`: an operation stays
  in its operands' type when the exact result always fits (`b0 & 0x80` is a `u8`), else both
  sides are cast to `i64` and `wraps` says whether wrapping arithmetic is needed (`varlong`'s
  `(b9 & 1) << 63`). Literals adopt a sibling's type when they fit. Stores into narrower types
  are explicit truncating casts; integer conditions are explicit `!= 0`; divisors are non-zero
  literals and shift amounts literals in `0..64`. `Program::int_range` gives any integer
  expression's value range (for `>>>` versus `>>` in TypeScript, plain versus wrapping
  operators in C, Rust and Zig).
- **Optional fields** decode into an optional local (`Bind { some: true }`); a read inside
  the block that bound it uses the non-optional value, elsewhere an `Unwrap` (missing field)
  comes first. Encode unwraps an optional field before writing it.
- **Fallback groups** keep the interpreter's rules: decode runs the group only when
  `cond && Remaining > 0`; a guard over a possibly-absent optional is built from `Present`/`Get`.
- **Routes**: `RouteDecl { state, direction, id, packet, frame }`, for documentation; frame
  dispatch is not part of this change.

## 5. Printers

Every printer is a rewrite on `Program`. Shared requirements:

- No runtime environment, no dynamic value type, no reflection, no global mutable state.
- The language's naming conventions; keyword escaping in that language's own form. Five
  languages take a trailing `_`; Zig uses its quoted escape (`@"test"`) for reserved words,
  a trailing `_` only where quoting is impossible (a primitive name, which `zig fmt` would
  unquote in field position) or the name would shadow a file-scope import alias.
- Typed records, sums as the language's tagged union, enums as named integers with constants
  (open: decode keeps unknown values), optionals and lists in the language's native form.
- Errors in the language's idiom, carrying the failing codec and, with a cursor, the offset.
- Output formatted as the language's formatter would leave it (gofmt, rustfmt, zig fmt) or a
  consistent documented style where there is no canonical formatter.
- Doc comments on public types and functions, derived from `Origin`.
- One file per category: `Program::output_stem` groups declarations by the routes they
  belong to (`{state}_{direction}`, `shared`, `encoding`), and the package's entry point
  re-exports every module, so the public surface does not depend on the split.
- Deterministic: the same schema emits the same bytes.

Per language (the details live in each printer's module docs):

| | Go | Python | Java | C# | C | C++ | TypeScript | Zig | Rust |
|---|---|---|---|---|---|---|---|---|---|
| versions | 1.16 → 1.24 (default 1.23) | 3.8 → 3.14 (default 3.11) | 8, 11, 17, 21 (default 21) | 8, 9, 10, 11, 12 (default 12) | C89, C90, C99, C11, C17, C23 (default C11) | C++98, 03, 11, 14, 17, 20, 23, 26 (default C++20) | ES2015 → ESNext (default ES2022) | 0.11 → 0.14 (default 0.14) | 2015, 2018, 2021, 2024 (default 2024) |
| output | `go.mod`, `runtime.go`, one `{stem}.go` | `bpsgen/` package: `runtime.py`, one `{stem}.py`, `__init__.py` | `bpsgen/` package: `Reader.java`, `Writer.java`, `BpsError.java`, one `{Stem}.java` | `BpsGen/` package: `Runtime.cs`, one `{stem}.cs` | `bpsgen.h`, `runtime.c`, one `{stem}.c` | `bpsgen.hpp`, one `{stem}.cpp` | `bpsgen.ts` index, `runtime.ts`, one `{stem}.ts` | `bpsgen.zig` index, `runtime.zig`, one `{stem}.zig` | crate, `src/packets/{stem}.rs` |
| record | struct | `@dataclass(slots=True)` | `record` (17+) | `record struct` (10+), reference `record`/class in value cycles | `typedef struct` | `struct` | `interface` | `struct` | `struct` |
| sum | sealed interface + variant structs | variant dataclasses + union alias | sealed interface + variant records (17+); interface + final classes (8/11) | interface + variant records | enum tag + `union` | `std::variant` (polyfilled <17) | discriminated union | `union(enum)` | `enum` |
| optional | pointer | `T \| None` (or `Optional[T]` <3.10) | `Optional<T>` | `T?` (value types); `null` for reference types | `bool has_x` + value | `std::optional` (polyfilled <17) | `x?: T` | `?T` | `Option` |
| list | slice | `list` (or `List[T]` <3.10) | `List<T>` | `List<T>` | pointer + length | `std::vector` | array | slice | `Vec` |
| error | `error` values | exception classes | `BpsError` exception | `BpsError : Exception` | status enum return | exceptions | `Error` subclass | error set | `Result` + error enum |
| gate | `gofmt -l`, `go vet`, `go test` | `py_compile`; ruff format/check, mypy `--strict` | `java`/`javac` compile + roundtrip | `dotnet build` (net8.0, LangVersion 8 and 12) | `cc -std=c11 -Wall -Wextra -Wpedantic -Werror`; `clang-format --dry-run --Werror` | `g++ -std=c++20 -Wall -Wextra -Wpedantic -Werror`; `clang-format --dry-run --Werror` | `node`; `tsc --strict --erasableSyntaxOnly`; prettier | `zig fmt --check`, `zig test` | `rustfmt --check`, clippy `-D warnings` on the emitted crate |

Java and C# were added 2026-10-19. Both lower to the same typed program; the non-obvious
decisions:

- **Java** wraps each output stem in a `public final class {Stem}` and nests its types there
  (Java requires the public top-level class to match the file name). Java 17+ uses `record`
  and `sealed interface ... permits`; Java 8/11 emits final classes with a constructor,
  getters, `equals`/`hashCode`/`toString`, and a plain interface for sums. Signed division
  and modulo use `Math.floorDiv`/`Math.floorMod`; every numeric conversion is an explicit
  cast (Java re-associates a widened operand at the narrow type, e.g. `int << 56`).
- **C#** emits `readonly record struct` on C# 10+ and `struct` on C# 8/9, except records that
  can reach themselves through value-layout edges (direct or `T?` record fields): those are
  reference types (`record`/`sealed class`), because `Nullable<T>` over a struct would be an
  infinite layout (CS0523). `?` is only spelled for value types; reference optionals are
  `null`, and every file carries `#nullable disable`. Shift counts are cast to `int` (C#
  requires `int`, the IR counts are `i64`), and record `Decode` calls are
  `global::BpsGen.{Type}.Decode` so a property sharing the type name cannot shadow it.

## 6. Tests

1. `typed` builds for `minecraft_26_3_wiki.bps`, `hytale/2026_02_17.bps`, and every
   `schemas/versions/v*.bps`; unit tests pin the naming, uniqueness, fragment, optional,
   fallback, sum, and string rules on small schemas.
2. Each printer round-trips every 26.3 packet the interpreter can synthesize (decode the
   interpreter's bytes, encode, compare), plus its small unit schema. Go also round-trips
   `v1_20_1` and the Hytale root.
3. Each printer's gate (section 5) runs in its test when the tool is installed; network-only
   linters (ruff, mypy, tsc) are run during verification and recorded, not in the suite.
4. The whole suite and the clippy gate stay green.

## 7. Order

1. Lowerer changes (section 3), the typed program, and the Go printer, together: Go is the
   first consumer and its round trip is the program's semantic gate.
2. The other six printers in parallel on the stable program.
3. Per-language review of the emitted `build/26_3` trees; fixes. **Done 2026-10-19.**
4. Regenerate `build/26_3`, README and spec updates. **Done 2026-10-19.**

## 8. Per-language idiom review (2026-10-19)

Baseline: every tree passed `cc`/`g++`/`node`/`cargo test` and gofmt/ruff/mypy/tsc/prettier/
zig fmt/rustfmt/clippy **except** `clang-format` on the emitted C. The review fixed, per
printer (each change is pinned by an assertion or a runtime check in that printer's test):

- **Go**: `// Package bpsgen` package doc; doc comments on every exported declaration (a
  `go/ast` doc linter over the tree reports none missing); counted decode loops preallocate
  (`make([]T, 0, int(count))`); plural initialisms print `IDs`/`UUIDs`/`EntityIDs`, not
  `Ids`/`Uuids`.
- **Python**: an unknown open-enum tag produced a pseudo-member without `_value_`, so
  `repr(Intent(7))` raised `AttributeError`; the member now carries `_value_` and is cached in
  `_value2member_map_`. `BpsError` gained `offset` as data (it was only in the message), and
  `__all__` now includes the alias unions in every module and the package.
- **C**: the emitted tree was not `clang-format`-canonical; the printer formats in place (as
  the C++ printer does) and the test runs the `--dry-run --Werror` gate. A schema field named
  after a codec's fixed local (`s`, `r`, `a`, `out`, `w`, `value`) used to make `cc` fail with
  "conflicting types"; the body now reserves those names and renames the field's local.
- **C++**: `[[nodiscard]]` on every decoder, a shared `CodecError` base behind
  `DecodeError`/`EncodeError`, `constexpr`/`noexcept` integer helpers, and borrowed-view
  parameters (`std::span<const std::uint8_t>`, `std::string_view`).
- **TypeScript**: record and union-arm fields are `readonly` (the codec never writes one); the
  catch-all arm's payload is required, which removed the emitted `value!` non-null assertion;
  the import scanner no longer mistakes an open enum's `const` member keys for type
  references (two phantom `import type`s and their `noUnusedLocals` errors are gone).
- **Zig**: failures carry the failing codec (`Reader`/`Writer` `err_codec`/`err_detail`, set by
  the innermost codec's `errdefer`, with `pos` the offset); `Writer` has `deinit`; a module
  imports a dependency once at file scope (`const encoding = @import("encoding.zig");`) instead
  of repeating `@import(...)` at more than 2 000 call sites, with every program stem reserved
  for locals
  so an alias can never be shadowed; reserved words use `@"test"`.
- **Rust**: 126 `.clone()` calls on encode paths (about 175 including sum payloads) were
  avoidable — optional fields bind through `.as_ref()` and sum payloads use the match's own
  reference, chosen by a use-scan that keeps a clone only where the body moves the value. The
  26.3 tree now emits **zero** clones; decode already had none. `encode` remains borrow-only,
  decoders return `Result`, and the crate stays rustfmt/clippy clean.

Verification record (all run on the final tree):

| Check | Result |
|---|---|
| `scripts/bytemill_cargo.sh test --no-fail-fast` | 365 passed, 0 failed, exit 0 |
| `scripts/bytemill_cargo.sh clippy --workspace --all-targets -- -D warnings` | clean |
| Go: `gofmt -l`, `go vet`, `go test`, round trips (26.3, 1.20.1, Hytale) | clean |
| Python: `compileall`, ruff format/check, `mypy --strict` (13 files), round trips | clean |
| C: `cc -Werror`, `clang-format --dry-run --Werror`, round trips | clean |
| C++: `g++ -Werror`, `clang-format --dry-run --Werror`, round trips | clean |
| TypeScript: `tsc --strict --erasableSyntaxOnly`, prettier, node round trips | clean |
| Zig: `zig fmt --check`, `zig test` (26.3 + Hytale) | clean |
| Rust: `cargo fmt --check`, `cargo clippy -D warnings`, round trips | clean |
| Java: `javac` (JDK 8) on the Java 8 26.3 tree; `java` round trips (26.3, Hytale, probe) | clean |
| C#: `dotnet build` (net8.0, LangVersion 12 and 8); 26.3 runtime round trip | clean |
| Two emits of the same schema are byte-identical, all nine | clean |
| Unknown open-enum tag (`07`) decodes and re-encodes, seven original printers | clean |
| Gated optional absent on the value encodes as no bytes, seven original printers | clean |
