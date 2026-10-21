//! Shape model: the hand-struct description, its JSON form and the error type.
//! Ports `bps/shapes/model.py`.

use crate::errors::{BmlError, BmlResult, ErrorKind};
use crate::json::{Value, dumps_pretty, loads};
use crate::ordmap::OrderedMap;

pub type ShapeError = BmlError;

pub fn shape_error(message: impl Into<String>) -> BmlError {
    BmlError::new(ErrorKind::Shape, message)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldShape {
    pub name: String,
    pub type_: String,
    pub docs: Vec<String>,
    pub wire: Option<String>,
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

    let fields: Vec<Value> = s
        .fields
        .iter()
        .map(|f| {
            let mut obj = OrderedMap::new();
            obj.insert("name", Value::str(f.name.clone()));
            obj.insert("type", Value::str(f.type_.clone()));
            obj.insert(
                "docs",
                Value::list(f.docs.iter().cloned().map(Value::str).collect()),
            );
            if let Some(wire) = &f.wire {
                obj.insert("wire", Value::str(wire.clone()));
            }
            Value::map(obj)
        })
        .collect();
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
    let mut out = dumps_pretty(&Value::map(root));
    out.push('\n');
    out
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
            .map(|f| {
                let Value::Map(m) = f else {
                    return Err(shape_error("shape json: field must be an object"));
                };
                Ok(FieldShape {
                    name: get_str(m, "name")?,
                    type_: get_str(m, "type")?,
                    docs: str_list(m.get("docs")),
                    wire: opt_str(m, "wire"),
                })
            })
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
    })
}
