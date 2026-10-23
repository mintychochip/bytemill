//! Synthetic packet values, porting `bps/synth.py`.
//!
//! One value per declared field type, small enough to satisfy the schema's own
//! constraints (`require` assertions, count expressions, guard evaluation). The
//! values follow the decoded shape the interpreter produces.

use std::collections::HashSet;

use crate::ast::{
    BlockItem, EncBody, Expr, FieldType, MatchArm, MatchType, PacketDecl, RepeatMode,
    ResolvedSchema, Tag, TypeRef, WireField, arm_tag_list, select_match_arm,
};
use crate::errors::{BmlError, BmlResult, py_repr_str};
use crate::interp::eval_expr;
use crate::ordmap::OrderedMap;
use crate::value::{Env, Value};

/// The `_Env` scope of one synthesis pass: the schema's encodings.
pub struct SynthScope<'a> {
    pub resolved: &'a ResolvedSchema,
}

/// The `SYNTH_OVERRIDES` table: `"<packet>/<field>"` -> value, in declaration order.
/// `Value` is `Rc`-based, so the table is per-thread. `f` may mutate it, as the
/// Python tests mutate the dict.
pub fn with_synth_overrides<R>(f: impl FnOnce(&mut OrderedMap<Value>) -> R) -> R {
    thread_local! {
        static TABLE: std::cell::RefCell<OrderedMap<Value>> =
            std::cell::RefCell::new(default_overrides());
    }
    TABLE.with(|t| f(&mut t.borrow_mut()))
}

fn default_overrides() -> OrderedMap<Value> {
    let mut m = OrderedMap::new();
    // Client settings: view distance must be 2..32 (ServerboundClientInformationPacket).
    m.insert("client_information/view_distance", Value::Int(8));
    // Login disconnect reason is a JSON text component string on the wire (LoginDisconnectPacket).
    m.insert("login_disconnect/reason", Value::str("\"abc\""));
    // RemoteDebugSampleType has one constant TICK_TIME (ordinal 0).
    m.insert("debug_sample/sample_type", Value::Int(0));
    // Recipe property sets: vanilla keys are namespaced identifiers (ClientboundUpdateRecipesPacket).
    m.insert(
        "update_recipes/property_sets",
        Value::list(vec![obj(vec![
            ("property_set_id", Value::str("minecraft:smithing_base")),
            ("items_count", Value::Int(1)),
            ("items", Value::list(vec![Value::Int(1)])),
        ])]),
    );
    // Display override: see synth.py for the DisplayInfo.java field layout.
    m.insert(
        "update_advancements/advancement_mappings",
        Value::list(vec![obj(vec![
            ("key", Value::str("minecraft:stone")),
            (
                "value",
                obj(vec![
                    ("has_parent_id", Value::Bool(true)),
                    ("parent_id", Value::str("minecraft:stone")),
                    ("has_display_data", Value::Bool(false)),
                    ("requirements_count", Value::Int(1)),
                    (
                        "requirements",
                        Value::list(vec![obj(vec![
                            ("criteria_count", Value::Int(1)),
                            ("criteria", Value::list(vec![Value::str("abc")])),
                        ])]),
                    ),
                    ("sends_telemetry_data", Value::Bool(true)),
                ]),
            ),
            ("x", Value::Float(1.5)),
            ("y", Value::Float(1.5)),
        ])]),
    );
    // Length-prefixed optional tag: byte length must match the following network NBT
    // (ServerboundCustomClickActionPacket.java UNTRUSTED_TAG_CODEC + lengthPrefixed).
    m.insert("custom_click_action/payload_size", Value::Int(2));
    m
}

/// A decoded-shape dict: entries in the given order.
fn obj(entries: Vec<(&str, Value)>) -> Value {
    let mut m = OrderedMap::new();
    for (k, v) in entries {
        m.insert(k, v);
    }
    Value::map(m)
}

fn raw_bytes() -> Value {
    Value::bytes(vec![0x01, 0x02, 0x03])
}

