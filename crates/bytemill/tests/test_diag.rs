//! Diagnostics layer tests: positions, carets, did-you-mean, import errors, JSON,
//! CLI `bml check` and `bml check --format`.

mod common;

use std::process::{Command, Output, Stdio};

use bytemill::Value;
use bytemill::diag::{Diagnostic, Severity, Span, diagnose_files, diagnose_source, to_json};
use bytemill::json::loads;

fn run_bml(args: &[&str]) -> Output {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    Command::new(env!("CARGO_BIN_EXE_bml"))
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn bml")
}

fn out_stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn out_stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn syntax_error_on_line_1() {
    let ds = diagnose_source("one.bml", "packet x { y: }");
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.severity, Severity::Error);
    assert!(d.message.contains("expected a name") || d.message.contains("}"));
    assert_eq!(d.file.as_deref(), Some("one.bml"));
    assert_eq!(d.line, 1);
    assert!(d.column >= 1);
    assert_eq!(d.end_line, 1);
    assert!(d.end_column > d.column);
}

#[test]
fn syntax_error_on_middle_line() {
    let src = "packet x {\n    a: u8\n    b: u8 ?\n    c: u8\n}";
    let ds = diagnose_source("mid.bml", src);
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.line, 3);
    assert!(d.column > 1);
}

#[test]
fn syntax_error_on_last_line_without_trailing_newline() {
    let src = "packet x {\n    a: u8\n    b: ";
    let ds = diagnose_source("last.bml", src);
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.line, 3);
    assert!(d.column > 1);
}

#[test]
fn syntax_error_after_multibyte_char() {
    let src = "packet x {\n    a: u8\n    b: u8 if a > \u{00E9}";
    let ds = diagnose_source("mb.bml", src);
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.line, 3);
    assert!(d.column > 10);
}

#[test]
fn syntax_error_crlf_file() {
    let src = "packet x {\r\n    a: u8\r\n    b: \r\n}";
    let ds = diagnose_source("crlf.bml", src);
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.line, 4);
}

#[test]
fn syntax_error_at_end_of_input() {
    let src = "packet x { a: u8\n  b: ";
    let ds = diagnose_source("eof.bml", src);
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert_eq!(d.line, 2);
    assert!(d.column >= 1);
}

#[test]
fn undeclared_name_points_at_use_in_correct_decl() {
    let dir = common::temp_tree("diag_names");
    let src = "packet x { a: u8\n  b: nosuch }\npacket y { c: nosuch }\n";
    let f = common::write_file(&dir, "two.bml", src);
    let ds = diagnose_files(&[&f]);
    assert_eq!(ds.len(), 2);
    let d0 = ds
        .iter()
        .find(|d| d.message.contains("nosuch") && d.line < 3)
        .expect("diag for packet x");
    assert_eq!(d0.line, 2);
    let d1 = ds
        .iter()
        .find(|d| d.message.contains("nosuch") && d.line > 2)
        .expect("diag for packet y");
    assert_eq!(d1.line, 3);
    common::cleanup(&dir);
}
#[test]
fn did_you_mean_for_close_name() {
    let dir = common::temp_tree("diag_suggest");
    let std_content = std::fs::read_to_string(common::schema_path("std/integers.bml")).unwrap();
    common::write_file(&dir, "std/integers.bml", &std_content);
    let src = "import \"std/integers.bml\"\npacket x { a: u16bee }\n";
    let f = common::write_file(&dir, "main.bml", src);
    let ds = diagnose_files(&[&f]);
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    let help = d.help.as_ref().expect("has suggestion help");
    assert!(help.contains("did you mean `u16be`"));
    common::cleanup(&dir);
}
#[test]
fn no_suggestion_for_far_name() {
    let dir = common::temp_tree("diag_nosug");
    let src = "packet x { a: completelydifferentname }\n";
    let f = common::write_file(&dir, "far.bml", src);
    let ds = diagnose_files(&[&f]);
    assert_eq!(ds.len(), 1);
    let d = &ds[0];
    assert!(d.help.is_none() || !d.help.as_ref().unwrap().contains("did you mean"));
    common::cleanup(&dir);
}

#[test]
fn two_undeclared_names_give_two_diagnostics() {
    let dir = common::temp_tree("diag_multi");
    let src = "packet x { a: foo\n  b: bar }\n";
    let f = common::write_file(&dir, "multi.bml", src);
    let ds = diagnose_files(&[&f]);
    assert_eq!(ds.len(), 2);
    common::cleanup(&dir);
}

