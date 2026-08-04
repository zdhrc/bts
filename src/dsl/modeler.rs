use crate::dsl::ast;
use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::model::{Array, Model, Number, Object, ObjectField, Span, SpanFields, SpanKind, Trace, Value};
use crate::dsl::spec;
use std::{collections::HashSet, fmt};

enum FieldValue {
    Value(Value),
    Object(Object),
    Strings(Vec<String>),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ExprType {
    String,
    Number,
    Boolean,
    Array,
    Object,
}

impl ExprType {
    fn of(expr: &ast::Expr) -> Self {
        match expr.kind {
            ast::ExprKind::Str(_) => Self::String,
            ast::ExprKind::Num(_) => Self::Number,
            ast::ExprKind::Bool(_) => Self::Boolean,
            ast::ExprKind::Array(_) => Self::Array,
            ast::ExprKind::Object(_) => Self::Object,
        }
    }
}

impl fmt::Display for ExprType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Array => "array",
            Self::Object => "object",
        })
    }
}

pub(super) struct Modeler {
    ast: ast::Ast,
    errors: Errors,
}

impl Modeler {
    fn new(ast: ast::Ast) -> Self {
        Self { ast, errors: Vec::new() }
    }

    fn model(mut self) -> Result<Model, Errors> {
        let mut traces = Vec::new();

        for decl in std::mem::take(&mut self.ast.decls) {
            match decl {
                ast::Decl::Attr(attr) => {
                    self.errors
                        .push(Error::new(ErrorKind::RootAttribute { keyword: attr.key }, attr.range));
                }
                ast::Decl::Block(block) => {
                    let Some(desc) = spec::SPEC.block(&block.kind) else {
                        self.errors.push(Error::new(
                            ErrorKind::UnknownBlock {
                                keyword: block.kind,
                                parent: spec::Place::Root,
                            },
                            block.range,
                        ));
                        continue;
                    };

                    if desc.id == spec::ids::TRACE && desc.allows(spec::Place::Root) {
                        if let Some(trace) = self.model_trace(block, desc) {
                            traces.push(trace);
                        }
                    } else {
                        self.errors.push(Error::new(
                            ErrorKind::BlockNotAllowed {
                                block: desc.id,
                                parent: spec::Place::Root,
                            },
                            block.range,
                        ));
                    }
                }
            }
        }

        if traces.is_empty() && self.errors.is_empty() {
            self.errors
                .push(Error::new(ErrorKind::EmptyShape { rule: spec::ids::NONEMPTY_SHAPE }, SrcRange::new(0, 0)));
        }

        if self.errors.is_empty() {
            Ok(Model { traces })
        } else {
            Err(self.errors)
        }
    }

    fn model_trace(&mut self, block: ast::Block, desc: &spec::BlockDesc) -> Option<Trace> {
        let ast::Block { name, decls, range, .. } = block;
        let name = self.model_name(name, range, desc);
        let (fields, blocks) = self.model_body(decls, desc, range);
        let children = blocks
            .into_iter()
            .filter_map(|block| self.model_span(block, desc.id))
            .collect();

        name.map(|name| Trace { name, fields, children })
    }

    fn model_span(&mut self, block: ast::Block, parent: spec::Id) -> Option<Span> {
        let ast::Block {
            kind,
            name,
            decls,
            range,
        } = block;
        let Some(desc) = spec::SPEC.block(&kind) else {
            self.errors.push(Error::new(
                ErrorKind::UnknownBlock {
                    keyword: kind,
                    parent: spec::Place::Block { id: parent },
                },
                range,
            ));
            return None;
        };

        if !desc.allows(spec::Place::Block { id: parent }) {
            self.errors.push(Error::new(
                ErrorKind::BlockNotAllowed {
                    block: desc.id,
                    parent: spec::Place::Block { id: parent },
                },
                range,
            ));
            return None;
        }

        let span_kind = if desc.id == spec::ids::TASK {
            SpanKind::Task
        } else if desc.id == spec::ids::LLM {
            SpanKind::Llm
        } else {
            unreachable!("block {} does not have a model lowering", desc.id.as_str());
        };

        let name = self.model_name(name, range, desc);
        let (fields, blocks) = self.model_body(decls, desc, range);
        let children = blocks
            .into_iter()
            .filter_map(|block| self.model_span(block, desc.id))
            .collect();

        name.map(|name| Span {
            name,
            kind: span_kind,
            fields,
            children,
        })
    }

