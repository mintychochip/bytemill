//! Ports tests/test_emit_rstom.py: the `rstom-packets` crate shape, generated from BML.
//!
//! Deviations from the Python test:
//! - The shared whole-protocol emit (Python's `setUpClass`) is a process-wide
//!   `LazyLock`, so the slow emit (plus its per-file `rustfmt` passes) runs once
//!   however many tests read it; its temp dir is left for the OS to reclaim
//!   because later tests still read it.
//! - Python `re` patterns are explicit scanners (no regex crate); each names the
//!   pattern it replaces.
//! - Python `@unittest.skipUnless` is the early-return `eprintln! + return`
//!   pattern, checked before the shared emit so the skips work even when the
//!   rstom worktree is absent (Python's `setUpClass` would fail first).
//! - The `cargo test` bound is Python's `timeout=900`, implemented by polling
//!   `try_wait` and killing the child past the deadline.

mod common;

use bytemill::errors::ErrorKind;
use bytemill::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);
static CARGO_MUTEX: Mutex<()> = Mutex::new(());

/// A unique throwaway directory: pid + counter + nanos, never pid-only.
fn unique_temp_dir(tag: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "bml-rstom-{tag}-{}-{n}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, text).unwrap();
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// One emit of the whole protocol, shared: it is the slow part.
struct Shared {
    out: PathBuf,
    src: PathBuf,
    units: Vec<bytemill::minestom::Unit>,
}

static SHARED: LazyLock<Shared> = LazyLock::new(|| {
    let resolved = common::load_minecraft().expect("load minecraft");
    let table =
        common::rstom_tables::real_table().expect("rstom worktree's packets.json not present");
    let units = bytemill::minestom::build_units(&resolved, &table).expect("build units");
    let out = unique_temp_dir("full").join("bps_rstom");
    // Absolute, so the crate also builds from here: the test never writes into rstom.
    let rstom_path = common::rstom_tables::rstom_dir()
        .join("crates/rstom")
        .to_string_lossy()
        .into_owned();
    bytemill::emit_rstom::emit_rstom(&resolved, &out, &table, &rstom_path).expect("emit rstom");
    let src = out.join("src");
    Shared { out, src, units }
});

fn shared() -> &'static Shared {
    &SHARED
}

