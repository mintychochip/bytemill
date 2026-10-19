//! Text file reads with Python's universal newlines.
//!
//! Python's `Path.read_text()` and text-mode `open()` turn `\r\n` and a lone `\r` into `\n`
//! before the caller sees the text. Offsets in lexer errors, string literals and everything
//! extracted from a source file therefore never contain a `\r`; `std::fs::read_to_string` keeps
//! them, so every read that Python made with `read_text` goes through here.

use std::io;
use std::path::Path;

/// `Path.read_text(encoding="utf-8")`.
pub fn read_text(path: &Path) -> io::Result<String> {
    let mut text = std::fs::read_to_string(path)?;
    if text.contains('\r') {
        text = text.replace("\r\n", "\n").replace('\r', "\n");
    }
    Ok(text)
}
