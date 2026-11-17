use crate::diag::{Diag, DiagPhase, Diags, SrcRange};
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
    /// For each node, the context keys that reach it and their suppliers.
    pub context: Vec<Vec<ContextSupply>>,
    /// The values elements supply for the context keys their kinds provide with a value.
    pub context_values: Vec<ContextValue>,
    /// Every context read with the element that owns it, in source order.
    pub context_reads: Vec<ContextRead>,
}

/// The value one element supplies for a context key.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ContextValue {
    /// The supplying element.
    pub owner: ElementId,
    /// Key name.
    pub key: String,
    /// The value expression, lowered as if written on the supplying element.
    pub binding: BindingId,
}

/// One read of a context key.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ContextRead {
    /// The element whose context is read.
    pub owner: ElementId,
    /// The key and any further path.
    pub path: Vec<String>,
    /// Source location of the read.
    pub range: SrcRange,
}

impl Model {
    /// Whether a kind is `base` or derives from it.
    pub fn derives(&self, kind: &str, base: &str) -> bool {
        let mut name = kind;
        for _ in 0..=self.kinds.len() {
            if name == base {
                return true;
            }
            let Some(id) = self.kind_names.get(name) else { return false };
            name = &self.kinds[id.0].base;
        }
        false
    }
}

/// One context key that reaches a node.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ContextSupply {
    /// Key name.
    pub key: String,
    /// The nearest providing element on every path that carries the key.
    pub suppliers: Vec<ElementId>,
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
    /// The root kind of this construct.
    pub fn root(self) -> Primitive {
        match self {
            Self::Node => Primitive::Node,
            Self::Edge => Primitive::Edge,
            Self::Fork => Primitive::Fork,
            Self::Junction => Primitive::Junction,
        }
    }
}

