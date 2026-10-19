//! The compatibility matrix: every typed printer and the language standards it
//! accepts. This is the source the article and `bml targets` read, so the page
//! does not keep a second copy of the target list.
//!
//! `default` is the standard a target uses when `emit <target>` gets no
//! version; every entry in `versions` is accepted by `emit <target>:<version>`
//! and by `--std`. `test_compat.rs` pins both directions against
//! [`EmitOptions::for_target`], including an exhaustive pass over every version
//! enum, so adding a standard to an emitter without adding it here fails a test.

use crate::json::Value;
use crate::ordmap::OrderedMap;

/// One typed printer and the standards it accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetCompat {
    /// The `emit` target name (`rust-typed`, `go`, `csharp`, …).
    pub target: &'static str,
    /// The language as a reader sees it (`Rust`, `C#`, …).
    pub language: &'static str,
    /// The standard used when no version is requested.
    pub default: &'static str,
    /// Every accepted standard or version, oldest first.
    pub versions: &'static [&'static str],
}

/// The nine typed printers, in README order.
pub const TARGETS: &[TargetCompat] = &[
    TargetCompat {
        target: "rust-typed",
        language: "Rust",
        default: "2024",
        versions: &["2015", "2018", "2021", "2024"],
    },
    TargetCompat {
        target: "go",
        language: "Go",
        default: "1.23",
        versions: &[
            "1.16", "1.17", "1.18", "1.19", "1.20", "1.21", "1.22", "1.23", "1.24",
        ],
    },
    TargetCompat {
        target: "python",
        language: "Python",
        default: "3.11",
        versions: &["3.8", "3.9", "3.10", "3.11", "3.12", "3.13", "3.14"],
    },
    TargetCompat {
        target: "java",
        language: "Java",
        default: "21",
        versions: &["8", "11", "17", "21"],
    },
    TargetCompat {
        target: "csharp",
        language: "C#",
        default: "12",
        versions: &["8", "9", "10", "11", "12"],
    },
    TargetCompat {
        target: "c",
        language: "C",
        default: "C11",
        versions: &["C89", "C90", "C99", "C11", "C17", "C23"],
    },
    TargetCompat {
        target: "cpp",
        language: "C++",
        default: "C++20",
        versions: &[
            "C++98", "C++03", "C++11", "C++14", "C++17", "C++20", "C++23", "C++26",
        ],
    },
    TargetCompat {
        target: "typescript",
        language: "TypeScript",
        default: "ES2022",
        versions: &["ES2015", "ES2018", "ES2020", "ES2022", "ESNext"],
    },
    TargetCompat {
        target: "zig",
        language: "Zig",
        default: "0.14",
        versions: &["0.11", "0.12", "0.13", "0.14"],
    },
];

/// The matrix, for callers that prefer a function to a constant.
pub fn targets() -> &'static [TargetCompat] {
    TARGETS
}

/// The matrix as a JSON array of objects.
pub fn targets_json() -> Value {
    Value::list(
        TARGETS
            .iter()
            .map(|entry| {
                let mut out = OrderedMap::new();
                out.insert("target", Value::str(entry.target));
                out.insert("language", Value::str(entry.language));
                out.insert("default", Value::str(entry.default));
                out.insert(
                    "versions",
                    Value::list(entry.versions.iter().copied().map(Value::str).collect()),
                );
                Value::map(out)
            })
            .collect(),
    )
}
