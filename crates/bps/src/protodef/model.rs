//! The ProtoDef type model: what a minecraft-data `protocol.json` says, parsed out of JSON.
//!
//! Only the constructors minecraft-data uses exist here. An unknown constructor is a parse
//! error, never silently passed through: the importer must refuse what it cannot translate.

use crate::json::loads;
use crate::ordmap::OrderedMap;
use crate::value::Value;

pub type Res<T> = Result<T, String>;

/// A ProtoDef type expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    /// A bare name: a native scalar (`varint`, `bool`, ...) or a named type of the protocol.
    Name(String),
    Container(Vec<Field>),
    Array {
        count: Count,
        elem: Box<Ty>,
    },
    Switch(Box<Switch>),
    Option(Box<Ty>),
    /// `mappings` are `(raw integer, name)` in document order.
    Mapper {
        base: Box<Ty>,
        mappings: Vec<(i128, String)>,
    },
    Buffer(Count),
    PString(Count),
    Bitfield(Vec<Bits>),
    /// `flags` are `(name, mask)`.
    Bitflags {
        base: String,
        flags: Vec<(String, u64)>,
    },
    MetaLoop {
        end: i128,
        elem: Box<Ty>,
    },
    TopBitSet(Box<Ty>),
    Holder {
        base_name: String,
        other_name: String,
        other: Box<Ty>,
    },
    HolderSet {
        base_name: String,
        base: Box<Ty>,
        other_name: String,
        other: Box<Ty>,
    },
    /// A use of a parametrised named type, `["particleData", {"compareTo": "particleId"}]`.
    Call {
        name: String,
        args: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Count {
    Type(Box<Ty>),
    /// A reference to an earlier field (or a `$param`).
    Field(String),
    Literal(i128),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: Option<String>,
    pub anon: bool,
    pub ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Switch {
    pub compare: String,
    /// `(key, type)` in document order.
    pub arms: Vec<(String, Ty)>,
    pub default: Option<Ty>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bits {
    pub name: String,
    pub size: u32,
    pub signed: bool,
}

/// A named type of the protocol.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeDef {
    Native,
    Def(Ty),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    Clientbound,
    Serverbound,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Clientbound => "clientbound",
            Direction::Serverbound => "serverbound",
        }
    }
}

/// One packet of a `(state, direction)` scope.
#[derive(Debug, Clone, PartialEq)]
pub struct PacketEntry {
    pub id: i128,
    pub name: String,
    /// The packet's body type; `None` is ProtoDef's `void` default (an empty packet).
    pub body: Option<Ty>,
}

/// The types and packets of one connection state and direction.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeTypes {
    pub state: String,
    pub direction: Direction,
    pub types: OrderedMap<TypeDef>,
    pub packets: Vec<PacketEntry>,
}

/// A parsed `protocol.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Protocol {
    pub global: OrderedMap<TypeDef>,
    pub scopes: Vec<ScopeTypes>,
}

const STATES: [&str; 5] = ["handshaking", "status", "login", "configuration", "play"];

impl Protocol {
    pub fn from_json(text: &str) -> Res<Protocol> {
        let root = loads(text).map_err(|e| format!("not JSON: {e}"))?;
        let Value::Map(root) = &root else {
            return Err("protocol.json is not an object".to_string());
        };
        let mut global = OrderedMap::new();
        if let Some(types) = root.get("types") {
            parse_type_table(types, &mut global).map_err(|e| format!("types: {e}"))?;
        }
        let mut scopes = Vec::new();
        for state in STATES {
            let Some(Value::Map(st)) = root.get(state) else {
                continue;
            };
            for (key, direction) in [
                ("toServer", Direction::Serverbound),
                ("toClient", Direction::Clientbound),
            ] {
                let Some(dir) = st.get(key) else { continue };
                let mut types = OrderedMap::new();
                if let Some(t) = dir.get("types") {
                    parse_type_table(t, &mut types).map_err(|e| format!("{state}.{key}: {e}"))?;
                }
                let packets = packet_table(&types)
                    .map_err(|e| format!("{state}.{key}: packet table: {e}"))?;
                scopes.push(ScopeTypes {
                    state: state.to_string(),
                    direction,
                    types,
                    packets,
                });
            }
        }
        Ok(Protocol { global, scopes })
    }
}

