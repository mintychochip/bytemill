//! Map BML units to hand packet files and compare them with the shape ledger.
//! Ports `bps/shapes/ledger.py`.

use crate::ast::{PacketDecl, ResolvedSchema};
use crate::emit_rstom::packet_fields;
use crate::errors::{BmlResult, shape};
use crate::minestom::Unit;
use crate::shapes::extract::{PACKET_ROOT, extract_shape};
use crate::shapes::model::{Shape, shape_from_json};
use std::path::{Path, PathBuf};

pub fn hand_packet_path(workspace: &Path, unit: &Unit) -> PathBuf {
    workspace
        .join(PACKET_ROOT)
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
    map.iter().copied().collect()
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

pub fn packet_report(
    unit: &Unit,
    resolved: &ResolvedSchema,
    workspace: &Path,
) -> BmlResult<String> {
    let path = hand_packet_path(workspace, unit);
    if !path.is_file() {
        return Ok(format!("missing hand file {}", path.display()));
    }
    let text = crate::text::read_text(&path)
        .map_err(|e| shape(format!("cannot read {}: {e}", path.display())))?;
    let s = match extract_shape(&text, &unit.cls.name, &module_path(unit), Some(&path)) {
        Ok(s) => s,
        Err(err) => return Ok(err.to_string()),
    };
    let reasons = mismatch(&s, &bml_field_pairs(&unit.pkt, resolved)?);
    if !reasons.is_empty() {
        return Ok(reasons.join("; "));
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
        if eligible_only && !mismatch(&shape, &bml_field_pairs(&unit.pkt, resolved)?).is_empty() {
            continue;
        }
        out.push((unit.clone(), shape));
    }
    Ok(out)
}
