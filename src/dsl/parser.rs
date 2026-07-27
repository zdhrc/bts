use crate::dsl::lexer::{LexErr, Token, TokenKind, lex};

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

#[derive(Debug, Clone, PartialEq)]
pub enum DslErr {
    // lexing errors
    Lex(LexErr),
    UnexpectedToken { at: usize },

    // parsing errors
    ExpectedDeclaration { at: usize },
    ExpectedIdentifier { at: usize },
    ExpectedStringLiteral { at: usize },
    ExpectedNumberLiteral { at: usize },
    ExpectedExpressionAssignment { at: usize },
}

impl From<LexErr> for DslErr {
    fn from(err: LexErr) -> Self {
        DslErr::Lex(err)
    }
}

impl std::fmt::Display for DslErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DslErr::Lex(err) => {
                write!(f, "{err}")
            }
            DslErr::UnexpectedToken { at } => {
                write!(f, "unexpected token at byte {at}")
            }
            DslErr::ExpectedDeclaration { at } => {
                write!(f, "expected declaration at {at}")
            }
            DslErr::ExpectedIdentifier { at } => {
                write!(f, "expected identifier at {at}")
            }
            DslErr::ExpectedStringLiteral { at } => {
                write!(f, "expected string literal at {at}")
            }
            DslErr::ExpectedNumberLiteral { at } => {
                write!(f, "expected number literal at {at}")
            }
            DslErr::ExpectedExpressionAssignment { at } => {
                write!(f, "expected expression assignment at {at}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parser {
    pub tokens: Vec<Token>,
    pub cursor: usize,
}

impl Parser {
    pub fn parse(&mut self) -> Result<Source, DslErr> {
        Ok(self.parse_source()?)
    }

    fn parse_source(&mut self) -> Result<Source, DslErr> {
        let mut decls = Vec::new();

        while !self.eof() {
            decls.push(self.parse_decl()?);
        }

        Ok(Source { decls })
    }

    fn parse_decl(&mut self) -> Result<Declaration, DslErr> {
        let ident = self.expect_ident()?;

        match self.peek_kind() {
            TokenKind::StrLit(_) | TokenKind::LBrace => {
                self.parse_block_after_kind(ident).map(Declaration::Block)
            }
            TokenKind::Equals => self.parse_attr_after_key(ident).map(Declaration::Attribute),
            _ => Err(DslErr::ExpectedDeclaration { at: self.cursor }),
        }
    }

    fn parse_block_after_kind(&mut self, kind: String) -> Result<Block, DslErr> {
        let name = match self.peek_kind() {
            TokenKind::StrLit(_) => Some(self.expect_string()?),
            _ => None,
        };

        self.expect_lbrace()?;
        let mut decls = Vec::new();
        while !self.check_rbrace() {
            decls.push(self.parse_decl()?);
        }
        self.expect_rbrace()?;

        Ok(Block { kind, name, decls })
    }

    fn parse_attr_after_key(&mut self, key: String) -> Result<Attribute, DslErr> {
        self.expect_equals()?;

        let value = self.parse_expr()?;

        Ok(Attribute { key, value })
    }

    fn parse_expr(&mut self) -> Result<Expression, DslErr> {
        match self.peek_kind() {
            // literals
            TokenKind::StrLit(_) => Ok(Expression::Str(self.expect_string()?)),
            TokenKind::NumLit(_) => Ok(Expression::Num(self.expect_number()?)),

            // bools
            TokenKind::Ident(s) if s == "true" => {
                self.advance();
                Ok(Expression::Bool(true))
            }
            TokenKind::Ident(s) if s == "false" => {
                self.advance();
                Ok(Expression::Bool(false))
            }

            // objects and arrays
            TokenKind::LBrack => self.parse_array(),
            TokenKind::LBrace => self.parse_object(),

            // errors
            _ => Err(DslErr::ExpectedExpressionAssignment { at: self.cursor }),
        }
    }

    fn parse_array(&mut self) -> Result<Expression, DslErr> {
        self.expect_lbrack()?;

        let mut values = Vec::new();
        while !self.check_rbrack() {
            values.push(self.parse_expr()?);

            if self.check_comma() {
                self.advance();
            } else {
                break;
            }
        }
        self.expect_rbrack()?;

        Ok(Expression::Array(values))
    }

    fn parse_object(&mut self) -> Result<Expression, DslErr> {
        self.expect_lbrace()?;

        let mut attrs = Vec::new();
        while !self.check_rbrace() {
            let key = self.expect_ident()?;
            attrs.push(self.parse_attr_after_key(key)?);
        }
        self.expect_rbrace()?;

        Ok(Expression::Object(attrs))
    }

    // checks
    fn check_lbrace(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::LBrace)
    }
    fn check_rbrace(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::RBrace)
    }
    fn check_lbrack(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::LBrack)
    }
    fn check_rbrack(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::RBrack)
    }
    fn check_comma(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Comma)
    }

    // expects
    fn expect_lbrace(&mut self) -> Result<(), DslErr> {
        if matches!(self.peek_kind(), TokenKind::LBrace) {
            self.advance();
            Ok(())
        } else {
            Err(DslErr::UnexpectedToken { at: self.cursor })
        }
    }
    fn expect_rbrace(&mut self) -> Result<(), DslErr> {
        if matches!(self.peek_kind(), TokenKind::RBrace) {
            self.advance();
            Ok(())
        } else {
            Err(DslErr::UnexpectedToken { at: self.cursor })
        }
    }
    fn expect_lbrack(&mut self) -> Result<(), DslErr> {
        if matches!(self.peek_kind(), TokenKind::LBrack) {
            self.advance();
            Ok(())
        } else {
            Err(DslErr::UnexpectedToken { at: self.cursor })
        }
    }
    fn expect_rbrack(&mut self) -> Result<(), DslErr> {
        if matches!(self.peek_kind(), TokenKind::RBrack) {
            self.advance();
            Ok(())
        } else {
            Err(DslErr::UnexpectedToken { at: self.cursor })
        }
    }
    fn expect_equals(&mut self) -> Result<(), DslErr> {
        if matches!(self.peek_kind(), TokenKind::Equals) {
            self.advance();
            Ok(())
        } else {
            Err(DslErr::UnexpectedToken { at: self.cursor })
        }
    }
    fn expect_ident(&mut self) -> Result<String, DslErr> {
        match self.peek_kind() {
            TokenKind::Ident(value) => {
                let value = value.clone();
                self.advance();
                Ok(value)
            }
            _ => Err(DslErr::ExpectedIdentifier { at: self.cursor }),
        }
    }
    fn expect_string(&mut self) -> Result<String, DslErr> {
        match self.peek_kind() {
            TokenKind::StrLit(value) => {
                let value = value.clone();
                self.advance();
                Ok(value)
            }
            _ => Err(DslErr::ExpectedStringLiteral { at: self.cursor }),
        }
    }
    fn expect_number(&mut self) -> Result<String, DslErr> {
        match self.peek_kind() {
            TokenKind::NumLit(value) => {
                let value = value.clone();
                self.advance();
                Ok(value)
            }
            _ => Err(DslErr::ExpectedNumberLiteral { at: self.cursor }),
        }
    }

    // matches
    fn match_comma(&mut self) -> bool {
        if matches!(self.peek_kind(), TokenKind::Comma) {
            self.advance();
            true
        } else {
            false
        }
    }

    // helpers
    fn peek(&self) -> &Token {
        &self.tokens[self.cursor]
    }
    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }
    fn advance(&mut self) -> &Token {
        let token = &self.tokens[self.cursor];
        if token.kind != TokenKind::Eof {
            self.cursor += 1;
        }
        token
    }
    fn eof(&self) -> bool {
        matches!(self.peek_kind(), TokenKind::Eof)
    }
}

#[test]
fn debug() {
    let src = include_str!("../../tests/fixtures/simple.bt");
    let tokens = lex(src).unwrap();

    let mut parser = Parser {
        tokens: tokens,
        cursor: 0,
    };
    let doc = parser.parse().unwrap();
    dbg!(&doc);
}

// - parses_empty_doc
// - parses_named_block
// - parses_unnamed_block
// - parses_nested_blocks
// - parses_string_attr
// - parses_number_attr
// - parses_bool_attr
// - parses_array_attr
// - parses_empty_array
// - parses_object_attr
// - parses_empty_object
// - parses_multiple_top_level_items
// - rejects_missing_block_open_brace
// - rejects_missing_block_close_brace
// - rejects_missing_attr_equals
// - rejects_missing_attr_value
// - rejects_unexpected_top_level_value
// - rejects_trailing_comma_in_array
// - rejects_unclosed_array
// - rejects_unclosed_object