    fn model_body(
        &mut self,
        decls: Vec<ast::Decl>,
        block: &spec::BlockDesc,
        range: SrcRange,
    ) -> (SpanFields, Vec<ast::Block>) {
        let mut fields = FieldsBuilder::default();
        let mut blocks = Vec::new();

        for decl in decls {
            match decl {
                ast::Decl::Block(block) => blocks.push(block),
                ast::Decl::Attr(attr) => self.model_field(&mut fields, block, attr),
            }
        }

        for field in block.body.fields {
            if field.cardinality == spec::Cardinality::Required && !fields.seen.contains(&field.id) {
                self.errors.push(Error::new(
                    ErrorKind::MissingField {
                        block: block.id,
                        field: field.id,
                    },
                    range,
                ));
            }
        }

        (fields.finish(), blocks)
    }

    fn model_field(&mut self, fields: &mut FieldsBuilder, block: &spec::BlockDesc, attr: ast::Attr) {
        let range = attr.range;
        let Some(field) = block.field(&attr.key) else {
            if !block.body.open {
                self.errors.push(Error::new(
                    ErrorKind::UnknownField {
                        block: block.id,
                        keyword: attr.key,
                    },
                    range,
                ));
            }
            return;
        };

        if field.cardinality != spec::Cardinality::Repeated && !fields.seen.insert(field.id) {
            self.errors.push(Error::new(
                ErrorKind::DuplicateField {
                    block: block.id,
                    field: field.id,
                },
                range,
            ));
            return;
        }

        if !self.validate_expr(&attr.value, block.id, field.id, field.value) {
            return;
        }

        if field.id == spec::ids::METRICS && !self.validate_metric_keys(&attr.value) {
            return;
        }

        let Some(value) = self.model_field_value(attr.value, field.value) else {
            return;
        };

        match (field.id, value) {
            (spec::ids::INPUT, FieldValue::Value(value)) => fields.input = Some(value),
            (spec::ids::OUTPUT, FieldValue::Value(value)) => fields.output = Some(value),
            (spec::ids::METADATA, FieldValue::Object(value)) => fields.metadata = Some(value),
            (spec::ids::METRICS, FieldValue::Object(value)) => fields.metrics = Some(value),
            (spec::ids::TAGS, FieldValue::Strings(value)) => fields.tags = Some(value),
            _ => unreachable!("field {} does not match its model lowering", field.id.as_str()),
        }
    }

    fn validate_metric_keys(&mut self, expr: &ast::Expr) -> bool {
        let ast::ExprKind::Object(attrs) = &expr.kind else {
            unreachable!("expression was validated as an object");
        };

        let mut valid = true;

        for attr in attrs {
            if spec::RESERVED_METRIC_KEYS.contains(&attr.key.as_str()) {
                self.errors.push(Error::new(
                    ErrorKind::ReservedMetricKey {
                        rule: spec::ids::RESERVED_METRICS,
                        key: attr.key.clone(),
                    },
                    attr.range,
                ));
                valid = false;
            }
        }

        valid
    }

    // fold, not all(): validate every element so each invalid item gets its own diagnostic
    #[allow(clippy::unnecessary_fold)]
    fn validate_expr(
        &mut self,
        expr: &ast::Expr,
        block: spec::Id,
        field: spec::Id,
        expected: &'static spec::ExprType,
    ) -> bool {
        let valid = match expected {
            spec::ExprType::Any => true,
            spec::ExprType::String => matches!(expr.kind, ast::ExprKind::Str(_)),
            spec::ExprType::Number => matches!(expr.kind, ast::ExprKind::Num(_)),
            spec::ExprType::Boolean => matches!(expr.kind, ast::ExprKind::Bool(_)),
            spec::ExprType::Array { items } => {
                let ast::ExprKind::Array(values) = &expr.kind else {
                    self.push_type_mismatch(expr, block, field, expected);
                    return false;
                };

                return values
                    .iter()
                    .fold(true, |valid, value| self.validate_expr(value, block, field, items) && valid);
            }
            spec::ExprType::Object { values } => {
                let ast::ExprKind::Object(attrs) = &expr.kind else {
                    self.push_type_mismatch(expr, block, field, expected);
                    return false;
                };

                return attrs.iter().fold(true, |valid, attr| {
                    self.validate_expr(&attr.value, block, field, values) && valid
                });
            }
        };

        if !valid {
            self.push_type_mismatch(expr, block, field, expected);
        }

        valid
    }

