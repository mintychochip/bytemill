//! Schema diagnostics: spans, line/column positions, source excerpts, suggestions.
//!
//! Library `BmlError` text stays the Python-compatible wording. Friendly messages,
//! carets, and did-you-mean hints exist only here.

use crate::analyze::analyze;
use crate::ast::Schema;
use crate::errors::{BmlError, py_repr_str};
use crate::json::{Value, dumps_sorted};
use crate::lexer::{TokKind, TokValue, Token, tokenize};
use crate::load::{LoadFault, Loaded, UnresolvedUse, load_for_diag};
use crate::ordmap::OrderedMap;
use crate::primitives::{BARE_PRIMITIVES, is_bare_primitive};
use crate::refs::{RefKind, references};
use std::path::Path;

/// Char offsets into one file's text, the same unit the lexer uses. `end` is exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// Diagnostic severity. Only errors fail a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
        }
    }
}

/// One schema problem, with a char span and 1-based line/column when known.
///
/// `line == 0` means the problem has no source position (a missing file, for example).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub file: Option<String>,
    pub span: Option<Span>,
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    pub help: Option<String>,
}

/// File name → source text, keyed by the same string [`Diagnostic::file`] carries.
#[derive(Debug, Clone, Default)]
pub struct SourceSet {
    files: Vec<(String, String)>,
}

impl SourceSet {
    pub fn new() -> Self {
        Self { files: Vec::new() }
    }

    pub fn insert(&mut self, name: impl Into<String>, text: impl Into<String>) {
        let name = name.into();
        let text = text.into();
        if let Some(slot) = self.files.iter_mut().find(|(n, _)| n == &name) {
            slot.1 = text;
        } else {
            self.files.push((name, text));
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, text)| text.as_str())
    }

    /// Read each diagnostic's file (universal newlines, same as the loader).
    pub fn from_diagnostics(diagnostics: &[Diagnostic]) -> Self {
        let mut set = Self::new();
        for diagnostic in diagnostics {
            let Some(file) = &diagnostic.file else {
                continue;
            };
            if set.get(file).is_some() {
                continue;
            }
            if let Ok(text) = crate::text::read_text(Path::new(file)) {
                set.insert(file.clone(), text);
            }
        }
        set
    }
}

/// Lex and parse one buffer. When it has no imports, also report unresolved names
/// and duplicates — imports cannot be resolved from a buffer alone.
pub fn diagnose_source(name: &str, text: &str) -> Vec<Diagnostic> {
    match crate::parser::parse(text) {
        Err(err) => vec![syntax_diag(name, text, &err)],
        Ok(schema) => source_body(name, text, &schema),
    }
}

/// What `bml check` reports. Empty means the schemas load and analyze.
pub fn diagnose_files(paths: &[impl AsRef<Path>]) -> Vec<Diagnostic> {
    match load_for_diag(paths) {
        Err(fault) => fault_diags(*fault),
        Ok(loaded) => loaded_diags(&loaded),
    }
}

