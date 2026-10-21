//! `emit rstom`: a drop-in mirror crate for rstom's `rstom-packets`.
//! Port of `bps/emit_rstom.py`.

use crate::ast::{
    self, BlockItem, EncBody, EncodingDecl, Expr, FieldType, PacketDecl, ResolvedSchema, RouteDecl,
    TypeRef, WireField,
};
use crate::errors::{BpsError, BpsResult, emit, py_repr_str};
use crate::minestom::{ClassTable, Unit};
use crate::ordmap::OrderedMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// BPS type -> (NetworkBuffer type constant, Rust type).
pub const WIRE_TYPES: &[(&str, &str, &str)] = &[
    ("u8", "BYTE", "i8"),
    ("i8", "BYTE", "i8"),
    ("u16be", "UNSIGNED_SHORT", "u16"),
    ("i16be", "SHORT", "i16"),
    ("u32be", "UNSIGNED_INT", "u32"),
    ("i32be", "INT", "i32"),
    ("u64be", "LONG", "i64"),
    ("i64be", "LONG", "i64"),
    ("f32be", "FLOAT", "f32"),
    ("f64be", "DOUBLE", "f64"),
    ("varint", "VAR_INT", "i32"),
    ("varlong", "VAR_LONG", "i64"),
    ("boolean", "BOOLEAN", "bool"),
    ("angle", "BYTE", "i8"),
    ("position", "BLOCK_POSITION", "CoordVec"),
    ("uuid", "UUID", "Uuid"),
    ("string", "STRING", "String"),
    ("identifier", "STRING", "String"),
    ("nbt_string", "STRING", "String"),
    ("rest", "RAW_BYTES", "Vec<u8>"),
];

/// Opaque registry-keyed payloads, carried as raw frames.
pub const OPAQUE: &[&str] = &[
    "nbt",
    "nbt_entry",
    "nbt_compound_body",
    "nbt_list_body",
    "nbt_payload",
    "nbt_nothing",
    "fixed_bitset",
    "entity_metadata",
    "meta_entry",
];

/// Self-delimiting payloads and their hand-written scanners.
pub const SELF_DELIMITING: &[(&str, &str)] = &[
    ("text_component", "nbt_tag_len"),
    ("nbt", "nbt_tag_len"),
    ("slot", "slot_len"),
];

/// Smallest self-delimiting payload of each hand-scanned kind.
pub const SELF_DELIMITING_DEFAULT: &[(&str, &[u8])] = &[
    ("text_component", b"\x00"),
    ("nbt", b"\x00"),
    ("slot", b"\x00"),
];

/// Encodings carried as one frame wherever they appear.
pub const GENERATED_FRAMES: &[&str] = &[
    "id_set",
    "ingredient",
    "recipe_display",
    "hashed_slot",
    "bitset",
    "debug_subscription_update",
    "debug_subscription_event",
];

fn wire_entry(name: &str) -> Option<(&'static str, &'static str)> {
    WIRE_TYPES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, w, r)| (*w, *r))
}

fn is_wire(name: &str) -> bool {
    wire_entry(name).is_some()
}

fn is_opaque(name: &str) -> bool {
    OPAQUE.contains(&name)
}

fn self_delimiting(name: &str) -> Option<&'static str> {
    SELF_DELIMITING
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, w)| *w)
}

fn self_delimiting_default(name: &str) -> Option<&'static [u8]> {
    SELF_DELIMITING_DEFAULT
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, b)| *b)
}

/// Python `re.split(r"[^a-zA-Z0-9]+", s)` capitalised, joined.
fn pascal(s: &str) -> String {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut cs = p.chars();
            let first = cs.next().unwrap();
            let mut out = first.to_ascii_uppercase().to_string();
            out.push_str(cs.as_str());
            out
        })
        .collect()
}

