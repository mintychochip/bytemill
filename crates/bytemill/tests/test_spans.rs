//! Byte span recording for decode and hexdump tests.

mod common;

use bytemill::interp::{
    decode_frame_payload_spans, decode_packet, decode_packet_spans, hexdump, spans_json,
};
use bytemill::spans::SpanNode;
use bytemill::wiregen;
use bytemill::{BmlResult, ResolvedSchema, Value, analyze};
use common::{load_minecraft, load_with_extra, schema_path};

fn load_hytale() -> BmlResult<ResolvedSchema> {
    analyze(&bytemill::load_schema(&[schema_path(
        "hytale/2026_02_17.bml",
    )])?)
}

fn load_minecraft_wiki() -> BmlResult<ResolvedSchema> {
    analyze(&bytemill::load_schema(&[schema_path(
        "minecraft_26_3_wiki.bml",
    )])?)
}

#[test]
fn animate_packet_spans_match_hand_checked() {
    let schema = load_minecraft().unwrap();
    let data = [0xac, 0x02, 0x02];
    let (env, spans) = decode_packet_spans(&schema, "animate", &data, true).unwrap();
    let plain = decode_packet(&schema, "animate", &data, true).unwrap();
    assert_eq!(env, plain);

    let e = spans.get("entity_id").unwrap();
    if let SpanNode::Leaf(s) = e {
        assert_eq!(s.offset, 0);
        assert_eq!(s.len, 2);
    } else {
        panic!("entity_id not leaf");
    }
    let a = spans.get("animation").unwrap();
    if let SpanNode::Leaf(s) = a {
        assert_eq!(s.offset, 2);
        assert_eq!(s.len, 1);
    } else {
        panic!("animation not leaf");
    }
    assert_eq!(spans.len(), 2);
}

#[test]
fn handshake_frame_spans_include_frame_fields_and_inner() {
    // from README example
    let bytes = [
        0x10, 0x00, 0x89, 0x06, 0x09, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x68, 0x6f, 0x73, 0x74, 0x63,
        0xdd, 0x01,
    ];
    let schema = load_minecraft_wiki().unwrap();
    let (id, env, spans) =
        decode_frame_payload_spans(&schema, "handshaking", "serverbound", &bytes).unwrap();
    assert_eq!(id, 0x00);

    // frame fields
    let l = spans.get("length").unwrap();
    if let SpanNode::Leaf(s) = l {
        assert_eq!(s.offset, 0);
        assert_eq!(s.len, 1);
    } else {
        panic!("length not leaf");
    }
    let i = spans.get("id").unwrap();
    if let SpanNode::Leaf(s) = i {
        assert_eq!(s.offset, 1);
        assert_eq!(s.len, 1);
    } else {
        panic!("id not leaf");
    }

    // inner field server_address present (leaf covering length+data)
    let sa = spans.get("server_address").unwrap();
    assert!(matches!(sa, SpanNode::Leaf(_)));
    // env still has only packet/fields
    assert!(env.contains_key("packet"));
    assert!(env.contains_key("fields"));
}

fn check_tiling(schema: &ResolvedSchema, packet: &str, seed: u64) -> usize {
    let data = match wiregen::generate(schema, packet, seed) {
        Ok(d) => d,
        Err(_) => return 0, // skip bad gen
    };
    let plain = match decode_packet(schema, packet, &data, true) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    let (spanned_env, spans) =
        decode_packet_spans(schema, packet, &data, true).unwrap_or_else(|e| {
            panic!("{packet} (seed {seed}): plain decode succeeded but spanned decode failed: {e}")
        });
    assert_eq!(plain, spanned_env);

    // collect top level spans sorted by offset
    let mut tops: Vec<_> = spans.iter().map(|(_, n)| n.span()).collect();
    tops.sort_by_key(|s| s.offset);
    let mut p = 0;
    for s in &tops {
        assert_eq!(s.offset, p, "gap or overlap at {}", packet);
        p += s.len;
    }
    assert_eq!(p, data.len(), "did not cover for {}", packet);

    // sub records and lists tile their span
    fn check_sub(node: &SpanNode) {
        match node {
            SpanNode::Record { span, fields } => {
                let mut ch: Vec<_> = fields.values().map(|n| n.span()).collect();
                ch.sort_by_key(|s| s.offset);
                let mut q = span.offset;
                for c in &ch {
                    assert_eq!(c.offset, q);
                    q += c.len;
                }
                for (_, f) in fields.iter() {
                    check_sub(f);
                }
            }
            SpanNode::List { span, items } => {
                let mut ch: Vec<_> = items.iter().map(|n| n.span()).collect();
                ch.sort_by_key(|s| s.offset);
                let mut q = span.offset;
                for c in &ch {
                    assert_eq!(c.offset, q);
                    q += c.len;
                }
                for it in items {
                    check_sub(it);
                }
            }
            _ => {}
        }
    }
    for (_, n) in spans.iter() {
        check_sub(n);
    }
    1
}

#[test]
fn tiling_invariant_over_hytale_and_minecraft() {
    let h = load_hytale().unwrap();
    let m = load_minecraft_wiki().unwrap();
    let mut h_verified = 0usize;
    let mut m_verified = 0usize;
    for (schema, name) in [(&h, "hytale"), (&m, "mc")] {
        for pkt_name in schema.packets.keys() {
            for seed in 0..4u64 {
                let v = check_tiling(schema, pkt_name, seed);
                if name == "hytale" {
                    h_verified += v;
                } else {
                    m_verified += v;
                }
            }
        }
    }
    assert!(h_verified >= 16, "h only verified {}", h_verified);
    assert!(m_verified >= 924, "m only verified {}", m_verified);
}

