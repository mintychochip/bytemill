//! Protodef -> BML lowering: one packet or named type at a time into [`IBlock`] trees.
//!
//! The decode semantics are those of the compiled ProtoDef in node-minecraft-protocol, which is
//! what minecraft-data's descriptions are tested against; where that differs from the
//! interpreted one (a lenient `mapper`, `switch` by strict equality) the compiled one wins.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use super::ir::{EncBody, FieldId, IArm, IBlock, IExpr, IItem, IMatch, IRepeat, IType};
use super::model::{
    Bits, Count, Field, Protocol, Res, ScopeTypes, Switch, Ty, TypeDef, parse_int_key,
};
use super::names;
use super::prelude::{PRELUDE_NAMES, Pre};
use super::scope::{Arena, Kind, ScopeId, ValRef, Visible};

/// A finished named encoding, waiting for its names to be assigned.
pub struct EncOut {
    pub arena: Arena,
    pub body: EncBody,
}

/// A finished packet.
pub struct PacketOut {
    pub arena: Arena,
    pub block: IBlock,
}

/// What a bare type name stands for.
enum Class<'p> {
    Void,
    Scalar(&'static str, Kind),
    Special(&'static str, Option<Pre>),
    User(&'p Ty),
}

/// Lowering state of one root (a packet or a named type).
pub struct Cx {
    pub arena: Arena,
    owner: String,
    /// The BML name of the root being lowered: helper encodings are named after it.
    slug: String,
    path: Vec<String>,
}

impl Cx {
    fn new(owner: &str, slug: &str) -> Cx {
        Cx {
            arena: Arena::new(),
            owner: owner.to_string(),
            slug: slug.to_string(),
            path: Vec::new(),
        }
    }

    fn fail<T>(&self, msg: impl std::fmt::Display) -> Res<T> {
        let at = if self.path.is_empty() {
            String::new()
        } else {
            format!(" at {}", self.path.join("."))
        };
        Err(format!("{}{at}: {msg}", self.owner))
    }
}

pub struct Importer<'p> {
    proto: &'p Protocol,
    shared: HashMap<String, &'p TypeDef>,
    pub encodings: BTreeMap<String, EncOut>,
    enc_names: HashMap<String, String>,
    taken: HashSet<String>,
    pub prelude: BTreeSet<Pre>,
}

/// The `$param`s of a parametrised type are replaced by the use site's path.
fn subst(ty: &Ty, args: &[(String, String)]) -> Ty {
    let s = |p: &str| -> String {
        match p.strip_prefix('$') {
            Some(k) => args
                .iter()
                .find(|(n, _)| n == k)
                .map_or_else(|| p.to_string(), |(_, v)| v.clone()),
            None => p.to_string(),
        }
    };
    let count = |c: &Count| match c {
        Count::Type(t) => Count::Type(Box::new(subst(t, args))),
        Count::Field(p) => Count::Field(s(p)),
        Count::Literal(n) => Count::Literal(*n),
    };
    let b = |t: &Ty| Box::new(subst(t, args));
    match ty {
        Ty::Name(n) => Ty::Name(n.clone()),
        Ty::Container(fs) => Ty::Container(
            fs.iter()
                .map(|f| Field {
                    name: f.name.clone(),
                    anon: f.anon,
                    ty: subst(&f.ty, args),
                })
                .collect(),
        ),
        Ty::Array { count: c, elem } => Ty::Array {
            count: count(c),
            elem: b(elem),
        },
        Ty::Switch(sw) => Ty::Switch(Box::new(Switch {
            compare: s(&sw.compare),
            arms: sw
                .arms
                .iter()
                .map(|(k, t)| (k.clone(), subst(t, args)))
                .collect(),
            default: sw.default.as_ref().map(|t| subst(t, args)),
        })),
        Ty::Option(t) => Ty::Option(b(t)),
        Ty::Mapper { base, mappings } => Ty::Mapper {
            base: b(base),
            mappings: mappings.clone(),
        },
        Ty::Buffer(c) => Ty::Buffer(count(c)),
        Ty::PString(c) => Ty::PString(count(c)),
        Ty::Bitfield(m) => Ty::Bitfield(m.clone()),
        Ty::Bitflags { base, flags } => Ty::Bitflags {
            base: base.clone(),
            flags: flags.clone(),
        },
        Ty::MetaLoop { end, elem } => Ty::MetaLoop {
            end: *end,
            elem: b(elem),
        },
        Ty::TopBitSet(t) => Ty::TopBitSet(b(t)),
        Ty::Holder {
            base_name,
            other_name,
            other,
        } => Ty::Holder {
            base_name: base_name.clone(),
            other_name: other_name.clone(),
            other: b(other),
        },
        Ty::HolderSet {
            base_name,
            base,
            other_name,
            other,
        } => Ty::HolderSet {
            base_name: base_name.clone(),
            base: b(base),
            other_name: other_name.clone(),
            other: b(other),
        },
        Ty::Call { name, args: a } => Ty::Call {
            name: name.clone(),
            args: a.iter().map(|(k, v)| (k.clone(), s(v))).collect(),
        },
    }
}

/// JavaScript's view of a switch key or a mapper value.
#[derive(Debug, Clone, PartialEq)]
enum Js {
    Num(i128),
    Bool(bool),
    Str(String),
}

impl Js {
    /// `isNaN(v) ? v : Number(v)`, `'true'`/`'false'` as booleans (the compiled switch's key rule
    /// and `sanitizeMappings`).
    fn of(v: &str) -> Js {
        match v {
            "true" => Js::Bool(true),
            "false" => Js::Bool(false),
            _ => parse_int_key(v).map_or_else(|| Js::Str(v.to_string()), Js::Num),
        }
    }

    fn falsy(&self) -> bool {
        match self {
            Js::Num(n) => *n == 0,
            Js::Bool(b) => !*b,
            Js::Str(s) => s.is_empty(),
        }
    }
}

/// The raw integers a switch key matches when the tag is a mapper field. The compiled mapper
/// reads `mappings[value] || value`: a mapped name when there is a truthy one, else the raw
/// number, which is then compared with `===` against the (typed) key.
fn mapper_tags(mappings: &[(i128, String)], key: &str) -> Vec<i128> {
    let want = Js::of(key);
    let mut tags = Vec::new();
    for (raw, name) in mappings {
        let mapped = Js::of(name);
        let seen = if mapped.falsy() {
            Js::Num(*raw)
        } else {
            mapped
        };
        if seen == want {
            tags.push(*raw);
        }
    }
    if let Js::Num(n) = want
        && !mappings.iter().any(|(raw, _)| *raw == n)
    {
        tags.push(n);
    }
    tags
}

struct ArmPlan<'t> {
    tags: Vec<i128>,
    ty: &'t Ty,
    note: String,
}

