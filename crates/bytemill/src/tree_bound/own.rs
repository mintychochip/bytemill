//! Which hand field carries each wire field of a scope.

use super::hand::Hand;
use crate::ast::Expr;
use crate::emit_rstom::{PacketShape, ident};
use crate::errors::{BmlResult, emit};
use crate::ordmap::OrderedMap;

/// What a wire field is to the hand side.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    /// The hand field's value, or one part of it: the hand field's index and the wire field's
    /// position among the ones it carries.
    Carried(usize, usize),
    /// The count of a list the hand field holds, derived from the list.
    Count(usize),
    /// The presence flag of an option the hand field holds, derived from the option.
    Flag(usize),
    /// No hand field carries it; the write puts this expression there, a read drops it.
    Const(String),
}

#[derive(Debug, Clone)]
pub struct Ownership {
    pub roles: Vec<Role>,
    /// Per hand field, the indexes of the wire fields it carries, in wire order.
    pub carried: Vec<Vec<usize>>,
}

/// The boolean field a presence guard reads.
pub fn guard_flag(guard: Option<&Expr>) -> Option<String> {
    match guard? {
        Expr::Ident(n) => Some(ident(n)),
        _ => None,
    }
}

fn position(wires: &[PacketShape], name: &str) -> Option<usize> {
    let id = ident(name);
    wires.iter().position(|w| w.ident == id)
}

/// The wire fields no other wire field derives: not a count, not a presence flag.
fn value_wires(wires: &[PacketShape]) -> Vec<usize> {
    let mut derived = vec![false; wires.len()];
    for w in wires {
        if let Some(c) = &w.count
            && let Some(i) = position(wires, c)
        {
            derived[i] = true;
        }
        if let Some(f) = guard_flag(w.guard.as_ref())
            && let Some(i) = position(wires, &f)
        {
            derived[i] = true;
        }
    }
    (0..wires.len()).filter(|i| !derived[*i]).collect()
}

/// Assign every wire field of `wires` to a hand field, a derived role or a constant.
pub fn assign(
    module: &str,
    wires: &[PacketShape],
    hands: &[Hand],
    constants: &OrderedMap<String>,
) -> BmlResult<Ownership> {
    let mut roles: Vec<Option<Role>> = vec![None; wires.len()];
    let mut carried: Vec<Vec<usize>> = vec![Vec::new(); hands.len()];
    let values = value_wires(wires);
    let single = hands.len() == 1 && hands[0].name == "_";
    let all_slots = !hands.is_empty() && hands.iter().all(|h| h.name.starts_with('_'));

    for (hi, h) in hands.iter().enumerate() {
        let idents: Vec<String> = match &h.bind.wire {
            Some(list) => list.clone(),
            None if single => values.iter().map(|&i| wires[i].ident.clone()).collect(),
            None if all_slots => {
                let slot: usize = h.name[1..].parse().unwrap_or(usize::MAX);
                match values.get(slot) {
                    Some(&i) => vec![wires[i].ident.clone()],
                    None => {
                        return Err(emit(format!(
                            "{module}: tuple slot {slot} has no wire field; the element wires \
                             are {}",
                            names(wires)
                        )));
                    }
                }
            }
            None => match position(wires, &h.name) {
                Some(i) => vec![wires[i].ident.clone()],
                None => {
                    return Err(emit(format!(
                        "{module}: hand field {} has no wire field of that name; bind it with \
                         \"wire\" (the wire fields are {})",
                        h.name,
                        names(wires)
                    )));
                }
            },
        };
        let mut slot = 0;
        for name in &idents {
            for i in resolve(module, wires, &h.name, name)? {
                if roles[i].is_some() {
                    return Err(emit(format!(
                        "{module}: wire field {} is carried by two hand fields",
                        wires[i].ident
                    )));
                }
                roles[i] = Some(Role::Carried(hi, slot));
                carried[hi].push(i);
                slot += 1;
            }
        }
        carried[hi].sort_unstable();
    }

    // A counted list's count and an option's flag come from the list or the option.
    for hi in 0..hands.len() {
        for &i in &carried[hi].clone() {
            let w = &wires[i];
            // A wire the shape gives a value for is a constant, not something to derive.
            let is_constant = |i: usize| {
                constants.contains_key(&wires[i].ident) || constants.contains_key(&wires[i].java)
            };
            if let Some(c) = &w.count
                && let Some(ci) = position(wires, c)
                && !is_constant(ci)
            {
                derive(&mut roles, &mut carried, ci, hi, Role::Count(hi));
            }
            if let Some(f) = guard_flag(w.guard.as_ref())
                && let Some(fi) = position(wires, &f)
                && !is_constant(fi)
            {
                derive(&mut roles, &mut carried, fi, hi, Role::Flag(hi));
            }
        }
    }

    // Counts and flags taken off a field's list leave its carried wire fields renumbered.
    for (hi, list) in carried.iter().enumerate() {
        for (k, &i) in list.iter().enumerate() {
            roles[i] = Some(Role::Carried(hi, k));
        }
    }

    for (i, w) in wires.iter().enumerate() {
        if roles[i].is_some() {
            continue;
        }
        match constants.get(&w.ident).or_else(|| constants.get(&w.java)) {
            Some(expr) => roles[i] = Some(Role::Const(expr.clone())),
            None => {
                return Err(emit(format!(
                    "{module}: wire field {} is carried by no hand field; add it to a field's \
                     \"wire\" or give it a value under \"constants\"",
                    w.ident
                )));
            }
        }
    }
    for (key, _) in constants.iter() {
        let used = wires.iter().zip(&roles).any(|(w, role)| {
            (w.ident == *key || w.java == *key) && matches!(role, Some(Role::Const(_)))
        });
        if !used {
            return Err(emit(format!(
                "{module}: constant {key} is not the value of any wire field the hand fields \
                 leave uncarried (the wire fields are {})",
                names(wires)
            )));
        }
    }
    Ok(Ownership {
        roles: roles.into_iter().map(Option::unwrap).collect(),
        carried,
    })
}

