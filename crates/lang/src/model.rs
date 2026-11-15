use crate::diag::SrcRange;
use std::collections::BTreeMap;

/// A compiled graph with stable model-local identities and resolved references.
#[derive(Debug, Clone, Default)]
pub struct Model {
    /// Source names indexed by source ranges, with user source at zero.
    pub sources: Vec<String>,
    /// Resolved kind schemas with inherited fields.
    pub kinds: Vec<Kind>,
    /// Kind names live separately from graph instance names.
    pub kind_names: BTreeMap<String, KindId>,
    /// Resolved traits with inherited entries.
    pub traits: Vec<Trait>,
    /// Trait names share the kind namespace, so no name is in both maps.
    pub trait_names: BTreeMap<String, TraitId>,
    /// Node definitions in declaration order.
    pub nodes: Vec<Node>,
    /// Directed and undirected connections, including explicit loop-back edges.
    pub edges: Vec<Edge>,
    /// Fork definitions and their attached continuations.
    pub forks: Vec<Fork>,
    /// Explicit convergence definitions.
    pub junctions: Vec<Junction>,
    /// Resolved network memberships and structural validation policies.
    pub networks: Vec<Network>,
    /// Authored root paths.
    pub paths: Vec<GraphPath>,
    /// Field and property expressions indexed by binding identity.
    pub bindings: Vec<Binding>,
    /// Resolved graph references with their use-site locations.
    pub refs: Vec<ResolvedRef>,
    /// The document-wide namespace shared by named graph elements.
    pub names: BTreeMap<String, ElementId>,
}

macro_rules! identity {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(pub usize);
    };
}
identity!(NodeId, "A node index within its owning model.");
identity!(EdgeId, "An edge index within its owning model.");
identity!(ForkId, "A fork index within its owning model.");
identity!(JunctionId, "A junction index within its owning model.");
identity!(BindingId, "A binding index within its owning model.");
identity!(RefId, "A reference index within its owning model.");

identity!(KindId, "A kind schema index within its owning model.");
identity!(TraitId, "A trait index within its owning model.");

/// The graph construct described by a kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Construct {
    /// A node.
    Node,
    /// An edge.
    Edge,
    /// A fork.
    Fork,
    /// A junction.
    Junction,
}
impl Construct {
    /// The root kind name for this construct.
    pub fn name(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Edge => "edge",
            Self::Fork => "fork",
            Self::Junction => "junction",
        }
    }
}

/// A schema value type independent of runtime evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Any value.
    Any,
    /// A string.
    String,
    /// A number.
    Number,
    /// A boolean.
    Boolean,
    /// An array.
    Array,
    /// An object.
    Object,
}

/// One property or content field declared by a kind.
#[derive(Debug, Clone)]
pub struct FieldSchema {
    /// Field name.
    pub name: String,
    /// Accepted value type.
    pub ty: FieldType,
    /// Whether an instance must supply this field.
    pub required: bool,
    /// A constant fallback for omitted fields.
    pub default: Option<Value>,
    /// Optional role without an operation binding. Input roles retain runtime reads.
    pub role: Option<String>,
    /// Schema field location.
    pub range: SrcRange,
}

/// The orientation of an edge instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    /// Ordered source and target endpoints.
    Directed,
    /// Unordered connectivity with stable authored left and right endpoints.
    Undirected,
}

/// Directions permitted by an edge kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directions {
    /// Only directed connections.
    Directed,
    /// Only undirected connections.
    Undirected,
    /// Either orientation.
    Either,
}
impl Directions {
    /// Whether this kind permits an instance orientation.
    pub fn allows(self, direction: Direction) -> bool {
        self == Self::Either
            || matches!(
                (self, direction),
                (Self::Directed, Direction::Directed) | (Self::Undirected, Direction::Undirected)
            )
    }
}

/// When a network's structural constraints must be checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationStage {
    /// Check the authored graph during compilation.
    Definition,
    /// A consumer must check each materialization group.
    Materialized,
    /// Both compilation and materialization checks are required.
    Both,
}

/// Which cycles a network prohibits, independently of loop punctuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acyclic {
    /// Cycles are allowed.
    None,
    /// All selected edges must be directed and have no directed cycle.
    Directed,
    /// Ignore direction while preserving distinct edges.
    Underlying,
}

/// Required reachability within a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    /// Disconnected components are allowed.
    None,
    /// Ignore direction when testing reachability.
    Weak,
    /// Respect direction, traversing undirected edges either way.
    Strong,
}

/// Inclusive nonnegative edge-count limits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DegreeBounds {
    /// Optional minimum.
    pub min: Option<usize>,
    /// Optional maximum.
    pub max: Option<usize>,
}

