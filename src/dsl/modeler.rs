use crate::dsl::ast;
use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::model::{
    Array, Child, Choice, CtxRef, Func, Maybe, Model, Number, Object, ObjectField, Part, Range, Repeat, Span, SpanFields,
    SpanKind, Template, Trace, Value, WeightedOption,
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
    Func,
    // a residual expr whose concrete type is only known during generation
    Abstract,
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
            ast::ExprKind::Func { .. } => Self::Func,
            // constant operator, index, and slice exprs fold to literals, only dynamic ones get here
            ast::ExprKind::Unary { .. }
            | ast::ExprKind::Binary { .. }
            | ast::ExprKind::Cond { .. }
            | ast::ExprKind::Index { .. }
            | ast::ExprKind::Slice { .. } => Self::Abstract,
            ast::ExprKind::VarRef(_) => unreachable!("variable references are resolved before type checks"),
            ast::ExprKind::LoopRef(_) => unreachable!("loop references are substituted before type checks"),
            ast::ExprKind::Spread(_) => unreachable!("spreads are spliced before type checks"),
            ast::ExprKind::For { .. } => unreachable!("for expressions are unrolled before type checks"),
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
            Self::Func => "function",
            Self::Abstract => "expression",
        })
    }
}

pub(super) struct Modeler {
    ast: ast::Ast,
    vars: HashMap<String, ast::Expr>,
    errors: Errors,
    // how many repeat blocks enclose the decl being lowered, gates repeat.index
    repeat_depth: usize,
}

impl Modeler {
    fn new(ast: ast::Ast) -> Self {
        Self {
            ast,
            vars: HashMap::new(),
            errors: Vec::new(),
            repeat_depth: 0,
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
            .filter_map(|block| self.model_child(block, desc.id))
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
            ast::ExprKind::Object(items) => {
                let mut resolved = Vec::with_capacity(items.len());
                let mut valid = true;
                for item in items {
                    let item = match item {
                        ast::ObjectItem::Attr(attr) => self
                            .resolve_expr(attr.value)
                            .map(|value| ast::ObjectItem::Attr(ast::Attr { value, ..attr })),
                        ast::ObjectItem::Spread(operand) => self.resolve_expr(operand).map(ast::ObjectItem::Spread),
                    };
                    match item {
                        Some(item) => resolved.push(item),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Object(resolved)
            }
            ast::ExprKind::Spread(operand) => ast::ExprKind::Spread(Box::new(self.resolve_expr(*operand)?)),
            ast::ExprKind::Func { name, args } => {
                let mut resolved = Vec::with_capacity(args.len());
                let mut valid = true;
                for arg in args {
                    match self.resolve_expr(arg) {
                        Some(arg) => resolved.push(arg),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Func { name, args: resolved }
            }
            ast::ExprKind::Unary { op, operand } => ast::ExprKind::Unary {
                op,
                operand: Box::new(self.resolve_expr(*operand)?),
            },
            ast::ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.resolve_expr(*lhs);
                let rhs = self.resolve_expr(*rhs);
                ast::ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs?),
                    rhs: Box::new(rhs?),
                }
            }
            ast::ExprKind::Cond { cond, then, otherwise } => {
                let cond = self.resolve_expr(*cond);
                let then = self.resolve_expr(*then);
                let otherwise = self.resolve_expr(*otherwise);
                ast::ExprKind::Cond {
                    cond: Box::new(cond?),
                    then: Box::new(then?),
                    otherwise: Box::new(otherwise?),
                }
            }
            ast::ExprKind::Index { target, index } => {
                let target = self.resolve_expr(*target);
                let index = self.resolve_expr(*index);
                ast::ExprKind::Index {
                    target: Box::new(target?),
                    index: Box::new(index?),
                }
            }
            ast::ExprKind::Slice { target, start, end } => {
                let target = self.resolve_expr(*target);
                let start = start.map(|bound| self.resolve_expr(*bound));
                let end = end.map(|bound| self.resolve_expr(*bound));
                ast::ExprKind::Slice {
                    target: Box::new(target?),
                    start: match start {
                        Some(bound) => Some(Box::new(bound?)),
                        None => None,
                    },
                    end: match end {
                        Some(bound) => Some(Box::new(bound?)),
                        None => None,
                    },
                }
            }
            // loop refs stay put, they substitute during unrolling
            ast::ExprKind::For {
                bindings,
                collection,
                key,
                body,
                cond,
            } => {
                let collection = self.resolve_expr(*collection);
                let key = key.map(|key| self.resolve_expr(*key));
                let body = self.resolve_expr(*body);
                let cond = cond.map(|cond| self.resolve_expr(*cond));
                ast::ExprKind::For {
                    bindings,
                    collection: Box::new(collection?),
                    key: match key {
                        Some(key) => Some(Box::new(key?)),
                        None => None,
                    },
                    body: Box::new(body?),
                    cond: match cond {
                        Some(cond) => Some(Box::new(cond?)),
                        None => None,
                    },
                }
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

                    // constant exprs interpolate as their folded literal
                    let Some(value) = self.fold_expr(value) else {
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

    // folds constant operator exprs to literals after var resolution, dynamic
    // ones keep folded operands and lower to residual model values
    fn fold_expr(&mut self, expr: ast::Expr) -> Option<ast::Expr> {
        let ast::Expr { kind, range } = expr;
        let kind = match kind {
            ast::ExprKind::Unary { op, operand } => return self.fold_unary(op, *operand, range),
            ast::ExprKind::Binary { op, lhs, rhs } => return self.fold_binary(op, *lhs, *rhs, range),
            ast::ExprKind::Cond { cond, then, otherwise } => return self.fold_cond(*cond, *then, *otherwise, range),
            ast::ExprKind::Index { target, index } => return self.fold_index(*target, *index, range),
            ast::ExprKind::Slice { target, start, end } => return self.fold_slice(*target, start, end, range),
            ast::ExprKind::For {
                bindings,
                collection,
                key,
                body,
                cond,
            } => return self.fold_for(bindings, *collection, key, *body, cond, range),
            ast::ExprKind::Array(values) => {
                let mut folded = Vec::with_capacity(values.len());
                let mut valid = true;
                for value in values {
                    match value.kind {
                        // constant spreads splice in place
                        ast::ExprKind::Spread(operand) => match self.fold_array_spread(*operand) {
                            Some(values) => folded.extend(values),
                            None => valid = false,
                        },
                        _ => match self.fold_expr(value) {
                            Some(value) => folded.push(value),
                            None => valid = false,
                        },
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Array(folded)
            }
            ast::ExprKind::Object(items) => {
                // spreads merge with later keys winning, plain objects keep their
                // entries so duplicate keys still diagnose in model_object
                if items.iter().any(|item| matches!(item, ast::ObjectItem::Spread(_))) {
                    return self.fold_object_merge(items, range);
                }

                let mut folded = Vec::with_capacity(items.len());
                let mut valid = true;
                for item in items {
                    let ast::ObjectItem::Attr(attr) = item else {
                        unreachable!("spread items take the merging path");
                    };
                    match self.fold_expr(attr.value) {
                        Some(value) => folded.push(ast::ObjectItem::Attr(ast::Attr { value, ..attr })),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Object(folded)
            }
            ast::ExprKind::Spread(_) => unreachable!("spreads only parse inside arrays and objects"),
            ast::ExprKind::LoopRef(_) => unreachable!("loop references are substituted before folding"),
            ast::ExprKind::Func { name, args } => {
                let mut folded = Vec::with_capacity(args.len());
                let mut valid = true;
                for arg in args {
                    match self.fold_expr(arg) {
                        Some(arg) => folded.push(arg),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Func { name, args: folded }
            }
            kind => kind,
        };

        Some(ast::Expr::new(kind, range))
    }

    fn fold_unary(&mut self, op: ast::UnaryOp, operand: ast::Expr, range: SrcRange) -> Option<ast::Expr> {
        let operand = self.fold_expr(operand)?;

        // negating a number literal happens textually so i64::MIN stays representable
        if op == ast::UnaryOp::Neg
            && let ast::ExprKind::Num(raw) = operand.kind
        {
            return Some(ast::Expr::new(ast::ExprKind::Num(negate_literal(&raw)), range));
        }

        let required = match op {
            ast::UnaryOp::Neg => StaticType::Number,
            ast::UnaryOp::Not => StaticType::Boolean,
        };
        if !self.check_operand(&operand, required, op.to_string()) {
            return None;
        }

        match operand.kind {
            // a constant not folds, neg of a literal already returned above
            ast::ExprKind::Bool(value) if op == ast::UnaryOp::Not => Some(ast::Expr::new(ast::ExprKind::Bool(!value), range)),
            kind => Some(ast::Expr::new(
                ast::ExprKind::Unary {
                    op,
                    operand: Box::new(ast::Expr::new(kind, operand.range)),
                },
                range,
            )),
        }
    }

    fn fold_binary(&mut self, op: ast::BinOp, lhs: ast::Expr, rhs: ast::Expr, range: SrcRange) -> Option<ast::Expr> {
        // fold both sides first so each reports its own diagnostics
        let lhs = self.fold_expr(lhs);
        let rhs = self.fold_expr(rhs);
        let (lhs, rhs) = (lhs?, rhs?);

        let valid = match op_class(op) {
            OpClass::Arith | OpClass::Cmp => {
                let left = self.check_operand(&lhs, StaticType::Number, op.to_string());
                self.check_operand(&rhs, StaticType::Number, op.to_string()) && left
            }
            OpClass::Logic => {
                let left = self.check_operand(&lhs, StaticType::Boolean, op.to_string());
                self.check_operand(&rhs, StaticType::Boolean, op.to_string()) && left
            }
            OpClass::Eq => self.check_equality_operands(&lhs, &rhs, op),
        };
        if !valid {
            return None;
        }

        // a constant zero divisor is always an error, even with a dynamic lhs
        if matches!(op, ast::BinOp::Div | ast::BinOp::Rem)
            && let ast::ExprKind::Num(raw) = &rhs.kind
            && raw.parse::<f64>().is_ok_and(|value| value == 0.0)
        {
            self.errors.push(Error::new(
                ErrorKind::DivisionByZero {
                    rule: spec::ids::NONZERO_DIVISORS,
                    op: op.to_string(),
                },
                rhs.range,
            ));
            return None;
        }

        if expr_is_constant(&lhs) && expr_is_constant(&rhs) {
            let left = self.const_scalar(&lhs);
            let right = self.const_scalar(&rhs)?;
            return self
                .eval_binary(op, left?, right, range)
                .map(|kind| ast::Expr::new(kind, range));
        }

        Some(ast::Expr::new(
            ast::ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            range,
        ))
    }

    fn fold_cond(&mut self, cond: ast::Expr, then: ast::Expr, otherwise: ast::Expr, range: SrcRange) -> Option<ast::Expr> {
        let cond = self.fold_expr(cond);
        let then = self.fold_expr(then);
        let otherwise = self.fold_expr(otherwise);
        let (cond, then, otherwise) = (cond?, then?, otherwise?);

        if static_type(&cond) != Some(StaticType::Boolean) {
            self.errors.push(Error::new(
                ErrorKind::NonBooleanCondition {
                    rule: spec::ids::BOOLEAN_CONDITIONS,
                    found: found_type(&cond),
                },
                cond.range,
            ));
            return None;
        }

        // a constant condition picks its branch during validation
        if let ast::ExprKind::Bool(value) = cond.kind {
            let taken = if value { then } else { otherwise };
            return Some(ast::Expr::new(taken.kind, range));
        }

        Some(ast::Expr::new(
            ast::ExprKind::Cond {
                cond: Box::new(cond),
                then: Box::new(then),
                otherwise: Box::new(otherwise),
            },
            range,
        ))
    }

    fn fold_index(&mut self, target: ast::Expr, index: ast::Expr, range: SrcRange) -> Option<ast::Expr> {
        // fold both sides first so each reports its own diagnostics
        let target = self.fold_expr(target);
        let index = self.fold_expr(index);
        let (target, index) = (target?, index?);

        let expected = match static_type(&target) {
            Some(StaticType::Array) => StaticType::Number,
            Some(StaticType::Object) => StaticType::String,
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::NonIndexableTarget {
                        rule: spec::ids::INDEXABLE_TARGETS,
                        found: found_type(&target),
                    },
                    target.range,
                ));
                return None;
            }
        };

        if static_type(&index) != Some(expected) {
            self.errors.push(Error::new(
                ErrorKind::IndexTypeMismatch {
                    rule: spec::ids::INDEXABLE_TARGETS,
                    expected: type_name(expected),
                    found: found_type(&index),
                },
                index.range,
            ));
            return None;
        }

        // a constant index into a container literal selects its element during
        // validation, the element itself may still be dynamic
        match target.kind {
            ast::ExprKind::Array(values) if expr_is_constant(&index) => {
                let Const::Num(number) = self.const_scalar(&index)? else {
                    unreachable!("index was type checked as a number");
                };
                let Number::Int(position) = number else {
                    self.errors.push(Error::new(
                        ErrorKind::NonIntegerIndex {
                            rule: spec::ids::INDEXABLE_TARGETS,
                        },
                        index.range,
                    ));
                    return None;
                };

                match usize::try_from(position).ok().filter(|&position| position < values.len()) {
                    Some(position) => {
                        let selected = values.into_iter().nth(position).expect("position is in bounds");
                        Some(ast::Expr::new(selected.kind, range))
                    }
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::IndexOutOfBounds {
                                rule: spec::ids::INDEX_BOUNDS,
                                index: position,
                                len: values.len(),
                            },
                            index.range,
                        ));
                        None
                    }
                }
            }
            ast::ExprKind::Object(items) if expr_is_constant(&index) => {
                let Const::Str(key) = self.const_scalar(&index)? else {
                    unreachable!("index was type checked as a string");
                };

                let selected = items.into_iter().find_map(|item| match item {
                    ast::ObjectItem::Attr(attr) if attr.key == key => Some(attr.value),
                    _ => None,
                });
                match selected {
                    Some(value) => Some(ast::Expr::new(value.kind, range)),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::UnknownObjectKey {
                                rule: spec::ids::INDEX_BOUNDS,
                                key,
                            },
                            index.range,
                        ));
                        None
                    }
                }
            }
            kind => Some(ast::Expr::new(
                ast::ExprKind::Index {
                    target: Box::new(ast::Expr::new(kind, target.range)),
                    index: Box::new(index),
                },
                range,
            )),
        }
    }

    fn fold_array_spread(&mut self, operand: ast::Expr) -> Option<Vec<ast::Expr>> {
        let operand = self.fold_expr(operand)?;
        match operand.kind {
            ast::ExprKind::Array(values) => Some(values),
            kind => {
                let operand = ast::Expr::new(kind, operand.range);
                self.errors.push(Error::new(
                    ErrorKind::SpreadTypeMismatch {
                        rule: spec::ids::SPREAD_OPERANDS,
                        expected: "array",
                        found: ExprType::of(&operand),
                    },
                    operand.range,
                ));
                None
            }
        }
    }

    fn fold_object_spread(&mut self, operand: ast::Expr) -> Option<Vec<ast::Attr>> {
        let operand = self.fold_expr(operand)?;
        match operand.kind {
            ast::ExprKind::Object(items) => Some(
                items
                    .into_iter()
                    .map(|item| match item {
                        ast::ObjectItem::Attr(attr) => attr,
                        ast::ObjectItem::Spread(_) => unreachable!("folded objects only hold attrs"),
                    })
                    .collect(),
            ),
            kind => {
                let operand = ast::Expr::new(kind, operand.range);
                self.errors.push(Error::new(
                    ErrorKind::SpreadTypeMismatch {
                        rule: spec::ids::SPREAD_OPERANDS,
                        expected: "object",
                        found: ExprType::of(&operand),
                    },
                    operand.range,
                ));
                None
            }
        }
    }

    // later entries win over keys a spread introduced, two explicit keys still collide
    fn fold_object_merge(&mut self, items: Vec<ast::ObjectItem>, range: SrcRange) -> Option<ast::Expr> {
        let mut merged: Vec<(ast::Attr, bool)> = Vec::new();
        let mut valid = true;

        for item in items {
            match item {
                ast::ObjectItem::Attr(attr) => match self.fold_expr(attr.value) {
                    Some(value) => {
                        let attr = ast::Attr { value, ..attr };
                        match merged.iter_mut().find(|(existing, _)| existing.key == attr.key) {
                            Some((_, true)) => {
                                self.errors.push(Error::new(
                                    ErrorKind::DuplicateObjectKey {
                                        rule: spec::ids::UNIQUE_OBJECT_KEYS,
                                        key: attr.key,
                                    },
                                    attr.range,
                                ));
                                valid = false;
                            }
                            Some(slot) => *slot = (attr, true),
                            None => merged.push((attr, true)),
                        }
                    }
                    None => valid = false,
                },
                ast::ObjectItem::Spread(operand) => match self.fold_object_spread(operand) {
                    Some(attrs) => {
                        for attr in attrs {
                            match merged.iter_mut().find(|(existing, _)| existing.key == attr.key) {
                                Some(slot) => *slot = (attr, false),
                                None => merged.push((attr, false)),
                            }
                        }
                    }
                    None => valid = false,
                },
            }
        }

        if !valid {
            return None;
        }

        let items = merged.into_iter().map(|(attr, _)| ast::ObjectItem::Attr(attr)).collect();
        Some(ast::Expr::new(ast::ExprKind::Object(items), range))
    }

    fn fold_slice(
        &mut self,
        target: ast::Expr,
        start: Option<Box<ast::Expr>>,
        end: Option<Box<ast::Expr>>,
        range: SrcRange,
    ) -> Option<ast::Expr> {
        // fold every part first so each reports its own diagnostics
        let target = self.fold_expr(target);
        let start = start.map(|bound| self.fold_expr(*bound));
        let end = end.map(|bound| self.fold_expr(*bound));
        let target = target?;
        let start = match start {
            Some(bound) => Some(bound?),
            None => None,
        };
        let end = match end {
            Some(bound) => Some(bound?),
            None => None,
        };

        if static_type(&target) != Some(StaticType::Array) {
            self.errors.push(Error::new(
                ErrorKind::NonSliceableTarget {
                    rule: spec::ids::SLICEABLE_TARGETS,
                    found: found_type(&target),
                },
                target.range,
            ));
            return None;
        }

        let mut valid = true;
        for bound in [&start, &end].into_iter().flatten() {
            if static_type(bound) != Some(StaticType::Number) {
                self.errors.push(Error::new(
                    ErrorKind::SliceTypeMismatch {
                        rule: spec::ids::SLICE_BOUNDS,
                        found: found_type(bound),
                    },
                    bound.range,
                ));
                valid = false;
            }
        }
        if !valid {
            return None;
        }

        // constant bounds must check out even when the target is dynamic
        let start_const = match &start {
            Some(bound) => self.check_slice_bound(bound),
            None => Some(Some(0)),
        };
        let end_const = match &end {
            Some(bound) => self.check_slice_bound(bound),
            None => Some(None),
        };
        let (start_const, end_const) = match (start_const, end_const) {
            (Some(start), Some(end)) => (start, end),
            _ => return None,
        };

        // a constant slice of a literal selects its elements during validation,
        // the elements themselves may still be dynamic
        let known = start_const.is_some() && (end_const.is_some() || end.is_none());
        if known && matches!(target.kind, ast::ExprKind::Array(_)) {
            let ast::ExprKind::Array(values) = target.kind else {
                unreachable!("the target was just matched as an array literal");
            };
            let len = values.len();
            let start = start_const.expect("a known slice has a constant start").min(len);
            let end = end_const.unwrap_or(len).min(len);
            let selected = if start >= end {
                Vec::new()
            } else {
                values.into_iter().skip(start).take(end - start).collect()
            };
            return Some(ast::Expr::new(ast::ExprKind::Array(selected), range));
        }

        Some(ast::Expr::new(
            ast::ExprKind::Slice {
                target: Box::new(target),
                start: start.map(Box::new),
                end: end.map(Box::new),
            },
            range,
        ))
    }

    // some(none) = dynamic, checked during generation, none = diagnostic pushed
    fn check_slice_bound(&mut self, bound: &ast::Expr) -> Option<Option<usize>> {
        if !expr_is_constant(bound) {
            return Some(None);
        }

        let Const::Num(number) = self.const_scalar(bound)? else {
            unreachable!("bound was type checked as a number");
        };
        let Number::Int(value) = number else {
            self.errors.push(Error::new(
                ErrorKind::NonIntegerSliceBound {
                    rule: spec::ids::SLICE_BOUNDS,
                },
                bound.range,
            ));
            return None;
        };

        match usize::try_from(value) {
            Ok(value) => Some(Some(value)),
            Err(_) => {
                self.errors.push(Error::new(
                    ErrorKind::NegativeSliceBound {
                        rule: spec::ids::SLICE_BOUNDS,
                    },
                    bound.range,
                ));
                None
            }
        }
    }

    // unrolls comprehensions during validation, bindings substitute by expression like vars
    fn fold_for(
        &mut self,
        bindings: Vec<String>,
        collection: ast::Expr,
        key: Option<Box<ast::Expr>>,
        body: ast::Expr,
        cond: Option<Box<ast::Expr>>,
        range: SrcRange,
    ) -> Option<ast::Expr> {
        let collection = self.fold_expr(collection)?;

        // a folded collection carries folded elements, so substituted exprs stay folded
        let iterations: Vec<HashMap<String, ast::Expr>> = match collection.kind {
            ast::ExprKind::Array(values) => values
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    let mut map = HashMap::new();
                    if bindings.len() == 2 {
                        let index = ast::Expr::new(ast::ExprKind::Num(index.to_string()), value.range);
                        map.insert(bindings[0].clone(), index);
                        map.insert(bindings[1].clone(), value);
                    } else {
                        map.insert(bindings[0].clone(), value);
                    }
                    map
                })
                .collect(),
            ast::ExprKind::Object(items) => items
                .into_iter()
                .map(|item| {
                    let ast::ObjectItem::Attr(attr) = item else {
                        unreachable!("folded objects only hold attrs");
                    };
                    let mut map = HashMap::new();
                    let key = ast::Expr::new(ast::ExprKind::Str(attr.key), attr.range);
                    map.insert(bindings[0].clone(), key);
                    if bindings.len() == 2 {
                        map.insert(bindings[1].clone(), attr.value);
                    }
                    map
                })
                .collect(),
            kind => {
                let collection = ast::Expr::new(kind, collection.range);
                self.errors.push(Error::new(
                    ErrorKind::ForCollectionMismatch {
                        rule: spec::ids::FOR_COLLECTIONS,
                        found: ExprType::of(&collection),
                    },
                    collection.range,
                ));
                return None;
            }
        };

        let object = key.is_some();
        let mut values = Vec::new();
        let mut attrs = Vec::new();
        let mut valid = true;

        for map in iterations {
            // filter first so skipped elements never fold their bodies
            if let Some(cond) = &cond {
                let cond = self.substitute((**cond).clone(), &map).and_then(|cond| self.fold_expr(cond));
                let Some(cond) = cond else {
                    valid = false;
                    continue;
                };
                match cond.kind {
                    ast::ExprKind::Bool(true) => {}
                    ast::ExprKind::Bool(false) => continue,
                    kind => {
                        let cond = ast::Expr::new(kind, cond.range);
                        self.errors.push(Error::new(
                            ErrorKind::NonConstantForFilter {
                                rule: spec::ids::STATIC_FOR,
                                found: ExprType::of(&cond),
                            },
                            cond.range,
                        ));
                        valid = false;
                        continue;
                    }
                }
            }

            let body = self.substitute(body.clone(), &map).and_then(|body| self.fold_expr(body));
            let Some(body) = body else {
                valid = false;
                continue;
            };

            match &key {
                Some(key) => {
                    let key = self.substitute((**key).clone(), &map).and_then(|key| self.fold_expr(key));
                    let Some(key) = key else {
                        valid = false;
                        continue;
                    };
                    match const_string(&key) {
                        Some(text) => attrs.push(ast::ObjectItem::Attr(ast::Attr {
                            key: text,
                            value: body,
                            range: key.range,
                        })),
                        None => {
                            self.errors.push(Error::new(
                                ErrorKind::NonConstantForKey {
                                    rule: spec::ids::STATIC_FOR,
                                    found: ExprType::of(&key),
                                },
                                key.range,
                            ));
                            valid = false;
                        }
                    }
                }
                None => values.push(body),
            }
        }

        if !valid {
            return None;
        }

        let kind = if object {
            ast::ExprKind::Object(attrs)
        } else {
            ast::ExprKind::Array(values)
        };
        Some(ast::Expr::new(kind, range))
    }

