"""`bps.shapes` is a package; every name imported from it by the tree still resolves."""

import importlib
import unittest

import bps.shapes

IMPORTED_NAMES = (
    "FieldShape",
    "NewShape",
    "SerializerShape",
    "Shape",
    "ShapeError",
    "_advance_item_start",
    "_docs_and_attrs_before",
    "bps_field_pairs",
    "extract_shape",
    "extract_shapes_for_units",
    "hand_packet_path",
    "load_ledger",
    "mismatch",
    "module_path",
    "packet_report",
    "shape_from_json",
    "shape_to_json",
)

SUBMODULES = ("model", "scan", "extract", "ledger")


class ShapesPackageTest(unittest.TestCase):
    def test_submodules_import(self):
        for name in SUBMODULES:
            with self.subTest(module=name):
                importlib.import_module(f"bps.shapes.{name}")

    def test_every_imported_name_is_reexported(self):
        for name in IMPORTED_NAMES:
            with self.subTest(name=name):
                self.assertTrue(hasattr(bps.shapes, name))


if __name__ == "__main__":
    unittest.main()
