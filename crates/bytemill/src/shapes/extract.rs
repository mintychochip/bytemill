//! Build a `Shape` from a hand-written rstom packet file.
//!
//! Port of `bps/shapes/extract.py`. The Python regular expressions are matched
//! by hand (no regex crate); each helper names the pattern it replaces.
//! Offsets are byte offsets (see `scan.rs` for the non-ASCII caveat).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::errors::BmlResult;
use crate::shapes::items::{Item, impl_body_bounds, scan_items};
use crate::shapes::model::{FieldShape, NewShape, SerializerShape, Shape, shape_error};
use crate::shapes::scan::{
    advance_item_start, at, collect_attrs_before, docs_and_attrs_before, find_fn_body_start,
    find_from, leading_module_docs, matching_brace, parse_struct_attrs, read_ident, read_type,
    skip_balanced, skip_item, skip_trivia, sub, sw,
};

/// `crates/rstom/src/network/packet`
pub const PACKET_ROOT: &str = "crates/rstom/src/network/packet";

const ALLOWED_TRAITS_FOR_CLS: &[&str] = &[
    "Packet",
    "ClientPacket",
    "ServerPacket",
    "ClientPacketGroup",
    "ServerPacketGroup",
];

const GENERATOR_NAMES: &[&str] = &[
    "Arc",
    "ClientPacket",
    "ClientPacketGroup",
    "ConstantSerializer",
    "CoordVec",
    "NetworkBuffer",
    "Packet",
    "PacketSerializer",
    "PacketValue",
    "ServerPacket",
    "ServerPacketGroup",
    "Uuid",
    "downcast",
    "fixed_raw_bytes",
    "BYTE",
    "UNSIGNED_SHORT",
    "SHORT",
    "UNSIGNED_INT",
    "INT",
    "LONG",
    "FLOAT",
    "DOUBLE",
    "VAR_INT",
    "VAR_LONG",
    "BOOLEAN",
    "BLOCK_POSITION",
    "UUID",
    "STRING",
    "RAW_BYTES",
    "FIXED_BYTES",
];

const COORD_VEC_PATHS: &[&str] = &["crate::coordinate::Vec", "crate::coordinate::vec::Vec"];
const GENERATED_TAIL_USE: &str = "pub use ext::*;";

/// Python `\w`.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `\s*` at `i`: offset after the whitespace run (possibly `i`).
fn skip_ws(s: &str, i: usize) -> usize {
    match s.get(i..) {
        Some(rest) => {
            i + rest
                .chars()
                .take_while(|c| c.is_whitespace())
                .map(char::len_utf8)
                .sum::<usize>()
        }
        None => i,
    }
}

/// `\s+` at `i`: offset after the whitespace run, or `None` when empty.
fn ws_end(s: &str, i: usize) -> Option<usize> {
    let e = skip_ws(s, i);
    if e > i { Some(e) } else { None }
}

/// `\w+` at `i`: offset after the word run, or `None` when empty.
fn word_end(s: &str, i: usize) -> Option<usize> {
    let rest = s.get(i..)?;
    let n: usize = rest
        .chars()
        .take_while(|c| is_word(*c))
        .map(char::len_utf8)
        .sum();
    if n == 0 { None } else { Some(i + n) }
}

/// `kw\s+` at `i`: offset after the whitespace following the keyword.
fn kw_ws(s: &str, i: usize, kw: &str) -> Option<usize> {
    if sw(s, i, kw) {
        ws_end(s, i + kw.len())
    } else {
        None
    }
}

/// `(?:pub(?:\([^)]*\))?\s+)?` at `p`: offset after the optional visibility, else `p`.
fn skip_pub(s: &str, p: usize) -> usize {
    if !sw(s, p, "pub") {
        return p;
    }
    let j = p + 3;
    let after_group = if sw(s, j, "(") {
        find_from(s, j + 1, ")").map(|k| k + 1)
    } else {
        None
    };
    after_group
        .and_then(|g| ws_end(s, g))
        .or_else(|| ws_end(s, j))
        .unwrap_or(p)
}

