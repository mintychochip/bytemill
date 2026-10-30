//! The binding table `emit rstom-tree` reads next to the shape ledger.
//!
//! A shape says how one packet's hand fields map to BML wire fields. Most hand types are shared:
//! `PlayerHand` is written as `ordinal()` and read through `from_ordinal` wherever it appears, and
//! BML's `text_component` is carried by `Component` through `COMPONENT` in every packet. The
//! table holds those conversions once; a shape's own keys win over it.
//!
//! ```json
//! {
//!   "types": {
//!     "PlayerHand": {
//!       "write": "$.ordinal()",
//!       "read": "PlayerHand::from_ordinal($)",
//!       "imports": ["use crate::entity::PlayerHand;"]
//!     },
//!     "Pos": {
//!       "wire": ["f64be", "f64be", "f64be", "f32be", "f32be"],
//!       "write": ["$.x()", "$.y()", "$.z()", "$.yaw()", "$.pitch()"],
//!       "read": "Pos::new($0, $1, $2, $3, $4)"
//!     }
//!   },
//!   "wire_types": {
//!     "text_component": { "rust": "Component", "codec": "COMPONENT", "imports": ["..."] }
//!   }
//! }
//! ```
//!
//! In a `write` template `$` is the hand place the value comes from. In a `read` template `$` is
//! the value read off the wire for a one-field type, and `$0`, `$1`, ... are the carried wire
//! fields' locals in order for a span.

use crate::errors::{BmlResult, emit};
use crate::json::loads;
use crate::ordmap::OrderedMap;
use crate::shapes::model::WriteSpec;
use crate::value::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// Conversions of one hand type over a sequence of BML wire types.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TypeBinding {
    /// The BML types of the wire fields the hand type carries, in order (`[]` after a repeated
    /// one); a binding applies only to fields carrying exactly these. Empty applies to any.
    pub wire: Vec<String>,
    pub write: Option<WriteSpec>,
    pub read: Option<String>,
    /// The `Type` that carries the wire fields and yields the hand type itself.
    pub codec: Option<String>,
    /// Buffer functions in place of a codec: a statement writing `$`, an expression reading it.
    pub put: Option<String>,
    pub take: Option<String>,
    pub imports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bindings {
    /// Each hand type's variants; the first whose `wire` types match a field is used.
    pub types: OrderedMap<Vec<TypeBinding>>,
    /// Hand types that are `Copy`, beyond the primitives: written and read by value.
    pub copy: BTreeSet<String>,
}

impl Bindings {
    pub fn empty() -> Bindings {
        Bindings::default()
    }

    /// The table next to `shapes_dir` (`<shapes>/../bindings.json`), or an empty one when absent.
    pub fn beside(shapes_dir: &Path) -> BmlResult<Bindings> {
        let Some(parent) = shapes_dir.parent() else {
            return Ok(Bindings::empty());
        };
        let path = parent.join("bindings.json");
        if !path.is_file() {
            return Ok(Bindings::empty());
        }
        Bindings::load(&path)
    }

    pub fn load(path: &Path) -> BmlResult<Bindings> {
        let text =
            std::fs::read_to_string(path).map_err(|e| emit(format!("{}: {e}", path.display())))?;
        Bindings::parse(&text).map_err(|e| emit(format!("{}: {}", path.display(), e.message)))
    }

    pub fn parse(text: &str) -> BmlResult<Bindings> {
        let Value::Map(root) = loads(text)? else {
            return Err(emit("bindings: expected an object"));
        };
        let mut types: OrderedMap<Vec<TypeBinding>> = OrderedMap::new();
        if let Some(Value::Map(t)) = root.get("types") {
            for (name, v) in t.iter() {
                let variants = match v {
                    Value::List(items) => items
                        .iter()
                        .map(|i| type_binding(name, i))
                        .collect::<BmlResult<Vec<_>>>()?,
                    one => vec![type_binding(name, one)?],
                };
                types.insert(name, variants);
            }
        }
        let copy = str_list(root.get("copy")).into_iter().collect();
        Ok(Bindings { types, copy })
    }

    /// The variant of `hand_type`'s binding that carries wire fields of `wire_types`.
    pub fn lookup(&self, hand_type: &str, wire_types: &[String]) -> Option<&TypeBinding> {
        self.types
            .get(hand_type)?
            .iter()
            .find(|b| b.wire.is_empty() || b.wire == wire_types)
    }
}

fn opt(m: &OrderedMap<Value>, key: &str) -> Option<String> {
    match m.get(key) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn type_binding(name: &str, v: &Value) -> BmlResult<TypeBinding> {
    let Value::Map(m) = v else {
        return Err(emit(format!("bindings: type {name} must be an object")));
    };
    let write = match m.get("write") {
        None => None,
        Some(Value::Str(s)) => Some(WriteSpec::One(s.to_string())),
        Some(Value::List(items)) => Some(WriteSpec::List(
            items
                .iter()
                .map(|i| match i {
                    Value::Str(s) => Ok(s.to_string()),
                    _ => Err(emit(format!(
                        "bindings: type {name}: write templates are strings"
                    ))),
                })
                .collect::<BmlResult<Vec<_>>>()?,
        )),
        Some(_) => {
            return Err(emit(format!(
                "bindings: type {name}: write is a string or a list of strings"
            )));
        }
    };
    let read = opt(m, "read");
    Ok(TypeBinding {
        wire: str_list(m.get("wire")),
        write,
        read,
        codec: opt(m, "codec"),
        put: opt(m, "put"),
        take: opt(m, "take"),
        imports: str_list(m.get("imports")),
    })
}
