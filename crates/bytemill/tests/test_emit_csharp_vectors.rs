//! `emit csharp --vectors` ships Check.cs + bmlgen.csproj (no NuGet) and a
//! TestRun describing how to execute the native roundtrip over embedded vectors
//! using `dotnet`. The emitted Check uses strict consume checks via Reader.Remaining.

use std::path::{Path, PathBuf};

fn repo(rel: &str) -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-cs-vec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn dotnet() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let bin = Path::new(&home).join(".dotnet/dotnet");
    bin.is_file().then_some(bin)
}

fn written_bmlgen_files(dir: &Path) -> Vec<(String, String)> {
    let root = dir.join("BmlGen");
    let mut files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("cs"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for p in files {
        let name = format!("BmlGen/{}", p.file_name().unwrap().to_string_lossy());
        let content = std::fs::read_to_string(&p).unwrap();
        out.push((name, content));
    }
    out
}

#[test]
fn csharp_hytale_with_vectors_runs() {
    let path = repo("schemas/hytale/2026_02_17.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    assert!(
        !set.vectors.is_empty(),
        "hytale generator must produce vectors"
    );
    let dir = scratch("hytale");
    let tr = bytemill::emit_csharp::emit_csharp_with_vectors(
        &resolved,
        &dir,
        bytemill::CSharpVersion::CSharp12,
        &set.vectors,
    )
    .unwrap();
    let tr = match tr {
        Some(t) => t,
        None => panic!("expected Some(TestRun) for non-empty vectors"),
    };
    assert!(tr.vectors >= 1, "embedded {} vectors", tr.vectors);
    assert_eq!(tr.file, "Check.cs");
    assert!(tr.command.contains("dotnet"));
    let Some(d) = dotnet() else {
        eprintln!("csharp_hytale_with_vectors_runs: no dotnet; skipping execution");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let args: Vec<&str> = tr.command.split_whitespace().collect();
    assert_eq!(args[0], "dotnet");
    let status = std::process::Command::new(&d)
        .args(&args[1..])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(status.success(), "dotnet run failed for hytale vectors");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_minecraft_with_vectors_embeds_many() {
    let path = repo("schemas/minecraft_26_3_wiki.bml");
    let resolved = bytemill::analyze(&bytemill::load_schema(&[path]).unwrap()).unwrap();
    let set =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = scratch("mc26");
    let tr = bytemill::emit_csharp::emit_csharp_with_vectors(
        &resolved,
        &dir,
        bytemill::CSharpVersion::CSharp12,
        &set.vectors,
    )
    .unwrap();
    let tr = match tr {
        Some(t) => t,
        None => panic!("expected Some for minecraft vectors"),
    };
    assert!(
        tr.vectors >= 50,
        "minecraft must embed at least 50 vectors, got {}",
        tr.vectors
    );
    let Some(d) = dotnet() else {
        eprintln!("csharp_minecraft_with_vectors_embeds_many: no dotnet; skipping");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let args: Vec<&str> = tr.command.split_whitespace().collect();
    let status = std::process::Command::new(&d)
        .args(&args[1..])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(status.success(), "minecraft vectors test run failed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_le_be_floats_hand_vectors_roundtrip() {
    // f32le 1.5 bytes: 00 00 c0 3f
    // f64le 2.25 bytes: 00 00 00 00 00 00 02 40
    // f32be 1.5 bytes: 3f c0 00 00
    // f64be 2.25 bytes: 40 02 00 00 00 00 00 00
    let source = "packet probe { a: f32le  b: f64le  c: f32be  d: f64be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let program = bytemill::ir::typed(&resolved).unwrap();
    let c = program.packet_codec("probe").unwrap();
    let dec = bytemill::emit_csharp::decode_fn(&program, c);
    let enc = bytemill::emit_csharp::encode_fn(&program, c);
    let uses_rec = bytemill::emit_csharp::codec_uses_record_method(&program, c);
    assert!(
        dec.ends_with(".Decode"),
        "decode_fn must end with .Decode: {dec}"
    );
    assert!(
        enc == "Encode" || enc.ends_with(".Encode"),
        "encode_fn shape for record vs codec: {enc}"
    );
    assert!(uses_rec, "probe should use record method style");
    let vectors = vec![bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
    }];
    let dir = scratch("floats");
    let tr = bytemill::emit_csharp::emit_csharp_with_vectors(
        &resolved,
        &dir,
        bytemill::CSharpVersion::CSharp8,
        &vectors,
    )
    .unwrap();
    let tr = match tr {
        Some(t) => t,
        None => panic!("expected vectors result"),
    };
    assert_eq!(tr.vectors, 1);
    let Some(d) = dotnet() else {
        eprintln!("csharp_le_be_floats... : no dotnet; skipping");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let args: Vec<&str> = tr.command.split_whitespace().collect();
    let status = std::process::Command::new(&d)
        .args(&args[1..])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(status.success(), "le/be floats hand vector run failed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_negative_truncation_and_trailing_fail() {
    let integers = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap();
    let source = integers + "packet probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let vectors = vec![
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "0102".to_string(),
        },
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "01".to_string(),
        },
        bytemill::vectors::Vector {
            packet: "probe".to_string(),
            route: None,
            hex: "010203".to_string(),
        },
    ];
    let dir = scratch("neg");
    let tr = bytemill::emit_csharp::emit_csharp_with_vectors(
        &resolved,
        &dir,
        bytemill::CSharpVersion::CSharp12,
        &vectors,
    )
    .unwrap();
    let tr = match tr {
        Some(t) => t,
        None => panic!("expected Some even when some vectors will fail at runtime"),
    };
    assert_eq!(tr.vectors, 3);
    let Some(d) = dotnet() else {
        eprintln!("csharp_negative... : no dotnet; skipping");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let args: Vec<&str> = tr.command.split_whitespace().collect();
    let status = std::process::Command::new(&d)
        .args(&args[1..])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(
        !status.success(),
        "negative vectors must cause non-zero exit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csharp_empty_vectors_is_none_and_files_match_with_version() {
    let integers = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap();
    let source = integers + "packet probe { n: u16be }\n";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir_vec = scratch("empty-vec");
    let res = bytemill::emit_csharp::emit_csharp_with_vectors(
        &resolved,
        &dir_vec,
        bytemill::CSharpVersion::CSharp8,
        &[],
    )
    .unwrap();
    assert!(res.is_none(), "empty vectors must return None");
    let from_vec = written_bmlgen_files(&dir_vec);

    let dir_ver = scratch("empty-ver");
    bytemill::emit_csharp::emit_csharp_with_version(
        &resolved,
        &dir_ver,
        bytemill::CSharpVersion::CSharp8,
    )
    .unwrap();
    let from_ver = written_bmlgen_files(&dir_ver);

    assert_eq!(
        from_vec, from_ver,
        "empty vectors must produce identical BmlGen files as with_version"
    );
    let _ = std::fs::remove_dir_all(&dir_vec);
    let _ = std::fs::remove_dir_all(&dir_ver);
}