/// `_PRIM[name]`: the value per primitive, small enough to satisfy the schema.
fn prim(name: &str) -> Option<Value> {
    let uuid = || obj(vec![("msb", Value::Int(1)), ("lsb", Value::Int(2))]);
    let slot0 = || obj(vec![("type", Value::Int(0)), ("value", obj(vec![]))]);
    let v = match name {
        "u8" | "i8" => Value::Int(1),
        "u16be" | "i16be" => Value::Int(0x1234),
        "u32be" | "i32be" => Value::Int(0x0102_0304),
        "u64be" | "i64be" => Value::Int(0x0102_0304_0506_0708),
        "f32be" => Value::Float(1.5),
        "f64be" => Value::Float(2.25),
        "f32le" => Value::Float(1.5),
        "f64le" => Value::Float(2.25),
        "varint" | "varlong" => Value::Int(1),
        "boolean" => Value::Bool(true),
        "angle" => Value::Int(64),
        "string" | "nbt_string" => Value::str("abc"),
        "identifier" => Value::str("minecraft:stone"),
        "uuid" => uuid(),
        "position" => obj(vec![("raw", Value::Int(0x4607_632C_15B4_833F))]),
        "nbt" => obj(vec![
            ("tag_type", Value::Int(10)),
            ("value", obj(vec![("entries", Value::list(vec![]))])),
        ]),
        "text_component" => obj(vec![
            ("tag_type", Value::Int(8)),
            ("value", Value::str("abc")),
        ]),
        "slot" => obj(vec![("item_count", Value::Int(0))]),
        "hashed_slot" => obj(vec![("has_item", Value::Bool(false))]),
        "id_or_sound_event" => obj(vec![("id", Value::Int(1))]),
        "game_profile" => obj(vec![
            ("profile_uuid", uuid()),
            ("username", Value::str("abc")),
            ("properties_count", Value::Int(0)),
            ("properties", Value::list(vec![])),
        ]),
        "entity_metadata" | "nbt_compound_body" => obj(vec![("entries", Value::list(vec![]))]),
        "meta_entry" => Value::Null,
        "nbt_nothing" => obj(vec![]),
        "nbt_tag" => obj(vec![("tag_type", Value::Int(0)), ("value", Value::Null)]),
        "lpvec3" => obj(vec![
            ("scale_byte", Value::Int(0)),
            ("packed_second", Value::Int(0)),
            ("packed_high_4", Value::Int(0)),
        ]),
        // angry_villager (id 0): SimpleParticleType with no payload (ParticleTypes.java).
        "particle_data" => obj(vec![
            ("particle_type", Value::Int(0)),
            ("value", obj(vec![])),
        ]),
        // An empty slot display, and a stonecutter recipe display of three empty ones.
        "slot_display" => slot0(),
        "recipe_display" => obj(vec![
            ("type", Value::Int(3)),
            (
                "value",
                obj(vec![
                    ("ingredient", slot0()),
                    ("result", slot0()),
                    ("crafting_station", slot0()),
                ]),
            ),
        ]),
        "id_set" => obj(vec![
            ("type", Value::Int(2)),
            ("ids", Value::list(vec![Value::Int(300)])),
        ]),
        "block_state" => obj(vec![("block_state", Value::Int(300))]),
        "water_blocks" => obj(vec![("water_blocks", Value::Int(300))]),
        "water_blocks_burst" => obj(vec![
            ("water_blocks", Value::Int(300)),
            ("burst_impulse_base", Value::Float(1.5)),
        ]),
        "power" => obj(vec![("power", Value::Float(1.5))]),
        "dust_color" => obj(vec![("color", Value::Int(300))]),
        "dust_color_scale" => obj(vec![
            ("color", Value::Int(300)),
            ("scale", Value::Float(1.5)),
        ]),
        "dust_color_transition" => obj(vec![
            ("from_color", Value::Int(300)),
            ("to_color", Value::Int(300)),
            ("scale", Value::Float(1.5)),
        ]),
        "spell_particle" => obj(vec![
            ("color", Value::Int(300)),
            ("power", Value::Float(1.5)),
        ]),
        "sculk_charge_roll" => obj(vec![("roll", Value::Float(1.5))]),
        "shriek_delay" => obj(vec![("delay", Value::Int(300))]),
        "trail_data" => obj(vec![
            ("x", Value::Float(2.25)),
            ("y", Value::Float(2.25)),
            ("z", Value::Float(2.25)),
            ("color", Value::Int(300)),
            ("duration", Value::Int(300)),
        ]),
        _ => return None,
    };
    Some(v)
}

