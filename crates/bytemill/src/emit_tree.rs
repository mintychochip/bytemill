//! Emit shaped packets and the wire-pin gate into an rstom workspace.
//! Ports `bps/emit_tree.py`.

use crate::ast::ResolvedSchema;
use crate::emit_rstom::{is_generated, packet_body};
use crate::errors::{BmlResult, emit};
use crate::minestom::{ClassTable, Unit, build_units};
use crate::pins::{PINS_TEST_RS, build_pins, pins_json, pins_json_is_owned};
use crate::shapes::extract::{crate_root, extract_shape, packet_root};
use crate::shapes::ledger::{known_deviations, load_ledger, mismatch_fields, module_path};
use crate::shapes::model::Shape;
use crate::tree_bind::Bindings;
use crate::tree_bound::bound_body;
use crate::tree_packet::{tree_packet, tree_packet_bound, validate_shape};
use crate::tree_split::{conservation_gaps, split_hand, uncovered_constructs};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `shutil.which("rustfmt")`.
fn which_rustfmt() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("rustfmt"))
        .find(|candidate| candidate.is_file())
}

/// Run rustfmt on `path` in place, with the child-skipping flags when an
/// out-of-line sibling (`mod ext;` / `mod tests;`) is present.
fn run_rustfmt(rustfmt: &Path, path: &Path, text: &str) -> Result<(), String> {
    let mut cmd = Command::new(rustfmt);
    if text.contains("mod ext;") || text.contains("mod tests;") {
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
fn rustfmt_text(text: &str) -> BmlResult<String> {
    let Some(rustfmt) = which_rustfmt() else {
        return Ok(text.to_string());
    };
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("bml-emit-tree-{}-{seq}.rs", std::process::id()));
    std::fs::write(&path, text).map_err(|e| emit(format!("rustfmt failed: {e}")))?;
    let result = match run_rustfmt(&rustfmt, &path, text) {
        Ok(()) => crate::text::read_text(&path).map_err(|e| emit(format!("rustfmt failed: {e}"))),
        Err(err) => Err(emit(format!("rustfmt failed: {err}"))),
    };
    let _ = std::fs::remove_file(&path);
    result
}

/// `_rustfmt_paths(paths)`: format written `.rs` files in place.
fn rustfmt_paths(paths: &[PathBuf]) -> BmlResult<()> {
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

fn read(path: &Path) -> BmlResult<String> {
    crate::text::read_text(path).map_err(|e| emit(format!("{}: {e}", path.display())))
}

fn is_owned_output(path: &Path) -> BmlResult<bool> {
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

fn refuse_hand_files(planned: &[(PathBuf, String)], hand_swaps: &[PathBuf]) -> BmlResult<()> {
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
    packet_root(workspace).join(format!("{}.rs", module_path(unit)))
}

fn require_extras(workspace: &Path, shape: &Shape, packet_dir: &Path) -> BmlResult<()> {
    let Some(extras) = shape.extras.as_deref().filter(|e| !e.is_empty()) else {
        return Ok(());
    };
    let ext = packet_dir.join(extras);
    if !ext.is_file() {
        return Err(emit(format!("create {} first", posix_rel(&ext, workspace))));
    }
    Ok(())
}

fn require_tests(workspace: &Path, shape: &Shape, packet_dir: &Path) -> BmlResult<()> {
    let Some(tests) = shape.tests.as_deref().filter(|e| !e.is_empty()) else {
        return Ok(());
    };
    let path = packet_dir.join(tests);
    if !path.is_file() {
        return Err(emit(format!(
            "create {} first",
            posix_rel(&path, workspace)
        )));
    }
    Ok(())
}

fn refuse_orphan_ext(module: &str, shape: &Shape, packet_path: &Path) -> BmlResult<()> {
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

fn refuse_orphan_tests(module: &str, shape: &Shape, packet_path: &Path) -> BmlResult<()> {
    if shape.tests.as_deref().is_some_and(|e| !e.is_empty()) {
        return Ok(());
    }
    if !packet_path.is_file() || !is_generated(packet_path) {
        return Ok(());
    }
    let stem = module.rsplit('/').next().unwrap_or("");
    let tests_name = format!("{stem}.tests.rs");
    let dir = packet_path.parent().unwrap_or(Path::new(""));
    if dir.join(&tests_name).is_file() {
        return Err(emit(format!(
            "{module}: tests file {tests_name} would be orphaned: record tests in the shape"
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
            // The same lint allowance on several statements needs to survive once.
            if !markers.iter().any(|m| m == stripped) {
                markers.push(stripped.to_string());
            }
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
    let markers = hand_prefix_markers(hand_text);
    // Each marker is carried by the generated file or by the ext, each in hand order. A line
    // that both carry (the same `#[derive(...)]` on the packet and on a nested enum) must not
    // be settled greedily, so choose the assignment that loses the fewest markers.
    let mut memo: std::collections::HashMap<(usize, usize, usize), usize> =
        std::collections::HashMap::new();
    fn best(
        k: usize,
        gi: usize,
        ei: usize,
        markers: &[String],
        gen_lines: &[&str],
        ext_lines: &[&str],
        memo: &mut std::collections::HashMap<(usize, usize, usize), usize>,
    ) -> usize {
        if k == markers.len() {
            return 0;
        }
        if let Some(&m) = memo.get(&(k, gi, ei)) {
            return m;
        }
        let m = &markers[k];
        let mut out = 1 + best(k + 1, gi, ei, markers, gen_lines, ext_lines, memo);
        if let Some(g) = (gi..gen_lines.len()).find(|&j| gen_lines[j] == m) {
            out = out.min(best(k + 1, g + 1, ei, markers, gen_lines, ext_lines, memo));
        }
        if let Some(e) = (ei..ext_lines.len()).find(|&j| ext_lines[j] == m) {
            out = out.min(best(k + 1, gi, e + 1, markers, gen_lines, ext_lines, memo));
        }
        memo.insert((k, gi, ei), out);
        out
    }
    let mut gi = 0;
    let mut ei = 0;
    let mut missing = Vec::new();
    for (k, marker) in markers.iter().enumerate() {
        let want = best(k, gi, ei, &markers, &gen_lines, &ext_lines, &mut memo);
        let gpos = (gi..gen_lines.len()).find(|&j| gen_lines[j] == *marker);
        let epos = (ei..ext_lines.len()).find(|&j| ext_lines[j] == *marker);
        let via_gen = gpos.map(|g| {
            best(
                k + 1,
                g + 1,
                ei,
                &markers,
                &gen_lines,
                &ext_lines,
                &mut memo,
            )
        });
        let via_ext = epos.map(|e| {
            best(
                k + 1,
                gi,
                e + 1,
                &markers,
                &gen_lines,
                &ext_lines,
                &mut memo,
            )
        });
        if let (Some(g), Some(v)) = (gpos, via_gen)
            && v == want
        {
            gi = g + 1;
        } else if let (Some(e), Some(v)) = (epos, via_ext)
            && v == want
        {
            ei = e + 1;
        } else {
            missing.push(marker.clone());
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
) -> BmlResult<()> {
    let ext_text = match ext_path {
        Some(p) if p.is_file() => read(p)?,
        _ => String::new(),
    };
    assert_markers_preserved(module, hand_text, generated_text, &ext_text)
}

/// The marker check over an ext text already in hand (planned or read).
pub fn assert_markers_preserved(
    module: &str,
    hand_text: &str,
    generated_text: &str,
    ext_text: &str,
) -> BmlResult<()> {
    let missing = missing_hand_markers(hand_text, generated_text, ext_text);
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

fn hand_matches_ledger(path: &Path, shape: &Shape) -> BmlResult<bool> {
    let text = read(path)?;
    // Hooks, `accepted` and the bindings are ledger-only: nothing in the hand file produces them.
    let mut expected = shape.clone();
    expected.hooks = Default::default();
    expected.accepted = Vec::new();
    expected.constants = Default::default();
    expected.bound = false;
    expected.serializer_attrs = Vec::new();
    expected.uses = Vec::new();
    expected.alias = None;
    for f in &mut expected.fields {
        f.bind = Default::default();
    }
    match extract_shape(&text, &shape.cls, &shape.module, Some(path)) {
        Ok(hand) => Ok(hand == expected),
        Err(_) => Ok(false),
    }
}

fn orphan_generated_packets(workspace: &Path, shaped_modules: &[String]) -> BmlResult<Vec<String>> {
    let root = packet_root(workspace);
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
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) -> BmlResult<()> {
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

/// What planning the packet files yields.
struct PacketPlan {
    /// Generated files to write (path, text).
    planned: Vec<(PathBuf, String)>,
    /// Hand files that are swapped for generated ones.
    hand_swaps: Vec<PathBuf>,
    /// Sibling files (`<stem>.ext.rs`, `<stem>.tests.rs`) a swap creates when absent. Hand-owned
    /// once they exist, so never compared or overwritten.
    siblings: Vec<(PathBuf, String)>,
}

/// The sibling to create for `name` in `dir`, or the text that is already there.
fn sibling_text(
    dir: &Path,
    name: &Option<String>,
    fresh: Option<&String>,
    siblings: &mut Vec<(PathBuf, String)>,
) -> BmlResult<String> {
    let Some(name) = name.as_deref().filter(|n| !n.is_empty()) else {
        return Ok(String::new());
    };
    let path = dir.join(name);
    if path.is_file() {
        return read(&path);
    }
    let Some(text) = fresh else {
        return Ok(String::new());
    };
    let text = rustfmt_text(text)?;
    siblings.push((path, text.clone()));
    Ok(text)
}

fn planned_packets(
    resolved: &ResolvedSchema,
    workspace: &Path,
    classes: &ClassTable,
    shapes: &[(String, Shape)],
    bindings: &Bindings,
    swap_hand: bool,
) -> BmlResult<PacketPlan> {
    let units = build_units(resolved, classes)?;
    let modules: Vec<String> = units.iter().map(module_path).collect();
    for (module, _) in shapes {
        if !modules.contains(module) {
            return Err(emit(format!("stale shape: {module}")));
        }
    }
    let known = known_deviations(workspace)?;
    let mut plan = PacketPlan {
        planned: Vec::new(),
        hand_swaps: Vec::new(),
        siblings: Vec::new(),
    };
    for unit in &units {
        let module = module_path(unit);
        let Some((_, shape)) = shapes.iter().find(|(m, _)| *m == module) else {
            continue;
        };
        validate_shape(shape)?;
        // A domain limit is the hand type's, kept by the generated packet; the known entry stays
        // as the record of it. Any other known deviation is the hand port's wire, not Bytemill's.
        if let Some(d) = known
            .iter()
            .find(|d| d.class == shape.cls && d.category != "domain-limit")
        {
            return Err(emit(format!(
                "{module}: {} is a known wire deviation ({}) in wire_pins_known.json; \
                 a packet swapped to Bytemill must not be listed there",
                shape.cls, d.category
            )));
        }
        let mut generated = String::new();
        let raw = if shape.has_bindings() {
            let body = bound_body(unit, resolved, shape, bindings)?;
            generated = format!("{}\n{}\n{}", body.writes, body.reads, body.ctor.join("\n"));
            tree_packet_bound(unit, shape, &body)?
        } else {
            let body = packet_body(unit, resolved)?;
            let reasons = mismatch_fields(shape, &body.fields);
            if !reasons.is_empty() {
                return Err(emit(format!("{module}: {}", reasons.join("; "))));
            }
            tree_packet(unit, shape, &body)?
        };
        let path = packet_output_path(workspace, unit);
        let packet_dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        refuse_orphan_ext(&module, shape, &path)?;
        refuse_orphan_tests(&module, shape, &path)?;
        let text = rustfmt_text(&raw)?;
        let hand_present = path.is_file() && !is_generated(&path);
        if swap_hand && hand_present {
            if !hand_matches_ledger(&path, shape)? {
                return Err(emit(format!(
                    "hand file changed since its shape was extracted: {module}"
                )));
            }
            let hand = read(&path)?;
            let uncovered = uncovered_constructs(shape, &hand, &generated)?;
            if !uncovered.is_empty() {
                return Err(emit(format!(
                    "{module}: the hand serializer carries behaviour the generated wire code \
                     does not: {}; reproduce each with a hook or record it under \"accepted\" \
                     (construct and reason) in the shape, or keep the packet hand-written",
                    uncovered.join(", ")
                )));
            }
            let split = split_hand(shape, &hand, &raw)?;
            let ext = sibling_text(
                &packet_dir,
                &shape.extras,
                split.ext.as_ref(),
                &mut plan.siblings,
            )?;
            sibling_text(
                &packet_dir,
                &shape.tests,
                split.tests.as_ref(),
                &mut plan.siblings,
            )?;
            let gaps = conservation_gaps(shape, &hand, &raw, &ext, &split.dropped)?;
            if !gaps.is_empty() {
                return Err(emit(format!(
                    "{module}: hand items would be lost: {}",
                    gaps.join("; ")
                )));
            }
            // Dropped helpers take their docs with them; nothing else may go missing.
            let mut kept_hand = hand.clone();
            for dropped in &split.dropped_text {
                kept_hand = kept_hand.replace(dropped.as_str(), "");
            }
            assert_markers_preserved(&module, &kept_hand, &text, &ext)?;
            plan.hand_swaps.push(path.clone());
        } else {
            require_extras(workspace, shape, &packet_dir)?;
            require_tests(workspace, shape, &packet_dir)?;
        }
        put(&mut plan.planned, path, text);
    }
    Ok(plan)
}

/// `emit_tree(resolved, workspace, classes, shapes_dir, check)`: write the pin
/// gate and shaped packets. Returns the paths written, or drift messages in check mode.
pub fn emit_tree(
    resolved: &ResolvedSchema,
    workspace: &Path,
    classes: &ClassTable,
    shapes_dir: &Path,
    check: bool,
) -> BmlResult<Vec<String>> {
    let shapes = if shapes_dir.is_dir() {
        load_ledger(shapes_dir)?
    } else {
        Vec::new()
    };
    let units = build_units(resolved, classes)?;
    let rows = build_pins(resolved, &units)?;
    let tests = crate_root(workspace).join("tests");
    let rust_path = tests.join("bml_wire_pins.rs");
    let json_path = tests.join("bml").join("wire_pins.json");
    let known_path = tests.join("bml").join("wire_pins_known.json");

    let crate_name = crate_root(workspace).file_name().map_or_else(
        || "netherstar".to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let pins_test = PINS_TEST_RS.replace("rstom::", &format!("{crate_name}::"));
    let mut planned: Vec<(PathBuf, String)> = vec![
        (rust_path, rustfmt_text(&pins_test)?),
        (json_path, format!("{}\n", pins_json(&rows))),
    ];
    let bindings = Bindings::beside(shapes_dir)?;
    let packets = planned_packets(resolved, workspace, classes, &shapes, &bindings, !check)?;
    let PacketPlan {
        planned: packet_planned,
        hand_swaps,
        siblings,
    } = packets;
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
    for (path, text) in siblings.iter().chain(planned.iter()) {
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

/// The generated source of one shaped packet, formatted, without the swap checks: what
/// `emit rstom-tree` would write for `class`, for authoring a shape.
pub fn render_packet(
    resolved: &ResolvedSchema,
    classes: &ClassTable,
    shapes_dir: &Path,
    class: &str,
) -> BmlResult<String> {
    let shapes = load_ledger(shapes_dir)?;
    let units = build_units(resolved, classes)?;
    let unit = units
        .iter()
        .find(|u| u.cls.name == class)
        .ok_or_else(|| emit(format!("no packet class {class}")))?;
    let module = module_path(unit);
    let (_, shape) = shapes
        .iter()
        .find(|(m, _)| *m == module)
        .ok_or_else(|| emit(format!("no shape for {class} in {}", shapes_dir.display())))?;
    let bindings = Bindings::beside(shapes_dir)?;
    let raw = if shape.has_bindings() {
        let body = bound_body(unit, resolved, shape, &bindings)?;
        tree_packet_bound(unit, shape, &body)?
    } else {
        let body = packet_body(unit, resolved)?;
        tree_packet(unit, shape, &body)?
    };
    rustfmt_text(&raw)
}

/// The wire fields `class` flattens to, one per line.
pub fn describe_packet(
    resolved: &ResolvedSchema,
    classes: &ClassTable,
    class: &str,
) -> BmlResult<String> {
    let units = build_units(resolved, classes)?;
    let unit = units
        .iter()
        .find(|u| u.cls.name == class)
        .ok_or_else(|| emit(format!("no packet class {class}")))?;
    let (wires, _) = crate::emit_rstom::packet_fields(&unit.pkt, resolved)?;
    Ok(crate::tree_bound::describe(&wires))
}
