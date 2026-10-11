from __future__ import annotations

from bps import ast
from bps.errors import ParseError
from bps.lexer import TokKind, Token, tokenize


class Parser:
    def __init__(self, tokens: list[Token]) -> None:
        self.tokens = tokens
        self.pos = 0

    def cur(self) -> Token:
        return self.tokens[self.pos]

    def peek(self, k: int = 1) -> Token:
        return self.tokens[self.pos + k]

    def at(self, kind: TokKind) -> bool:
        return self.cur().kind == kind

    def eat(self, kind: TokKind, msg: str | None = None) -> Token:
        tok = self.cur()
        if tok.kind != kind:
            raise ParseError(msg or f"expected {kind}, got {tok.kind}", offset=tok.pos)
        self.pos += 1
        return tok

    def try_eat(self, kind: TokKind) -> Token | None:
        if self.at(kind):
            self.pos += 1
            return self.tokens[self.pos - 1]
        return None

    def parse(self) -> ast.Schema:
        encodings: list[ast.EncodingDecl] = []
        enums: list[ast.EnumDecl] = []
        packets: list[ast.PacketDecl] = []
        protocols: list[ast.ProtocolDecl] = []
        imports: list[str] = []
        while not self.at(TokKind.EOF):
            name = self.cur().value
            if name == "import":
                self.pos += 1
                imports.append(str(self.eat(TokKind.STRING, "expected import path").value))
            elif name == "encoding":
                encodings.append(self.parse_encoding())
            elif name == "enum":
                enums.append(self.parse_enum())
            elif name == "packet":
                packets.append(self.parse_packet())
            elif name == "protocol":
                protocols.append(self.parse_protocol())
            else:
                raise ParseError(f"expected top-level declaration, got {name!r}", offset=self.cur().pos)
        return ast.Schema(
            tuple(encodings),
            tuple(enums),
            tuple(packets),
            tuple(protocols),
            tuple(imports),
        )

    def parse_encoding(self) -> ast.EncodingDecl:
        self.eat_ident("encoding")
        name = self.eat_ident_value()
        params = self.parse_params()
        if self.at(TokKind.EQ):
            self.eat(TokKind.EQ)
        if self.at(TokKind.LBRACE):
            body = self.parse_block_type()
            yield_expr = None
            if self.try_eat(TokKind.EQ):
                yield_expr = self.parse_expr()
            return ast.EncodingDecl(name, params, body, yield_expr)
        return ast.EncodingDecl(name, params, self.parse_type_ref())

    def parse_params(self) -> tuple[ast.Param, ...]:
        if not self.at(TokKind.LPAREN):
            return ()
        self.eat(TokKind.LPAREN)
        params: list[ast.Param] = []
        if not self.at(TokKind.RPAREN):
            while True:
                pname = self.eat_ident_value()
                default = None
                if self.try_eat(TokKind.EQ):
                    default = int(self.eat(TokKind.INT).value)
                params.append(ast.Param(pname, default))
                if not self.try_eat(TokKind.COMMA):
                    break
        self.eat(TokKind.RPAREN)
        return tuple(params)

    def parse_enum(self) -> ast.EnumDecl:
        self.eat_ident("enum")
        name = self.eat_ident_value()
        self.eat(TokKind.COLON)
        base = self.eat_ident_value()
        self.eat(TokKind.LBRACE)
        members: list[ast.EnumMember] = []
        while not self.at(TokKind.RBRACE):
            mname = self.eat_ident_value()
            self.eat(TokKind.EQ)
            val = int(self.eat(TokKind.INT).value)
            members.append(ast.EnumMember(mname, val))
            self.try_eat(TokKind.COMMA)
        self.eat(TokKind.RBRACE)
        return ast.EnumDecl(name, base, tuple(members))

    def parse_packet(self) -> ast.PacketDecl:
        self.eat_ident("packet")
        name = self.eat_ident_value()
        items = self.parse_block_items()
        return ast.PacketDecl(name, items)

    def parse_protocol(self) -> ast.ProtocolDecl:
        self.eat_ident("protocol")
        name = self.eat_ident_value()
        self.eat(TokKind.AT)
        version = self.eat_ident_value()
        self.eat(TokKind.LBRACE)
        protocol_id = 0
        frames: list[ast.FrameDecl] = []
        routes: list[ast.RouteDecl] = []
        while not self.at(TokKind.RBRACE):
            kw = self.cur().value
            if kw == "id":
                self.pos += 1
                self.eat(TokKind.EQ)
                protocol_id = int(self.eat(TokKind.INT).value)
            elif kw == "frame":
                self.pos += 1
                fname = self.eat_ident_value()
                items = self.parse_block_items()
                frames.append(ast.FrameDecl(fname, items))
            elif kw == "route":
                self.pos += 1
                state = self.eat_ident_value()
                self.eat(TokKind.DOT)
                direction = self.eat_ident_value()
                pid = int(self.eat(TokKind.INT).value)
                self.eat(TokKind.OP)  # =>
                pname = self.eat_ident_value()
                frame = None
                if self.cur().value == "frame":
                    self.pos += 1
                    frame = self.eat_ident_value()
                routes.append(ast.RouteDecl(state, direction, pid, pname, frame))
            else:
                raise ParseError(f"unexpected in protocol: {kw!r}", offset=self.cur().pos)
        self.eat(TokKind.RBRACE)
        return ast.ProtocolDecl(name, version, protocol_id, tuple(frames), tuple(routes))

    def parse_block_items(self) -> tuple[ast.BlockItem, ...]:
        self.eat(TokKind.LBRACE)
        items: list[ast.BlockItem] = []
        while not self.at(TokKind.RBRACE):
            if self.at(TokKind.ELLIPSIS):
                self.pos += 1
                sname = self.eat_ident_value()
                args = ()
                if self.at(TokKind.LPAREN):
                    args = self.parse_call_args()
                items.append(ast.BlockItem("spread", spread_name=sname, spread_args=args))
                continue
            if self.cur().value == "require":
                self.pos += 1
                items.append(ast.BlockItem("require", require_expr=self.parse_expr()))
                continue
            if self.cur().value == "if":
                self.pos += 1
                cond = self.parse_expr()
                group_items = self.parse_block_type().items
                items.append(
                    ast.BlockItem("group", group_if=cond, group_items=group_items)
                )
                continue
            # `match:` / `match =` are a field and a computed value. A statement
            # is `match (action) { ... }` or `switch action { ... }`.
            if self.cur().value in ("match", "switch") and self.peek().kind not in (
                TokKind.COLON,
                TokKind.EQ,
            ):
                items.append(
                    ast.BlockItem("match", match=self.parse_match_type(statement=True))
                )
                continue
            name = self.eat_ident_value()
            if self.try_eat(TokKind.EQ):
                items.append(
                    ast.BlockItem(
                        "computed",
                        computed_name=name,
                        computed_expr=self.parse_expr(),
                    )
                )
                continue
            self.eat(TokKind.COLON)
            type_ref = self.parse_field_type()
            if_expr = None
            if self.cur().value == "if":
                # `if cond {` after a *complete field* opens a conditional group
                # for the fields that follow it, so this field is emitted first.
                mark = self.pos
                self.pos += 1
                cond = self.parse_expr()
                if self.at(TokKind.LBRACE):
                    self.pos = mark
                    items.append(
                        ast.BlockItem(
                            "field",
                            wire=ast.WireField(name, type_ref, None, None),
                        )
                    )
                    items.append(self.parse_conditional_group_item())
                    continue
                if_expr = cond
            encode_expr = None
            if self.try_eat(TokKind.EQ):
                encode_expr = self.parse_expr()
            repeat = None
            if self.at(TokKind.LBRACK):
                repeat = self.parse_repeat()
            items.append(
                ast.BlockItem(
                    "field",
                    wire=ast.WireField(name, type_ref, if_expr, repeat, encode_expr),
                )
            )
            self.try_eat(TokKind.COMMA)
        self.eat(TokKind.RBRACE)
        return tuple(items)

    def parse_repeat(self) -> ast.Repeat:
        self.eat(TokKind.LBRACK)
        self.eat(TokKind.RBRACK)
        if self.cur().value == "until_end":
            self.pos += 1
            return ast.Repeat("until_end", None)
        if self.cur().value == "until":
            self.pos += 1
            return ast.Repeat("until", self.parse_expr())
        if self.cur().value == "count":
            self.pos += 1
            self.eat(TokKind.EQ)
            return ast.Repeat("count", self.parse_expr())
        raise ParseError("expected until, until_end, or count", offset=self.cur().pos)

    def parse_field_type(self) -> ast.TypeRef | ast.BlockType | ast.MatchType:
        if self.cur().value in ("match", "switch"):
            return self.parse_match_type()
        if self.at(TokKind.LBRACE):
            return self.parse_block_type()
        return self.parse_type_ref()

    def parse_match_type(self, statement: bool = False) -> ast.MatchType:
        kw = self.cur().value
        if kw not in ("match", "switch"):
            raise ParseError(f"expected match or switch, got {kw!r}", offset=self.cur().pos)
        self.pos += 1
        tag = self.parse_expr()
        self.eat(TokKind.LBRACE)
        arms: list[ast.MatchArm] = []
        while not self.at(TokKind.RBRACE):
            tag_val, tags = self.parse_arm_tags()
            self.eat_op("=>")
            if statement:
                if not self.at(TokKind.LBRACE):
                    raise ParseError("match arm must be a block", offset=self.cur().pos)
                body: ast.TypeRef | ast.BlockType = self.parse_block_type()
            elif self.at(TokKind.LBRACE):
                body = self.parse_block_type()
            else:
                body = self.parse_type_ref_only()
            arms.append(ast.MatchArm(tag_val, body, tags))
            self.try_eat(TokKind.COMMA)
        self.eat(TokKind.RBRACE)
        return ast.MatchType(tag, tuple(arms))

    def parse_arm_tags(self) -> tuple[int | str | None, tuple[int | str, ...]]:
        """One arm's tags: `_`, `0`, or `0 | 1 | name`."""
        if self.cur().value == "_":
            self.pos += 1
            if self.cur().kind == TokKind.OP and self.cur().value == "|":
                raise ParseError(
                    "'_' cannot be combined with other tags", offset=self.cur().pos
                )
            return None, ()
        tags = [self.parse_one_arm_tag()]
        while self.cur().kind == TokKind.OP and self.cur().value == "|":
            self.pos += 1
            tags.append(self.parse_one_arm_tag())
        if len(tags) == 1:
            return tags[0], ()
        return tags[0], tuple(tags)

    def parse_one_arm_tag(self) -> int | str:
        if self.cur().value == "_":
            raise ParseError(
                "'_' cannot be combined with other tags", offset=self.cur().pos
            )
        if self.at(TokKind.INT):
            return int(self.eat(TokKind.INT).value)
        return self.eat_ident_value()

    def parse_block_type(self) -> ast.BlockType:
        items = self.parse_block_items()
        return ast.BlockType(items)

    def parse_conditional_group_item(self) -> ast.BlockItem:
        """Parse `if cond { fields }` as a single group BlockItem."""
        cond, items = self.parse_conditional_group()
        return ast.BlockItem("group", group_if=cond, group_items=items)

    def parse_conditional_group(self) -> tuple[ast.Expr, tuple[ast.BlockItem, ...]]:
        """Parse `if cond { fields }`; returns (condition, items)."""
        self.pos += 1  # 'if'
        cond = self.parse_expr()
        items = self.parse_block_type().items
        return cond, items

    def parse_type_body(self) -> ast.TypeRef | ast.BlockType:
        if self.at(TokKind.LBRACE):
            return self.parse_block_type()
        return self.parse_type_ref()

    def parse_type_ref_only(self) -> ast.TypeRef:
        return self.parse_type_ref()

    def parse_type_ref(self) -> ast.TypeRef:
        name = self.eat_ident_value()
        args: tuple[ast.Expr, ...] = ()
        if self.at(TokKind.LPAREN):
            args = self.parse_call_args()
        transforms: list[str] = []
        while self.cur().value == "as":
            self.pos += 1
            transforms.append(self.parse_transform())
        return ast.TypeRef(name, args, tuple(transforms))

    def parse_transform(self) -> str:
        if self.cur().value in ("utf8", "mutf8"):
            t = self.cur().value
            self.pos += 1
            return t
        if self.cur().value == "fixed":
            self.pos += 1
            self.eat(TokKind.LPAREN)
            num = self.parse_expr()
            self.eat(TokKind.OP)  # /
            den = self.parse_expr()
            self.eat(TokKind.RPAREN)
            return f"fixed({self.expr_src(num)}/{self.expr_src(den)})"
        raise ParseError("expected transform", offset=self.cur().pos)

    def expr_src(self, e: ast.Expr) -> str:
        if e.kind == "int":
            return str(e.value)
        return "?"

    def parse_call_args(self) -> tuple[ast.Expr, ...]:
        self.eat(TokKind.LPAREN)
        args: list[ast.Expr] = []
        if not self.at(TokKind.RPAREN):
            while True:
                args.append(self.parse_expr())
                if not self.try_eat(TokKind.COMMA):
                    break
        self.eat(TokKind.RPAREN)
        return tuple(args)

    def parse_expr(self) -> ast.Expr:
        return self.parse_ternary()

    def parse_ternary(self) -> ast.Expr:
        cond = self.parse_or()
        if self.cur().kind == TokKind.OP and self.cur().value == "?":
            self.pos += 1
            yes = self.parse_expr()
            self.eat(TokKind.COLON)
            no = self.parse_ternary()
            return ast.Expr("ternary", left=cond, right=yes, args=(no,))
        return cond

    def parse_or(self) -> ast.Expr:
        left = self.parse_and()
        while self.cur().kind == TokKind.OP and self.cur().value == "||":
            self.pos += 1
            left = ast.Expr("binop", value="||", left=left, right=self.parse_and())
        return left

    def parse_and(self) -> ast.Expr:
        left = self.parse_cmp()
        while self.cur().kind == TokKind.OP and self.cur().value == "&&":
            self.pos += 1
            left = ast.Expr("binop", value="&&", left=left, right=self.parse_cmp())
        return left

    def parse_cmp(self) -> ast.Expr:
        left = self.parse_shift()
        ops = {"==", "!=", "<", "<=", ">", ">="}
        while self.cur().kind == TokKind.OP and self.cur().value in ops:
            op = self.cur().value
            self.pos += 1
            left = ast.Expr("binop", value=op, left=left, right=self.parse_shift())
        return left

    def parse_shift(self) -> ast.Expr:
        left = self.parse_bitwise()
        while self.cur().kind == TokKind.OP and self.cur().value in ("<<", ">>"):
            op = self.cur().value
            self.pos += 1
            left = ast.Expr("binop", value=op, left=left, right=self.parse_bitwise())
        return left

    def parse_add(self) -> ast.Expr:
        left = self.parse_mul()
        while self.cur().kind == TokKind.OP and self.cur().value in ("+", "-"):
            op = self.cur().value
            self.pos += 1
            left = ast.Expr("binop", value=op, left=left, right=self.parse_mul())
        return left

    def parse_bitwise(self) -> ast.Expr:
        left = self.parse_add()
        while self.cur().kind == TokKind.OP and self.cur().value in ("&", "|", "^"):
            op = self.cur().value
            self.pos += 1
            left = ast.Expr("binop", value=op, left=left, right=self.parse_add())
        return left

    def parse_mul(self) -> ast.Expr:
        left = self.parse_unary()
        while self.cur().kind == TokKind.OP and self.cur().value in ("*", "/", "%"):
            op = self.cur().value
            self.pos += 1
            left = ast.Expr("binop", value=op, left=left, right=self.parse_unary())
        return left

    def parse_unary(self) -> ast.Expr:
        if self.cur().kind == TokKind.OP and self.cur().value == "!":
            self.pos += 1
            return ast.Expr("unop", value="!", left=self.parse_unary())
        return self.parse_primary()

    def parse_primary(self) -> ast.Expr:
        if self.at(TokKind.INT):
            v = int(self.eat(TokKind.INT).value)
            return ast.Expr("int", value=v)
        if self.cur().value == "true":
            self.pos += 1
            return ast.Expr("bool", value=True)
        if self.cur().value == "false":
            self.pos += 1
            return ast.Expr("bool", value=False)
        if self.at(TokKind.STRING):
            v = self.eat(TokKind.STRING).value
            return ast.Expr("string", value=v)
        if self.at(TokKind.LPAREN):
            self.eat(TokKind.LPAREN)
            e = self.parse_expr()
            self.eat(TokKind.RPAREN)
            return e
        name = self.eat_ident_value()
        if self.at(TokKind.LPAREN):
            args = self.parse_call_args()
            return ast.Expr("call", name=name, args=args)
        path: list[str] = [name]
        while self.try_eat(TokKind.DOT):
            path.append(self.eat_ident_value())
        if len(path) == 1:
            return ast.Expr("ident", name=path[0])
        return ast.Expr("path", value=".".join(path))

    def eat_op(self, text: str) -> None:
        tok = self.cur()
        if tok.kind != TokKind.OP or tok.value != text:
            raise ParseError(f"expected {text!r}", offset=tok.pos)
        self.pos += 1

    def eat_ident(self, text: str) -> None:
        tok = self.cur()
        if tok.kind != TokKind.IDENT or tok.value != text:
            raise ParseError(f"expected {text!r}", offset=tok.pos)
        self.pos += 1

    def eat_ident_value(self) -> str:
        tok = self.eat(TokKind.IDENT)
        return str(tok.value)


def parse(source: str) -> ast.Schema:
    return Parser(tokenize(source)).parse()
