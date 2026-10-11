"""Rust frame walkers compiled from a schema's own encodings.

A payload with no length prefix (a recipe display, a particle, an advancement)
is carried by the generated Rust packets as raw bytes, and a reader has to scan
the structure to learn where it ends. Each walker here is compiled from the
encoding's definition, so it cannot drift from the schema the way a hand-written
mirror does.

`generate` returns the Rust source for the walkers of some root encodings and
every encoding they reach. An encoding it cannot compile (an `until` loop, a
call expression, a spread) is reported, not guessed at.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

from bps import ast
from bps.errors import EmitError

# Fixed-width scalars: byte size.
_FIXED = {
    "u8": 1,
    "i8": 1,
    "boolean": 1,
    "angle": 1,
    "u16be": 2,
    "i16be": 2,
    "u32be": 4,
    "i32be": 4,
    "f32be": 4,
    "u64be": 8,
    "i64be": 8,
    "f64be": 8,
    "position": 8,
    "uuid": 16,
}
_SIGNED = {"i8", "i16be", "i32be", "i64be"}
# Variable-width integers: most bytes they may take.
_VAR = {"varint": 5, "varlong": 10}
# A varint length, then that many bytes.
_STRINGS = {"string", "identifier"}


class Unsupported(EmitError):
    """The encoding uses a construct the walker compiler does not handle."""


def walker_name(encoding: str) -> str:
    return f"{encoding}_len"


def _skip_name(encoding: str) -> str:
    return f"skip_{encoding}"


def _alias_target(resolved: ast.ResolvedSchema, name: str) -> str:
    """Follow `encoding a = b` aliases to the encoding that has a body."""
    seen: set[str] = set()
    while name not in seen:
        seen.add(name)
        enc = resolved.encodings.get(name)
        if enc is None or not isinstance(enc.body, ast.TypeRef) or enc.body.args:
            break
        if enc.body.name in _FIXED or enc.body.name in _VAR or enc.body.name in _STRINGS:
            break
        name = enc.body.name
    return name


def _exprs_of(item: ast.BlockItem):
    """Every expression one block item evaluates, its own and its nested blocks'."""
    if item.kind == "computed" and item.computed_expr is not None:
        yield item.computed_expr
    if item.kind == "group":
        if item.group_if is not None:
            yield item.group_if
        for sub in item.group_items:
            yield from _exprs_of(sub)
    if item.kind == "match" and item.match is not None:
        yield item.match.tag_expr
        for arm in item.match.arms:
            yield from _exprs_of_type(arm.type_ref)
    if item.kind == "field" and item.wire is not None:
        wf = item.wire
        if wf.if_expr is not None:
            yield wf.if_expr
        if wf.repeat is not None and wf.repeat.expr is not None:
            yield wf.repeat.expr
        yield from _exprs_of_type(wf.type_ref)


def _exprs_of_type(t):
    if isinstance(t, ast.BlockType):
        for sub in t.items:
            yield from _exprs_of(sub)
    elif isinstance(t, ast.MatchType):
        yield t.tag_expr
        for arm in t.arms:
            yield from _exprs_of_type(arm.type_ref)
    elif isinstance(t, ast.TypeRef):
        yield from t.args


def _idents(expr: ast.Expr | None) -> set[str]:
    if expr is None:
        return set()
    found: set[str] = set()
    if expr.kind == "ident" and expr.name:
        found.add(expr.name)
    for sub in (expr.left, expr.right, *expr.args):
        found |= _idents(sub)
    return found


def _declared(items: tuple[ast.BlockItem, ...]) -> list[str]:
    """Names a block declares in its own scope: fields, computeds, and those of
    the groups and match arms that share it."""
    names: list[str] = []
    for item in items:
        if item.kind == "field" and item.wire is not None:
            names.append(item.wire.name)
            if isinstance(item.wire.type_ref, ast.MatchType):
                for arm in item.wire.type_ref.arms:
                    if isinstance(arm.type_ref, ast.BlockType):
                        names.extend(_declared(arm.type_ref.items))
        elif item.kind == "computed" and item.computed_name:
            names.append(item.computed_name)
        elif item.kind == "group":
            names.extend(_declared(item.group_items))
        elif item.kind == "match" and item.match is not None:
            for arm in item.match.arms:
                if isinstance(arm.type_ref, ast.BlockType):
                    names.extend(_declared(arm.type_ref.items))
    return names