/// Python `re.sub(r"[^a-zA-Z0-9_]", "_", s)`.
fn snake(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Length expression evaluated against bound parameters (`_eval_len_expr`).
fn eval_len_expr(expr: &Expr, bind: &OrderedMap<i128>) -> Option<i128> {
    match expr {
        Expr::Int(v) => Some(*v),
        Expr::Ident(name) => bind.get(name).copied(),
        Expr::Binop(op, l, r) => {
            let left = eval_len_expr(l, bind)?;
            let right = eval_len_expr(r, bind)?;
            match op.as_str() {
                "+" => Some(left + right),
                "-" => Some(left - right),
                "*" => Some(left * right),
                "/" => {
                    if right == 0 {
                        None
                    } else {
                        let q = left / right;
                        Some(if left % right != 0 && ((left < 0) != (right < 0)) {
                            q - 1
                        } else {
                            q
                        })
                    }
                }
                "%" => {
                    if right == 0 {
                        None
                    } else {
                        let m = left % right;
                        Some(if m != 0 && ((m < 0) != (right < 0)) {
                            m + right
                        } else {
                            m
                        })
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Python `int` arg value: int literal -> value, anything else -> 0.
fn arg_value(arg: &Expr) -> i128 {
    match arg {
        Expr::Int(v) => *v,
        _ => 0,
    }
}

/// Parameter bindings of an encoding reference: declared defaults, then
/// positional reference arguments.
fn bind_params(
    enc: &EncodingDecl,
    args: &[Expr],
    base: Option<&OrderedMap<i128>>,
) -> OrderedMap<i128> {
    let mut bind: OrderedMap<i128> = OrderedMap::new();
    for p in &enc.params {
        bind.insert(&p.name, p.default.unwrap_or(0));
    }
    if let Some(b) = base {
        for (k, v) in b.iter() {
            bind.insert(k, *v);
        }
    }
    for (i, arg) in args.iter().enumerate() {
        if i < enc.params.len() {
            bind.insert(&enc.params[i].name, arg_value(arg));
        }
    }
    bind
}

/// Byte length of a payload's frame, when the schema declares one (`_opaque_len`).
fn opaque_len(
    r: Option<&TypeRef>,
    resolved: Option<&ResolvedSchema>,
    params: Option<&OrderedMap<i128>>,
) -> Option<String> {
    let empty = OrderedMap::new();
    let params = params.unwrap_or(&empty);
    let r = r?;
    if r.name == "bytes" && !r.args.is_empty() {
        return eval_len_expr(&r.args[0], params).map(|v| v.to_string());
    }
    let resolved = resolved?;
    let enc = resolved.encodings.get(&r.name)?;
    let items = match &enc.body {
        EncBody::Block(b) => &b.items,
        _ => return None,
    };
    let bind = bind_params(enc, &r.args, Some(params));
    for item in items {
        let wf = match item {
            BlockItem::Field(wf) => wf,
            _ => continue,
        };
        let inner = match &wf.type_ref {
            FieldType::Ref(t) => t,
            _ => continue,
        };
        if inner.name != "bytes" || inner.args.is_empty() {
            continue;
        }
        let arg = &inner.args[0];
        match arg {
            Expr::Int(v) => return Some(v.to_string()),
            Expr::Ident(name) if bind.contains_key(name) => {
                return Some(bind.get(name).copied().unwrap_or(0).to_string());
            }
            _ => {}
        }
        if let Some(val) = eval_len_expr(arg, &bind) {
            return Some(val.to_string());
        }
    }
    None
}

/// (wire constant, Rust type) for a wire field's declared type (`_field_rust`).
fn field_rust(r: &TypeRef) -> (String, String) {
    if let Some((w, rt)) = wire_entry(&r.name) {
        if r.transforms.iter().any(|t| t == "utf8") {
            return (w.to_string(), "String".to_string());
        }
        return (w.to_string(), rt.to_string());
    }
    if r.name == "bytes" {
        return ("FIXED_BYTES".to_string(), "Vec<u8>".to_string());
    }
    ("RAW_BYTES".to_string(), "Vec<u8>".to_string())
}

use crate::analyze::reject_unlowered_scalars;
use crate::interp::encode_packet;
use crate::minestom::{PACKAGE_PREFIX, build_units, package_of};
use crate::synth::synthesize_packet;
use crate::value::{Env, Value};
use std::collections::HashSet;

/// The cached frame plan (`_Frames`), stashed on the resolved schema.
#[derive(Debug, Clone, Default)]
pub struct FramesPlan {
    /// Encodings carried in a fixed-length frame.
    pub framed: Vec<String>,
    pub walkers: crate::frames_gen::Walkers,
}

/// The plan for `resolved`, computed once and cached like Python's `_bps_frames`.
pub fn frames(resolved: &ResolvedSchema) -> BpsResult<Rc<FramesPlan>> {
    if let Some(plan) = resolved.frames_cache.borrow().clone() {
        return Ok(plan);
    }
    let lowered: BTreeSet<String> = WIRE_TYPES
        .iter()
        .map(|(n, _, _)| n.to_string())
        .chain(OPAQUE.iter().map(|n| n.to_string()))
        .chain(SELF_DELIMITING.iter().map(|(n, _)| n.to_string()))
        .collect();
    let recursive = crate::frames_gen::recursive_encodings(resolved);
    let mut framed: Vec<String> = Vec::new();
    for name in GENERATED_FRAMES
        .iter()
        .map(|s| s.to_string())
        .chain(recursive)
    {
        if resolved.encodings.contains_key(&name) && !framed.contains(&name) {
            framed.push(name);
        }
    }
    framed.sort();
    let mut roots: BTreeSet<String> = crate::frames_gen::element_encodings(resolved)
        .into_iter()
        .collect();
    roots.extend(framed.iter().cloned());
    let roots: Vec<String> = roots.into_iter().filter(|n| !lowered.contains(n)).collect();
    let mut known: OrderedMap<String> = OrderedMap::new();
    for (n, w) in SELF_DELIMITING {
        known.insert(n, w.to_string());
    }
    let walkers = crate::frames_gen::generate(resolved, &roots, &known)?;
    let mut broken: Vec<String> = framed
        .iter()
        .filter(|n| !lowered.contains(*n) && walkers.unsupported.contains_key(n))
        .cloned()
        .collect();
    broken.sort();
    if !broken.is_empty() {
        let parts: Vec<String> = broken
            .iter()
            .map(|n| {
                format!(
                    "{} ({})",
                    n,
                    walkers.unsupported.get(n).cloned().unwrap_or_default()
                )
            })
            .collect();
        return Err(emit(format!(
            "no frame walker can be compiled for {}",
            parts.join(", ")
        )));
    }
    let plan = Rc::new(FramesPlan { framed, walkers });
    *resolved.frames_cache.borrow_mut() = Some(plan.clone());
    Ok(plan)
}

/// The scanner for a self-delimiting payload field, if it is one (`_frame_walker`).
fn frame_walker(f: &PacketShape, resolved: Option<&ResolvedSchema>) -> BpsResult<Option<String>> {
    let r = match (&f.ref_, f.opaque) {
        (Some(r), true) => r,
        _ => return Ok(None),
    };
    let hand = self_delimiting(&r.name);
    match (hand, resolved) {
        (Some(h), _) => Ok(Some(h.to_string())),
        (None, None) => Ok(None),
        (None, Some(res)) => Ok(frames(res)?.walkers.names.get(&r.name).cloned()),
    }
}

/// The smallest valid payload of a self-delimiting kind, for a field's default (`_frame_default`).
fn frame_default(name: &str, resolved: Option<&ResolvedSchema>) -> BpsResult<Option<Vec<u8>>> {
    if let Some(hand) = self_delimiting_default(name) {
        return Ok(Some(hand.to_vec()));
    }
    let res = match resolved {
        None => return Ok(None),
        Some(r) => r,
    };
    let plan = frames(res)?;
    if !plan.walkers.names.contains_key(name) {
        return Ok(None);
    }
    let r = TypeRef {
        name: name.to_string(),
        args: Vec::new(),
        transforms: Vec::new(),
    };
    let env: crate::value::Env = OrderedMap::new();
    let scope = crate::synth::SynthScope { resolved: res };
    let value = crate::synth::value_for(&FieldType::Ref(r.clone()), &env, &scope)?;
    crate::interp::engine::encode_bare(&r, &value, res, &env)
}

/// How one repeat element's payload is framed (`_elem_frame_mode`).
fn elem_frame_mode(e: &PacketShape, resolved: &ResolvedSchema) -> BpsResult<String> {
    if let Some(w) = frame_walker(e, Some(resolved))? {
        return Ok(w);
    }
    if len_field(e).is_some() {
        return Ok("len_field".to_string());
    }
    if opaque_len(e.ref_.as_ref(), Some(resolved), Some(&e.params)).is_none()
        && (e.opaque || e.wire == "RAW_BYTES")
    {
        return Ok("prefix".to_string());
    }
    Ok("plain".to_string())
}

/// The field a `bytes(<ident>)` payload takes its length from (`_len_field`).
fn len_field(f: &PacketShape) -> Option<String> {
    let r = f.ref_.as_ref()?;
    if r.name != "bytes" || r.args.is_empty() {
        return None;
    }
    match &r.args[0] {
        Expr::Ident(name) => Some(name.clone()),
        _ => None,
    }
}

/// The Rust expression for the length a `bytes(<ident>)` payload reads (`_len_value`).
fn len_value(f: &PacketShape, types: &OrderedMap<String>) -> String {
    let ident = ident(&len_field(f).unwrap_or_default());
    if types
        .get(&ident)
        .map(|t| t.starts_with("Option<"))
        .unwrap_or(false)
    {
        format!("{ident}.unwrap_or(0)")
    } else {
        ident
    }
}

/// `_elem_needs_prefix`: only the `prefix` frame mode.
fn elem_needs_prefix(e: &PacketShape, resolved: &ResolvedSchema) -> BpsResult<bool> {
    Ok(elem_frame_mode(e, resolved)? == "prefix")
}

fn prefix_write(ident: &str, indent: &str) -> Vec<String> {
    vec![
        format!("{indent}buffer.write(VAR_INT, {ident}.len() as i32);"),
        format!("{indent}if !{ident}.is_empty() {{"),
        format!(
            "{indent}    buffer.write(fixed_raw_bytes({ident}.len() as i32), {ident}.clone());"
        ),
        format!("{indent}}}"),
    ]
}

fn prefix_read(ident: &str, indent: &str) -> Vec<String> {
    vec![
        format!("{indent}let {ident} = {{"),
        format!("{indent}    let len = buffer.read(VAR_INT) as usize;"),
        format!(
            "{indent}    if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len as i32)) }}"
        ),
        format!("{indent}}};"),
    ]
}

/// Read a payload whose length an earlier field declared (`_len_read`).
fn len_read(ident: &str, length: &str, indent: &str) -> Vec<String> {
    vec![
        format!("{indent}let {ident} = {{"),
        format!("{indent}    let n = {length} as usize;"),
        format!(
            "{indent}    if n == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(n as i32)) }}"
        ),
        format!("{indent}}};"),
    ]
}

/// Read a self-delimiting payload: scan its length, then that many bytes (`_scan_read`).
fn scan_read(ident: &str, walker: &str, indent: &str) -> Vec<String> {
    vec![
        format!("{indent}let {ident} = {{"),
        format!("{indent}    let mut raw = vec![0u8; buffer.readable_bytes() as usize];"),
        format!("{indent}    buffer.copy_to(buffer.read_index(), &mut raw);"),
        format!("{indent}    let len = crate::frames::{walker}(&raw) as i32;"),
        format!(
            "{indent}    if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len)) }}"
        ),
        format!("{indent}}};"),
    ]
}

/// Every identifier an expression reads, innermost first (`_expr_idents`).
fn expr_idents(expr: &Expr) -> Vec<String> {
    let mut found = Vec::new();
    collect_idents(expr, &mut found);
    found
}

fn collect_idents(expr: &Expr, found: &mut Vec<String>) {
    match expr {
        Expr::Ident(name) => found.push(name.clone()),
        Expr::Unop(_, a) => collect_idents(a, found),
        Expr::Binop(_, l, r) => {
            collect_idents(l, found);
            collect_idents(r, found);
        }
        Expr::Ternary(a, b, c) => {
            collect_idents(a, found);
            collect_idents(b, found);
            collect_idents(c, found);
        }
        Expr::Call(_, args) => {
            for a in args {
                collect_idents(a, found);
            }
        }
        _ => {}
    }
}

/// Element scope type map extended with the enclosing packet's fields (`_scope_types`).
fn scope_types(f: &PacketShape, top: Option<&[PacketShape]>) -> OrderedMap<String> {
    let mut types = f.elem_types.clone();
    for outer in top.unwrap_or(&[]) {
        let base = match &outer.repeat_mode {
            Some(_) => format!("Vec<{}>", outer.rtype),
            None => outer.rtype.clone(),
        };
        let mapped = if outer.guard.is_some() {
            format!("Option<{base}>")
        } else {
            base
        };
        if !types.contains_key(&outer.ident) {
            types.insert(&outer.ident, mapped);
        }
    }
    types
}

/// Write each element field from a bound `var` (`_elem_writes`).
fn elem_writes(
    f: &PacketShape,
    resolved: &ResolvedSchema,
    var: &str,
    top: Option<&[PacketShape]>,
) -> BpsResult<Vec<String>> {
    let types = scope_types(f, top);
    let mut out = Vec::new();
    for (name, expr) in &f.elem_computed {
        let operand = rust_operand(expr, &scope_types(f, top), "read")?;
        out.push(format!(
            "            let {name} = {};",
            strip_outer_parens(&operand)
        ));
    }
    for e in &f.element {
        if e.repeat_mode.is_some() {
            let iterable = if e.guard.is_some() {
                Some("items")
            } else {
                None
            };
            let inner = repeat_write(
                e,
                resolved,
                "                ",
                var,
                true,
                top,
                false,
                iterable,
            )?;
            if let Some(g) = &e.guard {
                let cond = rust_cond(g, &types, "read")?;
                out.push(format!(
                    "            if {cond} {{\n                if let Some(items) = &{} {{\n{inner}\n                }}\n            }}",
                    e.ident
                ));
            } else {
                out.push(inner);
            }
            continue;
        }
        let mode = elem_frame_mode(e, resolved)?;
        let write_var = if e.guard.is_some() {
            "v".to_string()
        } else {
            e.ident.clone()
        };
        let body: Vec<String> = if mode == "prefix" {
            prefix_write(&write_var, "                ")
        } else if mode != "plain" {
            vec![format!(
                "                buffer.write(RAW_BYTES, {write_var}.clone());"
            )]
        } else {
            let frame = frame_expr(e, resolved)?.unwrap_or_else(|| e.wire.clone());
            vec![format!(
                "                buffer.write({frame}, {write_var}.clone());"
            )]
        };
        if let Some(g) = &e.guard {
            let cond = rust_cond(g, &types, "read")?;
            let mut block = String::new();
            for line in &body {
                block.push_str(&format!("    {line}\n"));
            }
            out.push(format!(
                "            if {cond} {{\n                if let Some(v) = &{} {{\n{block}                }}\n            }}",
                e.ident
            ));
        } else {
            out.extend(body);
        }
    }
    Ok(out)
}

/// Read each element field as a local, ready to build the element (`_elem_reads`).
fn elem_reads(
    f: &PacketShape,
    resolved: &ResolvedSchema,
    var: &str,
    top: Option<&[PacketShape]>,
) -> BpsResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut computed: Vec<(String, Expr)> = f.elem_computed.clone();
    let mut bound: BTreeSet<String> = BTreeSet::new();
    let types = scope_types(f, top);

    fn drain(
        computed: &mut Vec<(String, Expr)>,
        bound: &BTreeSet<String>,
        types: &OrderedMap<String>,
        out: &mut Vec<String>,
    ) -> BpsResult<()> {
        let mut i = 0;
        while i < computed.len() {
            let ready = expr_idents(&computed[i].1)
                .iter()
                .all(|n| bound.contains(n));
            if ready {
                let (name, expr) = computed.remove(i);
                let operand = rust_operand(&expr, types, "read")?;
                out.push(format!(
                    "            let {name} = {};",
                    strip_outer_parens(&operand)
                ));
            } else {
                i += 1;
            }
        }
        Ok(())
    }

    for e in &f.element {
        if e.repeat_mode.is_some() {
            let inner = repeat_read(
                e,
                resolved,
                "                ",
                var,
                true,
                top,
                e.guard.is_none(),
                Some(&f.element),
            )?;
            if let Some(g) = &e.guard {
                let cond = rust_cond(g, &types, "read")?;
                out.push(format!(
                    "            let {ident}: {part} = if {cond} {{\n                let mut {ident}: Vec<{rtype}> = Vec::new();\n{inner}\n                Some({ident})\n            }} else {{ None }};",
                    ident = e.ident,
                    part = elem_part(e),
                    rtype = e.rtype,
                ));
            } else {
                out.push(inner);
            }
            continue;
        }
        let mode = elem_frame_mode(e, resolved)?;
        let body: Vec<String> = if mode == "prefix" {
            prefix_read(&e.ident, "                ")
        } else if mode == "len_field" {
            len_read(&e.ident, &len_value(e, &types), "                ")
        } else if mode != "plain" {
            scan_read(&e.ident, &mode, "                ")
        } else {
            let frame = frame_expr(e, resolved)?.unwrap_or_else(|| e.wire.clone());
            vec![format!(
                "                let {} = buffer.read({frame});",
                e.ident
            )]
        };
        if let Some(g) = &e.guard {
            let cond = rust_cond(g, &types, "read")?;
            out.push(format!(
                "            let {} = if {cond} {{\n{}\n                Some({})\n            }} else {{ None }};",
                e.ident,
                body.join("\n"),
                e.ident
            ));
        } else {
            out.extend(body);
        }
        bound.insert(e.ident.clone());
        drain(&mut computed, &bound, &types, &mut out)?;
    }
    for (name, expr) in &computed {
        let operand = rust_operand(expr, &types, "read")?;
        out.push(format!(
            "            let {name} = {};",
            strip_outer_parens(&operand)
        ));
    }
    Ok(out)
}

/// Packet-scope names an element's guards read, bound before the loop (`_outer_guard_refs`).
fn outer_guard_refs(f: &PacketShape) -> Vec<String> {
    let mut local: BTreeSet<String> = f.element.iter().map(|e| e.ident.clone()).collect();
    local.extend(f.elem_computed.iter().map(|(n, _)| n.clone()));
    let mut refs: BTreeSet<String> = BTreeSet::new();
    fn walk(elements: &[PacketShape], refs: &mut BTreeSet<String>, local: &mut BTreeSet<String>) {
        for e in elements {
            if let Some(g) = &e.guard {
                refs.extend(expr_idents(g));
            }
            local.insert(e.ident.clone());
            if e.repeat_mode.is_some() {
                walk(&e.element, refs, local);
            }
            local.extend(e.elem_computed.iter().map(|(n, _)| n.clone()));
        }
    }
    walk(&f.element, &mut refs, &mut local);
    refs.into_iter().filter(|r| !local.contains(r)).collect()
}

fn outer_bindings(f: &PacketShape, indent: &str) -> Vec<String> {
    outer_guard_refs(f)
        .into_iter()
        .map(|name| format!("{indent}let {name} = value.{name}.clone();"))
        .collect()
}

/// Write a repeat field (`_repeat_write`).
#[allow(clippy::too_many_arguments)]
fn repeat_write(
    f: &PacketShape,
    resolved: &ResolvedSchema,
    indent: &str,
    var: &str,
    nested: bool,
    top: Option<&[PacketShape]>,
    optional_vec: bool,
    iterable: Option<&str>,
) -> BpsResult<String> {
    let is_count = f.repeat_mode.as_deref() == Some("count");
    if is_count {
        let body = elem_writes(f, resolved, "item", top)?.join("\n");
        if nested {
            let mut loop_head = String::new();

            let it = iterable
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("&{}.clone()", f.ident));
            loop_head.push_str(&format!("{indent}for {var}_item in {it} {{\n"));
            return Ok(format!(
                "{loop_head}{}\n{body}\n{indent}}}",
                destructure(f, var)
            ));
        } else if optional_vec {
            let mut loop_head = String::new();
            for line in outer_bindings(f, indent) {
                loop_head.push_str(&format!("{line}\n"));
            }
            loop_head.push_str(&format!(
                "{indent}if let Some(items) = &{var}.{} {{\n{indent}    for {var}_item in items {{\n",
                f.ident
            ));
            let body = body.replacen(indent, &format!("{indent}    "), 1);
            return Ok(format!(
                "{loop_head}{}\n{body}\n{indent}    }}\n{indent}}}",
                destructure(f, var)
            ));
        }
        let mut loop_head = String::new();
        for line in outer_bindings(f, indent) {
            loop_head.push_str(&format!("{line}\n"));
        }
        loop_head.push_str(&format!(
            "{indent}for {var}_item in &{var}.{} {{\n",
            f.ident
        ));
        return Ok(format!(
            "{loop_head}{}\n{body}\n{indent}}}",
            destructure(f, var)
        ));
    }
    // until_end / until: the terminator is a schema detail; emit to the end.
    let body = elem_writes(f, resolved, var, top)?.join("\n");
    if nested {
        let loop_head = format!("{indent}for {var}_item in &{}.clone() {{\n", f.ident);
        return Ok(format!(
            "{loop_head}{}\n{body}\n{indent}}}",
            destructure(f, var)
        ));
    } else if optional_vec {
        let loop_head = format!(
            "{indent}if let Some(items) = &{var}.{} {{\n{indent}    for {var}_item in items {{\n",
            f.ident
        );
        return Ok(format!(
            "{loop_head}{}\n{body}\n{indent}    }}\n{indent}}}",
            destructure(f, var)
        ));
    }
    let loop_head = format!("{indent}for {var}_item in &{var}.{} {{\n", f.ident);
    Ok(format!(
        "{loop_head}{}\n{body}\n{indent}}}",
        destructure(f, var)
    ))
}

/// Read a repeat field (`_repeat_read`).
#[allow(clippy::too_many_arguments)]
fn repeat_read(
    f: &PacketShape,
    resolved: &ResolvedSchema,
    indent: &str,
    var: &str,
    nested: bool,
    top: Option<&[PacketShape]>,
    bind: bool,
    outer_scope: Option<&[PacketShape]>,
) -> BpsResult<String> {
    let reads = elem_reads(f, resolved, var, top)?.join("\n");
    if f.repeat_mode.as_deref() == Some("count") {
        let count = match &f.count {
            Some(c) => c.clone(),
            None => format!("{}_count", ident(&f.ident)),
        };
        let mut scope: Vec<PacketShape> = if nested {
            f.element.clone()
        } else {
            top.map(|t| t.to_vec()).unwrap_or_default()
        };
        scope.extend(outer_scope.map(|s| s.to_vec()).unwrap_or_default());
        let count = unwrap_count(&count, &scope);
        let lp = format!(
            "{indent}for _ in 0..{count} {{\n{reads}\n{indent}    {}.push({});\n{indent}}}",
            f.ident,
            elem_struct(f)
        );
        if !bind {
            return Ok(lp);
        }
        return Ok(format!(
            "{indent}let mut {}: Vec<{}> = Vec::new();\n{lp}",
            f.ident, f.rtype
        ));
    }
    Ok(format!(
        "{indent}let mut {} = Vec::new();\n{indent}while buffer.readable_bytes() > 0 {{\n{reads}\n{indent}    {}.push({});\n{indent}}}",
        f.ident,
        f.ident,
        elem_struct(f)
    ))
}

fn elem_names(f: &PacketShape) -> Vec<String> {
    f.element.iter().map(|e| e.ident.clone()).collect()
}

/// Build an element from the locals the read loop bound (`_elem_struct`).
fn elem_struct(f: &PacketShape) -> String {
    let names = elem_names(f);
    if names.len() == 1 {
        return names[0].clone();
    }
    format!("({})", names.join(", "))
}

/// An optional count is bound as `Option`, so unwrap it for the loop bound (`_unwrap_count`).
fn unwrap_count(count: &str, scope: &[PacketShape]) -> String {
    let optional = scope
        .iter()
        .any(|f| f.guard.is_some() && f.repeat_mode.is_none() && f.ident == count);
    if optional {
        format!("{count}.unwrap_or(0)")
    } else {
        count.to_string()
    }
}

/// Bind the element's fields from the loop variable (`_destructure`).
fn destructure(f: &PacketShape, var: &str) -> String {
    let names = elem_names(f);
    let binding = format!("{var}_item");
    if names.len() == 1 {
        return format!("            let {} = {binding}.clone();", names[0]);
    }
    format!(
        "            let ({}) = {binding}.clone();",
        names.join(", ")
    )
}

/// The `Type` expression for a payload field, or None for a plain wire type (`_frame_expr`).
fn frame_expr(f: &PacketShape, resolved: &ResolvedSchema) -> BpsResult<Option<String>> {
    if f.wire == "FIXED_BYTES" || f.opaque {
        let n = opaque_len(f.ref_.as_ref(), Some(resolved), Some(&f.params));
        return Ok(Some(match n {
            Some(n) => format!("fixed_raw_bytes({n})"),
            None => "RAW_BYTES".to_string(),
        }));
    }
    Ok(None)
}

/// `protocol_version` -> `protocolVersion` (`_java_name`).
fn java_name(ident: &str) -> String {
    let s = snake(ident);
    let parts: Vec<&str> = s.split('_').collect();
    let mut out = parts[0].to_string();
    for p in &parts[1..] {
        let mut cs = p.chars();
        if let Some(first) = cs.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(cs.as_str());
        }
    }
    out
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield",
];

/// A safe Rust identifier for a schema field name (`_ident`).
fn ident(name: &str) -> String {
    if RUST_KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// A zero value for a Rust type (`_default_value`).
fn default_value(rtype: &str) -> String {
    match rtype {
        "bool" => "false".to_string(),
        "f32" | "f64" => "0.0".to_string(),
        "u8" | "i8" | "u16" | "i16" | "u32" | "i32" | "u64" | "i64" => "0".to_string(),
        "String" => "String::new()".to_string(),
        "Uuid" => "Uuid::from_bits(0, 0)".to_string(),
        "CoordVec" => "CoordVec::new(0.0, 0.0, 0.0)".to_string(),
        _ if rtype.starts_with("Vec<") => "Vec::new()".to_string(),
        _ => "Default::default()".to_string(),
    }
}

/// A flattened field: its wire type, Rust type, and optionality (`PacketShape`).
#[derive(Debug, Clone)]
pub struct PacketShape {
    pub ident: String,
    pub java: String,
    pub wire: String,
    pub rtype: String,
    pub guard: Option<Expr>,
    /// An opaque payload is not self-delimiting as a plain field, so the
    /// serializer wraps it in a fixed-length frame.
    pub opaque: bool,
    pub ref_: Option<TypeRef>,
    pub params: OrderedMap<i128>,
    /// `count` mode writes a length prefix; the count value comes from the
    /// field named by `count`.
    pub repeat_mode: Option<String>,
    pub count: Option<String>,
    pub element: Vec<PacketShape>,
    /// The element scope's name -> Rust type map, for guards that read a
    /// sibling subfield of the same element.
    pub elem_types: OrderedMap<String>,
    /// The element scope's computed locals (a guard can read a computed discriminator).
    pub elem_computed: Vec<(String, Expr)>,
    /// Dotted path of enclosing field names this field was flattened out of.
    pub parent: String,
}

impl PacketShape {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &str,
        wire: &str,
        rtype: &str,
        opaque: bool,
        ref_: Option<TypeRef>,
        params: Option<OrderedMap<i128>>,
        repeat_mode: Option<String>,
        count: Option<String>,
        element: Option<Vec<PacketShape>>,
        guard: Option<Expr>,
        parent: &str,
    ) -> Self {
        PacketShape {
            ident: ident(name),
            java: java_name(name),
            wire: wire.to_string(),
            rtype: rtype.to_string(),
            guard,
            opaque,
            ref_,
            params: params.unwrap_or_default(),
            repeat_mode,
            count,
            element: element.unwrap_or_default(),
            elem_types: OrderedMap::new(),
            elem_computed: Vec::new(),
            parent: parent.to_string(),
        }
    }
}

/// The Rust element type: the one field's type, or a tuple for a record (`_elem_rtype`).
fn elem_rtype(element: &[PacketShape]) -> String {
    if element.len() == 1 && element[0].repeat_mode.is_none() {
        return elem_part(&element[0]);
    }
    if element.is_empty() {
        return "()".to_string();
    }
    format!(
        "({})",
        element.iter().map(elem_part).collect::<Vec<_>>().join(", ")
    )
}

/// One tuple slot's type: a guarded subfield is optional, like a packet field (`_elem_part`).
fn elem_part(e: &PacketShape) -> String {
    let base = if e.repeat_mode.is_some() {
        format!("Vec<{}>", e.rtype)
    } else {
        e.rtype.clone()
    };
    if e.guard.is_some() {
        format!("Option<{base}>")
    } else {
        base
    }
}

/// A Rust expression for a `count =` that is not a plain field name (`_count_expr`).
fn count_expr(expr: Option<&Expr>) -> Option<String> {
    match expr? {
        Expr::Ident(n) => Some(ident(n)),
        Expr::Binop(op, l, r) => {
            let left = count_expr(Some(l))?;
            let right = count_expr(Some(r))?;
            Some(format!("{left} {op} {right}"))
        }
        Expr::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

/// The field a `count = <ident>` reads, when it is a plain identifier (`_count_field`).
fn count_field(expr: Option<&Expr>) -> Option<String> {
    match expr {
        Some(Expr::Ident(n)) => Some(n.clone()),
        _ => None,
    }
}

fn and_guard(left: Option<Expr>, right: Option<Expr>) -> Option<Expr> {
    match (left, right) {
        (None, r) => r,
        (l, None) => l,
        (Some(l), Some(r)) => Some(Expr::Binop("&&".into(), Box::new(l), Box::new(r))),
    }
}

fn or_guard(left: Option<Expr>, right: Option<Expr>) -> Option<Expr> {
    match (left, right) {
        (None, r) => r,
        (l, None) => l,
        (Some(l), Some(r)) => Some(Expr::Binop("||".into(), Box::new(l), Box::new(r))),
    }
}

/// Integer tags of a match arm (`_arm_int_tags`).
fn arm_int_tags(arm: &ast::MatchArm) -> Vec<i128> {
    let raw: Vec<ast::Tag> = if !arm.tags.is_empty() {
        arm.tags.clone()
    } else {
        arm.tag.iter().cloned().collect()
    };
    raw.into_iter()
        .filter_map(|t| match t {
            ast::Tag::Int(i) => Some(i),
            _ => None,
        })
        .collect()
}

fn match_tag_name(expr: &Expr) -> BpsResult<String> {
    match expr {
        Expr::Ident(n) if !n.is_empty() => Ok(n.clone()),
        _ => Err(emit(format!("unsupported match tag expression: {expr:?}"))),
    }
}

/// (inner type, is_option) for a schema field name (`_field_rust_type`).
fn field_rust_type(name: &str, types: &OrderedMap<String>) -> (String, bool) {
    let rtype = types
        .get(name)
        .or_else(|| types.get(&ident(name)))
        .cloned()
        .unwrap_or_else(|| "i32".to_string());
    if rtype.starts_with("Option<") && rtype.ends_with('>') {
        return (rtype[7..rtype.len() - 1].to_string(), true);
    }
    (rtype, false)
}

fn rust_field_access(name: &str, types: &OrderedMap<String>, mode: &str) -> String {
    let prefix = if mode == "write" { "value." } else { "" };
    let access = format!("{prefix}{}", ident(name));
    let (inner, optional) = field_rust_type(name, types);
    if !optional {
        // A mask literal over a narrow wire type must fit the type, so the
        // operand widens: `actions & 128` over a `u8` is `(actions as i32) & 128`.
        if matches!(inner.as_str(), "i8" | "u8" | "i16" | "u16") {
            return format!("({access} as i32)");
        }
        return access;
    }
    if inner == "bool" {
        return format!("{access}.unwrap_or(false)");
    }
    if matches!(
        inner.as_str(),
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "f32" | "f64"
    ) {
        return format!("{access}.unwrap_or(0)");
    }
    format!("{access}.clone().unwrap_or_default()")
}

/// Rust scalar expression for a guard operand (`_rust_operand`).
fn rust_operand(expr: &Expr, types: &OrderedMap<String>, mode: &str) -> BpsResult<String> {
    match expr {
        Expr::Int(i) => Ok(i.to_string()),
        Expr::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
        Expr::Ident(n) if !n.is_empty() => Ok(rust_field_access(n, types, mode)),
        Expr::Binop(op, l, r) => {
            let left = rust_operand(l, types, mode)?;
            let right = rust_operand(r, types, mode)?;
            Ok(format!("({left} {op} {right})"))
        }
        Expr::Unop(op, inner) if op == "!" => {
            Ok(format!("!({})", rust_operand(inner, types, mode)?))
        }
        _ => Err(emit(format!("unsupported guard operand: {expr:?}"))),
    }
}

fn is_zero_int(e: &Expr) -> bool {
    matches!(e, Expr::Int(0))
}

/// Rust boolean expression for a schema guard; `None` is `true` (`_rust_guard`).
fn rust_guard(expr: Option<&Expr>, types: &OrderedMap<String>, mode: &str) -> BpsResult<String> {
    let Some(expr) = expr else {
        return Ok("true".to_string());
    };
    let truthy_ident = |name: &str| -> String {
        let (inner, _) = field_rust_type(name, types);
        let access = rust_field_access(name, types, mode);
        if inner == "bool" {
            access
        } else {
            format!("({access} != 0)")
        }
    };
    match expr {
        Expr::Int(i) => Ok(i.to_string()),
        Expr::Bool(b) => Ok(if *b { "true" } else { "false" }.to_string()),
        Expr::Ident(n) if !n.is_empty() => Ok(truthy_ident(n)),
        Expr::Unop(op, inner) if op == "!" => {
            Ok(format!("!({})", rust_guard(Some(inner), types, mode)?))
        }
        Expr::Binop(op, l, r) => {
            if op == "&&" || op == "||" {
                let left = rust_guard(Some(l), types, mode)?;
                let right = rust_guard(Some(r), types, mode)?;
                return Ok(format!("({left} {op} {right})"));
            }
            if matches!(op.as_str(), "==" | "!=")
                && is_zero_int(r)
                && let Expr::Ident(n) = &**l
                && !n.is_empty()
            {
                let (inner, _) = field_rust_type(n, types);
                if inner == "bool" {
                    let access = rust_field_access(n, types, mode);
                    return Ok(if op == "!=" {
                        access
                    } else {
                        format!("!{access}")
                    });
                }
            }
            let left = rust_operand(l, types, mode)?;
            let right = rust_operand(r, types, mode)?;
            Ok(format!("({left} {op} {right})"))
        }
        _ => Err(emit(format!("unsupported guard expression: {expr:?}"))),
    }
}

/// Drop one wrap-around `(...)` pair when it encloses the whole expression (`_strip_outer_parens`).
fn strip_outer_parens(text: &str) -> String {
    if !text.starts_with('(') {
        return text.to_string();
    }
    let mut depth: i32 = 0;
    for (i, ch) in text.char_indices() {
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                if i == text.len() - 1 {
                    return text[1..text.len() - 1].to_string();
                }
                return text.to_string();
            }
        }
    }
    text.to_string()
}

/// `_rust_guard` as a top-level `if` condition, without the wrapper parens (`_rust_cond`).
fn rust_cond(expr: &Expr, types: &OrderedMap<String>, mode: &str) -> BpsResult<String> {
    Ok(strip_outer_parens(&rust_guard(Some(expr), types, mode)?))
}

/// Whether a guard is true for synthesized default values (`_guard_holds`).
fn guard_holds(expr: Option<&Expr>, env: &Env, types: &OrderedMap<String>) -> BpsResult<bool> {
    let Some(expr) = expr else {
        return Ok(true);
    };
    match expr {
        Expr::Int(i) => Ok(*i != 0),
        Expr::Bool(b) => Ok(*b),
        Expr::Ident(n) if !n.is_empty() => {
            let v = env
                .get(&ident(n))
                .or_else(|| env.get(n))
                .cloned()
                .unwrap_or(Value::Int(0));
            let rtype = types
                .get(n)
                .or_else(|| types.get(&ident(n)))
                .cloned()
                .unwrap_or_else(|| "i32".to_string());
            if rtype == "bool" {
                return Ok(v.truthy());
            }
            Ok(value_int(&v)? != 0)
        }
        Expr::Unop(op, inner) if op == "!" => Ok(!guard_holds(Some(inner), env, types)?),
        Expr::Binop(op, l, r) => match op.as_str() {
            "&&" => Ok(guard_holds(Some(l), env, types)? && guard_holds(Some(r), env, types)?),
            "||" => Ok(guard_holds(Some(l), env, types)? || guard_holds(Some(r), env, types)?),
            "==" | "!=" | "<" | "<=" | ">" | ">=" => {
                let left = guard_holds_value(l, env)?;
                let right = guard_holds_value(r, env)?;
                Ok(match op.as_str() {
                    "==" => left == right,
                    "!=" => left != right,
                    "<" => left < right,
                    "<=" => left <= right,
                    ">" => left > right,
                    _ => left >= right,
                })
            }
            _ => Ok(false),
        },
        _ => Ok(false),
    }
}

/// Integer view of a synthesized value, as Python's `int()` (`bool` is `0`/`1`).
fn value_int(v: &Value) -> BpsResult<i128> {
    match v {
        Value::Bool(b) => Ok(*b as i128),
        Value::Float(f) => crate::value::float_to_int(*f).map_err(emit),
        _ => v.as_int().map_err(emit),
    }
}

/// Python floor division of integers (`//`).
fn floordiv(a: i128, b: i128) -> BpsResult<i128> {
    if b == 0 {
        return Err(emit("integer division or modulo by zero"));
    }
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        Ok(q - 1)
    } else {
        Ok(q)
    }
}

fn guard_holds_value(expr: &Expr, env: &Env) -> BpsResult<i128> {
    match expr {
        Expr::Int(i) => Ok(*i),
        Expr::Ident(n) if !n.is_empty() => match env.get(n) {
            Some(v) => value_int(v),
            None => Ok(0),
        },
        Expr::Binop(op, l, r) => {
            let left = guard_holds_value(l, env)?;
            let right = guard_holds_value(r, env)?;
            match op.as_str() {
                "+" => Ok(left + right),
                "-" => Ok(left - right),
                "*" => Ok(left * right),
                "/" => floordiv(left, right),
                _ => Ok(0),
            }
        }
        _ => Ok(0),
    }
}

/// The generated struct has one namespace per packet; a clash cannot be merged (`_declared_twice`).
fn declared_twice(ident_name: &str, first: &str, second: &str) -> BpsError {
    if first == second {
        return emit(format!(
            "repeated field {} is declared more than once in one scope; declare it once under a combined condition",
            py_repr_str(ident_name)
        ));
    }
    let f = if first.is_empty() {
        "the packet".to_string()
    } else {
        py_repr_str(first)
    };
    let s = if second.is_empty() {
        "the packet".to_string()
    } else {
        py_repr_str(second)
    };
    emit(format!(
        "field {} is declared by both {f} and {s}; the generated struct has one namespace per packet, so give them distinct names",
        py_repr_str(ident_name)
    ))
}

/// Reconcile a name declared twice in the same flattened scope (`_merge_declared`).
/// `idx` is the position of the existing declaration in `out`.
fn merge_declared(
    out: &mut Vec<PacketShape>,
    idx: usize,
    guard: Option<Expr>,
    parent: &str,
) -> BpsResult<()> {
    let existing_parent = out[idx].parent.clone();
    if !existing_parent.is_empty() && !parent.is_empty() && existing_parent != parent {
        return Err(declared_twice(&out[idx].ident, &existing_parent, parent));
    }
    if guard.is_none() && parent.is_empty() && out[idx].guard.is_some() {
        let mut existing = out.remove(idx);
        existing.guard = None;
        existing.parent = String::new();
        out.push(existing);
        return Ok(());
    }
    let current = out[idx].guard.take();
    out[idx].guard = or_guard(current, guard);
    Ok(())
}

/// Flatten a named encoding's own fields into the packet's field list (`_expand_encoding`).
/// Returns true when the encoding was expanded, so the caller does not also emit an opaque frame.
fn expand_encoding(
    r: &TypeRef,
    resolved: Option<&ResolvedSchema>,
    out: &mut Vec<PacketShape>,
    guard: Option<&Expr>,
    field_types: &mut OrderedMap<String>,
    parent: &str,
) -> BpsResult<bool> {
    let Some(resolved) = resolved else {
        return Ok(false);
    };
    // Wire types and opaque payloads have their own lowering; their block bodies
    // (a varint's continuation bytes) are not packet fields.
    if is_opaque(&r.name) || is_wire(&r.name) {
        return Ok(false);
    }
    let Some(enc) = resolved.encodings.get(&r.name) else {
        return Ok(false);
    };
    let EncBody::Block(body) = &enc.body else {
        return Ok(false);
    };
    // Parameters bound at the reference site travel with the expanded fields, so
    // `fixed_bitset(20)` resolves its inner `bytes(1 + (bits - 1) / 8)` to 3.
    let mut bind: OrderedMap<i128> = OrderedMap::new();
    for p in &enc.params {
        bind.insert(&p.name, p.default.unwrap_or(0));
    }
    for (i, arg) in r.args.iter().enumerate() {
        if i < enc.params.len() {
            let v = match arg {
                Expr::Int(v) => *v,
                _ => 0,
            };
            bind.insert(&enc.params[i].name, v);
        }
    }
    flatten(
        &body.items,
        out,
        Some(resolved),
        Some(&bind),
        guard,
        field_types,
        parent,
        true,
    )?;
    Ok(true)
}

/// A repeated field: a Vec whose elements are the flattened element type (`_repeat_shape`).
fn repeat_shape(
    wf: &WireField,
    resolved: Option<&ResolvedSchema>,
    bind: Option<&OrderedMap<i128>>,
    guard: Option<&Expr>,
    _field_types: &mut OrderedMap<String>,
    parent: &str,
) -> BpsResult<PacketShape> {
    let repeat = wf.repeat.as_ref();
    let mut count = None;
    if let Some(rep) = repeat
        && rep.mode == ast::RepeatMode::Count
    {
        count = count_field(rep.expr.as_ref()).or_else(|| count_expr(rep.expr.as_ref()));
    }
    let mut element: Vec<PacketShape> = Vec::new();
    let mut elem_types: OrderedMap<String> = OrderedMap::new();
    let mut computed: Vec<(String, Expr)> = Vec::new();
    match &wf.type_ref {
        FieldType::Block(b) => {
            flatten(
                &b.items,
                &mut element,
                resolved,
                bind,
                None,
                &mut elem_types,
                parent,
                false,
            )?;
            for item in &b.items {
                if let BlockItem::Computed { name, expr } = item {
                    computed.push((name.clone(), expr.clone()));
                }
            }
        }
        FieldType::Ref(r) => {
            // An opaque, registry-keyed element (a repeated Slot, say) has no plain
            // wire type; carry its bytes so the element is written at all. Dropping
            // it here silently erased the field from the wire (`Vec<()>`).
            let (wire, rtype) = field_rust(r);
            let opaque = wire == "RAW_BYTES";
            element.push(PacketShape::new(
                "_item",
                &wire,
                &rtype,
                opaque,
                Some(r.clone()),
                bind.cloned(),
                None,
                None,
                None,
                None,
                "",
            ));
        }
        FieldType::Match(_) => {}
    }
    let ref_ = match &wf.type_ref {
        FieldType::Ref(r) => Some(r.clone()),
        _ => None,
    };
    let mut shape = PacketShape::new(
        &wf.name,
        "REPEAT",
        &elem_rtype(&element),
        false,
        ref_,
        bind.cloned(),
        repeat.map(|r| r.mode.as_str().to_string()),
        count,
        Some(element.clone()),
        guard.cloned(),
        parent,
    );
    // The element scope's own name -> Rust type map, for guards that read a
    // sibling subfield of the same element.
    shape.elem_types = elem_types;
    shape.elem_computed = computed;
    Ok(shape)
}

/// Add one wire field (possibly expanded) to the flattened list (`_append_field`).
#[allow(clippy::too_many_arguments)]
fn append_field(
    out: &mut Vec<PacketShape>,
    wf: &WireField,
    resolved: Option<&ResolvedSchema>,
    bind: Option<&OrderedMap<i128>>,
    guard: Option<&Expr>,
    field_types: &mut OrderedMap<String>,
    parent: &str,
    expand_payloads: bool,
) -> BpsResult<()> {
    if wf.repeat.is_some() {
        let shape = repeat_shape(wf, resolved, bind, guard, field_types, parent)?;
        if let Some(clash) = out.iter().find(|f| f.ident == shape.ident) {
            return Err(declared_twice(&clash.ident, &clash.parent, parent));
        }
        out.push(shape);
        return Ok(());
    }
    let type_ref = match &wf.type_ref {
        FieldType::Match(mt) => {
            let match_parent = if parent.is_empty() {
                wf.name.clone()
            } else {
                format!("{parent}.{}", wf.name)
            };
            return flatten_match(mt, out, resolved, bind, guard, field_types, &match_parent);
        }
        FieldType::Block(b) => {
            return flatten(
                &b.items,
                out,
                resolved,
                bind,
                guard,
                field_types,
                parent,
                true,
            );
        }
        FieldType::Ref(r) => r,
    };
    if type_ref.name == "text_component" {
        // An NBT text component, not a single byte: carry its encoded payload
        // like every other registry-keyed payload (`nbt`, `slot`).
        let (wire, rtype) = ("RAW_BYTES", "Vec<u8>");
        if let Some(idx) = out.iter().position(|f| f.ident == ident(&wf.name)) {
            return merge_declared(out, idx, guard.cloned(), parent);
        }
        out.push(PacketShape::new(
            &wf.name,
            wire,
            rtype,
            true,
            Some(type_ref.clone()),
            bind.cloned(),
            None,
            None,
            None,
            guard.cloned(),
            parent,
        ));
        field_types.insert(
            &wf.name,
            if guard.is_some() {
                format!("Option<{rtype}>")
            } else {
                rtype.to_string()
            },
        );
        return Ok(());
    }
    let framed = is_opaque(&type_ref.name)
        || self_delimiting(&type_ref.name).is_some()
        || match resolved {
            Some(r) => {
                frames(r)?.framed.iter().any(|n| n == &type_ref.name)
                    || opaque_len(Some(type_ref), Some(r), bind).is_some()
            }
            None => false,
        };
    if expand_payloads
        && !framed
        && !is_wire(&type_ref.name)
        && expand_encoding(
            type_ref,
            resolved,
            out,
            guard,
            field_types,
            &if parent.is_empty() {
                wf.name.clone()
            } else {
                format!("{parent}.{}", wf.name)
            },
        )?
    {
        return Ok(());
    }
    let existing = out.iter().position(|f| f.ident == ident(&wf.name));
    let (wire, rtype) = field_rust(type_ref);
    if let Some(idx) = existing {
        return merge_declared(out, idx, guard.cloned(), parent);
    }
    out.push(PacketShape::new(
        &wf.name,
        &wire,
        &rtype,
        !is_wire(&type_ref.name),
        Some(type_ref.clone()),
        bind.cloned(),
        None,
        None,
        None,
        guard.cloned(),
        parent,
    ));
    field_types.insert(
        &wf.name,
        if guard.is_some() {
            format!("Option<{rtype}>")
        } else {
            rtype.clone()
        },
    );
    Ok(())
}

/// A match statement: one guarded group per tagged arm, plus the default arm (`_flatten_match`).
#[allow(clippy::too_many_arguments)]
fn flatten_match(
    mt: &ast::MatchType,
    out: &mut Vec<PacketShape>,
    resolved: Option<&ResolvedSchema>,
    bind: Option<&OrderedMap<i128>>,
    parent_guard: Option<&Expr>,
    field_types: &mut OrderedMap<String>,
    parent: &str,
) -> BpsResult<()> {
    let tag = match_tag_name(&mt.tag_expr)?;
    let is_default_arm = |a: &ast::MatchArm| {
        let default_tag = match &a.tag {
            None => true,
            Some(ast::Tag::Str(s)) => s == "_",
            _ => false,
        };
        default_tag && a.tags.is_empty()
    };
    let mut explicit: Vec<i128> = Vec::new();
    for arm in &mt.arms {
        if is_default_arm(arm) {
            continue;
        }
        let tags = arm_int_tags(arm);
        if tags.is_empty() {
            continue;
        }
        explicit.extend(tags.iter().copied());
        let mut arm_guard: Option<Expr> = None;
        for arm_tag in &tags {
            arm_guard = or_guard(
                arm_guard,
                Some(Expr::Binop(
                    "==".into(),
                    Box::new(Expr::Ident(tag.clone())),
                    Box::new(Expr::Int(*arm_tag)),
                )),
            );
        }
        let arm_guard = and_guard(parent_guard.cloned(), arm_guard);
        match &arm.type_ref {
            None => continue,
            Some(FieldType::Block(b)) => {
                flatten(
                    &b.items,
                    out,
                    resolved,
                    bind,
                    arm_guard.as_ref(),
                    field_types,
                    parent,
                    true,
                )?;
            }
            Some(FieldType::Ref(r)) => {
                if is_wire(&r.name) || is_opaque(&r.name) {
                    continue;
                }
                expand_encoding(r, resolved, out, arm_guard.as_ref(), field_types, parent)?;
            }
            Some(FieldType::Match(_)) => {}
        }
    }
    let Some(default) = mt.arms.iter().find(|a| is_default_arm(a)) else {
        return Ok(());
    };
    let Some(default_type) = &default.type_ref else {
        return Ok(());
    };
    if let FieldType::Ref(r) = default_type
        && r.name == "nbt_nothing"
    {
        return Ok(());
    }
    let mut default_guard: Option<Expr> = parent_guard.cloned();
    for t in &explicit {
        let part = Expr::Binop(
            "!=".into(),
            Box::new(Expr::Ident(tag.clone())),
            Box::new(Expr::Int(*t)),
        );
        default_guard = and_guard(default_guard, Some(part));
    }
    match default_type {
        FieldType::Block(b) => {
            flatten(
                &b.items,
                out,
                resolved,
                bind,
                default_guard.as_ref(),
                field_types,
                "",
                true,
            )?;
        }
        FieldType::Ref(r) => {
            expand_encoding(r, resolved, out, default_guard.as_ref(), field_types, "")?;
        }
        FieldType::Match(_) => {}
    }
    Ok(())
}

/// Flatten a packet body into wire-order fields with conditional guards (`_flatten`).
/// `expand_payloads` is false inside a repeat element, where a payload encoding stays
/// one framed field: flattening two Slots in one element would collide their field
/// names and merge their guards.
#[allow(clippy::too_many_arguments)]
fn flatten(
    items: &[BlockItem],
    out: &mut Vec<PacketShape>,
    resolved: Option<&ResolvedSchema>,
    bind: Option<&OrderedMap<i128>>,
    parent_guard: Option<&Expr>,
    field_types: &mut OrderedMap<String>,
    parent: &str,
    expand_payloads: bool,
) -> BpsResult<()> {
    for item in items {
        match item {
            BlockItem::Group { cond, items: inner } => {
                let guard = and_guard(parent_guard.cloned(), cond.clone());
                flatten(
                    inner,
                    out,
                    resolved,
                    bind,
                    guard.as_ref(),
                    field_types,
                    parent,
                    expand_payloads,
                )?;
            }
            BlockItem::MatchStmt(mt) => {
                flatten_match(mt, out, resolved, bind, parent_guard, field_types, parent)?;
            }
            BlockItem::Spread { name, .. } => {
                if let Some(resolved) = resolved
                    && let Some(enc) = resolved.encodings.get(name)
                    && let EncBody::Block(b) = &enc.body
                {
                    flatten(
                        &b.items,
                        out,
                        Some(resolved),
                        bind,
                        parent_guard,
                        field_types,
                        parent,
                        expand_payloads,
                    )?;
                }
            }
            BlockItem::Field(wf) => {
                let guard = and_guard(parent_guard.cloned(), wf.if_expr.clone());
                append_field(
                    out,
                    wf,
                    resolved,
                    bind,
                    guard.as_ref(),
                    field_types,
                    parent,
                    expand_payloads,
                )?;
            }
            BlockItem::Computed { .. } | BlockItem::Require(_) => {}
        }
    }
    Ok(())
}

/// `ConnectionSide` for a route's direction (`_connection_side_rust`).
fn connection_side_rust(direction: &str) -> String {
    if direction == "serverbound" {
        "ConnectionSide::Client".to_string()
    } else {
        "ConnectionSide::Server".to_string()
    }
}

/// `ConnectionState` for a route's state (`_connection_state_rust`).
fn connection_state_rust(state: &str) -> String {
    match state {
        "handshake" => "ConnectionState::Handshake".to_string(),
        "status" => "ConnectionState::Status".to_string(),
        "login" => "ConnectionState::Login".to_string(),
        "configuration" => "ConnectionState::Configuration".to_string(),
        "play" => "ConnectionState::Play".to_string(),
        _ => format!("ConnectionState::{}", pascal(state)),
    }
}

/// Keep routes whose ids are 0..n-1; `PacketRegistry` is positional (`_contiguous_registry_entries`).
///
/// Wiki routes such as `legacy_server_list_ping` at `0xfe` are omitted here
/// (same as `packets_gen` handshaking table).
pub fn contiguous_registry_entries(mut entries: Vec<(RouteDecl, Unit)>) -> Vec<(RouteDecl, Unit)> {
    entries.sort_by_key(|e| e.0.packet_id);
    let mut out = Vec::new();
    for (i, (route, unit)) in entries.into_iter().enumerate() {
        if route.packet_id != i as i128 {
            break;
        }
        out.push((route, unit));
    }
    out
}

/// The packet's flattened fields, plus the Rust type each name maps to (`_packet_fields`).
pub fn packet_fields(
    pkt: &PacketDecl,
    resolved: &ResolvedSchema,
) -> BpsResult<(Vec<PacketShape>, OrderedMap<String>)> {
    let mut fields: Vec<PacketShape> = Vec::new();
    let mut flatten_types: OrderedMap<String> = OrderedMap::new();
    if let Err(err) = flatten(
        &pkt.items,
        &mut fields,
        Some(resolved),
        None,
        None,
        &mut flatten_types,
        "",
        true,
    ) {
        return Err(emit(format!(
            "packet {}: {}",
            py_repr_str(&pkt.name),
            err.message
        )));
    }
    let mut field_types = flatten_types;
    for f in &fields {
        let base = match &f.repeat_mode {
            Some(_) => format!("Vec<{}>", f.rtype),
            None => f.rtype.clone(),
        };
        let mapped = if f.guard.is_some() {
            format!("Option<{base}>")
        } else {
            base
        };
        field_types.insert(&f.ident, mapped.clone());
        field_types.insert(&snake(&f.ident), mapped);
    }
    Ok((fields, field_types))
}

/// The template-independent wire body shared by the standalone and in-tree templates (`_PacketBody`).
#[derive(Debug, Clone)]
pub struct PacketBody {
    pub fields: Vec<PacketShape>,
    pub field_types: OrderedMap<String>,
    pub writes: String,
    pub reads: String,
    pub ctor: String,
    pub init: String,
    pub used_wires: Vec<String>,
    pub needs_fixed_raw_bytes: bool,
    pub rtypes: BTreeSet<String>,
}

/// Whether `word` occurs in `text` delimited by non-word characters (`\b{word}\b`).
fn has_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    text.match_indices(word).any(|(pos, _)| {
        let end = pos + word.len();
        let before = pos == 0 || !is_word(bytes[pos - 1]);
        let after = end >= bytes.len() || !is_word(bytes[end]);
        before && after
    })
}

/// How an unframed payload field is carried on the wire (`_payload_mode`).
///
/// `scan` is a self-delimiting payload (NBT, Slot, an id set): it is written raw
/// and read by scanning its length, exactly like the reference. Anything else
/// unframed reads to end of buffer, which is only correct for the last field; a
/// payload before it is length-prefixed, which the reference's concatenated
/// payloads are not, so it keeps the consistency check but no byte-for-byte pin.
fn payload_mode(
    f: &PacketShape,
    index: usize,
    count: usize,
    resolved: &ResolvedSchema,
) -> BpsResult<&'static str> {
    if frame_walker(f, Some(resolved))?.is_some_and(|w| !w.is_empty()) {
        return Ok("scan");
    }
    if len_field(f).is_some_and(|s| !s.is_empty()) {
        return Ok("len_field");
    }
    let unframed = f.ref_.as_ref().is_some_and(|r| r.name == "rest")
        || (f.opaque && opaque_len(f.ref_.as_ref(), Some(resolved), Some(&f.params)).is_none());
    if !unframed {
        return Ok("plain");
    }
    Ok(if index + 1 == count {
        "to_end"
    } else {
        "prefixed"
    })
}

/// `_frame_expr(f, resolved) or f.wire`.
fn frame_or_wire(f: &PacketShape, resolved: &ResolvedSchema) -> BpsResult<String> {
    Ok(match frame_expr(f, resolved)? {
        Some(s) if !s.is_empty() => s,
        _ => f.wire.clone(),
    })
}

/// Build the wire body for a unit's packet (`_packet_body`).
pub fn packet_body(unit: &Unit, resolved: &ResolvedSchema) -> BpsResult<PacketBody> {
    let (fields, field_types) = packet_fields(&unit.pkt, resolved)?;
    let n = fields.len();

    let mut writes: Vec<String> = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        if f.repeat_mode.is_some() {
            let opt_vec = f.guard.is_some();
            if let Some(g) = &f.guard {
                let cond = rust_cond(g, &field_types, "write")?;
                let inner = repeat_write(
                    f,
                    resolved,
                    "        ",
                    "value",
                    false,
                    Some(fields.as_slice()),
                    opt_vec,
                    None,
                )?;
                writes.push(format!("        if {cond} {{\n{inner}\n        }}"));
            } else {
                writes.push(repeat_write(
                    f,
                    resolved,
                    "        ",
                    "value",
                    false,
                    Some(fields.as_slice()),
                    false,
                    None,
                )?);
            }
            continue;
        }
        if payload_mode(f, i, n, resolved)? == "prefixed" {
            if let Some(g) = &f.guard {
                let cond = rust_cond(g, &field_types, "write")?;
                writes.push(format!(
                    "        if {cond} {{\n\
                     \x20           if let Some(v) = &value.{id} {{\n\
                     \x20               let slice = v.as_slice();\n\
                     \x20               buffer.write(VAR_INT, slice.len() as i32);\n\
                     \x20               if !slice.is_empty() {{\n\
                     \x20                   buffer.write(fixed_raw_bytes(slice.len() as i32), slice);\n\
                     \x20               }}\n\
                     \x20           }}\n\
                     \x20       }}",
                    id = f.ident
                ));
            } else {
                writes.push(format!(
                    "            let slice = value.{id}.as_slice();\n\
                     \x20           buffer.write(VAR_INT, slice.len() as i32);\n\
                     \x20           if !slice.is_empty() {{\n\
                     \x20               buffer.write(fixed_raw_bytes(slice.len() as i32), slice);\n\
                     \x20           }}",
                    id = f.ident
                ));
            }
            continue;
        }
        let frame = frame_or_wire(f, resolved)?;
        if let Some(g) = &f.guard {
            let cond = rust_cond(g, &field_types, "write")?;
            writes.push(format!(
                "        if {cond} {{\n            if let Some(v) = &value.{id} {{\n                buffer.write({frame}, v.clone());\n            }}\n        }}",
                id = f.ident
            ));
        } else {
            writes.push(format!(
                "        buffer.write({frame}, value.{}.clone());",
                f.ident
            ));
        }
    }

    let mut reads: Vec<String> = Vec::new();
    let mut read_types = field_types.clone();
    for (i, f) in fields.iter().enumerate() {
        let id = &f.ident;
        if f.repeat_mode.is_some() {
            if let Some(g) = &f.guard {
                let cond = rust_cond(g, &read_types, "read")?;
                let inner = repeat_read(
                    f,
                    resolved,
                    "        ",
                    "value",
                    false,
                    Some(fields.as_slice()),
                    true,
                    None,
                )?
                .trim()
                .to_string();
                reads.push(format!(
                    "        let {id} = if {cond} {{\n{inner}\n            Some({id})\n        }} else {{ None }};"
                ));
            } else {
                reads.push(repeat_read(
                    f,
                    resolved,
                    "        ",
                    "value",
                    false,
                    Some(fields.as_slice()),
                    true,
                    None,
                )?);
            }
            read_types.insert(id, format!("Vec<{}>", f.rtype));
            continue;
        }
        if payload_mode(f, i, n, resolved)? == "prefixed" {
            if let Some(g) = &f.guard {
                let cond = rust_cond(g, &read_types, "read")?;
                reads.push(format!(
                    "        let {id} = if {cond} {{\n\
                     \x20           let len = buffer.read(VAR_INT) as usize;\n\
                     \x20           Some(if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len as i32)) }})\n\
                     \x20       }} else {{ None }};"
                ));
            } else {
                reads.push(format!(
                    "        let len = buffer.read(VAR_INT) as usize;\n\
                     \x20       let {id} = if len == 0 {{ Vec::new() }} else {{ buffer.read(fixed_raw_bytes(len as i32)) }};"
                ));
            }
            let t = if f.guard.is_some() {
                format!("Option<{}>", f.rtype)
            } else {
                f.rtype.clone()
            };
            read_types.insert(id, t);
            continue;
        }
        if len_field(f).is_some_and(|s| !s.is_empty()) {
            let body = len_read(id, &len_value(f, &read_types), "            ").join("\n");
            if let Some(g) = &f.guard {
                let cond = rust_cond(g, &read_types, "read")?;
                reads.push(format!(
                    "        let {id} = if {cond} {{\n{body}\n            Some({id})\n        }} else {{ None }};"
                ));
                read_types.insert(id, format!("Option<{}>", f.rtype));
            } else {
                reads.push(body);
                read_types.insert(id, f.rtype.clone());
            }
            continue;
        }
        let walker = frame_walker(f, Some(resolved))?.filter(|w| !w.is_empty());
        if let Some(walker) = walker {
            if let Some(g) = &f.guard {
                let cond = rust_cond(g, &read_types, "read")?;
                let inner = scan_read(id, &walker, "            ").join("\n");
                reads.push(format!(
                    "        let {id} = if {cond} {{\n{inner}\n            Some({id})\n        }} else {{ None }};"
                ));
                read_types.insert(id, format!("Option<{}>", f.rtype));
            } else {
                reads.extend(scan_read(id, &walker, "        "));
                read_types.insert(id, f.rtype.clone());
            }
            continue;
        }
        let frame = frame_or_wire(f, resolved)?;
        if let Some(g) = &f.guard {
            let cond = rust_cond(g, &read_types, "read")?;
            reads.push(format!(
                "        let {id} = if {cond} {{ Some(buffer.read({frame})) }} else {{ None }};"
            ));
            read_types.insert(id, format!("Option<{}>", f.rtype));
            continue;
        }
        reads.push(format!("        let {id} = buffer.read({frame});"));
        read_types.insert(id, f.rtype.clone());
    }

    let ctor_type = |f: &PacketShape| {
        let base = match &f.repeat_mode {
            Some(_) => format!("Vec<{}>", f.rtype),
            None => f.rtype.clone(),
        };
        if f.guard.is_some() {
            format!("Option<{base}>")
        } else {
            base
        }
    };
    let ctor = if fields.is_empty() {
        String::new()
    } else {
        fields
            .iter()
            .map(|f| format!("{}: {}", f.ident, ctor_type(f)))
            .collect::<Vec<_>>()
            .join(",\n        ")
    };
    let init = if fields.is_empty() {
        String::new()
    } else {
        fields
            .iter()
            .map(|f| f.ident.clone())
            .collect::<Vec<_>>()
            .join(",\n            ")
    };

    let writes = {
        let joined = writes.join("\n");
        if joined.is_empty() {
            "        let _ = (buffer, value);".to_string()
        } else {
            joined
        }
    };
    let reads = {
        let joined = reads.join("\n");
        if joined.is_empty() {
            "        let _ = buffer;".to_string()
        } else {
            joined
        }
    };

    /// (wire constants, domain types) needed by these fields and their elements.
    fn collect(items: &[PacketShape], wires: &mut BTreeSet<String>, rtypes: &mut BTreeSet<String>) {
        for f in items {
            if f.repeat_mode.is_some() {
                collect(&f.element, wires, rtypes);
                continue;
            }
            wires.insert(f.wire.clone());
            rtypes.insert(f.rtype.clone());
        }
    }
    let mut collected: BTreeSet<String> = BTreeSet::new();
    let mut rtypes: BTreeSet<String> = BTreeSet::new();
    collect(&fields, &mut collected, &mut rtypes);

    // Import exactly the type constants the generated body references; a heuristic
    // over field shapes over-imports and trips `unused_imports`.
    let body_text = format!("{writes}\n{reads}");
    let mut candidates = collected;
    for w in ["VAR_INT", "RAW_BYTES", "FIXED_BYTES"] {
        candidates.insert(w.to_string());
    }
    let mut used_wires: BTreeSet<String> = candidates
        .into_iter()
        .filter(|w| has_word(&body_text, w))
        .collect();
    for (i, f) in fields.iter().enumerate() {
        if payload_mode(f, i, n, resolved)? == "prefixed" {
            used_wires.insert("VAR_INT".to_string());
            break;
        }
    }
    let needs_fixed_raw_bytes = body_text.contains("fixed_raw_bytes(");
    Ok(PacketBody {
        fields,
        field_types,
        writes,
        reads,
        ctor,
        init,
        used_wires: used_wires.into_iter().collect(),
        needs_fixed_raw_bytes,
        rtypes,
    })
}