/// A kind the language defines: a construct root or a built-in primitive.
///
/// Every element resolves to one, whatever kind it was authored with, since a
/// derived kind keeps the primitive of its base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Primitive {
    /// The root node kind.
    Node,
    /// The root edge kind.
    Edge,
    /// The root fork kind.
    Fork,
    /// The root junction kind.
    Junction,
    /// A node that validates a set of nodes and edges as a network.
    Network,
    /// A node that relates participants.
    Relation,
    /// A node that holds values.
    Vars,
    /// An edge that attaches a participant to a relation.
    Incidence,
    /// An edge that associates two nodes.
    Link,
    /// An edge that schedules its target after its source.
    Next,
    /// An edge that makes its target wait for its source.
    Requires,
    /// An edge that makes its target a member of its source.
    Contains,
    /// An edge that supplies a value to a field of its target.
    Feeds,
    /// A fork that selects one branch by weight.
    Weighted,
    /// A fork that runs its branches a number of times.
    Repeat,
    /// A fork that runs its branches once for each item.
    Each,
    /// A fork that runs every branch.
    Parallel,
    /// A fork that runs every branch whose guard holds.
    Conditional,
    /// A fork that runs the one branch whose guard holds.
    Exclusive,
    /// A junction that folds its arrivals into one value.
    Reduce,
    /// A junction that collects its arrivals.
    Accumulate,
    /// A junction that passes each arrival on.
    Merge,
    /// A junction that passes the first arrival on.
    First,
    /// A junction that waits for a number of arrivals.
    Quorum,
}
impl Primitive {
    /// Every primitive, roots first.
    pub const ALL: [Self; 24] = [
        Self::Node,
        Self::Edge,
        Self::Fork,
        Self::Junction,
        Self::Network,
        Self::Relation,
        Self::Vars,
        Self::Incidence,
        Self::Link,
        Self::Next,
        Self::Requires,
        Self::Contains,
        Self::Feeds,
        Self::Weighted,
        Self::Repeat,
        Self::Each,
        Self::Parallel,
        Self::Conditional,
        Self::Exclusive,
        Self::Reduce,
        Self::Accumulate,
        Self::Merge,
        Self::First,
        Self::Quorum,
    ];
    /// The kind name authors write.
    pub fn name(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Edge => "edge",
            Self::Fork => "fork",
            Self::Junction => "junction",
            Self::Network => "network",
            Self::Relation => "relation",
            Self::Vars => "vars",
            Self::Incidence => "incidence",
            Self::Link => "link",
            Self::Next => "next",
            Self::Requires => "requires",
            Self::Contains => "contains",
            Self::Feeds => "feeds",
            Self::Weighted => "weighted",
            Self::Repeat => "repeat",
            Self::Each => "each",
            Self::Parallel => "parallel",
            Self::Conditional => "conditional",
            Self::Exclusive => "exclusive",
            Self::Reduce => "reduce",
            Self::Accumulate => "accumulate",
            Self::Merge => "merge",
            Self::First => "first",
            Self::Quorum => "quorum",
        }
    }
    /// The construct the primitive belongs to.
    pub fn construct(self) -> Construct {
        match self {
            Self::Node | Self::Network | Self::Relation | Self::Vars => Construct::Node,
            Self::Edge | Self::Incidence | Self::Link | Self::Next | Self::Requires | Self::Contains | Self::Feeds => {
                Construct::Edge
            }
            Self::Fork | Self::Weighted | Self::Repeat | Self::Each | Self::Parallel | Self::Conditional | Self::Exclusive => {
                Construct::Fork
            }
            Self::Junction | Self::Reduce | Self::Accumulate | Self::Merge | Self::First | Self::Quorum => Construct::Junction,
        }
    }
    /// The primitive a built-in kind name means within a construct.
    pub fn named(construct: Construct, name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|primitive| primitive.construct() == construct && primitive.name() == name)
    }
    /// Whether this is a construct root, which installs no behavior of its own.
    pub fn is_root(self) -> bool {
        self == self.construct().root()
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
    /// Where the definition states the value of a computed field or a provided context key, when it does.
    pub value: Option<SrcRange>,
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
    pub primitive: Primitive,
    /// Edge direction allowance, absent for other constructs.
    pub direction: Option<Directions>,
    /// Effective property schema, including inherited entries.
    pub props: Vec<FieldSchema>,
    /// Effective node content schema, including inherited entries.
    pub fields: Vec<FieldSchema>,
    /// Whether a node of this kind accepts content fields the kind does not declare.
    pub open: bool,
    /// Context keys an instance supplies to what it leads to, including inherited and trait entries.
    pub provides: Vec<FieldSchema>,
    /// Context keys an instance reads itself, including inherited and trait entries.
    pub requires: Vec<FieldSchema>,
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
    /// Context keys a kind with this trait supplies to what it leads to.
    pub provides: Vec<FieldSchema>,
    /// Context keys a kind with this trait reads itself.
    pub requires: Vec<FieldSchema>,
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
    /// The language primitive the kind resolves to.
    pub primitive: Primitive,
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
    /// The language primitive the kind resolves to.
    pub primitive: Primitive,
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
    /// The language primitive the kind resolves to.
    pub primitive: Primitive,
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
    /// The language primitive the kind resolves to.
    pub primitive: Primitive,
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
    /// A call to a function a provider contributes.
    ///
    /// The language checks the arguments against the provider's descriptor and
    /// never folds the call. Its meaning belongs to the provider.
    Call {
        /// The function name, as the provider declares it.
        function: String,
        /// Arguments in authored order, including optional arguments when supplied.
        args: Vec<Value>,
        /// The result type the provider declares.
        returns: FieldType,
        /// The fields of the result the provider declares, when it declares any.
        shape: Option<ResultShape>,
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
}

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

/// What a provider declares: its identity, its kinds, its functions, and what it gives meaning to.
///
/// A provider fills in one value of this type. It holds no behavior, so a host can
/// compile and check source against a provider without running anything. The parts
/// of a provider that execute are defined by its consumers, outside this crate. That
/// includes what a function computes: a [`FunctionDesc`] says only how it is called.
///
/// A kind is supported when it is listed or derives from a listed kind. Listing a
/// construct root, such as `node`, supports that kind alone, since a kind derived
/// from a root is a new kind the provider knows nothing about. A context key is
/// supported when it is listed, or when a definition that provides the key supplies
/// its value, which needs no provider.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSchema<'a> {
    /// A stable identifier, unique among providers.
    pub id: &'a str,
    /// The provider's name in diagnostics.
    pub name: &'a str,
    /// The provider's kinds, traits and context keys, written in the language.
    ///
    /// Compilation supplies these in order, so a source range with source index
    /// `n + 1` lies in `definitions[n]`.
    pub definitions: &'a [crate::semantics::Source<'a>],
    /// Supported node kinds.
    pub nodes: &'a [&'a str],
    /// Supported edge kinds.
    pub edges: &'a [&'a str],
    /// Supported fork kinds.
    pub forks: &'a [&'a str],
    /// Supported junction kinds.
    pub junctions: &'a [&'a str],
    /// Context keys the provider supplies.
    pub context: &'a [&'a str],
    /// Functions the provider contributes to expressions.
    ///
    /// A name is unique among them and is not the name of a built-in function.
    pub functions: &'a [FunctionDesc],
}