#[test]
fn diagnostics_come_in_source_order_whatever_their_cause() {
    let dir = common::temp_tree("diag_order");
    common::write_file(&dir, "lib.bml", "encoding e = u8\n");
    // `typo` is declared nowhere (line 2); `e` only needs an import (line 3).
    let main = common::write_file(&dir, "main.bml", "packet p {\n  a: typo\n  b: e\n}\n");
    let ds = diagnose_files(&[&main]);
    assert_eq!(ds.len(), 2, "{ds:?}");
    assert_eq!((ds[0].line, ds[1].line), (2, 3), "{ds:?}");
    assert!(ds[0].message.contains("typo"), "{ds:?}");
    assert!(ds[1].message.contains("'e'"), "{ds:?}");
    common::cleanup(&dir);
}

#[test]
fn nearby_file_import_hint_is_kept() {
    let dir = common::temp_tree("diag_near");
    common::write_file(&dir, "lib.bml", "encoding e = u8\n");
    let main = common::write_file(&dir, "main.bml", "packet p { f: e }\n");
    let ds = diagnose_files(&[&main]);
    assert_eq!(ds.len(), 1);
    let h = ds[0].help.as_ref().unwrap();
    assert!(h.contains("import \"lib.bml\"") || h.contains("add  import"));
    common::cleanup(&dir);
}

#[test]
fn error_inside_imported_file_names_that_file() {
    let dir = common::temp_tree("diag_import_err");
    let bad = common::write_file(&dir, "bad.bml", "packet x { a: u8\n  b: \n}");
    let main = common::write_file(
        &dir,
        "main.bml",
        "import \"bad.bml\"\npacket y { ok: u8 }\n",
    );
    let ds = diagnose_files(&[&main]);
    assert_eq!(ds.len(), 1);
    assert_eq!(ds[0].file.as_deref(), Some(bad.to_str().unwrap()));
    assert!(ds[0].line >= 2);
    common::cleanup(&dir);
}

#[test]
fn valid_schema_yields_no_diagnostics() {
    let dir = common::temp_tree("diag_ok");
    let good = common::write_file(&dir, "good.bml", "packet p { x: u8 }\n");
    let ds = diagnose_files(&[&good]);
    assert!(ds.is_empty());
    common::cleanup(&dir);
}

#[test]

fn to_json_shape_and_key_order() {
    let d = Diagnostic {
        severity: Severity::Error,
        message: "m".into(),
        file: Some("f.bml".into()),
        span: Some(Span { start: 0, end: 1 }),
        line: 1,
        column: 1,
        end_line: 1,
        end_column: 2,
        help: None,
    };
    let j = to_json(std::slice::from_ref(&d));
    let parsed = loads(&j).expect("parses");
    if let Value::Map(root) = &parsed {
        assert!(root.contains_key("diagnostics"));
        assert!(root.contains_key("ok"));
        if let Some(Value::List(dlist)) = root.get("diagnostics") {
            assert_eq!(dlist.len(), 1);
            if let Value::Map(dm) = &dlist[0] {
                assert_eq!(dm.get("column"), Some(&Value::Int(1)));
                assert_eq!(dm.get("end_column"), Some(&Value::Int(2)));
                assert_eq!(dm.get("message"), Some(&Value::Str("m".into())));
                assert_eq!(dm.get("severity"), Some(&Value::Str("error".into())));
                assert!(dm.get("help").is_none());
            } else {
                panic!("diag map");
            }
        } else {
            panic!("diags list");
        }
        assert_eq!(root.get("ok"), Some(&Value::Bool(false)));
    } else {
        panic!("root map");
    }
    // sorted keys order from dumps_sorted
    assert!(j.find("\"column\"") < j.find("\"end_column\""));
    assert!(j.find("\"end_column\"") < j.find("\"end_line\""));
    assert!(j.find("\"end_line\"") < j.find("\"file\""));
    assert!(j.find("\"file\"") < j.find("\"line\""));
    assert!(j.find("\"line\"") < j.find("\"message\""));
    assert!(j.find("\"message\"") < j.find("\"severity\""));

    let d2 = Diagnostic {
        help: Some("h".into()),
        ..d.clone()
    };
    let j2 = to_json(&[d2]);
    let p2 = loads(&j2).expect("parses");
    if let Value::Map(rm) = &p2
        && let Some(Value::List(dl)) = rm.get("diagnostics")
        && let Value::Map(dm) = &dl[0]
    {
        assert_eq!(dm.get("help"), Some(&Value::Str("h".into())));
    }

    let j0 = to_json(&[]);
    let p0 = loads(&j0).expect("parses");
    if let Value::Map(rm) = &p0 {
        assert_eq!(rm.get("ok"), Some(&Value::Bool(true)));
        if let Some(Value::List(dl)) = rm.get("diagnostics") {
            assert!(dl.is_empty());
        }
    }
}

