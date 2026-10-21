//! Ports tests/test_frames_gen.py: generated Rust frame walkers vs the interpreter.
//!
//! The interpreter is the oracle: random valid payloads (drawn by decoding from
//! a random byte stream) are measured by the compiled Rust walker, and the two
//! lengths must agree, with trailing bytes after the payload the walker leaves
//! alone.
//!
//! Deviation from the Python test: the random byte stream is a fixed large
//! buffer with a small deterministic PRNG (seeded by the encoding name) instead
//! of an endless cursor over Python's `random.Random`. Payloads longer than
//! `MAX_PAYLOAD` are discarded, as the Python `_ensure` cap does. The check's
//! meaning — walker length == interpreter length on the same bytes — is
//! unchanged.
//!
//! The Python test also patches `engine._decode_repeat` to reject a count past the payload
//! cap: a list of zero-width elements would loop that many times without reading a byte.
//! `Cursor::repeat_limit` is the Rust equivalent; without it one encoding allocates until
//! the machine is out of memory.

mod common;

use bytemill::ast::TypeRef;
use bytemill::emit_rstom::{frames, FRAMES_RS};
use bytemill::errors::ErrorKind;
use bytemill::frames_gen::{generate, Walkers};
use bytemill::interp::cursor::Cursor;
use bytemill::interp::{decode_type_ref};

const SAMPLES: usize = 300;
const TRAILING: [u8; 3] = [0xAA, 0xBB, 0xCC];
const MAX_PAYLOAD: usize = 160;
/// The Python cursor errors past `MAX_PAYLOAD` bytes, so an attempt never walks
/// further; a fixed stream of exactly that length reproduces the bound.
const STREAM: usize = MAX_PAYLOAD;

/// A tiny deterministic PRNG (splitmix64), seeded by the encoding's name.
struct Rng(u64);

impl Rng {
    fn new(seed: &str) -> Self {
        let mut h = 0xcbf29ce484222325u64;
        for b in seed.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100000001b3);
        }
        Rng(h)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    /// The Python `_byte`: mostly small bytes, occasionally printable ones.
    fn byte(&mut self) -> u8 {
        let roll = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        if roll < 0.5 {
            (self.next() % 4) as u8
        } else if roll < 0.8 {
            4 + (self.next() % 13) as u8
        } else {
            0x20 + (self.next() % 0x5f) as u8
        }
    }
}

