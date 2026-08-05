use crate::dsl::ast;
use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::model::{
    Array, CtxRef, Model, Number, Object, ObjectField, Part, Span, SpanFields, SpanKind, Template, Trace, Value,
};
use crate::dsl::spec;
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    fmt,
};

enum FieldValue {
    Value(Value),
    Object(Object),
    Tags(Vec<Template>),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ExprType {
    String,
    Number,
    Boolean,
    Null,
    Array,
    Object,
}

impl ExprType {
    fn of(expr: &ast::Expr) -> Self {
        match expr.kind {
            ast::ExprKind::Str(_) => Self::String,
            ast::ExprKind::Template(_) => Self::String,
            ast::ExprKind::Num(_) => Self::Number,
            ast::ExprKind::Bool(_) => Self::Boolean,
            ast::ExprKind::Null => Self::Null,
            ast::ExprKind::Array(_) => Self::Array,
            ast::ExprKind::Object(_) => Self::Object,
            ast::ExprKind::VarRef(_) => unreachable!("variable references are resolved before type checks"),
        }
    }
}

impl fmt::Display for ExprType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Null => "null",
            Self::Array => "array",
            Self::Object => "object",
        })
    }
}

pub(super) struct Modeler {
    ast: ast::Ast,
    vars: HashMap<String, ast::Expr>,
    errors: Errors,
}

impl Modeler {
    fn new(ast: ast::Ast) -> Self {
        Self {
            ast,
            vars: HashMap::new(),
            errors: Vec::new(),
        }
    }

