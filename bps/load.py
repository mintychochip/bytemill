"""Load `.bps` files, follow their `import` paths, and check what each may use.

An import is a path relative to the file that contains it, and each file is
loaded once. A file sees its own declarations and those of the files it imports
*directly*; imports are not transitive (the `.proto` rule). So every name a file
uses is either declared in it or traceable to an `import` line in it. The
compiler's five primitives (`bps/primitives.py`) are the only names that need no
import: `varint` is an ordinary encoding in `schemas/std/varint.bps`.
"""

from __future__ import annotations

import os
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path

from bps.ast import Schema
from bps.errors import BpsError, ParseError, ResolveError
from bps.parser import parse
from bps.primitives import BARE_PRIMITIVES
from bps.refs import Ref, references


@dataclass(frozen=True)
class _Module:
    """One loaded file: what it declares and the files it imports directly."""

    path: Path
    schema: Schema
    imports: tuple[Path, ...]


def load_schema(paths: list[str | Path]) -> Schema:
    modules = _load_modules(paths)
    providers = _Providers(modules.values())
    for module in modules.values():
        _check_imports(module, modules, providers)
    return _merge(modules.values())


def _load_modules(paths: list[str | Path]) -> dict[Path, _Module]:
    """Every file reachable from `paths`, each imported file before its importer."""
    modules: dict[Path, _Module] = {}
    loading: set[Path] = set()

    def add(path: Path, importer: Path | None) -> None:
        path = path.resolve()
        if path in modules or path in loading:
            return
        if not path.is_file():
            by = f" (imported by {_show(importer)})" if importer is not None else ""
            raise ParseError(f"schema not found: {_show(path)}{by}")
        loading.add(path)
        schema = parse(path.read_text(encoding="utf-8"))
        imports = tuple((path.parent / imp).resolve() for imp in schema.imports)
        for imported in imports:
            add(imported, path)
        modules[path] = _Module(path, schema, imports)

    for raw in paths:
        add(Path(raw), None)
    return modules


class _Providers:
    """Which files declare each name."""

    def __init__(self, modules: Iterable[_Module]) -> None:
        self._types: dict[str, list[Path]] = {}  # encodings and enums
        self._packets: dict[str, list[Path]] = {}
        for module in modules:
            for enc in module.schema.encodings:
                self._types.setdefault(enc.name, []).append(module.path)
            for enum in module.schema.enums:
                self._types.setdefault(enum.name, []).append(module.path)
            for pkt in module.schema.packets:
                self._packets.setdefault(pkt.name, []).append(module.path)

    def of(self, ref: Ref) -> list[Path]:
        table = self._packets if ref.kind == "packet" else self._types
        return table.get(ref.name, [])


def _check_imports(
    module: _Module, modules: dict[Path, _Module], providers: _Providers
) -> None:
    """Fail when `module` uses a name that neither it nor a direct import declares."""
    visible_from = {module.path, *module.imports}
    missing: dict[Path, dict[str, None]] = {}  # file to import -> names it provides
    nowhere: dict[str, Ref] = {}  # no loaded file declares it: name -> first use
    for ref in references(module.schema):
        if ref.kind != "packet" and ref.name in BARE_PRIMITIVES:
            continue
        declared_in = providers.of(ref)
        if visible_from.intersection(declared_in):
            continue
        if not declared_in:
            nowhere.setdefault(ref.name, ref)
        for provider in declared_in:
            missing.setdefault(provider, {})[ref.name] = None
    if nowhere:
        nearby = _nearby_providers(module, modules)
        for name, ref in list(nowhere.items()):
            if found := nearby.of(ref):
                for provider in found:
                    missing.setdefault(provider, {})[name] = None
                del nowhere[name]
    if not missing and not nowhere:
        return

    count = len({name for names in missing.values() for name in names}) + len(nowhere)
    lines = [
        f"{_show(module.path)}: {count} name{'' if count == 1 else 's'} "
        "not declared here or in an imported file"
    ]
    for provider, names in missing.items():
        target = Path(os.path.relpath(provider, module.path.parent)).as_posix()
        lines.append(f'  add  import "{target}"  for {", ".join(names)}')
    for name, ref in nowhere.items():
        lines.append(
            f"  {name!r} (used in {ref.where}) is declared in no loaded or nearby .bps file"
        )
    raise ResolveError("\n".join(lines))


def _nearby_providers(module: _Module, loaded: dict[Path, _Module]) -> _Providers:
    """Providers among the unloaded `.bps` files beside `module` and one folder down.

    This only words an error: a name a nearby file declares gets the `import`
    line to add, not a dead end. A file that cannot be read is skipped so it
    cannot hide the error being reported.
    """
    found: list[_Module] = []
    for pattern in ("*.bps", "*/*.bps"):
        for path in sorted(module.path.parent.glob(pattern)):
            path = path.resolve()
            if path in loaded:
                continue
            try:
                schema = parse(path.read_text(encoding="utf-8"))
            except (OSError, UnicodeError, BpsError):
                continue
            found.append(_Module(path, schema, ()))
    return _Providers(found)


def _merge(modules: Iterable[_Module]) -> Schema:
    encodings: list = []
    enums: list = []
    packets: list = []
    protocols: list = []
    for module in modules:
        encodings.extend(module.schema.encodings)
        enums.extend(module.schema.enums)
        packets.extend(module.schema.packets)
        protocols.extend(module.schema.protocols)
    return Schema(tuple(encodings), tuple(enums), tuple(packets), tuple(protocols))


def _show(path: Path) -> str:
    try:
        return path.relative_to(Path.cwd()).as_posix()
    except ValueError:
        return path.as_posix()
