//! `emit python`: a stdlib module whose encode and decode functions are the IR.

use std::fs;
use std::path::Path;

use crate::ast::Expr;
use crate::errors::{emit, BpsResult};
use crate::ir::{lower, IrSchema, Stmt, Storage, TypeDef, WireOp};
use crate::ResolvedSchema;

const RUNTIME: &str = r#"class Cursor:
    def __init__(self, data):
        self.data = data
        self.pos = 0

    def u8(self):
        if self.pos >= len(self.data):
            raise EOFError("need 1 bytes, have 0")
        b = self.data[self.pos]
        self.pos += 1
        return b

    def bytes(self, n):
        if n < 0 or self.pos + n > len(self.data):
            raise EOFError("need %d bytes" % n)
        b = self.data[self.pos:self.pos + n]
        self.pos += n
        return b

    def rest(self):
        b = self.data[self.pos:]
        self.pos = len(self.data)
        return b

    def f32(self):
        import struct
        return struct.unpack(">f", self.bytes(4))[0]

    def f64(self):
        import struct
        return struct.unpack(">d", self.bytes(8))[0]


class Writer:
    def __init__(self):
        self.buf = bytearray()

    def u8(self, v):
        self.buf.append(v & 0xFF)

    def bytes(self, b):
        if isinstance(b, str):
            b = b.encode("utf-8")
        self.buf.extend(b)

    def f32(self, v):
        import struct
        self.buf.extend(struct.pack(">f", v))

    def f64(self, v):
        import struct
        self.buf.extend(struct.pack(">d", v))


def sign_extend(v, bits):
    if bits <= 0:
        return v
    mask = 1 << (bits - 1)
    if v & mask:
        return v - (1 << bits)
    return v


def utf16_units(s):
    return sum(2 if ord(c) > 0xFFFF else 1 for c in s)


def both(a, b, op):
    return op(a, b)
"#;

pub fn emit_python(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    let ir = lower(resolved)?;
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    let mut src = String::from(RUNTIME);
    src.push_str(&gen_body(&ir)?);
    fs::write(out_dir.join("bpsgen.py"), src).map_err(|e| emit(e.to_string()))?;
    Ok(())
}

fn gen_body(ir: &IrSchema) -> BpsResult<String> {
    let mut out = String::new();
    for (name, proc) in ir.decode.iter() {
        let params = proc.params.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ");
        let extra = if params.is_empty() { String::new() } else { format!(", {params}") };
        out.push_str(&format!("def decode_{}(cur{extra}):\n", py_name(name)));
        out.push_str("    env = {}\n");
        out.push_str("    _stack = []\n");
        if matches!(ir.types.get(name), Some(TypeDef::Scalar(_))) {
            for dest in prebind_dests(&proc.body) {
                out.push_str(&format!("    env[{dest:?}] = 0\n"));
            }
        }
        for p in &proc.params {
            out.push_str(&format!("    env[{p:?}] = {p}\n"));
        }
        let mut seq = 0u32;
        let body = stmts(&proc.body, "    ", false, &mut seq)?;
        if body.trim().is_empty() {
            out.push_str("    pass\n");
        } else {
            out.push_str(&body);
        }
        match &proc.result {
            Some(expr) => out.push_str(&format!("    return {}\n\n", py_expr(expr))),
            None => out.push_str("    return env\n\n"),
        }
    }
    for (name, proc) in ir.encode.iter() {
        let params = proc.params.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ");
        let extra = if params.is_empty() { String::new() } else { format!(", {params}") };
        out.push_str(&format!("def encode_{}(w, value{extra}):\n", py_name(name)));
        out.push_str("    env = {\"value\": value}\n");
        out.push_str("    _stack = []\n");
        if matches!(ir.types.get(name), Some(TypeDef::Scalar(_))) {
            for dest in prebind_dests(&proc.body) {
                out.push_str(&format!("    if {dest:?} not in env:\n        env[{dest:?}] = 0\n"));
            }
        }
        if let Some(TypeDef::Record { fields }) = ir.types.get(name) {
            for field in fields {
                out.push_str(&format!(
                    "    env[{name:?}] = value.get({name:?}) if isinstance(value, dict) else None\n",
                    name = field.name
                ));
            }
        }
        for p in &proc.params {
            out.push_str(&format!("    env[{p:?}] = {p}\n"));
        }
        let mut seq = 0u32;
        let body = stmts(&proc.body, "    ", true, &mut seq)?;
        if body.trim().is_empty() {
            out.push_str("    pass\n");
        } else {
            out.push_str(&body);
        }
        out.push('\n');
    }
    let _ = Storage::I32;
    Ok(out)
}

fn stmts(body: &[Stmt], indent: &str, is_encode: bool, seq: &mut u32) -> BpsResult<String> {
    let mut out = String::new();
    for stmt in body {
        out.push_str(&stmt_py(stmt, indent, is_encode, seq)?);
    }
    Ok(out)
}

