//! `emit go`: a Go package whose encode and decode functions are the IR.

use std::fs;
use std::path::Path;

use crate::ast::Expr;
use crate::errors::{emit, BpsResult};
use crate::ir::lower;
use crate::ir::{IrSchema, Stmt, Storage, TypeDef, WireOp};
use crate::ResolvedSchema;

const RUNTIME: &str = r#"package bpsgen

import "fmt"
import "unicode/utf16"

type Cursor struct {
	Data []byte
	Pos  int
}

func (c *Cursor) u8() (int64, error) {
	if c.Pos >= len(c.Data) {
		return 0, fmt.Errorf("need 1 bytes, have 0")
	}
	b := c.Data[c.Pos]
	c.Pos++
	return int64(b), nil
}

func (c *Cursor) bytes(n int) ([]byte, error) {
	if n < 0 || c.Pos+n > len(c.Data) {
		return nil, fmt.Errorf("need %d bytes, have %d", n, len(c.Data)-c.Pos)
	}
	b := append([]byte(nil), c.Data[c.Pos:c.Pos+n]...)
	c.Pos += n
	return b, nil
}

func (c *Cursor) rest() []byte {
	b := append([]byte(nil), c.Data[c.Pos:]...)
	c.Pos = len(c.Data)
	return b
}

func (c *Cursor) f32() (float64, error) {
	b, err := c.bytes(4)
	if err != nil {
		return 0, err
	}
	u := uint32(b[0])<<24 | uint32(b[1])<<16 | uint32(b[2])<<8 | uint32(b[3])
	return float64(float32frombits(u)), nil
}

func (c *Cursor) f64() (float64, error) {
	b, err := c.bytes(8)
	if err != nil {
		return 0, err
	}
	var u uint64
	for _, x := range b {
		u = u<<8 | uint64(x)
	}
	return float64frombits(u), nil
}

type Writer struct{ Buf []byte }

func (w *Writer) u8(v int64) { w.Buf = append(w.Buf, byte(v&0xFF)) }

func (w *Writer) bytes(b []byte) { w.Buf = append(w.Buf, b...) }

func (w *Writer) f32(v float64) {
	u := float32bits(float32(v))
	w.Buf = append(w.Buf, byte(u>>24), byte(u>>16), byte(u>>8), byte(u))
}

func (w *Writer) f64(v float64) {
	u := float64bits(v)
	for i := 7; i >= 0; i-- {
		w.Buf = append(w.Buf, byte(u>>(uint(i)*8)))
	}
}

func SignExtend(v int64, bits int) int64 {
	if bits <= 0 || bits >= 64 {
		return v
	}
	mask := int64(1) << (bits - 1)
	if v&mask != 0 {
		return v - (int64(1) << bits)
	}
	return v
}

func Utf16Units(s string) int64 {
	n := 0
	for _, r := range s {
		if r > 0xFFFF {
			n += 2
		} else {
			n++
		}
	}
	_ = utf16.Encode // keep the import honest if a later edit uses it
	return int64(n)
}

func ival(v any) int64 {
	switch x := v.(type) {
	case int64:
		return x
	case int32:
		return int64(x)
	case int16:
		return int64(x)
	case int8:
		return int64(x)
	case int:
		return int64(x)
	case uint8:
		return int64(x)
	case uint16:
		return int64(x)
	case uint32:
		return int64(x)
	case uint64:
		return int64(x)
	case float64:
		return int64(x)
	case float32:
		return int64(x)
	case bool:
		if x {
			return 1
		}
		return 0
	default:
		return 0
	}
}

func bval(v any) bool {
	switch x := v.(type) {
	case bool:
		return x
	case int64:
		return x != 0
	default:
		return false
	}
}

func fval(v any) float64 {
	switch x := v.(type) {
	case float64:
		return x
	case float32:
		return float64(x)
	case int64:
		return float64(x)
	case int32:
		return float64(x)
	default:
		return 0
	}
}

func sval(v any) string {
	if s, ok := v.(string); ok {
		return s
	}
	return ""
}
"#;

pub fn emit_go(resolved: &ResolvedSchema, out_dir: &Path) -> BpsResult<()> {
    let ir = lower(resolved)?;
    fs::create_dir_all(out_dir).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("go.mod"), "module bpsgen\n\ngo 1.23\n").map_err(|e| emit(e.to_string()))?;

    fs::write(out_dir.join("runtime.go"), runtime_with_bits()).map_err(|e| emit(e.to_string()))?;
    fs::write(out_dir.join("packets.go"), gen_packets(&ir)?).map_err(|e| emit(e.to_string()))?;
    Ok(())
}

