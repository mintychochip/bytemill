//! Name references, porting `bps/refs.py`.
//!
//! A declaration names another one in four places: a field's type, a spread,
//! an enum's base type, and a route's packet.

use crate::ast::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefKind {
    Type,
    Spread,
    Packet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub kind: RefKind,
    pub name: String,
    /// The declaration that mentions it, e.g. "packet keep_alive".
    pub where_: String,
}

pub fn references(schema: &Schema) -> Vec<Ref> {
    let mut out = Vec::new();
    for enc in &schema.encodings {
        out.extend(type_refs_enc(&enc.body, &format!("encoding {}", enc.name)));
    }
    for e in &schema.enums {
        out.push(Ref { kind: RefKind::Type, name: e.base.clone(), where_: format!("enum {}", e.name) });
    }
    for pkt in &schema.packets {
        out.extend(item_refs(&pkt.items, &format!("packet {}", pkt.name)));
    }
    for proto in &schema.protocols {
        for frame in &proto.frames {
            out.extend(item_refs(&frame.items, &format!("protocol {} frame {}", proto.name, frame.name)));
        }
        for route in &proto.routes {
            out.push(Ref {
                kind: RefKind::Packet,
                name: route.packet_name.clone(),
                where_: format!("route {}.{} 0x{:x}", route.state, route.direction, route.packet_id),
            });
        }
    }
    out
}

fn type_refs_enc(body: &EncBody, where_: &str) -> Vec<Ref> {
    match body {
        EncBody::Ref(t) => type_refs(&FieldType::Ref(t.clone()), where_),
        EncBody::Block(b) => item_refs(&b.items, where_),
    }
}

pub fn type_refs(t: &FieldType, where_: &str) -> Vec<Ref> {
    match t {
        FieldType::Ref(r) => vec![Ref { kind: RefKind::Type, name: r.name.clone(), where_: where_.to_string() }],
        FieldType::Block(b) => item_refs(&b.items, where_),
        FieldType::Match(m) => {
            let mut out = Vec::new();
            for arm in &m.arms {
                if let Some(body) = &arm.type_ref {
                    out.extend(type_refs(body, where_));
                }
            }
            out
        }
    }
}

pub fn item_refs(items: &[BlockItem], where_: &str) -> Vec<Ref> {
    let mut out = Vec::new();
    for item in items {
        match item {
            BlockItem::Field(wf) => out.extend(type_refs(&wf.type_ref, where_)),
            BlockItem::Spread { name, .. } => {
                out.push(Ref { kind: RefKind::Spread, name: name.clone(), where_: where_.to_string() });
            }
            BlockItem::Group { items, .. } => out.extend(item_refs(items, where_)),
            BlockItem::MatchStmt(m) => out.extend(type_refs(&FieldType::Match(m.clone()), where_)),
            BlockItem::Computed { .. } | BlockItem::Require(_) => {}
        }
    }
    out
}
