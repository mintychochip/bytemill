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

impl Decl {
    /// Fill `refs`: parse the piece, take its one declaration, collect the names it references
    /// that are neither packet routes nor bare primitives, deduplicated and sorted.
    pub fn fill_refs(&mut self) -> Res<()> {
        let parsed = crate::parse(&self.text)
            .map_err(|e| format!("{} {}: {e}", self.kind.keyword().trim(), self.name))?;
        let mut schema = crate::ast::Schema::default();
        match self.kind {
            Kind::Encoding => schema.encodings = parsed.encodings,
            Kind::Packet => schema.packets = parsed.packets,
        }
        let mut names = std::collections::BTreeSet::new();
        for r in crate::refs::references(&schema) {
            if r.kind == crate::refs::RefKind::Packet
                || crate::primitives::is_bare_primitive(&r.name)
            {
                continue;
            }
            names.insert(r.name);
        }
        self.refs = names.into_iter().collect();
        Ok(())
    }
}

/// The identity id of one declaration within one version. Equal ids across all versions mean
/// the declaration and everything reachable from it is textually identical.
pub type Id = u32;

/// Per version: `(kind, name) -> id`. Ids are interned from exact string keys, so equal keys
/// give equal ids across every version and different keys never collide.
///
/// The key of a declaration outside any cycle is its `(kind, name, text, sorted ids of the
/// declarations it references)`. A strongly connected component (two or more members, or a
/// self reference) gets one component key from its members' `(kind, name, text)` and the
/// sorted ids of the outside declarations they reference; each member's key is `(component
/// id, kind, name)`. Components are keyed children first, so a child's id is final before any
/// parent's key is built.
pub fn identity(versions: &[Vec<Decl>]) -> Res<Vec<BTreeMap<(Kind, String), Id>>> {
    // Nodes by name across the whole run; ids interned in one shared table.
    let mut intern: BTreeMap<String, Id> = BTreeMap::new();
    let mut next: Id = 0;
    let mut out = Vec::with_capacity(versions.len());
    for decls in versions {
        // Tarjan's SCC, iterative. Nodes are (kind, name); edges are the declaration's refs
        // that exist in this version (external and std names are ignored: they do not affect
        // identity).
        let index: BTreeMap<(Kind, &str), usize> = decls
            .iter()
            .enumerate()
            .map(|(i, d)| ((d.kind, d.name.as_str()), i))
            .collect();
        let n = decls.len();
        let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, d) in decls.iter().enumerate() {
            for r in &d.refs {
                if let Some(&j) = index.get(&(d.kind, r.as_str())) {
                    // A declaration only references its own kind through a type ref? No: a
                    // packet references encodings. Resolve across both kinds: try the same
                    // kind first, then any kind (names are unique per kind, and an encoding
                    // and a packet never share a name in one version — the lint forbids it).
                    edges[i].push(j);
                } else {
                    let other = match d.kind {
                        Kind::Encoding => Kind::Packet,
                        Kind::Packet => Kind::Encoding,
                    };
                    if let Some(&j) = index.get(&(other, r.as_str())) {
                        edges[i].push(j);
                    }
                }
            }
        }

        // Iterative Tarjan.
        let mut on_stack = vec![false; n];
        let mut stack: Vec<usize> = Vec::new();
        let mut low = vec![0u32; n];
        let mut disc = vec![u32::MAX; n];
        let mut components: Vec<Vec<usize>> = Vec::new();
        let mut self_loop = vec![false; n];
        for start in 0..n {
            if disc[start] != u32::MAX {
                continue;
            }
            // Each frame: (node, next edge index to try).
            let mut call: Vec<(usize, usize)> = vec![(start, 0)];
            let mut counter: u32 = 0;
            while let Some((v, mut ei)) = call.pop() {
                if ei == 0 {
                    disc[v] = counter;
                    low[v] = counter;
                    counter += 1;
                    stack.push(v);
                    on_stack[v] = true;
                }
                let mut recurse = None;
                while let Some(&w) = edges[v].get(ei) {
                    if disc[w] == u32::MAX {
                        call.push((v, ei + 1));
                        recurse = Some(w);
                        break;
                    }
                    if on_stack[w] {
                        low[v] = low[v].min(disc[w]);
                        if w == v {
                            self_loop[v] = true;
                        }
                    }
                    ei += 1;
                }
                if let Some(w) = recurse {
                    call.push((w, 0));
                    continue;
                }
                // All edges done: close the frame.
                if low[v] == disc[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        comp.push(w);
                        if w == v {
                            break;
                        }
                    }
                    comp.sort_unstable();
                    components.push(comp);
                }
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }

        // Components in discovery order are children first (Tarjan pops a component only
        // after all components it can reach are popped). Key each component.
        let mut ids: Vec<Option<Id>> = vec![None; n];
        for comp in &components {
            if comp.len() == 1 && !self_loop[comp[0]] {
                let v = comp[0];
                let d = &decls[v];
                let mut refs: Vec<String> = Vec::new();
                for &w in &edges[v] {
                    refs.push(
                        ids[w]
                            .expect("children are keyed before their parents")
                            .to_string(),
                    );
                }
                refs.sort_unstable();
                let key = format!(
                    "d\x1f{}\x1f{}\x1f{}\x1f{}",
                    d.kind.keyword().trim(),
                    d.name,
                    d.text,
                    refs.join("\x1e")
                );
                let id = *intern.entry(key).or_insert_with(|| {
                    let id = next;
                    next += 1;
                    id
                });
                ids[v] = Some(id);
            } else {
                let mut members: Vec<(String, String, String)> = comp
                    .iter()
                    .map(|&v| {
                        (
                            decls[v].kind.keyword().trim().to_string(),
                            decls[v].name.clone(),
                            decls[v].text.clone(),
                        )
                    })
                    .collect();
                members.sort();
                let mut outside: Vec<String> = Vec::new();
                for &v in comp {
                    for &w in &edges[v] {
                        if !comp.contains(&w) {
                            outside.push(
                                ids[w]
                                    .expect("children are keyed before their parents")
                                    .to_string(),
                            );
                        }
                    }
                }
                outside.sort();
                outside.dedup();
                let ckey = format!(
                    "c\x1f{}\x1f{}",
                    members
                        .iter()
                        .map(|(k, n, t)| format!("{k}\x1e{n}\x1e{t}"))
                        .collect::<Vec<_>>()
                        .join("\x1d"),
                    outside.join("\x1e")
                );
                let cid = *intern.entry(ckey).or_insert_with(|| {
                    let id = next;
                    next += 1;
                    id
                });
                for &v in comp {
                    let d = &decls[v];
                    let key = format!(
                        "m\x1f{cid}\x1f{}\x1f{}",
                        d.kind.keyword().trim(),
                        d.name
                    );
                    let id = *intern.entry(key).or_insert_with(|| {
                        let id = next;
                        next += 1;
                        id
                    });
                    ids[v] = Some(id);
                }
            }
        }

        let mut map = BTreeMap::new();
        for (i, d) in decls.iter().enumerate() {
            map.insert(
                (d.kind, d.name.clone()),
                ids[i].expect("every node belongs to exactly one component"),
            );
        }
        out.push(map);
    }
    Ok(out)
}

