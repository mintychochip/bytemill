//! `emit java` generates a `bpsgen` package whose codecs decode and encode
//! byte-exactly, executed through the `java` runtime when one is available.

use std::path::{Path, PathBuf};

use bytemill::JavaVersion;

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-java-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Packets exercising scalars, floats, gating, counted lists, strings, and a
/// sum. Self-contained: varint/integer definitions plus a small UTF-8 string
/// encoding, so no std imports are needed.
fn unit_source() -> String {
    std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/integers.bps")).unwrap()
        + r#"
encoding short_string(max_chars = 100) {
    byte_len: varint
    value: bytes(byte_len) as utf8
    require utf16_units(value) <= max_chars
}
packet probe { n: varint }
packet floats { a: f32be, b: f64be }
packet gated { v: varint
 require v == 777 }
packet holder { n: varint
 ids: varint[] count = n }
packet text { s: short_string(10) }
packet picked {
 kind: u8
 v: match kind {
 0 => u8
 1 => { a: u8, b: u8 }
 _ => boolean } }
"#
}

/// Concatenated generated sources, so text assertions see the whole tree.
fn generated(dir: &Path) -> String {
    let pkg = dir.join("bpsgen");
    let mut names: Vec<PathBuf> = std::fs::read_dir(&pkg)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "java"))
        .collect();
    names.sort();
    let mut out = String::new();
    for name in names {
        out.push_str(&std::fs::read_to_string(name).unwrap());
        out.push('\n');
    }
    out
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|exe| exe.is_file())
}

/// The major version of a `java` executable, if it runs.
fn java_major(java: &Path) -> Option<u32> {
    let out = std::process::Command::new(java)
        .arg("-version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let quote = text.split('"').nth(1)?;
    let mut parts = quote.split('.');
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

fn run_output(tool: &Path, args: &[&str], files: &[PathBuf], pkg: &Path) -> std::process::Output {
    let mut command = std::process::Command::new(tool);
    command.args(args);
    for file in files {
        command.arg(file);
    }
    command.current_dir(pkg).output().unwrap()
}

fn java_sources(pkg: &Path) -> Vec<PathBuf> {
    let mut sources: Vec<PathBuf> = std::fs::read_dir(pkg)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "java"))
        .collect();
    sources.sort();
    sources
}
/// The `javac` of a JDK 8 installation, if one is present: `$JAVA8_HOME`
/// first, then `$HOME/.jdks/jdk8*`, then a `1.8` `javac` on `PATH`.
fn jdk8_javac() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA8_HOME") {
        let javac = Path::new(&home).join("bin/javac");
        if javac.is_file() {
            return Some(javac);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let jdks = Path::new(&home).join(".jdks");
        if let Ok(entries) = std::fs::read_dir(&jdks) {
            let mut names: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("jdk8"))
                })
                .collect();
            names.sort();
            for dir in names {
                let javac = dir.join("bin/javac");
                if javac.is_file() {
                    return Some(javac);
                }
            }
        }
    }
    let javac = find_on_path("javac")?;
    let out = std::process::Command::new(&javac)
        .arg("-version")
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.trim_start().starts_with("javac 1.8").then_some(javac)
}

/// Writes `Check.java` into `pkg` and runs it, returning whether any
/// toolchain ran it: `javac` plus `java` when a compiler exists, else
/// multi-file source launch on Java 22+.
fn run_check(pkg: &Path) -> bool {
    run_check_src(pkg, CHECK_JAVA)
}

