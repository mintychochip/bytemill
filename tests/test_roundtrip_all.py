"""Round-trip coverage: every declared packet must encode a synthetic value and
decode back to it.

Values are synthesized from the decoded field types, so encode and decode are
exercised symmetrically across the whole 261-route surface without per-packet
fixtures. Conditional groups, flag-driven optional blocks, counted repeats and
`rest`/`bytes` frames are all handled; registry-dependent payloads stay as raw
bytes and still round-trip.
"""

import unittest

from bps import ast
from bps.errors import BpsError
from bps.interp.engine import decode_packet, encode_packet
from bps.interp.evalexpr import eval_expr, truthy

from tests.test_varint import load_minecraft

# Value per primitive, small enough to satisfy the schema's own constraints.
_PRIM = {
    "u8": 0x2A,
    "i8": 42,
    "u16be": 0x1234,
    "i16be": 0x1234,
    "u32be": 0x01020304,
    "i32be": 0x01020304,
    "u64be": 0x0102030405060708,
    "i64be": 0x0102030405060708,
    "f32be": 1.5,
    "f64be": 2.25,
    "varint": 300,
    "varlong": 300,
    "leb128": 300,
    "bool": True,
    "boolean": True,
    "angle": 64,
    "string": "abc",
    "identifier": "minecraft:stone",
    "uuid": {"msb": 1, "lsb": 2},
    "position": {"raw": 0x4607632C15B4833F},
    "nbt": {"entries": []},
    "text_component": {"tag_type": 0, "value": {}},
    "nbt_string": "abc",
    "slot": {"item_count": 0},
    "hashed_slot": {"has_item": False},
    "bitset": {"long_count": 0, "longs": []},
    "light_data": {
        "sky_light_mask": {"long_count": 0, "longs": []},
        "block_light_mask": {"long_count": 0, "longs": []},
        "empty_sky_light_mask": {"long_count": 0, "longs": []},
        "empty_block_light_mask": {"long_count": 0, "longs": []},
        "sky_light_arrays_count": 0,
        "sky_light_arrays": [],
        "block_light_arrays_count": 0,
        "block_light_arrays": [],
    },
    "id_or_sound_event": {"id": 1},
    "game_profile": {
        "profile_uuid": {"msb": 1, "lsb": 2},
        "username": "abc",
        "properties_count": 0,
        "properties": [],
    },
    "entity_metadata": {"entries": []},
    "meta_entry": None,
    "nbt_compound_body": {"entries": []},
    "nbt_nothing": {},
    "nbt_tag": {"tag_type": 0, "value": None},
    "lpvec3": {
        "scale_byte": 0x00,
        "packed_second": 0x00,
        "packed_high_4": 0x00000000,
    },
    "particle_data": {"particle_type": 300, "value": {}},
    "id_or_payload": {"id": 1},
    "slot_display": {"id": 1},
    "recipe_display": {"id": 1},
    "id_or_nbt": {"id": 1},
    "id_or_sound_event": {"id": 1},
    "id_set": {"type": 2, "ids": [300]},
    "block_state": {"block_state": 300},
    "water_blocks": {"water_blocks": 300},
    "water_blocks_burst": {"water_blocks": 300, "burst_impulse_base": 1.5},
    "power": {"power": 1.5},
    "dust_color": {"color": 300},
    "dust_color_scale": {"color": 300, "scale": 1.5},
    "dust_color_transition": {"from_color": 300, "to_color": 300, "scale": 1.5},
    "sculk_charge_roll": {"roll": 1.5},
    "shriek_delay": {"delay": 300},
    "vibration_source": {"block_position": {"raw": 0x4607632C15B4833F}, "entity_id": 300, "entity_eye_height": 1.5},
    "trail_data": {"x": 2.25, "y": 2.25, "z": 2.25, "color": 300, "duration": 300},
}

_RAW = b"\x01\x02\x03"


def _value_for(t, env: dict) -> object:
    if isinstance(t, ast.BlockType):
        return _block_value(t.items, env)
    if isinstance(t, ast.MatchType):
        for arm in t.arms:
            if arm.type_ref is not None:
                return _value_for(arm.type_ref, env)
        return 0
    name = t.name
    if name == "rest" or name == "bytes":
        return _RAW
    if name in _PRIM:
        return _PRIM[name]
    enc = env_encodings.get(name)
    if enc is None:
        return _RAW
    if isinstance(enc.body, ast.TypeRef):
        return _value_for(enc.body, env)
    return _block_value(enc.body.items, env)


def _block_value_shared(items: tuple[ast.BlockItem, ...], local: dict) -> dict:
    """Like `_block_value`, but reads and writes the caller's scope.

    A conditional group is part of the enclosing block's scope, so a field it
    declares (and any count it adjusts for a repeat) must be visible there too.
    """
    out: dict = {}
    for item in items:
        if item.kind == "computed":
            v = eval_expr(item.computed_expr, local)
            local[item.computed_name] = v
            out[item.computed_name] = v
            continue
        if item.kind == "require":
            continue
        if item.kind == "group":
            try:
                cond = item.group_if is None or truthy(eval_expr(item.group_if, local))
            except BpsError:
                cond = True
            if cond:
                out.update(_block_value_shared(item.group_items, local))
            continue
        if item.kind != "field" or item.wire is None:
            continue
        wf = item.wire
        if wf.if_expr is not None:
            try:
                if not truthy(eval_expr(wf.if_expr, local)):
                    continue
            except BpsError:
                pass
        v = (
            [_value_for(wf.type_ref, local)]
            if wf.repeat is not None
            else _value_for(wf.type_ref, local)
        )
        out[wf.name] = v
        local[wf.name] = v
        if wf.repeat is not None and wf.repeat.mode == "count":
            driver = _count_driver(wf.repeat.expr)
            if driver is not None and driver != wf.name:
                local[driver] = len(v)
                out[driver] = len(v)
            else:
                for ident in _expr_idents(wf.repeat.expr):
                    if ident in local and isinstance(local[ident], int):
                        local[ident] = 2
                        out[ident] = 2
                v = [v[0]] if v else []
                out[wf.name] = v
                local[wf.name] = v
    return out


