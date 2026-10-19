//! The compatibility matrix is the target table the article reads.

use bytemill::compat::{TARGETS, targets, targets_json};
use bytemill::json::{dumps, loads};
use bytemill::target_version::{
    CSharpVersion, CStandard, CppStandard, EmitOptions, GoVersion, JavaVersion, PythonVersion,
    RustEdition, TsTarget, ZigVersion,
};
use bytemill::value::Value;

fn versions(target: &str) -> &'static [&'static str] {
    TARGETS
        .iter()
        .find(|entry| entry.target == target)
        .unwrap_or_else(|| panic!("matrix lost target {target}"))
        .versions
}

fn contains(target: &str, value: &str) -> bool {
    versions(target)
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(value))
}

#[test]
fn every_matrix_version_is_accepted_by_the_emitter() {
    for entry in targets() {
        assert!(!entry.versions.is_empty(), "{}", entry.target);
        assert!(
            entry.versions.contains(&entry.default),
            "{} default {} is not in versions {:?}",
            entry.target,
            entry.default,
            entry.versions
        );
        for version in entry.versions {
            EmitOptions::for_target(entry.target, Some(version)).unwrap_or_else(|error| {
                panic!(
                    "{}:{version} rejected by the emitter: {error}",
                    entry.target
                )
            });
        }
    }
}

#[test]
fn target_names_are_unique_and_are_the_typed_printers() {
    let names: Vec<&str> = TARGETS.iter().map(|entry| entry.target).collect();
    assert_eq!(
        names,
        [
            "rust-typed",
            "go",
            "python",
            "java",
            "csharp",
            "c",
            "cpp",
            "typescript",
            "zig",
        ]
    );
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "duplicate target name");
}

#[test]
fn every_version_enum_variant_is_in_the_matrix() {
    for version in [
        CppStandard::Cpp98,
        CppStandard::Cpp03,
        CppStandard::Cpp11,
        CppStandard::Cpp14,
        CppStandard::Cpp17,
        CppStandard::Cpp20,
        CppStandard::Cpp23,
        CppStandard::Cpp26,
    ] {
        assert!(contains("cpp", version.name()), "cpp missing {version:?}");
    }

    for version in [
        CStandard::C89,
        CStandard::C90,
        CStandard::C99,
        CStandard::C11,
        CStandard::C17,
        CStandard::C23,
    ] {
        assert!(contains("c", version.name()), "c missing {version:?}");
    }

    for version in [
        RustEdition::Edition2015,
        RustEdition::Edition2018,
        RustEdition::Edition2021,
        RustEdition::Edition2024,
    ] {
        assert!(
            contains("rust-typed", version.as_str()),
            "rust-typed missing {version:?}"
        );
    }

    for version in [
        TsTarget::ES2015,
        TsTarget::ES2018,
        TsTarget::ES2020,
        TsTarget::ES2022,
        TsTarget::ESNext,
    ] {
        assert!(
            contains("typescript", version.as_str()),
            "typescript missing {version:?}"
        );
    }

    for version in [
        ZigVersion::V0_11,
        ZigVersion::V0_12,
        ZigVersion::V0_13,
        ZigVersion::V0_14,
    ] {
        assert!(contains("zig", version.as_str()), "zig missing {version:?}");
    }

    for version in [
        JavaVersion::Java8,
        JavaVersion::Java11,
        JavaVersion::Java17,
        JavaVersion::Java21,
    ] {
        assert!(
            contains("java", version.version_str()),
            "java missing {version:?}"
        );
    }

    for version in [
        CSharpVersion::CSharp8,
        CSharpVersion::CSharp9,
        CSharpVersion::CSharp10,
        CSharpVersion::CSharp11,
        CSharpVersion::CSharp12,
    ] {
        let name = version.version_str().trim_end_matches(".0");
        assert!(contains("csharp", name), "csharp missing {version:?}");
    }

    for version in [
        PythonVersion::Py38,
        PythonVersion::Py39,
        PythonVersion::Py310,
        PythonVersion::Py311,
        PythonVersion::Py312,
        PythonVersion::Py313,
        PythonVersion::Py314,
    ] {
        assert!(
            contains("python", version.version_str()),
            "python missing {version:?}"
        );
    }

    for version in [
        GoVersion::V1_16,
        GoVersion::V1_17,
        GoVersion::V1_18,
        GoVersion::V1_19,
        GoVersion::V1_20,
        GoVersion::V1_21,
        GoVersion::V1_22,
        GoVersion::V1_23,
        GoVersion::V1_24,
    ] {
        assert!(contains("go", version.mod_str()), "go missing {version:?}");
    }
}

#[test]
fn targets_json_carries_every_target_and_version() {
    let report = targets_json();
    let Value::List(entries) = &report else {
        panic!("targets_json is not a list");
    };
    assert_eq!(entries.len(), TARGETS.len());
    for (value, entry) in entries.iter().zip(TARGETS) {
        assert!(matches!(
            value.get("target"),
            Some(Value::Str(name)) if &**name == entry.target
        ));
        assert!(matches!(
            value.get("language"),
            Some(Value::Str(name)) if &**name == entry.language
        ));
        assert!(matches!(
            value.get("default"),
            Some(Value::Str(name)) if &**name == entry.default
        ));
        let Value::List(list) = value.get("versions").expect("versions") else {
            panic!("versions is not a list");
        };
        assert_eq!(list.len(), entry.versions.len());
    }

    let round_trip = loads(&dumps(&report)).expect("targets JSON round trips");
    let Value::List(round_trip) = &round_trip else {
        panic!("round-tripped targets JSON is not a list");
    };
    assert_eq!(round_trip.len(), TARGETS.len());
}
