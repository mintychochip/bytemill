//! Bound packets: `emit rstom-tree` bodies written over hand fields that do not mirror the wire.
//!
//! A shape binds each hand field to the BML wire fields it carries (`wire`), and says how the
//! hand value converts to and from them: a `codec` (a `Type` whose value is the hand type), or
//! `write` and `read` templates. The bindings table (`bindings.json`) holds the conversions that
//! hold for a hand type everywhere; a shape's own keys win. Lists bind their elements the same
//! way, one scope per level.

mod guard;
mod hand;
mod own;
mod plan;
mod read;
mod write;

use crate::ast::ResolvedSchema;
use crate::emit_rstom::{PacketShape, WIRE_TYPES, has_word, packet_fields};
use crate::errors::BmlResult;
use crate::minestom::Unit;
use crate::shapes::model::Shape;
use crate::tree_bind::Bindings;
use std::collections::BTreeSet;

pub use hand::{HandTy, is_copy};

/// The bodies of a bound packet, ready for `tree_packet`.
#[derive(Debug, Clone)]
pub struct BoundBody {
    pub fields: Vec<PacketShape>,
    pub writes: String,
    pub reads: String,
    /// Per hand field, the expression that builds it from the read locals.
    pub ctor: Vec<String>,
    pub used_wires: Vec<String>,
    pub needs_fixed_raw_bytes: bool,
    /// `use` lines the conversions need.
    pub imports: Vec<String>,
}

/// `template` with `$` (or `$0`) replaced by `subs[0]` and `$N` by `subs[N]`.
pub fn instantiate(template: &str, subs: &[String]) -> String {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut num = String::new();
        while j < chars.len() && chars[j].is_ascii_digit() {
            num.push(chars[j]);
            j += 1;
        }
        let idx: usize = if num.is_empty() {
            0
        } else {
            num.parse().unwrap_or(usize::MAX)
        };
        match subs.get(idx) {
            Some(s) => out.push_str(s),
            None => out.push_str(&chars[i..j].iter().collect::<String>()),
        }
        i = j;
    }
    out
}

pub fn bound_body(
    unit: &Unit,
    resolved: &ResolvedSchema,
    shape: &Shape,
    bindings: &Bindings,
) -> BmlResult<BoundBody> {
    let (wires, _) = packet_fields(&unit.pkt, resolved)?;
    let mut template_text = String::new();
    collect_text(&shape.fields, &mut template_text);
    for (_, v) in shape.constants.iter() {
        template_text.push_str(&format!("{v}\n"));
    }
    for (_, lines) in shape.hooks.iter() {
        template_text.push_str(&lines.join("\n"));
        template_text.push('\n');
    }
    let mut cx = plan::Cx {
        module: &shape.module,
        resolved,
        bindings,
        imports: BTreeSet::new(),
        template_text,
    };
    let hands = hand::packet_hands(&shape.fields);
    let sc = plan::scope(&mut cx, &wires, hands, &shape.constants, 0)?;
    let outer = guard::Types::default();
    let writes = write::scope_writes(&mut cx, &sc, &outer)?.join("\n");
    let read = read::scope_reads(&mut cx, &sc, &outer)?;
    let reads = read.stmts.join("\n");
    let writes = if writes.is_empty() {
        "let _ = (buffer, value);".to_string()
    } else {
        writes
    };
    let reads = if reads.is_empty() {
        "let _ = buffer;".to_string()
    } else {
        reads
    };
    let text = format!("{writes}\n{reads}");
    let mut used_wires: BTreeSet<String> = BTreeSet::new();
    for (_, constant, _) in WIRE_TYPES {
        if has_word(&text, constant) {
            used_wires.insert((*constant).to_string());
        }
    }
    let needs_fixed_raw_bytes = text.contains("fixed_raw_bytes(");
    Ok(BoundBody {
        fields: wires,
        writes,
        reads,
        ctor: read.exprs,
        used_wires: used_wires.into_iter().collect(),
        needs_fixed_raw_bytes,
        imports: cx.imports.into_iter().collect(),
    })
}

/// The wire fields of a packet as one line each, nested elements indented: what a shape's
/// `wire` idents name.
pub fn describe(wires: &[PacketShape]) -> String {
    fn line(w: &PacketShape, depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        let ty = w
            .ref_
            .as_ref()
            .map_or_else(|| w.wire.to_lowercase(), |r| r.name.clone());
        let mut text = format!("{pad}{}: {ty}", w.ident);
        if let Some(mode) = &w.repeat_mode {
            text.push_str(&format!("[] {mode}"));
            if let Some(c) = &w.count {
                text.push_str(&format!(" count={c}"));
            }
        }
        if let Some(g) = &w.guard {
            text.push_str(&format!(" if {}", guard::expr_text(g)));
        }
        if w.opaque {
            text.push_str(" (opaque)");
        }
        if !w.parent.is_empty() {
            text.push_str(&format!(" @{}", w.parent));
        }
        text.push_str(&format!("  -> {}\n", w.rtype));
        out.push_str(&text);
        for e in &w.element {
            line(e, depth + 1, out);
        }
    }
    let mut out = String::new();
    for w in wires {
        line(w, 0, &mut out);
    }
    out
}

/// Every template of the binds of `fields`, element binds included.
fn collect_text(fields: &[crate::shapes::model::FieldShape], out: &mut String) {
    use crate::shapes::model::WriteSpec;
    for f in fields {
        let b = &f.bind;
        for t in [&b.read, &b.take, &b.put].into_iter().flatten() {
            out.push_str(t);
            out.push('\n');
        }
        match &b.write {
            Some(WriteSpec::One(t)) => out.push_str(t),
            Some(WriteSpec::List(l)) => out.push_str(&l.join("\n")),
            Some(WriteSpec::Map(m)) => {
                for (_, v) in m.iter() {
                    out.push_str(v);
                    out.push('\n');
                }
            }
            None => {}
        }
        for item in [&b.item, &b.nest].into_iter().flatten() {
            if let Some(c) = &item.ctor {
                out.push_str(c);
                out.push('\n');
            }
            collect_text(&item.fields, out);
        }
    }
}
