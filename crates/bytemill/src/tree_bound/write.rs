//! The write half of a bound packet: statements in BML order that put each wire field on the buffer.

use super::guard::{Types, cond, idents, local};
use super::own::Role;
use super::plan::{Carrier, Cx, Kind, Scope, elem_scope, frame, write_value};
use crate::ast::Expr;
use crate::emit_rstom::PacketShape;
use crate::errors::{BmlResult, emit};
use std::collections::BTreeSet;

/// One statement and what it is conditional on.
struct Item {
    guard: Option<Expr>,
    /// The option place it unwraps into `v`.
    opt: Option<String>,
    text: String,
}

/// The idents guards under `wires` (nested elements included) read.
fn nested_guard_refs(wires: &[PacketShape], out: &mut BTreeSet<String>) {
    for w in wires {
        if let Some(g) = &w.guard {
            idents(g, out);
        }
        nested_guard_refs(&w.element, out);
    }
}

/// The idents the guards of the wire fields `sc` writes itself read: the ones a carrier owns
/// guard nothing the serializer evaluates.
fn guard_refs(sc: &Scope, out: &mut BTreeSet<String>) {
    for (j, w) in sc.wires.iter().enumerate() {
        if absorbed(sc, j) {
            continue;
        }
        if let Some(g) = &w.guard {
            idents(g, out);
        }
        nested_guard_refs(&w.element, out);
    }
}

/// Whether wire `j` is written as part of another wire field's codec rather than on its own.
pub(super) fn absorbed(sc: &Scope, j: usize) -> bool {
    match &sc.own.roles[j] {
        Role::Count(h) => sc.plans[*h].codec.is_some(),
        // A flag inside the carrier's own encoding is its business; one in front of an option
        // the carrier fills is not.
        Role::Flag(h) => sc.plans[*h].codec.is_some() && !sc.plans[*h].opt_layer,
        Role::Carried(h, k) => sc.plans[*h].codec.is_some() && *k > 0,
        _ => false,
    }
}

/// The statements that write every wire field of `sc`.
pub fn scope_writes(cx: &mut Cx, sc: &Scope, outer: &Types) -> BmlResult<Vec<String>> {
    let mut types = outer.clone();
    for w in sc.wires {
        if w.repeat_mode.is_none() {
            types.insert(&w.ident, &w.rtype, false);
        }
    }
    let mut refs = BTreeSet::new();
    guard_refs(sc, &mut refs);

    let mut out: Vec<String> = Vec::new();
    let mut by_local: Vec<Option<String>> = vec![None; sc.wires.len()];
    for (j, w) in sc.wires.iter().enumerate() {
        if !refs.contains(&w.ident) || w.repeat_mode.is_some() || absorbed(sc, j) {
            continue;
        }
        let (value, opt) = write_value(cx, sc, j)?;
        if opt.is_some() {
            return Err(emit(format!(
                "{}: wire field {} is read by a guard but is only written inside an option; \
                 carry it with a field that is always present",
                cx.module, w.ident
            )));
        }
        let name = local(&w.ident);
        out.push(format!("let {name} = {value};"));
        by_local[j] = Some(name);
    }

    let mut items: Vec<Item> = Vec::new();
    for (j, w) in sc.wires.iter().enumerate() {
        if absorbed(sc, j) {
            continue;
        }
        let mut guard = w.guard.clone();
        // A carrier that fills a plain field owns the guards inside its span.
        if let Role::Carried(h, _) = &sc.own.roles[j]
            && sc.plans[*h].codec.is_some()
            && !sc.plans[*h].opt_layer
        {
            guard = None;
        }
        match &sc.own.roles[j] {
            Role::Carried(h, _) if sc.plans[*h].kind == Kind::List => {
                let (text, opt) = list_write(cx, sc, *h, j, &types)?;
                items.push(Item { guard, opt, text });
            }
            role => {
                if let Role::Carried(h, _) = role
                    && let Some(Carrier::Direct { put, .. }) = &sc.plans[*h].codec
                {
                    let (place, opt) = direct_place(sc, *h);
                    let text = super::instantiate(put, std::slice::from_ref(&place));
                    items.push(Item { guard, opt, text });
                    continue;
                }
                let (value, opt) = match &by_local[j] {
                    Some(name) => (name.clone(), None),
                    None => write_value(cx, sc, j)?,
                };
                let frame = match role {
                    Role::Carried(h, _) => match &sc.plans[*h].codec {
                        Some(Carrier::Codec(c)) => c.clone(),
                        _ => frame(cx, w)?,
                    },
                    _ => frame(cx, w)?,
                };
                items.push(Item {
                    guard,
                    opt,
                    text: format!("buffer.write({frame}, {value});"),
                });
            }
        }
    }
    collapse_flags(sc, &mut items);
    out.extend(emit_items(items, &types)?);
    Ok(out)
}