/// Whether any field (recursively, through repeat elements) needs a hand-written `Default`.
fn needs_default_impl(items: &[PacketShape]) -> bool {
    items.iter().any(|f| {
        f.guard.is_some()
            || f.repeat_mode.is_some()
            || f.opaque
            || f.wire == "FIXED_BYTES"
            || f.rtype == "Uuid"
            || f.rtype == "CoordVec"
            || needs_default_impl(&f.element)
    })
}

/// The Rust source for one packet's serializer module (`_emit_packet`).
pub fn emit_packet(unit: &Unit, resolved: &ResolvedSchema) -> BpsResult<String> {
    let pkt = &unit.pkt;
    let body = packet_body(unit, resolved)?;
    let fields = &body.fields;
    let field_types = &body.field_types;
    let name = unit.cls.name.clone();
    let ser = format!("{}Serializer", name.strip_suffix("Packet").unwrap_or(&name));

    let mut struct_lines: Vec<String> = Vec::new();
    for f in fields {
        let base = match &f.repeat_mode {
            Some(_) => format!("Vec<{}>", f.rtype),
            None => f.rtype.clone(),
        };
        let rtype = if f.guard.is_some() {
            format!("Option<{base}>")
        } else {
            base
        };
        struct_lines.push(format!("    /// Java: `{}`.", f.java));
        struct_lines.push(format!("    pub {}: {},", f.ident, rtype));
    }
    if let Some(last) = struct_lines.last_mut()
        && last.ends_with(',')
    {
        last.pop();
    }
    let field_decls = if struct_lines.is_empty() {
        "    // no wire fields".to_string()
    } else {
        struct_lines.join("\n")
    };

    let mut imports = String::new();
    if !body.used_wires.is_empty() {
        imports.push_str(&format!(
            "use rstom::network::types::{{{}}};\n",
            body.used_wires.join(", ")
        ));
    }
    if body.needs_fixed_raw_bytes {
        imports.push_str("use rstom::network::types::fixed_raw_bytes;\n");
    }
    // A hand-written Default is needed whenever a field's zero value is not the
    // derive's: rstom's Uuid has no Default, and an opaque byte frame must be
    // exactly its declared length or the write path panics.
    let needs_default = needs_default_impl(fields);
    let big_tuple = fields.iter().any(|f| {
        f.repeat_mode.is_some() && f.rtype.starts_with('(') && f.rtype.matches(',').count() >= 11
    });
    let derive = match (big_tuple, needs_default) {
        (true, true) => "#[derive(Clone)]",
        (true, false) => "#[derive(Clone, Default)]",
        (false, true) => "#[derive(Clone, Debug, PartialEq)]",
        (false, false) => "#[derive(Clone, Debug, Default, PartialEq)]",
    };
    if body.rtypes.contains("Uuid") {
        imports.push_str("use rstom::network::uuid::Uuid;\n");
    }

    let mut default_env: Env = OrderedMap::new();
    let mut zero: Vec<String> = Vec::new();
    for f in fields {
        let frame = frame_expr(f, resolved)?;
        let (v, py_v): (String, Value) = if f.repeat_mode.is_some() {
            ("Vec::new()".to_string(), Value::list(Vec::new()))
        } else if f.opaque || frame.as_deref().is_some_and(|s| !s.is_empty()) {
            let declared = opaque_len(f.ref_.as_ref(), Some(resolved), Some(&f.params));
            let self_delim = match &f.ref_ {
                Some(r) if frame_walker(f, Some(resolved))?.is_some_and(|w| !w.is_empty()) => {
                    frame_default(&r.name, Some(resolved))?
                }
                _ => None,
            };
            match (declared, self_delim) {
                (Some(d), _) if !d.is_empty() => {
                    let n: usize = d
                        .parse()
                        .map_err(|_| emit(format!("bad declared length {d}")))?;
                    (format!("vec![0u8; {d}]"), Value::bytes(vec![0u8; n]))
                }
                (_, Some(sd)) => (rust_bytes(&sd), Value::bytes(sd)),
                _ => ("Vec::new()".to_string(), Value::bytes(Vec::new())),
            }
        } else {
            let v = default_value(&f.rtype);
            let py_v = if ["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"]
                .contains(&f.rtype.as_str())
            {
                Value::Int(0)
            } else if f.rtype == "bool" {
                Value::Bool(false)
            } else if f.rtype == "f32" || f.rtype == "f64" {
                Value::Float(0.0)
            } else {
                Value::str("")
            };
            if f.ident == "id" && f.rtype == "i32" {
                ("1".to_string(), Value::Int(1))
            } else if f.ident == "particle_type" && f.rtype == "i32" {
                ("300".to_string(), Value::Int(300))
            } else {
                (v, py_v)
            }
        };
        let active = match &f.guard {
            None => true,
            Some(g) => guard_holds(Some(g), &default_env, field_types)?,
        };
        if f.guard.is_some() && !active {
            zero.push("None".to_string());
        } else if f.guard.is_some() {
            zero.push(format!("Some({v})"));
        } else {
            zero.push(v);
        }
        if active {
            default_env.insert(&f.ident, py_v.clone());
            default_env.insert(&snake(&f.java), py_v);
        }
    }
    let default_body = if fields.is_empty() {
        "        Self\n".to_string()
    } else {
        format!("        Self::new({})\n", zero.join(", "))
    };
    let default_impl = if needs_default {
        format!(
            "impl Default for {name} {{\n    fn default() -> Self {{\n{default_body}    }}\n}}\n\n"
        )
    } else {
        String::new()
    };
    if body.rtypes.contains("CoordVec") {
        imports.push_str("use rstom::coordinate::Vec as CoordVec;\n");
    }

    let java_class = &unit.cls.java;
    let routes = unit
        .routes
        .iter()
        .map(|r| format!("{}.{} {:#04x}", r.state, r.direction, r.packet_id))
        .collect::<Vec<_>>()
        .join(", ");
    let (marker_import, marker_impl) = if unit.cls.group == "client" {
        (
            "use rstom::network::packet::client::client_packet::{ClientPacket, ClientPacketGroup};\n",
            format!(
                "\nimpl ClientPacket for {name} {{}}\nimpl ClientPacketGroup for {name} {{}}\n"
            ),
        )
    } else {
        (
            "use rstom::network::packet::server::server_packet::{ServerPacket, ServerPacketGroup};\n",
            format!(
                "\nimpl ServerPacket for {name} {{}}\nimpl ServerPacketGroup for {name} {{}}\n"
            ),
        )
    };

    let writes = &body.writes;
    let reads = &body.reads;
    let ctor = &body.ctor;
    let init = &body.init;
    let pkt_name = &pkt.name;
    Ok(format!(
        "//! Generated by `bml emit rust` from a resolved BPS schema.\n\
//!\n\
//! Wire layout transcribed from `schemas/minecraft_26_3*.bps`: BPS packet\n\
//! `{pkt_name}`, routed {routes}.\n\
\n\
use std::sync::Arc;\n\
\n\
{imports}{marker_import}use rstom::network::buffer::NetworkBuffer;\n\
use rstom::network::packet::packet::Packet;\n\
use rstom::network::packet::serializer::{{PacketSerializer, PacketValue, downcast}};\n\
\n\
/// The wire fields of BPS packet `{pkt_name}`, in wire order.\n\
{derive}\n\
pub struct {name} {{\n\
{field_decls}\n\
}}\n\
\n\
{default_impl}impl {name} {{\n\
    /// Takes the wire fields in order.\n\
    #[must_use]\n\
    pub fn new(\n\
        {ctor}\n\
    ) -> Self {{\n\
        Self {{\n\
            {init}\n\
        }}\n\
    }}\n\
\n\
    /// The schema's serializer.\n\
    #[must_use]\n\
    pub fn serializer() -> Arc<dyn PacketSerializer> {{\n\
        Arc::new({ser})\n\
    }}\n\
}}\n\
\n\
impl Packet for {name} {{\n\
    const CLASS_NAME: &'static str = \"{java_class}\";\n\
}}\n\
{marker_impl}\n\
/// The serializer of `{name}`, generated from the BPS schema.\n\
pub struct {ser};\n\
\n\
impl PacketSerializer for {ser} {{\n\
    fn write(&self, buffer: &mut NetworkBuffer, value: &PacketValue) {{\n\
        let value = downcast::<{name}>(value);\n\
{writes}\n\
    }}\n\
\n\
    fn read(&self, buffer: &mut NetworkBuffer) -> PacketValue {{\n\
{reads}\n\
        Box::new({name}::new(\n\
            {init}\n\
        ))\n\
    }}\n\
}}\n"
    ))
}

