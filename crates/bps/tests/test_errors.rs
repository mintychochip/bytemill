//! Ports tests/test_errors.py.

use bps::errors::{decode_at, parse, BpsError, ErrorKind};

#[test]
fn offset_optional() {
    let e = decode_at("bad", 3);
    let text = e.to_string();
    assert!(text.contains('3'), "{text}");
    assert_eq!(e.offset, Some(3));
}

#[test]
fn parse_error_is_an_error() {
    let e = parse("syntax");
    assert!(matches!(e, BpsError { kind: ErrorKind::Parse, .. }));
}

// A failed `require` prints the expression the way Python's dataclass repr does. That text is
// what users see, so it is pinned here; Python has no test for it.

fn require_failure(source: &str) -> String {
    let resolved = bps::analyze(&bps::parse(source).unwrap()).unwrap();
    bps::interp::decode_packet(&resolved, "p", &[0], false).unwrap_err().to_string()
}

#[test]
fn a_failed_require_shows_the_expression_like_the_python_repr() {
    let want = "require failed: Expr(kind='binop', value='==', \
left=Expr(kind='ident', value=None, left=None, right=None, args=(), name='x'), \
right=Expr(kind='int', value=1, left=None, right=None, args=(), name=None), args=(), name=None)";
    assert_eq!(require_failure("packet p { x: u8\n require x == 1 }\n"), want);
}

#[test]
fn a_ternary_in_a_require_has_a_one_element_args_tuple() {
    let want = "require failed: Expr(kind='ternary', value=None, \
left=Expr(kind='binop', value='==', \
left=Expr(kind='ident', value=None, left=None, right=None, args=(), name='x'), \
right=Expr(kind='int', value=1, left=None, right=None, args=(), name=None), args=(), name=None), \
right=Expr(kind='bool', value=True, left=None, right=None, args=(), name=None), \
args=(Expr(kind='bool', value=False, left=None, right=None, args=(), name=None),), name=None)";
    assert_eq!(require_failure("packet p { x: u8\n require x == 1 ? true : false }\n"), want);
}
