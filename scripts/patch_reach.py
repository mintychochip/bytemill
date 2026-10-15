#!/usr/bin/env python3
"""Which older descriptions carry the shapes that the vanilla 26.1 audit found wrong.

Versions before 26.1 have no vanilla source in this repository, so their descriptions are used as
published. The audit of 26.1 (`schemas/versions/patches/26.1.json`) found errors in descriptions that
vanilla has not changed for many versions, and the same errors can sit in the older files. For each
finding this lists the releases whose description has the shape that 26.1 vanilla contradicts:
whole-type equality with the unpatched 26.1 type where the type did not change over time, a probe of
the one field otherwise.

    scripts/patch_reach.py            # markdown on stdout
"""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "data/minecraft-data"


def types_of(doc, scope):
    if scope == "global":
        return doc.get("types", {})
    state, direction = scope.split(".")
    return doc.get(state, {}).get(direction, {}).get("types", {})


def field(node, name):
    """The type of the first field called `name` inside a protodef type tree."""
    if isinstance(node, dict):
        if node.get("name") == name and "type" in node:
            return node["type"]
        children = node.values()
    elif isinstance(node, list):
        children = node
    else:
        return None
    for child in children:
        found = field(child, name)
        if found is not None:
            return found
    return None


def flags_of(node):
    """The flag names of the first `bitflags` in a type tree."""
    if isinstance(node, list):
        if len(node) == 2 and node[0] == "bitflags":
            return node[1]["flags"]
        for child in node:
            found = flags_of(child)
            if found is not None:
                return found
    elif isinstance(node, dict):
        for child in node.values():
            found = flags_of(child)
            if found is not None:
                return found
    return None


def is_mapper(t):
    return isinstance(t, list) and t[0] == "mapper"


# (finding, packet type, scope, predicate on the type tree that is true for the wrong shape)
PROBES = [
    ("`move_minecart` steps use floats (`vec3f`, `f32` rotations); vanilla writes Vec3 doubles and rotation bytes",
     "packet_move_minecart", "play.toClient", lambda t: field(t, "position") == "vec3f"),
    ("`initialize_world_border.speed` is a `varint`; vanilla writes a VarLong",
     "packet_initialize_world_border", "play.toClient", lambda t: field(t, "speed") == "varint"),
    ("`world_border_lerp_size.speed` is a `varint`; vanilla writes a VarLong",
     "packet_world_border_lerp_size", "play.toClient", lambda t: field(t, "speed") == "varint"),
    ("`multi_block_change.records` are `varint`s; vanilla writes VarLongs",
     "packet_multi_block_change", "play.toClient",
     lambda t: field(t, "records") == ["array", {"countType": "varint", "type": "varint"}]),
    ("`update_structure_block.seed` is a `varint`; vanilla writes a VarLong",
     "packet_update_structure_block", "play.toServer", lambda t: field(t, "seed") == "varint"),
    ("`update_structure_block.flags` is an enum mapper; vanilla writes a bitmask byte",
     "packet_update_structure_block", "play.toServer", lambda t: is_mapper(field(t, "flags"))),
    ("`player_info` flag bits 6 and 7 are swapped (vanilla: list order 6, hat 7)",
     "packet_player_info", "play.toClient",
     lambda t: (flags_of(t) or [])[6:7] == ["update_hat"]),
    ("`login`/`respawn` `previousGamemode` is `u8`; vanilla writes a signed byte (-1 for none)",
     "SpawnInfo", "play.toClient", lambda t: field(t, "previousGamemode") == "u8"),
]
# Types that did not change over time are compared whole.
WHOLE = [
    ("`custom_click_action` has a presence flag; vanilla length-prefixes the payload",
     "packet_common_custom_click_action", "global"),
    ("`ChatTypeParameterType` is rotated (vanilla: sender 0, target 1, content 2)",
     "ChatTypeParameterType", "play.toClient"),
    ("`ItemBlockProperty` ranges are mandatory strings; vanilla has optional strings",
     "ItemBlockProperty", "global"),
]


def runs(releases, flags):
    """Contiguous runs of releases with the same flag, as `a..b` strings."""
    out, start = [], None
    for i, f in enumerate(flags):
        if f and start is None:
            start = i
        if start is not None and (not f or i == len(flags) - 1):
            end = i if f else i - 1
            out.append(releases[start] if start == end else f"{releases[start]}..{releases[end]}")
            start = None
    return ", ".join(out) or "none"


def main():
    manifest = json.loads((ROOT / "schemas/versions/manifest.json").read_text())["versions"]
    older = [e for e in manifest if e["protocol"] < 775]
    releases = [e["release"] for e in older]
    docs = {e["release"]: json.loads((DATA / e["data"] / "protocol.json").read_text()) for e in older}
    raw = json.loads((DATA / "pc/26.1/protocol.json").read_text())

    print("| Wrong in the 26.1 description (fixed there by a patch) | Same in the descriptions of |")
    print("| --- | --- |")
    for finding, name, scope, wrong in PROBES:
        flags = []
        for r in releases:
            t = types_of(docs[r], scope).get(name)
            flags.append(t is not None and bool(wrong(t)))
        assert wrong(types_of(raw, scope)[name]), f"{name}: 26.1 no longer has the wrong shape"
        print(f"| {finding} | {runs(releases, flags)} |")
    for finding, name, scope in WHOLE:
        bad = types_of(raw, scope)[name]
        flags = [types_of(docs[r], scope).get(name) == bad for r in releases]
        print(f"| {finding} | {runs(releases, flags)} |")
    return 0


if __name__ == "__main__":
    sys.exit(main())