/// Rust source for a string literal (`_rust_str`).
fn rust_str(value: &str) -> String {
    let body = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{body}\".to_string()")
}

/// Rust source for a byte literal (`_rust_bytes`).
pub fn rust_bytes(data: &[u8]) -> String {
    let hex: String = data.iter().map(|b| format!("\\x{b:02x}")).collect();
    format!("b\"{hex}\".to_vec()")
}

/// Rust source for an integer of the given width (`_rust_int`).
fn rust_int(value: i128, bits: u32, signed: bool) -> Option<String> {
    let mut v = value;
    if signed {
        let lo = -(1i128 << (bits - 1));
        let hi = (1i128 << (bits - 1)) - 1;
        if !(lo <= v && v <= hi) {
            v = (v - lo).rem_euclid(1i128 << bits) + lo;
        }
    } else if !(0 <= v && v < (1i128 << bits)) {
        return None;
    }
    Some(v.to_string())
}

/// Split a packed position into signed x, y, z components (`_position_components`).
fn position_components(raw: i128) -> (i128, i128, i128) {
    let ext = |v: i128, bits: u32| {
        (v + (1i128 << (bits - 1))).rem_euclid(1i128 << bits) - (1i128 << (bits - 1))
    };
    (
        ext(raw >> 38, 26),
        ext(raw.rem_euclid(4096), 12),
        ext((raw >> 12).rem_euclid(67108864), 26),
    )
}

