//! The wiki route scraper, porting `scripts/scrape_wiki_routes.py`: the packet id tables of
//! minecraft.wiki's "Java Edition protocol/Packets" page become a `.bps` route file, a stub
//! `packet` for every routed packet no hand-written file declares plus one `route` per row.
//!
//! The Python version reads the page with three regexes. This crate has no regex dependency,
//! so [`parse_tables`] scans for the same three patterns by hand. Each scanner quotes the
//! regex it replaces; the only places it is narrower are ASCII-only `\d` and an id that does
//! not fit in `u64`, neither of which the wiki produces.

use crate::parser::parse;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const WIKI_URL: &str = "https://minecraft.wiki/w/Java_Edition_protocol/Packets";

const PROTOC_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

const STATE_MAP: [(&str, &str); 5] = [
    ("Handshaking", "handshaking"),
    ("Status", "status"),
    ("Login", "login"),
    ("Configuration", "configuration"),
    ("Play", "play"),
];

/// Official Mojang names -> hand-maintained BPS packet names (richer field layouts).
const PACKET_ALIASES: [(&str, &str); 1] = [("intention", "handshake")];

/// Packets defined with full layouts in minecraft_26_3.bps (no stub generated).
const MANUAL_PACKETS: [&str; 9] = [
    "handshake",
    "legacy_server_list_ping",
    "status_request",
    "status_response",
    "ping_request",
    "pong_response",
    "login_start",
    "encryption_response",
    "keep_alive",
];

/// Routes whose official name needs a different BPS packet: a serverbound packet that
/// collides with a clientbound one (the hand-transcribed declarations use a `_serverbound`
/// suffix), and packets whose wire form differs between states.
const SERVERBOUND_ALIASES: [((&str, &str, &str), &str); 5] = [
    (("play", "serverbound", "change_difficulty"), "change_difficulty_serverbound"),
    (("play", "serverbound", "container_close"), "container_close_serverbound"),
    (("play", "serverbound", "move_vehicle"), "move_vehicle_serverbound"),
    (("play", "serverbound", "player_abilities"), "player_abilities_serverbound"),
    // Vanilla's configuration codec writes the dialog without the holder id play uses.
    (("configuration", "clientbound", "show_dialog"), "show_dialog_configuration"),
];

/// One row of a wiki packet table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRow {
    pub state: String,
    pub direction: String,
    pub packet_id: u64,
    pub official_name: String,
}

/// `curl -sL` of the wiki page.
pub fn fetch_html() -> Result<String, String> {
    let out = Command::new("curl")
        .args(["-sL", WIKI_URL])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exited with {}", out.status));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("the wiki page is not UTF-8: {e}"))
}

/// Python's `\w` for `str` patterns: letters, digits and underscore.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Python's `str.isspace` (regex `\s`, `str.strip`): Unicode White_Space plus U+001C..U+001F.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

/// `from` or the first position after it where `keep` fails.
fn skip_while(s: &str, from: usize, keep: impl Fn(char) -> bool) -> usize {
    s[from..].char_indices().find(|&(_, c)| !keep(c)).map_or(s.len(), |(i, _)| from + i)
}

/// The position after `literal` when `s` has it at `at`.
fn expect(s: &str, at: usize, literal: &str) -> Option<usize> {
    s[at..].starts_with(literal).then_some(at + literal.len())
}

/// A `<th colspan="4">STATE DIRECTION packets` heading.
struct Heading {
    start: usize,
    end: usize,
    state: String,
    direction: &'static str,
}