/// Offsets where `^` matches in multiline mode.
fn line_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1))
}

/// `_STRUCT_HEAD` match: line start, struct name end.
struct Head {
    start: usize,
    end: usize,
}

/// `_STRUCT_HEAD.search(text)` = `^(?:pub(?:\([^)]*\))?\s+)?struct\s+(\w+)` (multiline).
fn struct_head(text: &str) -> Option<Head> {
    line_starts(text).find_map(|p| {
        let k = skip_pub(text, p);
        let j = kw_ws(text, k, "struct")?;
        let end = word_end(text, j)?;
        Some(Head { start: p, end })
    })
}

/// `_USE_HEAD.search(text)` = `^(?:pub(?:\([^)]*\))?\s+)?use\s` (multiline); returns line start.
fn use_head(text: &str) -> Option<usize> {
    line_starts(text).find(|&p| {
        let k = skip_pub(text, p);
        sw(text, k, "use")
            && text[k + 3..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
    })
}

/// `_use_statement`.
fn use_statement(text: &str) -> BmlResult<String> {
    let start = use_head(text).ok_or_else(|| shape_error("malformed use"))?;
    let joined = sub(text, start, text.len())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Ok(joined
        .replace("{ ", "{")
        .replace(" }", "}")
        .replace(",}", "}"))
}

/// `_split_use`: (head, leaves).
fn split_use(statement: &str) -> BmlResult<(String, Vec<String>)> {
    let body = statement.strip_suffix(';').unwrap_or(statement);
    if let Some((head, inner)) = body.split_once('{') {
        let inner = inner.strip_suffix('}').unwrap_or(inner);
        let leaves = inner
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
        return Ok((head.to_string(), leaves));
    }
    if let Some((head, leaf)) = body.rsplit_once("::") {
        return Ok((format!("{head}::"), vec![leaf.to_string()]));
    }
    match body.rsplit_once(' ') {
        Some((head, leaf)) => Ok((format!("{head} "), vec![leaf.to_string()])),
        None => Err(shape_error("malformed use")),
    }
}

/// `_leaf_binding`: (full path, bound name).
fn leaf_binding(head: &str, leaf: &str) -> (String, String) {
    let p = skip_pub(head, 0);
    let raw = if sw(head, p, "use") {
        match ws_end(head, p + 3) {
            Some(q) => sub(head, q, head.len()),
            None => head,
        }
    } else {
        head
    };
    let module = raw.strip_suffix("::").unwrap_or(raw).trim();
    let (name, alias) = match leaf.split_once(" as ") {
        Some((n, a)) => (n, a),
        None => (leaf, ""),
    };
    let name = name.trim();
    if name == "self" {
        let bound = module.rsplit("::").next().unwrap_or("");
        return (module.to_string(), bound.to_string());
    }
    let path = if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}::{name}")
    };
    let alias = alias.trim();
    let bound = if alias.is_empty() { name } else { alias };
    (path, bound.to_string())
}

/// `_coordinate_names`: names the coordinate `Vec` is bound to in this file.
fn coordinate_names<'a>(use_texts: impl Iterator<Item = &'a str>) -> BmlResult<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for text in use_texts {
        let (head, leaves) = split_use(&use_statement(text)?)?;
        for leaf in &leaves {
            let (path, bound) = leaf_binding(&head, leaf);
            if COORD_VEC_PATHS.contains(&path.as_str()) {
                names.insert(bound);
            }
        }
    }
    Ok(names)
}

/// Whether `coord` matches at byte `i` of `text` (`(?<![\w:])(?:alts)(?!\w|\s*<)`).
fn coord_match_at(text: &str, i: usize, coord: &BTreeSet<String>) -> Option<usize> {
    if let Some(prev) = text[..i].chars().next_back()
        && (is_word(prev) || prev == ':')
    {
        return None;
    }
    for name in coord {
        if !sw(text, i, name) {
            continue;
        }
        let e = i + name.len();
        if let Some(c) = text[e..].chars().next()
            && is_word(c)
        {
            continue;
        }
        let after_ws = skip_ws(text, e);
        if at(text, after_ws) == Some(b'<') {
            continue;
        }
        return Some(e);
    }
    None
}

