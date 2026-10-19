//! Recursive-descent parser, a faithful port of `bps/parser.py`.
//!
//! Every error message is byte-identical to the Python original, including the
//! `TokKind.X` spellings that f-string interpolation of the Python enum
//! produces.

use crate::ast::*;
use crate::errors::{parse_at, py_repr_str, BpsResult};
use crate::lexer::{TokKind, TokValue, Token, tokenize};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn cur(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek(&self, k: usize) -> &Token {
        let i = (self.pos + k).min(self.tokens.len() - 1);
        &self.tokens[i]
    }

    fn at(&self, kind: TokKind) -> bool {
        self.cur().kind == kind
    }

    /// The current token's text, or `None` for EOF/int tokens. Python compares
    /// `cur().value` against keywords; int values never equal a keyword string.
    fn text(&self) -> Option<&str> {
        match &self.cur().value {
            TokValue::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn eat(&mut self, kind: TokKind, msg: Option<&str>) -> BpsResult<Token> {
        let tok = self.cur().clone();
        if tok.kind != kind {
            let m = match msg {
                Some(m) => m.to_string(),
                None => format!("expected {kind}, got {}", tok.kind),
            };
            return Err(parse_at(m, tok.pos));
        }
        self.pos += 1;
        Ok(tok)
    }

    fn try_eat(&mut self, kind: TokKind) -> Option<Token> {
        if self.at(kind) {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn eat_op(&mut self, text: &str) -> BpsResult<()> {
        let tok = self.cur();
        if tok.kind != TokKind::Op || tok.text() != Some(text) {
            return Err(parse_at(format!("expected {}", py_repr_str(text)), tok.pos));
        }
        self.pos += 1;
        Ok(())
    }

    fn eat_ident(&mut self, text: &str) -> BpsResult<()> {
        let tok = self.cur();
        if tok.kind != TokKind::Ident || tok.text() != Some(text) {
            return Err(parse_at(format!("expected {}", py_repr_str(text)), tok.pos));
        }
        self.pos += 1;
        Ok(())
    }

    fn eat_ident_value(&mut self) -> BpsResult<String> {
        let tok = self.eat(TokKind::Ident, None)?;
        match &tok.value {
            TokValue::Text(s) => Ok(s.clone()),
            _ => Err(parse_at("expected ident value", tok.pos)),
        }
    }

    pub fn parse(&mut self) -> BpsResult<Schema> {
        let mut schema = Schema::default();
        while !self.at(TokKind::Eof) {
            let name = self.text().map(|s| s.to_string());
            match name.as_deref() {
                Some("import") => {
                    self.pos += 1;
                    let tok = self.eat(TokKind::Str, Some("expected import path"))?;
                    match tok.value {
                        TokValue::Text(s) => schema.imports.push(s),
                        _ => unreachable!(),
                    }
                }
                Some("encoding") => schema.encodings.push(self.parse_encoding()?),
                Some("enum") => schema.enums.push(self.parse_enum()?),
                Some("packet") => schema.packets.push(self.parse_packet()?),
                Some("protocol") => schema.protocols.push(self.parse_protocol()?),
                _ => {
                    let tok = self.cur();
                    return Err(parse_at(
                        format!("expected top-level declaration, got {}", tok.py_repr_value()),
                        tok.pos,
                    ));
                }
            }
        }
        Ok(schema)
    }

    fn parse_encoding(&mut self) -> BpsResult<EncodingDecl> {
        self.eat_ident("encoding")?;
        let name = self.eat_ident_value()?;
        let params = self.parse_params()?;
        if self.at(TokKind::Eq) {
            self.pos += 1;
        }
        if self.at(TokKind::LBrace) {
            let body = self.parse_block_type()?;
            let mut yield_expr = None;
            if self.try_eat(TokKind::Eq).is_some() {
                yield_expr = Some(self.parse_expr()?);
            }
            return Ok(EncodingDecl { name, params, body: EncBody::Block(body), yield_expr });
        }
        Ok(EncodingDecl { name, params, body: EncBody::Ref(self.parse_type_ref()?), yield_expr: None })
    }

    fn parse_params(&mut self) -> BpsResult<Vec<Param>> {
        if !self.at(TokKind::LParen) {
            return Ok(Vec::new());
        }
        self.eat(TokKind::LParen, None)?;
        let mut params = Vec::new();
        if !self.at(TokKind::RParen) {
            loop {
                let pname = self.eat_ident_value()?;
                let mut default = None;
                if self.try_eat(TokKind::Eq).is_some() {
                    let tok = self.eat(TokKind::Int, None)?;
                    default = Some(tok.as_int()?);
                }
                params.push(Param { name: pname, default });
                if self.try_eat(TokKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.eat(TokKind::RParen, None)?;
        Ok(params)
    }

    fn parse_enum(&mut self) -> BpsResult<EnumDecl> {
        self.eat_ident("enum")?;
        let name = self.eat_ident_value()?;
        self.eat(TokKind::Colon, None)?;
        let base = self.eat_ident_value()?;
        self.eat(TokKind::LBrace, None)?;
        let mut members = Vec::new();
        while !self.at(TokKind::RBrace) {
            let mname = self.eat_ident_value()?;
            self.eat(TokKind::Eq, None)?;
            let tok = self.eat(TokKind::Int, None)?;
            members.push(EnumMember { name: mname, value: tok.as_int()? });
            self.try_eat(TokKind::Comma);
        }
        self.eat(TokKind::RBrace, None)?;
        Ok(EnumDecl { name, base, members })
    }

    fn parse_packet(&mut self) -> BpsResult<PacketDecl> {
        self.eat_ident("packet")?;
        let name = self.eat_ident_value()?;
        let items = self.parse_block_items()?;
        Ok(PacketDecl { name, items })
    }

    fn parse_protocol(&mut self) -> BpsResult<ProtocolDecl> {
        self.eat_ident("protocol")?;
        let name = self.eat_ident_value()?;
        self.eat(TokKind::At, None)?;
        let version = self.eat_ident_value()?;
        self.eat(TokKind::LBrace, None)?;
        let mut protocol_id: i128 = 0;
        let mut frames = Vec::new();
        let mut routes = Vec::new();
        while !self.at(TokKind::RBrace) {
            let kw = self.text().map(|s| s.to_string());
            match kw.as_deref() {
                Some("id") => {
                    self.pos += 1;
                    self.eat(TokKind::Eq, None)?;
                    let tok = self.eat(TokKind::Int, None)?;
                    protocol_id = tok.as_int()?;
                }
                Some("frame") => {
                    self.pos += 1;
                    let fname = self.eat_ident_value()?;
                    let items = self.parse_block_items()?;
                    frames.push(FrameDecl { name: fname, items });
                }
                Some("route") => {
                    self.pos += 1;
                    let state = self.eat_ident_value()?;
                    self.eat(TokKind::Dot, None)?;
                    let direction = self.eat_ident_value()?;
                    let tok = self.eat(TokKind::Int, None)?;
                    let pid = tok.as_int()?;
                    self.eat(TokKind::Op, None)?; // =>
                    let pname = self.eat_ident_value()?;
                    let mut frame = None;
                    if self.text() == Some("frame") {
                        self.pos += 1;
                        frame = Some(self.eat_ident_value()?);
                    }
                    routes.push(RouteDecl {
                        state,
                        direction,
                        packet_id: pid,
                        packet_name: pname,
                        frame,
                    });
                }
                _ => {
                    let tok = self.cur();
                    return Err(parse_at(
                        format!("unexpected in protocol: {}", tok.py_repr_value()),
                        tok.pos,
                    ));
                }
            }
        }
        self.eat(TokKind::RBrace, None)?;
        Ok(ProtocolDecl { name, version, protocol_id, frames, routes })
    }

    fn parse_block_items(&mut self) -> BpsResult<Vec<BlockItem>> {
        self.eat(TokKind::LBrace, None)?;
        let mut items = Vec::new();
        while !self.at(TokKind::RBrace) {
            if self.at(TokKind::Ellipsis) {
                self.pos += 1;
                let sname = self.eat_ident_value()?;
                let mut args = Vec::new();
                if self.at(TokKind::LParen) {
                    args = self.parse_call_args()?;
                }
                items.push(BlockItem::Spread { name: sname, args });
                continue;
            }
            if self.text() == Some("require") {
                self.pos += 1;
                items.push(BlockItem::Require(self.parse_expr()?));
                continue;
            }
            if self.text() == Some("if") {
                self.pos += 1;
                let cond = self.parse_expr()?;
                let group_items = self.parse_block_type()?.items;
                items.push(BlockItem::Group { cond: Some(cond), items: group_items });
                continue;
            }
            // `match:` / `match =` are a field and a computed value. A statement
            // is `match (action) { ... }` or `switch action { ... }`.
            if matches!(self.text(), Some("match") | Some("switch"))
                && !matches!(self.peek(1).kind, TokKind::Colon | TokKind::Eq)
            {
                items.push(BlockItem::MatchStmt(self.parse_match_type(true)?));
                continue;
            }
            let name = self.eat_ident_value()?;
            if self.try_eat(TokKind::Eq).is_some() {
                items.push(BlockItem::Computed { name, expr: self.parse_expr()? });
                continue;
            }
            self.eat(TokKind::Colon, None)?;
            let type_ref = self.parse_field_type()?;
            let mut if_expr = None;
            if self.text() == Some("if") {
                // `if cond {` after a *complete field* opens a conditional group
                // for the fields that follow it, so this field is emitted first.
                let mark = self.pos;
                self.pos += 1;
                let cond = self.parse_expr()?;
                if self.at(TokKind::LBrace) {
                    self.pos = mark;
                    items.push(BlockItem::Field(Box::new(WireField {
                        name,
                        type_ref,
                        if_expr: None,
                        repeat: None,
                        encode_expr: None,
                    })));
                    items.push(self.parse_conditional_group_item()?);
                    continue;
                }
                if_expr = Some(cond);
            }
            let mut encode_expr = None;
            if self.try_eat(TokKind::Eq).is_some() {
                encode_expr = Some(self.parse_expr()?);
            }
            let mut repeat = None;
            if self.at(TokKind::LBracket) {
                repeat = Some(self.parse_repeat()?);
            }
            items.push(BlockItem::Field(Box::new(WireField {
                name,
                type_ref,
                if_expr,
                repeat,
                encode_expr,
            })));
            self.try_eat(TokKind::Comma);
        }
        self.eat(TokKind::RBrace, None)?;
        Ok(items)
    }

    fn parse_repeat(&mut self) -> BpsResult<Repeat> {
        self.eat(TokKind::LBracket, None)?;
        self.eat(TokKind::RBracket, None)?;
        if self.text() == Some("until_end") {
            self.pos += 1;
            return Ok(Repeat { mode: RepeatMode::UntilEnd, expr: None });
        }
        if self.text() == Some("until") {
            self.pos += 1;
            return Ok(Repeat { mode: RepeatMode::Until, expr: Some(self.parse_expr()?) });
        }
        if self.text() == Some("count") {
            self.pos += 1;
            self.eat(TokKind::Eq, None)?;
            return Ok(Repeat { mode: RepeatMode::Count, expr: Some(self.parse_expr()?) });
        }
        Err(parse_at("expected until, until_end, or count", self.cur().pos))
    }

    fn parse_field_type(&mut self) -> BpsResult<FieldType> {
        if matches!(self.text(), Some("match") | Some("switch")) {
            return Ok(FieldType::Match(self.parse_match_type(false)?));
        }
        if self.at(TokKind::LBrace) {
            return Ok(FieldType::Block(self.parse_block_type()?));
        }
        Ok(FieldType::Ref(self.parse_type_ref()?))
    }

    fn parse_match_type(&mut self, statement: bool) -> BpsResult<MatchType> {
        let kw = self.text().map(|s| s.to_string());
        if !matches!(kw.as_deref(), Some("match") | Some("switch")) {
            let tok = self.cur();
            return Err(parse_at(
                format!("expected match or switch, got {}", tok.py_repr_value()),
                tok.pos,
            ));
        }
        self.pos += 1;
        let tag = self.parse_expr()?;
        self.eat(TokKind::LBrace, None)?;
        let mut arms = Vec::new();
        while !self.at(TokKind::RBrace) {
            let (tag_val, tags) = self.parse_arm_tags()?;
            self.eat_op("=>")?;
            let body = if statement {
                if !self.at(TokKind::LBrace) {
                    return Err(parse_at("match arm must be a block", self.cur().pos));
                }
                FieldType::Block(self.parse_block_type()?)
            } else if self.at(TokKind::LBrace) {
                FieldType::Block(self.parse_block_type()?)
            } else {
                FieldType::Ref(self.parse_type_ref()?)
            };
            arms.push(MatchArm { tag: tag_val, type_ref: Some(body), tags });
            self.try_eat(TokKind::Comma);
        }
        self.eat(TokKind::RBrace, None)?;
        Ok(MatchType { tag_expr: tag, arms })
    }

    fn parse_arm_tags(&mut self) -> BpsResult<(Option<Tag>, Vec<Tag>)> {
        if self.text() == Some("_") {
            self.pos += 1;
            if self.cur().kind == TokKind::Op && self.text() == Some("|") {
                return Err(parse_at(
                    "'_' cannot be combined with other tags",
                    self.cur().pos,
                ));
            }
            return Ok((None, Vec::new()));
        }
        let mut tags = vec![self.parse_one_arm_tag()?];
        while self.cur().kind == TokKind::Op && self.text() == Some("|") {
            self.pos += 1;
            tags.push(self.parse_one_arm_tag()?);
        }
        if tags.len() == 1 {
            let first = tags.pop().unwrap();
            return Ok((Some(first), Vec::new()));
        }
        let first = tags[0].clone();
        Ok((Some(first), tags))
    }

    fn parse_one_arm_tag(&mut self) -> BpsResult<Tag> {
        if self.text() == Some("_") {
            return Err(parse_at("'_' cannot be combined with other tags", self.cur().pos));
        }
        if self.at(TokKind::Int) {
            let tok = self.eat(TokKind::Int, None)?;
            return Ok(Tag::Int(tok.as_int()?));
        }
        Ok(Tag::Str(self.eat_ident_value()?))
    }

    fn parse_block_type(&mut self) -> BpsResult<BlockType> {
        Ok(BlockType { items: self.parse_block_items()? })
    }

    fn parse_conditional_group_item(&mut self) -> BpsResult<BlockItem> {
        let (cond, items) = self.parse_conditional_group()?;
        Ok(BlockItem::Group { cond: Some(cond), items })
    }

    fn parse_conditional_group(&mut self) -> BpsResult<(Expr, Vec<BlockItem>)> {
        self.pos += 1; // 'if'
        let cond = self.parse_expr()?;
        let items = self.parse_block_type()?.items;
        Ok((cond, items))
    }

    fn parse_type_ref(&mut self) -> BpsResult<TypeRef> {
        let name = self.eat_ident_value()?;
        let mut args = Vec::new();
        if self.at(TokKind::LParen) {
            args = self.parse_call_args()?;
        }
        let mut transforms = Vec::new();
        while self.text() == Some("as") {
            self.pos += 1;
            transforms.push(self.parse_transform()?);
        }
        Ok(TypeRef { name, args, transforms })
    }

    fn parse_transform(&mut self) -> BpsResult<String> {
        if matches!(self.text(), Some("utf8") | Some("mutf8")) {
            let t = self.text().unwrap().to_string();
            self.pos += 1;
            return Ok(t);
        }
        if self.text() == Some("fixed") {
            self.pos += 1;
            self.eat(TokKind::LParen, None)?;
            let num = self.parse_expr()?;
            self.eat(TokKind::Op, None)?; // /
            let den = self.parse_expr()?;
            self.eat(TokKind::RParen, None)?;
            return Ok(format!("fixed({}/{})", Self::expr_src(&num), Self::expr_src(&den)));
        }
        Err(parse_at("expected transform", self.cur().pos))
    }

    fn expr_src(e: &Expr) -> String {
        match e {
            Expr::Int(v) => v.to_string(),
            _ => "?".to_string(),
        }
    }

    fn parse_call_args(&mut self) -> BpsResult<Vec<Expr>> {
        self.eat(TokKind::LParen, None)?;
        let mut args = Vec::new();
        if !self.at(TokKind::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if self.try_eat(TokKind::Comma).is_none() {
                    break;
                }
            }
        }
        self.eat(TokKind::RParen, None)?;
        Ok(args)
    }

    fn parse_expr(&mut self) -> BpsResult<Expr> {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> BpsResult<Expr> {
        let cond = self.parse_or()?;
        if self.cur().kind == TokKind::Op && self.text() == Some("?") {
            self.pos += 1;
            let yes = self.parse_expr()?;
            self.eat(TokKind::Colon, None)?;
            let no = self.parse_ternary()?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(yes), Box::new(no)));
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> BpsResult<Expr> {
        let mut left = self.parse_and()?;
        while self.cur().kind == TokKind::Op && self.text() == Some("||") {
            self.pos += 1;
            let right = self.parse_and()?;
            left = Expr::Binop("||".into(), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> BpsResult<Expr> {
        let mut left = self.parse_cmp()?;
        while self.cur().kind == TokKind::Op && self.text() == Some("&&") {
            self.pos += 1;
            let right = self.parse_cmp()?;
            left = Expr::Binop("&&".into(), Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> BpsResult<Expr> {
        let mut left = self.parse_shift()?;
        loop {
            let op = match self.cur().kind {
                TokKind::Op => self.text().map(|s| s.to_string()),
                _ => None,
            };
            let Some(op) = op else { break };
            if !matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") {
                break;
            }
            self.pos += 1;
            let right = self.parse_shift()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> BpsResult<Expr> {
        let mut left = self.parse_bitwise()?;
        loop {
            let op = match self.cur().kind {
                TokKind::Op => self.text().map(|s| s.to_string()),
                _ => None,
            };
            let Some(op) = op else { break };
            if !matches!(op.as_str(), "<<" | ">>") {
                break;
            }
            self.pos += 1;
            let right = self.parse_bitwise()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> BpsResult<Expr> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.cur().kind {
                TokKind::Op => self.text().map(|s| s.to_string()),
                _ => None,
            };
            let Some(op) = op else { break };
            if !matches!(op.as_str(), "+" | "-") {
                break;
            }
            self.pos += 1;
            let right = self.parse_mul()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_bitwise(&mut self) -> BpsResult<Expr> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.cur().kind {
                TokKind::Op => self.text().map(|s| s.to_string()),
                _ => None,
            };
            let Some(op) = op else { break };
            if !matches!(op.as_str(), "&" | "|" | "^") {
                break;
            }
            self.pos += 1;
            let right = self.parse_add()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> BpsResult<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.cur().kind {
                TokKind::Op => self.text().map(|s| s.to_string()),
                _ => None,
            };
            let Some(op) = op else { break };
            if !matches!(op.as_str(), "*" | "/" | "%") {
                break;
            }
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Expr::Binop(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> BpsResult<Expr> {
        if self.cur().kind == TokKind::Op && self.text() == Some("!") {
            self.pos += 1;
            return Ok(Expr::Unop("!".into(), Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> BpsResult<Expr> {
        if self.at(TokKind::Int) {
            let tok = self.eat(TokKind::Int, None)?;
            return Ok(Expr::Int(tok.as_int()?));
        }
        if self.text() == Some("true") {
            self.pos += 1;
            return Ok(Expr::Bool(true));
        }
        if self.text() == Some("false") {
            self.pos += 1;
            return Ok(Expr::Bool(false));
        }
        if self.at(TokKind::Str) {
            let tok = self.eat(TokKind::Str, None)?;
            return match tok.value {
                TokValue::Text(s) => Ok(Expr::Str(s)),
                _ => unreachable!(),
            };
        }
        if self.at(TokKind::LParen) {
            self.eat(TokKind::LParen, None)?;
            let e = self.parse_expr()?;
            self.eat(TokKind::RParen, None)?;
            return Ok(e);
        }
        let name = self.eat_ident_value()?;
        if self.at(TokKind::LParen) {
            let args = self.parse_call_args()?;
            return Ok(Expr::Call(name, args));
        }
        let mut path = vec![name];
        while self.try_eat(TokKind::Dot).is_some() {
            path.push(self.eat_ident_value()?);
        }
        if path.len() == 1 {
            return Ok(Expr::Ident(path.pop().unwrap()));
        }
        Ok(Expr::Path(path.join(".")))
    }
}

pub fn parse(source: &str) -> BpsResult<Schema> {
    let tokens = tokenize(source)?;
    Parser::new(tokens).parse()
}
