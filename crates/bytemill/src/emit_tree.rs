//! Emit shaped packets and the wire-pin gate into an rstom workspace.
//! Ports `bps/emit_tree.py`.

use crate::ast::ResolvedSchema;
use crate::emit_rstom::{is_generated, packet_body};
use crate::errors::{BpsResult, emit};
use crate::minestom::{ClassTable, Unit, build_units};
use crate::pins::{PINS_TEST_RS, build_pins, pins_json, pins_json_is_owned};
use crate::shapes::extract::extract_shape;
use crate::shapes::ledger::{bps_field_pairs, load_ledger, mismatch, module_path};
use crate::shapes::model::Shape;
use crate::tree_packet::{tree_packet, validate_shape};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const PACKET_SCAN_ROOT: &str = "crates/rstom/src/network/packet";

/// `shutil.which("rustfmt")`.
fn which_rustfmt() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("rustfmt"))
        .find(|candidate| candidate.is_file())
}

/// Run rustfmt on `path` in place, with the ext-module flags when `mod ext;` is present.
fn run_rustfmt(rustfmt: &Path, path: &Path, text: &str) -> Result<(), String> {
    let mut cmd = Command::new(rustfmt);
    if text.contains("mod ext;") {
        cmd.args(["--unstable-features", "--skip-children"]);
    }
    cmd.args(["--edition", "2024"]).arg(path);
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let err = match (stderr.trim(), stdout.trim()) {
        (e, _) if !e.is_empty() => e.to_string(),
        (_, o) if !o.is_empty() => o.to_string(),
        _ => match out.status.code() {
            Some(c) => format!("exit {c}"),
            None => "exit -1".to_string(),
        },
    };
    Err(err)
}

static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

/// `_rustfmt_text(text)`: format through a temporary `.rs` file.
fn rustfmt_text(text: &str) -> BpsResult<String> {
    let Some(rustfmt) = which_rustfmt() else {
        return Ok(text.to_string());
    };
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("bps-emit-tree-{}-{seq}.rs", std::process::id()));
    std::fs::write(&path, text).map_err(|e| emit(format!("rustfmt failed: {e}")))?;
    let result = match run_rustfmt(&rustfmt, &path, text) {
        Ok(()) => crate::text::read_text(&path).map_err(|e| emit(format!("rustfmt failed: {e}"))),
        Err(err) => Err(emit(format!("rustfmt failed: {err}"))),
    };
    let _ = std::fs::remove_file(&path);
    result
}

/// `_rustfmt_paths(paths)`: format written `.rs` files in place.
fn rustfmt_paths(paths: &[PathBuf]) -> BpsResult<()> {
    let Some(rustfmt) = which_rustfmt() else {
        return Ok(());
    };
    for f in paths {
        if f.extension().and_then(|e| e.to_str()) != Some("rs") || !f.is_file() {
            continue;
        }
        let text = read(f)?;
        if let Err(err) = run_rustfmt(&rustfmt, f, &text) {
            return Err(emit(format!("rustfmt failed on {}: {err}", f.display())));
        }
    }
    Ok(())
}

fn read(path: &Path) -> BpsResult<String> {
    crate::text::read_text(path).map_err(|e| emit(format!("{}: {e}", path.display())))
}

fn is_owned_output(path: &Path) -> BpsResult<bool> {
    if !path.is_file() {
        return Ok(true);
    }
    if path.extension().and_then(|e| e.to_str()) == Some("rs") {
        return Ok(is_generated(path));
    }
    if path.file_name().and_then(|n| n.to_str()) == Some("wire_pins.json") {
        return Ok(pins_json_is_owned(&read(path)?));
    }
    Ok(false)
}