    fn model(mut self) -> Result<Model, Errors> {
        let mut traces = Vec::new();

        // collect vars first so refs work no matter the decl order
        let mut rest = Vec::with_capacity(self.ast.decls.len());
        for decl in std::mem::take(&mut self.ast.decls) {
            match decl {
                ast::Decl::Block(block) if spec::SPEC.block(&block.kind).is_some_and(|desc| desc.id == spec::ids::VARS) => {
                    self.collect_vars(block);
                }
                decl => rest.push(decl),
            }
        }

        for decl in rest {
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
            self.errors.push(Error::new(
                ErrorKind::EmptyShape {
                    rule: spec::ids::NONEMPTY_SHAPE,
                },
                SrcRange::new(0, 0),
            ));
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

    fn collect_vars(&mut self, block: ast::Block) {
        let desc = spec::SPEC
            .block_by_id(spec::ids::VARS)
            .expect("the spec describes the vars block");
        let ast::Block { name, decls, range, .. } = block;
        self.model_name(name, range, desc);

        for decl in decls {
            match decl {
                ast::Decl::Block(inner) => {
                    let parent = spec::Place::Block { id: spec::ids::VARS };
                    let error = match spec::SPEC.block(&inner.kind) {
                        Some(desc) => ErrorKind::BlockNotAllowed { block: desc.id, parent },
                        None => ErrorKind::UnknownBlock {
                            keyword: inner.kind,
                            parent,
                        },
                    };
                    self.errors.push(Error::new(error, inner.range));
                }
                ast::Decl::Attr(attr) => {
                    if expr_references_vars(&attr.value) {
                        self.errors.push(Error::new(
                            ErrorKind::VarInVar {
                                rule: spec::ids::STATIC_VARS,
                                name: attr.key,
                            },
                            attr.range,
                        ));
                    } else {
                        match self.vars.entry(attr.key) {
                            Entry::Occupied(entry) => {
                                self.errors.push(Error::new(
                                    ErrorKind::DuplicateVar {
                                        rule: spec::ids::UNIQUE_VARS,
                                        name: entry.key().clone(),
                                    },
                                    attr.range,
                                ));
                            }
                            Entry::Vacant(entry) => {
                                entry.insert(attr.value);
                            }
                        }
                    }
                }
            }
        }
    }

    // swaps var.<name> refs in before validation so all the usual checks hit the substituted value
    fn resolve_expr(&mut self, expr: ast::Expr) -> Option<ast::Expr> {
        let ast::Expr { kind, range } = expr;
        let kind = match kind {
            // use-site range wins so diags point at the ref, not the definition
            ast::ExprKind::VarRef(name) => self.lookup_var(name, range)?.kind,
            ast::ExprKind::Template(parts) => ast::ExprKind::Template(self.resolve_template_parts(parts)?),
            ast::ExprKind::Array(values) => {
                let mut resolved = Vec::with_capacity(values.len());
                let mut valid = true;
                for value in values {
                    match self.resolve_expr(value) {
                        Some(value) => resolved.push(value),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Array(resolved)
            }
            ast::ExprKind::Object(attrs) => {
                let mut resolved = Vec::with_capacity(attrs.len());
                let mut valid = true;
                for attr in attrs {
                    match self.resolve_expr(attr.value) {
                        Some(value) => resolved.push(ast::Attr { value, ..attr }),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Object(resolved)
            }
            kind => kind,
        };

        Some(ast::Expr::new(kind, range))
    }

    fn resolve_template_parts(&mut self, parts: Vec<ast::TemplatePart>) -> Option<Vec<ast::TemplatePart>> {
        let mut resolved = Vec::with_capacity(parts.len());
        let mut valid = true;

        for part in parts {
            match part {
                ast::TemplatePart::Ref { path, range } if path.len() == 2 && path[0] == "var" => {
                    let name = path.into_iter().nth(1).expect("path has two segments");
                    let Some(value) = self.lookup_var(name.clone(), range) else {
                        valid = false;
                        continue;
                    };

                    match value.kind {
                        ast::ExprKind::Str(text) => resolved.push(ast::TemplatePart::Lit(text)),
                        ast::ExprKind::Num(raw) => resolved.push(ast::TemplatePart::Lit(raw)),
                        ast::ExprKind::Bool(value) => {
                            resolved.push(ast::TemplatePart::Lit(if value { "true" } else { "false" }.to_owned()));
                        }
                        // a var thats itself a template splices inline
                        ast::ExprKind::Template(parts) => resolved.extend(parts),
                        _ => {
                            self.errors.push(Error::new(
                                ErrorKind::NonScalarInterpolation {
                                    rule: spec::ids::SCALAR_INTERPOLATION,
                                    name,
                                },
                                range,
                            ));
                            valid = false;
                        }
                    }
                }
                part => resolved.push(part),
            }
        }

        valid.then_some(resolved)
    }

    fn lookup_var(&mut self, name: String, range: SrcRange) -> Option<ast::Expr> {
        match self.vars.get(&name) {
            Some(value) => Some(value.clone()),
            None => {
                self.errors.push(Error::new(
                    ErrorKind::UnknownVariable {
                        rule: spec::ids::DEFINED_VARS,
                        name,
                    },
                    range,
                ));
                None
            }
        }
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

    fn model_body(&mut self, decls: Vec<ast::Decl>, block: &spec::BlockDesc, range: SrcRange) -> (SpanFields, Vec<ast::Block>) {
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

        let Some(value) = self.resolve_expr(attr.value) else {
            return;
        };

        if !self.validate_expr(&value, block.id, field.id, field.value) {
            return;
        }

        if field.id == spec::ids::METRICS && !self.validate_metric_keys(&value) {
            return;
        }

        let Some(value) = self.model_field_value(value, field.value) else {
            return;
        };

        match (field.id, value) {
            (spec::ids::INPUT, FieldValue::Value(value)) => fields.input = Some(value),
            (spec::ids::OUTPUT, FieldValue::Value(value)) => fields.output = Some(value),
            (spec::ids::METADATA, FieldValue::Object(value)) => fields.metadata = Some(value),
            (spec::ids::METRICS, FieldValue::Object(value)) => fields.metrics = Some(value),
            (spec::ids::TAGS, FieldValue::Tags(value)) => fields.tags = Some(value),
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
    fn validate_expr(&mut self, expr: &ast::Expr, block: spec::Id, field: spec::Id, expected: &'static spec::ExprType) -> bool {
        let valid = match expected {
            spec::ExprType::Any => true,
            spec::ExprType::String => matches!(expr.kind, ast::ExprKind::Str(_) | ast::ExprKind::Template(_)),
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
                self.require_tags(expr).map(FieldValue::Tags)
            }
            _ => unreachable!("expression constraint does not have a model lowering"),
        }
    }

    fn model_value(&mut self, expr: ast::Expr) -> Option<Value> {
        let ast::Expr { kind, range } = expr;
        match kind {
            ast::ExprKind::Str(value) => Some(Value::Str(value)),
            ast::ExprKind::Template(parts) => self.model_template(parts).map(collapse_template),
            ast::ExprKind::Bool(value) => Some(Value::Bool(value)),
            ast::ExprKind::Null => Some(Value::Null),
            ast::ExprKind::Num(value) => self.model_number(value, range).map(Value::Num),
            ast::ExprKind::Array(values) => Some(Value::Array(Array {
                elem: values.into_iter().filter_map(|value| self.model_value(value)).collect(),
            })),
            ast::ExprKind::Object(attrs) => Some(Value::Object(self.model_object(attrs))),
            ast::ExprKind::VarRef(_) => unreachable!("variable references are resolved before model lowering"),
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

    fn require_tags(&mut self, expr: ast::Expr) -> Option<Vec<Template>> {
        let ast::Expr { kind, .. } = expr;
        let ast::ExprKind::Array(values) = kind else {
            unreachable!("expression was validated as an array of strings");
        };

        let mut tags = Vec::new();
        let mut valid = true;

        for value in values {
            match value.kind {
                ast::ExprKind::Str(value) => tags.push(Template {
                    parts: vec![Part::Lit(value)],
                }),
                ast::ExprKind::Template(parts) => match self.model_template(parts) {
                    Some(template) => tags.push(template),
                    None => valid = false,
                },
                _ => unreachable!("array item was validated as a string"),
            }
        }

        valid.then_some(tags)
    }

    fn model_template(&mut self, parts: Vec<ast::TemplatePart>) -> Option<Template> {
        let mut modeled = Vec::with_capacity(parts.len());
        let mut valid = true;

        for part in parts {
            match part {
                ast::TemplatePart::Lit(value) => modeled.push(Part::Lit(value)),
                ast::TemplatePart::Ref { path, range } => match model_ctx_ref(&path) {
                    Some(ctx_ref) => modeled.push(Part::Ref(ctx_ref)),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::UnknownReference {
                                rule: spec::ids::KNOWN_REFERENCES,
                                path: path.join("."),
                            },
                            range,
                        ));
                        valid = false;
                    }
                },
            }
        }

        valid.then_some(Template { parts: modeled })
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
            spec::NameDesc::Forbidden => None,
        }
    }
}

fn expr_references_vars(expr: &ast::Expr) -> bool {
    match &expr.kind {
        ast::ExprKind::VarRef(_) => true,
        ast::ExprKind::Template(parts) => parts.iter().any(
            |part| matches!(part, ast::TemplatePart::Ref { path, .. } if path.first().is_some_and(|segment| segment == "var")),
        ),
        ast::ExprKind::Array(values) => values.iter().any(expr_references_vars),
        ast::ExprKind::Object(attrs) => attrs.iter().any(|attr| expr_references_vars(&attr.value)),
        _ => false,
    }
}

fn model_ctx_ref(path: &[String]) -> Option<CtxRef> {
    match path {
        [first, second] if first == "trace" && second == "index" => Some(CtxRef::TraceIndex),
        _ => None,
    }
}

// splicing can leave a template all lits, fold those back into a plain string so
// downstream only sees templates that actually resolve
fn collapse_template(template: Template) -> Value {
    if template.parts.iter().all(|part| matches!(part, Part::Lit(_))) {
        let joined = template
            .parts
            .into_iter()
            .map(|part| match part {
                Part::Lit(value) => value,
                Part::Ref(_) => unreachable!("all parts are literal"),
            })
            .collect();
        Value::Str(joined)
    } else {
        Value::Template(template)
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
    tags: Option<Vec<Template>>,
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
    UnknownReference {
        rule: spec::Id,
        path: String,
    },
    DuplicateVar {
        rule: spec::Id,
        name: String,
    },
    VarInVar {
        rule: spec::Id,
        name: String,
    },
    UnknownVariable {
        rule: spec::Id,
        name: String,
    },
    NonScalarInterpolation {
        rule: spec::Id,
        name: String,
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
            Self::UnknownReference { rule, path } => {
                let rule = rule_desc(*rule);
                write!(formatter, "unknown reference `${{{path}}}`; {}", rule.summary)
            }
            Self::DuplicateVar { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "variable `{name}` is defined more than once; {}", rule.summary)
            }
            Self::VarInVar { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "variable `{name}` references another variable; {}", rule.summary)
            }
            Self::UnknownVariable { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "unknown variable `{name}`; {}", rule.summary)
            }
            Self::NonScalarInterpolation { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "variable `{name}` cannot be interpolated; {}", rule.summary)
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

    fn tag_text(tag: &Template) -> &str {
        match tag.parts.as_slice() {
            [Part::Lit(value)] => value,
            _ => panic!("expected a literal tag"),
        }
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
        assert_eq!(trace.fields.tags.iter().map(tag_text).collect::<Vec<_>>(), ["chat", "prod"]);
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
    fn models_null_and_negative_numbers() {
        let model = model(r#"trace "example" { input = null metrics = { delta = -0.5 offset = -3 } }"#).unwrap();

        let fields = &model.traces[0].fields;
        assert!(matches!(fields.input, Some(Value::Null)));

        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Float(value)) if value == -0.5));
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Int(-3))));
    }

    #[test]
    fn models_templates_with_known_references() {
        let model = model(r#"trace "example" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let fields = &model.traces[0].fields;

        let Some(Value::Template(template)) = &fields.input else {
            panic!("expected a template");
        };
        assert!(matches!(&template.parts[0], Part::Lit(value) if value == "q "));
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::TraceIndex)));