fn runtime_with_bits() -> String {
    let mut src = RUNTIME
        .replace(
            "import \"fmt\"\nimport \"unicode/utf16\"\n",
            "import \"fmt\"\nimport \"math\"\n",
        )
        .replace("float32frombits(u)", "math.Float32frombits(u)")
        .replace("float32bits(float32(v))", "math.Float32bits(float32(v))")
        .replace("float64frombits(u)", "math.Float64frombits(u)")
        .replace("float64bits(v)", "math.Float64bits(v)")
        .replace(
            "\t_ = utf16.Encode // keep the import honest if a later edit uses it\n",
            "",
        );
    src.push_str(
        "\nfunc b2i(b bool) int64 { if b { return 1 }; return 0 }\nfunc min64(a, b int64) int64 { if a < b { return a }; return b }\nfunc max64(a, b int64) int64 { if a > b { return a }; return b }\n",
    );
    src
}

fn gen_packets(ir: &IrSchema) -> BpsResult<String> {
    let mut out = String::from("package bpsgen\n\nimport \"fmt\"\nimport \"reflect\"\n\n");
    for (name, def) in ir.types.iter() {
        if let TypeDef::Record { fields } = def {
            if let Some(target) = record_alias(name, ir) {
                out.push_str(&format!("type {} = {}\n\n", struct_name(name), struct_name(target)));
                continue;
            }
            out.push_str(&format!("type {} struct {{\n", struct_name(name)));
            for field in fields {
                out.push_str(&format!("\t{} {} `bps:\"{}\"`\n", export_field(&field.name), go_ty(&field.ty, ir), field.name));
            }
            out.push_str("}\n\n");
        }
    }
    out.push_str(&go_helpers(ir));

    for (name, proc) in ir.decode.iter() {
        let ret = return_type(name, ir);
        let params = proc.params.iter().map(|p| format!("{p} int64")).collect::<Vec<_>>().join(", ");
        let args = if params.is_empty() { String::new() } else { format!(", {params}") };
        out.push_str(&format!("func Decode_{}(c *Cursor{}) ({ret}, error) {{\n", goname(name), args));
        out.push_str("\tenv := map[string]any{}\n\t_ = env\n");
        if matches!(ir.types.get(name), Some(TypeDef::Scalar(_))) {
            for dest in prebind_dests(&proc.body) {
                out.push_str(&format!("\tenv[\"{dest}\"] = int64(0)\n"));
            }
        }
        for p in &proc.params {
            out.push_str(&format!("\tenv[\"{p}\"] = {p}\n"));
        }
        out.push_str(&stmts(&proc.body, "\t", &ret, ir, false)?);
        match &proc.result {
            Some(expr) => {
                out.push_str(&format!("\treturn {}, nil\n}}\n\n", cast_result(&ret, expr)));
            }
            None => {
                out.push_str(&format!("\treturn {}, nil\n}}\n\n", record_lit(name, ir)));
            }
        }
    }
    for (name, proc) in ir.encode.iter() {
        let arg = encode_arg(name, ir);
        let params = proc.params.iter().map(|p| format!("{p} int64")).collect::<Vec<_>>().join(", ");
        let args = if params.is_empty() { String::new() } else { format!(", {params}") };
        out.push_str(&format!(
            "func Encode_{}(w *Writer, value {arg}{}) error {{\n",
            goname(name),
            args
        ));
        out.push_str("\tenv := map[string]any{}\n\t_ = env\n");
        out.push_str(&format!("\tenv[\"value\"] = {}\n", value_to_any(name, ir)));
        if matches!(ir.types.get(name), Some(TypeDef::Scalar(_))) {
            for dest in prebind_dests(&proc.body) {
                out.push_str(&format!(
                    "\tif _, ok := env[\"{dest}\"]; !ok {{ env[\"{dest}\"] = int64(0) }}\n"
                ));
            }
        }
        if let Some(TypeDef::Record { fields }) = ir.types.get(name) {
            for field in fields {
                let gof = export_field(&field.name);
                if matches!(field.ty, crate::ir::Ty::List(_)) {
                    out.push_str(&format!("\tenv[\"{}\"] = anyList(value.{gof})\n", field.name));
                } else {
                    out.push_str(&format!("\tenv[\"{}\"] = value.{gof}\n", field.name));
                }
            }
        }
        for p in &proc.params {
            out.push_str(&format!("\tenv[\"{p}\"] = {p}\n"));
        }
        out.push_str(&stmts(&proc.body, "\t", "", ir, true)?);
        out.push_str("\treturn nil\n}\n\n");
    }
    Ok(out)
}

