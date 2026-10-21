//! `bml help`: the table of commands, the pages printed from it, and the hints for mistakes.
//!
//! [`COMMANDS`] is the one list of commands. [`crate::cli::main`] answers "unknown command" for a
//! name that is not in it and `bml help` prints it, so a command that is listed but not
//! dispatched, or dispatched but not listed, fails `tests/test_cli_help.rs` instead of quietly
//! missing from the help.

use crate::cli::EMIT_TARGETS;

/// How wide the pages are wrapped, and the most a hand-broken usage line or example may take.
pub const WIDTH: usize = 96;

/// One command: where it sits in the overview and what its page says.
pub struct Command {
    pub name: &'static str,
    /// The heading it is listed under in the overview.
    pub group: &'static str,
    /// The arguments shown after the name in the overview.
    pub synopsis: &'static str,
    /// One line, shown in the overview and in the page's title.
    pub summary: &'static str,
    /// The page's usage lines, without the leading `bml `.
    pub usage: &'static [&'static str],
    /// What the command does: paragraphs separated by a blank line, wrapped when printed.
    pub about: &'static str,
    /// `(flag, description)`; the description is wrapped when printed.
    pub options: &'static [(&'static str, &'static str)],
    pub examples: &'static [&'static str],
}

const INSPECT: &str = "Check and inspect a schema";
const BYTES: &str = "Decode, encode and verify bytes";
const GENERATE: &str = "Generate code";
const IMPORT: &str = "Import minecraft-data";
const HELP: &str = "Help";

