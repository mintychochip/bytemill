//! Scopes, field registry and name assignment for one packet or encoding.
//!
//! ProtoDef resolves a reference (`flags/x`, `../action`, a lexical name) to a specific field of
//! a specific container. BPS resolves a name in a flat environment that every nested block
//! inherits and that a block's own fields shadow. Lowering therefore refers to fields by
//! [`FieldId`]; once a whole root is lowered, [`Arena::assign_names`] picks the final
//! names so that no block between a reference and its target re-declares the target's name
//! (the decoder inherits the environment; the encoder merges a block's fields up front, so even
//! a field declared after the reference would shadow it).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::ir::{FieldId, IExpr};
use super::model::Res;
use super::names;

pub type ScopeId = usize;

/// What a referenced field holds, as far as `switch` arm keys are concerned.
#[derive(Debug, Clone)]
pub enum Kind {
    Int,
    Bool,
    Str,
    /// `(raw, mapped name)` of a mapper field.
    Mapper(Rc<Vec<(i128, String)>>),
    /// A bitfield or bitflags value: addressed through its members.
    Struct,
    Other(String),
}

/// How a protodef name is read back.
#[derive(Debug, Clone)]
pub enum ValRef {
    Field(FieldId),
    /// An `option`: a presence flag and the value; absent reads as ProtoDef's `undefined`.
    Opt {
        flag: FieldId,
        value: FieldId,
    },
    /// A `void` field.
    Absent,
}

#[derive(Debug, Clone)]
pub struct Visible {
    pub val: ValRef,
    pub kind: Kind,
    /// Members of an inline bitfield / bitflags: `(member, field, kind)`.
    pub members: Vec<(String, FieldId, Kind)>,
}

#[derive(Debug)]
struct FieldRec {
    hint: String,
    scope: ScopeId,
}

#[derive(Debug)]
struct ScopeRec {
    /// The enclosing ProtoDef container scope (`..` goes here).
    proto_parent: Option<ScopeId>,
    /// The BPS block whose namespace this scope's fields live in (itself, unless spliced).
    ns: ScopeId,
    /// A block the importer invented: invisible to ProtoDef paths.
    transparent: bool,
    visible: Vec<(String, Visible)>,
}

#[derive(Debug, Default)]
pub struct Arena {
    fields: Vec<FieldRec>,
    scopes: Vec<ScopeRec>,
    refs: Vec<(ScopeId, FieldId)>,
}

/// A resolved reference: the value, and the presence flag when the value is optional.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub value: IExpr,
    pub flag: Option<IExpr>,
    pub kind: Kind,
}

impl Resolved {
    /// The expression for the referenced value, `absent` when an optional value is not there.
    pub fn or(&self, absent: i128) -> IExpr {
        match &self.flag {
            Some(flag) => IExpr::Tern(
                Box::new(flag.clone()),
                Box::new(self.value.clone()),
                Box::new(IExpr::Int(absent)),
            ),
            None => self.value.clone(),
        }
    }
}

impl Arena {
    pub fn new() -> Arena {
        Arena::default()
    }

    /// A ProtoDef container scope with its own BPS namespace.
    pub fn scope(&mut self, parent: Option<ScopeId>) -> ScopeId {
        self.push_scope(parent, None, false)
    }

    /// A block that exists only in the output (an option's inner block, an array element
    /// wrapper): its fields are not nameable from ProtoDef paths.
    pub fn transparent(&mut self, parent: ScopeId) -> ScopeId {
        self.push_scope(Some(parent), None, true)
    }

    /// A container scope whose fields splice into the namespace of `like`.
    pub fn spliced(&mut self, parent: ScopeId, like: ScopeId) -> ScopeId {
        let ns = self.scopes[like].ns;
        self.push_scope(Some(parent), Some(ns), false)
    }

    fn push_scope(
        &mut self,
        parent: Option<ScopeId>,
        ns: Option<ScopeId>,
        transparent: bool,
    ) -> ScopeId {
        let id = self.scopes.len();
        self.scopes.push(ScopeRec {
            proto_parent: parent,
            ns: ns.unwrap_or(id),
            transparent,
            visible: Vec::new(),
        });
        id
    }

    pub fn field(&mut self, scope: ScopeId, hint: &str) -> FieldId {
        self.fields.push(FieldRec {
            hint: hint.to_string(),
            scope,
        });
        self.fields.len() - 1
    }

    pub fn is_transparent(&self, scope: ScopeId) -> bool {
        self.scopes[scope].transparent
    }

    pub fn register(&mut self, scope: ScopeId, name: &str, vis: Visible) {
        if !self.scopes[scope].transparent {
            self.scopes[scope].visible.push((name.to_string(), vis));
        }
    }

    /// Record that an expression evaluated in `scope` reads `field`.
    pub fn note_ref(&mut self, scope: ScopeId, field: FieldId) {
        self.refs.push((scope, field));
    }

