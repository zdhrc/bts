use crate::diag::SrcRange;

/// A compiled module with resolved references and checked language rules.
///
/// Consumers evaluate remaining expressions using their own runtime context.
/// Public fields permit inspection and transformation, but edits can invalidate
/// reference indexes and type guarantees. A default model is empty.
#[derive(Debug, Clone, Default)]
pub struct Model {
    /// Root trace definitions in declaration order.
    pub traces: Vec<Trace>,
    /// Scoped expressions to evaluate for each instance.
    pub bindings: Vec<Binding>,
    /// Resolved references indexed by [`RefId`].
    pub refs: Vec<ResolvedRef>,
    /// Reusable scorer definitions in declaration order.
    pub scorers: Vec<Scorer>,
    /// Automation definitions in declaration order.
    pub automations: Vec<Automation>,
    /// Facet definitions in declaration order.
    pub facets: Vec<Facet>,
    /// Dataset definitions in declaration order.
    pub datasets: Vec<Dataset>,
    /// Experiment definitions in declaration order.
    pub experiments: Vec<Experiment>,
}
/// Identifies a block within one compiled model.
///
/// The identifier is not stable across recompilation or edits to source.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct NodeId(
    /// The model-local identity assigned during compilation.
    pub u32,
);
/// An index into [`Model::refs`], valid only for the model that owns it.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RefId(
    /// The index into the owning model’s reference table.
    pub u32,
);
/// A checked route from a reference expression to a block or value.
///
/// Consumers first choose the module anchor or walk `up` enclosing scopes,
/// then follow `steps`, read `accessor`, and apply `path` to the resulting value.
/// Dynamic positions and indexes still require runtime evaluation.
#[derive(Debug, Clone)]
pub struct ResolvedRef {
    /// An absolute trace anchor, or `None` for a reference relative to its use site.
    pub module: Option<NodeId>,
    /// How many enclosing scopes to walk before following the path.
    pub up: usize,
    /// Ordered steps to follow or evaluate.
    pub steps: Vec<Step>,
    /// What to read after following the block path.
    pub accessor: Accessor,
    /// Selections applied to the referenced field value.
    pub path: Vec<Selection>,
    /// The complete reference expression’s source location.
    pub range: SrcRange,
}

/// An index or slice applied to a referenced field value.
#[derive(Debug, Clone)]
pub enum Selection {
    /// An element selection.
    Index(Value),
    /// A selection between optional bounds.
    Slice {
        /// The inclusive lower bound, or `None` to start at the beginning.
        start: Option<Value>,
        /// The exclusive upper bound, or `None` to continue to the end.
        end: Option<Value>,
    },
}

/// One step through block instances on a resolved reference path.
#[derive(Debug, Clone)]
pub enum Step {
    /// Selects a matching child block, optionally by runtime position.
    Child {
        /// Matching block identities in sibling order.
        candidates: Vec<NodeId>,
        /// An optional runtime expression selecting among matching siblings.
        position: Option<Value>,
    },
    /// Selects one repeat iteration using an evaluated index.
    Iteration(Value),
    /// Selects a repeat slice and follows the remaining path for every selected iteration.
    Iterations {
        /// The inclusive lower bound, or `None` to start at the beginning.
        start: Option<Value>,
        /// The exclusive upper bound, or `None` to continue to the end.
        end: Option<Value>,
    },
}

/// The block identity, field, or execution detail read at a reference target.
#[derive(Debug, Clone)]
pub enum Accessor {
    /// Returns the target block identity and kind without reading a payload field.
    Block {
        /// This block’s identity within the compiled model.
        node: NodeId,
        /// The kind of operation represented by this definition.
        kind: &'static str,
    },
    /// Reads a payload field from the selected block.
    Field(Field),
    /// Reads the current iteration index of an enclosing repeat.
    Index,
    /// Reads the enclosing repeat count.
    Count,
    /// Reads the zero-based index of the selected choice alternative.
    Chosen,
    /// Reads whether a maybe block was included.
    Included,
}
/// A payload field that source expressions can reference.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum Field {
    /// The input field.
    Input,
    /// The output field.
    Output,
    /// The expected result field.
    Expected,
    /// The authored error field.
    Error,
    /// Additional named data.
    Metadata,
    /// Numeric measurements.
    Metrics,
    /// Authored labels.
    Tags,
}
/// A scoped expression evaluated once per instance of its declaring block.
///
/// Constant variables are substituted during compilation and do not appear here.
#[derive(Debug, Clone)]
pub struct Binding {
    /// The name written in source.
    pub name: String,
    /// The value expression to inspect or evaluate.
    pub value: Value,
}