/// Arms keyed to integers: unreachable keys dropped, equal bodies merged.
fn int_arms<'t>(kind: &Kind, sw: &'t Switch) -> Res<Vec<ArmPlan<'t>>> {
    let mut out: Vec<ArmPlan<'t>> = Vec::new();
    for (key, ty) in &sw.arms {
        let tags = match kind {
            Kind::Int => parse_int_key(key).into_iter().collect(),
            Kind::Bool => match key.as_str() {
                "true" => vec![1],
                "false" => vec![0],
                _ => Vec::new(),
            },
            Kind::Mapper(m) => mapper_tags(m, key),
            other => return Err(format!("switch on a {other:?} value")),
        };
        if tags.is_empty() {
            continue;
        }
        if let Some(prev) = out.iter_mut().find(|a| a.ty == ty) {
            prev.tags.extend(tags);
            prev.note = format!("{}, {key}", prev.note);
        } else {
            out.push(ArmPlan {
                tags,
                ty,
                note: key.clone(),
            });
        }
    }
    Ok(out)
}

fn is_void(ty: &Ty) -> bool {
    matches!(ty, Ty::Name(n) if n == "void")
}

fn lit(n: i128) -> IExpr {
    IExpr::Int(n)
}

fn kind_of_scalar(name: &str) -> Option<(&'static str, Kind)> {
    let int = |n: &'static str| Some((n, Kind::Int));
    match name {
        "i8" => int("i8"),
        "u8" => int("u8"),
        "i16" => int("i16be"),
        "u16" => int("u16be"),
        "i32" => int("i32be"),
        "u32" => int("u32be"),
        "i64" => int("i64be"),
        "u64" => int("u64be"),
        "varint" => int("varint"),
        "varlong" => int("varlong"),
        "f32" => Some(("f32be", Kind::Other("float".into()))),
        "f64" => Some(("f64be", Kind::Other("float".into()))),
        "bool" => Some(("boolean", Kind::Bool)),
        _ => None,
    }
}

impl<'p> Importer<'p> {
    pub fn new(proto: &'p Protocol) -> Importer<'p> {
        let mut shared: HashMap<String, &'p TypeDef> = HashMap::new();
        for (name, def) in proto.global.iter() {
            shared.insert(name.to_string(), def);
        }
        for scope in &proto.scopes {
            for (name, def) in scope.types.iter() {
                if name == "packet" || name.starts_with("packet_") {
                    continue;
                }
                shared.entry(name.to_string()).or_insert(def);
            }
        }
        Importer {
            proto,
            shared,
            encodings: BTreeMap::new(),
            enc_names: HashMap::new(),
            taken: PRELUDE_NAMES.iter().map(|s| (*s).to_string()).collect(),
            prelude: BTreeSet::new(),
        }
    }

    // -- names ------------------------------------------------------------------------------

    fn classify(&self, name: &str) -> Res<Class<'p>> {
        if let Some((bml, kind)) = kind_of_scalar(name) {
            return Ok(Class::Scalar(bml, kind));
        }
        let special = |bml, pre| Ok(Class::Special(bml, pre));
        match name {
            "void" => return Ok(Class::Void),
            "restBuffer" => return special("rest", None),
            "UUID" => return special(Pre::Uuid.bml_name(), Some(Pre::Uuid)),
            "nbt" => return special(Pre::Nbt.bml_name(), Some(Pre::Nbt)),
            "optionalNbt" => return special(Pre::OptionalNbt.bml_name(), Some(Pre::OptionalNbt)),
            "anonymousNbt" => {
                return special(Pre::AnonymousNbt.bml_name(), Some(Pre::AnonymousNbt));
            }
            "anonOptionalNbt" => {
                return special(Pre::AnonOptionalNbt.bml_name(), Some(Pre::AnonOptionalNbt));
            }
            "lpVec3" => return special(Pre::LpVec3.bml_name(), Some(Pre::LpVec3)),
            "entityDelta" => return special(Pre::EntityDelta.bml_name(), Some(Pre::EntityDelta)),
            _ => {}
        }
        match self.shared.get(name).copied() {
            Some(TypeDef::Def(ty)) => Ok(Class::User(ty)),
            Some(TypeDef::Native) => Err(format!("native type {name:?} has no translation")),
            None => Err(format!("unknown type {name:?}")),
        }
    }

    /// The kind of value a type yields, following aliases.
    fn kind_of(&self, ty: &Ty, depth: usize) -> Kind {
        if depth > 16 {
            return Kind::Other("alias cycle".into());
        }
        match ty {
            Ty::Name(n) => match self.classify(n) {
                Ok(Class::Scalar(_, k)) => k,
                Ok(Class::User(t)) => self.kind_of(t, depth + 1),
                _ => Kind::Other(n.clone()),
            },
            Ty::Mapper { mappings, .. } => Kind::Mapper(Rc::new(mappings.clone())),
            Ty::Bitfield(_) | Ty::Bitflags { .. } => Kind::Struct,
            Ty::PString(_) => Kind::Str,
            _ => Kind::Other("composite".into()),
        }
    }

