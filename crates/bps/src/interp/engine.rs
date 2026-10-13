//! Tree-walking interpreter, porting `bps/interp/engine.py`.
//!
//! The Python interpreter re-expands spread/group/match items on every block
//! walk, tagging each expanded item with the parameter scope its expressions
//! read. This port does the same through [`ExpandedItem`], which carries an
//! owned scope snapshot where Python rides a `_param_env` tag on frozen
//! dataclasses.

use crate::ast::*;
use crate::errors::{decode, decode_at, encode, py_repr_str, BpsResult};
use crate::interp::cursor::{Cursor, WriteCursor};
use crate::interp::expr::{eval_expr, int_of};
use crate::value::{Env, Value};
use crate::ResolvedSchema;

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

pub fn decode_packet(
    resolved: &ResolvedSchema,
    packet_name: &str,
    data: &[u8],
    strict: bool,
) -> BpsResult<Env> {
    let pkt = resolved
        .packets
        .get(packet_name)
        .ok_or_else(|| decode(format!("unknown packet {}", py_repr_str(packet_name))))?;
    let mut cur = Cursor::new(data);
    let env = decode_block_items(&pkt.items, &mut cur, resolved, &Env::new(), None)?;
    if strict && cur.remaining() != 0 {
        return Err(decode_at(
            format!(
                "{} of {} bytes left unread after packet {}",
                cur.remaining(),
                data.len(),
                py_repr_str(packet_name)
            ),
            cur.pos,
        ));
    }
    Ok(env)
}

pub fn encode_packet(
    resolved: &ResolvedSchema,
    packet_name: &str,
    value: &Env,
) -> BpsResult<Vec<u8>> {
    let pkt = resolved
        .packets
        .get(packet_name)
        .ok_or_else(|| encode(format!("unknown packet {}", py_repr_str(packet_name))))?;
    let mut w = WriteCursor::new();
    encode_block_items(&pkt.items, &mut w, resolved, value)?;
    Ok(w.result())
}

/// Python-encoded bytes for one field value, or `None` if it cannot encode.
/// (The `_encode_bare` of `bps/emit_rstom.py`.)
pub fn encode_bare(
    ref_: &TypeRef,
    value: &Value,
    resolved: &ResolvedSchema,
    env: &Env,
) -> BpsResult<Option<Vec<u8>>> {
    let mut w = WriteCursor::new();
    match encode_type_ref(ref_, &mut w, resolved, env, value) {
        Ok(()) => Ok(Some(w.result())),
        Err(_) => Ok(None),
    }
}

/// Decode a length-prefixed frame; returns `(packet_id, {"packet", "fields"})`.
pub fn decode_frame_payload(
    resolved: &ResolvedSchema,
    state: &str,
    direction: &str,
    data: &[u8],
) -> BpsResult<(i128, Env)> {
    let mut cur = Cursor::new(data);
    let varint = || TypeRef { name: "varint".to_string(), args: Vec::new(), transforms: Vec::new() };
    let length = int_of(&decode_type_ref(&varint(), &mut cur, resolved, &Env::new())?)?;
    let frame_end = cur.pos
        + usize::try_from(length).map_err(|_| decode("negative frame length"))?;
    let packet_id = int_of(&decode_type_ref(&varint(), &mut cur, resolved, &Env::new())?)?;
    let payload = cur.read(frame_end - cur.pos)?;
    for (route, pkt) in &resolved.routes {
        if route.state == state && route.direction == direction && route.packet_id == packet_id {
            let fields = decode_packet(resolved, &pkt.name, payload, false)?;
            let mut out = Env::new();
            out.insert("packet", Value::str(pkt.name.clone()));
            out.insert("fields", Value::map(fields));
            return Ok((packet_id, out));
        }
    }
    Err(decode(format!("no route for id 0x{packet_id:x} in {state}.{direction}")))
}

// ---------------------------------------------------------------------------
// Expanded items
// ---------------------------------------------------------------------------

/// A match arm after expansion: its body is either a block of expanded items
/// or a bare type reference.
enum ExpandedArmBody {
    Block(Vec<ExpandedItem>),
    Ref,
}

