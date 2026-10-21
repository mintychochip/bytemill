//! `bml help`: the overview, a page for every command, and what a mistake prints.
//!
//! These run the real binary with stdin closed, so a command that wrongly reads stdin or a
//! schema while it should be showing help comes back empty instead of hanging.

use bytemill::cli::EMIT_TARGETS;
use bytemill::cli_help::{COMMANDS, WIDTH, find, overview, page};
use std::process::{Command, Output, Stdio};

fn bml(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bml"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn every_spelling_of_help_prints_the_overview_listing_every_command() {
    let reference = stdout(&bml(&["help"]));
    for args in [["help"], ["--help"], ["-h"]] {
        let output = bml(&args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(stdout(&output), reference, "{args:?}");
        assert!(stderr(&output).is_empty(), "{args:?}: {}", stderr(&output));
    }
    for command in COMMANDS {
        assert!(
            reference.contains(&format!("\n  {}", command.name)),
            "the overview lacks {}:\n{reference}",
            command.name
        );
    }
}

#[test]
fn a_bare_bml_is_a_usage_error_that_carries_the_overview() {
    let output = bml(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty());
    assert_eq!(stderr(&output), stdout(&bml(&["help"])));
}

#[test]
fn every_command_has_a_page_and_all_three_ways_to_ask_for_it_agree() {
    for command in COMMANDS {
        let name = command.name;
        let reference = bml(&["help", name]);
        assert_eq!(reference.status.code(), Some(0), "help {name}");
        let text = stdout(&reference);
        assert!(text.starts_with(&format!("bml {name} - ")), "{text}");
        assert!(text.contains("Usage:"), "{text}");
        for args in [[name, "--help"], [name, "-h"]] {
            let output = bml(&args);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{args:?}: {}",
                stderr(&output)
            );
            assert_eq!(stdout(&output), text, "{args:?}");
        }
    }
}

#[test]
fn shapes_subcommands_get_the_shapes_page() {
    let shapes = stdout(&bml(&["help", "shapes"]));
    for sub in ["extract", "report"] {
        assert_eq!(stdout(&bml(&["help", "shapes", sub])), shapes, "{sub}");
    }
    assert!(shapes.contains("shapes extract") && shapes.contains("shapes report"));
}

#[test]
fn every_listed_command_is_dispatched() {
    // A flag no command knows: each one must reject or fail on it itself. None may answer
    // "unknown command", which is what a command in the table but not in `main` would say.
    for command in COMMANDS.iter().filter(|c| c.name != "help") {
        let output = bml(&[command.name, "--no-such-flag"]);
        let code = output.status.code();
        assert!(
            matches!(code, Some(2 | 3)),
            "{}: exit {code:?}: {}",
            command.name,
            stderr(&output)
        );
        assert!(
            !stderr(&output).contains("unknown command"),
            "{} is listed but not dispatched",
            command.name
        );
    }
}

#[test]
fn every_command_bml_runs_is_in_the_table() {
    // `bml` refuses a name that is not in COMMANDS, so a command dropped from the table stops
    // working instead of merely missing from the help. This is the list as of the 0.1 series.
    for name in [
        "check",
        "dump-json",
        "routes",
        "stages",
        "decode",
        "encode",
        "wire-check",
        "emit",
        "targets",
        "shapes",
        "import-versions",
        "import-protodef",
    ] {
        assert!(
            find(name).is_some(),
            "`bml {name}` is not in COMMANDS, so bml refuses it"
        );
    }
}

#[test]
fn a_mistyped_command_gets_a_suggestion_and_a_pointer_to_help() {
    let output = bml(&["chek"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty());
    let text = stderr(&output);
    assert!(text.contains("unknown command \"chek\""), "{text}");
    assert!(text.contains("did you mean `check`?"), "{text}");
    assert!(text.contains("bml help"), "{text}");

    let prefix = stderr(&bml(&["import"]));
    assert!(
        prefix.contains("`import-versions`") && prefix.contains("`import-protodef`"),
        "{prefix}"
    );

    let nothing_close = stderr(&bml(&["zzzzzz"]));
    assert!(nothing_close.contains("unknown command"), "{nothing_close}");
    assert!(!nothing_close.contains("did you mean"), "{nothing_close}");
}

#[test]
fn help_for_a_topic_that_is_not_a_command_is_a_usage_error() {
    let output = bml(&["help", "emitt"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("did you mean `emit`?"),
        "{}",
        stderr(&output)
    );

    let extra = bml(&["help", "check", "extra"]);
    assert_eq!(extra.status.code(), Some(2));
    assert!(
        stderr(&extra).contains("unexpected argument"),
        "{}",
        stderr(&extra)
    );
}

#[test]
fn a_usage_error_points_at_the_help_for_that_command() {
    let output = bml(&["emit"]);
    assert_eq!(output.status.code(), Some(2));
    let text = stderr(&output);
    assert!(
        text.contains("emit: the following arguments are required: target"),
        "{text}"
    );
    assert!(text.contains("Run `bml help emit` for usage."), "{text}");

    // A flag-level error names no command, so the pointer is to the overview.
    let flag = stderr(&bml(&["encode", "--packet"]));
    assert!(flag.contains("expected one argument"), "{flag}");
    assert!(flag.contains("Run `bml help` for usage."), "{flag}");
}

#[test]
fn version_prints_the_crate_version() {
    for flag in ["--version", "-V"] {
        let output = bml(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        assert_eq!(
            stdout(&output),
            format!("bml {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn the_emit_page_lists_every_target_emit_accepts() {
    let text = page(find("emit").unwrap());
    for target in EMIT_TARGETS {
        assert!(
            text.contains(target),
            "the emit page lacks {target}:\n{text}"
        );
    }
    // A language added to the compatibility matrix must also be an emit target.
    for entry in bytemill::compat::TARGETS {
        assert!(
            EMIT_TARGETS.contains(&entry.target),
            "{} is in the compatibility matrix but `bml emit` does not accept it",
            entry.target
        );
    }
}

#[test]
fn no_printed_line_is_wider_than_the_wrap_width() {
    // Prose is wrapped, but usage lines and examples are printed as written, so a new one
    // that is too long has to be broken by hand where it can still be copied.
    let mut texts = vec![("the overview".to_string(), overview())];
    texts.extend(
        COMMANDS
            .iter()
            .map(|c| (format!("help {}", c.name), page(c))),
    );
    for (what, text) in texts {
        for line in text.lines() {
            assert!(
                line.chars().count() <= WIDTH,
                "{what}: {line:?} is wider than {WIDTH}"
            );
        }
    }
}