fn stmts(body: &[Stmt], indent: &str, ret: &str, ir: &IrSchema, is_encode: bool) -> BpsResult<String> {
    let mut out = String::new();
    for stmt in body {
        out.push_str(&stmt_go(stmt, indent, ret, ir, is_encode)?);
    }
    Ok(out)
}

fn stmt_go(stmt: &Stmt, indent: &str, ret: &str, ir: &IrSchema, is_encode: bool) -> BpsResult<String> {
    let fail = if is_encode {
        "return err".to_string()
    } else {
        format!("return {}, err", ret_zero(ret))
    };
    match stmt {
        Stmt::Let { name, expr } => Ok(format!("{indent}env[\"{name}\"] = {}\n", go_any(expr))),
        Stmt::Require(expr) => Ok(format!(
            "{indent}if !({}) {{\n{indent}\treturn {z}\n{indent}}}\n",
            go_bool(expr),
            z = if is_encode { "fmt.Errorf(\"require failed\")".to_string() } else { format!("{}, fmt.Errorf(\"require failed\")", ret_zero(ret)) }
        )),
        Stmt::If { cond, then_body, else_body, .. } => {
            let mut s = format!("{indent}if {} {{\n", go_bool(cond));
            s.push_str(&stmts(then_body, &format!("{indent}\t"), ret, ir, is_encode)?);
            if !else_body.is_empty() {
                s.push_str(&format!("{indent}}} else {{\n"));
                s.push_str(&stmts(else_body, &format!("{indent}\t"), ret, ir, is_encode)?);
            }
            s.push_str(&format!("{indent}}}\n"));
            Ok(s)
        }
        Stmt::Read { dst, op } => Ok(format!(
            "{indent}if v, err := {call}; err != nil {{\n{indent}\t{fail}\n{indent}}} else {{\n{indent}\tenv[\"{dst}\"] = v\n{indent}}}\n",
            call = read_call(op)
        )),
        Stmt::Write { op, value } => Ok(format!("{indent}{call}\n", call = write_call(op, value))),
        Stmt::CallDecode { dst, callee, args } => {
            let list = args.iter().map(go_int).collect::<Vec<_>>().join(", ");
            let extra = if list.is_empty() { String::new() } else { format!(", {list}") };
            Ok(format!(
                "{indent}if v, err := Decode_{}(c{extra}); err != nil {{\n{indent}\t{fail}\n{indent}}} else {{\n{indent}\tenv[\"{dst}\"] = v\n{indent}}}\n",
                goname(&callee.0)
            ))
        }
        Stmt::CallEncode { callee, args, value } => {
            let list = args.iter().map(go_int).collect::<Vec<_>>().join(", ");
            let extra = if list.is_empty() { String::new() } else { format!(", {list}") };
            Ok(format!(
                "{indent}if err := Encode_{}(w, {}{extra}); err != nil {{\n{indent}\t{fail}\n{indent}}}\n",
                goname(&callee.0),
                encode_passed(&callee.0, value, ir)
            ))
        }
        Stmt::Utf8 { dst, src } => Ok(format!(
            "{indent}env[\"{dst}\"] = string(env[\"{src}\"].([]byte))\n"
        )),
        Stmt::RepeatCount { n, elem, dst } => {
            if is_encode {
                let mut s = format!("{indent}for _, item := range anyItems(env[\"{dst}\"]) {{\n{indent}\tenv[\"__elem\"] = item\n");
                s.push_str(&stmts(elem, &format!("{indent}\t\t"), ret, ir, is_encode)?);
                s.push_str(&format!("{indent}}}\n"));
                let _ = n;
                return Ok(s);
            }
            let mut s = format!("{indent}{{\n{indent}\tn := int({})\n{indent}\titems := make([]any, 0, n)\n{indent}\tfor i := 0; i < n; i++ {{\n", go_int(n));
            s.push_str(&stmts(elem, &format!("{indent}\t\t"), ret, ir, is_encode)?);
            s.push_str(&format!("{indent}\t\titems = append(items, env[\"__elem\"])\n{indent}\t}}\n{indent}\tenv[\"{dst}\"] = items\n{indent}}}\n"));
            Ok(s)
        }
        Stmt::RepeatUntil { term, elem, dst } => {
            if is_encode {
                let mut s = format!("{indent}for _, item := range anyItems(env[\"{dst}\"]) {{\n{indent}\tenv[\"__elem\"] = item\n");
                s.push_str(&stmts(elem, &format!("{indent}\t\t"), ret, ir, is_encode)?);
                s.push_str(&format!("{indent}}}\n{indent}w.u8({})\n", go_int(term)));
                return Ok(s);
            }
            let mut s = format!("{indent}{{\n{indent}\titems := []any{{}}\n{indent}\tterm := int64({})\n{indent}\tfor c.Pos < len(c.Data) {{\n{indent}\t\tif int64(c.Data[c.Pos]) == term {{\n{indent}\t\t\tc.Pos++\n{indent}\t\t\tbreak\n{indent}\t\t}}\n", go_int(term));
            s.push_str(&stmts(elem, &format!("{indent}\t\t"), ret, ir, is_encode)?);
            s.push_str(&format!("{indent}\t\titems = append(items, env[\"__elem\"])\n{indent}\t}}\n{indent}\tenv[\"{dst}\"] = items\n{indent}}}\n"));
            Ok(s)
        }
        Stmt::RepeatUntilEnd { elem, dst } => {
            if is_encode {
                return Ok(format!(
                    "{indent}return fmt.Errorf(\"encode not supported for repeat mode until_end\")\n"
                ));
            }
            let mut s = format!("{indent}{{\n{indent}\titems := []any{{}}\n{indent}\tfor c.Pos < len(c.Data) {{\n");
            s.push_str(&stmts(elem, &format!("{indent}\t\t"), ret, ir, is_encode)?);
            s.push_str(&format!("{indent}\t\titems = append(items, env[\"__elem\"])\n{indent}\t}}\n{indent}\tenv[\"{dst}\"] = items\n{indent}}}\n"));
            Ok(s)
        }
        Stmt::Match { scrutinee, arms, default } => {
            let mut seen = std::collections::BTreeSet::new();
            let mut s = format!("{indent}switch {} {{\n", go_int(scrutinee));
            for arm in arms {
                let tags: Vec<i128> = arm.tags.iter().copied().filter(|tag| seen.insert(*tag)).collect();
                if tags.is_empty() {
                    continue;
                }
                let cases = tags.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ");
                s.push_str(&format!("{indent}case {cases}:\n"));
                s.push_str(&stmts(&arm.body, &format!("{indent}\t"), ret, ir, is_encode)?);
            }
            if let Some(body) = default {
                s.push_str(&format!("{indent}default:\n"));
                s.push_str(&stmts(body, &format!("{indent}\t"), ret, ir, is_encode)?);
            }
            s.push_str(&format!("{indent}}}\n"));
            Ok(s)
        }
        Stmt::Block { body, dst } => {
            let mut s = format!("{indent}{{\n{indent}\tchild := map[string]any{{}}\n{indent}\tfor k, v := range env {{\n{indent}\t\tchild[k] = v\n{indent}\t}}\n{indent}\tfor k, v := range fieldsToEnv(env[\"{dst}\"]) {{\n{indent}\t\tchild[k] = v\n{indent}\t}}\n{indent}\tsaved := env\n{indent}\tenv = child\n");
            s.push_str(&stmts(body, &format!("{indent}\t"), ret, ir, is_encode)?);
            s.push_str(&format!("{indent}\tenv = saved\n{indent}\tenv[\"{dst}\"] = child\n{indent}}}\n"));
            Ok(s)
        }
    }
}

