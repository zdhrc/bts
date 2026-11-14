#[cfg(test)]
use crate::ast::{Ast, Block, Decl};
use crate::ast::{Attr, BinOp, Expr, ExprKind, ObjectItem, TemplatePart, UnaryOp};
use crate::ast::{
    GraphDocument, GraphEdgeStep, GraphFork, GraphHeader, GraphJunction, GraphNode, GraphNodeDecl, GraphOrigin, GraphPath,
    GraphReferenceKind, GraphStep,
};
use crate::ast::{GraphPattern, GraphPatternConstraint, GraphPatternNode, GraphPatternStep};
use crate::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::lexing::{Token, TokenKind, Tokens};
use std::fmt;

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

// words with meaning in expression positions, rejected as loop binding names
const RESERVED_WORDS: &[&str] = &[
    "true", "false", "null", "var", "for", "in", "if", "self", "source", "target", "context",
];

#[derive(Debug, Clone, PartialEq)]
struct Parser<'src> {
    tokens: Tokens,
    src: &'src str,
    errors: Errors,
    index: usize,
    graph_syntax: bool,
}

impl<'src> Parser<'src> {
    fn new(tokens: Tokens, src: &'src str) -> Self {
        Self {
            tokens,
            src,
            errors: Vec::new(),
            index: 0,
            graph_syntax: false,
        }
    }

    fn parse_graph(mut self) -> Result<GraphDocument, Errors> {
        self.graph_syntax = true;
        let mut paths = Vec::new();
        while !self.eof() {
            match self.parse_graph_path() {
                Some(path) => paths.push(path),
                None => break,
            }
        }
        if self.errors.is_empty() {
            Ok(GraphDocument { paths })
        } else {
            Err(self.errors)
        }
    }

    fn parse_graph_path(&mut self) -> Option<GraphPath> {
        let start_offset = self.peek().range.start;
        let indent = self.graph_indent()?;
        let start = if self.check(token!(Junction)) {
            GraphOrigin::Junction(self.parse_junction()?)
        } else {
            GraphOrigin::Node(self.parse_graph_node()?)
        };
        let steps = self.parse_graph_steps(indent)?;
        let end = self.peek().range.end;
        if matches!(self.peek().kind, TokenKind::Arrow | TokenKind::Junction) {
            self.errors
                .push(Error::new(ErrorKind::UnattachedContinuation, self.peek().range));
            return None;
        }
        if matches!(self.peek().kind, TokenKind::Minus | TokenKind::Tilde | TokenKind::Equals) {
            let kind = if matches!(steps.last(), Some(GraphStep::Fork(_))) {
                ErrorKind::ForkNeedsSelection
            } else {
                ErrorKind::GraphIndentation
            };
            self.errors.push(Error::new(kind, self.peek().range));
            return None;
        }
        self.expect(token!(Semicolon), ErrorKind::ExpectedStatementEnd)?;
        Some(GraphPath {
            start,
            steps,
            range: SrcRange::new(start_offset, end),
        })
    }

    fn parse_graph_steps(&mut self, floor: usize) -> Option<Vec<GraphStep>> {
        let mut steps = Vec::new();
        let mut node_indent = floor;
        loop {
            if !matches!(self.peek().kind, TokenKind::Minus | TokenKind::Tilde | TokenKind::Equals) {
                break;
            }
            if self.graph_indent()? < node_indent {
                break;
            }
            if self.check(token!(Equals)) {
                steps.push(GraphStep::Fork(self.parse_fork()?));
                break;
            }
            let edge = self.parse_graph_edge()?;
            let start = match &edge.target {
                GraphNode::Declaration(node) => node.range.start,
                GraphNode::Reference(expr) => expr.range.start,
            };
            let index = self.tokens.partition_point(|token| token.range.start < start);
            node_indent = self.graph_indent_at(index)?;
            steps.push(GraphStep::Edge(edge));
        }
        Some(steps)
    }

    fn graph_indent(&mut self) -> Option<usize> {
        self.graph_indent_at(self.index)
    }

    fn graph_indent_at(&mut self, index: usize) -> Option<usize> {
        let token = &self.tokens[index];
        let prefix = &self.src[token.line_start..token.range.start];
        let indent = prefix.chars().take_while(|ch| matches!(ch, ' ' | '\t')).count();
        if prefix[..indent].contains('\t') {
            self.errors.push(Error::new(ErrorKind::GraphIndentation, token.range));
            return None;
        }
        Some(indent)
    }

    fn parse_fork(&mut self) -> Option<GraphFork> {
        let start = self.peek().range.start;
        let indent = self.graph_indent()?;
        self.expect(token!(Equals), ErrorKind::UnexpectedToken)?;
        self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;
        let header = self.parse_graph_header()?;
        self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;
        self.expect(token!(FatArrow), ErrorKind::UnexpectedToken)?;
        self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;
        let mut entries = Vec::new();
        loop {
            if !self.check(token!(Minus)) {
                self.errors.push(Error::new(ErrorKind::ExpectedForkEntry, self.peek().range));
                return None;
            }
            self.graph_indent()?;
            entries.push(self.parse_graph_edge()?);
            if self.consume(token!(Comma)).is_none() {
                break;
            }
            if self.check(token!(RBrack)) {
                break;
            }
        }
        let closing_line = self.peek().line_start;
        self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;
        let mut continuations = Vec::new();
        let mut sibling_indent = None;
        let mut previous_line = None;
        while matches!(self.peek().kind, TokenKind::Arrow | TokenKind::Junction) {
            let line = self.peek().line_start;
            let child_indent = self.graph_indent()?;
            if previous_line == Some(line) {
                self.errors.push(Error::new(ErrorKind::InlineSibling, self.peek().range));
                return None;
            }
            let inline = line == closing_line && continuations.is_empty();
            if !inline {
                if child_indent <= indent {
                    break;
                }
                if let Some(level) = sibling_indent {
                    if child_indent < level {
                        break;
                    }
                    if child_indent != level {
                        self.errors.push(Error::new(ErrorKind::GraphIndentation, self.peek().range));
                        return None;
                    }
                } else {
                    sibling_indent = Some(child_indent);
                }
            }
            let start_offset = self.peek().range.start;
            let origin = if self.consume(token!(Arrow)).is_some() {
                let range = self.peek().range;
                let node = self.parse_graph_node()?;
                let GraphNode::Reference(reference) = node else {
                    self.errors.push(Error::new(ErrorKind::SelectorDeclaration, range));
                    return None;
                };
                GraphOrigin::Selector(reference)
            } else {
                GraphOrigin::Junction(self.parse_junction()?)
            };
            let steps = self.parse_graph_steps(child_indent)?;
            let end = self.tokens[self.index - 1].range.end;
            previous_line = Some(self.tokens[self.index - 1].line_start);
            continuations.push(GraphPath {
                start: origin,
                steps,
                range: SrcRange::new(start_offset, end),
            });
        }
        let end = self.tokens[self.index - 1].range.end;
        Some(GraphFork {
            header,
            entries,
            continuations,
            range: SrcRange::new(start, end),
        })
    }

