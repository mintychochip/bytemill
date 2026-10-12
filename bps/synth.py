"""Synthetic packet values.

One value per declared field type, small enough to satisfy the schema's own
constraints (`require` assertions, count expressions, guard evaluation). The
interpreter round-trips these values for every packet, and the Rust emitter
uses them to compute a reference byte vector: the Python encoder is the
reference (its output is pinned by golden frames in `tests/test_play_packets.py`),
and the Rust serializer must agree with it byte for byte.

Values follow the *decoded* shape the interpreter produces: primitives as
Python scalars, structured types (`uuid`, `position`, `nbt`, `slot`, ...) as the
dicts `decode_packet` returns, repeated fields as lists.
"""

from __future__ import annotations

from bps import ast
from bps.errors import BpsError
from bps.interp.engine import select_match_arm
from bps.interp.evalexpr import eval_expr, truthy

# Per-packet field overrides for domains the generic synthesizer cannot infer (Task 4c).
SYNTH_OVERRIDES: dict[tuple[str, str], object] = {}

# Value per primitive, small enough to satisfy the schema's own constraints.
_PRIM: dict[str, object] = {
    "u8": 1,
    "i8": 1,
    "u16be": 0x1234,
    "i16be": 0x1234,
    "u32be": 0x01020304,
    "i32be": 0x01020304,
    "u64be": 0x0102030405060708,
    "i64be": 0x0102030405060708,
    "f32be": 1.5,
    "f64be": 2.25,
    "varint": 1,
    "varlong": 1,
    "boolean": True,
    "angle": 64,
    "string": "abc",
    "identifier": "minecraft:stone",
    "uuid": {"msb": 1, "lsb": 2},
    "position": {"raw": 0x4607632C15B4833F},
    "nbt": {"tag_type": 10, "value": {"entries": []}},
    "text_component": {"tag_type": 8, "value": "abc"},
    "nbt_string": "abc",
    "slot": {"item_count": 0},
    "hashed_slot": {"has_item": False},
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
    # An empty slot display, and a stonecutter recipe display of three empty ones.
    "slot_display": {"type": 0, "value": {}},
    "recipe_display": {
        "type": 3,
        "value": {
            "ingredient": {"type": 0, "value": {}},
            "result": {"type": 0, "value": {}},
            "crafting_station": {"type": 0, "value": {}},
        },
    },
    "id_set": {"type": 2, "ids": [300]},
    "block_state": {"block_state": 300},
    "water_blocks": {"water_blocks": 300},
    "water_blocks_burst": {"water_blocks": 300, "burst_impulse_base": 1.5},
    "power": {"power": 1.5},
    "dust_color": {"color": 300},
    "dust_color_scale": {"color": 300, "scale": 1.5},
    "dust_color_transition": {"from_color": 300, "to_color": 300, "scale": 1.5},
    "spell_particle": {"color": 300, "power": 1.5},
    "sculk_charge_roll": {"roll": 1.5},
    "shriek_delay": {"delay": 300},
    "trail_data": {"x": 2.25, "y": 2.25, "z": 2.25, "color": 300, "duration": 300},
}

_RAW = b"\x01\x02\x03"


class _Env:
    """Encoding scope shared by one synthesis pass (`_value_for` reads it)."""

    def __init__(self, resolved: ast.ResolvedSchema) -> None:
        self.encodings = resolved.encodings


def _value_for(t, env: dict, scope: _Env) -> object:
    if isinstance(t, ast.BlockType):
        return _block_value(t.items, env, scope)
    if isinstance(t, ast.MatchType):
        for arm in t.arms:
            if arm.type_ref is not None:
                return _value_for(arm.type_ref, env, scope)
        return 0
    name = t.name
    if name == "rest" or name == "bytes":
        return _RAW
    if name in _PRIM:
        return _PRIM[name]
    enc = scope.encodings.get(name)
    if enc is None:
        return _RAW
    if isinstance(enc.body, ast.TypeRef):
        return _value_for(enc.body, env, scope)
    if enc.yield_expr is not None:
        return _PRIM.get(name, 1)
    return _block_value(enc.body.items, env, scope)


def _concrete_arm_tag(arm: ast.MatchArm) -> int | None:
    raw = arm.tags if arm.tags else (() if arm.tag is None else (arm.tag,))
    for tag in raw:
        if isinstance(tag, int):
            return tag
    return None


def _synth_match(item: ast.BlockItem, local: dict, scope: _Env) -> dict:
    """Synthesize the arm selected by the scrutinee already in `local`.

    When no arm matches and there is no `_`, retarget a plain identifier
    scrutinee onto the first concrete tag so the value can still encode.
    """
    mt = item.match
    if mt is None:
        return {}
    try:
        tag = int(eval_expr(mt.tag_expr, local))
    except (BpsError, TypeError, ValueError):
        tag = None
    arm = select_match_arm(mt, tag) if tag is not None else None
    extra: dict = {}
    if arm is None and mt.arms:
        arm = next((a for a in mt.arms if _concrete_arm_tag(a) is not None), mt.arms[0])
        chosen = _concrete_arm_tag(arm) if arm is not None else None
        if chosen is not None and mt.tag_expr.kind == "ident" and mt.tag_expr.name:
            local[mt.tag_expr.name] = chosen
            extra[mt.tag_expr.name] = chosen
    if arm is not None and isinstance(arm.type_ref, ast.BlockType):
        extra.update(_block_value_shared(arm.type_ref.items, local, scope))
    return extra


