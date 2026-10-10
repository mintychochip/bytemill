from __future__ import annotations

from bps.errors import DecodeError, EncodeError


class Cursor:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.pos = 0

    def remaining(self) -> int:
        return len(self.data) - self.pos

    def read(self, n: int) -> bytes:
        if self.pos + n > len(self.data):
            raise DecodeError(f"need {n} bytes, have {self.remaining()}", offset=self.pos)
        chunk = self.data[self.pos : self.pos + n]
        self.pos += n
        return chunk

    def peek_u8(self) -> int:
        if self.pos >= len(self.data):
            raise DecodeError("unexpected end", offset=self.pos)
        return self.data[self.pos]

    def write(self, chunk: bytes) -> None:
        self.data += chunk  # type: ignore[assignment]

    @classmethod
    def for_encode(cls) -> Cursor:
        c = cls(b"")
        c.data = bytearray()
        return c

    def bytes_out(self) -> bytes:
        if isinstance(self.data, bytearray):
            return bytes(self.data)
        return self.data


class WriteCursor:
    def __init__(self) -> None:
        self.buf = bytearray()

    def emit(self, chunk: bytes) -> None:
        self.buf.extend(chunk)

    def result(self) -> bytes:
        return bytes(self.buf)
