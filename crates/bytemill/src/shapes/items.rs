//! Column-0 item scanner for hand-written rstom packet files.
//!
//! Port of `bps/shapes/items.py`. An item starts at its first doc or attribute
//! line (or at its code line when it has none) and ends at the depth-0 `;` or
//! the `}` that closes its outermost brace. Strings, chars, raw strings and
//! comments are skipped, so braces inside them never change the depth.
//!
//! The Python regular expressions are matched by hand; each `classify*` helper
//! documents the pattern it replaces. Offsets are byte offsets (see `scan.rs`).

use super::model::shape_error;
use super::scan::{attr_block_end, find_from, skip_block_comment, skip_rust_string, sub, sw};
use crate::errors::BpsResult;

/// A top-level item: `struct`, `impl`, `use`, `fn`, ...
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: String,
    pub name: Option<String>,
    pub trait_: Option<String>,
    /// Offset of the first doc-or-code line of the item.
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub cfg_test: bool,
}

/// A member of an impl body.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub kind: String,
    pub name: Option<String>,
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Python `\w` (unicode word character).
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `\s+` at `i`: end offset after the whitespace run, or `None` if empty.
fn ws_run(s: &str, i: usize) -> Option<usize> {
    let rest = s.get(i..)?;
    let n: usize = rest
        .chars()
        .take_while(|c| c.is_whitespace())
        .map(char::len_utf8)
        .sum();
    if n == 0 { None } else { Some(i + n) }
}

/// `\w+` at `i`: end offset after the word run, or `None` if empty.
fn word_run(s: &str, i: usize) -> Option<usize> {
    let rest = s.get(i..)?;
    let n: usize = rest
        .chars()
        .take_while(|c| is_word(*c))
        .map(char::len_utf8)
        .sum();
    if n == 0 { None } else { Some(i + n) }
}

/// `kw\s+` at `i`: end offset after the whitespace following the keyword.
fn kw_ws(s: &str, i: usize, kw: &str) -> Option<usize> {
    if sw(s, i, kw) {
        ws_run(s, i + kw.len())
    } else {
        None
    }
}

/// `kw\b` at `i`: end offset after the keyword when no word character follows.
fn kw_bound(s: &str, i: usize, kw: &str) -> Option<usize> {
    if !sw(s, i, kw) {
        return None;
    }
    let end = i + kw.len();
    match s.get(end..).and_then(|r| r.chars().next()) {
        Some(c) if is_word(c) => None,
        _ => Some(end),
    }
}

/// `(?:unsafe\s+)?impl\b` anchored at the start of `s`; end offset after `impl`.
fn impl_at(s: &str) -> Option<usize> {
    let start = kw_ws(s, 0, "unsafe").unwrap_or(0);
    kw_bound(s, start, "impl")
}

/// `_VIS.sub("", text, count=1)` with `_VIS = pub(?:\([^)]*\))?\s+`.
fn vis_strip(text: &str) -> String {
    for i in 0..text.len() {
        if !sw(text, i, "pub") {
            continue;
        }
        let j = i + 3;
        // `[^)]*` stops at the first `)`; if none exists the group is skipped.
        let group_end = if sw(text, j, "(") {
            find_from(text, j + 1, ")").map(|k| k + 1)
        } else {
            None
        };
        let end = group_end
            .and_then(|g| ws_run(text, g))
            .or_else(|| ws_run(text, j));
        if let Some(end) = end {
            return format!("{}{}", sub(text, 0, i), sub(text, end, text.len()));
        }
    }
    text.to_string()
}

/// `(?:const\s+|unsafe\s+|async\s+)*` at `i`: offset after the qualifiers.
fn skip_fn_qualifiers(h: &str, mut i: usize) -> usize {
    loop {
        let next = kw_ws(h, i, "const")
            .or_else(|| kw_ws(h, i, "unsafe"))
            .or_else(|| kw_ws(h, i, "async"));
        match next {
            Some(j) => i = j,
            None => return i,
        }
    }
}

