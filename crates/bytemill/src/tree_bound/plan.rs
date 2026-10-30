//! How each hand field of a scope converts to and from the wire fields it carries.

use super::hand::{Hand, HandTy, by_value, elem_plan};
use super::own::{Ownership, Role, assign};
use crate::ast::ResolvedSchema;
use crate::emit_rstom::{PacketShape, frame_expr};
use crate::errors::{BmlResult, emit};
use crate::ordmap::OrderedMap;
use crate::shapes::model::{ItemBind, WriteSpec};
use crate::tree_bind::Bindings;
use std::collections::BTreeSet;

/// What the bodies of one packet accumulate while they are built.
pub struct Cx<'a> {
    pub module: &'a str,
    pub resolved: &'a ResolvedSchema,
    pub bindings: &'a Bindings,
    /// `use` lines the conversions need.
    pub imports: BTreeSet<String>,
    /// Every template, hook and constant of the shape: names a read must keep a local for.
    pub template_text: String,
}

/// What carries a hand field's wire fields as a whole, in place of per-wire conversions.
#[derive(Debug, Clone, PartialEq)]
pub enum Carrier {
    /// A `Type` constant or expression: `buffer.write(codec, value)`, `buffer.read(codec)`.
    Codec(String),
    /// The hand type's own buffer functions: a statement over `$`, an expression.
    Direct { put: String, take: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// One or more plain wire fields, converted by templates or one codec.
    Value,
    /// A repeated wire field, one hand element per repetition.
    List,
}

/// The conversions of one hand field.
#[derive(Debug, Clone)]
pub struct HandPlan {
    pub ty: HandTy,
    /// The type with the `Option` that the carried wires' guard makes removed.
    pub inner: HandTy,
    /// The hand field is an `Option` whose presence the carried wires' guard decides.
    pub opt_layer: bool,
    /// Writes take the whole `Option`, not its content: the guard alone decides.
    pub write_whole: bool,
    /// The read template yields an `Option` of its own.
    pub read_flat: bool,
    /// The hand field is a plain type whose guarded wire fields are skipped: it reads as its
    /// default.
    pub guarded_default: bool,
    /// The expression such a field reads as when its guard skips the wire fields.
    pub absent: Option<String>,
    pub codec: Option<Carrier>,
    pub kind: Kind,
    /// Per carried wire field, the write template over `$`; `None` writes the place itself.
    pub writes: Vec<Option<String>>,
    pub read: Option<String>,
    pub item: Option<ItemBind>,
}

/// A scope: the wire fields of a packet or of a repeat's element, and the hand fields that carry
/// them.
pub struct Scope<'a> {
    pub wires: &'a [PacketShape],
    /// The hand fields that bind to wire fields: the scope's own, with nested structs replaced
    /// by their fields.
    pub hands: Vec<Hand>,
    /// The scope's own hand fields as declared, and where each one's value comes from.
    pub orig: Vec<Hand>,
    pub slots: Vec<super::hand::Slot>,
    pub own: Ownership,
    pub plans: Vec<HandPlan>,
    /// Nesting depth; names the loop variable of a list in this scope.
    pub depth: usize,
}

pub fn scope<'a>(
    cx: &mut Cx,
    wires: &'a [PacketShape],
    hands: Vec<Hand>,
    constants: &OrderedMap<String>,
    depth: usize,
) -> BmlResult<Scope<'a>> {
    let orig = hands.clone();
    let (hands, slots) = super::hand::flatten(hands);
    let own = assign(cx.module, wires, &hands, constants)?;
    let mut plans = Vec::new();
    for (hi, h) in hands.iter().enumerate() {
        plans.push(plan_hand(cx, h, wires, &own.carried[hi])?);
    }
    Ok(Scope {
        wires,
        hands,
        orig,
        slots,
        own,
        plans,
        depth,
    })
}

/// The element scope of the list the repeat wire `wires[j]` carries for hand field `hi`.
pub fn elem_scope<'a>(
    cx: &mut Cx,
    sc: &Scope<'a>,
    hi: usize,
    j: usize,
) -> BmlResult<(Scope<'a>, String, String)> {
    let w: &'a PacketShape = &sc.wires[j];
    let plan = &sc.plans[hi];
    let elem_ty = match &plan.inner {
        HandTy::Vec(inner) => (**inner).clone(),
        other => {
            return Err(emit(format!(
                "{}: field {} carries the repeated wire field {} and must be a Vec, not {}",
                cx.module,
                sc.hands[hi].name,
                w.ident,
                other.text()
            )));
        }
    };
    let var = format!("it{}", sc.depth);
    let ep = elem_plan(
        cx.module,
        &sc.hands[hi].name,
        &elem_ty,
        plan.item.as_ref(),
        &var,
    )?;
    let inner = scope(
        cx,
        &w.element,
        ep.hands.clone(),
        &ep.constants,
        sc.depth + 1,
    )?;
    Ok((inner, var, ep.ctor))
}

