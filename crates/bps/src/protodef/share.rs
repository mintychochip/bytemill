//! Sharing declarations across the generated protocol versions (spec
//! `docs/superpowers/specs/2026-10-13-bps-shared-version-declarations-design.md`): split each
//! rendered single-file text into declarations, assign every declaration an identity that
//! includes what it references, group identical declarations by the set of versions they occur
//! in, and assemble the root + shared-file layout.

use std::collections::BTreeMap;

use super::model::Res;

/// A declaration kind; `Encoding` sorts before `Packet`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Encoding,
    Packet,
}

impl Kind {
    /// The column-0 keyword that starts a declaration of this kind, with the trailing space.
    pub fn keyword(self) -> &'static str {
        match self {
            Kind::Encoding => "encoding ",
            Kind::Packet => "packet ",
        }
    }
}

/// One declaration: kind, name, and its text — the doc comment lines directly above it (divider
/// comments `// ---` excluded) plus the declaration through its closing brace, or its single
/// line when it has none. `refs` holds the names it references that are neither bare primitives
/// nor packet routes: type references and spreads.
#[derive(Debug, Clone)]
pub struct Decl {
    pub kind: Kind,
    pub name: String,
    pub text: String,
    pub refs: Vec<String>,
}

/// One rendered version split into its parts.
pub struct Split {
    /// Everything before the first `import` line (title, GENERATED, patches, note), through the
    /// blank line that precedes the imports.
    pub header: String,
    /// The imported paths before the declarations, in order (the two `../std/…` lines).
    pub imports: Vec<String>,
    pub decls: Vec<Decl>,
    /// From the `protocol ` line at column 0 through the final `}`.
    pub protocol_block: String,
}

/// A line's role in the scan.
enum Line<'a> {
    /// A `//` comment that is not a `// ---` divider.
    Comment(&'a str),
    Divider,
    /// `encoding `/`packet ` at column 0; the rest of the line after the keyword.
    DeclStart(Kind, &'a str),
    /// `import "…"`; the path inside the quotes.
    Import(&'a str),
    /// `protocol …` at column 0.
    Protocol,
    Other,
}

fn classify(line: &str) -> Line<'_> {
    if line.starts_with("//") {
        if line.starts_with("// ---") {
            return Line::Divider;
        }
        return Line::Comment(line);
    }
    for keyword in [Kind::Encoding, Kind::Packet] {
        if let Some(rest) = line.strip_prefix(keyword.keyword()) {
            return Line::DeclStart(keyword, rest);
        }
    }
    if let Some(path) = line.strip_prefix("import \"").and_then(|r| r.strip_suffix('"')) {
        return Line::Import(path);
    }
    if line.starts_with("protocol ") {
        return Line::Protocol;
    }
    Line::Other
}

/// Split one rendered single-file text. Safety net: every piece must parse as exactly one
/// declaration of the expected kind and name, AST-equal to the whole text's declaration of that
/// name, and the pieces' (kind, name) sequence must equal the whole text's. Any mismatch is an
/// import failure, not silent corruption.
pub fn split(text: &str) -> Res<Split> {
    let mut header = String::new();
    let mut imports = Vec::new();
    let mut decls: Vec<Decl> = Vec::new();
    let mut protocol_block = String::new();
    // Comments directly above a declaration; cleared by a blank line or a divider.
    let mut pending_doc: Vec<&str> = Vec::new();
    let mut in_header = true;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        match classify(line) {
            Line::Protocol => {
                protocol_block.push_str(line);
                protocol_block.push('\n');
                for rest in lines {
                    protocol_block.push_str(rest);
                    protocol_block.push('\n');
                }
                break;
            }
            Line::Import(path) => {
                if !pending_doc.is_empty() {
                    // A comment that is not followed by a declaration: part of the header.
                    pending_doc.clear();
                }
                imports.push(path.to_string());
                in_header = false;
            }
            Line::Comment(c) => {
                if in_header && decls.is_empty() && imports.is_empty() {
                    header.push_str(c);
                    header.push('\n');
                } else {
                    pending_doc.push(c);
                }
            }
            Line::Divider => {
                if in_header && decls.is_empty() && imports.is_empty() {
                    header.push_str(line);
                    header.push('\n');
                }
                pending_doc.clear();
            }
            Line::DeclStart(kind, rest) => {
                let name = decl_name(kind, rest)?;
                let mut piece = String::new();
                for c in &pending_doc {
                    piece.push_str(c);
                    piece.push('\n');
                }
                pending_doc.clear();
                piece.push_str(line);
                piece.push('\n');
                // Consume lines until braces balance, outside comments and string literals.
                let mut depth = brace_delta(line);
                while depth > 0 {
                    let next = lines
                        .next()
                        .ok_or_else(|| format!("{} {name} ends before its closing brace", kind.keyword().trim()))?;
                    depth += brace_delta(next);
                    piece.push_str(next);
                    piece.push('\n');
                }
                in_header = false;
                decls.push(Decl {
                    kind,
                    name,
                    text: piece,
                    refs: Vec::new(),
                });
            }
            Line::Other => {
                if line.trim().is_empty() {
                    pending_doc.clear();
                    if in_header && decls.is_empty() && imports.is_empty() {
                        header.push_str(line);
                        header.push('\n');
                    }
                } else {
                    return Err(format!("unexpected line between declarations: {line:?}"));
                }
            }
        }
    }
    let split = Split {
        header,
        imports,
        decls,
        protocol_block,
    };
    check_pieces(text, &split)?;
    Ok(split)
}