def _references(resolved: ast.ResolvedSchema, name: str) -> set[str]:
    """Named block encodings `name`'s body refers to."""
    enc = resolved.encodings.get(name)
    out: set[str] = set()
    if enc is None:
        return out

    def walk(t) -> None:
        if isinstance(t, ast.TypeRef):
            out.add(t.name)
        elif isinstance(t, ast.BlockType):
            for item in t.items:
                walk_item(item)
        elif isinstance(t, ast.MatchType):
            for arm in t.arms:
                walk(arm.type_ref)

    def walk_item(item: ast.BlockItem) -> None:
        if item.kind == "field" and item.wire is not None:
            walk(item.wire.type_ref)
        elif item.kind == "group":
            for sub in item.group_items:
                walk_item(sub)
        elif item.kind == "match" and item.match is not None:
            walk(item.match)
        elif item.kind == "spread" and item.spread_name:
            out.add(item.spread_name)

    walk(enc.body)
    return {n for n in out if n in resolved.encodings}


def recursive_encodings(resolved: ast.ResolvedSchema) -> set[str]:
    """Encodings that can reach themselves: they cannot be flattened into fields."""
    graph = {n: _references(resolved, n) for n in resolved.encodings}
    out: set[str] = set()
    for start in graph:
        stack = list(graph[start])
        seen: set[str] = set()
        while stack:
            cur = stack.pop()
            if cur == start:
                out.add(start)
                break
            if cur in seen:
                continue
            seen.add(cur)
            stack.extend(graph.get(cur, ()))
    return out


def element_encodings(resolved: ast.ResolvedSchema) -> set[str]:
    """Named encodings used inside a repeat element, directly or through others.

    A repeat element is not flattened into fields, so a payload there is one
    raw frame whose end only a walker can find.
    """
    found: set[str] = set()

    def in_element(t) -> None:
        if isinstance(t, ast.TypeRef):
            found.add(t.name)
        elif isinstance(t, ast.BlockType):
            for item in t.items:
                item_in_element(item)
        elif isinstance(t, ast.MatchType):
            for arm in t.arms:
                in_element(arm.type_ref)

    def item_in_element(item: ast.BlockItem) -> None:
        if item.kind == "field" and item.wire is not None:
            in_element(item.wire.type_ref)
        elif item.kind == "group":
            for sub in item.group_items:
                item_in_element(sub)
        elif item.kind == "match" and item.match is not None:
            in_element(item.match)

    def scan(items) -> None:
        for item in items:
            if item.kind == "field" and item.wire is not None:
                wf = item.wire
                if wf.repeat is not None:
                    in_element(wf.type_ref)
                if isinstance(wf.type_ref, ast.BlockType):
                    scan(wf.type_ref.items)
                elif isinstance(wf.type_ref, ast.MatchType):
                    for arm in wf.type_ref.arms:
                        if isinstance(arm.type_ref, ast.BlockType):
                            scan(arm.type_ref.items)
            elif item.kind == "group":
                scan(item.group_items)
            elif item.kind == "match" and item.match is not None:
                for arm in item.match.arms:
                    if isinstance(arm.type_ref, ast.BlockType):
                        scan(arm.type_ref.items)

    for pkt in resolved.packets.values():
        scan(pkt.items)
    for enc in resolved.encodings.values():
        if isinstance(enc.body, ast.BlockType):
            scan(enc.body.items)
    return {n for n in found if n in resolved.encodings}


@dataclass
class Walkers:
    """The compiled walkers: Rust source, encoding -> public fn, and what failed."""

    source: str = ""
    names: dict[str, str] = field(default_factory=dict)
    unsupported: dict[str, str] = field(default_factory=dict)