    // swaps loop bindings into a for body, per-iteration values are already folded
    fn substitute(&mut self, expr: ast::Expr, bindings: &HashMap<String, ast::Expr>) -> Option<ast::Expr> {
        let ast::Expr { kind, range } = expr;
        let kind = match kind {
            // use-site range wins so diags point at the ref, not the element
            ast::ExprKind::LoopRef(name) => match bindings.get(&name) {
                Some(value) => value.kind.clone(),
                // an unmatched ref belongs to an inner for, it substitutes later
                None => ast::ExprKind::LoopRef(name),
            },
            ast::ExprKind::Template(parts) => ast::ExprKind::Template(self.substitute_template_parts(parts, bindings)?),
            ast::ExprKind::Array(values) => {
                let mut substituted = Vec::with_capacity(values.len());
                for value in values {
                    substituted.push(self.substitute(value, bindings)?);
                }
                ast::ExprKind::Array(substituted)
            }
            ast::ExprKind::Object(items) => {
                let mut substituted = Vec::with_capacity(items.len());
                for item in items {
                    substituted.push(match item {
                        ast::ObjectItem::Attr(attr) => {
                            let value = self.substitute(attr.value, bindings)?;
                            ast::ObjectItem::Attr(ast::Attr { value, ..attr })
                        }
                        ast::ObjectItem::Spread(operand) => ast::ObjectItem::Spread(self.substitute(operand, bindings)?),
                    });
                }
                ast::ExprKind::Object(substituted)
            }
            ast::ExprKind::Func { name, args } => {
                let mut substituted = Vec::with_capacity(args.len());
                for arg in args {
                    substituted.push(self.substitute(arg, bindings)?);
                }
                ast::ExprKind::Func { name, args: substituted }
            }
            ast::ExprKind::Spread(operand) => ast::ExprKind::Spread(Box::new(self.substitute(*operand, bindings)?)),
            ast::ExprKind::Unary { op, operand } => ast::ExprKind::Unary {
                op,
                operand: Box::new(self.substitute(*operand, bindings)?),
            },
            ast::ExprKind::Binary { op, lhs, rhs } => ast::ExprKind::Binary {
                op,
                lhs: Box::new(self.substitute(*lhs, bindings)?),
                rhs: Box::new(self.substitute(*rhs, bindings)?),
            },
            ast::ExprKind::Cond { cond, then, otherwise } => ast::ExprKind::Cond {
                cond: Box::new(self.substitute(*cond, bindings)?),
                then: Box::new(self.substitute(*then, bindings)?),
                otherwise: Box::new(self.substitute(*otherwise, bindings)?),
            },
            ast::ExprKind::Index { target, index } => ast::ExprKind::Index {
                target: Box::new(self.substitute(*target, bindings)?),
                index: Box::new(self.substitute(*index, bindings)?),
            },
            ast::ExprKind::Slice { target, start, end } => ast::ExprKind::Slice {
                target: Box::new(self.substitute(*target, bindings)?),
                start: match start {
                    Some(bound) => Some(Box::new(self.substitute(*bound, bindings)?)),
                    None => None,
                },
                end: match end {
                    Some(bound) => Some(Box::new(self.substitute(*bound, bindings)?)),
                    None => None,
                },
            },
            ast::ExprKind::For {
                bindings: inner,
                collection,
                key,
                body,
                cond,
            } => {
                let collection = Box::new(self.substitute(*collection, bindings)?);

                // inner bindings shadow outer names in the body
                let visible: HashMap<String, ast::Expr> = bindings
                    .iter()
                    .filter(|(name, _)| !inner.contains(name))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();

                let key = match key {
                    Some(key) => Some(Box::new(self.substitute(*key, &visible)?)),
                    None => None,
                };
                let body = Box::new(self.substitute(*body, &visible)?);
                let cond = match cond {
                    Some(cond) => Some(Box::new(self.substitute(*cond, &visible)?)),
                    None => None,
                };

                ast::ExprKind::For {
                    bindings: inner,
                    collection,
                    key,
                    body,
                    cond,
                }
            }
            kind => kind,
        };

        Some(ast::Expr::new(kind, range))
    }

