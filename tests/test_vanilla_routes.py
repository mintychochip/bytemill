"""The route table is vanilla's packet registration order, not just the wiki's.

A packet's id is its position in the protocol's builder chain in vanilla's source
(`GameProtocols` and its siblings), so the names at each id are checked against
that. The wiki is where the route table was transcribed from; this is what makes
the id of every packet, including the ones with no recorded bytes, vanilla's.
"""

import re
import unittest
from pathlib import Path

from tests.test_varint import load_minecraft

PROTOCOL = (
    Path(__file__).resolve().parents[2]
    / "mc-ref" / "server-26.3" / "net" / "minecraft" / "network" / "protocol"
)

# (state, direction) -> the file and the builder chain that registers its packets.
CHAINS = {
    ("handshaking", "serverbound"): ("handshake/HandshakeProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("status", "serverbound"): ("status/StatusProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("status", "clientbound"): ("status/StatusProtocols.java", "CLIENTBOUND_TEMPLATE"),
    ("login", "serverbound"): ("login/LoginProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("login", "clientbound"): ("login/LoginProtocols.java", "CLIENTBOUND_TEMPLATE"),
    ("configuration", "serverbound"): ("configuration/ConfigurationProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("configuration", "clientbound"): ("configuration/ConfigurationProtocols.java", "CLIENTBOUND_TEMPLATE"),
    ("play", "serverbound"): ("game/GameProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("play", "clientbound"): ("game/GameProtocols.java", "CLIENTBOUND_TEMPLATE"),
}

# BPS names a packet as the wiki does; vanilla registers these under another name.
ALIASES = {
    "handshake": "intention",
    "login_start": "hello",
    "encryption_response": "key",
    # Vanilla's one `show_dialog` has two wire forms by state; BPS has a packet for each.
    "show_dialog_configuration": "show_dialog",
}


def _type_names() -> dict[tuple[str, str], str]:
    """`(file stem, CONSTANT)` -> the name a packet type is created with (`debug/event` is `debug_event`)."""
    names = {}
    for path in PROTOCOL.rglob("*PacketTypes.java"):
        declaration = r'PacketType<[^=;]+?>\s+(\w+)\s*=\s*create(?:Client|Server)bound\(\s*"([\w/]+)"\s*\)'
        for const, name in re.findall(declaration, path.read_text(encoding="utf-8")):
            names[(path.stem, const)] = name.replace("/", "_")
    return names


def _registered(file: str, template: str, names: dict[tuple[str, str], str]) -> list[str]:
    """The packet names a builder chain registers, in the order that numbers them."""
    text = (PROTOCOL / file).read_text(encoding="utf-8")
    start = text.index(f" {template} =")
    end = text.find("public static final", start + 1)
    chain = text[start : end if end != -1 else len(text)]
    registered = []
    for call, owner, const in re.findall(r"\.?(addPacket|withBundlePacket)\(\s*(\w+)\.(\w+)", chain):
        # A bundle is registered by its delimiter, which takes the first id.
        registered.append(names[(owner, const + "_DELIMITER" if call == "withBundlePacket" else const)])
    return registered


@unittest.skipUnless(PROTOCOL.is_dir(), "vanilla 26.3 source not present")
class VanillaRoutesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.resolved = load_minecraft()
        cls.names = _type_names()

    def bps_names(self, state: str, direction: str) -> dict[int, str]:
        out = {}
        for route, pkt in self.resolved.routes:
            if (route.state, route.direction) == (state, direction):
                name = pkt.name.removesuffix("_serverbound")
                out[route.packet_id] = ALIASES.get(name, name)
        return out

    def test_every_route_is_the_packet_vanilla_registers_at_that_id(self) -> None:
        for (state, direction), (file, template) in CHAINS.items():
            with self.subTest(state=state, direction=direction):
                vanilla = _registered(file, template, self.names)
                bps = self.bps_names(state, direction)
                self.assertEqual([bps.get(i) for i in range(len(vanilla))], vanilla)
                # The only id vanilla does not register is the pre-1.7 ping BPS also describes.
                self.assertEqual(sorted(set(bps) - set(range(len(vanilla)))), [0xFE] if state == "handshaking" else [])

    def test_the_aliases_are_all_in_use(self) -> None:
        """An alias nothing needs would hide a rename that later stops matching."""
        used = {
            pkt.name.removesuffix("_serverbound")
            for _, pkt in self.resolved.routes
        }
        self.assertEqual(sorted(set(ALIASES) - used), [])


if __name__ == "__main__":
    unittest.main()