fn stmt_py(stmt: &Stmt, indent: &str, is_encode: bool, seq: &mut u32) -> BpsResult<String> {
    match stmt {
        Stmt::Let { name, expr } => Ok(format!("{indent}env[{name:?}] = {}\n", py_expr(expr))),
        Stmt::Require(expr) => Ok(format!(
            "{indent}if not ({e}):\n{indent}    raise ValueError(\"require failed\")\n",
            e = py_expr(expr)
        )),
        Stmt::If { cond, then_body, else_body, .. } => {
            let mut s = format!("{indent}if {}:\n", py_expr(cond));
            s.push_str(&nested(then_body, indent, is_encode, seq)?);
            if !else_body.is_empty() {
                s.push_str(&format!("{indent}else:\n"));
                s.push_str(&nested(else_body, indent, is_encode, seq)?);
            }
            Ok(s)
        }
        Stmt::Read { dst, op } => Ok(format!("{indent}env[{dst:?}] = {}\n", read_call(op))),
        Stmt::Write { op, value } => Ok(format!("{indent}{}\n", write_call(op, value))),
        Stmt::CallDecode { dst, callee, args } => {
            let list = args.iter().map(py_expr).collect::<Vec<_>>().join(", ");
            let extra = if list.is_empty() { String::new() } else { format!(", {list}") };
            Ok(format!(
                "{indent}env[{dst:?}] = decode_{}(cur{extra})\n",
                py_name(&callee.0)
            ))
        }
        Stmt::CallEncode { callee, args, value } => {
            let list = args.iter().map(py_expr).collect::<Vec<_>>().join(", ");
            let extra = if list.is_empty() { String::new() } else { format!(", {list}") };
            Ok(format!(
                "{indent}encode_{}(w, {}{extra})\n",
                py_name(&callee.0),
                py_expr(value)
            ))
        }
        Stmt::Utf8 { dst, src } => Ok(format!(
            "{indent}env[{dst:?}] = env[{src:?}].decode(\"utf-8\")\n"
        )),
        Stmt::RepeatCount { n, elem, dst } => repeat(elem, dst, is_encode, seq, indent, Some(py_expr(n)), None),
        Stmt::RepeatUntil { term, elem, dst } => {
            repeat(elem, dst, is_encode, seq, indent, None, Some(py_expr(term)))
        }
        Stmt::RepeatUntilEnd { elem, dst } => {
            if is_encode {
                return Ok(format!("{indent}raise ValueError(\"until-end has no encode\")\n"));
            }
            repeat(elem, dst, false, seq, indent, None, None)
        }
        Stmt::Match { scrutinee, arms, default } => {
            let mut s = format!("{indent}_tag = {}\n", py_expr(scrutinee));
            let mut first = true;
            for arm in arms {
                let cond = arm.tags.iter().map(|t| format!("_tag == {t}")).collect::<Vec<_>>().join(" or ");
                let kw = if first { "if" } else { "elif" };
                first = false;
                s.push_str(&format!("{indent}{kw} {cond}:\n"));
                s.push_str(&nested(&arm.body, indent, is_encode, seq)?);
            }
            if let Some(body) = default {
                let kw = if first { "if True" } else { "else" };
                s.push_str(&format!("{indent}{kw}:\n"));
                s.push_str(&nested(body, indent, is_encode, seq)?);
            } else if first {
                s.push_str(&format!("{indent}pass\n"));
            }
            Ok(s)
        }
        Stmt::Block { body, dst } => {
            let mut s = format!(
                "{indent}_stack.append(env)\n{indent}_child = dict(env)\n{indent}_overlay = env.get({dst:?})\n{indent}if isinstance(_overlay, dict):\n{indent}    _child.update(_overlay)\n{indent}env = _child\n"
            );
            let inner = stmts(body, indent, is_encode, seq)?;
            if inner.trim().is_empty() {
                s.push_str(&format!("{indent}pass\n"));
            } else {
                s.push_str(&inner);
            }
            s.push_str(&format!(
                "{indent}_block = env\n{indent}env = _stack.pop()\n{indent}env[{dst:?}] = _block\n"
            ));
            Ok(s)
        }
    }
}

