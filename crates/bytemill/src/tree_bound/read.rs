//! The read half of a bound packet: statements in BML order that read each wire field into a local,
//! and the expression that builds each hand field from those locals.

use super::guard::{Types, cond, local};
use super::hand::{HandTy, local_name};
use super::own::Role;
use super::plan::{Carrier, Cx, Kind, Scope, elem_scope, frame};
use super::write::absorbed;
use crate::errors::{BmlResult, emit};

/// The statements of a scope's reads and the expression of each hand field, in hand order.
pub struct ScopeRead {
    pub stmts: Vec<String>,
    pub exprs: Vec<String>,
}

pub fn scope_reads(cx: &mut Cx, sc: &Scope, outer: &Types) -> BmlResult<ScopeRead> {
    let mut types = outer.clone();
    let mut stmts: Vec<String> = Vec::new();
    // A hand field converts when its last wire field has been read, as the field reads in
    // the hand serializer do: a conversion that fails does so before later fields are consumed.
    let last_wire: Vec<Option<usize>> = (0..sc.hands.len())
        .map(|h| sc.own.carried[h].iter().copied().max())
        .collect();
    let mut eager: Vec<Option<String>> = vec![None; sc.hands.len()];
    let mut guard_names = std::collections::BTreeSet::new();
    for (j, w) in sc.wires.iter().enumerate() {
        if let Some(g) = &w.guard
            && !super::write::absorbed(sc, j)
        {
            super::guard::idents(g, &mut guard_names);
        }
    }
    for (j, w) in sc.wires.iter().enumerate() {
        if absorbed(sc, j) {
            continue;
        }
        let owned_span = matches!(&sc.own.roles[j], Role::Carried(h, _)
            if sc.plans[*h].codec.is_some() && !sc.plans[*h].opt_layer);
        let guard = match &w.guard {
            Some(g) if !owned_span => Some(cond(g, &types)?),
            _ => None,
        };
        let name = local(&w.ident);
        match &sc.own.roles[j] {
            Role::Carried(h, _) if sc.plans[*h].kind == Kind::List => {
                stmts.push(list_read(cx, sc, *h, j, guard.as_deref(), &types)?);
                types.insert(&w.ident, &w.rtype, w.guard.is_some());
            }
            role => {
                let read = match role {
                    Role::Carried(h, _) => match &sc.plans[*h].codec {
                        Some(Carrier::Codec(c)) => format!("buffer.read({c})"),
                        Some(Carrier::Direct { take, .. }) => take.clone(),
                        None => format!("buffer.read({})", frame(cx, w)?),
                    },
                    _ => format!("buffer.read({})", frame(cx, w)?),
                };
                let is_const = matches!(role, Role::Const(_));
                let kept = is_const
                    && (crate::emit_rstom::has_word(&cx.template_text, &name)
                        || guard_names.contains(&w.ident));
                stmts.push(match (&guard, is_const && !kept) {
                    (None, true) => format!("let _ = {read};"),
                    (Some(g), true) => format!("if {g} {{\nlet _ = {read};\n}}"),
                    (None, false) => format!("let {name} = {read};"),
                    (Some(g), false) => {
                        format!("let {name} = if {g} {{ Some({read}) }} else {{ None }};")
                    }
                });
                types.insert(&w.ident, &w.rtype, w.guard.is_some() && !owned_span);
            }
        }
        for h in 0..sc.hands.len() {
            if last_wire[h] != Some(j)
                || sc.plans[h].kind != Kind::Value
                || sc.plans[h].codec.is_some()
            {
                continue;
            }
            let expr = hand_expr(cx, sc, h)?;
            let plain = sc.own.carried[h]
                .iter()
                .any(|&i| local(&sc.wires[i].ident) == expr);
            if !plain {
                let name = format!("{}_val", local_name(&sc.hands[h]));
                stmts.push(format!("let {name} = {expr};"));
                eager[h] = Some(name);
            }
        }
    }
    let mut flat = Vec::new();
    for (h, eager_name) in eager.iter().enumerate() {
        flat.push(match eager_name {
            Some(name) => name.clone(),
            None => hand_expr(cx, sc, h)?,
        });
    }
    let exprs = sc
        .slots
        .iter()
        .map(|slot| super::hand::resolve(slot, &flat, &mut stmts))
        .collect();
    Ok(ScopeRead { stmts, exprs })
}

/// `expr` over `local` mapped through an option: `local.map(f)` when `expr` is a call of `f` on
/// the local alone; `and_then` when `expr` is an option itself.
fn map_option(local: &str, expr: &str, flat: bool) -> String {
    if expr == local {
        return local.to_string();
    }
    let method = if flat { "and_then" } else { "map" };
    if let Some(path) = expr.strip_suffix(&format!("({local})")).filter(|p| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
    }) {
        return format!("{local}.{method}({path})");
    }
    format!("{local}.{method}(|{local}| {expr})")
}