    fn parse_junction(&mut self) -> Option<GraphJunction> {
        let start = self.peek().range.start;
        self.expect(token!(Junction), ErrorKind::UnexpectedToken)?;
        self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;
        self.expect(token!(Colon), ErrorKind::ExpectedGraphKind)?;
        let kind = self.expect(token!(Ident(value)), ErrorKind::ExpectedGraphKind)?;
        let props = if self.check(token!(LBrace)) {
            Some(self.parse_graph_fields()?)
        } else {
            None
        };
        let mut inputs = vec![self.parse_qualified_edge_reference()?];
        while self.consume(token!(Comma)).is_some() && !self.check(token!(RBrack)) {
            inputs.push(self.parse_qualified_edge_reference()?);
        }
        self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;
        self.expect(token!(Arrow), ErrorKind::UnexpectedToken)?;
        let target = self.parse_graph_node()?;
        let end = self.tokens[self.index - 1].range.end;
        Some(GraphJunction {
            kind,
            inputs,
            props,
            target,
            range: SrcRange::new(start, end),
        })
    }

    fn parse_qualified_edge_reference(&mut self) -> Option<Expr> {
        if self.check(token!(PatternOpen)) {
            return self.parse_pattern_reference();
        }
        let start = self.peek().range.start;
        self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;
        let mut expr = self.parse_graph_reference(start, GraphReferenceKind::Edge)?;
        while self.consume(token!(Dot)).is_some() {
            self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;
            let name = self.parse_graph_name()?;
            let end = self.peek().range.end;
            self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;
            expr = Expr::new(
                ExprKind::GraphMember {
                    target: Box::new(expr),
                    name,
                },
                SrcRange::new(start, end),
            );
        }
        Some(expr)
    }

    fn parse_pattern_reference(&mut self) -> Option<Expr> {
        let start = self.peek().range.start;
        self.expect(token!(PatternOpen), ErrorKind::UnexpectedToken)?;
        let first = self.parse_pattern_node()?;
        let selected_node = |node: &GraphPatternNode| match node {
            GraphPatternNode::Constraint(constraint) => usize::from(constraint.selected),
            GraphPatternNode::Reference(_) => 0,
        };
        let mut selections = selected_node(&first);
        let mut steps = Vec::new();
        while matches!(self.peek().kind, TokenKind::Minus | TokenKind::Tilde) {
            let step_start = self.peek().range.start;
            let cycle = self.next().kind == TokenKind::Tilde;
            self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;
            let mut edges = vec![self.parse_pattern_constraint()?];
            while self.consume(token!(Comma)).is_some() && !self.check(token!(RBrack)) {
                edges.push(self.parse_pattern_constraint()?);
            }
            self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;
            if cycle {
                self.expect(token!(CycleArrow), ErrorKind::UnexpectedToken)?;
            } else {
                self.expect(token!(Arrow), ErrorKind::UnexpectedToken)?;
            }
            let target = self.parse_pattern_node()?;
            selections += edges.iter().filter(|edge| edge.selected).count() + selected_node(&target);
            steps.push(GraphPatternStep {
                edges,
                cycle,
                target,
                range: SrcRange::new(step_start, self.tokens[self.index - 1].range.end),
            });
        }
        let end = self.peek().range.end;
        self.expect(token!(PatternClose), ErrorKind::UnexpectedToken)?;
        let range = SrcRange::new(start, end);
        if selections != 1 {
            self.errors.push(Error::new(ErrorKind::PatternSelection, range));
            return None;
        }
        Some(Expr::new(
            ExprKind::PatternRef(Box::new(GraphPattern {
                start: first,
                steps,
                range,
            })),
            range,
        ))
    }

    fn parse_pattern_node(&mut self) -> Option<GraphPatternNode> {
        let start = self.peek().range.start;
        self.expect(token!(LParen), ErrorKind::ExpectedGraphNode)?;
        if matches!(self.peek().kind, TokenKind::At | TokenKind::Colon) {
            let constraint = self.parse_pattern_constraint()?;
            self.expect(token!(RParen), ErrorKind::UnexpectedToken)?;
            Some(GraphPatternNode::Constraint(constraint))
        } else {
            self.parse_graph_reference(start, GraphReferenceKind::Node)
                .map(GraphPatternNode::Reference)
        }
    }

    fn parse_pattern_constraint(&mut self) -> Option<GraphPatternConstraint> {
        let start = self.peek().range.start;
        let selected = self.consume(token!(At)).is_some();
        self.expect(token!(Colon), ErrorKind::ExpectedGraphKind)?;
        let kind = self.expect(token!(Ident(value)), ErrorKind::ExpectedGraphKind)?;
        let props = if self.check(token!(LBrace)) {
            Some(self.parse_graph_fields()?)
        } else {
            None
        };
        let end = self.tokens[self.index - 1].range.end;
        Some(GraphPatternConstraint {
            selected,
            kind,
            props,
            range: SrcRange::new(start, end),
        })
    }

    fn parse_graph_node(&mut self) -> Option<GraphNode> {
        self.graph_indent()?;
        let start = self.peek().range.start;
        self.expect(token!(LParen), ErrorKind::ExpectedGraphNode)?;
        let declaration = self.check(token!(Colon))
            || (matches!(self.peek().kind, TokenKind::Ident(_) | TokenKind::String(_))
                && self.peek_ahead().kind == TokenKind::Colon);
        if declaration {
            let header = self.parse_graph_header()?;
            let mut end = self.peek().range.end;
            self.expect(token!(RParen), ErrorKind::UnexpectedToken)?;
            let body = if self.check(token!(LBrace)) {
                let body = self.parse_graph_fields()?;
                end = self.tokens[self.index - 1].range.end;
                Some(body)
            } else {
                None
            };
            Some(GraphNode::Declaration(GraphNodeDecl {
                header,
                body,
                range: SrcRange::new(start, end),
            }))
        } else {
            let reference = self.parse_graph_reference(start, GraphReferenceKind::Node)?;
            if self.check(token!(LBrace)) {
                self.errors.push(Error::new(ErrorKind::ReferenceBody, self.peek().range));
                return None;
            }
            Some(GraphNode::Reference(reference))
        }
    }

