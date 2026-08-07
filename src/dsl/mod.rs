mod ast;
mod diag;
mod lexer;
mod model;
mod modeler;
mod parser;
pub(crate) mod spec;

pub(crate) use ast::{BinOp, UnaryOp};
pub(crate) use diag::{Diag, DiagPhase, Diags, SrcRange};
pub(crate) use model::{
    Array, Binding, Child, Choice, CtxRef, Func, Maybe, Model, Number, Object, ObjectField, Part, Range, Repeat, Span,
    SpanFields, SpanKind, Template, Trace, Value,
};

use crate::dsl::{lexer::lex, modeler::model, parser::parse};

pub(crate) fn compile(src: &str) -> Result<Model, Diags> {
    let tokens = lex(src)?;
    let ast = parse(tokens, src)?;
    model(ast)
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
    fn returns_diagnostics_from_the_first_failing_phase() {
        let diags = match compile("trace \"unterminated") {
            Ok(_) => panic!("expected lexing diagnostics"),
            Err(diags) => diags,
        };

        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].when, DiagPhase::Lexing);
    }
}
