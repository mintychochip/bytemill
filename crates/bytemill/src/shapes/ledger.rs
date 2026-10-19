//! Map BPS units to hand packet files and compare them with the shape ledger.
//! Ports `bps/shapes/ledger.py`.

use crate::ast::{PacketDecl, ResolvedSchema};
use crate::emit_rstom::packet_fields;
use crate::errors::{shape, BpsResult};
use crate::minestom::Unit;
use crate::shapes::extract::{extract_shape, PACKET_ROOT};
use crate::shapes::model::{shape_from_json, Shape};
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

pub fn bps_field_pairs(pkt: &PacketDecl, resolved: &ResolvedSchema) -> BpsResult<Vec<(String, String)>> {
    let (fields, _types) = packet_fields(pkt, resolved)?;
    let mut out = Vec::with_capacity(fields.len());
    for f in &fields {
        let base = if f.repeat_mode.is_some() {
            format!("Vec<{}>", f.rtype)
        } else {
            f.rtype.clone()
        };
        let mapped = if f.guard.is_some() {
            format!("Option<{base}>")
        } else {
            base
        };
        out.push((f.ident.clone(), mapped));
    }
    Ok(out)
}

pub fn mismatch(shape: &Shape, bps_fields: &[(String, String)]) -> Vec<String> {
    let hand: Vec<(&str, &str)> = shape
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.type_.as_str()))
        .collect();
    if hand.len() != bps_fields.len() {
        return vec![format!(
            "field count: hand has {}, BPS has {}",
            hand.len(),
            bps_fields.len()
        )];
    }
    for ((hn, ht), (bn, bt)) in hand.iter().zip(bps_fields.iter()) {
        if hn != bn {
            return vec![format!(
                "field {hn}: hand name {}, BPS name {}",
                crate::errors::py_repr_str(hn),
                crate::errors::py_repr_str(bn)
            )];
        }
        if ht != bt {
            return vec![format!(
                "field {hn}: hand type {}, BPS type {}",
                crate::errors::py_repr_str(ht),
                crate::errors::py_repr_str(bt)
            )];
        }
    }
    Vec::new()
}

pub fn load_ledger(dir: &Path) -> BpsResult<Vec<(String, Shape)>> {
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

pub fn packet_report(unit: &Unit, resolved: &ResolvedSchema, workspace: &Path) -> BpsResult<String> {
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
    let reasons = mismatch(&s, &bps_field_pairs(&unit.pkt, resolved)?);
    if !reasons.is_empty() {
        return Ok(reasons.join("; "));
    }
    Ok("eligible".to_string())
}

/// `(unit, shape)` for each unit whose hand file parses; `eligible_only` skips
/// shapes whose fields no longer match BPS.
pub fn extract_shapes_for_units(
    resolved: &ResolvedSchema,
    workspace: &Path,
    units: &[Unit],
    eligible_only: bool,
    class_names: Option<&[String]>,
) -> BpsResult<Vec<(Unit, Shape)>> {
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
        if eligible_only && !mismatch(&shape, &bps_field_pairs(&unit.pkt, resolved)?).is_empty() {
            continue;
        }
        out.push((unit.clone(), shape));
    }
    Ok(out)
}
