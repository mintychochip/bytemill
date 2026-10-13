//! Ports tests/test_decode_strict.py.

use bps::interp::decode_packet;
use bps::parse;

fn resolved() -> bps::ResolvedSchema {
    bps::analyze(&parse("packet probe { x: u8 }").unwrap()).unwrap()
}

#[test]
fn trailing_bytes_are_an_error() {
    let err = decode_packet(&resolved(), "probe", &[0x01, 0x02], true).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("1 of 2 bytes"), "{text}");
}

#[test]
fn a_packet_that_uses_every_byte_decodes() {
    let dec = decode_packet(&resolved(), "probe", &[0x07], true).unwrap();
    assert_eq!(dec.get("x"), Some(&bps::Value::Int(7)));
    assert_eq!(dec.len(), 1);
}

#[test]
fn the_strict_error_names_the_packet_and_where_the_unread_bytes_start() {
    // Python raises `DecodeError(f"... after packet {name!r}", offset=cur.pos)`; the offset is
    // where the unread bytes begin, here after the one byte `probe` reads.
    let err = decode_packet(&resolved(), "probe", &[0x01, 0x02, 0x03], true).unwrap_err();
    assert_eq!(err.to_string(), "2 of 3 bytes left unread after packet 'probe' (offset 1)");
}
