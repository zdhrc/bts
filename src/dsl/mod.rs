mod ast;
mod diag;
mod filter;
mod lexer;
mod model;
mod modeler;
mod parser;
pub(crate) mod spec;

pub(crate) use diag::{Diag, DiagPhase, Diags, SrcRange};
pub(crate) use filter::WriteFilter;
pub(crate) use model::{
    Accessor, Array, ArrayElem, Automation, BinOp, Binding, Child, Choice, CtxRef, Field, Func, Maybe, Model, NOISE_SIZE_CAP,
    NodeId, Number, Object, ObjectField, Part, Range, RefId, Repeat, ResolvedRef, Scorer, ScorerArg, ScorerKind, ScorerLang,
    Selection, SpanFields, SpanKind, Step, Template, Trace, UnaryOp, Value, When,
};

use crate::dsl::{lexer::lex, modeler::model, parser::parse};

pub(crate) fn compile(src: &str) -> Result<Model, Diags> {
    let tokens = tracing::info_span!("lex").in_scope(|| lex(src))?;
    let ast = tracing::info_span!("parse").in_scope(|| parse(tokens, src))?;
    tracing::info_span!("model").in_scope(|| model(ast))
}

#[cfg(test)]
mod tests {
    use super::diag::DiagPhase;
    use super::*;

    #[test]
    fn compiles_source_into_model() {
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();

        assert_eq!(model.traces.len(), 1);
    }

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
    fn nested_scorers_require_a_top_level_definition() {
        let errors = compile("trace \"t\" { scorer \"missing\" { score = 0.5 } }").unwrap_err();
        assert!(errors.iter().any(|error| error.what.contains("unknown scorer \"missing\"")));
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
