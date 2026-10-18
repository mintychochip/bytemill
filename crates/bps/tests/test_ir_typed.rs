//! The typed program builds for every shipped schema, and pins the rules
//! printers rely on: naming, scoping, optional fields, fallback groups, sums,
//! enums, aliases, fragments, and string rebinding.

use bps::ir::typed::{
    BinOp, Bind, CodecKind, Expr, IntTy, ParamRole, Program, ReadOp, Stmt, Ty, TypeKind, typed,
};

fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn load(rel: &str) -> Program {
    let resolved = bps::analyze(&bps::load_schema(&[repo(rel)]).unwrap()).unwrap();
    typed(&resolved).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn source(text: &str) -> Program {
    let std = std::fs::read_to_string(repo("schemas/std/integers.bps")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap();
    let resolved = bps::analyze(&bps::parse(&(std + text)).unwrap()).unwrap();
    typed(&resolved).unwrap()
}

fn codec<'a>(p: &'a Program, ir: &str) -> &'a bps::ir::typed::Codec {
    &p.codecs[p.codec_by_ir(ir).unwrap_or_else(|| panic!("no codec {ir}"))]
}

/// Every statement of a body, nested ones included.
fn walk<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a Stmt>) {
    for s in stmts {
        out.push(s);
        match s {
            Stmt::If { then, els, .. } => {
                walk(then, out);
                walk(els, out);
            }
            Stmt::Match { arms, other, .. } => {
                for a in arms {
                    walk(&a.body, out);
                }
                if let Some(o) = other {
                    walk(o, out);
                }
            }
            Stmt::Repeat { body, .. } | Stmt::ForEach { body, .. } => walk(body, out),
            _ => {}
        }
    }
}

fn all(stmts: &[Stmt]) -> Vec<&Stmt> {
    let mut out = Vec::new();
    walk(stmts, &mut out);
    out
}

#[test]
fn builds_for_every_shipped_schema() {
    let mut roots = vec![
        "schemas/minecraft_26_3_wiki.bps".to_string(),
        "schemas/hytale/2026_02_17.bps".to_string(),
    ];
    let mut versions: Vec<String> = std::fs::read_dir(repo("schemas/versions"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('v') && n.ends_with(".bps"))
        .map(|n| format!("schemas/versions/{n}"))
        .collect();
    versions.sort();
    assert!(versions.len() >= 39, "{versions:?}");
    roots.extend(versions);
    for root in roots {
        let p = load(&root);
        assert!(!p.codecs.is_empty(), "{root}");
        assert!(!p.routes.is_empty(), "{root}");
        for c in &p.codecs {
            assert_eq!(c.decode.params.len(), c.params.len(), "{root}: {}", c.ir);
            assert_eq!(c.encode.params.len(), c.params.len(), "{root}: {}", c.ir);
            for body in [&c.decode, &c.encode] {
                let mut seen = std::collections::HashSet::new();
                for l in &body.locals {
                    assert!(
                        seen.insert(l.name.snake()),
                        "{root}: {} reuses local {}",
                        c.ir,
                        l.name
                    );
                }
            }
        }
    }
}

#[test]
fn names_are_unique_and_structural() {
    let p = load("schemas/minecraft_26_3_wiki.bps");
    let mut seen = std::collections::HashSet::new();
    for t in &p.types {
        assert!(
            seen.insert(t.name.pascal()),
            "type {} twice",
            t.name.pascal()
        );
    }
    let mut seen = std::collections::HashSet::new();
    for c in &p.codecs {
        assert!(
            seen.insert(c.name.pascal()),
            "codec {} twice",
            c.name.pascal()
        );
    }
    // A nested block is named by its parent and field, a block arm by its sum
    // and tag.
    let props = p.type_by_ir("game_profile_properties_block").unwrap();
    assert_eq!(p.types[props].name.pascal(), "GameProfileProperties");
    let arm = p.type_by_ir("meta_entry_value_arm6").unwrap();
    assert_eq!(p.types[arm].name.pascal(), "MetaEntryValueTag6");
    let sum = p.type_by_ir("meta_entry_value").unwrap();
    let TypeKind::Sum { variants } = &p.types[sum].kind else {
        panic!()
    };
    assert_eq!(variants[0].name.snake(), "tag0");
}

#[test]
fn a_fragment_gets_no_codec() {
    let p = load("schemas/minecraft_26_3_wiki.bps");
    assert!(p.codec_by_ir("nbt_payload").is_none());
    assert!(p.type_by_ir("nbt_payload_value").is_none());
    assert!(p.codec_by_ir("nbt").is_some());
}

#[test]
fn scalar_codecs_zero_only_conditionally_read_wire_names() {
    let p = load("schemas/std/varint.bps");
    let c = codec(&p, "varint");
    assert_eq!(c.kind, CodecKind::Scalar);
    assert_eq!(c.value, Ty::Int(IntTy::I32));
    let declared: Vec<String> = c
        .decode
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Declare { local, zero: true } => Some(c.decode.locals[*local].name.snake()),
            _ => None,
        })
        .collect();
    // `b1`..`b4` are read under `if`s and checked after them: they start at
    // zero. `b0` is read unconditionally first, so its read declares it.
    assert_eq!(declared, ["b1", "b2", "b3", "b4"]);
    let first_read = c
        .decode
        .stmts
        .iter()
        .find_map(|s| match s {
            Stmt::Read { dst, .. } => Some(dst),
            _ => None,
        })
        .unwrap();
    assert_eq!(c.decode.locals[first_read.local].name.snake(), "b0");
    assert!(first_read.declare);
    assert!(matches!(
        c.decode.stmts.last(),
        Some(Stmt::Return(Expr::Cast {
            to: Ty::Int(IntTy::I32),
            ..
        }))
    ));
}