/// Rustc-style human rendering. Missing source omits the excerpt but keeps the location.
pub fn render(diagnostics: &[Diagnostic], sources: &SourceSet) -> String {
    diagnostics
        .iter()
        .map(|d| render_one(d, sources))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `{"diagnostics":[...],"ok":false}` with keys sorted by [`dumps_sorted`].
/// `help` is omitted when absent. `ok` is true only when `diagnostics` is empty.
pub fn to_json(diagnostics: &[Diagnostic]) -> String {
    let items = diagnostics.iter().map(diagnostic_json).collect();
    let mut root = OrderedMap::new();
    root.insert("diagnostics", Value::list(items));
    root.insert("ok", Value::Bool(diagnostics.is_empty()));
    dumps_sorted(&Value::map(root))
}

fn diagnostic_json(d: &Diagnostic) -> Value {
    let mut obj = OrderedMap::new();
    obj.insert("column", Value::Int(d.column as i128));
    obj.insert("end_column", Value::Int(d.end_column as i128));
    obj.insert("end_line", Value::Int(d.end_line as i128));
    if let Some(file) = &d.file {
        obj.insert("file", Value::str(file.as_str()));
    }
    if let Some(help) = &d.help {
        obj.insert("help", Value::str(help.as_str()));
    }
    obj.insert("line", Value::Int(d.line as i128));
    obj.insert("message", Value::str(d.message.as_str()));
    obj.insert("severity", Value::str(d.severity.as_str()));
    Value::map(obj)
}

fn source_body(name: &str, text: &str, schema: &Schema) -> Vec<Diagnostic> {
    let mut out = duplicate_in_text(name, text, schema);
    if schema.imports.is_empty() {
        out.extend(unknown_in_schema(name, text, schema));
    }
    out
}

fn loaded_diags(loaded: &Loaded) -> Vec<Diagnostic> {
    let mut out = duplicates_across(&loaded.files);
    if let Err(err) = analyze(&loaded.schema)
        && !err.message.starts_with("duplicate ")
    {
        out.push(analyze_other(&err, &loaded.files));
    }
    out
}

fn fault_diags(fault: LoadFault) -> Vec<Diagnostic> {
    if !fault.uses.is_empty() {
        let text = fault.text.unwrap_or_default();
        return fault
            .uses
            .iter()
            .map(|use_| {
                name_diag(
                    &fault.shown,
                    &text,
                    use_,
                    &fault.visible_types,
                    &fault.visible_packets,
                )
            })
            .collect();
    }
    if let Some(text) = &fault.text
        && fault.error.offset.is_some()
    {
        return vec![syntax_diag(&fault.shown, text, &fault.error)];
    }
    vec![unpositioned(&fault.shown, fault.error.message)]
}

fn name_diag(
    file: &str,
    text: &str,
    use_: &UnresolvedUse,
    types: &[String],
    packets: &[String],
) -> Diagnostic {
    let message = format!(
        "{} is not declared here or in an imported file",
        py_repr_str(&use_.name)
    );
    let suggestion = suggest(&use_.name, &pool(use_.kind, types, packets));
    let help = name_help(use_, suggestion.as_deref());
    match locate(text, &use_.name, &use_.where_) {
        Some(span) => at_span(file, text, message, span, help),
        None => unpositioned(file, message).with_help(help),
    }
}

fn name_help(use_: &UnresolvedUse, suggestion: Option<&str>) -> Option<String> {
    if !use_.imports.is_empty() {
        let lines = use_
            .imports
            .iter()
            .map(|path| format!("add  import \"{path}\"  for {}", use_.name))
            .collect::<Vec<_>>();
        return Some(lines.join("\n"));
    }
    if let Some(name) = suggestion {
        return Some(format!("did you mean `{name}`?"));
    }
    let where_ = if use_.where_.is_empty() {
        "this file".to_string()
    } else {
        use_.where_.clone()
    };
    Some(format!(
        "{} (used in {where_}) is declared in no loaded or nearby .bml file",
        py_repr_str(&use_.name)
    ))
}

fn pool(kind: RefKind, types: &[String], packets: &[String]) -> Vec<String> {
    match kind {
        RefKind::Packet => packets.to_vec(),
        RefKind::Spread => types.to_vec(),
        RefKind::Type => {
            let mut names = types.to_vec();
            for primitive in BARE_PRIMITIVES {
                if !names.iter().any(|n| n == primitive) {
                    names.push((*primitive).to_string());
                }
            }
            names
        }
    }
}

fn unknown_in_schema(file: &str, text: &str, schema: &Schema) -> Vec<Diagnostic> {
    let (types, packets) = declared(schema);
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut out = Vec::new();
    for ref_ in references(schema) {
        let key = (ref_.name.clone(), ref_.where_.clone());
        if resolved(&ref_.name, ref_.kind, &types, &packets) || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        let use_ = UnresolvedUse {
            name: ref_.name.clone(),
            where_: ref_.where_.clone(),
            kind: ref_.kind,
            imports: Vec::new(),
        };
        let mut diag = name_diag(file, text, &use_, &types, &packets);
        // A buffer has no neighbouring files, so the nearby-file sentence does not apply.
        if diag.help.as_deref().is_some_and(|h| h.contains("nearby")) {
            diag.help = None;
        }
        out.push(diag);
    }
    out
}

fn resolved(name: &str, kind: RefKind, types: &[String], packets: &[String]) -> bool {
    match kind {
        RefKind::Packet => packets.iter().any(|n| n == name),
        RefKind::Spread => types.iter().any(|n| n == name),
        RefKind::Type => types.iter().any(|n| n == name) || is_bare_primitive(name),
    }
}

fn declared(schema: &Schema) -> (Vec<String>, Vec<String>) {
    let mut types = Vec::new();
    let mut packets = Vec::new();
    for enc in &schema.encodings {
        types.push(enc.name.clone());
    }
    for e in &schema.enums {
        types.push(e.name.clone());
    }
    for pkt in &schema.packets {
        packets.push(pkt.name.clone());
    }
    (types, packets)
}

fn duplicate_in_text(file: &str, text: &str, schema: &Schema) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    push_dups(
        &mut out,
        file,
        text,
        "encoding",
        schema.encodings.iter().map(|d| d.name.as_str()),
    );
    push_dups(
        &mut out,
        file,
        text,
        "enum",
        schema.enums.iter().map(|d| d.name.as_str()),
    );
    push_dups(
        &mut out,
        file,
        text,
        "packet",
        schema.packets.iter().map(|d| d.name.as_str()),
    );
    push_dups(
        &mut out,
        file,
        text,
        "protocol",
        schema.protocols.iter().map(|d| d.name.as_str()),
    );
    out
}