    fn parse_graph_header(&mut self) -> Option<GraphHeader> {
        let start = self.peek().range.start;
        let name_range = (!self.check(token!(Colon))).then_some(self.peek().range);
        let name = if name_range.is_some() {
            Some(self.parse_graph_name()?)
        } else {
            None
        };
        self.expect(token!(Colon), ErrorKind::ExpectedGraphKind)?;
        let kind = self.expect(token!(Ident(value)), ErrorKind::ExpectedGraphKind)?;
        let props = if self.check(token!(LBrace)) {
            Some(self.parse_graph_fields()?)
        } else {
            None
        };
        let end = self.tokens[self.index - 1].range.end;
        Some(GraphHeader {
            name,
            name_range,
            kind,
            props,
            range: SrcRange::new(start, end),
        })
    }

    fn parse_graph_name(&mut self) -> Option<String> {
        self.expect(
            |kind| match kind {
                TokenKind::Ident(name) | TokenKind::String(name) => Some(name.clone()),
                _ => None,
            },
            ErrorKind::ExpectedGraphName,
        )
    }

    fn parse_graph_fields(&mut self) -> Option<Vec<Attr>> {
        self.expect(token!(LBrace), ErrorKind::UnexpectedToken)?;
        let mut fields = Vec::new();
        while !self.check(token!(RBrace)) && !self.eof() {
            let range = self.peek().range;
            let key = self.expect(token!(Ident(value)), ErrorKind::ExpectedIdentifier)?;
            fields.push(self.parse_attr(key, range)?);
            if self.consume(token!(Comma)).is_none() && !self.check(token!(RBrace)) {
                self.errors.push(Error::new(ErrorKind::ExpectedFieldComma, self.peek().range));
                return None;
            }
        }
        self.expect(token!(RBrace), ErrorKind::UnexpectedToken)?;
        Some(fields)
    }

    fn parse_graph_edge(&mut self) -> Option<GraphEdgeStep> {
        let start = self.peek().range.start;
        let cycle = self.next().kind == TokenKind::Tilde;
        self.expect(token!(LBrack), ErrorKind::UnexpectedToken)?;
        let mut edges = vec![self.parse_graph_header()?];
        while self.consume(token!(Comma)).is_some() && !self.check(token!(RBrack)) {
            edges.push(self.parse_graph_header()?);
        }
        self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;
        if cycle {
            self.expect(token!(CycleArrow), ErrorKind::UnexpectedToken)?;
        } else {
            self.expect(token!(Arrow), ErrorKind::UnexpectedToken)?;
        }
        let target = self.parse_graph_node()?;
        let end = self.tokens[self.index - 1].range.end;
        Some(GraphEdgeStep {
            edges,
            cycle,
            target,
            range: SrcRange::new(start, end),
        })
    }

    // the opening boundary has already been consumed
    fn parse_graph_reference(&mut self, start: usize, kind: GraphReferenceKind) -> Option<Expr> {
        let closing = match kind {
            GraphReferenceKind::Node => TokenKind::RParen,
            GraphReferenceKind::Edge => TokenKind::RBrack,
        };
        let quoted_name = matches!(self.peek().kind, TokenKind::String(_)) && self.peek_ahead().kind == closing;
        let target = if quoted_name {
            let range = self.peek().range;
            let name = self.parse_graph_name()?;
            Expr::new(ExprKind::Ref { path: vec![name] }, range)
        } else {
            self.parse_expr()?
        };
        let end = self.peek().range.end;
        match kind {
            GraphReferenceKind::Node => self.expect(token!(RParen), ErrorKind::UnexpectedToken)?,
            GraphReferenceKind::Edge => self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?,
        }
        Some(Expr::new(
            ExprKind::GraphRef {
                kind,
                target: Box::new(target),
            },
            SrcRange::new(start, end),
        ))
    }

    #[cfg(test)]
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