/// `_normalise_types`: coordinate `Vec` aliases become `CoordVec`; `std::vec::Vec` becomes `Vec`.
fn normalise_types(text: &str, coord: &BTreeSet<String>) -> String {
    let mut out = String::with_capacity(text.len());
    if coord.is_empty() {
        out.push_str(text);
    } else {
        let mut i = 0;
        while i < text.len() {
            if let Some(end) = coord_match_at(text, i, coord) {
                out.push_str("CoordVec");
                i = end;
                continue;
            }
            let ch = text[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8().max(1);
        }
    }
    out.replace("std::vec::Vec", "Vec")
}

/// `_has_word(needed, bound)`: `re.search(rf"\b{bound}\b", needed)`.
fn has_word(needed: &str, bound: &str) -> bool {
    let boundary = |before: Option<char>, after: Option<char>| {
        before.is_some_and(is_word) != after.is_some_and(is_word)
    };
    if bound.is_empty() {
        let mut positions: Vec<usize> = needed.char_indices().map(|(i, _)| i).collect();
        positions.push(needed.len());
        return positions
            .into_iter()
            .any(|p| boundary(needed[..p].chars().next_back(), needed[p..].chars().next()));
    }
    let first = bound.chars().next();
    let last = bound.chars().next_back();
    let mut from = 0;
    while let Some(i) = find_from(needed, from, bound) {
        let before = needed[..i].chars().next_back();
        let after = needed[i + bound.len()..].chars().next();
        if boundary(before, first) && boundary(last, after) {
            return true;
        }
        from = i + 1;
    }
    false
}

/// `_rebuild_use`.
fn rebuild_use(head: &str, leaves: &[String]) -> Option<String> {
    match leaves {
        [] => None,
        [one] if one == "self" => Some(format!("{};", head.strip_suffix("::").unwrap_or(head))),
        [one] => Some(format!("{head}{one};")),
        many => Some(format!("{head}{{{}}};", many.join(", "))),
    }
}

/// `_hand_imports`.
fn hand_imports(use_texts: &[&str], needed: &str) -> BmlResult<Vec<String>> {
    let mut kept = Vec::new();
    for text in use_texts {
        let statement = use_statement(text)?;
        if statement == GENERATED_TAIL_USE {
            continue;
        }
        if statement.starts_with("pub") {
            kept.push(statement);
            continue;
        }
        let (head, leaves) = split_use(&statement)?;
        let mut survivors = Vec::new();
        for leaf in &leaves {
            let (path, bound) = leaf_binding(&head, leaf);
            if leaf == "*"
                || COORD_VEC_PATHS.contains(&path.as_str())
                || GENERATOR_NAMES.contains(&bound.as_str())
            {
                continue;
            }
            if bound == "_" || has_word(needed, &bound) {
                survivors.push(leaf.clone());
            }
        }
        if let Some(rebuilt) = rebuild_use(&head, &survivors) {
            kept.push(rebuilt);
        }
    }
    Ok(kept)
}

/// `_sibling_exists`.
fn sibling_exists(directory: Option<&Path>, name: &str) -> bool {
    directory.is_some_and(|d| d.join(name).is_file())
}

/// `_sibling_text`: contents of `<stem>.*.rs` siblings, sorted by path, joined by newlines.
fn sibling_text(directory: Option<&Path>, stem: &str) -> BmlResult<String> {
    let Some(dir) = directory else {
        return Ok(String::new());
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{stem}.");
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    n.starts_with(&prefix) && n.ends_with(".rs") && n.len() >= prefix.len() + 3
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    let mut texts = Vec::with_capacity(files.len());
    for f in &files {
        texts.push(crate::text::read_text(f).map_err(|e| shape_error(e.to_string()))?);
    }
    Ok(texts.join("\n"))
}

/// `_parse_struct_fields`.
fn parse_struct_fields(body: &str) -> BmlResult<Vec<FieldShape>> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < body.len() {
        pos = skip_trivia(body, pos);
        if pos >= body.len() || at(body, pos) == Some(b'}') {
            break;
        }
        let item = advance_item_start(body, pos)?;
        let (docs, _) = docs_and_attrs_before(body, item)?;
        pos = item;
        if !sw(body, pos, "pub ") {
            return Err(shape_error("non-public struct field"));
        }
        pos += 4;
        let (name, p) = read_ident(body, pos)?;
        pos = skip_trivia(body, p);
        if at(body, pos) != Some(b':') {
            return Err(shape_error(format!("malformed field {name}")));
        }
        pos += 1;
        let (typ, p) = read_type(body, pos);
        pos = skip_trivia(body, p);
        if at(body, pos) == Some(b',') {
            pos += 1;
        }
        fields.push(FieldShape {
            name,
            type_: typ,
            docs,
            wire: None,
        });
    }
    Ok(fields)
}

/// What `packet_struct` reads off a packet struct: (docs, derives, form, fields).
type PacketStruct = (Vec<String>, Vec<String>, String, Vec<FieldShape>);

/// `_packet_struct`.
fn packet_struct(text: &str, cls: &str) -> BmlResult<PacketStruct> {
    let head = struct_head(text).ok_or_else(|| shape_error(format!("malformed struct {cls}")))?;
    let (docs, attr_pos) = docs_and_attrs_before(text, head.start)?;
    let derives = parse_struct_attrs(text, attr_pos, head.start)?;
    let pos = skip_trivia(text, head.end);
    match at(text, pos) {
        Some(b';') => Ok((docs, derives, "unit".to_string(), Vec::new())),
        Some(b'{') => {
            let close = matching_brace(text, pos);
            let fields = parse_struct_fields(sub(text, pos + 1, close))?;
            Ok((docs, derives, "braced".to_string(), fields))
        }
        _ => Err(shape_error(format!("malformed struct {cls}"))),
    }
}

/// Whether `Box::new(<cls>::new(` occurs in `text` (`Box::new\(\s*{cls}::new\s*\(`).
fn box_new_calls(text: &str, cls: &str) -> bool {
    let mut from = 0;
    while let Some(i) = find_from(text, from, "Box::new(") {
        let j = skip_ws(text, i + "Box::new(".len());
        if sw(text, j, cls) {
            let k = j + cls.len();
            if sw(text, k, "::new") {
                let m = skip_ws(text, k + "::new".len());
                if at(text, m) == Some(b'(') {
                    return true;
                }
            }
        }
        from = i + 1;
    }
    false
}

/// `_serializer_read_via_new`.
fn serializer_read_via_new(impl_body: &str, cls: &str) -> BmlResult<bool> {
    let len = impl_body.len();
    let mut pos = 0;
    while pos < len {
        pos = skip_trivia(impl_body, pos);
        if pos >= len {
            break;
        }
        pos = advance_item_start(impl_body, pos)?;
        if !sw(impl_body, pos, "fn ") {
            pos = skip_item(impl_body, pos)?;
            continue;
        }
        pos += 3;
        let (name, p) = read_ident(impl_body, pos)?;
        pos = p;
        let sig_end = find_fn_body_start(impl_body, pos);
        let body_end = if sig_end > 0 {
            matching_brace(impl_body, sig_end - 1)
        } else {
            len
        };
        if name != "read" {
            pos = if body_end < len { body_end + 1 } else { len };
            continue;
        }
        let read_body = if sig_end > 0 {
            sub(impl_body, sig_end - 1, body_end + 1)
        } else {
            ""
        };
        return Ok(box_new_calls(read_body, cls));
    }
    Ok(false)
}

/// First `prefix` occurrence followed by `\s*IDENT\s*\)`; returns IDENT.
fn first_call(text: &str, prefix: &str) -> Option<String> {
    let mut from = 0;
    while let Some(i) = find_from(text, from, prefix) {
        let j = skip_ws(text, i + prefix.len());
        let ident_ok = at(text, j).is_some_and(|c| c.is_ascii_alphabetic() || c == b'_');
        if ident_ok {
            let mut e = j;
            while at(text, e).is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
                e += 1;
            }
            let k = skip_ws(text, e);
            if at(text, k) == Some(b')') {
                return Some(sub(text, j, e).to_string());
            }
        }
        from = i + 1;
    }
    None
}