fn push_dups<'a>(
    out: &mut Vec<Diagnostic>,
    file: &str,
    text: &str,
    kind: &str,
    names: impl Iterator<Item = &'a str>,
) {
    let mut seen: Vec<&str> = Vec::new();
    for name in names {
        if seen.contains(&name) {
            out.push(duplicate_diag(file, text, kind, name));
        } else {
            seen.push(name);
        }
    }
}

fn duplicates_across(files: &[crate::load::LoadedFile]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (kind, keyword) in [
        ("encoding", "encoding"),
        ("enum", "enum"),
        ("packet", "packet"),
        ("protocol", "protocol"),
    ] {
        let mut seen: Vec<(usize, String)> = Vec::new();
        for (fi, file) in files.iter().enumerate() {
            for name in decl_names(&file.text, keyword) {
                if seen.iter().any(|(_, n)| n == &name) {
                    out.push(duplicate_diag(&file.shown, &file.text, kind, &name));
                } else {
                    seen.push((fi, name));
                }
            }
        }
    }
    out
}

fn duplicate_diag(file: &str, text: &str, kind: &str, name: &str) -> Diagnostic {
    let message = format!("duplicate {kind} {}", py_repr_str(name));
    let span = nth_decl(text, kind, name, 1).or_else(|| nth_decl(text, kind, name, 0));
    match span {
        Some(span) => at_span(file, text, message, span, None),
        None => unpositioned(file, message),
    }
}

fn analyze_other(err: &BmlError, files: &[crate::load::LoadedFile]) -> Diagnostic {
    if let Some((name, where_)) = unknown_from_message(&err.message) {
        for file in files {
            if let Some(span) = locate(&file.text, &name, &where_) {
                return at_span(&file.shown, &file.text, err.message.clone(), span, None);
            }
        }
    }
    let file = files.first().map(|f| f.shown.clone());
    Diagnostic {
        severity: Severity::Error,
        message: err.message.clone(),
        file,
        span: None,
        line: 0,
        column: 0,
        end_line: 0,
        end_column: 0,
        help: None,
    }
}