fn read_call(op: &WireOp) -> String {
    match op {
        WireOp::U8 => "c.u8()".into(),
        WireOp::F32be => "c.f32()".into(),
        WireOp::F64be => "c.f64()".into(),
        WireOp::Bytes { len } => format!("c.bytes(int({}))", go_int(len)),
        WireOp::Rest => "func() ([]byte, error) { return c.rest(), nil }()".into(),
    }
}

fn write_call(op: &WireOp, value: &Expr) -> String {
    match op {
        WireOp::U8 => format!("w.u8({})", go_int(value)),
        WireOp::F32be => format!("w.f32(fval({}))", go_any(value)),
        WireOp::F64be => format!("w.f64(fval({}))", go_any(value)),
        WireOp::Bytes { .. } | WireOp::Rest => "w.bytes(env[\"value\"].([]byte))".to_string(),
    }
}

fn go_int(expr: &Expr) -> String {
    match expr {
        Expr::Int(v) if *v > i64::MAX as i128 || *v < i64::MIN as i128 => {
            format!("{}", (*v as u64) as i64)
        }
        Expr::Int(v) => format!("{v}"),
        Expr::Bool(true) => "int64(1)".into(),
        Expr::Bool(false) => "int64(0)".into(),
        Expr::Ident(name) => format!("ival(env[\"{name}\"])"),
        Expr::Unop(op, inner) if op == "!" => format!("b2i(!{})", go_bool(inner)),
        Expr::Binop(op, left, right) if matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") => {
            format!("b2i({})", go_bool(expr))
        }
        Expr::Binop(op, left, right) => format!("({} {} {})", go_int(left), go_op(op), go_int(right)),
        Expr::Ternary(cond, yes, no) => format!("(func() int64 {{ if {} {{ return {} }}; return {} }}())", go_bool(cond), go_int(yes), go_int(no)),
        Expr::Call(name, args) if name == "sign_extend" => {
            format!("SignExtend({}, int({}))", go_int(&args[0]), go_int(&args[1]))
        }
        Expr::Call(name, args) if name == "utf16_units" => format!("Utf16Units({})", go_str(&args[0])),
        Expr::Call(name, args) if name == "utf8_len" => format!("int64(len({}))", go_str(&args[0])),
        Expr::Call(name, args) if name == "len" => format!("int64(len(sval(env[\"{}\"])))", ident_of(&args[0])),
        Expr::Call(name, args) if name == "min" => format!("min64({}, {})", go_int(&args[0]), go_int(&args[1])),
        Expr::Call(name, args) if name == "max" => format!("max64({}, {})", go_int(&args[0]), go_int(&args[1])),
        other => format!("ival({})", go_any(other)),
    }
}

