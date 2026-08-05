use crate::dsl::ast::{Ast, Attr, BinOp, Block, Decl, Expr, ExprKind, UnaryOp};
use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::lexer::{Token, TokenKind, Tokens};
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
            tokens,
            errors: Vec::new(),
            index: 0,
        }
    }

    fn parse(mut self) -> Result<Ast, Errors> {
        let mut decls = Vec::new();

        while !self.eof() {
            let before = self.index;
            if let Some(decl) = self.parse_decl() {
                decls.push(decl);
            } else if self.index == before {
                // recovery stopped without consuming (eg a stray } at the root), force progress
                self.next();
            }
        }

        if self.errors.is_empty() {
            Ok(Ast { decls })
        } else {
            Err(self.errors)
        }
    }

    fn parse_decl(&mut self) -> Option<Decl> {
        let range = self.peek().range;
        let Some(ident) = self.expect(token!(Ident(value)), ErrorKind::ExpectedIdentifier) else {
            self.skip_declaration();
            return None;
        };

        let decl = match &self.peek().kind {
            TokenKind::String(_) | TokenKind::Template(_) | TokenKind::LBrace => {
                self.parse_block(ident, range).map(Decl::Block)
            }
            TokenKind::Equals => self.parse_attr(ident, range).map(Decl::Attr),
            _ => {
                self.errors
                    .push(Error::new(ErrorKind::ExpectedDeclaration, self.peek().range));
                None
            }
        };

        if decl.is_none() {
            self.skip_declaration();
        }

        decl
    }

    fn parse_block(&mut self, kind: String, range: SrcRange) -> Option<Block> {
        if self.check(token!(Template(value))) {
            self.errors.push(Error::new(ErrorKind::InterpolatedName, self.peek().range));
            self.next();
        }

        let name = self.consume(token!(String(value)));
        self.expect(token!(LBrace), ErrorKind::UnexpectedToken)?;

        let mut decls = Vec::new();

        while !self.check(token!(RBrace)) && !self.eof() {
            if let Some(decl) = self.parse_decl() {
                decls.push(decl);
            }
        }

        self.expect(token!(RBrace), ErrorKind::UnexpectedToken)?;

        Some(Block {
            kind,
            name,
            decls,
            range,
        })
    }

    fn parse_attr(&mut self, key: String, range: SrcRange) -> Option<Attr> {
        self.expect(token!(Equals), ErrorKind::UnexpectedToken)?;
        let value = self.parse_expr()?;

        Some(Attr { key, value, range })
    }

    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_conditional()
    }

    // cond ? then : otherwise, right associative
    fn parse_conditional(&mut self) -> Option<Expr> {
        let cond = self.parse_binary(1)?;

        if self.consume(token!(Question)).is_none() {
            return Some(cond);
        }

        let then = self.parse_expr()?;
        self.expect(token!(Colon), ErrorKind::ExpectedTernaryColon)?;
        let otherwise = self.parse_conditional()?;

        let range = SrcRange::new(cond.range.start, otherwise.range.end);
        Some(Expr::new(
            ExprKind::Cond {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            },
            range,
        ))
    }

    // left associative binary operators via precedence climbing
    fn parse_binary(&mut self, min_level: u8) -> Option<Expr> {
        let mut lhs = self.parse_unary()?;

        while let Some((op, level)) = bin_op(&self.peek().kind) {
            if level < min_level {
                break;
            }
            self.next();

            let rhs = self.parse_binary(level + 1)?;
            let range = SrcRange::new(lhs.range.start, rhs.range.end);
            lhs = Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                range,
            );
        }

        Some(lhs)
    }

    fn parse_unary(&mut self) -> Option<Expr> {
        let range = self.peek().range;
        let op = match &self.peek().kind {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            _ => return self.parse_postfix(),
        };
        self.next();

        let operand = self.parse_unary()?;
        let range = SrcRange::new(range.start, operand.range.end);
        Some(Expr::new(
            ExprKind::Unary {
                op,
                operand: Box::new(operand),
            },
            range,
        ))
    }

    // x[i] and x.f, .f sugars to ["f"]
    fn parse_postfix(&mut self) -> Option<Expr> {
        let mut expr = self.parse_primary()?;

        loop {
            match &self.peek().kind {
                TokenKind::LBrack => {
                    self.next();
                    let index = self.parse_expr()?;
                    let end = self.peek().range.end;
                    self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;

                    let range = SrcRange::new(expr.range.start, end);
                    expr = Expr::new(
                        ExprKind::Index {
                            target: Box::new(expr),
                            index: Box::new(index),
                        },
                        range,
                    );
                }
                TokenKind::Dot => {
                    self.next();
                    let name_range = self.peek().range;
                    let name = self.expect(token!(Ident(value)), ErrorKind::ExpectedAccessorField)?;

                    let range = SrcRange::new(expr.range.start, name_range.end);
                    expr = Expr::new(
                        ExprKind::Index {
                            target: Box::new(expr),
                            index: Box::new(Expr::new(ExprKind::Str(name), name_range)),
                        },
                        range,
                    );
                }
                _ => break,
            }
        }

        Some(expr)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        match &self.peek().kind {
            TokenKind::LParen => {
                let start = self.peek().range.start;
                self.next();

                let inner = self.parse_expr()?;
                let end = self.peek().range.end;
                self.expect(token!(RParen), ErrorKind::UnexpectedToken)?;

                // no ast node for grouping, the widened range keeps diagnostics on the parens
                Some(Expr::new(inner.kind, SrcRange::new(start, end)))
            }
            TokenKind::String(_) => {
                let range = self.peek().range;
                let value = self.expect(token!(String(value)), ErrorKind::ExpectedStringLiteral)?;
                Some(Expr::new(ExprKind::Str(value), range))
            }
            TokenKind::Template(_) => {
                let range = self.peek().range;
                let value = self.expect(token!(Template(value)), ErrorKind::ExpectedStringLiteral)?;
                Some(Expr::new(ExprKind::Template(value), range))
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
                } else if value == "null" {
                    self.next();
                    Some(Expr::new(ExprKind::Null, range))
                } else if value == "var" {
                    self.next();
                    self.expect(token!(Dot), ErrorKind::ExpectedVariableName)?;
                    let name_range = self.peek().range;
                    let name = self.expect(token!(Ident(value)), ErrorKind::ExpectedVariableName)?;
                    Some(Expr::new(ExprKind::VarRef(name), SrcRange::new(range.start, name_range.end)))
                } else if matches!(self.peek_ahead().kind, TokenKind::LParen) {
                    let name = value.clone();
                    self.next();
                    self.next();

                    let mut args = Vec::new();
                    while !self.check(token!(RParen)) {
                        args.push(self.parse_expr()?);

                        if self.consume(token!(Comma)).is_none() {
                            break;
                        }
                    }
                    let end = self.peek().range.end;
                    self.expect(token!(RParen), ErrorKind::UnexpectedToken)?;

                    Some(Expr::new(ExprKind::Func { name, args }, SrcRange::new(range.start, end)))
                } else {
                    self.errors.push(Error::new(ErrorKind::UnexpectedToken, self.peek().range));
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
                self.errors
                    .push(Error::new(ErrorKind::ExpectedExpressionAssignment, self.peek().range));
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
            self.errors.push(Error::new(kind, self.peek().range));
            None
        }
    }

    // helpers
    fn peek(&self) -> &Token {
        &self.tokens[self.index]
    }
    fn peek_ahead(&self) -> &Token {
        self.tokens.get(self.index + 1).unwrap_or(&self.tokens[self.tokens.len() - 1])
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

// binding levels, higher binds tighter; unary and primary sit above all of these
fn bin_op(kind: &TokenKind) -> Option<(BinOp, u8)> {
    match kind {
        TokenKind::PipePipe => Some((BinOp::Or, 1)),
        TokenKind::AmpAmp => Some((BinOp::And, 2)),
        TokenKind::EqEq => Some((BinOp::Eq, 3)),
        TokenKind::NotEq => Some((BinOp::Ne, 3)),
        TokenKind::Lt => Some((BinOp::Lt, 4)),
        TokenKind::LtEq => Some((BinOp::Le, 4)),
        TokenKind::Gt => Some((BinOp::Gt, 4)),
        TokenKind::GtEq => Some((BinOp::Ge, 4)),
        TokenKind::Plus => Some((BinOp::Add, 5)),
        TokenKind::Minus => Some((BinOp::Sub, 5)),
        TokenKind::Star => Some((BinOp::Mul, 6)),
        TokenKind::Slash => Some((BinOp::Div, 6)),
        TokenKind::Percent => Some((BinOp::Rem, 6)),
        _ => None,
    }
}

pub(super) fn parse(tokens: Vec<Token>) -> Result<Ast, Diags> {
    Parser::new(tokens)
        .parse()
        .map_err(|errors| errors.into_iter().map(Diag::from).collect())
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
    #[error("block names do not support interpolation")]
    InterpolatedName,
    #[error("expected variable name")]
    ExpectedVariableName,
    #[error("expected field name after `.`")]
    ExpectedAccessorField,
    #[error("expected `:` in conditional expression")]
    ExpectedTernaryColon,
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
    #[cfg(test)]
    fn kind(&self) -> ErrorKind {
        self.kind
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

    fn parse(src: &str) -> Result<Ast, Errors> {
        Parser::new(crate::dsl::lexer::lex(src).unwrap()).parse()
    }

    #[track_caller]
    fn assert_error_kinds(src: &str, want: &[ErrorKind]) {
        let errors = parse(src).unwrap_err();
        let got: Vec<_> = errors.iter().map(Error::kind).collect();

        assert_eq!(got, want);
    }

    fn block(decl: &Decl) -> &Block {
        match decl {
            Decl::Block(block) => block,
            Decl::Attr(attr) => panic!("expected a block, found attribute `{}`", attr.key),
        }
    }

    fn attr(decl: &Decl) -> &Attr {
        match decl {
            Decl::Attr(attr) => attr,
            Decl::Block(block) => panic!("expected an attribute, found block `{}`", block.kind),
        }
    }

    #[test]
    fn parses_empty_docs() {
        assert!(parse("").unwrap().decls.is_empty());
        assert!(parse("  \n\t ").unwrap().decls.is_empty());
    }

    #[test]
    fn parses_named_and_unnamed_blocks() {
        let ast = parse(r#"trace "a" {} group {}"#).unwrap();

        assert_eq!(ast.decls.len(), 2);
        let named = block(&ast.decls[0]);
        assert_eq!(named.kind, "trace");
        assert_eq!(named.name.as_deref(), Some("a"));
        assert!(named.decls.is_empty());
        let unnamed = block(&ast.decls[1]);
        assert_eq!(unnamed.kind, "group");
        assert_eq!(unnamed.name, None);
    }

    #[test]
    fn parses_nested_blocks() {
        let ast = parse(r#"trace "a" { task "b" { llm "c" {} } }"#).unwrap();

        let trace = block(&ast.decls[0]);
        let task = block(&trace.decls[0]);
        let llm = block(&task.decls[0]);
        assert_eq!((task.kind.as_str(), llm.kind.as_str()), ("task", "llm"));
        assert!(llm.decls.is_empty());
    }

    #[test]
    fn parses_scalar_attrs() {
        let ast = parse(r#"trace "a" { input = "hi" count = 4.5 flag = true }"#).unwrap();
        let trace = block(&ast.decls[0]);

        assert!(matches!(&attr(&trace.decls[0]).value.kind, ExprKind::Str(value) if value == "hi"));
        assert!(matches!(&attr(&trace.decls[1]).value.kind, ExprKind::Num(value) if value == "4.5"));
        assert!(matches!(attr(&trace.decls[2]).value.kind, ExprKind::Bool(true)));
    }

    #[test]
    fn parses_null_and_negative_number_attrs() {
        let ast = parse(r#"trace "a" { output = null delta = -0.5 items = [null, -1] }"#).unwrap();
        let trace = block(&ast.decls[0]);

        assert!(matches!(attr(&trace.decls[0]).value.kind, ExprKind::Null));

        // a negative literal is unary negation now
        let ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } = &attr(&trace.decls[1]).value.kind
        else {
            panic!("expected unary negation");
        };
        assert!(matches!(&operand.kind, ExprKind::Num(value) if value == "0.5"));

        let ExprKind::Array(items) = &attr(&trace.decls[2]).value.kind else {
            panic!("expected an array");
        };
        assert!(matches!(items[0].kind, ExprKind::Null));
        let ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } = &items[1].kind
        else {
            panic!("expected unary negation");
        };
        assert!(matches!(&operand.kind, ExprKind::Num(value) if value == "1"));
    }

    #[test]
    fn rejects_bare_identifiers_that_are_not_keyword_literals() {
        // the stray `nil` is re-scanned as a declaration during recovery, adding a second error
        assert_error_kinds(
            r#"trace "a" { output = nil }"#,
            &[ErrorKind::UnexpectedToken, ErrorKind::ExpectedDeclaration],
        );
    }

    #[test]
    fn parses_template_expressions() {
        use crate::dsl::ast::TemplatePart;

        let ast = parse(r#"trace "a" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let trace = block(&ast.decls[0]);

        let ExprKind::Template(parts) = &attr(&trace.decls[0]).value.kind else {
            panic!("expected a template");
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], TemplatePart::Lit(value) if value == "q "));
        assert!(matches!(&parts[1], TemplatePart::Ref { path, .. } if path == &["trace", "index"]));

        let ExprKind::Array(tags) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an array");
        };
        assert!(matches!(&tags[0].kind, ExprKind::Template(parts) if parts.len() == 2));
    }

    #[test]
    fn rejects_interpolated_block_names() {
        assert_error_kinds(r#"trace "${trace.index}" {}"#, &[ErrorKind::InterpolatedName]);
    }

    #[test]
    fn parses_variable_references() {
        let source = r#"trace "a" { metadata = var.meta output = [var.x, 1] }"#;
        let ast = parse(source).unwrap();
        let trace = block(&ast.decls[0]);

        let metadata = attr(&trace.decls[0]);
        assert!(matches!(&metadata.value.kind, ExprKind::VarRef(name) if name == "meta"));
        let range = metadata.value.range;
        assert_eq!(&source[range.start..range.end], "var.meta");

        let ExprKind::Array(items) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an array");
        };
        assert!(matches!(&items[0].kind, ExprKind::VarRef(name) if name == "x"));
    }

    #[test]
    fn parses_func_exprs() {
        let source = r#"trace "a" { input = choice("x", range(1, 2),) }"#;
        let ast = parse(source).unwrap();
        let trace = block(&ast.decls[0]);

        let input = attr(&trace.decls[0]);
        let ExprKind::Func { name, args } = &input.value.kind else {
            panic!("expected a func");
        };
        assert_eq!(name, "choice");
        assert!(matches!(&args[0].kind, ExprKind::Str(value) if value == "x"));
        let ExprKind::Func { name, args } = &args[1].kind else {
            panic!("expected a nested func");
        };
        assert_eq!(name, "range");
        assert_eq!(args.len(), 2);

        let range = input.value.range;
        assert_eq!(&source[range.start..range.end], r#"choice("x", range(1, 2),)"#);
    }

    #[test]
    fn parses_funcs_without_args() {
        let ast = parse(r#"trace "a" { input = uuid() }"#).unwrap();
        let trace = block(&ast.decls[0]);

        assert!(matches!(
            &attr(&trace.decls[0]).value.kind,
            ExprKind::Func { name, args } if name == "uuid" && args.is_empty()
        ));
    }

    #[test]
    fn rejects_unclosed_func_args() {
        assert_error_kinds(r#"trace "a" { input = choice("x" }"#, &[ErrorKind::UnexpectedToken]);
    }

    #[test]
    fn rejects_incomplete_variable_references() {
        assert_error_kinds(r#"trace "a" { metadata = var }"#, &[ErrorKind::ExpectedVariableName]);
        assert_error_kinds(r#"trace "a" { metadata = var. }"#, &[ErrorKind::ExpectedVariableName]);
    }

    #[test]
    fn parses_chained_index_and_accessor_exprs() {
        let source = r#"trace "a" { input = var.xs[0]["k"].name }"#;
        let ast = parse(source).unwrap();
        let input = attr(&block(&ast.decls[0]).decls[0]);

        // .name is the outermost selection
        let ExprKind::Index { target, index } = &input.value.kind else {
            panic!("expected an index expression");
        };
        assert!(matches!(&index.kind, ExprKind::Str(value) if value == "name"));

        let ExprKind::Index { target, index } = &target.kind else {
            panic!("expected a nested index expression");
        };
        assert!(matches!(&index.kind, ExprKind::Str(value) if value == "k"));

        let ExprKind::Index { target, index } = &target.kind else {
            panic!("expected a nested index expression");
        };
        assert!(matches!(&index.kind, ExprKind::Num(value) if value == "0"));
        assert!(matches!(&target.kind, ExprKind::VarRef(name) if name == "xs"));

        let range = input.value.range;
        assert_eq!(&source[range.start..range.end], r#"var.xs[0]["k"].name"#);
    }

    #[test]
    fn parses_expressions_as_indexes() {
        let expr = parse_value("var.xs[choice(0, 1) + 1]");

        let ExprKind::Index { index, .. } = &expr.kind else {
            panic!("expected an index expression");
        };
        assert!(matches!(index.kind, ExprKind::Binary { op: BinOp::Add, .. }));
    }

    #[test]
    fn parses_postfix_tighter_than_unary() {
        let expr = parse_value("-var.xs[0]");

        let ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } = &expr.kind
        else {
            panic!("expected unary negation");
        };
        assert!(matches!(operand.kind, ExprKind::Index { .. }));
    }

    #[test]
    fn parses_indexes_on_literals_and_calls() {
        assert!(matches!(parse_value("[1, 2][0]").kind, ExprKind::Index { .. }));
        assert!(matches!(parse_value("{ a = 1 }.a").kind, ExprKind::Index { .. }));
        assert!(matches!(parse_value("choice([1], [2])[0]").kind, ExprKind::Index { .. }));
        assert!(matches!(parse_value("(var.xs)[0]").kind, ExprKind::Index { .. }));
    }

    #[test]
    fn rejects_unterminated_index_exprs() {
        assert_error_kinds(r#"trace "a" { input = var.xs[0 }"#, &[ErrorKind::UnexpectedToken]);
    }

    #[test]
    fn rejects_accessors_without_a_field_name() {
        assert_error_kinds(r#"trace "a" { input = var.xs. }"#, &[ErrorKind::ExpectedAccessorField]);
        assert_error_kinds(r#"trace "a" { input = var.xs.[0] }"#, &[ErrorKind::ExpectedAccessorField]);
    }

    #[test]
    fn parses_array_attrs_with_optional_trailing_comma() {
        let ast = parse(r#"trace "a" { empty = [] tags = ["x", "y",] }"#).unwrap();
        let trace = block(&ast.decls[0]);

        let ExprKind::Array(empty) = &attr(&trace.decls[0]).value.kind else {
            panic!("expected an array");
        };
        assert!(empty.is_empty());

        let ExprKind::Array(tags) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an array");
        };
        assert_eq!(tags.len(), 2);
    }

    #[test]
    fn parses_object_attrs() {
        let ast = parse(r#"trace "a" { empty = {} meta = { model = "gpt" nested = { x = 1 } } }"#).unwrap();
        let trace = block(&ast.decls[0]);

        let ExprKind::Object(empty) = &attr(&trace.decls[0]).value.kind else {
            panic!("expected an object");
        };
        assert!(empty.is_empty());

        let ExprKind::Object(meta) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an object");
        };
        assert_eq!(meta.len(), 2);
        assert_eq!(meta[0].key, "model");
        assert!(matches!(&meta[1].value.kind, ExprKind::Object(nested) if nested.len() == 1));
    }

    #[test]
    fn parses_the_fixture() {
        let ast = parse(include_str!("../../tests/fixtures/simple.bt")).unwrap();

        assert_eq!(ast.decls.len(), 2);
        let vars = block(&ast.decls[0]);
        assert_eq!(vars.kind, "vars");
        assert_eq!(vars.name, None);
        assert_eq!(vars.decls.len(), 2);
        let trace = block(&ast.decls[1]);
        assert_eq!(trace.kind, "trace");
        assert_eq!(trace.name.as_deref(), Some("multi-turn-conversation"));
        assert_eq!(trace.decls.len(), 4);
    }

    #[test]
    fn rejects_a_block_missing_its_open_brace() {
        assert_error_kinds(r#"trace "a""#, &[ErrorKind::UnexpectedToken]);
    }

    #[test]
    fn rejects_a_block_missing_its_close_brace() {
        assert_error_kinds(r#"trace "a" { input = "x" "#, &[ErrorKind::UnexpectedToken]);
    }

    #[test]
    fn rejects_an_attr_missing_its_value() {
        assert_error_kinds("input =", &[ErrorKind::ExpectedExpressionAssignment]);
    }

    #[test]
    fn rejects_values_at_the_top_level() {
        assert_error_kinds("5", &[ErrorKind::ExpectedIdentifier]);
        assert_error_kinds(r#""str""#, &[ErrorKind::ExpectedIdentifier]);
    }

    #[test]
    fn rejects_declarations_without_a_body_or_value() {
        assert_error_kinds("foo bar", &[ErrorKind::ExpectedDeclaration, ErrorKind::ExpectedDeclaration]);
    }

    #[test]
    fn rejects_unclosed_arrays_and_objects() {
        assert_error_kinds(r#"trace "a" { tags = ["x" }"#, &[ErrorKind::UnexpectedToken]);
        assert_error_kinds(
            r#"trace "a" { meta = { x = 1 "#,
            &[ErrorKind::UnexpectedToken, ErrorKind::UnexpectedToken],
        );
    }

    #[test]
    fn recovers_from_stray_closing_brace_at_the_root() {
        let errors = parse("}").unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind(), ErrorKind::ExpectedIdentifier);

        let errors = parse(r#"trace "a" {} }"#).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].kind(), ErrorKind::ExpectedIdentifier);
    }

    #[test]
    fn continues_parsing_declarations_after_a_stray_closing_brace() {
        let errors = parse(r#"} trace "a" { input = }"#).unwrap_err();

        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].kind(), ErrorKind::ExpectedIdentifier);
        assert_eq!(errors[1].kind(), ErrorKind::ExpectedExpressionAssignment);
    }

    #[track_caller]
    fn parse_value(source: &str) -> Expr {
        let source = format!(r#"trace "t" {{ input = {source} }}"#);
        let ast = parse(&source).unwrap();
        attr(&block(&ast.decls[0]).decls[0]).value.clone()
    }

    fn binary(expr: &Expr) -> (BinOp, &Expr, &Expr) {
        let ExprKind::Binary { op, lhs, rhs } = &expr.kind else {
            panic!("expected a binary expression");
        };
        (*op, lhs, rhs)
    }

    fn num(expr: &Expr) -> &str {
        let ExprKind::Num(raw) = &expr.kind else {
            panic!("expected a number literal");
        };
        raw
    }

    #[test]
    fn parses_multiplication_tighter_than_addition() {
        let expr = parse_value("1 + 2 * 3");

        let (op, lhs, rhs) = binary(&expr);
        assert_eq!(op, BinOp::Add);
        assert_eq!(num(lhs), "1");
        let (op, lhs, rhs) = binary(rhs);
        assert_eq!(op, BinOp::Mul);
        assert_eq!((num(lhs), num(rhs)), ("2", "3"));
    }

    #[test]
    fn parses_parens_over_precedence_and_widens_the_range() {
        let source = r#"trace "t" { input = (1 + 2) * 3 }"#;
        let ast = parse(source).unwrap();
        let expr = &attr(&block(&ast.decls[0]).decls[0]).value;

        let (op, lhs, rhs) = binary(expr);
        assert_eq!(op, BinOp::Mul);
        assert_eq!(num(rhs), "3");
        let (op, _, _) = binary(lhs);
        assert_eq!(op, BinOp::Add);
        // grouped expr keeps the parens in its range
        assert_eq!(&source[lhs.range.start..lhs.range.end], "(1 + 2)");
    }

    #[test]
    fn parses_left_associative_chains() {
        let expr = parse_value("10 - 2 - 3");

        let (op, lhs, rhs) = binary(&expr);
        assert_eq!(op, BinOp::Sub);
        assert_eq!(num(rhs), "3");
        let (op, lhs, rhs) = binary(lhs);
        assert_eq!(op, BinOp::Sub);
        assert_eq!((num(lhs), num(rhs)), ("10", "2"));
    }

    #[test]
    fn parses_comparisons_left_associative() {
        let expr = parse_value("1 < 2 < 3");

        let (op, lhs, rhs) = binary(&expr);
        assert_eq!(op, BinOp::Lt);
        assert_eq!(num(rhs), "3");
        let (op, _, _) = binary(lhs);
        assert_eq!(op, BinOp::Lt);
    }

    #[test]
    fn parses_logical_looser_than_comparison() {
        let expr = parse_value("1 < 2 && true || false");

        let (op, lhs, _) = binary(&expr);
        assert_eq!(op, BinOp::Or);
        let (op, lhs, rhs) = binary(lhs);
        assert_eq!(op, BinOp::And);
        assert!(matches!(rhs.kind, ExprKind::Bool(true)));
        let (op, _, _) = binary(lhs);
        assert_eq!(op, BinOp::Lt);
    }

    #[test]
    fn parses_unary_binding_tighter_than_binary() {
        let expr = parse_value("-1 + 2");

        let (op, lhs, rhs) = binary(&expr);
        assert_eq!(op, BinOp::Add);
        assert_eq!(num(rhs), "2");
        let ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } = &lhs.kind
        else {
            panic!("expected unary negation");
        };
        assert_eq!(num(operand), "1");

        let expr = parse_value("!true && false");
        let (op, lhs, _) = binary(&expr);
        assert_eq!(op, BinOp::And);
        assert!(matches!(lhs.kind, ExprKind::Unary { op: UnaryOp::Not, .. }));
    }

    #[test]
    fn parses_stacked_unary_operators() {
        let expr = parse_value("--5");
        let ExprKind::Unary {
            op: UnaryOp::Neg,
            operand,
        } = &expr.kind
        else {
            panic!("expected unary negation");
        };
        assert!(matches!(operand.kind, ExprKind::Unary { op: UnaryOp::Neg, .. }));

        let expr = parse_value("!!true");
        let ExprKind::Unary {
            op: UnaryOp::Not,
            operand,
        } = &expr.kind
        else {
            panic!("expected unary not");
        };
        assert!(matches!(operand.kind, ExprKind::Unary { op: UnaryOp::Not, .. }));
    }

    #[test]
    fn parses_ternaries_right_associative() {
        let expr = parse_value("true ? 1 : false ? 2 : 3");

        let ExprKind::Cond { cond, then, otherwise } = &expr.kind else {
            panic!("expected a conditional");
        };
        assert!(matches!(cond.kind, ExprKind::Bool(true)));
        assert_eq!(num(then), "1");
        assert!(matches!(otherwise.kind, ExprKind::Cond { .. }));
    }

    #[test]
    fn parses_a_ternary_nested_in_the_then_branch() {
        let expr = parse_value("true ? false ? 1 : 2 : 3");

        let ExprKind::Cond { then, otherwise, .. } = &expr.kind else {
            panic!("expected a conditional");
        };
        assert!(matches!(then.kind, ExprKind::Cond { .. }));
        assert_eq!(num(otherwise), "3");
    }

    #[test]
    fn parses_operators_inside_arrays_args_and_objects() {
        let expr = parse_value("[1 + 2]");
        let ExprKind::Array(items) = &expr.kind else {
            panic!("expected an array");
        };
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].kind, ExprKind::Binary { op: BinOp::Add, .. }));

        let expr = parse_value("choice(1 + 2, true ? 1 : 2)");
        let ExprKind::Func { args, .. } = &expr.kind else {
            panic!("expected a function call");
        };
        assert_eq!(args.len(), 2);
        assert!(matches!(args[0].kind, ExprKind::Binary { .. }));
        assert!(matches!(args[1].kind, ExprKind::Cond { .. }));

        let expr = parse_value("{ a = 1 + 2 b = 3 }");
        let ExprKind::Object(attrs) = &expr.kind else {
            panic!("expected an object");
        };
        assert_eq!(attrs.len(), 2);
        assert!(matches!(attrs[0].value.kind, ExprKind::Binary { .. }));
    }

    #[test]
    fn parses_a_missing_comma_between_numbers_as_subtraction() {
        // documented behavior change: [1 -2] is subtraction, not two items
        let expr = parse_value("[1 -2]");
        let ExprKind::Array(items) = &expr.kind else {
            panic!("expected an array");
        };
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].kind, ExprKind::Binary { op: BinOp::Sub, .. }));
    }

    #[test]
    fn rejects_a_binary_missing_its_right_operand() {
        assert_error_kinds(r#"trace "t" { input = 1 + }"#, &[ErrorKind::ExpectedExpressionAssignment]);
    }

    #[test]
    fn rejects_a_ternary_missing_its_colon() {
        assert_error_kinds(r#"trace "t" { input = true ? 1 }"#, &[ErrorKind::ExpectedTernaryColon]);
    }

    #[test]
    fn rejects_an_unclosed_group() {
        // the missing rparen errors once, then recovery syncs on the closing brace
        assert_error_kinds(r#"trace "t" { input = (1 + 2 }"#, &[ErrorKind::UnexpectedToken]);
    }

    #[test]
    fn recovers_to_the_next_declaration_after_an_operator_error() {
        // the dangling + errors once, output = 2 still parses as the next decl
        assert_error_kinds(r#"trace "t" { input = 1 + output = 2 }"#, &[ErrorKind::UnexpectedToken]);
    }
}