    fn push_type_mismatch(&mut self, expr: &ast::Expr, block: spec::Id, field: spec::Id, expected: &'static spec::ExprType) {
        self.errors.push(Error::new(
            ErrorKind::TypeMismatch {
                block,
                field,
                expected,
                found: ExprType::of(expr),
            },
            expr.range,
        ));
    }

    fn model_field_value(&mut self, expr: ast::Expr, expected: &spec::ExprType) -> Option<FieldValue> {
        match expected {
            spec::ExprType::Any => self.model_value(expr).map(FieldValue::Value),
            spec::ExprType::Object { values } if matches!(**values, spec::ExprType::Any) => {
                self.require_object(expr).map(FieldValue::Object)
            }
            spec::ExprType::Array { items } if matches!(**items, spec::ExprType::String) => {
                self.require_tags(expr).map(FieldValue::Strings)
            }
            _ => unreachable!("expression constraint does not have a model lowering"),
        }
    }

    fn model_value(&mut self, expr: ast::Expr) -> Option<Value> {
        let ast::Expr { kind, range } = expr;
        match kind {
            ast::ExprKind::Str(value) => Some(Value::Str(value)),
            ast::ExprKind::Bool(value) => Some(Value::Bool(value)),
            ast::ExprKind::Num(value) => self.model_number(value, range).map(Value::Num),
            ast::ExprKind::Array(values) => Some(Value::Array(Array {
                elem: values.into_iter().filter_map(|value| self.model_value(value)).collect(),
            })),
            ast::ExprKind::Object(attrs) => Some(Value::Object(self.model_object(attrs))),
        }
    }

    fn model_object(&mut self, attrs: Vec<ast::Attr>) -> Object {
        let mut seen = HashSet::new();
        let mut elem = Vec::new();

        for attr in attrs {
            if !seen.insert(attr.key.clone()) {
                self.errors.push(Error::new(
                    ErrorKind::DuplicateObjectKey {
                        rule: spec::ids::UNIQUE_OBJECT_KEYS,
                        key: attr.key,
                    },
                    attr.range,
                ));
            } else if let Some(value) = self.model_value(attr.value) {
                elem.push(ObjectField { key: attr.key, value });
            }
        }

        Object { elem }
    }

    fn require_object(&mut self, expr: ast::Expr) -> Option<Object> {
        let ast::Expr { kind, .. } = expr;
        match kind {
            ast::ExprKind::Object(attrs) => Some(self.model_object(attrs)),
            _ => unreachable!("expression was validated as an object"),
        }
    }

    fn require_tags(&mut self, expr: ast::Expr) -> Option<Vec<String>> {
        let ast::Expr { kind, .. } = expr;
        let ast::ExprKind::Array(values) = kind else {
            unreachable!("expression was validated as an array of strings");
        };

        let mut tags = Vec::new();

        for value in values {
            match value.kind {
                ast::ExprKind::Str(value) => tags.push(value),
                _ => unreachable!("array item was validated as a string"),
            }
        }

        Some(tags)
    }

    fn model_number(&mut self, raw: String, range: SrcRange) -> Option<Number> {
        let number = if raw.contains('.') {
            raw.parse::<f64>().ok().filter(|number| number.is_finite()).map(Number::Float)
        } else {
            raw.parse::<i64>().ok().map(Number::Int)
        };

        if number.is_none() {
            self.errors.push(Error::new(
                ErrorKind::InvalidNumber {
                    rule: spec::ids::FINITE_NUMBERS,
                    raw,
                },
                range,
            ));
        }

        number
    }

