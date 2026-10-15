//! Storage classes assigned from a scalar yield. Spec section 4.3.

use bps::ast::Expr;
use bps::ir::{storage_of, Storage};
use std::collections::HashMap;

fn locals(pairs: &[(&str, Storage)]) -> HashMap<String, Storage> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
}

fn parse_expr(src: &str) -> Expr {
    let text = format!("encoding probe = {{ x: u8 }} = {src}\n");
    let schema = bps::parse(&text).unwrap();
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

fn u8_locals(enc: &bps::ast::EncodingDecl) -> HashMap<String, Storage> {
    use bps::ast::{BlockItem, EncBody, FieldType};
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
    let schema = bps::load_schema(&[repo(path)]).unwrap();
    let resolved = bps::analyze(&schema).unwrap();
    let enc = resolved.encodings.get(name).unwrap_or_else(|| panic!("missing {name}"));
    let yield_expr = enc.yield_expr.as_ref().unwrap_or_else(|| panic!("{name} has no yield"));
    storage_of(yield_expr, &u8_locals(enc))
}

#[test]
fn std_encodings_match_the_table() {
    let int = "schemas/std/integers.bps";
    let var = "schemas/std/varint.bps";
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