/// A named root definition and the behavior nested beneath it.
#[derive(Debug, Clone)]
pub struct Trace {
    /// This block’s identity within the compiled model.
    pub node: NodeId,
    /// The name written in source.
    pub name: String,
    /// Payload fields associated with this block.
    pub fields: SpanFields,
    /// Scoped expressions to evaluate for each instance.
    pub bindings: Vec<Binding>,
    /// Nested definitions in source order.
    pub children: Vec<Child>,
}
/// A nested span or a block whose children a consumer expands at runtime.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Child {
    /// An ordinary nested span.
    Span(Span),
    /// A scorer definition or authored score.
    Scorer(ScorerSpan),
    /// A repeated sequence.
    Repeat(Repeat),
    /// One selected alternative.
    Choice(Choice),
    /// An optional sequence.
    Maybe(Maybe),
}

/// An authored score attached to a generated trace or span.
#[derive(Debug, Clone)]
pub struct ScorerSpan {
    /// This block’s identity within the compiled model.
    pub node: NodeId,
    /// The name written in source.
    pub name: String,
    /// The score expression to evaluate.
    pub score: Value,
    /// The score expression’s source location.
    pub score_range: SrcRange,
    /// An optional explanation expression and its source location.
    pub reason: Option<(Value, SrcRange)>,
    /// Scoped expressions to evaluate for each instance.
    pub bindings: Vec<Binding>,
}

/// A named task, model call, tool call, or function within a trace.
#[derive(Debug, Clone)]
pub struct Span {
    /// This block’s identity within the compiled model.
    pub node: NodeId,
    /// The name written in source.
    pub name: String,
    /// The kind of operation represented by this definition.
    pub kind: SpanKind,
    /// Payload fields associated with this block.
    pub fields: SpanFields,
    /// Scoped expressions to evaluate for each instance.
    pub bindings: Vec<Binding>,
    /// Nested definitions in source order.
    pub children: Vec<Child>,
}

/// Children repeated a runtime-selected number of times.
#[derive(Debug, Clone)]
pub struct Repeat {
    /// This block’s identity within the compiled model.
    pub node: NodeId,
    #[allow(dead_code)]
    /// The name written in source.
    pub name: Option<String>,
    /// The number of iterations to evaluate.
    pub count: Value,
    /// The count expression’s source location.
    pub count_range: SrcRange,
    /// Expressions evaluated separately for each iteration. The count is evaluated in the parent scope.
    pub bindings: Vec<Binding>,
    /// Nested definitions in source order.
    pub children: Vec<Child>,
}

/// Alternative children from which a consumer selects one per instance.
#[derive(Debug, Clone)]
pub struct Choice {
    /// This block’s identity within the compiled model.
    pub node: NodeId,
    #[allow(dead_code)]
    /// The name written in source.
    pub name: Option<String>,
    /// Scoped expressions to evaluate for each instance.
    pub bindings: Vec<Binding>,
    /// Nested definitions in source order.
    pub children: Vec<Child>,
}

/// Children included according to a probability expression.
#[derive(Debug, Clone)]
pub struct Maybe {
    /// This block’s identity within the compiled model.
    pub node: NodeId,
    #[allow(dead_code)]
    /// The name written in source.
    pub name: Option<String>,
    /// The probability expression controlling inclusion.
    pub chance: Value,
    /// The probability expression’s source location.
    pub chance_range: SrcRange,
    /// Expressions evaluated only when the children are included.
    pub bindings: Vec<Binding>,
    /// Nested definitions in source order.
    pub children: Vec<Child>,
}

/// The role assigned to an ordinary span.
#[derive(Debug, Clone)]
pub enum SpanKind {
    /// A task span.
    Task,
    /// A language model call.
    Llm,
    /// A tool call.
    Tool,
    /// A function span.
    Function,
}

