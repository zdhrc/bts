use crate::dsl::diag::SrcRange;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct Ast {
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Block(Block),
    Attr(Attr),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: String,
    pub name: Option<String>,
    pub decls: Vec<Decl>,
    pub range: SrcRange,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attr {
    pub key: String,
    pub value: Expr,
    pub range: SrcRange,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub range: SrcRange,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Str(String),
    Template(Vec<TemplatePart>),
    Num(String),
    Bool(bool),
    Null,
    Array(Vec<Expr>),
    Object(Vec<Attr>),
    VarRef(String),
    Func {
        name: String,
        args: Vec<Expr>,
    },
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Cond {
        cond: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UnaryOp {
    Neg,
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

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum BinOp {
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TemplatePart {
    Lit(String),
    Ref { path: Vec<String>, range: SrcRange },
}

impl Expr {
    pub fn new(kind: ExprKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
}
