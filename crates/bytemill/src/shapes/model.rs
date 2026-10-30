//! Shape model: the hand-struct description, its JSON form and the error type.
//! Ports `bps/shapes/model.py`.

use crate::errors::{BmlError, BmlResult, ErrorKind};
use crate::json::{Value, dumps_pretty, loads};
use crate::ordmap::OrderedMap;

pub type ShapeError = BmlError;

pub fn shape_error(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Shape, message)
}

/// The write templates of a hand field: one wire value, one per carried wire field in order, or
/// one per carried wire ident. `$` stands for the hand place the value comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteSpec {
    One(String),
    List(Vec<String>),
    Map(OrderedMap<String>),
}

/// How the elements of a repeated wire field are projected to and from a hand element: the
/// element's own hand fields, which bind to the element's wire fields like a packet's do.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemBind {
    /// The hand element's fields. A single field named `_` is the element itself; `_0`, `_1`, ...
    /// are tuple slots. Empty derives the fields from the hand element type.
    pub fields: Vec<FieldShape>,
    /// The hand element from the field values (`Entry { namespace, id }`); absent builds a
    /// tuple, the one field, or a struct literal over the fields.
    pub ctor: Option<String>,
    /// Element wire idents no hand field carries, with the expression the write puts there.
    pub constants: OrderedMap<String>,
}

/// A packet module that adds a serializer for a type another module defines: the packet class
/// one hand type serves under two BML packets (a configuration and a play form).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AliasShape {
    /// The path of the hand type, as a `use` item would name it.
    pub path: String,
    /// The inherent function of the type that returns this module's serializer.
    pub serializer_fn: String,
}

/// How one hand field maps onto the BML wire fields.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldBind {
    /// The wire idents the field carries; `None` takes the same-named wire field,
    /// `Some([])` keeps the field off the wire.
    pub wire: Option<Vec<String>>,
    pub write: Option<WriteSpec>,
    /// The hand value from the wire locals: `$` is the (first) carried local, `$0`, `$1`, ...
    /// the carried locals in order, and a wire ident its own local.
    pub read: Option<String>,
    /// The `Type` that carries all the carried wire fields, replacing their type constants.
    pub codec: Option<String>,
    /// For a hand type with its own buffer functions instead of a `Type`: a statement that writes
    /// the hand value `$` to `buffer`, and an expression that reads it. Carries the wire fields
    /// like a codec does.
    pub put: Option<String>,
    pub take: Option<String>,
    /// An `Option` field whose guarded wire fields are written from the whole `Option` (the
    /// templates decide what `None` writes) rather than only when it is `Some`.
    pub write_whole: bool,
    /// The `read` template of an `Option` field already yields an `Option` (a lookup that can
    /// fail), so the guarded wire's `None` and the template's `None` both read as `None`.
    pub read_flat: bool,
    /// A carrier whose first wire field is guarded still fills a plain (not `Option`) hand
    /// field: the guard is part of the encoding the carrier owns.
    pub whole_span: bool,
    /// A hand field that is not an `Option` over guarded wire fields: the guard alone decides
    /// whether they are written, and a read that skips them leaves the type's default.
    pub guarded_default: bool,
    /// What a guarded plain field reads as when its guard skips the wire fields: an expression
    /// (implies `guarded_default`); absent means the type's default.
    pub absent: Option<String>,
    pub item: Option<ItemBind>,
    /// A hand field of a struct type whose own fields bind to the wire fields (flat, in this
    /// scope): the struct is read by building it from them, and written from them.
    pub nest: Option<ItemBind>,
}