#[test]
fn narrow_operations_stay_narrow_and_wide_ones_widen() {
    let p = load("schemas/std/varint.bps");
    let c = codec(&p, "varint");
    // `(b0 & 128) != 0` compares a u8 without a cast.
    let guard = all(&c.decode.stmts)
        .into_iter()
        .find_map(|s| match s {
            Stmt::If { cond, .. } => Some(cond.clone()),
            _ => None,
        })
        .unwrap();
    let Expr::Binary {
        op: BinOp::Ne, lhs, ..
    } = guard
    else {
        panic!("{guard:?}")
    };
    assert!(
        matches!(
            *lhs,
            Expr::Binary {
                op: BinOp::BitAnd,
                ty: Ty::Int(IntTy::U8),
                wraps: false,
                ..
            }
        ),
        "{lhs:?}"
    );
    // `(b1 & 127) << 7` leaves u8, so it widens to i64.
    let dump = p.dump_codec(p.codec_by_ir("varint").unwrap());
    assert!(dump.contains("(i64((b1 & 127u8)) << 7i64)"), "{dump}");
}

#[test]
fn strings_rebind_bytes_into_a_fresh_local() {
    let p = load("schemas/minecraft_core.bps");
    let c = codec(&p, "string");
    assert_eq!(c.kind, CodecKind::String);
    assert_eq!(c.params.len(), 1);
    assert_eq!(c.params[0].role, ParamRole::Arg);
    let stmts = all(&c.decode.stmts);
    let raw = stmts.iter().find_map(|s| match s {
        Stmt::Read {
            dst,
            op: ReadOp::Bytes(_),
        } => Some(dst.local),
        _ => None,
    });
    let text = stmts.iter().find_map(|s| match s {
        Stmt::Utf8 {
            dst,
            src: Expr::Local(src),
        } => Some((dst.local, *src)),
        _ => None,
    });
    let (text, src) = text.unwrap();
    assert_eq!(Some(src), raw);
    assert_ne!(text, src);
    assert_eq!(c.decode.locals[text].ty, Ty::Str);
}

#[test]
fn optional_fields_read_in_their_block_use_the_decoded_value() {
    let p = source(
        "packet p {\n  mode: u8\n  if mode == 0 {\n    has_x: boolean\n    if has_x {\n      x: u8\n    }\n  }\n}\n",
    );
    let c = codec(&p, "p");
    let stmts = all(&c.decode.stmts);
    // `has_x` decodes into a non-optional local, stored into the optional field
    // local, and the inner guard reads the non-optional one.
    let (value, field) = stmts
        .iter()
        .find_map(|s| match s {
            Stmt::Let {
                dst: Bind {
                    local, some: true, ..
                },
                value: Expr::Local(v),
            } => Some((*v, *local)),
            _ => None,
        })
        .unwrap();
    assert!(c.decode.locals[field].optional);
    assert!(!c.decode.locals[value].optional);
    let reads_value = stmts.iter().any(|s| match s {
        Stmt::If { cond, .. } => format!("{cond:?}").contains(&format!("Local({value})")),
        _ => false,
    });
    assert!(reads_value, "{}", p.dump_codec(p.codec_by_ir("p").unwrap()));
    // On encode, the optional field is unwrapped before it is written.
    let enc = all(&c.encode.stmts);
    assert!(
        enc.iter()
            .any(|s| matches!(s, Stmt::Unwrap { field, .. } if field == "has_x"))
    );
}

