//! IR, interpreter and typed IR for the new little-endian float primitives.

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::ir::typed::{ReadOp, Stmt, Ty, WriteOp, typed};
use bytemill::ir::{Stmt as IrStmt, WireOp, exec_decode, exec_encode, lower};
use bytemill::{Value, analyze, parse};

#[test]
fn f32le_f64le_lower_typed_exec_dump_and_interp_agree() {
    let src = r#"
packet probe {
    a: f32le
    b: f64le
    c: f32be
    d: f64be
    n: u8
    fs: f32le[] count = n
}
"#;
    let resolved = analyze(&parse(src).unwrap()).unwrap();
    let ir = lower(&resolved).unwrap();

    // lower registers the two new builtins with correct WireOp and Storage
    assert!(ir.types.contains_key("f32le"));
    assert!(ir.types.contains_key("f64le"));
    let p32 = ir.decode.get("f32le").unwrap();
    assert_eq!(p32.body.len(), 1);
    let f32le_reads = p32
        .body
        .iter()
        .filter(|s| {
            matches!(
                s,
                IrStmt::Read {
                    op: WireOp::F32le,
                    ..
                }
            )
        })
        .count();
    assert!(
        f32le_reads >= 1,
        "f32le decode proc must contain Read F32le (counted {})",
        f32le_reads
    );
    let p64 = ir.decode.get("f64le").unwrap();
    assert!(p64.body.iter().any(|s| matches!(
        s,
        IrStmt::Read {
            op: WireOp::F64le,
            ..
        }
    )));
    let e32 = ir.encode.get("f32le").unwrap();
    assert!(e32.body.iter().any(|s| matches!(
        s,
        IrStmt::Write {
            op: WireOp::F32le,
            ..
        }
    )));
    let e64 = ir.encode.get("f64le").unwrap();
    assert!(e64.body.iter().any(|s| matches!(
        s,
        IrStmt::Write {
            op: WireOp::F64le,
            ..
        }
    )));

    // packet lowering emits CallDecode to the le builtins (at least the two scalars)
    let probe_ir = ir.decode.get("probe").unwrap();
    let le_builtin_calls = probe_ir.body.iter().filter(|s| {
        matches!(s, IrStmt::CallDecode { callee, .. } if callee.0 == "f32le" || callee.0 == "f64le")
    }).count();
    assert!(
        le_builtin_calls >= 2,
        "probe must CallDecode the two le builtins (saw {})",
        le_builtin_calls
    );

    // typed program
    let tp = typed(&resolved).unwrap();
    let pidx = tp.codec_by_ir("probe").expect("probe codec present");
    let probe = &tp.codecs[pidx];

    let le_reads_typed = probe
        .decode
        .stmts
        .iter()
        .filter(|s| {
            matches!(
                s,
                Stmt::Read {
                    op: ReadOp::F32Le | ReadOp::F64Le,
                    ..
                }
            )
        })
        .count();
    assert!(
        le_reads_typed >= 2,
        "typed decode must surface >=2 F32Le/F64Le reads (saw {})",
        le_reads_typed
    );

    let le_writes_typed = probe
        .encode
        .stmts
        .iter()
        .filter(|s| {
            matches!(
                s,
                Stmt::Write {
                    op: WriteOp::F32Le | WriteOp::F64Le,
                    ..
                }
            )
        })
        .count();
    assert!(
        le_writes_typed >= 2,
        "typed encode must surface >=2 F*Le writes (saw {})",
        le_writes_typed
    );

    // endianness is wire only; value types stay F32/F64
    let f32_count = probe
        .decode
        .locals
        .iter()
        .filter(|l| l.ty == Ty::F32)
        .count()
        + probe
            .encode
            .locals
            .iter()
            .filter(|l| l.ty == Ty::F32)
            .count();
    assert!(
        f32_count >= 3,
        "a, fs[0], fs[1] and symmetric encode use Ty::F32 (saw {})",
        f32_count
    );
    let f64_count = probe
        .decode
        .locals
        .iter()
        .filter(|l| l.ty == Ty::F64)
        .count()
        + probe
            .encode
            .locals
            .iter()
            .filter(|l| l.ty == Ty::F64)
            .count();
    assert!(
        f64_count >= 2,
        "b and symmetric encode use Ty::F64 (saw {})",
        f64_count
    );

    // dump prints the le names for the ops
    let dump = tp.dump_codec(pidx);
    let le_names = dump.matches("f32le").count() + dump.matches("f64le").count();
    assert!(
        le_names >= 2,
        "typed dump must contain the le primitive names (saw {})",
        le_names
    );

    // IR interpreter roundtrips known values using le bytes
    let f32_1p5_le: &[u8] = &[0x00, 0x00, 0xc0, 0x3f];
    let v = exec_decode(&ir, "f32le", &[], f32_1p5_le).unwrap();
    assert!(
        matches!(&v, Value::Float(f) if (*f - 1.5).abs() < 1e-9),
        "f32le decode 1.5, got {:?}",
        v
    );
    let back = exec_encode(&ir, "f32le", &[], &v).unwrap();
    assert_eq!(back, f32_1p5_le.to_vec());

    let f64_2p25_le: &[u8] = &[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x40];
    let v64 = exec_decode(&ir, "f64le", &[], f64_2p25_le).unwrap();
    assert!(matches!(&v64, Value::Float(f) if (*f - 2.25).abs() < 1e-9));
    assert_eq!(
        exec_encode(&ir, "f64le", &[], &v64).unwrap(),
        f64_2p25_le.to_vec()
    );

    // same numeric value on BE vs LE yields different wire
    let f32_1p5_be: &[u8] = &[0x3f, 0xc0, 0x00, 0x00];
    let vbe = exec_decode(&ir, "f32be", &[], f32_1p5_be).unwrap();
    let le_of_be_val = exec_encode(&ir, "f32le", &[], &vbe).unwrap();
    assert_ne!(
        le_of_be_val,
        f32_1p5_be.to_vec(),
        "LE and BE encodings of 1.5 must differ in bytes"
    );

    // differential: interp and ir::exec agree on decode and on encode bytes (hand payload, list of le)
    // layout: f32le a, f64le b, f32be c, f64be d, u8 n, f32le[ n ]
    let payload: Vec<u8> = vec![
        0x00, 0x00, 0xc0, 0x3f, // a=1.5 le
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x40, // b=2.25 le
        0x3f, 0xc0, 0x00, 0x00, // c=1.5 be
        0x40, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // d=2.25 be
        2,    // n=2
        0x00, 0x00, 0x00, 0x00, // fs[0]=0.0 le
        0x00, 0x00, 0x80, 0x3f, // fs[1]=1.0 le
    ];
    let env_from_interp = decode_packet(&resolved, "probe", &payload, true).unwrap();
    let val_from_ir = exec_decode(&ir, "probe", &[], &payload).unwrap();
    let Value::Map(m) = &val_from_ir else {
        panic!("record decodes to Map")
    };
    let scalar_agree = ["a", "b", "c", "d", "n"]
        .iter()
        .filter(|&k| m.get(k) == env_from_interp.get(k))
        .count();
    assert!(
        scalar_agree >= 5,
        "interp and ir exec agree on all 5 scalar fields (agreed {})",
        scalar_agree
    );
    // list also
    match (m.get("fs"), env_from_interp.get("fs")) {
        (Some(Value::List(fs)), Some(Value::List(fsi))) => {
            assert_eq!(fs.len(), 2, "fs list len");
            assert_eq!(fsi.len(), 2);
        }
        _ => panic!("fs must be list in both"),
    }

    // encode from both sides agree, and equal input payload
    let enc_interp = encode_packet(&resolved, "probe", &env_from_interp).unwrap();
    let enc_ir = exec_encode(&ir, "probe", &[], &val_from_ir).unwrap();
    assert_eq!(enc_interp, enc_ir);
    assert_eq!(enc_interp, payload);

    // zero-length list case
    let mut env0 = env_from_interp.clone();
    env0.insert("n", Value::Int(0));
    env0.insert("fs", Value::list(vec![]));
    let p0 = encode_packet(&resolved, "probe", &env0).unwrap();
    let v0i = exec_decode(&ir, "probe", &[], &p0).unwrap();
    let e0i = decode_packet(&resolved, "probe", &p0, true).unwrap();
    assert_eq!(e0i.get("n"), Some(&Value::Int(0)));
    if let Value::Map(m0) = v0i {
        assert_eq!(m0.get("n"), Some(&Value::Int(0)));
    } else {
        panic!("still map");
    }
}