/// `_classify(header)`: (kind, name, trait) of a top-level item.
///
/// `_TOP_KINDS` is tried in order: `use\b`, `fn`, `struct`, `enum`, `union`,
/// `trait`, `mod`, `static`, `const`, `type`, `macro_rules!`, `impl`.
fn classify(header_text: &str) -> (String, Option<String>, Option<String>) {
    let head = vis_strip(header_text);
    let h = head.as_str();
    let name_after = |i: Option<usize>| -> Option<String> {
        let i = i?;
        let e = word_run(h, i)?;
        Some(sub(h, i, e).to_string())
    };
    let named = |kind: &str, name: Option<String>| (kind.to_string(), name, None);

    if kw_bound(h, 0, "use").is_some() {
        return named("use", None);
    }
    let fn_at = skip_fn_qualifiers(h, 0);
    if let Some(n) = name_after(kw_ws(h, fn_at, "fn")) {
        return named("fn", Some(n));
    }
    for kw in ["struct", "enum", "union", "trait", "mod"] {
        if let Some(n) = name_after(kw_ws(h, 0, kw)) {
            return named(kw, Some(n));
        }
    }
    // `static\s+(?:mut\s+)?(\w+)`: the optional `mut` backtracks when no name follows.
    if let Some(after_static) = kw_ws(h, 0, "static") {
        let with_mut = kw_ws(h, after_static, "mut").and_then(|j| name_after(Some(j)));
        if let Some(n) = with_mut.or_else(|| name_after(Some(after_static))) {
            return named("static", Some(n));
        }
    }
    if let Some(n) = name_after(kw_ws(h, 0, "const")) {
        return named("const", Some(n));
    }
    if let Some(n) = name_after(kw_ws(h, 0, "type")) {
        return named("type", Some(n));
    }
    // `macro_rules!\s*(\w+)`
    if sw(h, 0, "macro_rules!") {
        let j = ws_run(h, "macro_rules!".len()).unwrap_or("macro_rules!".len());
        if let Some(n) = name_after(Some(j)) {
            return named("macro", Some(n));
        }
    }
    if impl_at(h).is_some() {
        let (trait_, target) = impl_names(h);
        return ("impl".to_string(), Some(target), trait_);
    }
    ("other".to_string(), None, None)
}

/// `_classify_member` for impl members: (kind, name).
///
/// `_MEMBER_KINDS` is tried in order: `fn`, `const`, `type`.
fn classify_member(header_text: &str) -> (String, Option<String>) {
    let head = vis_strip(header_text);
    let h = head.as_str();
    let fn_at = skip_fn_qualifiers(h, 0);
    if let Some(j) = kw_ws(h, fn_at, "fn")
        && let Some(e) = word_run(h, j)
    {
        return ("fn".to_string(), Some(sub(h, j, e).to_string()));
    }
    for kw in ["const", "type"] {
        if let Some(j) = kw_ws(h, 0, kw)
            && let Some(e) = word_run(h, j)
        {
            return (kw.to_string(), Some(sub(h, j, e).to_string()));
        }
    }
    ("other".to_string(), None)
}

/// `_impl_names(head)`: (trait, target) of an impl header.
fn impl_names(head: &str) -> (Option<String>, String) {
    let after = impl_at(head).unwrap_or(0);
    let mut text = sub(head, after, head.len()).trim_start().to_string();
    if text.starts_with('<') {
        let end = angle_end(&text);
        text = sub(&text, end, text.len()).trim_start().to_string();
    }
    if let Some(brace) = text.find('{') {
        text.truncate(brace);
    }
    let text = text
        .split(" where ")
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let last_seg = |s: &str| -> String {
        without_generics(s)
            .rsplit("::")
            .next()
            .unwrap_or("")
            .to_string()
    };
    if let Some((trait_part, target_part)) = text.split_once(" for ") {
        (Some(last_seg(trait_part)), last_seg(target_part))
    } else {
        (None, last_seg(&text))
    }
}

