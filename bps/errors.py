from __future__ import annotations


class BpsError(Exception):
    def __init__(self, message: str, *, offset: int | None = None) -> None:
        self.offset = offset
        if offset is not None:
            super().__init__(f"{message} (offset {offset})")
        else:
            super().__init__(message)


class ParseError(BpsError):
    pass


class ResolveError(BpsError):
    pass


class DecodeError(BpsError):
    pass


class EncodeError(BpsError):
    pass
