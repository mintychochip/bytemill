//! The generated protocol schemas in `schemas/versions/`: every manifest entry loads, its route
//! table is the source's, and every packet survives encode -> decode -> encode.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bytemill::interp::{decode_packet, encode_packet};
use bytemill::protodef::versions::{VersionEntry, generate as generate_schema, import_versions, load_protocol, parse_manifest};
use bytemill::wiregen::generate;
use bytemill::{ResolvedSchema, analyze, load_schema};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The fetched minecraft-data tree (`data/minecraft-data/pc`): untracked and
/// possibly absent on a fresh clone. Tests that read it skip otherwise.
const MISSING_DATA_MSG: &str =
    "skipped: data/minecraft-data not fetched; run scripts/fetch_minecraft_data.py";

/// True when every manifest entry's `protocol.json` is present under
/// `data/minecraft-data/`, so the importer tests can run.
fn minecraft_data_present() -> bool {
    let dir = root().join("data/minecraft-data");
    if !dir.join("pc").is_dir() {
        return false;
    }
    entries()
        .iter()
        .all(|e| dir.join(&e.data).join("protocol.json").is_file())
}

fn entries() -> Vec<VersionEntry> {
    let text = std::fs::read_to_string(root().join("schemas/versions/manifest.json")).unwrap();
    parse_manifest(&text).unwrap()
}

fn load(entry: &VersionEntry) -> ResolvedSchema {
    let path = root().join("schemas/versions").join(entry.file_name());
    let schema = load_schema(&[path]).unwrap_or_else(|e| panic!("{}: {e}", entry.release));
    analyze(&schema).unwrap_or_else(|e| panic!("{}: {e}", entry.release))
}

#[test]
fn every_version_loads_and_declares_its_protocol_number() {
    for entry in entries() {
        let resolved = load(&entry);
        let proto = resolved
            .protocols
            .values()
            .next()
            .expect("a protocol block");
        assert_eq!(
            proto.protocol_id,
            i128::from(entry.protocol),
            "{}",
            entry.release
        );
    }
}

/// The range that was asked for: every Java release from 1.13 to 26.3 has a schema, found through the
/// protocol number it shares with its neighbours.
#[test]
fn the_manifest_covers_every_release_from_1_13_to_26_3() {
    const RELEASES: [&str; 51] = [
        "1.13", "1.13.1", "1.13.2", "1.14", "1.14.1", "1.14.2", "1.14.3", "1.14.4", "1.15",
        "1.15.1", "1.15.2", "1.16", "1.16.1", "1.16.2", "1.16.3", "1.16.4", "1.16.5", "1.17",
        "1.17.1", "1.18", "1.18.1", "1.18.2", "1.19", "1.19.1", "1.19.2", "1.19.3", "1.19.4",
        "1.20", "1.20.1", "1.20.2", "1.20.3", "1.20.4", "1.20.5", "1.20.6", "1.21", "1.21.1",
        "1.21.2", "1.21.3", "1.21.4", "1.21.5", "1.21.6", "1.21.7", "1.21.8", "1.21.9", "1.21.10",
        "1.21.11", "26.1", "26.1.1", "26.1.2", "26.2", "26.3",
    ];
    let entries = entries();
    let mut covered: Vec<&str> = entries
        .iter()
        .flat_map(|e| std::iter::once(e.release.as_str()).chain(e.also.iter().map(String::as_str)))
        .collect();
    let listed = covered.len();
    covered.sort_unstable();
    covered.dedup();
    assert_eq!(
        covered.len(),
        listed,
        "a release is listed under two protocol numbers"
    );
    let mut want = RELEASES.to_vec();
    want.sort_unstable();
    assert_eq!(covered, want);
    let protocols: Vec<u32> = entries.iter().map(|e| e.protocol).collect();
    assert_eq!(
        (protocols.first(), protocols.last()),
        (Some(&393), Some(&777))
    );
    assert!(
        protocols.windows(2).all(|w| w[0] < w[1]),
        "protocol numbers must ascend: {protocols:?}"
    );
}