/// `<th colspan="4">(\w+) (clientbound|serverbound) packets` (IGNORECASE): the first
/// heading that starts at or after `from`. `lower` is `html` ASCII-lowercased, so byte
/// offsets agree.
fn find_heading(html: &str, lower: &str, from: usize) -> Option<Heading> {
    const OPEN: &str = "<th colspan=\"4\">";
    const SUFFIX: &str = " packets";
    let mut at = from;
    while let Some(rel) = lower[at..].find(OPEN) {
        let start = at + rel;
        let word_start = start + OPEN.len();
        let word_end = skip_while(html, word_start, is_word);
        if word_end > word_start {
            if let Some(after_space) = lower[word_end..].strip_prefix(' ') {
                for direction in ["clientbound", "serverbound"] {
                    let Some(tail) = after_space.strip_prefix(direction) else { continue };
                    if tail.starts_with(SUFFIX) {
                        let word = &html[word_start..word_end];
                        let state = STATE_MAP
                            .iter()
                            .find(|(name, _)| *name == word)
                            .map_or_else(|| word.to_lowercase(), |(_, state)| (*state).to_string());
                        let end = word_end + 1 + direction.len() + SUFFIX.len();
                        return Some(Heading { start, end, state, direction });
                    }
                }
            }
        }
        at = start + 1;
    }
    None
}

/// `<tr[^>]*>(.*?)</tr>` (IGNORECASE, DOTALL): the byte range of each row body.
fn row_bodies(lower: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(rel) = lower[at..].find("<tr") {
        let tag_end = at + rel + "<tr".len();
        let Some(gt) = lower[tag_end..].find('>') else { break };
        let body_start = tag_end + gt + 1;
        let Some(close) = lower[body_start..].find("</tr>") else { break };
        out.push((body_start, body_start + close));
        at = body_start + close + "</tr>".len();
    }
    out
}

/// The row's packet id and official name when the pattern matches at `from`, just after a
/// `<td>`: `(\d+)</td>\s*<td>0x([0-9A-Fa-f]+)\s*</td>.*?<td><code>([^<]+)</code>`.
fn row_at(body: &str, lower: &str, from: usize) -> Option<(u64, String)> {
    let digits_end = skip_while(lower, from, |c| c.is_ascii_digit());
    if digits_end == from {
        return None;
    }
    let packet_id: u64 = body[from..digits_end].parse().ok()?;
    let mut i = expect(lower, digits_end, "</td>")?;
    i = skip_while(lower, i, is_py_space);
    i = expect(lower, i, "<td>0x")?;
    let hex_end = skip_while(lower, i, |c| c.is_ascii_hexdigit());
    if hex_end == i {
        return None;
    }
    i = skip_while(lower, hex_end, is_py_space);
    i = expect(lower, i, "</td>")?;
    // `.*?` then the code cell: the first `<td><code>` whose text runs to `</code>`.
    let mut at = i;
    while let Some(rel) = lower[at..].find("<td><code>") {
        let name_start = at + rel + "<td><code>".len();
        let name_end = skip_while(lower, name_start, |c| c != '<');
        if name_end > name_start && lower[name_end..].starts_with("</code>") {
            return Some((packet_id, body[name_start..name_end].trim_matches(is_py_space).to_string()));
        }
        at += rel + 1;
    }
    None
}

/// `row_re.search(body)`: the first `<td>` at which the row pattern matches.
fn match_row(body: &str, lower: &str) -> Option<(u64, String)> {
    let mut at = 0;
    while let Some(rel) = lower[at..].find("<td>") {
        let start = at + rel;
        if let Some(found) = row_at(body, lower, start + "<td>".len()) {
            return Some(found);
        }
        at = start + 1;
    }
    None
}

/// Every route row under each `STATE DIRECTION packets` heading of the page.
pub fn parse_tables(html: &str) -> Vec<RouteRow> {
    let lower = html.to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut pos = 0;
    while let Some(heading) = find_heading(html, &lower, pos) {
        let start = heading.end;
        let end = find_heading(html, &lower, start).map_or(html.len(), |next| next.start);
        let (chunk, chunk_lower) = (&html[start..end], &lower[start..end]);
        for (from, to) in row_bodies(chunk_lower) {
            if let Some((packet_id, official_name)) = match_row(&chunk[from..to], &chunk_lower[from..to]) {
                rows.push(RouteRow {
                    state: heading.state.clone(),
                    direction: heading.direction.to_string(),
                    packet_id,
                    official_name,
                });
            }
        }
        pos = end;
    }
    rows
}