/// `_value_for(t, env, scope)`: a field type's synthetic value.
pub fn value_for(t: &FieldType, env: &Env, scope: &SynthScope<'_>) -> BmlResult<Value> {
    match t {
        FieldType::Block(b) => block_value(&b.items, env, scope),
        FieldType::Match(mt) => {
            for arm in &mt.arms {
                if let Some(ft) = &arm.type_ref {
                    return value_for(ft, env, scope);
                }
            }
            Ok(Value::Int(0))
        }
        FieldType::Ref(tr) => ref_value(tr, env, scope),
    }
}

fn ref_value(tr: &TypeRef, env: &Env, scope: &SynthScope<'_>) -> BmlResult<Value> {
    let name = tr.name.as_str();
    if name == "rest" || name == "bytes" {
        return Ok(raw_bytes());
    }
    if let Some(v) = prim(name) {
        return Ok(v);
    }
    match name {
        "item_stack_template" => {
            return Ok(obj(vec![
                ("item_id", Value::Int(1)),
                ("item_count", Value::Int(1)),
                ("component_added", Value::Int(0)),
                ("component_removed", Value::Int(0)),
            ]));
        }
        "item_cost" => {
            return Ok(obj(vec![
                ("item_id", Value::Int(1)),
                ("count", Value::Int(1)),
                ("predicate_count", Value::Int(0)),
            ]));
        }
        _ => {}
    }
    let Some(enc) = scope.resolved.encodings.get(name) else {
        return Ok(raw_bytes());
    };
    match &enc.body {
        EncBody::Ref(inner) => ref_value(inner, env, scope),
        EncBody::Block(b) => {
            if enc.yield_expr.is_some() {
                // `_PRIM.get(name, 1)`: `name` is not a primitive here, so the default.
                Ok(Value::Int(1))
            } else {
                block_value(&b.items, env, scope)
            }
        }
    }
}

/// `_block_value(items, env, scope)` as a decoded-shape map.
fn block_value(items: &[BlockItem], env: &Env, scope: &SynthScope<'_>) -> BmlResult<Value> {
    let mut local = env.clone();
    Ok(Value::map(block_walk(items, &mut local, scope, true)?))
}

fn concrete_arm_tag(arm: &MatchArm) -> Option<i128> {
    arm_tag_list(arm).iter().find_map(|t| match t {
        Tag::Int(i) => Some(*i),
        Tag::Str(_) => None,
    })
}

/// Python `int(eval_expr(expr, env))`, with `None` where Python catches the error.
fn scrutinee(expr: &Expr, env: &Env) -> Option<i128> {
    match eval_expr(expr, env).ok()? {
        Value::Int(i) => Some(i),
        Value::Bool(b) => Some(i128::from(b)),
        Value::Float(f) if f.is_finite() => Some(f.trunc() as i128),
        Value::Str(s) => s.trim().parse().ok(),
        Value::Bytes(b) => std::str::from_utf8(&b).ok()?.trim().parse().ok(),
        _ => None,
    }
}

