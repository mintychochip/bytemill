//! `emit rust-typed` output compiles clean and round-trips every packet.
//!
//! Small unit checks run in the emitted crate's own `tests/unit.rs`; the
//! 26.3 and Hytale legs generate a `tests/roundtrip.rs` with every
//! synthesizable packet (interpreter bytes -> generated decode -> generated
//! encode -> compare) and run the guide's gates (`cargo fmt --check`,
//! `cargo clippy --all-targets -- -D warnings`, `cargo test`) inside.

use std::path::{Path, PathBuf};
use std::process::Output;

use bytemill::ir::typed::{CodecIdx, Origin, Program};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

/// Fresh emitted-crate directory for one leg.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-rust-typed-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Private compiler cache for the inner `cargo` runs (deleted afterwards).
fn fresh_target(name: &str) -> PathBuf {
    let dir = Path::new(&std::env::var("HOME").unwrap()).join(format!(
        ".cache/bps-scratch/rust-typed-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn cargo(dir: &Path, target: &Path, args: &[&str]) -> Output {
    std::process::Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("cargo is installed")
}

fn has_tool(dir: &Path, target: &Path, args: &[&str]) -> bool {
    cargo(dir, target, args).status.success()
}

fn assert_gate(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Codec index of the packet called `name` (resolved through the program).
fn packet_codec(program: &Program, name: &str) -> CodecIdx {
    program
        .codecs
        .iter()
        .position(|c| matches!(&c.origin, Origin::Packet { name: n, .. } if n == name))
        .unwrap_or_else(|| panic!("no codec for packet {name}"))
}

/// `tests/roundtrip.rs` body decoding and re-encoding every synthesizable
/// packet of `resolved`. Returns the packet count.
fn roundtrip_body(resolved: &bytemill::ResolvedSchema, program: &Program) -> (String, usize) {
    use bytemill::interp::encode_packet;
    use bytemill::synth::synthesize_packet;
    let mut body = String::from("use bps_generated::*;\n");
    let mut n = 0usize;
    for packet in resolved.packets.values() {
        let value = match synthesize_packet(resolved, packet) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let bytes = match encode_packet(resolved, &packet.name, &value) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let ci = packet_codec(program, &packet.name);
        assert!(
            bytemill::emit_rust_typed::codec_is_method(program, ci),
            "packet {} is not a method codec",
            packet.name
        );
        assert!(
            program.codecs[ci].params.is_empty(),
            "packet {} takes params",
            packet.name
        );
        let ty = program.codecs[ci].ty.expect("method codec has a type");
        let type_name = bytemill::emit_rust_typed::type_name_of(program, ty);
        let id: String = packet
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let list = bytes
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        // `rustfmt` fills array lines greedily; mirror it so `fmt --check` holds.
        let one = format!("    let raw: &[u8] = &[{list}];");
        let raw_stmt = if one.len() <= 100 {
            one
        } else {
            let mut out = String::from("    let raw: &[u8] = &[\n");
            let mut line = String::from("        ");
            for e in bytes.iter().map(|b| b.to_string()) {
                if line.len() + e.len() + 1 > 100 {
                    out.push_str(line.trim_end());
                    out.push('\n');
                    line = String::from("        ");
                }
                line.push_str(&e);
                line.push_str(", ");
            }
            out.push_str(line.trim_end());
            out.push_str("\n    ];");
            out
        };
        body.push_str(&format!(
            "#[test]\nfn pkt_{n}_{id}() {{\n{raw_stmt}\n    let mut r = Reader::new(raw);\n    let v = {type_name}::decode(&mut r).unwrap();\n    let mut w = Writer::new();\n    v.encode(&mut w).unwrap();\n    assert_eq!(w.into_bytes(), raw);\n}}\n"
        ));
        n += 1;
    }
    (body, n)
}

/// Emit `resolved` into `dir`, drop in `tests/roundtrip.rs`, and run the
/// guide's gates there. Cleans up scratch afterwards.
fn gate_emitted(dir: &Path, target: &Path, leg: &str, min_packets: usize, count: usize) {
    // The harness's own `tests/*.rs` are formatted in place (they are test
    // scaffolding, not printer output); the `fmt --check` gate below then
    // verifies the printer's `src/*.rs` untouched.
    if let Ok(tests) = std::fs::read_dir(dir.join("tests")) {
        for entry in tests.flatten() {
            let _ = std::process::Command::new("rustfmt")
                .args(["--edition", "2024"])
                .arg(entry.path())
                .status();
        }
    }
    assert!(
        count >= min_packets,
        "{leg}: only {count} packets (need {min_packets})"
    );
    println!("{leg}: round-tripped {count} packets");
    let fmt_ok = has_tool(dir, target, &["fmt", "--version"]);
    let fmt = fmt_ok.then(|| cargo(dir, target, &["fmt", "--check"]));
    let clippy_ok = has_tool(dir, target, &["clippy", "--version"]);
    let clippy = clippy_ok.then(|| {
        cargo(
            dir,
            target,
            &["clippy", "--all-targets", "--", "-D", "warnings"],
        )
    });
    let test = cargo(dir, target, &["test"]);
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(target);
    if let Some(out) = fmt {
        assert_gate(&out, &format!("{leg}: cargo fmt --check"));
    }
    if let Some(out) = clippy {
        assert_gate(&out, &format!("{leg}: cargo clippy"));
    }
    assert_gate(&test, &format!("{leg}: cargo test"));
}

/// No emitted function clones: an encode body binds an optional field with
/// `as_ref()` and a sum payload as the match's own reference, so `.clone()`
/// only survives where the body genuinely moves the value (none in the
/// shipped schemas). The printer's `//!` module doc states that rule.
fn assert_no_clones(dir: &Path) {
    let mut stack = vec![dir.join("src")];
    let mut hits = Vec::new();
    while let Some(p) = stack.pop() {
        for e in std::fs::read_dir(&p).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for (i, l) in text.lines().enumerate() {
                    if l.contains(".clone()") {
                        hits.push(format!("{}:{}: {}", path.display(), i + 1, l.trim()));
                    }
                }
            }
        }
    }
    assert!(hits.is_empty(), "emitted code clones:\n{}", hits.join("\n"));
}

#[test]
fn rust_typed_unit_vectors() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe {\n    n: varint\n    x: f32be\n    y: f64be\n}\npacket reqp {\n    n: varint\n    require n == 1\n}\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let program = bytemill::ir::typed(&resolved).unwrap();
    let dir = fresh_dir("unit");
    let target = fresh_target("unit");
    bytemill::emit_rust_typed::emit_rust_typed(&resolved, &dir).unwrap();

    // Resolve every name through the printer, never by grepping.
    let varint = program
        .codecs
        .iter()
        .position(|c| c.name.snake() == "varint")
        .expect("varint codec");
    assert!(!bytemill::emit_rust_typed::codec_is_method(&program, varint));
    let decode_varint = bytemill::emit_rust_typed::decode_fn(&program, varint);
    let encode_varint = bytemill::emit_rust_typed::encode_fn(&program, varint);
    let probe = packet_codec(&program, "probe");
    let probe_ty = bytemill::emit_rust_typed::type_name_of(
        &program,
        program.codecs[probe].ty.expect("probe has a type"),
    );
    let reqp = packet_codec(&program, "reqp");
    let reqp_ty = bytemill::emit_rust_typed::type_name_of(
        &program,
        program.codecs[reqp].ty.expect("reqp has a type"),
    );

    let tests = format!(
        "use bps_generated::*;\n\
         #[test]\nfn varint_300() {{\n    let mut r = Reader::new(&[0xac, 0x02]);\n    let v = {decode_varint}(&mut r).unwrap();\n    assert_eq!(v, 300);\n    let mut w = Writer::new();\n    {encode_varint}(&mut w, 300).unwrap();\n    assert_eq!(w.into_bytes(), vec![0xac, 0x02]);\n}}\n\
         #[test]\nfn varint_neg1() {{\n    let mut r = Reader::new(&[0xff, 0xff, 0xff, 0xff, 0x0f]);\n    let v = {decode_varint}(&mut r).unwrap();\n    assert_eq!(v, -1);\n    let mut w = Writer::new();\n    {encode_varint}(&mut w, -1).unwrap();\n    assert_eq!(w.into_bytes(), vec![0xff, 0xff, 0xff, 0xff, 0x0f]);\n}}\n\
         #[test]\nfn probe_floats() {{\n    let raw: &[u8] = &[0xac, 0x02, 0x3f, 0xc0, 0x00, 0x00, 0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];\n    let mut r = Reader::new(raw);\n    let v = {probe_ty}::decode(&mut r).unwrap();\n    assert_eq!(v.n, 300);\n    assert_eq!(v.x, 1.5f32);\n    assert_eq!(v.y, 1.5f64);\n    assert_eq!(r.remaining(), 0);\n    let mut w = Writer::new();\n    v.encode(&mut w).unwrap();\n    assert_eq!(w.into_bytes(), raw);\n}}\n\
         #[test]\nfn short_buffer_is_eof() {{\n    let mut r = Reader::new(&[0xac]);\n    let err = {decode_varint}(&mut r).unwrap_err();\n    assert!(matches!(err, Error::UnexpectedEof {{ .. }}), \"{{err:?}}\");\n    let mut r = Reader::new(&[0xac]);\n    let err = {probe_ty}::decode(&mut r).unwrap_err();\n    assert!(matches!(err, Error::UnexpectedEof {{ .. }}), \"{{err:?}}\");\n}}\n\
         #[test]\nfn require_fails() {{\n    let mut r = Reader::new(&[0x02]);\n    let err = {reqp_ty}::decode(&mut r).unwrap_err();\n    assert!(matches!(err, Error::RequireFailed {{ .. }}), \"{{err:?}}\");\n    // The typed program checks `require` on decode only (the lowerer drops\n    // it from encode bodies), so encoding a failing value still writes.\n    let v = {reqp_ty} {{ n: 2 }};\n    let mut w = Writer::new();\n    v.encode(&mut w).unwrap();\n    assert_eq!(w.into_bytes(), vec![0x02]);\n}}\n"
    );
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/unit.rs"), tests).unwrap();
    gate_emitted(&dir, &target, "unit", 0, 0);
}

