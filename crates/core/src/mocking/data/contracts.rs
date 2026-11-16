use super::{Attachment, EventBatch};
use serde_json::Value as JsonValue;
use std::error::Error;

/// Supplies the attachment representation used by value operations.
pub trait AttachmentFormat {
    /// Projects attachment metadata without reading files or making requests.
    /// Any JSON value can be projected for serialization and token counting.
    /// Where source permits runtime selection, objects support string keys and
    /// arrays support integer indexes and slices. Incompatible selections return
    /// an evaluation error. Scalar projections cannot be indexed, sliced, or spread.
    /// Returns `None` when this format does not support attachments.
    fn attachment(&self, _attachment: &Attachment) -> Option<JsonValue> {
        None
    }
}

impl AttachmentFormat for () {}

impl<T: AttachmentFormat + ?Sized> AttachmentFormat for &T {
    fn attachment(&self, attachment: &Attachment) -> Option<JsonValue> {
        (**self).attachment(attachment)
    }
}

/// Converts generated events into a destination's representation without sending them.
pub trait Format: AttachmentFormat {
    type Output;
    type Error: Error + 'static;

    fn encode(&self, events: &EventBatch) -> Result<Self::Output, Self::Error>;
}

/// Delivers an encoded batch and reports the acknowledged result.
pub trait Sink {
    type Input;
    type Receipt;
    type Error: Error + 'static;

    fn send(&self, input: &Self::Input) -> Result<Self::Receipt, Self::Error>;
}

use crate::dsl::{Automation, Dataset, Experiment, Facet, Object, Scorer, SrcRange, Template, Value};

/// A provider-lowered execution model with resolved references.
///
/// Consumers evaluate remaining expressions using their own runtime context.
/// Public fields permit inspection and transformation, but edits can invalidate
/// reference indexes and type guarantees. A default model is empty.
#[derive(Debug, Clone, Default)]
pub struct Model {
    /// Whether consumers must preserve the lowered progression order.
    pub ordered: bool,
    /// Root trace definitions in declaration order.
    pub traces: Vec<Trace>,
    /// Scoped expressions to evaluate for each instance.
    pub bindings: Vec<Binding>,
    /// Resolved references indexed by [`RefId`].
    pub refs: Vec<ResolvedRef>,
    /// Callable agent declarations and their root trace entrypoints.
    pub agents: Vec<Agent>,
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
    /// Selects the final iteration, or reads null when none ran.
    Last,
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
    /// Reads the payload record of a completed occurrence.
    Record,
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
    /// Reads a scoped expression declared by the selected block.
    Binding(String),
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
    /// The name the expression is declared and read under. Seeded draws depend on it.
    pub name: String,
    /// A readable name for consumers that print the expression. It has no effect on evaluation.
    pub label: Option<String>,
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
    /// The identifier the step was declared under, which tells apart steps that share a name.
    pub label: Option<String>,
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
    /// When set, the count is unused and the children run as a loop.
    pub again: Option<Again>,
    /// Expressions evaluated separately for each iteration. The count is evaluated in the parent scope.
    pub bindings: Vec<Binding>,
    /// Nested definitions in source order.
    pub children: Vec<Child>,
}

/// Turns a repeat into a loop that runs once and then again while a condition holds.
#[derive(Debug, Clone)]
pub struct Again {
    /// Evaluated in the scope of the iteration that just ran.
    pub condition: Value,
    /// The most times the loop may run again, evaluated in the parent scope.
    /// Running again past the limit is an error. Without a limit the bound is 10000.
    pub limit: Option<Value>,
    /// The source location of the element that loops back.
    pub range: SrcRange,
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

/// A named callable agent backed by a whole trace definition.
#[derive(Debug, Clone)]
pub struct Agent {
    /// The name written in source.
    pub name: String,
    /// A resolved reference to the whole trace this agent executes.
    pub entrypoint: RefId,
    /// The value returned by the agent independently of emitted span outputs.
    pub output: Value,
}

/// A runtime index or count made available by the enclosing trace or repeat.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CtxRef {
    /// The zero-based trace index.
    TraceIndex,
    /// The zero-based index of the innermost repeat.
    RepeatIndex,
    /// The index within a specific enclosing repetition group.
    RepeatIndexAt(NodeId),
    /// The count of the innermost repeat.
    RepeatCount,
}