    fn substitute_template_parts(
        &mut self,
        parts: Vec<ast::TemplatePart>,
        bindings: &HashMap<String, ast::Expr>,
    ) -> Option<Vec<ast::TemplatePart>> {
        let mut substituted = Vec::with_capacity(parts.len());
        let mut valid = true;

        for part in parts {
            match part {
                ast::TemplatePart::Ref { path, range } if path.len() == 1 && bindings.contains_key(&path[0]) => {
                    let name = path.into_iter().next().expect("path has one segment");
                    let value = bindings[&name].clone();

                    match value.kind {
                        ast::ExprKind::Str(text) => substituted.push(ast::TemplatePart::Lit(text)),
                        ast::ExprKind::Num(raw) => substituted.push(ast::TemplatePart::Lit(raw)),
                        ast::ExprKind::Bool(value) => {
                            substituted.push(ast::TemplatePart::Lit(if value { "true" } else { "false" }.to_owned()));
                        }
                        // an element thats itself a template splices inline
                        ast::ExprKind::Template(parts) => substituted.extend(parts),
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
                part => substituted.push(part),
            }
        }

        valid.then_some(substituted)
    }

    fn check_operand(&mut self, operand: &ast::Expr, required: StaticType, op: String) -> bool {
        if static_type(operand) == Some(required) {
            return true;
        }

        self.errors.push(Error::new(
            ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op,
                expected: type_name(required),
                found: found_type(operand),
            },
            operand.range,
        ));
        false
    }

    fn check_equality_operands(&mut self, lhs: &ast::Expr, rhs: &ast::Expr, op: ast::BinOp) -> bool {
        const SCALARS: &str = "string, number, or boolean";
        fn scalar(found: Option<StaticType>) -> bool {
            matches!(found, Some(StaticType::String | StaticType::Number | StaticType::Boolean))
        }

        let lhs_type = static_type(lhs);
        let rhs_type = static_type(rhs);
        let mut valid = true;

        for (side, side_type) in [(lhs, lhs_type), (rhs, rhs_type)] {
            if !scalar(side_type) {
                self.errors.push(Error::new(
                    ErrorKind::OperandTypeMismatch {
                        rule: spec::ids::OPERAND_TYPES,
                        op: op.to_string(),
                        expected: SCALARS,
                        found: found_type(side),
                    },
                    side.range,
                ));
                valid = false;
            }
        }

        // the left operand fixes the comparison type
        if valid && lhs_type != rhs_type {
            self.errors.push(Error::new(
                ErrorKind::OperandTypeMismatch {
                    rule: spec::ids::OPERAND_TYPES,
                    op: op.to_string(),
                    expected: type_name(lhs_type.expect("scalar operands have a static type")),
                    found: found_type(rhs),
                },
                rhs.range,
            ));
            valid = false;
        }

        valid
    }

    fn eval_binary(&mut self, op: ast::BinOp, lhs: Const, rhs: Const, range: SrcRange) -> Option<ast::ExprKind> {
        let kind = match op_class(op) {
            OpClass::Arith => {
                let (Const::Num(lhs), Const::Num(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as numbers");
                };
                match eval_arith(op, lhs, rhs) {
                    Some(number) => ast::ExprKind::Num(num_literal(number)),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::NonFiniteResult {
                                rule: spec::ids::FINITE_NUMBERS,
                            },
                            range,
                        ));
                        return None;
                    }
                }
            }
            OpClass::Cmp => {
                let (Const::Num(lhs), Const::Num(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as numbers");
                };
                ast::ExprKind::Bool(eval_cmp(op, lhs, rhs))
            }
            OpClass::Eq => {
                let equal = match (lhs, rhs) {
                    (Const::Str(lhs), Const::Str(rhs)) => lhs == rhs,
                    (Const::Bool(lhs), Const::Bool(rhs)) => lhs == rhs,
                    (Const::Num(Number::Int(lhs)), Const::Num(Number::Int(rhs))) => lhs == rhs,
                    (Const::Num(lhs), Const::Num(rhs)) => float_bound(lhs) == float_bound(rhs),
                    _ => unreachable!("operands were type checked as matching scalars"),
                };
                ast::ExprKind::Bool(if op == ast::BinOp::Eq { equal } else { !equal })
            }
            OpClass::Logic => {
                let (Const::Bool(lhs), Const::Bool(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as booleans");
                };
                ast::ExprKind::Bool(match op {
                    ast::BinOp::And => lhs && rhs,
                    ast::BinOp::Or => lhs || rhs,
                    _ => unreachable!("operator is logical"),
                })
            }
        };

        Some(kind)
    }

    fn const_scalar(&mut self, expr: &ast::Expr) -> Option<Const> {
        match &expr.kind {
            ast::ExprKind::Str(value) => Some(Const::Str(value.clone())),
            ast::ExprKind::Template(parts) => {
                let joined = parts
                    .iter()
                    .map(|part| match part {
                        ast::TemplatePart::Lit(value) => value.as_str(),
                        ast::TemplatePart::Ref { .. } => unreachable!("constant templates only hold literal parts"),
                    })
                    .collect();
                Some(Const::Str(joined))
            }
            ast::ExprKind::Num(raw) => self.model_number(raw.clone(), expr.range).map(Const::Num),
            ast::ExprKind::Bool(value) => Some(Const::Bool(*value)),
            _ => unreachable!("constant operands are scalar literals"),
        }
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

    fn model_child(&mut self, block: ast::Block, parent: spec::Id) -> Option<Child> {
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

        let name = self.model_name(name, range, desc);

        if desc.id == spec::ids::REPEAT {
            self.model_repeat(name, decls, desc, range).map(Child::Repeat)
        } else if desc.id == spec::ids::CHOICE {
            self.model_choice(name, decls, desc, range).map(Child::Choice)
        } else if desc.id == spec::ids::MAYBE {
            self.model_maybe(name, decls, desc, range).map(Child::Maybe)
        } else {
            self.model_span(name, decls, desc, range).map(Child::Span)
        }
    }

    fn model_span(
        &mut self,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Span> {
        let span_kind = if desc.id == spec::ids::TASK {
            SpanKind::Task
        } else if desc.id == spec::ids::LLM {
            SpanKind::Llm
        } else if desc.id == spec::ids::TOOL {
            SpanKind::Tool
        } else if desc.id == spec::ids::FUNCTION {
            SpanKind::Function
        } else {
            unreachable!("block {} does not have a model lowering", desc.id.as_str());
        };

        let (fields, blocks) = self.model_body(decls, desc, range);
        let children = blocks
            .into_iter()
            .filter_map(|block| self.model_child(block, desc.id))
            .collect();

        name.map(|name| Span {
            name,
            kind: span_kind,
            fields,
            children,
        })
    }

    fn model_repeat(
        &mut self,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Repeat> {
        let (mut fields, blocks) = self.model_dynamic_body(decls, desc, range);

        self.repeat_depth += 1;
        let children = self.model_dynamic_children(blocks, desc, range);
        self.repeat_depth -= 1;

        let (count, count_range) = fields.remove(&spec::ids::COUNT)?;

        // constant counts validate now, dynamic ones fail the run during generation
        if let Value::Num(number) = &count {
            let valid = match number {
                Number::Int(value) => {
                    if *value < 0 {
                        self.errors.push(Error::new(
                            ErrorKind::NegativeRepeatCount {
                                rule: spec::ids::REPEAT_COUNT,
                            },
                            count_range,
                        ));
                    }
                    *value >= 0
                }
                Number::Float(_) => {
                    self.errors.push(Error::new(
                        ErrorKind::NonIntegerRepeatCount {
                            rule: spec::ids::REPEAT_COUNT,
                        },
                        count_range,
                    ));
                    false
                }
            };
            if !valid {
                return None;
            }
        }

        Some(Repeat {
            name,
            count,
            count_range,
            children,
        })
    }

    fn model_choice(
        &mut self,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Choice> {
        let (_, blocks) = self.model_dynamic_body(decls, desc, range);
        let children = self.model_dynamic_children(blocks, desc, range);

        Some(Choice { name, children })
    }

    fn model_maybe(
        &mut self,
        name: Option<String>,
        decls: Vec<ast::Decl>,
        desc: &spec::BlockDesc,
        range: SrcRange,
    ) -> Option<Maybe> {
        let (mut fields, blocks) = self.model_dynamic_body(decls, desc, range);
        let children = self.model_dynamic_children(blocks, desc, range);

        let (chance, chance_range) = fields
            .remove(&spec::ids::CHANCE)
            .unwrap_or((Value::Num(Number::Float(0.5)), range));

        // constant chances validate now, dynamic ones fail the run during generation
        if let Value::Num(number) = &chance {
            let value = match number {
                Number::Int(value) => *value as f64,
                Number::Float(value) => *value,
            };
            if !(0.0..=1.0).contains(&value) {
                self.errors.push(Error::new(
                    ErrorKind::ChanceOutOfRange {
                        rule: spec::ids::MAYBE_CHANCE,
                    },
                    chance_range,
                ));
                return None;
            }
        }

        Some(Maybe {
            name,
            chance,
            chance_range,
            children,
        })
    }

    // mirrors model_body for dynamic blocks: config attrs must be numbers but may
    // stay dynamic, child blocks come back for the caller to recurse
    fn model_dynamic_body(
        &mut self,
        decls: Vec<ast::Decl>,
        block: &spec::BlockDesc,
        range: SrcRange,
    ) -> (HashMap<spec::Id, (Value, SrcRange)>, Vec<ast::Block>) {
        let mut fields = HashMap::new();
        let mut seen = HashSet::new();
        let mut blocks = Vec::new();

        for decl in decls {
            match decl {
                ast::Decl::Block(inner) => blocks.push(inner),
                ast::Decl::Attr(attr) => {
                    let attr_range = attr.range;
                    let Some(field) = block.field(&attr.key) else {
                        self.errors.push(Error::new(
                            ErrorKind::UnknownField {
                                block: block.id,
                                keyword: attr.key,
                            },
                            attr_range,
                        ));
                        continue;
                    };
                    let field_id = field.id;

                    if !seen.insert(field_id) {
                        self.errors.push(Error::new(
                            ErrorKind::DuplicateField {
                                block: block.id,
                                field: field_id,
                            },
                            attr_range,
                        ));
                        continue;
                    }

                    let Some(expr) = self.resolve_expr(attr.value) else {
                        continue;
                    };
                    let Some(expr) = self.fold_expr(expr) else {
                        continue;
                    };

                    if static_type(&expr) != Some(StaticType::Number) {
                        self.errors.push(Error::new(
                            ErrorKind::TypeMismatch {
                                block: block.id,
                                field: field_id,
                                expected: &spec::ExprType::Number,
                                found: found_type(&expr),
                            },
                            expr.range,
                        ));
                        continue;
                    }

                    // diagnostics for bad values point at the value, not the key
                    let value_range = expr.range;
                    if let Some(value) = self.model_value(expr) {
                        fields.insert(field_id, (value, value_range));
                    }
                }
            }
        }

        for field in block.body.fields {
            if field.cardinality == spec::Cardinality::Required && !seen.contains(&field.id) {
                self.errors.push(Error::new(
                    ErrorKind::MissingField {
                        block: block.id,
                        field: field.id,
                    },
                    range,
                ));
            }
        }

        (fields, blocks)
    }

    fn model_dynamic_children(&mut self, blocks: Vec<ast::Block>, block: &spec::BlockDesc, range: SrcRange) -> Vec<Child> {
        if blocks.is_empty() {
            self.errors.push(Error::new(
                ErrorKind::EmptyDynamicBlock {
                    rule: spec::ids::DYNAMIC_CHILDREN,
                    block: block.id,
                },
                range,
            ));
        }

        blocks
            .into_iter()
            .filter_map(|inner| self.model_child(inner, block.id))
            .collect()
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

        let Some(value) = self.fold_expr(value) else {
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
            (spec::ids::EXPECTED, FieldValue::Value(value)) => fields.expected = Some(value),
            (spec::ids::ERROR, FieldValue::Value(value)) => fields.error = Some(value),
            (spec::ids::METADATA, FieldValue::Object(value)) => fields.metadata = Some(value),
            (spec::ids::METRICS, FieldValue::Object(value)) => fields.metrics = Some(value),
            (spec::ids::TAGS, FieldValue::Tags(value)) => fields.tags = Some(value),
            _ => unreachable!("field {} does not match its model lowering", field.id.as_str()),
        }
    }

    fn validate_metric_keys(&mut self, expr: &ast::Expr) -> bool {
        let ast::ExprKind::Object(items) = &expr.kind else {
            unreachable!("expression was validated as an object");
        };

        let mut valid = true;

        for item in items {
            let ast::ObjectItem::Attr(attr) = item else {
                unreachable!("spreads are spliced before validation");
            };
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
                let ast::ExprKind::Object(items) = &expr.kind else {
                    self.push_type_mismatch(expr, block, field, expected);
                    return false;
                };

                return items.iter().fold(true, |valid, item| {
                    let ast::ObjectItem::Attr(attr) = item else {
                        unreachable!("spreads are spliced before validation");
                    };
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
            ast::ExprKind::Object(items) => Some(Value::Object(self.model_object(items))),
            ast::ExprKind::Func { name, args } => self.model_func(name, args, range).map(|func| Value::Func { func, range }),
            // only dynamic operator exprs survive folding
            ast::ExprKind::Unary { op, operand } => {
                let operand = self.model_value(*operand)?;
                Some(Value::Unary {
                    op,
                    operand: Box::new(operand),
                    range,
                })
            }
            ast::ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.model_value(*lhs);
                let rhs = self.model_value(*rhs);
                Some(Value::Binary {
                    op,
                    lhs: Box::new(lhs?),
                    rhs: Box::new(rhs?),
                    range,
                })
            }
            ast::ExprKind::Cond { cond, then, otherwise } => {
                let cond = self.model_value(*cond);
                let then = self.model_value(*then);
                let otherwise = self.model_value(*otherwise);
                Some(Value::Cond {
                    cond: Box::new(cond?),
                    then: Box::new(then?),
                    otherwise: Box::new(otherwise?),
                })
            }
            ast::ExprKind::Index { target, index } => {
                let target = self.model_value(*target);
                let index = self.model_value(*index);
                Some(Value::Index {
                    target: Box::new(target?),
                    index: Box::new(index?),
                    range,
                })
            }
            ast::ExprKind::Slice { target, start, end } => {
                let target = self.model_value(*target);
                let start = start.map(|bound| self.model_value(*bound));
                let end = end.map(|bound| self.model_value(*bound));
                Some(Value::Slice {
                    target: Box::new(target?),
                    start: match start {
                        Some(bound) => Some(Box::new(bound?)),
                        None => None,
                    },
                    end: match end {
                        Some(bound) => Some(Box::new(bound?)),
                        None => None,
                    },
                    range,
                })
            }
            ast::ExprKind::VarRef(_) => unreachable!("variable references are resolved before model lowering"),
            ast::ExprKind::LoopRef(_) => unreachable!("loop references are substituted before model lowering"),
            ast::ExprKind::Spread(_) => unreachable!("spreads are spliced before model lowering"),
            ast::ExprKind::For { .. } => unreachable!("for expressions are unrolled before model lowering"),
        }
    }

    fn model_func(&mut self, name: String, args: Vec<ast::Expr>, range: SrcRange) -> Option<Func> {
        // the descriptor's name doubles as the &'static str for diagnostics
        let Some(func) = spec::SPEC.function(&name).map(|desc| desc.name) else {
            self.errors.push(Error::new(
                ErrorKind::UnknownFunction {
                    rule: spec::ids::KNOWN_FUNCTIONS,
                    name,
                },
                range,
            ));
            return None;
        };

        match func {
            "choice" => {
                if args.is_empty() {
                    self.errors.push(Error::new(
                        ErrorKind::EmptyChoice {
                            rule: spec::ids::CHOICE_ALTERNATIVES,
                        },
                        range,
                    ));
                    return None;
                }

                let mut options = Vec::with_capacity(args.len());
                let mut valid = true;
                for arg in args {
                    match self.model_value(arg) {
                        Some(value) => options.push(value),
                        None => valid = false,
                    }
                }

                valid.then_some(Func::Choice(options))
            }
            "range" => {
                let Ok([min, max]) = <[ast::Expr; 2]>::try_from(args) else {
                    self.errors.push(Error::new(
                        ErrorKind::InvalidRangeArgs {
                            rule: spec::ids::RANGE_BOUNDS,
                        },
                        range,
                    ));
                    return None;
                };
                let min = self.model_range_bound(min);
                let max = self.model_range_bound(max)?;
                let min = min?;

                let bounds = match (min, max) {
                    (Number::Int(min), Number::Int(max)) => Range::Int { min, max },
                    (min, max) => Range::Float {
                        min: float_bound(min),
                        max: float_bound(max),
                    },
                };
                let ordered = match bounds {
                    Range::Int { min, max } => min <= max,
                    Range::Float { min, max } => min <= max,
                };

                if !ordered {
                    self.errors.push(Error::new(
                        ErrorKind::InvalidRangeBounds {
                            rule: spec::ids::RANGE_BOUNDS,
                        },
                        range,
                    ));
                    return None;
                }

                Some(Func::Range(bounds))
            }

            "weighted" => self.model_weighted(args, range),

            "normal" => {
                let [mean, stddev] = self.func_args(func, args, "exactly two arguments (mean, stddev)", range)?;
                let mean = self.model_dist_param(mean, func, "mean", "a finite number", |_| true);
                let stddev = self.model_dist_param(stddev, func, "stddev", "non-negative", |value| value >= 0.0);
                Some(Func::Normal {
                    mean: mean?,
                    stddev: stddev?,
                })
            }
            "lognormal" => {
                let [median, sigma] = self.func_args(func, args, "exactly two arguments (median, sigma)", range)?;
                let median = self.model_dist_param(median, func, "median", "positive", |value| value > 0.0);
                let sigma = self.model_dist_param(sigma, func, "sigma", "non-negative", |value| value >= 0.0);
                Some(Func::Lognormal {
                    median: median?,
                    sigma: sigma?,
                })
            }
            "exponential" => {
                let [mean] = self.func_args(func, args, "exactly one argument (mean)", range)?;
                let mean = self.model_dist_param(mean, func, "mean", "positive", |value| value > 0.0)?;
                Some(Func::Exponential { mean })
            }
            "pareto" => {
                let [min, shape] = self.func_args(func, args, "exactly two arguments (min, shape)", range)?;
                let min = self.model_dist_param(min, func, "min", "positive", |value| value > 0.0);
                let shape = self.model_dist_param(shape, func, "shape", "positive", |value| value > 0.0);
                Some(Func::Pareto {
                    min: min?,
                    shape: shape?,
                })
            }
            "beta" => {
                let [alpha, beta] = self.func_args(func, args, "exactly two arguments (alpha, beta)", range)?;
                let alpha = self.model_dist_param(alpha, func, "alpha", "positive", |value| value > 0.0);
                let beta = self.model_dist_param(beta, func, "beta", "positive", |value| value > 0.0);
                Some(Func::Beta {
                    alpha: alpha?,
                    beta: beta?,
                })
            }
            "poisson" => {
                let [mean] = self.func_args(func, args, "exactly one argument (mean)", range)?;
                // the sampler rejects astronomically large means, surface that here
                let mean = self.model_dist_param(mean, func, "mean", "positive and below 1e15", |value| {
                    value > 0.0 && value < 1.0e15
                })?;
                Some(Func::Poisson { mean })
            }
            "chance" => {
                let [probability] = self.func_args(func, args, "exactly one argument (probability)", range)?;
                let probability = self.model_dist_param(probability, func, "probability", "between 0 and 1", |value| {
                    (0.0..=1.0).contains(&value)
                })?;
                Some(Func::Chance { probability })
            }

            "upper" | "lower" | "trim" => {
                let [text] = self.func_args(func, args, "exactly one argument", range)?;
                let text = Box::new(self.model_typed_arg(text, func, StaticType::String)?);
                Some(match func {
                    "upper" => Func::Upper { text },
                    "lower" => Func::Lower { text },
                    _ => Func::Trim { text },
                })
            }
            "replace" => {
                let [text, from, to] = self.func_args(func, args, "exactly three arguments (text, from, to)", range)?;
                let text = self.model_typed_arg(text, func, StaticType::String);
                let from = self.model_typed_arg(from, func, StaticType::String);
                let to = self.model_typed_arg(to, func, StaticType::String);
                Some(Func::Replace {
                    text: Box::new(text?),
                    from: Box::new(from?),
                    to: Box::new(to?),
                })
            }
            "split" => {
                let [text, separator] = self.func_args(func, args, "exactly two arguments (text, separator)", range)?;
                if const_string(&separator).is_some_and(|separator| separator.is_empty()) {
                    self.errors.push(Error::new(
                        ErrorKind::EmptySplitSeparator {
                            rule: spec::ids::SPLIT_SEPARATOR,
                        },
                        separator.range,
                    ));
                    return None;
                }
                let text = self.model_typed_arg(text, func, StaticType::String);
                let separator = self.model_typed_arg(separator, func, StaticType::String);
                Some(Func::Split {
                    text: Box::new(text?),
                    separator: Box::new(separator?),
                })
            }
            "join" => {
                let [array, separator] = self.func_args(func, args, "exactly two arguments (array, separator)", range)?;
                let array = self.model_typed_arg(array, func, StaticType::Array);
                let separator = self.model_typed_arg(separator, func, StaticType::String);
                Some(Func::Join {
                    array: Box::new(array?),
                    separator: Box::new(separator?),
                })
            }
            "contains" => {
                let [target, needle] = self.func_args(func, args, "exactly two arguments (target, needle)", range)?;
                match static_type(&target) {
                    Some(StaticType::String) => {
                        let target = self.model_value(target);
                        let needle = self.model_typed_arg(needle, func, StaticType::String);
                        Some(Func::Contains {
                            target: Box::new(target?),
                            needle: Box::new(needle?),
                        })
                    }
                    Some(StaticType::Array) => {
                        let target = self.model_value(target);
                        let needle = self.model_scalar_arg(needle, func);
                        Some(Func::Contains {
                            target: Box::new(target?),
                            needle: Box::new(needle?),
                        })
                    }
                    _ => {
                        self.push_func_arg_type(&target, func, "string or array");
                        None
                    }
                }
            }
            "starts_with" | "ends_with" => {
                let [text, affix] = self.func_args(func, args, "exactly two arguments", range)?;
                let text = self.model_typed_arg(text, func, StaticType::String);
                let affix = self.model_typed_arg(affix, func, StaticType::String);
                let (text, affix) = (Box::new(text?), Box::new(affix?));
                Some(if func == "starts_with" {
                    Func::StartsWith { text, prefix: affix }
                } else {
                    Func::EndsWith { text, suffix: affix }
                })
            }
            "len" => {
                let [target] = self.func_args(func, args, "exactly one argument", range)?;
                if !matches!(static_type(&target), Some(StaticType::String | StaticType::Array)) {
                    self.push_func_arg_type(&target, func, "string or array");
                    return None;
                }
                let target = Box::new(self.model_value(target)?);
                Some(Func::Len { target })
            }
            "format" => self.model_format(args, range),

            "clamp" => {
                let [value, min, max] = self.func_args(func, args, "exactly three arguments (value, min, max)", range)?;
                // constant bounds must already be ordered
                if let (ast::ExprKind::Num(low), ast::ExprKind::Num(high)) = (&min.kind, &max.kind) {
                    let low = self.model_number(low.clone(), min.range);
                    let high = self.model_number(high.clone(), max.range);
                    if let (Some(low), Some(high)) = (low, high)
                        && float_bound(low) > float_bound(high)
                    {
                        self.errors.push(Error::new(
                            ErrorKind::ClampBoundsOutOfOrder {
                                rule: spec::ids::CLAMP_BOUNDS,
                            },
                            range,
                        ));
                        return None;
                    }
                }
                let value = self.model_typed_arg(value, func, StaticType::Number);
                let min = self.model_typed_arg(min, func, StaticType::Number);
                let max = self.model_typed_arg(max, func, StaticType::Number);
                Some(Func::Clamp {
                    value: Box::new(value?),
                    min: Box::new(min?),
                    max: Box::new(max?),
                })
            }
            "round" | "floor" | "ceil" | "abs" => {
                let [value] = self.func_args(func, args, "exactly one argument", range)?;
                let value = Box::new(self.model_typed_arg(value, func, StaticType::Number)?);
                Some(match func {
                    "round" => Func::Round { value },
                    "floor" => Func::Floor { value },
                    "ceil" => Func::Ceil { value },
                    _ => Func::Abs { value },
                })
            }
            "min" | "max" => {
                if args.len() < 2 {
                    self.errors.push(Error::new(
                        ErrorKind::FuncArity {
                            rule: spec::ids::FUNC_ARITY,
                            func,
                            expected: "at least two arguments",
                        },
                        range,
                    ));
                    return None;
                }
                let mut values = Vec::with_capacity(args.len());
                let mut valid = true;
                for arg in args {
                    match self.model_typed_arg(arg, func, StaticType::Number) {
                        Some(value) => values.push(value),
                        None => valid = false,
                    }
                }
                valid.then(|| if func == "min" { Func::Min(values) } else { Func::Max(values) })
            }

            "uuid" => {
                if !args.is_empty() {
                    self.errors.push(Error::new(
                        ErrorKind::FuncArity {
                            rule: spec::ids::FUNC_ARITY,
                            func,
                            expected: "no arguments",
                        },
                        range,
                    ));
                    return None;
                }
                Some(Func::Uuid)
            }
            "hex" | "alphanum" => {
                let [length] = self.func_args(func, args, "exactly one argument (length)", range)?;
                let length = self.model_random_length(length, func)?;
                Some(if func == "hex" {
                    Func::Hex { length }
                } else {
                    Func::Alphanum { length }
                })
            }

            _ => unreachable!("function {name} does not have a model lowering"),
        }
    }

    fn model_weighted(&mut self, args: Vec<ast::Expr>, range: SrcRange) -> Option<Func> {
        if args.is_empty() {
            self.errors.push(Error::new(
                ErrorKind::FuncArity {
                    rule: spec::ids::FUNC_ARITY,
                    func: "weighted",
                    expected: "at least one `[value, weight]` pair",
                },
                range,
            ));
            return None;
        }

        let mut options = Vec::with_capacity(args.len());
        let mut valid = true;
        for arg in args {
            let arg_range = arg.range;
            let ast::ExprKind::Array(mut elems) = arg.kind else {
                self.push_weighted_option(arg_range);
                valid = false;
                continue;
            };
            if elems.len() != 2 {
                self.push_weighted_option(arg_range);
                valid = false;
                continue;
            }
            let weight_expr = elems.pop().expect("pair has two elements");
            let value_expr = elems.pop().expect("pair has two elements");

            let weight_range = weight_expr.range;
            let weight = match weight_expr.kind {
                ast::ExprKind::Num(raw) => self.model_number(raw, weight_range).map(float_bound),
                _ => {
                    self.push_weighted_option(weight_range);
                    None
                }
            };
            let weight = match weight {
                Some(weight) if weight >= 0.0 => Some(weight),
                Some(_) => {
                    self.push_weighted_option(weight_range);
                    None
                }
                None => None,
            };

            match (self.model_value(value_expr), weight) {
                (Some(value), Some(weight)) => options.push(WeightedOption { value, weight }),
                _ => valid = false,
            }
        }

        if !valid {
            return None;
        }
        if options.iter().map(|option| option.weight).sum::<f64>() <= 0.0 {
            self.errors.push(Error::new(
                ErrorKind::WeightedTotal {
                    rule: spec::ids::WEIGHTED_OPTIONS,
                },
                range,
            ));
            return None;
        }

        Some(Func::Weighted(options))
    }

    fn model_format(&mut self, args: Vec<ast::Expr>, range: SrcRange) -> Option<Func> {
        let mut args = args.into_iter();
        let Some(template_expr) = args.next() else {
            self.errors.push(Error::new(
                ErrorKind::FuncArity {
                    rule: spec::ids::FUNC_ARITY,
                    func: "format",
                    expected: "a constant string template followed by one value per `{}` placeholder",
                },
                range,
            ));
            return None;
        };

        let Some(template) = const_string(&template_expr) else {
            self.push_func_arg_type(&template_expr, "format", "constant string");
            return None;
        };

        let rest: Vec<_> = args.collect();
        let placeholders = template.matches("{}").count();
        if placeholders != rest.len() {
            self.errors.push(Error::new(
                ErrorKind::FormatPlaceholders {
                    rule: spec::ids::FORMAT_TEMPLATE,
                    placeholders,
                    args: rest.len(),
                },
                range,
            ));
            return None;
        }

        let mut values = Vec::with_capacity(rest.len());
        let mut valid = true;
        for arg in rest {
            match self.model_scalar_arg(arg, "format") {
                Some(value) => values.push(value),
                None => valid = false,
            }
        }

        valid.then_some(Func::Format { template, args: values })
    }

    fn func_args<const N: usize>(
        &mut self,
        func: &'static str,
        args: Vec<ast::Expr>,
        expected: &'static str,
        range: SrcRange,
    ) -> Option<[ast::Expr; N]> {
        match <[ast::Expr; N]>::try_from(args) {
            Ok(args) => Some(args),
            Err(_) => {
                self.errors.push(Error::new(
                    ErrorKind::FuncArity {
                        rule: spec::ids::FUNC_ARITY,
                        func,
                        expected,
                    },
                    range,
                ));
                None
            }
        }
    }

    fn model_typed_arg(&mut self, expr: ast::Expr, func: &'static str, required: StaticType) -> Option<Value> {
        if static_type(&expr) != Some(required) {
            self.push_func_arg_type(&expr, func, type_name(required));
            return None;
        }
        self.model_value(expr)
    }

    fn model_scalar_arg(&mut self, expr: ast::Expr, func: &'static str) -> Option<Value> {
        if !matches!(
            static_type(&expr),
            Some(StaticType::String | StaticType::Number | StaticType::Boolean)
        ) {
            self.push_func_arg_type(&expr, func, "string, number, or boolean");
            return None;
        }
        self.model_value(expr)
    }

    fn push_func_arg_type(&mut self, expr: &ast::Expr, func: &'static str, expected: &'static str) {
        self.errors.push(Error::new(
            ErrorKind::FuncArgType {
                rule: spec::ids::FUNC_ARG_TYPES,
                func,
                expected,
                found: found_type(expr),
            },
            expr.range,
        ));
    }

    fn push_weighted_option(&mut self, range: SrcRange) {
        self.errors.push(Error::new(
            ErrorKind::WeightedOptionShape {
                rule: spec::ids::WEIGHTED_OPTIONS,
            },
            range,
        ));
    }

    // constant distribution parameter, already folded to a literal when constant
    fn model_dist_param(
        &mut self,
        expr: ast::Expr,
        func: &'static str,
        param: &'static str,
        expected: &'static str,
        valid: impl Fn(f64) -> bool,
    ) -> Option<f64> {
        let range = expr.range;
        let number = match expr.kind {
            ast::ExprKind::Num(raw) => self.model_number(raw, range)?,
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::NonConstantParam {
                        rule: spec::ids::DIST_PARAMS,
                        func,
                        param,
                    },
                    range,
                ));
                return None;
            }
        };

        let value = float_bound(number);
        if !valid(value) {
            self.errors.push(Error::new(
                ErrorKind::ParamOutOfRange {
                    rule: spec::ids::DIST_PARAMS,
                    func,
                    param,
                    expected,
                },
                range,
            ));
            return None;
        }

        Some(value)
    }

    fn model_random_length(&mut self, expr: ast::Expr, func: &'static str) -> Option<usize> {
        let range = expr.range;
        let number = match expr.kind {
            ast::ExprKind::Num(raw) => self.model_number(raw, range)?,
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::NonConstantParam {
                        rule: spec::ids::RANDOM_LENGTH,
                        func,
                        param: "length",
                    },
                    range,
                ));
                return None;
            }
        };

        match number {
            Number::Int(value) if value >= 0 => Some(value as usize),
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::ParamOutOfRange {
                        rule: spec::ids::RANDOM_LENGTH,
                        func,
                        param: "length",
                        expected: "a non-negative integer",
                    },
                    range,
                ));
                None
            }
        }
    }

    fn model_range_bound(&mut self, expr: ast::Expr) -> Option<Number> {
        let ast::Expr { kind, range } = expr;
        match kind {
            ast::ExprKind::Num(raw) => self.model_number(raw, range),
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::InvalidRangeArgs {
                        rule: spec::ids::RANGE_BOUNDS,
                    },
                    range,
                ));
                None
            }
        }
    }

    fn model_object(&mut self, items: Vec<ast::ObjectItem>) -> Object {
        let mut seen = HashSet::new();
        let mut elem = Vec::new();

        for item in items {
            let ast::ObjectItem::Attr(attr) = item else {
                unreachable!("spreads are spliced before model lowering");
            };
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
            ast::ExprKind::Object(items) => Some(self.model_object(items)),
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
                ast::TemplatePart::Ref { path, range } => match model_ctx_ref(&path, self.repeat_depth) {
                    Some(ctx_ref) => modeled.push(Part::Ref(ctx_ref)),
                    None => {
                        // a known reference in the wrong place gets its own diagnostic
                        let kind = if matches!(path.as_slice(), [first, second] if first == "repeat" && second == "index") {
                            ErrorKind::RepeatIndexOutsideRepeat {
                                rule: spec::ids::REPEAT_INDEX,
                            }
                        } else {
                            ErrorKind::UnknownReference {
                                rule: spec::ids::KNOWN_REFERENCES,
                                path: path.join("."),
                            }
                        };
                        self.errors.push(Error::new(kind, range));
                        valid = false;
                    }
                },
            }
        }

        valid.then_some(Template { parts: modeled })
    }

    fn model_number(&mut self, raw: String, range: SrcRange) -> Option<Number> {
        // folded float literals may carry exponent notation, source literals never do
        let number = if raw.contains(['.', 'e', 'E']) {
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
            spec::NamePolicy::Required => {
                if name.is_none() {
                    self.errors
                        .push(Error::new(ErrorKind::MissingName { block: block.id }, range));
                }
                name
            }
            spec::NamePolicy::Forbidden if name.is_some() => {
                self.errors
                    .push(Error::new(ErrorKind::UnexpectedName { block: block.id }, range));
                None
            }
            spec::NamePolicy::Optional => name,
            spec::NamePolicy::Forbidden => None,
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
        ast::ExprKind::Object(items) => items.iter().any(|item| match item {
            ast::ObjectItem::Attr(attr) => expr_references_vars(&attr.value),
            ast::ObjectItem::Spread(operand) => expr_references_vars(operand),
        }),
        ast::ExprKind::Func { args, .. } => args.iter().any(expr_references_vars),
        ast::ExprKind::Unary { operand, .. } => expr_references_vars(operand),
        ast::ExprKind::Binary { lhs, rhs, .. } => expr_references_vars(lhs) || expr_references_vars(rhs),
        ast::ExprKind::Cond { cond, then, otherwise } => {
            expr_references_vars(cond) || expr_references_vars(then) || expr_references_vars(otherwise)
        }
        ast::ExprKind::Index { target, index } => expr_references_vars(target) || expr_references_vars(index),
        ast::ExprKind::Slice { target, start, end } => {
            expr_references_vars(target)
                || start.as_deref().is_some_and(expr_references_vars)
                || end.as_deref().is_some_and(expr_references_vars)
        }
        ast::ExprKind::Spread(operand) => expr_references_vars(operand),
        ast::ExprKind::For {
            collection,
            key,
            body,
            cond,
            ..
        } => {
            expr_references_vars(collection)
                || key.as_deref().is_some_and(expr_references_vars)
                || expr_references_vars(body)
                || cond.as_deref().is_some_and(expr_references_vars)
        }
        _ => false,
    }
}

