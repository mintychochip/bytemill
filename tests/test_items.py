"""Column-0 item scanner for hand-written rstom packet files (bps/shapes/items.py)."""

from __future__ import annotations

import unittest

from bps.shapes.items import impl_members, scan_items
from tests.rstom_tables import RSTOM

PACKET_ROOT = RSTOM / "crates" / "rstom" / "src" / "network" / "packet"

MODE_SNIPPET = """\
//! Module docs.

use std::fmt;

/// Doc for Mode.
#[derive(Debug)]
pub enum Mode {
    A,
}

impl Mode {
    pub fn id(&self) -> u8 {
        1
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{}}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {}
}
"""

LEXICAL_SNIPPET = """\
// don't stop { here
const OPEN: char = '{';
const B: u8 = b'}';
const C: char = 'x';
const Q: char = '\\'';
const RAW: &str = r#"a"}"#;
const PATH: &str = r"C:\\";
const LIFE: &'static str = "s";
fn after<'a>(x: &'a str) -> &'a str {
    x
}
"""

MULTILINE_ATTR_SNIPPET = """\
#[cfg_attr(
    test,
    derive(Debug)
)]
pub struct Probe;
"""

GENERIC_IMPL_SNIPPET = """\
impl<T: Clone> Holder<T> {
    pub fn get(&self) -> T {
        self.0.clone()
    }
}
"""

MEMBER_SNIPPET = """\
impl Holder {
    /// Creates.
    #[must_use]
    pub fn new(a: i32) -> Self {
        Self { a }
    }

    pub const LIMIT: usize = 4;
    type Output = u8;

    pub fn serializer() -> Arc<dyn PacketSerializer> {
        Arc::new(S)
    }
}
"""


def _only_comments(gap: str) -> bool:
    return all(not line.strip() or line.strip().startswith("//") for line in gap.splitlines())


class ScanItemsTest(unittest.TestCase):
    def test_kinds_names_traits_and_cfg_test_in_source_order(self) -> None:
        items = scan_items(MODE_SNIPPET)
        self.assertEqual(
            [(i.kind, i.name, i.trait, i.cfg_test) for i in items],
            [
                ("use", None, None, False),
                ("enum", "Mode", None, False),
                ("impl", "Mode", None, False),
                ("impl", "Mode", "Display", False),
                ("mod", "tests", None, True),
            ],
        )

    def test_item_text_includes_doc_comment_and_attributes(self) -> None:
        enum = scan_items(MODE_SNIPPET)[1]
        self.assertEqual(
            enum.text,
            "/// Doc for Mode.\n#[derive(Debug)]\npub enum Mode {\n    A,\n}",
        )
        self.assertEqual(MODE_SNIPPET[enum.start : enum.end], enum.text)

    def test_multiline_attribute_belongs_to_the_item(self) -> None:
        (item,) = scan_items(MULTILINE_ATTR_SNIPPET)
        self.assertEqual((item.kind, item.name), ("struct", "Probe"))
        self.assertEqual(item.text, MULTILINE_ATTR_SNIPPET.rstrip("\n"))

    def test_generic_impl_names_the_target_type(self) -> None:
        (item,) = scan_items(GENERIC_IMPL_SNIPPET)
        self.assertEqual((item.kind, item.name, item.trait), ("impl", "Holder", None))

    def test_trait_path_is_reduced_to_its_last_segment(self) -> None:
        (item,) = scan_items("impl std::fmt::Display for Probe {\n}\n")
        self.assertEqual((item.name, item.trait), ("Probe", "Display"))

    def test_braces_quotes_and_comments_do_not_split_items(self) -> None:
        items = scan_items(LEXICAL_SNIPPET)
        self.assertEqual(
            [(i.kind, i.name) for i in items],
            [
                ("const", "OPEN"),
                ("const", "B"),
                ("const", "C"),
                ("const", "Q"),
                ("const", "RAW"),
                ("const", "PATH"),
                ("const", "LIFE"),
                ("fn", "after"),
            ],
        )
        self.assertEqual(items[-1].text, "fn after<'a>(x: &'a str) -> &'a str {\n    x\n}")


class ImplMembersTest(unittest.TestCase):
    def test_members_are_fn_const_and_type_with_absolute_offsets(self) -> None:
        (impl,) = scan_items(MEMBER_SNIPPET)
        members = impl_members(impl)
        self.assertEqual(
            [(m.kind, m.name) for m in members],
            [("fn", "new"), ("const", "LIMIT"), ("type", "Output"), ("fn", "serializer")],
        )
        new = members[0]
        self.assertEqual(
            new.text,
            "/// Creates.\n    #[must_use]\n    pub fn new(a: i32) -> Self {\n        Self { a }\n    }",
        )
        for m in members:
            self.assertEqual(MEMBER_SNIPPET[m.start : m.end], m.text)


class ItemsRealTreeTest(unittest.TestCase):
    def test_items_tile_every_packet_file_apart_from_comments(self) -> None:
        if not PACKET_ROOT.is_dir():
            self.skipTest("rstom worktree absent")
        for path in sorted(PACKET_ROOT.rglob("*.rs")):
            src = path.read_text(encoding="utf-8")
            items = scan_items(src)
            with self.subTest(path=str(path.relative_to(PACKET_ROOT))):
                prev_end = 0
                for item in items:
                    self.assertLessEqual(prev_end, item.start)
                    self.assertTrue(_only_comments(src[prev_end : item.start]), item.name)
                    self.assertEqual(src[item.start : item.end], item.text)
                    prev_end = item.end
                self.assertTrue(_only_comments(src[prev_end:]))


if __name__ == "__main__":
    unittest.main()
