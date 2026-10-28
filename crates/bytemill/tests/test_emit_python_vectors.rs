//! Vectors roundtrips for `emit_python_with_vectors` (and F7 little-endian floats).

use std::path::PathBuf;
use std::process::Command;

fn repo(rel: &str) -> PathBuf {
    std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../")).join(rel)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bml-py-vec-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn py_compile(dir: &std::path::Path) {
    let out = Command::new("python3")
        .args(["-m", "compileall", "-q", "bmlgen"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "py_compile failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn py_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.join("bmlgen")];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "py") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn run_vectors(dir: &std::path::Path, tr: &bytemill::vectors::TestRun) {
    let args: Vec<&str> = tr.command.split_whitespace().collect();
    assert!(!args.is_empty(), "empty command");
    let prog = args[0];
    let rest = &args[1..];
    let out = Command::new(prog)
        .args(rest)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "test_vectors failed\ncmd: {}\n{}\n{}",
        tr.command,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn ruff_format_check(dir: &std::path::Path) {
    let out = match Command::new("ruff")
        .args(["format", "--check", "bmlgen", "test_vectors.py"])
        .current_dir(dir)
        .output()
    {
        Ok(o) => o,
        Err(_) => return, // tool not present, skip quietly
    };
    assert!(
        out.status.success(),
        "ruff format --check failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ruff_check(dir: &std::path::Path) {
    let out = match Command::new("ruff")
        .args(["check", "bmlgen", "test_vectors.py"])
        .current_dir(dir)
        .output()
    {
        Ok(o) => o,
        Err(_) => return,
    };
    assert!(
        out.status.success(),
        "ruff check failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn mypy_strict(dir: &std::path::Path) {
    let out = match Command::new("python3")
        .args(["-m", "mypy", "--strict", "bmlgen", "test_vectors.py"])
        .current_dir(dir)
        .output()
    {
        Ok(o) => o,
        Err(_) => return,
    };
    assert!(
        out.status.success(),
        "mypy --strict failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn python_hytale_vectors() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/hytale/2026_02_17.bml")]).unwrap(),
    )
    .unwrap();
    let vs =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    assert!(
        !vs.vectors.is_empty(),
        "hytale must produce at least one vector"
    );
    let dir = scratch("hytale");
    let some = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        bytemill::PythonVersion::default(),
        &vs.vectors,
    )
    .unwrap();
    let tr = some.expect("vectors present");
    assert!(tr.vectors >= 1);
    py_compile(&dir);
    run_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn python_minecraft_vectors() {
    let resolved = bytemill::analyze(
        &bytemill::load_schema(&[repo("schemas/minecraft_26_3_wiki.bml")]).unwrap(),
    )
    .unwrap();
    let vs =
        bytemill::vectors::generate(&resolved, &bytemill::vectors::VectorSpec::default()).unwrap();
    let dir = scratch("mc26");
    let some = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        bytemill::PythonVersion::default(),
        &vs.vectors,
    )
    .unwrap();
    let tr = some.expect("vectors present");
    assert!(
        tr.vectors >= 664,
        "minecraft must embed >=664 vectors (includes EOF-skipped conditional groups)"
    );
    py_compile(&dir);
    let cmd_out = Command::new("python3")
        .args(["-m", "unittest", "test_vectors"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        cmd_out.status.success(),
        "must exit 0 (no more known bad vectors)"
    );
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&cmd_out.stdout),
        String::from_utf8_lossy(&cmd_out.stderr)
    );
    assert!(out.contains("OK"), "unittest output must report OK");
    ruff_format_check(&dir);
    ruff_check(&dir);
    mypy_strict(&dir);
    let _ = std::fs::remove_dir_all(&dir);
}
#[test]
fn python_le_be_vectors() {
    // f32le(1.5) = 0000c03f
    // f64le(2.25) = 0000000000000240
    // f32be(1.5) = 3fc00000
    // f64be(2.25) = 4002000000000000
    let source = "packet probe { a: f32le b: f64le c: f32be d: f64be }";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let vectors = vec![bytemill::vectors::Vector {
        packet: "probe".to_string(),
        route: None,
        hex: "0000c03f00000000000002403fc000004002000000000000".to_string(),
    }];
    let dir = scratch("lebe");
    let some = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        bytemill::PythonVersion::default(),
        &vectors,
    )
    .unwrap();
    let tr = some.expect("vector present");
    assert_eq!(tr.vectors, 1);
    py_compile(&dir);
    run_vectors(&dir, &tr);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn python_negative_controls() {
    let source = std::fs::read_to_string(repo("schemas/std/integers.bml")).unwrap()
        + "packet probe { n: u16be }";
    let resolved = bytemill::analyze(&bytemill::parse(&source).unwrap()).unwrap();
    let dir = scratch("neg");
    let _ = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        bytemill::PythonVersion::default(),
        &[],
    )
    .unwrap();
    // good payload passes
    std::fs::write(
        dir.join("good.py"),
        "import bmlgen\nv = bmlgen.Probe.decode(bmlgen.Reader(bytes([0x01, 0x02])))\nw = bmlgen.Writer()\nv.encode(w)\nassert w.get_bytes() == bytes([0x01, 0x02])\nprint('good')\n",
    )
    .unwrap();
    let out = Command::new("python3")
        .arg("good.py")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "good payload must pass");
    // truncated fails (non-zero exit when uncaught)
    std::fs::write(
        dir.join("bad_trunc.py"),
        "import bmlgen\nbmlgen.Probe.decode(bmlgen.Reader(bytes([0x01])))\n",
    )
    .unwrap();
    let out = Command::new("python3")
        .arg("bad_trunc.py")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "truncated payload must fail");
    // trailing fails
    std::fs::write(
        dir.join("bad_trail.py"),
        "import bmlgen\nr = bmlgen.Reader(bytes([0x01, 0x02, 0x03]))\nv = bmlgen.Probe.decode(r)\nif r.remaining != 0: raise Exception('trailing')\n",
    )
    .unwrap();
    let out = Command::new("python3")
        .arg("bad_trail.py")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(!out.status.success(), "trailing payload must fail");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn python_empty_vectors_none_and_identical() {
    let source = "packet p { x: u8 }";
    let resolved = bytemill::analyze(&bytemill::parse(source).unwrap()).unwrap();
    let dir = scratch("emptyv");
    let res = bytemill::emit_python::emit_python_with_vectors(
        &resolved,
        &dir,
        bytemill::PythonVersion::default(),
        &[],
    )
    .unwrap();
    assert!(res.is_none(), "empty vectors must yield None");
    let dir2 = scratch("emptybase");
    bytemill::emit_python::emit_python_with_version(
        &resolved,
        &dir2,
        bytemill::PythonVersion::default(),
    )
    .unwrap();
    let f1 = py_files(&dir);
    let f2 = py_files(&dir2);
    assert_eq!(f1.len(), f2.len(), "same file count");
    for (a, b) in f1.iter().zip(f2.iter()) {
        assert_eq!(a.file_name(), b.file_name());
        assert_eq!(
            std::fs::read(a).unwrap(),
            std::fs::read(b).unwrap(),
            "byte identical for {}",
            a.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}