/// `_synth_match(item, local, scope)`: the statement-form match's arm, merged.
fn synth_match(mt: &MatchType, local: &mut Env, scope: &SynthScope<'_>) -> BmlResult<Env> {
    let mut arm = scrutinee(&mt.tag_expr, local).and_then(|tag| select_match_arm(mt, tag));
    let mut extra = Env::new();
    if arm.is_none()
        && let Some(first) = mt.arms.first()
    {
        // No arm matched and there is no `_`: retarget a plain identifier
        // scrutinee onto the first concrete tag so the value can still encode.
        let fallback = mt
            .arms
            .iter()
            .find(|a| concrete_arm_tag(a).is_some())
            .unwrap_or(first);
        arm = Some(fallback);
        if let (Some(chosen), Expr::Ident(name)) = (concrete_arm_tag(fallback), &mt.tag_expr)
            && !name.is_empty()
        {
            local.insert(name, Value::Int(chosen));
            extra.insert(name, Value::Int(chosen));
        }
    }
    if let Some(arm) = arm
        && let Some(FieldType::Block(b)) = &arm.type_ref
    {
        let inner = block_walk(&b.items, local, scope, false)?;
        merge(&mut extra, &inner);
    }
    Ok(extra)
}

/// `out.update(other)`: keys keep their first position, values are overwritten.
fn merge(out: &mut Env, other: &Env) {
    for (k, v) in other.iter() {
        out.insert(k, v.clone());
    }
}

/// The shared walk of `_block_value` (`top = true`) and `_block_value_shared`
/// (`top = false`), which read and write the caller's scope `local`.
///
/// The two differ only in count-driver bookkeeping: a top-level block touches a
/// driver or identifier in its own `out` only when it is already there.
fn block_walk(
    items: &[BlockItem],
    local: &mut Env,
    scope: &SynthScope<'_>,
    top: bool,
) -> BmlResult<Env> {
    let mut out = Env::new();
    for item in items {
        match item {
            BlockItem::Computed { name, expr } => {
                let v = eval_expr(expr, local)?;
                local.insert(name, v.clone());
                out.insert(name, v);
            }
            BlockItem::Require(_) | BlockItem::Spread { .. } => {}
            BlockItem::Group { cond, items: sub } => {
                // A guard that reads a field not yet synthesized (e.g. a repeated
                // element's discriminator) counts as true, so the encode path has
                // values for the group.
                let include = match cond {
                    None => true,
                    Some(c) => match eval_expr(c, local) {
                        Ok(v) => v.truthy(),
                        Err(_) => true,
                    },
                };
                if include {
                    // The group shares the enclosing scope, so a count adjustment
                    // inside it is visible to the enclosing block.
                    let inner = block_walk(sub, local, scope, false)?;
                    merge(&mut out, &inner);
                }
            }
            BlockItem::MatchStmt(mt) => {
                let extra = synth_match(mt, local, scope)?;
                merge(&mut out, &extra);
            }
            BlockItem::Field(wf) => field_item(wf, local, &mut out, scope, top)?,
        }
    }
    Ok(out)
}

fn field_item(
    wf: &WireField,
    local: &mut Env,
    out: &mut Env,
    scope: &SynthScope<'_>,
    top: bool,
) -> BmlResult<()> {
    if let Some(cond) = &wf.if_expr
        && let Ok(v) = eval_expr(cond, local)
        && !v.truthy()
    {
        return Ok(());
    }
    let v = match &wf.repeat {
        Some(_) => Value::list(vec![value_for(&wf.type_ref, local, scope)?]),
        None => field_value(&wf.type_ref, local, out, scope)?,
    };
    out.insert(&wf.name, v.clone());
    local.insert(&wf.name, v.clone());
    fit_length(wf, &v, local, out);
    let Some(rep) = &wf.repeat else {
        return Ok(());
    };
    if rep.mode != RepeatMode::Count {
        return Ok(());
    }
    // A count-modulated repeat may also carry a length field elsewhere in the
    // block, or derive its count from an expression. The count the decode side
    // evaluates must equal the list length, so derive the driving value from it.
    let expr = rep.expr.as_ref();
    let len = list_len(&v);
    match count_driver(expr) {
        Some(driver) if driver != wf.name.as_str() => {
            if !top || out.contains_key(driver) {
                out.insert(driver, Value::Int(len));
            }
            local.insert(driver, Value::Int(len));
        }
        _ => {
            let mut idents = Vec::new();
            if let Some(e) = expr {
                expr_idents(e, &mut idents);
            }
            for ident in &idents {
                if matches!(local.get(ident), Some(Value::Int(_)) | Some(Value::Bool(_))) {
                    local.insert(ident, Value::Int(2));
                    if !top || out.contains_key(ident) {
                        out.insert(ident, Value::Int(2));
                    }
                }
            }
            // `v = [v[0]] if v else []`
            let first_only = Value::list(match &v {
                Value::List(items) => items.iter().take(1).cloned().collect(),
                _ => Vec::new(),
            });
            out.insert(&wf.name, first_only.clone());
            local.insert(&wf.name, first_only);
        }
    }
    Ok(())
}