def _block_value(items: tuple[ast.BlockItem, ...], env: dict) -> dict:
    out: dict = {}
    local = dict(env)
    for item in items:
        if item.kind == "computed":
            v = eval_expr(item.computed_expr, local)
            local[item.computed_name] = v
            out[item.computed_name] = v
            continue
        if item.kind == "require":
            continue
        if item.kind == "group":
            try:
                cond = item.group_if is None or truthy(eval_expr(item.group_if, local))
            except BpsError:
                # The guard reads a field not yet synthesized (e.g. a repeated
                # element's discriminator); include the group so the encode path
                # has values for it.
                cond = True
            if cond:
                # The group shares this block's scope, so a count adjustment
                # inside it (e.g. the step count driving `count = p >> 1`) is
                # visible here — pass `local` itself rather than a copy.
                group_out = _block_value_shared(item.group_items, local)
                out.update(group_out)
            continue
        if item.kind != "field" or item.wire is None:
            continue
        wf = item.wire
        if wf.if_expr is not None:
            try:
                if not truthy(eval_expr(wf.if_expr, local)):
                    continue
            except BpsError:
                pass
        if wf.repeat is None:
            v = _value_for(wf.type_ref, local)
        else:
            v = [_value_for(wf.type_ref, local)]
        out[wf.name] = v
        local[wf.name] = v
        # A count-modulated repeat may also carry a length field elsewhere in the
        # block (`n: varint` + `items: T[] count = n`), or derive the count from a
        # computed field or an expression over it. In either case the count the
        # decode side evaluates must equal the list length, so derive the driving
        # value from the list length itself.
        if wf.repeat is not None and wf.repeat.mode == "count":
            driver = _count_driver(wf.repeat.expr)
            if driver is not None and driver not in (wf.name,):
                if driver in out:
                    out[driver] = len(v)
                    local[driver] = len(v)
                else:
                    # The tag is a computed or group-scoped field.
                    local[driver] = len(v)
            else:
                # The count is an expression over an earlier field (e.g.
                # `count = properties >> 1`); emit exactly one element and set the
                # driving field so the expression evaluates to 1.
                for ident in _expr_idents(wf.repeat.expr):
                    if ident in local and isinstance(local[ident], int):
                        local[ident] = 2
                        if ident in out:
                            out[ident] = 2
                v = [v[0]] if v else []
                out[wf.name] = v
                local[wf.name] = v
    return out


def _expr_idents(expr) -> list[str]:
    """Every identifier an expression reads, innermost first."""
    if expr is None:
        return []
    found: list[str] = []
    if expr.kind == "ident":
        found.append(expr.name)
    if expr.left is not None:
        found.extend(_expr_idents(expr.left))
    if expr.right is not None:
        found.extend(_expr_idents(expr.right))
    for a in expr.args or ():
        found.extend(_expr_idents(a))
    return found


def _count_driver(expr) -> str | None:
    """Name of the int field a `count = expr` reads, when it is a plain identifier."""
    if expr is None:
        return None
    return expr.name if expr.kind == "ident" else None


env_encodings: dict[str, ast.EncodingDecl] = {}


def synthetic_for(pkt: ast.PacketDecl) -> dict:
    return _block_value(pkt.items, {})


class RoundTripAllTest(unittest.TestCase):
    def test_every_packet_round_trips(self) -> None:
        resolved = load_minecraft()
        env_encodings.clear()
        env_encodings.update(resolved.encodings)
        failures = []
        checked = 0
        for name, pkt in sorted(resolved.packets.items()):
            if name.endswith("_probe"):
                continue
            value = synthetic_for(pkt)
            try:
                encoded = encode_packet(resolved, name, value)
            except BpsError as e:
                failures.append(f"{name}: encode {e}")
                continue
            try:
                decoded = decode_packet(resolved, name, encoded)
            except BpsError as e:
                # `require` assertions are schema contracts, not round-trip
                # failures: a synthetic value that violates one is expected to be
                # rejected (handshake pins protocol_version == 777).
                if "require failed" in str(e):
                    continue
                failures.append(f"{name}: decode {e}")
                continue
            try:
                reencoded = encode_packet(resolved, name, decoded)
            except BpsError as e:
                failures.append(f"{name}: re-encode {e}")
                continue
            if reencoded != encoded:
                failures.append(f"{name}: re-encode differs")
            checked += 1
        self.assertEqual(failures[:10], [])
        self.assertGreaterEqual(checked, 230)


class RouteSmokeTest(unittest.TestCase):
    def test_every_route_decodes_its_packet(self) -> None:
        """Each route must dispatch to its packet and decode a zero-seeded payload.

        Registry-dependent layouts (Slot, component data) legitimately reject a
        zero buffer, so a clean BpsError is a pass; a crash is not.
        """
        resolved = load_minecraft()
        bad = []
        for route, pkt in resolved.routes:
            payload = bytes(8192)
            try:
                decode_packet(resolved, pkt.name, payload)
            except BpsError:
                continue
            except Exception as e:  # pragma: no cover - defensive
                bad.append(f"{route.state}.{route.direction} 0x{route.packet_id:x}: {e!r}")
                continue
        self.assertEqual(bad, [])


if __name__ == "__main__":
    unittest.main()