/// The wire fields a `wire` entry names: one ident, `a..b` (every wire field from `a` to `b`, in
/// wire order) or `@field` (every wire field flattened out of the BML field `field`).
fn resolve(module: &str, wires: &[PacketShape], hand: &str, entry: &str) -> BmlResult<Vec<usize>> {
    let unknown = |what: &str| {
        emit(format!(
            "{module}: hand field {hand} binds unknown wire field {what} (the wire fields are {})",
            names(wires)
        ))
    };
    if let Some(parent) = entry.strip_prefix('@') {
        let found: Vec<usize> = wires
            .iter()
            .enumerate()
            .filter(|(_, w)| w.parent == parent || w.parent.starts_with(&format!("{parent}.")))
            .map(|(i, _)| i)
            .collect();
        if found.is_empty() {
            return Err(unknown(entry));
        }
        return Ok(found);
    }
    if let Some((from, to)) = entry.split_once("..") {
        let a = position(wires, from).ok_or_else(|| unknown(from))?;
        let b = position(wires, to).ok_or_else(|| unknown(to))?;
        if a > b {
            return Err(emit(format!(
                "{module}: hand field {hand}: {entry} runs backwards"
            )));
        }
        return Ok((a..=b).collect());
    }
    position(wires, entry)
        .map(|i| vec![i])
        .ok_or_else(|| unknown(entry))
}

fn derive(
    roles: &mut [Option<Role>],
    carried: &mut [Vec<usize>],
    at: usize,
    hi: usize,
    role: Role,
) {
    match &roles[at] {
        None => roles[at] = Some(role),
        // Listed under the same hand field: the hand field derives it rather than carrying it.
        Some(Role::Carried(owner, _)) if *owner == hi => {
            carried[hi].retain(|&i| i != at);
            roles[at] = Some(role);
        }
        // Carried by another field or already derived: leave it.
        Some(_) => {}
    }
}

fn names(wires: &[PacketShape]) -> String {
    wires
        .iter()
        .map(|w| w.ident.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