impl Protocol {
    /// Replace or add named types as a patch file says, and return the reasons given.
    ///
    /// The vendored descriptions stay byte for byte what upstream published; where a description
    /// is known to be wrong, a patch says so, with the evidence, and changes only what is
    /// wrong. A patch is `{"reason", "scope", "types"}`: `scope` is `global` or
    /// `<state>.<toClient|toServer>`, `types` maps type names to protodef definitions. A patch to
    /// a scope's `packet` type changes its routes.
    pub fn apply_patches(&mut self, text: &str) -> Res<Vec<String>> {
        let root = loads(text).map_err(|e| format!("patch file is not JSON: {e}"))?;
        let Some(Value::List(patches)) = root.get("patches") else {
            return Err("patch file has no \"patches\" list".to_string());
        };
        let mut reasons = Vec::new();
        let mut touched: Vec<usize> = Vec::new();
        for patch in patches.iter() {
            let reason = str_of(
                patch.get("reason").ok_or("a patch needs a \"reason\"")?,
                "reason",
            )?;
            let scope = str_of(
                patch.get("scope").ok_or("a patch needs a \"scope\"")?,
                "scope",
            )?;
            let Some(Value::Map(types)) = patch.get("types") else {
                return Err(format!("patch {reason:?} has no \"types\" object"));
            };
            let table: &mut OrderedMap<TypeDef> = if scope == "global" {
                &mut self.global
            } else {
                let (state, dir) = scope
                    .split_once('.')
                    .ok_or_else(|| format!("patch scope {scope:?}"))?;
                let direction = match dir {
                    "toClient" => Direction::Clientbound,
                    "toServer" => Direction::Serverbound,
                    other => return Err(format!("patch scope direction {other:?}")),
                };
                let ix = self
                    .scopes
                    .iter()
                    .position(|s| s.state == state && s.direction == direction)
                    .ok_or_else(|| {
                        format!("patch scope {scope:?} does not exist in the protocol")
                    })?;
                if !touched.contains(&ix) {
                    touched.push(ix);
                }
                &mut self.scopes[ix].types
            };
            for (name, def) in types.iter() {
                let ty =
                    parse_ty(def).map_err(|e| format!("patch {reason:?}: type {name}: {e}"))?;
                table.insert(name, TypeDef::Def(ty));
            }
            reasons.push(reason);
        }
        for ix in touched {
            let scope = &mut self.scopes[ix];
            scope.packets = packet_table(&scope.types).map_err(|e| {
                format!(
                    "{}.{}: packet table: {e}",
                    scope.state,
                    scope.direction.as_str()
                )
            })?;
        }
        Ok(reasons)
    }
}

fn parse_type_table(v: &Value, out: &mut OrderedMap<TypeDef>) -> Res<()> {
    let Value::Map(m) = v else {
        return Err("type table is not an object".to_string());
    };
    for (name, def) in m.iter() {
        let parsed = match def {
            Value::Str(s) if &**s == "native" => TypeDef::Native,
            other => TypeDef::Def(parse_ty(other).map_err(|e| format!("type {name}: {e}"))?),
        };
        out.insert(name, parsed);
    }
    Ok(())
}

fn str_of(v: &Value, what: &str) -> Res<String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{what}: expected a string, got {}",
            other.type_name()
        )),
    }
}

fn int_of(v: &Value, what: &str) -> Res<i128> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!(
            "{what}: expected an integer, got {}",
            other.type_name()
        )),
    }
}

/// `"0x1f"`, `"31"` or `"-1"` as an integer.
pub fn parse_int_key(key: &str) -> Option<i128> {
    let (neg, digits) = match key.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, key),
    };
    let value = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => i128::from_str_radix(hex, 16).ok()?,
        None => {
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            digits.parse::<i128>().ok()?
        }
    };
    Some(if neg { -value } else { value })
}

fn parse_count(args: &OrderedMap<Value>, what: &str) -> Res<Count> {
    if let Some(ct) = args.get("countType") {
        return Ok(Count::Type(Box::new(
            parse_ty(ct).map_err(|e| format!("{what} countType: {e}"))?,
        )));
    }
    match args.get("count") {
        Some(Value::Int(n)) => Ok(Count::Literal(*n)),
        Some(Value::Str(s)) => Ok(Count::Field(s.to_string())),
        Some(other) => Err(format!("{what}: bad count {}", other.type_name())),
        None => Err(format!("{what} needs count or countType")),
    }
}

pub fn parse_ty(v: &Value) -> Res<Ty> {
    match v {
        Value::Str(s) => Ok(Ty::Name(s.to_string())),
        Value::List(items) => {
            let Some(Value::Str(ctor)) = items.first() else {
                return Err("type constructor list must start with a name".to_string());
            };
            parse_ctor(ctor, items.get(1))
        }
        other => Err(format!(
            "a type is a string or a list, got {}",
            other.type_name()
        )),
    }
}

fn obj<'a>(arg: Option<&'a Value>, ctor: &str) -> Res<&'a OrderedMap<Value>> {
    match arg {
        Some(Value::Map(m)) => Ok(m),
        _ => Err(format!("{ctor} needs an object argument")),
    }
}