/// Drop a guard that is exactly the presence flag of the option the item unwraps.
fn collapse_flags(sc: &Scope, items: &mut [Item]) {
    for item in items.iter_mut() {
        let (Some(Expr::Ident(flag)), Some(opt)) = (&item.guard, &item.opt) else {
            continue;
        };
        let flag = crate::emit_rstom::ident(flag);
        let is_flag_of_opt = sc.wires.iter().enumerate().any(|(j, w)| {
            w.ident == flag
                && matches!(&sc.own.roles[j], Role::Flag(h) if &sc.hands[*h].place == opt)
        });
        if is_flag_of_opt {
            item.guard = None;
        }
    }
}

fn emit_items(items: Vec<Item>, types: &Types) -> BmlResult<Vec<String>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let mut j = i + 1;
        while j < items.len() && items[j].guard == items[i].guard && items[j].opt == items[i].opt {
            j += 1;
        }
        let body = items[i..j]
            .iter()
            .map(|it| it.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let head = match (&items[i].guard, &items[i].opt) {
            (None, None) => {
                out.push(body);
                i = j;
                continue;
            }
            (Some(g), None) => format!("if {}", cond(g, types)?),
            (None, Some(o)) => format!("if let Some(v) = &{o}"),
            (Some(g), Some(o)) => format!("if ({}) && let Some(v) = &{o}", cond(g, types)?),
        };
        out.push(format!("{head} {{\n{body}\n}}"));
        i = j;
    }
    Ok(out)
}

/// The loop that writes a list field's elements, and the option it unwraps.
fn list_write(
    cx: &mut Cx,
    sc: &Scope,
    h: usize,
    j: usize,
    types: &Types,
) -> BmlResult<(String, Option<String>)> {
    let hand = &sc.hands[h];
    let plan = &sc.plans[h];
    let (place, is_ref, opt) = if plan.opt_layer && !plan.write_whole {
        ("v".to_string(), true, Some(hand.place.clone()))
    } else {
        (hand.place.clone(), hand.is_ref, None)
    };
    let (inner, var, _) = elem_scope(cx, sc, h, j)?;
    let iter = match &hand.bind.write {
        Some(crate::shapes::WriteSpec::One(t)) => {
            super::instantiate(t, std::slice::from_ref(&place))
        }
        _ if is_ref => place,
        _ => format!("&{place}"),
    };
    let body = scope_writes(cx, &inner, types)?.join("\n");
    Ok((format!("for {var} in {iter} {{\n{body}\n}}"), opt))
}

/// The place a direct carrier's `put` writes, and the option it unwraps.
fn direct_place(sc: &Scope, h: usize) -> (String, Option<String>) {
    let hand = &sc.hands[h];
    if sc.plans[h].opt_layer && !sc.plans[h].write_whole {
        ("v".to_string(), Some(hand.place.clone()))
    } else {
        (hand.place.clone(), None)
    }
}