/// `_parse_serializer_body`: (style, name).
fn parse_serializer_body(body: &str) -> BmlResult<(String, String)> {
    if body.contains("ConstantSerializer::new")
        && let Some(name) = first_call(body, "ConstantSerializer::new(")
    {
        return Ok(("constant".to_string(), name));
    }
    if let Some(name) = first_call(body, "Arc::new(")
        && name != "ConstantSerializer"
    {
        return Ok(("struct".to_string(), name));
    }
    Err(shape_error("unrecognized serializer() body"))
}

/// `_raw_params`: the `fn new(...)` parameter list, whitespace collapsed, no trailing comma.
fn raw_params(body: &str, after_name: usize) -> BmlResult<String> {
    let open =
        find_from(body, after_name, "(").ok_or_else(|| shape_error("substring not found"))?;
    let close = skip_balanced(body, open, b'(', b')').saturating_sub(1);
    let inner = sub(body, open + 1, close);
    let joined = inner.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(joined.trim_end_matches(',').trim_end().to_string())
}

/// `_is_trivial_new_body`.
fn is_trivial_new_body(inner: &str, field_names: &[String]) -> bool {
    let stripped: Vec<&str> = inner
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect();
    let compact = stripped
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if compact == "Self" {
        return field_names.is_empty();
    }
    if !(compact.starts_with("Self {") && compact.ends_with('}')) {
        return false;
    }
    let inner_part = sub(&compact, 6, compact.len() - 1).trim();
    if inner_part.is_empty() {
        return field_names.is_empty();
    }
    let parts: Vec<&str> = inner_part
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != field_names.len() {
        return false;
    }
    parts
        .iter()
        .zip(field_names)
        .all(|(part, name)| *part == name.as_str() || *part == format!("{name}: {name}"))
}

