mod diag;
mod lexer;
mod modeler;
mod parser;
mod spec;
mod syntax;

pub(crate) use diag::{Diag, DiagPhase, Diags, SrcRange};
pub(crate) use modeler::{Array, Model, Number, Object, ObjectField, Span, SpanFields, SpanKind, Trace, Value};

use crate::dsl::{lexer::lex, modeler::model, parser::parse};

pub(crate) fn compile(src: &str) -> Result<Model, Diags> {
    let tokens = lex(src)?;
    let ast = parse(tokens)?;
    model(ast)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_source_into_model() {
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();

        assert_eq!(model.traces.len(), 1);
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