/// The complete output tree: relative path (from the versions directory) -> file text. Keys
/// are `v1_13.bps` roots and `common/<set>.bps` shared files.
#[derive(Debug, Default)]
pub struct Layout {
    pub files: BTreeMap<String, String>,
}

/// The runs of consecutive manifest indices in a version set, e.g. `{0,1,4}` ->
/// `[[0,1],[4]]`.
fn runs_of(set: &[usize], last_index: usize) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut iter = set.iter().copied().peekable();
    while let Some(first) = iter.next() {
        let mut last = first;
        while last < last_index && iter.peek() == Some(&(last + 1)) {
            last += 1;
            iter.next();
        }
        runs.push((first, last));
    }
    runs
}

/// The piece name of one run: `from_<first>` when it reaches the last entry, `only_<first>`
/// for a single entry, else `<first>_to_<last>`. `<id>` is the release with `.` -> `_`.
fn run_name(run: (usize, usize), releases: &[&str], last_index: usize) -> String {
    let id = |i: usize| releases[i].replace('.', "_");
    let (first, last) = run;
    if last == last_index {
        format!("from_{}", id(first))
    } else if first == last {
        format!("only_{}", id(first))
    } else {
        format!("{}_to_{}", id(first), id(last))
    }
}

/// The file name of a version set: its runs' names joined with `+`.
fn set_name(set: &[usize], releases: &[&str], last_index: usize) -> String {
    let runs = runs_of(set, last_index);
    let names: Vec<String> = runs.iter().map(|&r| run_name(r, releases, last_index)).collect();
    names.join("+")
}