/// `(bits, signed)` for the primitive rtypes (`_PRIM_RTYPES`; u8 is signed in the reference).
fn prim_rtype(rtype: &str) -> Option<(u32, bool)> {
    match rtype {
        "i8" | "u8" => Some((8, true)),
        "i16" => Some((16, true)),
        "u16" => Some((16, false)),
        "i32" => Some((32, true)),
        "u32" => Some((32, false)),
        "i64" => Some((64, true)),
        "u64" => Some((64, false)),
        _ => None,
    }
}

/// A repeat element's literal: the element type is never an Option (`_rust_element`).
fn rust_element(
    e: &PacketShape,
    value: &Value,
    resolved: &ResolvedSchema,
    env: Option<&Env>,
) -> BpsResult<Option<String>> {
    if e.guard.is_some() && matches!(value, Value::Null) {
        return Ok(None);
    }
    rust_inner(e, value, resolved, env)
}

/// Rust source for one field value, or None when it cannot be rendered (`_rust_literal`).
fn rust_literal(
    f: &PacketShape,
    value: &Value,
    resolved: &ResolvedSchema,
    env: Option<&Env>,
) -> BpsResult<Option<String>> {
    if f.guard.is_some() && matches!(value, Value::Null) {
        return Ok(Some("None".to_string()));
    }
    let Some(lit) = rust_inner(f, value, resolved, env)? else {
        return Ok(None);
    };
    Ok(Some(if f.guard.is_some() {
        format!("Some({lit})")
    } else {
        lit
    }))
}

