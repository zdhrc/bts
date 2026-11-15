use crate::diag::SrcRange;
use std::fmt;

/// Candidate graph paths before semantic validation.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphDocument {
    /// Kind declarations in source order.
    pub kinds: Vec<KindDecl>,
    /// Root paths in source order.
    pub paths: Vec<GraphPath>,
}

/// A schema declaration for one graph construct.
#[derive(Debug, Clone, PartialEq)]
pub struct KindDecl {
    /// The graph construct this kind specializes.
    pub construct: crate::model::Construct,
    /// The new kind name.
    pub name: String,
    /// The base kind name.
    pub base: String,
    /// Schema descriptors.
    pub schema: Vec<Attr>,
    /// Complete declaration location.
    pub range: SrcRange,
}

/// A path with its structural attachment preserved.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphPath {
    /// The node, selector or junction introducing this path.
    pub start: GraphOrigin,
    /// Connections in source order.
    pub steps: Vec<GraphStep>,
    /// The path range, including the terminator for a root statement.
    pub range: SrcRange,
}

/// The authoring position at the start of a path.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphOrigin {
    /// A root node declaration or reference.
    Node(GraphNode),
    /// A reference selecting a target of the enclosing fork.
    Selector(Expr),
    /// A junction combining explicitly listed incoming edges.
    Junction(GraphJunction),
}

/// A connection or split from the current node.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphStep {
    /// An ordinary or cycle connection.
    Edge(GraphEdgeStep),
    /// A typed fork with structurally attached continuations.
    Fork(GraphFork),
}

/// A typed fork and the paths attached to it by layout.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphFork {
    /// The authored name, kind and properties.
    pub header: GraphHeader,
    /// Each entry contains exactly one connection step.
    pub entries: Vec<GraphEdgeStep>,
    /// Sibling selectors and junctions, never implicit connections to each other.
    pub continuations: Vec<GraphPath>,
    /// The complete fork range, including attached paths.
    pub range: SrcRange,
}

/// A convergence point with exact edge references and an explicit destination.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphJunction {
    /// Required junction kind.
    pub kind: String,
    /// Qualified or unqualified references to input edges.
    pub inputs: Vec<Expr>,
    /// Configuration properties separate from the destination node.
    pub props: Option<Vec<Attr>>,
    /// The destination declaration or reference.
    pub target: GraphNode,
    /// The junction range, including its destination.
    pub range: SrcRange,
}

/// A node declaration or a reference to an existing node.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphNode {
    /// A new node with separate properties and content fields.
    Declaration(GraphNodeDecl),
    /// A node reference expression, without resolving its target.
    Reference(Expr),
}

/// A typed node and its optional content body.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeDecl {
    /// The authored name, kind and properties.
    pub header: GraphHeader,
    /// Content fields, distinct from properties.
    pub body: Option<Vec<Attr>>,
    /// The declaration range, including its body when present.
    pub range: SrcRange,
}

/// A typed graph element header.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphHeader {
    /// The optional identifier or plain quoted name.
    pub name: Option<String>,
    /// The source range of the authored name.
    pub name_range: Option<SrcRange>,
    /// The required kind, before checking its contract.
    pub kind: String,
    /// Configuration fields, absent when no property object was written.
    pub props: Option<Vec<Attr>>,
    /// The header range, excluding surrounding graph delimiters.
    pub range: SrcRange,
}

/// One connection step, possibly declaring multiple edges with the same endpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdgeStep {
    /// Authored edge headers in source order.
    pub edges: Vec<GraphHeader>,
    /// Whether the connection is directed or undirected.
    pub direction: crate::model::Direction,
    /// Whether this step uses explicit cycle punctuation.
    pub cycle: bool,
    /// The destination declaration or reference.
    pub target: GraphNode,
    /// The connector and destination range.
    pub range: SrcRange,
}

/// The graph boundary used for a reference expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphReferenceKind {
    /// Parentheses select a node.
    Node,
    /// Brackets select an edge or fork.
    Edge,
}

/// A path pattern that selects one existing node or edge.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphPattern {
    /// The initial node reference or constraint.
    pub start: GraphPatternNode,
    /// Connections that constrain the matching path.
    pub steps: Vec<GraphPatternStep>,
    /// The complete pattern range, including its enclosing punctuation.
    pub range: SrcRange,
}

/// An existing node reference or a node constrained by kind and properties.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphPatternNode {
    /// A node-valued reference, without creating a node.
    Reference(Expr),
    /// A node match, optionally marked as the selected result.
    Constraint(GraphPatternConstraint),
}

/// A kind and optional properties that constrain an existing graph element.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphPatternConstraint {
    /// Whether the authored marker selects this element as the result.
    pub selected: bool,
    /// The required node or edge kind.
    pub kind: String,
    /// Authored property constraints, without evaluating them.
    pub props: Option<Vec<Attr>>,
    /// The constraint range, excluding its surrounding delimiters.
    pub range: SrcRange,
}

