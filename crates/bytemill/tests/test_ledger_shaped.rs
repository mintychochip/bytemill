//! Shape ledger invariants: every committed ledger file reserialises and the ledger covers the
//! protocol, one shape per BML packet.

mod common;

fn ledger_present() -> bool {
    if common::committed_shapes_present() {
        return true;
    }
    eprintln!("{}", common::MISSING_SHAPES_MSG);
    false
}

fn ledger() -> Vec<(String, bytemill::shapes::Shape)> {
    bytemill::shapes::load_ledger(&common::repo_root().join("targets/rstom/shapes")).unwrap()
}

#[test]
fn a_new_body_or_a_read_through_new_needs_a_new() {
    if !ledger_present() {
        return;
    }
    let broken: Vec<String> = ledger()
        .into_iter()
        .filter(|(_, s)| (!s.new.body.is_empty() || s.serializer.read_via_new) && !s.new.present)
        .map(|(_, s)| s.cls)
        .collect();
    assert!(broken.is_empty(), "no `new` to carry the body or the read: {broken:?}");
}

#[test]
fn every_bml_unit_has_exactly_one_shape() {
    if !ledger_present() {
        return;
    }
    let Some(table) = common::rstom_tables::real_table() else {
        eprintln!("skipped: rstom packets.json not present");
        return;
    };
    let resolved = common::load_minecraft().unwrap();
    let units = bytemill::minestom::build_units(&resolved, &table).unwrap();
    let mut modules: Vec<String> = ledger().into_iter().map(|(m, _)| m).collect();
    modules.sort();
    let mut wanted: Vec<String> = units.iter().map(bytemill::shapes::module_path).collect();
    wanted.sort();
    assert_eq!(modules, wanted);
}