class _Compiler:
    def __init__(self, resolved: ast.ResolvedSchema, known: dict[str, str]) -> None:
        self.resolved = resolved
        self.known = known
        self.pending: list[str] = []
        self.done: dict[str, str] = {}

    def want(self, name: str) -> str:
        """The fn that skips `name`, queueing its compilation."""
        target = _alias_target(self.resolved, name)
        if target not in self.done and target not in self.pending:
            self.pending.append(target)
        return _skip_name(target)

    def compile(self, name: str) -> str:
        enc = self.resolved.encodings[name]
        if not isinstance(enc.body, ast.BlockType):
            raise Unsupported(f"{name}: not a block encoding")
        consts = {p.name: (p.default if p.default is not None else 0) for p in enc.params}
        referenced: set[str] = set()
        for item in enc.body.items:
            for e in _exprs_of(item):
                referenced |= _idents(e)
        body = _Body(self, name, consts, referenced)
        body.block(enc.body.items, 1)
        lines = [
            "#[allow(unused_assignments, unused_mut, unused_variables)]",
            f"fn {_skip_name(name)}(bytes: &[u8], pos: &mut usize, depth: u32) -> Option<()> {{",
            "    if depth > 256 {",
            "        return None;",
            "    }",
            *body.lines,
            "    Some(())",
            "}",
        ]
        return "\n".join(lines)


