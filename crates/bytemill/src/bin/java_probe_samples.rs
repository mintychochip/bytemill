//! `java_probe_samples`, porting `scripts/java_probe_samples.py`: compare BML packet layouts
//! with the bytes real Minestom writes.
//!
//! ```text
//! cargo run -q -p bytemill --bin java_probe_samples                    # packets the schema cannot reproduce
//! cargo run -q -p bytemill --bin java_probe_samples -- set_objective   # every recorded sample for one packet
//! ```
//!
//! The samples come from rstom's `probes/expected/*.json`. For a packet it shows the bytes
//! Java wrote, the field values Java built it from (when recorded), and either how the schema
//! decoded them or why it could not. The probe reader is the one the test suite uses
//! (`tests/common/java_probes.rs`), included rather than copied. The decoded value of a
//! passing sample prints as JSON, where the Python script printed a dict repr.

#[path = "../../tests/common/mod.rs"]
mod common;

use bytemill::interp::decode_packet;
use bytemill::json::dumps;
use bytemill::{analyze, load_schema};
use common::java_probes as java;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: java_probe_samples [-h] [--schema SCHEMA] [packets ...]\n\n\
Compare BML packet layouts with the bytes real Minestom writes.\n\n\
    cargo run -q -p bytemill --bin java_probe_samples                    # packets the schema cannot reproduce\n\
    cargo run -q -p bytemill --bin java_probe_samples -- set_objective   # every recorded sample for one packet\n\n\
The samples come from rstom's `probes/expected/*.json`. For a packet it shows the\n\
bytes Java wrote, the field values Java built it from (when recorded), and either\n\
how the schema decoded them or why it could not.\n\n\
positional arguments:\n  packets          BML packet names; none lists the failing ones\n\n\
options:\n  -h, --help       show this help message and exit\n  \
--schema SCHEMA  root schema file (default: the whole protocol)";

struct Args {
    packets: Vec<String>,
    schema: PathBuf,
}

/// `None` for `--help`.
fn parse_args(argv: &[String]) -> Result<Option<Args>, String> {
    let usage_error = |message: String| format!("{USAGE}\njava_probe_samples: error: {message}");
    let mut args = Args {
        packets: Vec::new(),
        schema: common::repo_root().join("schemas/minecraft_26_3_wiki.bml"),
    };
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;
        if arg == "-h" || arg == "--help" {
            return Ok(None);
        } else if arg == "--schema" {
            let value = argv.get(i).ok_or_else(|| {
                usage_error("argument --schema: expected one argument".to_string())
            })?;
            args.schema = PathBuf::from(value);
            i += 1;
        } else if let Some(value) = arg.strip_prefix("--schema=") {
            args.schema = PathBuf::from(value);
        } else if arg.starts_with('-') {
            return Err(usage_error(format!("unrecognized arguments: {arg}")));
        } else {
            args.packets.push(arg.to_string());
        }
    }
    Ok(Some(args))
}

fn run(args: &Args) -> Result<i32, String> {
    if !java::available() {
        eprintln!("rstom probe fixtures not found at {}", java::probes_dir().display());
        return Ok(2);
    }
    let resolved = load_schema(std::slice::from_ref(&args.schema))
        .and_then(|schema| analyze(&schema))
        .map_err(|e| e.to_string())?;

    if args.packets.is_empty() {
        let failed = java::failures(&resolved);
        for (packet, items) in &failed {
            let (sample, reason) = &items[0];
            println!(
                "{packet}: {} sample(s) differ; {}: {reason}",
                items.len(),
                sample.label()
            );
        }
        println!("{} packet(s) differ", failed.len());
        return Ok(0);
    }

    let by_class = java::packets_by_class(&resolved);
    let mut shown = 0;
    for sample in java::samples() {
        let Some((_, packets)) = by_class.iter().find(|(class, _)| *class == sample.java_class)
        else {
            continue;
        };
        for packet in packets {
            if !args.packets.contains(packet) {
                continue;
            }
            shown += 1;
            println!(
                "{packet} <- {}  {}  ({} bytes)",
                sample.java_class,
                sample.label(),
                sample.data.len()
            );
            println!("  bytes  {}", java::hex(&sample.data));
            if let Some(value) = &sample.value {
                println!("  java   {}", dumps(value).chars().take(1500).collect::<String>());
            }
            match java::check(&resolved, packet, &sample.data) {
                None => {
                    let decoded =
                        decode_packet(&resolved, packet, &sample.data, true).map_err(|e| e.to_string())?;
                    println!("  ok     {}", dumps(&bytemill::Value::map(decoded)));
                }
                Some(reason) => println!("  FAILS  {reason}"),
            }
        }
    }
    if shown == 0 {
        eprintln!("no recorded samples for {}", args.packets.join(", "));
        return Ok(1);
    }
    Ok(0)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(code) => ExitCode::from(code as u8),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}