fn go_bool(expr: &Expr) -> String {
    match expr {
        Expr::Bool(b) => b.to_string(),
        Expr::Binop(op, left, right) if matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") => {
            format!("({} {} {})", go_int(left), op, go_int(right))
        }
        Expr::Binop(op, left, right) if op == "&&" => format!("({} && {})", go_bool(left), go_bool(right)),
        Expr::Binop(op, left, right) if op == "||" => format!("({} || {})", go_bool(left), go_bool(right)),
        Expr::Unop(op, inner) if op == "!" => format!("(!{})", go_bool(inner)),
        Expr::Ident(name) => format!("bval(env[\"{name}\"])"),
        other => format!("(ival({}) != 0)", go_any(other)),
    }
}

fn go_str(expr: &Expr) -> String {
    match expr {
        Expr::Str(s) => format!("{s:?}"),
        Expr::Ident(name) => format!("sval(env[\"{name}\"])"),
        other => format!("sval({})", go_any(other)),
    }
}

fn go_any(expr: &Expr) -> String {
    match expr {
        Expr::Str(_) => go_str(expr),
        Expr::Bool(_) => go_bool(expr),
        Expr::Ident(name) => format!("env[\"{name}\"]"),
        other => go_int(other),
    }
}

fn go_op(op: &str) -> &str {
    op
}

fn ident_of(expr: &Expr) -> String {
    match expr {
        Expr::Ident(name) => name.clone(),
        _ => String::new(),
    }
}

fn goname(name: &str) -> String {
    let s = name.replace(':', "_");
    if ["type", "func", "map", "range", "select", "chan", "go", "defer", "var"].contains(&s.as_str()) {
        format!("{s}_")
    } else {
        s
    }
}

fn struct_name(name: &str) -> String {
    let s = goname(name);
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
        None => s,
    }
}

fn export_field(name: &str) -> String {
    struct_name(name)
}

fn go_ty(ty: &crate::ir::Ty, ir: &IrSchema) -> String {
    go_ty_at(ty, ir, true)
}
fn encode_arg(name: &str, ir: &IrSchema) -> String {
    match ir.types.get(name) {
        Some(TypeDef::Scalar(Storage::Bool)) => "bool".into(),
        Some(TypeDef::Scalar(Storage::Bytes)) => "[]byte".into(),
        Some(TypeDef::Scalar(Storage::F64)) => "float64".into(),
        Some(TypeDef::Scalar(Storage::Str)) | Some(TypeDef::String) => "string".into(),
        Some(TypeDef::Scalar(_)) => "int64".into(),
        Some(TypeDef::Record { .. }) => struct_name(name),
        _ => "any".into(),
    }
}