#[test]
fn rust_typed_26_3_packets_roundtrip() {
    let path = repo("schemas/minecraft_26_3_wiki.bps");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let program = bytemill::ir::typed(&resolved).unwrap();
    let dir = fresh_dir("26-3");
    let target = fresh_target("26-3");
    bytemill::emit_rust_typed::emit_rust_typed(&resolved, &dir).unwrap();
    assert_no_clones(&dir);
    let (body, n) = roundtrip_body(&resolved, &program);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/roundtrip.rs"), body).unwrap();
    gate_emitted(&dir, &target, "26.3", 100, n);
}

#[test]
fn rust_typed_hytale_packets_roundtrip() {
    let path = repo("schemas/hytale/2026_02_17.bps");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let program = bytemill::ir::typed(&resolved).unwrap();
    let dir = fresh_dir("hytale");
    let target = fresh_target("hytale");
    bytemill::emit_rust_typed::emit_rust_typed(&resolved, &dir).unwrap();
    assert_no_clones(&dir);
    let (body, n) = roundtrip_body(&resolved, &program);
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/roundtrip.rs"), body).unwrap();
    gate_emitted(&dir, &target, "hytale", 1, n);
}

/// `emit_rust_typed_with_edition` writes the requested edition into
/// `Cargo.toml`. Text-only: no cargo gates run here.
#[test]
fn rust_typed_edition_option() {
    let source = std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + "packet probe {\n    n: varint\n}\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    for (tag, edition) in [
        ("2021", bytemill::RustEdition::Edition2021),
        ("2024", bytemill::RustEdition::Edition2024),
    ] {
        let dir = fresh_dir(&format!("edition-{tag}"));
        bytemill::emit_rust_typed::emit_rust_typed_with_edition(&resolved, &dir, edition).unwrap();
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(
            manifest.contains(&format!("edition = \"{tag}\"")),
            "Cargo.toml must request edition {tag}:\n{manifest}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
