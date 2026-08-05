use crate::dsl::diag::SrcRange;

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
    Num(String),
    Bool(bool),
    Null,
    Array(Vec<Expr>),
    Object(Vec<Attr>),
}

impl Expr {
    pub fn new(kind: ExprKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
}