/// A function a provider contributes, as the language sees it.
///
/// The language checks a call against this and leaves the call in the model as
/// [`Func::Call`]. It never evaluates or folds one.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct FunctionDesc {
    /// The name, syntax, summary, examples and rules, as for a built-in function.
    pub doc: crate::spec::FuncDesc,
    /// Parameters in order. A call supplies every parameter that is not optional,
    /// and optional parameters come last.
    pub params: &'static [ParamDesc],
    /// The type of the result, or any when it is known only on evaluation.
    pub returns: FieldType,
    /// The fields of the result, when reads into it can be typed before evaluation.
    pub shape: Option<ResultShape>,
}

/// What the result of a provider function contains.
///
/// A read into the result is typed from this, so a field of the wrong type is
/// reported at compilation. A field the shape does not list is typed on evaluation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResultShape {
    /// A value of one type.
    Scalar(FieldType),
    /// An object with the listed fields.
    Object(&'static [(&'static str, ResultShape)]),
    /// An array whose elements share a shape.
    Array(&'static ResultShape),
}

/// One parameter of a provider function.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ParamDesc {
    /// The parameter name in diagnostics.
    pub name: &'static str,
    /// The accepted types. An argument fed by context is checked on evaluation.
    pub accepts: &'static [FieldType],
    /// Whether null is accepted beside those types.
    pub nullable: bool,
    /// Whether a call may leave the argument out.
    pub optional: bool,
    /// Whether the argument must be a constant.
    pub constant: bool,
    /// Inclusive bounds a constant argument must be an integer within.
    pub bounds: Option<(i64, i64)>,
}

/// One call to a provider function, with the element whose value contains it.
#[derive(Debug, Clone, Copy)]
pub struct CallSite<'m> {
    /// The element that owns the field or property the call is written in.
    pub owner: ElementId,
    /// The function name.
    pub function: &'m str,
    /// Arguments in authored order.
    pub args: &'m [Value],
    /// Source location of the call.
    pub range: SrcRange,
}

impl<'a> ProviderSchema<'a> {
    /// Returns the definition source a source range index refers to.
    ///
    /// Index zero is the user's source, which the caller holds, so it returns `None`.
    pub fn definition(&self, source: usize) -> Option<crate::semantics::Source<'a>> {
        self.definitions.get(source.checked_sub(1)?).copied()
    }
}