    #[cfg(test)]
    fn parse_decl(&mut self) -> Option<Decl> {
        let range = self.peek().range;
        let Some(ident) = self.expect(token!(Ident(value)), ErrorKind::ExpectedIdentifier) else {
            self.skip_declaration();
            return None;
        };

        let decl = match &self.peek().kind {
            TokenKind::String(_) | TokenKind::TemplateOpen | TokenKind::LBrace => {
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

    #[cfg(test)]
    fn parse_block(&mut self, kind: String, range: SrcRange) -> Option<Block> {
        if self.check(token!(TemplateOpen)) {
            // consume the whole template so recovery resumes after it
            if let Some(template) = self.parse_template() {
                self.errors.push(Error::new(ErrorKind::InterpolatedName, template.range));
            }
        }

        let name_range = self.check(token!(String(value))).then(|| self.peek().range);
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
            name_range,
            decls,
            range,
        })
    }

    fn parse_attr(&mut self, key: String, range: SrcRange) -> Option<Attr> {
        if self.graph_syntax {
            self.expect(token!(Colon), ErrorKind::UnexpectedToken)?;
        } else {
            self.expect(token!(Equals), ErrorKind::UnexpectedToken)?;
        }
        let value = self.parse_expr()?;
        let range = if self.graph_syntax {
            SrcRange::new(range.start, value.range.end)
        } else {
            range
        };

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

    // x[i] and x.f, .f sugars to ["f"], x[a:b] slices
    fn parse_postfix(&mut self) -> Option<Expr> {
        let expr = self.parse_primary()?;
        self.parse_postfix_from(expr)
    }

    // the postfix loop from an already-parsed head, shared with interpolation holes
    fn parse_postfix_from(&mut self, mut expr: Expr) -> Option<Expr> {
        loop {
            match &self.peek().kind {
                TokenKind::LBrack => {
                    self.next();
                    let expr_start = expr.range.start;

                    let start = if self.check(token!(Colon)) {
                        None
                    } else {
                        Some(self.parse_expr()?)
                    };

                    let kind = if self.consume(token!(Colon)).is_some() {
                        let end = if self.check(token!(RBrack)) {
                            None
                        } else {
                            Some(self.parse_expr()?)
                        };
                        ExprKind::Slice {
                            target: Box::new(expr),
                            start: start.map(Box::new),
                            end: end.map(Box::new),
                        }
                    } else {
                        let index = start.expect("an index without a colon parsed an expression");
                        ExprKind::Index {
                            target: Box::new(expr),
                            index: Box::new(index),
                        }
                    };

                    let end = self.peek().range.end;
                    self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;

                    expr = Expr::new(kind, SrcRange::new(expr_start, end));
                }
                TokenKind::Dot => {
                    self.next();
                    if self.graph_syntax && self.consume(token!(LBrack)).is_some() {
                        let name = self.parse_graph_name()?;
                        let end = self.peek().range.end;
                        self.expect(token!(RBrack), ErrorKind::UnexpectedToken)?;
                        let start = expr.range.start;
                        expr = Expr::new(
                            ExprKind::GraphMember {
                                target: Box::new(expr),
                                name,
                            },
                            SrcRange::new(start, end),
                        );
                        continue;
                    }
                    let name_range = self.peek().range;
                    let name = self.expect(token!(Ident(value)), ErrorKind::ExpectedAccessorField)?;

                    let range = SrcRange::new(expr.range.start, name_range.end);
                    // a dot extends a reference path, on anything else it sugars to a string index
                    expr = match expr.kind {
                        ExprKind::Ref { mut path } => {
                            path.push(name);
                            Expr::new(ExprKind::Ref { path }, range)
                        }
                        kind => Expr::new(
                            ExprKind::Index {
                                target: Box::new(Expr::new(kind, expr.range)),
                                index: Box::new(Expr::new(ExprKind::Str(name), name_range)),
                            },
                            range,
                        ),
                    };
                }
                _ => break,
            }
        }

        Some(expr)
    }

    // a template arrives as literal chunks around expression holes
    fn parse_template(&mut self) -> Option<Expr> {
        let start = self.peek().range.start;
        self.expect(token!(TemplateOpen), ErrorKind::ExpectedStringLiteral)?;

        let mut parts = Vec::new();
        loop {
            if let Some(value) = self.consume(token!(TemplateChunk(value))) {
                parts.push(TemplatePart::Lit(value));
            } else if self.check(token!(InterpOpen)) {
                let open = self.peek().range;
                self.next();
                match self.parse_interpolation() {
                    Some((expr, end)) => parts.push(TemplatePart::Ref {
                        expr,
                        range: SrcRange::new(open.start, end),
                    }),
                    None => self.skip_interpolation(),
                }
            } else if self.check(token!(TemplateClose)) {
                let end = self.peek().range.end;
                self.next();
                return Some(Expr::new(ExprKind::Template(parts), SrcRange::new(start, end)));
            } else {
                // lexing closes every template it emits, so this is unreachable
                self.errors.push(Error::new(ErrorKind::UnexpectedToken, self.peek().range));
                return None;
            }
        }
    }

    // candidate holes accept expressions, existing syntax still requires a reference
    fn parse_interpolation(&mut self) -> Option<(Expr, usize)> {
        if self.graph_syntax {
            let expr = self.parse_expr()?;
            let end = self.peek().range.end;
            self.expect(token!(InterpClose), ErrorKind::UnexpectedToken)?;
            return Some((expr, end));
        }
        let range = self.peek().range;
        let name = self.expect(token!(Ident(value)), ErrorKind::ExpectedInterpolationPath)?;
        let head = Expr::new(ExprKind::Ref { path: vec![name] }, range);
        let expr = self.parse_postfix_from(head)?;

        if !self.check(token!(InterpClose)) {
            self.errors.push(Error::new(ErrorKind::UnexpectedToken, self.peek().range));
            return None;
        }
        let end = self.peek().range.end;
        self.next();
        Some((expr, end))
    }

    // drops a bad hole's tokens through its closing brace so the template resumes
    fn skip_interpolation(&mut self) {
        while !self.check(token!(TemplateClose)) && !self.eof() {
            if self.consume(token!(InterpClose)).is_some() {
                return;
            }
            self.next();
        }
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        match &self.peek().kind {
            TokenKind::PatternOpen if self.graph_syntax => self.parse_pattern_reference(),
            TokenKind::LParen => {
                let start = self.peek().range.start;
                self.next();

                if self.graph_syntax {
                    return self.parse_graph_reference(start, GraphReferenceKind::Node);
                }

                let inner = self.parse_expr()?;
                let end = self.peek().range.end;
                self.expect(token!(RParen), ErrorKind::UnexpectedToken)?;

                // no ast node for grouping, the widened range keeps diagnostics on the parens
                Some(Expr::new(inner.kind, SrcRange::new(start, end)))
            }
            TokenKind::InterpOpen if self.graph_syntax => {
                let start = self.next().range.start;
                let expr = self.parse_expr()?;
                let end = self.peek().range.end;
                self.expect(token!(InterpClose), ErrorKind::UnexpectedToken)?;
                Some(Expr::new(expr.kind, SrcRange::new(start, end)))
            }
            TokenKind::LBrack if self.graph_syntax => {
                let start = self.next().range.start;
                self.parse_graph_reference(start, GraphReferenceKind::Edge)
            }
            TokenKind::String(_) => {
                let range = self.peek().range;
                let value = self.expect(token!(String(value)), ErrorKind::ExpectedStringLiteral)?;
                Some(Expr::new(ExprKind::Str(value), range))
            }
            TokenKind::TemplateOpen => self.parse_template(),
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
                    // any other ident starts a reference path, the modeler resolves it
                    let name = value.clone();
                    self.next();
                    Some(Expr::new(ExprKind::Ref { path: vec![name] }, range))
                }
            }
            TokenKind::LBrack | TokenKind::ArrayOpen if self.graph_syntax || self.check(token!(LBrack)) => {
                let start = self.peek().range.start;
                self.next();

                if self.at_for_keyword() {
                    return self.parse_for(start, false);
                }

                let mut values = Vec::new();
                while !self.at_array_close() {
                    let range = self.peek().range;
                    if self.consume(token!(Ellipsis)).is_some() {
                        let operand = self.parse_expr()?;
                        let range = SrcRange::new(range.start, operand.range.end);
                        values.push(Expr::new(ExprKind::Spread(Box::new(operand)), range));
                    } else {
                        values.push(self.parse_expr()?);
                    }

                    if self.consume(token!(Comma)).is_none() {
                        break;
                    }
                }
                let end = self.peek().range.end;
                self.expect_array_close()?;

                Some(Expr::new(ExprKind::Array(values), SrcRange::new(start, end)))
            }
            TokenKind::LBrace => {
                let start = self.peek().range.start;
                self.expect(token!(LBrace), ErrorKind::UnexpectedToken)?;

                if self.at_for_keyword() {
                    return self.parse_for(start, true);
                }

                let mut items = Vec::new();
                while !self.check(token!(RBrace)) {
                    if self.consume(token!(Ellipsis)).is_some() {
                        items.push(ObjectItem::Spread(self.parse_expr()?));
                    } else {
                        let range = self.peek().range;
                        let key = self.expect(token!(Ident(value)), ErrorKind::UnexpectedToken)?;

                        items.push(ObjectItem::Attr(self.parse_attr(key, range)?));
                    }

                    // candidate fields always need commas between entries
                    // existing syntax also allows a line break as a separator
                    if self.consume(token!(Comma)).is_none()
                        && !self.check(token!(RBrace))
                        && !self.eof()
                        && (self.graph_syntax || !self.newline_before_peek())
                    {
                        let kind = if self.graph_syntax {
                            ErrorKind::ExpectedFieldComma
                        } else {
                            ErrorKind::ExpectedObjectComma
                        };
                        self.errors.push(Error::new(kind, self.peek().range));
                    }
                }
                let end = self.peek().range.end;
                self.expect(token!(RBrace), ErrorKind::UnexpectedToken)?;

                Some(Expr::new(ExprKind::Object(items), SrcRange::new(start, end)))
            }
            _ => {
                self.errors.push(Error::new(ErrorKind::ExpectedExpression, self.peek().range));
                None
            }
        }
    }

