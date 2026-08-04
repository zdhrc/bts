//! The typed domain model produced by compiling a shape: traces, spans, and their field values.

#[derive(Debug, Clone)]
pub(crate) struct Model {
    pub(crate) traces: Vec<Trace>,
}

#[derive(Debug, Clone)]
pub(crate) struct Trace {
    pub(crate) name: String,
    pub(crate) fields: SpanFields,
    pub(crate) children: Vec<Span>,
}

#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub(crate) name: String,
    pub(crate) kind: SpanKind,
    pub(crate) fields: SpanFields,
    pub(crate) children: Vec<Span>,
}

#[derive(Debug, Clone)]
pub(crate) enum SpanKind {
    Task,
    Llm,
}

#[derive(Debug, Clone)]
pub(crate) struct SpanFields {
    pub(crate) input: Option<Value>,
    pub(crate) output: Option<Value>,
    pub(crate) metadata: Option<Object>,
    pub(crate) metrics: Option<Object>,
    pub(crate) tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum Value {
    Str(String),
    Num(Number),
    Bool(bool),
    Array(Array),
    Object(Object),
}

#[derive(Debug, Clone)]
pub(crate) enum Number {
    Int(i64),
    Float(f64),
}

#[derive(Debug, Clone)]
pub(crate) struct Array {
    pub(crate) elem: Vec<Value>,
}

#[derive(Debug, Clone)]
pub(crate) struct Object {
    pub(crate) elem: Vec<ObjectField>,
}

#[derive(Debug, Clone)]
pub(crate) struct ObjectField {
    pub(crate) key: String,
    pub(crate) value: Value,
}