/// The `// Shared by: …` header line: each run reads `X (protocol N) onwards`, `X (protocol
/// N) only` or `X (protocol N) to Y (protocol M)`, joined with `, `.
fn shared_by_line(set: &[usize], releases: &[&str], protocols: &[u32], last_index: usize) -> String {
    let one = |i: usize| format!("{} (protocol {})", releases[i], protocols[i]);
    let parts: Vec<String> = runs_of(set, last_index)
        .iter()
        .map(|&(first, last)| {
            if last == last_index {
                format!("{} onwards", one(first))
            } else if first == last {
                format!("{} only", one(first))
            } else {
                format!("{} to {}", one(first), one(last))
            }
        })
        .collect();
    format!("// Shared by: {}.\n", parts.join(", "))
}

/// The `common/…` import path a file at depth uses for a shared file named `name`.
fn common_import(name: &str, from_root: bool) -> String {
    if from_root {
        format!("common/{name}.bps")
    } else {
        format!("../../common/{name}.bps")
    }
}

/// Assemble the layout. `splits[i]` and `ids[i]` belong to `entries[i]`, in manifest order.
/// Errors when two occurrences of one id carry different texts.
pub fn share(
    entries: &[super::versions::VersionEntry],
    splits: &[Split],
    ids: &[BTreeMap<(Kind, String), Id>],
) -> Res<Layout> {
    assert_eq!(entries.len(), splits.len());
    assert_eq!(entries.len(), ids.len());
    let releases: Vec<&str> = entries.iter().map(|e| e.release.as_str()).collect();
    let protocols: Vec<u32> = entries.iter().map(|e| e.protocol).collect();
    let last = releases.len().saturating_sub(1);

    // id -> (text, set of version indices). A second text for one id is an import failure.
    let mut by_id: BTreeMap<Id, (Kind, String, String, Vec<usize>)> = BTreeMap::new();
    for (vi, map) in ids.iter().enumerate() {
        for (key, id) in map {
            let d = splits[vi]
                .decls
                .iter()
                .find(|d| (d.kind, d.name.as_str()) == (key.0, key.1.as_str()))
                .expect("every id maps back to a declaration of its version");
            match by_id.get_mut(id) {
                Some((_, _, text, set)) => {
                    if *text != d.text {
                        return Err(format!(
                            "id {id} carries two different texts: {} and {}",
                            text.lines().next().unwrap_or_default(),
                            d.text.lines().next().unwrap_or_default()
                        ));
                    }
                    set.push(vi);
                }
                None => {
                    by_id.insert(
                        *id,
                        (d.kind, d.name.clone(), d.text.clone(), vec![vi]),
                    );
                }
            }
        }
    }

    // id -> refs (as ids, restricted to ids that exist in every version the id occurs in).
    // A declaration's references have sets that contain its own, so each referenced id is
    // present in every version of the set.
    let mut refs_of: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
    for (vi, map) in ids.iter().enumerate() {
        for d in &splits[vi].decls {
            let id = map[&(d.kind, d.name.clone())];
            let mut out_refs: Vec<Id> = Vec::new();
            for r in &d.refs {
                // The referenced name's id in this version; it must be in the set (or the
                // reference is external and ignored).
                let mut found = None;
                for other_kind in [d.kind, match d.kind {
                    Kind::Encoding => Kind::Packet,
                    Kind::Packet => Kind::Encoding,
                }] {
                    if let Some(other_id) = map.get(&(other_kind, r.clone())) {
                        found = Some(*other_id);
                        break;
                    }
                }
                if let Some(other_id) = found {
                    out_refs.push(other_id);
                }
            }
            out_refs.sort_unstable();
            out_refs.dedup();
            refs_of.entry(id).or_default().extend(out_refs);
        }
    }
    for v in refs_of.values_mut() {
        v.sort_unstable();
        v.dedup();
    }

    // Group ids by their version set.
    let mut by_set: BTreeMap<Vec<usize>, Vec<Id>> = BTreeMap::new();
    for (id, (_, _, _, set)) in &by_id {
        let mut set = set.clone();
        set.sort_unstable();
        by_set.entry(set).or_default().push(*id);
    }

    let mut layout = Layout::default();

    // Shared files: one per set. Widest set first (largest |set|, then lexicographic name)
    // decides the import order everywhere.
    let mut sets: Vec<(Vec<usize>, Vec<Id>, String)> = by_set
        .into_iter()
        .map(|(set, ids)| {
            let name = set_name(&set, &releases, last);
            (set, ids, name)
        })
        .collect();
    sets.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.2.cmp(&b.2)));

    // For each shared file: the ids it defines, the ids its declarations reference, and
    // whether that referenced id's file imports into it (a referenced id whose set contains
    // this file's set is defined in an earlier, wider file).
    let file_of: BTreeMap<Id, String> = sets
        .iter()
        .flat_map(|(_, ids, name)| ids.iter().map(move |id| (*id, name.clone())))
        .collect();

    for (set, set_ids, name) in &sets {
        let mut body = String::new();
        body.push_str(&shared_by_line(set, &releases, &protocols, last));
        body.push_str(
            "// GENERATED by `bpsc import-versions` from data/minecraft-data (PrismarineJS/minecraft-data).\n",
        );
        body.push_str("// Do not edit: change the importer or schemas/versions/manifest.json and regenerate.\n");
        body.push_str("\nimport \"../../std/integers.bps\"\nimport \"../../std/varint.bps\"\n");

        // Which referenced ids live in other shared files?
        let mut needed: BTreeSet<String> = BTreeSet::new();
        for id in set_ids {
            for r in &refs_of[id] {
                let other = &file_of[r];
                if other != name {
                    needed.insert(other.clone());
                }
            }
        }
        // Order: widest set first, then file name. `needed` holds names; recover widths.
        let mut imports: Vec<(usize, String)> = needed
            .into_iter()
            .map(|n| {
                let width = sets
                    .iter()
                    .find(|(_, _, name)| *name == n)
                    .map(|(set, _, _)| set.len())
                    .unwrap_or(0);
                (width, n)
            })
            .collect();
        imports.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, n) in imports {
            body.push_str(&format!("import \"{}\"\n", common_import(&n, false)));
        }
        body.push('\n');

        // Declarations: encodings before packets, each group sorted by name.
        let encs: BTreeMap<String, String> = set_ids
            .iter()
            .filter(|id| by_id[id].0 == Kind::Encoding)
            .map(|id| (by_id[id].1.clone(), by_id[id].2.clone()))
            .collect();
        let pkts: BTreeMap<String, String> = set_ids
            .iter()
            .filter(|id| by_id[id].0 == Kind::Packet)
            .map(|id| (by_id[id].1.clone(), by_id[id].2.clone()))
            .collect();
        let mut first = true;
        for text in encs.values() {
            if first && !encs.is_empty() {
                first = false;
            }
            body.push_str(text);
            body.push('\n');
        }
        if !pkts.is_empty() && !encs.is_empty() {
            // Keep the section banner the single-file files had.
            body.push_str("// --- packets ---\n\n");
        }
        for text in pkts.values() {
            body.push_str(text);
            body.push('\n');
        }
        layout.files.insert(format!("common/{name}.bps"), body);
    }

    // Roots: header, std imports, shared imports, protocol block.
    for (vi, s) in splits.iter().enumerate() {
        let mut root = String::new();
        root.push_str(&s.header);
        let std_imports: Vec<&String> = s.imports.iter().collect();
        for p in &std_imports {
            root.push_str(&format!("import \"{p}\"\n"));
        }
        // The shared files whose set contains this version: widest first, then name.
        let mut mine: Vec<(usize, String)> = sets
            .iter()
            .filter(|(set, _, _)| set.binary_search(&vi).is_ok())
            .map(|(set, _, name)| (set.len(), name.clone()))
            .collect();
        mine.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, n) in &mine {
            root.push_str(&format!("import \"{}\"\n", common_import(n, true)));
        }
        root.push('\n');
        root.push_str(&s.protocol_block);
        layout
            .files
            .insert(entries[vi].file_name(), root);
    }

    Ok(layout)
}

