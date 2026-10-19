//! `emit csharp` produces a `BpsGen` C# library: a runtime plus one file per
//! route category, with record structs (or plain structs on old targets) whose
//! `Decode`/`Encode` pairs follow the typed program.

use std::path::{Path, PathBuf};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bps-cs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn std_source() -> String {
    std::fs::read_to_string(repo("schemas/std/integers.bps")).unwrap()
        + &std::fs::read_to_string(repo("schemas/std/varint.bps")).unwrap()
}

/// Every generated `.cs` file under `BpsGen`, concatenated, so assertions see
/// the whole tree.
fn generated(dir: &Path) -> String {
    let root = dir.join("BpsGen");
    let mut files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cs"))
        .collect();
    files.sort();
    let mut out = String::new();
    for file in files {
        out.push_str(&format!(
            "// ---- {} ----\n",
            file.file_name().unwrap().to_string_lossy()
        ));
        out.push_str(&std::fs::read_to_string(&file).unwrap());
        out.push('\n');
    }
    out
}

#[test]
fn csharp_varint_probe_roundtrip() {
    let source = std_source() + "packet probe { n: varint }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir = scratch("probe");
    bytemill::emit_csharp::emit_csharp_with_version(&resolved, &dir, bytemill::CSharpVersion::CSharp12)
        .unwrap();

    let root = dir.join("BpsGen");
    assert!(
        root.join("Runtime.cs").is_file(),
        "missing BpsGen/Runtime.cs"
    );
    let runtime = std::fs::read_to_string(root.join("Runtime.cs")).unwrap();
    assert!(
        runtime.contains("namespace BpsGen"),
        "runtime needs the BpsGen namespace:\n{runtime}"
    );
    assert!(
        runtime.contains("public class BpsError : Exception"),
        "runtime needs BpsError:\n{runtime}"
    );
    assert!(
        runtime.contains("public ref struct Reader")
            || runtime.contains("public class Reader")
            || runtime.contains("public struct Reader"),
        "runtime needs a Reader:\n{runtime}"
    );
    for method in ["ReadU8()", "ReadBytes(", "ReadF32()", "ReadF64()"] {
        assert!(
            runtime.contains(method),
            "runtime Reader needs {method}:\n{runtime}"
        );
    }
    assert!(
        runtime.contains("public sealed class Writer") || runtime.contains("public class Writer"),
        "runtime needs a Writer:\n{runtime}"
    );
    for method in ["WriteU8(", "WriteBytes(", "WriteF32(", "ToByteArray()"] {
        assert!(
            runtime.contains(method),
            "runtime Writer needs {method}:\n{runtime}"
        );
    }

    let text = generated(&dir);
    // The probe packet is a record struct with a Decode/Encode pair wired to
    // the varint codec, so generated decode feeds generated encode.
    assert!(
        text.contains("public readonly record struct Probe"),
        "probe record:\n{text}"
    );
    assert!(
        text.contains("public static Probe Decode(ref Reader"),
        "probe Decode:\n{text}"
    );
    assert!(
        text.contains("public void Encode(Writer"),
        "probe Encode:\n{text}"
    );
    assert!(text.contains("VarintCodec"), "varint codec:\n{text}");
    assert!(
        text.contains("VarintCodec.Decode(ref r"),
        "probe decode reads varint:\n{text}"
    );
    assert!(
        text.contains("VarintCodec.Encode(w"),
        "probe encode writes varint:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_version_options() {
    let source = std_source()
        + "packet probe { n: varint, tags: varint[] count = n }\n\
           packet floats { a: f32be, b: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();

    let old = scratch("v8");
    bytemill::emit_csharp::emit_csharp_with_version(&resolved, &old, bytemill::CSharpVersion::CSharp8)
        .unwrap();
    let eight = generated(&old);
    assert!(
        !eight.contains("record struct"),
        "C# 8 must not use record structs:\n{eight}"
    );
    assert!(
        eight.contains("public struct Probe"),
        "C# 8 probe struct:\n{eight}"
    );
    assert!(
        eight.contains("public struct Floats"),
        "C# 8 floats struct:\n{eight}"
    );
    assert!(eight.contains("List<int>"), "C# 8 lists:\n{eight}");

    let new = scratch("v12");
    bytemill::emit_csharp::emit_csharp_with_version(&resolved, &new, bytemill::CSharpVersion::CSharp12)
        .unwrap();
    let twelve = generated(&new);
    assert!(
        twelve.contains("public readonly record struct Probe"),
        "C# 12 probe record struct:\n{twelve}"
    );
    assert!(
        twelve.contains("public readonly record struct Floats"),
        "C# 12 floats record struct:\n{twelve}"
    );

    for (label, text) in [("C# 8", &eight), ("C# 12", &twelve)] {
        assert!(
            text.contains("namespace BpsGen"),
            "{label} namespace:\n{text}"
        );
        assert!(
            text.contains("public static Probe Decode(ref Reader"),
            "{label} Decode:\n{text}"
        );
        assert!(
            text.contains("public void Encode(Writer"),
            "{label} Encode:\n{text}"
        );
    }
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(&new);
}

/// `~/.dotnet/dotnet` when installed; compile gates are skipped without it.
fn dotnet() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let bin = Path::new(&home).join(".dotnet/dotnet");
    bin.is_file().then_some(bin)
}

