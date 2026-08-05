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
    pub(crate) tags: Vec<Template>,
}

#[derive(Debug, Clone)]
pub(crate) enum Value {
    Str(String),
    Template(Template),
    Num(Number),
    Bool(bool),
    Null,
    Array(Array),
    Object(Object),
}

#[derive(Debug, Clone)]
pub(crate) struct Template {
    pub(crate) parts: Vec<Part>,
}

#[derive(Debug, Clone)]
pub(crate) enum Part {
    Lit(String),
    Ref(CtxRef),
}

// already validated by the modeler so resolving one can't fail
#[derive(Debug, Clone, Copy)]
pub(crate) enum CtxRef {
    TraceIndex,
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
