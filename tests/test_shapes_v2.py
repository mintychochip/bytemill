"""Shape v2: model attributes, extras and tests for non-packet items, coordinate Vec, imports."""

from __future__ import annotations

import dataclasses
import tempfile
import unittest
from pathlib import Path

from bps.shapes import ShapeError, extract_shape, shape_from_json, shape_to_json
from bps.shapes.model import FieldShape, NewShape, SerializerShape, Shape

ROOT = Path(__file__).resolve().parents[1]
LEDGER = ROOT / "targets" / "rstom" / "shapes"
PACKET_PATH = "server/play/probe_packet"

_STRUCT = """\
#[derive(Clone, Debug)]
pub struct ProbePacket {
    pub name: String,
}
"""

_SERIALIZER = """\
struct ProbePacketSerializer;

impl PacketSerializer for ProbePacketSerializer {
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {
        let packet = downcast::<ProbePacket>(value);
        buffer.write(STRING, &packet.name);
    }

    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {
        Box::new(ProbePacket {
            name: buffer.read(STRING),
        })
    }
}
"""

_HEADER = """\
//! Port.

use std::sync::Arc;

use crate::network::buffer::NetworkBuffer;
use crate::network::packet::packet::Packet;
use crate::network::packet::serializer::{PacketSerializer, PacketValue, downcast};
use crate::network::types::STRING;
"""

MODE_ITEMS = """\
/// A mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    On,
    Off,
}

impl Mode {
    pub fn id(&self) -> u8 {
        match self {
            Mode::On => 1,
            Mode::Off => 0,
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{{}}")
    }
}

impl PartialEq for ProbePacket {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

pub const MAX: usize = 4;

pub fn helper() -> usize {
    MAX
}
"""

TESTS_MODULE = """\
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_is_max() {
        assert_eq!(helper(), MAX);
    }
}
"""

GENERATED_TAIL = """\

mod ext;
pub use ext::*;

#[cfg(test)]
mod tests;
"""


def _packet(
    *,
    struct_decl: str = _STRUCT,
    head: str = "",
    new_sig: str = "name: String",
    new_body: str = "Self { name }",
    items: str = "",
    tail: str = "",
) -> str:
    parts = [
        _HEADER,
        head,
        struct_decl,
        "impl ProbePacket {",
        "    #[must_use]",
        f"    pub fn new({new_sig}) -> Self {{",
        f"        {new_body}",
        "    }",
        "",
        "    #[must_use]",
        "    pub fn serializer() -> Arc<dyn PacketSerializer> {",
        "        Arc::new(ProbePacketSerializer)",
        "    }",
        "}",
        "",
        "impl Packet for ProbePacket {}",
        "",
        _SERIALIZER,
        items,
        tail,
    ]
    return "\n".join(parts)


def _extract(source: str, packet_path: Path | None = None) -> Shape:
    return extract_shape(source, "ProbePacket", PACKET_PATH, packet_path=packet_path)


def _base_shape() -> Shape:
    return Shape(
        cls="ProbePacket",
        module=PACKET_PATH,
        module_docs=(),
        struct_docs=(),
        derives=("Clone",),
        form="braced",
        fields=(FieldShape(name="id", type="i32", docs=()),),
        new=NewShape(present=True, const=False, must_use=False, attrs=(), docs=()),
        serializer_fn_docs=(),
        serializer_fn_must_use=False,
        serializer=SerializerShape(style="struct", name="ProbePacketSerializer", public=False, docs=()),
        extras=None,
    )


class LedgerRoundTripTest(unittest.TestCase):
    def test_every_committed_ledger_file_reserialises_byte_for_byte(self) -> None:
        files = sorted(LEDGER.rglob("*.json"))
        self.assertEqual(len(files), 64)
        for path in files:
            text = path.read_text(encoding="utf-8")
            with self.subTest(path=path.name):
                self.assertEqual(shape_to_json(shape_from_json(text)), text)


class ShapeV2ModelTest(unittest.TestCase):
    def test_v2_attributes_round_trip_through_json(self) -> None:
        from bps.shapes.model import Accepted

        shape = dataclasses.replace(
            _base_shape(),
            fields=(FieldShape(name="id", type="i32", docs=(), wire="ident"),),
            new=NewShape(
                present=True,
                const=False,
                must_use=False,
                attrs=(),
                docs=(),
                params="id: i32, label: impl Into<String>",
            ),
            imports=("use uuid::Uuid;",),
            tests="probe_packet.tests.rs",
            hooks={"write.start": ("let x = 1;",), "read.after:ident": ("let y = 2;",)},
            accepted=(Accepted(construct="pub enum Mode", reason="kept in extras"),),
            drops=("use crate::old::Thing;",),
        )
        self.assertEqual(shape_from_json(shape_to_json(shape)), shape)


