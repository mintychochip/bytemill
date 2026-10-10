from __future__ import annotations

import struct
from typing import Any

from bps import ast
from bps.errors import DecodeError, EncodeError
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
) -> dict[str, Any]:
    env: dict[str, Any] = dict(param_env)
    expanded = _expand_items(items, resolved, param_env)
    for item in expanded:
        if item.kind == "require":
            if not truthy(eval_expr(item.require_expr, env)):
                raise DecodeError(f"require failed: {item.require_expr}")
            continue
        if item.kind == "computed":
            env[item.computed_name] = eval_expr(item.computed_expr, env)
            continue
        if item.kind == "field" and item.wire:
            wf = item.wire
            if wf.if_expr is not None and not truthy(eval_expr(wf.if_expr, env)):
                continue
            if wf.repeat:
                env[wf.name] = _decode_repeat(wf, cur, resolved, env)
            else:
                env[wf.name] = _decode_type(wf.type_ref, cur, resolved, env)
    return env


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
                out.extend(_expand_items(enc.body.items, resolved, bind))
            else:
                out.append(
                    ast.BlockItem(
                        "field",
                        wire=ast.WireField("_spread", enc.body, None, None),
                    )
                )
        else:
            out.append(item)
    return out


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


def decode_type_with_tag(
    t: ast.TypeRef | ast.BlockType | ast.MatchType,
    cur: Cursor,
    resolved: ast.ResolvedSchema,
    env: dict[str, Any],
    tag: int,
) -> Any:
    if isinstance(t, ast.MatchType):
        for arm in t.arms:
            if arm.tag is None:
                if arm.type_ref is None:
                    continue
                return _decode_arm_type(arm.type_ref, cur, resolved, env)
            arm_tag = arm.tag
            if isinstance(arm_tag, str):
                # enum member - skip for now
                continue
            if int(arm_tag) == tag:
                return _decode_arm_type(arm.type_ref, cur, resolved, env)
        raise DecodeError(f"no match arm for tag {tag}")
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
        bind = _bind_params(enc.params, ref.args, env)
        if isinstance(enc.body, ast.BlockType):
            decoded = _decode_block_items(enc.body.items, cur, resolved, bind)
            if enc.name in ("string", "identifier") and "value" in decoded:
                return decoded["value"]
            if enc.name == "boolean" and "value" in decoded:
                return decoded["value"] != 0
            return decoded
        return _decode_type_ref(enc.body, cur, resolved, bind)
    if name == "leb128":
        bits = int(eval_expr(ref.args[0], env)) if ref.args else 32
        signed = False
        if len(ref.args) > 1:
            if ref.args[1].kind == "bool":
                signed = bool(ref.args[1].value)
            elif ref.args[1].kind == "ident" and ref.args[1].name == "signed":
                signed = True
        return _read_leb128(cur, bits, signed)
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
    if name == "i8":
        return struct.unpack(">b", cur.read(1))[0]
    if name == "u16be":
        return struct.unpack(">H", cur.read(2))[0]
    if name == "i16be":
        return struct.unpack(">h", cur.read(2))[0]
    if name == "u32be":
        return struct.unpack(">I", cur.read(4))[0]
    if name == "i32be":
        return struct.unpack(">i", cur.read(4))[0]
    if name == "u64be":
        return struct.unpack(">Q", cur.read(8))[0]
    if name == "i64be":
        return struct.unpack(">q", cur.read(8))[0]
    if name == "bool" or name == "boolean":
        b = cur.read(1)[0]
        return b != 0
    if name == "f32be":
        return struct.unpack(">f", cur.read(4))[0]
    if name == "f64be":
        return struct.unpack(">d", cur.read(8))[0]
    raise DecodeError(f"cannot decode type {name!r}")


def _read_leb128(cur: Cursor, bits: int, signed: bool) -> int:
    shift = 0
    result = 0
    max_bytes = 10 if bits == 64 else 5
    for _ in range(max_bytes):
        b = cur.read(1)[0]
        result |= (b & 0x7F) << shift
        shift += 7
        if (b & 0x80) == 0:
            break
    else:
        raise DecodeError("leb128 too long", offset=cur.pos)
    if signed:
        if bits == 32:
            if result & (1 << 31):
                result -= 1 << 32
        elif bits == 64:
            if result & (1 << 63):
                result -= 1 << 64
    return result


def _write_leb128(w: WriteCursor, value: int, bits: int) -> None:
    if bits == 32:
        value &= 0xFFFFFFFF
    elif bits == 64:
        value &= 0xFFFFFFFFFFFFFFFF
    while True:
        b = value & 0x7F
        value >>= 7
        if value:
            w.emit(bytes([b | 0x80]))
        else:
            w.emit(bytes([b]))
            break


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
        if item.kind == "field" and item.wire:
            wf = item.wire
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
        _encode_block_items(t.items, w, resolved, val)
        return
    if isinstance(t, ast.TypeRef):
        _encode_type_ref(t, w, resolved, env, val)
        return
    raise EncodeError("match encode not implemented")


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
        bind = _bind_params(enc.params, ref.args, env)
        if isinstance(enc.body, ast.BlockType):
            if not isinstance(val, dict):
                raise EncodeError(f"expected dict for {name}")
            _encode_block_items(enc.body.items, w, resolved, {**env, **val})
            return
        return _encode_type_ref(enc.body, w, resolved, bind, val)
    if name == "leb128" or name == "varint" or name == "varlong":
        bits = 64 if name == "varlong" else 32
        if name == "leb128" and ref.args:
            bits = int(eval_expr(ref.args[0], env))
        _write_leb128(w, int(val), bits)
        return
    if name == "bytes":
        if isinstance(val, str):
            raw = val.encode("utf-8")
        else:
            raw = bytes(val)
        w.emit(raw)
        return
    if name == "u16be":
        w.emit(struct.pack(">H", int(val) & 0xFFFF))
        return
    if name == "i64be":
        w.emit(struct.pack(">q", int(val)))
        return
    if name == "u64be":
        w.emit(struct.pack(">Q", int(val)))
        return
    if name == "u8":
        w.emit(struct.pack(">B", int(val) & 0xFF))
        return
    if name == "i8":
        w.emit(struct.pack(">b", int(val)))
        return
    if name == "bool" or name == "boolean":
        w.emit(bytes([1 if val else 0]))
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
    length = _read_leb128(cur, 32, True)
    frame_end = cur.pos + length
    packet_id = _read_leb128(cur, 32, True)
    payload_data = cur.read(frame_end - cur.pos)
    for route, pkt in resolved.routes:
        if route.state == state and route.direction == direction and route.packet_id == packet_id:
            fields = decode_packet(resolved, pkt.name, payload_data)
            return packet_id, {"packet": pkt.name, "fields": fields}
    raise DecodeError(f"no route for id 0x{packet_id:x} in {state}.{direction}")
