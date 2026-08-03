use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::syntax;
use std::collections::HashSet;
use thiserror::Error as Err;

#[derive(Debug)]
pub(crate) struct Model {
    pub(crate) traces: Vec<Trace>,
}

#[derive(Debug)]
pub(crate) struct Trace {
    pub(crate) name: String,
    pub(crate) fields: SpanFields,
    pub(crate) children: Vec<Span>,
}

#[derive(Debug)]
pub(crate) struct Span {
    pub(crate) name: String,
    pub(crate) kind: SpanKind,
    pub(crate) fields: SpanFields,
    pub(crate) children: Vec<Span>,
}

#[derive(Debug)]
pub(crate) enum SpanKind {
    Task,
    Llm,
}

#[derive(Debug)]
pub(crate) struct SpanFields {
    pub(crate) input: Option<Value>,
    pub(crate) output: Option<Value>,
    pub(crate) metadata: Option<Object>,
    pub(crate) metrics: Option<Object>,
    pub(crate) tags: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum Value {
    Str(String),
    Num(Number),
    Bool(bool),
    Array(Array),
    Object(Object),
}

#[derive(Debug)]
pub(crate) enum Number {
    Int(i64),
    Float(f64),
}

#[derive(Debug)]
pub(crate) struct Array {
    pub(crate) elem: Vec<Value>,
}

#[derive(Debug)]
pub(crate) struct Object {
    pub(crate) elem: Vec<ObjectField>,
}

#[derive(Debug)]
pub(crate) struct ObjectField {
    pub(crate) key: String,
    pub(crate) value: Value,
}

pub(super) struct Modeler {
    ast: syntax::Ast,
    errors: Errors,
}

impl Modeler {
    fn new(ast: syntax::Ast) -> Self {
        Self { ast, errors: Vec::new() }
    }

    fn model(mut self) -> Result<Model, Errors> {
        let mut traces = Vec::new();

        for decl in std::mem::take(&mut self.ast.decls) {
            match decl {
                syntax::Decl::Attr(attr) => {
                    self.errors.push(Error::new(ErrorKind::UnexpectedRootAttr, attr.range));
                }
                syntax::Decl::Block(block) if block.kind == "trace" => {
                    if let Some(trace) = self.model_trace(block) {
                        traces.push(trace);
                    }
                }
                syntax::Decl::Block(block) => {
                    self.errors.push(Error::new(ErrorKind::UnexpectedRootBlock, block.range));
                }
            }
        }

        if self.errors.is_empty() {
            Ok(Model { traces })
        } else {
            Err(self.errors)
        }
    }

    fn model_trace(&mut self, block: syntax::Block) -> Option<Trace> {
        let syntax::Block { name, decls, range, .. } = block;
        let name = self.require_name(name, range);
        let (fields, blocks) = self.model_body(decls);
        let children = blocks.into_iter().filter_map(|block| self.model_span(block)).collect();

        name.map(|name| Trace { name, fields, children })
    }

    fn model_span(&mut self, block: syntax::Block) -> Option<Span> {
        let syntax::Block { kind, name, decls, range } = block;
        let span_kind = match kind.as_str() {
            "task" => SpanKind::Task,
            "llm" => SpanKind::Llm,
            "trace" => {
                self.errors.push(Error::new(ErrorKind::NestedTrace, range));
                return None;
            }
            _ => {
                self.errors.push(Error::new(ErrorKind::UnknownSpanKind, range));
                return None;
            }
        };

        let name = self.require_name(name, range);
        let (fields, blocks) = self.model_body(decls);
        let children = blocks.into_iter().filter_map(|block| self.model_span(block)).collect();

        name.map(|name| Span {
            name,
            kind: span_kind,
            fields,
            children,
        })
    }

    fn model_body(&mut self, decls: Vec<syntax::Decl>) -> (SpanFields, Vec<syntax::Block>) {
        let mut fields = FieldsBuilder::default();
        let mut blocks = Vec::new();

        for decl in decls {
            match decl {
                syntax::Decl::Block(block) => blocks.push(block),
                syntax::Decl::Attr(attr) => self.model_field(&mut fields, attr),
            }
        }

        (fields.finish(), blocks)
    }