use std::collections::BTreeSet;

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(kind: Kind, name: &str, body: &str) -> Decl {
        let mut d = Decl {
            kind,
            name: name.into(),
            text: format!("{body}\n"),
            refs: Vec::new(),
        };
        d.fill_refs().unwrap();
        d
    }

    #[test]
    fn identical_declarations_get_one_id_and_a_change_splits_the_dependants() {
        // Two versions; `wrapper` references `inner` in both, but `inner` differs. All four
        // ids are distinct: `wrapper` differs across versions because its referenced id
        // differs even though its text is equal.
        let v0 = vec![
            decl(Kind::Encoding, "inner", "encoding inner = {\n    a: u8\n}"),
            decl(
                Kind::Encoding,
                "wrapper",
                "encoding wrapper = {\n    x: inner\n}",
            ),
        ];
        let v1 = vec![
            decl(Kind::Encoding, "inner", "encoding inner = {\n    a: u16be\n}"),
            decl(
                Kind::Encoding,
                "wrapper",
                "encoding wrapper = {\n    x: inner\n}",
            ),
        ];
        let ids = identity(&[v0.clone(), v1.clone()]).unwrap();
        let w0 = ids[0][&(Kind::Encoding, "wrapper".into())];
        let w1 = ids[1][&(Kind::Encoding, "wrapper".into())];
        assert_ne!(w0, w1);
        let i0 = ids[0][&(Kind::Encoding, "inner".into())];
        let i1 = ids[1][&(Kind::Encoding, "inner".into())];
        assert_ne!(i0, i1);
    }

    #[test]
    fn a_recursive_group_shares_as_a_unit() {
        // `node` references `entry`, `entry` references `node`: one SCC. Versions 0 and 1 are
        // identical; version 2 changes `node`, so both members move.
        let mk = |long: bool| -> Vec<Decl> {
            vec![
                decl(
                    Kind::Encoding,
                    "node",
                    if long {
                        "encoding node = {\n    tag: u8\n    next: entry\n}"
                    } else {
                        "encoding node = {\n    next: entry\n}"
                    },
                ),
                decl(
                    Kind::Encoding,
                    "entry",
                    "encoding entry = {\n    value: node\n}",
                ),
            ]
        };
        let ids = identity(&[mk(false), mk(false), mk(true)]).unwrap();
        assert_eq!(
            ids[0][&(Kind::Encoding, "node".into())],
            ids[1][&(Kind::Encoding, "node".into())]
        );
        assert_eq!(
            ids[0][&(Kind::Encoding, "entry".into())],
            ids[1][&(Kind::Encoding, "entry".into())]
        );
        assert_ne!(
            ids[0][&(Kind::Encoding, "entry".into())],
            ids[2][&(Kind::Encoding, "entry".into())]
        );
        assert_ne!(
            ids[0][&(Kind::Encoding, "node".into())],
            ids[2][&(Kind::Encoding, "node".into())]
        );
    }

    #[test]
    fn a_packet_referencing_a_changed_encoding_is_not_identical() {
        let v0 = vec![
            decl(Kind::Encoding, "slot", "encoding slot = {\n    a: u8\n}"),
            decl(
                Kind::Packet,
                "set_slot",
                "packet set_slot {\n    s: slot\n}",
            ),
        ];
        let v1 = vec![
            decl(Kind::Encoding, "slot", "encoding slot = {\n    a: i32be\n}"),
            decl(
                Kind::Packet,
                "set_slot",
                "packet set_slot {\n    s: slot\n}",
            ),
        ];
        let ids = identity(&[v0, v1]).unwrap();
        assert_ne!(
            ids[0][&(Kind::Packet, "set_slot".into())],
            ids[1][&(Kind::Packet, "set_slot".into())]
        );
    }

    #[test]
    fn a_name_absent_from_its_own_version_does_not_affect_identity() {
        // `wrapper` references `missing`, declared nowhere: external, ignored.
        let v0 = vec![decl(
            Kind::Encoding,
            "wrapper",
            "encoding wrapper = {\n    x: missing\n}",
        )];
        let ids = identity(&[v0.clone(), v0]).unwrap();
        assert_eq!(
            ids[0][&(Kind::Encoding, "wrapper".into())],
            ids[1][&(Kind::Encoding, "wrapper".into())]
        );
    }

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

    // -- layout ------------------------------------------------------------------

    fn entry(release: &str, protocol: u32) -> super::super::versions::VersionEntry {
        super::super::versions::VersionEntry {
            protocol,
            release: release.into(),
            also: Vec::new(),
            data: format!("pc/{release}"),
            patches: None,
            note: None,
        }
    }

    /// Wrap decls into a splittable single-file text.
    fn render_of(decls: &[Decl], release: &str, protocol: u32) -> String {
        let mut out = format!("// header {release}\n\nimport \"../std/integers.bps\"\n\n");
        for d in decls {
            out.push_str(&d.text);
            out.push('\n');
        }
        out.push_str(&format!(
            "protocol minecraft @ v{}_x {{\n    id = {protocol}\n}}\n",
            release.replace('.', "_")
        ));
        out
    }

    fn layout_for(entries: &[super::super::versions::VersionEntry], versions: &[Vec<Decl>]) -> super::Layout {
        let mut splits = Vec::new();
        for (i, v) in versions.iter().enumerate() {
            let text = render_of(v, &entries[i].release, entries[i].protocol);
            let mut s = split(&text).unwrap();
            for d in &mut s.decls {
                d.fill_refs().unwrap();
            }
            splits.push(s);
        }
        let ids = identity(versions).unwrap();
        share(entries, &splits, &ids).unwrap()
    }

    #[test]
    fn contiguous_and_non_contiguous_sets_get_the_spec_names() {
        // Three versions: 0 and 1 share `a`; 0 and 2 share `b`.
        let mk = |a: u8, b: u8| -> Vec<Decl> {
            let width = |w: u8| if w == 0 { "u8" } else { "u16be" };
            vec![
                decl(Kind::Encoding, "a", &format!("encoding a = {{\n    x: {}\n}}", width(a))),
                decl(Kind::Encoding, "b", &format!("encoding b = {{\n    x: {}\n}}", width(b))),
            ]
        };
        let entries = vec![entry("1.13", 393), entry("1.13.1", 401), entry("1.13.2", 404)];
        let versions = vec![mk(0, 0), mk(0, 1), mk(1, 0)];
        let layout = layout_for(&entries, &versions);
        // `a` occurs in versions {0,1}: one run -> `1_13_to_1_13_1.bps`.
        // `b` occurs in versions {0,2}: two runs; the second reaches the last entry ->
        // `only_1_13+from_1_13_2.bps`. The changed variants make two more singleton files.
        assert!(
            layout.files.contains_key("common/1_13_to_1_13_1.bps"),
            "{:?}",
            layout.files.keys().collect::<Vec<_>>()
        );
        assert!(
            layout
                .files
                .contains_key("common/only_1_13+from_1_13_2.bps"),
            "{:?}",
            layout.files.keys().collect::<Vec<_>>()
        );
        assert!(layout.files.contains_key("common/only_1_13_1.bps"));
        assert!(layout.files.contains_key("common/from_1_13_2.bps"));
        for r in ["v1_13.bps", "v1_13_1.bps", "v1_13_2.bps"] {
            assert!(layout.files.contains_key(r));
        }
    }

    #[test]
    fn a_set_reaching_the_last_entry_has_no_end_so_appending_renames_nothing() {
        let mk = |a: u8| -> Vec<Decl> {
            let width = if a == 0 { "u8" } else { "u16be" };
            vec![decl(
                Kind::Encoding,
                "a",
                &format!("encoding a = {{\n    x: {width}\n}}"),
            )]
        };
        let entries3 = vec![entry("1.13", 393), entry("1.13.1", 401), entry("1.13.2", 404)];
        let versions3 = vec![mk(0), mk(0), mk(0)];
        let l3 = layout_for(&entries3, &versions3);
        assert!(l3.files.contains_key("common/from_1_13.bps"), "{:?}", l3.files.keys().collect::<Vec<_>>());
        let entries4 = vec![
            entry("1.13", 393),
            entry("1.13.1", 401),
            entry("1.13.2", 404),
            entry("1.14", 477),
        ];
        let versions4 = vec![mk(0), mk(0), mk(0), mk(0)];
        let l4 = layout_for(&entries4, &versions4);
        assert!(l4.files.contains_key("common/from_1_13.bps"), "{:?}", l4.files.keys().collect::<Vec<_>>());
    }

    #[test]
    fn shared_files_import_what_they_reference_and_the_header_lists_the_runs() {
        // `a` (versions {0}) references `b` (versions {0,1}): a's file imports b's file.
        let mk = |a: u8| -> Vec<Decl> {
            let width = if a == 0 { "u8" } else { "u16be" };
            vec![
                decl(
                    Kind::Encoding,
                    "b",
                    "encoding b = {\n    x: u8\n}",
                ),
                decl(
                    Kind::Encoding,
                    "a",
                    &format!("encoding a = {{\n    x: b\n    y: {}\n}}", width),
                ),
            ]
        };
        let entries = vec![entry("1.13", 393), entry("1.13.1", 401)];
        let versions = vec![mk(0), mk(1)];
        let layout = layout_for(&entries, &versions);
        let a_file = layout
            .files
            .get("common/only_1_13.bps")
            .expect("a is only in version 0");
        assert!(
            a_file.starts_with("// Shared by: 1.13 (protocol 393) only.\n"),
            "{a_file}"
        );
        assert!(a_file.contains("// GENERATED by `bpsc import-versions`"));
        assert!(a_file.contains("import \"../../std/integers.bps\""));
        assert!(a_file.contains("import \"../../std/varint.bps\""));
        // a references b: the shared file holding b is imported.
        assert!(
            a_file.contains("import \"../../common/from_1_13.bps\""),
            "{a_file}"
        );
        let b_file = layout
            .files
            .get("common/from_1_13.bps")
            .expect("b is in both versions");
        assert!(
            b_file.starts_with("// Shared by: 1.13 (protocol 393) onwards.\n"),
            "{b_file}"
        );
    }

    #[test]
    fn roots_import_the_files_defining_their_declarations_then_the_protocol_block() {
        // `a` is identical in both versions (one shared file); `b` differs (two singletons).
        let mk = |_a: u8, b: u8| -> Vec<Decl> {
            let width = |w: u8| if w == 0 { "u8" } else { "u16be" };
            vec![
                decl(Kind::Encoding, "a", "encoding a = {\n    x: u8\n}"),
                decl(Kind::Encoding, "b", &format!("encoding b = {{\n    x: {}\n}}", width(b))),
            ]
        };
        let entries = vec![entry("1.13", 393), entry("1.13.1", 401)];
        let versions = vec![mk(0, 0), mk(0, 1)];
        let layout = layout_for(&entries, &versions);
        let root = &layout.files["v1_13.bps"];
        // The header, then the two std imports, then the shared file's import, then the block.
        assert!(root.starts_with("// header 1.13\n\n"), "{root}");
        assert!(root.contains("import \"../std/integers.bps\""));
        assert!(root.contains("import \"common/from_1_13.bps\""), "{root}");
        let block_at = root.find("protocol minecraft").unwrap();
        let imports_end = root.rfind("import \"").unwrap();
        assert!(imports_end < block_at);
        assert!(root[imports_end..block_at].contains("common/"));
    }

    #[test]
    fn two_occurrences_of_one_id_with_different_texts_are_an_error() {
        // Hand-build ids maps that claim equal ids for different texts.
        let entries = vec![entry("1.13", 393), entry("1.13.1", 401)];
        let versions = vec![
            vec![decl(Kind::Encoding, "a", "encoding a = {\n    x: u8\n}")],
            vec![decl(Kind::Encoding, "a", "encoding a = {\n    x: u16be\n}")],
        ];
        let mut ids = identity(&versions).unwrap();
        // Overwrite: claim v1's `a` has v0's id.
        let id0 = ids[0][&(Kind::Encoding, "a".to_string())];
        ids[1].insert((Kind::Encoding, "a".to_string()), id0);
        let mut splits = Vec::new();
        for (i, v) in versions.iter().enumerate() {
            let text = render_of(v, &entries[i].release, entries[i].protocol);
            let mut s = split(&text).unwrap();
            for d in &mut s.decls {
                d.fill_refs().unwrap();
            }
            splits.push(s);
        }
        assert!(share(&entries, &splits, &ids).is_err());
    }

    #[test]
    fn a_recursive_group_shares_as_a_file_unit() {
        // `node` <-> `entry` cycle; versions 0,1 share both, version 2 differs: the two
        // members land in the same file per set.
        let mk = |long: bool| -> Vec<Decl> {
            vec![
                decl(
                    Kind::Encoding,
                    "node",
                    if long {
                        "encoding node = {\n    tag: u8\n    next: entry\n}"
                    } else {
                        "encoding node = {\n    next: entry\n}"
                    },
                ),
                decl(
                    Kind::Encoding,
                    "entry",
                    "encoding entry = {\n    value: node\n}",
                ),
            ]
        };
        let entries = vec![entry("1.13", 393), entry("1.13.1", 401), entry("1.13.2", 404)];
        let versions = vec![mk(false), mk(false), mk(true)];
        let layout = layout_for(&entries, &versions);
        let shared = layout
            .files
            .get("common/1_13_to_1_13_1.bps")
            .unwrap_or_else(|| panic!("keys: {:?}", layout.files.keys().collect::<Vec<_>>()));
        assert!(shared.contains("encoding node"));
        assert!(shared.contains("encoding entry"));
        // The recursive group's file imports nothing for the cycle itself.
        assert!(!shared.contains("import \"common/"));
    }
}
