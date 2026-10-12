"""Shape model: the hand-struct description, its JSON form and the error type."""

from __future__ import annotations

import json
from dataclasses import dataclass, field


class ShapeError(Exception):
    """The hand file has a construct the extractor cannot represent."""


@dataclass(frozen=True)
class FieldShape:
    name: str
    type: str
    docs: tuple[str, ...]
    wire: str | None = None


@dataclass(frozen=True)
class NewShape:
    present: bool
    const: bool
    must_use: bool
    attrs: tuple[str, ...]
    docs: tuple[str, ...]
    body: tuple[str, ...] = ()
    params: str | None = None


@dataclass(frozen=True)
class SerializerShape:
    style: str
    name: str
    public: bool
    docs: tuple[str, ...]
    read_via_new: bool = False


@dataclass(frozen=True)
class Accepted:
    """A hand item the shape keeps verbatim instead of generating it."""

    construct: str
    reason: str


@dataclass(frozen=True)
class Shape:
    cls: str
    module: str
    module_docs: tuple[str, ...]
    struct_docs: tuple[str, ...]
    derives: tuple[str, ...]
    form: str
    fields: tuple[FieldShape, ...]
    new: NewShape
    serializer_fn_docs: tuple[str, ...]
    serializer_fn_must_use: bool
    serializer: SerializerShape
    extras: str | None
    imports: tuple[str, ...] = ()
    tests: str | None = None
    hooks: dict[str, tuple[str, ...]] = field(default_factory=dict)
    accepted: tuple[Accepted, ...] = ()
    drops: tuple[str, ...] = ()


def _field_json(f: FieldShape) -> dict[str, object]:
    obj: dict[str, object] = {"name": f.name, "type": f.type, "docs": list(f.docs)}
    if f.wire is not None:
        obj["wire"] = f.wire
    return obj


def shape_to_json(s: Shape) -> str:
    obj = {
        "class": s.cls,
        "module": s.module,
        "module_docs": list(s.module_docs),
        "struct": {
            "docs": list(s.struct_docs),
            "derives": list(s.derives),
            "form": s.form,
        },
        "fields": [_field_json(f) for f in s.fields],
        "new": {
            "present": s.new.present,
            "must_use": s.new.must_use,
            "const": s.new.const,
            "attrs": list(s.new.attrs),
            "docs": list(s.new.docs),
            "body": list(s.new.body),
        },
        "serializer_fn": {
            "docs": list(s.serializer_fn_docs),
            "must_use": s.serializer_fn_must_use,
        },
        "serializer_struct": {
            "style": s.serializer.style,
            "name": s.serializer.name,
            "pub": s.serializer.public,
            "docs": list(s.serializer.docs),
            "read_via_new": s.serializer.read_via_new,
        },
        "extras": s.extras,
    }
    if s.new.params is not None:
        obj["new"]["params"] = s.new.params
    if s.imports:
        obj["imports"] = list(s.imports)
    if s.tests is not None:
        obj["tests"] = s.tests
    if s.hooks:
        obj["hooks"] = {k: list(v) for k, v in s.hooks.items()}
    if s.accepted:
        obj["accepted"] = [{"construct": a.construct, "reason": a.reason} for a in s.accepted]
    if s.drops:
        obj["drops"] = list(s.drops)
    return json.dumps(obj, indent=2) + "\n"


def shape_from_json(text: str) -> Shape:
    obj = json.loads(text)
    return Shape(
        cls=obj["class"],
        module=obj["module"],
        module_docs=tuple(obj["module_docs"]),
        struct_docs=tuple(obj["struct"]["docs"]),
        derives=tuple(obj["struct"]["derives"]),
        form=obj["struct"]["form"],
        fields=tuple(
            FieldShape(
                name=f["name"], type=f["type"], docs=tuple(f["docs"]), wire=f.get("wire")
            )
            for f in obj["fields"]
        ),
        new=NewShape(
            present=obj["new"]["present"],
            const=obj["new"]["const"],
            must_use=obj["new"]["must_use"],
            attrs=tuple(obj["new"]["attrs"]),
            docs=tuple(obj["new"]["docs"]),
            body=tuple(obj["new"].get("body", ())),
            params=obj["new"].get("params"),
        ),
        serializer_fn_docs=tuple(obj["serializer_fn"]["docs"]),
        serializer_fn_must_use=obj["serializer_fn"]["must_use"],
        serializer=SerializerShape(
            style=obj["serializer_struct"]["style"],
            name=obj["serializer_struct"]["name"],
            public=obj["serializer_struct"]["pub"],
            docs=tuple(obj["serializer_struct"]["docs"]),
            read_via_new=bool(obj["serializer_struct"].get("read_via_new", False)),
        ),
        extras=obj["extras"],
        imports=tuple(obj.get("imports", ())),
        tests=obj.get("tests"),
        hooks={k: tuple(v) for k, v in obj.get("hooks", {}).items()},
        accepted=tuple(Accepted(a["construct"], a["reason"]) for a in obj.get("accepted", ())),
        drops=tuple(obj.get("drops", ())),
    )
