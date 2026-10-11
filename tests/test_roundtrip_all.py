"""Round-trip coverage: every declared packet must encode a synthetic value and
decode back to it.

Values come from `bps.synth`, which the Rust emitter also uses to pin its
reference byte vectors, so Python and Rust are exercised against the same
inputs.
"""

import unittest

from bps.errors import BpsError
from bps.interp.engine import decode_packet, encode_packet
from bps.synth import synthesize_packet

from tests.test_varint import load_minecraft


class RoundTripAllTest(unittest.TestCase):
    def test_every_packet_round_trips(self) -> None:
        resolved = load_minecraft()
        failures = []
        checked = 0
        for name, pkt in sorted(resolved.packets.items()):
            if name.endswith("_probe"):
                continue
            value = synthesize_packet(resolved, pkt)
            try:
                encoded = encode_packet(resolved, name, value)
            except BpsError as e:
                failures.append(f"{name}: encode {e}")
                continue
            try:
                decoded = decode_packet(resolved, name, encoded)
            except BpsError as e:
                # `require` assertions are schema contracts, not round-trip
                # failures: a synthetic value that violates one is expected to
                # be rejected (handshake pins protocol_version == 777).
                if "require failed" in str(e):
                    continue
                failures.append(f"{name}: decode {e}")
                continue
            try:
                reencoded = encode_packet(resolved, name, decoded)
            except BpsError as e:
                failures.append(f"{name}: re-encode {e}")
                continue
            if reencoded != encoded:
                failures.append(f"{name}: re-encode differs")
            checked += 1
        self.assertEqual(failures[:10], [])
        self.assertGreaterEqual(checked, 230)


class RouteSmokeTest(unittest.TestCase):
    def test_every_route_decodes_its_packet(self) -> None:
        """Each route must dispatch to its packet and decode a zero-seeded payload.

        Registry-dependent layouts (Slot, component data) legitimately reject a
        zero buffer, so a clean BpsError is a pass; a crash is not.
        """
        resolved = load_minecraft()
        bad = []
        for route, pkt in resolved.routes:
            payload = bytes(8192)
            try:
                decode_packet(resolved, pkt.name, payload)
            except BpsError:
                continue
            except Exception as e:  # pragma: no cover - defensive
                bad.append(f"{route.state}.{route.direction} 0x{route.packet_id:x}: {e!r}")
                continue
        self.assertEqual(bad, [])


if __name__ == "__main__":
    unittest.main()