fn unknown_from_message(message: &str) -> Option<(String, String)> {
    if let Some(rest) = message.strip_prefix("unknown type ") {
        return split_repr_in(rest);
    }
    if let Some(rest) = message.strip_prefix("unknown spread encoding ") {
        return split_repr_in(rest);
    }
    if let Some((where_, rest)) = message.split_once(" references unknown packet ") {
        let name = unquote_py(rest)?;
        return Some((name, where_.to_string()));
    }
    None
}

fn split_repr_in(rest: &str) -> Option<(String, String)> {
    let name = unquote_py(rest)?;
    let quoted = py_repr_str(&name);
    let after = rest.strip_prefix(&quoted)?;
    let where_ = after.strip_prefix(" in ")?.to_string();
    Some((name, where_))
}

fn syntax_diag(file: &str, text: &str, err: &BmlError) -> Diagnostic {
    let offset = err.offset.unwrap_or(0);
    let span = span_for(text, offset, &err.message);
    let found = found_text(text, &span);
    let message = friendly(&err.message, &found);
    at_span(file, text, message, span, None)
}

fn unpositioned(file: &str, message: String) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        message,
        file: Some(file.to_string()),
        span: None,
        line: 0,
        column: 0,
        end_line: 0,
        end_column: 0,
        help: None,
    }
}

impl Diagnostic {
    fn with_help(mut self, help: Option<String>) -> Self {
        self.help = help;
        self
    }
}

fn at_span(
    file: &str,
    text: &str,
    message: String,
    span: Span,
    help: Option<String>,
) -> Diagnostic {
    let lines = index_lines(text);
    let (line, column) = lines.position(span.start);
    let (end_line, mut end_column) = lines.position(span.end);
    if end_line == line && end_column <= column {
        end_column = column + 1;
    }
    Diagnostic {
        severity: Severity::Error,
        message,
        file: Some(file.to_string()),
        span: Some(span),
        line,
        column,
        end_line,
        end_column,
        help,
    }
}

// ---------------------------------------------------------------------------
// Positions. Offsets are Unicode scalar values, matching the lexer. `\r\n` is
// one line break (two offsets). A trailing break does not open a new line, so
// an error at end of input stays on the last line.
// ---------------------------------------------------------------------------

struct LineIndex {
    starts: Vec<usize>,
    ends: Vec<usize>,
    len: usize,
}

fn index_lines(text: &str) -> LineIndex {
    let chars: Vec<char> = text.chars().collect();
    let mut starts = vec![0];
    let mut ends = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let br = break_len(&chars, i);
        if br > 0 {
            ends.push(i);
            i += br;
            if i < chars.len() {
                starts.push(i);
            }
        } else {
            i += 1;
        }
    }
    if ends.len() < starts.len() {
        ends.push(chars.len());
    }
    LineIndex {
        starts,
        ends,
        len: chars.len(),
    }
}

impl LineIndex {
    fn position(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.len);
        for (idx, &start) in self.starts.iter().enumerate() {
            let content_end = self.ends[idx];
            let region_end = self.starts.get(idx + 1).copied().unwrap_or(self.len);
            let last = idx + 1 == self.starts.len();
            if offset < region_end || last {
                let col = if offset <= content_end {
                    offset - start + 1
                } else {
                    content_end - start + 1
                };
                return (idx + 1, col.max(1));
            }
        }
        (1, 1)
    }
}

fn break_len(chars: &[char], i: usize) -> usize {
    if i >= chars.len() {
        return 0;
    }
    if chars[i] == '\r' && i + 1 < chars.len() && chars[i + 1] == '\n' {
        2
    } else if chars[i] == '\n' || chars[i] == '\r' {
        1
    } else {
        0
    }
}

