use crate::dsl::ast::{BinOp, UnaryOp};
use crate::dsl::diag::SrcRange;

#[derive(Debug, Clone)]
pub(crate) struct Model {
    pub(crate) traces: Vec<Trace>,
}

#[derive(Debug, Clone)]
pub(crate) struct Trace {
    pub(crate) name: String,
    pub(crate) fields: SpanFields,
    pub(crate) children: Vec<Child>,
}

// a span or a dynamic block whose expansion the planner decides per trace;
// spans are the common child so boxing them away buys nothing
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(crate) enum Child {
    Span(Span),
    Repeat(Repeat),
    Choice(Choice),
    Maybe(Maybe),
}

#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub(crate) name: String,
    pub(crate) kind: SpanKind,
    pub(crate) fields: SpanFields,
    pub(crate) children: Vec<Child>,
}

// names on dynamic blocks are inert for now, kept for future traversal
#[derive(Debug, Clone)]
pub(crate) struct Repeat {
    #[allow(dead_code)]
    pub(crate) name: Option<String>,
    pub(crate) count: Value,
    pub(crate) count_range: SrcRange,
    pub(crate) children: Vec<Child>,
}

#[derive(Debug, Clone)]
pub(crate) struct Choice {
    #[allow(dead_code)]
    pub(crate) name: Option<String>,
    pub(crate) children: Vec<Child>,
}

#[derive(Debug, Clone)]
pub(crate) struct Maybe {
    #[allow(dead_code)]
    pub(crate) name: Option<String>,
    pub(crate) chance: Value,
    pub(crate) chance_range: SrcRange,
    pub(crate) children: Vec<Child>,
}

#[derive(Debug, Clone)]
pub(crate) enum SpanKind {
    Task,
    Llm,
    Tool,
    Function,
}

#[derive(Debug, Clone)]
pub(crate) struct SpanFields {
    pub(crate) input: Option<Value>,
    pub(crate) output: Option<Value>,
    pub(crate) expected: Option<Value>,
    pub(crate) error: Option<Value>,
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
    Func(Func),
    // dynamic operator exprs, constant ones fold away in the modeler
    Unary {
        op: UnaryOp,
        operand: Box<Value>,
        range: SrcRange,
    },
    Binary {
        op: BinOp,
        lhs: Box<Value>,
        rhs: Box<Value>,
        range: SrcRange,
    },
    // no range, a conditional itself can't fail, only its operands can
    Cond {
        cond: Box<Value>,
        then: Box<Value>,
        otherwise: Box<Value>,
    },
    // dynamic index selections, constant ones fold away in the modeler
    Index {
        target: Box<Value>,
        index: Box<Value>,
        range: SrcRange,
    },
    // dynamic slice selections, constant ones fold away in the modeler
    Slice {
        target: Box<Value>,
        start: Option<Box<Value>>,
        end: Option<Box<Value>>,
        range: SrcRange,
    },
}

// already validated by the modeler so evaluating one can't fail
#[derive(Debug, Clone)]
pub(crate) enum Func {
    Choice(Vec<Value>),
    Range(Range),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Range {
    Int { min: i64, max: i64 },
    Float { min: f64, max: f64 },
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
    RepeatIndex,
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