    pub fn note_expr(&mut self, scope: ScopeId, e: &IExpr) {
        let mut fs = Vec::new();
        e.fields(&mut fs);
        for f in fs {
            self.note_ref(scope, f);
        }
    }

    /// The nearest container scope at or above `scope` (skipping invented blocks).
    fn real(&self, mut scope: ScopeId) -> Option<ScopeId> {
        loop {
            if !self.scopes[scope].transparent {
                return Some(scope);
            }
            scope = self.scopes[scope].proto_parent?;
        }
    }

    fn find_visible(&self, mut scope: Option<ScopeId>, name: &str) -> Option<&Visible> {
        while let Some(s) = scope {
            if let Some((_, v)) = self.scopes[s].visible.iter().rev().find(|(n, _)| n == name) {
                return Some(v);
            }
            scope = self.scopes[s].proto_parent;
        }
        None
    }

    /// ProtoDef's path rules: leading `..` climb container scopes, the first name is looked up
    /// there and then lexically outward (the compiled code closes over enclosing variables),
    /// and later segments are members of a bitfield or bitflags value.
    pub fn resolve(&mut self, use_scope: ScopeId, path: &str) -> Res<Resolved> {
        let parts: Vec<&str> = path.split('/').collect();
        let mut at = self.real(use_scope);
        let mut i = 0;
        while parts.get(i) == Some(&"..") {
            let cur = at.ok_or_else(|| format!("reference {path:?} climbs above the root"))?;
            at = self.scopes[cur].proto_parent.and_then(|p| self.real(p));
            i += 1;
        }
        let name = *parts
            .get(i)
            .ok_or_else(|| format!("empty reference {path:?}"))?;
        let vis = self
            .find_visible(at, name)
            .ok_or_else(|| format!("reference {path:?}: no field named {name:?} in scope"))?
            .clone();
        let rest = &parts[i + 1..];
        let (value, flag, kind) = match rest {
            [] => match &vis.val {
                ValRef::Field(f) => (IExpr::Field(*f), None, vis.kind.clone()),
                ValRef::Opt { flag, value } => (
                    IExpr::Field(*value),
                    Some(IExpr::Field(*flag)),
                    vis.kind.clone(),
                ),
                ValRef::Absent => return Err(format!("reference {path:?} names a void field")),
            },
            [member] => {
                let (_, f, k) = vis
                    .members
                    .iter()
                    .find(|(m, _, _)| m == member)
                    .ok_or_else(|| {
                        format!("reference {path:?}: {name:?} has no member {member:?}")
                    })?;
                (IExpr::Field(*f), None, k.clone())
            }
            _ => {
                return Err(format!(
                    "reference {path:?} is nested deeper than one member"
                ));
            }
        };
        let r = Resolved { value, flag, kind };
        self.note_expr(use_scope, &r.value);
        if let Some(f) = &r.flag {
            self.note_expr(use_scope, f);
        }
        Ok(r)
    }

    /// Final names for every field, indexed by [`FieldId`].
    pub fn assign_names(&self) -> Res<Vec<String>> {
        let ns_of = |s: ScopeId| self.scopes[s].ns;
        // Parent namespace of each namespace owner.
        let mut ns_parent: HashMap<ScopeId, Option<ScopeId>> = HashMap::new();
        for (id, rec) in self.scopes.iter().enumerate() {
            if rec.ns == id {
                ns_parent.insert(id, rec.proto_parent.map(ns_of));
            }
        }
        // Targets referenced from a nested namespace, per namespace on the way up.
        let mut protected: HashMap<ScopeId, Vec<FieldId>> = HashMap::new();
        for &(use_scope, field) in &self.refs {
            let target_ns = ns_of(self.fields[field].scope);
            let mut x = ns_of(use_scope);
            while x != target_ns {
                protected.entry(x).or_default().push(field);
                x = ns_parent.get(&x).copied().flatten().ok_or_else(|| {
                    format!(
                        "field {:?} is read from outside its scope chain",
                        self.fields[field].hint
                    )
                })?;
            }
        }
        let mut by_ns: HashMap<ScopeId, Vec<FieldId>> = HashMap::new();
        for (fid, rec) in self.fields.iter().enumerate() {
            by_ns.entry(ns_of(rec.scope)).or_default().push(fid);
        }
        let mut out = vec![String::new(); self.fields.len()];
        // Namespace owners are created before the namespaces nested in them.
        for ns in 0..self.scopes.len() {
            if self.scopes[ns].ns != ns {
                continue;
            }
            let forbidden: HashSet<String> = protected
                .get(&ns)
                .into_iter()
                .flatten()
                .map(|&f| out[f].clone())
                .collect();
            let mut used: HashSet<String> = HashSet::new();
            for &fid in by_ns.get(&ns).into_iter().flatten() {
                let base = names::snake(&self.fields[fid].hint);
                let name = names::unique(&base, |c| used.contains(c) || forbidden.contains(c));
                used.insert(name.clone());
                out[fid] = name;
            }
        }
        Ok(out)
    }
}
