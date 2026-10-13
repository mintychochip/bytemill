//! JSON reader/writer with Python `json` module semantics — the CLI's
//! `decode`/`encode` paths, `dump-json`, the shape ledger, the pin gate and
//! rstom's `packets.json` all depend on byte-identical formatting.

use crate::errors::{BpsError, BpsResult};
pub use crate::value::Value;
use std::fmt::Write as _;

/// `json.dumps(v)` — insertion order, Python's default separators.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, None, false);
    out
}

/// `json.dumps(v, sort_keys=True)` — Python's default compact separators.
pub fn dumps_sorted(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, None, true);
    out
}

/// `json.dumps(v, sort_keys=True, indent=2)`.
pub fn dumps_pretty_sorted(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, Some(0), true);
    out
}

/// `json.dumps(v, indent=2)` — insertion order (the shape ledger).
pub fn dumps_pretty(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v, Some(0), false);
    out
}

fn sorted_pairs(v: &Value) -> Vec<(String, Value)> {
    let Value::Map(m) = v else { return Vec::new() };
    let mut pairs: Vec<(String, Value)> =
        m.iter().map(|(k, val)| (k.to_string(), val.clone())).collect();
    pairs.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    pairs
}

fn write_value(out: &mut String, v: &Value, indent: Option<usize>, sort_keys: bool) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::Float(f) => out.push_str(&float_repr(*f)),
        Value::Str(s) => write_json_string(out, s),
        Value::Bytes(b) => {
            // Python's `json.dumps` raises on bytes, so the Python CLI crashes on any packet
            // with a byte field; this port prints them as an array of ints instead.
            let mut arr = String::from("[");
            for (i, byte) in b.iter().enumerate() {
                if i > 0 {
                    arr.push_str(", ");
                }
                let _ = write!(arr, "{byte}");
            }
            out.push_str(&arr);
            out.push(']');
        }
        Value::List(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            match indent {
                None => {
                    out.push('[');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        write_value(out, item, None, sort_keys);
                    }
                    out.push(']');
                }
                Some(depth) => {
                    out.push('[');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        out.push('\n');
                        push_indent(out, depth + 1);
                        write_value(out, item, Some(depth + 1), sort_keys);
                    }
                    out.push('\n');
                    push_indent(out, depth);
                    out.push(']');
                }
            }
        }
        Value::Map(m) => {
            let pairs: Vec<(String, Value)> = if sort_keys {
                sorted_pairs(v)
            } else {
                m.iter().map(|(k, val)| (k.to_string(), val.clone())).collect()
            };
            if pairs.is_empty() {
                out.push_str("{}");
                return;
            }
            match indent {
                None => {
                    out.push('{');
                    for (i, (k, val)) in pairs.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        write_json_string(out, k);
                        out.push_str(": ");
                        write_value(out, val, None, sort_keys);
                    }
                    out.push('}');
                }
                Some(depth) => {
                    out.push('{');
                    for (i, (k, val)) in pairs.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        out.push('\n');
                        push_indent(out, depth + 1);
                        write_json_string(out, k);
                        out.push_str(": ");
                        write_value(out, val, Some(depth + 1), sort_keys);
                    }
                    out.push('\n');
                    push_indent(out, depth);
                    out.push('}');
                }
            }
        }
    }
}