/// The BPS packet an official packet name routes to.
pub fn packet_name(official: &str, state: &str, direction: &str, packet_id: u64) -> String {
    if let Some((_, name)) = SERVERBOUND_ALIASES
        .iter()
        .find(|(key, _)| *key == (state, direction, official))
    {
        return (*name).to_string();
    }
    match (state, direction, official) {
        ("login", "serverbound", "hello") => return "login_start".to_string(),
        ("login", "serverbound", "key") => return "encryption_response".to_string(),
        ("login", "clientbound", "hello") => return "hello".to_string(),
        ("handshaking", "serverbound", "intention") => return "handshake".to_string(),
        _ => {}
    }
    if let Some((_, name)) = PACKET_ALIASES.iter().find(|(from, _)| *from == official) {
        return (*name).to_string();
    }
    let name = official.replace('-', "_");
    if name.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        return format!("pkt_{state}_{direction}_{packet_id:02x}");
    }
    name
}

/// The route file. `imports` are paths relative to the output file: the file declaring
/// `varint` (the frame uses it) and each file declaring packets that routes bind to. A name
/// the output uses without importing it will not load.
pub fn emit_bps(
    rows: &[RouteRow],
    extra_routes: &[String],
    manual: &BTreeSet<String>,
    imports: &[String],
) -> String {
    let mut needed_packets: BTreeSet<String> = BTreeSet::new();
    let mut routes: BTreeSet<String> = BTreeSet::new();
    for r in rows {
        let name = packet_name(&r.official_name, &r.state, &r.direction, r.packet_id);
        routes.insert(format!(
            "    route {}.{} 0x{:02x} => {name}",
            r.state, r.direction, r.packet_id
        ));
        needed_packets.insert(name);
    }
    routes.extend(extra_routes.iter().cloned());

    let mut lines: Vec<String> = vec![
        "// Packet stubs + routes transcribed from minecraft.wiki (protocol 777 / 26.3).".into(),
        "// Regenerate: cargo run -q -p bps --bin scrape_wiki_routes -- -o schemas/minecraft_26_3_wiki.bps"
            .into(),
        String::new(),
    ];
    if !imports.is_empty() {
        lines.extend(imports.iter().map(|path| format!("import \"{path}\"")));
        lines.push(String::new());
    }
    for name in &needed_packets {
        if manual.contains(name) || MANUAL_PACKETS.contains(&name.as_str()) {
            continue;
        }
        lines.push(format!("packet {name} {{"));
        lines.push("    body: rest".into());
        lines.push("}\n".into());
    }
    lines.push("protocol minecraft @ v26_3 {".into());
    lines.push("    id = 777".into());
    lines.push("    frame default {".into());
    lines.push("        length: varint".into());
    lines.push("        id: varint".into());
    lines.push("        payload: rest".into());
    lines.push("    }\n".into());
    lines.extend(routes);
    lines.push("}".into());
    lines.join("\n") + "\n"
}

/// `Path.resolve()`: symlinks resolved for the part that exists, the rest kept as given.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut existing = absolute.as_path();
    let mut tail = Vec::new();
    loop {
        if let Ok(mut resolved) = existing.canonicalize() {
            resolved.extend(tail.iter().rev());
            return resolved;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                existing = parent;
            }
            _ => return absolute,
        }
    }
}