fn line_text(text: &str, line: usize) -> String {
    let lines = index_lines(text);
    if lines.starts.is_empty() {
        return String::new();
    }
    let idx = line.saturating_sub(1).min(lines.starts.len() - 1);
    let start = lines.starts[idx];
    let end = lines.ends[idx];
    text.chars().skip(start).take(end - start).collect()
}

// ---------------------------------------------------------------------------
// Friendly wording. The parser's closed set of messages is mapped; the
// original `BmlError` text is not rewritten.
// ---------------------------------------------------------------------------

fn friendly(message: &str, found: &str) -> String {
    if let Some(mapped) = map_message(message, found) {
        return mapped;
    }
    replace_kinds(message)
}

fn map_message(message: &str, found: &str) -> Option<String> {
    if message.starts_with("unexpected character ")
        || message.starts_with("unterminated string")
        || message.starts_with("hex literal")
        || message.starts_with("int too large")
        || message == "match arm must be a block"
        || message == "'_' cannot be combined with other tags"
    {
        return Some(message.to_string());
    }
    if let Some(rest) = message.strip_prefix("expected TokKind.") {
        let (expected, got) = rest.split_once(", got TokKind.")?;
        let found_disp = if found.is_empty() {
            kind_phrase(got)
        } else {
            found.to_string()
        };
        return Some(format!(
            "expected {}, found {found_disp}",
            kind_phrase(expected)
        ));
    }
    if let Some(rest) = message.strip_prefix("expected ") {
        if let Some(quoted) = unquote_py(rest) {
            return Some(format!("expected `{quoted}`"));
        }
        if rest == "import path" {
            return Some(with_found("expected an import path", found));
        }
        if rest == "ident value" {
            return Some(with_found("expected a name", found));
        }
        if rest == "until, until_end, or count" {
            return Some(with_found(
                "expected `until`, `until_end`, or `count`",
                found,
            ));
        }
        if rest == "transform" {
            return Some(with_found("expected a transform", found));
        }
    }
    if message.starts_with("expected top-level declaration, got ") {
        return Some(with_found("expected a declaration", found));
    }
    if message.starts_with("unexpected in protocol:") {
        return Some(format!("unexpected {found} in a protocol"));
    }
    if message.starts_with("expected match or switch, got ") {
        return Some(format!("expected `match` or `switch`, found {found}"));
    }
    None
}

fn with_found(prefix: &str, found: &str) -> String {
    if found.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}, found {found}")
    }
}

fn kind_phrase(name: &str) -> String {
    match name {
        "IDENT" => "a name".to_string(),
        "INT" => "a number".to_string(),
        "STRING" => "a string".to_string(),
        "LBRACE" => "`{`".to_string(),
        "RBRACE" => "`}`".to_string(),
        "LPAREN" => "`(`".to_string(),
        "RPAREN" => "`)`".to_string(),
        "LBRACK" => "`[`".to_string(),
        "RBRACK" => "`]`".to_string(),
        "SEMI" => "`;`".to_string(),
        "COLON" => "`:`".to_string(),
        "COMMA" => "`,`".to_string(),
        "DOT" => "`.`".to_string(),
        "EQ" => "`=`".to_string(),
        "AT" => "`@`".to_string(),
        "ELLIPSIS" => "`...`".to_string(),
        "OP" => "an operator".to_string(),
        "EOF" => "end of input".to_string(),
        other => other.to_string(),
    }
}