/// Emit a shipped schema with `ver` into a scratch dir.
fn emit_schema(schema_rel: &str, tag: &str, ver: bytemill::CSharpVersion) -> PathBuf {
    let path = repo(schema_rel);
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let dir = scratch(tag);
    bytemill::emit_csharp::emit_csharp_with_version(&resolved, &dir, ver).unwrap();
    dir
}

/// No value-type record may hold itself directly: a `T?` field on a struct
/// is `Nullable<T>`, still a value type, so the layout is infinite
/// (CS0523). `List<T>` fields are references and are fine.
fn assert_no_struct_self_reference(text: &str) {
    let mut bad = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t
            .strip_prefix("public readonly record struct ")
            .or_else(|| t.strip_prefix("public record struct "))
            .or_else(|| t.strip_prefix("public struct "))
        {
            let name: String = name
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            current = Some(name);
            continue;
        }
        if t == "}" {
            current = None;
            continue;
        }
        let Some(owner) = &current else {
            continue;
        };
        if !(t.starts_with("public ") && t.contains("{ get;")) {
            continue;
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        if toks.len() < 2 {
            continue;
        }
        let ty = toks[1].trim_end_matches('?');
        if ty == owner {
            bad.push(format!("struct {owner} holds itself directly: {t}"));
        }
    }
    assert!(
        bad.is_empty(),
        "self-referential structs:\n{}",
        bad.join("\n")
    );
}

/// Compile the emitted `BpsGen` tree with `dotnet build` (`net8.0`,
/// `LangVersion {lang}`), asserting success with no CS0523/CS8669.
fn build_gate(dir: &Path, dotnet: &Path, lang: &str) {
    let csproj = format!(
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <LangVersion>{lang}</LangVersion>\n    <OutputType>Library</OutputType>\n    <Nullable>disable</Nullable>\n    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n  </PropertyGroup>\n  <ItemGroup>\n    <Compile Include=\"BpsGen/**/*.cs\" />\n  </ItemGroup>\n</Project>\n"
    );
    std::fs::write(dir.join("bpsgen.csproj"), csproj).unwrap();
    let out = std::process::Command::new(dotnet)
        .args(["build", "bpsgen.csproj", "--nologo", "-v", "q"])
        .current_dir(dir)
        .output()
        .unwrap();
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "dotnet build failed:\n{log}");
    assert!(!log.contains("CS0523"), "struct layout cycle:\n{log}");
    assert!(
        !log.contains("CS8669"),
        "nullable annotation warning:\n{log}"
    );
}