/// `_angle_end`: offset just past the `>` closing the leading `<` of `text`.
fn angle_end(text: &str) -> usize {
    let b = text.as_bytes();
    let mut depth: i64 = 0;
    for (i, &ch) in b.iter().enumerate() {
        if ch == b'<' {
            depth += 1;
        } else if ch == b'>' && !(i > 0 && b[i - 1] == b'-') {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
    }
    text.len()
}

/// `_without_generics`.
fn without_generics(text: &str) -> &str {
    text.split('<').next().unwrap_or("").trim()
}

/// `_next_line`: offset after the newline at or following `pos`, capped at `limit`.
fn next_line(src: &str, pos: usize, limit: usize) -> usize {
    match find_from(src, pos, "\n") {
        None => limit,
        Some(nl) => (nl + 1).min(limit),
    }
}

/// `_item_end`: offset just past the item starting at `pos`.
fn item_end(src: &str, pos: usize) -> usize {
    let b = src.as_bytes();
    let n = b.len();
    let mut depth: i64 = 0;
    let mut i = pos;
    while i < n {
        let ch = b[i];
        if ch == b'"' || ch == b'\'' {
            i = skip_rust_string(src, i);
            continue;
        }
        if sw(src, i, "//") {
            i = find_from(src, i, "\n").unwrap_or(n);
            continue;
        }
        if sw(src, i, "/*") {
            i = skip_block_comment(src, i);
            continue;
        }
        if matches!(ch, b'(' | b'[' | b'{') {
            depth += 1;
        } else if matches!(ch, b')' | b']' | b'}') {
            depth -= 1;
            if depth == 0 && ch == b'}' {
                let mut j = i + 1;
                while j < n && matches!(b[j], b' ' | b'\t') {
                    j += 1;
                }
                return if j < n && b[j] == b';' { j + 1 } else { i + 1 };
            }
        } else if ch == b';' && depth == 0 {
            return i + 1;
        }
        i += 1;
    }
    n
}

/// `_entries`: (first doc-or-code offset, code offset, end offset) per item in `src[pos:limit]`.
fn entries(
    src: &str,
    mut pos: usize,
    limit: usize,
    indent: &str,
) -> BpsResult<Vec<(usize, usize, usize)>> {
    let mut out = Vec::new();
    let mut pending: Option<usize> = None;
    while pos < limit {
        let eol = match sub(src, pos, limit).find('\n') {
            Some(i) => pos + i,
            None => limit,
        };
        let line = sub(src, pos, eol);
        let has_indent = line.starts_with(indent);
        let stripped = if has_indent {
            &line[indent.len()..]
        } else {
            line
        };
        let first = pos + if has_indent { indent.len() } else { 0 };
        if stripped.trim().is_empty()
            || (stripped.starts_with("//") && !stripped.starts_with("///"))
        {
            pos = eol + 1;
            continue;
        }
        if stripped.starts_with("///") {
            pending = pending.or(Some(first));
            pos = eol + 1;
            continue;
        }
        if stripped.starts_with("/*") {
            if stripped.starts_with("/**") && !stripped.starts_with("/**/") {
                pending = pending.or(Some(first));
            }
            pos = skip_block_comment(src, first);
            while pos < limit && matches!(super::scan::at(src, pos), Some(b' ' | b'\t')) {
                pos += 1;
            }
            continue;
        }
        if stripped.starts_with("#[") {
            pending = pending.or(Some(first));
            let attr_end = attr_block_end(src, first)?;
            pos = next_line(src, attr_end, limit);
            continue;
        }
        let start = pending.take().unwrap_or(first);
        let end = item_end(src, first);
        out.push((start, first, end));
        pos = next_line(src, end, limit);
    }
    Ok(out)
}

/// Offset of the first line starting with `(?:unsafe\s+)?impl\b` (`^` in multiline mode).
fn impl_header_line(text: &str) -> Option<usize> {
    let mut p = 0;
    loop {
        if impl_at(sub(text, p, text.len())).is_some() {
            return Some(p);
        }
        p = find_from(text, p, "\n")? + 1;
    }
}

/// `_CFG_TEST` search: `^#\[cfg\(test\)\]\s*$` (multiline) within `s`.
fn cfg_test_in(s: &str) -> bool {
    let mut p = 0;
    loop {
        if sw(s, p, "#[cfg(test)]") {
            let rest = sub(s, p + "#[cfg(test)]".len(), s.len());
            if rest.split('\n').next().unwrap_or("").trim().is_empty() {
                return true;
            }
        }
        match find_from(s, p, "\n") {
            Some(nl) => p = nl + 1,
            None => return false,
        }
    }
}

/// `scan_items`.
pub fn scan_items(src: &str) -> BpsResult<Vec<Item>> {
    let mut items = Vec::new();
    for (start, code, end) in entries(src, 0, src.len(), "")? {
        let (kind, name, trait_) = classify(sub(src, code, end));
        items.push(Item {
            kind,
            name,
            trait_,
            start,
            end,
            text: sub(src, start, end).to_string(),
            cfg_test: cfg_test_in(sub(src, start, code)),
        });
    }
    Ok(items)
}

/// `impl_members`.
pub fn impl_members(item: &Item) -> BpsResult<Vec<Member>> {
    if item.kind != "impl" {
        return Ok(Vec::new());
    }
    let src = item.text.as_str();
    let (body_start, body_end) = impl_body_bounds(src)?;
    let mut members = Vec::new();
    for (start, code, end) in entries(src, body_start, body_end, "    ")? {
        let (kind, name) = classify_member(sub(src, code, end));
        members.push(Member {
            kind,
            name,
            start: item.start + start,
            end: item.start + end,
            text: sub(src, start, end).to_string(),
        });
    }
    Ok(members)
}

/// `impl_body_bounds`: offsets of the impl body inside an impl item's text (between its braces).
pub fn impl_body_bounds(text: &str) -> BpsResult<(usize, usize)> {
    let header = impl_header_line(text).ok_or_else(|| shape_error("malformed impl"))?;
    let open = find_from(text, header, "{").ok_or_else(|| shape_error("substring not found"))?;
    let close = text
        .rfind('}')
        .ok_or_else(|| shape_error("substring not found"))?;
    Ok((open + 1, close))
}