    fn model_field(&mut self, fields: &mut FieldsBuilder, attr: syntax::Attr) {
        let range = attr.range;
        if !fields.seen.insert(attr.key.clone()) {
            self.errors.push(Error::new(ErrorKind::DuplicateAttr, range));
            return;
        }

        match attr.key.as_str() {
            "input" => fields.input = self.model_value(attr.value),
            "output" => fields.output = self.model_value(attr.value),
            "metadata" => fields.metadata = self.require_object(attr.value),
            "metrics" => fields.metrics = self.require_object(attr.value),
            "tags" => fields.tags = self.require_tags(attr.value),
            _ => self.errors.push(Error::new(ErrorKind::UnknownAttr, range)),
        }
    }

    fn model_value(&mut self, expr: syntax::Expr) -> Option<Value> {
        let syntax::Expr { kind, range } = expr;
        match kind {
            syntax::ExprKind::Str(value) => Some(Value::Str(value)),
            syntax::ExprKind::Bool(value) => Some(Value::Bool(value)),
            syntax::ExprKind::Num(value) => self.model_number(value, range).map(Value::Num),
            syntax::ExprKind::Array(values) => Some(Value::Array(Array {
                elem: values.into_iter().filter_map(|value| self.model_value(value)).collect(),
            })),
            syntax::ExprKind::Object(attrs) => Some(Value::Object(self.model_object(attrs))),
        }
    }

    fn model_object(&mut self, attrs: Vec<syntax::Attr>) -> Object {
        let mut seen = HashSet::new();
        let mut elem = Vec::new();

        for attr in attrs {
            if !seen.insert(attr.key.clone()) {
                self.errors.push(Error::new(ErrorKind::DuplicateObjectKey, attr.range));
            } else if let Some(value) = self.model_value(attr.value) {
                elem.push(ObjectField { key: attr.key, value });
            }
        }

        Object { elem }
    }

    fn require_object(&mut self, expr: syntax::Expr) -> Option<Object> {
        let syntax::Expr { kind, range } = expr;
        match kind {
            syntax::ExprKind::Object(attrs) => Some(self.model_object(attrs)),
            _ => {
                self.errors.push(Error::new(ErrorKind::ExpectedObject, range));
                None
            }
        }
    }

    fn require_tags(&mut self, expr: syntax::Expr) -> Option<Vec<String>> {
        let syntax::Expr { kind, range } = expr;
        let syntax::ExprKind::Array(values) = kind else {
            self.errors.push(Error::new(ErrorKind::ExpectedStringArray, range));
            return None;
        };

        let mut tags = Vec::new();
        let mut valid = true;

        for value in values {
            match value.kind {
                syntax::ExprKind::Str(value) => tags.push(value),
                _ => {
                    self.errors.push(Error::new(ErrorKind::ExpectedStringArray, value.range));
                    valid = false;
                }
            }
        }

        valid.then_some(tags)
    }

    fn model_number(&mut self, raw: String, range: SrcRange) -> Option<Number> {
        let number = if raw.contains('.') {
            raw.parse::<f64>().ok().filter(|number| number.is_finite()).map(Number::Float)
        } else {
            raw.parse::<i64>().ok().map(Number::Int)
        };

        if number.is_none() {
            self.errors.push(Error::new(ErrorKind::InvalidNumber, range));
        }

        number
    }