class OtherItemsTest(unittest.TestCase):
    def test_non_packet_items_become_extras_and_inline_tests_are_recorded(self) -> None:
        source = _packet(items=MODE_ITEMS, tail="\n" + TESTS_MODULE)
        shape = _extract(source)
        self.assertEqual(shape.extras, "probe_packet.ext.rs")
        self.assertEqual(shape.tests, "probe_packet.tests.rs")
        self.assertEqual([f.name for f in shape.fields], ["name"])
        self.assertEqual(shape.serializer.name, "ProbePacketSerializer")

    def test_inline_tests_alone_do_not_make_extras(self) -> None:
        shape = _extract(_packet(tail="\n" + TESTS_MODULE))
        self.assertIsNone(shape.extras)
        self.assertEqual(shape.tests, "probe_packet.tests.rs")


class GeneratedTailTest(unittest.TestCase):
    def test_generated_tail_is_not_an_extra_until_a_sibling_ext_file_exists(self) -> None:
        source = _packet(tail=GENERATED_TAIL)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "probe_packet.rs"
            path.write_text(source, encoding="utf-8")
            shape = _extract(source, path)
            self.assertIsNone(shape.extras)
            self.assertIsNone(shape.tests)
            (Path(tmp) / "probe_packet.ext.rs").write_text("", encoding="utf-8")
            self.assertEqual(_extract(source, path).extras, "probe_packet.ext.rs")


class NewParamsTest(unittest.TestCase):
    def test_params_are_recorded_verbatim_when_they_differ_from_fields(self) -> None:
        source = _packet(
            struct_decl="#[derive(Clone, Debug)]\npub struct ProbePacket {\n"
            "    pub position: i8,\n    pub score_name: String,\n}\n",
            new_sig="position: i8, score_name: impl Into<String>",
            new_body="let score_name = score_name.into();\n        Self { position, score_name }",
        )
        self.assertEqual(
            _extract(source).new.params,
            "position: i8, score_name: impl Into<String>",
        )

    def test_params_are_none_when_they_match_the_fields(self) -> None:
        self.assertIsNone(_extract(_packet()).new.params)


class CoordinateVecTest(unittest.TestCase):
    def test_bare_vec_from_coordinate_becomes_coord_vec_and_std_vec_loses_its_path(self) -> None:
        source = _packet(
            head="use crate::coordinate::Vec;\n",
            struct_decl="#[derive(Clone, Debug)]\npub struct ProbePacket {\n"
            "    pub location: Vec,\n    pub history: std::vec::Vec<String>,\n}\n",
            new_sig="location: Vec, history: std::vec::Vec<String>",
            new_body="Self { location, history }",
        )
        shape = _extract(source)
        self.assertEqual(
            [(f.name, f.type) for f in shape.fields],
            [("location", "CoordVec"), ("history", "Vec<String>")],
        )
        self.assertEqual(shape.imports, ("use crate::coordinate::Vec as CoordVec;",))

    def test_params_use_the_same_normalised_types(self) -> None:
        source = _packet(
            head="use crate::coordinate::Vec;\n",
            struct_decl="#[derive(Clone, Debug)]\npub struct ProbePacket {\n"
            "    pub location: Vec,\n    pub history: Vec<String>,\n    pub label: String,\n}\n",
            new_sig="location: Vec, history: std::vec::Vec<String>, label: impl Into<String>",
            new_body="Self { location, history, label: label.into() }",
        )
        self.assertEqual(
            _extract(source).new.params,
            "location: CoordVec, history: Vec<String>, label: impl Into<String>",
        )

    def test_bare_vec_without_coordinate_import_is_unchanged(self) -> None:
        source = _packet(
            struct_decl="#[derive(Clone, Debug)]\npub struct ProbePacket {\n    pub name: Vec<u8>,\n}\n",
            new_sig="name: Vec<u8>",
        )
        self.assertEqual(_extract(source).fields[0].type, "Vec<u8>")


class ImportsTest(unittest.TestCase):
    def test_imports_keep_only_names_used_by_the_hand_packet(self) -> None:
        source = _packet(
            head="use crate::network::types::BOOLEAN;\n"
            "use crate::coordinate::{Point, Pos};\n"
            "use crate::text::Component;\n"
            "use uuid::Uuid;\n",
            struct_decl="#[derive(Clone, Debug)]\npub struct ProbePacket {\n"
            "    pub id: Uuid,\n    pub text: Component,\n}\n",
            new_sig="id: Uuid, text: Component",
            new_body="Self { id, text }",
        )
        self.assertEqual(
            _extract(source).imports,
            ("use crate::text::Component;", "use uuid::Uuid;"),
        )


class UnrepresentableTest(unittest.TestCase):
    def test_generics_lifetimes_and_tuple_structs_are_refused(self) -> None:
        cases = {
            "generic": "#[derive(Clone, Debug)]\npub struct ProbePacket<T> {\n    pub name: T,\n}\n",
            "lifetime": "#[derive(Clone, Debug)]\npub struct ProbePacket<'a> {\n    pub name: &'a str,\n}\n",
            "tuple": "#[derive(Clone, Debug)]\npub struct ProbePacket(pub String);\n",
        }
        for label, struct_decl in cases.items():
            with self.subTest(label=label), self.assertRaises(ShapeError):
                _extract(_packet(struct_decl=struct_decl))


if __name__ == "__main__":
    unittest.main()