/// Writes `src` as `Check.java` into `pkg` and runs it; see [`run_check`].
fn run_check_src(pkg: &Path, src: &str) -> bool {
    std::fs::write(pkg.join("Check.java"), src).unwrap();
    let sources = java_sources(pkg);
    let java = find_on_path("java");
    let javac = find_on_path("javac");
    match (javac, java) {
        (Some(javac), Some(java)) => {
            let compile = run_output(&javac, &["-d", "classes"], &sources, pkg);
            assert!(
                compile.status.success(),
                "javac failed:\n{}\n{}",
                String::from_utf8_lossy(&compile.stdout),
                String::from_utf8_lossy(&compile.stderr)
            );
            let run = std::process::Command::new(&java)
                .args(["-cp", "classes", "Check"])
                .current_dir(pkg)
                .output()
                .unwrap();
            assert!(
                run.status.success(),
                "java Check failed:\n{}\n{}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
            assert!(
                String::from_utf8_lossy(&run.stdout).contains("ok"),
                "Check printed no ok:\n{}",
                String::from_utf8_lossy(&run.stdout)
            );
            true
        }
        (None, Some(java)) => {
            if java_major(&java).unwrap_or(0) < 22 {
                return false;
            }
            let run = std::process::Command::new(&java)
                .arg("Check.java")
                .current_dir(pkg)
                .output()
                .unwrap();
            assert!(
                run.status.success(),
                "java Check.java failed:\n{}\n{}",
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
            assert!(
                String::from_utf8_lossy(&run.stdout).contains("ok"),
                "Check printed no ok:\n{}",
                String::from_utf8_lossy(&run.stdout)
            );
            true
        }
        _ => false,
    }
}

#[test]
fn java_varint_probe_roundtrip() {
    let source = unit_source();
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    for ver in [JavaVersion::Java21, JavaVersion::Java8] {
        let tag = format!("{ver:?}").to_ascii_lowercase();
        let dir = scratch(&format!("roundtrip-{tag}"));
        bytemill::emit_java::emit_java_with_version(&resolved, &dir, ver).unwrap();
        let pkg = dir.join("bpsgen");
        assert!(pkg.join("Unrouted.java").is_file(), "missing Unrouted.java");
        assert!(pkg.join("Encoding.java").is_file(), "missing Encoding.java");
        assert!(pkg.join("Reader.java").is_file(), "missing Reader.java");
        assert!(pkg.join("Writer.java").is_file(), "missing Writer.java");
        assert!(pkg.join("BpsError.java").is_file(), "missing BpsError.java");
        if !run_check(&pkg) {
            eprintln!("java_varint_probe_roundtrip: no Java toolchain; skipping {ver:?} execution");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn java_version_options() {
    let source = unit_source();
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir8 = scratch("ver8");
    bytemill::emit_java::emit_java_with_version(&resolved, &dir8, JavaVersion::Java8).unwrap();
    let dir21 = scratch("ver21");
    bytemill::emit_java::emit_java_with_version(&resolved, &dir21, JavaVersion::Java21).unwrap();
    let text8 = generated(&dir8);
    let text21 = generated(&dir21);
    assert!(
        text21.contains("public record Probe("),
        "Java 21 must use records:\n{text21}"
    );
    assert!(
        text21.contains("sealed interface"),
        "Java 21 must seal sums:\n{text21}"
    );
    assert!(
        text8.contains("public static final class Probe"),
        "Java 8 must use classes:\n{text8}"
    );
    assert!(
        text8.contains("class "),
        "Java 8 output must declare classes:\n{text8}"
    );
    assert!(
        !text8.contains("record "),
        "Java 8 output must not use records:\n{text8}"
    );
    assert!(
        !text8.contains("sealed"),
        "Java 8 output must not seal sums:\n{text8}"
    );
    assert!(
        text8.contains("interface "),
        "Java 8 sums must stay plain interfaces:\n{text8}"
    );
    let _ = std::fs::remove_dir_all(&dir8);
    let _ = std::fs::remove_dir_all(&dir21);
}

/// One driver for both record and class output: it only constructs packets
/// through all-args constructors, decodes bytes, and re-encodes byte-exactly,
/// so no accessor spelling differs between versions.
const CHECK_JAVA: &str = r#"class Check {
    static void expect(boolean ok, String message) {
        if (!ok) {
            throw new AssertionError(message);
        }
    }

    static String hex(byte[] b) {
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < b.length; i++) {
            if (i > 0) {
                sb.append(' ');
            }
            sb.append(Integer.toHexString(b[i] & 0xFF));
        }
        return sb.toString();
    }

    static void expectBytes(byte[] got, byte[] want, String message) {
        if (!java.util.Arrays.equals(got, want)) {
            throw new AssertionError(message + " got " + hex(got) + " want " + hex(want));
        }
    }

    public static void main(String[] args) {
        Writer w = new Writer();
        Encoding.encodeVarint(w, 300);
        expectBytes(w.toByteArray(), new byte[]{(byte) 0xAC, 0x02}, "varint 300");
        expect(Encoding.decodeVarint(new Reader(w.toByteArray())) == 300, "decode 300");

        Writer neg = new Writer();
        Encoding.encodeVarint(neg, -1);
        expectBytes(
            neg.toByteArray(),
            new byte[]{(byte) 0xFF, (byte) 0xFF, (byte) 0xFF, (byte) 0xFF, 0x0F},
            "varint -1");
        expect(Encoding.decodeVarint(new Reader(neg.toByteArray())) == -1, "decode -1");

        byte[] probeIn = new byte[]{(byte) 0xAC, 0x02};
        Unrouted.Probe probe = Unrouted.Probe.decode(new Reader(probeIn));
        Writer probeOut = new Writer();
        probe.encode(probeOut);
        expectBytes(probeOut.toByteArray(), probeIn, "probe roundtrip");

        Unrouted.Floats floats = new Unrouted.Floats(1.5f, -2.25);
        Writer floatsOut = new Writer();
        floats.encode(floatsOut);
        byte[] floatsWant = new byte[]{
            0x3F, (byte) 0xC0, 0x00, 0x00,
            (byte) 0xC0, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00};
        expectBytes(floatsOut.toByteArray(), floatsWant, "floats bytes");
        Unrouted.Floats floatsBack = Unrouted.Floats.decode(new Reader(floatsOut.toByteArray()));
        Writer floatsOut2 = new Writer();
        floatsBack.encode(floatsOut2);
        expectBytes(floatsOut2.toByteArray(), floatsWant, "floats re-encode");

        byte[] gatedOk = new byte[]{(byte) 0x89, 0x06};
        Unrouted.Gated gated = Unrouted.Gated.decode(new Reader(gatedOk));
        Writer gatedOut = new Writer();
        gated.encode(gatedOut);
        expectBytes(gatedOut.toByteArray(), gatedOk, "gated roundtrip");
        boolean requireThrew = false;
        try {
            Unrouted.Gated.decode(new Reader(new byte[]{(byte) 0x8A, 0x06}));
        } catch (BpsError e) {
            requireThrew = true;
        }
        expect(requireThrew, "require must fail");

        boolean shortThrew = false;
        try {
            Unrouted.Probe.decode(new Reader(new byte[]{(byte) 0xAC}));
        } catch (BpsError e) {
            shortThrew = true;
        }
        expect(shortThrew, "short buffer must fail");

        Unrouted.Holder holder = new Unrouted.Holder(2, java.util.Arrays.asList(7, 8));
        Writer holderOut = new Writer();
        holder.encode(holderOut);
        expectBytes(holderOut.toByteArray(), new byte[]{0x02, 0x07, 0x08}, "holder bytes");
        Unrouted.Holder holderBack = Unrouted.Holder.decode(new Reader(holderOut.toByteArray()));
        Writer holderOut2 = new Writer();
        holderBack.encode(holderOut2);
        expectBytes(holderOut2.toByteArray(), holderOut.toByteArray(), "holder roundtrip");

        Unrouted.Text text = new Unrouted.Text("h\u00e9llo");
        Writer textOut = new Writer();
        text.encode(textOut);
        expectBytes(
            textOut.toByteArray(),
            new byte[]{0x06, 0x68, (byte) 0xC3, (byte) 0xA9, 0x6C, 0x6C, 0x6F},
            "text bytes");
        Unrouted.Text textBack = Unrouted.Text.decode(new Reader(textOut.toByteArray()));
        Writer textOut2 = new Writer();
        textBack.encode(textOut2);
        expectBytes(textOut2.toByteArray(), textOut.toByteArray(), "text roundtrip");

        Unrouted.Picked picked =
            Unrouted.Picked.decode(new Reader(new byte[]{0x01, 0x05, 0x06}));
        Writer pickedOut = new Writer();
        picked.encode(pickedOut);
        expectBytes(pickedOut.toByteArray(), new byte[]{0x01, 0x05, 0x06}, "picked roundtrip");

        System.out.println("ok");
    }
}
"#;

/// The wrapper class for an output stem: `play_clientbound` is
/// `PlayClientbound`. Mirrors the emitter's stem naming.
fn stem_class(stem: &str) -> String {
    let mut out = String::new();
    for piece in stem.split('_') {
        let mut chars = piece.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.is_empty() {
        out.push('X');
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Whether the generated tree declares a nested type called `base`: records,
/// sums (sealed or plain interfaces), and enums (static final classes).
fn declares_type(generated: &str, base: &str) -> bool {
    [
        format!("record {base}("),
        format!("interface {base} "),
        format!("final class {base} "),
    ]
    .iter()
    .any(|pat| generated.contains(pat))
}

/// Round-trip every synthesizable packet of `schema_rel`: the interpreter's
/// bytes into the generated `decode`, back through the generated `encode`,
/// byte-compared. Only decode/encode calls appear, so no constructor or
/// accessor spelling matters.
fn roundtrip_schema_packets(schema_rel: &str, tag: &str, min_packets: usize) {
    use bytemill::ir::typed::TypeKind;

    let resolved = bytemill::analyze(&bytemill::load_schema(&[repo(schema_rel)]).unwrap()).unwrap();
    let program = bytemill::ir::typed::typed(&resolved).unwrap();
    let dir = scratch(&format!("packets-{tag}"));
    bytemill::emit_java::emit_java_with_version(&resolved, &dir, JavaVersion::Java21).unwrap();
    let pkg = dir.join("bpsgen");
    let text = generated(&dir);
    let mut body = String::from("class Check {\n    public static void main(String[] args) {\n");
    let mut n = 0;
    for packet in resolved.packets.values() {
        let Some(codec) = program.packet_codec(&packet.name) else {
            continue;
        };
        if !program.codecs[codec].params.is_empty() {
            continue;
        }
        let value = match bytemill::synth::synthesize_packet(&resolved, packet) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let bytes = match bytemill::interp::encode_packet(&resolved, &packet.name, &value) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        // Records expose `Type.decode` plus an instance `encode`; sums,
        // enums, and free codecs use static calls on the stem wrapper.
        let (decode, encode) = if let Some(ty) = program.codecs[codec].ty {
            let mut target = ty;
            while let TypeKind::Alias { target: next } = &program.types[target].kind {
                target = *next;
            }
            let owner = stem_class(&program.output_stem(&program.types[target].origin));
            let base = program.types[target].name.pascal();
            if !declares_type(&text, &base) {
                continue;
            }
            // A claimed suffix (`base2`, …) means another type shares the
            // spelling: skip rather than reference the wrong type.
            if (2..20).any(|k| declares_type(&text, &format!("{base}{k}"))) {
                continue;
            }
            let rf = format!("{owner}.{base}");
            let decode = format!("{rf}.decode(new Reader(raw{n}))");
            let encode = if matches!(program.types[target].kind, TypeKind::Record { .. }) {
                format!("v{n}.encode(w{n})")
            } else {
                format!("{rf}.encode(w{n}, v{n})")
            };
            (decode, encode)
        } else {
            let owner = stem_class(&program.output_stem(&program.codecs[codec].origin));
            let fname = program.codecs[codec].name.pascal();
            if !text.contains(&format!("decode{fname}(")) {
                continue;
            }
            (
                format!("{owner}.decode{fname}(new Reader(raw{n}))"),
                format!("{owner}.encode{fname}(w{n}, v{n})"),
            )
        };
        let raw = bytes
            .iter()
            .map(|b| format!("(byte) 0x{b:02X}"))
            .collect::<Vec<_>>()
            .join(", ");
        body.push_str(&format!(
            "        byte[] raw{n} = new byte[]{{{raw}}};\n        var v{n} = {decode};\n        Writer w{n} = new Writer();\n        {encode};\n        if (!java.util.Arrays.equals(w{n}.toByteArray(), raw{n})) {{\n            throw new AssertionError({name:?} + \" bytes differ\");\n        }}\n",
            name = packet.name,
        ));
        n += 1;
    }
    assert!(
        n >= min_packets,
        "only {n} packets round-tripped for {schema_rel}"
    );
    body.push_str("        System.out.println(\"ok\");\n    }\n}\n");
    if !run_check_src(&pkg, &body) {
        eprintln!("roundtrip_schema_packets: no Java toolchain; skipping {schema_rel} execution");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn java_26_3_packets_roundtrip() {
    roundtrip_schema_packets("schemas/minecraft_26_3_wiki.bps", "26-3", 100);
}

#[test]
fn java_hytale_packets_roundtrip() {
    roundtrip_schema_packets("schemas/hytale/2026_02_17.bps", "hytale", 1);
}
#[test]
fn java_8_26_3_builds() {
    let Some(javac) = jdk8_javac() else {
        eprintln!("java_8_26_3_builds: no JDK 8; skipping");
        return;
    };
    let java = javac.with_file_name("java");
    let resolved =
        bytemill::analyze(&bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bps")]).unwrap())
            .unwrap();
    let dir = scratch("jdk8-26-3");
    bytemill::emit_java::emit_java_with_version(&resolved, &dir, JavaVersion::Java8).unwrap();
    let pkg = dir.join("bpsgen");
    let sources = java_sources(&pkg);
    assert!(sources.len() >= 10, "only {} .java files", sources.len());
    std::fs::write(pkg.join("Check.java"), CHECK_JAVA8).unwrap();
    let sources = java_sources(&pkg);
    std::fs::create_dir_all(pkg.join("classes")).unwrap();
    let compile = run_output(&javac, &["-d", "classes"], &sources, &pkg);
    assert!(
        compile.status.success(),
        "JDK 8 javac failed:\n{}\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = std::process::Command::new(&java)
        .args(["-cp", "classes", "Check"])
        .current_dir(&pkg)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "JDK 8 java Check failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("java8 smoke ok"),
        "Check printed no marker:\n{}",
        String::from_utf8_lossy(&run.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Java 8 smoke driver for the 26.3 tree: explicit types only (no `var`).
const CHECK_JAVA8: &str = r#"class Check {
    public static void main(String[] args) {
        Writer w = new Writer();
        Encoding.encodeVarint(w, 300);
        byte[] enc = w.toByteArray();
        if (enc.length != 2 || enc[0] != (byte) 0xAC || enc[1] != 0x02) {
            throw new AssertionError("varint 300 bytes differ");
        }
        int back = Encoding.decodeVarint(new Reader(enc));
        if (back != 300) {
            throw new AssertionError("varint 300 decode differ");
        }
        byte[] raw = new byte[]{0, 0, 0, 0, 0, 0, 1, 44};
        Shared.KeepAlive ka = Shared.KeepAlive.decode(new Reader(raw));
        if (ka.getKeepAliveId() != 300L) {
            throw new AssertionError("KeepAlive id differ");
        }
        Writer out = new Writer();
        ka.encode(out);
        byte[] again = out.toByteArray();
        if (again.length != 8 || !java.util.Arrays.equals(again, raw)) {
            throw new AssertionError("KeepAlive bytes differ");
        }
        System.out.println("java8 smoke ok");
    }
}
"#;