/// The ids of a version's description (patches applied) by (state, direction): the oracle for the
/// route tables.
fn source_routes(entry: &VersionEntry) -> BTreeMap<(String, String, i128), String> {
    let (proto, _) = load_protocol(
        entry,
        &root().join("data/minecraft-data"),
        &root().join("schemas/versions"),
    )
    .unwrap();
    let mut out = BTreeMap::new();
    for scope in &proto.scopes {
        for pk in &scope.packets {
            out.insert(
                (
                    scope.state.clone(),
                    scope.direction.as_str().to_string(),
                    pk.id,
                ),
                pk.name.clone(),
            );
        }
    }
    out
}

#[test]
fn route_tables_are_the_sources() {
    if !minecraft_data_present() {
        eprintln!("{MISSING_DATA_MSG}");
        return;
    }
    for entry in entries() {
        let resolved = load(&entry);
        let want = source_routes(&entry);
        let got: BTreeMap<(String, String, i128), String> = resolved
            .routes
            .iter()
            .map(|(r, p)| {
                (
                    (r.state.clone(), r.direction.clone(), r.packet_id),
                    p.name.clone(),
                )
            })
            .collect();
        assert_eq!(got.len(), want.len(), "{}: route count", entry.release);
        for (key, name) in &want {
            let packet = got
                .get(key)
                .unwrap_or_else(|| panic!("{}: no route for {key:?}", entry.release));
            assert!(
                packet == name || packet.starts_with(&format!("{name}_")),
                "{}: {key:?} routes to {packet}, the source says {name}",
                entry.release
            );
        }
    }
}

/// Random bytes drawn from each packet's own schema: they must decode strictly, and encoding
/// what was decoded must give the same bytes.
const SEEDS: u64 = 12;