fn push_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// `ensure_ascii=True` string escaping, matching Python's `json.encoder`.
pub fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let cp = c as u32;
                if cp > 0xffff {
                    let v = cp - 0x10000;
                    let hi = 0xd800 + (v >> 10);
                    let lo = 0xdc00 + (v & 0x3ff);
                    let _ = write!(out, "\\u{hi:04x}\\u{lo:04x}");
                } else {
                    let _ = write!(out, "\\u{cp:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// Rust's shortest digits and Python's differ only on an exact tie: when two decimals of the
/// shortest length are equally close to the double, Rust takes the upper one and Python's `repr`
/// (David Gay's dtoa) the one with an even last digit. A tie means the double's exact decimal
/// expansion is one digit longer than the shortest form. For `a == m / 2^k` with `m` odd that
/// expansion is the digits of `m * 5^k`, so only a product that fits `u128` can tie.
/// Returns Python's digits and the decimal exponent of the first digit.
fn python_shortest(a: f64, digits: String, e: i32) -> (String, i32) {
    let bits = a.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (mut m, mut e2) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1u64 << 52), biased - 1075)
    };
    let trailing = m.trailing_zeros();
    m >>= trailing;
    e2 += trailing as i32;
    if e2 >= 0 {
        return (digits, e); // an integer: its expansion is its shortest form
    }
    let k = (-e2) as u32;
    let Some(product) = 5u128.checked_pow(k).and_then(|p| p.checked_mul(u128::from(m))) else {
        return (digits, e);
    };
    let exact = product.to_string();
    let n = digits.len();
    if exact.len() != n + 1 {
        return (digits, e);
    }
    // A tie: `exact` ends in 5, halfway between `lower` and `lower + 1`.
    let lower = &exact[..n];
    let mut upper = lower.as_bytes().to_vec();
    let carried = upper.iter_mut().rev().all(|b| {
        if *b == b'9' {
            *b = b'0';
            true
        } else {
            *b += 1;
            false
        }
    });
    if carried {
        return (digits, e); // all nines: the upper candidate has an extra digit
    }
    let upper = String::from_utf8(upper).expect("ascii digits");
    let even = if (lower.as_bytes()[n - 1] - b'0') % 2 == 0 { lower.to_string() } else { upper };
    let exp_first = exact.len() as i32 - 1 - k as i32;
    let candidate = format!("{even}e{}", exp_first - (n as i32 - 1));
    if candidate.parse::<f64>() == Ok(a) {
        (even, exp_first)
    } else {
        (digits, e)
    }
}

/// Python `repr(float)`: shortest round-trip digits, fixed notation while the
/// decimal exponent is in `[-4, 16)`, scientific otherwise with a signed,
/// zero-padded exponent.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-Infinity".to_string() } else { "Infinity".to_string() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0".to_string() } else { "0.0".to_string() };
    }
    let neg = f < 0.0;
    let a = f.abs();
    // `{:e}` gives shortest-round-trip digits, e.g. "2.25e0", "1e16", "1e-5".
    let sci = format!("{a:e}");
    let (mant, exp_str) = sci.split_once('e').expect("format has exponent");
    let e: i32 = exp_str.parse().expect("valid exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let (digits, e) = python_shortest(a, digits, e);
    let n = digits.len() as i32;

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if e < -4 || e >= 16 {
        out.push_str(&digits[..1]);
        if n > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let sign = if e < 0 { '-' } else { '+' };
        let _ = write!(out, "e{sign}{:02}", e.abs());
    } else if e >= 0 {
        // Decimal point sits after e+1 digits.
        let point = (e + 1) as usize;
        if (e + 1) >= n {
            out.push_str(&digits);
            for _ in 0..(e + 1 - n) {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..point]);
            out.push('.');
            out.push_str(&digits[point..]);
        }
    } else {
        out.push_str("0.");
        for _ in 0..(-e - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    }
    out
}

// ---------------------------------------------------------------------------
// Reader: `json.loads`, strict like Python's default scanner.
// ---------------------------------------------------------------------------

pub fn loads(text: &str) -> BpsResult<Value> {
    let mut p = JsonParser { chars: text.chars().collect(), pos: 0 };
    p.skip_ws();
    let v = p.parse_value()?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(json_err("Extra data"));
    }
    Ok(v)
}

