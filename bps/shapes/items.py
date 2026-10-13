"""Column-0 item scanner for hand-written rstom packet files.
An item starts at its first doc or attribute line (or at its code line when it has none) and
ends at the depth-0 `;` or the `}` that closes its outermost brace. Strings, chars, raw strings
and comments are skipped, so braces inside them never change the depth.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from bps.shapes.scan import _attr_block_end, _skip_block_comment, _skip_rust_string

_TOP_KINDS: tuple[tuple[str, str, bool], ...] = (
    (r"use\b", "use", False),
    (r"(?:const\s+|unsafe\s+|async\s+)*fn\s+(\w+)", "fn", True),
    (r"struct\s+(\w+)", "struct", True),
    (r"enum\s+(\w+)", "enum", True),
    (r"union\s+(\w+)", "union", True),
    (r"trait\s+(\w+)", "trait", True),
    (r"mod\s+(\w+)", "mod", True),
    (r"static\s+(?:mut\s+)?(\w+)", "static", True),
    (r"const\s+(\w+)", "const", True),
    (r"type\s+(\w+)", "type", True),
    (r"macro_rules!\s*(\w+)", "macro", True),
    (r"(?:unsafe\s+)?impl\b", "impl", False),
)

_MEMBER_KINDS: tuple[tuple[str, str], ...] = (
    (r"(?:const\s+|unsafe\s+|async\s+)*fn\s+(\w+)", "fn"),
    (r"const\s+(\w+)", "const"),
    (r"type\s+(\w+)", "type"),
)

_VIS = re.compile(r"pub(?:\([^)]*\))?\s+")
_CFG_TEST = re.compile(r"^#\[cfg\(test\)\]\s*$", re.MULTILINE)


@dataclass(frozen=True)
class Item:
    kind: str
    name: str | None
    trait: str | None
    start: int
    end: int
    text: str
    cfg_test: bool


@dataclass(frozen=True)
class Member:
    kind: str
    name: str | None
    start: int
    end: int
    text: str


def scan_items(src: str) -> list[Item]:
    items: list[Item] = []
    for start, code, end in _entries(src, 0, len(src), ""):
        text = src[start:end]
        kind, name, trait = _classify(src[code:end])
        cfg_test = bool(_CFG_TEST.search(src[start:code]))
        items.append(Item(kind, name, trait, start, end, text, cfg_test))
    return items


def impl_members(item: Item) -> list[Member]:
    if item.kind != "impl":
        return []
    src = item.text
    body_start, body_end = impl_body_bounds(src)
    members: list[Member] = []
    for start, code, end in _entries(src, body_start, body_end, "    "):
        kind, name = _classify_member(src[code:end])
        members.append(
            Member(kind, name, item.start + start, item.start + end, src[start:end])
        )
    return members


def impl_body_bounds(text: str) -> tuple[int, int]:
    """Offsets of the impl body inside an impl item's text (between its braces)."""
    header = re.search(r"^(?:unsafe\s+)?impl\b", text, re.MULTILINE)
    return text.index("{", header.start()) + 1, text.rindex("}")


def _entries(src: str, pos: int, limit: int, indent: str) -> list[tuple[int, int, int]]:
    """(first-doc-or-code offset, code offset, end offset) for each item in ``src[pos:limit]``."""
    out: list[tuple[int, int, int]] = []
    pending: int | None = None
    while pos < limit:
        eol = src.find("\n", pos, limit)
        eol = limit if eol < 0 else eol
        line = src[pos:eol]
        stripped = line[len(indent) :] if line.startswith(indent) else line
        first = pos + (len(indent) if line.startswith(indent) else 0)
        if not stripped.strip() or (stripped.startswith("//") and not stripped.startswith("///")):
            pos = eol + 1
            continue
        if stripped.startswith("///"):
            pending = first if pending is None else pending
            pos = eol + 1
            continue
        if stripped.startswith("/*"):
            if stripped.startswith("/**") and not stripped.startswith("/**/"):
                pending = first if pending is None else pending
            pos = _skip_block_comment(src, first)
            while pos < limit and src[pos] in " \t":
                pos += 1
            continue
        if stripped.startswith("#["):
            pending = first if pending is None else pending
            pos = _next_line(src, _attr_block_end(src, first), limit)
            continue
        start = pending if pending is not None else first
        pending = None
        end = _item_end(src, first)
        out.append((start, first, end))
        pos = _next_line(src, end, limit)
    return out


def _next_line(src: str, pos: int, limit: int) -> int:
    nl = src.find("\n", pos)
    return limit if nl < 0 else min(nl + 1, limit)


def _item_end(src: str, pos: int) -> int:
    depth = 0
    i = pos
    n = len(src)
    while i < n:
        ch = src[i]
        if ch in "\"'":
            i = _skip_rust_string(src, i)
            continue
        if src.startswith("//", i):
            nl = src.find("\n", i)
            i = n if nl < 0 else nl
            continue
        if src.startswith("/*", i):
            i = _skip_block_comment(src, i)
            continue
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
            if depth == 0 and ch == "}":
                j = i + 1
                while j < n and src[j] in " \t":
                    j += 1
                return j + 1 if j < n and src[j] == ";" else i + 1
        elif ch == ";" and depth == 0:
            return i + 1
        i += 1
    return n


def _classify(header_text: str) -> tuple[str, str | None, str | None]:
    head = _VIS.sub("", header_text, count=1)
    for pattern, kind, named in _TOP_KINDS:
        m = re.match(pattern, head)
        if m is None:
            continue
        if kind == "impl":
            trait, target = _impl_names(head)
            return kind, target, trait
        return kind, (m.group(1) if named else None), None
    return "other", None, None


def _classify_member(header_text: str) -> tuple[str, str | None]:
    head = _VIS.sub("", header_text, count=1)
    for pattern, kind in _MEMBER_KINDS:
        m = re.match(pattern, head)
        if m is not None:
            return kind, m.group(1)
    return "other", None


def _impl_names(head: str) -> tuple[str | None, str]:
    text = re.sub(r"^(?:unsafe\s+)?impl\b", "", head, count=1).lstrip()
    if text.startswith("<"):
        text = text[_angle_end(text) :].lstrip()
    brace = text.find("{")
    if brace >= 0:
        text = text[:brace]
    text = text.split(" where ", 1)[0].strip()
    if " for " in text:
        trait_part, target_part = text.split(" for ", 1)
        trait: str | None = _without_generics(trait_part).rsplit("::", 1)[-1]
    else:
        trait = None
        target_part = text
    target = _without_generics(target_part).rsplit("::", 1)[-1]
    return trait, target


def _angle_end(text: str) -> int:
    depth = 0
    for i, ch in enumerate(text):
        if ch == "<":
            depth += 1
        elif ch == ">" and text[i - 1] != "-":
            depth -= 1
            if depth == 0:
                return i + 1
    return len(text)


def _without_generics(text: str) -> str:
    return text.split("<", 1)[0].strip()
