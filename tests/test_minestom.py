"""BPS packets get the Minestom class `rstom-packets` names them by."""

import unittest
from pathlib import Path

from bps.analyze import analyze
from bps.errors import EmitError
from bps.minestom import ClassTable, build_units
from bps.parser import parse

from tests import java_probes as java
from tests.rstom_tables import real_table, small_table
from tests.test_varint import load_minecraft

ROOT = Path(__file__).resolve().parents[1]


def _protocol(*routes: str) -> str:
    body = "".join(f"    route {r}\n" for r in routes)
    frame = "    frame default {\n        length: varint\n        id: varint\n        payload: rest\n    }\n"
    return f"protocol minecraft @ v1 {{\n    id = 1\n{frame}{body}}}\n"


def _units(source: str, table: ClassTable):
    std = "".join(
        (ROOT / "schemas" / "std" / f"{name}.bps").read_text(encoding="utf-8")
        for name in ("integers", "varint")
    )
    return build_units(analyze(parse(std + source)), table)


class MinestomClassesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.resolved = load_minecraft()
        cls.table = real_table()
        cls.units = build_units(cls.resolved, cls.table)

    def by_route(self) -> dict[tuple[str, str, int], str]:
        return {
            (r.state, r.direction, r.packet_id): u.cls.name for u in self.units for r in u.routes
        }

    @unittest.skipUnless(java.available(), "rstom probe fixtures not present")
    def test_the_class_at_an_id_is_the_class_minestom_registers_there(self) -> None:
        """Checked against the registry tables recorded from Minestom itself."""
        recorded = {(s.state, s.direction, s.packet_id): s.java_class for s in java.slots()}
        routed = self.by_route()
        self.assertEqual({slot: routed.get(slot) for slot in recorded}, recorded)
        # The one route Minestom's id tables do not list is the pre-1.7 ping.
        self.assertEqual(set(routed) - set(recorded), {("handshaking", "serverbound", 0xFE)})

    def test_every_packet_has_a_struct_for_each_direction_it_travels(self) -> None:
        both = {u.pkt.name for u in self.units if u.direction == "serverbound"} & {
            u.pkt.name for u in self.units if u.direction == "clientbound"
        }
        self.assertEqual(
            sorted(both),
            ["custom_payload", "finish_configuration", "keep_alive", "select_known_packs"],
        )
        self.assertEqual({u.pkt.name for u in self.units}, set(self.resolved.packets))
        self.assertEqual(len(self.units), len(self.resolved.packets) + len(both))

    def test_a_packet_in_both_directions_is_a_client_and_a_server_class(self) -> None:
        classes = {u.direction: u.cls for u in self.units if u.pkt.name == "keep_alive"}
        self.assertEqual(classes["serverbound"].java, self.java("client.common.ClientKeepAlivePacket"))
        self.assertEqual(classes["clientbound"].java, self.java("server.common.KeepAlivePacket"))

    def test_a_route_outside_the_id_table_is_found_by_the_packets_name(self) -> None:
        legacy = next(u for u in self.units if u.pkt.name == "legacy_server_list_ping")
        self.assertEqual(
            (legacy.cls.group, legacy.cls.package, legacy.cls.name),
            ("client", "status", "LegacyServerListPingPacket"),  # Minestom files it with status
        )

    def test_a_class_with_two_wire_forms_is_one_module_per_form(self) -> None:
        """Minestom keeps `ShowDialogPacket`'s two serializers on one class; a struct has one."""
        forms = {u.pkt.name: u.cls for u in self.units if u.cls.name == "ShowDialogPacket"}
        self.assertEqual(sorted(forms), ["show_dialog", "show_dialog_configuration"])
        self.assertEqual(forms["show_dialog"].java, self.java("server.common.ShowDialogPacket"))
        self.assertEqual(
            forms["show_dialog_configuration"].java,
            self.java("server.configuration.ShowDialogPacket"),
        )
        self.assertEqual(forms["show_dialog_configuration"].package, "configuration")

    def test_no_two_structs_share_a_java_class_name(self) -> None:
        """`PacketEntry::resolve` swaps a serializer in by class name, so a repeat would win."""
        names = [u.cls.java for u in self.units]
        self.assertEqual(sorted(n for n in set(names) if names.count(n) > 1), [])

    @staticmethod
    def java(tail: str) -> str:
        return f"net.minestom.server.network.packet.{tail}"


class ClassMappingErrorTest(unittest.TestCase):
    def test_a_route_with_no_class_is_an_error(self) -> None:
        table = small_table(("play", "SERVER", ["ProbePacket"]))
        with self.assertRaisesRegex(EmitError, "route play.clientbound 0x01 => other"):
            _units(
                "packet probe { a: u8 } packet other { b: u8 }\n"
                + _protocol("play.clientbound 0x00 => probe", "play.clientbound 0x01 => other"),
                table,
            )

    def test_a_packet_with_no_route_is_an_error(self) -> None:
        with self.assertRaisesRegex(EmitError, "packet 'probe' has no route"):
            _units("packet probe { a: u8 }\n" + _protocol(), small_table(("play", "SERVER", ["ProbePacket"])))

    def test_one_packet_cannot_be_two_classes_in_one_direction(self) -> None:
        table = small_table(("configuration", "SERVER", ["APacket"]), ("play", "SERVER", ["BPacket"]))
        with self.assertRaisesRegex(EmitError, "probe.*APacket.*BPacket"):
            _units(
                "packet probe { a: u8 }\n"
                + _protocol("configuration.clientbound 0x00 => probe", "play.clientbound 0x00 => probe"),
                table,
            )

    def test_two_packets_on_one_class_must_be_declared(self) -> None:
        table = small_table(("configuration", "SERVER", ["APacket"]), ("play", "SERVER", ["APacket"]))
        with self.assertRaisesRegex(EmitError, "APacket.*first.*second"):
            _units(
                "packet first { a: u8 } packet second { b: u8 }\n"
                + _protocol("configuration.clientbound 0x00 => first", "play.clientbound 0x00 => second"),
                table,
            )


if __name__ == "__main__":
    unittest.main()