/// The packet modules of the emitted crate, as `client/play/foo_packet`.
fn packet_modules(src: &Path, skip_tests: bool) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for group in ["client", "server"] {
        let mut stack = vec![src.join(group)];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let name = match path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                if name == "mod.rs" {
                    continue;
                }
                // rstom-packets keeps `*.tests.rs` beside its packets; they are not modules.
                if skip_tests && name.ends_with(".tests.rs") {
                    continue;
                }
                if !name.ends_with(".rs") {
                    continue;
                }
                let rel = path.strip_prefix(src).unwrap();
                let mut parts: Vec<String> = rel
                    .parent()
                    .map(|p| {
                        p.components()
                            .map(|c| c.as_os_str().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                parts.push(path.file_stem().unwrap().to_string_lossy().into_owned());
                out.insert(parts.join("/"));
            }
        }
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `pub mod (\w+);`, anchored to column 0 when `anchored`
/// (`re.findall(r"^pub mod (\w+);", text, re.M)`), anywhere otherwise.
fn pub_mods(text: &str, anchored: bool) -> Vec<String> {
    fn one(decl: &str) -> Option<String> {
        let rest = decl.strip_prefix("pub mod ")?;
        let end = rest.find(|c: char| !is_word_char(c)).unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() || !rest[end..].starts_with(';') {
            return None;
        }
        Some(name.to_string())
    }
    if anchored {
        return text.lines().filter_map(one).collect();
    }
    let mut out = Vec::new();
    let mut search = text;
    while let Some(i) = search.find("pub mod ") {
        let rest = &search[i..];
        if let Some(name) = one(rest) {
            out.push(name);
        }
        search = &rest["pub mod ".len()..];
    }
    out
}

/// Whether the module declares `const CLASS_NAME: &'static str = "<java>";`,
/// allowing whitespace around the `=` like `rf'const CLASS_NAME: &\'static str
/// =\s*"{re.escape(cls.java)}";'`.
fn declares_class_name(text: &str, java: &str) -> bool {
    let marker = "const CLASS_NAME: &'static str =";
    let mut search = text;
    while let Some(i) = search.find(marker) {
        let after = search[i + marker.len()..]
            .trim_start_matches(|c: char| c.is_whitespace())
            .to_string();
        if after.starts_with(&format!("\"{java}\";")) {
            return true;
        }
        search = &search[i + marker.len()..];
    }
    false
}

/// `re.findall(r"pub use \w+::(\w+_registry)", text)`.
fn registry_exports(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut search = text;
    while let Some(i) = search.find("pub use ") {
        let rest = &search[i + "pub use ".len()..];
        let end1 = rest.find(|c: char| !is_word_char(c)).unwrap_or(rest.len());
        if let Some(tail) = rest[end1..].strip_prefix("::") {
            let end2 = tail.find(|c: char| !is_word_char(c)).unwrap_or(tail.len());
            let name = &tail[..end2];
            if name.ends_with("_registry") {
                out.insert(name.to_string());
            }
        }
        search = rest;
    }
    out
}

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(char::is_whitespace)
}

/// `(\w+)"` at the start of `rest`: the word and what follows the closing quote.
fn word_then_quote(rest: &str) -> Option<(String, &str)> {
    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    if end == 0 || !rest[end..].starts_with('"') {
        return None;
    }
    Some((rest[..end].to_string(), &rest[end + 1..]))
}

/// `re.findall(r'"(\w+)"', body)`.
fn quoted_words(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find('"') {
        match word_then_quote(&rest[i + 1..]) {
            Some((word, after)) => {
                out.push(word);
                rest = after;
            }
            None => rest = &rest[i + 1..],
        }
    }
    out
}

type TableRow<'a> = ((String, String), Vec<String>, &'a str);

/// One row just after its `(`: `\s*"(\w+)",\s*"(\w+)",\s*&\[(.*?)\],?\s*\)`.
fn parse_table_row(rest: &str) -> Option<TableRow<'_>> {
    let rest = skip_ws(rest).strip_prefix('"')?;
    let (side, rest) = word_then_quote(rest)?;
    let rest = skip_ws(rest.strip_prefix(',')?).strip_prefix('"')?;
    let (state, rest) = word_then_quote(rest)?;
    let rest = skip_ws(rest.strip_prefix(',')?).strip_prefix("&[")?;
    // `(.*?)\],?\s*\)`: the shortest body whose `]` is followed by `,?\s*)`.
    let mut from = 0;
    loop {
        let close = from + rest[from..].find(']')?;
        let tail = &rest[close + 1..];
        let tail = tail.strip_prefix(',').unwrap_or(tail);
        if let Some(after) = skip_ws(tail).strip_prefix(')') {
            return Some(((side, state), quoted_words(&rest[..close]), after));
        }
        from = close + 1;
    }
}

/// The `(side, state, [classes])` rows of `tests/registry_tables.rs`, parsed like
/// `re.findall(r'\(\s*"(\w+)",\s*"(\w+)",\s*&\[(.*?)\],?\s*\)', text, re.S)`: the whole text
/// is scanned, so rustfmt-wrapped rows parse too, and the `registry_for` match arms (which
/// lack `&[`) are skipped.
fn parse_registry_tables(text: &str) -> BTreeMap<(String, String), Vec<String>> {
    let mut out = BTreeMap::new();
    let mut search = text;
    while let Some(i) = search.find('(') {
        let after_paren = &search[i + 1..];
        match parse_table_row(after_paren) {
            Some(((side, state), names, after)) => {
                out.insert((side, state), names);
                search = after;
            }
            None => search = after_paren,
        }
    }
    out
}

fn str_field(map: &Value, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// `tests/rstom_tables.py::CARGO_TARGET`, read the same way.
fn cargo_target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BML_CARGO_TARGET_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        return shm.join("rstom-bps-target");
    }
    std::env::temp_dir().join("bml-rstom-target")
}

fn resolved(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().unwrap().join(path)
}