fn float_bound(number: Number) -> f64 {
    match number {
        Number::Int(value) => value as f64,
        Number::Float(value) => value,
    }
}

// statically known result type of an expr, none = unknown (eg heterogeneous choice)
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum StaticType {
    String,
    Number,
    Boolean,
    Null,
    Array,
    Object,
}

// constant scalar operand pulled out of a folded literal
enum Const {
    Str(String),
    Num(Number),
    Bool(bool),
}

enum OpClass {
    Arith,
    Cmp,
    Eq,
    Logic,
}

fn op_class(op: ast::BinOp) -> OpClass {
    match op {
        ast::BinOp::Add | ast::BinOp::Sub | ast::BinOp::Mul | ast::BinOp::Div | ast::BinOp::Rem => OpClass::Arith,
        ast::BinOp::Lt | ast::BinOp::Le | ast::BinOp::Gt | ast::BinOp::Ge => OpClass::Cmp,
        ast::BinOp::Eq | ast::BinOp::Ne => OpClass::Eq,
        ast::BinOp::And | ast::BinOp::Or => OpClass::Logic,
    }
}

fn static_type(expr: &ast::Expr) -> Option<StaticType> {
    match &expr.kind {
        ast::ExprKind::Str(_) | ast::ExprKind::Template(_) => Some(StaticType::String),
        ast::ExprKind::Num(_) => Some(StaticType::Number),
        ast::ExprKind::Bool(_) => Some(StaticType::Boolean),
        ast::ExprKind::Null => Some(StaticType::Null),
        ast::ExprKind::Array(_) => Some(StaticType::Array),
        ast::ExprKind::Object(_) => Some(StaticType::Object),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg, ..
        } => Some(StaticType::Number),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Not, ..
        } => Some(StaticType::Boolean),
        ast::ExprKind::Binary { op, .. } => Some(match op_class(*op) {
            OpClass::Arith => StaticType::Number,
            _ => StaticType::Boolean,
        }),
        ast::ExprKind::Cond { then, otherwise, .. } => {
            let then = static_type(then)?;
            let otherwise = static_type(otherwise)?;
            (then == otherwise).then_some(then)
        }
        ast::ExprKind::Func { name, args } => match name.as_str() {
            "range" | "normal" | "lognormal" | "exponential" | "pareto" | "beta" | "poisson" | "len" | "clamp" | "round"
            | "floor" | "ceil" | "abs" | "min" | "max" => Some(StaticType::Number),
            "chance" | "contains" | "starts_with" | "ends_with" => Some(StaticType::Boolean),
            "upper" | "lower" | "trim" | "replace" | "join" | "format" | "uuid" | "hex" | "alphanum" => {
                Some(StaticType::String)
            }
            "split" => Some(StaticType::Array),
            // a choice is only typed when every alternative agrees
            "choice" => unify_static_types(args.iter()),
            // a weighted pick is typed by its pair values; malformed pairs error later
            "weighted" => unify_static_types(args.iter().filter_map(|arg| match &arg.kind {
                ast::ExprKind::Array(elems) if elems.len() == 2 => Some(&elems[0]),
                _ => None,
            })),
            _ => None,
        },
        // a dynamic index is only typed when the target is a literal with agreeing elements
        ast::ExprKind::Index { target, .. } => match &target.kind {
            ast::ExprKind::Array(values) => unify_static_types(values.iter()),
            ast::ExprKind::Object(items) => unify_static_types(items.iter().map(|item| match item {
                ast::ObjectItem::Attr(attr) => &attr.value,
                ast::ObjectItem::Spread(_) => unreachable!("spreads are spliced before type checks"),
            })),
            _ => None,
        },
        // a residual slice always selects from an array
        ast::ExprKind::Slice { .. } => Some(StaticType::Array),
        ast::ExprKind::VarRef(_) => unreachable!("variable references are resolved before type checks"),
        ast::ExprKind::LoopRef(_) => unreachable!("loop references are substituted before type checks"),
        ast::ExprKind::Spread(_) => unreachable!("spreads are spliced before type checks"),
        ast::ExprKind::For { .. } => unreachable!("for expressions are unrolled before type checks"),
    }
}