/// Optional payload expressions and generation directives for a trace or span.
///
/// An absent field differs from a field explicitly set to [`Value::Null`].
#[derive(Debug, Clone, Default)]
pub struct SpanFields {
    /// The authored input expression.
    pub input: Option<Value>,
    /// The authored output expression.
    pub output: Option<Value>,
    /// The expected result expression, if provided.
    pub expected: Option<Value>,
    /// An authored error value, if provided.
    pub error: Option<Value>,
    /// Additional named values attached to the definition.
    pub metadata: Option<Object>,
    /// Named numeric measurements.
    pub metrics: Option<Object>,
    /// Labels attached to the definition.
    pub tags: Vec<Template>,
    /// A duration expression in seconds and its source location. This is a generation directive.
    pub duration: Option<(Value, SrcRange)>,
}
/// A reusable scorer definition for a code generation or deployment consumer.
#[derive(Debug, Clone)]
pub struct Scorer {
    /// The name written in source.
    pub name: String,
    /// The requested output language, or `None` to let the consumer choose.
    pub lang: Option<ScorerLang>,
    /// An optional output file stem for grouping generated scorers.
    pub file: Option<String>,
    /// The kind of operation represented by this definition.
    pub kind: ScorerKind,
}

/// A named topic extraction definition.
#[derive(Debug, Clone)]
pub struct Facet {
    /// The name written in source.
    pub name: String,
    /// The instructions used by the model-based operation.
    pub prompt: String,
    /// Optional descriptive text supplied in source.
    pub description: Option<String>,
    /// An optional pattern identifying responses that contain no matching topic.
    pub no_match_pattern: Option<String>,
}

/// A named rule describing when a scorer or topic extraction should run.
#[derive(Debug, Clone)]
pub struct Automation {
    /// The name written in source.
    pub name: String,
    /// The kind of operation represented by this definition.
    pub kind: AutomationKind,
    /// The fraction of eligible events selected for the automation.
    pub sampling_rate: f64,
    /// Whether the automation is enabled.
    pub enabled: bool,
}

/// The operation requested by an automation definition.
#[derive(Debug, Clone)]
pub enum AutomationKind {
    /// A scorer definition or authored score.
    Scorer {
        /// Names of the scorers bound to this automation.
        scorers: Vec<String>,
        /// Whether the rule targets root spans.
        root: bool,
        /// Names of spans targeted by the rule.
        span_names: Vec<String>,
    },
    /// Topic extraction using named facets.
    Topics {
        /// Names of the facets used by this automation.
        facets: Vec<String>,
    },
}

/// A named collection of cases and their source definitions.
#[derive(Debug, Clone)]
pub struct Dataset {
    /// The name written in source.
    pub name: String,
    /// Optional descriptive text supplied in source.
    pub description: Option<String>,
    /// Dataset cases in declaration order.
    pub cases: Vec<DatasetCase>,
}

/// A dataset evaluation definition linking a task, scorers, and optional baseline.
#[derive(Debug, Clone)]
pub struct Experiment {
    /// The name written in source.
    pub name: String,
    /// Optional descriptive text supplied in source.
    pub description: Option<String>,
    /// The name of the referenced dataset.
    pub dataset: String,
    /// A reference to the trace or static span used as the experiment task.
    pub task: RefId,
    /// Names of the scorers to invoke for each result.
    pub scorers: Vec<String>,
    /// The name of a baseline experiment, if provided.
    pub baseline: Option<String>,
}

/// One named input case, optionally derived from modeled traces or spans.
#[derive(Debug, Clone)]
pub struct DatasetCase {
    /// The name written in source.
    pub name: String,
    /// The source used to create the case.
    pub source: DatasetSource,
    /// The expected result expression, if provided.
    pub expected: Option<Value>,
    /// Additional named values attached to the definition.
    pub metadata: Option<Value>,
    /// Labels attached to the definition.
    pub tags: Option<Value>,
}

/// The authored source from which a consumer creates a dataset case.
#[derive(Debug, Clone)]
pub enum DatasetSource {
    /// Input supplied directly by the case.
    Inline(Value),
    /// A reference to an entire trace.
    Trace(RefId),
    /// A reference to one static span subtree.
    Span(RefId),
    /// An ordered group of referenced traces.
    Group(Vec<RefId>),
}

/// The requested programming language for generated scorer code.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ScorerLang {
    /// Python source output.
    Python,
    /// TypeScript source output.
    Typescript,
}