/// `len(v)` for the list a repeat field produces (always a list here).
fn list_len(v: &Value) -> i128 {
    match v {
        Value::List(items) => items.len() as i128,
        _ => 0,
    }
}

/// `_expr_idents(expr)`: every identifier an expression reads, innermost first.
fn expr_idents(expr: &Expr, found: &mut Vec<String>) {
    match expr {
        Expr::Ident(name) => found.push(name.clone()),
        Expr::Int(_) | Expr::Bool(_) | Expr::Str(_) | Expr::Path(_) => {}
        Expr::Unop(_, inner) => expr_idents(inner, found),
        Expr::Binop(_, left, right) => {
            expr_idents(left, found);
            expr_idents(right, found);
        }
        Expr::Ternary(cond, yes, no) => {
            expr_idents(cond, found);
            expr_idents(yes, found);
            expr_idents(no, found);
        }
        Expr::Call(_, args) => {
            for a in args {
                expr_idents(a, found);
            }
        }
    }
}

/// `_count_driver(expr)`: the name of the int field a `count = expr` reads, when
/// the expression is a plain identifier.
fn count_driver(expr: Option<&Expr>) -> Option<&str> {
    match expr {
        Some(Expr::Ident(name)) => Some(name.as_str()),
        _ => None,
    }
}

/// `_equality_pin(expr)`: `ident == int` (either order) as a `(name, value)` pin.
fn equality_pin(expr: &Expr) -> Option<(String, Value)> {
    let Expr::Binop(op, left, right) = expr else {
        return None;
    };
    if op != "==" {
        return None;
    }
    match (left.as_ref(), right.as_ref()) {
        (Expr::Ident(name), Expr::Int(v)) | (Expr::Int(v), Expr::Ident(name)) => {
            Some((name.clone(), Value::Int(*v)))
        }
        _ => None,
    }
}

/// `_require_equalities(items)`: pins from `require ident == int`, through groups
/// and block-valued match arms.
fn require_equalities(items: &[BlockItem]) -> Vec<(String, Value)> {
    let mut pins = Vec::new();
    for item in items {
        match item {
            BlockItem::Require(expr) => pins.extend(equality_pin(expr)),
            BlockItem::Group { items: sub, .. } => pins.extend(require_equalities(sub)),
            BlockItem::MatchStmt(mt) => {
                for arm in &mt.arms {
                    if let Some(FieldType::Block(b)) = &arm.type_ref {
                        pins.extend(require_equalities(&b.items));
                    }
                }
            }
            BlockItem::Field(_) | BlockItem::Computed { .. } | BlockItem::Spread { .. } => {}
        }
    }
    pins
}

/// `_declared_wire_fields(items)`: field names, through groups and block arms.
fn declared_wire_fields(items: &[BlockItem]) -> HashSet<String> {
    let mut names = HashSet::new();
    for item in items {
        match item {
            BlockItem::Field(wf) => {
                names.insert(wf.name.clone());
            }
            BlockItem::Group { items: sub, .. } => names.extend(declared_wire_fields(sub)),
            BlockItem::MatchStmt(mt) => {
                for arm in &mt.arms {
                    if let Some(FieldType::Block(b)) = &arm.type_ref {
                        names.extend(declared_wire_fields(&b.items));
                    }
                }
            }
            BlockItem::Computed { .. } | BlockItem::Require(_) | BlockItem::Spread { .. } => {}
        }
    }
    names
}

