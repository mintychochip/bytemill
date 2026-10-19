//! Ports tests/test_shapes_real_tree_imports.py: kept hand `use` lines bind
//! names the packet needs (none owned by the generator), a packet importing
//! the coordinate `Vec` has no bare `Vec` field, and only the known packets
//! resist extraction.
//!
//! Python's three regexes are reproduced by explicit scanning with the same
//! semantics (`strip_use_statements`, `has_coord_vec_import`, `has_bare_vec`;
//! `has_word` is the `\b<name>\b` check). `bytemill::emit_rstom::WIRE_TYPES` is
//! already `pub`, so `GENERATOR_OWNED` builds from it directly. Each test
//! rebuilds the unit/shape pairs (Python shares them via `setUpClass`);
//! sibling files are read in sorted order (Python's glob order is arbitrary).

mod common;

use bytemill::minestom::Unit;
use bytemill::shapes::Shape;
use std::collections::BTreeSet;
use std::path::Path;

const KNOWN_UNEXTRACTABLE: &[&str] = &["ClientSignedCommandChatPacket", "ShowDialogPacket"];

fn packet_sources_present() -> bool {
    common::rstom_tables::rstom_dir()
        .join("crates/rstom/src/network/packet")
        .is_dir()
}

/// Every unit whose hand file extracts, with its shape.
type Pairs = Vec<(Unit, Shape)>;

/// Python's `setUpClass`: every unit whose hand file extracts, plus the names
/// of the units that have no hand file or raise `ShapeError`. `None` when the
/// worktree (or its packets.json) is absent, mirroring the class-level skip.
fn collect_pairs() -> Option<(Pairs, Vec<String>)> {
    if !packet_sources_present() {
        return None;
    }
    if !common::rstom_tables::packets_gen_schema().is_file() {
        return None;
    }
    let resolved = common::load_minecraft().unwrap();
    let table = common::rstom_tables::real_table().unwrap();
    let units = bytemill::minestom::build_units(&resolved, &table).unwrap();
    let rstom = common::rstom_tables::rstom_dir();
    let mut pairs = Vec::new();
    let mut skipped = Vec::new();
    for unit in units {
        let path = bytemill::shapes::hand_packet_path(&rstom, &unit);
        if !path.is_file() {
            skipped.push(unit.cls.name.clone());
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        let module = bytemill::shapes::module_path(&unit);
        match bytemill::shapes::extract_shape(&source, &unit.cls.name, &module, Some(&path)) {
            Ok(shape) => pairs.push((unit, shape)),
            Err(err) if err.is(bytemill::errors::ErrorKind::Shape) => {
                skipped.push(unit.cls.name.clone());
            }
            Err(err) => panic!("extract_shape({}) failed: {err}", unit.cls.name),
        }
    }
    Some((pairs, skipped))
}

fn hand_and_siblings(rstom: &Path, unit: &Unit) -> (String, String) {
    let path = bytemill::shapes::hand_packet_path(rstom, unit);
    let hand = std::fs::read_to_string(&path).unwrap();
    let prefix = format!("{}.", unit.cls.stem);
    let own = format!("{}.rs", unit.cls.stem);
    let mut siblings = Vec::new();
    if let Some(parent) = path.parent()
        && let Ok(entries) = std::fs::read_dir(parent)
    {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `{stem}.*.rs` never matches the packet file itself.
            if name != own && name.starts_with(&prefix) && name.ends_with(".rs") {
                siblings.push(std::fs::read_to_string(entry.path()).unwrap());
            }
        }
    }
    siblings.sort();
    (hand, siblings.join("\n"))
}

fn generator_owned() -> BTreeSet<&'static str> {
    let mut owned: BTreeSet<&'static str> = [
        "Arc",
        "ClientPacket",
        "ClientPacketGroup",
        "ConstantSerializer",
        "NetworkBuffer",
        "Packet",
        "PacketSerializer",
        "PacketValue",
        "ServerPacket",
        "ServerPacketGroup",
        "Uuid",
        "downcast",
        "fixed_raw_bytes",
        "FIXED_BYTES",
        "RAW_BYTES",
        "VAR_INT",
    ]
    .into_iter()
    .collect();
    for (_, wire, _) in bytemill::emit_rstom::WIRE_TYPES {
        owned.insert(*wire);
    }
    owned
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// End of a `use ...;` statement starting at a line start, reproducing
/// `_USE = re.compile(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?[ \t]+)?use[ \t][^;]*;")`.
fn use_match_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    let mut k = j;
    if bytes.get(k..).is_some_and(|r| r.starts_with(b"pub")) {
        let mut m = k + 3;
        let mut group_ok = true;
        if bytes.get(m) == Some(&b'(') {
            match bytes[m..].iter().position(|&c| c == b')') {
                Some(off) => m += off + 1,
                None => group_ok = false,
            }
        }
        if group_ok {
            let mut w = m;
            while w < bytes.len() && (bytes[w] == b' ' || bytes[w] == b'\t') {
                w += 1;
            }
            if w > m {
                k = w;
            }
        }
    }
    if !bytes.get(k..).is_some_and(|r| r.starts_with(b"use")) {
        return None;
    }
    let after = k + 3;
    if bytes.get(after) != Some(&b' ') && bytes.get(after) != Some(&b'\t') {
        return None;
    }
    let semi = bytes[after + 1..].iter().position(|&c| c == b';')?;
    Some(after + 1 + semi + 1)
}