/// Every command `bml` runs, in the order the overview lists them.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "check",
        group: INSPECT,
        synopsis: "<schema>...",
        summary: "Parse, resolve and type-check; prints ok",
        usage: &["check <schema>..."],
        about: "Loads each schema and every file it imports, resolves names and type-checks the \
                result. Prints `ok` and exits 0 when it is valid; otherwise prints the problem \
                and exits 3.",
        options: &[],
        examples: &["bml check schemas/minecraft_26_3_wiki.bml"],
    },
    Command {
        name: "routes",
        group: INSPECT,
        synopsis: "<schema>...",
        summary: "Print the packet routing table",
        usage: &["routes <schema>..."],
        about: "Prints one line per routed packet as `<state>.<direction> 0x<id> -> <packet>`, \
                for example `configuration.clientbound 0x00 -> cookie_request`.",
        options: &[],
        examples: &["bml routes schemas/minecraft_26_3_wiki.bml"],
    },
    Command {
        name: "dump-json",
        group: INSPECT,
        synopsis: "<schema>...",
        summary: "Print the parsed schema as JSON",
        usage: &["dump-json <schema>..."],
        about: "Prints the parsed schema as one JSON document.",
        options: &[],
        examples: &["bml dump-json schemas/std/varint.bml"],
    },
    Command {
        name: "stages",
        group: INSPECT,
        synopsis: "<schema>...",
        summary: "Print each compiler phase as JSON",
        usage: &["stages <schema>..."],
        about: "Runs the compiler phases in order (syntax, semantic analysis, lowering, typing) \
                and prints the result as one JSON object with the keys `syntax`, `semantic`, \
                `lowered` and `typed`. A failure stops at the phase that raised it: nothing is \
                printed to stdout and the exit status is 3.",
        options: &[],
        examples: &["bml stages schemas/std/varint.bml"],
    },
    Command {
        name: "decode",
        group: BYTES,
        synopsis: "<schema>... [options]",
        summary: "Decode hex bytes from stdin to JSON",
        usage: &[
            "decode <schema>... --packet <name> [--hex]",
            "decode <schema>... --state <state> --direction <direction> [--compression <spec>]\n\
             [--hex]",
        ],
        about: "Reads hex bytes from stdin (whitespace anywhere is ignored) and prints the \
                decoded packet as JSON with its keys sorted. Say what the bytes are with \
                --packet, or with --state and --direction; with neither it exits 1.\n\n\
                --packet decodes them as the body of that packet. --state and --direction \
                decode a whole frame, laid out as the schema's `frame` block says (for \
                Minecraft: length, id, payload), and pick the packet by its id from that \
                state's routes, so the schema needs routes. The output is then `{\"fields\": \
                {...}, \"id\": N, \"packet\": \"name\"}`, and bytes after the frame are \
                ignored.",
        options: &[
            (
                "--packet <name>",
                "Decode the bytes as the body of this packet: no length, no packet id.",
            ),
            (
                "--state <state>",
                "With --direction: decode a frame, using the routes of this state.",
            ),
            ("--direction <direction>", "clientbound or serverbound."),
            (
                "--compression <spec>",
                "With --state and --direction (not --packet): the frame is a compressed one, \
                 with an outer length, a data length, then raw or zlib bytes. The schema's \
                 frame must be `length`, `id`, `payload`. <spec> is `zlib` or \
                 `zlib:<threshold>`; decoding ignores the threshold, since the data length says \
                 whether a frame is compressed.",
            ),
            (
                "--hex",
                "Accepted for compatibility; the input is always hex.",
            ),
        ],
        examples: &[
            "echo 'ac0202' | bml decode schemas/minecraft_26_3_play.bml --packet animate --hex",
            "echo '04 02 ac0202' | bml decode schemas/minecraft_26_3_wiki.bml \\\n\
             --state play --direction clientbound",
        ],
    },
    Command {
        name: "encode",
        group: BYTES,
        synopsis: "--packet <name> <schema>... <json>",
        summary: "Encode a JSON object to hex",
        usage: &[
            "encode --packet <name> <schema>... <json>",
            "encode --packet <name> --state <state> --direction <direction> --compression <spec>\n\
             <schema>... <json>",
        ],
        about: "Encodes a JSON object as the bytes of one packet and prints them as lowercase \
                hex. The JSON is the last argument and must be an object.",
        options: &[
            ("--packet <name>", "The packet to encode (required)."),
            (
                "--state <state>",
                "With --compression: the state whose route gives the packet its id in the frame.",
            ),
            (
                "--direction <direction>",
                "With --compression: clientbound or serverbound.",
            ),
            (
                "--compression <spec>",
                "Wrap the packet in a Minecraft frame with a compression envelope: `zlib` or \
                 `zlib:<threshold>`. A payload shorter than the threshold is stored raw, with \
                 data length 0.",
            ),
        ],
        examples: &[
            "bml encode --packet animate schemas/minecraft_26_3_play.bml \\\n\
             '{\"entity_id\": 300, \"animation\": 2}'",
        ],
    },
    Command {
        name: "wire-check",
        group: BYTES,
        synopsis: "<schema>... [options]",
        summary: "Check that wire vectors round-trip exactly",
        usage: &["wire-check <schema>... [--file <vectors.jsonl>] [--dump <decoded.jsonl>]"],
        about: "Checks wire vectors against the schemas: one JSON object per line, \
                {\"state\", \"direction\", \"id\", \"hex\"}, where `hex` is the packet payload \
                after its id and `direction` is clientbound or serverbound (toClient and \
                toServer also work). Each vector must decode strictly, with every byte \
                consumed, and re-encode to the same bytes.\n\n\
                Prints `checked N, failed M`. The first 50 failures go to stderr and the exit \
                status is 3 if any failed.",
        options: &[
            (
                "--file <path>",
                "Read the vectors from a file instead of stdin.",
            ),
            (
                "--dump <path>",
                "Write `{\"line\": N, \"value\": <decoded>}` for every vector that round-tripped, \
                 one per line.",
            ),
        ],
        examples: &["bml wire-check schemas/minecraft_26_3_wiki.bml --file vectors.jsonl"],
    },
    Command {
        name: "emit",
        group: GENERATE,
        synopsis: "<target> <schema>... [options]",
        summary: "Generate a codec package for a language",
        usage: &[
            "emit <target>[:<standard>] <schema>... --out <dir> [--std <standard>]",
            "emit rstom-tree <schema>... --rstom <workspace>\n\
             [--shapes <dir>] [--packets-json <file>] [--check]",
        ],
        about: "Generates a package for <target> from the schemas into --out. Language targets \
                take a standard, written `go:1.23` or `--std 1.23`; `bml targets` lists what \
                each accepts. `rstom` writes a drop-in mirror of rstom's `rstom-packets` crate, \
                and `rstom-tree` writes shaped packets and the wire-pin gate into an rstom \
                workspace.",
        options: &[
            (
                "--out <dir>",
                "Directory to write into. Required, except for rstom-tree.",
            ),
            (
                "--std <standard>",
                "Language standard or version, for example `1.23` for go. Also spelled \
                 --target-version or --edition.",
            ),
            (
                "--rstom <workspace>",
                "rstom-tree: the rstom workspace to generate into (required).",
            ),
            (
                "--shapes <dir>",
                "rstom-tree: the shape ledgers `bml shapes extract` writes. Default: \
                 targets/rstom/shapes in the source tree this bml was built from, which is not \
                 tracked by git.",
            ),
            (
                "--packets-json <file>",
                "rstom and rstom-tree: rstom's packets_gen schema, which names each packet's \
                 Minestom class. Default: tools/packets_gen/schema/packets.json, looked for in \
                 --rstom (in --out for rstom) and then two directories above it.",
            ),
            (
                "--check",
                "rstom-tree only: write nothing; print what has drifted and exit 3, or print \
                 `ok`.",
            ),
        ],
        examples: &[
            "bml emit go:1.23 schemas/std/varint.bml --out ./bmlgen_go",
            "bml emit rust-typed schemas/minecraft_26_3_wiki.bml --out ./packets",
            "bml emit rstom-tree schemas/minecraft_26_3_wiki.bml --rstom ../rstom --check",
        ],
    },
    Command {
        name: "targets",
        group: GENERATE,
        synopsis: "",
        summary: "List emit targets and their standards (JSON)",
        usage: &["targets"],
        about: "Prints the emit target matrix as JSON: a list with one `{target, language, \
                default, versions}` entry per language, where `default` is the standard used \
                when none is asked for and `versions` is every standard `bml emit` accepts for \
                it.",
        options: &[],
        examples: &["bml targets"],
    },
    Command {
        name: "shapes",
        group: GENERATE,
        synopsis: "extract|report <schema>...",
        summary: "Compare packets with an rstom workspace",
        usage: &[
            "shapes extract <schema>... --rstom <workspace> --out <dir>\n\
             [--eligible] [--class <name>]... [--packets-json <file>]",
            "shapes report <schema>... --rstom <workspace> [--packets-json <file>]",
        ],
        about: "Works with the hand-written packet classes of an rstom workspace. `extract` \
                reads each class's source file and writes its shape (its fields, as JSON) to \
                --out, one file per class, in the layout `bml emit rstom-tree --shapes` reads; \
                a class with no source file, or whose shape can't be extracted, is skipped. \
                `report` prints one line per class: its name, a tab, and its status, which is \
                `eligible` when its fields match the schema and otherwise says what is missing \
                or different.",
        options: &[
            ("--rstom <workspace>", "The rstom workspace (required)."),
            ("--out <dir>", "extract: where the ledgers go (required)."),
            (
                "--eligible",
                "extract: keep only classes whose fields match the schema.",
            ),
            (
                "--class <name>",
                "extract: only this class; repeat for several.",
            ),
            (
                "--packets-json <file>",
                "rstom's packets_gen schema. Default: tools/packets_gen/schema/packets.json in \
                 the rstom workspace.",
            ),
        ],
        examples: &[
            "bml shapes extract schemas/minecraft_26_3_wiki.bml --rstom ../rstom --eligible \\\n\
             --out targets/rstom/shapes",
            "bml shapes report schemas/minecraft_26_3_wiki.bml --rstom ../rstom",
        ],
    },
    Command {
        name: "import-versions",
        group: IMPORT,
        synopsis: "[options]",
        summary: "Regenerate schemas/versions from minecraft-data",
        usage: &["import-versions [--manifest <file>] [--data <dir>] [--out <dir>] [--check]"],
        about: "Regenerates the per-version schemas in schemas/versions from minecraft-data's \
                ProtoDef files. The manifest lists the protocol numbers and releases to import \
                and where each one's files are under --data. Run \
                scripts/fetch_minecraft_data.py first to download the data.\n\n\
                The defaults are relative to the source tree this bml was built from; with an \
                installed binary, pass all three paths.",
        options: &[
            (
                "--manifest <file>",
                "Which protocol numbers and releases to import. Default: \
                 schemas/versions/manifest.json.",
            ),
            (
                "--data <dir>",
                "Where the ProtoDef files were fetched. Default: data/minecraft-data.",
            ),
            (
                "--out <dir>",
                "Where the schemas are written. Default: schemas/versions.",
            ),
            (
                "--check",
                "Write nothing; print `ok` if every file is up to date, or a `stale: <path>` \
                 line for each one that is not, and exit 3.",
            ),
        ],
        examples: &["bml import-versions --check"],
    },
    Command {
        name: "import-protodef",
        group: IMPORT,
        synopsis: "<protocol.json> [options]",
        summary: "Convert one ProtoDef protocol.json to a schema",
        usage: &[
            "import-protodef <protocol.json> --protocol <number> --release <version>\n\
             [--also <version>]... [--patches <file>] [--out <file>]",
        ],
        about: "Converts one ProtoDef `protocol.json` to a schema and prints it, or writes it \
                with --out. `bml help import-versions` imports every version at once.",
        options: &[
            (
                "--protocol <number>",
                "The protocol number the file describes (required).",
            ),
            (
                "--release <version>",
                "The release it belongs to, such as 1.21.4 (required).",
            ),
            (
                "--also <version>",
                "Another release that shares this protocol number, noted in the schema's \
                 header; repeat for several.",
            ),
            (
                "--patches <file>",
                "Corrections to apply before importing: a JSON file `{\"patches\": [{\"reason\", \
                 \"scope\", \"types\"}]}` whose `types` replace or add named types. See \
                 schemas/versions/patches/.",
            ),
            ("--out <file>", "Write the schema here instead of stdout."),
        ],
        examples: &[
            "bml import-protodef data/minecraft-data/pc/1.21.4/protocol.json \\\n\
             --protocol 769 --release 1.21.4",
        ],
    },
    Command {
        name: "help",
        group: HELP,
        synopsis: "[command]",
        summary: "Show this list, or one command's options",
        usage: &["help [command]", "<command> --help"],
        about: "Prints the list of commands, or the options of one command. `bml <command> \
                --help` and `bml <command> -h` print the same page, and `bml --version` prints \
                the version.",
        options: &[],
        examples: &["bml help", "bml help emit"],
    },
];

