//! Command line, porting `bps/cli.py`.
//!
//! Argument handling is hand-rolled to match the Python `argparse` surface the
//! tests exercise: the same subcommands, the same flags, exit code 2 for usage
//! errors and 3 for `BpsError`/`ShapeError`. The Python CLI has no `import-versions`,
//! `import-protodef` or `wire-check`; those are Rust-only.

use crate::analyze::analyze;
use crate::emit_json::schema_to_json;
use crate::emit_rstom::emit_rstom;
use crate::emit_rust::emit_rust;
use crate::emit_tree::emit_tree;
use crate::errors::{BpsError, BpsResult, emit};
use crate::interp::{decode_frame_payload, decode_packet, encode_packet};
use crate::json::{Value, dumps_sorted, loads};
use crate::load::load_schema;
use crate::minestom::{ClassTable, build_units};
use crate::shapes::{
    bps_field_pairs, extract_shape, hand_packet_path, mismatch, module_path, packet_report,
    shape_to_json,
};
use std::path::{Path, PathBuf};

/// Where `packets_gen`'s schema is in an rstom workspace, relative to it.
const PACKETS_GEN_SCHEMA: &str = "tools/packets_gen/schema/packets.json";
const PROTOC_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const DEFAULT_SHAPES_REL: &str = "targets/rstom/shapes";

fn default_shapes() -> PathBuf {
    Path::new(PROTOC_ROOT).join(DEFAULT_SHAPES_REL)
}

fn load_and_analyze(paths: &[String]) -> BpsResult<crate::ast::ResolvedSchema> {
    let schema = load_schema(&paths.iter().map(PathBuf::from).collect::<Vec<_>>())?;
    analyze(&schema)
}

fn default_packets_json_hint(target: &str, anchor: &Path) -> String {
    let resolved = anchor
        .canonicalize()
        .unwrap_or_else(|_| anchor.to_path_buf());
    if target == "rstom-tree" {
        return format!("<workspace>/{PACKETS_GEN_SCHEMA}");
    }
    if resolved.join(PACKETS_GEN_SCHEMA).is_file() {
        return format!("<workspace>/{PACKETS_GEN_SCHEMA}");
    }
    format!("<out>/../../{PACKETS_GEN_SCHEMA}")
}

/// rstom's class table: the one named, or the one beside an rstom workspace / crate.
fn class_table(given: Option<&str>, anchor: &Path, target: &str) -> BpsResult<ClassTable> {
    let path = match given {
        Some(p) => PathBuf::from(p),
        None => {
            let resolved = anchor
                .canonicalize()
                .unwrap_or_else(|_| anchor.to_path_buf());
            let beside = resolved.join(PACKETS_GEN_SCHEMA);
            if beside.is_file() {
                beside
            } else {
                // Two levels up from the crate, like Python's
                // `resolved.parent.parent`.
                resolved
                    .parent()
                    .and_then(|p| p.parent())
                    .unwrap_or(&resolved)
                    .join(PACKETS_GEN_SCHEMA)
            }
        }
    };
    if !path.is_file() {
        let default = default_packets_json_hint(target, anchor);
        return Err(emit(format!(
            "emit {target} needs rstom's packets_gen schema, which names each packet's Minestom class: {} is not a file (pass --packets-json; by default it is {default})",
            path.display()
        )));
    }
    ClassTable::load(&path)
}

#[derive(Default)]
struct Args {
    schemas: Vec<String>,
    packet: Option<String>,
    state: Option<String>,
    direction: Option<String>,
    hex: bool,
    target: Option<String>,
    out: Option<String>,
    rstom: Option<String>,
    shapes: Option<String>,
    check: bool,
    packets_json: Option<String>,
    classes: Vec<String>,
    eligible: bool,
    json_value: Option<String>,
    std: Option<String>,
}

/// Pull a flag's value out of `argv`, leaving the positionals.
fn take_flag(argv: &mut Vec<String>, names: &[&str]) -> Result<Option<String>, String> {
    let mut found: Option<String> = None;
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        let matches = names.iter().any(|n| {
            arg == *n || (n.len() > 2 && n.starts_with("--") && arg.starts_with(&format!("{n}=")))
        });
        if !matches {
            i += 1;
            continue;
        }
        if let Some(eq) = arg.find('=')
            && arg.starts_with("--")
        {
            let value = argv[i][eq + 1..].to_string();
            argv.remove(i);
            found = Some(value);
            continue;
        }
        if i + 1 >= argv.len() {
            return Err(format!("argument {arg}: expected one argument"));
        }
        let value = argv[i + 1].clone();
        argv.remove(i);
        argv.remove(i);
        found = Some(value);
    }
    Ok(found)
}