class _Body:
    """The statements of one skip fn, with the locals later expressions read."""

    def __init__(
        self, c: _Compiler, owner: str, consts: dict[str, int], referenced: set[str]
    ) -> None:
        self.c = c
        self.owner = owner
        self.consts = consts
        # Only the fields a later expression reads become numeric locals.
        self.referenced = referenced
        self.lines: list[str] = []
        self.scopes: list[set[str]] = []
        self.uid = 0

    # -- helpers ----------------------------------------------------------------

    def emit(self, indent: int, text: str) -> None:
        self.lines.append("    " * indent + text)

    def fail(self, why: str) -> Unsupported:
        return Unsupported(f"{self.owner}: {why}")

    def local(self, name: str) -> str:
        return f"v_{name}"

    def in_scope(self, name: str) -> bool:
        return any(name in s for s in self.scopes)

    # -- expressions -------------------------------------------------------------

    def expr(self, e: ast.Expr) -> tuple[str, str]:
        """(rust code, "int" | "bool")."""
        k = e.kind
        if k == "int":
            return f"{int(e.value)}i64", "int"
        if k == "bool":
            return ("true" if e.value else "false"), "bool"
        if k == "ident":
            if e.name in self.consts and not self.in_scope(e.name):
                return f"{int(self.consts[e.name])}i64", "int"
            if not self.in_scope(e.name):
                raise self.fail(f"expression reads {e.name!r}, which is not a value in scope")
            return self.local(e.name), "int"
        if k == "unop" and e.value == "!":
            return f"(!{self.as_bool(self.expr(e.left))})", "bool"
        if k == "binop":
            return self.binop(e)
        raise self.fail(f"unsupported expression kind {k!r}")

    def as_bool(self, pair: tuple[str, str]) -> str:
        code, kind = pair
        return code if kind == "bool" else f"({code} != 0)"

    def as_int(self, pair: tuple[str, str]) -> str:
        code, kind = pair
        return f"({code} as i64)" if kind == "bool" else code

    def binop(self, e: ast.Expr) -> tuple[str, str]:
        op = e.value
        left, right = self.expr(e.left), self.expr(e.right)
        if op in ("&&", "||"):
            return f"({self.as_bool(left)} {op} {self.as_bool(right)})", "bool"
        a, b = self.as_int(left), self.as_int(right)
        if op in ("==", "!=", "<", "<=", ">", ">="):
            return f"({a} {op} {b})", "bool"
        simple = {"&": "&", "|": "|", "^": "^"}
        if op in simple:
            return f"({a} {simple[op]} {b})", "int"
        wrapping = {"+": "wrapping_add", "-": "wrapping_sub", "*": "wrapping_mul"}
        if op in wrapping:
            return f"{a}.{wrapping[op]}({b})", "int"
        helper = {"/": "walk_div", "%": "walk_rem", "<<": "walk_shl", ">>": "walk_shr"}
        if op in helper:
            return f"{helper[op]}({a}, {b})", "int"
        raise self.fail(f"unsupported operator {op!r}")

    # -- structure ---------------------------------------------------------------

    def block(self, items: tuple[ast.BlockItem, ...], indent: int) -> None:
        names = _declared(items)
        scope: set[str] = set()
        self.scopes.append(scope)
        for name in names:
            if name in self.referenced and name not in scope:
                scope.add(name)
                self.emit(indent, f"let mut {self.local(name)}: i64 = 0;")
        for item in items:
            self.item(item, indent)
        self.scopes.pop()

    def item(self, item: ast.BlockItem, indent: int) -> None:
        if item.kind == "require":
            return
        if item.kind == "spread":
            raise self.fail(f"spread of {item.spread_name!r} is not supported")
        if item.kind == "computed":
            code = self.as_int(self.expr(item.computed_expr))
            self.emit(indent, f"{self.local(item.computed_name)} = {code};")
            return
        if item.kind == "group":
            self.emit(indent, f"if {_unparen(self.as_bool(self.expr(item.group_if)))} {{")
            self.group_items(item.group_items, indent + 1)
            self.emit(indent, "}")
            return
        if item.kind == "match" and item.match is not None:
            self.match(item.match, indent, None)
            return
        if item.kind == "field" and item.wire is not None:
            self.field(item.wire, indent)
            return
        raise self.fail(f"unsupported block item {item.kind!r}")

    def group_items(self, items: tuple[ast.BlockItem, ...], indent: int) -> None:
        """A conditional group shares its block's scope: its locals are already declared."""
        for item in items:
            self.item(item, indent)

    def field(self, wf: ast.WireField, indent: int) -> None:
        if wf.if_expr is not None:
            self.emit(indent, f"if {_unparen(self.as_bool(self.expr(wf.if_expr)))} {{")
            indent += 1
        if wf.repeat is not None:
            self.repeat(wf, indent)
        else:
            self.type(wf.type_ref, wf.name, indent)
        if wf.if_expr is not None:
            self.emit(indent - 1, "}")

    def repeat(self, wf: ast.WireField, indent: int) -> None:
        rep = wf.repeat
        if rep.mode != "count" or rep.expr is None:
            raise self.fail(f"repeat mode {rep.mode!r} is not supported")
        n = self.as_int(self.expr(rep.expr))
        self.uid += 1
        loop = f"n{self.uid}"
        self.emit(indent, f"let {loop} = walk_count({n}, bytes)?;")
        self.emit(indent, f"for _ in 0..{loop} {{")
        t = wf.type_ref
        if isinstance(t, ast.BlockType):
            self.block(t.items, indent + 1)
        else:
            self.type(t, None, indent + 1)
        self.emit(indent, "}")

    def match(self, mt: ast.MatchType, indent: int, bind: str | None) -> None:
        tag = self.as_int(self.expr(mt.tag_expr))
        self.emit(indent, f"match {_unparen(tag)} {{")
        default = None
        for arm in mt.arms:
            tags = arm.tags if arm.tags else (() if arm.tag is None else (arm.tag,))
            if not tags:
                default = arm
                continue
            if any(not isinstance(t, int) for t in tags):
                raise self.fail("match arm tags must be integers")
            pattern = " | ".join(f"{t}" for t in tags)
            self.emit(indent + 1, f"{pattern} => {{")
            self.arm(arm, indent + 2, bind)
            self.emit(indent + 1, "}")
        if default is not None:
            self.emit(indent + 1, "_ => {")
            self.arm(default, indent + 2, bind)
            self.emit(indent + 1, "}")
        else:
            self.emit(indent + 1, "_ => return None,")
        self.emit(indent, "}")

    def arm(self, arm: ast.MatchArm, indent: int, bind: str | None) -> None:
        t = arm.type_ref
        if t is None:
            return
        if isinstance(t, ast.BlockType):
            # An arm block shares the enclosing scope, so its locals are declared there.
            for item in t.items:
                self.item(item, indent)
        else:
            self.type(t, bind, indent)

    def type(self, t, bind: str | None, indent: int) -> None:
        """Skip one value of type `t`; keep it in the local `bind` when one is named."""
        if isinstance(t, ast.BlockType):
            for item in t.items:
                self.item(item, indent)
            return
        if isinstance(t, ast.MatchType):
            self.match(t, indent, bind)
            return
        name = t.name
        wanted = bind is not None and self.in_scope(bind)
        dest = f"{self.local(bind)} = " if wanted else ""
        if name == "rest":
            self.emit(indent, "*pos = bytes.len();")
        elif name == "bytes":
            if not t.args:
                raise self.fail("bytes() without a length")
            n = self.as_int(self.expr(t.args[0]))
            self.emit(indent, f"walk_skip(bytes, pos, walk_count({n}, bytes)?)?;")
        elif name in _FIXED:
            size = _FIXED[name]
            if wanted:
                signed = "true" if name in _SIGNED else "false"
                self.emit(indent, f"{dest}walk_be(bytes, pos, {size}, {signed})?;")
            else:
                self.emit(indent, f"walk_skip(bytes, pos, {size})?;")
        elif name in _VAR:
            self.emit(indent, f"{dest}walk_var(bytes, pos, {_VAR[name]})?;")
        elif name in _STRINGS:
            self.emit(indent, "let len = walk_var(bytes, pos, 5)?;")
            self.emit(indent, "walk_skip(bytes, pos, walk_count(len, bytes)?)?;")
        elif name == "nbt_nothing":
            pass
        elif name in self.c.known:
            self.emit(indent, f"walk_hand(bytes, pos, {self.c.known[name]})?;")
        elif name in self.c.resolved.encodings:
            if t.args:
                enc = self.c.resolved.encodings[_alias_target(self.c.resolved, name)]
                if enc.params:
                    raise self.fail(f"{name} is used with arguments, which is not supported")
            if wanted:
                raise self.fail(f"{name} is read as a number but is a composite")
            self.emit(indent, f"{self.c.want(name)}(bytes, pos, depth + 1)?;")
        else:
            raise self.fail(f"unknown type {name!r}")