fn go_ty_at(ty: &crate::ir::Ty, ir: &IrSchema, ptr_record: bool) -> String {
    use crate::ir::Ty;
    match ty {
        Ty::Scalar(storage) => go_storage(*storage).into(),
        Ty::List(inner) => format!("[]{}", go_ty_at(inner, ir, false)),
        Ty::Optional(inner) => format!("*{}", go_ty_at(inner, ir, false)),
        Ty::Named(id) => match ir.types.get(&id.0) {
            Some(TypeDef::Scalar(storage)) => go_storage(*storage).into(),
            Some(TypeDef::String) => "string".into(),
            Some(TypeDef::Record { .. }) if ptr_record => format!("*{}", struct_name(&id.0)),
            Some(TypeDef::Record { .. }) => struct_name(&id.0),
            _ => "any".into(),
        },
    }
}

fn go_storage(storage: Storage) -> &'static str {
    match storage {
        Storage::I8 => "int8",
        Storage::I16 => "int16",
        Storage::I32 => "int32",
        Storage::I64 | Storage::I128 => "int64",
        Storage::U8 => "uint8",
        Storage::U16 => "uint16",
        Storage::U32 => "uint32",
        Storage::U64 => "uint64",
        Storage::Bool => "bool",
        Storage::F64 => "float64",
        Storage::Str => "string",
        Storage::Bytes => "[]byte",
    }
}

fn return_type(name: &str, ir: &IrSchema) -> String {
    match ir.types.get(name) {
        Some(TypeDef::Scalar(storage)) => go_storage(*storage).into(),
        Some(TypeDef::String) => "string".into(),
        Some(TypeDef::Record { .. }) => struct_name(name),
        _ => "any".into(),
    }
}



fn value_to_any(_name: &str, _ir: &IrSchema) -> &'static str {
    "value"
}

fn ret_zero(ret: &str) -> String {
    match ret {
        "bool" => "false".into(),
        "string" => "\"\"".into(),
        "[]byte" | "any" => "nil".into(),
        "struct{}" => "struct{}{}".into(),
        other if other.starts_with('*') || other.starts_with("[]") || other.chars().next().is_some_and(|c| c.is_uppercase()) => {
            format!("{other}{{}}")
        }
        _ => "0".into(),
    }
}

fn cast_result(ret: &str, expr: &Expr) -> String {
    match ret {
        "string" => go_str(expr),
        "bool" => go_bool(expr),
        "[]byte" => format!("{}.([]byte)", go_any(expr)),
        "int64" => go_int(expr),
        "int32" => format!("int32({})", go_int(expr)),
        "int16" => format!("int16({})", go_int(expr)),
        "int8" => format!("int8({})", go_int(expr)),
        "uint8" => format!("uint8({})", go_int(expr)),
        "uint16" => format!("uint16({})", go_int(expr)),
        "uint32" => format!("uint32({})", go_int(expr)),
        "uint64" => format!("uint64({})", go_int(expr)),
        "float64" => format!("fval({})", go_any(expr)),
        other if other.chars().next().is_some_and(|c| c.is_uppercase()) => {
            format!("deref{other}({})", go_any(expr))
        }
        _ => go_any(expr),
    }
}

