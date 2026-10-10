import unittest

from bps.errors import BpsError
from bps.interp.engine import decode_packet

from tests.test_varint import load_minecraft

# Packets whose wiki layout is registry- or payload-dependent and therefore kept as
# `body: rest` in v1. This list is a gate: transcribing one of these (or stubbing a
# new packet) requires updating it deliberately.
REST_FALLBACKS = frozenset(
    {
        "add_entity",
        "commands",
        "debug_block_value",
        "debug_chunk_value",
        "debug_entity_value",
        "debug_event",
        "disguised_chat",
        "explode",
        "interact",
        "level_chunk_with_light",
        "level_particles",
        "light_update",
        "merchant_offers",
        "place_ghost_recipe",
        "player_chat",
        "player_info_update",
        "player_position",
        "recipe_book_add",
        "server_links",
        "set_entity_motion",
        "set_equipment",
        "set_objective",
        "set_player_team",
        "set_score",
        "show_dialog",
        "teleport_entity",
        "update_advancements",
        "update_recipes",
        "waypoint",
    }
)


def is_rest_stub(pkt) -> bool:
    if len(pkt.items) != 1:
        return False
    item = pkt.items[0]
    return (
        item.kind == "field"
        and item.wire is not None
        and getattr(item.wire.type_ref, "name", None) == "rest"
    )


class RouteCoverageTest(unittest.TestCase):
    def test_every_route_resolves_to_its_packet(self) -> None:
        resolved = load_minecraft()
        self.assertEqual(len(resolved.routes), 261)
        for route, pkt in resolved.routes:
            self.assertEqual(route.packet_name, pkt.name)
            self.assertGreaterEqual(route.packet_id, 0)

    def test_rest_fallback_set_is_explicit(self) -> None:
        resolved = load_minecraft()
        routed = {pkt.name for _, pkt in resolved.routes}
        stubs = {name for name in routed if is_rest_stub(resolved.packets[name])}
        self.assertEqual(stubs, REST_FALLBACKS)

    def test_non_stub_packets_smoke_decode(self) -> None:
        """Every transcribed packet must decode (or fail cleanly) on a zero buffer."""
        resolved = load_minecraft()
        seen: set[str] = set()
        for _, pkt in resolved.routes:
            if pkt.name in seen or is_rest_stub(pkt):
                continue
            seen.add(pkt.name)
            try:
                decode_packet(resolved, pkt.name, bytes(4096))
            except BpsError:
                pass  # clean decode failure (require/EOF) is acceptable
        self.assertGreater(len(seen), 150)


if __name__ == "__main__":
    unittest.main()