#[test]
fn every_packet_module_of_rstom_packets_exists_at_the_same_path() {
    let rstom = common::rstom_tables::rstom_dir();
    let packets_dir = rstom.join("crates/rstom-packets");
    if !packets_dir.is_dir() {
        eprintln!("skipped: rstom-packets not present");
        return;
    }
    let shared = shared();
    let schema_text =
        std::fs::read_to_string(common::rstom_tables::packets_gen_schema()).unwrap();
    let data = bytemill::json::loads(&schema_text).unwrap();
    let mut hand_shared = BTreeSet::new();
    if let Some(Value::List(items)) = data.get("shared") {
        for item in items.iter() {
            if let Some(module) = str_field(item, "module") {
                hand_shared.insert(module);
            }
        }
    }
    let theirs: BTreeSet<String> = packet_modules(&packets_dir.join("src"), true)
        .difference(&hand_shared)
        .cloned()
        .collect();
    let ours = packet_modules(&shared.src, false);
    let missing: Vec<&String> = theirs.difference(&ours).collect();
    assert!(missing.is_empty(), "modules rstom-packets has and BML lacks: {missing:?}");
    // The one extra: the configuration form of a class Minestom gives two serializers.
    let extra: BTreeSet<String> = ours.difference(&theirs).cloned().collect();
    assert_eq!(
        extra,
        BTreeSet::from(["server/configuration/show_dialog_packet".to_string()])
    );
}

#[test]
fn each_module_declares_the_struct_and_class_name_of_its_unit() {
    let shared = shared();
    for unit in &shared.units {
        let cls = &unit.cls;
        let path = shared
            .src
            .join(&cls.group)
            .join(&cls.package)
            .join(format!("{}.rs", cls.stem));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("{}: missing module {}", cls.name, path.display()));
        assert!(
            text.contains(&format!("pub struct {} {{", cls.name)),
            "{}: struct declaration",
            cls.name
        );
        assert!(
            declares_class_name(&text, &cls.java),
            "{}: CLASS_NAME {:?}",
            cls.name,
            cls.java
        );
        let marker = if cls.group == "client" { "ClientPacket" } else { "ServerPacket" };
        assert!(
            text.contains(&format!("impl {marker} for {} {{}}", cls.name)),
            "{}: {marker} marker",
            cls.name
        );
    }
}

#[test]
fn each_package_lists_its_modules_and_serializers() {
    let shared = shared();
    for group in ["client", "server"] {
        let mut packages: Vec<String> = std::fs::read_dir(shared.src.join(group))
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        packages.sort();
        let group_text =
            std::fs::read_to_string(shared.src.join(format!("{group}.rs"))).unwrap();
        let declared = pub_mods(&group_text, false);
        assert_eq!(declared, packages, "{group}: package list");
        for package in &packages {
            let mut stems: Vec<String> = std::fs::read_dir(shared.src.join(group).join(package))
                .unwrap()
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()) == Some("rs")
                        && p.file_name().and_then(|n| n.to_str()) != Some("mod.rs")
                })
                .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
                .collect();
            stems.sort();
            let text = std::fs::read_to_string(
                shared.src.join(group).join(package).join("mod.rs"),
            )
            .unwrap();
            assert_eq!(pub_mods(&text, true), stems, "{group}/{package}: module list");
            assert_eq!(
                text.matches("::CLASS_NAME,").count(),
                stems.len(),
                "{group}/{package}: serializer rows"
            );
        }
    }
    let ported = std::fs::read_to_string(shared.src.join("ported.rs")).unwrap();
    assert_eq!(ported.matches("ported.extend(crate::").count(), 11, "ported package count");
}

#[test]
fn the_old_flat_layout_is_gone() {
    let shared = shared();
    assert!(!shared.src.join("packets").exists());
}

#[test]
fn the_registry_functions_are_the_ones_rstom_packets_exports() {
    let shared = shared();
    let text =
        std::fs::read_to_string(shared.src.join("packet_vanilla").join("mod.rs")).unwrap();
    let exported = registry_exports(&text);
    let states = ["handshake", "status", "login", "configuration", "play"];
    let expected: BTreeSet<String> = ["client", "server"]
        .iter()
        .flat_map(|side| {
            states.iter().map(move |state| format!("{side}_{state}_registry"))
        })
        .collect();
    assert_eq!(exported, expected);
}

