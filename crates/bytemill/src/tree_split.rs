//! Splitting a hand packet file when `emit rstom-tree` swaps it for a generated one, and the
//! checks that keep a swap from dropping hand behaviour.
//!
//! A generated packet file holds the struct, `new`, `serializer`, the `Packet` impls and the
//! serializer. Everything else of the hand file has to survive in `<stem>.ext.rs` (items and
//! methods) or `<stem>.tests.rs` (the inline test module), be a private helper that nothing
//! references any more, or the swap is refused. This module only computes texts; the caller
//! writes them once every packet of the run has passed its checks.

use std::collections::BTreeSet;

use crate::errors::{BmlResult, emit};
use crate::shapes::items::{Item, impl_members, scan_items};
use crate::shapes::model::Shape;
use crate::shapes::scan::matching_brace;

/// Traits whose impls the generated file renders itself.
const GENERATED_TRAITS: &[&str] = &[
    "Packet",
    "ClientPacket",
    "ClientPacketGroup",
    "ServerPacket",
    "ServerPacketGroup",
];

/// Substrings that mark hand serializer behaviour beyond the wire layout: limits, panics,
/// conversions and clamps.
const AUDIT_PATTERNS: &[&str] = &[
    "assert!",
    "assert_eq!",
    "assert_ne!",
    "debug_assert",
    "panic!",
    "unreachable!",
    "unimplemented!",
    "todo!",
    ".expect(",
    ".unwrap(",
    ".unwrap_or",
    "try_from",
    "try_into",
    ".list(",
    ".set(",
    ".map_value(",
    ".clamp(",
    ".min(",
    ".max(",
    "MAX_",
];

/// Words that look like a call (`word(`) but are not one.
const NOT_CALLS: &[&str] = &[
    "if", "while", "match", "for", "loop", "return", "fn", "let", "in", "as", "move", "unsafe",
    "else", "break", "continue", "mut", "ref", "dyn", "impl", "where",
];

/// The sibling texts a swap needs and the hand helpers it drops as dead.
#[derive(Debug, Default)]
pub struct Split {
    /// The new `<stem>.ext.rs` text, when the shape names extras.
    pub ext: Option<String>,
    /// The new `<stem>.tests.rs` text, when the shape names tests.
    pub tests: Option<String>,
    /// `<kind> <name>` of each private helper that nothing references after the swap.
    pub dropped: Vec<String>,
    /// The source text of those helpers, with their docs and attributes.
    pub dropped_text: Vec<String>,
}

fn wants(name: &Option<String>) -> bool {
    name.as_deref().is_some_and(|n| !n.is_empty())
}

/// Whether `word` occurs in `text` delimited by non-identifier characters.
pub fn has_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let at = start + pos;
        let end = at + word.len();
        let before = at == 0 || !ident(bytes[at - 1]);
        let after = end == bytes.len() || !ident(bytes[end]);
        if before && after {
            return true;
        }
        start = at + 1;
    }
    false
}

/// `text` without comment and attribute lines: the code a name can be referenced from.
fn code_text(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("//") || t.starts_with("#["))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_inline_tests(item: &Item) -> bool {
    item.cfg_test && item.kind == "mod" && item.name.as_deref() == Some("tests")
}

/// The body of the hand file's inline `#[cfg(test)] mod tests { ... }`.
fn inline_tests_body(items: &[Item]) -> BmlResult<Option<String>> {
    let mut found = items.iter().filter(|i| is_inline_tests(i));
    let first = found.next();
    if found.next().is_some() {
        return Err(emit("more than one inline #[cfg(test)] mod tests"));
    }
    let Some(item) = first else {
        return Ok(None);
    };
    let head = item
        .text
        .find("mod tests")
        .ok_or_else(|| emit("inline tests module has no name"))?;
    let open = item.text[head..]
        .find('{')
        .map(|p| head + p)
        .ok_or_else(|| emit("inline tests module has no body"))?;
    let close = matching_brace(&item.text, open);
    Ok(Some(item.text[open + 1..close].to_string()))
}

