//! Byte spans recorded during decode and support for annotated hexdumps.

use std::rc::Rc;

use crate::ResolvedSchema;
use crate::errors::BmlResult;
use crate::ordmap::OrderedMap;
use crate::value::{Env, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanNode {
    Leaf(Span),
    Record {
        span: Span,
        fields: OrderedMap<SpanNode>,
    },
    List {
        span: Span,
        items: Vec<SpanNode>,
    },
}

pub type FieldSpans = OrderedMap<SpanNode>;

impl SpanNode {
    pub fn span(&self) -> Span {
        match self {
            SpanNode::Leaf(s) => *s,
            SpanNode::Record { span, .. } => *span,
            SpanNode::List { span, .. } => *span,
        }
    }
}

pub fn decode_packet_spans(
    resolved: &ResolvedSchema,
    packet: &str,
    data: &[u8],
    strict: bool,
) -> BmlResult<(Env, FieldSpans)> {
    crate::interp::engine::decode_packet_spans(resolved, packet, data, strict)
}

pub fn decode_frame_payload_spans(
    resolved: &ResolvedSchema,
    state: &str,
    direction: &str,
    data: &[u8],
) -> BmlResult<(i128, Env, FieldSpans)> {
    crate::interp::engine::decode_frame_payload_spans(resolved, state, direction, data)
}

pub fn spans_json(spans: &FieldSpans) -> crate::json::Value {
    fn node_json(node: &SpanNode) -> crate::json::Value {
        match node {
            SpanNode::Leaf(Span { offset, len }) => {
                let mut m = OrderedMap::new();
                m.insert("offset", Value::Int(*offset as i128));
                m.insert("len", Value::Int(*len as i128));
                Value::Map(Rc::new(m))
            }
            SpanNode::Record {
                span: Span { offset, len },
                fields,
            } => {
                let mut m = OrderedMap::new();
                m.insert("offset", Value::Int(*offset as i128));
                m.insert("len", Value::Int(*len as i128));
                m.insert("fields", fields_json(fields));
                Value::Map(Rc::new(m))
            }
            SpanNode::List {
                span: Span { offset, len },
                items,
            } => {
                let mut m = OrderedMap::new();
                m.insert("offset", Value::Int(*offset as i128));
                m.insert("len", Value::Int(*len as i128));
                let arr: Vec<Value> = items.iter().map(node_json).collect();
                m.insert("items", Value::list(arr));
                Value::Map(Rc::new(m))
            }
        }
    }
    fn fields_json(fs: &FieldSpans) -> crate::json::Value {
        let mut m = OrderedMap::new();
        for (k, n) in fs.iter() {
            m.insert(k, node_json(n));
        }
        Value::Map(Rc::new(m))
    }
    fields_json(spans)
}

pub fn hexdump(data: &[u8], spans: &FieldSpans, fields: &Env) -> String {
    fn format_bytes(d: &[u8], s: &Span) -> String {
        if s.len == 0 {
            return "(empty)".to_string();
        }
        let end = s.offset.saturating_add(s.len);
        let sl = if s.offset < d.len() {
            &d[s.offset..std::cmp::min(end, d.len())]
        } else {
            &[]
        };
        let parts: Vec<String> = sl.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let mut res = parts.join(" ");
        if sl.len() > 8 {
            res.push_str(" ...");
        }
        res
    }

    fn format_val(v: &Value) -> String {
        match v {
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Str(s) => {
                if s.len() > 32 {
                    let cut = &s[..32];
                    let mut q = format!("{:?}", cut);
                    if q.ends_with('"') {
                        q.pop();
                    }
                    q + "...\""
                } else {
                    format!("{:?}", s.as_ref())
                }
            }
            Value::Bytes(b) => format!("<{} bytes>", b.len()),
            _ => "null".to_string(),
        }
    }

    fn collect_leaves(
        fs: &FieldSpans,
        vs: &Env,
        prefix: &str,
        leaves: &mut Vec<(usize, usize, String, String)>,
    ) {
        for (name, node) in fs.iter() {
            let path = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{}.{}", prefix, name)
            };
            let val = vs.get(name).cloned().unwrap_or(Value::Null);
            collect_node(node, &val, &path, leaves);
        }
    }

    fn collect_node(
        node: &SpanNode,
        val: &Value,
        path: &str,
        leaves: &mut Vec<(usize, usize, String, String)>,
    ) {
        match node {
            SpanNode::Leaf(s) => {
                let vs = format_val(val);
                leaves.push((s.offset, s.len, path.to_string(), vs));
            }
            SpanNode::Record { fields, .. } => {
                if let Value::Map(m) = val {
                    collect_leaves(fields, m, path, leaves);
                }
            }
            SpanNode::List { items, .. } => {
                if let Value::List(lst) = val {
                    for (i, subn) in items.iter().enumerate() {
                        let ip = format!("{}[{}]", path, i);
                        let iv = lst.get(i).cloned().unwrap_or(Value::Null);
                        collect_node(subn, &iv, &ip, leaves);
                    }
                }
            }
        }
    }

    let mut leaves: Vec<(usize, usize, String, String)> = vec![];
    collect_leaves(spans, fields, "", &mut leaves);
    leaves.sort_by_key(|t| t.0);

    let mut lines: Vec<String> = vec![];
    let mut covered: usize = 0;
    for (off, len, pth, vstr) in leaves {
        let sp = Span { offset: off, len };
        let bstr = format_bytes(data, &sp);
        lines.push(format!("{:04x}    {:<22} {} = {}", off, bstr, pth, vstr));
        if off + len > covered {
            covered = off + len;
        }
    }
    if covered < data.len() {
        let sp = Span {
            offset: covered,
            len: data.len() - covered,
        };
        let bstr = format_bytes(data, &sp);
        lines.push(format!("{:04x}    {:<22} unread", covered, bstr));
    }
    lines.join("\n")
}

// Recorder used by decode path in engine.
pub(crate) struct Recorder {
    stack: Vec<Level>,
}

enum Level {
    Record {
        fields: OrderedMap<SpanNode>,
        start: usize,
    },
    List {
        items: Vec<SpanNode>,
        start: usize,
    },
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Recorder {
            stack: vec![Level::Record {
                fields: OrderedMap::new(),
                start: 0,
            }],
        }
    }

    pub(crate) fn push_record(&mut self, start: usize) {
        self.stack.push(Level::Record {
            fields: OrderedMap::new(),
            start,
        });
    }

    pub(crate) fn pop_record(&mut self) -> (OrderedMap<SpanNode>, usize) {
        match self.stack.pop() {
            Some(Level::Record { fields, start }) => (fields, start),
            _ => (OrderedMap::new(), 0),
        }
    }

    pub(crate) fn push_list(&mut self, start: usize) {
        self.stack.push(Level::List {
            items: Vec::new(),
            start,
        });
    }

    pub(crate) fn pop_list(&mut self) -> (Vec<SpanNode>, usize) {
        match self.stack.pop() {
            Some(Level::List { items, start }) => (items, start),
            _ => (Vec::new(), 0),
        }
    }

    pub(crate) fn record_named(&mut self, name: &str, node: SpanNode) {
        if let Some(top) = self.stack.last_mut() {
            match top {
                Level::Record { fields, .. } => {
                    fields.insert(name, node);
                }
                Level::List { items, .. } => {
                    items.push(node);
                }
            }
        }
    }

    pub(crate) fn take_fields(&mut self) -> FieldSpans {
        if let Some(Level::Record { fields, .. }) = self.stack.first_mut() {
            std::mem::replace(fields, OrderedMap::new())
        } else {
            OrderedMap::new()
        }
    }
}