/// `os.path.relpath(path, base)` for two absolute paths.
fn relative_to(path: &Path, base: &Path) -> PathBuf {
    let path: Vec<_> = path.components().collect();
    let base: Vec<_> = base.components().collect();
    let shared = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in shared..base.len() {
        out.push("..");
    }
    for component in &path[shared..] {
        out.push(component.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

const USAGE: &str = "usage: scrape_wiki_routes [-h] [-o OUT] [--manual-replace [MANUAL_REPLACE ...]]\n\n\
Scrape minecraft.wiki's packet id tables into a .bps route file.\n\n\
options:\n  -h, --help            show this help message and exit\n  \
-o OUT, --out OUT     write .bps fragment (default: stdout)\n  \
--manual-replace [MANUAL_REPLACE ...]\n                        schema files whose packet declarations replace generated stubs";

struct Args {
    out: Option<PathBuf>,
    manual_replace: Vec<PathBuf>,
}

/// `None` for `--help`.
fn parse_args(argv: &[String]) -> Result<Option<Args>, String> {
    let usage_error = |message: String| format!("{USAGE}\nscrape_wiki_routes: error: {message}");
    let mut args = Args { out: None, manual_replace: Vec::new() };
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;
        if arg == "-h" || arg == "--help" {
            return Ok(None);
        } else if arg == "-o" || arg == "--out" {
            let value = argv.get(i).filter(|v| !v.starts_with('-')).ok_or_else(|| {
                usage_error("argument -o/--out: expected one argument".to_string())
            })?;
            args.out = Some(PathBuf::from(value));
            i += 1;
        } else if let Some(value) = arg.strip_prefix("--out=") {
            args.out = Some(PathBuf::from(value));
        } else if arg == "--manual-replace" || arg.starts_with("--manual-replace=") {
            if let Some(first) = arg.strip_prefix("--manual-replace=") {
                args.manual_replace.push(PathBuf::from(first));
            }
            while i < argv.len() && !argv[i].starts_with('-') {
                args.manual_replace.push(PathBuf::from(&argv[i]));
                i += 1;
            }
        } else {
            return Err(usage_error(format!("unrecognized arguments: {arg}")));
        }
    }
    Ok(Some(args))
}

/// The script's `main`: `Err` is a usage error (exit 2), `Ok` the exit code.
pub fn main(argv: &[String]) -> Result<i32, String> {
    let Some(args) = parse_args(argv)? else {
        println!("{USAGE}");
        return Ok(0);
    };
    let html = match fetch_html() {
        Ok(html) => html,
        Err(message) => {
            eprintln!("{message}");
            return Ok(1);
        }
    };
    let rows = parse_tables(&html);
    if rows.is_empty() {
        eprintln!("no routes parsed");
        return Ok(1);
    }
    let mut manual = BTreeSet::new();
    let mut packet_files = Vec::new();
    for path in &args.manual_replace {
        let declared = match crate::text::read_text(path)
            .map_err(|e| format!("{}: {e}", path.display()))
            .and_then(|text| parse(&text).map_err(|e| e.to_string()))
        {
            Ok(schema) => schema.packets,
            Err(message) => {
                eprintln!("{message}");
                return Ok(1);
            }
        };
        manual.extend(declared.iter().map(|pkt| pkt.name.clone()));
        if !declared.is_empty() {
            packet_files.push(path.clone());
        }
    }
    let extra = vec!["    route handshaking.serverbound 0xfe => legacy_server_list_ping".to_string()];
    let out_dir = match &args.out {
        Some(out) => resolve(out.parent().unwrap_or(Path::new(""))),
        None => resolve(Path::new("")),
    };
    let std_varint = Path::new(PROTOC_ROOT).join("schemas/std/varint.bps");
    let imports: Vec<String> = packet_files
        .iter()
        .chain(std::iter::once(&std_varint))
        .map(|path| relative_to(&resolve(path), &out_dir).to_string_lossy().into_owned())
        .collect();
    let text = emit_bps(&rows, &extra, &manual, &imports);
    eprintln!("parsed {} wiki routes", rows.len());
    match &args.out {
        Some(out) => {
            if let Err(e) = std::fs::write(out, &text) {
                eprintln!("{}: {e}", out.display());
                return Ok(1);
            }
            eprintln!("wrote {}", out.display());
        }
        None => print!("{text}"),
    }
    Ok(0)
}
