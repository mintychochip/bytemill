//! Storage classes assigned from a scalar yield. Spec section 4.3.

use bytemill::ast::Expr;
use bytemill::ir::{storage_of, Storage};
use std::collections::HashMap;

fn locals(pairs: &[(&str, Storage)]) -> HashMap<String, Storage> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
}

fn parse_expr(src: &str) -> Expr {
    let text = format!("encoding probe = {{ x: u8 }} = {src}\n");
    let schema = bytemill::parse(&text).unwrap();
    schema.encodings[0].yield_expr.clone().unwrap()
}

#[test]
fn sign_extend_picks_the_smallest_signed_class() {
    let env = locals(&[("raw", Storage::U8)]);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 8)"), &env), Storage::I8);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 12)"), &env), Storage::I16);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 26)"), &env), Storage::I32);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 32)"), &env), Storage::I32);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 64)"), &env), Storage::I64);
    assert_eq!(storage_of(&parse_expr("sign_extend(raw, 65)"), &env), Storage::I128);
}

#[test]
fn comparison_is_bool_and_unsigned_or_is_u16() {
    let env = locals(&[("b0", Storage::U8), ("b1", Storage::U8), ("raw", Storage::U8)]);
    assert_eq!(storage_of(&parse_expr("raw != 0"), &env), Storage::Bool);
    assert_eq!(storage_of(&parse_expr("false"), &env), Storage::Bool);
    assert_eq!(storage_of(&parse_expr("(b0 << 8) | b1"), &env), Storage::U16);
}