/// Reproduce `_USE.sub("", hand)`.
fn strip_use_statements(hand: &str) -> String {
    let bytes = hand.as_bytes();
    let mut out = String::with_capacity(hand.len());
    let mut kept_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if (i == 0 || bytes[i - 1] == b'\n')
            && let Some(end) = use_match_end(bytes, i)
        {
            out.push_str(&hand[kept_start..i]);
            kept_start = end;
            i = end;
            continue;
        }
        i += 1;
    }
    out.push_str(&hand[kept_start..]);
    out
}

/// The names a kept `use` statement binds, reproducing `_bound_names`.
fn bound_names(statement: &str) -> Vec<String> {
    let mut inner = statement;
    if let Some(rest) = inner.strip_prefix("use")
        && rest.starts_with(|c: char| c.is_whitespace())
    {
        inner = rest.trim_start();
    }
    let inner = inner.trim_end_matches(';');
    let mut names = Vec::new();
    for part in inner.split(['{', '}', ',']) {
        let part = part.trim();
        if part.is_empty() || part.ends_with("::") {
            continue;
        }
        let name = if let Some((_, alias)) = part.rsplit_once(" as ") {
            alias
        } else if let Some((_, last)) = part.rsplit_once("::") {
            last
        } else {
            part
        };
        let name = name.trim().to_string();
        if !["", "_", "*", "self"].contains(&name.as_str()) {
            names.push(name);
        }
    }
    names
}

/// Reproduce `re.search(rf"\b{re.escape(name)}\b", haystack)`.
fn has_word(haystack: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let h: Vec<char> = haystack.chars().collect();
    let n: Vec<char> = name.chars().collect();
    if n.len() > h.len() {
        return false;
    }
    let first_word = is_word_char(n[0]);
    let last_word = is_word_char(n[n.len() - 1]);
    for i in 0..=(h.len() - n.len()) {
        if !h[i..].starts_with(n.as_slice()) {
            continue;
        }
        let left_ok = match i.checked_sub(1).map(|p| h[p]) {
            None => first_word,
            Some(c) => is_word_char(c) != first_word,
        };
        let right_ok = match h.get(i + n.len()).copied() {
            None => last_word,
            Some(c) => is_word_char(c) != last_word,
        };
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

/// Reproduce `_COORD_VEC_IMPORT.search(hand)`.
fn has_coord_vec_import(hand: &str) -> bool {
    let prefix = "crate::coordinate::";
    let mut search = hand;
    while let Some(pos) = search.find(prefix) {
        let mut rest = &search[pos + prefix.len()..];
        if let Some(after) = rest.strip_prefix("vec::") {
            rest = after;
        }
        if let Some(after) = rest.strip_prefix("Vec") {
            if after.chars().next().is_none_or(|c| !is_word_char(c)) {
                return true;
            }
        } else if let Some(brace) = rest.strip_prefix('{') {
            let segment = brace.split('}').next().unwrap_or(brace);
            if has_word(segment, "Vec") {
                return true;
            }
        }
        search = &search[pos + 1..];
    }
    false
}

/// Reproduce `_BARE_VEC.search(field.type)`.
fn has_bare_vec(ty: &str) -> bool {
    let chars: Vec<char> = ty.chars().collect();
    let needle = ['V', 'e', 'c'];
    for i in 0..chars.len() {
        if !chars[i..].starts_with(&needle) {
            continue;
        }
        let prev_ok = match i.checked_sub(1).map(|p| chars[p]) {
            None => true,
            Some(c) => !is_word_char(c) && c != ':',
        };
        if !prev_ok {
            continue;
        }
        let mut j = i + needle.len();
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if chars.get(j) != Some(&'<') {
            return true;
        }
    }
    false
}

#[test]
fn every_kept_import_binds_a_name_the_packet_uses_and_the_generator_does_not_own() {
    let Some((pairs, _)) = collect_pairs() else {
        eprintln!("skipped: rstom worktree packet sources not present");
        return;
    };
    let rstom = common::rstom_tables::rstom_dir();
    let owned = generator_owned();
    for (unit, shape) in &pairs {
        let (hand, siblings) = hand_and_siblings(&rstom, unit);
        let needed = format!("{}{}", strip_use_statements(&hand), siblings);
        for statement in &shape.imports {
            if statement.starts_with("pub ") {
                continue;
            }
            for name in bound_names(statement) {
                assert!(
                    !owned.contains(name.as_str()),
                    "cls={} statement={statement} name={name} is generator-owned",
                    shape.cls
                );
                assert!(
                    has_word(&needed, &name),
                    "cls={} statement={statement} name={name} is never used",
                    shape.cls
                );
            }
        }
    }
}

#[test]
fn a_packet_importing_the_coordinate_vec_has_no_bare_vec_field() {
    let Some((pairs, _)) = collect_pairs() else {
        eprintln!("skipped: rstom worktree packet sources not present");
        return;
    };
    let rstom = common::rstom_tables::rstom_dir();
    for (unit, shape) in &pairs {
        let (hand, _) = hand_and_siblings(&rstom, unit);
        if !has_coord_vec_import(&hand) {
            continue;
        }
        for field in &shape.fields {
            assert!(
                !has_bare_vec(&field.type_),
                "cls={} field={} has bare Vec: {}",
                shape.cls,
                field.name,
                field.type_
            );
        }
    }
}

#[test]
fn only_known_unextractable_packets_are_skipped() {
    let Some((_, skipped)) = collect_pairs() else {
        eprintln!("skipped: rstom worktree packet sources not present");
        return;
    };
    let mut unexpected: Vec<String> = skipped
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|name| !KNOWN_UNEXTRACTABLE.contains(&name.as_str()))
        .collect();
    unexpected.sort();
    assert!(unexpected.is_empty(), "unexpected skips: {unexpected:?}");
}
