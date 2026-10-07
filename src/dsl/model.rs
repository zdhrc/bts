use crate::dsl::diag::SrcRange;

#[derive(Debug, Clone)]
pub(crate) struct Model {
    pub(crate) traces: Vec<Trace>,
    // root-scope bindings, evaluated once per generated trace before its own
    pub(crate) bindings: Vec<Binding>,
    // resolved block references, indexed by RefId; Value::BlockRef stays an
    // opaque handle so the modeler can resolve after its single walk completes
    pub(crate) refs: Vec<ResolvedRef>,
    // root scorer blocks in declaration order, consumed by scg rather than sdg
    pub(crate) scorers: Vec<Scorer>,
    pub(crate) automations: Vec<Automation>,
}

// a block's identity, assigned in walk order; stable across a compile so both
// reference resolution and generation address the same node
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(crate) struct NodeId(pub(crate) u32);

// a handle into Model.refs
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct RefId(pub(crate) u32);

// a validated block reference: walk `up` scopes from the referencing instance
// to the anchor, descend the steps, read the accessor, then drill into the json
#[derive(Debug, Clone)]
pub(crate) struct ResolvedRef {
    pub(crate) up: usize,
    pub(crate) steps: Vec<Step>,
    pub(crate) accessor: Accessor,
    // selections on the field's json value, evaluated at generation
    pub(crate) path: Vec<Selection>,
    pub(crate) range: SrcRange,
}

#[derive(Debug, Clone)]
pub(crate) enum Selection {
    Index(Value),
    Slice { start: Option<Value>, end: Option<Value> },
}