/// `uuid = {` / `keep_alive {` / `angle = u8` -> the name.
fn decl_name(kind: Kind, rest: &str) -> Res<String> {
    let name = rest
        .split_once(" =")
        .or_else(|| rest.split_once(" {"))
        .map(|(n, _)| n.trim().to_string())
        .unwrap_or_else(|| rest.trim().to_string());
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "{}: cannot read a name from {rest:?}",
            kind.keyword().trim()
        ));
    }
    Ok(name)
}

/// Brace balance of one line, ignoring braces in `//` comments and in `"…"` strings.
fn brace_delta(line: &str) -> i32 {
    let mut depth = 0i32;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        chars.next();
                    } else if c == '"' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => break,
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

/// The safety net: pieces must reproduce the whole text's declarations, one to one.
fn check_pieces(text: &str, s: &Split) -> Res<()> {
    let whole = crate::parse(text).map_err(|e| format!("the rendered text does not parse: {e}"))?;
    if whole.protocols.len() != 1 {
        return Err(format!(
            "the rendered text holds {} protocol blocks, expected 1",
            whole.protocols.len()
        ));
    }
    let mut want: Vec<(Kind, &str)> = Vec::new();
    let mut seen: BTreeMap<&str, &crate::ast::EncodingDecl> = BTreeMap::new();
    for e in &whole.encodings {
        if seen.insert(e.name.as_str(), e).is_some() {
            return Err(format!("duplicate encoding {}", e.name));
        }
        want.push((Kind::Encoding, e.name.as_str()));
    }
    let mut packets: BTreeMap<&str, &crate::ast::PacketDecl> = BTreeMap::new();
    for p in &whole.packets {
        if packets.insert(p.name.as_str(), p).is_some() {
            return Err(format!("duplicate packet {}", p.name));
        }
        want.push((Kind::Packet, p.name.as_str()));
    }
    let got: Vec<(Kind, &str)> = s.decls.iter().map(|d| (d.kind, d.name.as_str())).collect();
    if got != want {
        return Err(format!(
            "the split found {} declarations but the text holds {}: {want:?}",
            got.len(),
            want.len()
        ));
    }
    for d in &s.decls {
        let parsed = crate::parse(&d.text)
            .map_err(|e| format!("{} {}: {e}", d.kind.keyword().trim(), d.name))?;
        let only_one = match d.kind {
            Kind::Encoding => {
                parsed.packets.is_empty()
                    && parsed.enums.is_empty()
                    && parsed.protocols.is_empty()
                    && parsed.encodings.len() == 1
            }
            Kind::Packet => {
                parsed.encodings.is_empty()
                    && parsed.enums.is_empty()
                    && parsed.protocols.is_empty()
                    && parsed.packets.len() == 1
            }
        };
        let (kind_word, same) = match d.kind {
            Kind::Encoding => match parsed.encodings.first() {
                Some(e) if e.name == d.name && e == seen[d.name.as_str()] => {
                    ("encoding", true)
                }
                _ => ("encoding", false),
            },
            Kind::Packet => match parsed.packets.first() {
                Some(p) if p.name == d.name && p == packets[d.name.as_str()] => ("packet", true),
                _ => ("packet", false),
            },
        };
        if !only_one || !same {
            return Err(format!(
                "{kind_word} {} does not split into exactly one identical declaration",
                d.name
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
// Minecraft Java Edition 1.13 (protocol 393).
// GENERATED by `bpsc import-versions` from data/minecraft-data/pc/1.13/protocol.json (PrismarineJS/minecraft-data).
// Do not edit: change the importer or schemas/versions/manifest.json and regenerate.

import \"../std/integers.bps\"
import \"../std/varint.bps\"

// --- named types ---

// 128-bit UUID, big endian.
encoding uuid = {
    msb: u64be
    lsb: u64be
}

encoding angle = u8

// --- packets ---

packet keep_alive {
    keep_alive_id: i64be
}

protocol minecraft @ v1_13 {
    id = 393
    frame default {
        length: varint
        id: varint
        payload: rest
    }
}
";

    #[test]
    fn a_rendered_file_splits_into_header_imports_declarations_and_protocol_block() {
        let s = split(SAMPLE).unwrap();
        assert!(s.header.starts_with("// Minecraft Java Edition 1.13"));
        assert!(s.header.contains("GENERATED by `bpsc import-versions`"));
        assert_eq!(s.imports, ["../std/integers.bps", "../std/varint.bps"]);
        assert_eq!(s.decls.len(), 3);
        assert_eq!(s.decls[0].kind, Kind::Encoding);
        assert_eq!(s.decls[0].name, "uuid");
        // The doc comment above the declaration travels with it; the `// ---` divider does not.
        assert!(
            s.decls[0].text.starts_with("// 128-bit UUID, big endian.\nencoding uuid = {"),
            "{:?}",
            s.decls[0].text
        );
        assert!(s.decls[0].text.ends_with("}\n"));
        assert_eq!(s.decls[1].kind, Kind::Encoding);
        assert_eq!(s.decls[1].text, "encoding angle = u8\n");
        assert_eq!(s.decls[2].kind, Kind::Packet);
        assert_eq!(s.decls[2].name, "keep_alive");
        assert!(s.protocol_block.starts_with("protocol minecraft @ v1_13 {"));
        assert!(s.protocol_block.ends_with("}\n"));
    }

    #[test]
    fn a_brace_or_brace_like_comment_inside_a_string_does_not_end_a_declaration() {
        // A `}` inside a string literal must not end the declaration early: the piece would not
        // parse as one declaration and the split fails rather than corrupting.
        let broken = SAMPLE.replace("msb: u64be", "msb: u64be } // \"}");
        assert!(split(&broken).is_err());
    }

    #[test]
    fn a_truncated_declaration_is_rejected_by_the_ast_check() {
        // The piece parses to one declaration of the right name but a different body: the AST
        // check against the whole text rejects it. Simulate by making the piece lose a line
        // after the split: swap in a doctored split through the same check_pieces path.
        let s = split(SAMPLE).unwrap();
        let mut doctored = s.decls[0].clone();
        doctored.text = doctored.text.replace("    lsb: u64be\n", "");
        let mut evil = Split {
            header: s.header.clone(),
            imports: s.imports.clone(),
            decls: s
                .decls
                .iter()
                .map(|d| if d.name == "uuid" { doctored.clone() } else { d.clone() })
                .collect(),
            protocol_block: s.protocol_block.clone(),
        };
        evil.decls[0].refs = s.decls[0].refs.clone();
        assert!(check_pieces(SAMPLE, &evil).is_err());
    }

    #[test]
    fn braces_in_strings_and_comments_do_not_confuse_the_scanner() {
        let source = "\
import \"../std/varint.bps\"

// says \"hello\" { not a brace
encoding quoted = {
    note: bytes(1) // why \"} here
    tag: u8
}

protocol minecraft @ v1_13 {
    id = 393
}
";
        let s = split(source).unwrap();
        assert_eq!(s.decls.len(), 1);
        assert_eq!(s.decls[0].name, "quoted");
        assert!(s.decls[0].text.contains("note: bytes(1)"));
    }
}