/// Either executable score expressions or a model-based judge definition.
#[derive(Debug, Clone)]
pub enum ScorerKind {
    /// Score expressions and ordered conditional steps.
    Code {
        /// The fallback score used when no earlier step returns a score.
        score: Value,
        /// Ordered steps to follow or evaluate.
        steps: Vec<ScorerStep>,
    },
    /// A model-based judge with labeled score options.
    Judge {
        /// The requested judge model name.
        model: String,
        /// Judge instructions with optional scorer-argument interpolation.
        prompt: Value,
        /// Allowed judge responses and their scores.
        options: Vec<JudgeOption>,
    },
}
/// A conditional scorer branch. Nested steps run before its own score.
#[derive(Debug, Clone)]
pub struct When {
    /// The condition controlling this expression or branch.
    pub cond: Value,
    /// The score expression to evaluate.
    pub score: Option<Value>,
    /// Ordered steps to follow or evaluate.
    pub steps: Vec<ScorerStep>,
}

/// A conditional or repeated step evaluated before a scorer fallback.
#[derive(Debug, Clone)]
pub enum ScorerStep {
    /// A conditional scorer branch.
    When(When),
    /// A repeated sequence.
    Repeat {
        /// The number of iterations to evaluate.
        count: Value,
        /// Ordered steps to follow or evaluate.
        steps: Vec<ScorerStep>,
    },
}

/// A named judge response and its numeric score.
#[derive(Debug, Clone)]
pub struct JudgeOption {
    /// The response label returned by the judge.
    pub label: String,
    /// The score expression to evaluate.
    pub score: f64,
}
/// An input supplied when a consumer invokes a scorer.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ScorerArg {
    /// The input field.
    Input,
    /// The output field.
    Output,
    /// The expected result field.
    Expected,
    /// Additional named data.
    Metadata,
}
/// A runtime index or count made available by the enclosing trace or repeat.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CtxRef {
    /// The zero-based trace index.
    TraceIndex,
    /// The zero-based index of the innermost repeat.
    RepeatIndex,
    /// The count of the innermost repeat.
    RepeatCount,
}
/// An operator applied to one expression.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UnaryOp {
    /// Numeric negation.
    Neg,
    /// Boolean negation.
    Not,
}

/// An operator applied to two expressions.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BinOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Division.
    Div,
    /// Remainder.
    Rem,
    /// Equality.
    Eq,
    /// Inequality.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal to.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal to.
    Ge,
    /// Boolean conjunction with short-circuit evaluation.
    And,
    /// Boolean disjunction with short-circuit evaluation.
    Or,
}