fn json_err(msg: &str) -> BpsError {
    BpsError::new(crate::errors::ErrorKind::Shape, format!("json error: {msg}"))
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: char) -> BpsResult<()> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(json_err(&format!("Expecting '{c}'")))
        }
    }

    fn parse_value(&mut self) -> BpsResult<Value> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => Ok(Value::Str(std::rc::Rc::from(self.parse_string()?.as_str()))),
            Some('t') => self.parse_lit("true", Value::Bool(true)),
            Some('f') => self.parse_lit("false", Value::Bool(false)),
            Some('n') => self.parse_lit("null", Value::Null),
            Some('N') => self.parse_lit("NaN", Value::Float(f64::NAN)),
            Some('I') => self.parse_lit("Infinity", Value::Float(f64::INFINITY)),
            Some('-') => {
                if self.chars.get(self.pos + 1) == Some(&'I') {
                    self.parse_lit("-Infinity", Value::Float(f64::NEG_INFINITY))
                } else {
                    self.parse_number()
                }
            }
            Some(c) if c.is_ascii_digit() => self.parse_number(),
            _ => Err(json_err("Expecting value")),
        }
    }

    fn parse_lit(&mut self, lit: &str, val: Value) -> BpsResult<Value> {
        for expected in lit.chars() {
            if self.bump() != Some(expected) {
                return Err(json_err(&format!("Expecting {lit}")));
            }
        }
        Ok(val)
    }

    fn parse_object(&mut self) -> BpsResult<Value> {
        self.expect('{')?;
        let mut map = crate::ordmap::OrderedMap::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::map(map));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let val = self.parse_value()?;
            map.insert(&key, val);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err(json_err("Expecting ',' delimiter")),
            }
        }
        Ok(Value::map(map))
    }

    fn parse_array(&mut self) -> BpsResult<Value> {
        self.expect('[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(Value::list(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.bump() {
                Some(',') => continue,
                Some(']') => break,
                _ => return Err(json_err("Expecting ',' delimiter")),
            }
        }
        Ok(Value::list(items))
    }

    fn parse_string(&mut self) -> BpsResult<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(json_err("Unterminated string")),
                Some('"') => break,
                Some('\\') => {
                    let esc = self.bump().ok_or_else(|| json_err("Unterminated string"))?;
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{08}'),
                        'f' => out.push('\u{0c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xd800..0xdc00).contains(&cp) {
                                // High surrogate: expect a following low one.
                                if self.peek() == Some('\\')
                                    && self.chars.get(self.pos + 1) == Some(&'u')
                                {
                                    self.pos += 2;
                                    let lo = self.parse_hex4()?;
                                    if (0xdc00..0xe000).contains(&lo) {
                                        let c = 0x10000 + ((cp - 0xd800) << 10) + (lo - 0xdc00);
                                        out.push(
                                            char::from_u32(c)
                                                .ok_or_else(|| json_err("Invalid escape"))?,
                                        );
                                        continue;
                                    }
                                    return Err(json_err("Invalid \\uXXXX escape"));
                                }
                                return Err(json_err("lone surrogate"));
                            }
                            out.push(
                                char::from_u32(cp).ok_or_else(|| json_err("Invalid \\uXXXX escape"))?,
                            );
                        }
                        _ => return Err(json_err("Invalid \\escape")),
                    }
                }
                Some(c) if (c as u32) < 0x20 => {
                    return Err(json_err("Invalid control character"));
                }
                Some(c) => out.push(c),
            }
        }
        Ok(out)
    }

    fn parse_hex4(&mut self) -> BpsResult<u32> {
        let mut v: u32 = 0;
        for _ in 0..4 {
            let c = self.bump().ok_or_else(|| json_err("Invalid \\uXXXX escape"))?;
            let d = c.to_digit(16).ok_or_else(|| json_err("Invalid \\uXXXX escape"))?;
            v = v * 16 + d;
        }
        Ok(v)
    }

    fn parse_number(&mut self) -> BpsResult<Value> {
        let start = self.pos;
        let mut is_float = false;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        // Integer part: 0 or [1-9][0-9]* (no leading zeros).
        match self.peek() {
            Some('0') => {
                self.pos += 1;
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    return Err(json_err("Invalid number"));
                }
            }
            Some(c) if c.is_ascii_digit() => {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return Err(json_err("Expecting value")),
        }
        if self.peek() == Some('.') {
            is_float = true;
            self.pos += 1;
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(json_err("Expecting digit"));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.pos += 1;
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(json_err("Expecting digit"));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            text.parse::<f64>()
                .map(Value::Float)
                .map_err(|_| json_err("Invalid number"))
        } else {
            match text.parse::<i128>() {
                Ok(i) => Ok(Value::Int(i)),
                Err(_) => text
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|_| json_err("Invalid number")),
            }
        }
    }
}
