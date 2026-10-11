"""BPS packet layouts against the bytes real Minestom writes.

The other tests encode synthetic values and decode them back, so a layout that
reads too little or the wrong shape still round-trips. These start from bytes the
Java server produced. Every recorded packet must decode strictly (no bytes left
over) and re-encode to the same bytes, and the route table must agree with Java's
registry on which ids exist.

Minestom is an implementation, not the protocol: vanilla is. `KNOWN_MISMATCHES`
lists the packets where Minestom's bytes differ from vanilla 26.3's wire format
and BPS follows vanilla, each with the reason. A listed packet that starts
passing fails the test, so the list cannot go stale.
"""

import unittest

from tests import java_probes as java
from tests.test_varint import load_minecraft

# packet -> how Minestom's bytes differ from vanilla's wire format, which BPS follows.
KNOWN_MISMATCHES: dict[str, str] = {
    "delete_chat": (
        "vanilla writes MessageSignature.Packed (varint id + 1, then the 256-byte "
        "signature only when that is 0); Minestom writes just the 256 bytes"
    ),
    "move_minecart_along_track": (
        "vanilla's MinecartStep writes yaw and pitch as ROTATION_BYTE (one byte); "
        "Minestom writes them as 4-byte floats"
    ),
}

# Java classes that serve two BPS packets because vanilla writes them differently by state.
SPLIT_CLASSES: dict[str, list[str]] = {
    "ShowDialogPacket": ["show_dialog", "show_dialog_configuration"],
}

# Packets with at least one recorded sample. The recorded packets are the only
# independent check these layouts get, so this must not shrink unnoticed.
COVERED_PACKETS = 136


@unittest.skipUnless(java.available(), "rstom probe fixtures not present")
class JavaGroundTruthTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.resolved = load_minecraft()

    def test_java_and_bps_register_the_same_ids(self) -> None:
        route = java.route_packets(self.resolved)
        java_slots = {(s.state, s.direction, s.packet_id) for s in java.slots()}
        self.assertEqual(sorted(java_slots - set(route)), [], "ids Java has and BPS lacks")
        # Minestom does not register the pre-1.7 legacy ping, which BPS routes at 0xFE.
        legacy = [("handshaking", "serverbound", 0xFE)]
        self.assertEqual(sorted(set(route) - java_slots), legacy, "ids BPS has and Java lacks")

    def test_each_java_class_is_one_bps_packet(self) -> None:
        by_class = java.packets_by_class(self.resolved)
        many = {c: p for c, p in by_class.items() if len(p) != 1 and c not in SPLIT_CLASSES}
        self.assertEqual(many, {})
        for java_class, packets in SPLIT_CLASSES.items():
            self.assertEqual(by_class[java_class], sorted(packets))

    def test_every_recorded_sample_belongs_to_a_bps_packet(self) -> None:
        by_class = java.packets_by_class(self.resolved)
        orphans = sorted({s.java_class for s in java.samples() if s.java_class not in by_class})
        self.assertEqual(orphans, [])

    def test_recorded_samples_still_cover_the_packets_they_did(self) -> None:
        by_class = java.packets_by_class(self.resolved)
        covered = {p for s in java.samples() for p in by_class.get(s.java_class, [])}
        self.assertGreaterEqual(len(covered), COVERED_PACKETS)

    def test_recorded_packets_round_trip_through_the_schema(self) -> None:
        failed = java.failures(self.resolved)
        lines = []
        for packet, items in sorted(failed.items()):
            if packet in KNOWN_MISMATCHES:
                continue
            first = items[0]
            lines.append(
                f"{packet}: {len(items)} sample(s) differ, e.g. {first.sample.label}: "
                f"{first.reason}\n    bytes {first.sample.data.hex()}"
            )
        self.assertEqual(lines, [], "\n" + "\n".join(lines))
        fixed = sorted(set(KNOWN_MISMATCHES) - set(failed))
        self.assertEqual(fixed, [], "listed in KNOWN_MISMATCHES but now round-trip; remove them")


if __name__ == "__main__":
    unittest.main()