/// The BML type a carried wire field is bound by in the table: its type name, `[]` after a
/// repeated one.
fn wire_name(w: &PacketShape) -> String {
    let base = w
        .ref_
        .as_ref()
        .map(|r| r.name.clone())
        .unwrap_or_else(|| w.wire.to_lowercase());
    if w.repeat_mode.is_some() {
        format!("{base}[]")
    } else {
        base
    }
}

fn carrier_of(
    codec: &Option<String>,
    put: &Option<String>,
    take: &Option<String>,
) -> Option<Option<Carrier>> {
    match (codec, put, take) {
        (Some(c), None, None) => Some(Some(Carrier::Codec(c.clone()))),
        (None, Some(put), Some(take)) => Some(Some(Carrier::Direct {
            put: put.clone(),
            take: take.clone(),
        })),
        (None, None, None) => Some(None),
        _ => None,
    }
}

fn plan_hand(
    cx: &mut Cx,
    hand: &Hand,
    wires: &[PacketShape],
    carried: &[usize],
) -> BmlResult<HandPlan> {
    let module = cx.module;
    let ty = HandTy::parse(&hand.ty);
    let bind = &hand.bind;
    let names: Vec<String> = carried.iter().map(|&i| wire_name(&wires[i])).collect();
    let first_guarded = carried.first().is_some_and(|&i| wires[i].guard.is_some());
    let guarded = carried
        .iter()
        .filter(|&&i| wires[i].guard.is_some())
        .count();

    // The table binds the type the wires carry: the `Option`'s content when the first carried
    // wire is guarded, else the field's own type.
    let lookup_ty = match (&ty, first_guarded) {
        (HandTy::Option(inner), true) => (**inner).clone(),
        (t, _) => t.clone(),
    };
    let tb = cx.bindings.lookup(&lookup_ty.text(), &names).cloned();
    let own_carrier = carrier_of(&bind.codec, &bind.put, &bind.take).ok_or_else(|| {
        emit(format!(
            "{module}: field {} binds a codec, or both put and take, not a mix",
            hand.name
        ))
    })?;
    let table_carrier = tb
        .as_ref()
        .and_then(|t| carrier_of(&t.codec, &t.put, &t.take))
        .flatten();
    let codec = own_carrier.clone().or(table_carrier);

    // A carrier owns its whole span, guards inside it included; only the first wire's guard
    // decides whether the span is there at all.
    let opt_layer = if codec.is_some() {
        // An option the carrier fills; a plain field's carrier owns the guards in its span.
        first_guarded
            && (matches!(ty, HandTy::Option(_)) || bind.guarded_default || bind.absent.is_some())
            && !bind.whole_span
    } else {
        if guarded != 0 && guarded != carried.len() {
            return Err(emit(format!(
                "{module}: field {} carries guarded and unguarded wire fields",
                hand.name
            )));
        }
        guarded > 0
    };
    let defaulted = bind.guarded_default || bind.absent.is_some();
    let inner = match (&ty, opt_layer) {
        (HandTy::Option(inner), true) if !defaulted => (**inner).clone(),
        (t, true) if defaulted => t.clone(),
        (_, true) => {
            return Err(emit(format!(
                "{module}: field {} carries guarded wire fields and must be an Option, not {}",
                hand.name, hand.ty
            )));
        }
        (t, false) => t.clone(),
    };
    let has_repeat = carried.iter().any(|&i| wires[i].repeat_mode.is_some());
    let kind = if has_repeat && codec.is_none() {
        if carried.len() != 1 {
            return Err(emit(format!(
                "{module}: field {} carries a repeated wire field and others; bind a codec or split it",
                hand.name
            )));
        }
        Kind::List
    } else {
        Kind::Value
    };
    let mut writes: Vec<Option<String>> = vec![None; carried.len()];
    let mut read = None;
    let mut used_table = codec.is_some() && own_carrier.is_none();
    if kind == Kind::Value && codec.is_none() {
        let spec = bind
            .write
            .clone()
            .or_else(|| tb.as_ref().and_then(|t| t.write.clone()));
        if bind.write.is_none() && spec.is_some() {
            used_table = true;
        }
        match spec {
            None => {
                if carried.len() > 1 {
                    return Err(emit(format!(
                        "{module}: field {} carries {} wire fields ({}); give it \"write\" and \"read\" templates",
                        hand.name,
                        carried.len(),
                        names.join(", ")
                    )));
                }
            }
            Some(WriteSpec::One(t)) if carried.len() == 1 => writes[0] = Some(t),
            Some(WriteSpec::List(l)) if l.len() == carried.len() => {
                writes = l.into_iter().map(Some).collect();
            }
            Some(WriteSpec::Map(m)) => {
                for (k, &i) in carried.iter().enumerate() {
                    match m.get(&wires[i].ident).or_else(|| m.get(&wires[i].java)) {
                        Some(t) => writes[k] = Some(t.clone()),
                        None => {
                            return Err(emit(format!(
                                "{module}: field {}: no write template for wire field {}",
                                hand.name, wires[i].ident
                            )));
                        }
                    }
                }
            }
            Some(_) => {
                return Err(emit(format!(
                    "{module}: field {}: the write templates do not match its {} wire fields",
                    hand.name,
                    carried.len()
                )));
            }
        }
        read = bind.read.clone().or_else(|| {
            let r = tb.as_ref().and_then(|t| t.read.clone());
            if r.is_some() && bind.write.is_none() {
                used_table = true;
            }
            r
        });
    }
    if used_table && let Some(t) = &tb {
        cx.imports.extend(t.imports.iter().cloned());
    }
    Ok(HandPlan {
        ty,
        inner,
        opt_layer,
        write_whole: (bind.write_whole || defaulted) && opt_layer,
        read_flat: bind.read_flat && opt_layer,
        guarded_default: (bind.guarded_default || bind.absent.is_some()) && opt_layer,
        absent: bind.absent.clone(),
        codec,
        kind,
        writes,
        read,
        item: bind.item.clone(),
    })
}