/// A compiled value or an expression left for a consumer to evaluate.
///
/// Compilation folds expressions whose values are known. References, random
/// functions, and expressions depending on runtime data remain represented here.
/// This type does not provide an evaluator.
#[derive(Debug, Clone)]
pub enum Value {
    /// A string value.
    Str(String),
    /// Text containing interpolated values.
    Template(Template),
    /// A numeric value.
    Num(Number),
    /// A boolean value.
    Bool(bool),
    /// An explicit null value.
    Null,
    /// An ordered collection.
    Array(Array),
    /// A collection of named entries.
    Object(Object),
    /// An attachment reference to be materialized by a consumer.
    Attachment {
        /// The attachment path written in source.
        path: String,
        /// The name to use when presenting the attachment.
        filename: String,
        /// The attachment media type.
        content_type: String,
        /// The attachment key used to connect payloads with uploads.
        key: String,
    },
    /// A scoped variable resolved by name during evaluation.
    VarRef(String),
    /// A validated runtime context reference.
    CtxRef(CtxRef),
    /// An argument supplied when a scorer is invoked.
    ArgRef(ScorerArg),
    /// A block reference resolved through the owning model.
    BlockRef {
        /// The index of the resolved reference in [`Model::refs`].
        ref_id: RefId,
        /// The expression’s byte range in the original source.
        range: SrcRange,
    },
    /// A function call.
    Func {
        /// The function and its arguments.
        func: Func,
        /// The expression’s byte range in the original source.
        range: SrcRange,
    },
    /// An operation with one operand.
    Unary {
        /// The operator applied to the operands.
        op: UnaryOp,
        /// The expression to which the operator applies.
        operand: Box<Value>,
        /// The expression’s byte range in the original source.
        range: SrcRange,
    },
    /// An operation with two operands.
    Binary {
        /// The operator applied to the operands.
        op: BinOp,
        /// The left operand.
        lhs: Box<Value>,
        /// The right operand.
        rhs: Box<Value>,
        /// The expression’s byte range in the original source.
        range: SrcRange,
    },
    /// A conditional expression that evaluates only the selected branch.
    Cond {
        /// The condition controlling this expression or branch.
        cond: Box<Value>,
        /// The expression evaluated when the condition is true.
        then: Box<Value>,
        /// The expression evaluated when the condition is false.
        otherwise: Box<Value>,
    },
    /// An element selection.
    Index {
        /// The value to index, slice, or inspect.
        target: Box<Value>,
        /// The expression selecting an element.
        index: Box<Value>,
        /// The expression’s byte range in the original source.
        range: SrcRange,
    },
    /// A selection between optional bounds.
    Slice {
        /// The value to index, slice, or inspect.
        target: Box<Value>,
        /// The inclusive lower bound, or `None` to start at the beginning.
        start: Option<Box<Value>>,
        /// The exclusive upper bound, or `None` to continue to the end.
        end: Option<Box<Value>>,
        /// The expression’s byte range in the original source.
        range: SrcRange,
    },
}
/// A validated function call whose result is computed by a consumer.
///
/// Argument structure is checked during compilation. Runtime values can still
/// cause failures such as an invalid index or numeric overflow.
#[derive(Debug, Clone)]
pub enum Func {
    /// An attachment reference to be materialized by a consumer.
    Attachment {
        /// The attachment path written in source.
        path: String,
        /// The attachment media type.
        content_type: String,
    },
    /// One selected alternative.
    Choice(Vec<Value>),
    /// A uniform sample between numeric bounds.
    Range(Range),
    /// A sample selected using relative weights.
    Weighted(Vec<WeightedOption>),
    /// A normal distribution sample.
    Normal {
        /// The distribution mean.
        mean: f64,
        /// The distribution standard deviation.
        stddev: f64,
    },
    /// A lognormal distribution sample.
    Lognormal {
        /// The distribution median.
        median: f64,
        /// The standard deviation in logarithmic space.
        sigma: f64,
    },
    /// An exponential distribution sample.
    Exponential {
        /// The distribution mean.
        mean: f64,
    },
    /// A Pareto distribution sample.
    Pareto {
        /// The lower bound.
        min: f64,
        /// The Pareto shape parameter.
        shape: f64,
    },
    /// A beta distribution sample.
    Beta {
        /// The first beta distribution parameter.
        alpha: f64,
        /// The second beta distribution parameter.
        beta: f64,
    },
    /// A Poisson distribution sample.
    Poisson {
        /// The distribution mean.
        mean: f64,
    },
    /// A boolean sampled with the given probability.
    Chance {
        /// The probability of returning true.
        probability: f64,
    },
    /// Converts text to uppercase.
    Upper {
        /// The text expression to evaluate.
        text: Box<Value>,
    },
    /// Converts text to lowercase.
    Lower {
        /// The text expression to evaluate.
        text: Box<Value>,
    },
    /// Removes surrounding whitespace.
    Trim {
        /// The text expression to evaluate.
        text: Box<Value>,
    },
    /// Replaces matching text.
    Replace {
        /// The text expression to evaluate.
        text: Box<Value>,
        /// The text to replace.
        from: Box<Value>,
        /// The replacement text.
        to: Box<Value>,
    },
    /// Splits text using a separator.
    Split {
        /// The text expression to evaluate.
        text: Box<Value>,
        /// The separator used to split or join text.
        separator: Box<Value>,
    },
    /// Joins array elements using a separator.
    Join {
        /// The array expression to join.
        array: Box<Value>,
        /// The separator used to split or join text.
        separator: Box<Value>,
    },
    /// Tests whether the target contains a value.
    Contains {
        /// The value to index, slice, or inspect.
        target: Box<Value>,
        /// The value or text to search for.
        needle: Box<Value>,
    },
    /// Tests a text prefix.
    StartsWith {
        /// The text expression to evaluate.
        text: Box<Value>,
        /// The prefix to test.
        prefix: Box<Value>,
    },
    /// Tests a text suffix.
    EndsWith {
        /// The text expression to evaluate.
        text: Box<Value>,
        /// The suffix to test.
        suffix: Box<Value>,
    },
    /// Measures the length of a value.
    Len {
        /// The value to index, slice, or inspect.
        target: Box<Value>,
    },
    /// Counts tokens using the consumer’s tokenizer.
    Tokens {
        /// The value expression to inspect or evaluate.
        value: Box<Value>,
    },
    /// Interleaves literal pieces with evaluated arguments.
    Format {
        /// Literal text around each format argument. There is one more piece than argument.
        pieces: Vec<String>,
        /// Function arguments in source order.
        args: Vec<Value>,
    },
    /// Restricts a number to the supplied bounds.
    Clamp {
        /// The value expression to inspect or evaluate.
        value: Box<Value>,
        /// The lower bound.
        min: Box<Value>,
        /// The upper bound.
        max: Box<Value>,
    },
    /// Rounds a number to the nearest integer.
    Round {
        /// The value expression to inspect or evaluate.
        value: Box<Value>,
    },
    /// Rounds a number downward.
    Floor {
        /// The value expression to inspect or evaluate.
        value: Box<Value>,
    },
    /// Rounds a number upward.
    Ceil {
        /// The value expression to inspect or evaluate.
        value: Box<Value>,
    },
    /// Takes the absolute value.
    Abs {
        /// The value expression to inspect or evaluate.
        value: Box<Value>,
    },
    /// Finds the smallest argument.
    Min(Vec<Value>),
    /// Finds the largest argument.
    Max(Vec<Value>),
    /// Produces a UUID string.
    Uuid,
    /// Produces random hexadecimal text.
    Hex {
        /// The requested output length.
        length: usize,
    },
    /// Produces random alphanumeric text.
    Alphanum {
        /// The requested output length.
        length: usize,
    },
    /// Produces a requested amount of generated noise.
    Noise {
        /// The requested noise size.
        size: Box<Value>,
    },
}
/// Maximum permitted noise size, limiting accidental allocations during evaluation.
pub const NOISE_SIZE_CAP: i64 = 8 << 20;