/// `_verbatim_new_body_lines`.
fn verbatim_new_body_lines(inner: &str) -> Vec<String> {
    let physical: Vec<&str> = inner.lines().filter(|l| !l.trim().is_empty()).collect();
    let Some(min_indent) = physical
        .iter()
        .map(|l| l.len() - l.trim_start().len())
        .min()
    else {
        return Vec::new();
    };
    physical
        .iter()
        .map(|l| format!("        {}", sub(l, min_indent, l.len()).trim_end()))
        .collect()
}

/// `_new_body_lines`.
fn new_body_lines(fn_body: &str, field_names: &[String]) -> Vec<String> {
    let fb = fn_body.trim_start();
    if !fb.starts_with('{') {
        return Vec::new();
    }
    let end = matching_brace(fb, 0);
    let inner = sub(fb, 1, end);
    if is_trivial_new_body(inner, field_names) {
        return Vec::new();
    }
    verbatim_new_body_lines(inner)
}

#[derive(Default)]
struct InherentImpl {
    new: NewShape,
    ser_docs: Vec<String>,
    ser_must_use: bool,
    style: String,
    ser_name: String,
    extra: bool,
}

fn is_must_use(attr: &str) -> bool {
    attr.trim().starts_with("#[must_use")
}

/// `_parse_inherent_impl`.
fn parse_inherent_impl(body: &str, field_names: &[String]) -> BmlResult<InherentImpl> {
    let mut out = InherentImpl {
        style: "constant".to_string(),
        ..Default::default()
    };
    let len = body.len();
    let mut pos = 0;
    while pos < len {
        pos = skip_trivia(body, pos);
        if pos >= len {
            break;
        }
        let item = advance_item_start(body, pos)?;
        let (docs, _) = docs_and_attrs_before(body, item)?;
        let attrs = collect_attrs_before(body, item);
        pos = item;
        let mut is_const = false;
        if sw(body, pos, "pub ") {
            pos += 4;
        }
        if sw(body, pos, "const ") {
            is_const = true;
            pos += "const ".len();
        }
        if !sw(body, pos, "fn ") {
            let (name, p) = read_ident(body, pos)?;
            if name != "CLASS_NAME" {
                out.extra = true;
            }
            pos = skip_item(body, p)?;
            continue;
        }
        pos += 3;
        let (name, p) = read_ident(body, pos)?;
        pos = p;
        let sig_end = find_fn_body_start(body, pos);
        let fn_body = sub(body, sig_end, len);
        let body_end = if sig_end > 0 {
            matching_brace(body, sig_end - 1)
        } else {
            len
        };
        match name.as_str() {
            "new" => {
                let must_use = attrs.iter().any(|a| is_must_use(a));
                let other: Vec<String> =
                    attrs.iter().filter(|a| !is_must_use(a)).cloned().collect();
                let braced = if sig_end > 0 {
                    sub(body, sig_end - 1, body_end + 1)
                } else {
                    ""
                };
                let new_body = new_body_lines(braced, field_names);
                out.new = NewShape {
                    present: true,
                    const_: is_const,
                    must_use,
                    attrs: other,
                    docs,
                    body: new_body,
                    params: Some(raw_params(body, pos)?),
                };
            }
            "serializer" => {
                out.ser_docs = docs;
                out.ser_must_use = attrs.iter().any(|a| is_must_use(a));
                let (style, ser_name) = parse_serializer_body(fn_body)?;
                out.style = style;
                out.ser_name = ser_name;
            }
            _ => out.extra = true,
        }
        pos = if body_end < len { body_end + 1 } else { len };
    }
    Ok(out)
}