impl Model {
    /// Returns every call to a provider function, in binding order.
    pub fn calls(&self) -> Vec<CallSite<'_>> {
        fn each<'m>(values: &'m [Value], owner: ElementId, found: &mut Vec<CallSite<'m>>) {
            values.iter().for_each(|value| walk(value, owner, found));
        }
        fn walk<'m>(value: &'m Value, owner: ElementId, found: &mut Vec<CallSite<'m>>) {
            match value {
                Value::Input { .. }
                | Value::FieldRef(_)
                | Value::Str(_)
                | Value::Num(_)
                | Value::Bool(_)
                | Value::Null
                | Value::Context { .. }
                | Value::BindingRef(..)
                | Value::GraphRef { .. } => {}
                Value::Template(template) => {
                    for part in &template.parts {
                        if let Part::Dynamic(value) = part {
                            walk(value, owner, found);
                        }
                    }
                }
                Value::Array(array) => {
                    for ArrayElem::Item(value) | ArrayElem::Spread(value) in &array.elem {
                        walk(value, owner, found);
                    }
                }
                Value::Object(object) => object.elem.iter().for_each(|field| walk(&field.value, owner, found)),
                Value::Unary { operand, .. } => walk(operand, owner, found),
                Value::Binary { lhs, rhs, .. } => {
                    walk(lhs, owner, found);
                    walk(rhs, owner, found);
                }
                Value::Cond { cond, then, otherwise } => {
                    walk(cond, owner, found);
                    walk(then, owner, found);
                    walk(otherwise, owner, found);
                }
                Value::Index { target, index, .. } => {
                    walk(target, owner, found);
                    walk(index, owner, found);
                }
                Value::Slice { target, start, end, .. } => {
                    walk(target, owner, found);
                    for bound in start.iter().chain(end) {
                        walk(bound, owner, found);
                    }
                }
                Value::Func { func, range } => match func {
                    Func::Call { function, args, .. } => {
                        found.push(CallSite {
                            owner,
                            function,
                            args,
                            range: *range,
                        });
                        each(args, owner, found);
                    }
                    Func::Choice(values) | Func::Min(values) | Func::Max(values) => each(values, owner, found),
                    Func::Format { args, .. } => each(args, owner, found),
                    Func::Weighted(options) => options.iter().for_each(|option| walk(&option.value, owner, found)),
                    Func::Range(_)
                    | Func::Normal { .. }
                    | Func::Lognormal { .. }
                    | Func::Exponential { .. }
                    | Func::Pareto { .. }
                    | Func::Beta { .. }
                    | Func::Poisson { .. }
                    | Func::Chance { .. }
                    | Func::Uuid
                    | Func::Hex { .. }
                    | Func::Alphanum { .. } => {}
                    Func::Upper { text } | Func::Lower { text } | Func::Trim { text } => walk(text, owner, found),
                    Func::Len { target: value }
                    | Func::Round { value }
                    | Func::Floor { value }
                    | Func::Ceil { value }
                    | Func::Abs { value } => walk(value, owner, found),
                    Func::Replace { text, from, to } => {
                        walk(text, owner, found);
                        walk(from, owner, found);
                        walk(to, owner, found);
                    }
                    Func::Clamp { value, min, max } => {
                        walk(value, owner, found);
                        walk(min, owner, found);
                        walk(max, owner, found);
                    }
                    Func::Split {
                        text: first,
                        separator: second,
                    }
                    | Func::Join {
                        array: first,
                        separator: second,
                    }
                    | Func::Compose {
                        array: first,
                        separator: second,
                    }
                    | Func::Render {
                        template: first,
                        values: second,
                    }
                    | Func::Sample {
                        array: first,
                        count: second,
                    }
                    | Func::Contains {
                        target: first,
                        needle: second,
                    }
                    | Func::StartsWith {
                        text: first,
                        prefix: second,
                    }
                    | Func::EndsWith {
                        text: first,
                        suffix: second,
                    } => {
                        walk(first, owner, found);
                        walk(second, owner, found);
                    }
                },
            }
        }
        let mut found = Vec::new();
        for binding in &self.bindings {
            walk(&binding.value, binding.owner, &mut found);
        }
        found
    }

    /// Reports every listed element whose kind a provider does not support, and
    /// every context read of those elements that nothing supplies.
    ///
    /// The elements are the ones the provider would walk. Other elements of the
    /// model may belong to another provider and are left alone.
    pub fn unsupported(&self, provider: &ProviderSchema, elements: &[ElementId]) -> Diags {
        let model = self;
        let mut diags = Vec::new();
        for element in elements {
            let (construct, kind, range, supported) = match *element {
                ElementId::Node(id) => ("node", &model.nodes[id.0].kind, model.nodes[id.0].range, provider.nodes),
                ElementId::Edge(id) => ("edge", &model.edges[id.0].kind, model.edges[id.0].range, provider.edges),
                ElementId::Fork(id) => ("fork", &model.forks[id.0].kind, model.forks[id.0].range, provider.forks),
                ElementId::Junction(id) => (
                    "junction",
                    &model.junctions[id.0].kind,
                    model.junctions[id.0].range,
                    provider.junctions,
                ),
            };
            let supports = |base: &&str| {
                if *base == construct {
                    kind == base
                } else {
                    model.derives(kind, base)
                }
            };
            if !supported.iter().any(supports) {
                diags.push(Diag {
                    when: DiagPhase::Validation,
                    what: format!("{construct} kind `{kind}` is not supported by {}", provider.name),
                    r#where: range,
                });
            }
        }
        for read in &model.context_reads {
            let Some(key) = read.path.first() else { continue };
            if !elements.contains(&read.owner) || provider.context.contains(&key.as_str()) {
                continue;
            }
            // a definition that provides the key with a value supplies it without a provider
            let supplied = match read.owner {
                ElementId::Node(node) => (model.context.get(node.0).into_iter().flatten())
                    .filter(|supply| &supply.key == key)
                    .flat_map(|supply| &supply.suppliers)
                    .any(|supplier| (model.context_values.iter()).any(|value| &value.key == key && value.owner == *supplier)),
                _ => false,
            };
            if !supplied {
                diags.push(Diag {
                    when: DiagPhase::Validation,
                    what: format!("context key `{key}` is not supplied by {}", provider.name),
                    r#where: read.range,
                });
            }
        }
        diags.sort_by_key(|diag| (diag.r#where.source, diag.r#where.start));
        diags
    }
}