#[derive(Debug, Clone)]
pub(crate) enum Step {
    // same-kind-and-name candidates in sibling order; a position picks among
    // them at generation, and a lone candidate needs none
    Child {
        candidates: Vec<NodeId>,
        position: Option<Value>,
    },
    // one iteration of a repeat collection
    Iteration(Value),
    // an iteration slice; the rest of the reference projects over each
    // selected iteration and the whole reference evaluates to an array
    Iterations {
        start: Option<Value>,
        end: Option<Value>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum Accessor {
    Field(Field),
    // the current iteration of an enclosing named repeat
    Index,
    Count,
    // the 0-based pick of a choice
    Chosen,
    // whether a maybe fired
    Included,
}

// the referenceable surface of a span or trace
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub(crate) enum Field {
    Input,
    Output,
    Expected,
    Error,
    Metadata,
    Metrics,
    Tags,
}

// a dynamic scoped variable, evaluated once per instantiation of its declaring
// block; constant vars substitute in the modeler and never get here
#[derive(Debug, Clone)]
pub(crate) struct Binding {
    pub(crate) name: String,
    pub(crate) value: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct Trace {
    pub(crate) node: NodeId,
    pub(crate) name: String,
    pub(crate) fields: SpanFields,
    pub(crate) bindings: Vec<Binding>,
    pub(crate) children: Vec<Child>,
}

// a span or a dynamic block whose expansion the planner decides per trace;
// spans are the common child so boxing them away buys nothing
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub(crate) enum Child {
    Span(Span),
    Scorer(ScorerSpan),
    Repeat(Repeat),
    Choice(Choice),
    Maybe(Maybe),
}

#[derive(Debug, Clone)]
pub(crate) struct ScorerSpan {
    pub(crate) node: NodeId,
    pub(crate) name: String,
    pub(crate) score: Value,
    pub(crate) score_range: SrcRange,
    pub(crate) reason: Option<(Value, SrcRange)>,
    pub(crate) bindings: Vec<Binding>,
}

#[derive(Debug, Clone)]
pub(crate) struct Span {
    pub(crate) node: NodeId,
    pub(crate) name: String,
    pub(crate) kind: SpanKind,
    pub(crate) fields: SpanFields,
    pub(crate) bindings: Vec<Binding>,
    pub(crate) children: Vec<Child>,
}

#[derive(Debug, Clone)]
pub(crate) struct Repeat {
    pub(crate) node: NodeId,
    #[allow(dead_code)]
    pub(crate) name: Option<String>,
    pub(crate) count: Value,
    pub(crate) count_range: SrcRange,
    // evaluated per iteration, count is drawn in the parent scope
    pub(crate) bindings: Vec<Binding>,
    pub(crate) children: Vec<Child>,
}

#[derive(Debug, Clone)]
pub(crate) struct Choice {
    pub(crate) node: NodeId,
    #[allow(dead_code)]
    pub(crate) name: Option<String>,
    pub(crate) bindings: Vec<Binding>,
    pub(crate) children: Vec<Child>,
}

#[derive(Debug, Clone)]
pub(crate) struct Maybe {
    pub(crate) node: NodeId,
    #[allow(dead_code)]
    pub(crate) name: Option<String>,
    pub(crate) chance: Value,
    pub(crate) chance_range: SrcRange,
    // evaluated only when the children are included, chance is drawn in the parent scope
    pub(crate) bindings: Vec<Binding>,
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
    // total seconds, a generation directive rather than a payload field; the
    // range points diagnostics at the expression when a dynamic value fails
    pub(crate) duration: Option<(Value, SrcRange)>,
}

// a component pushed to braintrust rather than generated as spans; scorer
// expressions are the transpilable subset, validated by the modeler so
// emission never fails on a compiled model
#[derive(Debug, Clone)]
pub(crate) struct Scorer {
    pub(crate) name: String,
    // none = unset in source; the push side picks the default
    pub(crate) lang: Option<ScorerLang>,
    // output file stem this scorer packs into when built; none = its own slug
    pub(crate) file: Option<String>,
    pub(crate) kind: ScorerKind,
}

#[derive(Debug, Clone)]
pub(crate) struct Automation {
    pub(crate) name: String,
    pub(crate) scorers: Vec<String>,
    pub(crate) root: bool,
    pub(crate) span_names: Vec<String>,
    pub(crate) sampling_rate: f64,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScorerLang {
    Python,
    Typescript,
}

#[derive(Debug, Clone)]
pub(crate) enum ScorerKind {
    Code {
        // the fallback when no `when` case matches
        score: Value,
        steps: Vec<ScorerStep>,
    },
    Judge {
        model: String,
        // a string or template; holes only interpolate scorer args
        prompt: Value,
        options: Vec<JudgeOption>,
    },
}

// nested checks run before this block's score
#[derive(Debug, Clone)]
pub(crate) struct When {
    pub(crate) cond: Value,
    pub(crate) score: Option<Value>,
    pub(crate) steps: Vec<ScorerStep>,
}

#[derive(Debug, Clone)]
pub(crate) enum ScorerStep {
    When(When),
    Repeat { count: Value, steps: Vec<ScorerStep> },
}

#[derive(Debug, Clone)]
pub(crate) struct JudgeOption {
    pub(crate) label: String,
    pub(crate) score: f64,
}

// the runtime arguments braintrust passes a scorer; read by the emitted
// scorer code, never evaluated during generation
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ScorerArg {
    Input,
    Output,
    Expected,
    Metadata,
}

// a validated context reference, usable as a value or a template part
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum CtxRef {
    TraceIndex,
    RepeatIndex,
    RepeatCount,
}

// the model owns its operator vocab; the modeler maps from the ast enums
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
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
    Attachment {
        path: String,
        filename: String,
        content_type: String,
        key: String,
    },
    // a reference to a scoped binding, looked up in the environment during
    // generation; the modeler guarantees the name is bound
    VarRef(String),
    // a context index resolved during generation, validated by the modeler
    CtxRef(CtxRef),
    // a scorer runtime argument, resolved by the emitted scorer code rather
    // than during generation; only appears inside scorer expressions
    ArgRef(ScorerArg),
    // a block reference resolved through Model.refs during generation
    BlockRef {
        ref_id: RefId,
        range: SrcRange,
    },
    // range points at the call site so generation failures have a location
    Func {
        func: Func,
        range: SrcRange,
    },
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

// structure and argument types are validated by the modeler; evaluation can
// still fail on values only known during generation (bounds, overflow, elements)
#[derive(Debug, Clone)]
pub(crate) enum Func {
    Attachment {
        path: String,
        content_type: String,
    },
    Choice(Vec<Value>),
    Range(Range),
    Weighted(Vec<WeightedOption>),
    Normal {
        mean: f64,
        stddev: f64,
    },
    Lognormal {
        median: f64,
        sigma: f64,
    },
    Exponential {
        mean: f64,
    },
    Pareto {
        min: f64,
        shape: f64,
    },
    Beta {
        alpha: f64,
        beta: f64,
    },
    Poisson {
        mean: f64,
    },
    Chance {
        probability: f64,
    },
    Upper {
        text: Box<Value>,
    },
    Lower {
        text: Box<Value>,
    },
    Trim {
        text: Box<Value>,
    },
    Replace {
        text: Box<Value>,
        from: Box<Value>,
        to: Box<Value>,
    },
    Split {
        text: Box<Value>,
        separator: Box<Value>,
    },
    Join {
        array: Box<Value>,
        separator: Box<Value>,
    },
    Contains {
        target: Box<Value>,
        needle: Box<Value>,
    },
    StartsWith {
        text: Box<Value>,
        prefix: Box<Value>,
    },
    EndsWith {
        text: Box<Value>,
        suffix: Box<Value>,
    },
    Len {
        target: Box<Value>,
    },
    Tokens {
        value: Box<Value>,
    },
    Format {
        // the template split on `{}`, interleaved with args at generation:
        // pieces[0] args[0] pieces[1] ... ; always one more piece than args
        pieces: Vec<String>,
        args: Vec<Value>,
    },
    Clamp {
        value: Box<Value>,
        min: Box<Value>,
        max: Box<Value>,
    },
    Round {
        value: Box<Value>,
    },
    Floor {
        value: Box<Value>,
    },
    Ceil {
        value: Box<Value>,
    },
    Abs {
        value: Box<Value>,
    },
    Min(Vec<Value>),
    Max(Vec<Value>),
    Uuid,
    Hex {
        length: usize,
    },
    Alphanum {
        length: usize,
    },
    Noise {
        size: Box<Value>,
    },
}

// backstop so a typo'd noise size can't balloon a run
pub(crate) const NOISE_SIZE_CAP: i64 = 8 << 20;

#[derive(Debug, Clone)]
pub(crate) struct WeightedOption {
    pub(crate) value: Value,
    pub(crate) weight: f64,
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
    // a scoped binding interpolated into the template, scalar by validation
    VarRef(String),
    // a residual selection or block reference interpolated into the template;
    // scalar-ness the modeler couldn't see statically is enforced at generation
    Dynamic(Value),
}

#[derive(Debug, Clone)]
pub(crate) enum Number {
    Int(i64),
    Float(f64),
}

#[derive(Debug, Clone)]
pub(crate) struct Array {
    pub(crate) elem: Vec<ArrayElem>,
}

// spreads with a statically known shape splice during validation; one whose
// shape only exists at generation splices when the array evaluates
#[derive(Debug, Clone)]
pub(crate) enum ArrayElem {
    Item(Value),
    Spread(Value),
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
