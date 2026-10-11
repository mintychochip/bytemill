from __future__ import annotations

import struct
from typing import Any

from bps import ast
from bps.errors import BpsError, DecodeError, EncodeError
from bps.interp.cursor import Cursor, WriteCursor
from bps.interp.evalexpr import eval_expr, truthy


def decode_packet(resolved: ast.ResolvedSchema, packet_name: str, data: bytes) -> dict[str, Any]:
    if packet_name not in resolved.packets:
        raise DecodeError(f"unknown packet {packet_name!r}")
    pkt = resolved.packets[packet_name]
    cur = Cursor(data)
    env = _decode_block_items(pkt.items, cur, resolved, {})
    if cur.remaining() != 0:
        pass  # trailing bytes allowed for rest fields in future
    return env


def encode_packet(resolved: ast.ResolvedSchema, packet_name: str, value: dict[str, Any]) -> bytes:
    if packet_name not in resolved.packets:
        raise EncodeError(f"unknown packet {packet_name!r}")
    pkt = resolved.packets[packet_name]
    w = WriteCursor()
    _encode_block_items(pkt.items, w, resolved, value)
    return w.result()


def _decode_block_items(
    items: tuple[ast.BlockItem, ...],
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    param_env: dict[str, Any],
    prebind: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Decode a block; returns only the names this block defines (no parent env leak).

    `prebind` supplies values visible to conditions and `require` (optional
    varint bytes are 0 when absent) without being stamped as spread parameters,
    which would overwrite the bytes actually read.
    """
    env: dict[str, Any] = {}
    if prebind:
        env.update(prebind)
    env.update(param_env)
    out: dict[str, Any] = {}
    expanded = _expand_items(items, resolved, param_env)

    def bind(name: str, value: Any) -> None:
        env[name] = value
        if name != "_spread":
            out[name] = value

    for item in expanded:
        if item.kind == "require":
            scope = _merge_group_env({**param_env, **env}, item) if item._param_env else env
            if not truthy(eval_expr(item.require_expr, scope)):
                raise DecodeError(f"require failed: {item.require_expr}")
            continue
        if item.kind == "group":
            # A group that wraps a spread carries that spread's parameters, and
            # nested groups keep the same scope; otherwise the guard uses this
            # block's own env.
            group_env = _merge_group_env({**param_env, **env}, item)
            if item.group_if is not None:
                try:
                    cond = truthy(eval_expr(item.group_if, group_env))
                except BpsError:
                    cond = True
            else:
                cond = True
            if cond:
                if cur.remaining() == 0:
                    # Optional trailing group absent on the wire (matches the
                    # encode side, which also omits it under this generator).
                    continue
                for k, v in _decode_block_items(
                    item.group_items,
                    cur,
                    resolved,
                    _merge_group_env(group_env, item),
                    prebind,
                ).items():
                    bind(k, v)
            continue
        if item.kind == "computed":
            bind(item.computed_name, eval_expr(item.computed_expr, env))
            continue
        if item.kind == "match" and item.match is not None:
            tag = int(eval_expr(item.match.tag_expr, env))
            arm = select_match_arm(item.match, tag)
            if arm is None:
                raise DecodeError(f"no match arm for tag {tag}")
            if isinstance(arm.type_ref, ast.BlockType):
                for k, v in _decode_block_items(
                    arm.type_ref.items, cur, resolved, env, prebind
                ).items():
                    bind(k, v)
            elif arm.type_ref is not None:
                raise DecodeError("statement match arm must be a block")
            continue
        if item.kind == "field" and item.wire:
            wf = item.wire
            if wf.if_expr is not None and not truthy(eval_expr(wf.if_expr, env)):
                continue
            if wf.repeat:
                bind(wf.name, _decode_repeat(wf, cur, resolved, env))
            else:
                bind(wf.name, _decode_type(wf.type_ref, cur, resolved, env))
    return out


def _expand_items(
    items: tuple[ast.BlockItem, ...],
    resolved: ast.ResolvedSchema,
    param_env: dict[str, Any],
) -> list[ast.BlockItem]:
    out: list[ast.BlockItem] = []
    for item in items:
        if item.kind == "spread":
            enc = resolved.encodings[item.spread_name]
            bind = _bind_params(enc.params, item.spread_args, param_env)
            if isinstance(enc.body, ast.BlockType):
                # Wrap the expanded block in an always-present group whose env
                # carries the spread's parameters, so nested spreads and fields
                # that read them still resolve.
                out.append(
                    _with_param_env(
                        ast.BlockItem(
                            "group",
                            group_if=None,
                            group_items=tuple(
                                _expand_items(enc.body.items, resolved, bind)
                            ),
                        ),
                        bind,
                    )
                )
            else:
                out.append(_with_param_env(
                    ast.BlockItem(
                        "field",
                        wire=ast.WireField("_spread", enc.body, None, None),
                    ),
                    bind,
                ))
        elif item.kind == "group":
            out.append(
                _with_param_env(
                    ast.BlockItem(
                        "group",
                        group_if=item.group_if,
                        group_items=tuple(
                            _expand_items(item.group_items, resolved, param_env)
                        ),
                    ),
                    item._param_env or param_env,
                )
            )
        elif item.kind == "match" and item.match is not None:
            new_arms = []
            for arm in item.match.arms:
                body = arm.type_ref
                if isinstance(body, ast.BlockType):
                    body = ast.BlockType(
                        tuple(_expand_items(body.items, resolved, param_env))
                    )
                new_arms.append(ast.MatchArm(arm.tag, body, arm.tags))
            out.append(
                _with_param_env(
                    ast.BlockItem(
                        "match",
                        match=ast.MatchType(item.match.tag_expr, tuple(new_arms)),
                    ),
                    param_env,
                )
            )
        else:
            out.append(_with_param_env(item, param_env))
    return out


def _with_param_env(item: ast.BlockItem, env: dict[str, Any] | None) -> ast.BlockItem:
    """Tag an expanded item with the parameter scope its expressions read."""
    if not env or item._param_env is env:
        return item
    from dataclasses import replace as _dc_replace

    return _dc_replace(item, _param_env=env)


def _bind_params(
    params: tuple[ast.Param, ...],
    args: tuple[ast.Expr, ...],
    env: dict[str, Any],
) -> dict[str, Any]:
    bind: dict[str, Any] = dict(env)
    for i, p in enumerate(params):
        if i < len(args):
            bind[p.name] = eval_expr(args[i], env)
        elif p.default is not None:
            bind[p.name] = p.default
        else:
            raise DecodeError(f"missing param {p.name}")
    return bind


def _decode_repeat(
    wf: ast.WireField,
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
) -> list[Any]:
    rep = wf.repeat
    assert rep is not None
    out: list[Any] = []
    if rep.mode == "count":
        n = int(eval_expr(rep.expr, env))
        for _ in range(n):
            out.append(_decode_type(wf.type_ref, cur, resolved, env))
        return out
    if rep.mode == "until_end":
        while cur.remaining() > 0:
            out.append(_decode_type(wf.type_ref, cur, resolved, env))
        return out
    if rep.mode == "until":
        term = int(eval_expr(rep.expr, env))
        while True:
            if cur.remaining() == 0:
                break
            # peek for u8 terminator in metadata
            if cur.peek_u8() == term:
                cur.read(1)
                break
            out.append(_decode_type(wf.type_ref, cur, resolved, env))
        return out
    raise DecodeError(f"unknown repeat {rep.mode}")


def _decode_type(
    t: ast.TypeRef | ast.BlockType | ast.MatchType,
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
) -> Any:
    if isinstance(t, ast.BlockType):
        return _decode_block_items(t.items, cur, resolved, env)
    if isinstance(t, ast.MatchType):
        tag = int(eval_expr(t.tag_expr, env))
        return decode_type_with_tag(t, cur, resolved, env, tag)
    return _decode_type_ref(t, cur, resolved, env)


def _is_default_arm(arm: ast.MatchArm) -> bool:
    return arm.tag is None and not arm.tags


def _arm_tag_list(arm: ast.MatchArm) -> tuple[int | str, ...]:
    if arm.tags:
        return arm.tags
    if arm.tag is None:
        return ()
    return (arm.tag,)


def select_match_arm(mt: ast.MatchType, tag: int) -> ast.MatchArm | None:
    """First arm whose tags include `tag`. `_` is the fallback, wherever it sits."""
    default: ast.MatchArm | None = None
    for arm in mt.arms:
        if _is_default_arm(arm):
            default = arm
            continue
        for arm_tag in _arm_tag_list(arm):
            if isinstance(arm_tag, str):
                continue
            if int(arm_tag) == tag:
                return arm
    return default


def decode_type_with_tag(
    t: ast.TypeRef | ast.BlockType | ast.MatchType,
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
    tag: int,
) -> Any:
    if isinstance(t, ast.MatchType):
        arm = select_match_arm(t, tag)
        if arm is None:
            raise DecodeError(f"no match arm for tag {tag}")
        return _decode_arm_type(arm.type_ref, cur, resolved, env)
    return _decode_type(t, cur, resolved, env)


def _decode_arm_type(
    t: ast.TypeRef | ast.BlockType | None,
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
) -> Any:
    if t is None:
        return None
    if isinstance(t, ast.BlockType):
        return _decode_block_items(t.items, cur, resolved, env)
    return _decode_type_ref(t, cur, resolved, env)


def _decode_type_ref(
    ref: ast.TypeRef,
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
) -> Any:
    name = ref.name
    if name in resolved.encodings:
        enc = resolved.encodings[name]
        # Parameters only: a parameterless encoding must not inherit the caller's
        # whole scope, or its field names collide with the caller's.
        bind = _bind_params(enc.params, ref.args, {} if not enc.params else env)
        if isinstance(enc.body, ast.BlockType):
            if enc.yield_expr is not None:
                return _decode_scalar(enc, cur, resolved, bind)
            # Parameters of the encoding stay in scope for its own fields and
            # for any `...spread` inside it.
            decoded = _decode_block_items(_expand_items(enc.body.items, resolved, bind), cur, resolved, bind)
            if enc.name in ("string", "identifier", "nbt_string") and "value" in decoded:
                return decoded["value"]
            return decoded
        return _decode_type_ref(enc.body, cur, resolved, bind)
    if name == "bytes":
        n = int(eval_expr(ref.args[0], env))
        raw = cur.read(n)
        for tr in ref.transforms:
            if tr == "utf8":
                return raw.decode("utf-8")
        return raw
    if name == "rest":
        return cur.read(cur.remaining())
    if name == "u8":
        return struct.unpack(">B", cur.read(1))[0]
    if name == "f32be":
        return struct.unpack(">f", cur.read(4))[0]
    if name == "f64be":
        return struct.unpack(">d", cur.read(8))[0]
    raise DecodeError(f"cannot decode type {name!r}")


def _collect_wire_names(items: tuple[ast.BlockItem, ...]) -> list[str]:
    names: list[str] = []
    for item in items:
        if item.kind == "field" and item.wire is not None and item.wire.name != "_spread":
            names.append(item.wire.name)
        elif item.kind == "group":
            names.extend(_collect_wire_names(item.group_items))
        elif item.kind == "match" and item.match is not None:
            for arm in item.match.arms:
                if isinstance(arm.type_ref, ast.BlockType):
                    names.extend(_collect_wire_names(arm.type_ref.items))
    return names


def _decode_scalar(
    enc: ast.EncodingDecl,
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    bind: dict[str, Any],
) -> Any:
    """Decode a block encoding whose value is `yield_expr`, not the field dict.

    Optional wire fields (a varint's continuation bytes) are 0 when absent, so
    the yield expression and any `require` can mention them unconditionally.
    """
    assert isinstance(enc.body, ast.BlockType)
    assert enc.yield_expr is not None
    zeros = {name: 0 for name in _collect_wire_names(enc.body.items)}
    decoded = _decode_block_items(enc.body.items, cur, resolved, bind, zeros)
    env = dict(zeros)
    env.update(bind)
    env.update(decoded)
    return eval_expr(enc.yield_expr, env)


def _encode_scalar(
    enc: ast.EncodingDecl,
    w: WriteCursor,
    resolved: ast.ResolvedSchema,
    bind: dict[str, Any],
    val: Any,
) -> None:
    """Encode a scalar by writing each field from its encode expression.

    `value` is the scalar being written. Each field is bound under its name
    after its expression runs, so a later `if` can see the byte just chosen.
    """
    assert isinstance(enc.body, ast.BlockType)
    env = dict(bind)
    env["value"] = val
    for name in _collect_wire_names(enc.body.items):
        env.setdefault(name, 0)
    _encode_scalar_items(enc.body.items, w, resolved, env)


def _encode_scalar_items(
    items: tuple[ast.BlockItem, ...],
    w: WriteCursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
) -> None:
    for item in items:
        if item.kind in ("require", "spread"):
            continue
        if item.kind == "computed" and item.computed_expr is not None:
            env[item.computed_name] = eval_expr(item.computed_expr, env)
            continue
        if item.kind == "group":
            if item.group_if is not None and not truthy(eval_expr(item.group_if, env)):
                continue
            _encode_scalar_items(item.group_items, w, resolved, env)
            continue
        if item.kind == "match" and item.match is not None:
            tag_value = int(eval_expr(item.match.tag_expr, env))
            arm = select_match_arm(item.match, tag_value)
            if arm is None:
                raise EncodeError(f"no match arm for tag {tag_value}")
            if isinstance(arm.type_ref, ast.BlockType):
                _encode_scalar_items(arm.type_ref.items, w, resolved, env)
            continue
        if item.kind == "field" and item.wire is not None:
            wf = item.wire
            if wf.if_expr is not None and not truthy(eval_expr(wf.if_expr, env)):
                continue
            if wf.encode_expr is not None:
                written = eval_expr(wf.encode_expr, env)
            else:
                written = env.get(wf.name)
            env[wf.name] = written
            if wf.repeat:
                _encode_repeat(wf, w, resolved, env, written)
            else:
                _encode_type(wf.type_ref, w, resolved, env, written)


def _group_field_names(items: tuple[ast.BlockItem, ...]) -> list[ast.WireField]:
    names: list[ast.WireField] = []
    for it in items:
        if it.kind == "field" and it.wire is not None:
            names.append(it.wire)
        elif it.kind == "group":
            names.extend(_group_field_names(it.group_items))
        elif it.kind == "match" and it.match is not None:
            for arm in it.match.arms:
                if isinstance(arm.type_ref, ast.BlockType):
                    names.extend(_group_field_names(arm.type_ref.items))
    return names


def _merge_param_env(env: dict[str, Any], item: ast.BlockItem) -> dict[str, Any]:
    """Spread parameters ride on the expanded group; keep them in scope."""
    if item._param_env:
        merged = dict(env)
        merged.update(item._param_env)
        return merged
    return env


def _merge_group_env(group_env: dict[str, Any], item: ast.BlockItem) -> dict[str, Any]:
    """A nested group inside a spread keeps the same parameter scope."""
    if item._param_env:
        merged = dict(group_env)
        merged.update(item._param_env)
        return merged
    return group_env


def _raw_bytes(val: Any) -> bytes | None:
    """Coerce a wire value into raw bytes: bytes, list of ints, or dict with bytes."""
    if isinstance(val, (bytes, bytearray)):
        return bytes(val)
    if isinstance(val, str):
        return val.encode("utf-8")
    if isinstance(val, list) and all(isinstance(b, int) for b in val):
        return bytes(val)
    if isinstance(val, dict):
        for key in ("value", "data", "longs"):
            if key in val:
                got = _raw_bytes(val[key])
                if got is not None:
                    return got
    return None


def _encode_block_items(
    items: tuple[ast.BlockItem, ...],
    w: WriteCursor,
    resolved: ast.ResolvedSchema,
    value: dict[str, Any],
) -> None:
    env = value
    expanded = _expand_items(items, resolved, {})
    for item in expanded:
        if item.kind in ("require", "computed"):
            continue
        if item.kind == "group":
            if item.group_if is None:
                _encode_block_items(
                    item.group_items, w, resolved, _merge_param_env(env, item)
                )
                continue
            try:
                present = truthy(eval_expr(item.group_if, env))
            except BpsError:
                # The guard names a field not present in the value being encoded;
                # trust the value's own keys instead.
                present = any(w.name in env for w in _group_field_names(item.group_items))
            if present:
                _encode_block_items(item.group_items, w, resolved, env)
            continue
        if item.kind == "match" and item.match is not None:
            scope = _merge_param_env(env, item)
            tag_value = int(eval_expr(item.match.tag_expr, scope))
            arm = select_match_arm(item.match, tag_value)
            if arm is None:
                raise EncodeError(f"no match arm for tag {tag_value}")
            if isinstance(arm.type_ref, ast.BlockType):
                _encode_block_items(arm.type_ref.items, w, resolved, scope)
            elif arm.type_ref is not None:
                raise EncodeError("statement match arm must be a block")
            continue
        if item.kind == "field" and item.wire:
            wf = item.wire
            if wf.name == "_spread":
                # Primitive spread: no caller-visible value, nothing to encode.
                continue
            if wf.if_expr is not None and not truthy(eval_expr(wf.if_expr, env)):
                continue
            v = env.get(wf.name)
            if wf.repeat:
                _encode_repeat(wf, w, resolved, env, v)
            else:
                _encode_type(wf.type_ref, w, resolved, env, v)


def _encode_repeat(
    wf: ast.WireField,
    w: WriteCursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
    seq: Any,
) -> None:
    if not isinstance(seq, list):
        raise EncodeError(f"expected list for {wf.name}")
    rep = wf.repeat
    assert rep is not None
    if rep.mode == "count":
        for item in seq:
            # The element encodes in the enclosing scope, so a `match` inside the
            # element can read a discriminator the enclosing block set (e.g. the
            # NBT entry's `tag_type`). Values for the element's own fields come
            # from `item` via `_encode_type`, which merges them over the env.
            _encode_type(wf.type_ref, w, resolved, env, item)
        return
    if rep.mode == "until":
        for item in seq:
            _encode_type(wf.type_ref, w, resolved, env, item)
        term = int(eval_expr(rep.expr, env))
        w.emit(bytes([term]))
        return
    raise EncodeError(f"encode not supported for repeat mode {rep.mode}")


def _encode_type(
    t: ast.TypeRef | ast.BlockType | ast.MatchType,
    w: WriteCursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
    val: Any,
) -> None:
    if isinstance(t, ast.BlockType):
        if not isinstance(val, dict):
            raise EncodeError("expected dict for block")
        _encode_block_items(t.items, w, resolved, {**env, **val})
        return
    if isinstance(t, ast.TypeRef):
        _encode_type_ref(t, w, resolved, env, val)
        return
    if isinstance(t, ast.MatchType):
        tag_value = int(eval_expr(t.tag_expr, env))
        arm = select_match_arm(t, tag_value)
        if arm is None:
            raise EncodeError(f"no match arm for tag {tag_value}")
        if arm.type_ref is None:
            return
        _encode_type(arm.type_ref, w, resolved, env, val)
        return
    raise EncodeError(f"cannot encode type {t!r}")


def _encode_type_ref(
    ref: ast.TypeRef,
    w: WriteCursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
    val: Any,
) -> None:
    name = ref.name
    if name in resolved.encodings:
        enc = resolved.encodings[name]
        # Parameters only: a parameterless encoding must not inherit the caller's
        # whole scope, or its field names collide with the caller's.
        bind = _bind_params(enc.params, ref.args, {} if not enc.params else env)
        if isinstance(enc.body, ast.BlockType):
            if enc.yield_expr is not None:
                _encode_scalar(enc, w, resolved, bind, val)
                return
            if isinstance(val, str) and enc.name in ("string", "identifier", "nbt_string"):
                raw = val.encode("utf-8")
                if enc.name == "nbt_string":
                    _encode_type_ref(ast.TypeRef("u16be"), w, resolved, {}, len(raw))
                else:
                    _encode_type_ref(ast.TypeRef("varint"), w, resolved, {}, len(raw))
                w.emit(raw)
                return
            if enc.name == "uuid" and isinstance(val, dict):
                w.emit(struct.pack(">Q", int(val["msb"])))
                w.emit(struct.pack(">Q", int(val["lsb"])))
                return
            if enc.name == "fixed_bitset":
                raw = _raw_bytes(val)
                if raw is None:
                    raise EncodeError(f"expected dict for {name}")
                w.emit(raw)
                return
            # A declared-but-empty payload (e.g. an empty NBT compound arm) needs
            # no value at all; only a non-dict for a non-empty body is an error.
            if not isinstance(val, dict) and enc.body.items:
                raise EncodeError(f"expected dict for {name}")
            payload = val if isinstance(val, dict) else {}
            expanded = _expand_items(enc.body.items, resolved, bind)
            _encode_block_items(expanded, w, resolved, {**env, **bind, **payload})
            return
        return _encode_type_ref(enc.body, w, resolved, bind, val)
    if name == "bytes":
        n = int(eval_expr(ref.args[0], env))
        raw = val.encode("utf-8") if isinstance(val, str) else bytes(val)
        if len(raw) < n:
            raw = raw + bytes(n - len(raw))  # fixed-size frame: zero pad
        elif len(raw) > n:
            raise EncodeError(f"bytes({n}) got {len(raw)} bytes")
        w.emit(raw)
        return
    if name == "rest":
        raw = _raw_bytes(val)
        if raw is None:
            raise EncodeError(f"expected bytes for rest, got {type(val).__name__}")
        w.emit(raw)
        return
    if name == "f32be":
        w.emit(struct.pack(">f", float(val)))
        return
    if name == "f64be":
        w.emit(struct.pack(">d", float(val)))
        return
    if name == "u8":
        w.emit(struct.pack(">B", int(val) & 0xFF))
        return
    raise EncodeError(f"cannot encode type {name!r}")


def decode_frame_payload(
    resolved: ast.ResolvedSchema,
    state: str,
    direction: str,
    data: bytes,
) -> tuple[int, dict[str, Any]]:
    """Decode length-prefixed frame: returns (packet_id, payload_fields)."""
    cur = Cursor(data)
    length = int(_decode_type_ref(ast.TypeRef("varint"), cur, resolved, {}))
    frame_end = cur.pos + length
    packet_id = int(_decode_type_ref(ast.TypeRef("varint"), cur, resolved, {}))
    payload_data = cur.read(frame_end - cur.pos)
    for route, pkt in resolved.routes:
        if route.state == state and route.direction == direction and route.packet_id == packet_id:
            fields = decode_packet(resolved, pkt.name, payload_data)
            return packet_id, {"packet": pkt.name, "fields": fields}
    raise DecodeError(f"no route for id 0x{packet_id:x} in {state}.{direction}")