_PRELUDE = '''
fn walk_skip(bytes: &[u8], pos: &mut usize, n: usize) -> Option<()> {
    let end = pos.checked_add(n)?;
    if end > bytes.len() {
        return None;
    }
    *pos = end;
    Some(())
}

/// A count read from the wire can only be honest if that many bytes remain.
fn walk_count(n: i64, bytes: &[u8]) -> Option<usize> {
    usize::try_from(n).ok().filter(|n| *n <= bytes.len())
}

fn walk_be(bytes: &[u8], pos: &mut usize, n: usize, signed: bool) -> Option<i64> {
    let end = pos.checked_add(n)?;
    let chunk = bytes.get(*pos..end)?;
    let mut value: i64 = 0;
    for byte in chunk {
        value = (value << 8) | i64::from(*byte);
    }
    if signed && n < 8 && chunk[0] & 0x80 != 0 {
        value -= 1i64 << (8 * n);
    }
    *pos = end;
    Some(value)
}

/// A LEB128 integer of at most `max` bytes; a 5-byte one is a signed 32-bit VarInt.
fn walk_var(bytes: &[u8], pos: &mut usize, max: usize) -> Option<i64> {
    let mut acc: u64 = 0;
    for i in 0..max {
        let byte = *bytes.get(*pos + i)?;
        acc |= u64::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            *pos += i + 1;
            return Some(if max == 5 { i64::from(acc as u32 as i32) } else { acc as i64 });
        }
    }
    None
}

/// A payload another walker already measures.
fn walk_hand(bytes: &[u8], pos: &mut usize, len: fn(&[u8]) -> usize) -> Option<()> {
    let rest = bytes.get(*pos..)?;
    if rest.is_empty() {
        return None;
    }
    *pos += len(rest);
    Some(())
}

fn walk_div(a: i64, b: i64) -> i64 {
    if b == 0 { 0 } else { a.div_euclid(b) }
}

fn walk_rem(a: i64, b: i64) -> i64 {
    if b == 0 { 0 } else { a.rem_euclid(b) }
}

fn walk_shl(a: i64, b: i64) -> i64 {
    u32::try_from(b).ok().and_then(|b| a.checked_shl(b)).unwrap_or(0)
}

fn walk_shr(a: i64, b: i64) -> i64 {
    u32::try_from(b).ok().and_then(|b| a.checked_shr(b)).unwrap_or(0)
}
'''