/// The names a `use` statement brings into scope.
fn use_leaves(use_text: &str) -> Vec<String> {
    let body = use_text
        .trim()
        .trim_start_matches("pub ")
        .trim_start_matches("use ")
        .trim_end_matches(';');
    let chars: Vec<char> = body.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut leaves = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_ident(chars[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_ident(chars[i]) {
            i += 1;
        }
        let ident: String = chars[start..i].iter().collect();
        let mut j = i;
        while j < chars.len() && chars[j] == ' ' {
            j += 1;
        }
        // A leaf ends its path: `,` or `}` or the end of the statement follows. `x as y` makes
        // `y` the leaf, `x` is followed by `as`.
        let ends = j >= chars.len() || matches!(chars[j], ',' | '}');
        if ends && ident != "self" && ident != "as" {
            leaves.push(ident);
        }
    }
    leaves
}

/// Names the generated parent file defines or imports: what an ext child reaches through
/// `use super::*;`.
fn parent_names(shape: &Shape, generated: &str) -> Vec<String> {
    let mut names = vec![shape.cls.clone()];
    if shape.serializer.style == "struct" {
        names.push(shape.serializer.name.clone());
    }
    for line in generated.lines() {
        if line.starts_with("use ") || line.starts_with("pub use ") {
            names.extend(use_leaves(line));
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Whether the first code line of an item starts with `pub`.
fn is_pub(item_text: &str) -> bool {
    item_text
        .lines()
        .map(str::trim_start)
        .find(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with("#["))
        .is_some_and(|l| l.starts_with("pub"))
}

struct Part {
    kind: String,
    name: Option<String>,
    private: bool,
    text: String,
    /// The text as the hand file had it, before moved code is renamed.
    orig: String,
}

impl Part {
    fn of(item: &Item) -> Part {
        Part {
            kind: item.kind.clone(),
            name: item.name.clone(),
            private: !is_pub(&item.text),
            text: item.text.clone(),
            orig: item.text.clone(),
        }
    }

    fn key(&self) -> Option<String> {
        self.name.as_ref().map(|n| format!("{} {n}", self.kind))
    }
}

/// `use_text` reduced to the leaves `code` references and `skip` does not already provide, with
/// a relative `super::` path moved one module down; `None` when nothing is left.
fn filtered_use(use_text: &str, code: &str, skip: &[String], deeper: bool) -> Option<String> {
    use crate::shapes::extract::{leaf_binding, rebuild_use, split_use, use_statement};
    let statement = use_statement(use_text).ok()?;
    // Re-exports belong to the generated parent, which carries the shape's imports.
    if statement.starts_with("pub") {
        return None;
    }
    let (head, leaves) = split_use(&statement).ok()?;
    let survivors: Vec<String> = leaves
        .into_iter()
        .filter(|leaf| {
            let (path, bound) = leaf_binding(&head, leaf);
            if skip.contains(&bound)
                || crate::shapes::extract::COORD_VEC_PATHS.contains(&path.as_str())
            {
                return false;
            }
            // A trait is used by its methods, not by name.
            let by_method = TRAIT_METHODS
                .iter()
                .any(|(t, methods)| *t == bound && methods.iter().any(|m| code.contains(m)));
            bound == "_" || has_bare_word(code, &bound) || by_method
        })
        .collect();
    let head = if deeper && head.starts_with("use super::") {
        head.replacen("use super::", "use super::super::", 1)
    } else {
        head
    };
    rebuild_use(&head, &survivors)
}

/// Whether `word` occurs in `text` as a name, not as the tail of a path (`a::word`).
fn has_bare_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(pos) = text[from..].find(word) {
        let at = from + pos;
        let end = at + word.len();
        let before = text[..at].chars().next_back();
        let after = text[end..].chars().next();
        let bare = !before.is_some_and(|c| ident(c) || c == ':') && !after.is_some_and(ident);
        if bare {
            return true;
        }
        from = at + 1;
    }
    false
}

/// `text` with the coordinate vector renamed `CoordVec` in code; comment lines stay as written.
fn normalise_code(text: &str, coord: &std::collections::BTreeSet<String>) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.trim_start().starts_with("//") {
                line.to_string()
            } else {
                crate::shapes::extract::normalise_types(line, coord)
            }
        })
        .collect()
}

