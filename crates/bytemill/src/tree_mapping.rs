//! Pair hand struct fields with BML wire fields and remap generated idents.
//!
//! `mismatch` decides eligibility by name, with a unique-leftover-type
//! fallback; the tree renderer then emits BML-order writes and reads that
//! name the hand fields. Public hand APIs never change.

use crate::emit_rstom::PacketBody;
use crate::shapes::ledger::{DerivedField, DerivedKind, pair_body_fields};
use crate::shapes::model::Shape;

/// The renames and derived accesses a generated packet body needs to name the
/// hand struct's fields.
#[derive(Debug, Clone, Default)]
pub struct RemapPlan {
    pub renames: Vec<(String, String)>,
    pub derived: Vec<DerivedField>,
}

/// Build the remap plan for `shape` over `body`, or `None` when the fields do
/// not pair (the caller must refuse the swap, like `mismatch`).
pub fn remap_plan(shape: &Shape, body: &PacketBody) -> Option<RemapPlan> {
    let pair = pair_body_fields(shape, body.fields.as_slice())?;
    let mut renames = Vec::new();
    for (i, &j) in pair.hand_to_bml.iter().enumerate() {
        let b = &body.fields[j].ident;
        let h = &shape.fields[i].name;
        if b != h {
            renames.push((b.clone(), h.clone()));
        }
    }
    Some(RemapPlan {
        renames,
        derived: pair.derived,
    })
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

/// Replace each derived wire access (`value.<count>`, `value.<flag>`, with any
/// trailing `.clone()`) by the hand expression that produces it.
pub fn apply_derived_writes(text: &str, derived: &[DerivedField]) -> String {
    let mut out = text.to_string();
    for d in derived {
        let expr = match d.kind {
            DerivedKind::Count => format!("value.{}.len() as {}", d.hand, d.rtype),
            DerivedKind::Flag => format!("value.{}.is_some()", d.hand),
        };
        out = replace_value_access(&out, &d.bml, &expr);
    }
    out
}

/// Replace whole-word `value.<ident>` accesses. A directly following
/// `.clone()` is consumed with the access, so the generated write does not
/// clone the derived expression.
fn replace_value_access(text: &str, ident: &str, expr: &str) -> String {
    let needle = format!("value.{ident}");
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = needle.chars().collect();
    let clone: Vec<char> = ".clone()".chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i..].starts_with(&needle) {
            let end = i + needle.len();
            let right_ok = end == chars.len() || !is_ident_char(chars[end]);
            if right_ok {
                out.push_str(expr);
                i = end;
                if chars[i..].starts_with(&clone) {
                    i += clone.len();
                }
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}