fn take_switch(argv: &mut Vec<String>, name: &str) -> bool {
    let mut found = false;
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == name {
            argv.remove(i);
            found = true;
        } else {
            i += 1;
        }
    }
    found
}

/// `bpsc import-versions` and `bpsc import-protodef`: minecraft-data's ProtoDef descriptions
/// as BPS schemas (see `crate::protodef`). Rust-only; the Python CLI has no importer.
fn import_cli(cmd: &str, mut argv: Vec<String>) -> Result<i32, String> {
    use crate::protodef::{FileSpec, Protocol, import, import_versions};
    let root = Path::new(PROTOC_ROOT);
    if cmd == "import-versions" {
        let manifest = take_flag(&mut argv, &["--manifest"])?.map_or_else(
            || root.join("schemas/versions/manifest.json"),
            PathBuf::from,
        );
        let data = take_flag(&mut argv, &["--data"])?
            .map_or_else(|| root.join("data/minecraft-data"), PathBuf::from);
        let out = take_flag(&mut argv, &["--out"])?
            .map_or_else(|| root.join("schemas/versions"), PathBuf::from);
        let check = take_switch(&mut argv, "--check");
        if let Some(extra) = argv.first() {
            return Err(format!("import-versions: unrecognized argument {extra}"));
        }
        let report = match import_versions(&manifest, &data, &out, check) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{e}");
                return Ok(3);
            }
        };
        if check {
            if report.stale.is_empty() {
                println!(
                    "ok: {} roots, {} shared files up to date",
                    report.roots, report.shared
                );
                return Ok(0);
            }
            for p in &report.stale {
                eprintln!("stale: {}", p.display());
            }
            return Ok(3);
        }
        println!(
            "wrote {}, unchanged {}, removed {}: {} roots, {} shared files, {} declarations kept as {}",
            report.written.len(),
            report.unchanged,
            report.removed.len(),
            report.roots,
            report.shared,
            report.declarations,
            report.kept
        );
        return Ok(0);
    }
    // import-protodef <protocol.json> --protocol N --release V [--also V]... [--patches FILE] [--out FILE]
    let protocol = take_flag(&mut argv, &["--protocol"])?
        .ok_or("import-protodef: the following arguments are required: --protocol")?
        .parse::<u32>()
        .map_err(|_| "import-protodef: --protocol is not a number".to_string())?;
    let release = take_flag(&mut argv, &["--release"])?
        .ok_or("import-protodef: the following arguments are required: --release")?;
    let mut also = Vec::new();
    while let Some(a) = take_flag(&mut argv, &["--also"])? {
        also.push(a);
    }
    let out = take_flag(&mut argv, &["--out"])?;
    let patches = take_flag(&mut argv, &["--patches"])?;
    let [path] = argv.as_slice() else {
        return Err("import-protodef: expected exactly one protocol.json".to_string());
    };
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let result = Protocol::from_json(&text).and_then(|mut proto| {
        let mut reasons = Vec::new();
        if let Some(f) = &patches {
            let patch_text = std::fs::read_to_string(f).map_err(|e| format!("{f}: {e}"))?;
            reasons = proto.apply_patches(&patch_text)?;
        }
        let spec = FileSpec {
            protocol,
            release: &release,
            also: &also,
            source: path,
            patches: &reasons,
            note: None,
        };
        import(&proto, &spec)
    });
    match result {
        Ok(src) => {
            match out {
                Some(f) => std::fs::write(&f, src).map_err(|e| format!("{f}: {e}"))?,
                None => print!("{src}"),
            }
            Ok(0)
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(3)
        }
    }
}

/// `bpsc wire-check <schema>... [--file vectors.jsonl] [--dump decoded.jsonl]`: every vector
/// (stdin by default) must decode strictly and re-encode to the same bytes; `--dump` writes the
/// decoded values. See `crate::wire`.
fn wire_check_cli(mut argv: Vec<String>) -> Result<i32, String> {
    let file = take_flag(&mut argv, &["--file"])?;
    let dump = take_flag(&mut argv, &["--dump"])?;
    let text = match file {
        Some(f) => std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?,
        None => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin()
                .read_to_string(&mut buf)
                .map_err(|e| format!("stdin: {e}"))?;
            buf
        }
    };
    let resolved = match load_and_analyze(&argv) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return Ok(3);
        }
    };
    let report = crate::wire::check_vectors(&resolved, &text, dump.is_some())?;
    if let Some(path) = dump {
        std::fs::write(&path, report.decoded.join("\n")).map_err(|e| format!("{path}: {e}"))?;
    }
    for f in report.failures.iter().take(50) {
        eprintln!("line {}: {}: {}", f.line, f.packet, f.reason);
    }
    println!(
        "checked {}, failed {}",
        report.checked,
        report.failures.len()
    );
    Ok(if report.failures.is_empty() { 0 } else { 3 })
}

