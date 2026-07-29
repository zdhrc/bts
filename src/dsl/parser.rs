use crate::dsl::lexer::{Token, TokenKind};
use crate::dsl::syntax::{Attribute, Block, Declaration, Expression, Source};

macro_rules! token {
    ($variant:ident) => {
        |kind| matches!(kind, TokenKind::$variant).then_some(())
    };

    ($variant:ident($value:ident)) => {
        |kind| match kind {
            TokenKind::$variant($value) => Some($value.clone()),
            _ => None,
        }
    };
}

#[derive(Debug, Clone, PartialEq)]
pub struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens: tokens, cursor: 0 }
    }

    pub fn parse(&mut self) -> Result<Source> {
        let mut decls = Vec::new();

        while !self.eof() {
            let ident = self.expect(token!(Ident(value)), |at| Error::ExpectedIdentifier { at })?;
            let decl = match &self.peek().kind {
                TokenKind::StrLit(_) | TokenKind::LBrace => Declaration::Block(self.parse_block(ident)?),
                TokenKind::Equals => Declaration::Attribute(self.parse_attribute(ident)?),
                _ => return Err(Error::ExpectedDeclaration { at: self.cursor }),
            };
            decls.push(decl);
        }

        Ok(Source { decls })
    }

    fn parse_block(&mut self, kind: String) -> Result<Block> {
        let name = self.consume(token!(StrLit(value)));
        self.expect(token!(LBrace), |at| Error::UnexpectedToken { at })?;

        let mut decls = Vec::new();
        while !self.check(token!(RBrace)) {
            let ident = self.expect(token!(Ident(value)), |at| Error::ExpectedIdentifier { at })?;
            let decl = match &self.peek().kind {
                TokenKind::StrLit(_) | TokenKind::LBrace => Declaration::Block(self.parse_block(ident)?),
                TokenKind::Equals => Declaration::Attribute(self.parse_attribute(ident)?),
                _ => return Err(Error::ExpectedDeclaration { at: self.cursor }),
            };
            decls.push(decl)
        }
        self.expect(token!(RBrace), |at| Error::UnexpectedToken { at })?;

        Ok(Block { kind, name, decls })
    }

    fn parse_attribute(&mut self, key: String) -> Result<Attribute> {
        self.expect(token!(Equals), |at| Error::UnexpectedToken { at })?;
        let value = self.parse_expression()?;
        Ok(Attribute { key, value })
    }

    fn parse_expression(&mut self) -> Result<Expression> {
        match &self.peek().kind {
            TokenKind::StrLit(_) => {
                let value = self.expect(token!(StrLit(value)), |at| Error::ExpectedStringLiteral { at })?;
                Ok(Expression::Str(value))
            }
            TokenKind::NumLit(_) => {
                let value = self.expect(token!(NumLit(value)), |at| Error::ExpectedNumberLiteral { at })?;
                Ok(Expression::Num(value))
            }
            TokenKind::Ident(value) => {
                if value == "true" {
                    self.next();
                    Ok(Expression::Bool(true))
                } else {
                    self.next();
                    Ok(Expression::Bool(false))
                }
            }
            TokenKind::LBrack => {
                self.expect(token!(LBrack), |at| Error::UnexpectedToken { at })?;

                let mut values = Vec::new();
                while !self.check(token!(RBrack)) {
                    values.push(self.parse_expression()?);

                    if self.consume(token!(Comma)).is_none() {
                        break;
                    }
                }
                self.expect(token!(RBrack), |at| Error::UnexpectedToken { at })?;

                Ok(Expression::Array(values))
            }
            TokenKind::LBrace => {
                self.expect(token!(LBrace), |at| Error::UnexpectedToken { at })?;

                let mut attrs = Vec::new();
                while !self.check(token!(RBrace)) {
                    let key = self.expect(token!(Ident(value)), |at| Error::UnexpectedToken { at })?;

                    attrs.push(self.parse_attribute(key)?);
                }
                self.expect(token!(RBrace), |at| Error::UnexpectedToken { at })?;

                Ok(Expression::Object(attrs))
            }
            _ => Err(Error::ExpectedExpressionAssignment { at: self.cursor }),
        }
    }

    fn check<T>(&self, matcher: impl FnOnce(&TokenKind) -> Option<T>) -> bool {
        matcher(&self.peek().kind).is_some()
    }

    fn consume<T>(&mut self, matcher: impl FnOnce(&TokenKind) -> Option<T>) -> Option<T> {
        let value = matcher(&self.peek().kind)?;
        self.next();
        Some(value)
    }

    fn expect<T>(&mut self, matcher: impl FnOnce(&TokenKind) -> Option<T>, err: impl FnOnce(usize) -> Error) -> Result<T> {
        let at = self.cursor;
        self.consume(matcher).ok_or_else(|| err(at))
    }

    // helpers
    fn peek(&self) -> &Token {
        &self.tokens[self.cursor]
    }
    fn next(&mut self) -> &Token {
        let token = &self.tokens[self.cursor];
        if token.kind != TokenKind::Eof {
            self.cursor += 1;
        }
        token
    }
    fn eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    UnexpectedToken { at: usize },
    ExpectedDeclaration { at: usize },
    ExpectedIdentifier { at: usize },
    ExpectedStringLiteral { at: usize },
    ExpectedNumberLiteral { at: usize },
    ExpectedExpressionAssignment { at: usize },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnexpectedToken { at } => {
                write!(f, "unexpected token at {at}")
            }
            Error::ExpectedDeclaration { at } => {
                write!(f, "expected declaration at {at}")
            }
            Error::ExpectedIdentifier { at } => {
                write!(f, "expected identifier at {at}")
            }
            Error::ExpectedStringLiteral { at } => {
                write!(f, "expected string literal at {at}")
            }
            Error::ExpectedNumberLiteral { at } => {
                write!(f, "expected number literal at {at}")
            }
            Error::ExpectedExpressionAssignment { at } => {
                write!(f, "expected expression assignment at {at}")
            }
        }
    }
}

impl std::error::Error for Error {}

#[test]
fn debug() {
    let src = include_str!("../../tests/fixtures/simple.bt");
    let tokens = crate::dsl::lexer::Lexer::new(src).lex().unwrap();

    let mut parser = Parser { tokens: tokens, cursor: 0 };
    let source = parser.parse().unwrap();
    dbg!(&source);
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