    fn model_name(&mut self, name: Option<String>, range: SrcRange, block: &spec::BlockDesc) -> Option<String> {
        match block.name {
            spec::NameDesc::Required => {
                if name.is_none() {
                    self.errors
                        .push(Error::new(ErrorKind::MissingName { block: block.id }, range));
                }
                name
            }
            spec::NameDesc::Forbidden if name.is_some() => {
                self.errors
                    .push(Error::new(ErrorKind::UnexpectedName { block: block.id }, range));
                None
            }
            spec::NameDesc::Optional => name,
            spec::NameDesc::Forbidden => {
                unreachable!("the typed model currently requires block names")
            }
        }
    }
}

pub(super) fn model(ast: ast::Ast) -> Result<Model, Diags> {
    Modeler::new(ast)
        .model()
        .map_err(|errors| errors.into_iter().map(Diag::from).collect())
}

#[derive(Default)]
struct FieldsBuilder {
    input: Option<Value>,
    output: Option<Value>,
    metadata: Option<Object>,
    metrics: Option<Object>,
    tags: Option<Vec<String>>,
    seen: HashSet<spec::Id>,
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

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) struct Error {
    kind: ErrorKind,
    range: SrcRange,
}

pub(super) type Errors = Vec<Error>;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(super) enum ErrorKind {
    RootAttribute {
        keyword: String,
    },
    UnknownBlock {
        keyword: String,
        parent: spec::Place,
    },
    BlockNotAllowed {
        block: spec::Id,
        parent: spec::Place,
    },
    MissingName {
        block: spec::Id,
    },
    UnexpectedName {
        block: spec::Id,
    },
    UnknownField {
        block: spec::Id,
        keyword: String,
    },
    MissingField {
        block: spec::Id,
        field: spec::Id,
    },
    DuplicateField {
        block: spec::Id,
        field: spec::Id,
    },
    TypeMismatch {
        block: spec::Id,
        field: spec::Id,
        expected: &'static spec::ExprType,
        found: ExprType,
    },
    DuplicateObjectKey {
        rule: spec::Id,
        key: String,
    },
    InvalidNumber {
        rule: spec::Id,
        raw: String,
    },
    ReservedMetricKey {
        rule: spec::Id,
        key: String,
    },
    EmptyShape {
        rule: spec::Id,
    },
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootAttribute { keyword } => {
                write!(
                    formatter,
                    "attribute `{keyword}` is not allowed at the root; allowed blocks: "
                )?;
                fmt_allowed_blocks(formatter, spec::Place::Root)
            }
            Self::UnknownBlock { keyword, parent } => {
                write!(formatter, "unknown block `{keyword}` ")?;
                fmt_place(formatter, *parent)?;
                formatter.write_str("; allowed blocks here: ")?;
                fmt_allowed_blocks(formatter, *parent)
            }
            Self::BlockNotAllowed { block, parent } => {
                let block = block_desc(*block);
                write!(formatter, "block `{}` is not allowed ", block.keyword)?;
                fmt_place(formatter, *parent)?;
                formatter.write_str("; allowed placements: ")?;
                fmt_places(formatter, block.allowed_in)
            }
            Self::MissingName { block } => {
                let block = block_desc(*block);
                write!(
                    formatter,
                    "block `{}` requires a name; expected `{}`",
                    block.keyword, block.syntax
                )
            }
            Self::UnexpectedName { block } => {
                let block = block_desc(*block);
                write!(formatter, "block `{}` does not accept a name", block.keyword)
            }
            Self::UnknownField { block, keyword } => {
                let block = block_desc(*block);
                write!(formatter, "unknown field `{keyword}` in block `{}`", block.keyword)?;
                if block.body.fields.is_empty() {
                    formatter.write_str("; this block does not accept fields")
                } else {
                    formatter.write_str("; expected one of: ")?;
                    for (index, field) in block.body.fields.iter().enumerate() {
                        if index > 0 {
                            formatter.write_str(", ")?;
                        }
                        write!(formatter, "`{}`", field.keyword)?;
                    }
                    Ok(())
                }
            }
            Self::MissingField { block, field } => {
                let block = block_desc(*block);
                let field = field_desc(block.id, *field);
                write!(formatter, "block `{}` requires field `{}`", block.keyword, field.keyword)
            }
            Self::DuplicateField { block, field } => {
                let block = block_desc(*block);
                let field = field_desc(block.id, *field);
                write!(
                    formatter,
                    "field `{}` may appear only once in block `{}`",
                    field.keyword, block.keyword
                )
            }
            Self::TypeMismatch {
                block,
                field,
                expected,
                found,
            } => {
                let block = block_desc(*block);
                let field = field_desc(block.id, *field);
                write!(
                    formatter,
                    "field `{}` in block `{}` expects {expected}, but found {found}",
                    field.keyword, block.keyword
                )
            }
            Self::DuplicateObjectKey { rule, key } => {
                let rule = rule_desc(*rule);
                write!(formatter, "duplicate object key `{key}`; {}", rule.summary)
            }
            Self::InvalidNumber { rule, raw } => {
                let rule = rule_desc(*rule);
                write!(formatter, "invalid number `{raw}`; {}", rule.summary)
            }
            Self::ReservedMetricKey { rule, key } => {
                let rule = rule_desc(*rule);
                write!(formatter, "metric key `{key}` is reserved; {}", rule.summary)
            }
            Self::EmptyShape { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "shape declares no traces; {}", rule.summary)
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for Error {}

fn block_desc(id: spec::Id) -> &'static spec::BlockDesc {
    spec::SPEC
        .block_by_id(id)
        .unwrap_or_else(|| panic!("error references unknown block {}", id.as_str()))
}