pub fn main(argv: &[String]) -> Result<i32, String> {
    let mut argv = argv.to_vec();
    if argv.is_empty() {
        return Err("usage: bpsc <command> ...".to_string());
    }
    let cmd = argv.remove(0);
    if cmd == "import-versions" || cmd == "import-protodef" {
        return import_cli(&cmd, argv);
    }
    if cmd == "wire-check" {
        return wire_check_cli(argv);
    }
    let mut a = Args::default();
    let mut shapes_cmd: Option<String> = None;

    match cmd.as_str() {
        "check" | "dump-json" | "routes" => {
            a.schemas = argv;
        }
        "decode" => {
            a.packet = take_flag(&mut argv, &["--packet"])?;
            a.state = take_flag(&mut argv, &["--state"])?;
            a.direction = take_flag(&mut argv, &["--direction"])?;
            a.hex = take_switch(&mut argv, "--hex");
            a.schemas = argv;
        }
        "encode" => {
            a.packet = take_flag(&mut argv, &["--packet"])?;
            if a.packet.is_none() {
                return Err("encode: the following arguments are required: --packet".to_string());
            }
            // `encode --packet P [schemas...] json_value`: the JSON value is last.
            if argv.is_empty() {
                return Err("encode: the following arguments are required: json_value".to_string());
            }
            a.json_value = Some(argv.pop().unwrap());
            a.schemas = argv;
        }
        "emit" => {
            if argv.is_empty() {
                return Err("emit: the following arguments are required: target".to_string());
            }
            let raw_target = argv.remove(0);
            let (target_name, target_std) = if let Some((t, s)) = raw_target.split_once(':') {
                (t.to_string(), Some(s.to_string()))
            } else {
                (raw_target, None)
            };
            a.target = Some(target_name);
            if !matches!(
                a.target.as_deref(),
                Some("rust")
                    | Some("rust-typed")
                    | Some("rstom")
                    | Some("rstom-tree")
                    | Some("go")
                    | Some("python")
                    | Some("c")
                    | Some("typescript")
                    | Some("cpp")
                    | Some("zig")
                    | Some("java")
                    | Some("csharp")
                    | Some("cs")
            ) {
                return Err(format!(
                    "emit: argument target: invalid choice: {:?} (choose from 'rust', 'rust-typed', 'rstom', 'rstom-tree', 'go', 'python', 'c', 'typescript', 'cpp', 'zig', 'java', 'csharp')",
                    a.target.unwrap(),
                ));
            }
            a.std =
                take_flag(&mut argv, &["--std", "--target-version", "--edition"])?.or(target_std);
            a.out = take_flag(&mut argv, &["--out"])?;
            a.rstom = take_flag(&mut argv, &["--rstom"])?;
            a.shapes = take_flag(&mut argv, &["--shapes"])?;
            a.packets_json = take_flag(&mut argv, &["--packets-json"])?;
            a.check = take_switch(&mut argv, "--check");
            a.schemas = argv;
        }
        "shapes" => {
            if argv.is_empty() {
                return Err("shapes: the following arguments are required: shapes_cmd".to_string());
            }
            shapes_cmd = Some(argv.remove(0));
            match shapes_cmd.as_deref() {
                Some("extract") => {
                    a.rstom = take_flag(&mut argv, &["--rstom"])?;
                    a.out = take_flag(&mut argv, &["--out"])?;
                    a.packets_json = take_flag(&mut argv, &["--packets-json"])?;
                    a.eligible = take_switch(&mut argv, "--eligible");
                    while let Some(c) = take_flag(&mut argv, &["--class"])? {
                        a.classes.push(c);
                    }
                    if a.rstom.is_none() {
                        return Err(
                            "shapes extract: the following arguments are required: --rstom"
                                .to_string(),
                        );
                    }
                    if a.out.is_none() {
                        return Err(
                            "shapes extract: the following arguments are required: --out"
                                .to_string(),
                        );
                    }
                    a.schemas = argv;
                }
                Some("report") => {
                    a.rstom = take_flag(&mut argv, &["--rstom"])?;
                    a.packets_json = take_flag(&mut argv, &["--packets-json"])?;
                    if a.rstom.is_none() {
                        return Err(
                            "shapes report: the following arguments are required: --rstom"
                                .to_string(),
                        );
                    }
                    a.schemas = argv;
                }
                _ => {
                    return Err(format!(
                        "shapes: argument shapes_cmd: invalid choice: {:?} (choose from 'extract', 'report')",
                        shapes_cmd.unwrap()
                    ));
                }
            }
        }
        _ => {
            return Err(format!(
                "argument cmd: invalid choice: {cmd:?} (choose from 'check', 'dump-json', 'routes', 'decode', 'encode', 'emit', 'shapes', 'import-versions', 'import-protodef', 'wire-check')"
            ));
        }
    }

    // The Python code prints BpsError/ShapeError to stderr and exits 3.
    match run(&cmd, &a, shapes_cmd.as_deref()) {
        Ok(code) => Ok(code),
        Err(e) => {
            eprintln!("{e}");
            Ok(3)
        }
    }
}

