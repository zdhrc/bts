use crate::dsl::parser::{Attribute, Block, Declaration, Expression, Source};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct Payload {
    events: Vec<Span>,
}

#[derive(Debug, Serialize)]
struct Span {
    root_span_id: String,
    span_id: String,
    span_parents: Vec<String>,
    span_attributes: SpanAttributes,

    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct SpanAttributes {
    name: String,

    #[serde(rename = "type")]
    span_type: String,
}

#[derive(Debug, Clone, PartialEq)]
struct SpanContext {
    root_span_id: Option<String>,
    parent_span_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GenErr {
    UnexpectedRootDecl,
    UnexpectedAttr { attr: String },

    InvalidNumber { value: String },
}

impl std::fmt::Display for GenErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenErr::UnexpectedRootDecl => {
                write!(
                    f,
                    "unexpected root declaration. only block declarations supported"
                )
            }
            GenErr::UnexpectedAttr { attr } => {
                write!(f, "unexpected span attribute {attr}")
            }
            GenErr::InvalidNumber { value } => {
                write!(f, "invalid number {value}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Generator {
    next_id: usize,
}

impl Generator {
    pub fn new() -> Self {
        Self { next_id: 0 }
    }

    pub fn generate(&self, source: Source) -> Result<Payload, GenErr> {
        let mut spans = Vec::new();
        for decl in &source.decls {
            match decl {
                Declaration::Block(block) => {
                    let ctx = SpanContext {
                        root_span_id: None,
                        parent_span_id: None,
                    };
                    self.generate_span(block, ctx, &mut spans)?;
                }
                _ => return Err(GenErr::UnexpectedRootDecl),
            }
        }
        Ok(Payload { events: spans })
    }

    fn generate_span(
        &self,
        block: &Block,
        ctx: SpanContext,
        spans: &mut Vec<Span>,
    ) -> Result<(), GenErr> {
        let span_id = Uuid::new_v4().to_string();
        let root_span_id = ctx.root_span_id.clone().unwrap_or_else(|| span_id.clone());

        let mut span = Span {
            span_id: span_id.clone(),
            root_span_id: root_span_id.clone(),
            span_parents: ctx.parent_span_id.clone().into_iter().collect(),
            span_attributes: SpanAttributes {
                name: block.name.clone().unwrap_or_else(|| block.kind.clone()),
                span_type: block.kind.clone(),
            },
            input: None,
            output: None,
            metadata: None,
            metrics: None,
            tags: None,
        };

        for decl in &block.decls {
            if let Declaration::Attribute(attr) = decl {
                self.apply_attr_to_span(&mut span, attr)?;
            }
        }

        spans.push(span);

        for decl in &block.decls {
            if let Declaration::Block(c_block) = decl {
                let c_ctx = SpanContext {
                    root_span_id: Some(root_span_id.clone()),
                    parent_span_id: Some(span_id.clone()),
                };

                self.generate_span(c_block, c_ctx, spans)?;
            }
        }

        Ok(())
    }

    fn generate_expr(&self, expr: &Expression) -> Result<serde_json::Value, GenErr> {
        match expr {
            Expression::Str(value) => Ok(serde_json::Value::String(value.clone())),
            Expression::Num(value) => {
                let num = value.parse::<f64>().map_err(|_| GenErr::InvalidNumber {
                    value: value.clone(),
                })?;
                Ok(serde_json::json!(num))
            }
            Expression::Bool(value) => Ok(serde_json::Value::Bool(*value)),
            Expression::Array(values) => {
                let values = values
                    .iter()
                    .map(|value| self.generate_expr(value))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(serde_json::Value::Array(values))
            }
            Expression::Object(attrs) => {
                let mut object = serde_json::Map::new();
                for attr in attrs {
                    object.insert(attr.key.clone(), self.generate_expr(&attr.value)?);
                }

                Ok(serde_json::Value::Object(object))
            }
        }
    }

    fn apply_attr_to_span(&self, span: &mut Span, attr: &Attribute) -> Result<(), GenErr> {
        match attr.key.as_str() {
            "input" => {
                span.input = Some(self.generate_expr(&attr.value)?);
            }
            "output" => {
                span.output = Some(self.generate_expr(&attr.value)?);
            }
            "metadata" => {
                span.metadata = Some(self.generate_expr(&attr.value)?);
            }
            "metrics" => {
                span.metrics = Some(self.generate_expr(&attr.value)?);
            }
            "tags" => {
                span.tags = Some(self.generate_expr(&attr.value)?);
            }
            _ => {
                return Err(GenErr::UnexpectedAttr {
                    attr: attr.key.clone(),
                });
            }
        }
        Ok(())
    }
}

#[test]
fn debug() {
    let src = include_str!("../../tests/fixtures/simple.bt");
    let tokens = crate::dsl::lexer::lex(src).unwrap();

    let mut parser = crate::dsl::parser::Parser {
        tokens: tokens,
        cursor: 0,
    };
    let source = parser.parse().unwrap();

    let generator = Generator::new();

    let payload = generator.generate(source).unwrap();
    let json = serde_json::to_string_pretty(&payload).unwrap();

    println!("{json}");
}
