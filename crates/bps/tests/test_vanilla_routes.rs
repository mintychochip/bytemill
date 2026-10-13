//! Ports tests/test_vanilla_routes.py: the route table is vanilla's packet
//! registration order, not just the wiki's.
//!
//! A packet's id is its position in the protocol's builder chain in vanilla's
//! source (`GameProtocols` and its siblings), so the names at each id are
//! checked against that. The wiki is where the route table was transcribed
//! from; this is what makes the id of every packet, including the ones with
//! no recorded bytes, vanilla's.
//!
//! Deviation: the Python test uses `re` to read the builder chains; `std`
//! has no regex engine, so the two patterns are reproduced with plain string
//! scanning (each documented where it runs). The `CHAINS` and `ALIASES`
//! tables are identical.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

// (state, direction) -> the file and the builder chain that registers its packets.
const CHAINS: &[(&str, &str, &str, &str)] = &[
    ("handshaking", "serverbound", "handshake/HandshakeProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("status", "serverbound", "status/StatusProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("status", "clientbound", "status/StatusProtocols.java", "CLIENTBOUND_TEMPLATE"),
    ("login", "serverbound", "login/LoginProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("login", "clientbound", "login/LoginProtocols.java", "CLIENTBOUND_TEMPLATE"),
    ("configuration", "serverbound", "configuration/ConfigurationProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("configuration", "clientbound", "configuration/ConfigurationProtocols.java", "CLIENTBOUND_TEMPLATE"),
    ("play", "serverbound", "game/GameProtocols.java", "SERVERBOUND_TEMPLATE"),
    ("play", "clientbound", "game/GameProtocols.java", "CLIENTBOUND_TEMPLATE"),
];

// BPS names a packet as the wiki does; vanilla registers these under another name.
const ALIASES: &[(&str, &str)] = &[
    ("handshake", "intention"),
    ("login_start", "hello"),
    ("encryption_response", "key"),
    // Vanilla's one `show_dialog` has two wire forms by state; BPS has a packet for each.
    ("show_dialog_configuration", "show_dialog"),
];

/// `tests/test_vanilla_routes.py`'s `PROTOCOL`: two levels above the test
/// file, i.e. the repo root's sibling.
fn protocol_dir() -> PathBuf {
    common::repo_root().parent().unwrap().join("mc-ref/server-26.3/net/minecraft/network/protocol")
}

fn alias(name: &str) -> &str {
    ALIASES.iter().find(|(from, _)| *from == name).map(|(_, to)| *to).unwrap_or(name)
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

fn read_word(bytes: &[u8], mut i: usize) -> (usize, usize) {
    let start = i;
    while i < bytes.len() && is_word(bytes[i]) {
        i += 1;
    }
    (start, i)
}

/// One `PacketType<...> CONST = create(Client|Server)bound("name")`
/// declaration, mirroring
/// `PacketType<[^=;]+?>\s+(\w+)\s*=\s*create(Client|Server)bound\(\s*"([\w/]+)"\s*\)`:
/// `[^=;]+?` cannot cross `=` or `;`, so the constant's `=` is the first one
/// after `PacketType<`, and `(\w+)` is the word run ending at it.
fn scan_type_names(text: &str, stem: &str, out: &mut BTreeMap<(String, String), String>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(rel) = text[i..].find("PacketType<") else {
            break;
        };
        let j = i + rel + "PacketType<".len();
        let mut k = j;
        while k < bytes.len() && bytes[k] != b'=' && bytes[k] != b';' {
            k += 1;
        }
        if k < bytes.len() && bytes[k] == b'=' {
            let mut end = k;
            while end > j && matches!(bytes[end - 1], b' ' | b'\t' | b'\n' | b'\r') {
                end -= 1;
            }
            let mut start = end;
            while start > j && is_word(bytes[start - 1]) {
                start -= 1;
            }
            // The run needs `\s+` before it and `[^=;]+?` (at least one char) before that.
            if start < end && start > j + 1 && matches!(bytes[start - 1], b' ' | b'\t' | b'\n' | b'\r') {
                let mut p = skip_ws(bytes, k + 1);
                let mut maker = None;
                for prefix in ["createClientbound(", "createServerbound("] {
                    if text[p..].starts_with(prefix) {
                        maker = Some(prefix.len());
                        break;
                    }
                }
                if let Some(len) = maker {
                    p = skip_ws(bytes, p + len);
                    if p < bytes.len() && bytes[p] == b'"' {
                        let (ns, ne) = {
                            let mut q = p + 1;
                            while q < bytes.len()
                                && (is_word(bytes[q]) || bytes[q] == b'/')
                            {
                                q += 1;
                            }
                            (p + 1, q)
                        };
                        // `"name"` then `)`, i.e. `"([\w/]+)"\s*\)`.
                        if ne > ns && ne < bytes.len() && bytes[ne] == b'"' {
                            let r = skip_ws(bytes, ne + 1);
                            if r < bytes.len() && bytes[r] == b')' {
                                out.insert(
                                    (stem.to_string(), text[start..end].to_string()),
                                    text[ns..ne].replace('/', "_"),
                                );
                            }
                        }
                    }
                }
            }
        }
        i = j;
    }
}

/// `(file stem, CONSTANT)` -> the name a packet type is created with
/// (`debug/event` is `debug_event`).
fn type_names(protocol: &Path) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    let mut dirs = vec![protocol.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let is_types = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("PacketTypes.java"));
            if !is_types {
                continue;
            }
            let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
            let text = std::fs::read_to_string(&path).unwrap();
            scan_type_names(&text, &stem, &mut out);
        }
    }
    out
}