/// An ordinary or cycle connection within a reference pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphPatternStep {
    /// Edge constraints sharing the same endpoints.
    pub edges: Vec<GraphPatternConstraint>,
    /// Whether the connection is directed or undirected.
    pub direction: crate::model::Direction,
    /// Whether this step uses explicit cycle punctuation.
    pub cycle: bool,
    /// The destination reference or constraint.
    pub target: GraphPatternNode,
    /// The connection and destination range.
    pub range: SrcRange,
}

/// A parsed field assignment and its location in source.
#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    /// The key identifying this entry.
    pub key: String,
    /// The value expression to inspect or evaluate.
    pub value: Expr,
    /// The expression’s byte range in the original source.
    pub range: SrcRange,
}

/// An expression with its original source location, before type checking.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// The parsed expression form.
    pub kind: ExprKind,
    /// The expression’s byte range in the original source.
    pub range: SrcRange,
}

/// The structure of an expression before names and types are checked.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A required incoming field value, legal only on instance fields.
    Upstream,
    /// A path-pattern reference selecting an existing graph element.
    PatternRef(Box<GraphPattern>),
    /// An explicit graph reference whose identity is resolved later.
    GraphRef {
        /// The authored graph boundary.
        kind: GraphReferenceKind,
        /// A name or expression denoting the referenced element.
        target: Box<Expr>,
    },
    /// A graph member selected with a bracketed name after a dot.
    GraphMember {
        /// The graph value being qualified.
        target: Box<Expr>,
        /// The authored member name.
        name: String,
    },
    /// A string value.
    Str(String),
    /// Text containing interpolated values.
    Template(Vec<TemplatePart>),
    /// The numeric spelling from source, before conversion or range checks.
    Num(String),
    /// A boolean value.
    Bool(bool),
    /// An explicit null value.
    Null,
    /// An ordered collection.
    Array(Vec<Expr>),
    /// A collection of named entries.
    Object(Vec<ObjectItem>),
    /// A reference expression whose meaning depends on its context.
    Ref {
        /// Written path segments, without looking them up in a model.
        path: Vec<String>,
    },
    /// A function call.
    Func {
        /// The name written in source.
        name: String,
        /// Function arguments in source order.
        args: Vec<Expr>,
    },
    /// An element selection.
    Index {
        /// The value to index, slice, or inspect.
        target: Box<Expr>,
        /// The expression selecting an element.
        index: Box<Expr>,
    },
    /// A selection between optional bounds.
    Slice {
        /// The value to index, slice, or inspect.
        target: Box<Expr>,
        /// The inclusive lower bound, or `None` to start at the beginning.
        start: Option<Box<Expr>>,
        /// The exclusive upper bound, or `None` to continue to the end.
        end: Option<Box<Expr>>,
    },
    /// A collection expanded into its surrounding container.
    Spread(Box<Expr>),
    /// A collection comprehension.
    For {
        /// Variable names introduced for each comprehension iteration.
        bindings: Vec<String>,
        /// The collection expression to iterate over.
        collection: Box<Expr>,
        /// An output key for an object comprehension, or `None` for an array.
        key: Option<Box<Expr>>,
        /// The expression evaluated for each iteration.
        body: Box<Expr>,
        /// The condition controlling this expression or branch.
        cond: Option<Box<Expr>>,
    },
    /// An operation with one operand.
    Unary {
        /// The operator applied to the operands.
        op: UnaryOp,
        /// The expression to which the operator applies.
        operand: Box<Expr>,
    },
    /// An operation with two operands.
    Binary {
        /// The operator applied to the operands.
        op: BinOp,
        /// The left operand.
        lhs: Box<Expr>,
        /// The right operand.
        rhs: Box<Expr>,
    },
    /// A conditional expression that evaluates only the selected branch.
    Cond {
        /// The condition controlling this expression or branch.
        cond: Box<Expr>,
        /// The expression evaluated when the condition is true.
        then: Box<Expr>,
        /// The expression evaluated when the condition is false.
        otherwise: Box<Expr>,
    },
}

/// A named object entry or a spread expression.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjectItem {
    /// A named field assignment.
    Attr(Attr),
    /// A collection expanded into its surrounding container.
    Spread(Expr),
}

/// An operator applied to one expression.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UnaryOp {
    /// Numeric negation.
    Neg,
    /// Boolean negation.
    Not,
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Neg => "-",
            Self::Not => "!",
        })
    }
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

impl fmt::Display for BinOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::And => "&&",
            Self::Or => "||",
        })
    }
}
/// Literal text or an expression embedded in a parsed template.
#[derive(Debug, Clone, PartialEq)]
pub enum TemplatePart {
    /// Literal text copied into the result.
    Lit(String),
    /// A reference expression whose meaning depends on its context.
    Ref {
        /// The expression embedded in the template.
        expr: Expr,
        /// The expression’s byte range in the original source.
        range: SrcRange,
    },
}

impl Expr {
    /// Pairs an expression form with a source range without validating its meaning.
    pub fn new(kind: ExprKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
}