#[test]
fn the_registry_test_expects_the_tables_minestom_recorded() {
    if !common::java_probes::available() {
        eprintln!("skipped: rstom probe fixtures not present");
        return;
    }
    let shared = shared();
    let text = std::fs::read_to_string(shared.out.join("tests/registry_tables.rs")).unwrap();
    let expected = parse_registry_tables(&text);
    let probe_text = std::fs::read_to_string(
        common::rstom_tables::rstom_dir().join("probes/expected/packetprobe.json"),
    )
    .unwrap();
    let probe = bytemill::json::loads(&probe_text).unwrap();
    let mut recorded: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    if let Some(Value::List(cases)) = probe.get("cases") {
        for case in cases.iter() {
            if str_field(case, "op").as_deref() != Some("registryTable") {
                continue;
            }
            let side = str_field(case, "side").expect("registryTable side");
            let state = str_field(case, "state").expect("registryTable state");
            let mut packets = Vec::new();
            if let Some(Value::List(ids)) = case.get("packets") {
                for packet in ids.iter() {
                    match packet {
                        Value::Str(name) => packets.push(name.to_string()),
                        other => panic!("registryTable packet name, got {other:?}"),
                    }
                }
            }
            recorded.insert((side, state), packets);
        }
    }
    assert_eq!(recorded.len(), 10, "recorded registry tables");
    assert_eq!(expected, recorded);
}

#[test]
fn the_compile_test_never_uses_rstoms_own_target_dir() {
    let target = cargo_target_dir();
    let rstom = common::rstom_tables::rstom_dir();
    assert_ne!(
        resolved(&target),
        resolved(&rstom.join("target")),
        "cargo target dir must not be the worktree's"
    );
    assert!(
        !target.starts_with(&rstom),
        "{} is inside {}",
        target.display(),
        rstom.display()
    );
}