/// The packet names a builder chain registers, in the order that numbers them.
/// Mirrors `\.?(addPacket|withBundlePacket)\(\s*(\w+)\.(\w+)` over the chain
/// from ` TEMPLATE =` to the next `public static final`; a bundle is
/// registered by its delimiter, which takes the first id.
fn registered(
    protocol: &Path,
    file: &str,
    template: &str,
    names: &BTreeMap<(String, String), String>,
) -> Vec<String> {
    let text = std::fs::read_to_string(protocol.join(file)).unwrap();
    let needle = format!(" {template} =");
    let start = text.find(&needle).unwrap_or_else(|| panic!("{file} has no ` {template} =`"));
    let after = start + 1;
    let end = text[after..]
        .find("public static final")
        .map(|e| after + e)
        .unwrap_or(text.len());
    let chain = &text[start..end];
    let bytes = chain.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let next_add = chain[i..].find("addPacket(");
        let next_bundle = chain[i..].find("withBundlePacket(");
        let (rel, bundle) = match (next_add, next_bundle) {
            (Some(a), Some(b)) if b < a => (b, true),
            (Some(a), Some(_)) => (a, false),
            (None, Some(b)) => (b, true),
            (None, None) => break,
            (Some(a), None) => (a, false),
        };
        // Substring search finds the same groups as `\.?(...)`: the optional
        // dot only shifts where the match starts.
        let name_len = if bundle { "withBundlePacket".len() } else { "addPacket".len() };
        let open = i + rel + name_len;
        debug_assert_eq!(bytes[open], b'(');
        // `findall` continues after the match end; on a malformed call skip
        // past `(` so the scan stays total. Real chains always parse.
        let mut next = open + 1;
        let p = skip_ws(bytes, open + 1);
        let (os, oe) = read_word(bytes, p);
        if oe > os && bytes.get(oe) == Some(&b'.') {
            let (cs, ce) = read_word(bytes, oe + 1);
            if ce > cs {
                let owner = &chain[os..oe];
                let constant = chain[cs..ce].to_string();
                let key = if bundle { format!("{constant}_DELIMITER") } else { constant };
                let name = names
                    .get(&(owner.to_string(), key.clone()))
                    .unwrap_or_else(|| panic!("{file} registers unknown {owner}.{key}"));
                out.push(name.clone());
                next = ce;
            }
        }
        i = next;
    }
    out
}

fn bps_names(resolved: &bps::ResolvedSchema, state: &str, direction: &str) -> BTreeMap<i128, String> {
    let mut out = BTreeMap::new();
    for (route, pkt) in &resolved.routes {
        if route.state == state && route.direction == direction {
            let name = pkt.name.strip_suffix("_serverbound").unwrap_or(&pkt.name);
            out.insert(route.packet_id, alias(name).to_string());
        }
    }
    out
}

#[test]
fn every_route_is_the_packet_vanilla_registers_at_that_id() {
    let protocol = protocol_dir();
    if !protocol.is_dir() {
        eprintln!("skipped: vanilla 26.3 source not present");
        return;
    }
    let resolved = common::load_minecraft().unwrap();
    let names = type_names(&protocol);
    for &(state, direction, file, template) in CHAINS.iter() {
        let case = format!("state={state} direction={direction}");
        let vanilla = registered(&protocol, file, template, &names);
        let bps = bps_names(&resolved, state, direction);
        let got: Vec<Option<String>> =
            (0..vanilla.len()).map(|id| bps.get(&(id as i128)).cloned()).collect();
        let want: Vec<Option<String>> = vanilla.iter().map(|n| Some(n.clone())).collect();
        assert_eq!(got, want, "{case}");
        // The only id vanilla does not register is the pre-1.7 ping BPS also describes.
        let mut extra: Vec<i128> = bps
            .keys()
            .copied()
            .filter(|id| *id < 0 || *id as usize >= vanilla.len())
            .collect();
        extra.sort();
        let expected = if state == "handshaking" { vec![0xFE] } else { vec![] };
        assert_eq!(extra, expected, "{case}");
    }
}

#[test]
fn the_aliases_are_all_in_use() {
    if !protocol_dir().is_dir() {
        eprintln!("skipped: vanilla 26.3 source not present");
        return;
    }
    // An alias nothing needs would hide a rename that later stops matching.
    let resolved = common::load_minecraft().unwrap();
    let used: BTreeSet<&str> = resolved
        .routes
        .iter()
        .map(|(_, pkt)| pkt.name.strip_suffix("_serverbound").unwrap_or(&pkt.name))
        .collect();
    let mut unused: Vec<&str> =
        ALIASES.iter().map(|(from, _)| *from).filter(|a| !used.contains(a)).collect();
    unused.sort();
    assert_eq!(unused, Vec::<&str>::new());
}