/// A network node's resolved selection and enforcement contract.
#[derive(Debug, Clone)]
pub struct Network {
    /// The node carrying this configuration.
    pub node: NodeId,
    /// Explicit participating nodes, including isolated nodes.
    pub nodes: Vec<NodeId>,
    /// Explicit participating edges.
    pub edges: Vec<EdgeId>,
    /// Required validation stage.
    pub validate: ValidationStage,
    /// Cycle restriction.
    pub acyclic: Acyclic,
    /// Connectivity restriction.
    pub connected: Connectivity,
    /// Whether selected edges may connect a node to itself.
    pub self_loops: bool,
    /// Optional limit per endpoint pair and direction category.
    pub max_parallel: Option<usize>,
    /// Directed incoming degree bounds for every included node.
    pub incoming: DegreeBounds,
    /// Directed outgoing degree bounds for every included node.
    pub outgoing: DegreeBounds,
    /// Undirected degree bounds, counting a self-loop twice.
    pub degree: DegreeBounds,
}

/// A resolved user-defined kind.
#[derive(Debug, Clone)]
pub struct Kind {
    /// Declared kind name.
    pub name: String,
    /// Graph construct category.
    pub construct: Construct,
    /// Authored base name.
    pub base: String,
    /// Resolved base identity for a user-defined base.
    pub base_kind: Option<KindId>,
    /// Inherited primitive or neutral root operation.
    pub primitive: String,
    /// Edge direction allowance, absent for other constructs.
    pub direction: Option<Directions>,
    /// Effective property schema, including inherited entries.
    pub props: Vec<FieldSchema>,
    /// Effective node content schema, including inherited entries.
    pub fields: Vec<FieldSchema>,
    /// Whether a node of this kind accepts content fields the kind does not declare.
    pub open: bool,
    /// Every trait this kind has, including those of its base and the traits they extend.
    pub traits: Vec<TraitId>,
    /// Declaration location.
    pub range: SrcRange,
}

/// A resolved trait: a schema that kinds attach, with no behavior of its own.
#[derive(Debug, Clone)]
pub struct Trait {
    /// Declared trait name.
    pub name: String,
    /// The trait this one extends.
    pub base: Option<TraitId>,
    /// Constructs whose kinds may have this trait.
    pub on: Vec<Construct>,
    /// Effective property schema, including entries of extended traits.
    pub props: Vec<FieldSchema>,
    /// Effective node content schema, including entries of extended traits.
    pub fields: Vec<FieldSchema>,
    /// Definition location.
    pub range: SrcRange,
}

/// The identity and category of a graph element.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ElementId {
    /// A node definition.
    Node(NodeId),
    /// An edge definition.
    Edge(EdgeId),
    /// A fork definition.
    Fork(ForkId),
    /// A junction definition.
    Junction(JunctionId),
}

/// An authored node with no domain-specific schema.
#[derive(Debug, Clone)]
pub struct Node {
    /// Optional authored name.
    pub name: Option<String>,
    /// Open node-kind identifier.
    pub kind: String,
    /// Configuration properties.
    pub props: Vec<BindingId>,
    /// Content fields.
    pub fields: Vec<BindingId>,
    /// Full source location.
    pub range: SrcRange,
}

/// A connection between two existing node identities.
#[derive(Debug, Clone)]
pub struct Edge {
    /// Optional authored name.
    pub name: Option<String>,
    /// Open edge-kind identifier.
    pub kind: String,
    /// Instance orientation.
    pub direction: Direction,
    /// Source or authored left node.
    pub source: NodeId,
    /// Target or authored right node.
    pub target: NodeId,
    /// Configuration properties.
    pub props: Vec<BindingId>,
    /// Explicit cycle marker, without an inferred retry policy.
    pub cycle: bool,
    /// Owning fork when declared in a fork entry.
    pub fork: Option<ForkId>,
    /// Source location of the edge header.
    pub range: SrcRange,
}

/// A typed graph split from one node.
#[derive(Debug, Clone)]
pub struct Fork {
    /// Optional authored name.
    pub name: Option<String>,
    /// Authored kind, including the built-in graph primitives.
    pub kind: String,
    /// Source node.
    pub source: NodeId,
    /// Configuration properties.
    pub props: Vec<BindingId>,
    /// Edge identities for each single-step branch.
    pub branches: Vec<Vec<EdgeId>>,
    /// Structurally attached sibling paths.
    pub continuations: Vec<GraphPath>,
    /// Source location.
    pub range: SrcRange,
}

/// Explicit convergence of the listed incoming edges.
#[derive(Debug, Clone)]
pub struct Junction {
    /// Authored kind defining how incoming data is combined.
    pub kind: String,
    /// Exact incoming edge identities.
    pub inputs: Vec<EdgeId>,
    /// Configuration properties without an implied arrival policy.
    pub props: Vec<BindingId>,
    /// Destination node.
    pub target: NodeId,
    /// Owning fork for an attached junction.
    pub fork: Option<ForkId>,
    /// Source location.
    pub range: SrcRange,
}

