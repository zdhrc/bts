use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::lexer::{Token, TokenKind, Tokens};
use crate::dsl::syntax::{Ast, Attr, Block, Decl, Expr, ExprKind};
use thiserror::Error as Err;

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
struct Parser {
    tokens: Tokens,
    errors: Errors,
    index: usize,
}

impl Parser {
    fn new(tokens: Tokens) -> Self {
        Self {
            tokens: tokens,
            errors: Vec::new(),
            index: 0,
        }
    }

    fn parse(mut self) -> Result<Ast, Errors> {
        let mut decls = Vec::new();

        while !self.eof() {
            if let Some(decl) = self.parse_decl() {
                decls.push(decl);
            }
        }

        if self.errors.is_empty() { Ok(Ast { decls }) } else { Err(self.errors) }
    }

    fn parse_decl(&mut self) -> Option<Decl> {
        let range = self.peek().range;
        let Some(ident) = self.expect(token!(Ident(value)), ErrorKind::ExpectedIdentifier) else {
            self.skip_declaration();
            return None;
        };

        let decl = match &self.peek().kind {
            TokenKind::String(_) | TokenKind::LBrace => self.parse_block(ident, range).map(Decl::Block),
            TokenKind::Equals => self.parse_attr(ident, range).map(Decl::Attr),
            _ => {
                self.errors.push(Error::new(ErrorKind::ExpectedDeclaration, self.peek().range.clone()));
                None
            }
        };

        if decl.is_none() {
            self.skip_declaration();
        }

        decl
    }

    fn parse_block(&mut self, kind: String, range: SrcRange) -> Option<Block> {
        let name = self.consume(token!(String(value)));
        self.expect(token!(LBrace), ErrorKind::UnexpectedToken)?;

        let mut decls = Vec::new();

        while !self.check(token!(RBrace)) && !self.eof() {
            if let Some(decl) = self.parse_decl() {
                decls.push(decl);
            }
        }

        self.expect(token!(RBrace), ErrorKind::UnexpectedToken)?;

        Some(Block { kind, name, decls, range })
    }

    fn parse_attr(&mut self, key: String, range: SrcRange) -> Option<Attr> {
        self.expect(token!(Equals), ErrorKind::UnexpectedToken)?;
        let value = self.parse_expr()?;

        Some(Attr { key, value, range })
    }

    fn parse_expr(&mut self) -> Option<Expr> {
        match &self.peek().kind {
            TokenKind::String(_) => {
                let range = self.peek().range;
                let value = self.expect(token!(String(value)), ErrorKind::ExpectedStringLiteral)?;
                Some(Expr::new(ExprKind::Str(value), range))
            }
            TokenKind::Number(_) => {
                let range = self.peek().range;
                let value = self.expect(token!(Number(value)), ErrorKind::ExpectedNumberLiteral)?;
                Some(Expr::new(ExprKind::Num(value), range))
            }
            TokenKind::Ident(value) => {
                let range = self.peek().range;
                if value == "true" {
                    self.next();
                    Some(Expr::new(ExprKind::Bool(true), range))
                } else if value == "false" {
                    self.next();
                    Some(Expr::new(ExprKind::Bool(false), range))
                } else {
                    self.errors.push(Error::new(ErrorKind::UnexpectedToken, self.peek().range.clone()));
                    None
                }
            }
            TokenKind::LBrack => {
                let start = self.peek().range.start;
                self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;

                let mut values = Vec::new();
                while !self.check(token!(RBrack)) {
                    values.push(self.parse_expr()?);

                    if self.consume(token!(Comma)).is_none() {
                        break;
                    }
                }
                let end = self.peek().range.end;
                self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;

                Some(Expr::new(ExprKind::Array(values), SrcRange::new(start, end)))
            }
            TokenKind::LBrace => {
                let start = self.peek().range.start;
                self.expect(token!(LBrace), ErrorKind::UnexpectedToken)?;

                let mut attrs = Vec::new();
                while !self.check(token!(RBrace)) {
                    let range = self.peek().range;
                    let key = self.expect(token!(Ident(value)), ErrorKind::UnexpectedToken)?;

                    attrs.push(self.parse_attr(key, range)?);
                }
                let end = self.peek().range.end;
                self.expect(token!(RBrace), ErrorKind::UnexpectedToken)?;

                Some(Expr::new(ExprKind::Object(attrs), SrcRange::new(start, end)))
            }
            _ => {
                self.errors.push(Error::new(ErrorKind::ExpectedExpressionAssignment, self.peek().range.clone()));
                None
            }
        }
    }

    fn skip_declaration(&mut self) {
        while !self.eof() {
            match self.peek().kind {
                TokenKind::Ident(_) | TokenKind::RBrace => break,
                _ => {
                    self.next();
                }
            }
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

    fn expect<T>(&mut self, matcher: impl FnOnce(&TokenKind) -> Option<T>, kind: ErrorKind) -> Option<T> {
        if let Some(value) = self.consume(matcher) {
            Some(value)
        } else {
            self.errors.push(Error::new(kind, self.peek().range.clone()));
            None
        }
        // let at = self.index;
        // self.consume(matcher).ok_or_else(|| err(at))
    }

    // helpers
    fn peek(&self) -> &Token {
        &self.tokens[self.index]
    }
    fn next(&mut self) -> &Token {
        let token = &self.tokens[self.index];
        if token.kind != TokenKind::Eof {
            self.index += 1;
        }
        token
    }
    fn eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }
}

pub(super) fn parse(tokens: Vec<Token>) -> Result<Ast, Diags> {
    Parser::new(tokens).parse().map_err(|errors| errors.into_iter().map(Diag::from).collect())
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
#[error("{kind}")]
pub(super) struct Error {
    kind: ErrorKind,
    range: SrcRange,
}

pub(super) type Errors = Vec<Error>;

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
pub(super) enum ErrorKind {
    #[error("unknown token")]
    UnexpectedToken,
    #[error("expected declaration")]
    ExpectedDeclaration,
    #[error("expected identifier")]
    ExpectedIdentifier,
    #[error("expected string")]
    ExpectedStringLiteral,
    #[error("expected number")]
    ExpectedNumberLiteral,
    #[error("expected expression assignment")]
    ExpectedExpressionAssignment,
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
    fn kind(&self) -> ErrorKind {
        self.kind
    }
    fn range(&self) -> SrcRange {
        self.range
    }
}

impl From<Error> for Diag {
    fn from(error: Error) -> Self {
        let Error { kind, range } = error;

        Diag {
            when: DiagPhase::Parsing,
            what: kind.to_string(),
            r#where: range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug() {
        let src = include_str!("../../tests/fixtures/simple.bt");
        let tokens = crate::dsl::lexer::lex(src).unwrap();
        let source = Parser::new(tokens).parse().unwrap();
        dbg!(&source);
    }
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