    fn require_name(&mut self, name: Option<String>, range: SrcRange) -> Option<String> {
        if name.is_none() {
            self.errors.push(Error::new(ErrorKind::MissingName, range));
        }
        name
    }
}

pub(super) fn model(ast: syntax::Ast) -> Result<Model, Diags> {
    Modeler::new(ast).model().map_err(|errors| errors.into_iter().map(Diag::from).collect())
}

#[derive(Default)]
struct FieldsBuilder {
    input: Option<Value>,
    output: Option<Value>,
    metadata: Option<Object>,
    metrics: Option<Object>,
    tags: Option<Vec<String>>,
    seen: HashSet<String>,
}

impl FieldsBuilder {
    fn finish(self) -> SpanFields {
        SpanFields {
            input: self.input,
            output: self.output,
            metadata: self.metadata,
            metrics: self.metrics,
            tags: self.tags.unwrap_or_default(),
        }
    }
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
    #[error("attribute is not allowed at the root")]
    UnexpectedRootAttr,
    #[error("block is not allowed at the root")]
    UnexpectedRootBlock,
    #[error("unknown span kind")]
    UnknownSpanKind,
    #[error("trace blocks cannot be nested")]
    NestedTrace,
    #[error("block requires a name")]
    MissingName,
    #[error("unknown attribute")]
    UnknownAttr,
    #[error("duplicate attribute")]
    DuplicateAttr,
    #[error("expected object")]
    ExpectedObject,
    #[error("expected array of strings")]
    ExpectedStringArray,
    #[error("duplicate object key")]
    DuplicateObjectKey,
    #[error("invalid number")]
    InvalidNumber,
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
            when: DiagPhase::Validation,
            what: kind.to_string(),
            r#where: range,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(source: &str) -> Result<Model, Errors> {
        let tokens = crate::dsl::lexer::lex(source).unwrap();
        let ast = crate::dsl::parser::parse(tokens).unwrap();
        Modeler::new(ast).model()
    }

    #[test]
    fn models_fixture_as_typed_domain() {
        let model = model(include_str!("../../tests/fixtures/simple.bt")).unwrap();

        assert_eq!(model.traces.len(), 1);
        let trace = &model.traces[0];
        assert_eq!(trace.name, "multi-turn-conversation");
        assert_eq!(trace.children.len(), 2);
        assert!(trace.children.iter().all(|span| matches!(&span.kind, SpanKind::Task)));
        assert!(trace.children.iter().all(|span| matches!(span.children.as_slice(), [Span { kind: SpanKind::Llm, .. }])));
        assert_eq!(trace.fields.tags.iter().map(String::as_str).collect::<Vec<_>>(), ["chat", "prod"]);
    }

    #[test]
    fn rejects_invalid_domain_fields() {
        let source = r#"
            trace "example" {
                input = "first"
                input = "second"
                metadata = true
                tags = ["valid", false ]
            }
            "#;
        let result = model(source);
        let errors = match result {
            Err(errors) => errors,
            Ok(_) => panic!("expected semantic errors"),
        };

        assert_eq!(
            errors.iter().map(Error::kind).collect::<Vec<_>>(),
            vec![ErrorKind::DuplicateAttr, ErrorKind::ExpectedObject, ErrorKind::ExpectedStringArray,]
        );

        let range = errors[1].range();
        assert_eq!(&source[range.start..range.end], "true");
    }

    #[test]
    fn collects_an_error_for_each_invalid_tag() {
        let errors = match model(r#"trace "example" { tags = [true , false , 1] }"#) {
            Err(errors) => errors,
            Ok(_) => panic!("expected semantic errors"),
        };

        assert_eq!(errors.len(), 3);
        assert!(errors.iter().all(|error| error.kind() == ErrorKind::ExpectedStringArray));
    }

    #[test]
    fn reports_validation_diagnostics_at_the_invalid_source() {
        let source = r#"trace "example" { metadata = true }"#;
        let tokens = crate::dsl::lexer::lex(source).unwrap();
        let ast = crate::dsl::parser::parse(tokens).unwrap();

        let diagnostics = match super::model(ast) {
            Err(diagnostics) => diagnostics,
            Ok(_) => panic!("expected a validation diagnostic"),
        };

        assert_eq!(
            diagnostics,
            vec![Diag {
                when: DiagPhase::Validation,
                what: "expected object".to_owned(),
                r#where: SrcRange::new(29, 33),
            }]
        );
    }
}