// none when empty or the types disagree
fn unify_static_types<'a>(exprs: impl Iterator<Item = &'a ast::Expr>) -> Option<StaticType> {
    let mut unified = None;
    for expr in exprs {
        let expr = static_type(expr)?;
        match unified {
            None => unified = Some(expr),
            Some(previous) if previous == expr => {}
            Some(_) => return None,
        }
    }
    unified
}

// the found side of operand diagnostics, prefers the static type over the expr shape
fn found_type(expr: &ast::Expr) -> ExprType {
    match static_type(expr) {
        Some(StaticType::String) => ExprType::String,
        Some(StaticType::Number) => ExprType::Number,
        Some(StaticType::Boolean) => ExprType::Boolean,
        Some(StaticType::Null) => ExprType::Null,
        Some(StaticType::Array) => ExprType::Array,
        Some(StaticType::Object) => ExprType::Object,
        None => ExprType::of(expr),
    }
}

fn type_name(required: StaticType) -> &'static str {
    match required {
        StaticType::String => "string",
        StaticType::Number => "number",
        StaticType::Boolean => "boolean",
        StaticType::Null => "null",
        StaticType::Array => "array",
        StaticType::Object => "object",
    }
}

// no funcs or per-trace template refs anywhere beneath
fn expr_is_constant(expr: &ast::Expr) -> bool {
    match &expr.kind {
        ast::ExprKind::Func { .. } => false,
        ast::ExprKind::Template(parts) => parts.iter().all(|part| matches!(part, ast::TemplatePart::Lit(_))),
        ast::ExprKind::Array(values) => values.iter().all(expr_is_constant),
        ast::ExprKind::Object(items) => items.iter().all(|item| match item {
            ast::ObjectItem::Attr(attr) => expr_is_constant(&attr.value),
            ast::ObjectItem::Spread(_) => unreachable!("spreads are spliced before constness checks"),
        }),
        ast::ExprKind::Unary { operand, .. } => expr_is_constant(operand),
        ast::ExprKind::Binary { lhs, rhs, .. } => expr_is_constant(lhs) && expr_is_constant(rhs),
        ast::ExprKind::Cond { cond, then, otherwise } => {
            expr_is_constant(cond) && expr_is_constant(then) && expr_is_constant(otherwise)
        }
        // constant indexes and slices fold away, residual ones are dynamic
        ast::ExprKind::Index { .. } | ast::ExprKind::Slice { .. } => false,
        _ => true,
    }
}

// constant string value of a folded expr, none when dynamic or another type
fn const_string(expr: &ast::Expr) -> Option<String> {
    match &expr.kind {
        ast::ExprKind::Str(value) => Some(value.clone()),
        ast::ExprKind::Template(parts) => parts
            .iter()
            .map(|part| match part {
                ast::TemplatePart::Lit(value) => Some(value.as_str()),
                ast::TemplatePart::Ref { .. } => None,
            })
            .collect(),
        _ => None,
    }
}

fn negate_literal(raw: &str) -> String {
    match raw.strip_prefix('-') {
        Some(stripped) => stripped.to_owned(),
        None => format!("-{raw}"),
    }
}

// {:?} keeps a .0 on floats so the literal reparses as a float
fn num_literal(number: Number) -> String {
    match number {
        Number::Int(value) => value.to_string(),
        Number::Float(value) => format!("{value:?}"),
    }
}

// none = overflow or a non-finite float result, zero divisors are caught before eval
fn eval_arith(op: ast::BinOp, lhs: Number, rhs: Number) -> Option<Number> {
    match (lhs, rhs) {
        (Number::Int(lhs), Number::Int(rhs)) => {
            let result = match op {
                ast::BinOp::Add => lhs.checked_add(rhs),
                ast::BinOp::Sub => lhs.checked_sub(rhs),
                ast::BinOp::Mul => lhs.checked_mul(rhs),
                // checked catches i64::MIN / -1
                ast::BinOp::Div => lhs.checked_div(rhs),
                ast::BinOp::Rem => lhs.checked_rem(rhs),
                _ => unreachable!("operator is arithmetic"),
            };
            result.map(Number::Int)
        }
        (lhs, rhs) => {
            let (lhs, rhs) = (float_bound(lhs), float_bound(rhs));
            let result = match op {
                ast::BinOp::Add => lhs + rhs,
                ast::BinOp::Sub => lhs - rhs,
                ast::BinOp::Mul => lhs * rhs,
                ast::BinOp::Div => lhs / rhs,
                ast::BinOp::Rem => lhs % rhs,
                _ => unreachable!("operator is arithmetic"),
            };
            result.is_finite().then_some(Number::Float(result))
        }
    }
}

fn eval_cmp(op: ast::BinOp, lhs: Number, rhs: Number) -> bool {
    fn compare<T: PartialOrd>(op: ast::BinOp, lhs: T, rhs: T) -> bool {
        match op {
            ast::BinOp::Lt => lhs < rhs,
            ast::BinOp::Le => lhs <= rhs,
            ast::BinOp::Gt => lhs > rhs,
            ast::BinOp::Ge => lhs >= rhs,
            _ => unreachable!("operator is a comparison"),
        }
    }

    match (lhs, rhs) {
        (Number::Int(lhs), Number::Int(rhs)) => compare(op, lhs, rhs),
        (lhs, rhs) => compare(op, float_bound(lhs), float_bound(rhs)),
    }
}

