//! Target language versions and standards configuration across BPS emitters.

use crate::errors::{BpsResult, emit};
use std::fmt;

/// C++ language standards supported by `emit cpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum CppStandard {
    Cpp98,
    Cpp03,
    Cpp11,
    Cpp14,
    Cpp17,
    #[default]
    Cpp20,
    Cpp23,
    Cpp26,
}

impl CppStandard {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s.to_ascii_lowercase().replace(['-', '_'], "");
        match norm.as_str() {
            "c++98" | "cpp98" | "98" => Ok(Self::Cpp98),
            "c++03" | "cpp03" | "03" => Ok(Self::Cpp03),
            "c++11" | "cpp11" | "11" => Ok(Self::Cpp11),
            "c++14" | "cpp14" | "14" => Ok(Self::Cpp14),
            "c++17" | "cpp17" | "17" => Ok(Self::Cpp17),
            "c++20" | "cpp20" | "20" => Ok(Self::Cpp20),
            "c++23" | "cpp23" | "23" => Ok(Self::Cpp23),
            "c++26" | "cpp26" | "26" => Ok(Self::Cpp26),
            _ => Err(emit(format!(
                "unsupported C++ standard {s:?}; choose from 'c++98', 'c++03', 'c++11', 'c++14', 'c++17', 'c++20', 'c++23', 'c++26'"
            ))),
        }
    }

    pub fn flag(&self) -> &'static str {
        match self {
            Self::Cpp98 => "-std=c++98",
            Self::Cpp03 => "-std=c++03",
            Self::Cpp11 => "-std=c++11",
            Self::Cpp14 => "-std=c++14",
            Self::Cpp17 => "-std=c++17",
            Self::Cpp20 => "-std=c++20",
            Self::Cpp23 => "-std=c++23",
            Self::Cpp26 => "-std=c++26",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Cpp98 => "c++98",
            Self::Cpp03 => "c++03",
            Self::Cpp11 => "c++11",
            Self::Cpp14 => "c++14",
            Self::Cpp17 => "c++17",
            Self::Cpp20 => "c++20",
            Self::Cpp23 => "c++23",
            Self::Cpp26 => "c++26",
        }
    }

    pub fn supports_cstdint(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_auto(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_range_for(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_enum_class(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_constexpr(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_noexcept(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_nullptr(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_unique_ptr(&self) -> bool {
        *self >= Self::Cpp11
    }

    pub fn supports_std_optional(&self) -> bool {
        *self >= Self::Cpp17
    }

    pub fn supports_std_variant(&self) -> bool {
        *self >= Self::Cpp17
    }

    pub fn supports_std_string_view(&self) -> bool {
        *self >= Self::Cpp17
    }

    pub fn supports_nodiscard(&self) -> bool {
        *self >= Self::Cpp17
    }

    pub fn supports_std_span(&self) -> bool {
        *self >= Self::Cpp20
    }

    pub fn supports_designated_init(&self) -> bool {
        *self >= Self::Cpp20
    }

    pub fn supports_braced_init(&self) -> bool {
        *self >= Self::Cpp11
    }
}

impl fmt::Display for CppStandard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Go versions supported by `emit go`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum GoVersion {
    V1_16,
    V1_17,
    V1_18,
    V1_19,
    V1_20,
    V1_21,
    V1_22,
    #[default]
    V1_23,
    V1_24,
}

impl GoVersion {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s.to_ascii_lowercase().trim_start_matches("go").to_string();
        match norm.as_str() {
            "1.16" => Ok(Self::V1_16),
            "1.17" => Ok(Self::V1_17),
            "1.18" => Ok(Self::V1_18),
            "1.19" => Ok(Self::V1_19),
            "1.20" => Ok(Self::V1_20),
            "1.21" => Ok(Self::V1_21),
            "1.22" => Ok(Self::V1_22),
            "1.23" => Ok(Self::V1_23),
            "1.24" => Ok(Self::V1_24),
            _ => Err(emit(format!(
                "unsupported Go version {s:?}; choose from '1.16', '1.17', '1.18', '1.19', '1.20', '1.21', '1.22', '1.23', '1.24'"
            ))),
        }
    }

    pub fn mod_str(&self) -> &'static str {
        match self {
            Self::V1_16 => "1.16",
            Self::V1_17 => "1.17",
            Self::V1_18 => "1.18",
            Self::V1_19 => "1.19",
            Self::V1_20 => "1.20",
            Self::V1_21 => "1.21",
            Self::V1_22 => "1.22",
            Self::V1_23 => "1.23",
            Self::V1_24 => "1.24",
        }
    }

    pub fn supports_any(&self) -> bool {
        *self >= Self::V1_18
    }

    pub fn supports_range_int(&self) -> bool {
        *self >= Self::V1_22
    }
}

impl fmt::Display for GoVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "go{}", self.mod_str())
    }
}

/// Python versions supported by `emit python`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum PythonVersion {
    Py38,
    Py39,
    Py310,
    #[default]
    Py311,
    Py312,
    Py313,
    Py314,
}