/// One candidate value and its relative sampling weight.
#[derive(Debug, Clone)]
pub struct WeightedOption {
    /// The value expression to inspect or evaluate.
    pub value: Value,
    /// The relative sampling weight. Zero disables this candidate.
    pub weight: f64,
}

/// Inclusive bounds for an integer or floating-point sample.
#[derive(Debug, Clone, Copy)]
pub enum Range {
    /// An integer value or integer bounds.
    Int {
        /// The lower bound.
        min: i64,
        /// The upper bound.
        max: i64,
    },
    /// A floating-point value or floating-point bounds.
    Float {
        /// The lower bound.
        min: f64,
        /// The upper bound.
        max: f64,
    },
}

/// Text interleaved with values to be resolved by a consumer.
#[derive(Debug, Clone)]
pub struct Template {
    /// Template parts in source order.
    pub parts: Vec<Part>,
}

/// One literal or evaluated part of a text template.
#[derive(Debug, Clone)]
pub enum Part {
    /// Literal text copied into the result.
    Lit(String),
    /// A reference expression whose meaning depends on its context.
    Ref(CtxRef),
    /// A scoped variable resolved by name during evaluation.
    VarRef(String),
    /// An expression whose interpolated value is checked at runtime.
    Dynamic(Value),
}

/// An integer or floating-point literal preserved by compilation.
#[derive(Debug, Clone)]
pub enum Number {
    /// An integer value or integer bounds.
    Int(i64),
    /// A floating-point value or floating-point bounds.
    Float(f64),
}

/// An ordered collection of values and runtime spreads.
#[derive(Debug, Clone)]
pub struct Array {
    /// Entries in source order.
    pub elem: Vec<ArrayElem>,
}
/// A single array value or a collection expanded into the array.
#[derive(Debug, Clone)]
pub enum ArrayElem {
    /// One value in an array.
    Item(Value),
    /// A collection expanded into its surrounding container.
    Spread(Value),
}

/// An ordered collection of named value expressions.
#[derive(Debug, Clone)]
pub struct Object {
    /// Entries in source order.
    pub elem: Vec<ObjectField>,
}

/// A key and its compiled value expression.
#[derive(Debug, Clone)]
pub struct ObjectField {
    /// The key identifying this entry.
    pub key: String,
    /// The value expression to inspect or evaluate.
    pub value: Value,
}