def generate(
    resolved: ast.ResolvedSchema, roots: list[str], known: dict[str, str]
) -> Walkers:
    """Compile walkers for `roots` and everything they reach.

    `known` maps encodings that already have a hand-written walker (NBT, slots)
    to its Rust fn; they are called, not recompiled.
    """
    comp = _Compiler(resolved, known)
    result = Walkers()
    bodies: dict[str, str] = {}
    for root in roots:
        comp.want(root)
    while comp.pending:
        name = comp.pending.pop(0)
        try:
            bodies[name] = comp.compile(name)
        except Unsupported as err:
            result.unsupported[name] = str(err)
        comp.done[name] = name
    broken = set(result.unsupported)
    # A walker that calls a broken one is itself broken.
    ok = sorted(n for n in bodies if n not in broken and not (_closure(resolved, n, known) & broken))
    for root in roots:
        target = _alias_target(resolved, root)
        if target in ok:
            result.names[root] = walker_name(target)
        else:
            result.unsupported.setdefault(root, f"{root}: reaches an encoding without a walker")
    publics = [
        f"/// Length of one `{n}` payload at the front of `bytes`.\n"
        "#[must_use]\n"
        f"pub fn {walker_name(n)}(bytes: &[u8]) -> usize {{\n"
        "    let mut pos = 0usize;\n"
        f"    if {_skip_name(n)}(bytes, &mut pos, 0).is_none() {{\n"
        "        return bytes.len();\n"
        "    }\n"
        "    pos.min(bytes.len())\n"
        "}"
        for n in ok
    ]
    if ok:
        text = "\n\n".join(bodies[n] for n in ok)
        helpers = "\n".join(h for name, h in _helpers() if f"{name}(" in text)
        result.source = "\n" + helpers + "\n" + text + "\n\n" + "\n\n".join(publics) + "\n"
    return result


def _closure(resolved: ast.ResolvedSchema, name: str, known: dict[str, str]) -> set[str]:
    """Encodings reachable from `name`'s body that need their own walker."""
    out: set[str] = set()
    stack = [name]
    while stack:
        cur = stack.pop()
        for ref in _references(resolved, cur):
            target = _alias_target(resolved, ref)
            if target in known or target in _FIXED or target in _VAR or target in _STRINGS:
                continue
            if target not in out and target != name:
                out.add(target)
                stack.append(target)
    return out


def _unparen(code: str) -> str:
    """Drop one pair of parentheses that wraps all of `code` (Rust warns about them)."""
    if not (code.startswith("(") and code.endswith(")")):
        return code
    depth = 0
    for i, ch in enumerate(code):
        depth += ch == "("
        depth -= ch == ")"
        if depth == 0 and i < len(code) - 1:
            return code
    return code[1:-1]


def _helpers() -> list[tuple[str, str]]:
    """The prelude's helper fns, by name: a generated crate keeps only those it calls."""
    return [
        (m.group(2), m.group(1))
        for m in re.finditer(r"((?:///[^\n]*\n)*fn (\w+)\(.*?\n\}\n)", _PRELUDE, re.S)
    ]