    // for is only a keyword right after [ or {, and { for = 1 } stays an attr
    fn at_for_keyword(&self) -> bool {
        matches!(&self.peek().kind, TokenKind::Ident(value) if value == "for")
            && !matches!(self.peek_ahead().kind, TokenKind::Equals)
            && !(self.graph_syntax && self.peek_ahead().kind == TokenKind::Colon)
    }

    fn at_array_close(&self) -> bool {
        if self.graph_syntax {
            self.check(token!(ArrayClose))
        } else {
            self.check(token!(RBrack))
        }
    }

    fn expect_array_close(&mut self) -> Option<()> {
        if self.graph_syntax {
            self.expect(token!(ArrayClose), ErrorKind::UnexpectedToken)
        } else {
            self.expect(token!(RBrack), ErrorKind::UnexpectedToken)
        }
    }

    // [for x in xs : body if cond] and { for k, v in xs : key => value if cond }
    fn parse_for(&mut self, start: usize, object: bool) -> Option<Expr> {
        self.next();

        let mut bindings = vec![self.parse_for_binding()?];
        if self.consume(token!(Comma)).is_some() {
            let range = self.peek().range;
            let binding = self.parse_for_binding()?;
            if bindings.contains(&binding) {
                self.errors.push(Error::new(ErrorKind::DuplicateForBinding, range));
                return None;
            }
            bindings.push(binding);
        }

        if !matches!(&self.peek().kind, TokenKind::Ident(value) if value == "in") {
            self.errors.push(Error::new(ErrorKind::ExpectedForIn, self.peek().range));
            return None;
        }
        self.next();

        // bindings are not in scope for the collection itself
        let collection = self.parse_expr()?;
        self.expect(token!(Colon), ErrorKind::ExpectedForColon)?;

        let (key, body, cond) = self.parse_for_tail(object)?;

        let end = self.peek().range.end;
        if object {
            self.expect(token!(RBrace), ErrorKind::UnexpectedToken)?;
        } else {
            self.expect_array_close()?;
        }

        Some(Expr::new(
            ExprKind::For {
                bindings,
                collection: Box::new(collection),
                key: key.map(Box::new),
                body: Box::new(body),
                cond: cond.map(Box::new),
            },
            SrcRange::new(start, end),
        ))
    }

    fn parse_for_tail(&mut self, object: bool) -> Option<(Option<Expr>, Expr, Option<Expr>)> {
        let (key, body) = if object {
            let key = self.parse_expr()?;
            self.expect(token!(FatArrow), ErrorKind::ExpectedForArrow)?;
            (Some(key), self.parse_expr()?)
        } else {
            (None, self.parse_expr()?)
        };

        let cond = if matches!(&self.peek().kind, TokenKind::Ident(value) if value == "if") {
            self.next();
            Some(self.parse_expr()?)
        } else {
            None
        };

        Some((key, body, cond))
    }

    fn parse_for_binding(&mut self) -> Option<String> {
        let range = self.peek().range;
        let binding = self.expect(token!(Ident(value)), ErrorKind::ExpectedForBinding)?;

        if RESERVED_WORDS.contains(&binding.as_str()) {
            self.errors.push(Error::new(ErrorKind::ReservedForBinding, range));
            return None;
        }

        Some(binding)
    }

    #[cfg(test)]
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
        &self.tokens[(self.index + 1).min(self.tokens.len() - 1)]
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
    // only whitespace and comments sit between tokens and comments never span
    // lines, so any \n in the gap before the current token is a line break
    fn newline_before_peek(&self) -> bool {
        self.index > 0 && self.src[self.tokens[self.index - 1].range.end..self.peek().range.start].contains('\n')
    }
}

// binding levels, higher binds tighter, unary and primary sit above all of these
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

pub(super) fn parse(tokens: Vec<Token>, src: &str) -> Result<GraphDocument, Diags> {
    Parser::new(tokens, src)
        .parse_graph()
        .map_err(|errors| errors.into_iter().map(Diag::from).collect())
}

