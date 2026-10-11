"""Load `.bps` files and resolve `import` paths.

An import is a path relative to the file that contains it. Each file is
included once, so a schema passed on the CLI and also imported is not declared
twice.
"""

from __future__ import annotations

from pathlib import Path

from bps.ast import Schema
from bps.errors import ParseError
from bps.parser import parse


def load_schema(paths: list[str | Path]) -> Schema:
    encodings: list = []
    enums: list = []
    packets: list = []
    protocols: list = []
    seen: set[Path] = set()

    def add(path: Path) -> None:
        path = path.resolve()
        if path in seen:
            return
        if not path.is_file():
            raise ParseError(f"schema not found: {path}")
        seen.add(path)
        schema = parse(path.read_text(encoding="utf-8"))
        for imp in schema.imports:
            add(path.parent / imp)
        encodings.extend(schema.encodings)
        enums.extend(schema.enums)
        packets.extend(schema.packets)
        protocols.extend(schema.protocols)

    for raw in paths:
        add(Path(raw))
    return Schema(tuple(encodings), tuple(enums), tuple(packets), tuple(protocols))
