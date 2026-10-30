//! Map BML units to hand packet files and compare them with the shape ledger.
//! Ports `bps/shapes/ledger.py`.

use crate::ast::{Expr, PacketDecl, ResolvedSchema};
use crate::emit_rstom::{PacketShape, is_generated, packet_fields};
use crate::errors::{BmlResult, shape};
use crate::minestom::Unit;
use crate::shapes::extract::{crate_root, extract_shape, packet_root};
use crate::shapes::model::{Shape, shape_from_json};
use crate::value::Value;
use std::path::{Path, PathBuf};

pub fn hand_packet_path(workspace: &Path, unit: &Unit) -> PathBuf {
    packet_root(workspace)
        .join(&unit.cls.group)
        .join(&unit.cls.package)
        .join(format!("{}.rs", unit.cls.stem))
}

pub fn module_path(unit: &Unit) -> String {
    format!("{}/{}/{}", unit.cls.group, unit.cls.package, unit.cls.stem)
}

/// The `Vec`/`Option` wrapping both pair sources apply, so `mismatch` and the
/// tree renderer agree on field identity. Keep in sync with `body_pairs`.
pub fn mapped_pair_rtype(rtype: &str, repeated: bool, guarded: bool) -> String {
    let base = if repeated {
        format!("Vec<{rtype}>")
    } else {
        rtype.to_string()
    };
    if guarded {
        format!("Option<{base}>")
    } else {
        base
    }
}

pub fn bml_field_pairs(
    pkt: &PacketDecl,
    resolved: &ResolvedSchema,
) -> BmlResult<Vec<(String, String)>> {
    let (fields, _) = packet_fields(pkt, resolved)?;
    let mut out = Vec::with_capacity(fields.len());
    for f in &fields {
        out.push((
            f.ident.clone(),
            mapped_pair_rtype(&f.rtype, f.repeat_mode.is_some(), f.guard.is_some()),
        ));
    }
    Ok(out)
}

/// Pair hand fields with BML fields by name, falling back to unique leftover
/// types. Returns hand index to BML index, or `None` when no bijection
/// exists. Order-insensitive: renames and order swaps both pair; ambiguity,
/// missing fields, and type clashes fail closed.
pub fn pair_fields(hand: &[(&str, &str)], bml: &[(String, String)]) -> Option<Vec<usize>> {
    let (map, _) = pair_fields_partial(hand, bml)?;
    map.into_iter().collect()
}