/// `code` with every relative `super::` path one module further up.
fn deepen_super(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    let mut rest = code;
    while let Some(at) = rest.find("super::") {
        let before = rest[..at].chars().next_back();
        let boundary = !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':');
        out.push_str(&rest[..at]);
        if boundary && !rest[at..].starts_with("super::super::") {
            out.push_str("super::super::");
        } else {
            out.push_str("super::");
        }
        rest = &rest[at + "super::".len()..];
    }
    out.push_str(rest);
    out
}

/// Traits whose import code needs for their methods alone.
const TRAIT_METHODS: &[(&str, &[&str])] = &[
    ("JavaEnum", &[".ordinal("]),
    (
        "Point",
        &[
            ".x()",
            ".y()",
            ".z()",
            ".block_x()",
            ".block_y()",
            ".block_z()",
        ],
    ),
    ("Type", &[".write(", ".read(", ".list(", ".optional("]),
    (
        "Registry",
        &[
            ".size(",
            ".get_key(",
            ".get(",
            ".keys()",
            ".iter()",
            ".contains(",
        ],
    ),
];

const COORD_USE: &str = "use crate::coordinate::Vec as CoordVec;";

/// Whether the impl `text` is `impl Type { ... }`, not `impl Trait for Type { ... }`.
fn is_inherent(text: &str) -> bool {
    text.lines()
        .map(str::trim_start)
        .find(|l| l.starts_with("impl"))
        .is_some_and(|head| {
            let head = head.split('{').next().unwrap_or(head);
            !head.contains(" for ")
        })
}

/// An inherent impl with each private method that `refs` mentions made `pub(super)`.
fn promote_members(impl_text: &str, refs: &str) -> String {
    let mut out = String::with_capacity(impl_text.len() + 32);
    for line in impl_text.split_inclusive('\n') {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        let mut rest = t;
        let mut quals = String::new();
        for q in ["const ", "unsafe ", "async "] {
            if let Some(r) = rest.strip_prefix(q) {
                quals.push_str(q);
                rest = r;
            }
        }
        let name = rest.strip_prefix("fn ").map(|r| {
            r.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        });
        match name {
            Some(n) if indent.len() == 4 && !n.is_empty() && has_word(refs, &n) => {
                out.push_str(indent);
                out.push_str("pub(super) ");
                out.push_str(&quals);
                out.push_str(rest);
            }
            _ => out.push_str(line),
        }
    }
    out
}