/// `BPS_SEEDS=300 cargo test ...` runs a longer pass (about 98% of the branch sites).
fn seeds() -> u64 {
    std::env::var("BPS_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SEEDS)
}

fn round_trip_failures(resolved: &ResolvedSchema, release: &str) -> (usize, Vec<String>) {
    let mut failures = Vec::new();
    let mut checked = 0;
    for (route, pkt) in &resolved.routes {
        let name = &pkt.name;
        for seed in 0..seeds() {
            let tag = format!(
                "{release} {}.{} {name} seed {seed}",
                route.state, route.direction
            );
            let bytes = match generate(resolved, name, seed) {
                Ok(b) => b,
                Err(e) => {
                    failures.push(format!("{tag}: generate {e}"));
                    continue;
                }
            };
            let decoded = match decode_packet(resolved, name, &bytes, true) {
                Ok(d) => d,
                Err(e) => {
                    failures.push(format!("{tag}: decode {e}"));
                    continue;
                }
            };
            match encode_packet(resolved, name, &decoded) {
                Ok(re) if re == bytes => checked += 1,
                Ok(re) => failures.push(format!(
                    "{tag}: re-encode differs ({} vs {} bytes)",
                    bytes.len(),
                    re.len()
                )),
                Err(e) => failures.push(format!("{tag}: encode {e}")),
            }
        }
    }
    (checked, failures)
}

#[test]
fn generated_packets_round_trip_in_every_version() {
    let mut all = Vec::new();
    let mut checked = 0;
    for entry in entries() {
        let (n, f) = round_trip_failures(&load(&entry), &entry.release);
        checked += n;
        all.extend(f);
    }
    let shown: Vec<&str> = all.iter().take(40).map(String::as_str).collect();
    assert!(
        all.is_empty(),
        "{} failures ({checked} ok), first {}:\n{}",
        all.len(),
        shown.len(),
        shown.join("\n")
    );
}

#[test]
#[ignore = "diagnostic: lists every round-trip failure"]
fn list_round_trip_failures() {
    let mut out = Vec::new();
    for entry in entries() {
        let (_, f) = round_trip_failures(&load(&entry), &entry.release);
        out.extend(f);
    }
    std::fs::write(
        std::env::var("BPS_FAILURES").unwrap_or_else(|_| "/tmp/bps_failures.txt".into()),
        out.join("\n"),
    )
    .unwrap();
}

#[test]
#[ignore = "diagnostic: branch coverage of the generated packets, BPS_SEEDS runs per packet"]
fn report_generator_coverage() {
    let seeds = seeds();
    for entry in entries() {
        let resolved = load(&entry);
        let mut seen = std::collections::HashSet::new();
        for (_, pkt) in &resolved.routes {
            for seed in 0..seeds {
                let _ = bytemill::wiregen::generate_covering(&resolved, &pkt.name, seed, &mut seen);
            }
        }
        let total = bytemill::wiregen::branch_sites(&resolved);
        eprintln!(
            "{:8} {:5} / {:5} branch sites = {:.0}%",
            entry.release,
            seen.len(),
            total,
            100.0 * seen.len() as f64 / total as f64
        );
    }
}

#[test]
#[ignore = "writes wire vectors drawn from every generated schema, for the ProtoDef oracle and the Python parity run"]
fn write_generated_vectors() {
    let dir = std::path::PathBuf::from(
        std::env::var("BPS_VECTORS_OUT").expect("BPS_VECTORS_OUT=<directory>"),
    );
    std::fs::create_dir_all(&dir).unwrap();
    let only = std::env::var("BPS_ONLY").ok();
    for entry in entries() {
        if only.as_deref().is_some_and(|o| o != entry.release) {
            continue;
        }
        let resolved = load(&entry);
        let mut out = String::new();
        for (route, pkt) in &resolved.routes {
            for seed in 0..seeds() {
                let Ok(bytes) = generate(&resolved, &pkt.name, seed) else {
                    continue;
                };
                if decode_packet(&resolved, &pkt.name, &bytes, true).is_err() {
                    continue;
                }
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                let dir_name = if route.direction == "clientbound" {
                    "toClient"
                } else {
                    "toServer"
                };
                // The id alone names the packet: the oracle's names are the data's, not BPS's.
                out.push_str(&format!(
                    "{{\"state\":\"{}\",\"direction\":\"{dir_name}\",\"id\":{},\"hex\":\"{hex}\"}}\n",
                    route.state, route.packet_id
                ));
            }
        }
        std::fs::write(dir.join(format!("{}.jsonl", entry.release)), out).unwrap();
    }
}

#[test]
fn committed_schemas_are_what_the_importer_writes() {
    if !minecraft_data_present() {
        eprintln!("{MISSING_DATA_MSG}");
        return;
    }
    let report = import_versions(
        &root().join("schemas/versions/manifest.json"),
        &root().join("data/minecraft-data"),
        &root().join("schemas/versions"),
        true,
    )
    .unwrap();
    assert!(
        report.stale.is_empty(),
        "stale (run `bml import-versions`): {:?}",
        report.stale
    );
    assert_eq!(report.roots, entries().len());
    assert!(report.shared > 0, "no shared files");
}

/// One packet per packet type and version, generated by the ProtoDef oracle from the same
/// `protocol.json` (`scripts/verify_versions.py --pin`), decoded strictly and encoded back, no Node
/// needed.
#[test]
fn oracle_vectors_round_trip() {
    let dir = root().join("crates/bytemill/tests/fixtures/protodef_vectors");
    let mut total = 0;
    for entry in entries() {
        let text = std::fs::read_to_string(dir.join(format!("{}.jsonl", entry.release)))
            .unwrap_or_else(|e| {
                panic!(
                    "{}: {e} (run scripts/verify_versions.py --pin)",
                    entry.release
                )
            });
        let report = bytemill::wire::check_vectors(&load(&entry), &text, false).unwrap();
        let shown: Vec<String> = report
            .failures
            .iter()
            .take(5)
            .map(|f| format!("line {} {}: {}", f.line, f.packet, f.reason))
            .collect();
        assert!(
            report.failures.is_empty(),
            "{}: {} failures, first:\n{}",
            entry.release,
            report.failures.len(),
            shown.join("\n")
        );
        assert_eq!(
            report.checked,
            text.lines().count(),
            "{}: some vectors had no route",
            entry.release
        );
        total += report.checked;
    }
    assert!(total > 7000, "only {total} vectors");
}

/// Compare two name-keyed declaration maps: same names, AST-equal values.
fn compare_maps<V: PartialEq + std::fmt::Debug>(
    release: &str,
    kind: &str,
    a: &std::collections::BTreeMap<&str, V>,
    b: &std::collections::BTreeMap<&str, V>,
) {
    assert_eq!(a.len(), b.len(), "{release}: {kind} count differs");
    for (name, da) in a {
        let db = b
            .get(name)
            .unwrap_or_else(|| panic!("{release}: {kind} {name} missing from the single file"));
        assert_eq!(da, db, "{release}: {kind} {name} differs");
    }
}

/// The merged schema loaded from the root equals the merged schema loaded from the single-file
/// rendering, declaration by declaration (AST PartialEq; declaration order is not meaningful),
/// protocol block included. Built fresh in a scratch directory, never the committed tree.
#[test]
fn shared_layout_means_what_the_single_file_means() {
    if !minecraft_data_present() {
        eprintln!("{MISSING_DATA_MSG}");
        return;
    }
    struct TempRoot(std::path::PathBuf);
    impl TempRoot {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "bps-share-equiv-{tag}-{}-{n}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
            ));
            Self(dir)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let tmp = TempRoot::new("equiv");
    std::fs::create_dir_all(tmp.0.join("std")).unwrap();
    for f in ["integers.bps", "varint.bps"] {
        std::fs::copy(root().join("schemas/std").join(f), tmp.0.join("std").join(f)).unwrap();
    }
    let report = import_versions(
        &root().join("schemas/versions/manifest.json"),
        &root().join("data/minecraft-data"),
        &tmp.0.join("out"),
        false,
    )
    .unwrap();
    assert!(report.stale.is_empty());
    let manifest_dir = root().join("schemas/versions/manifest.json");
    let manifest_dir = manifest_dir.parent().unwrap();
    for entry in entries() {
        let name = entry.file_name();
        let single = generate_schema(&entry, &root().join("data/minecraft-data"), manifest_dir)
            .unwrap_or_else(|e| panic!("{}: {e}", entry.release));
        let single_dir = tmp.0.join("single");
        std::fs::create_dir_all(&single_dir).unwrap();
        std::fs::write(single_dir.join(&name), &single).unwrap();
        let merged_root = load_schema(&[tmp.0.join("out").join(&name)])
            .unwrap_or_else(|e| panic!("{} root: {e}", entry.release));
        let merged_single = load_schema(&[single_dir.join(&name)])
            .unwrap_or_else(|e| panic!("{} single: {e}", entry.release));
        let release = &entry.release;
        let (a, b) = (&merged_root, &merged_single);
        {
            let map_a: std::collections::BTreeMap<&str, &bytemill::ast::EncodingDecl> =
                a.encodings.iter().map(|e| (e.name.as_str(), e)).collect();
            let map_b: std::collections::BTreeMap<&str, &bytemill::ast::EncodingDecl> =
                b.encodings.iter().map(|e| (e.name.as_str(), e)).collect();
            compare_maps(release, "encoding", &map_a, &map_b);
        }
        {
            let map_a: std::collections::BTreeMap<&str, &bytemill::ast::PacketDecl> =
                a.packets.iter().map(|p| (p.name.as_str(), p)).collect();
            let map_b: std::collections::BTreeMap<&str, &bytemill::ast::PacketDecl> =
                b.packets.iter().map(|p| (p.name.as_str(), p)).collect();
            compare_maps(release, "packet", &map_a, &map_b);
        }
        {
            let map_a: std::collections::BTreeMap<&str, &bytemill::ast::EnumDecl> =
                a.enums.iter().map(|e| (e.name.as_str(), e)).collect();
            let map_b: std::collections::BTreeMap<&str, &bytemill::ast::EnumDecl> =
                b.enums.iter().map(|e| (e.name.as_str(), e)).collect();
            compare_maps(release, "enum", &map_a, &map_b);
        }
            let release = &entry.release;

        assert_eq!(
            a.protocols.len(),
            b.protocols.len(),
            "{release}: protocol count differs"
        );
        assert_eq!(
            a.protocols.first(),
            b.protocols.first(),
            "{release}: protocol block differs"
        );
    }
}

/// Every shared file parses and analyzes on its own: imports resolve, nothing external remains.
#[test]
fn every_shared_file_checks_alone() {
    let mut n = 0;
    for dir in std::fs::read_dir(root().join("schemas/versions/common")).unwrap() {
        let path = dir.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("bps") {
            continue;
        }
        let schema = load_schema(std::slice::from_ref(&path)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        analyze(&schema).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        n += 1;
    }
    assert!(n > 0, "no shared files under common/");
}