fn pair_fields_partial(
    hand: &[(&str, &str)],
    bml: &[(String, String)],
) -> Option<(Vec<Option<usize>>, Vec<bool>)> {
    if hand.len() != bml.len() {
        return None;
    }
    let n = hand.len();
    let mut map: Vec<Option<usize>> = vec![None; n];
    let mut used = vec![false; n];
    for (i, (hn, ht)) in hand.iter().enumerate() {
        if let Some(j) = bml.iter().position(|(bn, _)| bn == hn) {
            if used[j] || bml[j].1 != **ht {
                return None;
            }
            map[i] = Some(j);
            used[j] = true;
        }
    }
    loop {
        let mut progress = false;
        for i in 0..n {
            if map[i].is_some() {
                continue;
            }
            let cands: Vec<usize> = (0..n)
                .filter(|&j| !used[j] && bml[j].1 == hand[i].1)
                .collect();
            let shared = (0..n).any(|k| k != i && map[k].is_none() && hand[k].1 == hand[i].1);
            if cands.len() == 1 && !shared {
                map[i] = Some(cands[0]);
                used[cands[0]] = true;
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    Some((map, used))
}

/// A BML field that the generated wire code derives from a hand field instead
/// of reading it from the packet struct: a list's count, or an `Option`'s
/// presence flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedField {
    pub bml: String,
    pub hand: String,
    pub kind: DerivedKind,
    /// The Rust type of the derived wire local (the count field's rtype).
    pub rtype: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedKind {
    Count,
    Flag,
}

/// Hand-to-wire pairing plus the wire fields whose values are derived.
#[derive(Debug, Clone)]
pub struct PairPlan {
    pub hand_to_bml: Vec<usize>,
    pub derived: Vec<DerivedField>,
}

/// Pair the shape's fields with a packet's flattened BML fields, allowing a
/// list's count and an `Option`'s presence flag to be derived from the list or
/// option itself. Returns `None` when a hand field is unpaired, a wire field is
/// neither paired nor derivable, or a type clashes.
pub fn pair_body_fields(shape: &Shape, fields: &[PacketShape]) -> Option<PairPlan> {
    let hand: Vec<(&str, &str)> = shape
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.type_.as_str()))
        .collect();
    if hand.len() > fields.len() {
        return None;
    }
    let bml: Vec<(String, String)> = fields
        .iter()
        .map(|f| {
            (
                f.ident.clone(),
                mapped_pair_rtype(&f.rtype, f.repeat_mode.is_some(), f.guard.is_some()),
            )
        })
        .collect();
    let n = hand.len();
    let m = bml.len();
    let mut map: Vec<Option<usize>> = vec![None; n];
    let mut used = vec![false; m];
    let mut bml_to_hand: Vec<Option<usize>> = vec![None; m];

    // Counts and presence flags are derived from the list/option they
    // describe; they may only pair by an exact name, never by type alone
    // (`i32` next to a count field is not the count).
    let mut derived_idents = vec![false; m];
    for f in fields {
        if let Some(count) = &f.count
            && let Some(ci) = fields.iter().position(|g| &g.ident == count)
        {
            derived_idents[ci] = true;
        }
        if let Some(flag) = guard_flag(f.guard.as_ref())
            && let Some(fi) = fields.iter().position(|g| g.ident == flag)
        {
            derived_idents[fi] = true;
        }
    }

    // 1. Name matches must agree on the wrapped type.
    for (i, (hn, ht)) in hand.iter().enumerate() {
        if let Some(j) = bml.iter().position(|(bn, _)| bn == hn) {
            if used[j] || bml[j].1 != **ht {
                return None;
            }
            map[i] = Some(j);
            used[j] = true;
            bml_to_hand[j] = Some(i);
        }
    }

    // 2. Unique leftover type matches (renames and reorders).
    loop {
        let mut progress = false;
        for i in 0..n {
            if map[i].is_some() {
                continue;
            }
            let cands: Vec<usize> = (0..m)
                .filter(|&j| !used[j] && !derived_idents[j] && bml[j].1 == hand[i].1)
                .collect();
            let shared = (0..n).any(|k| k != i && map[k].is_none() && hand[k].1 == hand[i].1);
            if cands.len() == 1 && !shared {
                let j = cands[0];
                map[i] = Some(j);
                used[j] = true;
                bml_to_hand[j] = Some(i);
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }

    // 3. A list or option may consume a wire field whose wrapped type matches.
    let mut progress = true;
    while progress {
        progress = false;
        for i in 0..n {
            if map[i].is_some() {
                continue;
            }
            for j in 0..m {
                if used[j] || derived_idents[j] || bml[j].1 != hand[i].1 {
                    continue;
                }
                let f = &fields[j];
                if f.repeat_mode.is_some() || f.guard.is_some() {
                    map[i] = Some(j);
                    used[j] = true;
                    bml_to_hand[j] = Some(i);
                    progress = true;
                    break;
                }
            }
        }
    }

    // 4. Derive the count of a bound list and the flag of a bound option.
    let mut covered = used;
    let mut derived: Vec<DerivedField> = Vec::new();
    for (j, f) in fields.iter().enumerate() {
        let Some(hand_i) = bml_to_hand[j] else {
            continue;
        };
        let hand_type = hand[hand_i].1;
        let mut derive = |bml: &str, kind: DerivedKind, rtype: &str| -> Option<()> {
            match kind {
                DerivedKind::Count if !hand_type.starts_with("Vec<") => return None,
                DerivedKind::Flag if !hand_type.starts_with("Option<") => return None,
                _ => {}
            }
            let fi = fields.iter().position(|g| g.ident == bml)?;
            if bml_to_hand[fi].is_some() {
                return Some(());
            }
            if covered[fi] || derived.iter().any(|d| d.bml == bml) {
                return None;
            }
            covered[fi] = true;
            derived.push(DerivedField {
                bml: bml.to_string(),
                hand: hand[hand_i].0.to_string(),
                kind,
                rtype: rtype.to_string(),
            });
            Some(())
        };
        if let Some(count) = &f.count {
            let ci = fields.iter().position(|g| &g.ident == count)?;
            derive(count, DerivedKind::Count, &fields[ci].rtype)?;
        }
        if let Some(flag) = guard_flag(f.guard.as_ref()) {
            let fi = fields.iter().position(|g| g.ident == flag)?;
            derive(&flag, DerivedKind::Flag, &fields[fi].rtype)?;
        }
    }
    if map.iter().any(Option::is_none) || covered.iter().any(|&c| !c) {
        return None;
    }
    Some(PairPlan {
        hand_to_bml: map.into_iter().map(Option::unwrap).collect(),
        derived,
    })
}

/// The boolean field a presence guard reads, or `None` for a computed guard.
fn guard_flag(guard: Option<&Expr>) -> Option<String> {
    match guard? {
        Expr::Ident(name) => Some(crate::emit_rstom::ident(name)),
        _ => None,
    }
}

/// Reasons `shape` cannot pair with `fields`; empty means eligible.
pub fn mismatch_fields(shape: &Shape, fields: &[PacketShape]) -> Vec<String> {
    if pair_body_fields(shape, fields).is_some() {
        return Vec::new();
    }
    let bml: Vec<(String, String)> = fields
        .iter()
        .map(|f| {
            (
                f.ident.clone(),
                mapped_pair_rtype(&f.rtype, f.repeat_mode.is_some(), f.guard.is_some()),
            )
        })
        .collect();
    let mut reasons = mismatch(shape, &bml);
    if reasons.is_empty() {
        reasons.push("fields do not pair with the derived BML fields".to_string());
    }
    reasons
}

pub fn mismatch(shape: &Shape, bml_fields: &[(String, String)]) -> Vec<String> {
    let hand: Vec<(&str, &str)> = shape
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.type_.as_str()))
        .collect();
    if hand.len() != bml_fields.len() {
        return vec![format!(
            "field count: hand has {}, BML has {}",
            hand.len(),
            bml_fields.len()
        )];
    }
    if pair_fields(&hand, bml_fields).is_some() {
        return Vec::new();
    }
    for ((hn, ht), (bn, bt)) in hand.iter().zip(bml_fields.iter()) {
        if hn != &bn.as_str() {
            return vec![format!(
                "field {hn}: hand name {}, BML name {}",
                crate::errors::py_repr_str(hn),
                crate::errors::py_repr_str(bn)
            )];
        }
        if ht != &bt.as_str() {
            return vec![format!(
                "field {hn}: hand type {}, BML type {}",
                crate::errors::py_repr_str(ht),
                crate::errors::py_repr_str(bt)
            )];
        }
    }
    vec!["fields do not pair with BML fields".to_string()]
}

pub fn load_ledger(dir: &Path) -> BmlResult<Vec<(String, Shape)>> {
    let mut paths: Vec<PathBuf> = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "json") {
                out.push(path);
            }
        }
    }
    walk(dir, &mut paths);
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let rel = path.strip_prefix(dir).unwrap_or(&path);
        let module = rel.with_extension("").to_string_lossy().replace('\\', "/");
        let text = crate::text::read_text(&path)
            .map_err(|e| shape(format!("cannot read {}: {e}", path.display())))?;
        let s = shape_from_json(&text)?;
        out.push((module, s));
    }
    Ok(out)
}