fn parse_ctor(ctor: &str, arg: Option<&Value>) -> Res<Ty> {
    match ctor {
        "container" => {
            let Some(Value::List(fields)) = arg else {
                return Err("container needs a field list".to_string());
            };
            let mut out = Vec::with_capacity(fields.len());
            for f in fields.iter() {
                let Value::Map(fm) = f else {
                    return Err("container field is not an object".to_string());
                };
                let anon = matches!(fm.get("anon"), Some(Value::Bool(true)));
                let name = match fm.get("name") {
                    Some(v) => Some(str_of(v, "field name")?),
                    None => None,
                };
                let ty_v = fm.get("type").ok_or("container field has no type")?;
                let ty = parse_ty(ty_v)
                    .map_err(|e| format!("field {}: {e}", name.as_deref().unwrap_or("<anon>")))?;
                out.push(Field { name, anon, ty });
            }
            Ok(Ty::Container(out))
        }
        "array" => {
            let a = obj(arg, "array")?;
            let elem = parse_ty(a.get("type").ok_or("array has no type")?)?;
            Ok(Ty::Array {
                count: parse_count(a, "array")?,
                elem: Box::new(elem),
            })
        }
        "buffer" => Ok(Ty::Buffer(parse_count(obj(arg, "buffer")?, "buffer")?)),
        "pstring" => {
            let a = obj(arg, "pstring")?;
            if let Some(enc) = a.get("encoding")
                && str_of(enc, "pstring encoding")? != "utf8"
            {
                return Err("pstring encoding other than utf8".to_string());
            }
            Ok(Ty::PString(parse_count(a, "pstring")?))
        }
        "option" => Ok(Ty::Option(Box::new(parse_ty(
            arg.ok_or("option needs a type")?,
        )?))),
        "switch" => {
            let a = obj(arg, "switch")?;
            if a.get("compareToValue").is_some() {
                return Err("switch compareToValue is not supported".to_string());
            }
            let compare = str_of(
                a.get("compareTo").ok_or("switch has no compareTo")?,
                "compareTo",
            )?;
            let Some(Value::Map(fields)) = a.get("fields") else {
                return Err("switch has no fields".to_string());
            };
            let mut arms = Vec::with_capacity(fields.len());
            for (key, ty) in fields.iter() {
                arms.push((
                    key.to_string(),
                    parse_ty(ty).map_err(|e| format!("switch arm {key}: {e}"))?,
                ));
            }
            let default = match a.get("default") {
                Some(d) => Some(parse_ty(d).map_err(|e| format!("switch default: {e}"))?),
                None => None,
            };
            Ok(Ty::Switch(Box::new(Switch {
                compare,
                arms,
                default,
            })))
        }
        "mapper" => {
            let a = obj(arg, "mapper")?;
            let base = parse_ty(a.get("type").ok_or("mapper has no type")?)?;
            let Some(Value::Map(m)) = a.get("mappings") else {
                return Err("mapper has no mappings".to_string());
            };
            let mut mappings = Vec::with_capacity(m.len());
            for (k, v) in m.iter() {
                let raw = parse_int_key(k)
                    .ok_or_else(|| format!("mapper key {k:?} is not an integer"))?;
                let name = match v {
                    Value::Str(s) => s.to_string(),
                    Value::Int(i) => i.to_string(),
                    Value::Bool(b) => b.to_string(),
                    other => return Err(format!("mapper value for {k}: {}", other.type_name())),
                };
                mappings.push((raw, name));
            }
            Ok(Ty::Mapper {
                base: Box::new(base),
                mappings,
            })
        }
        "bitfield" => {
            let Some(Value::List(members)) = arg else {
                return Err("bitfield needs a member list".to_string());
            };
            let mut out = Vec::with_capacity(members.len());
            for m in members.iter() {
                let Value::Map(mm) = m else {
                    return Err("bitfield member is not an object".to_string());
                };
                let name = str_of(
                    mm.get("name").ok_or("bitfield member has no name")?,
                    "bitfield name",
                )?;
                let size = int_of(
                    mm.get("size").ok_or("bitfield member has no size")?,
                    "bitfield size",
                )?;
                let signed = matches!(mm.get("signed"), Some(Value::Bool(true)));
                let size = u32::try_from(size).map_err(|_| format!("bitfield size {size}"))?;
                out.push(Bits { name, size, signed });
            }
            Ok(Ty::Bitfield(out))
        }
        "bitflags" => {
            let a = obj(arg, "bitflags")?;
            let base = str_of(
                a.get("type").ok_or("bitflags has no type")?,
                "bitflags type",
            )?;
            let shift = matches!(a.get("shift"), Some(Value::Bool(true)));
            let mut flags = Vec::new();
            match a.get("flags") {
                Some(Value::List(names)) => {
                    for (i, n) in names.iter().enumerate() {
                        let bit = u32::try_from(i).map_err(|_| "too many flags".to_string())?;
                        let mask = 1u64.checked_shl(bit).ok_or("bitflags beyond 64 bits")?;
                        flags.push((str_of(n, "flag name")?, mask));
                    }
                }
                Some(Value::Map(m)) => {
                    for (k, v) in m.iter() {
                        let n = u64::try_from(int_of(v, "flag value")?)
                            .map_err(|_| "negative flag".to_string())?;
                        let mask = if shift {
                            1u64.checked_shl(u32::try_from(n).map_err(|_| "flag shift")?)
                        } else {
                            Some(n)
                        };
                        flags.push((k.to_string(), mask.ok_or("flag shift beyond 64 bits")?));
                    }
                }
                _ => return Err("bitflags has no flags".to_string()),
            }
            Ok(Ty::Bitflags { base, flags })
        }
        "entityMetadataLoop" => {
            let a = obj(arg, "entityMetadataLoop")?;
            let end = int_of(
                a.get("endVal").ok_or("entityMetadataLoop has no endVal")?,
                "endVal",
            )?;
            let elem = parse_ty(a.get("type").ok_or("entityMetadataLoop has no type")?)?;
            Ok(Ty::MetaLoop {
                end,
                elem: Box::new(elem),
            })
        }
        "topBitSetTerminatedArray" => {
            let a = obj(arg, "topBitSetTerminatedArray")?;
            let elem = parse_ty(
                a.get("type")
                    .ok_or("topBitSetTerminatedArray has no type")?,
            )?;
            Ok(Ty::TopBitSet(Box::new(elem)))
        }
        "registryEntryHolder" => {
            let a = obj(arg, "registryEntryHolder")?;
            let base_name = str_of(
                a.get("baseName")
                    .ok_or("registryEntryHolder has no baseName")?,
                "baseName",
            )?;
            let (other_name, other) = named_type(a.get("otherwise"), "otherwise")?;
            Ok(Ty::Holder {
                base_name,
                other_name,
                other: Box::new(other),
            })
        }
        "registryEntryHolderSet" => {
            let a = obj(arg, "registryEntryHolderSet")?;
            let (base_name, base) = named_type(a.get("base"), "base")?;
            let (other_name, other) = named_type(a.get("otherwise"), "otherwise")?;
            Ok(Ty::HolderSet {
                base_name,
                base: Box::new(base),
                other_name,
                other: Box::new(other),
            })
        }
        name => {
            // A use of a parametrised named type.
            let mut args = Vec::new();
            if let Some(Value::Map(m)) = arg {
                for (k, v) in m.iter() {
                    args.push((k.to_string(), str_of(v, "type argument")?));
                }
            } else if arg.is_some() {
                return Err(format!("{name}: arguments must be an object"));
            }
            Ok(Ty::Call {
                name: name.to_string(),
                args,
            })
        }
    }
}