/// A path retaining authoring structure without imposing runtime scheduling.
#[derive(Debug, Clone)]
pub struct GraphPath {
    /// Its initial authoring position.
    pub origin: PathOrigin,
    /// Connections and splits in source order.
    pub steps: Vec<PathStep>,
    /// Source location.
    pub range: SrcRange,
}

/// The start of a root or attached path.
#[derive(Debug, Clone)]
pub enum PathOrigin {
    /// A root node.
    Node(NodeId),
    /// A selector attached to one fork.
    Selector {
        /// The enclosing fork.
        fork: ForkId,
        /// The selected target node.
        target: NodeId,
    },
    /// A junction destination.
    Junction(JunctionId),
}

/// One step through the authored graph.
#[derive(Debug, Clone)]
pub enum PathStep {
    /// One or more edges with common endpoints.
    Edges(Vec<EdgeId>),
    /// A fork and its attached continuations.
    Fork(ForkId),
    /// Sibling chains sharing the node reached so far as their origin.
    Arms(Vec<Vec<PathStep>>),
}

/// A property or content expression owned by a graph element.
#[derive(Debug, Clone)]
pub struct Binding {
    /// Owning graph identity.
    pub owner: ElementId,
    /// Whether this binding belongs to properties rather than content.
    pub property: bool,
    /// Field path, including nested keys when selected independently.
    pub path: Vec<String>,
    /// Declared schema type, also required of values supplied at runtime.
    pub ty: FieldType,
    /// Lowered expression, preserving dynamic evaluation.
    pub value: Value,
    /// Source location.
    pub range: SrcRange,
}

/// A graph-valued reference resolved without cloning its target.
#[derive(Debug, Clone)]
pub struct ResolvedRef {
    /// The selected graph definition.
    pub target: ElementId,
    /// The authored reference location.
    pub range: SrcRange,
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
    /// A required value delivered for this instance field at runtime.
    Input {
        /// Destination binding identity.
        binding: BindingId,
        /// Type declared by the owning kind, or any for an open kind.
        ty: FieldType,
    },
    /// A destination field identity, without reading its value.
    FieldRef(BindingId),
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
    /// Context supplied for this graph element by an evaluator.
    Context {
        /// The element whose context is read.
        owner: ElementId,
        /// The authored path within that context.
        path: Vec<String>,
    },
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
    /// A resolved field or variable binding and the location of this read.
    BindingRef(BindingId, SrcRange),
    /// A graph reference resolved through the owning model.
    GraphRef {
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
    /// An English language operation evaluated by the consumer.
    English {
        /// The English operation to perform.
        operation: EnglishOperation,
        /// Arguments in authored order, including optional arguments when supplied.
        args: Vec<Value>,
    },
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
    /// Substitutes named values into text.
    Render {
        /// The template to render.
        template: Box<Value>,
        /// The named scalar values.
        values: Box<Value>,
    },
    /// Joins nonempty text components.
    Compose {
        /// The text components, including optional nulls.
        array: Box<Value>,
        /// The separator between components.
        separator: Box<Value>,
    },
    /// Samples array positions without replacement in source order.
    Sample {
        /// The source array.
        array: Box<Value>,
        /// The number of positions to select.
        count: Box<Value>,
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

/// English language operations evaluated by consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnglishOperation {
    /// Generate a procedural English utterance.
    Express,
    /// Generate an utterance with explicit selection and conversation context.
    Expression,
    /// Inspect eligible expression choices without sampling.
    ExpressionCandidates,
    /// Render explicit grammatical features as English text.
    Realize,
    /// Select a supported wording variation.
    Vary,
    /// Combine explicitly related propositions.
    Combine,
    /// Refer to an entity using explicit discourse context.
    Refer,
    /// Return an updated discourse context.
    Remember,
    /// Select details not established in a discourse context.
    Unmentioned,
    /// Format a decimal using an English regional convention.
    FormatNumber,
    /// Format an explicit date using an English regional convention.
    FormatDate,
    /// Format an explicit time using an English regional convention.
    FormatTime,
}

impl EnglishOperation {
    /// The corresponding built-in function name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Express => "express",
            Self::Expression => "expression",
            Self::ExpressionCandidates => "expression_candidates",
            Self::Realize => "realize",
            Self::Vary => "vary",
            Self::Combine => "combine",
            Self::Refer => "refer",
            Self::Remember => "remember",
            Self::Unmentioned => "unmentioned",
            Self::FormatNumber => "format_number",
            Self::FormatDate => "format_date",
            Self::FormatTime => "format_time",
        }
    }
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
    /// A resolved field or variable binding and the location of this read.
    BindingRef(BindingId),
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