def _block_value_shared(items: tuple[ast.BlockItem, ...], local: dict, scope: _Env) -> dict:
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
                out.update(_block_value_shared(item.group_items, local, scope))
            continue
        if item.kind == "match":
            out.update(_synth_match(item, local, scope))
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
            [_value_for(wf.type_ref, local, scope)]
            if wf.repeat is not None
            else _field_value(wf.type_ref, local, out, scope)
        )
        out[wf.name] = v
        local[wf.name] = v
        _fit_length(wf, v, local, out)
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


def _block_value(items: tuple[ast.BlockItem, ...], env: dict, scope: _Env) -> dict:
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
                # inside it is visible here — pass `local` itself.
                out.update(_block_value_shared(item.group_items, local, scope))
            continue
        if item.kind == "match":
            out.update(_synth_match(item, local, scope))
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
            [_value_for(wf.type_ref, local, scope)]
            if wf.repeat is not None
            else _field_value(wf.type_ref, local, out, scope)
        )
        out[wf.name] = v
        local[wf.name] = v
        _fit_length(wf, v, local, out)
        # A count-modulated repeat may also carry a length field elsewhere in
        # the block, or derive the count from a computed field or an expression
        # over it. In either case the count the decode side evaluates must equal
        # the list length, so derive the driving value from the list length.
        if wf.repeat is not None and wf.repeat.mode == "count":
            driver = _count_driver(wf.repeat.expr)
            if driver is not None and driver != wf.name:
                if driver in out:
                    out[driver] = len(v)
                    local[driver] = len(v)
                else:
                    local[driver] = len(v)
            else:
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


def _equality_pin(expr: ast.Expr) -> tuple[str, object] | None:
    if expr.kind != "binop" or expr.value != "==":
        return None
    left, right = expr.left, expr.right
    if left is not None and left.kind == "ident" and right is not None and right.kind == "int":
        return left.name, int(right.value)
    if right is not None and right.kind == "ident" and left is not None and left.kind == "int":
        return right.name, int(left.value)
    return None


def _require_equalities(items: tuple[ast.BlockItem, ...]) -> list[tuple[str, object]]:
    pins: list[tuple[str, object]] = []
    for item in items:
        if item.kind == "require" and item.require_expr is not None:
            pin = _equality_pin(item.require_expr)
            if pin is not None:
                pins.append(pin)
        elif item.kind == "group" and item.group_items:
            pins.extend(_require_equalities(item.group_items))
        elif item.kind == "match" and item.match is not None:
            for arm in item.match.arms:
                if isinstance(arm.type_ref, ast.BlockType):
                    pins.extend(_require_equalities(arm.type_ref.items))
    return pins


def _declared_wire_fields(items: tuple[ast.BlockItem, ...]) -> set[str]:
    names: set[str] = set()
    for item in items:
        if item.kind == "field" and item.wire is not None:
            names.add(item.wire.name)
        elif item.kind == "group" and item.group_items:
            names |= _declared_wire_fields(item.group_items)
        elif item.kind == "match" and item.match is not None:
            for arm in item.match.arms:
                if isinstance(arm.type_ref, ast.BlockType):
                    names |= _declared_wire_fields(arm.type_ref.items)
    return names


def _validate_synth_overrides(resolved: ast.ResolvedSchema) -> None:
    for (pkt_name, field_name), _ in SYNTH_OVERRIDES.items():
        pkt = resolved.packets.get(pkt_name)
        if pkt is None:
            raise BpsError(f"SYNTH_OVERRIDES references unknown packet {pkt_name!r}")
        if field_name not in _declared_wire_fields(pkt.items):
            raise BpsError(
                f"SYNTH_OVERRIDES references unknown field {field_name!r} on packet {pkt_name!r}"
            )


def synthesize_packet(resolved: ast.ResolvedSchema, pkt: ast.PacketDecl) -> dict:
    """A decoded-shape value for `pkt` that the interpreter encodes cleanly."""
    if SYNTH_OVERRIDES:
        _validate_synth_overrides(resolved)
    out = _block_value(pkt.items, {}, _Env(resolved))
    for name, val in _require_equalities(pkt.items):
        out[name] = val
    for (pkt_name, field_name), val in SYNTH_OVERRIDES.items():
        if pkt_name == pkt.name:
            out[field_name] = val
    return out


def _fit_length(wf: ast.WireField, value: object, local: dict, out: dict) -> None:
    """A `bytes(n)` payload is exactly `n` bytes: make `n` the data's length."""
    t = wf.type_ref
    if wf.repeat is not None or not isinstance(t, ast.TypeRef) or t.name != "bytes" or not t.args:
        return
    driver = _count_driver(t.args[0])
    if driver is not None and driver in local and isinstance(value, (bytes, bytearray)):
        local[driver] = len(value)
        if driver in out:
            out[driver] = len(value)


def _field_value(t, local: dict, out: dict, scope: _Env) -> object:
    """A field's value; for a `match` type, the arm its scrutinee selects.

    When no arm matches and there is no `_`, a plain identifier scrutinee is
    retargeted onto the first concrete tag so the value can still encode.
    """
    if not isinstance(t, ast.MatchType):
        return _value_for(t, local, scope)
    try:
        tag = int(eval_expr(t.tag_expr, local))
    except (BpsError, TypeError, ValueError):
        tag = None
    arm = select_match_arm(t, tag) if tag is not None else None
    if arm is None:
        arm = next((a for a in t.arms if _concrete_arm_tag(a) is not None), None)
        chosen = _concrete_arm_tag(arm) if arm is not None else None
        if chosen is not None and t.tag_expr.kind == "ident" and t.tag_expr.name:
            # `out` may be a conditional group's result, merged into its parent's.
            local[t.tag_expr.name] = chosen
            out[t.tag_expr.name] = chosen
    if arm is None or arm.type_ref is None:
        return {}
    return _value_for(arm.type_ref, local, scope)