/// The `T` in `Option<T>` / the bare value: no optionality wrapper (`_rust_inner`).
fn rust_inner(
    f: &PacketShape,
    value: &Value,
    resolved: &ResolvedSchema,
    env: Option<&Env>,
) -> BpsResult<Option<String>> {
    if f.repeat_mode.is_some() {
        let Value::List(items) = value else {
            return Ok(None);
        };
        if items.is_empty() {
            return Ok(Some("Vec::new()".to_string()));
        }
        let mut parts: Vec<String> = Vec::new();
        for item in items.iter() {
            let lit = if f.element.len() == 1 && f.element[0].ident == "_item" {
                rust_element(&f.element[0], item, resolved, env)?
            } else if matches!(item, Value::Map(_)) {
                let mut lits: Vec<String> = Vec::new();
                for e in &f.element {
                    let ev = py_field_value(e, item);
                    match rust_literal(e, &ev, resolved, None)? {
                        Some(el) => lits.push(el),
                        None => return Ok(None),
                    }
                }
                Some(format!("({})", lits.join(", ")))
            } else {
                None
            };
            match lit {
                Some(l) => parts.push(l),
                None => return Ok(None),
            }
        }
        return Ok(Some(format!("vec![{}]", parts.join(", "))));
    }

    let frame = frame_expr(f, resolved)?;
    if f.opaque || frame.as_deref().is_some_and(|s| !s.is_empty()) || f.rtype == "Vec<u8>" {
        let mut data: Vec<u8> = match (&f.ref_, value) {
            (Some(r), _) => {
                // Through the interpreter: a `bytes(<ident>)` frame is zero-padded to
                // the length the sibling field declares, which a raw value cannot be.
                let mut scope: Env = env.cloned().unwrap_or_else(OrderedMap::new);
                for (k, v) in f.params.iter() {
                    scope.insert(k, Value::Int(*v));
                }
                match encode_bare_value(r, value, resolved, &scope) {
                    Some(d) => d,
                    None => match value {
                        Value::Bytes(b) => b.to_vec(),
                        Value::Str(s) => s.as_bytes().to_vec(),
                        _ => return Ok(None),
                    },
                }
            }
            (None, Value::Bytes(b)) => b.to_vec(),
            (None, Value::Str(s)) => s.as_bytes().to_vec(),
            _ => return Ok(None),
        };
        // A declared frame is exactly its length: the interpreter zero-pads a
        // short value and rejects a long one, and `fixed_raw_bytes` panics on
        // any other length, so a literal must match that contract.
        if let Some(n) = opaque_len(f.ref_.as_ref(), Some(resolved), Some(&f.params)) {
            let n: usize = n
                .parse()
                .map_err(|_| emit(format!("bad declared length {n}")))?;
            if data.len() < n {
                data.resize(n, 0);
            } else if data.len() > n {
                return Ok(None);
            }
        }
        return Ok(Some(rust_bytes(&data)));
    }

    if let Value::Bool(b) = value {
        return Ok(Some(if *b { "true" } else { "false" }.to_string()));
    }
    if f.rtype == "String" {
        return Ok(match value {
            Value::Str(s) => Some(rust_str(s)),
            _ => None,
        });
    }
    if f.rtype == "f32" || f.rtype == "f64" {
        return Ok(match value {
            Value::Float(x) => Some(crate::json::float_repr(*x)),
            _ => None,
        });
    }
    if f.rtype == "Uuid" {
        let (Some(msb), Some(lsb)) = (value.get("msb"), value.get("lsb")) else {
            return Ok(None);
        };
        let mask: i128 = (1i128 << 64) - 1;
        let msb = (value_int(msb)? & mask) as u64;
        let lsb = (value_int(lsb)? & mask) as u64;
        return Ok(Some(format!("Uuid::from_bits({msb}, {lsb})")));
    }
    if f.rtype == "CoordVec" {
        let Some(raw) = value.get("raw") else {
            return Ok(None);
        };
        let (x, y, z) = position_components(value_int(raw)?);
        return Ok(Some(format!(
            "CoordVec::new({}, {}, {})",
            crate::json::float_repr(x as f64),
            crate::json::float_repr(y as f64),
            crate::json::float_repr(z as f64)
        )));
    }
    if let (Some((bits, signed)), Value::Int(v)) = (prim_rtype(&f.rtype), value) {
        return Ok(rust_int(*v, bits, signed));
    }
    Ok(None)
}

/// The reference value for a field, following its flattened parent path (`_py_field_value`).
/// Absent keys come back as `Value::Null`, which the literal renderers treat as None.
fn py_field_value(f: &PacketShape, value: &Value) -> Value {
    let mut current = value;
    if !f.parent.is_empty() {
        for key in f.parent.split('.') {
            match current.get(key) {
                Some(v) => current = v,
                None => return Value::Null,
            }
        }
        if !matches!(current, Value::Map(_)) {
            return Value::Null;
        }
    }
    for key in [f.ident.as_str(), f.ident.trim_end_matches('_')] {
        if let Some(v) = current.get(key) {
            return v.clone();
        }
    }
    Value::Null
}

