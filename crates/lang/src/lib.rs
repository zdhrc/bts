//! Parse and validate bts source independently of its consumers.
//!
//! Use [`syntax::parse`] to inspect source structure without checking its meaning.
//! Use [`semantics::compile`] to resolve references and check the language's semantic rules.
//! Compilation produces a [`Model`] for consumers to evaluate, generate from, or
//! inspect. This crate does not execute the model or contact external services.
#![warn(missing_docs)]

mod ast;
mod diag;
mod expr;
mod lexing;
mod model;
mod modeling;
mod parsing;
pub mod spec;

pub use diag::{Diag, DiagPhase, Diags, SrcRange};
pub use model::*;

/// Semantic validation and resolved models for language consumers.
///
/// Compilation parses source, checks language rules, and resolves references.
/// Runtime-dependent expressions remain in the model for consumers to evaluate.
/// Use [`crate::syntax`] when only source structure is needed.
pub mod semantics {
    use crate::{Diags, Model, lexing::lex, modeling::model, parsing::parse};

    /// Parses source, resolves references, and checks the language's semantic rules.
    ///
    /// Returns diagnostics from the first failing phase. Empty source is rejected.
    /// Runtime-dependent values are retained as expressions for consumers to evaluate.
    /// Successful compilation does not guarantee that every runtime evaluation succeeds.
    /// Modifying the returned model can invalidate its references and type guarantees.
    ///
    pub fn compile(src: &str) -> Result<Model, Diags> {
        compile_module(src, false)
    }

    /// Compiles source with an explicit policy for empty modules.
    ///
    /// With `allow_empty = true`, a module containing no declarations returns an
    /// empty model. All nonempty source receives the same checks as [`compile`].
    /// This supports consumers that interpret an empty module as removing resources.
    pub fn compile_module(src: &str, allow_empty: bool) -> Result<Model, Diags> {
        let tokens = tracing::info_span!("lex").in_scope(|| lex(src))?;
        let ast = tracing::info_span!("parse").in_scope(|| parse(tokens, src))?;
        if allow_empty && ast.paths.is_empty() && ast.kinds.is_empty() {
            return Ok(Model::default());
        }
        tracing::info_span!("model").in_scope(|| model(ast))
    }
}

/// Source structure before reference resolution and semantic validation.
///
/// Parsing checks structure without checking names, types, or allowed fields.
/// Use [`crate::semantics::compile`] to lower and validate the graph.
pub mod syntax {
    use crate::{Diag, DiagPhase, Diags, ast};

    pub use crate::ast::GraphDocument as Ast;
    pub use crate::ast::{Attr, BinOp, Expr, ExprKind, ObjectItem, TemplatePart, UnaryOp};
    pub use crate::ast::{
        GraphDocument, GraphEdgeStep, GraphFork, GraphHeader, GraphJunction, GraphNode, GraphNodeDecl, GraphOrigin, GraphPath,
        GraphReferenceKind, GraphStep, KindDecl,
    };
    pub use crate::ast::{GraphPattern, GraphPatternConstraint, GraphPatternNode, GraphPatternStep};

    /// Parses a source graph without checking its meaning.
    ///
    /// Nodes, connections, forks and junctions retain their authored structure.
    /// Indentation attaches selectors and junctions to forks. Names, kinds and
    /// target membership and pattern matches are resolved later.
    pub fn parse(source: &str) -> Result<Ast, crate::Diags> {
        let tokens = crate::lexing::lex(source)?;
        crate::parsing::parse(tokens, source)
    }

    /// Parses one expression and requires all input to belong to that expression.
    ///
    /// Names and types are not checked. Consumers can apply their own expression
    /// rules, as generation filters do. Invalid syntax returns source diagnostics.
    pub fn parse_expression(source: &str) -> Result<Expr, crate::Diags> {
        let tokens = crate::lexing::lex(source)?;
        crate::parsing::parse_expression(tokens, source)
    }

    /// Parses dotted names and quoted bracket names into path segments.
    ///
    /// This checks path syntax only. It does not look up names in a model or decide
    /// which resources a consumer may select. Numeric indexes and other expressions
    /// are rejected. Diagnostics refer to byte positions in `src`.
    ///
    /// ```
    /// assert_eq!(bts_lang::syntax::parse_traversal(r#"trace["run"].task.answer"#)?,
    ///            ["trace", "run", "task", "answer"]);
    /// # Ok::<(), bts_lang::Diags>(())
    /// ```
    pub fn parse_traversal(src: &str) -> Result<Vec<String>, Diags> {
        fn segments(expr: ast::Expr) -> Result<Vec<String>, Diags> {
            let range = expr.range;
            match expr.kind {
                ast::ExprKind::Ref { path } => Ok(path),
                ast::ExprKind::Index { target, index } if matches!(index.kind, ast::ExprKind::Str(_)) => {
                    let mut path = segments(*target)?;
                    let ast::ExprKind::Str(name) = index.kind else {
                        unreachable!()
                    };
                    path.push(name);
                    Ok(path)
                }
                _ => Err(vec![Diag {
                    when: DiagPhase::Parsing,
                    what: "expected a traversal with dotted or quoted bracket segments".to_owned(),
                    r#where: range,
                }]),
            }
        }
        segments(parse_expression(src)?)
    }
}