/// The command named `name`, if there is one.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// `bml <version>`, the crate's own version, as `--version` prints it.
pub fn version_line() -> String {
    format!("bml {}", env!("CARGO_PKG_VERSION"))
}

/// Every command with its summary, and how to get to the rest.
pub fn overview() -> String {
    let labels: Vec<String> = COMMANDS
        .iter()
        .map(|c| format!("{} {}", c.name, c.synopsis).trim_end().to_string())
        .collect();
    let width = labels.iter().map(String::len).max().unwrap_or(0);

    let mut out = format!(
        "{}: the Bytemill compiler for BML schemas\n\nUsage:\n  bml <command> [options]\n  bml help \
         <command>\n",
        version_line()
    );
    let mut group = "";
    for (command, label) in COMMANDS.iter().zip(&labels) {
        if command.group != group {
            group = command.group;
            out.push_str(&format!("\n{group}:\n"));
        }
        out.push_str(&format!("  {label:<width$}  {}\n", command.summary));
    }
    out.push('\n');
    out.push_str(&wrap(
        "A <schema> is a .bml file. The files it imports are loaded with it, and you can name \
         several. A flag takes its value as `--flag value` or `--flag=value`. Run `bml help \
         <command>` or `bml <command> --help` for a command's options, and `bml --version` for \
         the version.",
        "",
        "",
    ));
    out.push_str("\n\n");
    out.push_str(&wrap(
        "Exit status: 0 success, 2 usage error, 3 the command ran and found a problem (an invalid \
         schema, a failed check, stale or drifting output).",
        "",
        "",
    ));
    out.push_str(
        "\n\nExamples:\n  bml check schemas/std/varint.bml\n  bml routes \
         schemas/minecraft_26_3_wiki.bml\n  echo 'ac0202' | bml decode \
         schemas/minecraft_26_3_play.bml --packet animate --hex\n  bml emit go:1.23 \
         schemas/std/varint.bml --out ./bmlgen_go\n  bml help emit",
    );
    out
}

