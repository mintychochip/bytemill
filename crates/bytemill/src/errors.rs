//! Error types, mirroring `bps/errors.py`.
//!
//! `BmlError` carries an optional byte/char offset; `Display` matches the Python
//! wire format exactly: `"{message}"` or `"{message} (offset {offset})"`.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Parse,
    Resolve,
    Decode,
    Encode,
    Emit,
    Shape,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Parse => "ParseError",
            ErrorKind::Resolve => "ResolveError",
            ErrorKind::Decode => "DecodeError",
            ErrorKind::Encode => "EncodeError",
            ErrorKind::Emit => "EmitError",
            ErrorKind::Shape => "ShapeError",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmlError {
    pub kind: ErrorKind,
    pub message: String,
    pub offset: Option<usize>,
}

impl BmlError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        BmlError {
            kind,
            message: message.into(),
            offset: None,
        }
    }

    pub fn with_offset(kind: ErrorKind, message: impl Into<String>, offset: usize) -> Self {
        BmlError {
            kind,
            message: message.into(),
            offset: Some(offset),
        }
    }

    pub fn parse(message: impl Into<String>) -> Self {
        BmlError::new(ErrorKind::Parse, message)
    }

    pub fn parse_at(message: impl Into<String>, offset: usize) -> Self {
        BmlError::with_offset(ErrorKind::Parse, message, offset)
    }

    pub fn resolve(message: impl Into<String>) -> Self {
        BmlError::new(ErrorKind::Resolve, message)
    }

    pub fn decode(message: impl Into<String>) -> Self {
        BmlError::new(ErrorKind::Decode, message)
    }

    pub fn decode_at(message: impl Into<String>, offset: usize) -> Self {
        BmlError::with_offset(ErrorKind::Decode, message, offset)
    }

    pub fn encode(message: impl Into<String>) -> Self {
        BmlError::new(ErrorKind::Encode, message)
    }

    pub fn emit(message: impl Into<String>) -> Self {
        BmlError::new(ErrorKind::Emit, message)
    }

    pub fn shape(message: impl Into<String>) -> Self {
        BmlError::new(ErrorKind::Shape, message)
    }

    pub fn is(&self, kind: ErrorKind) -> bool {
        self.kind == kind
    }
}

impl fmt::Display for BmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            Some(off) => write!(f, "{} (offset {})", self.message, off),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for BmlError {}

pub type BmlResult<T> = Result<T, BmlError>;

/// Free constructors, named after the Python exception classes.
pub fn parse(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Parse, message)
}

pub fn parse_at(message: impl Into<String>, offset: usize) -> BmlError {
    BmlError::with_offset(ErrorKind::Parse, message, offset)
}

pub fn resolve(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Resolve, message)
}

pub fn decode(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Decode, message)
}

pub fn decode_at(message: impl Into<String>, offset: usize) -> BmlError {
    BmlError::with_offset(ErrorKind::Decode, message, offset)
}

pub fn encode(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Encode, message)
}

pub fn emit(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Emit, message)
}

pub fn shape(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Shape, message)
}

/// Python `repr(str)`: single quotes unless the text contains `'` and no `"`,
/// with `\n`-style escapes for control characters, and `\xNN`/`\uNNNN` for other characters
/// `str.isprintable` rejects. Parser and interpreter error messages interpolate `{name!r}`,
/// so this must match.
pub fn py_repr_str(s: &str) -> String {
    let has_single = s.contains('\'');
    let has_double = s.contains('"');
    let quote = if has_single && !has_double { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_is_printable(c) => {
                let n = c as u32;
                let _ = if n <= 0xff {
                    std::fmt::Write::write_fmt(&mut out, format_args!("\\x{n:02x}"))
                } else if n <= 0xffff {
                    std::fmt::Write::write_fmt(&mut out, format_args!("\\u{n:04x}"))
                } else {
                    std::fmt::Write::write_fmt(&mut out, format_args!("\\U{n:08x}"))
                };
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `str.isprintable` for one character. Rust has no general-category table, so this
/// lists what `isprintable` rejects that can plausibly appear in a schema or message:
/// controls, the Unicode space separators other than `' '`, and the common format characters.
fn py_is_printable(c: char) -> bool {
    let n = c as u32;
    !(n < 0x20
        || (0x7f..=0xa0).contains(&n)
        || n == 0xad
        || n == 0x1680
        || n == 0x180e
        || (0x2000..=0x200f).contains(&n)
        || (0x2028..=0x202f).contains(&n)
        || (0x205f..=0x206f).contains(&n)
        || n == 0x3000
        || n == 0xfeff
        || (0xfff9..=0xfffb).contains(&n))
}