struct ExpandedArm {
    tag: Option<Tag>,
    tags: Vec<Tag>,
    body: Option<ExpandedArmBody>,
}

/// An item after spread expansion. `tag` is the parameter scope snapshot the
/// Python interpreter attaches as `_param_env`; `None` means "no scope"
/// (Python's falsy empty dict).
enum ExpandedItem {
    Require { expr: Expr, tag: Option<Env> },
    Group { cond: Option<Expr>, items: Vec<ExpandedItem>, tag: Option<Env> },
    Computed { name: String, expr: Expr },
    Match { tag_expr: Expr, arms: Vec<ExpandedArm>, tag: Option<Env> },
    Field { wf: WireField },
}

fn non_empty(env: Env) -> Option<Env> {
    if env.is_empty() {
        None
    } else {
        Some(env)
    }
}

/// `{**env, **tag}`: the tag's scope wins, in place.
fn merge_tag(env: &Env, tag: &Option<Env>) -> Env {
    match tag {
        Some(t) => Env::merged_over(env, t),
        None => env.clone(),
    }
}

fn bind_params(params: &[Param], args: &[Expr], env: &Env) -> BpsResult<Env> {
    let mut bind = env.clone();
    for (i, p) in params.iter().enumerate() {
        if i < args.len() {
            // Args evaluate in the caller's scope, not in `bind`.
            let v = eval_expr(&args[i], env)?;
            bind.insert(&p.name, v);
        } else if let Some(d) = p.default {
            bind.insert(&p.name, Value::Int(d));
        } else {
            return Err(decode(format!("missing param {}", p.name)));
        }
    }
    Ok(bind)
}

fn expand_items(
    items: &[BlockItem],
    resolved: &ResolvedSchema,
    param_env: &Env,
) -> BpsResult<Vec<ExpandedItem>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            BlockItem::Spread { name, args } => {
                let enc = resolved.encodings.get(name).ok_or_else(|| {
                    decode(format!("unknown spread encoding {}", py_repr_str(name)))
                })?;
                let bind = bind_params(&enc.params, args, param_env)?;
                match &enc.body {
                    EncBody::Block(body) => {
                        let inner = expand_items(&body.items, resolved, &bind)?;
                        out.push(ExpandedItem::Group { cond: None, items: inner, tag: non_empty(bind) });
                    }
                    EncBody::Ref(t) => {
                        out.push(ExpandedItem::Field {
                            wf: WireField {
                                name: "_spread".to_string(),
                                type_ref: FieldType::Ref(t.clone()),
                                if_expr: None,
                                repeat: None,
                                encode_expr: None,
                            },
                        });
                    }
                }
            }
            BlockItem::Group { cond, items } => {
                let inner = expand_items(items, resolved, param_env)?;
                out.push(ExpandedItem::Group {
                    cond: cond.clone(),
                    items: inner,
                    tag: non_empty(param_env.clone()),
                });
            }
            BlockItem::MatchStmt(m) => {
                let mut arms = Vec::with_capacity(m.arms.len());
                for arm in &m.arms {
                    let body = match &arm.type_ref {
                        Some(FieldType::Block(b)) => Some(ExpandedArmBody::Block(
                            expand_items(&b.items, resolved, param_env)?,
                        )),
                        Some(FieldType::Ref(_)) => Some(ExpandedArmBody::Ref),
                        Some(FieldType::Match(_)) | None => None,
                    };
                    arms.push(ExpandedArm {
                        tag: arm.tag.clone(),
                        tags: arm.tags.clone(),
                        body,
                    });
                }
                out.push(ExpandedItem::Match {
                    tag_expr: m.tag_expr.clone(),
                    arms,
                    tag: non_empty(param_env.clone()),
                });
            }
            BlockItem::Field(wf) => {
                out.push(ExpandedItem::Field { wf: wf.clone() });
            }
            BlockItem::Computed { name, expr } => {
                out.push(ExpandedItem::Computed { name: name.clone(), expr: expr.clone() });
            }
            BlockItem::Require(expr) => {
                out.push(ExpandedItem::Require { expr: expr.clone(), tag: non_empty(param_env.clone()) });
            }
        }
    }
    Ok(out)
}