fn record_lit(name: &str, ir: &IrSchema) -> String {
    let Some(TypeDef::Record { fields }) = ir.types.get(name) else {
        return "nil".into();
    };
    let parts = fields
        .iter()
        .map(|field| {
            format!("{}: {}", export_field(&field.name), field_get(&field.ty, "env", &field.name, ir))
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{}{{{parts}}}", struct_name(name))
}

fn field_get(ty: &crate::ir::Ty, map: &str, name: &str, ir: &IrSchema) -> String {
    use crate::ir::Ty;
    let key = format!("{map}[\"{name}\"]");
    match ty {
        Ty::Scalar(Storage::Bool) => format!("bval({key})"),
        Ty::Scalar(Storage::Str) => format!("sval({key})"),
        Ty::Scalar(Storage::Bytes) => format!("asBytes({key})"),
        Ty::Scalar(Storage::F64) => format!("fval({key})"),
        Ty::Scalar(storage) => format!("{}(ival({key}))", go_storage(*storage)),
        Ty::List(inner) => format!("{}({key})", list_fn(inner, ir)),
        Ty::Optional(inner) => format!("{}({key})", list_fn(inner, ir)),
        Ty::Named(id) => match ir.types.get(&id.0) {
            Some(TypeDef::Scalar(Storage::Bool)) => format!("bval({key})"),
            Some(TypeDef::Scalar(Storage::Str)) | Some(TypeDef::String) => format!("sval({key})"),
            Some(TypeDef::Scalar(Storage::Bytes)) => format!("asBytes({key})"),
            Some(TypeDef::Scalar(Storage::F64)) => format!("fval({key})"),
            Some(TypeDef::Scalar(storage)) => format!("{}(ival({key}))", go_storage(*storage)),
            Some(TypeDef::Record { .. }) => format!("asPtr{}({key})", struct_name(&id.0)),
            _ => key,
        },
    }
}

fn list_fn(inner: &crate::ir::Ty, ir: &IrSchema) -> String {
    use crate::ir::Ty;
    match inner {
        Ty::Named(id) if matches!(ir.types.get(&id.0), Some(TypeDef::Record { .. })) => {
            format!("asList{}", struct_name(&id.0))
        }
        Ty::Scalar(Storage::Bool) => "asListBool".into(),
        Ty::Scalar(Storage::Str) => "asListString".into(),
        Ty::Scalar(Storage::Bytes) => "asListBytes".into(),
        Ty::Scalar(storage) => format!("asList{}", go_storage(*storage)),
        Ty::Named(id) => match ir.types.get(&id.0) {
            Some(TypeDef::Scalar(storage)) => format!("asList{}", go_storage(*storage)),
            Some(TypeDef::String) => "asListString".into(),
            _ => "asListAny".into(),
        },
        _ => "asListAny".into(),
    }
}

fn encode_passed(callee: &str, value: &Expr, ir: &IrSchema) -> String {
    match ir.types.get(callee) {
        Some(TypeDef::Record { .. }) => format!("deref{}({})", struct_name(callee), go_any(value)),
        Some(TypeDef::String) | Some(TypeDef::Scalar(Storage::Str)) => go_str(value),
        Some(TypeDef::Scalar(Storage::Bool)) => go_bool(value),
        Some(TypeDef::Scalar(Storage::Bytes)) => format!("asBytes({})", go_any(value)),
        Some(TypeDef::Scalar(Storage::F64)) => format!("fval({})", go_any(value)),
        _ => go_int(value),
    }
}

fn record_alias<'a>(name: &str, ir: &'a IrSchema) -> Option<&'a str> {
    let TypeDef::Record { fields } = ir.types.get(name)? else {
        return None;
    };
    if !fields.is_empty() {
        return None;
    }
    let proc = ir.decode.get(name)?;
    let [Stmt::CallDecode { dst, callee, args }] = proc.body.as_slice() else {
        return None;
    };
    if dst != "value" || !args.is_empty() {
        return None;
    }
    if !matches!(proc.result, Some(Expr::Ident(ref n)) if n == "value") {
        return None;
    }
    if !matches!(ir.types.get(&callee.0), Some(TypeDef::Record { .. })) {
        return None;
    }
    Some(callee.0.as_str())
}