/// `item_text` with `pub(super)` before the item itself, after its docs and attributes.
fn promote(item_text: &str) -> String {
    let mut out = String::with_capacity(item_text.len() + 12);
    let mut done = false;
    for line in item_text.split_inclusive('\n') {
        let t = line.trim_start();
        if !done && !t.trim().is_empty() && !t.starts_with("//") && !t.starts_with("#[") {
            let indent = &line[..line.len() - t.len()];
            out.push_str(indent);
            out.push_str("pub(super) ");
            out.push_str(t);
            done = true;
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Compute the sibling texts for swapping `hand` for `generated` (the raw generated packet
/// text).
pub fn split_hand(shape: &Shape, hand: &str, generated: &str) -> BmlResult<Split> {
    let items = scan_items(hand)?;
    let hand_uses: Vec<String> = items
        .iter()
        .filter(|it| it.kind == "use")
        .map(|it| it.text.clone())
        .collect();
    // The generated packet names the coordinate vector `CoordVec` and keeps `Vec` for the
    // standard one, so moved code is read the same way.
    let coord = crate::shapes::extract::coordinate_names(hand_uses.iter().map(String::as_str))?;
    let parent = parent_names(shape, generated);
    let tests = if wants(&shape.tests) {
        let body = inline_tests_body(&items)?.ok_or_else(|| {
            emit(format!(
                "{}: the shape names a tests sibling but the hand file has no inline tests",
                shape.module
            ))
        })?;
        let body = body.trim_start().to_string();
        let body = if coord.is_empty() {
            body
        } else {
            normalise_code(&body, &coord)
        };
        // The inline tests reached the hand file's imports through `use super::*`; the
        // generated packet imports less, so the sibling imports what its code uses.
        let code = code_text(&body);
        // Names the tests import themselves need no second import.
        let mut skip = parent.clone();
        for line in body
            .lines()
            .map(str::trim_start)
            .filter(|l| l.starts_with("use "))
        {
            skip.extend(use_leaves(line));
        }
        let mut prelude: Vec<String> = hand_uses
            .iter()
            .filter_map(|u| filtered_use(u, &code, &skip, true))
            .collect();
        if has_word(&code, "CoordVec")
            && !parent.iter().any(|n| n == "CoordVec")
            && !body.contains(COORD_USE)
        {
            prelude.push(COORD_USE.to_string());
        }
        prelude.sort();
        prelude.dedup();
        if prelude.is_empty() {
            Some(body)
        } else {
            Some(format!("{}\n\n{body}", prelude.join("\n")))
        }
    } else {
        None
    };
    if !wants(&shape.extras) {
        return Ok(Split {
            ext: None,
            tests,
            ..Split::default()
        });
    }

    let mut parts: Vec<Part> = Vec::new();
    for item in &items {
        if is_inline_tests(item) {
            continue;
        }
        let name = item.name.as_deref().unwrap_or("");
        match item.kind.as_str() {
            "use" => {}
            "struct" if name == shape.cls => {}
            "struct" if shape.serializer.style == "struct" && name == shape.serializer.name => {}
            "impl" => match item.trait_.as_deref() {
                Some(t) if name == shape.cls && GENERATED_TRAITS.contains(&t) => {}
                Some("PacketSerializer") if name == shape.serializer.name => {}
                None if name == shape.cls => {
                    let moved: Vec<String> = impl_members(item)?
                        .into_iter()
                        .filter(|m| !matches!(m.name.as_deref(), Some("new" | "serializer")))
                        .map(|m| m.text)
                        .collect();
                    if !moved.is_empty() {
                        let text = format!("impl {} {{\n{}\n}}", shape.cls, moved.join("\n\n"));
                        parts.push(Part {
                            kind: "impl".into(),
                            name: None,
                            private: false,
                            orig: text.clone(),
                            text,
                        });
                    }
                }
                _ => parts.push(Part::of(item)),
            },
            _ => parts.push(Part::of(item)),
        }
    }

    if !coord.is_empty() {
        for part in &mut parts {
            part.text = normalise_code(&part.text, &coord);
        }
    }

    // Private helpers nothing references any more (the generated serializer replaced their only
    // caller) would be dead code; drop them and say so.
    let generated_code = code_text(generated);
    let tests_code = tests.as_deref().map(code_text).unwrap_or_default();
    let hook_code: String = shape
        .hooks
        .iter()
        .flat_map(|(_, lines)| lines.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    let mut alive = vec![true; parts.len()];
    loop {
        let mut changed = false;
        for i in 0..parts.len() {
            if !alive[i] || !parts[i].private {
                continue;
            }
            if !matches!(
                parts[i].kind.as_str(),
                "fn" | "const" | "static" | "struct" | "enum" | "type"
            ) {
                continue;
            }
            let Some(name) = parts[i].name.clone() else {
                continue;
            };
            let referenced = has_word(&generated_code, &name)
                || has_word(&tests_code, &name)
                || has_word(&hook_code, &name)
                || parts
                    .iter()
                    .enumerate()
                    .any(|(j, p)| j != i && alive[j] && has_word(&code_text(&p.text), &name));
            if !referenced {
                alive[i] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let dropped: Vec<String> = parts
        .iter()
        .zip(&alive)
        .filter(|(_, live)| !**live)
        .filter_map(|(p, _)| p.key())
        .collect();
    let dropped_text: Vec<String> = parts
        .iter()
        .zip(&alive)
        .filter(|(_, live)| !**live)
        .map(|(p, _)| p.orig.clone())
        .collect();
    let kept: Vec<&Part> = parts
        .iter()
        .zip(&alive)
        .filter(|(_, live)| **live)
        .map(|(p, _)| p)
        .collect();

    // A private helper the generated parent now calls is reached from outside its module.
    let reached_text = format!("{generated_code}\n{tests_code}");
    let body = kept
        .iter()
        .map(|p| {
            let reached = p.private
                && matches!(
                    p.kind.as_str(),
                    "fn" | "const" | "static" | "struct" | "enum" | "type"
                )
                && p.name
                    .as_deref()
                    .is_some_and(|n| has_word(&generated_code, n) || has_word(&tests_code, n));
            if reached {
                promote(&p.text)
            } else if p.kind == "impl" && is_inherent(&p.text) {
                promote_members(&p.text, &reached_text)
            } else {
                p.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let body = deepen_super(&body);
    let code_body = code_text(&body);
    let needs_super = parent.iter().any(|n| has_word(&code_body, n));
    let mut text = format!("//! Hand-written helpers for {}.\n", shape.cls);
    if !dropped.is_empty() {
        text.push_str(&format!(
            "//!\n//! Dropped with the swap, nothing references them any more: {}.\n",
            dropped
                .iter()
                .map(|d| d.split_once(' ').map_or(d.as_str(), |(_, n)| n))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    text.push('\n');
    if needs_super {
        text.push_str("use super::*;\n");
    }
    let mut wrote_use = needs_super;
    let mut ext_uses: Vec<String> = hand_uses
        .iter()
        .filter_map(|u| filtered_use(u, &code_body, &parent, true))
        .collect();
    if has_word(&code_body, "CoordVec")
        && !parent.iter().any(|n| n == "CoordVec")
        && !body.contains(COORD_USE)
    {
        ext_uses.push(COORD_USE.to_string());
    }
    ext_uses.sort();
    ext_uses.dedup();
    for use_text in &ext_uses {
        text.push_str(use_text);
        text.push('\n');
        wrote_use = true;
    }
    if wrote_use {
        text.push('\n');
    }
    text.push_str(&body);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(Split {
        ext: Some(text),
        tests,
        dropped,
        dropped_text,
    })
}

/// The identity of every top-level item of `text`, and of every member of its impls.
fn item_keys(text: &str) -> BmlResult<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    for item in scan_items(text)? {
        match item.kind.as_str() {
            "use" => {}
            "impl" => {
                let target = item.name.clone().unwrap_or_default();
                let head = match &item.trait_ {
                    Some(t) => {
                        keys.insert(format!("impl {t} for {target}"));
                        format!("impl {t} for {target}")
                    }
                    None => format!("impl {target}"),
                };
                for m in impl_members(&item)? {
                    let name = m.name.clone().unwrap_or_else(|| m.text.trim().to_string());
                    keys.insert(format!("{head}::{} {name}", m.kind));
                }
            }
            kind => {
                let name = item
                    .name
                    .clone()
                    .unwrap_or_else(|| item.text.trim().to_string());
                keys.insert(format!("{kind} {name}"));
            }
        }
    }
    Ok(keys)
}

/// The hand items the generated file renders itself.
fn regenerated_keys(shape: &Shape) -> BTreeSet<String> {
    let cls = &shape.cls;
    let mut keys: BTreeSet<String> = BTreeSet::new();
    keys.insert(format!("struct {cls}"));
    keys.insert(format!("impl {cls}::fn new"));
    keys.insert(format!("impl {cls}::fn serializer"));
    keys.insert(format!("impl Packet for {cls}"));
    keys.insert(format!("impl Packet for {cls}::const CLASS_NAME"));
    for t in GENERATED_TRAITS.iter().filter(|t| **t != "Packet") {
        keys.insert(format!("impl {t} for {cls}"));
    }
    if shape.serializer.style == "struct" {
        let ser = &shape.serializer.name;
        keys.insert(format!("struct {ser}"));
        keys.insert(format!("impl PacketSerializer for {ser}"));
        keys.insert(format!("impl PacketSerializer for {ser}::fn write"));
        keys.insert(format!("impl PacketSerializer for {ser}::fn read"));
    }
    if wants(&shape.tests) {
        keys.insert("mod tests".to_string());
    }
    keys
}

/// Hand items that are in neither the generated file, the ext sibling, nor the dropped
/// helpers. A swap with any is refused.
pub fn conservation_gaps(
    shape: &Shape,
    hand: &str,
    generated: &str,
    ext: &str,
    dropped: &[String],
) -> BmlResult<Vec<String>> {
    let hand_keys = item_keys(hand)?;
    let mut have = item_keys(generated)?;
    if !ext.is_empty() {
        have.extend(item_keys(ext)?);
    }
    let regenerated = regenerated_keys(shape);
    Ok(hand_keys
        .into_iter()
        .filter(|k| !regenerated.contains(k) && !have.contains(k) && !dropped.contains(k))
        .collect())
}

/// The non-wire constructs of the hand `write` and `read`: asserts, limits, conversions, clamps
/// and calls to helper functions the generated wire code does not know about.
pub fn serializer_constructs(shape: &Shape, hand: &str) -> BmlResult<Vec<String>> {
    if shape.serializer.style != "struct" {
        return Ok(Vec::new());
    }
    let mut found: BTreeSet<String> = BTreeSet::new();
    for item in scan_items(hand)? {
        if item.kind != "impl"
            || item.trait_.as_deref() != Some("PacketSerializer")
            || item.name.as_deref() != Some(shape.serializer.name.as_str())
        {
            continue;
        }
        for member in impl_members(&item)? {
            if !matches!(member.name.as_deref(), Some("write" | "read")) {
                continue;
            }
            let code = code_text(&member.text);
            for pattern in AUDIT_PATTERNS {
                if code.contains(pattern) {
                    found.insert((*pattern).to_string());
                }
            }
            found.extend(helper_calls(&code));
        }
    }
    Ok(found.into_iter().collect())
}

/// Free function calls (`name(` with no receiver or path) in `code`.
fn helper_calls(code: &str) -> Vec<String> {
    let b = code.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut calls = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !(b[i].is_ascii_lowercase() || b[i] == b'_') || (i > 0 && ident(b[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && ident(b[i]) {
            i += 1;
        }
        let word = &code[start..i];
        let before = code[..start].trim_end();
        let declares = before
            .strip_suffix("fn")
            .is_some_and(|rest| !rest.as_bytes().last().is_some_and(|c| ident(*c)));
        // `a.f(`, `A::f(` and `fn f(` are not free calls; `field: f(` is one.
        let free = !before.ends_with('.') && !before.ends_with("::") && !declares;
        if free && b.get(i) == Some(&b'(') && !NOT_CALLS.contains(&word) {
            calls.push(word.to_string());
        }
    }
    calls
}

/// The serializer constructs neither reproduced by a hook, nor by the generated wire code
/// (`generated`, the bodies of a bound packet, whose conversions carry some), nor recorded under
/// `accepted`.
pub fn uncovered_constructs(shape: &Shape, hand: &str, generated: &str) -> BmlResult<Vec<String>> {
    let hook_text: String = shape
        .hooks
        .iter()
        .flat_map(|(_, lines)| lines.iter().cloned())
        .chain(std::iter::once(generated.to_string()))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(serializer_constructs(shape, hand)?
        .into_iter()
        .filter(|c| {
            !shape.accepted.iter().any(|a| a.construct == *c) && !hook_text.contains(c.as_str())
        })
        .collect())
}
