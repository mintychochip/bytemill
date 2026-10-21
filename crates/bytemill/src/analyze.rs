//! Schema resolution, porting `bps/analyze.py`.

use crate::ast::*;
use crate::errors::{BpsResult, emit, py_repr_str, resolve};
use crate::ordmap::OrderedMap;
use crate::primitives::is_bare_primitive;
use crate::refs::{Ref, RefKind, item_refs, references, type_refs};
use std::collections::HashSet;

fn dedupe<V>(kind: &str, decls: &[V], name_of: impl Fn(&V) -> &str) -> BpsResult<OrderedMap<V>>
where
    V: Clone,
{
    let mut table: OrderedMap<V> = OrderedMap::new();
    for d in decls {
        let name = name_of(d);
        if table.contains_key(name) {
            return Err(resolve(format!("duplicate {kind} {}", py_repr_str(name))));
        }
        table.insert(name, d.clone());
    }
    Ok(table)
}

pub fn analyze(schema: &Schema) -> BpsResult<ResolvedSchema> {
    let encodings = dedupe("encoding", &schema.encodings, |e| &e.name)?;
    let enums = dedupe("enum", &schema.enums, |e| &e.name)?;
    let packets = dedupe("packet", &schema.packets, |p| &p.name)?;
    let protocols = dedupe("protocol", &schema.protocols, |p| &p.name)?;

    for ref_ in references(schema) {
        resolve_one(&ref_, &encodings, &enums, &packets)?;
    }

    let mut routes = Vec::new();
    for proto in &schema.protocols {
        for route in &proto.routes {
            let pkt = packets
                .get(&route.packet_name)
                .ok_or_else(|| {
                    resolve(format!(
                        "{} references unknown packet {}",
                        ref_where_route(route),
                        py_repr_str(&route.packet_name)
                    ))
                })?
                .clone();
            routes.push((route.clone(), pkt));
        }
    }

    Ok(ResolvedSchema {
        schema: schema.clone(),
        encodings,
        enums,
        packets,
        protocols,
        routes,
        frames_cache: std::cell::RefCell::new(None),
    })
}

fn ref_where_route(route: &RouteDecl) -> String {
    format!(
        "route {}.{} 0x{:x}",
        route.state, route.direction, route.packet_id
    )
}

/// Raise `EmitError` when a packet reaches a scalar encoding `target` cannot
/// lower. A scalar encoding is a block with a trailing `= expr`: its value is
/// the expression, not the block's fields. Scalars declared but never reached
/// from a packet do not matter, and a lowered name is never entered, because
/// the emitter never looks inside it.
pub fn reject_unlowered_scalars(
    resolved: &ResolvedSchema,
    lowered: &HashSet<String>,
    target: &str,
) -> BpsResult<()> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut unlowered: Vec<String> = Vec::new();

    fn visit(
        refs: Vec<Ref>,
        resolved: &ResolvedSchema,
        lowered: &HashSet<String>,
        seen: &mut HashSet<String>,
        unlowered: &mut Vec<String>,
    ) {
        for ref_ in refs {
            let Some(enc) = resolved.encodings.get(&ref_.name) else {
                continue;
            };
            if seen.contains(&ref_.name) || lowered.contains(&ref_.name) {
                continue;
            }
            seen.insert(ref_.name.clone());
            if enc.yield_expr.is_some() && !unlowered.contains(&ref_.name) {
                unlowered.push(ref_.name.clone());
            }
            let inner = match &enc.body {
                EncBody::Block(b) => item_refs(&b.items, &format!("encoding {}", enc.name)),
                EncBody::Ref(t) => type_refs(
                    &FieldType::Ref(t.clone()),
                    &format!("encoding {}", enc.name),
                ),
            };
            visit(inner, resolved, lowered, seen, unlowered);
        }
    }

    for pkt in resolved.packets.values() {
        let refs = item_refs(&pkt.items, &format!("packet {}", pkt.name));
        visit(refs, resolved, lowered, &mut seen, &mut unlowered);
    }

    if unlowered.is_empty() {
        return Ok(());
    }
    // Python sorts the names before joining.
    unlowered.sort();
    let names = unlowered
        .iter()
        .map(|n| py_repr_str(n))
        .collect::<Vec<_>>()
        .join(", ");
    Err(emit(format!(
        "{target} emitter has no lowering for scalar encoding {names}; only the std scalars map to {target} types"
    )))
}

fn resolve_one(
    ref_: &Ref,
    encodings: &OrderedMap<EncodingDecl>,
    enums: &OrderedMap<EnumDecl>,
    packets: &OrderedMap<PacketDecl>,
) -> BpsResult<()> {
    match ref_.kind {
        RefKind::Packet => {
            if !packets.contains_key(&ref_.name) {
                return Err(resolve(format!(
                    "{} references unknown packet {}",
                    ref_.where_,
                    py_repr_str(&ref_.name)
                )));
            }
        }
        RefKind::Spread => {
            if !encodings.contains_key(&ref_.name) {
                return Err(resolve(format!(
                    "unknown spread encoding {} in {}",
                    py_repr_str(&ref_.name),
                    ref_.where_
                )));
            }
        }
        RefKind::Type => {
            if !encodings.contains_key(&ref_.name)
                && !enums.contains_key(&ref_.name)
                && !is_bare_primitive(&ref_.name)
            {
                return Err(resolve(format!(
                    "unknown type {} in {}",
                    py_repr_str(&ref_.name),
                    ref_.where_
                )));
            }
        }
    }
    Ok(())
}