impl PythonVersion {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s
            .to_ascii_lowercase()
            .trim_start_matches("python")
            .trim_start_matches("py")
            .to_string();
        match norm.as_str() {
            "3.8" | "38" => Ok(Self::Py38),
            "3.9" | "39" => Ok(Self::Py39),
            "3.10" | "310" => Ok(Self::Py310),
            "3.11" | "311" => Ok(Self::Py311),
            "3.12" | "312" => Ok(Self::Py312),
            "3.13" | "313" => Ok(Self::Py313),
            "3.14" | "314" => Ok(Self::Py314),
            _ => Err(emit(format!(
                "unsupported Python version {s:?}; choose from '3.8', '3.9', '3.10', '3.11', '3.12', '3.13', '3.14'"
            ))),
        }
    }

    pub fn version_str(&self) -> &'static str {
        match self {
            Self::Py38 => "3.8",
            Self::Py39 => "3.9",
            Self::Py310 => "3.10",
            Self::Py311 => "3.11",
            Self::Py312 => "3.12",
            Self::Py313 => "3.13",
            Self::Py314 => "3.14",
        }
    }

    pub fn supports_match(&self) -> bool {
        *self >= Self::Py310
    }

    pub fn supports_union_pipe(&self) -> bool {
        *self >= Self::Py310
    }

    pub fn supports_dataclass_slots(&self) -> bool {
        *self >= Self::Py310
    }

    pub fn supports_dataclass_kw_only(&self) -> bool {
        *self >= Self::Py310
    }

    pub fn supports_typing_self(&self) -> bool {
        *self >= Self::Py311
    }

    pub fn supports_pep695_type(&self) -> bool {
        *self >= Self::Py312
    }
}

impl fmt::Display for PythonVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "python{}", self.version_str())
    }
}

/// C language standards supported by `emit c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum CStandard {
    C89,
    C90,
    C99,
    #[default]
    C11,
    C17,
    C23,
}

impl CStandard {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s.to_ascii_lowercase().replace(['-', '_'], "");
        match norm.as_str() {
            "c89" | "89" | "ansi" => Ok(Self::C89),
            "c90" | "90" => Ok(Self::C90),
            "c99" | "99" => Ok(Self::C99),
            "c11" | "11" => Ok(Self::C11),
            "c17" | "17" | "c18" | "18" => Ok(Self::C17),
            "c23" | "23" => Ok(Self::C23),
            _ => Err(emit(format!(
                "unsupported C standard {s:?}; choose from 'c89', 'c90', 'c99', 'c11', 'c17', 'c23'"
            ))),
        }
    }

    pub fn flag(&self) -> &'static str {
        match self {
            Self::C89 => "-std=c89",
            Self::C90 => "-std=c90",
            Self::C99 => "-std=c99",
            Self::C11 => "-std=c11",
            Self::C17 => "-std=c17",
            Self::C23 => "-std=c23",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::C89 => "c89",
            Self::C90 => "c90",
            Self::C99 => "c99",
            Self::C11 => "c11",
            Self::C17 => "c17",
            Self::C23 => "c23",
        }
    }

    pub fn supports_stdint(&self) -> bool {
        *self >= Self::C99
    }

    pub fn supports_stdbool(&self) -> bool {
        *self >= Self::C99
    }

    pub fn supports_line_comments(&self) -> bool {
        *self >= Self::C99
    }

    pub fn supports_mixed_decls(&self) -> bool {
        *self >= Self::C99
    }

    pub fn supports_anonymous_unions(&self) -> bool {
        *self >= Self::C11
    }

    pub fn supports_c23_bool(&self) -> bool {
        *self >= Self::C23
    }
}

impl fmt::Display for CStandard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Rust editions supported by `emit rust-typed` and `emit rust`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum RustEdition {
    Edition2015,
    Edition2018,
    Edition2021,
    #[default]
    Edition2024,
}

