#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub decls: Vec<Declaration>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Declaration {
    Block(Block),
    Attribute(Attribute),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: String,
    pub name: Option<String>,
    pub decls: Vec<Declaration>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub key: String,
    pub value: Expression,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    Str(String),
    Num(String),
    Bool(bool),
    Array(Vec<Expression>),
    Object(Vec<Attribute>),
}