/// The type expression a plain wire field is written and read with.
pub fn frame(cx: &Cx, w: &PacketShape) -> BmlResult<String> {
    match frame_expr(w, cx.resolved)? {
        Some(s) if s != "RAW_BYTES" => Ok(s),
        Some(_) => Err(emit(format!(
            "{}: wire field {} has no fixed length; bind a codec for it",
            cx.module, w.ident
        ))),
        None if w.opaque => Err(emit(format!(
            "{}: wire field {} ({}) is an opaque payload; bind a codec for it",
            cx.module,
            w.ident,
            wire_name(w)
        ))),
        None => Ok(w.wire.clone()),
    }
}

/// The value expression of a derived or plain write of wire field `j`, over `place`.
/// `place` is the expression of the hand value and whether it is a reference.
pub fn write_value(cx: &mut Cx, sc: &Scope, j: usize) -> BmlResult<(String, Option<String>)> {
    let w = &sc.wires[j];
    match &sc.own.roles[j] {
        Role::Const(e) => Ok((e.clone(), None)),
        Role::Flag(h) => Ok((format!("{}.is_some()", sc.hands[*h].place), None)),
        Role::Count(h) => {
            let hand = &sc.hands[*h];
            let plan = &sc.plans[*h];
            let cast = &w.rtype;
            if plan.opt_layer && !plan.write_whole {
                Ok((format!("v.len() as {cast}"), Some(hand.place.clone())))
            } else {
                Ok((format!("{}.len() as {cast}", hand.place), None))
            }
        }
        Role::Carried(h, k) => {
            let hand = &sc.hands[*h];
            let plan = &sc.plans[*h];
            let (place, is_ref, opt) = if plan.opt_layer && !plan.write_whole {
                ("v".to_string(), true, Some(hand.place.clone()))
            } else {
                (hand.place.clone(), hand.is_ref, None)
            };
            let value = match &plan.writes[*k] {
                Some(t) => super::instantiate(t, std::slice::from_ref(&place)),
                None => {
                    let inner = if plan.write_whole {
                        plan.ty.text()
                    } else {
                        plan.inner.text()
                    };
                    if !same_type(&inner, &w.rtype)
                        && plan.codec.is_none()
                        && w.repeat_mode.is_none()
                    {
                        return Err(emit(format!(
                            "{}: field {}: hand type {} does not match wire type {} of {}; \
                             bind a codec or write/read templates",
                            cx.module, hand.name, inner, w.rtype, w.ident
                        )));
                    }
                    by_value(&place, is_ref, &inner, &cx.bindings.copy)
                }
            };
            Ok((value, opt))
        }
    }
}

/// `ty` with every path reduced to its last segment (`crate::network::uuid::Uuid` is `Uuid`).
fn unpathed(ty: &str) -> String {
    let mut out = String::with_capacity(ty.len());
    let mut seg = String::new();
    let mut chars = ty.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_alphanumeric() || c == '_' {
            seg.push(c);
        } else if c == ':' && chars.peek() == Some(&':') {
            chars.next();
            seg.clear();
        } else {
            out.push_str(&seg);
            seg.clear();
            out.push(c);
        }
    }
    out.push_str(&seg);
    out
}

/// Whether hand type `hand` and wire type `wire` are the same type, however the hand spells paths.
pub fn same_type(hand: &str, wire: &str) -> bool {
    unpathed(hand) == unpathed(wire)
}