fn repeat(
    elem: &[Stmt],
    dst: &str,
    is_encode: bool,
    seq: &mut u32,
    indent: &str,
    count: Option<String>,
    until: Option<String>,
) -> BpsResult<String> {
    let id = *seq;
    *seq += 1;
    let mut s = String::new();
    if is_encode {
        s.push_str(&format!(
            "{indent}for _item_{id} in env[{dst:?}] or []:\n{indent}    env[\"__elem\"] = _item_{id}\n"
        ));
        let inner = stmts(elem, &format!("{indent}    "), true, seq)?;
        if inner.trim().is_empty() {
            s.push_str(&format!("{indent}    pass\n"));
        } else {
            s.push_str(&inner);
        }
        if let Some(term) = until {
            s.push_str(&format!("{indent}w.u8({term})\n"));
        }
        let _ = count;
        return Ok(s);
    }
    s.push_str(&format!("{indent}_items_{id} = []\n"));
    if let Some(n) = count {
        s.push_str(&format!("{indent}for _ in range({n}):\n"));
    } else if let Some(term) = until {
        s.push_str(&format!(
            "{indent}while cur.pos < len(cur.data):\n{indent}    if cur.data[cur.pos] == ({term}) & 0xFF:\n{indent}        cur.pos += 1\n{indent}        break\n"
        ));
    } else {
        s.push_str(&format!("{indent}while cur.pos < len(cur.data):\n"));
    }
    let pad = format!("{indent}    ");
    let inner = stmts(elem, &pad, false, seq)?;
    if inner.trim().is_empty() {
        s.push_str(&format!("{pad}pass\n"));
    } else {
        s.push_str(&inner);
    }
    s.push_str(&format!(
        "{pad}_items_{id}.append(env[\"__elem\"])\n{indent}env[{dst:?}] = _items_{id}\n"
    ));
    Ok(s)
}

fn nested(body: &[Stmt], indent: &str, is_encode: bool, seq: &mut u32) -> BpsResult<String> {
    let inner = stmts(body, &format!("{indent}    "), is_encode, seq)?;
    if inner.is_empty() {
        Ok(format!("{indent}    pass\n"))
    } else {
        Ok(inner)
    }
}



fn read_call(op: &WireOp) -> String {
    match op {
        WireOp::U8 => "cur.u8()".into(),
        WireOp::F32be => "cur.f32()".into(),
        WireOp::F64be => "cur.f64()".into(),
        WireOp::Bytes { len } => format!("cur.bytes({})", py_expr(len)),
        WireOp::Rest => "cur.rest()".into(),
    }
}

fn write_call(op: &WireOp, value: &Expr) -> String {
    match op {
        WireOp::U8 => format!("w.u8({})", py_expr(value)),
        WireOp::F32be => format!("w.f32({})", py_expr(value)),
        WireOp::F64be => format!("w.f64({})", py_expr(value)),
        WireOp::Bytes { .. } | WireOp::Rest => format!("w.bytes({})", py_expr(value)),
    }
}

fn py_expr(expr: &Expr) -> String {
    match expr {
        Expr::Int(v) => v.to_string(),
        Expr::Bool(true) => "True".into(),
        Expr::Bool(false) => "False".into(),
        Expr::Str(s) => format!("{s:?}"),
        Expr::Ident(name) => format!("env[{name:?}]"),
        Expr::Unop(op, inner) if op == "!" => format!("(not {})", py_expr(inner)),
        Expr::Unop(op, inner) => format!("({op}{})", py_expr(inner)),
        Expr::Binop(op, left, right) if op == "&&" => {
            format!("both({}, {}, lambda a, b: bool(a) and bool(b))", py_expr(left), py_expr(right))
        }
        Expr::Binop(op, left, right) if op == "||" => {
            format!("both({}, {}, lambda a, b: bool(a) or bool(b))", py_expr(left), py_expr(right))
        }
        Expr::Binop(op, left, right) if op == "/" => format!("({} // {})", py_expr(left), py_expr(right)),
        Expr::Binop(op, left, right) => format!("({} {} {})", py_expr(left), op, py_expr(right)),
        Expr::Ternary(cond, yes, no) => format!("({} if {} else {})", py_expr(yes), py_expr(cond), py_expr(no)),
        Expr::Call(name, args) if name == "sign_extend" => {
            format!("sign_extend({}, {})", py_expr(&args[0]), py_expr(&args[1]))
        }
        Expr::Call(name, args) if name == "utf16_units" => format!("utf16_units({})", py_expr(&args[0])),
        Expr::Call(name, args) if name == "utf8_len" => format!("len({}.encode(\"utf-8\"))", py_expr(&args[0])),
        Expr::Call(name, args) if name == "len" => format!("len({})", py_expr(&args[0])),
        Expr::Call(name, args) if name == "min" => format!("min({}, {})", py_expr(&args[0]), py_expr(&args[1])),
        Expr::Call(name, args) if name == "max" => format!("max({}, {})", py_expr(&args[0]), py_expr(&args[1])),
        Expr::Call(name, _) => format!("_unknown({name:?})"),
        Expr::Path(p) => format!("env[{p:?}]"),
    }
}

fn py_name(name: &str) -> String {
    name.replace(':', "_")
}

fn prebind_dests(stmts: &[Stmt]) -> Vec<String> {
    let mut names = Vec::new();
    fn walk(stmts: &[Stmt], names: &mut Vec<String>) {
        for stmt in stmts {
            match stmt {
                Stmt::Read { dst, .. } | Stmt::CallDecode { dst, .. } | Stmt::Let { name: dst, .. } => {
                    if !names.iter().any(|n| n == dst) {
                        names.push(dst.clone());
                    }
                }
                Stmt::If { then_body, else_body, .. } => {
                    walk(then_body, names);
                    walk(else_body, names);
                }
                _ => {}
            }
        }
    }
    walk(stmts, &mut names);
    names
}