#[test]
fn fallback_groups_skip_when_no_bytes_remain() {
    let p = source("packet p {\n  flag: u8\n  if flag != 0 {\n    x: u8\n  }\n}\n");
    let c = codec(&p, "p");
    let cond = all(&c.decode.stmts)
        .into_iter()
        .find_map(|s| match s {
            Stmt::If { cond, .. } => Some(cond.clone()),
            _ => None,
        })
        .unwrap();
    let Expr::Binary {
        op: BinOp::And,
        rhs,
        ..
    } = cond
    else {
        panic!("{cond:?}")
    };
    assert!(
        matches!(*rhs, Expr::Binary { op: BinOp::Gt, lhs, .. } if matches!(*lhs, Expr::Remaining))
    );
}

#[test]
fn match_fields_lift_into_sum_codecs_chosen_by_a_tag() {
    let p = source(
        "packet p {\n  kind: u8\n  v: match kind {\n    0 => u8\n    1 | 2 => { a: u8, b: u8 }\n    _ => i8\n  }\n}\n",
    );
    let sum = p.type_by_ir("p_v").unwrap();
    let TypeKind::Sum { variants } = &p.types[sum].kind else {
        panic!()
    };
    let names: Vec<String> = variants.iter().map(|v| v.name.snake()).collect();
    assert_eq!(names, ["tag0", "tag1", "other"]);
    assert_eq!(variants[1].tags, [1, 2]);
    let sc = codec(&p, "p_v");
    assert_eq!(sc.kind, CodecKind::Sum);
    assert_eq!(sc.params[0].role, ParamRole::Tag);
    // The packet passes `kind` as the tag; encode checks the variant.
    let pc = codec(&p, "p");
    assert!(all(&pc.decode.stmts).iter().any(
        |s| matches!(s, Stmt::Decode { codec, .. } if *codec == p.codec_by_ir("p_v").unwrap())
    ));
    assert!(
        all(&sc.encode.stmts)
            .iter()
            .any(|s| matches!(s, Stmt::ExpectVariant { .. }))
    );
    // The block arm is its own record codec.
    let arm = p.type_by_ir("p_v_arm1").unwrap();
    assert_eq!(p.types[arm].name.pascal(), "PVTag1");
    assert!(p.types[arm].codec.is_some());
}

#[test]
fn nested_blocks_capture_the_parent_values_they_read() {
    let p = source("packet p {\n  n: u8\n  items: {\n    data: bytes(n)\n  }[] count = 2\n}\n");
    let item = codec(&p, "p_items_block");
    assert_eq!(item.params.len(), 1);
    assert_eq!(item.params[0].role, ParamRole::Capture { ir: "n".into() });
    assert_eq!(item.params[0].ty, Ty::Int(IntTy::U8));
}

#[test]
fn negation_flips_integer_comparisons_but_not_float_ones() {
    let p = source("packet p {\n  n: u8\n  f: f32be\n  g: f32be\n  require n == 3 && f < g\n}\n");
    let c = codec(&p, "p");
    let cond = all(&c.decode.stmts)
        .into_iter()
        .find_map(|s| match s {
            Stmt::Require { cond, .. } => Some(cond.clone()),
            _ => None,
        })
        .unwrap();
    let Expr::Binary {
        op: BinOp::Or,
        lhs,
        rhs,
        ..
    } = p.negated(&c.decode, &cond)
    else {
        panic!("{cond:?}")
    };
    assert!(
        matches!(*lhs, Expr::Binary { op: BinOp::Ne, .. }),
        "{lhs:?}"
    );
    // `!(f < g)` is not `f >= g` when either is NaN.
    assert!(matches!(*rhs, Expr::Not(_)), "{rhs:?}");
}

#[test]
fn enums_cast_from_their_base() {
    let p = load("schemas/minecraft_26_3.bps");
    let c = codec(&p, "intent");
    assert_eq!(c.kind, CodecKind::Enum);
    let Some(Stmt::Return(Expr::Cast {
        to: Ty::Named(t), ..
    })) = c.decode.stmts.last()
    else {
        panic!("{}", p.dump_codec(p.codec_by_ir("intent").unwrap()));
    };
    assert!(matches!(
        p.types[*t].kind,
        TypeKind::Enum {
            base: IntTy::I32,
            ..
        }
    ));
}

#[test]
#[ignore = "prints the typed program of a few 26.3 codecs"]
fn dump_samples() {
    let p = load("schemas/minecraft_26_3_wiki.bps");
    for ir in [
        "varint",
        "varlong",
        "string",
        "boolean",
        "u16be",
        "position",
        "game_profile",
        "game_profile_properties_block",
        "handshake",
        "set_objective",
        "meta_entry",
        "meta_entry_value",
        "meta_entry_value_arm6",
        "entity_metadata",
        "nbt",
        "nbt_compound_body",
        "nbt_list_body",
        "nbt_list_body_items_block",
        "player_info_update",
        "intent",
    ] {
        match p.codec_by_ir(ir) {
            Some(c) => println!("{}", p.dump_codec(c)),
            None => println!("-- no codec {ir}"),
        }
    }
}