#[test]
fn cli_check_human_prints_location_and_exits_3() {
    let dir = common::temp_tree("cli_h");
    let bad1 = common::write_file(
        &dir,
        "bad1.bml",
        "packet x {\n    a: u8\n    b: nosuchtype\n}\n",
    );
    let o = run_bml(&["check", bad1.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(3));
    let e = out_stderr(&o);
    assert!(e.contains("error: 'nosuchtype' is not declared"));
    assert!(e.contains(" --> "));
    assert!(e.contains(" | "));
    assert!(e.contains("nosuchtype"));
    common::cleanup(&dir);
}

#[test]
fn cli_check_syntax_human() {
    let dir = common::temp_tree("cli_syn");
    let bad2 = common::write_file(
        &dir,
        "bad2.bml",
        "packet x {\n    a: u8\n    c: u8 if a >\n}\n",
    );
    let o = run_bml(&["check", bad2.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(3));
    let e = out_stderr(&o);
    assert!(e.contains("error: expected a name, found `}`"));
    assert!(e.contains(" --> "));
    common::cleanup(&dir);
}

#[test]
fn cli_check_json_valid_and_invalid() {
    let dir = common::temp_tree("cli_j");
    let good = common::write_file(&dir, "g.bml", "packet p { x: u8 }\n");
    let o = run_bml(&["check", "--format", "json", good.to_str().unwrap()]);
    assert!(o.status.success());
    let pgood = loads(&out_stdout(&o)).expect("parses valid json");
    if let Value::Map(rm) = &pgood {
        assert_eq!(rm.get("ok"), Some(&Value::Bool(true)));
        if let Some(Value::List(dl)) = rm.get("diagnostics") {
            assert!(dl.is_empty());
        } else {
            panic!("diags list");
        }
    } else {
        panic!("root map");
    }

    let bad = common::write_file(&dir, "b.bml", "packet x { a: nosuch }\n");
    let o2 = run_bml(&["check", "--format", "json", bad.to_str().unwrap()]);
    assert_eq!(o2.status.code(), Some(3));
    let pbad = loads(&out_stdout(&o2)).expect("parses error json");
    if let Value::Map(rm) = &pbad {
        assert_eq!(rm.get("ok"), Some(&Value::Bool(false)));
        if let Some(Value::List(dl)) = rm.get("diagnostics") {
            assert_eq!(dl.len(), 1);
            if let Value::Map(dm) = &dl[0] {
                let msg = dm.get("message").and_then(|v| {
                    if let Value::Str(s) = v {
                        Some(s.to_string())
                    } else {
                        None
                    }
                });
                assert!(msg.unwrap().contains("nosuch"));
            } else {
                panic!("diag map");
            }
        } else {
            panic!("diags list");
        }
    } else {
        panic!("root map");
    }
    common::cleanup(&dir);
}

#[test]
fn cli_check_bad_format_exits_2() {
    let dir = common::temp_tree("cli_f");
    let f = common::write_file(&dir, "f.bml", "packet p { x: u8 }\n");
    let o = run_bml(&["check", "--format", "yaml", f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    let e = out_stderr(&o);
    assert!(e.contains("check: argument --format: invalid choice"));
    common::cleanup(&dir);
}

#[test]
fn cli_check_missing_file_exits_3_in_both_formats() {
    // A load error without a source position must still be reported as an error, never as `ok`.
    let dir = common::temp_tree("cli_missing");
    let missing = dir.join("nope.bml");
    let path = missing.to_str().unwrap();

    let human = run_bml(&["check", path]);
    assert_eq!(human.status.code(), Some(3));
    assert!(
        out_stderr(&human).contains("schema not found"),
        "{}",
        out_stderr(&human)
    );
    assert!(!out_stdout(&human).contains("ok"), "{}", out_stdout(&human));

    let json = run_bml(&["check", "--format", "json", path]);
    assert_eq!(json.status.code(), Some(3));
    let parsed = loads(&out_stdout(&json)).expect("valid json");
    let Value::Map(root) = &parsed else {
        panic!("root map");
    };
    assert_eq!(root.get("ok"), Some(&Value::Bool(false)));
    let Some(Value::List(diags)) = root.get("diagnostics") else {
        panic!("diagnostics list");
    };
    assert_eq!(diags.len(), 1);
    let Value::Map(diag) = &diags[0] else {
        panic!("diagnostic map");
    };
    let Some(Value::Str(message)) = diag.get("message") else {
        panic!("message string");
    };
    assert!(message.contains("schema not found"), "{message}");
    assert_eq!(diag.get("severity"), Some(&Value::str("error")));
    common::cleanup(&dir);
}
