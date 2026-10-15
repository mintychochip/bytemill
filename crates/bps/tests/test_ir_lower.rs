//! Scalar encodings lower to calls, not inlined byte walks.

use bps::ast::Expr;
use bps::ir::{Stmt, Storage, TypeDef, WireOp, lower};

fn repo(rel: &str) -> String {
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel);
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn i32be_calls_u32be_and_u8_is_one_read() {
    let source = repo("schemas/std/integers.bps") + "packet probe { n: i32be }\n";
    let resolved = bps::analyze(&bps::parse(&source).unwrap()).unwrap();
    let ir = lower(&resolved).unwrap();

    assert!(matches!(
        ir.types.get("i32be"),
        Some(TypeDef::Scalar(Storage::I32))
    ));

    let decode = ir.decode.get("i32be").unwrap();
    assert!(
        decode
            .body
            .iter()
            .any(|s| matches!(s, Stmt::CallDecode { callee, .. } if callee.0 == "u32be")),
        "i32be decode should call u32be, got {:?}",
        decode.body
    );
    assert!(
        !decode.body.iter().any(|s| matches!(s, Stmt::Read { .. })),
        "i32be must not inline u8 reads"
    );

    let u8_decode = ir.decode.get("u8").unwrap();
    assert_eq!(u8_decode.body.len(), 1);
    assert!(matches!(u8_decode.body[0], Stmt::Read { ref op, .. } if matches!(op, WireOp::U8)));

    let u8_encode = ir.encode.get("u8").unwrap();
    let expect = Expr::Binop(
        "&".into(),
        Box::new(Expr::Ident("value".into())),
        Box::new(Expr::Int(0xFF)),
    );
    assert!(
        matches!(&u8_encode.body[0], Stmt::Write { op: WireOp::U8, value } if value == &expect)
    );
}

fn std_schema() -> bps::ast::ResolvedSchema {
    let source = repo("schemas/std/integers.bps") + &repo("schemas/std/varint.bps");
    bps::analyze(&bps::parse(&source).unwrap()).unwrap()
}

#[test]
fn scalars_roundtrip_known_bytes() {
    use bps::Value;
    use bps::ir::{exec_decode, exec_encode};
    let ir = lower(&std_schema()).unwrap();

    let i8_neg = exec_encode(&ir, "i8", &[], &Value::Int(-1)).unwrap();
    assert_eq!(i8_neg, vec![0xff]);
    assert_eq!(
        exec_decode(&ir, "i8", &[], &i8_neg).unwrap(),
        Value::Int(-1)
    );

    let v300 = exec_encode(&ir, "varint", &[], &Value::Int(300)).unwrap();
    assert_eq!(v300, vec![0xac, 0x02]);
    assert_eq!(
        exec_decode(&ir, "varint", &[], &v300).unwrap(),
        Value::Int(300)
    );

    let vneg = exec_encode(&ir, "varint", &[], &Value::Int(-1)).unwrap();
    assert_eq!(vneg, vec![0xff, 0xff, 0xff, 0xff, 0x0f]);
    assert_eq!(
        exec_decode(&ir, "varint", &[], &vneg).unwrap(),
        Value::Int(-1)
    );

    let yes = exec_encode(&ir, "boolean", &[], &Value::Bool(true)).unwrap();
    assert_eq!(yes, vec![1]);
    assert_eq!(
        exec_decode(&ir, "boolean", &[], &yes).unwrap(),
        Value::Bool(true)
    );
}

fn string_schema() -> bps::ast::ResolvedSchema {
    let source = repo("schemas/std/integers.bps")
        + &repo("schemas/std/varint.bps")
        + r#"
encoding string(max_chars = 32767) {
    byte_len: varint
    value: bytes(byte_len) as utf8
    require utf16_units(value) <= max_chars
}
encoding identifier = { ...string(32767) }
packet probe { s: string(16)  id: identifier }
"#;
    bps::analyze(&bps::parse(&source).unwrap()).unwrap()
}

#[test]
fn string_shaped_is_one_string_not_a_record() {
    use bps::Value;
    use bps::ir::{TypeDef, exec_decode, exec_encode};
    let ir = lower(&string_schema()).unwrap();
    assert!(matches!(ir.types.get("string"), Some(TypeDef::String)));
    assert!(matches!(ir.types.get("identifier"), Some(TypeDef::String)));

    let hi = Value::str("hi");
    let enc = exec_encode(&ir, "string", &[Value::Int(16)], &hi).unwrap();
    assert_eq!(enc, vec![0x02, b'h', b'i']);
    assert_eq!(
        exec_decode(&ir, "string", &[Value::Int(16)], &enc).unwrap(),
        hi
    );

    let mut too_long = vec![0x11];
    too_long.extend(std::iter::repeat_n(b'a', 17));
    assert!(exec_decode(&ir, "string", &[Value::Int(16)], &too_long).is_err());

    let id = exec_encode(&ir, "identifier", &[], &hi).unwrap();
    assert_eq!(id, enc);
}

#[test]
fn minecraft_core_string_shapes_are_exactly_these() {
    use bps::ir::TypeDef;
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../"))
        .join("schemas/minecraft_core.bps");
    let resolved = bps::analyze(&bps::load_schema(&[path]).unwrap()).unwrap();
    let ir = lower(&resolved).unwrap();
    let mut names: Vec<&str> = ir
        .types
        .iter()
        .filter(|(_, def)| matches!(def, TypeDef::String))
        .map(|(name, _)| name)
        .collect();
    names.sort();
    assert_eq!(names, vec!["identifier", "nbt_string", "string"]);
}