/// A class the pin gate triaged as differing from the BML wire layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownDeviation {
    pub class: String,
    pub category: String,
}

/// The classes listed in `<crate>/tests/bml/wire_pins_known.json`. A packet listed there is not
/// wire-compatible with the schema and must stay hand-written, unless its category is
/// `domain-limit`: that row records a limit of the hand type (a value the type cannot hold) that a
/// generated packet keeps, and the pin gate still reads it as a known difference.
pub fn known_deviations(workspace: &Path) -> BmlResult<Vec<KnownDeviation>> {
    let path = crate_root(workspace)
        .join("tests")
        .join("bml")
        .join("wire_pins_known.json");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = crate::text::read_text(&path)
        .map_err(|e| shape(format!("cannot read {}: {e}", path.display())))?;
    let Value::List(items) = crate::json::loads(&text)? else {
        return Err(shape(format!("{}: expected a list", path.display())));
    };
    let mut out = Vec::new();
    for item in items.iter() {
        let field = |key: &str| match item.get(key) {
            Some(Value::Str(s)) => s.to_string(),
            _ => String::new(),
        };
        let class = field("class");
        if !class.is_empty() {
            out.push(KnownDeviation {
                class,
                category: field("category"),
            });
        }
    }
    Ok(out)
}

pub fn packet_report(
    unit: &Unit,
    resolved: &ResolvedSchema,
    workspace: &Path,
) -> BmlResult<String> {
    let path = hand_packet_path(workspace, unit);
    if !path.is_file() {
        return Ok(format!("missing hand file {}", path.display()));
    }
    // A swapped packet is no longer hand code: the shape ledger holds what it was generated from.
    if is_generated(&path) {
        return Ok("generated".to_string());
    }
    let text = crate::text::read_text(&path)
        .map_err(|e| shape(format!("cannot read {}: {e}", path.display())))?;
    let s = match extract_shape(&text, &unit.cls.name, &module_path(unit), Some(&path)) {
        Ok(s) => s,
        Err(err) => return Ok(err.to_string()),
    };
    let (fields, _) = packet_fields(&unit.pkt, resolved)?;
    let reasons = mismatch_fields(&s, &fields);
    if !reasons.is_empty() {
        return Ok(reasons.join("; "));
    }
    if let Some(d) = known_deviations(workspace)?
        .iter()
        .find(|d| d.class == unit.cls.name)
    {
        return Ok(format!(
            "known wire deviation ({}), listed in wire_pins_known.json",
            d.category
        ));
    }
    Ok("eligible".to_string())
}

