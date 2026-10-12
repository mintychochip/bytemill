"""Committed wave-1 shape ledger invariants (no rstom worktree required)."""

from __future__ import annotations

import unittest
from pathlib import Path

from bps.shapes import load_ledger

ROOT = Path(__file__).resolve().parents[1]
SHAPES = ROOT / "targets" / "rstom" / "shapes"

WAVE1_NEW_BODY_PACKETS = frozenset(
    {
        "ClientNameItemPacket",
        "ClientPluginMessagePacket",
    }
)

WAVE1_READ_VIA_NEW_PACKETS = frozenset(
    {
        "ClientClickWindowButtonPacket",
        "ClientCloseWindowPacket",
        "ClientHeldItemChangePacket",
        "ClientLockDifficultyPacket",
        "ClientNameItemPacket",
        "ClientPickItemFromEntityPacket",
        "UpdateHealthPacket",
        "UpdateSimulationDistancePacket",
        "UpdateViewDistancePacket",
        "UpdateViewPositionPacket",
        "WindowPropertyPacket",
        "WorldBorderCenterPacket",
        "WorldBorderLerpSizePacket",
        "WorldBorderSizePacket",
        "WorldBorderWarningDelayPacket",
        "WorldBorderWarningReachPacket",
    }
)


class Wave1LedgerTest(unittest.TestCase):
    def test_non_trivial_new_body_packets(self) -> None:
        shapes = load_ledger(SHAPES)
        found = {s.cls for s in shapes.values() if s.new.body}
        self.assertEqual(found, WAVE1_NEW_BODY_PACKETS)

    def test_read_via_new_packets(self) -> None:
        shapes = load_ledger(SHAPES)
        found = {s.cls for s in shapes.values() if s.serializer.read_via_new}
        self.assertEqual(found, WAVE1_READ_VIA_NEW_PACKETS)

    def test_exactly_sixty_four_shapes(self) -> None:
        shapes = load_ledger(SHAPES)
        self.assertEqual(len(shapes), 64)