#[test]
fn csharp_26_3_packets_roundtrip() {
    let dir = emit_schema(
        "schemas/minecraft_26_3_wiki.bps",
        "26-3",
        bytemill::CSharpVersion::CSharp12,
    );
    let text = generated(&dir);
    // `equipment_entry` chains through an optional `next` field: a
    // value-layout cycle, so it must be a reference record.
    assert!(
        text.contains("public record EquipmentEntry"),
        "EquipmentEntry must be a reference record:\n{text}"
    );
    assert_no_struct_self_reference(&text);
    if let Some(d) = dotnet() {
        build_gate(&dir, &d, "12");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_hytale_packets_roundtrip() {
    let dir = emit_schema(
        "schemas/hytale/2026_02_17.bps",
        "hytale",
        bytemill::CSharpVersion::CSharp12,
    );
    let text = generated(&dir);
    assert_no_struct_self_reference(&text);
    if let Some(d) = dotnet() {
        build_gate(&dir, &d, "12");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_8_probe_builds() {
    let source = std_source() + "packet probe { n: varint }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir = scratch("v8-build");
    bytemill::emit_csharp::emit_csharp_with_version(&resolved, &dir, bytemill::CSharpVersion::CSharp8)
        .unwrap();
    let text = generated(&dir);
    assert!(
        text.contains("public struct Probe"),
        "C# 8 probe struct:\n{text}"
    );
    assert_no_struct_self_reference(&text);
    if let Some(d) = dotnet() {
        build_gate(&dir, &d, "8");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_26_3_runtime_roundtrip() {
    let dir = emit_schema(
        "schemas/minecraft_26_3_wiki.bps",
        "26-3-rt",
        bytemill::CSharpVersion::CSharp12,
    );
    let text = generated(&dir);
    assert!(
        text.contains("record struct KeepAlive"),
        "KeepAlive record:\n{text}"
    );
    assert!(text.contains("class VarintCodec"), "varint codec:\n{text}");
    let Some(dotnet) = dotnet() else {
        eprintln!("csharp_26_3_runtime_roundtrip: no dotnet; skipping execution");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    std::fs::write(
        dir.join("bpsgen.csproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n    <LangVersion>12</LangVersion>\n    <OutputType>Exe</OutputType>\n    <Nullable>disable</Nullable>\n    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n    <StartupObject>Check</StartupObject>\n  </PropertyGroup>\n  <ItemGroup>\n    <Compile Include=\"BpsGen/**/*.cs\" />\n    <Compile Include=\"Check.cs\" />\n  </ItemGroup>\n</Project>\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Check.cs"),
        "using System;\nusing BpsGen;\n\nstatic class Check\n{\n    public static void Main()\n    {\n        Writer w = new Writer();\n        VarintCodec.Encode(w, 300);\n        byte[] enc = w.ToByteArray();\n        if (enc.Length != 2 || enc[0] != 0xAC || enc[1] != 0x02)\n        {\n            throw new Exception(\"varint 300 bytes differ\");\n        }\n        Reader r0 = new Reader(enc);\n        int back = VarintCodec.Decode(ref r0);\n        if (back != 300)\n        {\n            throw new Exception(\"varint 300 decode differ\");\n        }\n        byte[] raw = new byte[] { 0, 0, 0, 0, 0, 0, 1, 44 };\n        Reader r1 = new Reader(raw);\n        KeepAlive p = KeepAlive.Decode(ref r1);\n        if (p.KeepAliveId != 300L)\n        {\n            throw new Exception(\"KeepAlive id differ\");\n        }\n        Writer w2 = new Writer();\n        p.Encode(w2);\n        byte[] again = w2.ToByteArray();\n        if (again.Length != 8)\n        {\n            throw new Exception(\"KeepAlive bytes differ\");\n        }\n        for (int i = 0; i < 8; i++)\n        {\n            if (again[i] != raw[i])\n            {\n                throw new Exception(\"KeepAlive bytes differ\");\n            }\n        }\n        Console.WriteLine(\"csharp smoke ok\");\n    }\n}\n",
    )
    .unwrap();
    let build = std::process::Command::new(&dotnet)
        .args(["build", "bpsgen.csproj", "--nologo", "-v", "q"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "dotnet build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let run = std::process::Command::new(&dotnet)
        .args(["run", "--no-build", "-v", "q"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dotnet run failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("csharp smoke ok"),
        "Check printed no marker:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