/// `(unit, shape)` for each unit whose hand file parses; `eligible_only` skips
/// shapes whose fields no longer match BML.
pub fn extract_shapes_for_units(
    resolved: &ResolvedSchema,
    workspace: &Path,
    units: &[Unit],
    eligible_only: bool,
    class_names: Option<&[String]>,
) -> BmlResult<Vec<(Unit, Shape)>> {
    let known = if eligible_only {
        known_deviations(workspace)?
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    for unit in units {
        if let Some(names) = class_names
            && !names.contains(&unit.cls.name)
        {
            continue;
        }
        let path = hand_packet_path(workspace, unit);
        if !path.is_file() {
            continue;
        }
        let text = crate::text::read_text(&path)
            .map_err(|e| shape(format!("cannot read {}: {e}", path.display())))?;
        let shape = match extract_shape(&text, &unit.cls.name, &module_path(unit), Some(&path)) {
            Ok(s) => s,
            Err(_) if eligible_only => continue,
            Err(e) => return Err(e),
        };
        if eligible_only {
            let (fields, _) = packet_fields(&unit.pkt, resolved)?;
            if !mismatch_fields(&shape, &fields).is_empty()
                || known.iter().any(|d| d.class == unit.cls.name)
            {
                continue;
            }
        }
        out.push((unit.clone(), shape));
    }
    Ok(out)
}