fn named_type(v: Option<&Value>, what: &str) -> Res<(String, Ty)> {
    let Some(Value::Map(m)) = v else {
        return Err(format!("{what} must be an object"));
    };
    let name = str_of(
        m.get("name").ok_or_else(|| format!("{what} has no name"))?,
        "name",
    )?;
    let ty = parse_ty(m.get("type").ok_or_else(|| format!("{what} has no type"))?)?;
    Ok((name, ty))
}

/// The `packet` container of a scope: a varint-mapped name, then a switch on it.
fn packet_table(types: &OrderedMap<TypeDef>) -> Res<Vec<PacketEntry>> {
    let Some(TypeDef::Def(Ty::Container(fields))) = types.get("packet") else {
        return Ok(Vec::new());
    };
    let name_field = fields
        .iter()
        .find(|f| f.name.as_deref() == Some("name"))
        .ok_or("no name field")?;
    let params = fields
        .iter()
        .find(|f| f.name.as_deref() == Some("params"))
        .ok_or("no params field")?;
    let Ty::Mapper { mappings, .. } = &name_field.ty else {
        return Err("packet.name is not a mapper".to_string());
    };
    let Ty::Switch(sw) = &params.ty else {
        return Err("packet.params is not a switch".to_string());
    };
    let mut out = Vec::with_capacity(mappings.len());
    for (id, name) in mappings {
        let body = sw
            .arms
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, t)| t.clone());
        let body = match body {
            Some(Ty::Name(n)) if n == "void" => None,
            other => other,
        };
        out.push(PacketEntry {
            id: *id,
            name: name.clone(),
            body,
        });
    }
    Ok(out)
}