fn run(cmd: &str, a: &Args, shapes_cmd: Option<&str>) -> BpsResult<i32> {
    match cmd {
        "check" => {
            load_and_analyze(&a.schemas)?;
            println!("ok");
            Ok(0)
        }
        "dump-json" => {
            let resolved = load_and_analyze(&a.schemas)?;
            println!("{}", schema_to_json(&resolved.schema));
            Ok(0)
        }
        "routes" => {
            let resolved = load_and_analyze(&a.schemas)?;
            for (route, pkt) in &resolved.routes {
                println!(
                    "{}.{} 0x{:02x} -> {}",
                    route.state, route.direction, route.packet_id, pkt.name
                );
            }
            Ok(0)
        }
        "decode" => {
            let resolved = load_and_analyze(&a.schemas)?;
            let raw = read_stdin()?;
            let data = hex_bytes(&raw)?;
            let out = if let Some(packet) = &a.packet {
                decode_packet(&resolved, packet, &data, false)?
            } else if let (Some(state), Some(direction)) = (&a.state, &a.direction) {
                let (pid, out) = decode_frame_payload(&resolved, state, direction, &data)?;
                let mut with_id = crate::value::Env::new();
                with_id.insert("id", Value::Int(pid));
                for (k, v) in out.iter() {
                    with_id.insert(k, v.clone());
                }
                with_id
            } else {
                eprintln!("need --packet or --state and --direction");
                return Ok(1);
            };
            println!("{}", dumps_sorted(&Value::map(out)));
            Ok(0)
        }
        "encode" => {
            let resolved = load_and_analyze(&a.schemas)?;
            let val = loads(a.json_value.as_deref().unwrap_or(""))?;
            let Value::Map(m) = val else {
                return Err(BpsError::new(
                    crate::errors::ErrorKind::Encode,
                    "encode needs a JSON object",
                ));
            };
            let out = encode_packet(&resolved, a.packet.as_deref().unwrap_or(""), &m)?;
            println!("{}", hex_lower(&out));
            Ok(0)
        }
        "emit" => {
            let resolved = load_and_analyze(&a.schemas)?;
            let target = a.target.as_deref().unwrap_or("");
            if target == "rstom-tree" {
                let Some(rstom) = &a.rstom else {
                    return Err(emit("emit rstom-tree needs --rstom <workspace>"));
                };
                let workspace = PathBuf::from(rstom);
                let shapes = match &a.shapes {
                    Some(s) => PathBuf::from(s),
                    None => default_shapes(),
                };
                let classes = class_table(a.packets_json.as_deref(), &workspace, "rstom-tree")?;
                let drift = emit_tree(&resolved, &workspace, &classes, &shapes, a.check)?;
                if a.check {
                    if !drift.is_empty() {
                        eprintln!("{}", drift.join("\n"));
                        return Ok(3);
                    }
                    println!("ok");
                    return Ok(0);
                }
                println!("emitted rstom-tree into {workspace:?}");
                return Ok(0);
            }
            if a.check {
                return Err(emit(format!(
                    "emit {target} does not support --check (only emit rstom-tree does)"
                )));
            }
            let Some(out) = &a.out else {
                return Err(emit(format!("emit {target} needs --out <directory>")));
            };
            let out_dir = PathBuf::from(out);
            let opts = crate::target_version::EmitOptions::for_target(target, a.std.as_deref())?;
            if target == "rust" {
                emit_rust(&resolved, &out_dir)?;
            } else if target == "rust-typed" {
                crate::emit_rust_typed::emit_rust_typed_with_edition(
                    &resolved,
                    &out_dir,
                    opts.rust_edition,
                )?;
            } else if target == "go" {
                crate::emit_go::emit_go_with_version(&resolved, &out_dir, opts.go_ver)?;
            } else if target == "python" {
                crate::emit_python::emit_python_with_version(&resolved, &out_dir, opts.py_ver)?;
            } else if target == "c" {
                crate::emit_c::emit_c_with_standard(&resolved, &out_dir, opts.c_std)?;
            } else if target == "typescript" {
                crate::emit_ts::emit_typescript_with_target(&resolved, &out_dir, opts.ts_target)?;
            } else if target == "cpp" {
                crate::emit_cpp::emit_cpp_with_standard(&resolved, &out_dir, opts.cpp_std)?;
            } else if target == "zig" {
                crate::emit_zig::emit_zig_with_version(&resolved, &out_dir, opts.zig_ver)?;
            } else if target == "java" {
                crate::emit_java::emit_java_with_version(&resolved, &out_dir, opts.java_ver)?;
            } else if target == "csharp" || target == "cs" {
                crate::emit_csharp::emit_csharp_with_version(&resolved, &out_dir, opts.csharp_ver)?;
            } else {
                let classes = class_table(a.packets_json.as_deref(), &out_dir, "rstom")?;
                emit_rstom(&resolved, &out_dir, &classes, "../rstom")?;
            }
            println!("emitted {target} crate to {out}");
            Ok(0)
        }
        "shapes" => {
            let resolved = load_and_analyze(&a.schemas)?;
            let workspace = PathBuf::from(a.rstom.as_deref().unwrap_or(""));
            let classes = class_table(a.packets_json.as_deref(), &workspace, "rstom-tree")?;
            let units = build_units(&resolved, &classes)?;
            match shapes_cmd {
                Some("report") => {
                    for unit in &units {
                        let status = packet_report(unit, &resolved, &workspace)?;
                        println!("{}\t{status}", unit.cls.name);
                    }
                    Ok(0)
                }
                Some("extract") => {
                    let out = PathBuf::from(a.out.as_deref().unwrap_or(""));
                    let filter: Option<Vec<String>> = if a.classes.is_empty() {
                        None
                    } else {
                        Some(a.classes.clone())
                    };
                    for unit in &units {
                        if let Some(f) = &filter
                            && !f.contains(&unit.cls.name)
                        {
                            continue;
                        }
                        let path = hand_packet_path(&workspace, unit);
                        if !path.is_file() {
                            continue;
                        }
                        let mod_path = module_path(unit);
                        let text = crate::text::read_text(&path).map_err(|e| {
                            BpsError::new(crate::errors::ErrorKind::Shape, format!("{e}"))
                        })?;
                        let Ok(shape) =
                            extract_shape(&text, &unit.cls.name, &mod_path, Some(&path))
                        else {
                            continue;
                        };
                        if a.eligible {
                            let pairs = bps_field_pairs(&unit.pkt, &resolved)?;
                            if !mismatch(&shape, &pairs).is_empty() {
                                continue;
                            }
                        }
                        let dest = out.join(format!("{mod_path}.json"));
                        if let Some(parent) = dest.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| {
                                BpsError::new(crate::errors::ErrorKind::Shape, format!("{e}"))
                            })?;
                        }
                        std::fs::write(&dest, shape_to_json(&shape)).map_err(|e| {
                            BpsError::new(crate::errors::ErrorKind::Shape, format!("{e}"))
                        })?;
                    }
                    Ok(0)
                }
                _ => Ok(0),
            }
        }
        _ => Ok(0),
    }
}

fn read_stdin() -> BpsResult<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| BpsError::new(crate::errors::ErrorKind::Parse, format!("{e}")))?;
    Ok(buf.trim().to_string())
}

/// `bytes.fromhex(raw.replace(" ", ""))`.
fn hex_bytes(raw: &str) -> BpsResult<Vec<u8>> {
    let text: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(BpsError::new(
            crate::errors::ErrorKind::Parse,
            "non-hexadecimal number found in fromhex() arg",
        ));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            _ => {
                return Err(BpsError::new(
                    crate::errors::ErrorKind::Parse,
                    "non-hexadecimal number found in fromhex() arg",
                ));
            }
        }
    }
    Ok(out)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