/// Random valid payloads of `name`: a decode that succeeds consumed one.
fn valid_payloads(
    resolved: &bytemill::ResolvedSchema,
    name: &str,
    count: usize,
) -> Vec<Vec<u8>> {
    let mut rng = Rng::new(name);
    let mut out = Vec::new();
    let mut attempts = 0;
    while out.len() < count && attempts < count * 60 {
        attempts += 1;
        let stream: Vec<u8> = (0..STREAM).map(|_| rng.byte()).collect();
        let mut cur = Cursor::new(&stream);
        cur.repeat_limit = Some(MAX_PAYLOAD);
        let ref_ = TypeRef { name: name.to_string(), args: Vec::new(), transforms: Vec::new() };
        match decode_type_ref(&ref_, &mut cur, resolved, &bytemill::Env::new()) {
            Ok(_) => {
                if cur.pos >= 1 && cur.pos <= MAX_PAYLOAD {
                    out.push(stream[..cur.pos].to_vec());
                }
            }
            // A clean decode failure (require/EOF) is expected; a crash is not.
            Err(e) => {
                assert!(
                    matches!(e.kind, ErrorKind::Decode | ErrorKind::Encode),
                    "{name}: non-clean failure {e}"
                );
            }
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Compile `rust_frames` and compare `nbt_tag_len` to hand-computed lengths.
fn nbt_tag_len_mismatches(rust_frames: &str, cases: &[(Vec<u8>, usize)]) -> Vec<String> {
    let table: String = cases
        .iter()
        .map(|(payload, want)| format!("        (\"{}\", {want}),", hex(payload)))
        .collect::<Vec<_>>()
        .join("\n");
    let main = format!(
        r#"mod frames;

fn unhex(s: &str) -> Vec<u8> {{
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}}

fn main() {{
    let cases: &[(&str, usize)] = &[
{table}
    ];
    let mut bad = 0;
    for (hex, want) in cases {{
        let bytes = unhex(hex);
        let got = frames::nbt_tag_len(&bytes);
        if got != *want {{
            println!("nbt_tag_len {{got}} != {{want}} for {{hex}}");
            bad += 1;
        }}
    }}
    if bad > 0 {{
        std::process::exit(1);
    }}
}}
"#
    );
    compile_and_run(rust_frames, &main)
}

/// Walker length != interpreter length, over random valid payloads of each name.
fn differences(
    resolved: &bytemill::ResolvedSchema,
    names: &[(String, String)],
    rust_frames: &str,
) -> Vec<String> {
    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, _) in names {
        let payloads = valid_payloads(resolved, name, SAMPLES);
        if payloads.len() < SAMPLES / 4 {
            return vec![format!(
                "{name}: only {} random payloads decoded, the check would be empty",
                payloads.len()
            )];
        }
        cases.extend(payloads.into_iter().map(|p| (name.clone(), p)));
    }
    let arms: String = names
        .iter()
        .map(|(n, f)| format!("        \"{n}\" => frames::{f}(bytes),"))
        .collect::<Vec<_>>()
        .join("\n");
    let table: String = cases
        .iter()
        .map(|(n, p)| {
            let mut with_trailing = p.clone();
            with_trailing.extend_from_slice(&TRAILING);
            format!("        (\"{n}\", \"{}\", {}),", hex(&with_trailing), p.len())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let main = format!(
        r#"mod frames;

fn walk(name: &str, bytes: &[u8]) -> usize {{
    match name {{
{arms}
        _ => panic!("no walker for {{name}}"),
    }}
}}

fn unhex(s: &str) -> Vec<u8> {{
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}}

fn main() {{
    let cases: &[(&str, &str, usize)] = &[
{table}
    ];
    let mut bad = 0;
    for (name, hex, want) in cases {{
        let bytes = unhex(hex);
        let got = walk(name, &bytes);
        if got != *want {{
            println!("{{name}}: walker {{got}} != interpreter {{want}} for {{hex}}");
            bad += 1;
            if bad > 20 {{
                break;
            }}
        }}
    }}
    if bad > 0 {{
        std::process::exit(1);
    }}
}}
"#
    );
    compile_and_run(rust_frames, &main)
}

fn compile_and_run(frames_rs: &str, main_rs: &str) -> Vec<String> {
    if std::process::Command::new("rustc").arg("--version").output().is_err() {
        eprintln!("rustc not installed; skipping");
        return Vec::new();
    }
    let dir = common::temp_tree("frames");
    std::fs::write(dir.join("frames.rs"), frames_rs).unwrap();
    std::fs::write(dir.join("main.rs"), main_rs).unwrap();
    let built = std::process::Command::new("rustc")
        .args(["--edition", "2021", "-A", "warnings", "-O", "main.rs", "-o", "runner"])
        .current_dir(&dir)
        .output()
        .unwrap();
    if !built.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return vec![format!(
            "rustc failed:\n{}",
            String::from_utf8_lossy(&built.stderr)
                .chars()
                .rev()
                .take(3000)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>()
        )];
    }
    let ran = std::process::Command::new(dir.join("runner")).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&ran.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

fn std_schema(source: &str) -> bytemill::ResolvedSchema {
    let root = common::repo_root();
    let std: String = ["integers", "varint"]
        .iter()
        .map(|n| std::fs::read_to_string(root.join("schemas/std").join(format!("{n}.bml"))).unwrap())
        .collect();
    bytemill::analyze(&bytemill::parse(&(std + source)).unwrap()).unwrap()
}

fn walker_pairs(w: &Walkers) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = w
        .names
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    out.sort();
    out
}

#[test]
fn walkers_follow_guards_counts_matches_lengths_and_recursion() {
    let resolved = std_schema(
        r#"
        encoding leaf = {
            n: u8
            tail: bytes(n)
        }
        encoding tree = {
            kind: varint
            body: match kind {
                0 => leaf
                1 => {
                    count: u8
                    kids: tree[] count = count
                }
                2 => {
                    flags: u8
                    extra: u16be if flags & 4 != 0
                    more: i32be if (flags >> 1) & 1 == 1
                }
                3 => { a: varint, b: boolean, c: varlong }
                _ => {}
            }
        }
        packet probe { t: tree }
        "#,
    );
    let roots = vec!["tree".to_string()];
    let walkers = generate(&resolved, &roots, &bytemill::ordmap::OrderedMap::new()).unwrap();
    assert!(walkers.unsupported.is_empty(), "{:?}", walkers.unsupported);
    assert!(
        differences(&resolved, &walker_pairs(&walkers), &walkers.source).is_empty()
    );
}

#[test]
fn every_walker_the_protocol_needs_matches_the_interpreter() {
    let resolved = common::load_minecraft().unwrap();
    let plan = frames(&resolved).unwrap();
    assert!(plan.walkers.unsupported.is_empty(), "{:?}", plan.walkers.unsupported);
    // id_set is reached through its `ingredient` alias; test each encoding once.
    let names: Vec<(String, String)> = plan
        .walkers
        .names
        .iter()
        .filter(|(k, _)| *k != "ingredient")
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    for required in ["slot_display", "recipe_display", "particle_data", "id_set"] {
        assert!(names.iter().any(|(n, _)| n == required), "missing {required}");
    }
    let rust = format!("{FRAMES_RS}{}", plan.walkers.source);
    assert!(differences(&resolved, &names, &rust).is_empty());
}

#[test]
fn nbt_tag_len_uses_big_endian_modified_utf8_lengths() {
    let resolved = common::load_minecraft().unwrap();
    let plan = frames(&resolved).unwrap();
    let rust = format!("{FRAMES_RS}{}", plan.walkers.source);
    let payload_258 = vec![b'x'; 258];
    let cases: Vec<(Vec<u8>, usize)> = vec![
        // TAG_String "abc": tag + u16 BE len 3 + 3 bytes; trailing untouched.
        (hex_bytes("080003616263ffff"), 6),
        // TAG_Compound { "abc": 5 }: tag + entry + name + byte + TAG_End.
        (hex_bytes("0a0100036162630500ee"), 9),
        // u16 BE length 5 (high 0, low nonzero): 1 + 2 + 5.
        ([vec![0x08, 0x00, 0x05], b"hello".to_vec(), vec![0xff]].concat(), 8),
        // u16 BE length 0x0102 = 258: 1 + 2 + 258.
        ([vec![0x08, 0x01, 0x02], payload_258, vec![0xaa]].concat(), 261),
    ];
    assert!(nbt_tag_len_mismatches(&rust, &cases).is_empty());
}

#[test]
fn an_until_loop_is_reported_not_guessed() {
    let resolved = std_schema("encoding run = { items: u8[] until 0 }\npacket probe { r: run }\n");
    let roots = vec!["run".to_string()];
    let walkers = generate(&resolved, &roots, &bytemill::ordmap::OrderedMap::new()).unwrap();
    assert!(walkers.unsupported.contains_key("run"));
    assert!(walkers.names.is_empty());
}

#[test]
fn a_count_past_the_repeat_limit_is_rejected_without_looping() {
    // Zero-width elements read nothing, so only the limit stops a huge count.
    let resolved = std_schema(
        "encoding unit = {}\nencoding bag = { n: i32be\n items: unit[] count = n }\npacket probe { b: bag }\n",
    );
    let bag = TypeRef { name: "bag".to_string(), args: Vec::new(), transforms: Vec::new() };
    let mut over = Cursor::new(&[0x7f, 0xff, 0xff, 0xff]);
    over.repeat_limit = Some(MAX_PAYLOAD);
    let err = decode_type_ref(&bag, &mut over, &resolved, &bytemill::Env::new()).unwrap_err();
    assert!(matches!(err.kind, ErrorKind::Decode), "{err}");
    let mut within = Cursor::new(&[0, 0, 0, 3]);
    within.repeat_limit = Some(MAX_PAYLOAD);
    assert!(decode_type_ref(&bag, &mut within, &resolved, &bytemill::Env::new()).is_ok());
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
