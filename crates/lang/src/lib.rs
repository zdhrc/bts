//! Parse and validate bts source independently of its consumers.
//!
//! Use [`syntax::parse`] to inspect source structure without checking its meaning.
//! Use [`semantics::compile`] to resolve references and check the language's semantic rules.
//! Compilation produces a [`Model`] for consumers to evaluate, generate from, or
//! inspect. This crate does not execute the model or contact external services.
#![warn(missing_docs)]

mod ast;
mod diag;
mod lexing;
mod model;
mod modeling;
mod parsing;
pub mod spec;

pub use diag::{Diag, DiagPhase, Diags, SrcRange};
pub use model::{
    Accessor, Array, ArrayElem, Automation, AutomationKind, BinOp, Binding, Child, Choice, CtxRef, Dataset, DatasetCase,
    DatasetSource, Experiment, Facet, Field, Func, JudgeOption, Maybe, Model, NOISE_SIZE_CAP, NodeId, Number, Object,
    ObjectField, Part, Range, RefId, Repeat, ResolvedRef, Scorer, ScorerArg, ScorerKind, ScorerLang, ScorerSpan, ScorerStep,
    Selection, Span, SpanFields, SpanKind, Step, Template, Trace, UnaryOp, Value, WeightedOption, When,
};

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
    /// ```
    /// let model = bts_lang::semantics::compile(r#"trace "run" { input = "hello" }"#)?;
    /// assert_eq!(model.traces[0].name, "run");
    /// # Ok::<(), bts_lang::Diags>(())
    /// ```
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
        if allow_empty && ast.decls.is_empty() {
            return Ok(Model::default());
        }
        tracing::info_span!("model").in_scope(|| model(ast))
    }
}

/// Source structure before reference resolution and semantic validation.
///
/// Parsing checks structure without checking names, types, or allowed fields.
/// Use [`crate::semantics::compile`] for semantic checks.
pub mod syntax {
    use crate::{Diag, DiagPhase, Diags, ast};

    pub use crate::ast::{Ast, Attr, BinOp, Block, Decl, Expr, ExprKind, ObjectItem, TemplatePart, UnaryOp};

    /// Parses a complete source module without checking its meaning.
    ///
    /// Empty source is allowed. Unknown block names and unresolved references can
    /// appear in the returned tree. Malformed source returns lexing or parsing
    /// diagnostics with byte ranges into `source`.
    ///
    /// ```
    /// let source = r#"trace "run" { output = task.missing.output }"#;
    /// assert!(bts_lang::syntax::parse(source).is_ok());
    /// assert!(bts_lang::semantics::compile(source).is_err());
    /// ```
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

#[cfg(test)]
mod tests {
    use super::semantics::compile;
    use super::*;
    use crate::diag::DiagPhase;

    #[test]
    fn compiles_every_spec_example() {
        for example in spec::SPEC.examples {
            let result = compile(example.source);
            assert_eq!(result.is_ok(), example.valid, "example {}", example.id.as_str());
        }
    }

    #[test]
    fn rejects_unbound_or_ambiguous_scoring_automations() {
        let source = r#"automation "bad" {
            type = "scorer"
            scorers = ["missing"]
            scope = "span"
            root = true
            span_names = ["answer"]
        }"#;
        let errors = compile(source).unwrap_err();
        assert!(errors.iter().any(|error| error.what.contains("unknown scorer \"missing\"")));
        assert!(errors.iter().any(|error| error.what.contains("set either root = true")));
    }

