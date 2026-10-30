//! The hand side of a binding: hand type text, and the places a scope reads and builds hand values at.

use crate::errors::{BmlResult, emit};
use crate::shapes::model::{FieldBind, FieldShape, ItemBind};
use std::collections::BTreeSet;

/// A hand field type, as far as projections need to look inside it.
#[derive(Debug, Clone, PartialEq)]
pub enum HandTy {
    Name(String),
    Option(Box<HandTy>),
    Vec(Box<HandTy>),
    Tuple(Vec<HandTy>),
}

/// The text between the `<` after `prefix` and the matching `>` that ends `text`.
fn generic_inner<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(prefix)?.strip_prefix('<')?;
    let inner = rest.strip_suffix('>')?;
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    (depth == 0).then_some(inner)
}

/// The top-level comma-separated parts of `text`.
fn split_top(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        parts.push(&text[start..]);
    }
    parts
}

impl HandTy {
    pub fn parse(text: &str) -> HandTy {
        let text = text.trim();
        if let Some(inner) = generic_inner(text, "Option") {
            return HandTy::Option(Box::new(HandTy::parse(inner)));
        }
        if let Some(inner) = generic_inner(text, "Vec") {
            return HandTy::Vec(Box::new(HandTy::parse(inner)));
        }
        if let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            let parts = split_top(inner);
            if parts.len() >= 2 || (parts.len() == 1 && inner.trim_end().ends_with(',')) {
                return HandTy::Tuple(parts.into_iter().map(HandTy::parse).collect());
            }
        }
        HandTy::Name(text.to_string())
    }

    pub fn text(&self) -> String {
        match self {
            HandTy::Name(n) => n.clone(),
            HandTy::Option(i) => format!("Option<{}>", i.text()),
            HandTy::Vec(i) => format!("Vec<{}>", i.text()),
            HandTy::Tuple(ts) => format!(
                "({})",
                ts.iter().map(HandTy::text).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// Whether a hand type is `Copy` for projections: the primitives, the shared value types and the
/// types the bindings list under `copy`. An unlisted type is cloned, which is correct for every
/// type and only noisy for a `Copy` one.
pub fn is_copy(ty: &str, extra: &BTreeSet<String>) -> bool {
    const COPY: &[&str] = &[
        "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "bool", "Uuid",
        "CoordVec", "usize", "isize",
    ];
    match HandTy::parse(ty) {
        HandTy::Option(inner) => is_copy(&inner.text(), extra),
        HandTy::Vec(_) => false,
        HandTy::Tuple(ts) => ts.iter().all(|t| is_copy(&t.text(), extra)),
        HandTy::Name(n) => {
            // A path-qualified type (`crate::network::uuid::Uuid`) is the type its last segment names.
            let name = n.rsplit("::").next().unwrap_or(&n).to_string();
            COPY.contains(&name.as_str()) || extra.contains(&name)
        }
    }
}

/// A hand field of a scope, with the place its value lives at.
#[derive(Debug, Clone)]
pub struct Hand {
    pub name: String,
    /// The expression of the whole field value: `value.entries`, `item.0`, `item`.
    pub place: String,
    /// Whether `place` is itself a reference (`item` of a `for item in &list` over a one-field
    /// element), so a by-value use needs a deref or a clone.
    pub is_ref: bool,
    pub ty: String,
    pub bind: FieldBind,
}

/// `place` as a value of `ty`: copied when `ty` is `Copy`, else cloned.
pub fn by_value(place: &str, is_ref: bool, ty: &str, copy: &BTreeSet<String>) -> String {
    match (is_copy(ty, copy), is_ref) {
        (true, true) => format!("*{place}"),
        (true, false) => place.to_string(),
        (false, _) => format!("{place}.clone()"),
    }
}

/// The hands of a packet: one per struct field, read off the struct `value`.
pub fn packet_hands(fields: &[FieldShape]) -> Vec<Hand> {
    fields
        .iter()
        .map(|f| Hand {
            name: f.name.clone(),
            place: format!("value.{}", f.name),
            is_ref: false,
            ty: f.type_.clone(),
            bind: f.bind.clone(),
        })
        .collect()
}

/// How a repeat element is built from the values of its hand fields.
#[derive(Debug, Clone)]
pub struct ElemPlan {
    pub hands: Vec<Hand>,
    pub ctor: String,
    pub constants: crate::ordmap::OrderedMap<String>,
}

fn slot_index(name: &str) -> Option<usize> {
    name.strip_prefix('_')?.parse().ok()
}

/// The element hands and constructor of a list field of type `list_ty` (`Vec<T>`), iterating a
/// reference named `var`. `T`'s fields come from the field's `item` binding, else from the
/// tuple or single type.
pub fn elem_plan(
    module: &str,
    field: &str,
    elem_ty: &HandTy,
    item: Option<&ItemBind>,
    var: &str,
) -> BmlResult<ElemPlan> {
    let fields: Vec<FieldShape> = match item {
        Some(i) if !i.fields.is_empty() => i.fields.clone(),
        _ => match elem_ty {
            HandTy::Tuple(ts) => ts
                .iter()
                .enumerate()
                .map(|(i, t)| FieldShape {
                    name: format!("_{i}"),
                    type_: t.text(),
                    ..Default::default()
                })
                .collect(),
            other => vec![FieldShape {
                name: "_".to_string(),
                type_: other.text(),
                ..Default::default()
            }],
        },
    };
    let single = fields.len() == 1 && fields[0].name == "_";
    let tuple = !single && fields.iter().all(|f| slot_index(&f.name).is_some());
    let hands: Vec<Hand> = fields
        .iter()
        .map(|f| {
            let (place, is_ref) = if single {
                (var.to_string(), true)
            } else if let Some(i) = slot_index(&f.name) {
                (format!("{var}.{i}"), false)
            } else {
                (format!("{var}.{}", f.name), false)
            };
            Hand {
                name: f.name.clone(),
                place,
                is_ref,
                ty: f.type_.clone(),
                bind: f.bind.clone(),
            }
        })
        .collect();
    let ctor = match item.and_then(|i| i.ctor.clone()) {
        Some(c) => c,
        None => {
            let names: Vec<String> = hands.iter().map(local_name).collect();
            if single {
                names[0].clone()
            } else if tuple {
                format!("({})", names.join(", "))
            } else if let HandTy::Name(n) = elem_ty {
                format!("{n} {{ {} }}", names.join(", "))
            } else {
                return Err(emit(format!(
                    "{module}: field {field}: give the element of {} an `item` binding with its fields",
                    elem_ty.text()
                )));
            }
        }
    };
    Ok(ElemPlan {
        hands,
        ctor,
        constants: item.map(|i| i.constants.clone()).unwrap_or_default(),
    })
}

/// The local a hand field's value is bound to while a scope builds its element: the field name,
/// or `slot0` for a tuple slot and `elem` for the one-field element.
pub fn local_name(h: &Hand) -> String {
    if h.name == "_" {
        "elem".to_string()
    } else if let Some(i) = slot_index(&h.name) {
        format!("slot{i}")
    } else {
        crate::emit_rstom::ident(&h.name)
    }
}

/// Where the value of one hand field of a scope comes from.
#[derive(Debug, Clone)]
pub enum Slot {
    /// A hand field that binds to wire fields itself: its index among the scope's flat hands.
    Leaf(usize),
    /// A struct built from its own bound fields: each is read into a local the constructor
    /// names.
    Group {
        ctor: String,
        kids: Vec<(String, Slot)>,
    },
}

/// `hands` with every nested struct replaced by its fields, and how to put the structs back.
pub fn flatten(hands: Vec<Hand>) -> (Vec<Hand>, Vec<Slot>) {
    let mut flat = Vec::new();
    let slots = hands.into_iter().map(|h| expand(h, &mut flat)).collect();
    (flat, slots)
}

fn expand(h: Hand, flat: &mut Vec<Hand>) -> Slot {
    let Some(nest) = h.bind.nest.clone() else {
        flat.push(h);
        return Slot::Leaf(flat.len() - 1);
    };
    let mut kids = Vec::new();
    for f in &nest.fields {
        let child = Hand {
            name: f.name.clone(),
            place: format!("{}.{}", h.place, f.name),
            is_ref: false,
            ty: f.type_.clone(),
            bind: f.bind.clone(),
        };
        let local = local_name(&child);
        kids.push((local, expand(child, flat)));
    }
    let ctor = nest.ctor.clone().unwrap_or_else(|| {
        let names: Vec<&str> = kids.iter().map(|(l, _)| l.as_str()).collect();
        format!("{} {{ {} }}", h.ty, names.join(", "))
    });
    Slot::Group { ctor, kids }
}

/// The expression of a slot, with the `let`s its nested structs need appended to `stmts`.
pub fn resolve(slot: &Slot, exprs: &[String], stmts: &mut Vec<String>) -> String {
    match slot {
        Slot::Leaf(i) => exprs[*i].clone(),
        Slot::Group { ctor, kids } => {
            for (local, kid) in kids {
                let e = resolve(kid, exprs, stmts);
                if e != *local {
                    stmts.push(format!("let {local} = {e};"));
                }
            }
            ctor.clone()
        }
    }
}
