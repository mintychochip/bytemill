//! Lexer, a faithful port of `bps/lexer.py`.
//!
//! Positions are counted in Unicode scalar values (Python `str` indices), not
//! bytes, so `Token.pos` matches the offsets in Python error messages. Identifiers use
//! Python's `str.isalpha`/`isalnum` (Unicode), approximated by `char::is_alphabetic` /
//! `is_alphanumeric`; number literals stay ASCII-only.

use crate::errors::{BpsResult, parse, parse_at, py_repr_str};
use std::fmt;

pub const KEYWORDS: &[&str] = &[
    "encoding",
    "enum",
    "packet",
    "protocol",
    "frame",
    "route",
    "require",
    "match",
    "switch",
    "until",
    "until_end",
    "count",
    "if",
    "as",
    "signed",
    "true",
    "false",
    "rest",
    "import",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokKind {
    Ident,
    Int,
    Str,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Semi,
    Colon,
    Comma,
    Dot,
    Eq,
    At,
    Ellipsis,
    Op,
    Eof,
}

/// `Display` matches Python's `str(TokKind.X)`, which f-string interpolation
/// produces in parser error messages: `TokKind.IDENT`, not `IDENT`.
impl fmt::Display for TokKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            TokKind::Ident => "IDENT",
            TokKind::Int => "INT",
            TokKind::Str => "STRING",
            TokKind::LBrace => "LBRACE",
            TokKind::RBrace => "RBRACE",
            TokKind::LParen => "LPAREN",
            TokKind::RParen => "RPAREN",
            TokKind::LBracket => "LBRACK",
            TokKind::RBracket => "RBRACK",
            TokKind::Semi => "SEMI",
            TokKind::Colon => "COLON",
            TokKind::Comma => "COMMA",
            TokKind::Dot => "DOT",
            TokKind::Eq => "EQ",
            TokKind::At => "AT",
            TokKind::Ellipsis => "ELLIPSIS",
            TokKind::Op => "OP",
            TokKind::Eof => "EOF",
        };
        write!(f, "TokKind.{name}")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokValue {
    Text(String),
    Int(i128),
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokKind,
    pub value: TokValue,
    pub pos: usize,
}

impl Token {
    /// The token's text (`str(tok.value)` for idents/ops, `None` for EOF).
    pub fn text(&self) -> Option<&str> {
        match &self.value {
            TokValue::Text(s) => Some(s.as_str()),
            TokValue::Int(_) | TokValue::None => None,
        }
    }

    /// Python's `repr(tok.value)`: a quoted string, a bare int, or `None`.
    pub fn py_repr_value(&self) -> String {
        match &self.value {
            TokValue::Text(s) => py_repr_str(s),
            TokValue::Int(v) => v.to_string(),
            TokValue::None => "None".to_string(),
        }
    }

    /// Python's `int(tok.value)`; ident values raise, int values convert.
    pub fn as_int(&self) -> BpsResult<i128> {
        match &self.value {
            TokValue::Int(v) => Ok(*v),
            _ => Err(parse(format!("expected int token, got {:?}", self.value))),
        }
    }
}

pub fn tokenize(source: &str) -> BpsResult<Vec<Token>> {
    let chars: Vec<char> = source.chars().collect();
    let n = chars.len();
    let mut tokens: Vec<Token> = Vec::new();
    let mut i = 0usize;

    macro_rules! peek {
        ($k:expr) => {
            if i + $k < n { chars[i + $k] } else { '\0' }
        };
    }

    while i < n {
        let start = i;
        let c = chars[i];
        if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
            i += 1;
            continue;
        }
        if c == '/' && peek!(1) == '/' {
            i += 2;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && peek!(1) == '*' {
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '"' {
            i += 1;
            let mut buf = String::new();
            while i < n && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < n {
                    i += 1;
                    buf.push(chars[i]);
                    i += 1;
                } else {
                    buf.push(chars[i]);
                    i += 1;
                }
            }
            if i >= n {
                return Err(parse_at("unterminated string", start));
            }
            i += 1;
            tokens.push(Token {
                kind: TokKind::Str,
                value: TokValue::Text(buf),
                pos: start,
            });
            continue;
        }
        if c.is_ascii_digit() || (c == '-' && peek!(1).is_ascii_digit()) {
            if c == '0' && peek!(1).eq_ignore_ascii_case(&'x') {
                i += 2;
                while i < n && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                if text.len() == 2 {
                    // Python's `int("0x", 16)` raises a raw ValueError here.
                    return Err(parse_at(
                        format!("hex literal has no digits: {text}"),
                        start,
                    ));
                }
                // Python int(text, 16) with an arbitrary-precision result; schemas
                // stay within u64/i128 range.
                let val = i128::from_str_radix(&text[2..], 16)
                    .map_err(|_| parse_at(format!("int too large: {text}"), start))?;
                tokens.push(Token {
                    kind: TokKind::Int,
                    value: TokValue::Int(val),
                    pos: start,
                });
                continue;
            }
            i += 1;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let val: i128 = text
                .parse()
                .map_err(|_| parse_at(format!("int too large: {text}"), start))?;
            tokens.push(Token {
                kind: TokKind::Int,
                value: TokValue::Int(val),
                pos: start,
            });
            continue;
        }
        if c == '.' && peek!(1) == '.' && peek!(2) == '.' {
            tokens.push(Token {
                kind: TokKind::Ellipsis,
                value: TokValue::Text("...".into()),
                pos: start,
            });
            i += 3;
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            i += 1;
            while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            tokens.push(Token {
                kind: TokKind::Ident,
                value: TokValue::Text(text),
                pos: start,
            });
            continue;
        }
        let two: String = [c, peek!(1)].iter().collect();
        const TWO_OPS: &[&str] = &["==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "->", "=>"];
        if TWO_OPS.contains(&two.as_str()) {
            tokens.push(Token {
                kind: TokKind::Op,
                value: TokValue::Text(two),
                pos: start,
            });
            i += 2;
            continue;
        }
        let single = match c {
            '{' => Some(TokKind::LBrace),
            '}' => Some(TokKind::RBrace),
            '(' => Some(TokKind::LParen),
            ')' => Some(TokKind::RParen),
            '[' => Some(TokKind::LBracket),
            ']' => Some(TokKind::RBracket),
            ';' => Some(TokKind::Semi),
            ':' => Some(TokKind::Colon),
            ',' => Some(TokKind::Comma),
            '.' => Some(TokKind::Dot),
            '=' => Some(TokKind::Eq),
            '@' => Some(TokKind::At),
            _ => None,
        };
        if let Some(kind) = single {
            tokens.push(Token {
                kind,
                value: TokValue::Text(c.to_string()),
                pos: start,
            });
            i += 1;
            continue;
        }
        if "<>+-*/%&|^!?".contains(c) {
            tokens.push(Token {
                kind: TokKind::Op,
                value: TokValue::Text(c.to_string()),
                pos: start,
            });
            i += 1;
            continue;
        }
        return Err(parse_at(
            format!("unexpected character {}", py_repr_str(&c.to_string())),
            start,
        ));
    }

    tokens.push(Token {
        kind: TokKind::Eof,
        value: TokValue::None,
        pos: i,
    });
    Ok(tokens)
}