impl RustEdition {
    pub fn parse(s: &str) -> BpsResult<Self> {
        match s.trim() {
            "2015" => Ok(Self::Edition2015),
            "2018" => Ok(Self::Edition2018),
            "2021" => Ok(Self::Edition2021),
            "2024" => Ok(Self::Edition2024),
            _ => Err(emit(format!(
                "unsupported Rust edition {s:?}; choose from '2015', '2018', '2021', '2024'"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Edition2015 => "2015",
            Self::Edition2018 => "2018",
            Self::Edition2021 => "2021",
            Self::Edition2024 => "2024",
        }
    }
}

impl fmt::Display for RustEdition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// TypeScript / JavaScript targets supported by `emit typescript`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum TsTarget {
    ES2015,
    ES2018,
    ES2020,
    #[default]
    ES2022,
    ESNext,
}

impl TsTarget {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s.to_ascii_lowercase().replace('-', "");
        match norm.as_str() {
            "es2015" | "es6" => Ok(Self::ES2015),
            "es2018" => Ok(Self::ES2018),
            "es2020" => Ok(Self::ES2020),
            "es2022" => Ok(Self::ES2022),
            "esnext" => Ok(Self::ESNext),
            _ => Err(emit(format!(
                "unsupported TypeScript target {s:?}; choose from 'es2015', 'es2018', 'es2020', 'es2022', 'esnext'"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ES2015 => "es2015",
            Self::ES2018 => "es2018",
            Self::ES2020 => "es2020",
            Self::ES2022 => "es2022",
            Self::ESNext => "esnext",
        }
    }
}

impl fmt::Display for TsTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Zig versions supported by `emit zig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ZigVersion {
    V0_11,
    V0_12,
    V0_13,
    #[default]
    V0_14,
}

impl ZigVersion {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s.trim_start_matches('v').trim();
        match norm {
            "0.11" => Ok(Self::V0_11),
            "0.12" => Ok(Self::V0_12),
            "0.13" => Ok(Self::V0_13),
            "0.14" => Ok(Self::V0_14),
            _ => Err(emit(format!(
                "unsupported Zig version {s:?}; choose from '0.11', '0.12', '0.13', '0.14'"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::V0_11 => "0.11",
            Self::V0_12 => "0.12",
            Self::V0_13 => "0.13",
            Self::V0_14 => "0.14",
        }
    }
}

impl fmt::Display for ZigVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
/// Java versions supported by `emit java`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum JavaVersion {
    Java8,
    Java11,
    Java17,
    #[default]
    Java21,
}

impl JavaVersion {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s
            .to_ascii_lowercase()
            .trim_start_matches("java")
            .trim_start_matches("1.")
            .to_string();
        match norm.as_str() {
            "8" => Ok(Self::Java8),
            "11" => Ok(Self::Java11),
            "17" => Ok(Self::Java17),
            "21" => Ok(Self::Java21),
            _ => Err(emit(format!(
                "unsupported Java version {s:?}; choose from '8', '11', '17', '21'"
            ))),
        }
    }

    pub fn version_str(&self) -> &'static str {
        match self {
            Self::Java8 => "8",
            Self::Java11 => "11",
            Self::Java17 => "17",
            Self::Java21 => "21",
        }
    }

    pub fn supports_records(&self) -> bool {
        *self >= Self::Java17
    }

    pub fn supports_sealed_classes(&self) -> bool {
        *self >= Self::Java17
    }
}

impl fmt::Display for JavaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "java{}", self.version_str())
    }
}

/// C# language versions supported by `emit csharp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum CSharpVersion {
    CSharp8,
    CSharp9,
    CSharp10,
    CSharp11,
    #[default]
    CSharp12,
}

impl CSharpVersion {
    pub fn parse(s: &str) -> BpsResult<Self> {
        let norm = s
            .to_ascii_lowercase()
            .replace(['#', '-'], "")
            .trim_start_matches("c")
            .trim_start_matches("cs")
            .trim_start_matches("net")
            .to_string();
        match norm.as_str() {
            "8" | "8.0" => Ok(Self::CSharp8),
            "9" | "9.0" => Ok(Self::CSharp9),
            "10" | "10.0" => Ok(Self::CSharp10),
            "11" | "11.0" => Ok(Self::CSharp11),
            "12" | "12.0" => Ok(Self::CSharp12),
            _ => Err(emit(format!(
                "unsupported C# version {s:?}; choose from '8', '9', '10', '11', '12'"
            ))),
        }
    }

    pub fn version_str(&self) -> &'static str {
        match self {
            Self::CSharp8 => "8.0",
            Self::CSharp9 => "9.0",
            Self::CSharp10 => "10.0",
            Self::CSharp11 => "11.0",
            Self::CSharp12 => "12.0",
        }
    }

    pub fn supports_record_struct(&self) -> bool {
        *self >= Self::CSharp10
    }

    pub fn supports_record(&self) -> bool {
        *self >= Self::CSharp9
    }
}

impl fmt::Display for CSharpVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "c#{}", self.version_str())
    }
}

/// Target language and requested version/standard configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EmitOptions {
    pub cpp_std: CppStandard,
    pub go_ver: GoVersion,
    pub py_ver: PythonVersion,
    pub c_std: CStandard,
    pub rust_edition: RustEdition,
    pub ts_target: TsTarget,
    pub zig_ver: ZigVersion,
    pub java_ver: JavaVersion,
    pub csharp_ver: CSharpVersion,
}

impl EmitOptions {
    /// Configure from an optional standard string for a specific target name.
    pub fn for_target(target: &str, std_opt: Option<&str>) -> BpsResult<Self> {
        let mut opts = Self::default();
        let Some(s) = std_opt else {
            return Ok(opts);
        };
        match target {
            "cpp" => opts.cpp_std = CppStandard::parse(s)?,
            "go" => opts.go_ver = GoVersion::parse(s)?,
            "python" => opts.py_ver = PythonVersion::parse(s)?,
            "c" => opts.c_std = CStandard::parse(s)?,
            "rust" | "rust-typed" => opts.rust_edition = RustEdition::parse(s)?,
            "typescript" => opts.ts_target = TsTarget::parse(s)?,
            "zig" => opts.zig_ver = ZigVersion::parse(s)?,
            "java" => opts.java_ver = JavaVersion::parse(s)?,
            "csharp" | "cs" => opts.csharp_ver = CSharpVersion::parse(s)?,
            _ => {}
        }
        Ok(opts)
    }
}