fn bind_into(env: &mut Env, out: &mut Env, name: &str, v: Value) {
    env.insert(name, v.clone());
    if name != "_spread" {
        out.insert(name, v);
    }
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

fn decode_block_items(
    items: &[BlockItem],
    cur: &mut Cursor<'_>,
    resolved: &ResolvedSchema,
    param_env: &Env,
    prebind: Option<&Env>,
) -> BpsResult<Env> {
    // Expansion happens once per block; the Python interpreter re-expands the
    // already-expanded items on every recursive walk, which is equivalent
    // because the re-tag only re-stamps fields (whose tag nothing reads).
    let expanded = expand_items(items, resolved, param_env)?;
    decode_expanded(&expanded, cur, resolved, param_env, prebind)
}

fn decode_expanded(
    expanded: &[ExpandedItem],
    cur: &mut Cursor<'_>,
    resolved: &ResolvedSchema,
    param_env: &Env,
    prebind: Option<&Env>,
) -> BpsResult<Env> {
    // `prebind` values (optional varint bytes are 0 when absent) are visible to
    // conditions and `require` without being stamped as spread parameters.
    let mut env = match prebind {
        Some(p) => Env::merged_over(p, param_env),
        None => param_env.clone(),
    };
    let mut out = Env::new();

    for item in expanded {
        match item {
            ExpandedItem::Require { expr, tag } => {
                let scope = match tag {
                    Some(t) => Env::merged_over(&env, t),
                    None => env.clone(),
                };
                if !eval_expr(expr, &scope)?.truthy() {
                    return Err(decode(format!("require failed: {}", expr.py_repr())));
                }
            }
            ExpandedItem::Group { cond, items, tag } => {
                let group_env = merge_tag(&env, tag);
                let present = match cond {
                    Some(c) => match eval_expr(c, &group_env) {
                        Ok(v) => v.truthy(),
                        // A guard naming a field the wire did not provide keeps
                        // the group, matching the Python fallback.
                        Err(_) => true,
                    },
                    None => true,
                };
                if !present {
                    continue;
                }
                if cur.remaining() == 0 {
                    // Optional trailing group absent on the wire.
                    continue;
                }
                let decoded = decode_expanded(items, cur, resolved, &group_env, prebind)?;
                for (k, v) in decoded.iter() {
                    bind_into(&mut env, &mut out, k, v.clone());
                }
            }
            ExpandedItem::Computed { name, expr } => {
                let v = eval_expr(expr, &env)?;
                bind_into(&mut env, &mut out, name, v);
            }
            ExpandedItem::Match { tag_expr, arms, .. } => {
                let tag = int_of(&eval_expr(tag_expr, &env)?)?;
                let arm = select_match_arm_tags(arms, tag)
                    .ok_or_else(|| decode(format!("no match arm for tag {tag}")))?;
                match &arm.body {
                    Some(ExpandedArmBody::Block(b)) => {
                        let decoded = decode_expanded(b, cur, resolved, &env, prebind)?;
                        for (k, v) in decoded.iter() {
                            bind_into(&mut env, &mut out, k, v.clone());
                        }
                    }
                    Some(ExpandedArmBody::Ref) => {
                        return Err(decode("statement match arm must be a block"));
                    }
                    None => {}
                }
            }
            ExpandedItem::Field { wf, .. } => {
                if let Some(cond) = &wf.if_expr {
                    if !eval_expr(cond, &env)?.truthy() {
                        continue;
                    }
                }
                let v = match &wf.repeat {
                    Some(rep) => decode_repeat(wf, rep, cur, resolved, &env)?,
                    None => decode_type(&wf.type_ref, cur, resolved, &env)?,
                };
                bind_into(&mut env, &mut out, &wf.name, v);
            }
        }
    }
    Ok(out)
}

/// [`select_match_arm`] over expanded arms: first arm whose int tags include
/// `tag`; `_` is the fallback wherever it sits.
fn select_match_arm_tags(arms: &[ExpandedArm], tag: i128) -> Option<&ExpandedArm> {
    let mut default: Option<&ExpandedArm> = None;
    for arm in arms {
        if arm.tag.is_none() && arm.tags.is_empty() {
            default = Some(arm);
            continue;
        }
        let tags: &[Tag] = if !arm.tags.is_empty() {
            &arm.tags
        } else if arm.tag.is_none() {
            &[]
        } else {
            std::slice::from_ref(arm.tag.as_ref().unwrap())
        };
        for t in tags {
            if let Tag::Int(v) = t {
                if *v == tag {
                    return Some(arm);
                }
            }
        }
    }
    default
}

fn decode_repeat(
    wf: &WireField,
    rep: &Repeat,
    cur: &mut Cursor<'_>,
    resolved: &ResolvedSchema,
    env: &Env,
) -> BpsResult<Value> {
    let mut out = Vec::new();
    match rep.mode {
        RepeatMode::Count => {
            let n = int_of(&eval_expr(rep.expr.as_ref().unwrap(), env)?)?;
            let n = usize::try_from(n).unwrap_or(0);
            if cur.repeat_limit.is_some_and(|limit| n > limit) {
                return Err(decode("count too large for the check"));
            }
            for _ in 0..n {
                out.push(decode_type(&wf.type_ref, cur, resolved, env)?);
            }
        }
        RepeatMode::UntilEnd => {
            while cur.remaining() > 0 {
                out.push(decode_type(&wf.type_ref, cur, resolved, env)?);
            }
        }
        RepeatMode::Until => {
            let term = int_of(&eval_expr(rep.expr.as_ref().unwrap(), env)?)?;
            loop {
                if cur.remaining() == 0 {
                    break;
                }
                if i128::from(cur.peek_u8()?) == term {
                    cur.read(1)?;
                    break;
                }
                out.push(decode_type(&wf.type_ref, cur, resolved, env)?);
            }
        }
    }
    Ok(Value::list(out))
}

fn decode_type(
    t: &FieldType,
    cur: &mut Cursor<'_>,
    resolved: &ResolvedSchema,
    env: &Env,
) -> BpsResult<Value> {
    match t {
        FieldType::Block(b) => {
            let decoded = decode_block_items(&b.items, cur, resolved, env, None)?;
            Ok(Value::map(decoded))
        }
        FieldType::Match(m) => {
            let tag = int_of(&eval_expr(&m.tag_expr, env)?)?;
            let arm = select_match_arm(m, tag)
                .ok_or_else(|| decode(format!("no match arm for tag {tag}")))?;
            match &arm.type_ref {
                None => Ok(Value::Null),
                Some(FieldType::Block(b)) => {
                    let decoded = decode_block_items(&b.items, cur, resolved, env, None)?;
                    Ok(Value::map(decoded))
                }
                Some(other) => decode_type(other, cur, resolved, env),
            }
        }
        FieldType::Ref(r) => decode_type_ref(r, cur, resolved, env),
    }
}

/// Decode one type reference (the Python `_decode_type_ref`; used by the
/// frame-walker differential test to draw random payloads).
pub fn decode_type_ref(
    ref_: &TypeRef,
    cur: &mut Cursor<'_>,
    resolved: &ResolvedSchema,
    env: &Env,
) -> BpsResult<Value> {
    if let Some(enc) = resolved.encodings.get(&ref_.name) {
        // Parameters only: a parameterless encoding must not inherit the
        // caller's whole scope, or its field names collide with the caller's.
        let scope = if enc.params.is_empty() { Env::new() } else { env.clone() };
        let bind = bind_params(&enc.params, &ref_.args, &scope)?;
        if let EncBody::Block(body) = &enc.body {
            if let Some(yield_expr) = &enc.yield_expr {
                return decode_scalar(body, yield_expr, cur, resolved, &bind);
            }
            // Parameters of the encoding stay in scope for its own fields and
            // for any `...spread` inside it.
            let decoded = decode_block_items(&body.items, cur, resolved, &bind, None)?;
            if matches!(enc.name.as_str(), "string" | "identifier" | "nbt_string")
                && decoded.contains_key("value")
            {
                return Ok(decoded.get("value").cloned().unwrap());
            }
            return Ok(Value::map(decoded));
        }
        let EncBody::Ref(inner) = &enc.body else { unreachable!() };
        return decode_type_ref(inner, cur, resolved, &bind);
    }
    match ref_.name.as_str() {
        "bytes" => {
            let n = eval_expr(
                ref_.args.first().ok_or_else(|| decode("bytes() needs a size"))?,
                env,
            )?;
            let n = int_of(&n)?;
            let n = usize::try_from(n)
                .map_err(|_| decode(format!("bytes({n}) got a negative size")))?;
            let raw = cur.read(n)?;
            for tr in &ref_.transforms {
                if tr == "utf8" {
                    return match std::str::from_utf8(raw) {
                        Ok(s) => Ok(Value::str(s.to_string())),
                        Err(_) => Err(decode("bytes are not valid utf-8")),
                    };
                }
            }
            Ok(Value::bytes(raw.to_vec()))
        }
        "rest" => {
            let raw = cur.read(cur.remaining())?;
            Ok(Value::bytes(raw.to_vec()))
        }
        "u8" => {
            let raw = cur.read(1)?;
            Ok(Value::Int(i128::from(raw[0])))
        }
        "f32be" => {
            let raw = cur.read(4)?;
            Ok(Value::Float(f32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64))
        }
        "f64be" => {
            let raw = cur.read(8)?;
            let mut b = [0u8; 8];
            b.copy_from_slice(raw);
            Ok(Value::Float(f64::from_be_bytes(b)))
        }
        _ => Err(decode(format!("cannot decode type {}", py_repr_str(&ref_.name)))),
    }
}

fn collect_wire_names(items: &[BlockItem]) -> Vec<String> {
    let mut names = Vec::new();
    fn walk(items: &[BlockItem], names: &mut Vec<String>) {
        for item in items {
            match item {
                BlockItem::Field(wf) if wf.name != "_spread" => names.push(wf.name.clone()),
                BlockItem::Group { items, .. } => walk(items, names),
                BlockItem::MatchStmt(m) => {
                    for arm in &m.arms {
                        if let Some(FieldType::Block(b)) = &arm.type_ref {
                            walk(&b.items, names);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    walk(items, &mut names);
    names
}

fn decode_scalar(
    body: &BlockType,
    yield_expr: &Expr,
    cur: &mut Cursor<'_>,
    resolved: &ResolvedSchema,
    bind: &Env,
) -> BpsResult<Value> {
    // Optional wire fields (a varint's continuation bytes) are 0 when absent, so
    // the yield expression and any `require` can mention them unconditionally.
    let mut zeros = Env::new();
    for name in collect_wire_names(&body.items) {
        zeros.insert(&name, Value::Int(0));
    }
    let decoded = decode_block_items(&body.items, cur, resolved, bind, Some(&zeros))?;
    let mut env = zeros;
    for (k, v) in bind.iter() {
        env.insert(k, v.clone());
    }
    for (k, v) in decoded.iter() {
        env.insert(k, v.clone());
    }
    eval_expr(yield_expr, &env)
}

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

fn encode_block_items(
    items: &[BlockItem],
    w: &mut WriteCursor,
    resolved: &ResolvedSchema,
    value: &Env,
) -> BpsResult<()> {
    let expanded = expand_items(items, resolved, &Env::new())?;
    encode_expanded(&expanded, w, resolved, value)
}

fn encode_expanded(
    expanded: &[ExpandedItem],
    w: &mut WriteCursor,
    resolved: &ResolvedSchema,
    value: &Env,
) -> BpsResult<()> {
    for item in expanded {
        match item {
            ExpandedItem::Require { .. } | ExpandedItem::Computed { .. } => continue,
            ExpandedItem::Group { cond, items, tag } => {
                if cond.is_none() {
                    encode_expanded(items, w, resolved, &merge_tag(value, tag))?;
                    continue;
                }
                let present = match eval_expr(cond.as_ref().unwrap(), value) {
                    Ok(v) => v.truthy(),
                    // The guard names a field not present in the value being
                    // encoded; trust the value's own keys instead.
                    Err(_) => group_field_names(items)
                        .iter()
                        .any(|wf| value.contains_key(wf)),
                };
                if present {
                    encode_expanded(items, w, resolved, value)?;
                }
            }
            ExpandedItem::Match { tag_expr, arms, tag } => {
                let scope = merge_tag(value, tag);
                let tag_value = int_of(&eval_expr(tag_expr, &scope)?)?;
                let arm = select_match_arm_tags(arms, tag_value)
                    .ok_or_else(|| encode(format!("no match arm for tag {tag_value}")))?;
                match &arm.body {
                    Some(ExpandedArmBody::Block(b)) => {
                        encode_expanded(b, w, resolved, &scope)?
                    }
                    Some(ExpandedArmBody::Ref) => {
                        return Err(encode("statement match arm must be a block"));
                    }
                    None => {}
                }
            }
            ExpandedItem::Field { wf, .. } => {
                if wf.name == "_spread" {
                    // Primitive spread: no caller-visible value, nothing to encode.
                    continue;
                }
                if let Some(cond) = &wf.if_expr {
                    if !eval_expr(cond, value)?.truthy() {
                        continue;
                    }
                }
                let v = value.get(&wf.name).cloned().unwrap_or(Value::Null);
                match &wf.repeat {
                    Some(rep) => encode_repeat(wf, rep, w, resolved, value, &v)?,
                    None => encode_type(&wf.type_ref, w, resolved, value, &v)?,
                }
            }
        }
    }
    Ok(())
}

/// `_group_field_names` over expanded items, including `_spread`.
fn group_field_names(items: &[ExpandedItem]) -> Vec<String> {
    let mut names = Vec::new();
    for it in items {
        match it {
            ExpandedItem::Field { wf, .. } => names.push(wf.name.clone()),
            ExpandedItem::Group { items, .. } => names.extend(group_field_names(items)),
            ExpandedItem::Match { arms, .. } => {
                for arm in arms {
                    if let Some(ExpandedArmBody::Block(b)) = &arm.body {
                        names.extend(group_field_names(b));
                    }
                }
            }
            _ => {}
        }
    }
    names
}

fn encode_repeat(
    wf: &WireField,
    rep: &Repeat,
    w: &mut WriteCursor,
    resolved: &ResolvedSchema,
    env: &Env,
    seq: &Value,
) -> BpsResult<()> {
    let Value::List(items) = seq else {
        return Err(encode(format!("expected list for {}", wf.name)));
    };
    match rep.mode {
        RepeatMode::Count | RepeatMode::Until => {
            for item in items.iter() {
                encode_type(&wf.type_ref, w, resolved, env, item)?;
            }
        }
        RepeatMode::UntilEnd => {
            return Err(encode(format!(
                "encode not supported for repeat mode {}",
                rep.mode.as_str()
            )));
        }
    }
    if rep.mode == RepeatMode::Until {
        let term = int_of(&eval_expr(rep.expr.as_ref().unwrap(), env)?)?;
        w.emit(&[term as u8]);
    }
    Ok(())
}

fn encode_type(
    t: &FieldType,
    w: &mut WriteCursor,
    resolved: &ResolvedSchema,
    env: &Env,
    val: &Value,
) -> BpsResult<()> {
    match t {
        FieldType::Block(b) => {
            let Value::Map(m) = val else {
                return Err(encode("expected dict for block"));
            };
            let mut merged = env.clone();
            for (k, v) in m.iter() {
                merged.insert(k, v.clone());
            }
            encode_block_items(&b.items, w, resolved, &merged)
        }
        FieldType::Ref(r) => encode_type_ref(r, w, resolved, env, val),
        FieldType::Match(m) => {
            let tag_value = int_of(&eval_expr(&m.tag_expr, env)?)?;
            let arm = select_match_arm(m, tag_value)
                .ok_or_else(|| encode(format!("no match arm for tag {tag_value}")))?;
            match &arm.type_ref {
                None => Ok(()),
                Some(body) => encode_type(body, w, resolved, env, val),
            }
        }
    }
}

fn encode_type_ref(
    ref_: &TypeRef,
    w: &mut WriteCursor,
    resolved: &ResolvedSchema,
    env: &Env,
    val: &Value,
) -> BpsResult<()> {
    if let Some(enc) = resolved.encodings.get(&ref_.name) {
        // Parameters only: a parameterless encoding must not inherit the
        // caller's whole scope, or its field names collide with the caller's.
        let scope = if enc.params.is_empty() { Env::new() } else { env.clone() };
        let bind = bind_params(&enc.params, &ref_.args, &scope)?;
        let EncBody::Block(body) = &enc.body else {
            let EncBody::Ref(inner) = &enc.body else { unreachable!() };
            return encode_type_ref(inner, w, resolved, &bind, val);
        };

        if let Some(yield_expr) = &enc.yield_expr {
            return encode_scalar(body, w, resolved, &bind, val, yield_expr);
        }
        if let Value::Str(s) = val {
            if matches!(enc.name.as_str(), "string" | "identifier" | "nbt_string") {
                let raw = s.as_bytes();
                let len_name = if enc.name == "nbt_string" { "u16be" } else { "varint" };
                let len = TypeRef {
                    name: len_name.to_string(),
                    args: Vec::new(),
                    transforms: Vec::new(),
                };
                encode_type_ref(&len, w, resolved, &Env::new(), &Value::Int(raw.len() as i128))?;
                w.emit(raw);
                return Ok(());
            }
        }
        if enc.name == "uuid" {
            if let Value::Map(m) = val {
                let msb = m.get("msb").ok_or_else(|| encode("uuid needs msb"))?;
                let lsb = m.get("lsb").ok_or_else(|| encode("uuid needs lsb"))?;
                let msb = u64::try_from(int_of(msb)?)
                    .map_err(|_| encode("uuid msb out of range"))?;
                let lsb = u64::try_from(int_of(lsb)?)
                    .map_err(|_| encode("uuid lsb out of range"))?;
                w.emit(&msb.to_be_bytes());
                w.emit(&lsb.to_be_bytes());
                return Ok(());
            }
        }
        if enc.name == "fixed_bitset" {
            let raw = raw_bytes(val)
                .ok_or_else(|| encode(format!("expected dict for {}", enc.name)))?;
            w.emit(&raw);
            return Ok(());
        }
        // A declared-but-empty payload (e.g. an empty NBT compound arm) needs no
        // value at all; only a non-dict for a non-empty body is an error.
        let payload = match val {
            Value::Map(m) => (**m).clone(),
            _ => {
                if !body.items.is_empty() {
                    return Err(encode(format!("expected dict for {}", enc.name)));
                }
                Env::new()
            }
        };
        let mut merged = env.clone();
        for (k, v) in bind.iter() {
            merged.insert(k, v.clone());
        }
        for (k, v) in payload.iter() {
            merged.insert(k, v.clone());
        }
        return encode_block_items(&body.items, w, resolved, &merged);
    }
    match ref_.name.as_str() {
        "bytes" => {
            let n = eval_expr(
                ref_.args.first().ok_or_else(|| encode("bytes() needs a size"))?,
                env,
            )?;
            let n = int_of(&n)?;
            let n = usize::try_from(n)
                .map_err(|_| encode(format!("bytes({n}) got a negative size")))?;
            let raw = match val {
                Value::Str(s) => s.as_bytes().to_vec(),
                Value::Bytes(b) => b.to_vec(),
                Value::List(items) => {
                    let mut out = Vec::with_capacity(items.len());
                    for it in items.iter() {
                        match it {
                            Value::Int(i) if (0..=255).contains(i) => out.push(*i as u8),
                            Value::Bool(b) => out.push(u8::from(*b)),
                            _ => {
                                return Err(encode(format!(
                                    "bytes({n}) got a non-byte list item"
                                )))
                            }
                        }
                    }
                    out
                }
                _ => return Err(encode(format!("bytes({n}) got {}", val.type_name()))),
            };
            if raw.len() < n {
                let mut padded = raw;
                padded.resize(n, 0);
                w.emit(&padded);
            } else if raw.len() > n {
                return Err(encode(format!("bytes({n}) got {} bytes", raw.len())));
            } else {
                w.emit(&raw);
            }
            Ok(())
        }
        "rest" => {
            let raw = raw_bytes(val)
                .ok_or_else(|| encode(format!("expected bytes for rest, got {}", val.type_name())))?;
            w.emit(&raw);
            Ok(())
        }
        "f32be" => {
            let f = as_f64(val)?;
            w.emit(&(f as f32).to_be_bytes());
            Ok(())
        }
        "f64be" => {
            let f = as_f64(val)?;
            w.emit(&f.to_be_bytes());
            Ok(())
        }
        "u8" => {
            let v = int_of(val)?;
            w.emit(&[(v & 0xFF) as u8]);
            Ok(())
        }
        _ => Err(encode(format!("cannot encode type {}", py_repr_str(&ref_.name)))),
    }
}

fn as_f64(val: &Value) -> BpsResult<f64> {
    match val {
        Value::Int(i) => Ok(*i as f64),
        Value::Bool(b) => Ok(i128::from(*b) as f64),
        Value::Float(f) => Ok(*f),
        Value::Str(s) => s
            .parse::<f64>()
            .map_err(|_| encode(format!("could not convert string to float: {}", py_repr_str(s)))),
        _ => Err(encode(format!("must be real number, not {}", val.type_name()))),
    }
}

fn encode_scalar(
    body: &BlockType,
    w: &mut WriteCursor,
    resolved: &ResolvedSchema,
    bind: &Env,
    val: &Value,
    _yield_expr: &Expr,
) -> BpsResult<()> {
    // `value` is the scalar being written. Each field is bound under its name
    // after its expression runs, so a later `if` can see the byte just chosen.
    let mut env = bind.clone();
    env.insert("value", val.clone());
    for name in collect_wire_names(&body.items) {
        if !env.contains_key(&name) {
            env.insert(&name, Value::Int(0));
        }
    }
    encode_scalar_items(&body.items, w, resolved, &mut env)
}

fn encode_scalar_items(
    items: &[BlockItem],
    w: &mut WriteCursor,
    resolved: &ResolvedSchema,
    env: &mut Env,
) -> BpsResult<()> {
    for item in items {
        match item {
            BlockItem::Require(_) | BlockItem::Spread { .. } => continue,
            BlockItem::Computed { name, expr } => {
                let v = eval_expr(expr, env)?;
                env.insert(name, v);
            }
            BlockItem::Group { cond, items } => {
                if let Some(c) = cond {
                    if !eval_expr(c, env)?.truthy() {
                        continue;
                    }
                }
                encode_scalar_items(items, w, resolved, env)?;
            }
            BlockItem::MatchStmt(m) => {
                let tag_value = int_of(&eval_expr(&m.tag_expr, env)?)?;
                let arm = select_match_arm(m, tag_value)
                    .ok_or_else(|| encode(format!("no match arm for tag {tag_value}")))?;
                if let Some(FieldType::Block(b)) = &arm.type_ref {
                    encode_scalar_items(&b.items, w, resolved, env)?;
                }
            }
            BlockItem::Field(wf) => {
                if let Some(cond) = &wf.if_expr {
                    if !eval_expr(cond, env)?.truthy() {
                        continue;
                    }
                }
                let written = match &wf.encode_expr {
                    Some(e) => eval_expr(e, env)?,
                    None => env.get(&wf.name).cloned().unwrap_or(Value::Null),
                };
                env.insert(&wf.name, written.clone());
                match &wf.repeat {
                    Some(rep) => encode_repeat(wf, rep, w, resolved, env, &written)?,
                    None => encode_type(&wf.type_ref, w, resolved, env, &written)?,
                }
            }
        }
    }
    Ok(())
}

/// Coerce a wire value into raw bytes: bytes, list of ints, or dict with
/// `value`/`data`/`longs` (the Python `_raw_bytes`).
fn raw_bytes(val: &Value) -> Option<Vec<u8>> {
    match val {
        Value::Bytes(b) => Some(b.to_vec()),
        Value::Str(s) => Some(s.as_bytes().to_vec()),
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items.iter() {
                match it {
                    Value::Int(i) if (0..=255).contains(i) => out.push(*i as u8),
                    Value::Bool(b) => out.push(u8::from(*b)),
                    _ => return None,
                }
            }
            Some(out)
        }
        Value::Map(m) => {
            for key in ["value", "data", "longs"] {
                if let Some(v) = m.get(key) {
                    if let Some(got) = raw_bytes(v) {
                        return Some(got);
                    }
                }
            }
            None
        }
        _ => None,
    }
}