fn model_ctx_ref(path: &[String], repeat_depth: usize) -> Option<CtxRef> {
    match path {
        [first, second] if first == "trace" && second == "index" => Some(CtxRef::TraceIndex),
        [first, second] if first == "repeat" && second == "index" && repeat_depth > 0 => Some(CtxRef::RepeatIndex),
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
    expected: Option<Value>,
    error: Option<Value>,
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
            expected: self.expected,
            error: self.error,
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
    UnknownFunction {
        rule: spec::Id,
        name: String,
    },
    EmptyChoice {
        rule: spec::Id,
    },
    InvalidRangeArgs {
        rule: spec::Id,
    },
    InvalidRangeBounds {
        rule: spec::Id,
    },
    FuncArity {
        rule: spec::Id,
        func: &'static str,
        expected: &'static str,
    },
    FuncArgType {
        rule: spec::Id,
        func: &'static str,
        expected: &'static str,
        found: ExprType,
    },
    NonConstantParam {
        rule: spec::Id,
        func: &'static str,
        param: &'static str,
    },
    ParamOutOfRange {
        rule: spec::Id,
        func: &'static str,
        param: &'static str,
        expected: &'static str,
    },
    WeightedOptionShape {
        rule: spec::Id,
    },
    WeightedTotal {
        rule: spec::Id,
    },
    FormatPlaceholders {
        rule: spec::Id,
        placeholders: usize,
        args: usize,
    },
    EmptySplitSeparator {
        rule: spec::Id,
    },
    ClampBoundsOutOfOrder {
        rule: spec::Id,
    },
    OperandTypeMismatch {
        rule: spec::Id,
        op: String,
        expected: &'static str,
        found: ExprType,
    },
    NonBooleanCondition {
        rule: spec::Id,
        found: ExprType,
    },
    NonIndexableTarget {
        rule: spec::Id,
        found: ExprType,
    },
    IndexTypeMismatch {
        rule: spec::Id,
        expected: &'static str,
        found: ExprType,
    },
    NonIntegerIndex {
        rule: spec::Id,
    },
    IndexOutOfBounds {
        rule: spec::Id,
        index: i64,
        len: usize,
    },
    UnknownObjectKey {
        rule: spec::Id,
        key: String,
    },
    DivisionByZero {
        rule: spec::Id,
        op: String,
    },
    NonFiniteResult {
        rule: spec::Id,
    },
    NonSliceableTarget {
        rule: spec::Id,
        found: ExprType,
    },
    SliceTypeMismatch {
        rule: spec::Id,
        found: ExprType,
    },
    NonIntegerSliceBound {
        rule: spec::Id,
    },
    NegativeSliceBound {
        rule: spec::Id,
    },
    SpreadTypeMismatch {
        rule: spec::Id,
        expected: &'static str,
        found: ExprType,
    },
    ForCollectionMismatch {
        rule: spec::Id,
        found: ExprType,
    },
    NonConstantForFilter {
        rule: spec::Id,
        found: ExprType,
    },
    NonConstantForKey {
        rule: spec::Id,
        found: ExprType,
    },
    EmptyDynamicBlock {
        rule: spec::Id,
        block: spec::Id,
    },
    NegativeRepeatCount {
        rule: spec::Id,
    },
    NonIntegerRepeatCount {
        rule: spec::Id,
    },
    ChanceOutOfRange {
        rule: spec::Id,
    },
    RepeatIndexOutsideRepeat {
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
            Self::UnknownFunction { rule, name } => {
                let rule = rule_desc(*rule);
                write!(formatter, "unknown function `{name}`; {}", rule.summary)
            }
            Self::EmptyChoice { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `choice` has no alternatives; {}", rule.summary)
            }
            Self::InvalidRangeArgs { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `range` expects two number arguments; {}", rule.summary)
            }
            Self::InvalidRangeBounds { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "range bounds are out of order; {}", rule.summary)
            }
            Self::FuncArity { rule, func, expected } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `{func}` expects {expected}; {}", rule.summary)
            }
            Self::FuncArgType {
                rule,
                func,
                expected,
                found,
            } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "function `{func}` expects a {expected} argument, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantParam { rule, func, param } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "function `{func}` {param} must be a constant number; {}",
                    rule.summary
                )
            }
            Self::ParamOutOfRange {
                rule,
                func,
                param,
                expected,
            } => {
                let rule = rule_desc(*rule);
                write!(formatter, "function `{func}` {param} must be {expected}; {}", rule.summary)
            }
            Self::WeightedOptionShape { rule } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "weighted option must be a `[value, weight]` pair with a constant non-negative weight; {}",
                    rule.summary
                )
            }
            Self::WeightedTotal { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "weighted options have no positive weight; {}", rule.summary)
            }
            Self::FormatPlaceholders {
                rule,
                placeholders,
                args,
            } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "format template has {placeholders} `{{}}` placeholders but {args} arguments; {}",
                    rule.summary
                )
            }
            Self::EmptySplitSeparator { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "split separator is empty; {}", rule.summary)
            }
            Self::ClampBoundsOutOfOrder { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "clamp bounds are out of order; {}", rule.summary)
            }
            Self::OperandTypeMismatch {
                rule,
                op,
                expected,
                found,
            } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "operator `{op}` expects {expected} operands, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonBooleanCondition { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "conditional expects a boolean condition, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonIndexableTarget { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot index into {found}; {}", rule.summary)
            }
            Self::IndexTypeMismatch { rule, expected, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "index expects {expected}, but found {found}; {}", rule.summary)
            }
            Self::NonIntegerIndex { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "array index must be an integer; {}", rule.summary)
            }
            Self::IndexOutOfBounds { rule, index, len } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "index {index} is out of bounds for an array of {len} elements; {}",
                    rule.summary
                )
            }
            Self::UnknownObjectKey { rule, key } => {
                let rule = rule_desc(*rule);
                write!(formatter, "object has no key `{key}`; {}", rule.summary)
            }
            Self::DivisionByZero { rule, op } => {
                let rule = rule_desc(*rule);
                write!(formatter, "operator `{op}` divides by a constant zero; {}", rule.summary)
            }
            Self::NonFiniteResult { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "expression result is not a finite number; {}", rule.summary)
            }
            Self::NonSliceableTarget { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot slice {found}; {}", rule.summary)
            }
            Self::SliceTypeMismatch { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "slice bound expects a number, but found {found}; {}", rule.summary)
            }
            Self::NonIntegerSliceBound { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "slice bound must be an integer; {}", rule.summary)
            }
            Self::NegativeSliceBound { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "slice bound must be non-negative; {}", rule.summary)
            }
            Self::SpreadTypeMismatch { rule, expected, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot spread {found} into an {expected}; {}", rule.summary)
            }
            Self::ForCollectionMismatch { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for expression expects an array or object collection with a shape known before generation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantForFilter { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for filter expects a boolean known before generation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantForKey { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for key expects a string known before generation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::EmptyDynamicBlock { rule, block } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "block `{}` has no child blocks; {}",
                    block_desc(*block).keyword,
                    rule.summary
                )
            }
            Self::NegativeRepeatCount { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "repeat count is negative; {}", rule.summary)
            }
            Self::NonIntegerRepeatCount { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "repeat count is not an integer; {}", rule.summary)
            }
            Self::ChanceOutOfRange { rule } => {
                let rule = rule_desc(*rule);
                write!(formatter, "maybe chance is not between 0 and 1; {}", rule.summary)
            }
            Self::RepeatIndexOutsideRepeat { rule } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "`${{repeat.index}}` is not inside a repeat block; {}",
                    rule.summary
                )
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
        let ast = crate::dsl::parser::parse(tokens, source).unwrap();
        Modeler::new(ast).model()
    }

    fn tag_text(tag: &Template) -> &str {
        match tag.parts.as_slice() {
            [Part::Lit(value)] => value,
            _ => panic!("expected a literal tag"),
        }
    }

    fn as_span(child: &Child) -> &Span {
        match child {
            Child::Span(span) => span,
            other => panic!("expected a span child, found {other:?}"),
        }
    }

    #[test]
    fn models_fixture_as_typed_domain() {
        let model = model(include_str!("../../tests/fixtures/simple.bt")).unwrap();

        assert_eq!(model.traces.len(), 1);
        let trace = &model.traces[0];
        assert_eq!(trace.name, "support-sessions");
        assert_eq!(trace.children.len(), 2);
        assert!(
            trace
                .children
                .iter()
                .all(|child| matches!(&as_span(child).kind, SpanKind::Task))
        );
        assert!(trace.children.iter().all(|child| matches!(
            as_span(child).children.as_slice(),
            [Child::Span(Span { kind: SpanKind::Llm, .. })]
        )));
        assert_eq!(trace.fields.tags.iter().map(tag_text).collect::<Vec<_>>(), ["chat", "prod"]);
    }

    #[test]
    fn models_tool_and_function_spans() {
        let source = r#"
            trace "example" {
                function "handle_request" {
                    tool "search" {
                        llm "rerank" {}
                    }
                }
            }
        "#;
        let model = model(source).unwrap();

        let function = as_span(&model.traces[0].children[0]);
        assert!(matches!(function.kind, SpanKind::Function));
        let tool = as_span(&function.children[0]);
        assert!(matches!(tool.kind, SpanKind::Tool));
        assert!(matches!(as_span(&tool.children[0]).kind, SpanKind::Llm));
    }

    #[test]
    fn models_expected_and_error_fields() {
        let source = r#"
            trace "example" {
                llm "answer" {
                    expected = { answer = "4" }
                    error = choice(null, "timeout")
                }
            }
        "#;
        let model = model(source).unwrap();

        let fields = &as_span(&model.traces[0].children[0]).fields;
        assert!(matches!(&fields.expected, Some(Value::Object(object)) if object.elem[0].key == "answer"));
        assert!(matches!(&fields.error, Some(Value::Func { func: Func::Choice(options), .. }) if options.len() == 2));
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
        let model = model(r#"trace "example" { input = null metrics = { delta = -0.5, offset = -3 } }"#).unwrap();

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
            r#"vars { a = 1 b = choice(var.a, 2) } trace "example" {}"#,
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
    fn models_choice_and_range_funcs() {
        let model = model(r#"trace "t" { input = choice("a", 1) metrics = { n = range(1, 5), x = range(0, 1.5) } }"#).unwrap();
        let fields = &model.traces[0].fields;

        let Some(Value::Func {
            func: Func::Choice(options),
            ..
        }) = &fields.input
        else {
            panic!("expected a choice");
        };
        assert!(matches!(&options[0], Value::Str(value) if value == "a"));
        assert!(matches!(options[1], Value::Num(Number::Int(1))));

        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(
            metrics.elem[0].value,
            Value::Func {
                func: Func::Range(Range::Int { min: 1, max: 5 }),
                ..
            }
        ));
        assert!(matches!(
            metrics.elem[1].value,
            Value::Func { func: Func::Range(Range::Float { min, max }), .. } if min == 0.0 && max == 1.5
        ));
    }

    #[test]
    fn resolves_variables_inside_func_args() {
        let model = model(r#"vars { m = "gpt" } trace "t" { input = choice(var.m, "x") }"#).unwrap();

        let Some(Value::Func {
            func: Func::Choice(options),
            ..
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a choice");
        };
        assert!(matches!(&options[0], Value::Str(value) if value == "gpt"));
    }

    #[test]
    fn rejects_unknown_functions() {
        let source = r#"trace "t" { input = shuffle(1, 2) }"#;
        let errors = model(source).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownFunction {
                rule: spec::ids::KNOWN_FUNCTIONS,
                name: "shuffle".to_owned(),
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "shuffle(1, 2)");
    }

    #[test]
    fn rejects_choices_without_alternatives() {
        let errors = model(r#"trace "t" { input = choice() }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::EmptyChoice {
                rule: spec::ids::CHOICE_ALTERNATIVES,
            }
        );
    }

    #[test]
    fn rejects_invalid_range_args_and_bounds() {
        for source in [
            r#"trace "t" { input = range(1) }"#,
            r#"trace "t" { input = range(1, 2, 3) }"#,
            r#"trace "t" { input = range(1, "x") }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::InvalidRangeArgs {
                    rule: spec::ids::RANGE_BOUNDS,
                }
            );
        }

        for source in [
            r#"trace "t" { input = range(5, 1) }"#,
            r#"trace "t" { input = range(1.5, 0.5) }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::InvalidRangeBounds {
                    rule: spec::ids::RANGE_BOUNDS,
                }
            );
        }
    }

    #[test]
    fn models_weighted_funcs() {
        let model = model(r#"trace "t" { input = weighted(["a", 8], [range(1, 3), 2], ["c", 0]) }"#).unwrap();

        let Some(Value::Func {
            func: Func::Weighted(options),
            ..
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a weighted pick");
        };
        assert_eq!(options.len(), 3);
        assert!(matches!(&options[0].value, Value::Str(value) if value == "a"));
        assert!((options[0].weight - 8.0).abs() < f64::EPSILON);
        assert!(matches!(
            &options[1].value,
            Value::Func {
                func: Func::Range(_),
                ..
            }
        ));
        assert!(options[2].weight == 0.0);
    }

    #[test]
    fn rejects_invalid_weighted_options() {
        for source in [
            r#"trace "t" { input = weighted("a") }"#,
            r#"trace "t" { input = weighted(["a", 1, 2]) }"#,
            r#"trace "t" { input = weighted(["a", range(1, 2)]) }"#,
            r#"trace "t" { input = weighted(["a", 0 - 1]) }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert!(
                matches!(errors[0].kind(), ErrorKind::WeightedOptionShape { .. }),
                "unexpected error for {source}: {}",
                errors[0]
            );
        }

        let errors = model(r#"trace "t" { input = weighted(["a", 0], ["b", 0]) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::WeightedTotal { .. }));

        let errors = model(r#"trace "t" { input = weighted() }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "weighted", .. }));
    }

    #[test]
    fn models_distribution_funcs() {
        let model = model(
            r#"
                trace "t" {
                    metrics = {
                        a = normal(0.5, 0.1)
                        b = lognormal(300, 0.5)
                        c = exponential(250)
                        d = pareto(100, 1.5)
                        e = beta(2, 5)
                        f = poisson(3)
                    }
                    input = chance(0.25)
                }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(
            metrics.elem[0].value,
            Value::Func { func: Func::Normal { mean, stddev }, .. } if mean == 0.5 && stddev == 0.1
        ));
        assert!(matches!(
            metrics.elem[1].value,
            Value::Func { func: Func::Lognormal { median, sigma }, .. } if median == 300.0 && sigma == 0.5
        ));
        assert!(matches!(
            metrics.elem[2].value,
            Value::Func { func: Func::Exponential { mean }, .. } if mean == 250.0
        ));
        assert!(matches!(
            metrics.elem[3].value,
            Value::Func { func: Func::Pareto { min, shape }, .. } if min == 100.0 && shape == 1.5
        ));
        assert!(matches!(
            metrics.elem[4].value,
            Value::Func { func: Func::Beta { alpha, beta }, .. } if alpha == 2.0 && beta == 5.0
        ));
        assert!(matches!(
            metrics.elem[5].value,
            Value::Func { func: Func::Poisson { mean }, .. } if mean == 3.0
        ));
        assert!(matches!(
            fields.input,
            Some(Value::Func { func: Func::Chance { probability }, .. }) if probability == 0.25
        ));
    }

    #[test]
    fn rejects_invalid_distribution_params() {
        let errors = model(r#"trace "t" { input = normal(1) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "normal", .. }));

        let errors = model(r#"trace "t" { input = normal(range(0, 1), 1) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantParam {
                func: "normal",
                param: "mean",
                ..
            }
        ));

        for (source, func, param) in [
            (r#"trace "t" { input = normal(0, 0 - 1) }"#, "normal", "stddev"),
            (r#"trace "t" { input = lognormal(0, 1) }"#, "lognormal", "median"),
            (r#"trace "t" { input = exponential(0) }"#, "exponential", "mean"),
            (r#"trace "t" { input = pareto(0, 1) }"#, "pareto", "min"),
            (r#"trace "t" { input = beta(1, 0) }"#, "beta", "beta"),
            (r#"trace "t" { input = poisson(0) }"#, "poisson", "mean"),
            (r#"trace "t" { input = chance(1.5) }"#, "chance", "probability"),
        ] {
            let errors = model(source).unwrap_err();
            match errors[0].kind() {
                ErrorKind::ParamOutOfRange {
                    func: found_func,
                    param: found_param,
                    ..
                } => {
                    assert_eq!((*found_func, *found_param), (func, param), "wrong param for {source}");
                }
                other => panic!("unexpected error for {source}: {other}"),
            }
        }
    }

    #[test]
    fn models_string_funcs() {
        let model = model(
            r#"
                vars { tags = ["a", "b"] }
                trace "t" {
                    input = upper(choice("get", "post"))
                    output = format("model={} n={}", "gpt", range(1, 5))
                    expected = join(var.tags, ", ")
                    error = contains(var.tags, "a")
                    metadata = { parts = split("a,b", ","), has = contains("abc", "b") }
                }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert!(matches!(
            &fields.input,
            Some(Value::Func {
                func: Func::Upper { .. },
                ..
            })
        ));
        assert!(matches!(
            &fields.output,
            Some(Value::Func { func: Func::Format { template, args }, .. })
                if template == "model={} n={}" && args.len() == 2
        ));
        assert!(matches!(
            &fields.expected,
            Some(Value::Func {
                func: Func::Join { .. },
                ..
            })
        ));
        assert!(matches!(
            &fields.error,
            Some(Value::Func {
                func: Func::Contains { .. },
                ..
            })
        ));
    }

    #[test]
    fn rejects_invalid_string_func_args() {
        for (source, func) in [
            (r#"trace "t" { input = upper(1) }"#, "upper"),
            (r#"trace "t" { input = trim(null) }"#, "trim"),
            (r#"trace "t" { input = split("a", 1) }"#, "split"),
            (r#"trace "t" { input = join("a", ",") }"#, "join"),
            (r#"trace "t" { input = contains(1, "x") }"#, "contains"),
            (r#"trace "t" { input = len(true) }"#, "len"),
            (r#"trace "t" { input = starts_with("a", 1) }"#, "starts_with"),
            (r#"trace "t" { input = upper(choice("a", 1)) }"#, "upper"),
        ] {
            let errors = model(source).unwrap_err();
            assert!(
                matches!(errors[0].kind(), ErrorKind::FuncArgType { func: found, .. } if *found == func),
                "unexpected error for {source}: {}",
                errors[0]
            );
        }

        let errors = model(r#"trace "t" { input = replace("a", "b") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "replace", .. }));

        let errors = model(r#"trace "t" { input = split("a,b", "") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::EmptySplitSeparator { .. }));
    }

    #[test]
    fn rejects_invalid_format_calls() {
        let errors = model(r#"trace "t" { input = format(range(0, 1), 1) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::FuncArgType {
                func: "format",
                expected: "constant string",
                ..
            }
        ));

        let errors = model(r#"trace "t" { input = format("{} {}", 1) }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::FormatPlaceholders {
                rule: spec::ids::FORMAT_TEMPLATE,
                placeholders: 2,
                args: 1,
            }
        );

        let errors = model(r#"trace "t" { input = format("{}", [1]) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "format", .. }));
    }

    #[test]
    fn models_numeric_funcs() {
        let model = model(
            r#"
                trace "t" {
                    metrics = {
                        a = clamp(lognormal(300, 0.5), 20, 30000)
                        b = round(normal(0, 1))
                        c = min(range(1, 10), 5)
                        d = abs(0 - range(1, 5))
                    }
                }
            "#,
        )
        .unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        assert!(matches!(
            &metrics.elem[0].value,
            Value::Func {
                func: Func::Clamp { .. },
                ..
            }
        ));
        assert!(matches!(
            &metrics.elem[1].value,
            Value::Func {
                func: Func::Round { .. },
                ..
            }
        ));
        assert!(matches!(
            &metrics.elem[2].value,
            Value::Func { func: Func::Min(values), .. } if values.len() == 2
        ));
        assert!(matches!(
            &metrics.elem[3].value,
            Value::Func {
                func: Func::Abs { .. },
                ..
            }
        ));
    }

    #[test]
    fn rejects_invalid_numeric_func_args() {
        let errors = model(r#"trace "t" { input = clamp(range(1, 5), 10, 2) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::ClampBoundsOutOfOrder { .. }));

        let errors = model(r#"trace "t" { input = round("x") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "round", .. }));

        let errors = model(r#"trace "t" { input = min(1) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "min", .. }));

        let errors = model(r#"trace "t" { input = max(1, "x") }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "max", .. }));
    }

    #[test]
    fn models_id_funcs_and_rejects_invalid_lengths() {
        let model_ok = model(r#"vars { n = 8 } trace "t" { input = [uuid(), hex(16), alphanum(var.n)] }"#).unwrap();
        let Some(Value::Array(array)) = &model_ok.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert!(matches!(array.elem[0], Value::Func { func: Func::Uuid, .. }));
        assert!(matches!(
            array.elem[1],
            Value::Func {
                func: Func::Hex { length: 16 },
                ..
            }
        ));
        assert!(matches!(
            array.elem[2],
            Value::Func {
                func: Func::Alphanum { length: 8 },
                ..
            }
        ));

        let errors = model(r#"trace "t" { input = uuid(1) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArity { func: "uuid", .. }));

        let errors = model(r#"trace "t" { input = hex(range(1, 4)) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantParam {
                func: "hex",
                param: "length",
                ..
            }
        ));

        for source in [
            r#"trace "t" { input = hex(0 - 1) }"#,
            r#"trace "t" { input = alphanum(1.5) }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert!(matches!(errors[0].kind(), ErrorKind::ParamOutOfRange { param: "length", .. }));
        }
    }

    #[test]
    fn types_func_results_for_operator_and_arg_checks() {
        // string and number results compose with operators and other funcs
        model(r#"trace "t" { input = upper("a") == "A" ? 1 : 2 }"#).unwrap();
        model(r#"trace "t" { input = len("abc") + poisson(2) }"#).unwrap();
        model(r#"trace "t" { input = lower(format("{}", weighted(["A", 1], ["B", 3]))) }"#).unwrap();

        // a mixed weighted pick has no static type, so typed args reject it
        let errors = model(r#"trace "t" { input = upper(weighted(["a", 1], [2, 1])) }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::FuncArgType { func: "upper", .. }));
    }

    #[test]
    fn rejects_funcs_where_a_specific_type_is_expected() {
        let tags = model(r#"trace "t" { tags = [choice("a", "b")] }"#).unwrap_err();
        assert!(matches!(
            tags[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
        assert!(tags[0].to_string().contains("expects string, but found function"));

        let metadata = model(r#"trace "t" { metadata = choice({}, {}) }"#).unwrap_err();
        assert!(matches!(
            metadata[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
    }

    #[test]
    fn rejects_interpolating_func_variables() {
        let errors = model(r#"vars { s = choice("a", "b") } trace "t" { input = "${var.s}" }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonScalarInterpolation {
                rule: spec::ids::SCALAR_INTERPOLATION,
                name: "s".to_owned(),
            }
        );
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
        let source = r#"trace "example" { metrics = { start = 1, tokens = 4, end = 2 } }"#;
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
        let duplicate = model(r#"trace "example" { metadata = { key = 1, key = 2 } }"#).unwrap_err();
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
        let ast = crate::dsl::parser::parse(tokens, source).unwrap();

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

    #[test]
    fn folds_constant_arithmetic() {
        let model = model(r#"trace "t" { metrics = { a = 1 + 2 * 3, b = 7 / 2, c = 7 % 2, d = 10 - 12 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Int(7))));
        // integer division truncates toward zero
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Int(3))));
        assert!(matches!(metrics.elem[2].value, Value::Num(Number::Int(1))));
        assert!(matches!(metrics.elem[3].value, Value::Num(Number::Int(-2))));
    }

    #[test]
    fn folds_float_promotion_and_keeps_floats_float() {
        let model = model(r#"trace "t" { metrics = { a = 1 + 0.5, b = 3.0 / 2, c = 1.5 + 1.5 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Float(value)) if value == 1.5));
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Float(value)) if value == 1.5));
        // a whole float result stays a float through re-literalization
        assert!(matches!(metrics.elem[2].value, Value::Num(Number::Float(value)) if value == 3.0));
    }

    #[test]
    fn folds_comparisons_equality_and_logic() {
        let source = r#"trace "t" { metrics = {
            a = 1 < 2
            b = "x" == "x"
            c = 1.0 == 1
            d = true && false
            e = !(1 > 2)
            f = 1 != 2
        } }"#;
        let model = model(source).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        let expected = [true, true, true, false, true, true];
        for (field, want) in metrics.elem.iter().zip(expected) {
            assert!(
                matches!(field.value, Value::Bool(value) if value == want),
                "field {}",
                field.key
            );
        }
    }

    #[test]
    fn folds_constant_ternaries_into_typed_positions() {
        let model = model(r#"trace "t" { tags = [true ? "a" : "b", false ? "c" : "d"] }"#).unwrap();
        let tags = &model.traces[0].fields.tags;

        assert!(matches!(&tags[0].parts[0], Part::Lit(value) if value == "a"));
        assert!(matches!(&tags[1].parts[0], Part::Lit(value) if value == "d"));
    }

    #[test]
    fn folds_negation_textually() {
        let model = model(r#"trace "t" { metrics = { a = -9223372036854775808, b = --5, c = -1.5 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        // i64::MIN only exists via textual negation, the bare literal overflows
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Int(i64::MIN))));
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Int(5))));
        assert!(matches!(metrics.elem[2].value, Value::Num(Number::Float(value)) if value == -1.5));
    }

    #[test]
    fn folds_grouped_expressions() {
        let model = model(r#"trace "t" { input = (1 + 2) * 3 }"#).unwrap();

        assert!(matches!(model.traces[0].fields.input, Some(Value::Num(Number::Int(9)))));
    }

    #[test]
    fn rejects_mismatched_operand_types() {
        let source = r#"trace "t" { input = 1 + "a" }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op: "+".to_owned(),
                expected: "number",
                found: ExprType::String,
            }
        );
        assert_eq!(&source[errors[0].range().start..errors[0].range().end], "\"a\"");

        let errors = model(r#"trace "t" { input = 1 && true }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op: "&&".to_owned(),
                expected: "boolean",
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { input = !5 }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op: "!".to_owned(),
                expected: "boolean",
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { input = [1] + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Array,
                ..
            }
        ));
    }

    #[test]
    fn rejects_null_and_cross_type_equality() {
        // null is not comparable, both sides report
        let errors = model(r#"trace "t" { input = null == null }"#).unwrap_err();
        assert_eq!(errors.len(), 2);
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                expected: "string, number, or boolean",
                found: ExprType::Null,
                ..
            }
        ));

        // the left operand fixes the comparison type
        let errors = model(r#"trace "t" { input = "a" == 1 }"#).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                expected: "string",
                found: ExprType::Number,
                ..
            }
        ));
    }

    #[test]
    fn rejects_operands_without_a_static_type() {
        // heterogeneous choice has no unified type
        let errors = model(r#"trace "t" { input = choice("a", 1) + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));

        // a dynamic ternary with mixed branch types is untyped as an operand
        let errors = model(r#"trace "t" { input = (range(0, 1) == 1 ? 1 : "x") + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
    }

    #[test]
    fn rejects_non_boolean_conditions() {
        let errors = model(r#"trace "t" { input = 1 ? 2 : 3 }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonBooleanCondition {
                rule: spec::ids::BOOLEAN_CONDITIONS,
                found: ExprType::Number,
            }
        );
    }

    #[test]
    fn rejects_constant_zero_divisors() {
        for source in [
            r#"trace "t" { input = 1 / 0 }"#,
            r#"trace "t" { input = 1 % 0 }"#,
            r#"trace "t" { input = 1 / 0.0 }"#,
            // static even with a dynamic lhs
            r#"trace "t" { input = range(1, 2) / 0 }"#,
        ] {
            let errors = model(source).unwrap_err();
            assert!(
                matches!(errors[0].kind(), ErrorKind::DivisionByZero { .. }),
                "source: {source}"
            );
        }

        // the diagnostic points at the zero
        let source = r#"trace "t" { input = 1 / 0 }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(&source[errors[0].range().start..errors[0].range().end], "0");
    }

    #[test]
    fn rejects_non_finite_constant_results() {
        let errors = model(r#"trace "t" { input = 9223372036854775807 + 1 }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonFiniteResult {
                rule: spec::ids::FINITE_NUMBERS,
            }
        );

        let big = format!("{}.0", "9".repeat(200));
        let errors = model(&format!(r#"trace "t" {{ input = {big} * {big} }}"#)).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::NonFiniteResult { .. }));
    }

    #[test]
    fn folds_variables_in_operator_exprs() {
        let model = model(r#"vars { n = 2 } trace "t" { input = var.n + 1 }"#).unwrap();

        assert!(matches!(model.traces[0].fields.input, Some(Value::Num(Number::Int(3)))));
    }

    #[test]
    fn rejects_operator_exprs_referencing_vars_in_vars() {
        let errors = model(r#"vars { a = 1 b = 1 + var.a } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarInVar { .. }));
    }

    #[test]
    fn interpolates_folded_constant_vars() {
        let model = model(r#"vars { n = 1 + 2 } trace "t" { output = "n ${var.n}" }"#).unwrap();

        assert!(matches!(&model.traces[0].fields.output, Some(Value::Str(value)) if value == "n 3"));
    }

    #[test]
    fn rejects_interpolating_dynamic_vars() {
        let errors = model(r#"vars { d = 1 + range(1, 2) } trace "t" { output = "${var.d}" }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::NonScalarInterpolation { .. }));
    }

    #[test]
    fn rejects_dynamic_exprs_for_typed_fields() {
        let errors = model(r#"trace "t" { tags = [range(1, 2) > 1 ? "a" : "b"] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
        assert!(errors[0].to_string().contains("found expression"));

        let errors = model(r#"trace "t" { metadata = 1 + range(0, 1) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
    }

    #[test]
    fn models_dynamic_operator_exprs() {
        let source = r#"trace "t" { input = 1 + range(0, 1) }"#;
        let model = model(source).unwrap();

        let Some(Value::Binary { op, lhs, rhs, range }) = &model.traces[0].fields.input else {
            panic!("expected a dynamic binary expression");
        };
        assert_eq!(*op, ast::BinOp::Add);
        assert!(matches!(**lhs, Value::Num(Number::Int(1))));
        assert!(matches!(
            **rhs,
            Value::Func {
                func: Func::Range(Range::Int { min: 0, max: 1 }),
                ..
            }
        ));
        assert_eq!(&source[range.start..range.end], "1 + range(0, 1)");
    }

    #[test]
    fn models_dynamic_ternaries() {
        let model = model(r#"trace "t" { input = range(0, 1) == 0 ? "a" : "b" }"#).unwrap();

        let Some(Value::Cond {
            cond, then, otherwise, ..
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a dynamic conditional");
        };
        assert!(matches!(**cond, Value::Binary { op: ast::BinOp::Eq, .. }));
        assert!(matches!(&**then, Value::Str(value) if value == "a"));
        assert!(matches!(&**otherwise, Value::Str(value) if value == "b"));
    }

    #[test]
    fn folds_constant_indexes_into_literal_containers() {
        let model = model(
            r#"
            vars {
                xs = [10, 20, 30]
                user = { name = "ada", langs = ["en", "fr"] }
            }
            trace "t" {
                input = var.xs[1 + 1]
                output = var.user["name"]
                metrics = { pick = [{ n = 1 }, { n = 2 }][0].n }
                tags = [var.user.langs[1]]
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert!(matches!(fields.input, Some(Value::Num(Number::Int(30)))));
        assert!(matches!(&fields.output, Some(Value::Str(value)) if value == "ada"));
        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Int(1))));
        assert_eq!(tag_text(&fields.tags[0]), "fr");
    }

    #[test]
    fn folds_a_constant_index_selecting_a_dynamic_element() {
        // selection is static even though the selected element is not
        let model = model(r#"trace "t" { input = [range(1, 5), "x"][0] }"#).unwrap();

        assert!(matches!(
            model.traces[0].fields.input,
            Some(Value::Func {
                func: Func::Range(Range::Int { min: 1, max: 5 }),
                ..
            })
        ));
    }

    #[test]
    fn rejects_out_of_bounds_constant_indexes() {
        let source = r#"trace "t" { input = [1, 2][2] }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::IndexOutOfBounds {
                rule: spec::ids::INDEX_BOUNDS,
                index: 2,
                len: 2,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "2");

        let errors = model(r#"trace "t" { input = [1, 2][-1] }"#).unwrap_err();
        assert!(matches!(errors[0].kind(), ErrorKind::IndexOutOfBounds { index: -1, .. }));
    }

    #[test]
    fn rejects_unknown_constant_object_keys() {
        let errors = model(r#"trace "t" { input = { a = 1 }.b }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownObjectKey {
                rule: spec::ids::INDEX_BOUNDS,
                key: "b".to_owned(),
            }
        );
        assert!(errors[0].to_string().contains("object has no key `b`"));
    }

    #[test]
    fn rejects_non_integer_constant_indexes() {
        let errors = model(r#"trace "t" { input = [1, 2][0.5] }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonIntegerIndex {
                rule: spec::ids::INDEXABLE_TARGETS,
            }
        );
    }

    #[test]
    fn rejects_indexing_non_containers() {
        for (source, found) in [
            (r#"trace "t" { input = 5[0] }"#, ExprType::Number),
            (r#"trace "t" { input = "x"[0] }"#, ExprType::String),
            (r#"trace "t" { input = true.a }"#, ExprType::Boolean),
            (r#"trace "t" { input = null[0] }"#, ExprType::Null),
            (r#"trace "t" { input = range(1, 2)[0] }"#, ExprType::Number),
        ] {
            let errors = model(source).unwrap_err();
            assert_eq!(
                errors[0].kind(),
                &ErrorKind::NonIndexableTarget {
                    rule: spec::ids::INDEXABLE_TARGETS,
                    found,
                },
                "source: {source}"
            );
        }
    }

    #[test]
    fn rejects_mismatched_index_types() {
        let errors = model(r#"trace "t" { input = [1, 2]["a"] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::IndexTypeMismatch {
                rule: spec::ids::INDEXABLE_TARGETS,
                expected: "number",
                found: ExprType::String,
            }
        );

        let errors = model(r#"trace "t" { input = { a = 1 }[0] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::IndexTypeMismatch {
                rule: spec::ids::INDEXABLE_TARGETS,
                expected: "string",
                found: ExprType::Number,
            }
        );

        // a heterogeneous choice has no static type, so it can't index
        let errors = model(r#"trace "t" { input = [1, 2][choice(0, "a")] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::IndexTypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
    }

    #[test]
    fn models_dynamic_index_selections() {
        let source = r#"trace "t" { input = [1, 2][range(0, 1)] }"#;
        let model = model(source).unwrap();

        let Some(Value::Index { target, index, range }) = &model.traces[0].fields.input else {
            panic!("expected a dynamic index expression");
        };
        assert!(matches!(&**target, Value::Array(array) if array.elem.len() == 2));
        assert!(matches!(
            **index,
            Value::Func {
                func: Func::Range(Range::Int { min: 0, max: 1 }),
                ..
            }
        ));
        assert_eq!(&source[range.start..range.end], "[1, 2][range(0, 1)]");
    }

    #[test]
    fn types_dynamic_indexes_by_their_unified_element_type() {
        // homogeneous elements type the index, so it can be an operand
        let model = model(r#"trace "t" { metrics = { n = [1, 2][range(0, 1)] * 10 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Binary { .. }));
    }

    #[test]
    fn rejects_untyped_dynamic_indexes_as_operands_and_typed_fields() {
        let errors = model(r#"trace "t" { input = [1, "a"][range(0, 1)] + 1 }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::OperandTypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));

        let errors = model(r#"trace "t" { tags = [["a", "b"][range(0, 1)]] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Abstract,
                ..
            }
        ));
    }

    #[test]
    fn rejects_index_exprs_referencing_vars_in_vars() {
        let errors = model(r#"vars { a = [1] b = var.a[0] } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarInVar { .. }));
    }

    fn ints(value: &Value) -> Vec<i64> {
        let Value::Array(array) = value else {
            panic!("expected an array");
        };
        array
            .elem
            .iter()
            .map(|value| match value {
                Value::Num(Number::Int(value)) => *value,
                other => panic!("expected an integer, found {other:?}"),
            })
            .collect()
    }

    #[test]
    fn folds_constant_slices_with_clamped_bounds() {
        let model = model(
            r#"
            vars { xs = [1, 2, 3, 4] }
            trace "t" {
                input = var.xs[1:3]
                output = var.xs[:2]
                metadata = {
                    tail = var.xs[2:]
                    all = var.xs[:]
                    clamped = var.xs[1:99]
                    empty = var.xs[3:1]
                }
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        assert_eq!(ints(fields.input.as_ref().unwrap()), [2, 3]);
        assert_eq!(ints(fields.output.as_ref().unwrap()), [1, 2]);
        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(ints(&metadata.elem[0].value), [3, 4]);
        assert_eq!(ints(&metadata.elem[1].value), [1, 2, 3, 4]);
        assert_eq!(ints(&metadata.elem[2].value), [2, 3, 4]);
        assert!(ints(&metadata.elem[3].value).is_empty());
    }

    #[test]
    fn folds_slices_into_typed_positions() {
        let model = model(r#"trace "t" { tags = ["a", "b", "c"][1:] }"#).unwrap();

        assert_eq!(
            model.traces[0].fields.tags.iter().map(tag_text).collect::<Vec<_>>(),
            ["b", "c"]
        );
    }

    #[test]
    fn models_dynamic_slice_selections() {
        let source = r#"trace "t" { input = [1, 2, 3][range(0, 1):] }"#;
        let model = model(source).unwrap();

        let Some(Value::Slice {
            target,
            start,
            end,
            range,
        }) = &model.traces[0].fields.input
        else {
            panic!("expected a dynamic slice expression");
        };
        assert!(matches!(&**target, Value::Array(array) if array.elem.len() == 3));
        assert!(matches!(
            start.as_deref(),
            Some(Value::Func {
                func: Func::Range(Range::Int { min: 0, max: 1 }),
                ..
            })
        ));
        assert!(end.is_none());
        assert_eq!(&source[range.start..range.end], "[1, 2, 3][range(0, 1):]");
    }

    #[test]
    fn rejects_invalid_slice_targets_and_bounds() {
        let errors = model(r#"trace "t" { input = 5[0:1] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonSliceableTarget {
                rule: spec::ids::SLICEABLE_TARGETS,
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { input = { a = 1 }[0:1] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonSliceableTarget {
                found: ExprType::Object,
                ..
            }
        ));

        let errors = model(r#"trace "t" { input = [1, 2]["a":] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::SliceTypeMismatch {
                rule: spec::ids::SLICE_BOUNDS,
                found: ExprType::String,
            }
        );

        let errors = model(r#"trace "t" { input = [1, 2][0.5:] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonIntegerSliceBound {
                rule: spec::ids::SLICE_BOUNDS,
            }
        );

        // constant bounds check even when another bound is dynamic
        let source = r#"trace "t" { input = [1, 2][range(0, 1):-1] }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NegativeSliceBound {
                rule: spec::ids::SLICE_BOUNDS,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "-1");
    }

    #[test]
    fn splices_constant_array_spreads() {
        let model = model(
            r#"
            vars { xs = [2, 3] }
            trace "t" { input = [1, ...var.xs, 4] }
            "#,
        )
        .unwrap();

        assert_eq!(ints(model.traces[0].fields.input.as_ref().unwrap()), [1, 2, 3, 4]);
    }

    #[test]
    fn splices_spreads_holding_dynamic_elements() {
        let model = model(r#"vars { xs = [range(1, 5)] } trace "t" { input = [...var.xs] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert!(matches!(
            array.elem[0],
            Value::Func {
                func: Func::Range(Range::Int { min: 1, max: 5 }),
                ..
            }
        ));
    }

    #[test]
    fn merges_object_spreads_with_later_entries_winning() {
        let model = model(
            r#"
            vars { meta = { model = "gpt", temperature = 0.2 } }
            trace "t" {
                metadata = { ...var.meta, temperature = 0.9 }
                metrics = { temperature = 1, ...var.meta }
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        // the explicit key overrides the spread but keeps its position
        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem.len(), 2);
        assert_eq!(metadata.elem[0].key, "model");
        assert_eq!(metadata.elem[1].key, "temperature");
        assert!(matches!(metadata.elem[1].value, Value::Num(Number::Float(value)) if value == 0.9));

        // a later spread also overrides an earlier explicit key
        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Float(value)) if value == 0.2));
    }

    #[test]
    fn rejects_explicit_duplicate_keys_even_through_a_merge() {
        let errors = model(r#"trace "t" { metadata = { ...{}, a = 1, a = 2 } }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateObjectKey {
                rule: spec::ids::UNIQUE_OBJECT_KEYS,
                key: "a".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_mismatched_and_dynamic_spread_operands() {
        let errors = model(r#"trace "t" { input = [...{ a = 1 }] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::SpreadTypeMismatch {
                rule: spec::ids::SPREAD_OPERANDS,
                expected: "array",
                found: ExprType::Object,
            }
        );

        let errors = model(r#"trace "t" { metadata = { ...[1] } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::SpreadTypeMismatch {
                rule: spec::ids::SPREAD_OPERANDS,
                expected: "object",
                found: ExprType::Array,
            }
        );

        // a choice of arrays has no constant shape
        let errors = model(r#"trace "t" { input = [...choice([1], [2])] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::SpreadTypeMismatch {
                found: ExprType::Func,
                ..
            }
        ));
    }

    #[test]
    fn rejects_spreads_referencing_vars_in_vars() {
        let errors = model(r#"vars { a = [1] b = [...var.a] } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarInVar { .. }));
    }

    #[test]
    fn unrolls_array_for_exprs() {
        let model = model(r#"trace "t" { input = [for x in [1, 2, 3] : x * 2] }"#).unwrap();

        assert_eq!(ints(model.traces[0].fields.input.as_ref().unwrap()), [2, 4, 6]);
    }

    #[test]
    fn unrolls_for_exprs_with_index_bindings_and_template_splices() {
        let model = model(r#"trace "t" { input = [for i, x in ["a", "b"] : "${i}-${x}"] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert!(matches!(&array.elem[0], Value::Str(value) if value == "0-a"));
        assert!(matches!(&array.elem[1], Value::Str(value) if value == "1-b"));
    }

    #[test]
    fn unrolls_for_exprs_with_constant_filters() {
        let model = model(r#"trace "t" { input = [for x in [1, 2, 3, 4] : x if x % 2 == 0] }"#).unwrap();

        assert_eq!(ints(model.traces[0].fields.input.as_ref().unwrap()), [2, 4]);
    }

    #[test]
    fn unrolls_for_exprs_over_objects() {
        let model = model(
            r#"
            vars { meta = { a = 1, b = 2, c = 3 } }
            trace "t" {
                input = [for k in var.meta : k]
                metadata = { for k, v in var.meta : k => v if k != "b" }
            }
            "#,
        )
        .unwrap();
        let fields = &model.traces[0].fields;

        let Some(Value::Array(keys)) = &fields.input else {
            panic!("expected an array");
        };
        let keys: Vec<_> = keys
            .elem
            .iter()
            .map(|value| match value {
                Value::Str(value) => value.as_str(),
                other => panic!("expected a string, found {other:?}"),
            })
            .collect();
        assert_eq!(keys, ["a", "b", "c"]);

        let metadata = fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem.len(), 2);
        assert_eq!((metadata.elem[0].key.as_str(), metadata.elem[1].key.as_str()), ("a", "c"));
    }

    #[test]
    fn unrolls_object_for_exprs_over_arrays() {
        let model = model(r#"trace "t" { metadata = { for i, x in ["a", "b"] : x => i } }"#).unwrap();

        let metadata = model.traces[0].fields.metadata.as_ref().unwrap();
        assert_eq!(metadata.elem[0].key, "a");
        assert!(matches!(metadata.elem[0].value, Value::Num(Number::Int(0))));
        assert_eq!(metadata.elem[1].key, "b");
        assert!(matches!(metadata.elem[1].value, Value::Num(Number::Int(1))));
    }

    #[test]
    fn unrolls_for_exprs_into_typed_positions() {
        let model = model(r#"trace "t" { tags = [for x in ["a", "b"] : "t-${x}"] }"#).unwrap();

        // spliced tags stay templates of literal parts, joined during generation
        let tags: Vec<String> = model.traces[0]
            .fields
            .tags
            .iter()
            .map(|tag| {
                tag.parts
                    .iter()
                    .map(|part| match part {
                        Part::Lit(value) => value.as_str(),
                        Part::Ref(_) => panic!("expected literal parts"),
                    })
                    .collect()
            })
            .collect();
        assert_eq!(tags, ["t-a", "t-b"]);
    }

    #[test]
    fn unrolls_for_exprs_keeping_dynamic_bodies() {
        let model = model(r#"trace "t" { input = [for x in [1, 2] : x + range(0, 1)] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert_eq!(array.elem.len(), 2);
        assert!(array.elem.iter().all(|value| matches!(value, Value::Binary { .. })));
    }

    #[test]
    fn unrolls_nested_for_exprs_with_shadowing() {
        let model = model(r#"trace "t" { input = [for x in [[1, 2], [3]] : [for x in x : x * 10]] }"#).unwrap();

        let Some(Value::Array(outer)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        assert_eq!(ints(&outer.elem[0]), [10, 20]);
        assert_eq!(ints(&outer.elem[1]), [30]);
    }

    #[test]
    fn rejects_for_exprs_over_dynamic_or_scalar_collections() {
        let errors = model(r#"trace "t" { input = [for x in choice([1], [2]) : x] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::ForCollectionMismatch {
                rule: spec::ids::FOR_COLLECTIONS,
                found: ExprType::Func,
            }
        );

        let errors = model(r#"trace "t" { input = [for x in 5 : x] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::ForCollectionMismatch {
                found: ExprType::Number,
                ..
            }
        ));
    }

    #[test]
    fn rejects_for_filters_that_are_not_constant_booleans() {
        let errors = model(r#"trace "t" { input = [for x in [range(0, 1)] : x if x > 0] }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonConstantForFilter {
                rule: spec::ids::STATIC_FOR,
                found: ExprType::Abstract,
            }
        );

        let errors = model(r#"trace "t" { input = [for x in [1] : x if 5] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantForFilter {
                found: ExprType::Number,
                ..
            }
        ));
    }

    #[test]
    fn rejects_for_keys_that_are_not_constant_strings() {
        let errors = model(r#"trace "t" { metadata = { for i, x in [1] : i => x } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonConstantForKey {
                rule: spec::ids::STATIC_FOR,
                found: ExprType::Number,
            }
        );

        let errors = model(r#"trace "t" { metadata = { for x in [1] : "${trace.index}" => x } }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::NonConstantForKey {
                found: ExprType::String,
                ..
            }
        ));
    }

    #[test]
    fn rejects_duplicate_keys_produced_by_unrolling() {
        let errors = model(r#"trace "t" { metadata = { for x in ["a", "a"] : x => 1 } }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::DuplicateObjectKey { key, .. } if key == "a"));
    }

    #[test]
    fn rejects_interpolating_non_scalar_bindings() {
        let errors = model(r#"trace "t" { input = [for x in [[1]] : "${x}"] }"#).unwrap_err();

        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonScalarInterpolation {
                rule: spec::ids::SCALAR_INTERPOLATION,
                name: "x".to_owned(),
            }
        );
    }

    #[test]
    fn splices_binding_templates_and_keeps_context_references() {
        let model = model(r#"trace "t" { input = [for x in ["q ${trace.index}"] : "${x}!"] }"#).unwrap();

        let Some(Value::Array(array)) = &model.traces[0].fields.input else {
            panic!("expected an array");
        };
        let Value::Template(template) = &array.elem[0] else {
            panic!("expected a template");
        };
        assert!(matches!(&template.parts[0], Part::Lit(value) if value == "q "));
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::TraceIndex)));
        assert!(matches!(&template.parts[2], Part::Lit(value) if value == "!"));
    }

    #[test]
    fn rejects_for_exprs_referencing_vars_in_vars() {
        let errors = model(r#"vars { a = [1] b = [for x in var.a : x] } trace "t" { input = 1 }"#).unwrap_err();

        assert!(matches!(errors[0].kind(), ErrorKind::VarInVar { .. }));
    }

    #[test]
    fn associates_operator_errors_with_spec_rules() {
        let errors = model(r#"trace "t" { input = 1 + "a" }"#).unwrap_err();
        assert!(
            errors[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::OPERAND_TYPES).unwrap().summary)
        );

        let errors = model(r#"trace "t" { input = 1 / 0 }"#).unwrap_err();
        assert!(
            errors[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::NONZERO_DIVISORS).unwrap().summary)
        );

        let errors = model(r#"trace "t" { input = 1 ? 2 : 3 }"#).unwrap_err();
        assert!(
            errors[0]
                .to_string()
                .contains(spec::SPEC.rule(spec::ids::BOOLEAN_CONDITIONS).unwrap().summary)
        );
    }

    #[test]
    fn models_dynamic_blocks_with_optional_names() {
        let model = model(include_str!("../../tests/fixtures/dynamic.bt")).unwrap();

        let children = &model.traces[0].children;
        let Child::Repeat(repeat) = &children[0] else {
            panic!("expected a repeat child");
        };
        assert_eq!(repeat.name.as_deref(), Some("turns"));
        assert!(matches!(
            repeat.count,
            Value::Func {
                func: Func::Range(_),
                ..
            }
        ));
        assert!(matches!(
            repeat.children.as_slice(),
            [Child::Span(Span {
                kind: SpanKind::Task,
                ..
            })]
        ));

        let Child::Choice(choice) = &children[1] else {
            panic!("expected a choice child");
        };
        assert_eq!(choice.name, None);
        assert_eq!(choice.children.len(), 2);

        let Child::Maybe(maybe) = &children[2] else {
            panic!("expected a maybe child");
        };
        assert!(matches!(maybe.chance, Value::Num(Number::Float(value)) if value == 0.25));
    }

    #[test]
    fn defaults_a_maybe_without_a_chance_to_a_coin_flip() {
        let model = model(r#"trace "t" { maybe { task "extra" {} } }"#).unwrap();

        let Child::Maybe(maybe) = &model.traces[0].children[0] else {
            panic!("expected a maybe child");
        };
        assert!(matches!(maybe.chance, Value::Num(Number::Float(value)) if value == 0.5));
    }

    #[test]
    fn rejects_dynamic_blocks_at_the_root_and_span_blocks_they_forbid() {
        let errors = model(r#"repeat { count = 1 task "turn" {} }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::REPEAT,
                parent: spec::Place::Root,
            }
        );

        let errors = model(r#"trace "t" { choice { vars { a = 1 } task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::BlockNotAllowed {
                block: spec::ids::VARS,
                parent: spec::Place::Block { id: spec::ids::CHOICE },
            }
        );
    }

    #[test]
    fn rejects_invalid_dynamic_bodies() {
        let errors = model(r#"trace "t" { repeat { task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::MissingField {
                block: spec::ids::REPEAT,
                field: spec::ids::COUNT,
            }
        );

        let errors = model(r#"trace "t" { repeat { count = 1 count = 2 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::DuplicateField {
                block: spec::ids::REPEAT,
                field: spec::ids::COUNT,
            }
        );

        // span fields don't leak into dynamic bodies
        let errors = model(r#"trace "t" { maybe { input = "hi" task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::UnknownField {
                block: spec::ids::MAYBE,
                keyword: "input".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_invalid_counts_and_chances() {
        let source = r#"trace "t" { repeat { count = "three" task "turn" {} } }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::TypeMismatch {
                block: spec::ids::REPEAT,
                field: spec::ids::COUNT,
                expected: &spec::ExprType::Number,
                found: ExprType::String,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "\"three\"");

        let errors = model(r#"trace "t" { repeat { count = 0 - 2 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NegativeRepeatCount {
                rule: spec::ids::REPEAT_COUNT,
            }
        );

        let errors = model(r#"trace "t" { repeat { count = 1.5 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::NonIntegerRepeatCount {
                rule: spec::ids::REPEAT_COUNT,
            }
        );

        let errors = model(r#"trace "t" { maybe { chance = 1.5 task "turn" {} } }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::ChanceOutOfRange {
                rule: spec::ids::MAYBE_CHANCE,
            }
        );
    }

    #[test]
    fn allows_zero_counts_and_dynamic_bounds() {
        let model = model(
            r#"
            trace "t" {
                repeat { count = 0 task "turn" {} }
                maybe { chance = range(0.0, 1.0) task "extra" {} }
            }
            "#,
        )
        .unwrap();

        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat child");
        };
        assert!(matches!(repeat.count, Value::Num(Number::Int(0))));
    }

    #[test]
    fn rejects_empty_dynamic_blocks() {
        let errors = model(r#"trace "t" { choice {} }"#).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::EmptyDynamicBlock {
                rule: spec::ids::DYNAMIC_CHILDREN,
                block: spec::ids::CHOICE,
            }
        );
    }

    #[test]
    fn scopes_repeat_index_to_repeat_blocks() {
        let source = r#"trace "t" { input = "turn ${repeat.index}" }"#;
        let errors = model(source).unwrap_err();
        assert_eq!(
            errors[0].kind(),
            &ErrorKind::RepeatIndexOutsideRepeat {
                rule: spec::ids::REPEAT_INDEX,
            }
        );
        let range = errors[0].range();
        assert_eq!(&source[range.start..range.end], "${repeat.index}");

        // valid inside a repeat, including through nested dynamic blocks
        let model = model(
            r#"
            trace "t" {
                repeat {
                    count = 2
                    maybe { task "turn" { input = "turn ${repeat.index}" } }
                }
            }
            "#,
        )
        .unwrap();
        let Child::Repeat(repeat) = &model.traces[0].children[0] else {
            panic!("expected a repeat child");
        };
        let Child::Maybe(maybe) = &repeat.children[0] else {
            panic!("expected a maybe child");
        };
        let Some(Value::Template(template)) = &as_span(&maybe.children[0]).fields.input else {
            panic!("expected a template input");
        };
        assert!(matches!(template.parts[1], Part::Ref(CtxRef::RepeatIndex)));
    }
}