#[test]
fn emitted_crate_compiles_and_passes_its_tests() {
    if Command::new("cargo").arg("--version").output().is_err() {
        eprintln!("skipped: cargo not installed");
        return;
    }
    let rstom = common::rstom_tables::rstom_dir();
    if !rstom.join("Cargo.toml").is_file() {
        eprintln!("skipped: rstom workspace not present");
        return;
    }
    let shared = shared();
    // One cargo run at a time: the machine also runs a benchmark loop.
    let _guard = CARGO_MUTEX.lock().unwrap();
    let script = common::repo_root().join("scripts/rstom_cargo.sh");
    let manifest = shared.out.join("Cargo.toml").to_string_lossy().into_owned();
    let mut child = Command::new(&script)
        .args(["test", "--manifest-path", manifest.as_str(), "--quiet"])
        .env("BML_RSTOM_DIR", &rstom)
        .env("BML_CARGO_TARGET_DIR", cargo_target_dir())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rstom_cargo.sh");
    // Python's `timeout=900`, by polling: never wait unboundedly.
    let deadline = Instant::now() + Duration::from_secs(900);
    loop {
        match child.try_wait().expect("poll cargo") {
            Some(_) => break,
            None => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let output = child.wait_with_output().expect("cargo output");
                    panic!(
                        "emitted crate tests timed out after 900s: {}{}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    let output = child.wait_with_output().expect("cargo output");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn every_packet_pins_the_rust_encoder_to_the_python_reference() {
    let shared = shared();
    let mut tests: Vec<PathBuf> = std::fs::read_dir(shared.out.join("tests"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("roundtrip_") && n.ends_with(".rs"))
                .unwrap_or(false)
        })
        .collect();
    tests.sort();
    let mut unpinned = Vec::new();
    for t in &tests {
        let text = std::fs::read_to_string(t).unwrap();
        if !text.contains("Pinned by the Python interpreter") {
            let stem = t.file_stem().unwrap().to_string_lossy().into_owned();
            unpinned.push(stem.strip_prefix("roundtrip_").unwrap_or(&stem).to_string());
        }
    }
    unpinned.sort();
    assert_eq!(tests.len(), shared.units.len(), "one test per struct");
    assert!(unpinned.is_empty(), "packets that only check consistency: {unpinned:?}");
}

#[test]
fn an_encoding_reused_in_one_packet_is_rejected() {
    let dir = unique_temp_dir("dupenc");
    let source = common::rstom_tables::probe_source(
        &common::repo_root(),
        "encoding pair = {\n    n: varint\n    items: varint[] count = n\n}\npacket probe { first: pair  second: pair }",
    );
    let schema = dir.join("probe.bml");
    write_file(&schema, &source);
    let resolved = bytemill::analyze(&bytemill::load_schema(std::slice::from_ref(&schema)).unwrap()).unwrap();
    let err = bytemill::emit_rstom::emit_rstom(
        &resolved,
        &dir.join("crate"),
        &common::rstom_tables::probe_table(),
        "../rstom",
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Emit, "{err}");
    // `assertRaisesRegex(EmitError, "'n'.*first.*second")`, without a regex crate.
    let msg = err.to_string();
    let n = msg.find("'n'").unwrap_or_else(|| panic!("{msg}"));
    let first = msg.find("'first'").unwrap_or_else(|| panic!("{msg}"));
    let second = msg.find("'second'").unwrap_or_else(|| panic!("{msg}"));
    assert!(n < first && first < second, "{msg}");
    cleanup(&dir);
}

#[test]
fn a_re_emit_drops_stale_generated_files_and_keeps_hand_written_ones() {
    let dir = unique_temp_dir("sweep");
    let schema = dir.join("probe.bml");
    write_file(
        &schema,
        &common::rstom_tables::probe_source(&common::repo_root(), "packet probe { a: u8 }"),
    );
    let resolved = bytemill::analyze(&bytemill::load_schema(std::slice::from_ref(&schema)).unwrap()).unwrap();
    let out = dir.join("crate");
    let generated = "//! Generated by `bml emit rust`: left over from an earlier emit.\n";
    let stale = [
        out.join("src/packets/old.rs"),
        out.join("src/client/play/gone_packet.rs"),
        out.join("tests/roundtrip_gone.rs"),
    ];
    let hand = [out.join("src/util.rs"), out.join("tests/mine.rs")];
    for path in stale.iter().chain(hand.iter()) {
        let text = if stale.contains(path) { generated } else { "// by hand\n" };
        write_file(path, text);
    }
    bytemill::emit_rstom::emit_rstom(&resolved, &out, &common::rstom_tables::probe_table(), "../rstom")
        .unwrap();
    let remaining: Vec<&PathBuf> = stale.iter().filter(|p| p.exists()).collect();
    assert!(remaining.is_empty(), "stale files survive: {remaining:?}");
    assert!(!out.join("src/packets").exists(), "an emptied directory is removed");
    let lost: Vec<&PathBuf> = hand.iter().filter(|p| !p.exists()).collect();
    assert!(lost.is_empty(), "hand-written files dropped: {lost:?}");
    assert!(out.join("src/server/play/probe_packet.rs").is_file());
    cleanup(&dir);
}

/// The emitter reads the first line back to decide a file is its own. Files written before the CLI
/// became `bml` carry the old banner and must still count, or a tree generated back then would
/// look hand-written and never be refreshed.
#[test]
fn is_generated_accepts_the_current_and_the_pre_rename_banner_on_the_first_line_only() {
    let dir = unique_temp_dir("marker");
    let cases = [
        ("current.rs", "//! Generated by `bml emit rust`: probe.\n", true),
        ("pre_rename.rs", "//! Generated by `bps emit rust`: older.\n", true),
        ("hand.rs", "// by hand\n", false),
        ("second_line.rs", "// by hand\n//! Generated by `bml emit rust`\n", false),
    ];
    for (name, text, expected) in cases {
        let path = dir.join(name);
        write_file(&path, text);
        assert_eq!(bytemill::emit_rstom::is_generated(&path), expected, "{name}");
    }
    cleanup(&dir);
}