    /// Can a value of this type take no bytes on the wire?
    fn can_be_empty(&self, ty: &Ty, depth: usize) -> bool {
        if depth > 12 {
            return false;
        }
        let next = depth + 1;
        match ty {
            Ty::Name(n) => match self.classify(n) {
                Ok(Class::Void | Class::Special(_, None)) => true,
                Ok(Class::Scalar(..) | Class::Special(..)) | Err(_) => false,
                Ok(Class::User(t)) => self.can_be_empty(t, next),
            },
            Ty::Container(fs) => fs.iter().all(|f| self.can_be_empty(&f.ty, next)),
            Ty::Array { count, .. } | Ty::Buffer(count) | Ty::PString(count) => {
                !matches!(count, Count::Type(_))
            }
            Ty::Switch(sw) => {
                sw.arms.iter().any(|(_, t)| self.can_be_empty(t, next))
                    || sw
                        .default
                        .as_ref()
                        .is_none_or(|d| self.can_be_empty(d, next))
            }
            Ty::Mapper { base, .. } => self.can_be_empty(base, next),
            Ty::Option(_)
            | Ty::Bitfield(_)
            | Ty::Bitflags { .. }
            | Ty::MetaLoop { .. }
            | Ty::TopBitSet(_)
            | Ty::Holder { .. }
            | Ty::HolderSet { .. } => false,
            Ty::Call { name, .. } => match self.shared.get(name).copied() {
                Some(TypeDef::Def(t)) => self.can_be_empty(t, next),
                _ => false,
            },
        }
    }

    fn mark(&mut self, pre: Option<Pre>) {
        if let Some(p) = pre {
            self.prelude.insert(p);
        }
    }