/// Python-encoded bytes for one field value, or None if it cannot encode (`_encode_bare`).
fn encode_bare_value(
    r: &TypeRef,
    value: &Value,
    resolved: &ResolvedSchema,
    env: &Env,
) -> Option<Vec<u8>> {
    crate::interp::engine::encode_bare(r, value, resolved, env)
        .ok()
        .flatten()
}

/// `_parity_args`: constructor literals + the Python-encoded reference bytes.
/// `None` when the packet's value cannot be rendered (or any step fails); the
/// caller falls back to a default-value self-roundtrip.
pub fn parity_args(
    pkt: &PacketDecl,
    fields: &[PacketShape],
    resolved: &ResolvedSchema,
) -> BpsResult<Option<(Vec<String>, Vec<u8>)>> {
    let attempt = || -> BpsResult<Option<(Vec<String>, Vec<u8>)>> {
        for (i, f) in fields.iter().enumerate() {
            if f.repeat_mode.is_some() {
                let mut any = false;
                for e in &f.element {
                    if elem_needs_prefix(e, resolved)? {
                        any = true;
                        break;
                    }
                }
                if any {
                    return Ok(None);
                }
            }
            if payload_mode(f, i, fields.len(), resolved)? == "prefixed" {
                return Ok(None);
            }
        }
        let py = synthesize_packet(resolved, pkt)?;
        let expected = encode_packet(resolved, &pkt.name, &py)?;
        let py_value = Value::map(py.clone());
        let mut lits = Vec::new();
        for f in fields {
            match rust_literal(f, &py_field_value(f, &py_value), resolved, Some(&py))? {
                Some(lit) => lits.push(lit),
                None => return Ok(None),
            }
        }
        Ok(Some((lits, expected)))
    };
    // Python wraps everything after `_packet_fields` in a broad `except`.
    Ok(attempt().unwrap_or(None))
}

/// `_emit_roundtrip_test`: one test per struct.
fn emit_roundtrip_test(unit: &Unit, resolved: &ResolvedSchema) -> BpsResult<String> {
    let cls = &unit.cls;
    let name = &cls.name;
    let (fields, _types) = packet_fields(&unit.pkt, resolved)?;
    let parity = parity_args(&unit.pkt, &fields, resolved)?;
    let (ctor, pin, use_block) = if let Some((literals, pin_bytes)) = parity {
        let ctor = format!("{}::new({})", name, literals.join(", "));
        let pin = format!(
            "    // Pinned by the Python interpreter's encoding of the same\n\
             \x20   // synthetic value (`bps/synth.py`); this asserts the Rust\n\
             \x20   // serializer produces identical bytes, not just inverse ones.\n\
             \x20   assert_eq!(bytes, {});\n",
            rust_bytes(&pin_bytes)
        );
        let mut use_block = String::new();
        for (us, marker) in [
            ("use rstom::coordinate::Vec as CoordVec;", "CoordVec::new"),
            ("use rstom::network::uuid::Uuid;", "Uuid::from_bits"),
        ] {
            if ctor.contains(marker) {
                use_block.push_str(us);
                use_block.push('\n');
            }
        }
        use_block.push('\n');
        (ctor, pin, use_block)
    } else {
        (
            format!("{}::default()", name),
            "    // No pinned reference for this packet's synthetic value; the\n\
             \x20   // encode/decode pair is only checked for consistency.\n"
                .to_string(),
            String::new(),
        )
    };
    Ok(format!(
        "//! Generated by `bml emit rust`: round-trip for `{name}`.\n\
         \n\
         use bps_rstom::{group}::{package}::{stem}::{name};\n\
         \n\
         use rstom::network::buffer::NetworkBuffer;\n\
         use rstom::network::packet::serializer::PacketValue;\n\
         {use_block}\
         #[test]\n\
         fn round_trips() {{\n\
         \x20   let packet = {ctor};\n\
         \x20   let bytes = NetworkBuffer::make_array(|buffer| {{\n\
         \x20       {name}::serializer().write(buffer, &(Box::new(packet.clone()) as PacketValue))\n\
         \x20   }});\n\
         {pin}\
         \x20   let mut buffer = NetworkBuffer::wrap(bytes.clone(), 0, bytes.len() as i64);\n\
         \x20   let decoded = {name}::serializer().read(&mut buffer);\n\
         \x20   let bytes2 = NetworkBuffer::make_array(|buffer| {{\n\
         \x20       {name}::serializer().write(buffer, &decoded)\n\
         \x20   }});\n\
         \x20   assert_eq!(bytes, bytes2);\n\
         }}\n",
        group = cls.group,
        package = cls.package,
        stem = cls.stem,
    ))
}

/// `_emit_package_mod`: a package's `mod.rs`.
fn emit_package_mod(group: &str, package: &str, units: &[&Unit]) -> String {
    let mods: Vec<String> = units
        .iter()
        .map(|u| format!("pub mod {};", u.cls.stem))
        .collect();
    let rows: Vec<String> = units
        .iter()
        .map(|u| {
            let c = &u.cls;
            format!(
                "        ({stem}::{name}::CLASS_NAME, {stem}::{name}::serializer()),",
                stem = c.stem,
                name = c.name
            )
        })
        .collect();
    format!(
        "//! Generated by `bml emit rust`: port of `{PACKAGE_PREFIX}.{group}.{package}`.\n\
         \n\
         {mods}\n\
         \n\
         /// The serializers of this package, keyed by Java class name (consumed by\n\
         /// [`crate::ported`]).\n\
         #[must_use]\n\
         pub fn list() -> Vec<(&'static str, rstom::network::packet::serializer::Serializer)> {{\n\
         \x20   use rstom::network::packet::packet::Packet as _;\n\
         \x20   vec![\n\
         {rows}\n\
         \x20   ]\n\
         }}\n",
        mods = mods.join("\n"),
        rows = rows.join("\n"),
        PACKAGE_PREFIX = PACKAGE_PREFIX,
    )
}

/// `_emit_group_mod`.
fn emit_group_mod(group: &str, packages: &[String]) -> String {
    let sender = if group == "client" {
        "the client sends (serverbound)"
    } else {
        "the server sends (clientbound)"
    };
    let mods: Vec<String> = packages.iter().map(|p| format!("pub mod {p};")).collect();
    format!(
        "//! Generated by `bml emit rust`: the packets {sender}, by connection state.\n\n{}\n",
        mods.join("\n")
    )
}

/// `_registry_entries`: the ten id tables, keyed `client_play`.
pub fn registry_entries(units: &[Unit]) -> OrderedMap<Vec<(RouteDecl, Unit)>> {
    let mut out: OrderedMap<Vec<(RouteDecl, Unit)>> = OrderedMap::new();
    for group in GROUPS {
        for state in STATES {
            out.insert(&format!("{group}_{state}"), Vec::new());
        }
    }
    for unit in units {
        for route in &unit.routes {
            let key = format!("{}_{}", unit.cls.group, package_of(&route.state));
            // Every key is built above from the same group/state tables.
            let entries = out
                .get_mut(&key)
                .expect("registry key is one of the ten tables");
            entries.push((route.clone(), unit.clone()));
        }
    }
    let mut result: OrderedMap<Vec<(RouteDecl, Unit)>> = OrderedMap::new();
    for (key, entries) in out.iter() {
        result.insert(key, contiguous_registry_entries(entries.clone()));
    }
    result
}

/// `_emit_registry_fn`: `key` is e.g. `client_play` (group_state).
fn emit_registry_fn(key: &str, entries: &[(RouteDecl, Unit)]) -> String {
    let (group, state) = key.split_once('_').unwrap_or((key, ""));
    let fn_name = format!("{group}_{state}_registry");
    let static_name = format!("{}_{}", group.to_uppercase(), state.to_uppercase());
    let conn_state = connection_state_rust(state);
    let conn_side = connection_side_rust(if group == "client" {
        "serverbound"
    } else {
        "clientbound"
    });
    let uses: BTreeSet<String> = entries
        .iter()
        .map(|(_, u)| {
            format!(
                "use crate::{}::{}::{}::{};",
                u.cls.group, u.cls.package, u.cls.stem, u.cls.name
            )
        })
        .collect();
    let entry_import = if entries.is_empty() {
        String::new()
    } else {
        "use rstom::network::packet::serializer::PacketEntry;\n".to_string()
    };
    let entry_lines: Vec<String> = entries
        .iter()
        .map(|(_, u)| {
            format!(
                "                PacketEntry::ported::<{}>(\"{}\", {}::serializer()),",
                u.cls.name, u.cls.java, u.cls.name
            )
        })
        .collect();
    let uses: Vec<String> = uses.into_iter().collect();
    format!(
        "//! Generated by `bml emit rust`: `{fn_name}`.\n\
         \n\
         use std::sync::OnceLock;\n\
         \n\
         use super::resolved_registry;\n\
         use rstom::network::connection_state::ConnectionState;\n\
         use rstom::network::packet::packet_registry::{{ConnectionSide, PacketRegistry}};\n\
         {entry_import}\n\
         {uses}\n\
         \n\
         static {static_name}: OnceLock<PacketRegistry> = OnceLock::new();\n\
         \n\
         #[must_use]\n\
         pub fn {fn_name}() -> &'static PacketRegistry {{\n\
         \x20   {static_name}.get_or_init(|| {{\n\
         \x20       resolved_registry(\n\
         \x20           {conn_state},\n\
         \x20           {conn_side},\n\
         \x20           vec![\n\
         {entry_lines}\n\
         \x20           ],\n\
         \x20       )\n\
         \x20   }})\n\
         }}\n",
        uses = uses.join("\n"),
        entry_lines = entry_lines.join("\n"),
    )
}

/// `_emit_registry_tables_test`.
fn emit_registry_tables_test(by_reg: &OrderedMap<Vec<(RouteDecl, Unit)>>) -> String {
    let mut rows = Vec::new();
    let mut arms = Vec::new();
    for side in ["client", "server"] {
        for state in STATES {
            let names: Vec<String> = by_reg
                .get(&format!("{side}_{state}"))
                .map(|v| {
                    v.iter()
                        .map(|(_, u)| format!("\"{}\"", u.cls.name))
                        .collect()
                })
                .unwrap_or_default();
            rows.push(format!(
                "    (\"{}\", \"{}\", &[{}]),",
                side.to_uppercase(),
                state.to_uppercase(),
                names.join(", ")
            ));
            arms.push(format!(
                "        (\"{}\", \"{}\") => packet_vanilla::{side}_{state}_registry(),",
                side.to_uppercase(),
                state.to_uppercase()
            ));
        }
    }
    format!(
        "//! Generated by `bml emit rust`: each id table lists the classes Minestom registers, in id order.\n\
         \n\
         use bps_rstom::PacketRegistry;\n\
         use bps_rstom::packet_vanilla;\n\
         \n\
         /// `(side, state, simple class names in id order)`.\n\
         const TABLES: &[(&str, &str, &[&str])] = &[\n\
         {rows}\n\
         ];\n\
         \n\
         fn registry_for(side: &str, state: &str) -> &'static PacketRegistry {{\n\
         \x20   match (side, state) {{\n\
         {arms}\n\
         \x20       (side, state) => panic!(\"unknown registry: {{side}} {{state}}\"),\n\
         \x20   }}\n\
         }}\n\
         \n\
         #[test]\n\
         fn registry_tables_list_the_classes_in_id_order() {{\n\
         \x20   assert_eq!(TABLES.len(), 10);\n\
         \x20   for (side, state, want) in TABLES {{\n\
         \x20       let got: Vec<&str> = registry_for(side, state)\n\
         \x20           .iter()\n\
         \x20           .map(|info| info.simple_class_name())\n\
         \x20           .collect();\n\
         \x20       assert_eq!(&got, want, \"{{side}}/{{state}}: packet order or membership drifted\");\n\
         \x20   }}\n\
         }}\n",
        rows = rows.join("\n"),
        arms = arms.join("\n"),
    )
}

/// `_ported_rs`.
fn ported_rs(packages: &[(String, String)]) -> String {
    let extends: Vec<String> = packages
        .iter()
        .map(|(g, p)| format!("    ported.extend(crate::{g}::{p}::list());"))
        .collect();
    format!(
        "//! Generated by `bml emit rust`: ported serializer table.\n\
         \n\
         use rstom::network::packet::serializer::Serializer;\n\
         \n\
         /// Every ported packet serializer, keyed by its Java class name.\n\
         #[must_use]\n\
         pub fn all() -> Vec<(&'static str, Serializer)> {{\n\
         \x20   let mut ported = Vec::new();\n\
         {}\n\
         \x20   ported\n\
         }}\n",
        extends.join("\n")
    )
}

// ---- verbatim Python string constants ----
const VANILLA_BODY: &str = r#"use rstom::network::connection_state::ConnectionState;
use rstom::network::packet::packet_error::PacketError;
use rstom::network::packet::packet_parser::PacketParser;
use rstom::network::packet::packet_registry::{ConnectionSide, PacketRegistry};
use rstom::network::packet::serializer::{PacketEntry, PacketValue};

@TRANSITION_USES@

pub(crate) fn resolved_registry(
    state: ConnectionState,
    side: ConnectionSide,
    entries: Vec<PacketEntry>,
) -> PacketRegistry {
    let ported = crate::ported::all();
    PacketRegistry::registry(
        state,
        side,
        entries
            .into_iter()
            .map(|entry| entry.resolve(&ported))
            .collect(),
    )
}

#[must_use]
pub fn next_client_state(
    packet: &PacketValue,
    current_state: ConnectionState,
) -> Result<ConnectionState, PacketError> {
    if let Some(handshake) = packet.downcast_ref::<ClientHandshakePacket>() {
        return Ok(match handshake.intent {
            1 => ConnectionState::Status,
            2 | 3 => ConnectionState::Login,
            _ => current_state,
        });
    }
    if packet.downcast_ref::<ClientLoginAcknowledgedPacket>().is_some()
        || packet.downcast_ref::<ClientConfigurationAckPacket>().is_some()
    {
        return Ok(ConnectionState::Configuration);
    }
    if packet.downcast_ref::<ClientFinishConfigurationPacket>().is_some() {
        return Ok(ConnectionState::Play);
    }
    Ok(current_state)
}