impl FieldBind {
    pub fn is_empty(&self) -> bool {
        *self == FieldBind::default()
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldShape {
    pub name: String,
    pub type_: String,
    pub docs: Vec<String>,
    pub bind: FieldBind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewShape {
    pub present: bool,
    pub const_: bool,
    pub must_use: bool,
    pub attrs: Vec<String>,
    pub docs: Vec<String>,
    pub body: Vec<String>,
    pub params: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SerializerShape {
    pub style: String,
    pub name: String,
    pub public: bool,
    pub docs: Vec<String>,
    pub read_via_new: bool,
}

/// A hand item the shape keeps verbatim instead of generating it.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub construct: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shape {
    pub cls: String,
    pub module: String,
    pub module_docs: Vec<String>,
    pub struct_docs: Vec<String>,
    pub derives: Vec<String>,
    pub form: String,
    pub fields: Vec<FieldShape>,
    pub new: NewShape,
    pub serializer_fn_docs: Vec<String>,
    pub serializer_fn_must_use: bool,
    pub serializer: SerializerShape,
    pub extras: Option<String>,
    pub imports: Vec<String>,
    pub tests: Option<String>,
    /// Insertion-ordered, like the Python dict.
    pub hooks: OrderedMap<Vec<String>>,
    pub accepted: Vec<Accepted>,
    pub drops: Vec<String>,
    /// Wire idents no hand field carries, with the expression the write puts there; a read
    /// consumes and drops them.
    pub constants: OrderedMap<String>,
    /// The hand fields are bound to the wire fields (`tree_bound`), with or without keys of
    /// their own, rather than mirroring them one to one.
    pub bound: bool,
    /// `use` lines the bindings need that the hand file did not import for its struct.
    pub uses: Vec<String>,
    /// The module defines no type of its own: it serializes the hand type named here.
    pub alias: Option<AliasShape>,
    /// Attributes on the `impl PacketSerializer` of a bound packet (the lint allowances its
    /// conversions need), one line each.
    pub serializer_attrs: Vec<String>,
}

impl Shape {
    /// Whether the shape binds hand fields to wire fields, rather than mirroring them by name
    /// and type alone.
    pub fn has_bindings(&self) -> bool {
        self.bound || !self.constants.is_empty() || self.fields.iter().any(|f| !f.bind.is_empty())
    }
}

/// `json.dumps(obj, indent=2) + "\n"`.
pub fn shape_to_json(s: &Shape) -> String {
    let mut root = OrderedMap::new();
    root.insert("class", Value::str(s.cls.clone()));
    root.insert("module", Value::str(s.module.clone()));
    root.insert(
        "module_docs",
        Value::list(s.module_docs.iter().cloned().map(Value::str).collect()),
    );
    let mut struct_obj = OrderedMap::new();
    struct_obj.insert(
        "docs",
        Value::list(s.struct_docs.iter().cloned().map(Value::str).collect()),
    );
    struct_obj.insert(
        "derives",
        Value::list(s.derives.iter().cloned().map(Value::str).collect()),
    );
    struct_obj.insert("form", Value::str(s.form.clone()));
    root.insert("struct", Value::map(struct_obj));

    let fields: Vec<Value> = s.fields.iter().map(field_to_value).collect();
    root.insert("fields", Value::list(fields));

    let mut new_obj = OrderedMap::new();
    new_obj.insert("present", Value::Bool(s.new.present));
    new_obj.insert("must_use", Value::Bool(s.new.must_use));
    new_obj.insert("const", Value::Bool(s.new.const_));
    new_obj.insert(
        "attrs",
        Value::list(s.new.attrs.iter().cloned().map(Value::str).collect()),
    );
    new_obj.insert(
        "docs",
        Value::list(s.new.docs.iter().cloned().map(Value::str).collect()),
    );
    new_obj.insert(
        "body",
        Value::list(s.new.body.iter().cloned().map(Value::str).collect()),
    );
    if let Some(p) = &s.new.params {
        new_obj.insert("params", Value::str(p.clone()));
    }
    root.insert("new", Value::map(new_obj));

    let mut ser_fn = OrderedMap::new();
    ser_fn.insert(
        "docs",
        Value::list(
            s.serializer_fn_docs
                .iter()
                .cloned()
                .map(Value::str)
                .collect(),
        ),
    );
    ser_fn.insert("must_use", Value::Bool(s.serializer_fn_must_use));
    root.insert("serializer_fn", Value::map(ser_fn));

    let mut ser = OrderedMap::new();
    ser.insert("style", Value::str(s.serializer.style.clone()));
    ser.insert("name", Value::str(s.serializer.name.clone()));
    ser.insert("pub", Value::Bool(s.serializer.public));
    ser.insert(
        "docs",
        Value::list(s.serializer.docs.iter().cloned().map(Value::str).collect()),
    );
    ser.insert("read_via_new", Value::Bool(s.serializer.read_via_new));
    root.insert("serializer_struct", Value::map(ser));

    root.insert(
        "extras",
        match &s.extras {
            Some(e) => Value::str(e.clone()),
            None => Value::Null,
        },
    );
    if !s.imports.is_empty() {
        root.insert(
            "imports",
            Value::list(s.imports.iter().cloned().map(Value::str).collect()),
        );
    }
    if let Some(t) = &s.tests {
        root.insert("tests", Value::str(t.clone()));
    }
    if !s.hooks.is_empty() {
        let mut hooks = OrderedMap::new();
        for (k, v) in s.hooks.iter() {
            hooks.insert(k, Value::list(v.iter().cloned().map(Value::str).collect()));
        }
        root.insert("hooks", Value::map(hooks));
    }
    if !s.accepted.is_empty() {
        let items = s
            .accepted
            .iter()
            .map(|a| {
                let mut obj = OrderedMap::new();
                obj.insert("construct", Value::str(a.construct.clone()));
                obj.insert("reason", Value::str(a.reason.clone()));
                Value::map(obj)
            })
            .collect();
        root.insert("accepted", Value::list(items));
    }
    if !s.drops.is_empty() {
        root.insert(
            "drops",
            Value::list(s.drops.iter().cloned().map(Value::str).collect()),
        );
    }
    if !s.constants.is_empty() {
        root.insert("constants", string_map_to_value(&s.constants));
    }
    if s.bound {
        root.insert("bound", Value::Bool(true));
    }
    if let Some(a) = &s.alias {
        let mut o = OrderedMap::new();
        o.insert("path", Value::str(a.path.clone()));
        o.insert("serializer_fn", Value::str(a.serializer_fn.clone()));
        root.insert("alias", Value::map(o));
    }
    if !s.uses.is_empty() {
        root.insert(
            "uses",
            Value::list(s.uses.iter().cloned().map(Value::str).collect()),
        );
    }
    if !s.serializer_attrs.is_empty() {
        root.insert(
            "serializer_attrs",
            Value::list(s.serializer_attrs.iter().cloned().map(Value::str).collect()),
        );
    }
    let mut out = dumps_pretty(&Value::map(root));
    out.push('\n');
    out
}

fn write_spec_to_value(w: &WriteSpec) -> Value {
    match w {
        WriteSpec::One(s) => Value::str(s.clone()),
        WriteSpec::List(l) => Value::list(l.iter().cloned().map(Value::str).collect()),
        WriteSpec::Map(m) => string_map_to_value(m),
    }
}

fn string_map_to_value(m: &OrderedMap<String>) -> Value {
    let mut o = OrderedMap::new();
    for (k, v) in m.iter() {
        o.insert(k, Value::str(v.clone()));
    }
    Value::map(o)
}

fn string_map_from_value(v: Option<&Value>, what: &str) -> BmlResult<OrderedMap<String>> {
    let mut out = OrderedMap::new();
    match v {
        None => {}
        Some(Value::Map(m)) => {
            for (k, v) in m.iter() {
                match v {
                    Value::Str(s) => out.insert(k, s.to_string()),
                    _ => {
                        return Err(shape_error(format!(
                            "shape json: {what} must map to strings"
                        )));
                    }
                }
            }
        }
        Some(_) => return Err(shape_error(format!("shape json: {what} must be an object"))),
    }
    Ok(out)
}

fn write_spec_from_value(v: &Value) -> BmlResult<WriteSpec> {
    match v {
        Value::Str(s) => Ok(WriteSpec::One(s.to_string())),
        Value::List(_) => Ok(WriteSpec::List(str_list(Some(v)))),
        Value::Map(_) => Ok(WriteSpec::Map(string_map_from_value(Some(v), "write")?)),
        _ => Err(shape_error("shape json: write is a string, list or object")),
    }
}

fn field_to_value(f: &FieldShape) -> Value {
    let mut obj = OrderedMap::new();
    obj.insert("name", Value::str(f.name.clone()));
    obj.insert("type", Value::str(f.type_.clone()));
    obj.insert(
        "docs",
        Value::list(f.docs.iter().cloned().map(Value::str).collect()),
    );
    bind_to_obj(&f.bind, &mut obj);
    Value::map(obj)
}

fn item_to_value(i: &ItemBind) -> Value {
    let mut o = OrderedMap::new();
    o.insert(
        "fields",
        Value::list(i.fields.iter().map(field_to_value).collect()),
    );
    if let Some(c) = &i.ctor {
        o.insert("ctor", Value::str(c.clone()));
    }
    if !i.constants.is_empty() {
        o.insert("constants", string_map_to_value(&i.constants));
    }
    Value::map(o)
}

fn field_from_value(f: &Value) -> BmlResult<FieldShape> {
    let Value::Map(m) = f else {
        return Err(shape_error("shape json: field must be an object"));
    };
    Ok(FieldShape {
        name: get_str(m, "name")?,
        type_: get_str(m, "type")?,
        docs: str_list(m.get("docs")),
        bind: bind_from_map(m)?,
    })
}

fn item_from_value(v: &Value) -> BmlResult<ItemBind> {
    let Value::Map(m) = v else {
        return Err(shape_error("shape json: item must be an object"));
    };
    let fields = match m.get("fields") {
        Some(Value::List(items)) => items
            .iter()
            .map(field_from_value)
            .collect::<BmlResult<Vec<_>>>()?,
        _ => Vec::new(),
    };
    Ok(ItemBind {
        fields,
        ctor: opt_str(m, "ctor"),
        constants: string_map_from_value(m.get("constants"), "item constants")?,
    })
}

fn bind_to_obj(b: &FieldBind, obj: &mut OrderedMap<Value>) {
    if let Some(wire) = &b.wire {
        match wire.as_slice() {
            [one] => obj.insert("wire", Value::str(one.clone())),
            many => obj.insert(
                "wire",
                Value::list(many.iter().cloned().map(Value::str).collect()),
            ),
        }
    }
    if let Some(w) = &b.write {
        obj.insert("write", write_spec_to_value(w));
    }
    for (key, v) in [
        ("read", &b.read),
        ("codec", &b.codec),
        ("put", &b.put),
        ("take", &b.take),
        ("absent", &b.absent),
    ] {
        if let Some(v) = v {
            obj.insert(key, Value::str(v.clone()));
        }
    }
    if b.write_whole {
        obj.insert("write_whole", Value::Bool(true));
    }
    if b.read_flat {
        obj.insert("read_flat", Value::Bool(true));
    }
    if b.whole_span {
        obj.insert("whole_span", Value::Bool(true));
    }
    if b.guarded_default {
        obj.insert("guarded_default", Value::Bool(true));
    }
    if let Some(i) = &b.item {
        obj.insert("item", item_to_value(i));
    }
    if let Some(i) = &b.nest {
        obj.insert("nest", item_to_value(i));
    }
}

fn bind_from_map(m: &OrderedMap<Value>) -> BmlResult<FieldBind> {
    let wire = match m.get("wire") {
        Some(Value::Str(s)) => Some(vec![s.to_string()]),
        Some(Value::List(items)) => Some(
            items
                .iter()
                .map(|i| match i {
                    Value::Str(s) => Ok(s.to_string()),
                    _ => Err(shape_error("shape json: wire idents are strings")),
                })
                .collect::<BmlResult<Vec<_>>>()?,
        ),
        _ => None,
    };
    let write = match m.get("write") {
        Some(v) => Some(write_spec_from_value(v)?),
        None => None,
    };
    let item = match m.get("item") {
        Some(v) => Some(item_from_value(v)?),
        None => None,
    };
    let nest = match m.get("nest") {
        Some(v) => Some(item_from_value(v)?),
        None => None,
    };
    Ok(FieldBind {
        wire,
        write,
        read: opt_str(m, "read"),
        codec: opt_str(m, "codec"),
        put: opt_str(m, "put"),
        take: opt_str(m, "take"),
        write_whole: matches!(m.get("write_whole"), Some(Value::Bool(true))),
        read_flat: matches!(m.get("read_flat"), Some(Value::Bool(true))),
        whole_span: matches!(m.get("whole_span"), Some(Value::Bool(true))),
        guarded_default: matches!(m.get("guarded_default"), Some(Value::Bool(true))),
        absent: opt_str(m, "absent"),
        item,
        nest,
    })
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

fn get_str(map: &OrderedMap<Value>, key: &str) -> BmlResult<String> {
    match map.get(key) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        _ => Err(shape_error(format!("shape json: missing string {key:?}"))),
    }
}

fn opt_str(map: &OrderedMap<Value>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

pub fn shape_from_json(text: &str) -> BmlResult<Shape> {
    let Value::Map(root) = loads(text)? else {
        return Err(shape_error("shape json: expected an object"));
    };
    let cls = get_str(&root, "class")?;
    let module = get_str(&root, "module")?;
    let module_docs = str_list(root.get("module_docs"));
    let Value::Map(st) = root.get("struct").cloned().unwrap_or(Value::Null) else {
        return Err(shape_error("shape json: missing struct"));
    };
    let struct_docs = str_list(st.get("docs"));
    let derives = str_list(st.get("derives"));
    let form = get_str(&st, "form")?;
    let fields = match root.get("fields") {
        Some(Value::List(items)) => items
            .iter()
            .map(field_from_value)
            .collect::<BmlResult<Vec<_>>>()?,
        _ => Vec::new(),
    };
    let Value::Map(new) = root.get("new").cloned().unwrap_or(Value::Null) else {
        return Err(shape_error("shape json: missing new"));
    };
    let bool_of = |m: &OrderedMap<Value>, k: &str| matches!(m.get(k), Some(Value::Bool(true)));
    let new = NewShape {
        present: bool_of(&new, "present"),
        const_: bool_of(&new, "const"),
        must_use: bool_of(&new, "must_use"),
        attrs: str_list(new.get("attrs")),
        docs: str_list(new.get("docs")),
        body: str_list(new.get("body")),
        params: opt_str(&new, "params"),
    };
    let Value::Map(ser_fn) = root.get("serializer_fn").cloned().unwrap_or(Value::Null) else {
        return Err(shape_error("shape json: missing serializer_fn"));
    };
    let serializer_fn_docs = str_list(ser_fn.get("docs"));
    let serializer_fn_must_use = matches!(ser_fn.get("must_use"), Some(Value::Bool(true)));
    let Value::Map(ser) = root
        .get("serializer_struct")
        .cloned()
        .unwrap_or(Value::Null)
    else {
        return Err(shape_error("shape json: missing serializer_struct"));
    };
    let serializer = SerializerShape {
        style: get_str(&ser, "style")?,
        name: get_str(&ser, "name")?,
        public: matches!(ser.get("pub"), Some(Value::Bool(true))),
        docs: str_list(ser.get("docs")),
        read_via_new: matches!(ser.get("read_via_new"), Some(Value::Bool(true))),
    };
    let extras = match root.get("extras") {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    };
    let imports = str_list(root.get("imports"));
    let tests = match root.get("tests") {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    };
    let mut hooks: OrderedMap<Vec<String>> = OrderedMap::new();
    if let Some(Value::Map(h)) = root.get("hooks") {
        for (k, v) in h.iter() {
            hooks.insert(k, str_list(Some(v)));
        }
    }
    let accepted = match root.get("accepted") {
        Some(Value::List(items)) => items
            .iter()
            .map(|a| {
                let Value::Map(m) = a else {
                    return Err(shape_error("shape json: accepted must be objects"));
                };
                Ok(Accepted {
                    construct: get_str(m, "construct")?,
                    reason: get_str(m, "reason")?,
                })
            })
            .collect::<BmlResult<Vec<_>>>()?,
        _ => Vec::new(),
    };
    let drops = str_list(root.get("drops"));
    let constants = string_map_from_value(root.get("constants"), "constants")?;
    let bound = matches!(root.get("bound"), Some(Value::Bool(true)));
    let serializer_attrs = str_list(root.get("serializer_attrs"));
    let uses = str_list(root.get("uses"));
    let alias = match root.get("alias") {
        Some(Value::Map(m)) => Some(AliasShape {
            path: get_str(m, "path")?,
            serializer_fn: get_str(m, "serializer_fn")?,
        }),
        _ => None,
    };
    Ok(Shape {
        cls,
        module,
        module_docs,
        struct_docs,
        derives,
        form,
        fields,
        new,
        serializer_fn_docs,
        serializer_fn_must_use,
        serializer,
        extras,
        imports,
        tests,
        hooks,
        accepted,
        drops,
        constants,
        bound,
        uses,
        alias,
        serializer_attrs,
    })
}
