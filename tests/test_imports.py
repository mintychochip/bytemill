"""File-scoped names: a file sees its own declarations and the files it imports.

Nothing is global. A name declared in another file is visible only when this
file imports that file, and an import does not pass its own imports along (the
`.proto` rule), so every use of `varint` traces to an `import` line in the same
file.
"""

import importlib.util
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.errors import ParseError, ResolveError
from bps.interp.engine import decode_packet
from bps.load import load_schema
from bps.parser import parse

ROOT = Path(__file__).resolve().parents[1]


class _TempTree(unittest.TestCase):
    """A throwaway directory of `.bps` files."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)

    def write(self, name: str, text: str) -> Path:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def load_error(self, *paths: Path) -> str:
        with self.assertRaises(ResolveError) as ctx:
            load_schema(list(paths))
        return str(ctx.exception)


class ImportVisibilityTest(_TempTree):
    def test_a_name_declared_in_another_file_needs_an_import(self) -> None:
        lib = self.write("lib.bps", "encoding ident = { value: u8 }\n")
        main = self.write("main.bps", "packet probe { x: ident }\n")
        message = self.load_error(lib, main)
        self.assertIn("main.bps", message)
        self.assertIn("ident", message)
        self.assertIn('import "lib.bps"', message)

    def test_importing_the_file_makes_its_names_visible(self) -> None:
        self.write("lib.bps", "encoding ident = { value: u8 }\n")
        main = self.write("main.bps", 'import "lib.bps"\npacket probe { x: ident }\n')
        resolved = analyze(load_schema([main]))
        self.assertEqual(decode_packet(resolved, "probe", b"\x07"), {"x": {"value": 7}})

    def test_imports_are_not_transitive(self) -> None:
        self.write("c.bps", "encoding thing = { n: u8 }\n")
        self.write("b.bps", 'import "c.bps"\nencoding wrapped = { t: thing }\n')
        a = self.write(
            "a.bps", 'import "b.bps"\npacket probe { w: wrapped  t: thing }\n'
        )
        message = self.load_error(a)
        self.assertIn('import "c.bps"', message)
        # `wrapped` comes from the file `a.bps` does import.
        self.assertNotIn("wrapped", message)

    def test_the_import_to_add_is_relative_to_the_importing_file(self) -> None:
        lib = self.write("sub/lib.bps", "encoding ident = { value: u8 }\n")
        user = self.write("other/deep/user.bps", "packet probe { x: ident }\n")
        message = self.load_error(lib, user)
        self.assertIn('import "../../sub/lib.bps"', message)

    def test_every_missing_import_is_reported_at_once(self) -> None:
        one = self.write("one.bps", "encoding aa = { n: u8 }\n")
        two = self.write("two.bps", "encoding bb = { n: u8 }\n")
        user = self.write("user.bps", "packet probe { a: aa  b: bb }\n")
        message = self.load_error(one, two, user)
        self.assertIn('import "one.bps"', message)
        self.assertIn('import "two.bps"', message)

    def test_a_name_declared_nowhere_is_not_blamed_on_a_missing_import(self) -> None:
        main = self.write("main.bps", "packet probe { x: ghost }\n")
        message = self.load_error(main)
        self.assertIn("ghost", message)
        self.assertNotIn('import "', message)

    def test_a_nearby_file_that_nothing_loaded_is_still_suggested(self) -> None:
        self.write("std/num.bps", "encoding num = u8\n")
        main = self.write("main.bps", "packet probe { x: num }\n")
        message = self.load_error(main)
        self.assertIn('import "std/num.bps"', message)

    def test_an_unreadable_neighbor_does_not_hide_the_real_error(self) -> None:
        self.write("broken.bps", "this is not a schema\n")
        main = self.write("main.bps", "packet probe { x: ghost }\n")
        self.assertIn("ghost", self.load_error(main))

    def test_a_route_needs_the_packet_file_imported(self) -> None:
        packets = self.write("packets.bps", "packet ping { }\n")
        routes = self.write(
            "routes.bps",
            "protocol p @ v1 {\n    id = 1\n    route play.clientbound 0x00 => ping\n}\n",
        )
        self.assertIn('import "packets.bps"', self.load_error(packets, routes))
        routes.write_text('import "packets.bps"\n' + routes.read_text(encoding="utf-8"))
        resolved = analyze(load_schema([routes]))
        self.assertEqual([pkt.name for _, pkt in resolved.routes], ["ping"])

    def test_a_frame_field_needs_its_type_imported(self) -> None:
        lib = self.write("lib.bps", "encoding num = u8\n")
        proto = self.write(
            "proto.bps",
            "protocol p @ v1 {\n    id = 1\n    frame default {\n"
            "        length: num\n        payload: rest\n    }\n}\n",
        )
        self.assertIn('import "lib.bps"', self.load_error(lib, proto))

    def test_an_enum_base_needs_its_type_imported(self) -> None:
        lib = self.write("lib.bps", "encoding num = u8\n")
        enum = self.write("kinds.bps", "enum kind : num {\n    a = 1\n}\n")
        self.assertIn('import "lib.bps"', self.load_error(lib, enum))

    def test_a_spread_needs_its_encoding_imported(self) -> None:
        lib = self.write("lib.bps", "encoding ident = { value: u8 }\n")
        user = self.write("user.bps", "encoding wrapper = {\n    ...ident\n}\n")
        self.assertIn('import "lib.bps"', self.load_error(lib, user))

    def test_a_missing_import_target_names_the_file_that_asked_for_it(self) -> None:
        main = self.write("main.bps", 'import "nope.bps"\n')
        with self.assertRaises(ParseError) as ctx:
            load_schema([main])
        self.assertIn("nope.bps", str(ctx.exception))
        self.assertIn("main.bps", str(ctx.exception))


class AnalyzeResolvesEveryDeclarationTest(unittest.TestCase):
    def test_unknown_types_are_rejected_wherever_they_appear(self) -> None:
        sources = {
            "packet field": "packet probe { x: ghost }",
            "enum base": "enum kind : ghost { a = 1 }",
            "frame field": "protocol p @ v1 {\n id = 1\n frame default { id: ghost }\n}",
        }
        for where, source in sources.items():
            with self.subTest(where=where):
                with self.assertRaises(ResolveError):
                    analyze(parse(source))


class ShippedSchemaImportTest(unittest.TestCase):
    def test_each_shipped_schema_loads_on_its_own(self) -> None:
        for path in sorted((ROOT / "schemas").rglob("*.bps")):
            with self.subTest(schema=path.relative_to(ROOT).as_posix()):
                analyze(load_schema([path]))

    def test_the_route_file_is_one_root_for_the_whole_protocol(self) -> None:
        resolved = analyze(load_schema([ROOT / "schemas" / "minecraft_26_3_wiki.bps"]))
        self.assertEqual(len(resolved.routes), 261)
        self.assertEqual(len(resolved.packets), 231)
        self.assertIn("varint", resolved.encodings)


def _scraper():
    path = ROOT / "scripts" / "scrape_wiki_routes.py"
    spec = importlib.util.spec_from_file_location("scrape_wiki_routes", path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class RouteScraperTest(_TempTree):
    def test_generated_route_file_loads_with_the_imports_it_is_given(self) -> None:
        scraper = _scraper()
        rows = [scraper.RouteRow("handshaking", "serverbound", 0, "intention")]
        shutil.copy(
            ROOT / "schemas" / "std" / "varint.bps", self._varint_target()
        )
        self.write("packets.bps", "packet handshake { }\n")
        routes = self.write(
            "routes.bps",
            scraper.emit_bps(rows, [], imports=["std/varint.bps", "packets.bps"]),
        )
        resolved = analyze(load_schema([routes]))
        self.assertEqual([pkt.name for _, pkt in resolved.routes], ["handshake"])

    def _varint_target(self) -> Path:
        target = self.root / "std" / "varint.bps"
        target.parent.mkdir(parents=True, exist_ok=True)
        return target


if __name__ == "__main__":
    unittest.main()