fn repo(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn u8_locals(enc: &bytemill::ast::EncodingDecl) -> HashMap<String, Storage> {
    use bytemill::ast::{BlockItem, EncBody, FieldType};
    let mut locals = HashMap::new();
    let EncBody::Block(body) = &enc.body else {
        return locals;
    };
    for item in &body.items {
        let BlockItem::Field(wf) = item else {
            continue;
        };
        let FieldType::Ref(r) = &wf.type_ref else {
            continue;
        };
        if r.name == "u8" {
            locals.insert(wf.name.clone(), Storage::U8);
        }
    }
    locals
}

fn class_of(path: &str, name: &str) -> Storage {
    let schema = bytemill::load_schema(&[repo(path)]).unwrap();
    let resolved = bytemill::analyze(&schema).unwrap();
    let enc = resolved.encodings.get(name).unwrap_or_else(|| panic!("missing {name}"));
    let yield_expr = enc.yield_expr.as_ref().unwrap_or_else(|| panic!("{name} has no yield"));
    storage_of(yield_expr, &u8_locals(enc))
}

#[test]
fn std_encodings_match_the_table() {
    let int = "schemas/std/integers.bml";
    let var = "schemas/std/varint.bml";
    assert_eq!(class_of(int, "i8"), Storage::I8);
    assert_eq!(class_of(int, "i16be"), Storage::I16);
    assert_eq!(class_of(int, "i16le"), Storage::I16);
    assert_eq!(class_of(int, "i32be"), Storage::I32);
    assert_eq!(class_of(int, "i32le"), Storage::I32);
    assert_eq!(class_of(var, "varint"), Storage::I32);
    assert_eq!(class_of(int, "i64be"), Storage::I64);
    assert_eq!(class_of(int, "i64le"), Storage::I64);
    assert_eq!(class_of(var, "varlong"), Storage::I64);
    assert_eq!(class_of(int, "u16be"), Storage::U16);
    assert_eq!(class_of(int, "u16le"), Storage::U16);
    assert_eq!(class_of(int, "u32be"), Storage::U32);
    assert_eq!(class_of(int, "u32le"), Storage::U32);
    assert_eq!(class_of(int, "u64be"), Storage::U64);
    assert_eq!(class_of(int, "u64le"), Storage::U64);
    assert_eq!(class_of(int, "boolean"), Storage::Bool);
}
#[test]
fn interval_rule_shrinks_computed_bit_fields() {
    let flags = locals(&[("flags", Storage::U8)]);
    // `commands_node.node_type = flags & 3` and the versions' `flags_*`
    // fields are u8 now, not i128.
    assert_eq!(storage_of(&parse_expr("flags & 3"), &flags), Storage::U8);
    assert_eq!(storage_of(&parse_expr("flags & 1"), &flags), Storage::U8);
    assert_eq!(
        storage_of(&parse_expr("(flags >> 5) & 1"), &flags),
        Storage::U8
    );
    assert_eq!(storage_of(&parse_expr("flags >> 6"), &flags), Storage::U8);
    let slot = locals(&[("slot_byte", Storage::U8)]);
    assert_eq!(
        storage_of(&parse_expr("slot_byte & 127"), &slot),
        Storage::U8
    );
    // `%` by a positive constant, `|` of bounded operands, literals.
    let raw = locals(&[("raw", Storage::U64)]);
    assert_eq!(storage_of(&parse_expr("raw % 4096"), &raw), Storage::U16);
    assert_eq!(
        storage_of(&parse_expr("300"), &HashMap::new()),
        Storage::U16
    );
    assert_eq!(storage_of(&parse_expr("-1"), &HashMap::new()), Storage::I8);
    assert_eq!(
        storage_of(&parse_expr("70000"), &HashMap::new()),
        Storage::U32
    );
    assert_eq!(
        storage_of(&parse_expr("18446744073709551615"), &HashMap::new()),
        Storage::U64
    );
    // Unknown names stay unbounded: still i128.
    assert_eq!(
        storage_of(&parse_expr("unknown & 3"), &HashMap::new()),
        Storage::U8,
        // `&` needs only the mask: any integer masked with 3 fits u8.
    );
    assert_eq!(
        storage_of(&parse_expr("unknown >> 2"), &HashMap::new()),
        Storage::I128
    );
    assert_eq!(
        storage_of(&parse_expr("unknown + 1"), &HashMap::new()),
        Storage::I128
    );
}

#[test]
fn f32be_is_f32_storage() {
    let schema = bytemill::load_schema(&[repo("schemas/std/integers.bml")]).unwrap();
    let resolved = bytemill::analyze(&schema).unwrap();
    let ir = bytemill::ir::lower(&resolved).unwrap();
    assert_eq!(
        ir.types.get("f32be"),
        Some(&bytemill::ir::TypeDef::Scalar(Storage::F32))
    );
    assert_eq!(
        ir.types.get("f64be"),
        Some(&bytemill::ir::TypeDef::Scalar(Storage::F64))
    );
}

/// After the interval rule no `i128` field remains in 26.3, the versions, or
/// hytale, and no `__any` type remains anywhere. A real `i128` would fail
/// here with its `file: type.field` path; keep it and report the field.
#[test]
fn no_i128_or_any_fields_in_shipped_schemas() {
    use bytemill::ir::TypeDef;
    let mut roots = vec!["schemas/minecraft_26_3_wiki.bml".to_string()];
    let versions = std::fs::read_dir(repo("schemas/versions")).unwrap();
    let mut names: Vec<String> = versions
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('v') && n.ends_with(".bml"))
        .map(|n| format!("schemas/versions/{n}"))
        .collect();
    names.sort();
    roots.extend(names);
    roots.push("schemas/hytale/2026_02_17.bml".to_string());
    assert!(!roots.is_empty());

    let mut bad = Vec::new();
    for root in &roots {
        let schema = bytemill::load_schema(&[repo(root)]).unwrap();
        let resolved = bytemill::analyze(&schema).unwrap();
        let ir = bytemill::ir::lower(&resolved).unwrap();
        for (name, def) in ir.types.iter() {
            let is_any = name.contains("__any");
            let fields: &[bytemill::ir::Field] = match def {
                TypeDef::Record { fields } => fields,
                _ => &[],
            };
            if is_any {
                bad.push(format!("{root}: type {name}"));
            }
            for field in fields {
                if is_any_ty(&field.ty) {
                    bad.push(format!("{root}: {name}.{} is __any", field.name));
                }
                if field_storage(&field.ty, &ir) == Some(Storage::I128) {
                    bad.push(format!("{root}: {name}.{} is i128", field.name));
                }
            }
            match def {
                TypeDef::Scalar(Storage::I128) => {
                    bad.push(format!("{root}: scalar {name} is i128"))
                }
                TypeDef::Enum {
                    base: Storage::I128,
                    ..
                } => {
                    bad.push(format!("{root}: enum {name} base is i128"));
                }
                _ => {}
            }
        }
    }
    assert!(
        bad.is_empty(),
        "unexpected wide fields:\n{}",
        bad.join("\n")
    );
}

fn is_any_ty(ty: &bytemill::ir::Ty) -> bool {
    match ty {
        bytemill::ir::Ty::Named(id) => id.0.contains("__any"),
        bytemill::ir::Ty::List(inner) | bytemill::ir::Ty::Optional(inner) => is_any_ty(inner),
        bytemill::ir::Ty::Scalar(_) => false,
    }
}

fn field_storage(ty: &bytemill::ir::Ty, ir: &bytemill::ir::IrSchema) -> Option<Storage> {
    match ty {
        bytemill::ir::Ty::Scalar(class) => Some(*class),
        bytemill::ir::Ty::Named(id) => match ir.types.get(&id.0) {
            Some(bytemill::ir::TypeDef::Scalar(class)) => Some(*class),
            Some(bytemill::ir::TypeDef::Enum { base, .. }) => Some(*base),
            _ => None,
        },
        _ => None,
    }
}