fn replace_kinds(message: &str) -> String {
    let mut out = String::new();
    let mut rest = message;
    while let Some(at) = rest.find("TokKind.") {
        out.push_str(&rest[..at]);
        rest = &rest[at + "TokKind.".len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        out.push_str(&kind_phrase(&rest[..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// A Python `repr` that occupies the whole string, unquoted. Escapes are undone
/// just enough to recover the token text the parser interpolated.
fn unquote_py(s: &str) -> Option<String> {
    let mut chars = s.chars();
    let quote = chars.next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                other => out.push(other),
            }
        } else if c == quote {
            return chars.next().is_none().then_some(out);
        } else {
            out.push(c);
        }
    }
    None
}

fn span_for(text: &str, offset: usize, message: &str) -> Span {
    if let Ok(tokens) = tokenize(text)
        && let Some(tok) = tokens.iter().find(|t| t.pos == offset)
    {
        return token_span(text, tok);
    }
    let len = text.chars().count();
    let offset = offset.min(len);
    let extra = if let Some(rest) = message
        .strip_prefix("int too large: ")
        .or_else(|| message.strip_prefix("hex literal has no digits: "))
    {
        rest.chars().count()
    } else if offset < len {
        1
    } else {
        0
    };
    Span {
        start: offset,
        end: (offset + extra).min(len.max(offset)),
    }
}

fn token_span(text: &str, tok: &Token) -> Span {
    let chars: Vec<char> = text.chars().collect();
    let end = match tok.kind {
        TokKind::Ident | TokKind::Op | TokKind::Ellipsis => {
            tok.pos + tok.text().map(|s| s.chars().count()).unwrap_or(1)
        }
        TokKind::Int => scan_int_end(&chars, tok.pos),
        TokKind::Str => scan_str_end(&chars, tok.pos),
        TokKind::Eof => tok.pos,
        _ => tok.pos + 1,
    };
    Span {
        start: tok.pos,
        end: end.min(chars.len().max(tok.pos)),
    }
}

fn scan_int_end(chars: &[char], start: usize) -> usize {
    let mut i = start;
    if i < chars.len() && chars[i] == '-' {
        i += 1;
    }
    if i + 1 < chars.len() && chars[i] == '0' && chars[i + 1].eq_ignore_ascii_case(&'x') {
        i += 2;
        while i < chars.len() && chars[i].is_ascii_hexdigit() {
            i += 1;
        }
        return i;
    }
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    i
}

fn scan_str_end(chars: &[char], start: usize) -> usize {
    let mut i = start + 1;
    while i < chars.len() && chars[i] != '"' {
        if chars[i] == '\\' && i + 1 < chars.len() {
            i += 2;
        } else {
            i += 1;
        }
    }
    if i < chars.len() { i + 1 } else { chars.len() }
}

fn found_text(text: &str, span: &Span) -> String {
    if span.end <= span.start {
        return "end of input".to_string();
    }
    let slice: String = text
        .chars()
        .skip(span.start)
        .take(span.end - span.start)
        .collect();
    if slice.is_empty() {
        "end of input".to_string()
    } else {
        format!("`{slice}`")
    }
}

// ---------------------------------------------------------------------------
// Locating a name use inside the declaration `where_` names. The AST has no
// spans, so the file is re-lexed and the use is the ident in a type/spread/
// packet position inside that declaration — not the first textual hit.
// ---------------------------------------------------------------------------

fn locate(text: &str, name: &str, where_: &str) -> Option<Span> {
    let tokens = tokenize(text).ok()?;
    if let Some((start, end)) = where_range(&tokens, where_)
        && let Some(idx) = (start..end).find(|&i| is_name_use(&tokens, i, name))
    {
        return Some(ident_span(&tokens[idx]));
    }
    if let Some(idx) = (0..tokens.len()).find(|&i| is_name_use(&tokens, i, name)) {
        return Some(ident_span(&tokens[idx]));
    }
    tokens
        .iter()
        .find(|t| t.kind == TokKind::Ident && t.text() == Some(name))
        .map(ident_span)
}

fn ident_span(tok: &Token) -> Span {
    let len = tok.text().map(|s| s.chars().count()).unwrap_or(1);
    Span {
        start: tok.pos,
        end: tok.pos + len,
    }
}

fn where_range(tokens: &[Token], where_: &str) -> Option<(usize, usize)> {
    if let Some(rest) = where_.strip_prefix("route ") {
        return route_range(tokens, rest);
    }
    if let Some(rest) = where_.strip_prefix("protocol ")
        && let Some((proto, frame)) = rest.split_once(" frame ")
    {
        return frame_range(tokens, proto, frame);
    }
    let (keyword, name) = decl_where(where_)?;
    let start = find_decl(tokens, keyword, name)?;
    Some((start, top_level_end(tokens, start)))
}

fn decl_where(where_: &str) -> Option<(&str, &str)> {
    for keyword in ["packet", "encoding", "enum", "protocol"] {
        if let Some(name) = where_
            .strip_prefix(keyword)
            .and_then(|s| s.strip_prefix(' '))
            && !name.is_empty()
            && !name.contains(' ')
        {
            return Some((keyword, name));
        }
    }
    None
}

fn find_decl(tokens: &[Token], keyword: &str, name: &str) -> Option<usize> {
    tokens.iter().enumerate().find_map(|(i, tok)| {
        (tok.text() == Some(keyword) && tokens.get(i + 1).and_then(|n| n.text()) == Some(name))
            .then_some(i)
    })
}

fn top_level_end(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0i32;
    let mut i = start + 1;
    while i < tokens.len() && tokens[i].kind != TokKind::Eof {
        match tokens[i].kind {
            TokKind::LBrace => depth += 1,
            TokKind::RBrace => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            TokKind::Ident if depth == 0 && is_decl_keyword(tokens[i].text()) => return i,
            _ => {}
        }
        i += 1;
    }
    i
}

fn is_decl_keyword(text: Option<&str>) -> bool {
    matches!(
        text,
        Some("import" | "encoding" | "enum" | "packet" | "protocol")
    )
}

fn frame_range(tokens: &[Token], proto: &str, frame: &str) -> Option<(usize, usize)> {
    let proto_at = find_decl(tokens, "protocol", proto)?;
    let proto_end = top_level_end(tokens, proto_at);
    let frame_at = (proto_at..proto_end).find(|&i| {
        tokens[i].text() == Some("frame") && tokens.get(i + 1).and_then(|t| t.text()) == Some(frame)
    })?;
    let brace = (frame_at..proto_end).find(|&i| tokens[i].kind == TokKind::LBrace)?;
    let mut depth = 0i32;
    for (i, tok) in tokens.iter().enumerate().take(proto_end).skip(brace) {
        match tok.kind {
            TokKind::LBrace => depth += 1,
            TokKind::RBrace => {
                depth -= 1;
                if depth == 0 {
                    return Some((frame_at, i + 1));
                }
            }
            _ => {}
        }
    }
    Some((frame_at, proto_end))
}

fn route_range(tokens: &[Token], rest: &str) -> Option<(usize, usize)> {
    let (head, id_txt) = rest.rsplit_once(' ')?;
    let (state, direction) = head.split_once('.')?;
    let hex = id_txt.strip_prefix("0x")?;
    let id = i128::from_str_radix(hex, 16).ok()?;
    let at = tokens.iter().enumerate().find_map(|(i, tok)| {
        let hit = tok.text() == Some("route")
            && tokens.get(i + 1).and_then(|t| t.text()) == Some(state)
            && tokens.get(i + 2).map(|t| t.kind) == Some(TokKind::Dot)
            && tokens.get(i + 3).and_then(|t| t.text()) == Some(direction)
            && matches!(
                tokens.get(i + 4),
                Some(t) if t.kind == TokKind::Int && matches!(&t.value, TokValue::Int(v) if *v == id)
            );
        hit.then_some(i)
    })?;
    Some((at, (at + 8).min(tokens.len())))
}

fn is_name_use(tokens: &[Token], idx: usize, name: &str) -> bool {
    let Some(tok) = tokens.get(idx) else {
        return false;
    };
    if tok.kind != TokKind::Ident || tok.text() != Some(name) {
        return false;
    }
    let Some(prev) = idx.checked_sub(1).and_then(|i| tokens.get(i)) else {
        return false;
    };
    match prev.kind {
        TokKind::Colon | TokKind::Ellipsis => true,
        TokKind::Op => prev.text() == Some("=>"),
        TokKind::Eq => brace_depth(tokens, idx) == 0,
        _ => false,
    }
}

fn brace_depth(tokens: &[Token], idx: usize) -> i32 {
    tokens
        .iter()
        .take(idx)
        .fold(0, |depth, tok| match tok.kind {
            TokKind::LBrace => depth + 1,
            TokKind::RBrace => depth - 1,
            _ => depth,
        })
}

fn nth_decl(text: &str, keyword: &str, name: &str, nth: usize) -> Option<Span> {
    let tokens = tokenize(text).ok()?;
    let mut seen = 0usize;
    for (i, tok) in tokens.iter().enumerate() {
        if tok.text() == Some(keyword) && tokens.get(i + 1).and_then(|t| t.text()) == Some(name) {
            if seen == nth {
                return Some(ident_span(&tokens[i + 1]));
            }
            seen += 1;
        }
    }
    None
}

fn decl_names(text: &str, keyword: &str) -> Vec<String> {
    let Ok(tokens) = tokenize(text) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for (i, tok) in tokens.iter().enumerate() {
        if tok.text() == Some(keyword)
            && let Some(name) = tokens.get(i + 1).and_then(|t| t.text())
        {
            names.push(name.to_string());
        }
    }
    names
}

/// Edit distance `<= 2` and strictly less than half the name's length
/// (`d * 2 < len`). Closest wins; ties break lexicographically.
fn suggest(name: &str, visible: &[String]) -> Option<String> {
    let len = name.chars().count();
    let mut best: Option<(usize, &str)> = None;
    for cand in visible {
        if cand == name {
            continue;
        }
        let distance = edit_distance(name, cand);
        if distance == 0 || distance > 2 || distance * 2 >= len {
            continue;
        }
        let take = match best {
            None => true,
            Some((best_d, best_n)) => {
                distance < best_d || (distance == best_d && cand.as_str() < best_n)
            }
        };
        if take {
            best = Some((distance, cand));
        }
    }
    best.map(|(_, s)| s.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != cb);
            current.push(substitute.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

fn render_one(d: &Diagnostic, sources: &SourceSet) -> String {
    let mut out = format!("error: {}", d.message);
    match &d.file {
        Some(file) if d.line == 0 => out.push_str(&format!("\n --> {file}")),
        Some(file) => out.push_str(&format!("\n --> {file}:{}:{}", d.line, d.column)),
        None if d.line > 0 => out.push_str(&format!("\n --> {}:{}", d.line, d.column)),
        None => {}
    }
    if d.line > 0
        && let Some(text) = d.file.as_deref().and_then(|f| sources.get(f))
    {
        push_snippet(&mut out, d, text);
    }
    if let Some(help) = &d.help {
        for line in help.lines() {
            out.push_str("\nhelp: ");
            out.push_str(line);
        }
    }
    out
}

fn push_snippet(out: &mut String, d: &Diagnostic, text: &str) {
    let src = line_text(text, d.line).replace('\t', " ");
    let width = d.line.max(d.end_line).to_string().len();
    let carets = "^".repeat(caret_len(d, &src));
    let pad = " ".repeat(d.column.saturating_sub(1));
    out.push('\n');
    out.push_str(&format!("{:width$} |", "", width = width));
    out.push('\n');
    out.push_str(&format!("{:>width$} | {src}", d.line, width = width));
    out.push('\n');
    out.push_str(&format!("{:width$} | {pad}{carets}", "", width = width));
}

fn caret_len(d: &Diagnostic, src_line: &str) -> usize {
    let len = src_line.chars().count();
    if d.end_line != d.line {
        return len.saturating_sub(d.column.saturating_sub(1)).max(1);
    }
    d.end_column.saturating_sub(d.column).max(1)
}