    #[test]
    fn models_facets_and_topics_automations() {
        let model = compile(
            r#"
            facet "Churn risk" { prompt = "Summarize risk." description = "Retention" no_match_pattern = "^NONE$" }
            automation "topics" { type = "topics" facets = ["Churn risk"] scope = "trace" enabled = false sampling_rate = 0.25 }
        "#,
        )
        .unwrap();
        assert_eq!(model.facets[0].prompt, "Summarize risk.");
        assert_eq!(model.facets[0].description.as_deref(), Some("Retention"));
        assert_eq!(model.facets[0].no_match_pattern.as_deref(), Some("^NONE$"));
        assert!(matches!(&model.automations[0].kind, AutomationKind::Topics { facets } if facets == &["Churn risk"]));
        assert!(!model.automations[0].enabled);
        assert_eq!(model.automations[0].sampling_rate, 0.25);
        assert!(compile(r#"facet "only" { prompt = "Extract." }"#).is_ok());
    }

    #[test]
    fn rejects_invalid_facets_and_type_specific_automation_fields() {
        for source in [
            r#"facet "f" { prompt = "" }"#,
            r#"facet "f" { prompt = uuid() }"#,
            r#"facet "f" { description = "No prompt" }"#,
            r#"facet "f" { prompt = "x" prompt = "y" }"#,
            r#"facet "f" { prompt = "x" } facet "f" { prompt = "y" }"#,
            r#"automation "a" { type = "topics" facets = ["missing"] scope = "trace" }"#,
            r#"facet "f" { prompt = "x" } automation "a" { type = "topics" facets = ["f"] scope = "span" }"#,
            r#"facet "f" { prompt = "x" } automation "a" { type = "topics" facets = ["f"] scope = "trace" root = false }"#,
            r#"facet "f" { prompt = "x" } automation "a" { type = "topics" facets = [] scope = "trace" }"#,
            r#"facet "f" { prompt = "x" } automation "a" { type = "topics" facets = ["f", "f"] scope = "trace" }"#,
            r#"facet "f" { prompt = "x" } automation "a" { type = "topics" facets = ["f"] }"#,
            r#"automation "a" { type = "topics" scope = "trace" }"#,
            r#"scorer "s" { code { score = 1 } } automation "a" { type = "scorer" scorers = ["s"] facets = ["f"] scope = "span" root = true }"#,
        ] {
            assert!(compile(source).is_err(), "{source}");
        }
    }

    #[test]
    fn nested_scorers_require_a_top_level_definition() {
        let errors = compile("trace \"t\" { scorer \"missing\" { score = 0.5 } }").unwrap_err();
        assert!(errors.iter().any(|error| error.what.contains("unknown scorer \"missing\"")));
    }

    #[test]
    fn models_dataset_description_and_source_cases() {
        let source = r#"
            trace "run" { input = "hello" task "step" { output = "done" tool "lookup" { output = "found" } } }
            trace "run-alt" { task "first-step" { output = "ok" } }
            dataset "examples" {
                description = "Regression cases"
                case "inline" { input = { text = "hello" } expected = "done" }
                case "whole" { trace = trace["run"] tags = ["regression"] }
                case "step" { span = trace.run.task.step }
                case "nested" { span = trace["run"].task["step"].tool.lookup }
                case "quoted" { span = trace["run-alt"].task["first-step"] }
                case "group" { traces = [trace["run"], trace["run"]] }
            }
        "#;
        let model = compile(source).unwrap();
        assert_eq!(model.datasets.len(), 1);
        assert_eq!(model.datasets[0].description.as_deref(), Some("Regression cases"));
        assert_eq!(model.datasets[0].cases.len(), 6);
    }

    #[test]
    fn models_experiments_with_forward_typed_references() {
        let model = compile(r#"
            experiment "fixed" { dataset = dataset["cases"] task = trace.workflow.task.answer scorers = [scorer.correct] baseline = experiment["baseline"] }
            experiment "baseline" { dataset = dataset.cases task = trace["workflow"] scorers = [scorer["correct"]] description = "Before the fix" }
            dataset "cases" { case "one" { input = "question" } }
            scorer "correct" { code { score = 1 } }
            trace "workflow" { input = "question" task "answer" { output = "answer" } }
        "#).unwrap();
        assert_eq!(model.experiments.len(), 2);
        assert_eq!(model.experiments[0].baseline.as_deref(), Some("baseline"));
        assert_eq!(model.experiments[0].dataset, "cases");
        let reference = &model.refs[model.experiments[0].task.0 as usize];
        assert!(matches!(reference.accessor, Accessor::Block { kind: "task", .. }));
    }

    #[test]
    fn rejects_invalid_experiment_bindings_and_baseline_cycles() {
        let resources = r#"dataset "cases" { case "one" { input = "x" } } scorer "correct" { code { score = 1 } } trace "t" { input = "x" task "answer" { output = "y" } choice "route" { task "branch" { output = "z" } } }"#;
        for fields in [
            "dataset = dataset.missing task = trace.t scorers = [scorer.correct]",
            "dataset = dataset.cases task = trace.t scorers = [scorer.missing]",
            "dataset = dataset.cases task = trace.t.output scorers = [scorer.correct]",
            "dataset = dataset.cases task = trace.t.choice.route.task.branch scorers = [scorer.correct]",
            "dataset = dataset.cases task = trace.t scorers = []",
            "dataset = dataset.cases task = trace.t scorers = [scorer.correct, scorer.correct]",
            "dataset = dataset.cases task = trace.t scorers = [scorer.correct] baseline = experiment.e",
            "dataset = dataset.cases task = trace.t scorers = [scorer.correct] baseline = experiment.missing",
            "dataset = dataset.cases task = trace.t scorers = [\"correct\"]",
            "dataset = \"cases\" task = trace.t scorers = [scorer.correct]",
            "dataset = dataset.cases task = trace.t",
        ] {
            assert!(
                compile(&format!("{resources} experiment \"e\" {{ {fields} }}")).is_err(),
                "{fields}"
            );
        }
        assert!(
            compile(&format!(
                r#"{resources}
            experiment "a" {{ dataset = dataset.cases task = trace.t scorers = [scorer.correct] baseline = experiment.b }}
            experiment "b" {{ dataset = dataset.cases task = trace.t scorers = [scorer.correct] baseline = experiment.a }}
        "#
            ))
            .is_err()
        );
    }

    #[test]
    fn dataset_example_covers_inline_trace_and_span_cases() {
        let model = compile(include_str!("../../../examples/dataset_cases.bt")).unwrap();
        let dataset = &model.datasets[0];
        assert_eq!(
            dataset.description.as_deref(),
            Some("Billing support examples from authored inputs and trace spans")
        );
        assert!(
            dataset
                .cases
                .iter()
                .any(|case| matches!(case.source, DatasetSource::Inline(_)))
        );
        assert!(
            dataset
                .cases
                .iter()
                .any(|case| matches!(case.source, DatasetSource::Trace(_)))
        );
        assert!(dataset.cases.iter().any(|case| matches!(case.source, DatasetSource::Span(_))));
    }

    #[test]
    fn rejects_dataset_span_paths_missing_from_the_module() {
        let source = r#"
            trace "run" { task "step" { tool "lookup" { output = "found" } } }
            dataset "examples" {
                case "wrong-parent" { span = trace.run.tool.lookup }
                case "missing-trace" { span = trace.other.task.step }
            }
        "#;
        let errors = compile(source).unwrap_err();
        assert!(errors.iter().any(|error| error.what.contains("lookup")));
        assert!(errors.iter().any(|error| error.what.contains("other")));
    }

    #[test]
    fn rejects_dataset_span_paths_through_dynamic_blocks() {
        let source = r#"
            trace "run" { choice "route" { task "step" { output = "done" } } }
            dataset "examples" { case "step" { span = trace.run.choice.route.task.step } }
        "#;
        let errors = compile(source).unwrap_err();
        assert!(errors.iter().any(|error| error.what.contains("directed generation")));
    }

    #[test]
    fn returns_diagnostics_from_the_first_failing_phase() {
        let diags = match compile("trace \"unterminated") {
            Ok(_) => panic!("expected lexing diagnostics"),
            Err(diags) => diags,
        };

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].when, DiagPhase::Lexing);
    }
}

#[cfg(test)]
mod module_reference_tests {
    use super::semantics::compile;
    use crate::{Accessor, DatasetSource};
    #[test]
    fn preserves_block_identity_through_aliases_and_forward_references() {
        let model = compile(
            r#"
            vars { source = trace["support"] }
            dataset "cases" {
                case "whole" { trace = var.source }
                case "span" { span = var.source.task["lookup"] }
                case "fields" { input = var.source.input expected = var.source.task["lookup"].output }
                case "group" { traces = [var.source, trace["support"]] }
            }
            trace "support" { input = "question" task "lookup" { output = "answer" } }
        "#,
        )
        .unwrap();
        let DatasetSource::Trace(id) = model.datasets[0].cases[0].source else {
            panic!("trace source")
        };
        assert!(
            matches!(model.refs[id.0 as usize].accessor, Accessor::Block { node, kind: "trace" } if node == model.traces[0].node)
        );
    }
    #[test]
    fn validates_resolved_source_types() {
        for source in [
            r#"trace = trace["support"].input"#,
            r#"trace = trace["support"].task["lookup"]"#,
            r#"span = trace["support"]"#,
            r#"traces = [trace["support"], trace["support"].input]"#,
            r#"input = trace["support"]"#,
            r#"trace = "support""#,
        ] {
            let module = format!(
                r#"trace "support" {{ input = "question" task "lookup" {{ output = "answer" }} }} dataset "cases" {{ case "bad" {{ {source} }} }}"#
            );
            assert!(compile(&module).is_err(), "{source}");
        }
    }
    #[test]
    fn bracket_selection_handles_field_names_as_block_names() {
        compile(r#"vars { source = trace["input"] } dataset "cases" { case "whole" { trace = var.source } } trace "input" { input = 1 }"#).unwrap();
    }
    #[test]
    fn collections_preserve_reference_targets_through_variables() {
        compile(r#"vars { sources = [trace["support"], trace["support"]] } trace "support" {} dataset "cases" { case "group" { traces = var.sources } }"#).unwrap();
    }
    #[test]
    fn validates_unused_identity_aliases() {
        assert!(compile(r#"vars { source = trace["missing"] } trace "actual" {}"#).is_err());
    }
    #[test]
    fn module_field_alias_cycles_fail_during_modeling() {
        assert!(compile(r#"vars { question = trace["support"].input } trace "support" { input = var.question } dataset "cases" { case "inline" { input = var.question } }"#).is_err());
    }
}
