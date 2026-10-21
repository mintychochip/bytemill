//! Byte cursors, porting `bps/interp/cursor.py`.

use crate::errors::{BpsResult, decode_at};

pub struct Cursor<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    /// Reject a counted repeat of more than this many elements; `None` never rejects.
    /// A check that decodes random bytes sets it: a list of zero-width elements would
    /// otherwise loop `count` times without reading a byte.
    pub repeat_limit: Option<usize>,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Cursor {
            data,
            pos: 0,
            repeat_limit: None,
        }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn read(&mut self, n: usize) -> BpsResult<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(decode_at(
                format!("need {n} bytes, have {}", self.remaining()),
                self.pos,
            ));
        }
        let chunk = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(chunk)
    }

    pub fn peek_u8(&self) -> BpsResult<u8> {
        if self.pos >= self.data.len() {
            return Err(decode_at("unexpected end", self.pos));
        }
        Ok(self.data[self.pos])
    }
}

pub struct WriteCursor {
    pub buf: Vec<u8>,
}

impl WriteCursor {
    pub fn new() -> Self {
        WriteCursor { buf: Vec::new() }
    }

    pub fn emit(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    pub fn result(self) -> Vec<u8> {
        self.buf
    }
}

impl Default for WriteCursor {
    fn default() -> Self {
        Self::new()
    }
}
