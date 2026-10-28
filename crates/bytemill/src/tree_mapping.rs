//! Pair hand struct fields with BML wire fields and remap generated idents.
//!
//! `mismatch` decides eligibility by name, with a unique-leftover-type
//! fallback; the tree renderer then emits BML-order writes and reads that
//! name the hand fields. Public hand APIs never change.

use crate::emit_rstom::PacketBody;
use crate::shapes::ledger::{mapped_pair_rtype, pair_fields};
use crate::shapes::model::Shape;

/// BML (ident, rtype) pairs from a packet body. Applies the same wrapping as
/// `bml_field_pairs` (via `mapped_pair_rtype`) so both pair sources agree.
pub fn body_pairs(body: &PacketBody) -> Vec<(String, String)> {
    body.fields
        .iter()
        .map(|f| {
            (
                f.ident.clone(),
                mapped_pair_rtype(&f.rtype, f.repeat_mode.is_some(), f.guard.is_some()),
            )
        })
        .collect()
}

/// BML-ident to hand-name renames for this shape and body, or `None` when the
/// fields do not pair (the caller must refuse the swap, like `mismatch`).
pub fn remap_plan(shape: &Shape, body: &PacketBody) -> Option<Vec<(String, String)>> {
    let hand: Vec<(&str, &str)> = shape
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.type_.as_str()))
        .collect();
    let bml = body_pairs(body);
    let map = pair_fields(&hand, &bml)?;
    Some(
        map.iter()
            .enumerate()
            .filter_map(|(i, &j)| {
                let (b, _) = &bml[j];
                let h = hand[i].0;
                if b != h {
                    Some((b.clone(), h.to_string()))
                } else {
                    None
                }
            })
            .collect(),
    )
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
/// Replace whole-word occurrences of each BML ident with the hand name.
/// Whole-word so `id` never corrupts `uuid`; longest BML ident first so
/// overlapping names resolve deterministically. Char-based so non-ASCII
/// doc text passes through untouched.
pub fn remap_idents(text: &str, plan: &[(String, String)]) -> String {
    if plan.is_empty() {
        return text.to_string();
    }
    let mut order: Vec<usize> = (0..plan.len()).collect();
    order.sort_by_key(|&k| std::cmp::Reverse(plan[k].0.len()));
    let chars: Vec<char> = text.chars().collect();
    let needles: Vec<Vec<char>> = plan.iter().map(|(b, _)| b.chars().collect()).collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let mut hit: Option<usize> = None;
        for &k in &order {
            let n = &needles[k];
            if chars[i..].starts_with(n) {
                let left_ok = i == 0 || !is_ident_char(chars[i - 1]);
                let right_ok = i + n.len() == chars.len() || !is_ident_char(chars[i + n.len()]);
                if left_ok && right_ok {
                    hit = Some(k);
                    break;
                }
            }
        }
        if let Some(k) = hit {
            out.push_str(&plan[k].1);
            i += needles[k].len();
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}
