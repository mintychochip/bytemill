//! Interpreter, porting `bps/interp/`.

pub mod cursor;
pub mod engine;
pub mod expr;

pub use crate::spans::{
    FieldSpans, Span, SpanNode, decode_frame_payload_spans, decode_packet_spans, hexdump,
    spans_json,
};
pub use engine::{
    decode_frame_payload, decode_packet, decode_packet_eof_skips, decode_type_ref, encode_bare,
    encode_packet,
};
pub use expr::eval_expr;