fn hand_expr(cx: &mut Cx, sc: &Scope, h: usize) -> BmlResult<String> {
    let hand = &sc.hands[h];
    let plan = &sc.plans[h];
    let carried = &sc.own.carried[h];
    if carried.is_empty() {
        return match &hand.bind.read {
            Some(t) => Ok(super::instantiate(t, &[])),
            None => Err(emit(format!(
                "{}: field {} carries no wire field; give it a \"read\" expression",
                cx.module, hand.name
            ))),
        };
    }
    let locals: Vec<String> = carried.iter().map(|&i| local(&sc.wires[i].ident)).collect();
    if plan.kind == Kind::List || plan.codec.is_some() {
        // A carrier's read lands in the local of the first wire it carries; a carrier that
        // yields an option under a guard leaves one option too many.
        if plan.read_flat && plan.codec.is_some() {
            return Ok(format!("{}.flatten()", locals[0]));
        }
        if plan.guarded_default {
            return Ok(unwrap_absent(&locals[0], plan.absent.as_deref()));
        }
        return Ok(locals[0].clone());
    }
    let inner = match &plan.read {
        Some(t) => super::instantiate(t, &locals),
        None if locals.len() == 1 => {
            let w = &sc.wires[carried[0]];
            let inner_ty = plan.inner.text();
            if !super::plan::same_type(&inner_ty, &w.rtype) && w.repeat_mode.is_none() {
                return Err(emit(format!(
                    "{}: field {}: hand type {} does not match wire type {} of {}; \
                     bind a codec or write/read templates",
                    cx.module, hand.name, inner_ty, w.rtype, w.ident
                )));
            }
            locals[0].clone()
        }
        None => {
            return Err(emit(format!(
                "{}: field {} carries {} wire fields; give it a \"read\" template",
                cx.module,
                hand.name,
                locals.len()
            )));
        }
    };
    if !plan.opt_layer {
        return Ok(inner);
    }
    if plan.guarded_default {
        return Ok(unwrap_absent(
            &map_option(&locals[0], &inner, false),
            plan.absent.as_deref(),
        ));
    }
    if locals.len() == 1 {
        return Ok(map_option(&locals[0], &inner, plan.read_flat));
    }
    let pats = locals
        .iter()
        .map(|l| format!("Some({l})"))
        .collect::<Vec<_>>()
        .join(", ");
    let built = if plan.read_flat {
        inner
    } else {
        format!("Some({inner})")
    };
    Ok(format!(
        "if let ({pats}) = ({}) {{ {built} }} else {{ None }}",
        locals.join(", ")
    ))
}

fn list_read(
    cx: &mut Cx,
    sc: &Scope,
    h: usize,
    j: usize,
    guard: Option<&str>,
    types: &Types,
) -> BmlResult<String> {
    let w = &sc.wires[j];
    let plan = &sc.plans[h];
    let (inner, _, ctor) = elem_scope(cx, sc, h, j)?;
    let HandTy::Vec(elem_ty) = &plan.inner else {
        return Err(emit(format!(
            "{}: {} is not a Vec",
            cx.module,
            plan.inner.text()
        )));
    };
    let name = local(&w.ident);
    let read = super::read::scope_reads(cx, &inner, types)?;
    let mut body = read.stmts.join("\n");
    for (k, eh) in inner.orig.iter().enumerate() {
        let l = local_name(eh);
        if read.exprs[k] != l {
            body.push_str(&format!("\nlet {l} = {};", read.exprs[k]));
        }
    }
    body.push_str(&format!("\n{name}.push({ctor});"));
    let head = match w.repeat_mode.as_deref() {
        Some("count") => {
            let count = w.count.clone().unwrap_or_default();
            let bare = count.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            let count = if bare && !count.is_empty() {
                let l = local(&crate::emit_rstom::ident(&count));
                match types.0.get(&crate::emit_rstom::ident(&count)) {
                    Some((_, true)) => format!("{l}.unwrap_or(0)"),
                    _ => l,
                }
            } else {
                count
            };
            format!("for _ in 0..{count}")
        }
        _ => "while buffer.readable_bytes() > 0".to_string(),
    };
    let build = format!(
        "let mut {name}: Vec<{}> = Vec::new();\n{head} {{\n{body}\n}}",
        elem_ty.text()
    );
    Ok(match guard {
        None => build,
        Some(g) => format!("let {name} = if {g} {{\n{build}\nSome({name})\n}} else {{ None }};"),
    })
}

/// `option` with what a skipped guard leaves: the shape's `absent` expression, else the default.
fn unwrap_absent(option: &str, absent: Option<&str>) -> String {
    match absent {
        Some(a) => format!("{option}.unwrap_or_else(|| {a})"),
        None => format!("{option}.unwrap_or_default()"),
    }
}