fn field_desc(block: spec::Id, field: spec::Id) -> &'static spec::FieldDesc {
    spec::SPEC.field(block, field).unwrap_or_else(|| {
        panic!(
            "error references unknown field {} in block {}",
            field.as_str(),
            block.as_str()
        )
    })
}

fn rule_desc(id: spec::Id) -> &'static spec::RuleDesc {
    spec::SPEC
        .rule(id)
        .unwrap_or_else(|| panic!("error references unknown rule {}", id.as_str()))
}

fn fmt_place(formatter: &mut fmt::Formatter<'_>, place: spec::Place) -> fmt::Result {
    match place {
        spec::Place::Root => formatter.write_str("at the root"),
        spec::Place::Block { id } => write!(formatter, "inside block `{}`", block_desc(id).keyword),
    }
}

fn fmt_places(formatter: &mut fmt::Formatter<'_>, places: &[spec::Place]) -> fmt::Result {
    for (index, place) in places.iter().enumerate() {
        if index > 0 {
            formatter.write_str(", ")?;
        }
        fmt_place(formatter, *place)?;
    }
    Ok(())
}

fn fmt_allowed_blocks(formatter: &mut fmt::Formatter<'_>, place: spec::Place) -> fmt::Result {
    let mut count = 0;
    for block in spec::SPEC.blocks.iter().filter(|block| block.allows(place)) {
        if count > 0 {
            formatter.write_str(", ")?;
        }
        write!(formatter, "`{}`", block.keyword)?;
        count += 1;
    }

    if count == 0 {
        formatter.write_str("none")?;
    }

    Ok(())
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
    #[cfg(test)]
    fn kind(&self) -> &ErrorKind {
        &self.kind
    }
    #[cfg(test)]
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
        assert!(
            trace
                .children
                .iter()
                .all(|span| matches!(span.children.as_slice(), [Span { kind: SpanKind::Llm, .. }]))
        );
        assert_eq!(
            trace.fields.tags.iter().map(String::as_str).collect::<Vec<_>>(),
            ["chat", "prod"]
        );
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

        let trace = spec::SPEC.block_by_id(spec::ids::TRACE).unwrap();
        let metadata = trace.field("metadata").unwrap();
        let tags = trace.field("tags").unwrap();
        let spec::ExprType::Array { items: tag_item } = *tags.value else {
            panic!("tags must be described as an array");
        };

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateField {
                block: spec::ids::TRACE,
                field: spec::ids::INPUT,
            }
        );
        assert_eq!(
            errors[1].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::TRACE,
                field: spec::ids::METADATA,
                expected: metadata.value,
                found: ExprType::Boolean,
            }
        );
        assert_eq!(
            errors[2].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::TRACE,
                field: spec::ids::TAGS,
                expected: tag_item,
                found: ExprType::Boolean,
            }
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
        assert!(errors.iter().all(|error| {
            matches!(
                error.kind(),
                ErrorKind::TypeMismatch {
                    block: spec::ids::TRACE,
                    field: spec::ids::TAGS,
                    expected: spec::ExprType::String,
                    ..
                }
            )
        }));
    }

    #[test]
    fn applies_block_rules_from_the_spec() {
        let root_errors = model(r#"task "root" {}"#).unwrap_err();
        assert_eq!(
            root_errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::TASK,
                parent: spec::Place::Root,
            }
        );

        let nested_errors = model(r#"trace "outer" { trace "inner" {} }"#).unwrap_err();
        assert_eq!(
            nested_errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::TRACE,
                parent: spec::Place::Block { id: spec::ids::TRACE },
            }
        );

        let name_errors = model("trace {}").unwrap_err();
        assert_eq!(name_errors[0].kind(), &ErrorKind::MissingName { block: spec::ids::TRACE });
    }

    #[test]
    fn distinguishes_unknown_blocks_and_root_attributes() {
        let root_attr = model("input = true").unwrap_err();
        assert_eq!(
            root_attr[0].kind(),
            &ErrorKind::RootAttribute {
                keyword: "input".to_owned(),
            }
        );

        let unknown = model(r#"trace "outer" { custom "inner" {} }"#).unwrap_err();
        assert_eq!(
            unknown[0].kind(),
            &ErrorKind::UnknownBlock {
                keyword: "custom".to_owned(),
                parent: spec::Place::Block { id: spec::ids::TRACE },
            }
        );
    }

    #[test]
    fn rejects_fields_absent_from_the_block_spec() {
        let errors = model(r#"trace "example" { unknown = true }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownField {
                block: spec::ids::TRACE,
                keyword: "unknown".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_reserved_metric_keys() {
        let source = r#"trace "example" { metrics = { start = 1 tokens = 4 end = 2 } }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(errors.len(), 2);
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::ReservedMetricKey {
                rule: spec::ids::RESERVED_METRICS,
                key: "start".to_owned(),
            }
        );
        assert_eq!(
            errors[1].kind(),
            &ErrorKind::ReservedMetricKey {
                rule: spec::ids::RESERVED_METRICS,
                key: "end".to_owned(),
            }
        );

        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "start");
    }

    #[test]
    fn rejects_shapes_without_traces() {
        for source in ["", "   \n  "] {
            let errors = model(source).unwrap_err();

            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].kind(), &ErrorKind::EmptyShape { rule: spec::ids::NONEMPTY_SHAPE });
        }
    }

    #[test]
    fn does_not_stack_the_empty_shape_error_onto_other_failures() {
        let errors = model("input = true").unwrap_err();

        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0].kind(), ErrorKind::RootAttribute { .. }));
    }

    #[test]
    fn associates_custom_validation_with_spec_rules() {
        let duplicate = model(r#"trace "example" { metadata = { key = 1 key = 2 } }"#).unwrap_err();
        assert_eq!(
            duplicate[0].kind(),
            &ErrorKind::DuplicateObjectKey {
                rule: spec::ids::UNIQUE_OBJECT_KEYS,
                key: "key".to_owned(),
            }
        );
        assert!(
            duplicate[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::UNIQUE_OBJECT_KEYS).unwrap().summary)
        );

        let raw = "999999999999999999999999999999999999";
        let invalid_number = model(&format!(r#"trace "example" {{ input = {raw} }}"#)).unwrap_err();
        assert_eq!(
            invalid_number[0].kind(),
            &ErrorKind::InvalidNumber {
                rule: spec::ids::FINITE_NUMBERS,
                raw: raw.to_owned(),
            }
        );
        assert!(
            invalid_number[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::FINITE_NUMBERS).unwrap().summary)
        );
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
                what: "field `metadata` in block `trace` expects object, but found boolean".to_owned(),
                r#where: SrcRange::new(29, 33),
            }]
        );
    }
}