fn go_helpers(ir: &IrSchema) -> String {
    use crate::ir::Ty;
    use std::collections::BTreeSet;
    let mut ptrs = BTreeSet::new();
    for (name, def) in ir.types.iter() {
        if matches!(def, TypeDef::Record { .. }) {
            ptrs.insert(name.to_string());
        }
    }
    let mut lists: BTreeSet<String> = BTreeSet::new();
    for def in ir.types.values() {
        let TypeDef::Record { fields } = def else { continue };
        for field in fields {
            match &field.ty {
                Ty::Named(id) if matches!(ir.types.get(&id.0), Some(TypeDef::Record { .. })) => {
                    ptrs.insert(id.0.clone());
                }
                Ty::List(inner) => {
                    lists.insert(list_fn(inner, ir));
                    if let Ty::Named(id) = inner.as_ref()
                        && matches!(ir.types.get(&id.0), Some(TypeDef::Record { .. }))
                    {
                        ptrs.insert(id.0.clone());
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = String::from(
        "func asBytes(v any) []byte {\n\tswitch x := v.(type) {\n\tcase []byte:\n\t\treturn x\n\tcase string:\n\t\treturn []byte(x)\n\tdefault:\n\t\treturn nil\n\t}\n}\n\nfunc asListAny(v any) []any {\n\tif raw, ok := v.([]any); ok {\n\t\treturn raw\n\t}\n\treturn nil\n}\n\nfunc asListBool(v any) []bool {\n\traw, _ := v.([]any)\n\tout := make([]bool, 0, len(raw))\n\tfor _, item := range raw {\n\t\tout = append(out, bval(item))\n\t}\n\treturn out\n}\n\nfunc asListString(v any) []string {\n\traw, _ := v.([]any)\n\tout := make([]string, 0, len(raw))\n\tfor _, item := range raw {\n\t\tout = append(out, sval(item))\n\t}\n\treturn out\n}\n\nfunc asListBytes(v any) [][]byte {\n\traw, _ := v.([]any)\n\tout := make([][]byte, 0, len(raw))\n\tfor _, item := range raw {\n\t\tout = append(out, asBytes(item))\n\t}\n\treturn out\n}\n\n",
    );
    out.insert_str(
        0,
        "func anyItems(v any) []any {\n\tif raw, ok := v.([]any); ok {\n\t\treturn raw\n\t}\n\trv := reflect.ValueOf(v)\n\tif !rv.IsValid() || rv.Kind() != reflect.Slice {\n\t\treturn nil\n\t}\n\tout := make([]any, rv.Len())\n\tfor i := 0; i < rv.Len(); i++ {\n\t\tout[i] = rv.Index(i).Interface()\n\t}\n\treturn out\n}\n\nfunc fieldsToEnv(v any) map[string]any {\n\tif m, ok := v.(map[string]any); ok {\n\t\treturn m\n\t}\n\trv := reflect.ValueOf(v)\n\tif !rv.IsValid() {\n\t\treturn map[string]any{}\n\t}\n\tif rv.Kind() == reflect.Pointer {\n\t\tif rv.IsNil() {\n\t\t\treturn map[string]any{}\n\t\t}\n\t\trv = rv.Elem()\n\t}\n\tif rv.Kind() != reflect.Struct {\n\t\treturn map[string]any{}\n\t}\n\tout := map[string]any{}\n\trt := rv.Type()\n\tfor i := 0; i < rv.NumField(); i++ {\n\t\tkey := rt.Field(i).Tag.Get(\"bps\")\n\t\tif key == \"\" {\n\t\t\tkey = rt.Field(i).Name\n\t\t}\n\t\tout[key] = rv.Field(i).Interface()\n\t}\n\treturn out\n}\n\nfunc anyList[T any](v []T) []any {\n\tout := make([]any, len(v))\n\tfor i, e := range v {\n\t\tout[i] = e\n\t}\n\treturn out\n}\n\n",
    );
    let mut seen_num = std::collections::BTreeSet::new();
    for storage in [
        Storage::I8, Storage::I16, Storage::I32, Storage::I64, Storage::U8, Storage::U16,
        Storage::U32, Storage::U64,
    ] {
        let g = go_storage(storage);
        if !seen_num.insert(g) {
            continue;
        }
        out.push_str(&format!(
            "func asList{g}(v any) []{g} {{\n\traw, _ := v.([]any)\n\tout := make([]{g}, 0, len(raw))\n\tfor _, item := range raw {{\n\t\tout = append(out, {g}(ival(item)))\n\t}}\n\treturn out\n}}\n\n"
        ));
    }
    for name in ptrs {
        let s = struct_name(&name);
        let deref = if let Some(target) = record_alias(&name, ir) {
            format!("func deref{s}(v any) {s} {{ return deref{}(v) }}\n", struct_name(target))
        } else {
            let lit = match ir.types.get(&name) {
                Some(TypeDef::Record { fields }) => fields
                    .iter()
                    .map(|field| format!("{}: {}", export_field(&field.name), field_get(&field.ty, "m", &field.name, ir)))
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => String::new(),
            };
            format!("func deref{s}(v any) {s} {{\n\tswitch x := v.(type) {{\n\tcase {s}:\n\t\treturn x\n\tcase *{s}:\n\t\tif x == nil {{\n\t\t\treturn {s}{{}}\n\t\t}}\n\t\treturn *x\n\tcase map[string]any:\n\t\tm := x\n\t\t_ = m\n\t\treturn {s}{{{lit}}}\n\tdefault:\n\t\treturn {s}{{}}\n\t}}\n}}\n")
        };
        out.push_str(&format!(
            "{deref}func asPtr{s}(v any) *{s} {{\n\tx := deref{s}(v)\n\treturn &x\n}}\nfunc asList{s}(v any) []{s} {{\n\traw, _ := v.([]any)\n\tout := make([]{s}, 0, len(raw))\n\tfor _, item := range raw {{\n\t\tout = append(out, deref{s}(item))\n\t}}\n\treturn out\n}}\n\n"
        ));
    }
    let _ = lists;
    out
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
