from __future__ import annotations

from dataclasses import dataclass
from enum import Enum, auto

from bps.errors import ParseError

KEYWORDS = frozenset(
    {
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
    }
)


class TokKind(Enum):
    IDENT = auto()
    INT = auto()
    STRING = auto()
    LBRACE = auto()
    RBRACE = auto()
    LPAREN = auto()
    RPAREN = auto()
    LBRACK = auto()
    RBRACK = auto()
    SEMI = auto()
    COLON = auto()
    COMMA = auto()
    DOT = auto()
    EQ = auto()
    AT = auto()
    ELLIPSIS = auto()
    OP = auto()
    EOF = auto()


@dataclass(frozen=True)
class Token:
    kind: TokKind
    value: str | int | None
    pos: int


def tokenize(source: str) -> list[Token]:
    tokens: list[Token] = []
    i = 0
    n = len(source)

    def peek(k: int = 0) -> str:
        return source[i + k] if i + k < n else ""

    def advance(k: int = 1) -> None:
        nonlocal i
        i += k

    while i < n:
        start = i
        c = source[i]
        if c in " \t\r\n":
            advance()
            continue
        if c == "/" and peek(1) == "/":
            advance(2)
            while i < n and source[i] != "\n":
                advance()
            continue
        if c == "/" and peek(1) == "*":
            advance(2)
            while i < n - 1 and not (source[i] == "*" and source[i + 1] == "/"):
                advance()
            advance(2)
            continue
        if c == '"':
            advance()
            buf: list[str] = []
            while i < n and source[i] != '"':
                if source[i] == "\\" and i + 1 < n:
                    advance()
                    buf.append(source[i])
                    advance()
                else:
                    buf.append(source[i])
                    advance()
            if i >= n:
                raise ParseError("unterminated string", offset=start)
            advance()
            tokens.append(Token(TokKind.STRING, "".join(buf), start))
            continue
        if c.isdigit() or (c == "-" and peek(1).isdigit()):
            if c == "0" and peek(1).lower() == "x":
                advance(2)
                while i < n and source[i] in "0123456789abcdefABCDEF":
                    advance()
                text = source[start:i]
                val = int(text, 16)
            else:
                advance()
                while i < n and source[i].isdigit():
                    advance()
                text = source[start:i]
                val = int(text, 10)
            tokens.append(Token(TokKind.INT, val, start))
            continue
        if c == "." and peek(1) == "." and peek(2) == ".":
            tokens.append(Token(TokKind.ELLIPSIS, "...", start))
            advance(3)
            continue
        if c.isalpha() or c == "_":
            advance()
            while i < n and (source[i].isalnum() or source[i] == "_"):
                advance()
            text = source[start:i]
            tokens.append(Token(TokKind.IDENT, text, start))
            continue
        two = {"==", "!=", "<=", ">=", "&&", "||", "<<", ">>", "->", "=>"}
        if c + peek(1) in two:
            tokens.append(Token(TokKind.OP, c + peek(1), start))
            advance(2)
            continue
        single = {
            "{": TokKind.LBRACE,
            "}": TokKind.RBRACE,
            "(" : TokKind.LPAREN,
            ")": TokKind.RPAREN,
            "[": TokKind.LBRACK,
            "]": TokKind.RBRACK,
            ";": TokKind.SEMI,
            ":": TokKind.COLON,
            ",": TokKind.COMMA,
            ".": TokKind.DOT,
            "=": TokKind.EQ,
            "@": TokKind.AT,
        }
        if c in single:
            tokens.append(Token(single[c], c, start))
            advance()
            continue
        if c in "<>+-*/%&|^!?":
            tokens.append(Token(TokKind.OP, c, start))
            advance()
            continue
        raise ParseError(f"unexpected character {c!r}", offset=start)

    tokens.append(Token(TokKind.EOF, None, i))
    return tokens