#[test]
fn spread_inlined_and_computed_absent_if_no_span() {
    let src = r#"
    encoding point { x: u8, y: u8 }
    packet P {
      ...point
      flag: u8
      if flag > 0 { extra: u8 }
      c = 9
    }
    "#;
    let schema = load_with_extra(src).unwrap();
    // data for x y flag=0 , c computed no wire
    let data = [10, 20, 0];
    let (env, spans) = decode_packet_spans(&schema, "P", &data, true).unwrap();
    let xs = spans.get("x").unwrap();
    if let SpanNode::Leaf(s) = xs {
        assert_eq!(s.offset, 0);
        assert_eq!(s.len, 1);
    } else {
        panic!("x not leaf");
    }
    let ys = spans.get("y").unwrap();
    if let SpanNode::Leaf(s) = ys {
        assert_eq!(s.offset, 1);
        assert_eq!(s.len, 1);
    } else {
        panic!("y not leaf");
    }
    let fs = spans.get("flag").unwrap();
    if let SpanNode::Leaf(s) = fs {
        assert_eq!(s.offset, 2);
        assert_eq!(s.len, 1);
    } else {
        panic!("flag not leaf");
    }
    assert!(!spans.contains_key("extra")); // if false
    assert!(!spans.contains_key("c")); // computed
    // spread inlined, not a _spread key
    assert!(!spans.contains_key("_spread"));
    assert_eq!(env.len(), 4); // x y flag c
}

#[test]
fn list_of_records_and_json_shape() {
    let src = r#"
    packet L {
      count: u8
      items: { v: u8 }[] count = count
    }
    "#;
    let schema = load_with_extra(src).unwrap();
    let data = [2, 3, 4];
    let (_env, spans) = decode_packet_spans(&schema, "L", &data, true).unwrap();
    let items = spans.get("items").unwrap();
    if let SpanNode::List { items: subs, .. } = items {
        assert_eq!(subs.len(), 2);
        if let SpanNode::Record { span, .. } = &subs[0] {
            assert_eq!(span.offset, 1);
            assert_eq!(span.len, 1);
        } else {
            panic!("item 0 not record");
        }
    } else {
        panic!("not list");
    }
    let j = spans_json(&spans);
    if let Value::Map(m) = &j {
        assert!(m.contains_key("count"));
        let it = m.get("items").unwrap();
        if let Value::Map(mi) = it {
            assert!(mi.contains_key("items"));
            if let Value::List(ilist) = mi.get("items").unwrap() {
                assert_eq!(ilist.len(), 2);
            } else {
                panic!("items not list");
            }
        } else {
            panic!("items not map");
        }
    } else {
        panic!("top json not map");
    }
}

#[test]
fn hexdump_tiles_and_unread_and_empty() {
    let src2 = "packet Q { a: u8, b: u8 }";
    let schema2 = load_with_extra(src2).unwrap();
    let data = [0x01, 0x02, 0x03]; // extra for nonstrict
    let (env, spans) = decode_packet_spans(&schema2, "Q", &data, false).unwrap();
    let dump = hexdump(&data, &spans, &env);
    assert!(dump.contains("unread"));
    assert!(dump.lines().count() >= 2);

    // zero len leaf
    let src3 = "packet Z { e: bytes(0) }";
    let s3 = load_with_extra(src3).unwrap();
    let d3 = [];
    let (e3, sp3) = decode_packet_spans(&s3, "Z", &d3, true).unwrap();
    let dm = hexdump(&d3, &sp3, &e3);
    assert!(dm.contains("(empty)"));
}

#[test]
fn spans_json_leaf_record_list() {
    let src = "packet J { x: u8, r: { y: u8 }, l: { z: u8 }[] count = 1 }";
    let schema = load_with_extra(src).unwrap();
    let data = [1, 2, 3];
    let (_, spans) = decode_packet_spans(&schema, "J", &data, true).unwrap();
    let j = spans_json(&spans);
    if let Value::Map(m) = &j {
        let lx = m.get("x").unwrap();
        if let Value::Map(mx) = lx {
            if let Value::Int(off) = mx.get("offset").unwrap() {
                assert_eq!(*off, 0);
            } else {
                panic!("x offset not int");
            }
            if let Value::Int(lenv) = mx.get("len").unwrap() {
                assert_eq!(*lenv, 1);
            } else {
                panic!("x len not int");
            }
        } else {
            panic!("x not map");
        }
        let lr = m.get("r").unwrap();
        if let Value::Map(mr) = lr {
            if let Value::Int(off) = mr.get("offset").unwrap() {
                assert_eq!(*off, 1);
            } else {
                panic!("r offset not");
            }
            assert!(mr.contains_key("fields"));
        } else {
            panic!("r not map");
        }
        let ll = m.get("l").unwrap();
        if let Value::Map(ml) = ll {
            if let Value::Int(lenv) = ml.get("len").unwrap() {
                assert_eq!(*lenv, 1);
            } else {
                panic!("l len not");
            }
            if let Value::List(ilist) = ml.get("items").unwrap() {
                assert_eq!(ilist.len(), 1);
            } else {
                panic!("l items not list");
            }
        } else {
            panic!("l not map");
        }
    } else {
        panic!("top not map");
    }
}