/// The `(packet, field, value)` entries of the override table, in table order.
fn override_entries() -> Vec<(String, String, Value)> {
    with_synth_overrides(|table| {
        table
            .iter()
            .map(|(key, val)| {
                let (pkt, field) = key.split_once('/').unwrap_or((key, ""));
                (pkt.to_string(), field.to_string(), val.clone())
            })
            .collect()
    })
}

fn check_override(resolved: &ResolvedSchema, pkt_name: &str, field_name: &str) -> BmlResult<()> {
    let pkt = resolved.packets.get(pkt_name).ok_or_else(|| {
        BmlError::resolve(format!(
            "SYNTH_OVERRIDES references unknown packet {}",
            py_repr_str(pkt_name)
        ))
    })?;
    if !declared_wire_fields(&pkt.items).contains(field_name) {
        return Err(BmlError::resolve(format!(
            "SYNTH_OVERRIDES references unknown field {} on packet {}",
            py_repr_str(field_name),
            py_repr_str(pkt_name)
        )));
    }
    Ok(())
}

/// `validate_all_synth_overrides(resolved)`: every override names a real packet and field.
pub fn validate_all_synth_overrides(resolved: &ResolvedSchema) -> BmlResult<()> {
    for (pkt_name, field_name, _) in override_entries() {
        check_override(resolved, &pkt_name, &field_name)?;
    }
    Ok(())
}

/// `synthesize_packet(resolved, pkt)`: a decoded-shape value for `pkt` that the
/// interpreter encodes cleanly.
pub fn synthesize_packet(resolved: &ResolvedSchema, pkt: &PacketDecl) -> BmlResult<Env> {
    let scope = SynthScope { resolved };
    let overrides = override_entries();
    for (pkt_name, field_name, _) in &overrides {
        if pkt_name == &pkt.name {
            check_override(resolved, pkt_name, field_name)?;
        }
    }
    let mut local = Env::new();
    let mut out = block_walk(&pkt.items, &mut local, &scope, true)?;
    for (name, val) in require_equalities(&pkt.items) {
        out.insert(&name, val);
    }
    for (pkt_name, field_name, val) in overrides {
        if pkt_name == pkt.name {
            out.insert(&field_name, val);
        }
    }
    Ok(out)
}

/// `_fit_length`: a `bytes(n)` payload is exactly `n` bytes, so `n` becomes its length.
fn fit_length(wf: &WireField, value: &Value, local: &mut Env, out: &mut Env) {
    if wf.repeat.is_some() {
        return;
    }
    let FieldType::Ref(t) = &wf.type_ref else {
        return;
    };
    if t.name != "bytes" {
        return;
    }
    let Some(driver) = count_driver(t.args.first()) else {
        return;
    };
    if !local.contains_key(driver) {
        return;
    }
    let Value::Bytes(data) = value else {
        return;
    };
    let n = Value::Int(data.len() as i128);
    local.insert(driver, n.clone());
    if out.contains_key(driver) {
        out.insert(driver, n);
    }
}

/// `_field_value`: a field's value; for a `match` type, the arm its scrutinee selects.
fn field_value(
    t: &FieldType,
    local: &mut Env,
    out: &mut Env,
    scope: &SynthScope<'_>,
) -> BmlResult<Value> {
    let FieldType::Match(mt) = t else {
        return value_for(t, local, scope);
    };
    let mut arm = scrutinee(&mt.tag_expr, local).and_then(|tag| select_match_arm(mt, tag));
    if arm.is_none() {
        arm = mt.arms.iter().find(|a| concrete_arm_tag(a).is_some());
        if let (Some(chosen), Expr::Ident(name)) = (arm.and_then(concrete_arm_tag), &mt.tag_expr)
            && !name.is_empty()
        {
            // `out` may be a conditional group's result, merged into its parent's.
            local.insert(name, Value::Int(chosen));
            out.insert(name, Value::Int(chosen));
        }
    }
    match arm.and_then(|a| a.type_ref.as_ref()) {
        None => Ok(obj(vec![])),
        Some(ft) => value_for(ft, local, scope),
    }
}