    /// The BML encoding for a named type of the protocol, lowered on first use.
    pub fn ensure_encoding(&mut self, proto_name: &str) -> Res<String> {
        if let Some(n) = self.enc_names.get(proto_name) {
            return Ok(n.clone());
        }
        let def: &'p Ty = match self.shared.get(proto_name).copied() {
            Some(TypeDef::Def(t)) => t,
            Some(TypeDef::Native) => {
                return Err(format!("native type {proto_name:?} has no translation"));
            }
            None => return Err(format!("unknown type {proto_name:?}")),
        };
        let bml = names::unique(&names::snake(proto_name), |c| self.taken.contains(c));
        self.taken.insert(bml.clone());
        self.enc_names.insert(proto_name.to_string(), bml.clone());
        let mut cx = Cx::new(&format!("type {proto_name}"), &bml);
        let body = self.lower_named(&mut cx, def)?;
        self.encodings.insert(
            bml.clone(),
            EncOut {
                arena: cx.arena,
                body,
            },
        );
        Ok(bml)
    }

    // -- roots ------------------------------------------------------------------------------

    fn lower_named(&mut self, cx: &mut Cx, def: &Ty) -> Res<EncBody> {
        let scope = cx.arena.scope(None);
        let mut items = Vec::new();
        match def {
            Ty::Name(n) => {
                return match self.classify(n).or_else(|e| cx.fail(e))? {
                    Class::Void => Ok(EncBody::Block(IBlock::default())),
                    Class::Scalar(b, _) => Ok(EncBody::Alias(b.to_string())),
                    Class::Special(b, pre) => {
                        self.mark(pre);
                        Ok(EncBody::Alias(b.to_string()))
                    }
                    Class::User(_) => Ok(EncBody::Alias(self.ensure_encoding(n)?)),
                };
            }
            Ty::Mapper { base, .. } => {
                if let Ty::Name(n) = &**base
                    && let Class::Scalar(b, _) = self.classify(n).or_else(|e| cx.fail(e))?
                {
                    return Ok(EncBody::Alias(b.to_string()));
                }
                return cx.fail("a named mapper over a non-scalar type");
            }
            Ty::Container(fields) => self.container_items(cx, scope, fields, &mut items)?,
            Ty::Bitfield(members) => self.bitfield(cx, scope, "raw", true, members, &mut items)?,
            Ty::Bitflags { base, flags } => {
                self.bitflags(cx, scope, "raw", true, base, flags, &mut items)?
            }
            other => self.field(cx, scope, "value", other, &mut items)?,
        }
        Ok(EncBody::Block(IBlock { items }))
    }

    /// The body of a packet: the fields of its container.
    pub fn lower_packet(
        &mut self,
        scope: &ScopeTypes,
        name: &str,
        body: Option<&Ty>,
    ) -> Res<PacketOut> {
        let mut cx = Cx::new(
            &format!(
                "packet {name} ({}.{})",
                scope.state,
                scope.direction.as_str()
            ),
            name,
        );
        let root = cx.arena.scope(None);
        let mut items = Vec::new();
        let mut ty = body;
        let mut guard = 0;
        // A packet body is a container, or a name for one (`packet_common_*` are shared).
        while let Some(Ty::Name(n)) = ty {
            guard += 1;
            if guard > 8 {
                return cx.fail("packet body alias chain too long");
            }
            let def = scope.types.get(n).or_else(|| self.proto.global.get(n));
            match def {
                Some(TypeDef::Def(t)) => ty = Some(t),
                _ => break,
            }
        }
        match ty {
            None => {}
            Some(Ty::Container(fields)) => {
                self.container_items(&mut cx, root, fields, &mut items)?
            }
            Some(Ty::Name(n)) if n == "void" => {}
            Some(other) => self.field(&mut cx, root, "value", other, &mut items)?,
        }
        Ok(PacketOut {
            arena: cx.arena,
            block: IBlock { items },
        })
    }

    // -- containers -------------------------------------------------------------------------

    fn container_items(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        fields: &[Field],
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        for f in fields {
            if f.anon {
                cx.path.push("<anon>".to_string());
                self.anon_field(cx, scope, &f.ty, out)?;
                cx.path.pop();
                continue;
            }
            let Some(name) = &f.name else {
                return cx.fail("a field with neither a name nor anon");
            };
            cx.path.push(name.clone());
            self.field(cx, scope, name, &f.ty, out)?;
            cx.path.pop();
        }
        Ok(())
    }

    /// An `anon` field: a container flattens into its parent, a bitfield's members become the
    /// parent's, a switch's chosen arm splices its fields in.
    fn anon_field(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        ty: &Ty,
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        match ty {
            Ty::Container(fields) => self.container_items(cx, scope, fields, out),
            Ty::Bitfield(members) => self.bitfield(cx, scope, "bits", true, members, out),
            Ty::Switch(sw) => self.switch(cx, scope, sw, None, out).map(|_| ()),
            other => cx.fail(format!("cannot inline an anonymous {}", ctor_name(other))),
        }
    }

    // -- one field --------------------------------------------------------------------------

    /// Lower field `name` of type `ty` into `out` and make it nameable from `scope`.
    fn field(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        name: &str,
        ty: &Ty,
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        let plain = |kind: Kind, fid: FieldId| Visible {
            val: ValRef::Field(fid),
            kind,
            members: Vec::new(),
        };
        match ty {
            Ty::Name(n) => {
                let (bml, kind) = match self.classify(n).or_else(|e| cx.fail(e))? {
                    Class::Void => {
                        cx.arena.register(
                            scope,
                            name,
                            Visible {
                                val: ValRef::Absent,
                                kind: Kind::Other("void".into()),
                                members: Vec::new(),
                            },
                        );
                        return Ok(());
                    }
                    Class::Scalar(b, k) => (b.to_string(), k),
                    Class::Special(b, pre) => {
                        self.mark(pre);
                        (b.to_string(), Kind::Other(b.to_string()))
                    }
                    Class::User(def) => (self.ensure_encoding(n)?, self.kind_of(def, 0)),
                };
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Name(bml),
                    repeat: None,
                });
                cx.arena.register(scope, name, plain(kind, fid));
            }
            Ty::Container(fields) => {
                let child = cx.arena.scope(Some(scope));
                let mut items = Vec::new();
                self.container_items(cx, child, fields, &mut items)?;
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Block(IBlock { items }),
                    repeat: None,
                });
                cx.arena
                    .register(scope, name, plain(Kind::Other("container".into()), fid));
            }
            Ty::Array { count, elem } => {
                let n = self.count(cx, scope, &format!("{name}_count"), count, out)?;
                let elem_ty = self.type_of(cx, scope, elem, "item")?;
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: elem_ty,
                    repeat: Some(IRepeat::Count(n)),
                });
                cx.arena
                    .register(scope, name, plain(Kind::Other("array".into()), fid));
            }
            Ty::Option(inner) => {
                let flag = cx.arena.field(scope, &format!("has_{name}"));
                out.push(IItem::Field {
                    fid: flag,
                    ty: IType::Name("boolean".into()),
                    repeat: None,
                });
                let kind = self.kind_of(inner, 0);
                if let Ty::Name(n) = &**inner
                    && matches!(self.classify(n).or_else(|e| cx.fail(e))?, Class::Void)
                {
                    cx.arena.register(scope, name, plain(Kind::Bool, flag));
                    return Ok(());
                }
                let value = cx.arena.field(scope, name);
                let inner_ty = self.type_of(cx, scope, inner, name)?;
                cx.arena.note_ref(scope, flag);
                let empty = self.can_be_empty(inner, 0);
                out.push(guarded(
                    IExpr::Field(flag),
                    vec![IItem::Field {
                        fid: value,
                        ty: inner_ty,
                        repeat: None,
                    }],
                    empty,
                ));
                cx.arena.register(
                    scope,
                    name,
                    Visible {
                        val: ValRef::Opt { flag, value },
                        kind,
                        members: Vec::new(),
                    },
                );
            }
            Ty::Mapper { base, mappings } => {
                let base_ty = self.type_of(cx, scope, base, name)?;
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: base_ty,
                    repeat: None,
                });
                cx.arena.register(
                    scope,
                    name,
                    plain(Kind::Mapper(Rc::new(mappings.clone())), fid),
                );
            }
            Ty::Buffer(count) => {
                let n = self.count(cx, scope, &format!("{name}_len"), count, out)?;
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Bytes(n),
                    repeat: None,
                });
                cx.arena
                    .register(scope, name, plain(Kind::Other("buffer".into()), fid));
            }
            Ty::PString(count) => {
                let hint = if name == "value" {
                    "byte_len".to_string()
                } else {
                    format!("{name}_byte_len")
                };
                let n = self.count(cx, scope, &hint, count, out)?;
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Utf8(n),
                    repeat: None,
                });
                cx.arena.register(scope, name, plain(Kind::Str, fid));
            }
            Ty::Bitfield(members) => self.bitfield(cx, scope, name, false, members, out)?,
            Ty::Bitflags { base, flags } => {
                self.bitflags(cx, scope, name, true, base, flags, out)?
            }
            Ty::Switch(sw) => {
                self.switch(cx, scope, sw, Some(name), out)?;
            }
            Ty::MetaLoop { end, elem } => {
                let elem_ty = self.type_of(cx, scope, elem, "entry")?;
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: elem_ty,
                    repeat: Some(IRepeat::Until(*end)),
                });
                cx.arena
                    .register(scope, name, plain(Kind::Other("array".into()), fid));
            }
            Ty::TopBitSet(elem) => {
                let helper = self.top_bit_set(cx, name, elem)?;
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Name(helper),
                    repeat: None,
                });
                cx.arena
                    .register(scope, name, plain(Kind::Other("array".into()), fid));
            }
            Ty::Holder {
                other_name, other, ..
            } => {
                let child = cx.arena.scope(Some(scope));
                let id = cx.arena.field(child, "id");
                let value = cx.arena.field(child, other_name);
                let inner = self.type_of(cx, child, other, other_name)?;
                cx.arena.note_ref(child, id);
                let empty = self.can_be_empty(other, 0);
                let items = vec![
                    IItem::Field {
                        fid: id,
                        ty: IType::Name("varint".into()),
                        repeat: None,
                    },
                    guarded(
                        IExpr::bin("==", IExpr::Field(id), lit(0)),
                        vec![IItem::Field {
                            fid: value,
                            ty: inner,
                            repeat: None,
                        }],
                        empty,
                    ),
                ];
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Block(IBlock { items }),
                    repeat: None,
                });
                cx.arena
                    .register(scope, name, plain(Kind::Other("holder".into()), fid));
            }
            Ty::HolderSet {
                base_name,
                base,
                other_name,
                other,
                ..
            } => {
                let child = cx.arena.scope(Some(scope));
                let tag = cx.arena.field(child, "set_type");
                let base_field = cx.arena.field(child, base_name);
                let other_field = cx.arena.field(child, other_name);
                let base_ty = self.type_of(cx, child, base, base_name)?;
                let other_ty = self.type_of(cx, child, other, other_name)?;
                cx.arena.note_ref(child, tag);
                let base_empty = self.can_be_empty(base, 0);
                let items = vec![
                    IItem::Field {
                        fid: tag,
                        ty: IType::Name("varint".into()),
                        repeat: None,
                    },
                    guarded(
                        IExpr::bin("==", IExpr::Field(tag), lit(0)),
                        vec![IItem::Field {
                            fid: base_field,
                            ty: base_ty,
                            repeat: None,
                        }],
                        base_empty,
                    ),
                    // One id more than zero is an empty list: no bytes at all.
                    guarded(
                        IExpr::bin(">", IExpr::Field(tag), lit(0)),
                        vec![IItem::Field {
                            fid: other_field,
                            ty: other_ty,
                            repeat: Some(IRepeat::Count(IExpr::bin(
                                "-",
                                IExpr::Field(tag),
                                lit(1),
                            ))),
                        }],
                        true,
                    ),
                ];
                let fid = cx.arena.field(scope, name);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Block(IBlock { items }),
                    repeat: None,
                });
                cx.arena
                    .register(scope, name, plain(Kind::Other("holder set".into()), fid));
            }
            Ty::Call {
                name: type_name,
                args,
            } => {
                let expanded = self.expand_call(cx, type_name, args)?;
                self.field(cx, scope, name, &expanded, out)?;
            }
        }
        Ok(())
    }

    fn expand_call(&self, cx: &Cx, type_name: &str, args: &[(String, String)]) -> Res<Ty> {
        match self.shared.get(type_name).copied() {
            Some(TypeDef::Def(body)) => Ok(subst(body, args)),
            _ => cx.fail(format!("{type_name:?} is not a parametrised type")),
        }
    }

    /// `T` in a position with no field name of its own (an array element, an option's value,
    /// a switch arm): a plain reference when one will do, else an invented block.
    fn type_of(&mut self, cx: &mut Cx, scope: ScopeId, ty: &Ty, hint: &str) -> Res<IType> {
        match ty {
            Ty::Name(n) => Ok(match self.classify(n).or_else(|e| cx.fail(e))? {
                Class::Void => IType::Block(IBlock::default()),
                Class::Scalar(b, _) => IType::Name(b.to_string()),
                Class::Special(b, pre) => {
                    self.mark(pre);
                    IType::Name(b.to_string())
                }
                Class::User(_) => IType::Name(self.ensure_encoding(n)?),
            }),
            Ty::Container(fields) => {
                let child = cx.arena.scope(Some(scope));
                let mut items = Vec::new();
                self.container_items(cx, child, fields, &mut items)?;
                Ok(IType::Block(IBlock { items }))
            }
            Ty::Mapper { base, .. } => self.type_of(cx, scope, base, hint),
            Ty::Call { name, args } => {
                let expanded = self.expand_call(cx, name, args)?;
                self.type_of(cx, scope, &expanded, hint)
            }
            other => {
                let child = cx.arena.transparent(scope);
                let mut items = Vec::new();
                let wrapper_hint = if hint == "item" || hint == "entry" {
                    "value"
                } else {
                    hint
                };
                self.field(cx, child, wrapper_hint, other, &mut items)?;
                Ok(IType::Block(IBlock { items }))
            }
        }
    }

    /// The count expression of an array/buffer/string; a `countType` becomes a field named
    /// `hint`.
    fn count(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        hint: &str,
        count: &Count,
        out: &mut Vec<IItem>,
    ) -> Res<IExpr> {
        match count {
            Count::Type(t) => {
                let hint = hint.to_string();
                let ty = self.type_of(cx, scope, t, &hint)?;
                let fid = cx.arena.field(scope, &hint);
                out.push(IItem::Field {
                    fid,
                    ty,
                    repeat: None,
                });
                cx.arena.note_ref(scope, fid);
                Ok(IExpr::Field(fid))
            }
            Count::Field(path) => match cx.arena.resolve(scope, path) {
                Ok(r) => Ok(r.or(0)),
                Err(e) => cx.fail(e),
            },
            Count::Literal(n) => Ok(lit(*n)),
        }
    }

    // -- bit packed values ------------------------------------------------------------------

    fn uint_for_bits(&self, cx: &Cx, total: u32) -> Res<&'static str> {
        match total {
            8 => Ok("u8"),
            16 => Ok("u16be"),
            32 => Ok("u32be"),
            64 => Ok("u64be"),
            other => cx.fail(format!("a {other}-bit bitfield has no integer encoding")),
        }
    }

    /// A bitfield: the big-endian integer, then one computed value per member, first member in
    /// the top bits. `flatten` makes the members the parent's own names (an anonymous bitfield or
    /// a named type's body); otherwise they are `<name>_<member>` and reachable as `name/member`.
    fn bitfield(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        name: &str,
        flatten: bool,
        members: &[Bits],
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        let total: u32 = members.iter().map(|m| m.size).sum();
        let raw_ty = self.uint_for_bits(cx, total)?;
        let raw = cx.arena.field(scope, name);
        out.push(IItem::Field {
            fid: raw,
            ty: IType::Name(raw_ty.into()),
            repeat: None,
        });
        let mut shift = total;
        let mut registered = Vec::new();
        for m in members {
            shift -= m.size;
            let mut e = IExpr::Field(raw);
            if shift > 0 {
                e = IExpr::bin(">>", e, lit(i128::from(shift)));
            }
            if shift + m.size < total || total == 0 {
                let mask = (1i128 << m.size) - 1;
                e = IExpr::bin("&", e, lit(mask));
            }
            if m.signed {
                e = IExpr::Call("sign_extend", vec![e, lit(i128::from(m.size))]);
            }
            let hint = if flatten {
                m.name.clone()
            } else {
                format!("{name}_{}", m.name)
            };
            let fid = cx.arena.field(scope, &hint);
            cx.arena.note_ref(scope, raw);
            out.push(IItem::Computed { fid, expr: e });
            registered.push((m.name.clone(), fid, Kind::Int));
        }
        if flatten {
            for (member, fid, kind) in registered {
                cx.arena.register(
                    scope,
                    &member,
                    Visible {
                        val: ValRef::Field(fid),
                        kind,
                        members: Vec::new(),
                    },
                );
            }
        } else {
            cx.arena.register(
                scope,
                name,
                Visible {
                    val: ValRef::Field(raw),
                    kind: Kind::Struct,
                    members: registered,
                },
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn bitflags(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        name: &str,
        register: bool,
        base: &str,
        flags: &[(String, u64)],
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        let Class::Scalar(base_ty, _) = self.classify(base).or_else(|e| cx.fail(e))? else {
            return cx.fail(format!("bitflags over {base:?}"));
        };
        let raw = cx.arena.field(scope, name);
        out.push(IItem::Field {
            fid: raw,
            ty: IType::Name(base_ty.into()),
            repeat: None,
        });
        let mut members = Vec::new();
        for (flag, mask) in flags {
            let fid = cx.arena.field(scope, &format!("{name}_{flag}"));
            cx.arena.note_ref(scope, raw);
            let e = IExpr::bin(
                "!=",
                IExpr::bin("&", IExpr::Field(raw), lit(i128::from(*mask))),
                lit(0),
            );
            out.push(IItem::Computed { fid, expr: e });
            members.push((flag.clone(), fid, Kind::Bool));
        }
        if register {
            cx.arena.register(
                scope,
                name,
                Visible {
                    val: ValRef::Field(raw),
                    kind: Kind::Struct,
                    members,
                },
            );
        }
        Ok(())
    }

    // -- topBitSetTerminatedArray -----------------------------------------------------------

    /// `elem` is a container whose first field is a byte: bit 7 of that byte says another element
    /// follows, and the low 7 bits are the field. A recursive encoding reads the chain.
    fn top_bit_set(&mut self, cx: &Cx, field_name: &str, elem: &Ty) -> Res<String> {
        let Ty::Container(fields) = elem else {
            return cx.fail("topBitSetTerminatedArray over a non-container");
        };
        let Some(first) = fields.first() else {
            return cx.fail("topBitSetTerminatedArray over an empty container");
        };
        let (Some(first_name), Ty::Name(first_ty)) = (&first.name, &first.ty) else {
            return cx.fail("topBitSetTerminatedArray whose first field is not a named scalar");
        };
        if !matches!(first_ty.as_str(), "i8" | "u8") || first.anon {
            return cx.fail("topBitSetTerminatedArray whose first field is not a byte");
        }
        let base = names::snake(&format!("{}_{}", cx.slug, field_name));
        let helper = names::unique(&base, |c| self.taken.contains(c));
        self.taken.insert(helper.clone());
        let mut hcx = Cx::new(&format!("{} (chain of {field_name})", cx.owner), &helper);
        let scope = hcx.arena.scope(None);
        let raw = hcx.arena.field(scope, &format!("{first_name}_byte"));
        let first_fid = hcx.arena.field(scope, first_name);
        hcx.arena.note_ref(scope, raw);
        let mut items = vec![
            IItem::Field {
                fid: raw,
                ty: IType::Name("u8".into()),
                repeat: None,
            },
            IItem::Computed {
                fid: first_fid,
                expr: IExpr::bin("&", IExpr::Field(raw), lit(0x7f)),
            },
        ];
        hcx.arena.register(
            scope,
            first_name,
            Visible {
                val: ValRef::Field(first_fid),
                kind: Kind::Int,
                members: Vec::new(),
            },
        );
        self.container_items(&mut hcx, scope, &fields[1..], &mut items)?;
        let next = hcx.arena.field(scope, "next");
        hcx.arena.note_ref(scope, raw);
        items.push(IItem::If {
            cond: IExpr::bin("!=", IExpr::bin("&", IExpr::Field(raw), lit(0x80)), lit(0)),
            items: vec![IItem::Field {
                fid: next,
                ty: IType::Name(helper.clone()),
                repeat: None,
            }],
        });
        self.encodings.insert(
            helper.clone(),
            EncOut {
                arena: hcx.arena,
                body: EncBody::Block(IBlock { items }),
            },
        );
        Ok(helper)
    }

    // -- switch -----------------------------------------------------------------------------

    /// A switch. `name` is `Some` for a field (the arm is its value) and `None` for an anonymous
    /// one (the arm's fields splice into the enclosing block).
    fn switch(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        sw: &Switch,
        name: Option<&str>,
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        let target = match cx.arena.resolve(scope, &sw.compare) {
            Ok(t) => t,
            Err(e) => return cx.fail(e),
        };
        let default_void = sw.default.as_ref().is_none_or(is_void);
        if matches!(target.kind, Kind::Str) {
            return self.switch_by_string(cx, scope, sw, &target.value, name, out);
        }
        let arms = int_arms(&target.kind, sw).or_else(|e| cx.fail(e))?;
        let reachable: Vec<&ArmPlan> = arms.iter().filter(|a| !is_void(a.ty)).collect();
        if reachable.is_empty() && default_void {
            // Every arm is `void`: there is nothing on the wire.
            if let Some(n) = name {
                cx.arena.register(
                    scope,
                    n,
                    Visible {
                        val: ValRef::Absent,
                        kind: Kind::Other("void".into()),
                        members: Vec::new(),
                    },
                );
            }
            return Ok(());
        }
        let lowest = arms
            .iter()
            .flat_map(|a| a.tags.iter().copied())
            .min()
            .unwrap_or(0)
            .min(0);
        let tag = target.or(lowest - 1);
        cx.arena.note_expr(scope, &tag);

        // `scoreboard_objective`: a switch whose arms are `option`s, read later by another switch.
        if let Some(n) = name
            && let Some(inner) = options_only(sw, &arms)
        {
            return self.switch_of_options(
                cx,
                scope,
                n,
                tag,
                &arms,
                sw.default.as_ref(),
                inner,
                out,
            );
        }

        match name {
            Some(n) => {
                let mut plans = Vec::with_capacity(arms.len() + 1);
                for a in &arms {
                    let body = self.type_of(cx, scope, a.ty, n)?;
                    plans.push(IArm {
                        tags: a.tags.clone(),
                        body,
                        note: Some(a.note.clone()),
                    });
                }
                let body = match &sw.default {
                    Some(d) => self.type_of(cx, scope, d, n)?,
                    None => IType::Block(IBlock::default()),
                };
                plans.push(IArm {
                    tags: Vec::new(),
                    body,
                    note: None,
                });
                let fid = cx.arena.field(scope, n);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Match(Box::new(IMatch { tag, arms: plans })),
                    repeat: None,
                });
                cx.arena.register(
                    scope,
                    n,
                    Visible {
                        val: ValRef::Field(fid),
                        kind: Kind::Other("switch".into()),
                        members: Vec::new(),
                    },
                );
            }
            None => {
                let mut plans = Vec::with_capacity(arms.len() + 1);
                for a in &arms {
                    let body = self.arm_block(cx, scope, scope, a.ty)?;
                    plans.push(IArm {
                        tags: a.tags.clone(),
                        body,
                        note: Some(a.note.clone()),
                    });
                }
                let body = match &sw.default {
                    Some(d) => self.arm_block(cx, scope, scope, d)?,
                    None => IType::Block(IBlock::default()),
                };
                plans.push(IArm {
                    tags: Vec::new(),
                    body,
                    note: None,
                });
                out.push(IItem::MatchStmt(IMatch { tag, arms: plans }));
            }
        }
        Ok(())
    }

    /// An anonymous switch's arm as a block whose fields splice into `like`'s namespace.
    fn arm_block(&mut self, cx: &mut Cx, scope: ScopeId, like: ScopeId, ty: &Ty) -> Res<IType> {
        let mut ty = ty;
        let mut guard = 0;
        while let Ty::Name(n) = ty {
            guard += 1;
            if guard > 8 {
                return cx.fail("alias chain too long");
            }
            match self.classify(n).or_else(|e| cx.fail(e))? {
                Class::Void => return Ok(IType::Block(IBlock::default())),
                Class::User(def) => ty = def,
                _ => {
                    return cx.fail(format!(
                        "an anonymous switch arm of type {n:?} cannot splice fields"
                    ));
                }
            }
        }
        match ty {
            Ty::Container(fields) => {
                let arm_scope = cx.arena.spliced(scope, like);
                let mut items = Vec::new();
                self.container_items(cx, arm_scope, fields, &mut items)?;
                Ok(IType::Block(IBlock { items }))
            }
            other => cx.fail(format!(
                "an anonymous switch arm of type {} cannot splice fields",
                ctor_name(other)
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn switch_of_options(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        name: &str,
        tag: IExpr,
        arms: &[ArmPlan],
        default: Option<&Ty>,
        inner: &Ty,
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        let flag = cx.arena.field(scope, &format!("has_{name}"));
        let value = cx.arena.field(scope, name);
        let kind = self.kind_of(inner, 0);
        let mut plans = Vec::new();
        for a in arms {
            let Ty::Option(arm_inner) = a.ty else {
                if is_void(a.ty) {
                    continue;
                }
                return cx.fail("a switch mixing option and non-option arms");
            };
            let inner_ty = self.type_of(cx, scope, arm_inner, name)?;
            cx.arena.note_ref(scope, flag);
            let empty = self.can_be_empty(arm_inner, 0);
            let items = vec![
                IItem::Field {
                    fid: flag,
                    ty: IType::Name("boolean".into()),
                    repeat: None,
                },
                guarded(
                    IExpr::Field(flag),
                    vec![IItem::Field {
                        fid: value,
                        ty: inner_ty,
                        repeat: None,
                    }],
                    empty,
                ),
            ];
            plans.push(IArm {
                tags: a.tags.clone(),
                body: IType::Block(IBlock { items }),
                note: Some(a.note.clone()),
            });
        }
        if default.is_some_and(|d| !is_void(d)) {
            return cx.fail("a switch of options with a non-void default");
        }
        // Every other arm leaves the value absent, which later switches read as `undefined`.
        let absent = vec![IItem::Computed {
            fid: flag,
            expr: IExpr::Bool(false),
        }];
        plans.push(IArm {
            tags: Vec::new(),
            body: IType::Block(IBlock { items: absent }),
            note: None,
        });
        out.push(IItem::MatchStmt(IMatch { tag, arms: plans }));
        cx.arena.register(
            scope,
            name,
            Visible {
                val: ValRef::Opt { flag, value },
                kind,
                members: Vec::new(),
            },
        );
        Ok(())
    }

    /// A switch on a string field becomes `if tag == "key"` groups; BML arms are integers.
    fn switch_by_string(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        sw: &Switch,
        tag: &IExpr,
        name: Option<&str>,
        out: &mut Vec<IItem>,
    ) -> Res<()> {
        // Keys that are numbers or booleans never equal a string under `===`.
        let mut arms: Vec<(Vec<&str>, &Ty)> = Vec::new();
        for (key, ty) in &sw.arms {
            if !matches!(Js::of(key), Js::Str(_)) {
                continue;
            }
            if key.contains(['\n', '\r']) {
                return cx.fail(format!("switch key {key:?} has a line break"));
            }
            if let Some(prev) = arms.iter_mut().find(|(_, t)| *t == ty) {
                prev.0.push(key);
            } else {
                arms.push((vec![key.as_str()], ty));
            }
        }
        let any_default = sw.default.as_ref().is_some_and(|d| !is_void(d));
        if arms.iter().all(|(_, t)| is_void(t)) && !any_default {
            if let Some(n) = name {
                cx.arena.register(
                    scope,
                    n,
                    Visible {
                        val: ValRef::Absent,
                        kind: Kind::Other("void".into()),
                        members: Vec::new(),
                    },
                );
            }
            return Ok(());
        }
        let eq_any = |keys: &[&str]| {
            keys.iter()
                .map(|k| IExpr::bin("==", tag.clone(), IExpr::Str((*k).to_string())))
                .reduce(|a, b| IExpr::bin("||", a, b))
                .unwrap_or(IExpr::Bool(false))
        };
        // Groups live in a block of their own (a field) or in the enclosing one (anonymous).
        let (group_scope, like) = match name {
            Some(_) => {
                let b = cx.arena.transparent(scope);
                (b, b)
            }
            None => (scope, scope),
        };
        let mut groups = Vec::new();
        for (keys, ty) in &arms {
            if is_void(ty) {
                continue;
            }
            let items = self.string_arm_items(cx, scope, group_scope, like, ty, name.is_some())?;
            let cond = eq_any(keys);
            cx.arena.note_expr(group_scope, &cond);
            let empty = self.can_be_empty(ty, 0);
            groups.push(guarded(cond, items, empty));
        }
        if let Some(d) = &sw.default
            && !is_void(d)
        {
            let all: Vec<&str> = arms.iter().flat_map(|(k, _)| k.iter().copied()).collect();
            let cond = IExpr::Not(Box::new(eq_any(&all)));
            cx.arena.note_expr(group_scope, &cond);
            let items = self.string_arm_items(cx, scope, group_scope, like, d, name.is_some())?;
            let empty = self.can_be_empty(d, 0);
            groups.push(guarded(cond, items, empty));
        }
        match name {
            Some(n) => {
                let fid = cx.arena.field(scope, n);
                out.push(IItem::Field {
                    fid,
                    ty: IType::Block(IBlock { items: groups }),
                    repeat: None,
                });
                cx.arena.register(
                    scope,
                    n,
                    Visible {
                        val: ValRef::Field(fid),
                        kind: Kind::Other("switch".into()),
                        members: Vec::new(),
                    },
                );
            }
            None => out.extend(groups),
        }
        Ok(())
    }

    /// The items of one string-keyed arm: a container's fields splice in; any other type is a
    /// `value` field (fields only, not for anonymous switches).
    fn string_arm_items(
        &mut self,
        cx: &mut Cx,
        scope: ScopeId,
        group_scope: ScopeId,
        like: ScopeId,
        ty: &Ty,
        as_field: bool,
    ) -> Res<Vec<IItem>> {
        let mut resolved = ty;
        if let Ty::Name(n) = ty
            && let Class::User(def) = self.classify(n).or_else(|e| cx.fail(e))?
            && matches!(def, Ty::Container(_))
            && !as_field
        {
            resolved = def;
        }
        match resolved {
            Ty::Container(fields) => {
                let arm_scope = cx.arena.spliced(scope, like);
                let mut items = Vec::new();
                self.container_items(cx, arm_scope, fields, &mut items)?;
                Ok(items)
            }
            other if as_field => {
                // Arms are mutually exclusive, so each may call its field `value`.
                let arm_scope = cx.arena.transparent(group_scope);
                let body = self.type_of(cx, arm_scope, other, "value")?;
                let fid = cx.arena.field(arm_scope, "value");
                Ok(vec![IItem::Field {
                    fid,
                    ty: body,
                    repeat: None,
                }])
            }
            other => cx.fail(format!(
                "an anonymous string switch arm of type {} cannot splice fields",
                ctor_name(other)
            )),
        }
    }
}

/// Presence guarded by `cond`. The decoder skips an `if` group when no bytes are left, which is
/// right for a truncated packet but loses the group when its content is legitimately empty (the
/// decoded value then has no field for it and cannot be encoded again). A `match` statement has
/// no such shortcut, so a group that can be empty is written as one.
fn guarded(cond: IExpr, items: Vec<IItem>, can_be_empty: bool) -> IItem {
    if !can_be_empty {
        return IItem::If { cond, items };
    }
    IItem::MatchStmt(IMatch {
        tag: cond,
        arms: vec![
            IArm {
                tags: vec![1],
                body: IType::Block(IBlock { items }),
                note: None,
            },
            IArm {
                tags: Vec::new(),
                body: IType::Block(IBlock::default()),
                note: None,
            },
        ],
    })
}

/// When every arm of `arms` (and no other) is an `option` over the same inner type, that type.
fn options_only<'t>(sw: &'t Switch, arms: &[ArmPlan<'t>]) -> Option<&'t Ty> {
    let mut inner: Option<&Ty> = None;
    for a in arms {
        match a.ty {
            Ty::Option(i) => match inner {
                None => inner = Some(i),
                Some(prev) if prev == &**i => {}
                Some(_) => return None,
            },
            t if is_void(t) => {}
            _ => return None,
        }
    }
    let _ = sw;
    inner
}

fn ctor_name(ty: &Ty) -> &'static str {
    match ty {
        Ty::Name(_) => "name",
        Ty::Container(_) => "container",
        Ty::Array { .. } => "array",
        Ty::Switch(_) => "switch",
        Ty::Option(_) => "option",
        Ty::Mapper { .. } => "mapper",
        Ty::Buffer(_) => "buffer",
        Ty::PString(_) => "pstring",
        Ty::Bitfield(_) => "bitfield",
        Ty::Bitflags { .. } => "bitflags",
        Ty::MetaLoop { .. } => "entityMetadataLoop",
        Ty::TopBitSet(_) => "topBitSetTerminatedArray",
        Ty::Holder { .. } => "registryEntryHolder",
        Ty::HolderSet { .. } => "registryEntryHolderSet",
        Ty::Call { .. } => "parametrised type",
    }
}