        assert_eq!(fields.tags.len(), 1);
        assert!(matches!(fields.tags[0].parts[1], Part::Ref(CtxRef::TraceIndex)));
    }

    #[test]
    fn rejects_unknown_references() {
        let source = r#"trace "example" { input = "${trace.idx}" }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownReference {
                rule: spec::ids::KNOWN_REFERENCES,
                path: "trace.idx".to_owned(),
            }
        );
        assert!(errors[0].to_string().contains("unknown reference `${trace.idx}`"));

        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "${trace.idx}");
    }

    #[test]
    fn treats_templates_as_strings_in_type_checks() {
        let errors = model(r#"trace "example" { metadata = "${trace.index}" }"#).unwrap_err();

        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::String,
                ..
            }
        ));
    }

    #[test]
    fn resolves_variables_in_scalar_object_and_array_positions() {
        let model = model(
            r#"
            vars {
                greeting = "hey"
                meta = { model = "gpt" }
                n = 4
            }
            trace "example" {
                input = var.greeting
                output = [var.n, var.n]
                metadata = var.meta
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert!(matches!(&fields.input, Some(Value::Str(value)) if value == "hey"));

        let Some(Value::Array(output)) = &fields.output else {
            panic!("expected an array");
        };
        assert!(matches!(output.elem[1], Value::Num(Number::Int(4))));

        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem[0].key, "model");
        assert!(matches!(&metadata.elem[0].value, Value::Str(value) if value == "gpt"));
    }

    #[test]
    fn interpolates_scalar_variables_and_collapses_static_templates() {
        let model = model(
            r#"
            vars { m = "gpt" t = 0.2 f = false }
            trace "example" { input = "m=${var.m} t=${var.t} f=${var.f}" }
            "#,
        )
        .unwrap();

        assert!(matches!(
            &model.traces[0].fields.input,
            Some(Value::Str(value)) if value == "m=gpt t=0.2 f=false"
        ));
    }

    #[test]
    fn splices_template_variables_and_keeps_context_references() {
        let model = model(
            r#"
            vars { q = "q ${trace.index}" }
            trace "example" { input = "${var.q}!" }
            "#,
        )
        .unwrap();

        let Some(Value::Template(template)) = &model.traces[0].fields.input else {
            panic!("expected a template");
        };
        assert!(matches!(&template.parts[0], Part::Lit(value) if value == "q "));
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::TraceIndex)));
        assert!(matches!(&template.parts[2], Part::Lit(value) if value == "!"));
    }

    #[test]
    fn resolves_variables_defined_after_their_use() {
        let model = model(
            r#"
            trace "example" { input = var.greeting }
            vars { greeting = "hey" }
            "#,
        )
        .unwrap();

        assert!(matches!(&model.traces[0].fields.input, Some(Value::Str(value)) if value == "hey"));
    }

    #[test]
    fn reports_variable_type_mismatches_at_the_use_site() {
        let source = r#"vars { m = true } trace "example" { metadata = var.m }"#;
        let errors = model(source).unwrap_err();

        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Boolean,
                ..
            }
        ));
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "var.m");
    }

    #[test]
    fn rejects_unknown_variables() {
        for source in [
            r#"trace "example" { input = var.missing }"#,
            r#"trace "example" { input = "${var.missing}" }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::UnknownVariable {
                    rule: spec::ids::DEFINED_VARS,
                    name: "missing".to_owned(),
                }
            );
        }
    }

    #[test]
    fn rejects_duplicate_variables_across_vars_blocks() {
        let errors = model(r#"vars { a = 1 } vars { a = 2 } trace "example" {}"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateVar {
                rule: spec::ids::UNIQUE_VARS,
                name: "a".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_variables_referencing_other_variables() {
        for source in [
            r#"vars { a = 1 b = var.a } trace "example" {}"#,
            r#"vars { a = 1 b = "${var.a}" } trace "example" {}"#,
            r#"vars { a = 1 b = { c = var.a } } trace "example" {}"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::VarInVar {
                    rule: spec::ids::STATIC_VARS,
                    name: "b".to_owned(),
                }
            );
        }
    }

    #[test]
    fn rejects_interpolating_non_scalar_variables() {
        let source = r#"vars { m = { x = 1 } } trace "example" { input = "${var.m}" }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonScalarInterpolation {
                rule: spec::ids::SCALAR_INTERPOLATION,
                name: "m".to_owned(),
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "${var.m}");
    }

    #[test]
    fn rejects_named_vars_blocks_and_nested_blocks_in_vars() {
        let named = model(r#"vars "named" {} trace "example" {}"#).unwrap_err();
        assert_eq!(named[0].kind(), &ErrorKind::UnexpectedName { block: spec::ids::VARS });

        let nested = model(r#"vars { task "t" {} } trace "example" {}"#).unwrap_err();
        assert_eq!(
            nested[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::TASK,
                parent: spec::Place::Block { id: spec::ids::VARS },
            }
        );
    }

    #[test]
    fn rejects_null_where_a_specific_type_is_expected() {
        let errors = model(r#"trace "example" { tags = [null] }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::TRACE,
                field: spec::ids::TAGS,
                expected: &spec::ExprType::String,
                found: ExprType::Null,
            }
        );
        assert!(errors[0].to_string().contains("expects string, but found null"));
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
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::EmptyShape {
                    rule: spec::ids::NONEMPTY_SHAPE
                }
            );
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