/// One command's page: usage, what it does, its options and examples.
pub fn page(command: &Command) -> String {
    let mut out = format!("bml {} - {}\n\nUsage:\n", command.name, command.summary);
    for line in command.usage {
        out.push_str(&indent_lines(&format!("bml {line}")));
        out.push('\n');
    }
    for paragraph in command.about.split("\n\n") {
        out.push('\n');
        out.push_str(&wrap(paragraph, "", ""));
        out.push('\n');
    }
    if command.name == "emit" {
        out.push('\n');
        out.push_str(&wrap(
            &format!(
                "Targets: {}. (`cs` also means csharp.)",
                EMIT_TARGETS.join(", ")
            ),
            "",
            "",
        ));
        out.push('\n');
    }
    if !command.options.is_empty() {
        out.push_str("\nOptions:\n");
        out.push_str(&options(command.options));
    }
    if !command.examples.is_empty() {
        out.push_str("\nExamples:\n");
        for example in command.examples {
            out.push_str(&indent_lines(example));
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

/// Flags in a column, descriptions wrapped beside them; a flag too long for the column puts its
/// description on the next line.
fn options(options: &[(&str, &str)]) -> String {
    let flag_width = options
        .iter()
        .map(|(flag, _)| flag.len())
        .filter(|len| *len <= 26)
        .max()
        .unwrap_or(0);
    let indent = " ".repeat(2 + flag_width + 2);
    let mut out = String::new();
    for (flag, description) in options {
        if flag.len() <= flag_width {
            out.push_str(&wrap(
                description,
                &format!("  {flag:<flag_width$}  "),
                &indent,
            ));
        } else {
            out.push_str(&format!("  {flag}\n"));
            out.push_str(&wrap(description, &indent, &indent));
        }
        out.push('\n');
    }
    out
}

/// A usage line or example exactly as written, indented two spaces, and six after a line break
/// in it: a long one is split by hand where a reader can still copy it.
fn indent_lines(text: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, line)| format!("{}{line}", if i == 0 { "  " } else { "      " }))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Words flowed into lines of at most [`WIDTH`] columns: `first` starts the first line and
/// `rest` every later one. A `code span` is never broken across lines.
fn wrap(text: &str, first: &str, rest: &str) -> String {
    let mut lines = Vec::new();
    let mut line = first.to_string();
    let mut has_word = false;
    for chunk in chunks(text) {
        if has_word && line.len() + 1 + chunk.len() > WIDTH {
            lines.push(std::mem::replace(&mut line, rest.to_string()));
            has_word = false;
        }
        if has_word {
            line.push(' ');
        }
        line.push_str(&chunk);
        has_word = true;
    }
    lines.push(line);
    lines.join("\n")
}

/// The words of `text`, with the words of one backtick span joined into a single chunk.
fn chunks(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for word in text.split_whitespace() {
        match out.last_mut() {
            Some(last) if open => {
                last.push(' ');
                last.push_str(word);
            }
            _ => out.push(word.to_string()),
        }
        if word.matches('`').count() % 2 == 1 {
            open = !open;
        }
    }
    out
}

/// `bml help [command]`: the overview, or one command's page, on stdout.
pub fn run_help(args: &[String]) -> Result<i32, String> {
    let page_of = |command: &Command| {
        println!("{}", page(command));
        Ok(0)
    };
    match args {
        [] => {
            println!("{}", overview());
            Ok(0)
        }
        [topic] if topic == "-h" || topic == "--help" => {
            page_of(find("help").expect("help is in COMMANDS"))
        }
        [topic] => match find(topic) {
            Some(command) => page_of(command),
            None => Err(unknown_command(topic)),
        },
        // `bml help shapes extract` is the shapes page, which covers both.
        [topic, sub] if topic == "shapes" && (sub == "extract" || sub == "report") => {
            page_of(find("shapes").expect("shapes is in COMMANDS"))
        }
        [_, extra, ..] => Err(format!(
            "bml help: unexpected argument {extra:?}\nRun `bml help` for the list of commands."
        )),
    }
}

/// The message for a command name `bml` does not know, with the nearest commands if any.
pub fn unknown_command(word: &str) -> String {
    let mut message = format!("bml: unknown command {word:?}");
    let near = suggestions(word);
    if !near.is_empty() {
        let names: Vec<String> = near.iter().map(|name| format!("`{name}`")).collect();
        message.push_str(&format!("\n  did you mean {}?", names.join(" or ")));
    }
    message.push_str("\nRun `bml help` for the list of commands.");
    message
}

/// Commands that `word` starts or nearly spells: nearest first, at most three.
fn suggestions(word: &str) -> Vec<&'static str> {
    let word = word.to_ascii_lowercase();
    let mut scored: Vec<(bool, usize, &'static str)> = COMMANDS
        .iter()
        .filter_map(|command| {
            let distance = edit_distance(&word, command.name);
            let prefix = word.len() >= 2 && command.name.starts_with(&word);
            let near = word.len() >= 3 && distance <= 2;
            (prefix || near).then_some((!prefix, distance, command.name))
        })
        .collect();
    scored.sort();
    scored
        .into_iter()
        .take(3)
        .map(|(_, _, name)| name)
        .collect()
}

/// Levenshtein distance between two ASCII words.
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

/// A pointer to the right page for a usage error from [`crate::cli::main`], unless the message
/// already points somewhere or is not about usage.
pub fn hint(message: &str) -> Option<String> {
    if message.contains("bml help") {
        return None;
    }
    let first = message.split([':', ' ']).next().unwrap_or("");
    if let Some(command) = find(first) {
        return Some(format!("Run `bml help {}` for usage.", command.name));
    }
    message
        .starts_with("argument ")
        .then(|| "Run `bml help` for usage.".to_string())
}
