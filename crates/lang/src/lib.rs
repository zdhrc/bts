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
    use crate::{Diag, DiagPhase, Diags, Model, SrcRange, lexing::lex, modeling::model, parsing::parse};

    /// A named source supplied by the caller without filesystem or package loading.
    #[derive(Debug, Clone, Copy)]
    pub struct Source<'a> {
        /// A display name for diagnostics and model provenance.
        pub name: &'a str,
        /// Source text in the ordinary language syntax.
        pub text: &'a str,
    }

    /// Parses source, resolves references, and checks the language's semantic rules.
    ///
    /// Lexing or parsing errors are returned without validating. Validation reports
    /// every independent mistake in the module once, each at its own location, and
    /// skips checks that depend on an element that already failed. Identical
    /// diagnostics are reported once. Empty source is rejected.
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
        compile_module_with_definitions(
            Source {
                name: "<source>",
                text: src,
            },
            &[],
            allow_empty,
        )
    }

    /// Compiles user source together with caller-supplied kind definitions.
    ///
    /// Definition sources may contain only definitions. All kinds and traits share one
    /// namespace and resolve together, including forward references across sources.
    /// Duplicate names are errors, including identical declarations.
    /// Source ranges use local byte offsets and a source index. Index zero identifies
    /// `source`, and index `n + 1` identifies `definitions[n]`. Successful models
    /// retain the corresponding names in [`Model::sources`]. The caller retains
    /// source text and can use these indexes to render diagnostics with [`Diag::render`].
    pub fn compile_with_definitions(source: Source<'_>, definitions: &[Source<'_>]) -> Result<Model, Diags> {
        compile_module_with_definitions(source, definitions, false)
    }

    /// Compiles with supplied definitions and an explicit empty user module policy.
    ///
    /// Provider declarations do not make an empty user module nonempty. When empty
    /// user source is allowed, supplied kinds are still validated and retained.
    pub fn compile_module_with_definitions(
        source: Source<'_>,
        definitions: &[Source<'_>],
        allow_empty: bool,
    ) -> Result<Model, Diags> {
        fn document(source: Source<'_>, index: usize) -> Result<crate::ast::GraphDocument, Diags> {
            let mut tokens = tracing::info_span!("lex")
                .in_scope(|| lex(source.text))
                .map_err(|mut errors| {
                    for error in &mut errors {
                        error.r#where.source = index;
                    }
                    errors
                })?;
            for token in &mut tokens {
                token.range.source = index;
            }
            tracing::info_span!("parse").in_scope(|| parse(tokens, source.text))
        }

        let mut ast = document(source, 0)?;
        let empty = ast.paths.is_empty() && ast.definitions.is_empty();
        let mut supplied_definitions = Vec::new();
        for (index, definition) in definitions.iter().enumerate() {
            let supplied = document(*definition, index + 1)?;
            if let Some(path) = supplied.paths.first() {
                return Err(vec![Diag {
                    when: DiagPhase::Validation,
                    what: "definition sources may contain only definitions".to_owned(),
                    r#where: path.range,
                }]);
            }
            supplied_definitions.extend(supplied.definitions);
        }
        if empty && !allow_empty {
            return Err(vec![Diag {
                when: DiagPhase::Validation,
                what: "expected at least one graph statement".to_owned(),
                r#where: SrcRange::new(0, 0),
            }]);
        }
        supplied_definitions.append(&mut ast.definitions);
        ast.definitions = supplied_definitions;
        let mut compiled = if ast.paths.is_empty() && ast.definitions.is_empty() {
            Model::default()
        } else {
            tracing::info_span!("model").in_scope(|| model(ast))?
        };
        compiled.sources = std::iter::once(source)
            .chain(definitions.iter().copied())
            .map(|source| source.name.to_owned())
            .collect();
        Ok(compiled)
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
        Definition, GraphDocument, GraphEdgeStep, GraphFork, GraphHeader, GraphJunction, GraphNode, GraphNodeDecl, GraphOrigin,
        GraphPath, GraphReferenceKind, GraphStep,
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