pub(super) fn parse_expression(tokens: Vec<Token>, src: &str) -> Result<Expr, Diags> {
    let mut parser = Parser::new(tokens, src);
    parser.graph_syntax = true;
    let expr = parser.parse_expr();
    if !parser.eof() {
        parser
            .errors
            .push(Error::new(ErrorKind::UnexpectedToken, parser.peek().range));
    }
    match (expr, parser.errors.is_empty()) {
        (Some(expr), true) => Ok(expr),
        _ => Err(parser.errors.into_iter().map(Diag::from).collect()),
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) struct Error {
    kind: ErrorKind,
    range: SrcRange,
}

pub(super) type Errors = Vec<Error>;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ErrorKind {
    ExpectedGraphNode,
    ExpectedGraphName,
    ExpectedGraphKind,
    ExpectedStatementEnd,
    ExpectedFieldComma,
    ReferenceBody,
    UnattachedContinuation,
    GraphIndentation,
    InlineSibling,
    SelectorDeclaration,
    ExpectedForkEntry,
    ForkNeedsSelection,
    PatternSelection,
    UnexpectedToken,
    #[cfg(test)]
    ExpectedDeclaration,
    ExpectedIdentifier,
    ExpectedStringLiteral,
    ExpectedNumberLiteral,
    ExpectedExpression,
    #[cfg(test)]
    InterpolatedName,
    ExpectedInterpolationPath,
    ExpectedAccessorField,
    ExpectedTernaryColon,
    ExpectedForBinding,
    ReservedForBinding,
    DuplicateForBinding,
    ExpectedForIn,
    ExpectedForColon,
    ExpectedForArrow,
    ExpectedObjectComma,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ExpectedGraphNode => "expected a node declaration or reference",
            Self::ExpectedGraphName => "expected an identifier or plain quoted graph name",
            Self::ExpectedGraphKind => "expected a required graph kind after `:`",
            Self::ExpectedStatementEnd => "expected `;` after a graph statement",
            Self::ExpectedFieldComma => "expected `,` between fields",
            Self::ReferenceBody => "a node reference cannot attach a body",
            Self::UnattachedContinuation => "selector or junction has no fork at this indentation level",
            Self::GraphIndentation => "graph continuations require spaces and aligned sibling indentation",
            Self::InlineSibling => "sibling continuations must start on separate aligned lines",
            Self::SelectorDeclaration => "a fork selector must reference a node, not declare one",
            Self::ExpectedForkEntry => "expected one edge step with a target in the fork entry",
            Self::ForkNeedsSelection => "select a fork target or add a junction before continuing the path",
            Self::PatternSelection => "a pattern reference requires exactly one `@` selection marker",
            Self::UnexpectedToken => "unexpected token",
            #[cfg(test)]
            Self::ExpectedDeclaration => "expected declaration",
            Self::ExpectedIdentifier => "expected identifier",
            Self::ExpectedStringLiteral => "expected string",
            Self::ExpectedNumberLiteral => "expected number",
            Self::ExpectedExpression => "expected expression",
            #[cfg(test)]
            Self::InterpolatedName => "block names do not support interpolation",
            Self::ExpectedInterpolationPath => "expected a reference path in interpolation",
            Self::ExpectedAccessorField => "expected field name after `.`",
            Self::ExpectedTernaryColon => "expected `:` in conditional expression",
            Self::ExpectedForBinding => "expected loop binding name after `for`",
            Self::ReservedForBinding => "loop binding name is reserved",
            Self::DuplicateForBinding => "loop binding name is already bound",
            Self::ExpectedForIn => "expected `in` in for expression",
            Self::ExpectedForColon => "expected `:` in for expression",
            Self::ExpectedForArrow => "expected `=>` after the key in an object for expression",
            Self::ExpectedObjectComma => "expected `,` between object items on the same line",
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for Error {}

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
        Parser::new(crate::lexing::lex(src).unwrap(), src).parse()
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

    fn item_attr(item: &ObjectItem) -> &Attr {
        match item {
            ObjectItem::Attr(attr) => attr,
            ObjectItem::Spread(_) => panic!("expected an attribute, found a spread"),
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
    fn parses_bare_identifiers_as_references() {
        // `nil` is not a keyword literal, it parses as a reference the modeler rejects
        let ast = parse(r#"trace "a" { output = nil }"#).unwrap();
        let output = attr(&block(&ast.decls[0]).decls[0]);
        assert!(matches!(&output.value.kind, ExprKind::Ref { path } if path == &["nil"]));
    }

    #[test]
    fn parses_template_expressions() {
        use crate::ast::TemplatePart;

        let ast = parse(r#"trace "a" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let trace = block(&ast.decls[0]);

        let ExprKind::Template(parts) = &attr(&trace.decls[0]).value.kind else {
            panic!("expected a template");
        };
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], TemplatePart::Lit(value) if value == "q "));
        let TemplatePart::Ref { expr, .. } = &parts[1] else {
            panic!("expected a reference hole");
        };
        assert!(matches!(&expr.kind, ExprKind::Ref { path } if path == &["trace", "index"]));

        let ExprKind::Array(tags) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an array");
        };
        assert!(matches!(&tags[0].kind, ExprKind::Template(parts) if parts.len() == 2));
    }

    #[test]
    fn parses_interpolation_holes_with_indexes_and_quoted_segments() {
        use crate::ast::TemplatePart;

        let ast = parse(r#"trace "a" { input = "v ${var.xs[repeat.index - 1]} n ${llm["Chat Completion"].output}" }"#).unwrap();
        let ExprKind::Template(parts) = &attr(&block(&ast.decls[0]).decls[0]).value.kind else {
            panic!("expected a template");
        };
        assert_eq!(parts.len(), 4);

        // ${var.xs[repeat.index - 1]} is an index whose bracket holds a full expression
        let TemplatePart::Ref { expr, .. } = &parts[1] else {
            panic!("expected a reference hole");
        };
        let ExprKind::Index { target, index } = &expr.kind else {
            panic!("expected an index");
        };
        assert!(matches!(&target.kind, ExprKind::Ref { path } if path == &["var", "xs"]));
        assert!(matches!(&index.kind, ExprKind::Binary { op: BinOp::Sub, .. }));

        // ${llm["Chat Completion"].output} is a quoted segment then a field selection
        let TemplatePart::Ref { expr, .. } = &parts[3] else {
            panic!("expected a reference hole");
        };
        let ExprKind::Index { target, index } = &expr.kind else {
            panic!("expected an index");
        };
        assert!(matches!(&index.kind, ExprKind::Str(value) if value == "output"));
        let ExprKind::Index { target, index } = &target.kind else {
            panic!("expected an index");
        };
        assert!(matches!(&target.kind, ExprKind::Ref { path } if path == &["llm"]));
        assert!(matches!(&index.kind, ExprKind::Str(value) if value == "Chat Completion"));
    }

    #[test]
    fn rejects_calls_at_an_interpolation_holes_top_level() {
        assert_error_kinds(r#"a = "${upper(x)}""#, &[ErrorKind::UnexpectedToken]);
    }

    #[test]
    fn rejects_interpolated_block_names() {
        assert_error_kinds(r#"trace "${trace.index}" {}"#, &[ErrorKind::InterpolatedName]);
    }

    #[test]
    fn rejects_interpolations_without_a_reference_path() {
        assert_error_kinds(r#"a = "${}""#, &[ErrorKind::ExpectedInterpolationPath]);
        assert_error_kinds(r#"a = "${1x}""#, &[ErrorKind::ExpectedInterpolationPath]);
        assert_error_kinds(r#"a = "${.index}""#, &[ErrorKind::ExpectedInterpolationPath]);
    }

    #[test]
    fn rejects_malformed_interpolation_paths() {
        assert_error_kinds(r#"a = "${trace.}""#, &[ErrorKind::ExpectedAccessorField]);
        assert_error_kinds(r#"a = "${tr ace}""#, &[ErrorKind::UnexpectedToken]);
        assert_error_kinds(r#"a = "${a + b}""#, &[ErrorKind::UnexpectedToken]);
        assert_error_kinds(r#"a = "${x?y:z}""#, &[ErrorKind::UnexpectedToken]);
    }

    #[test]
    fn recovers_template_parsing_after_a_bad_hole() {
        // the bad hole drops out, the template and its later parts survive
        let errors = parse(r#"a = "x ${a +} y ${b.}""#).unwrap_err();
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn parses_variable_references() {
        let source = r#"trace "a" { metadata = var.meta output = [var.x, 1] }"#;
        let ast = parse(source).unwrap();
        let trace = block(&ast.decls[0]);

        let metadata = attr(&trace.decls[0]);
        assert!(matches!(&metadata.value.kind, ExprKind::Ref { path } if path == &["var", "meta"]));
        let range = metadata.value.range;
        assert_eq!(&source[range.start..range.end], "var.meta");

        let ExprKind::Array(items) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an array");
        };
        assert!(matches!(&items[0].kind, ExprKind::Ref { path } if path == &["var", "x"]));
    }

    #[test]
    fn parses_context_references() {
        let source = r#"trace "a" { input = trace.index metrics = { turn = repeat.index + 1, of = repeat.count } }"#;
        let ast = parse(source).unwrap();
        let trace = block(&ast.decls[0]);

        let input = attr(&trace.decls[0]);
        assert!(matches!(&input.value.kind, ExprKind::Ref { path } if path == &["trace", "index"]));
        let range = input.value.range;
        assert_eq!(&source[range.start..range.end], "trace.index");

        let ExprKind::Object(items) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an object");
        };
        let ObjectItem::Attr(turn) = &items[0] else {
            panic!("expected an attribute");
        };
        let ExprKind::Binary { lhs, .. } = &turn.value.kind else {
            panic!("expected a binary expression");
        };
        assert!(matches!(&lhs.kind, ExprKind::Ref { path } if path == &["repeat", "index"]));
        let ObjectItem::Attr(of) = &items[1] else {
            panic!("expected an attribute");
        };
        assert!(matches!(&of.value.kind, ExprKind::Ref { path } if path == &["repeat", "count"]));
    }

    #[test]
    fn parses_loop_references_as_paths() {
        let ast = parse(r#"trace "a" { input = [for trace in [1] : trace] }"#).unwrap();
        let ExprKind::For { body, .. } = &attr(&block(&ast.decls[0]).decls[0]).value.kind else {
            panic!("expected a for expression");
        };
        assert!(matches!(&body.kind, ExprKind::Ref { path } if path == &["trace"]));
    }

    #[test]
    fn rejects_context_references_missing_a_field() {
        assert_error_kinds(r#"trace "a" { input = repeat. }"#, &[ErrorKind::ExpectedAccessorField]);
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
        // a bare `var` parses as a reference, the modeler rejects it as unknown
        let ast = parse(r#"trace "a" { metadata = var }"#).unwrap();
        let metadata = attr(&block(&ast.decls[0]).decls[0]);
        assert!(matches!(&metadata.value.kind, ExprKind::Ref { path } if path == &["var"]));

        assert_error_kinds(r#"trace "a" { metadata = var. }"#, &[ErrorKind::ExpectedAccessorField]);
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
        assert!(matches!(&target.kind, ExprKind::Ref { path } if path == &["var", "xs"]));

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
        let ast = parse(r#"trace "a" { empty = {} meta = { model = "gpt", nested = { x = 1 } } }"#).unwrap();
        let trace = block(&ast.decls[0]);

        let ExprKind::Object(empty) = &attr(&trace.decls[0]).value.kind else {
            panic!("expected an object");
        };
        assert!(empty.is_empty());

        let ExprKind::Object(meta) = &attr(&trace.decls[1]).value.kind else {
            panic!("expected an object");
        };
        assert_eq!(meta.len(), 2);
        assert_eq!(item_attr(&meta[0]).key, "model");
        assert!(matches!(&item_attr(&meta[1]).value.kind, ExprKind::Object(nested) if nested.len() == 1));
    }

    #[test]
    fn separates_object_items_with_commas_or_newlines() {
        for source in [
            "{ a = 1, b = 2 }",
            "{ a = 1, b = 2, }",
            "{\n a = 1\n b = 2\n}",
            "{\n a = 1,\n b = 2\n}",
        ] {
            let expr = parse_value(source);
            let ExprKind::Object(items) = &expr.kind else {
                panic!("expected an object");
            };
            assert_eq!(items.len(), 2, "source: {source}");
        }
    }

    #[test]
    fn rejects_same_line_object_items_without_a_comma() {
        assert_error_kinds(r#"trace "t" { input = { a = 1 b = 2 } }"#, &[ErrorKind::ExpectedObjectComma]);
        assert_error_kinds(
            r#"trace "t" { input = { ...var.meta b = 2 } }"#,
            &[ErrorKind::ExpectedObjectComma],
        );
    }

    #[test]
    fn parses_vars_and_nested_spans() {
        let ast = parse(
            r#"vars { system = "help" meta = { model = "small" } }
            trace "support-sessions" {
                input = "question"
                output = task.turn_1.output
                task "turn_0" { output = "first" }
                task "turn_1" { output = "second" }
                metadata = { channel = "chat" }
                tags = ["chat"]
            }"#,
        )
        .unwrap();

        assert_eq!(ast.decls.len(), 2);
        let vars = block(&ast.decls[0]);
        assert_eq!(vars.kind, "vars");
        assert_eq!(vars.name, None);
        assert_eq!(vars.decls.len(), 2);
        let trace = block(&ast.decls[1]);
        assert_eq!(trace.kind, "trace");
        assert_eq!(trace.name.as_deref(), Some("support-sessions"));
        assert_eq!(trace.decls.len(), 6);
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
        assert_error_kinds("input =", &[ErrorKind::ExpectedExpression]);
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
        assert_eq!(errors[1].kind(), ErrorKind::ExpectedExpression);
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

        let expr = parse_value("{ a = 1 + 2, b = 3 }");
        let ExprKind::Object(items) = &expr.kind else {
            panic!("expected an object");
        };
        assert_eq!(items.len(), 2);
        assert!(matches!(item_attr(&items[0]).value.kind, ExprKind::Binary { .. }));
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
    fn parses_slices_with_optional_bounds() {
        let source = r#"trace "t" { input = var.xs[1:3] }"#;
        let ast = parse(source).unwrap();
        let input = attr(&block(&ast.decls[0]).decls[0]);

        let ExprKind::Slice { target, start, end } = &input.value.kind else {
            panic!("expected a slice expression");
        };
        assert!(matches!(&target.kind, ExprKind::Ref { path } if path == &["var", "xs"]));
        assert!(matches!(&start.as_ref().unwrap().kind, ExprKind::Num(value) if value == "1"));
        assert!(matches!(&end.as_ref().unwrap().kind, ExprKind::Num(value) if value == "3"));
        let range = input.value.range;
        assert_eq!(&source[range.start..range.end], "var.xs[1:3]");

        let ExprKind::Slice { start, end, .. } = parse_value("var.xs[:2]").kind else {
            panic!("expected a slice expression");
        };
        assert!(start.is_none() && end.is_some());

        let ExprKind::Slice { start, end, .. } = parse_value("var.xs[1:]").kind else {
            panic!("expected a slice expression");
        };
        assert!(start.is_some() && end.is_none());

        let ExprKind::Slice { start, end, .. } = parse_value("var.xs[:]").kind else {
            panic!("expected a slice expression");
        };
        assert!(start.is_none() && end.is_none());
    }

    #[test]
    fn parses_slices_with_expression_bounds_and_chains_them() {
        let ExprKind::Slice { start, .. } = parse_value("var.xs[1 + 1:]").kind else {
            panic!("expected a slice expression");
        };
        assert!(matches!(start.unwrap().kind, ExprKind::Binary { op: BinOp::Add, .. }));

        // a full ternary in brackets stays an index, its colon belongs to the ternary
        assert!(matches!(parse_value("var.xs[true ? 0 : 1]").kind, ExprKind::Index { .. }));

        // slices chain like indexes
        let ExprKind::Slice { target, .. } = parse_value("var.xs[1:][0:2]").kind else {
            panic!("expected a slice expression");
        };
        assert!(matches!(target.kind, ExprKind::Slice { .. }));
    }

    #[test]
    fn rejects_unterminated_slices() {
        assert_error_kinds(r#"trace "t" { input = var.xs[1: }"#, &[ErrorKind::ExpectedExpression]);
        assert_error_kinds(r#"trace "t" { input = var.xs[: }"#, &[ErrorKind::ExpectedExpression]);
    }

    #[test]
    fn parses_spread_elements_in_arrays() {
        let source = r#"trace "t" { input = [1, ...var.xs, 2] }"#;
        let ast = parse(source).unwrap();
        let input = attr(&block(&ast.decls[0]).decls[0]);

        let ExprKind::Array(values) = &input.value.kind else {
            panic!("expected an array");
        };
        assert_eq!(values.len(), 3);
        let ExprKind::Spread(operand) = &values[1].kind else {
            panic!("expected a spread element");
        };
        assert!(matches!(&operand.kind, ExprKind::Ref { path } if path == &["var", "xs"]));
        let range = values[1].range;
        assert_eq!(&source[range.start..range.end], "...var.xs");
    }

    #[test]
    fn parses_spread_items_in_objects() {
        let expr = parse_value(r#"{ a = 1, ...var.meta, b = 2 }"#);

        let ExprKind::Object(items) = &expr.kind else {
            panic!("expected an object");
        };
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0], ObjectItem::Attr(attr) if attr.key == "a"));
        assert!(
            matches!(&items[1], ObjectItem::Spread(operand) if matches!(&operand.kind, ExprKind::Ref { path } if path == &["var", "meta"]))
        );
        assert!(matches!(&items[2], ObjectItem::Attr(attr) if attr.key == "b"));
    }

    #[test]
    fn rejects_spreads_without_an_operand() {
        assert_error_kinds(r#"trace "t" { input = [...] }"#, &[ErrorKind::ExpectedExpression]);
    }

    #[test]
    fn parses_array_for_exprs() {
        let source = r#"trace "t" { input = [for x in var.xs : x * 2] }"#;
        let ast = parse(source).unwrap();
        let input = attr(&block(&ast.decls[0]).decls[0]);

        let ExprKind::For {
            bindings,
            collection,
            key,
            body,
            cond,
        } = &input.value.kind
        else {
            panic!("expected a for expression");
        };
        assert_eq!(bindings, &["x".to_owned()]);
        assert!(matches!(&collection.kind, ExprKind::Ref { path } if path == &["var", "xs"]));
        assert!(key.is_none() && cond.is_none());

        let ExprKind::Binary { lhs, .. } = &body.kind else {
            panic!("expected a binary body");
        };
        assert!(matches!(&lhs.kind, ExprKind::Ref { path } if path == &["x"]));

        let range = input.value.range;
        assert_eq!(&source[range.start..range.end], "[for x in var.xs : x * 2]");
    }

    #[test]
    fn parses_for_exprs_with_two_bindings_and_a_filter() {
        let expr = parse_value("[for i, x in var.xs : x if i > 0]");

        let ExprKind::For { bindings, cond, .. } = &expr.kind else {
            panic!("expected a for expression");
        };
        assert_eq!(bindings, &["i".to_owned(), "x".to_owned()]);
        assert!(matches!(cond.as_ref().unwrap().kind, ExprKind::Binary { op: BinOp::Gt, .. }));
    }

    #[test]
    fn parses_object_for_exprs() {
        let expr = parse_value(r#"{ for k, v in var.meta : k => v }"#);

        let ExprKind::For { bindings, key, body, .. } = &expr.kind else {
            panic!("expected a for expression");
        };
        assert_eq!(bindings, &["k".to_owned(), "v".to_owned()]);
        assert!(matches!(&key.as_ref().unwrap().kind, ExprKind::Ref { path } if path == &["k"]));
        assert!(matches!(&body.kind, ExprKind::Ref { path } if path == &["v"]));
    }

    #[test]
    fn parses_nested_for_exprs_with_shadowing_scopes() {
        let expr = parse_value("[for x in var.xs : [for y in x : y]]");

        let ExprKind::For { body, .. } = &expr.kind else {
            panic!("expected a for expression");
        };
        let ExprKind::For { collection, body, .. } = &body.kind else {
            panic!("expected a nested for expression");
        };
        assert!(matches!(&collection.kind, ExprKind::Ref { path } if path == &["x"]));
        assert!(matches!(&body.kind, ExprKind::Ref { path } if path == &["y"]));
    }

    #[test]
    fn keeps_for_as_an_object_key_outside_comprehensions() {
        let expr = parse_value("{ for = 1 }");

        let ExprKind::Object(items) = &expr.kind else {
            panic!("expected an object");
        };
        assert!(matches!(&items[0], ObjectItem::Attr(attr) if attr.key == "for"));
    }

    // recovery may add trailing errors, only the first one matters here
    #[track_caller]
    fn assert_first_error(src: &str, want: ErrorKind) {
        let errors = parse(src).unwrap_err();
        assert_eq!(errors[0].kind(), want);
    }

    #[test]
    fn parses_out_of_scope_refs_for_the_modeler_to_reject() {
        // x is out of scope in the collection and after the for expr, but scoping
        // is the modeler's concern, both parse as plain references
        assert!(parse(r#"trace "t" { input = [for x in x : x] }"#).is_ok());
        assert!(parse(r#"trace "t" { input = [[for x in var.xs : x], x] }"#).is_ok());
    }

    #[test]
    fn rejects_malformed_for_exprs() {
        // a missing binding lands on `in`, which reads as a reserved binding name
        assert_first_error(r#"trace "t" { input = [for in var.xs : 1] }"#, ErrorKind::ReservedForBinding);
        assert_first_error(r#"trace "t" { input = [for x var.xs : x] }"#, ErrorKind::ExpectedForIn);
        assert_first_error(r#"trace "t" { input = [for x in var.xs x] }"#, ErrorKind::ExpectedForColon);
        assert_first_error(r#"trace "t" { input = { for k in var.m : k } }"#, ErrorKind::ExpectedForArrow);
        assert_first_error(
            r#"trace "t" { input = [for var in var.xs : 1] }"#,
            ErrorKind::ReservedForBinding,
        );
        assert_first_error(
            r#"trace "t" { input = [for x, x in var.xs : x] }"#,
            ErrorKind::DuplicateForBinding,
        );
    }

    #[test]
    fn rejects_a_binary_missing_its_right_operand() {
        assert_error_kinds(r#"trace "t" { input = 1 + }"#, &[ErrorKind::ExpectedExpression]);
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
        // the dangling + swallows the next key as a reference, the stray = errors once
        assert_error_kinds(r#"trace "t" { input = 1 + output = 2 }"#, &[ErrorKind::ExpectedIdentifier]);
    }
}