#[must_use]
pub fn next_server_state(
    packet: &PacketValue,
    current_state: ConnectionState,
) -> Result<ConnectionState, PacketError> {
    if current_state == ConnectionState::Handshake {
        return Err(PacketError::IllegalState(
            "No server Handshake packet exists".to_string(),
        ));
    }
    if packet.downcast_ref::<LoginSuccessPacket>().is_some()
        || packet.downcast_ref::<StartConfigurationPacket>().is_some()
    {
        return Ok(ConnectionState::Configuration);
    }
    if packet.downcast_ref::<FinishConfigurationPacket>().is_some() {
        return Ok(ConnectionState::Play);
    }
    Ok(current_state)
}

pub struct ClientPacketParser;

pub struct ServerPacketParser;

impl PacketParser for ClientPacketParser {
    fn handshake(&self) -> &PacketRegistry {
        client_handshake_registry()
    }
    fn status(&self) -> &PacketRegistry {
        client_status_registry()
    }
    fn login(&self) -> &PacketRegistry {
        client_login_registry()
    }
    fn configuration(&self) -> &PacketRegistry {
        client_configuration_registry()
    }
    fn play(&self) -> &PacketRegistry {
        client_play_registry()
    }
}

impl PacketParser for ServerPacketParser {
    fn handshake(&self) -> &PacketRegistry {
        server_handshake_registry()
    }
    fn status(&self) -> &PacketRegistry {
        server_status_registry()
    }
    fn login(&self) -> &PacketRegistry {
        server_login_registry()
    }
    fn configuration(&self) -> &PacketRegistry {
        server_configuration_registry()
    }
    fn play(&self) -> &PacketRegistry {
        server_play_registry()
    }
}

pub static CLIENT_PACKET_PARSER: ClientPacketParser = ClientPacketParser;
pub static SERVER_PACKET_PARSER: ServerPacketParser = ServerPacketParser;
"#;

/// The hand-written scanner prelude, prefixing the generated walkers.
pub const FRAMES_RS: &str = r#"//! Generated by `bml emit rust`: self-delimiting payload scanners.

/// Bytes of one non-zigzag LEB128 VarInt at the front of `bytes`.
fn varint_len(bytes: &[u8]) -> Option<usize> {
    for (i, byte) in bytes.iter().enumerate().take(5) {
        if byte & 0x80 == 0 {
            return Some(i + 1);
        }
    }
    None
}

/// Value of one VarInt, for the payloads whose shape depends on it.
fn varint_value(bytes: &[u8]) -> Option<i32> {
    let mut result: i32 = 0;
    for (i, byte) in bytes.iter().enumerate().take(5) {
        result |= i32::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some(result);
        }
    }
    None
}

fn skip_u16(bytes: &[u8], pos: &mut usize) -> Option<()> {
    let hi = *bytes.get(*pos)? as usize;
    let lo = *bytes.get(*pos + 1)? as usize;
    *pos += 2 + ((hi << 8) | lo);
    Some(())
}

fn skip_i32(bytes: &[u8], pos: &mut usize) -> Option<i32> {
    let value = i32::from_be_bytes([
        *bytes.get(*pos)?,
        *bytes.get(*pos + 1)?,
        *bytes.get(*pos + 2)?,
        *bytes.get(*pos + 3)?,
    ]);
    *pos += 4;
    Some(value)
}

/// One NBT payload body, addressed by its tag id (compound entries, list
/// items, ...), the shape Java's `readPayload` switches on.
fn skip_nbt_body(tag: u8, bytes: &[u8], pos: &mut usize, depth: u32) -> Option<()> {
    if depth > 512 {
        return None;
    }
    match tag {
        0 => {}
        1 => *pos += 1,
        2 => *pos += 2,
        3 | 5 => *pos += 4,
        4 | 6 => *pos += 8,
        7 => *pos += usize::try_from(skip_i32(bytes, pos)?).ok()?,
        8 => skip_u16(bytes, pos)?,
        9 => {
            let element = *bytes.get(*pos)?;
            *pos += 1;
            for _ in 0..skip_i32(bytes, pos)? {
                skip_nbt_body(element, bytes, pos, depth + 1)?;
            }
        }
        10 => loop {
            let entry_type = *bytes.get(*pos)?;
            *pos += 1;
            if entry_type == 0 {
                break;
            }
            skip_u16(bytes, pos)?;
            skip_nbt_body(entry_type, bytes, pos, depth + 1)?;
        },
        11 => *pos += 4 * usize::try_from(skip_i32(bytes, pos)?).ok()?,
        12 => *pos += 8 * usize::try_from(skip_i32(bytes, pos)?).ok()?,
        _ => return None,
    }
    (*pos <= bytes.len()).then_some(())
}

/// Length of one tag-id-prefixed NBT payload (`nbt`, `text_component`).
#[must_use]
pub fn nbt_tag_len(bytes: &[u8]) -> usize {
    let tag = *bytes.first().unwrap_or(&0);
    let mut pos = 1usize;
    if skip_nbt_body(tag, bytes, &mut pos, 0).is_none() {
        return bytes.len();
    }
    pos.min(bytes.len())
}

/// Length of one Slot payload.
#[must_use]
pub fn slot_len(bytes: &[u8]) -> usize {
    let mut pos = varint_len(bytes).unwrap_or(bytes.len());
    let count = varint_value(bytes).unwrap_or(0);
    if count > 0 {
        for _ in 0..3 {
            let n = varint_len(bytes.get(pos..).unwrap_or(&[])).unwrap_or(0);
            pos += n;
        }
    }
    pos.min(bytes.len())
}
"#;

const LIB_RS: &str = r#"//! Generated by `bml emit rust`: packet payloads, `ported`, and `packet_vanilla`.
//!
//! The module tree, class names and registry functions of `rstom-packets`, with
//! each payload's wire layout generated from the BPS schema instead of ported
//! by hand. A packet's fields are the schema's, not the Java record's.

pub mod client;
pub mod frames;
pub mod packet_vanilla;
pub mod ported;
pub mod server;

pub use rstom::network::packet::{
    packet::Packet,
    packet_error::PacketError,
    packet_registry::{ConnectionSide, PacketInfo, PacketRegistry},
    serializer::{PacketEntry, PacketSerializer, PacketValue, Serializer, downcast},
};
"#;

const STATES: &[&str] = &["handshake", "status", "login", "configuration", "play"];
const GROUPS: &[&str] = &["client", "server"];

/// The connection state machine matches on these Minestom classes, so a schema
/// that routes none of them cannot be emitted.
const TRANSITIONS: &[&str] = &[
    "ClientHandshakePacket",
    "ClientLoginAcknowledgedPacket",
    "ClientConfigurationAckPacket",
    "ClientFinishConfigurationPacket",
    "FinishConfigurationPacket",
    "LoginSuccessPacket",
    "StartConfigurationPacket",
];

/// `_transition_uses`.
fn transition_uses(units: &[Unit]) -> BpsResult<String> {
    let mut uses = Vec::new();
    for name in TRANSITIONS {
        // Python's dict keeps the last unit for a repeated class name.
        let cls = units
            .iter()
            .rev()
            .find(|u| u.cls.name == *name)
            .map(|u| &u.cls)
            .ok_or_else(|| {
                emit(format!(
                    "the connection state machine needs {name}, which no route provides"
                ))
            })?;
        uses.push(format!(
            "use crate::{}::{}::{}::{};",
            cls.group, cls.package, cls.stem, cls.name
        ));
    }
    uses.sort();
    Ok(uses.join("\n"))
}

/// `_parser_methods`: `PacketParser` methods for one side.
fn parser_methods(group: &str) -> String {
    STATES
        .iter()
        .map(|state| {
            format!(
                "    fn {state}(&self) -> &PacketRegistry {{\n        {group}_{state}_registry()\n    }}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `_vanilla_mod_rs`.
fn vanilla_mod_rs(registries: &[String], units: &[Unit]) -> BpsResult<String> {
    let reexport: Vec<String> = registries
        .iter()
        .map(|k| format!("mod {k};\npub use {k}::{k}_registry;"))
        .collect();
    let body = VANILLA_BODY;
    let head = match body.split_once("impl PacketParser for ClientPacketParser {") {
        Some((before, _)) => before,
        None => body,
    };
    let head = head.replace("@TRANSITION_USES@", &transition_uses(units)?);
    let parsers = format!(
        "impl PacketParser for ClientPacketParser {{\n{}\n}}\n\n\
         impl PacketParser for ServerPacketParser {{\n{}\n}}\n\n\
         pub static CLIENT_PACKET_PARSER: ClientPacketParser = ClientPacketParser;\n\
         pub static SERVER_PACKET_PARSER: ServerPacketParser = ServerPacketParser;\n",
        parser_methods("client"),
        parser_methods("server"),
    );
    Ok(format!(
        "//! Generated by `bml emit rust`: `PacketVanilla` id tables and parsers.\n\n{}\n\n{}{}",
        reexport.join("\n"),
        head,
        parsers
    ))
}

/// `_is_generated`: the first line cites the emitter. Files written before the CLI became
/// `bml` cite `bps emit rust`; they stay recognised, so a tree generated back then is
/// regenerated rather than taken for hand-written code.
pub fn is_generated(path: &Path) -> bool {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut line = String::new();
    if std::io::BufReader::new(file).read_line(&mut line).is_err() {
        return false;
    }
    line.contains("`bml emit rust`") || line.contains("`bps emit rust`")
}

/// `_drop_stale`: remove generated files an earlier emit left behind.
fn drop_stale(out_dir: &Path, written: &[PathBuf]) -> BpsResult<()> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
                out.push(p);
            } else {
                out.push(p);
            }
        }
    }
    let src = out_dir.join("src");
    let tests = out_dir.join("tests");
    for root in [&src, &tests] {
        let mut files = Vec::new();
        walk(root, &mut files);
        for path in files {
            if path.extension().and_then(|e| e.to_str()) == Some("rs")
                && !written.contains(&path)
                && is_generated(&path)
            {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    // Deepest directories first, so emptied parents are removed too.
    let mut dirs = Vec::new();
    walk(&src, &mut dirs);
    let mut dirs: Vec<PathBuf> = dirs.into_iter().filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs.iter().rev() {
        let empty = std::fs::read_dir(dir)
            .map(|mut rd| rd.next().is_none())
            .unwrap_or(false);
        if empty {
            let _ = std::fs::remove_dir(dir);
        }
    }
    Ok(())
}

/// `emit_rstom`: emit an rstom-compatible packet crate. Returns the emitted files.
/// `rstom_path` is the default `../rstom` dependency path.
pub fn emit_rstom(
    resolved: &ResolvedSchema,
    out_dir: &Path,
    classes: &ClassTable,
    rstom_path: &str,
) -> BpsResult<Vec<PathBuf>> {
    let lowered: HashSet<String> = WIRE_TYPES
        .iter()
        .map(|(n, _, _)| n.to_string())
        .chain(OPAQUE.iter().map(|s| s.to_string()))
        .chain(SELF_DELIMITING.iter().map(|(n, _)| n.to_string()))
        .collect();
    reject_unlowered_scalars(resolved, &lowered, "rstom")?;
    let units = build_units(resolved, classes)?;
    let src = out_dir.join("src");
    let mut written: Vec<PathBuf> = Vec::new();

    fn write(written: &mut Vec<PathBuf>, path: PathBuf, text: &str) -> BpsResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| emit(e.to_string()))?;
        }
        std::fs::write(&path, text).map_err(|e| emit(e.to_string()))?;
        written.push(path);
        Ok(())
    }

    // (group, package) pairs in first-seen order, with their units.
    let mut packages: Vec<((String, String), Vec<&Unit>)> = Vec::new();
    for unit in &units {
        let cls = &unit.cls;
        let key = (cls.group.clone(), cls.package.clone());
        match packages.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(unit),
            None => packages.push((key, vec![unit])),
        }
        let path = src
            .join(&cls.group)
            .join(&cls.package)
            .join(format!("{}.rs", cls.stem));
        write(&mut written, path, &emit_packet(unit, resolved)?)?;
        let test = out_dir.join("tests").join(format!(
            "roundtrip_{}_{}_{}.rs",
            cls.group, cls.package, cls.stem
        ));
        write(&mut written, test, &emit_roundtrip_test(unit, resolved)?)?;
    }
    for ((group, package), members) in &packages {
        write(
            &mut written,
            src.join(group).join(package).join("mod.rs"),
            &emit_package_mod(group, package, members),
        )?;
    }
    for group in GROUPS {
        let mut names: Vec<String> = packages
            .iter()
            .filter(|((g, _), _)| g == group)
            .map(|((_, p), _)| p.clone())
            .collect();
        names.sort();
        write(
            &mut written,
            src.join(format!("{group}.rs")),
            &emit_group_mod(group, &names),
        )?;
    }
    let mut ported: Vec<(String, String)> = packages.iter().map(|(k, _)| k.clone()).collect();
    ported.sort();
    write(&mut written, src.join("ported.rs"), &ported_rs(&ported))?;

    let by_reg = registry_entries(&units);
    for (key, entries) in by_reg.iter() {
        write(
            &mut written,
            src.join("packet_vanilla").join(format!("{key}.rs")),
            &emit_registry_fn(key, entries),
        )?;
    }
    let mut registries: Vec<String> = by_reg.keys().map(|k| k.to_string()).collect();
    registries.sort();
    write(
        &mut written,
        src.join("packet_vanilla").join("mod.rs"),
        &vanilla_mod_rs(&registries, &units)?,
    )?;
    write(
        &mut written,
        out_dir.join("tests").join("registry_tables.rs"),
        &emit_registry_tables_test(&by_reg),
    )?;

    write(&mut written, src.join("lib.rs"), LIB_RS)?;
    let frames_text = format!("{}{}", FRAMES_RS, frames(resolved)?.walkers.source);
    write(&mut written, src.join("frames.rs"), &frames_text)?;
    write(
        &mut written,
        out_dir.join("Cargo.toml"),
        &format!(
            "[package]\nname = \"bps_rstom\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nrstom = {{ path = \"{rstom_path}\" }}\n\n\
             [dev-dependencies]\nrstom = {{ path = \"{rstom_path}\" }}\n"
        ),
    )?;
    drop_stale(out_dir, &written)?;

    // Emitted code is stable but not canonical by construction; rustfmt is the
    // single source of truth for layout. Failures are ignored, as in Python.
    for f in &written {
        if f.extension().and_then(|e| e.to_str()) == Some("rs") {
            let _ = std::process::Command::new("rustfmt")
                .arg("--edition")
                .arg("2024")
                .arg(f)
                .output();
        }
    }
    Ok(written)
}
