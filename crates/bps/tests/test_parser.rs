//! Ports tests/test_parser.py.

mod common;

use bps::parse;

#[test]
fn parse_core() {
    let path = common::repo_root().join("schemas/minecraft_core.bps");
    let src = std::fs::read_to_string(&path).unwrap();
    let s = parse(&src).unwrap();
    assert_eq!(s.imports, vec!["std/integers.bps", "std/varint.bps"]);
    assert!(!s.encodings.iter().any(|e| e.name == "varint"));
}

#[test]
fn parse_all_schemas() {
    let mut paths = Vec::new();
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "bps") {
                out.push(path);
            }
        }
    }
    walk(&common::repo_root().join("schemas"), &mut paths);
    paths.sort();
    assert!(!paths.is_empty());
    for path in paths {
        let src = std::fs::read_to_string(&path).unwrap();
        parse(&src).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

// The three tests below pin Python lexer behavior the schema-mutation fuzz showed the port
// had lost; Python has no test for them.

#[test]
fn identifiers_may_use_unicode_letters_like_python() {
    // Python lexes identifiers with `isalpha()`/`isalnum()`, so `é字` is a name like any other.
    let schema = parse("encoding é字 = { x: u8 }\npacket probe { v: é字 }\n").unwrap();
    assert_eq!(schema.encodings[0].name, "é字");
}

#[test]
fn an_unexpected_character_is_quoted_like_python_repr() {
    // `repr("'")` is `"'"`; controls and the no-break space print escaped, not raw.
    let cases = [
        ("packet p { a: u8 }'", "\"'\""),
        ("packet p { a: u8 }#", "'#'"),
        ("packet p {\0}", "'\\x00'"),
        ("packet p {\u{a0}}", "'\\xa0'"),
        ("packet p {\u{200b}}", "'\\u200b'"),
    ];
    for (source, shown) in cases {
        let message = parse(source).unwrap_err().to_string();
        let want = format!("unexpected character {shown} (offset ");
        assert!(message.starts_with(&want), "{source:?}: {message}");
    }
}

#[test]
fn a_hex_literal_needs_digits() {
    // Python dies with a raw ValueError from `int("0x", 16)`; the port reports a parse error.
    let message = parse("packet p { a: u8 if x == 0x }\n").unwrap_err().to_string();
    assert!(message.contains("hex literal has no digits: 0x"), "{message}");
}

#[test]
fn parse_errors_quote_operators_and_name_tokens_like_python() {
    // `expected {text!r}` quotes with repr; `expected {kind}` prints `TokKind.LBRACK`.
    let cases = [
        ("encoding e = { k: u8\n v: match k { 0 u8 } }\n", "expected '=>' (offset 37)"),
        ("packet [ x ]\n", "expected TokKind.IDENT, got TokKind.LBRACK (offset 7)"),
        (
            "encoding e = { a: u8[ b }\n",
            "expected TokKind.RBRACK, got TokKind.IDENT (offset 22)",
        ),
    ];
    for (source, want) in cases {
        assert_eq!(parse(source).unwrap_err().to_string(), want, "{source:?}");
    }
}
