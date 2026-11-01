use crate::diag::SrcRange;
use std::fmt;

/// The parsed declarations of a module, before semantic validation.
#[derive(Debug, Clone, PartialEq)]
pub struct Ast {
    /// Declarations in source order.
    pub decls: Vec<Decl>,
}

/// A block or field assignment as written in source.
#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    /// A nested block declaration.
    Block(Block),
    /// A named field assignment.
    Attr(Attr),
}

/// A parsed block whose keyword, fields, and placement have not been validated.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The block keyword as written, without checking whether it is supported.
    pub kind: String,
    /// The name written in source.
    pub name: Option<String>,
    /// The name literal’s source range, if the block is named.
    pub name_range: Option<SrcRange>,
    /// Declarations in source order.
    pub decls: Vec<Decl>,
    /// The expression’s byte range in the original source.
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