fn refuse_hand_files(planned: &[(PathBuf, String)], hand_swaps: &[PathBuf]) -> BpsResult<()> {
    for (path, _) in planned {
        if hand_swaps.contains(path) {
            continue;
        }
        if !is_owned_output(path)? {
            return Err(emit(format!(
                "refusing to overwrite hand file {}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// `Path.as_posix()` of `path` relative to `base`.
fn posix_rel(path: &Path, base: &Path) -> String {
    let rel = path.strip_prefix(base).unwrap_or(path);
    rel.components()
        .map(|c| match c {
            Component::Normal(s) => s.to_string_lossy().into_owned(),
            other => other.as_os_str().to_string_lossy().into_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn packet_output_path(workspace: &Path, unit: &Unit) -> PathBuf {
    workspace
        .join(PACKET_SCAN_ROOT)
        .join(format!("{}.rs", module_path(unit)))
}

fn require_extras(workspace: &Path, shape: &Shape, packet_dir: &Path) -> BpsResult<()> {
    let Some(extras) = shape.extras.as_deref().filter(|e| !e.is_empty()) else {
        return Ok(());
    };
    let ext = packet_dir.join(extras);
    if !ext.is_file() {
        return Err(emit(format!("create {} first", posix_rel(&ext, workspace))));
    }
    Ok(())
}

fn refuse_orphan_ext(module: &str, shape: &Shape, packet_path: &Path) -> BpsResult<()> {
    if shape.extras.as_deref().is_some_and(|e| !e.is_empty()) {
        return Ok(());
    }
    if !packet_path.is_file() || !is_generated(packet_path) {
        return Ok(());
    }
    let stem = module.rsplit('/').next().unwrap_or("");
    let ext_name = format!("{stem}.ext.rs");
    let dir = packet_path.parent().unwrap_or(Path::new(""));
    if dir.join(&ext_name).is_file() {
        return Err(emit(format!(
            "{module}: ext file {ext_name} would be orphaned: set extras in the shape"
        )));
    }
    Ok(())
}

/// Python `str.splitlines()`.
fn splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let len = match c {
            '\r' => {
                if let Some(&(_, '\n')) = chars.peek() {
                    chars.next();
                    2
                } else {
                    1
                }
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}'
            | '\u{2029}' => c.len_utf8(),
            _ => continue,
        };
        out.push(&text[start..i]);
        start = i + len;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Doc and attribute-opening lines before the first `#[cfg(test)]`, trimmed.
fn hand_prefix_markers(hand_text: &str) -> Vec<String> {
    let lines = splitlines(hand_text);
    let mut markers = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let stripped = line.trim();
        if stripped.starts_with("#[cfg(test)]") {
            break;
        }
        if stripped.starts_with("///") || stripped.starts_with("//!") {
            markers.push(stripped.to_string());
            continue;
        }
        if stripped.starts_with("#[") {
            if stripped == "#[must_use]" {
                let mut j = i + 1;
                while j < lines.len() && lines[j].trim().is_empty() {
                    j += 1;
                }
                if j < lines.len() {
                    let nxt = lines[j].trim();
                    if nxt.contains("fn new") || nxt.contains("fn serializer") {
                        markers.push(stripped.to_string());
                    }
                }
                continue;
            }
            markers.push(stripped.to_string());
        }
    }
    markers
}

/// The Python `_missing_hand_markers`: doc/attr lines before `#[cfg(test)]`
/// that the generated file (or its `.ext.rs`) does not carry.
pub fn missing_hand_markers(hand_text: &str, generated_text: &str, ext_text: &str) -> Vec<String> {
    let gen_lines: Vec<&str> = splitlines(generated_text)
        .into_iter()
        .map(|l| l.trim())
        .collect();
    let ext_lines: Vec<&str> = if ext_text.is_empty() {
        Vec::new()
    } else {
        splitlines(ext_text).into_iter().map(|l| l.trim()).collect()
    };
    let mut gi = 0;
    let mut ei = 0;
    let mut missing = Vec::new();
    for marker in hand_prefix_markers(hand_text) {
        let gpos = (gi..gen_lines.len()).find(|&j| gen_lines[j] == marker);
        let epos = (ei..ext_lines.len()).find(|&j| ext_lines[j] == marker);
        if let Some(g) = gpos {
            gi = g + 1;
        } else if let Some(e) = epos {
            ei = e + 1;
        } else {
            missing.push(marker);
        }
    }
    missing
}

/// The Python `_assert_hand_content_preserved`.
pub fn assert_hand_content_preserved(
    module: &str,
    hand_text: &str,
    generated_text: &str,
    ext_path: Option<&Path>,
) -> BpsResult<()> {
    let ext_text = match ext_path {
        Some(p) if p.is_file() => read(p)?,
        _ => String::new(),
    };
    let missing = missing_hand_markers(hand_text, generated_text, &ext_text);
    if !missing.is_empty() {
        let preview = missing
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        let extra = if missing.len() > 3 {
            format!(" (+{} more)", missing.len() - 3)
        } else {
            String::new()
        };
        return Err(emit(format!(
            "{module}: hand content lost: {preview}{extra}"
        )));
    }
    Ok(())
}

fn hand_matches_ledger(path: &Path, shape: &Shape) -> BpsResult<bool> {
    let text = read(path)?;
    match extract_shape(&text, &shape.cls, &shape.module, Some(path)) {
        Ok(hand) => Ok(&hand == shape),
        Err(_) => Ok(false),
    }
}

fn orphan_generated_packets(workspace: &Path, shaped_modules: &[String]) -> BpsResult<Vec<String>> {
    let root = workspace.join(PACKET_SCAN_ROOT);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_rs(&root, &mut files)?;
    files.sort();
    let mut out = Vec::new();
    for path in files {
        if !is_generated(&path) {
            continue;
        }
        let rel = path.strip_prefix(&root).unwrap_or(&path).with_extension("");
        let rel = posix_rel(&rel, Path::new(""));
        if !shaped_modules.contains(&rel) {
            out.push(format!("{}: orphan", path.display()));
        }
    }
    Ok(out)
}

/// Recursive `rglob("*.rs")`.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> BpsResult<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| emit(format!("{}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| emit(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// `dict[key] = value` on an insertion-ordered list.
fn put(planned: &mut Vec<(PathBuf, String)>, path: PathBuf, text: String) {
    match planned.iter_mut().find(|(p, _)| *p == path) {
        Some(slot) => slot.1 = text,
        None => planned.push((path, text)),
    }
}

/// What planning the packet files yields: the files to write (path, text) and the hand files
/// that are swapped for generated ones.
type PacketPlan = (Vec<(PathBuf, String)>, Vec<PathBuf>);

fn planned_packets(
    resolved: &ResolvedSchema,
    workspace: &Path,
    classes: &ClassTable,
    shapes: &[(String, Shape)],
    swap_hand: bool,
) -> BpsResult<PacketPlan> {
    let units = build_units(resolved, classes)?;
    let modules: Vec<String> = units.iter().map(module_path).collect();
    for (module, _) in shapes {
        if !modules.contains(module) {
            return Err(emit(format!("stale shape: {module}")));
        }
    }
    let mut planned: Vec<(PathBuf, String)> = Vec::new();
    let mut hand_swaps: Vec<PathBuf> = Vec::new();
    for unit in &units {
        let module = module_path(unit);
        let Some((_, shape)) = shapes.iter().find(|(m, _)| *m == module) else {
            continue;
        };
        validate_shape(shape)?;
        let reasons = mismatch(shape, &bps_field_pairs(&unit.pkt, resolved)?);
        if !reasons.is_empty() {
            return Err(emit(format!("{module}: {}", reasons.join("; "))));
        }
        let body = packet_body(unit, resolved)?;
        let path = packet_output_path(workspace, unit);
        let packet_dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        require_extras(workspace, shape, &packet_dir)?;
        refuse_orphan_ext(&module, shape, &path)?;
        let raw = tree_packet(unit, shape, &body)?;
        let text = rustfmt_text(&raw)?;
        if swap_hand && path.is_file() && !is_generated(&path) {
            if !hand_matches_ledger(&path, shape)? {
                return Err(emit(format!(
                    "hand file changed since its shape was extracted: {module}"
                )));
            }
            let ext_path = shape
                .extras
                .as_deref()
                .filter(|e| !e.is_empty())
                .map(|e| packet_dir.join(e));
            assert_hand_content_preserved(&module, &read(&path)?, &text, ext_path.as_deref())?;
            hand_swaps.push(path.clone());
        }
        put(&mut planned, path, text);
    }
    Ok((planned, hand_swaps))
}

/// `emit_tree(resolved, workspace, classes, shapes_dir, check)`: write the pin
/// gate and shaped packets. Returns the paths written, or drift messages in check mode.
pub fn emit_tree(
    resolved: &ResolvedSchema,
    workspace: &Path,
    classes: &ClassTable,
    shapes_dir: &Path,
    check: bool,
) -> BpsResult<Vec<String>> {
    let shapes = if shapes_dir.is_dir() {
        load_ledger(shapes_dir)?
    } else {
        Vec::new()
    };
    let units = build_units(resolved, classes)?;
    let rows = build_pins(resolved, &units)?;
    let tests = workspace.join("crates").join("rstom").join("tests");
    let rust_path = tests.join("bps_wire_pins.rs");
    let json_path = tests.join("bps").join("wire_pins.json");
    let known_path = tests.join("bps").join("wire_pins_known.json");

    let mut planned: Vec<(PathBuf, String)> = vec![
        (rust_path, rustfmt_text(PINS_TEST_RS)?),
        (json_path, format!("{}\n", pins_json(&rows))),
    ];
    let (packet_planned, hand_swaps) =
        planned_packets(resolved, workspace, classes, &shapes, !check)?;
    for (path, text) in packet_planned {
        put(&mut planned, path, text);
    }

    if check {
        let mut messages: Vec<String> = Vec::new();
        let shaped_modules: Vec<String> = shapes.iter().map(|(m, _)| m.clone()).collect();
        for (path, want) in &planned {
            if !path.is_file() {
                messages.push(format!("{}: missing", path.display()));
                continue;
            }
            if read(path)? != *want {
                messages.push(format!("{}: drift", path.display()));
            }
        }
        for (module, _) in &shapes {
            let Some(unit) = units.iter().find(|u| module_path(u) == *module) else {
                continue;
            };
            let path = packet_output_path(workspace, unit);
            if path.is_file() && !is_generated(&path) {
                messages.push(format!("{}: hand-written", path.display()));
            }
        }
        messages.extend(orphan_generated_packets(workspace, &shaped_modules)?);
        return Ok(messages);
    }

    refuse_hand_files(&planned, &hand_swaps)?;

    let mut written: Vec<PathBuf> = Vec::new();
    for (path, text) in &planned {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| emit(e.to_string()))?;
        }
        std::fs::write(path, text).map_err(|e| emit(format!("{}: {e}", path.display())))?;
        written.push(path.clone());
    }

    if !known_path.is_file() {
        if let Some(parent) = known_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| emit(e.to_string()))?;
        }
        std::fs::write(&known_path, "[]\n")
            .map_err(|e| emit(format!("{}: {e}", known_path.display())))?;
        written.push(known_path.clone());
    }

    rustfmt_paths(&written)?;
    Ok(written.iter().map(|p| posix_rel(p, workspace)).collect())
}
