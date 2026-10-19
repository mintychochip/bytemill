//! Ports tests/test_items.py: the column-0 item scanner.

mod common;

use bytemill::shapes::items::{impl_members, scan_items, Item, Member};

const MODE_SNIPPET: &str = r#"//! Module docs.

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
"#;

const LEXICAL_SNIPPET: &str = "// don't stop { here\nconst OPEN: char = '{';\nconst B: u8 = b'}';\nconst C: char = 'x';\nconst Q: char = '\\'';\nconst RAW: &str = r#\"a\"}\"#;\nconst PATH: &str = r\"C:\\\\\";\nconst LIFE: &'static str = \"s\";\nfn after<'a>(x: &'a str) -> &'a str {\n    x\n}\n";

const MULTILINE_ATTR_SNIPPET: &str = r#"#[cfg_attr(
    test,
    derive(Debug)
)]
pub struct Probe;
"#;

const GENERIC_IMPL_SNIPPET: &str = r#"impl<T: Clone> Holder<T> {
    pub fn get(&self) -> T {
        self.0.clone()
    }
}
"#;

const MEMBER_SNIPPET: &str = r#"impl Holder {
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
"#;

fn only_comments(gap: &str) -> bool {
    gap.lines().all(|l| l.trim().is_empty() || l.trim().starts_with("//"))
}

#[test]
fn kinds_names_traits_and_cfg_test_in_source_order() {
    let items = scan_items(MODE_SNIPPET).unwrap();
    let got: Vec<(&str, Option<&str>, Option<&str>, bool)> = items
        .iter()
        .map(|i| (i.kind.as_str(), i.name.as_deref(), i.trait_.as_deref(), i.cfg_test))
        .collect();
    assert_eq!(
        got,
        vec![
            ("use", None, None, false),
            ("enum", Some("Mode"), None, false),
            ("impl", Some("Mode"), None, false),
            ("impl", Some("Mode"), Some("Display"), false),
            ("mod", Some("tests"), None, true),
        ]
    );
}

#[test]
fn item_text_includes_doc_comment_and_attributes() {
    let items = scan_items(MODE_SNIPPET).unwrap();
    let e = &items[1];
    assert_eq!(
        e.text,
        "/// Doc for Mode.\n#[derive(Debug)]\npub enum Mode {\n    A,\n}"
    );
    assert_eq!(&MODE_SNIPPET[e.start..e.end], e.text);
}

#[test]
fn multiline_attribute_belongs_to_the_item() {
    let items = scan_items(MULTILINE_ATTR_SNIPPET).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!((items[0].kind.as_str(), items[0].name.as_deref()), ("struct", Some("Probe")));
    assert_eq!(items[0].text, MULTILINE_ATTR_SNIPPET.trim_end_matches('\n'));
}

#[test]
fn generic_impl_names_the_target_type() {
    let items = scan_items(GENERIC_IMPL_SNIPPET).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        (items[0].kind.as_str(), items[0].name.as_deref(), items[0].trait_.as_deref()),
        ("impl", Some("Holder"), None)
    );
}

#[test]
fn trait_path_is_reduced_to_its_last_segment() {
    let items = scan_items("impl std::fmt::Display for Probe {\n}\n").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!((items[0].name.as_deref(), items[0].trait_.as_deref()), (Some("Probe"), Some("Display")));
}

#[test]
fn braces_quotes_and_comments_do_not_split_items() {
    let items = scan_items(LEXICAL_SNIPPET).unwrap();
    let got: Vec<(&str, Option<&str>)> =
        items.iter().map(|i| (i.kind.as_str(), i.name.as_deref())).collect();
    assert_eq!(
        got,
        vec![
            ("const", Some("OPEN")),
            ("const", Some("B")),
            ("const", Some("C")),
            ("const", Some("Q")),
            ("const", Some("RAW")),
            ("const", Some("PATH")),
            ("const", Some("LIFE")),
            ("fn", Some("after")),
        ]
    );
    assert_eq!(
        items.last().unwrap().text,
        "fn after<'a>(x: &'a str) -> &'a str {\n    x\n}"
    );
}

#[test]
fn column_zero_block_comments_do_not_join_the_next_item() {
    let src = "/* header\n   note */\nuse crate::a::B;\n\n/* outer /* nested */ still */\n#[derive(Debug)]\npub struct Probe {\n    pub x: i32,\n}\n\n/* tests\n   here */\n#[cfg(test)]\nmod tests {\n}\n";
    let items = scan_items(src).unwrap();
    let got: Vec<(&str, Option<&str>, bool)> = items
        .iter()
        .map(|i| (i.kind.as_str(), i.name.as_deref(), i.cfg_test))
        .collect();
    assert_eq!(
        got,
        vec![("use", None, false), ("struct", Some("Probe"), false), ("mod", Some("tests"), true)]
    );
    assert_eq!(items[0].text, "use crate::a::B;");
    assert_eq!(
        items[1].text,
        "#[derive(Debug)]\npub struct Probe {\n    pub x: i32,\n}"
    );
    assert_eq!(items[2].text, "#[cfg(test)]\nmod tests {\n}");
}

#[test]
fn members_are_fn_const_and_type_with_absolute_offsets() {
    let items = scan_items(MEMBER_SNIPPET).unwrap();
    assert_eq!(items.len(), 1);
    let members: Vec<Member> = impl_members(&items[0]).unwrap();
    let got: Vec<(&str, Option<&str>)> =
        members.iter().map(|m| (m.kind.as_str(), m.name.as_deref())).collect();
    assert_eq!(
        got,
        vec![
            ("fn", Some("new")),
            ("const", Some("LIMIT")),
            ("type", Some("Output")),
            ("fn", Some("serializer")),
        ]
    );
    let new = &members[0];
    let snippet = &MEMBER_SNIPPET[new.start..new.end];
    assert_eq!(
        snippet,
        "/// Creates.\n    #[must_use]\n    pub fn new(a: i32) -> Self {\n        Self { a }\n    }"
    );
    for m in &members {
        let text = &MEMBER_SNIPPET[m.start..m.end];
        assert!(text.ends_with(';') || text.ends_with('}'), "{:?}", m.name);
    }
    let _: Option<&Item> = None;
}

#[test]
fn items_tile_every_packet_file_apart_from_comments() {
    let packet_root =
        common::rstom_tables::rstom_dir().join("crates/rstom/src/network/packet");
    if !packet_root.is_dir() {
        eprintln!("rstom worktree absent; skipping");
        return;
    }
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&packet_root, &mut paths);
    paths.sort();
    for path in paths {
        let src = std::fs::read_to_string(&path).unwrap();
        let items = scan_items(&src).unwrap();
        let mut prev_end = 0usize;
        for item in &items {
            assert!(prev_end <= item.start, "{}", path.display());
            let gap = &src[prev_end..item.start];
            assert!(only_comments(gap), "{}: gap is not comments", path.display());
            let text = src[item.start..item.end].trim_end();
            assert!(text.ends_with(';') || text.ends_with('}'), "{}", path.display());
            prev_end = item.end;
        }
        assert!(only_comments(&src[prev_end..]), "{}: tail is not comments", path.display());
    }
}