/// `extract_shape(source, cls, module, packet_path=...)`.
pub fn extract_shape(
    source: &str,
    cls: &str,
    module: &str,
    packet_path: Option<&Path>,
) -> BmlResult<Shape> {
    let stem = module.rsplit_once('/').map_or(module, |(_, s)| s);
    let items: Vec<Item> = scan_items(source)?;
    let coord_names = coordinate_names(
        items
            .iter()
            .filter(|it| it.kind == "use")
            .map(|it| it.text.as_str()),
    )?;
    let packet = items
        .iter()
        .find(|it| it.kind == "struct" && it.name.as_deref() == Some(cls))
        .ok_or_else(|| shape_error(format!("missing struct {cls}")))?;
    let (struct_docs, derives, form, raw_fields) = packet_struct(&packet.text, cls)?;
    let fields: Vec<FieldShape> = raw_fields
        .into_iter()
        .map(|f| FieldShape {
            type_: normalise_types(&f.type_, &coord_names),
            ..f
        })
        .collect();
    let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();

    let mut new_shape = NewShape::default();
    let mut ser_fn_docs: Vec<String> = Vec::new();
    let mut ser_fn_must_use = false;
    let mut ser_style = "constant".to_string();
    let mut ser_name = String::new();
    let mut serializer_structs: Vec<SerializerShape> = Vec::new();
    let mut serializer_impls: Vec<(String, String)> = Vec::new();
    let mut use_texts: Vec<&str> = Vec::new();
    let mut main_impl_seen = false;
    let mut inherent_extra = false;
    let mut module_extra = false;
    let mut inline_tests = false;

    for item in &items {
        let name_is_cls = item.name.as_deref() == Some(cls);
        let name_str = item.name.as_deref().unwrap_or("");
        match item.kind.as_str() {
            "use" => use_texts.push(&item.text),
            "struct" if name_is_cls => {}
            "struct" if name_str.ends_with("Serializer") => {
                let head =
                    struct_head(&item.text).ok_or_else(|| shape_error("malformed struct"))?;
                let (docs, _) = docs_and_attrs_before(&item.text, head.start)?;
                serializer_structs.push(SerializerShape {
                    style: "struct".to_string(),
                    name: name_str.to_string(),
                    public: sub(&item.text, head.start, head.end).starts_with("pub"),
                    docs,
                    read_via_new: false,
                });
            }
            "impl" => {
                let (body_start, body_end) = impl_body_bounds(&item.text)?;
                let body = sub(&item.text, body_start, body_end);
                if name_is_cls && item.trait_.is_none() {
                    let inh = parse_inherent_impl(body, &field_names)?;
                    if inh.new.present || !inh.ser_name.is_empty() {
                        if main_impl_seen {
                            module_extra = true;
                        } else {
                            main_impl_seen = true;
                            new_shape = inh.new;
                            ser_fn_docs = inh.ser_docs;
                            ser_fn_must_use = inh.ser_must_use;
                            ser_style = inh.style;
                            ser_name = inh.ser_name;
                            inherent_extra = inh.extra;
                        }
                    } else {
                        module_extra = true;
                    }
                } else if name_is_cls
                    && item
                        .trait_
                        .as_deref()
                        .is_some_and(|t| ALLOWED_TRAITS_FOR_CLS.contains(&t))
                {
                } else if item.trait_.as_deref() == Some("PacketSerializer")
                    && name_str.ends_with("Serializer")
                {
                    serializer_impls.push((name_str.to_string(), body.to_string()));
                } else {
                    module_extra = true;
                }
            }
            "mod" => {
                let has_block = item.text.trim_end().ends_with('}');
                let mod_name = item.name.as_deref();
                if has_block && item.cfg_test {
                    inline_tests = true;
                } else if !has_block
                    && (mod_name == Some("ext") || (mod_name == Some("tests") && item.cfg_test))
                {
                } else {
                    module_extra = true;
                }
            }
            _ => module_extra = true,
        }
    }

    let (mut serializer_struct, chosen_len): (Option<SerializerShape>, usize) =
        if ser_style == "struct" {
            let chosen: Vec<&SerializerShape> = serializer_structs
                .iter()
                .filter(|s| s.name == ser_name)
                .collect();
            if chosen.is_empty() {
                return Err(shape_error(format!("missing serializer struct for {cls}")));
            }
            (Some(chosen[0].clone()), chosen.len())
        } else {
            let first = serializer_structs.first().cloned();
            let n = usize::from(first.is_some());
            (first, n)
        };
    module_extra = module_extra || serializer_structs.len() > chosen_len;
    for (target, body) in &serializer_impls {
        match serializer_struct.as_mut() {
            Some(s) if *target == s.name => {
                s.read_via_new = serializer_read_via_new(body, cls)?;
            }
            _ => module_extra = true,
        }
    }
    let ser = serializer_struct.unwrap_or(SerializerShape {
        style: ser_style,
        name: ser_name,
        public: false,
        docs: Vec::new(),
        read_via_new: false,
    });

    let raw_params_text = normalise_types(new_shape.params.as_deref().unwrap_or(""), &coord_names);
    let default_params = fields
        .iter()
        .map(|f| format!("{}: {}", f.name, f.type_))
        .collect::<Vec<_>>()
        .join(", ");
    new_shape.params = if new_shape.present && raw_params_text != default_params {
        Some(raw_params_text.clone())
    } else {
        None
    };

    let mut haystack_parts: Vec<&str> = fields.iter().map(|f| f.type_.as_str()).collect();
    haystack_parts.push(&raw_params_text);
    haystack_parts.extend(new_shape.body.iter().map(String::as_str));
    let haystack = haystack_parts.join(" ");

    let sibling = packet_path.and_then(|p| p.parent());
    let needed = format!("{haystack}{}", sibling_text(sibling, stem)?);
    let imports = hand_imports(&use_texts, &needed)?;

    let tests_name = format!("{stem}.tests.rs");
    let tests = if inline_tests || sibling_exists(sibling, &tests_name) {
        Some(tests_name)
    } else {
        None
    };
    let ext_name = format!("{stem}.ext.rs");
    let has_extra = module_extra || inherent_extra || sibling_exists(sibling, &ext_name);

    Ok(Shape {
        cls: cls.to_string(),
        module: module.to_string(),
        module_docs: leading_module_docs(source),
        struct_docs,
        derives,
        form,
        fields,
        new: new_shape,
        serializer_fn_docs: ser_fn_docs,
        serializer_fn_must_use: ser_fn_must_use,
        serializer: ser,
        extras: if has_extra { Some(ext_name) } else { None },
        imports,
        tests,
        ..Default::default()
    })
}
