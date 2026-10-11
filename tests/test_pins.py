"""Pin rows: BPS reference bytes for every contiguous id-table entry."""

import json
import unittest
from collections import defaultdict

from bps.emit_rstom import _registry_entries
from bps.interp.engine import decode_packet, encode_packet
from bps.minestom import build_units
from bps.pins import build_pins, pins_json

from tests.rstom_tables import real_table
from tests.test_varint import load_minecraft

_SIDE = {"serverbound": "CLIENT", "clientbound": "SERVER"}
_STATE = {"handshaking": "HANDSHAKE", "status": "STATUS", "login": "LOGIN",
          "configuration": "CONFIGURATION", "play": "PLAY"}
_STATE_TO_ROUTE = {v: k for k, v in _STATE.items()}
_SIDE_TO_DIRECTION = {"CLIENT": "serverbound", "SERVER": "clientbound"}


class PinsTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.resolved = load_minecraft()
        cls.table = real_table()
        cls.units = build_units(cls.resolved, cls.table)
        cls.rows = build_pins(cls.resolved, cls.units)
        cls._packet_by_row = {}
        for entries in _registry_entries(cls.units).values():
            for route, unit in entries:
                key = (_SIDE[route.direction], _STATE[route.state], route.packet_id)
                cls._packet_by_row[key] = unit.pkt.name

    def test_one_row_per_id_table_entry(self) -> None:
        self.assertEqual(len(self.rows), 260)
        by_group: dict[tuple[str, str], list[int]] = defaultdict(list)
        for row in self.rows:
            by_group[(row.side, row.state)].append(row.id)
        for ids in by_group.values():
            self.assertEqual(ids, list(range(len(ids))))

    def test_every_row_round_trips_in_the_interpreter(self) -> None:
        for row in self.rows:
            packet = self._packet_by_row[(row.side, row.state, row.id)]
            data = bytes.fromhex(row.hex)
            value = decode_packet(self.resolved, packet, data, strict=True)
            again = encode_packet(self.resolved, packet, value)
            self.assertEqual(again, data, msg=f"{row.side}/{row.state}/{row.id} {row.cls}")

    def test_class_names_come_from_the_class_table(self) -> None:
        for row in self.rows:
            state = _STATE_TO_ROUTE[row.state]
            direction = _SIDE_TO_DIRECTION[row.side]
            cls = self.table.at(state, direction, row.id)
            self.assertIsNotNone(cls)
            self.assertEqual(row.cls, cls.name)

    def test_json_is_stable_one_row_per_line(self) -> None:
        forward = pins_json(self.rows)
        backward = pins_json(list(reversed(self.rows)))
        self.assertEqual(forward, backward)
        doc = json.loads(forward)
        self.assertIn("_generated", doc)
        self.assertEqual(len(doc["rows"]), len(self.rows))

