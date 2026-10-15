//! The version table (`schemas/versions/manifest.json`) and the batch import over it.

use std::fs;
use std::path::{Path, PathBuf};

use super::model::{Protocol, Res};
use super::render::{FileSpec, import, version_ident};
use crate::json::loads;
use crate::value::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct VersionEntry {
    pub protocol: u32,
    /// The newest release with this protocol number; the file is named after it.
    pub release: String,
    pub also: Vec<String>,
    /// The vendored description, relative to the data directory without `/protocol.json`.
    pub data: String,
    /// A patch file, relative to the manifest, that corrects the description.
    pub patches: Option<String>,
    pub note: Option<String>,
}

impl VersionEntry {
    pub fn file_name(&self) -> String {
        format!("{}.bps", version_ident(&self.release))
    }
}

fn string_field(v: &Value, key: &str) -> Res<String> {
    match v.get(key) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        _ => Err(format!("manifest entry needs a string {key:?}")),
    }
}

pub fn parse_manifest(text: &str) -> Res<Vec<VersionEntry>> {
    let root = loads(text).map_err(|e| format!("manifest is not JSON: {e}"))?;
    let Some(Value::List(list)) = root.get("versions") else {
        return Err("manifest has no \"versions\" list".to_string());
    };
    let mut out = Vec::with_capacity(list.len());
    for v in list.iter() {
        let protocol = match v.get("protocol") {
            Some(Value::Int(n)) => {
                u32::try_from(*n).map_err(|_| format!("protocol {n} out of range"))?
            }
            _ => return Err("manifest entry needs an integer \"protocol\"".to_string()),
        };
        let mut also = Vec::new();
        if let Some(Value::List(a)) = v.get("also") {
            for x in a.iter() {
                let Value::Str(s) = x else {
                    return Err("\"also\" holds strings".to_string());
                };
                also.push(s.to_string());
            }
        }
        let note = match v.get("note") {
            Some(Value::Str(s)) => Some(s.to_string()),
            _ => None,
        };
        let patches = match v.get("patches") {
            Some(Value::Str(s)) => Some(s.to_string()),
            _ => None,
        };
        out.push(VersionEntry {
            protocol,
            release: string_field(v, "release")?,
            also,
            data: string_field(v, "data")?,
            patches,
            note,
        });
    }
    Ok(out)
}

/// The vendored description of one version with its patches applied, and the patches' reasons.
/// `manifest_dir` anchors the patch file.
pub fn load_protocol(
    entry: &VersionEntry,
    data_dir: &Path,
    manifest_dir: &Path,
) -> Res<(Protocol, Vec<String>)> {
    let path = data_dir.join(&entry.data).join("protocol.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut proto = Protocol::from_json(&text).map_err(|e| format!("{}: {e}", entry.data))?;
    let mut reasons = Vec::new();
    if let Some(rel) = &entry.patches {
        let patch_path = manifest_dir.join(rel);
        let patch_text = fs::read_to_string(&patch_path)
            .map_err(|e| format!("{}: {e}", patch_path.display()))?;
        reasons = proto
            .apply_patches(&patch_text)
            .map_err(|e| format!("{rel}: {e}"))?;
    }
    Ok((proto, reasons))
}

/// The generated source for one version. `manifest_dir` anchors the patch file.
pub fn generate(entry: &VersionEntry, data_dir: &Path, manifest_dir: &Path) -> Res<String> {
    let (proto, reasons) = load_protocol(entry, data_dir, manifest_dir)?;
    let source = format!("data/minecraft-data/{}/protocol.json", entry.data);
    let spec = FileSpec {
        protocol: entry.protocol,
        release: &entry.release,
        also: &entry.also,
        source: &source,
        patches: &reasons,
        note: entry.note.as_deref(),
    };
    import(&proto, &spec)
        .map_err(|e| format!("{} (protocol {}): {e}", entry.release, entry.protocol))
}

#[derive(Debug, Default)]
pub struct Report {
    pub written: Vec<PathBuf>,
    pub unchanged: usize,
    /// With `check`: files whose committed text differs from a fresh import (or are missing).
    pub stale: Vec<PathBuf>,
}

/// Import every manifest entry (or just `only`). With `check` nothing is written: stale files
/// are reported instead.
pub fn import_versions(
    manifest: &Path,
    data_dir: &Path,
    out_dir: &Path,
    check: bool,
    only: Option<u32>,
) -> Res<Report> {
    let text = fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let entries = parse_manifest(&text)?;
    let manifest_dir = manifest.parent().unwrap_or(Path::new("."));
    let mut report = Report::default();
    for entry in entries
        .iter()
        .filter(|e| only.is_none_or(|p| p == e.protocol))
    {
        let fresh = generate(entry, data_dir, manifest_dir)?;
        let path = out_dir.join(entry.file_name());
        let current = fs::read_to_string(&path).ok();
        if current.as_deref() == Some(fresh.as_str()) {
            report.unchanged += 1;
            continue;
        }
        if check {
            report.stale.push(path);
            continue;
        }
        fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
        fs::write(&path, fresh).map_err(|e| format!("{}: {e}", path.display()))?;
        report.written.push(path);
    }
    Ok(report)
}
