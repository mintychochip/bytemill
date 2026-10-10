from __future__ import annotations

import json
from typing import Any

from bps import ast


def schema_to_json(schema: ast.Schema) -> str:
    return json.dumps(_schema_dict(schema), indent=2, sort_keys=True)


def _schema_dict(schema: ast.Schema) -> dict[str, Any]:
    return {
        "encodings": [e.name for e in schema.encodings],
        "enums": [e.name for e in schema.enums],
        "packets": [p.name for p in schema.packets],
        "protocols": [
            {
                "name": p.name,
                "version": p.version,
                "id": p.protocol_id,
                "routes": len(p.routes),
            }
            for p in schema.protocols
        ],
    }
