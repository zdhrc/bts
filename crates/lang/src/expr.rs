use crate::{
    ast,
    diag::{Diag, DiagPhase, Diags, SrcRange},
    model::*,
    spec,
};
use std::{collections::HashMap, fmt};
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ExprType {
    String,
    Number,
    Boolean,
    Null,
    Array,
    Object,
    Func,
    // a residual expr whose concrete type is only known during evaluation
    Abstract,
    Graph,
}

impl ExprType {
    fn of(folded: &Folded) -> Self {
        match &folded.kind {
            FoldedKind::Array(_) => Self::Array,
            FoldedKind::Object(_) => Self::Object,
            FoldedKind::Value(value) => match value {
                Value::Str(_) | Value::Template(_) => Self::String,
                Value::Num(_) => Self::Number,
                Value::Bool(_) => Self::Boolean,
                Value::Null => Self::Null,
                Value::Array(_) => Self::Array,
                Value::Object(_) => Self::Object,
                Value::Func { .. } => Self::Func,
                // constant exprs fold to values, only dynamic ones get here
                Value::Input { .. }
                | Value::FieldRef(_)
                | Value::BindingRef(_, _)
                | Value::GraphRef { .. }
                | Value::Context { .. }
                | Value::Unary { .. }
                | Value::Binary { .. }
                | Value::Cond { .. }
                | Value::Index { .. }
                | Value::Slice { .. } => Self::Abstract,
            },
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
            Self::Graph => "graph element",
        })
    }
}

// an expr evaluated in place: constant parts folded to model values, dynamic
// parts left residual, containers stay structured so field checks can point
// at per-element ranges
#[derive(Debug, Clone)]
struct Folded {
    kind: FoldedKind,
    range: SrcRange,
}

#[derive(Debug, Clone)]
enum FoldedKind {
    // scalars, templates, funcs, refs, and residual operator exprs
    Value(Value),
    Array(Vec<Folded>),
    Object(Vec<FoldedField>),
}

#[derive(Debug, Clone)]
struct FoldedField {
    key: String,
    value: Folded,
    range: SrcRange,
}

// an array entry mid-fold, spreads survive only when their shape is unknown
enum FoldedElem {
    Item(Folded),
    Spread(Folded),
}

impl Folded {
    fn new(kind: FoldedKind, range: SrcRange) -> Self {
        Self { kind, range }
    }

    fn value(value: Value, range: SrcRange) -> Self {
        Self::new(FoldedKind::Value(value), range)
    }

    // drops ranges once checks are done, the model keeps only failure sites
    fn into_value(self) -> Value {
        match self.kind {
            FoldedKind::Value(value) => value,
            FoldedKind::Array(values) => Value::Array(Array {
                elem: values.into_iter().map(|value| ArrayElem::Item(value.into_value())).collect(),
            }),
            FoldedKind::Object(fields) => Value::Object(Object {
                elem: fields
                    .into_iter()
                    .map(|field| ObjectField {
                        key: field.key,
                        value: field.value.into_value(),
                    })
                    .collect(),
            }),
        }
    }
}

#[derive(Default)]
struct Scope {
    loops: HashMap<String, Folded>,
}
enum Iteration {
    Skipped,
    Invalid,
    Value(Folded),
    Entry { key: String, range: SrcRange, value: Folded },
}

#[derive(Clone)]
pub(crate) struct ReferenceValue {
    pub value: Value,
    pub bindings: Vec<(BindingId, Value, FieldType)>,
}
type Resolver<'a> = dyn FnMut(ast::Expr) -> Result<ReferenceValue, Diags> + 'a;
struct ExpressionModeler<'a> {
    scopes: Vec<Scope>,
    vars: HashMap<BindingId, Folded>,
    types: HashMap<BindingId, FieldType>,
    errors: Errors,
    reference_errors: Diags,
    // the functions the provider contributes, checked from their descriptors
    functions: &'a [FunctionDesc],
    resolve: &'a mut Resolver<'a>,
}
impl<'a> ExpressionModeler<'a> {
    fn var_value(&self, name: &BindingId) -> Option<&Folded> {
        let mut value = self.vars.get(name)?;
        for _ in 0..self.vars.len() {
            let FoldedKind::Value(Value::BindingRef(id, _)) = &value.kind else {
                return Some(value);
            };
            value = self.vars.get(id)?;
        }
        None
    }
    fn lookup_loop(&self, name: &str) -> Option<Folded> {
        self.scopes.iter().rev().find_map(|scope| scope.loops.get(name).cloned())
    }
    fn resolve_expr(&mut self, expr: ast::Expr) -> Option<Folded> {
        let range = expr.range;
        match (self.resolve)(expr) {
            Ok(reference) => {
                for (id, value, ty) in reference.bindings {
                    self.types.insert(id, ty);
                    self.vars.insert(id, folded_value(value, range));
                }
                Some(folded_value(reference.value, range))
            }
            Err(errors) => {
                self.reference_errors.extend(errors);
                None
            }
        }
    }
    fn fold_expr(&mut self, expr: ast::Expr) -> Option<Folded> {
        if reference_head(&expr).is_some_and(|name| !self.scopes.iter().rev().any(|scope| scope.loops.contains_key(name)))
            && matches!(expr.kind, ast::ExprKind::Index { .. } | ast::ExprKind::Slice { .. })
        {
            return self.resolve_expr(expr);
        }
        let ast::Expr { kind, range } = expr;
        match kind {
            ast::ExprKind::Upstream => {
                self.reference_errors.push(Diag {
                    when: DiagPhase::Validation,
                    what: "upstream markers belong on instance content fields".to_owned(),
                    r#where: range,
                });
                None
            }
            ast::ExprKind::GraphRef { .. } | ast::ExprKind::GraphMember { .. } | ast::ExprKind::PatternRef(_) => {
                self.resolve_expr(ast::Expr { kind, range })
            }
            ast::ExprKind::Str(value) => Some(Folded::value(Value::Str(value), range)),
            ast::ExprKind::Template(parts) => self.fold_template(parts, range),
            ast::ExprKind::Num(raw) => self
                .model_number(raw, false, range)
                .map(|number| Folded::value(Value::Num(number), range)),
            ast::ExprKind::Bool(value) => Some(Folded::value(Value::Bool(value), range)),
            ast::ExprKind::Null => Some(Folded::value(Value::Null, range)),
            ast::ExprKind::Array(values) => self.fold_array(values, range),
            ast::ExprKind::Object(items) => self.fold_object(items, range),
            ast::ExprKind::Ref { path } => {
                if let Some(mut value) = path.first().and_then(|name| self.lookup_loop(name)) {
                    for key in path.into_iter().skip(1) {
                        value = self.fold_index(value, Folded::value(Value::Str(key), range), range)?;
                    }
                    Some(value)
                } else {
                    self.resolve_expr(ast::Expr {
                        kind: ast::ExprKind::Ref { path },
                        range,
                    })
                }
            }
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
                let func = self.model_func(name, folded, range)?;
                Some(Folded::value(Value::Func { func, range }, range))
            }
            ast::ExprKind::Unary { op, operand } => self.fold_unary(op, *operand, range),
            ast::ExprKind::Binary { op, lhs, rhs } => self.fold_binary(op, *lhs, *rhs, range),
            ast::ExprKind::Cond { cond, then, otherwise } => self.fold_cond(*cond, *then, *otherwise, range),
            ast::ExprKind::Index { target, index } => {
                // fold both sides first so each reports its own diagnostics
                let target = self.fold_expr(*target);
                let index = self.fold_expr(*index);
                let (target, index) = (target?, index?);
                self.fold_index(target, index, range)
            }
            ast::ExprKind::Slice { target, start, end } => self.fold_slice(*target, start, end, range),
            ast::ExprKind::For {
                bindings,
                collection,
                key,
                body,
                cond,
            } => self.fold_for(bindings, *collection, key, *body, cond, range),
            ast::ExprKind::Spread(_) => unreachable!("spreads only parse inside arrays and objects"),
        }
    }

    fn fold_template(&mut self, parts: Vec<ast::TemplatePart>, range: SrcRange) -> Option<Folded> {
        let mut modeled: Vec<Part> = Vec::with_capacity(parts.len());
        let mut valid = true;

        for part in parts {
            match part {
                ast::TemplatePart::Lit(value) => modeled.push(Part::Lit(value)),
                ast::TemplatePart::Ref { expr, range } => {
                    let Some(folded) = self.fold_expr(expr) else {
                        valid = false;
                        continue;
                    };
                    match folded.kind {
                        FoldedKind::Value(Value::Str(text)) => modeled.push(Part::Lit(text)),
                        FoldedKind::Value(Value::Num(number)) => modeled.push(Part::Lit(num_text(number))),
                        FoldedKind::Value(Value::Bool(value)) => {
                            modeled.push(Part::Lit(if value { "true" } else { "false" }.to_owned()));
                        }
                        // a template value splices its parts inline
                        FoldedKind::Value(Value::Template(template)) => modeled.extend(template.parts),
                        // dynamic defs resolve per scope instantiation, scalar by type
                        FoldedKind::Value(Value::BindingRef(name, reference_range)) => {
                            let var_type = self.value_type(&Value::BindingRef(name, reference_range));
                            match var_type {
                                Some(StaticType::String | StaticType::Number | StaticType::Boolean) => {
                                    modeled.push(Part::BindingRef(name));
                                }
                                _ => {
                                    self.errors.push(Error::new(
                                        ErrorKind::NonScalarInterpolation {
                                            rule: spec::ids::SCALAR_INTERPOLATION,
                                            found: self
                                                .found_type(&Folded::value(Value::BindingRef(name, reference_range), range)),
                                        },
                                        range,
                                    ));
                                    valid = false;
                                }
                            }
                        }
                        FoldedKind::Value(value) => {
                            let folded = Folded::value(value, range);
                            let scalar = matches!(
                                self.static_type(&folded),
                                Some(StaticType::String | StaticType::Number | StaticType::Boolean)
                            );
                            if scalar || self.defers_to_evaluation(&folded) {
                                modeled.push(Part::Dynamic(folded.into_value()));
                            } else {
                                self.errors.push(Error::new(
                                    ErrorKind::NonScalarInterpolation {
                                        rule: spec::ids::SCALAR_INTERPOLATION,
                                        found: self.found_type(&folded),
                                    },
                                    range,
                                ));
                                valid = false;
                            }
                        }
                        kind @ (FoldedKind::Array(_) | FoldedKind::Object(_)) => {
                            self.errors.push(Error::new(
                                ErrorKind::NonScalarInterpolation {
                                    rule: spec::ids::SCALAR_INTERPOLATION,
                                    found: self.found_type(&Folded::new(kind, range)),
                                },
                                range,
                            ));
                            valid = false;
                        }
                    }
                }
            }
        }

        if !valid {
            return None;
        }

        // an all-literal template folds to a plain string so downstream only
        // sees templates that actually resolve
        if modeled.iter().all(|part| matches!(part, Part::Lit(_))) {
            let joined = modeled
                .into_iter()
                .map(|part| match part {
                    Part::Lit(value) => value,
                    Part::BindingRef(_) | Part::Dynamic(_) => unreachable!("all parts are literal"),
                })
                .collect();
            return Some(Folded::value(Value::Str(joined), range));
        }

        Some(Folded::value(Value::Template(Template { parts: modeled }), range))
    }

    fn fold_array(&mut self, values: Vec<ast::Expr>, range: SrcRange) -> Option<Folded> {
        let mut folded = Vec::with_capacity(values.len());
        let mut valid = true;
        for value in values {
            match value.kind {
                // constant-shape spreads splice in place, reference-fed ones
                // stay residual and splice at evaluation
                ast::ExprKind::Spread(operand) => match self.fold_array_spread(*operand) {
                    Some(elems) => folded.extend(elems),
                    None => valid = false,
                },
                _ => match self.fold_expr(value) {
                    Some(value) => folded.push(FoldedElem::Item(value)),
                    None => valid = false,
                },
            }
        }
        if !valid {
            return None;
        }

        // an array with a residual spread has no static positions, so it
        // lowers to a value instead of a folded container
        if folded.iter().any(|elem| matches!(elem, FoldedElem::Spread(_))) {
            let elem = folded
                .into_iter()
                .map(|elem| match elem {
                    FoldedElem::Item(value) => ArrayElem::Item(value.into_value()),
                    FoldedElem::Spread(value) => ArrayElem::Spread(value.into_value()),
                })
                .collect();
            return Some(Folded::value(Value::Array(Array { elem }), range));
        }

        let items = folded
            .into_iter()
            .map(|elem| match elem {
                FoldedElem::Item(value) => value,
                FoldedElem::Spread(_) => unreachable!("residual spreads returned above"),
            })
            .collect();
        Some(Folded::new(FoldedKind::Array(items), range))
    }

    fn fold_unary(&mut self, op: ast::UnaryOp, operand: ast::Expr, range: SrcRange) -> Option<Folded> {
        // a minus folds into an adjacent number literal so i64::MIN parses
        if op == ast::UnaryOp::Neg
            && let ast::ExprKind::Num(raw) = operand.kind
        {
            let number = self.model_number(raw, true, range)?;
            return Some(Folded::value(Value::Num(number), range));
        }

        let operand = self.fold_expr(operand)?;

        let required = match op {
            ast::UnaryOp::Neg => StaticType::Number,
            ast::UnaryOp::Not => StaticType::Boolean,
        };
        if !self.check_operand(&operand, required, op.to_string()) {
            return None;
        }

        match operand.kind {
            FoldedKind::Value(Value::Bool(value)) if op == ast::UnaryOp::Not => Some(Folded::value(Value::Bool(!value), range)),
            // negating a constant computes now, overflow can only hit i64::MIN
            FoldedKind::Value(Value::Num(number)) => {
                let negated = match number {
                    Number::Int(value) => value.checked_neg().map(Number::Int),
                    Number::Float(value) => Some(Number::Float(-value)),
                };
                match negated {
                    Some(number) => Some(Folded::value(Value::Num(number), range)),
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::NonFiniteResult {
                                rule: spec::ids::FINITE_NUMBERS,
                            },
                            range,
                        ));
                        None
                    }
                }
            }
            kind => Some(Folded::value(
                Value::Unary {
                    op: model_unary_op(op),
                    operand: Box::new(Folded::new(kind, operand.range).into_value()),
                    range,
                },
                range,
            )),
        }
    }

    fn fold_binary(&mut self, op: ast::BinOp, lhs: ast::Expr, rhs: ast::Expr, range: SrcRange) -> Option<Folded> {
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
            && let FoldedKind::Value(Value::Num(number)) = &rhs.kind
            && float_bound(number.clone()) == 0.0
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

        if is_constant(&lhs) && is_constant(&rhs) {
            return self.eval_binary(op, const_scalar(&lhs), const_scalar(&rhs), range);
        }

        Some(Folded::value(
            Value::Binary {
                op: model_bin_op(op),
                lhs: Box::new(lhs.into_value()),
                rhs: Box::new(rhs.into_value()),
                range,
            },
            range,
        ))
    }

    fn fold_cond(&mut self, cond: ast::Expr, then: ast::Expr, otherwise: ast::Expr, range: SrcRange) -> Option<Folded> {
        let cond = self.fold_expr(cond);
        let then = self.fold_expr(then);
        let otherwise = self.fold_expr(otherwise);
        let (cond, then, otherwise) = (cond?, then?, otherwise?);

        if self.static_type(&cond) != Some(StaticType::Boolean) && !self.defers_to_evaluation(&cond) {
            self.errors.push(Error::new(
                ErrorKind::NonBooleanCondition {
                    rule: spec::ids::BOOLEAN_CONDITIONS,
                    found: self.found_type(&cond),
                },
                cond.range,
            ));
            return None;
        }

        // a constant condition picks its branch during validation
        if let FoldedKind::Value(Value::Bool(value)) = cond.kind {
            let taken = if value { then } else { otherwise };
            return Some(Folded::new(taken.kind, range));
        }

        Some(Folded::value(
            Value::Cond {
                cond: Box::new(cond.into_value()),
                then: Box::new(then.into_value()),
                otherwise: Box::new(otherwise.into_value()),
            },
            range,
        ))
    }

    // inspect known collection structure without evaluating its dynamic leaves
    fn selection_shape(&self, value: &Folded) -> Option<Folded> {
        match &value.kind {
            FoldedKind::Value(Value::BindingRef(id, _)) => self.selection_shape(self.var_value(id)?),
            FoldedKind::Value(Value::Index { target, index, .. }) => {
                let target = self.selection_shape(&folded_value((**target).clone(), value.range))?;
                let selected = match (&target.kind, index.as_ref()) {
                    (FoldedKind::Array(values), Value::Num(Number::Int(index))) => values.get(usize::try_from(*index).ok()?)?,
                    (FoldedKind::Object(fields), Value::Str(key)) => &fields.iter().find(|field| &field.key == key)?.value,
                    _ => return None,
                };
                self.selection_shape(selected)
            }
            _ => Some(value.clone()),
        }
    }

    fn fold_index(&mut self, target: Folded, index: Folded, range: SrcRange) -> Option<Folded> {
        if matches!(&target.kind, FoldedKind::Value(Value::BindingRef(_, _) | Value::Index { .. }))
            && let Some(definition) = self.selection_shape(&target)
            && matches!(definition.kind, FoldedKind::Array(_) | FoldedKind::Object(_))
            && is_constant(&index)
        {
            let selected = self.fold_index(definition, index.clone(), range)?;
            if static_value(&selected.clone().into_value()) {
                return Some(selected);
            }
            return Some(Folded::value(
                Value::Index {
                    target: Box::new(target.into_value()),
                    index: Box::new(index.into_value()),
                    range,
                },
                range,
            ));
        }
        let expected = match self.static_type(&target) {
            Some(StaticType::Array) => StaticType::Number,
            Some(StaticType::Object) => StaticType::String,
            // a target fed by external context is only shaped at evaluation
            None if self.folded_reaches_context(&target) => {
                return Some(Folded::value(
                    Value::Index {
                        target: Box::new(target.into_value()),
                        index: Box::new(index.into_value()),
                        range,
                    },
                    range,
                ));
            }
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::NonIndexableTarget {
                        rule: spec::ids::INDEXABLE_TARGETS,
                        found: self.found_type(&target),
                    },
                    target.range,
                ));
                return None;
            }
        };

        if self.static_type(&index) != Some(expected) && !self.defers_to_evaluation(&index) {
            self.errors.push(Error::new(
                ErrorKind::IndexTypeMismatch {
                    rule: spec::ids::INDEXABLE_TARGETS,
                    expected: type_name(expected),
                    found: self.found_type(&index),
                },
                index.range,
            ));
            return None;
        }

        // a constant index into a container literal selects its element during
        // validation, the element itself may still be dynamic
        match target.kind {
            FoldedKind::Array(values) if is_constant(&index) => {
                let Const::Num(number) = const_scalar(&index) else {
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
                        Some(Folded::new(selected.kind, range))
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
            FoldedKind::Object(fields) if is_constant(&index) => {
                let Const::Str(key) = const_scalar(&index) else {
                    unreachable!("index was type checked as a string");
                };

                let selected = fields.into_iter().find_map(|field| (field.key == key).then_some(field.value));
                match selected {
                    Some(value) => Some(Folded::new(value.kind, range)),
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
            kind => Some(Folded::value(
                Value::Index {
                    target: Box::new(Folded::new(kind, target.range).into_value()),
                    index: Box::new(index.into_value()),
                    range,
                },
                range,
            )),
        }
    }

    fn fold_array_spread(&mut self, operand: ast::Expr) -> Option<Vec<FoldedElem>> {
        let operand = self.fold_expr(operand)?;

        // a dynamic var splices as accessors into the one evaluated binding
        if let FoldedKind::Value(Value::BindingRef(name, _)) = &operand.kind {
            let ranges = match self.var_value(name).map(|value| &value.kind) {
                Some(FoldedKind::Array(values)) => Some(values.iter().map(|value| value.range).collect::<Vec<_>>()),
                _ => None,
            };
            if let Some(ranges) = ranges {
                return Some(
                    ranges
                        .into_iter()
                        .enumerate()
                        .map(|(index, range)| {
                            FoldedElem::Item(Folded::value(
                                Value::Index {
                                    target: Box::new(Value::BindingRef(*name, operand.range)),
                                    index: Box::new(Value::Num(Number::Int(index as i64))),
                                    range,
                                },
                                range,
                            ))
                        })
                        .collect(),
                );
            }
        }

        // arrays whose shape only exists at evaluation splice there
        let dynamic = self.static_type(&operand) == Some(StaticType::Array) && !matches!(operand.kind, FoldedKind::Array(_));
        if dynamic || self.defers_to_evaluation(&operand) {
            return Some(vec![FoldedElem::Spread(operand)]);
        }

        match operand.kind {
            FoldedKind::Array(values) => Some(values.into_iter().map(FoldedElem::Item).collect()),
            kind => {
                let operand = Folded::new(kind, operand.range);
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

    // later entries win over keys a spread introduced, two explicit keys collide
    fn fold_object(&mut self, items: Vec<ast::ObjectItem>, range: SrcRange) -> Option<Folded> {
        let mut merged: Vec<(FoldedField, bool)> = Vec::new();
        let mut valid = true;

        for item in items {
            match item {
                ast::ObjectItem::Attr(attr) => match self.fold_expr(attr.value) {
                    Some(value) => {
                        let field = FoldedField {
                            key: attr.key,
                            value,
                            range: attr.range,
                        };
                        match merged.iter_mut().find(|(existing, _)| existing.key == field.key) {
                            Some((_, true)) => {
                                self.errors.push(Error::new(
                                    ErrorKind::DuplicateObjectKey {
                                        rule: spec::ids::UNIQUE_OBJECT_KEYS,
                                        key: field.key,
                                    },
                                    field.range,
                                ));
                                valid = false;
                            }
                            Some(slot) => *slot = (field, true),
                            None => merged.push((field, true)),
                        }
                    }
                    None => valid = false,
                },
                ast::ObjectItem::Spread(operand) => match self.fold_object_spread(operand) {
                    Some(fields) => {
                        for field in fields {
                            match merged.iter_mut().find(|(existing, _)| existing.key == field.key) {
                                Some(slot) => *slot = (field, false),
                                None => merged.push((field, false)),
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

        let fields = merged.into_iter().map(|(field, _)| field).collect();
        Some(Folded::new(FoldedKind::Object(fields), range))
    }

    fn fold_object_spread(&mut self, operand: ast::Expr) -> Option<Vec<FoldedField>> {
        let operand = self.fold_expr(operand)?;

        // a dynamic var splices as accessors into the one evaluated binding
        if let FoldedKind::Value(Value::BindingRef(name, _)) = &operand.kind {
            let keys = match self.var_value(name).map(|value| &value.kind) {
                Some(FoldedKind::Object(fields)) => Some(
                    fields
                        .iter()
                        .map(|field| (field.key.clone(), field.range))
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            };
            let Some(keys) = keys else {
                self.errors.push(Error::new(
                    ErrorKind::SpreadTypeMismatch {
                        rule: spec::ids::SPREAD_OPERANDS,
                        expected: "object",
                        found: ExprType::of(&operand),
                    },
                    operand.range,
                ));
                return None;
            };
            return Some(
                keys.into_iter()
                    .map(|(key, range)| FoldedField {
                        value: Folded::value(
                            Value::Index {
                                target: Box::new(Value::BindingRef(*name, operand.range)),
                                index: Box::new(Value::Str(key.clone())),
                                range,
                            },
                            range,
                        ),
                        key,
                        range,
                    })
                    .collect(),
            );
        }

        match operand.kind {
            FoldedKind::Object(fields) => Some(fields),
            kind => {
                let operand = Folded::new(kind, operand.range);
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

    fn fold_slice(
        &mut self,
        target: ast::Expr,
        start: Option<Box<ast::Expr>>,
        end: Option<Box<ast::Expr>>,
        range: SrcRange,
    ) -> Option<Folded> {
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

        self.slice_values(target, start, end, range)
    }

    fn slice_values(&mut self, target: Folded, start: Option<Folded>, end: Option<Folded>, range: SrcRange) -> Option<Folded> {
        let sliceable = self.static_type(&target) == Some(StaticType::Array)
            // a target fed by external context is only shaped at evaluation
            || (self.static_type(&target).is_none() && self.folded_reaches_context(&target));
        if !sliceable {
            self.errors.push(Error::new(
                ErrorKind::NonSliceableTarget {
                    rule: spec::ids::SLICEABLE_TARGETS,
                    found: self.found_type(&target),
                },
                target.range,
            ));
            return None;
        }

        let mut valid = true;
        for bound in [&start, &end].into_iter().flatten() {
            if self.static_type(bound) != Some(StaticType::Number) && !self.defers_to_evaluation(bound) {
                self.errors.push(Error::new(
                    ErrorKind::SliceTypeMismatch {
                        rule: spec::ids::SLICE_BOUNDS,
                        found: self.found_type(bound),
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
        if known && matches!(target.kind, FoldedKind::Array(_)) {
            let FoldedKind::Array(values) = target.kind else {
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
            return Some(Folded::new(FoldedKind::Array(selected), range));
        }

        Some(Folded::value(
            Value::Slice {
                target: Box::new(target.into_value()),
                start: start.map(|bound| Box::new(bound.into_value())),
                end: end.map(|bound| Box::new(bound.into_value())),
                range,
            },
            range,
        ))
    }

    // some(none) = dynamic, checked during evaluation, none = diagnostic pushed
    fn check_slice_bound(&mut self, bound: &Folded) -> Option<Option<usize>> {
        if !is_constant(bound) {
            return Some(None);
        }

        let Const::Num(number) = const_scalar(bound) else {
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

        match value {
            // oversized positive bounds still clamp to the array length
            value if value >= 0 => Some(Some(usize::try_from(value).unwrap_or(usize::MAX))),
            _ => {
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

    // unrolls comprehensions during validation, bindings live in a loop scope frame
    fn fold_for(
        &mut self,
        bindings: Vec<String>,
        collection: ast::Expr,
        key: Option<Box<ast::Expr>>,
        body: ast::Expr,
        cond: Option<Box<ast::Expr>>,
        range: SrcRange,
    ) -> Option<Folded> {
        let collection = self.fold_expr(collection)?;
        let iterations = self.for_iterations(&bindings, collection)?;

        let object = key.is_some();
        let mut values = Vec::new();
        let mut fields: Vec<FoldedField> = Vec::new();
        let mut valid = true;

        for loops in iterations {
            self.scopes.push(Scope { loops });
            let outcome = self.fold_iteration(&key, &body, &cond);
            self.scopes.pop();

            match outcome {
                Iteration::Skipped => {}
                Iteration::Invalid => valid = false,
                Iteration::Value(value) => values.push(value),
                Iteration::Entry { key, range, value } => {
                    // duplicate keys diagnose like literal object entries
                    if fields.iter().any(|field| field.key == key) {
                        self.errors.push(Error::new(
                            ErrorKind::DuplicateObjectKey {
                                rule: spec::ids::UNIQUE_OBJECT_KEYS,
                                key,
                            },
                            range,
                        ));
                        valid = false;
                    } else {
                        fields.push(FoldedField { key, value, range });
                    }
                }
            }
        }

        if !valid {
            return None;
        }

        let kind = if object {
            FoldedKind::Object(fields)
        } else {
            FoldedKind::Array(values)
        };
        Some(Folded::new(kind, range))
    }

    // per-iteration loop bindings, a dynamic var iterates by its definition's
    // shape, bindings access the one evaluated value by position
    fn for_iterations(&mut self, bindings: &[String], collection: Folded) -> Option<Vec<HashMap<String, Folded>>> {
        match collection.kind {
            FoldedKind::Array(values) => Some(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let mut map = HashMap::new();
                        if bindings.len() == 2 {
                            let index = Folded::value(Value::Num(Number::Int(index as i64)), value.range);
                            map.insert(bindings[0].clone(), index);
                            map.insert(bindings[1].clone(), value);
                        } else {
                            map.insert(bindings[0].clone(), value);
                        }
                        map
                    })
                    .collect(),
            ),
            FoldedKind::Object(fields) => Some(
                fields
                    .into_iter()
                    .map(|field| {
                        let mut map = HashMap::new();
                        let key = Folded::value(Value::Str(field.key), field.range);
                        map.insert(bindings[0].clone(), key);
                        if bindings.len() == 2 {
                            map.insert(bindings[1].clone(), field.value);
                        }
                        map
                    })
                    .collect(),
            ),
            FoldedKind::Value(Value::BindingRef(name, reference_range)) => {
                enum VarShape {
                    Array(Vec<SrcRange>),
                    Object(Vec<(String, SrcRange)>),
                }

                let shape = match self.var_value(&name).map(|value| &value.kind) {
                    Some(FoldedKind::Array(values)) => Some(VarShape::Array(values.iter().map(|value| value.range).collect())),
                    Some(FoldedKind::Object(fields)) => Some(VarShape::Object(
                        fields.iter().map(|field| (field.key.clone(), field.range)).collect(),
                    )),
                    _ => None,
                };
                let Some(shape) = shape else {
                    let collection = Folded::value(Value::BindingRef(name, reference_range), collection.range);
                    self.errors.push(Error::new(
                        ErrorKind::ForCollectionMismatch {
                            rule: spec::ids::FOR_COLLECTIONS,
                            found: ExprType::of(&collection),
                        },
                        collection.range,
                    ));
                    return None;
                };

                let accessor = |index: Value, range| {
                    Folded::value(
                        Value::Index {
                            target: Box::new(Value::BindingRef(name, reference_range)),
                            index: Box::new(index),
                            range,
                        },
                        range,
                    )
                };
                match shape {
                    VarShape::Array(ranges) => Some(
                        ranges
                            .into_iter()
                            .enumerate()
                            .map(|(index, range)| {
                                let mut map = HashMap::new();
                                let element = accessor(Value::Num(Number::Int(index as i64)), range);
                                if bindings.len() == 2 {
                                    let index = Folded::value(Value::Num(Number::Int(index as i64)), range);
                                    map.insert(bindings[0].clone(), index);
                                    map.insert(bindings[1].clone(), element);
                                } else {
                                    map.insert(bindings[0].clone(), element);
                                }
                                map
                            })
                            .collect(),
                    ),
                    VarShape::Object(keys) => Some(
                        keys.into_iter()
                            .map(|(key, range)| {
                                let mut map = HashMap::new();
                                map.insert(bindings[0].clone(), Folded::value(Value::Str(key.clone()), range));
                                if bindings.len() == 2 {
                                    map.insert(bindings[1].clone(), accessor(Value::Str(key), range));
                                }
                                map
                            })
                            .collect(),
                    ),
                }
            }
            // graph handles are identities, not collections to unroll
            kind @ FoldedKind::Value(Value::GraphRef { .. }) => {
                let collection = Folded::new(kind, collection.range);
                self.errors.push(Error::new(
                    ErrorKind::ForGraphRefCollection {
                        rule: spec::ids::STATIC_STRUCTURE,
                    },
                    collection.range,
                ));
                None
            }
            kind => {
                let collection = Folded::new(kind, collection.range);
                self.errors.push(Error::new(
                    ErrorKind::ForCollectionMismatch {
                        rule: spec::ids::FOR_COLLECTIONS,
                        found: ExprType::of(&collection),
                    },
                    collection.range,
                ));
                None
            }
        }
    }

    // filter first so skipped elements never fold their bodies
    fn fold_iteration(&mut self, key: &Option<Box<ast::Expr>>, body: &ast::Expr, cond: &Option<Box<ast::Expr>>) -> Iteration {
        if let Some(cond) = cond {
            let Some(cond) = self.fold_expr((**cond).clone()) else {
                return Iteration::Invalid;
            };
            match cond.kind {
                FoldedKind::Value(Value::Bool(true)) => {}
                FoldedKind::Value(Value::Bool(false)) => return Iteration::Skipped,
                _ => {
                    self.errors.push(Error::new(
                        ErrorKind::NonConstantForFilter {
                            rule: spec::ids::STATIC_FOR,
                            found: ExprType::of(&cond),
                        },
                        cond.range,
                    ));
                    return Iteration::Invalid;
                }
            }
        }

        let Some(value) = self.fold_expr(body.clone()) else {
            return Iteration::Invalid;
        };

        match key {
            Some(key) => {
                let Some(key) = self.fold_expr((**key).clone()) else {
                    return Iteration::Invalid;
                };
                match const_string(&key) {
                    Some(text) => Iteration::Entry {
                        key: text,
                        range: key.range,
                        value,
                    },
                    None => {
                        self.errors.push(Error::new(
                            ErrorKind::NonConstantForKey {
                                rule: spec::ids::STATIC_FOR,
                                found: ExprType::of(&key),
                            },
                            key.range,
                        ));
                        Iteration::Invalid
                    }
                }
            }
            None => Iteration::Value(value),
        }
    }

    fn check_operand(&mut self, operand: &Folded, required: StaticType, op: String) -> bool {
        if let FoldedKind::Value(value) = &operand.kind {
            match value {
                Value::Cond { then, otherwise, .. } => {
                    let then = folded_value((**then).clone(), operand.range);
                    let otherwise = folded_value((**otherwise).clone(), operand.range);
                    let left = self.check_operand(&then, required, op.clone());
                    let right = self.check_operand(&otherwise, required, op);
                    return left && right;
                }
                Value::BindingRef(id, _) if self.types.get(id).and_then(|ty| schema_static_type(*ty)).is_none() => {
                    if let Some(value) = self.vars.get(id).cloned() {
                        return self.check_operand(&value, required, op);
                    }
                }
                _ => {}
            }
        }
        if self.static_type(operand) == Some(required) || self.defers_to_evaluation(operand) {
            return true;
        }

        self.errors.push(Error::new(
            ErrorKind::OperandTypeMismatch {
                rule: spec::ids::OPERAND_TYPES,
                op,
                expected: type_name(required),
                found: self.found_type(operand),
            },
            operand.range,
        ));
        false
    }

    // a value fed by external context has no static type by design, its type
    // checks move to evaluation instead of failing validation
    fn defers_to_evaluation(&self, folded: &Folded) -> bool {
        self.static_type(folded).is_none() && self.folded_reaches_context(folded)
    }

    fn folded_reaches_context(&self, folded: &Folded) -> bool {
        match &folded.kind {
            FoldedKind::Value(value) => self.value_reaches_context(value),
            FoldedKind::Array(values) => values.iter().any(|value| self.folded_reaches_context(value)),
            FoldedKind::Object(fields) => fields.iter().any(|field| self.folded_reaches_context(&field.value)),
        }
    }

    fn value_reaches_context(&self, value: &Value) -> bool {
        let any = |values: &[Value]| values.iter().any(|value| self.value_reaches_context(value));
        match value {
            Value::GraphRef { .. } | Value::FieldRef(_) => false,
            Value::Input { .. } => true,
            Value::Context { .. } => true,
            Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Null => false,
            Value::Template(template) => template.parts.iter().any(|part| match part {
                Part::Dynamic(value) => self.value_reaches_context(value),
                Part::Lit(_) | Part::BindingRef(_) => false,
            }),
            Value::Array(array) => array.elem.iter().any(|elem| match elem {
                ArrayElem::Item(value) | ArrayElem::Spread(value) => self.value_reaches_context(value),
            }),
            Value::Object(object) => object.elem.iter().any(|field| self.value_reaches_context(&field.value)),
            // a binding reaches through its definition
            Value::BindingRef(name, _) => self.var_value(name).is_some_and(|def| self.folded_reaches_context(def)),
            Value::Unary { operand, .. } => self.value_reaches_context(operand),
            Value::Binary { lhs, rhs, .. } => self.value_reaches_context(lhs) || self.value_reaches_context(rhs),
            Value::Cond { cond, then, otherwise } => {
                self.value_reaches_context(cond) || self.value_reaches_context(then) || self.value_reaches_context(otherwise)
            }
            Value::Index { target, index, .. } => self.value_reaches_context(target) || self.value_reaches_context(index),
            Value::Slice { target, start, end, .. } => {
                self.value_reaches_context(target)
                    || start.as_deref().is_some_and(|bound| self.value_reaches_context(bound))
                    || end.as_deref().is_some_and(|bound| self.value_reaches_context(bound))
            }
            Value::Func { func, .. } => match func {
                Func::Choice(options) | Func::Min(options) | Func::Max(options) => any(options),
                Func::Weighted(options) => options.iter().any(|option| self.value_reaches_context(&option.value)),
                Func::Range(_)
                | Func::Normal { .. }
                | Func::Lognormal { .. }
                | Func::Exponential { .. }
                | Func::Pareto { .. }
                | Func::Beta { .. }
                | Func::Poisson { .. }
                | Func::Chance { .. }
                | Func::Uuid
                | Func::Hex { .. }
                | Func::Alphanum { .. } => false,
                Func::Upper { text } | Func::Lower { text } | Func::Trim { text } => self.value_reaches_context(text),
                Func::Replace { text, from, to } => {
                    self.value_reaches_context(text) || self.value_reaches_context(from) || self.value_reaches_context(to)
                }
                Func::Split { text, separator } => self.value_reaches_context(text) || self.value_reaches_context(separator),
                Func::Join { array, separator }
                | Func::Compose { array, separator }
                | Func::Render {
                    template: array,
                    values: separator,
                }
                | Func::Sample { array, count: separator } => {
                    self.value_reaches_context(array) || self.value_reaches_context(separator)
                }
                Func::Contains { target, needle } => self.value_reaches_context(target) || self.value_reaches_context(needle),
                Func::StartsWith { text, prefix } => self.value_reaches_context(text) || self.value_reaches_context(prefix),
                Func::EndsWith { text, suffix } => self.value_reaches_context(text) || self.value_reaches_context(suffix),
                Func::Len { target } => self.value_reaches_context(target),
                Func::Format { args, .. } | Func::Call { args, .. } => any(args),
                Func::Clamp { value, min, max } => {
                    self.value_reaches_context(value) || self.value_reaches_context(min) || self.value_reaches_context(max)
                }
                Func::Round { value } | Func::Floor { value } | Func::Ceil { value } | Func::Abs { value } => {
                    self.value_reaches_context(value)
                }
            },
        }
    }

    fn check_equality_operands(&mut self, lhs: &Folded, rhs: &Folded, op: ast::BinOp) -> bool {
        const SCALARS: &str = "string, number, or boolean";
        fn scalar(found: Option<StaticType>) -> bool {
            matches!(found, Some(StaticType::String | StaticType::Number | StaticType::Boolean))
        }

        let lhs_type = self.static_type(lhs);
        let rhs_type = self.static_type(rhs);
        let mut valid = true;
        // reference-fed sides settle their types at evaluation
        let deferred = self.defers_to_evaluation(lhs) || self.defers_to_evaluation(rhs);

        for (side, side_type) in [(lhs, lhs_type), (rhs, rhs_type)] {
            // a null literal compares against a possibly-absent reference
            let null_probe = side_type == Some(StaticType::Null) && deferred;
            if !scalar(side_type) && !null_probe && !self.defers_to_evaluation(side) {
                self.errors.push(Error::new(
                    ErrorKind::OperandTypeMismatch {
                        rule: spec::ids::OPERAND_TYPES,
                        op: op.to_string(),
                        expected: SCALARS,
                        found: self.found_type(side),
                    },
                    side.range,
                ));
                valid = false;
            }
        }

        // the left operand fixes the comparison type
        if valid && !deferred && lhs_type != rhs_type {
            self.errors.push(Error::new(
                ErrorKind::OperandTypeMismatch {
                    rule: spec::ids::OPERAND_TYPES,
                    op: op.to_string(),
                    expected: type_name(lhs_type.expect("scalar operands have a static type")),
                    found: self.found_type(rhs),
                },
                rhs.range,
            ));
            valid = false;
        }

        valid
    }

    fn eval_binary(&mut self, op: ast::BinOp, lhs: Const, rhs: Const, range: SrcRange) -> Option<Folded> {
        let value = match op_class(op) {
            OpClass::Arith => {
                let (Const::Num(lhs), Const::Num(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as numbers");
                };
                match eval_arith(op, lhs, rhs) {
                    Some(number) => Value::Num(number),
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
                Value::Bool(eval_cmp(op, lhs, rhs))
            }
            OpClass::Eq => {
                let equal = match (lhs, rhs) {
                    (Const::Str(lhs), Const::Str(rhs)) => lhs == rhs,
                    (Const::Bool(lhs), Const::Bool(rhs)) => lhs == rhs,
                    (Const::Num(Number::Int(lhs)), Const::Num(Number::Int(rhs))) => lhs == rhs,
                    (Const::Num(lhs), Const::Num(rhs)) => float_bound(lhs) == float_bound(rhs),
                    _ => unreachable!("operands were type checked as matching scalars"),
                };
                Value::Bool(if op == ast::BinOp::Eq { equal } else { !equal })
            }
            OpClass::Logic => {
                let (Const::Bool(lhs), Const::Bool(rhs)) = (lhs, rhs) else {
                    unreachable!("operands were type checked as booleans");
                };
                Value::Bool(match op {
                    ast::BinOp::And => lhs && rhs,
                    ast::BinOp::Or => lhs || rhs,
                    _ => unreachable!("operator is logical"),
                })
            }
        };

        Some(Folded::value(value, range))
    }

    // none = unknown, out of scope, or an invalid definition, all diagnosed
    fn model_func(&mut self, name: String, args: Vec<Folded>, range: SrcRange) -> Option<Func> {
        // a name defined twice has no single meaning, whoever defines it
        let provided = self.functions.iter().filter(|desc| desc.doc.name == name).count();
        if provided + usize::from(spec::SPEC.function(&name).is_some()) > 1 {
            self.errors.push(Error::new(ErrorKind::AmbiguousFunction { name }, range));
            return None;
        }
        // the descriptor's name doubles as the &'static str for diagnostics
        let Some(func) = spec::SPEC.function(&name).map(|desc| desc.name) else {
            if let Some(desc) = self.functions.iter().find(|desc| desc.doc.name == name) {
                return self.model_call(*desc, args, range);
            }
            let known =
                (spec::SPEC.functions.iter().map(|desc| desc.name)).chain(self.functions.iter().map(|desc| desc.doc.name));
            let nearest = crate::modeling::suggestion(&name, known);
            self.errors.push(Error::new(
                ErrorKind::UnknownFunction {
                    rule: spec::ids::KNOWN_FUNCTIONS,
                    name,
                    nearest,
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

                Some(Func::Choice(args.into_iter().map(Folded::into_value).collect()))
            }
            "range" => {
                let Ok([min, max]) = <[Folded; 2]>::try_from(args) else {
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
                if const_string(&separator).is_some_and(|separator: String| separator.is_empty()) {
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
            "render" => {
                let [template, values] = self.func_args(func, args, "exactly two arguments (template, values)", range)?;
                let template = self.model_typed_arg(template, func, StaticType::String);
                let values = self.model_typed_arg(values, func, StaticType::Object);
                Some(Func::Render {
                    template: Box::new(template?),
                    values: Box::new(values?),
                })
            }
            "compose" => {
                let mut args = args;
                if args.len() == 1 {
                    args.push(Folded::value(Value::Str(" ".into()), range));
                }
                let [array, separator] = self.func_args(func, args, "one or two arguments (parts, separator)", range)?;
                let array = self.model_typed_arg(array, func, StaticType::Array);
                let separator = self.model_typed_arg(separator, func, StaticType::String);
                Some(Func::Compose {
                    array: Box::new(array?),
                    separator: Box::new(separator?),
                })
            }
            "sample" => {
                let [array, count] = self.func_args(func, args, "exactly two arguments (array, count)", range)?;
                let array = self.model_typed_arg(array, func, StaticType::Array);
                let count = self.model_typed_arg(count, func, StaticType::Number);
                Some(Func::Sample {
                    array: Box::new(array?),
                    count: Box::new(count?),
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
                match self.static_type(&target) {
                    Some(StaticType::String) => {
                        let needle = self.model_typed_arg(needle, func, StaticType::String);
                        Some(Func::Contains {
                            target: Box::new(target.into_value()),
                            needle: Box::new(needle?),
                        })
                    }
                    Some(StaticType::Array) => {
                        let needle = self.model_scalar_arg(needle, func);
                        Some(Func::Contains {
                            target: Box::new(target.into_value()),
                            needle: Box::new(needle?),
                        })
                    }
                    // a reference-fed target settles string-vs-array when it evaluates
                    _ if self.defers_to_evaluation(&target) => {
                        let needle = self.model_scalar_arg(needle, func);
                        Some(Func::Contains {
                            target: Box::new(target.into_value()),
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
                let known = matches!(self.static_type(&target), Some(StaticType::String | StaticType::Array));
                if !known && !self.defers_to_evaluation(&target) {
                    self.push_func_arg_type(&target, func, "string or array");
                    return None;
                }
                Some(Func::Len {
                    target: Box::new(target.into_value()),
                })
            }
            "format" => self.model_format(args, range),

            "clamp" => {
                let [value, min, max] = self.func_args(func, args, "exactly three arguments (value, min, max)", range)?;
                // constant bounds must already be ordered
                if let (FoldedKind::Value(Value::Num(low)), FoldedKind::Value(Value::Num(high))) = (&min.kind, &max.kind)
                    && float_bound(low.clone()) > float_bound(high.clone())
                {
                    self.errors.push(Error::new(
                        ErrorKind::ClampBoundsOutOfOrder {
                            rule: spec::ids::CLAMP_BOUNDS,
                        },
                        range,
                    ));
                    return None;
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

    // a provider function: the descriptor says what to check, and the call is never folded
    fn model_call(&mut self, desc: FunctionDesc, args: Vec<Folded>, range: SrcRange) -> Option<Func> {
        let func = desc.doc.name;
        // optional parameters come last, so a call supplies a prefix of the list
        let required = desc.params.iter().filter(|param| !param.optional).count();
        let most = desc.params.len();
        if !(required..=most).contains(&args.len()) {
            let word = |count: usize| match count {
                0 => "no".to_owned(),
                1 => "one".to_owned(),
                2 => "two".to_owned(),
                3 => "three".to_owned(),
                count => count.to_string(),
            };
            let count = match (required, most) {
                (0, 0) => "no arguments".to_owned(),
                (1, 1) => "exactly one argument".to_owned(),
                (required, most) if required == most => format!("exactly {} arguments", word(most)),
                (required, most) if required + 1 == most => format!("{} or {} arguments", word(required), word(most)),
                (required, most) => format!("between {} and {} arguments", word(required), word(most)),
            };
            let names: Vec<&str> = desc.params.iter().map(|param| param.name).collect();
            let expected = match names.as_slice() {
                [] => count,
                names => format!("{count} ({})", names.join(", ")),
            };
            self.errors.push(Error::new(ErrorKind::CallArity { func, expected }, range));
            return None;
        }
        // a rule beyond the two every function shares is the provider's own account of its parameters
        let shared = [spec::ids::FUNC_ARITY, spec::ids::FUNC_ARG_TYPES];
        let rule = (desc.doc.rules.iter())
            .find(|rule| !shared.contains(&rule.id))
            .map(|rule| rule.summary);
        let mut valid = true;
        let mut values = Vec::with_capacity(args.len());
        for (arg, param) in args.into_iter().zip(desc.params) {
            let bounded = match (&arg.kind, param.bounds) {
                (FoldedKind::Value(Value::Num(number)), Some((low, high))) => {
                    matches!(number, Number::Int(value) if (low..=high).contains(value))
                }
                _ => true,
            };
            // an argument with alternatives is accepted when each of them is
            let mut allowed: Vec<StaticType> = param.accepts.iter().filter_map(|ty| schema_static_type(*ty)).collect();
            if param.nullable {
                allowed.push(StaticType::Null);
            }
            let typed = param.accepts.contains(&FieldType::Any) || self.matches_static_types(&arg, &allowed);
            if param.constant && !is_constant(&arg) {
                self.errors.push(Error::new(
                    ErrorKind::CallParam {
                        func,
                        param: param.name,
                        expected: format!("a constant {}", accepted_types(param)),
                        rule,
                    },
                    arg.range,
                ));
                valid = false;
            } else if !typed && !self.defers_to_evaluation(&arg) {
                self.errors.push(Error::new(
                    ErrorKind::CallArgType {
                        func,
                        expected: accepted_types(param),
                        found: self.found_type(&arg),
                    },
                    arg.range,
                ));
                valid = false;
            } else if !bounded {
                let (low, high) = param.bounds.unwrap_or_default();
                self.errors.push(Error::new(
                    ErrorKind::CallParam {
                        func,
                        param: param.name,
                        expected: format!("an integer between {low} and {high}"),
                        rule,
                    },
                    arg.range,
                ));
                valid = false;
            }
            values.push(arg.into_value());
        }
        valid.then(|| Func::Call {
            function: func.to_owned(),
            args: values,
            returns: desc.returns,
            shape: desc.shape,
        })
    }

    fn model_weighted(&mut self, args: Vec<Folded>, range: SrcRange) -> Option<Func> {
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
            let FoldedKind::Array(mut elems) = arg.kind else {
                self.push_weighted_option(arg_range);
                valid = false;
                continue;
            };
            if elems.len() != 2 {
                self.push_weighted_option(arg_range);
                valid = false;
                continue;
            }
            let weight_value = elems.pop().expect("pair has two elements");
            let value = elems.pop().expect("pair has two elements");

            let weight_range = weight_value.range;
            let weight = match weight_value.kind {
                FoldedKind::Value(Value::Num(number)) => Some(float_bound(number)),
                _ => None,
            };
            let weight = match weight {
                Some(weight) if weight >= 0.0 => Some(weight),
                _ => {
                    self.push_weighted_option(weight_range);
                    None
                }
            };

            match weight {
                Some(weight) => options.push(WeightedOption {
                    value: value.into_value(),
                    weight,
                }),
                None => valid = false,
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

    fn model_format(&mut self, args: Vec<Folded>, range: SrcRange) -> Option<Func> {
        let mut args = args.into_iter();
        let Some(template_value) = args.next() else {
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

        let Some(template) = const_string(&template_value) else {
            self.push_func_arg_type(&template_value, "format", "constant string");
            return None;
        };

        let rest: Vec<_> = args.collect();
        let pieces: Vec<String> = template.split("{}").map(str::to_owned).collect();
        let placeholders = pieces.len() - 1;
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

        valid.then_some(Func::Format { pieces, args: values })
    }

    fn matches_static_types(&self, folded: &Folded, allowed: &[StaticType]) -> bool {
        match self.static_type(folded) {
            Some(kind) => allowed.contains(&kind),
            None => match &folded.kind {
                FoldedKind::Value(value) => self.value_matches_static_types(value, allowed),
                _ => false,
            },
        }
    }

    fn value_matches_static_types(&self, value: &Value, allowed: &[StaticType]) -> bool {
        match value {
            Value::BindingRef(name, _) => self
                .var_value(name)
                .is_some_and(|value| self.matches_static_types(value, allowed)),
            Value::Func {
                func: Func::Choice(options),
                ..
            } => options.iter().all(|value| self.value_matches_static_types(value, allowed)),
            Value::Func {
                func: Func::Weighted(options),
                ..
            } => options
                .iter()
                .all(|option| self.value_matches_static_types(&option.value, allowed)),
            Value::Cond { then, otherwise, .. } => {
                self.value_matches_static_types(then, allowed) && self.value_matches_static_types(otherwise, allowed)
            }
            _ => self.value_type(value).is_some_and(|kind| allowed.contains(&kind)),
        }
    }

    fn func_args<const N: usize>(
        &mut self,
        func: &'static str,
        args: Vec<Folded>,
        expected: &'static str,
        range: SrcRange,
    ) -> Option<[Folded; N]> {
        match <[Folded; N]>::try_from(args) {
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

    fn model_typed_arg(&mut self, arg: Folded, func: &'static str, required: StaticType) -> Option<Value> {
        if self.static_type(&arg) != Some(required) && !self.defers_to_evaluation(&arg) {
            self.push_func_arg_type(&arg, func, type_name(required));
            return None;
        }
        Some(arg.into_value())
    }

    fn model_scalar_arg(&mut self, arg: Folded, func: &'static str) -> Option<Value> {
        let scalar = matches!(
            self.static_type(&arg),
            Some(StaticType::String | StaticType::Number | StaticType::Boolean)
        );
        if !scalar && !self.defers_to_evaluation(&arg) {
            self.push_func_arg_type(&arg, func, "string, number, or boolean");
            return None;
        }
        Some(arg.into_value())
    }

    fn push_func_arg_type(&mut self, arg: &Folded, func: &'static str, expected: &'static str) {
        self.errors.push(Error::new(
            ErrorKind::FuncArgType {
                rule: spec::ids::FUNC_ARG_TYPES,
                func,
                expected,
                found: self.found_type(arg),
            },
            arg.range,
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

    // constant distribution parameter, already folded to a value when constant
    fn model_dist_param(
        &mut self,
        arg: Folded,
        func: &'static str,
        param: &'static str,
        expected: &'static str,
        valid: impl Fn(f64) -> bool,
    ) -> Option<f64> {
        let range = arg.range;
        let number = match arg.kind {
            FoldedKind::Value(Value::Num(number)) => number,
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

    fn model_random_length(&mut self, arg: Folded, func: &'static str) -> Option<usize> {
        let range = arg.range;
        let number = match arg.kind {
            FoldedKind::Value(Value::Num(number)) => number,
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

        let length = match number {
            Number::Int(value) => usize::try_from(value).ok(),
            Number::Float(_) => None,
        };
        match length {
            Some(length) => Some(length),
            None => {
                self.errors.push(Error::new(
                    ErrorKind::ParamOutOfRange {
                        rule: spec::ids::RANDOM_LENGTH,
                        func,
                        param: "length",
                        expected: "a non-negative integer that fits this platform's length limit",
                    },
                    range,
                ));
                None
            }
        }
    }

    fn model_range_bound(&mut self, bound: Folded) -> Option<Number> {
        match bound.kind {
            FoldedKind::Value(Value::Num(number)) => Some(number),
            _ => {
                self.errors.push(Error::new(
                    ErrorKind::InvalidRangeArgs {
                        rule: spec::ids::RANGE_BOUNDS,
                    },
                    bound.range,
                ));
                None
            }
        }
    }

    // resolves a context path or emits the diagnostic shared by both syntaxes
    fn model_number(&mut self, raw: String, negated: bool, range: SrcRange) -> Option<Number> {
        let raw = if negated { format!("-{raw}") } else { raw };
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

    // statically known result type, none = unknown (eg heterogeneous choice)
    fn static_type(&self, folded: &Folded) -> Option<StaticType> {
        match &folded.kind {
            FoldedKind::Array(_) => Some(StaticType::Array),
            FoldedKind::Object(_) => Some(StaticType::Object),
            FoldedKind::Value(value) => self.value_type(value),
        }
    }

    fn value_type(&self, value: &Value) -> Option<StaticType> {
        if let Some(kind) = self.result_shape(value).and_then(shape_type) {
            return Some(kind);
        }
        match value {
            Value::Str(_) | Value::Template(_) => Some(StaticType::String),
            Value::Num(_) => Some(StaticType::Number),
            Value::Bool(_) => Some(StaticType::Boolean),
            Value::Null => Some(StaticType::Null),
            Value::Array(_) => Some(StaticType::Array),
            Value::Object(_) => Some(StaticType::Object),
            // a binding is typed by its definition
            Value::BindingRef(name, _) => self
                .types
                .get(name)
                .and_then(|ty| schema_static_type(*ty))
                .or_else(|| self.vars.get(name).and_then(|value| self.static_type(value))),
            // a referenced field's shape is only known at evaluation
            Value::GraphRef { .. } => Some(StaticType::Graph),
            Value::Context { .. } => None,
            Value::FieldRef(_) => Some(StaticType::Graph),
            Value::Input { ty, .. } => match ty {
                FieldType::Any => None,
                FieldType::String => Some(StaticType::String),
                FieldType::Number => Some(StaticType::Number),
                FieldType::Boolean => Some(StaticType::Boolean),
                FieldType::Array => Some(StaticType::Array),
                FieldType::Object => Some(StaticType::Object),
            },
            Value::Func { func, .. } => self.func_type(func),
            Value::Unary { op: UnaryOp::Neg, .. } => Some(StaticType::Number),
            Value::Unary { op: UnaryOp::Not, .. } => Some(StaticType::Boolean),
            Value::Binary { op, .. } => Some(match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => StaticType::Number,
                _ => StaticType::Boolean,
            }),
            Value::Cond { then, otherwise, .. } => {
                let then = self.value_type(then)?;
                let otherwise = self.value_type(otherwise)?;
                (then == otherwise).then_some(then)
            }
            // a dynamic index is typed when the target's elements agree
            Value::Index { target, index, .. } => match &**target {
                Value::Array(array) => unify_types(array.elem.iter().map(|elem| match elem {
                    ArrayElem::Item(value) => self.value_type(value),
                    // a spread's elements are unknown until evaluation
                    ArrayElem::Spread(_) => None,
                })),
                Value::Object(object) => unify_types(object.elem.iter().map(|field| self.value_type(&field.value))),
                Value::BindingRef(name, _) => match self.var_value(name).map(|value| &value.kind) {
                    Some(FoldedKind::Array(values)) => match &**index {
                        Value::Num(Number::Int(position)) => usize::try_from(*position)
                            .ok()
                            .and_then(|position| values.get(position))
                            .and_then(|value| self.static_type(value)),
                        _ => unify_types(values.iter().map(|value| self.static_type(value))),
                    },
                    Some(FoldedKind::Object(fields)) => match &**index {
                        Value::Str(key) => fields
                            .iter()
                            .find(|field| &field.key == key)
                            .and_then(|field| self.static_type(&field.value)),
                        _ => unify_types(fields.iter().map(|field| self.static_type(&field.value))),
                    },
                    _ => None,
                },
                _ => None,
            },
            // a residual slice always selects from an array
            Value::Slice { .. } => Some(StaticType::Array),
        }
    }

    // keep known result fields typed through bindings and nested indexes
    fn result_shape(&self, value: &Value) -> Option<ResultShape> {
        match value {
            Value::BindingRef(name, _) => match &self.var_value(name)?.kind {
                FoldedKind::Value(value) => self.result_shape(value),
                _ => None,
            },
            // a provider says what its function returns
            Value::Func {
                func: Func::Call { shape, .. },
                ..
            } => *shape,
            Value::Index { target, index, .. } => match self.result_shape(target)? {
                ResultShape::Object(fields) => {
                    let Value::Str(key) = &**index else { return None };
                    fields.iter().find_map(|(name, shape)| (*name == key).then_some(*shape))
                }
                ResultShape::Array(element) => Some(*element),
                ResultShape::Scalar(_) => None,
            },
            Value::Slice { target, .. } => {
                let shape = self.result_shape(target)?;
                matches!(shape, ResultShape::Array(_)).then_some(shape)
            }
            Value::Cond { then, otherwise, .. } => {
                let shape = self.result_shape(then)?;
                (Some(shape) == self.result_shape(otherwise)).then_some(shape)
            }
            Value::Func {
                func: Func::Choice(values),
                ..
            } => {
                let shape = self.result_shape(values.first()?)?;
                values
                    .iter()
                    .all(|value| self.result_shape(value) == Some(shape))
                    .then_some(shape)
            }
            _ => None,
        }
    }

    // result type by function, a pick is typed when its alternatives agree
    fn func_type(&self, func: &Func) -> Option<StaticType> {
        match func {
            Func::Choice(options) => unify_types(options.iter().map(|option| self.value_type(option))),
            Func::Weighted(options) => unify_types(options.iter().map(|option| self.value_type(&option.value))),
            Func::Range(_)
            | Func::Normal { .. }
            | Func::Lognormal { .. }
            | Func::Exponential { .. }
            | Func::Pareto { .. }
            | Func::Beta { .. }
            | Func::Poisson { .. }
            | Func::Len { .. }
            | Func::Clamp { .. }
            | Func::Round { .. }
            | Func::Floor { .. }
            | Func::Ceil { .. }
            | Func::Abs { .. }
            | Func::Min(_)
            | Func::Max(_) => Some(StaticType::Number),
            Func::Chance { .. } | Func::Contains { .. } | Func::StartsWith { .. } | Func::EndsWith { .. } => {
                Some(StaticType::Boolean)
            }
            Func::Upper { .. }
            | Func::Lower { .. }
            | Func::Trim { .. }
            | Func::Replace { .. }
            | Func::Join { .. }
            | Func::Compose { .. }
            | Func::Render { .. }
            | Func::Format { .. }
            | Func::Uuid
            | Func::Hex { .. }
            | Func::Alphanum { .. } => Some(StaticType::String),
            Func::Split { .. } | Func::Sample { .. } => Some(StaticType::Array),
            Func::Call { returns, .. } => schema_static_type(*returns),
        }
    }

    // the found side of diagnostics, prefers the static type over the value shape
    fn found_type(&self, folded: &Folded) -> ExprType {
        match self.static_type(folded) {
            Some(StaticType::Graph) => ExprType::Graph,
            Some(StaticType::String) => ExprType::String,
            Some(StaticType::Number) => ExprType::Number,
            Some(StaticType::Boolean) => ExprType::Boolean,
            Some(StaticType::Null) => ExprType::Null,
            Some(StaticType::Array) => ExprType::Array,
            Some(StaticType::Object) => ExprType::Object,
            None => ExprType::of(folded),
        }
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
    Graph,
    String,
    Number,
    Boolean,
    Null,
    Array,
    Object,
}

// the type of a value with a known shape, none when the shape leaves it to evaluation
fn shape_type(shape: ResultShape) -> Option<StaticType> {
    match shape {
        ResultShape::Scalar(ty) => schema_static_type(ty),
        ResultShape::Object(_) => Some(StaticType::Object),
        ResultShape::Array(_) => Some(StaticType::Array),
    }
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

// residual dynamic exprs cross into the model here, off the ast op enums
fn model_unary_op(op: ast::UnaryOp) -> UnaryOp {
    match op {
        ast::UnaryOp::Neg => UnaryOp::Neg,
        ast::UnaryOp::Not => UnaryOp::Not,
    }
}

fn model_bin_op(op: ast::BinOp) -> BinOp {
    match op {
        ast::BinOp::Add => BinOp::Add,
        ast::BinOp::Sub => BinOp::Sub,
        ast::BinOp::Mul => BinOp::Mul,
        ast::BinOp::Div => BinOp::Div,
        ast::BinOp::Rem => BinOp::Rem,
        ast::BinOp::Eq => BinOp::Eq,
        ast::BinOp::Ne => BinOp::Ne,
        ast::BinOp::Lt => BinOp::Lt,
        ast::BinOp::Le => BinOp::Le,
        ast::BinOp::Gt => BinOp::Gt,
        ast::BinOp::Ge => BinOp::Ge,
        ast::BinOp::And => BinOp::And,
        ast::BinOp::Or => BinOp::Or,
    }
}

// none when empty or the types disagree
fn unify_types(types: impl Iterator<Item = Option<StaticType>>) -> Option<StaticType> {
    let mut unified = None;
    for found in types {
        let found = found?;
        match unified {
            None => unified = Some(found),
            Some(previous) if previous == found => {}
            Some(_) => return None,
        }
    }
    unified
}

fn type_name(required: StaticType) -> &'static str {
    match required {
        StaticType::Graph => "graph element",
        StaticType::String => "string",
        StaticType::Number => "number",
        StaticType::Boolean => "boolean",
        StaticType::Null => "null",
        StaticType::Array => "array",
        StaticType::Object => "object",
    }
}

// the types a provider parameter accepts, as a diagnostic reads them
fn accepted_types(param: &crate::ParamDesc) -> String {
    let names: Vec<&str> = (param.accepts.iter())
        .map(|ty| schema_static_type(*ty).map_or("value", type_name))
        .chain(param.nullable.then_some("null"))
        .collect();
    match names.as_slice() {
        [] => "value".to_owned(),
        [only] => (*only).to_owned(),
        [first, second] => format!("{first} or {second}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

fn is_constant(folded: &Folded) -> bool {
    match &folded.kind {
        FoldedKind::Value(value) => value_is_constant(value),
        FoldedKind::Array(values) => values.iter().all(is_constant),
        FoldedKind::Object(fields) => fields.iter().all(|field| is_constant(&field.value)),
    }
}

fn value_is_constant(value: &Value) -> bool {
    match value {
        Value::Str(_) | Value::Num(_) | Value::Bool(_) | Value::Null => true,
        Value::Array(array) => array.elem.iter().all(|elem| match elem {
            ArrayElem::Item(value) => value_is_constant(value),
            // a surviving spread only exists because its shape is dynamic
            ArrayElem::Spread(_) => false,
        }),
        Value::Object(object) => object.elem.iter().all(|field| value_is_constant(&field.value)),
        Value::Template(_)
        | Value::Func { .. }
        | Value::BindingRef(_, _)
        | Value::GraphRef { .. }
        | Value::Input { .. }
        | Value::FieldRef(_)
        | Value::Context { .. } => false,
        Value::Unary { .. } | Value::Binary { .. } | Value::Cond { .. } | Value::Index { .. } | Value::Slice { .. } => false,
    }
}

fn const_scalar(folded: &Folded) -> Const {
    match &folded.kind {
        FoldedKind::Value(Value::Str(value)) => Const::Str(value.clone()),
        FoldedKind::Value(Value::Num(number)) => Const::Num(number.clone()),
        FoldedKind::Value(Value::Bool(value)) => Const::Bool(*value),
        _ => unreachable!("constant operands are scalar literals"),
    }
}

// constant string value of a folded expr, none when dynamic or another type
fn const_string(folded: &Folded) -> Option<String> {
    match &folded.kind {
        FoldedKind::Value(Value::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

// interpolated numbers render like evaluation-time templates, {:?} keeps a .0
fn num_text(number: Number) -> String {
    match number {
        Number::Int(value) => value.to_string(),
        Number::Float(value) => format!("{value:?}"),
    }
}

// metadata and metrics were validated as objects, duplicate keys already merged
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

#[derive(Debug, Clone)]
struct Error {
    kind: ErrorKind,
    range: SrcRange,
}
type Errors = Vec<Error>;
#[derive(Debug, Clone)]
enum ErrorKind {
    DuplicateObjectKey {
        rule: spec::Id,
        key: String,
    },
    InvalidNumber {
        rule: spec::Id,
        raw: String,
    },
    NonScalarInterpolation {
        rule: spec::Id,
        found: ExprType,
    },
    ForGraphRefCollection {
        rule: spec::Id,
    },
    UnknownFunction {
        rule: spec::Id,
        name: String,
        nearest: String,
    },
    AmbiguousFunction {
        name: String,
    },
    CallArity {
        func: &'static str,
        expected: String,
    },
    CallArgType {
        func: &'static str,
        expected: String,
        found: ExprType,
    },
    CallParam {
        func: &'static str,
        param: &'static str,
        expected: String,
        // the rule the provider documents for the function, when it has one of its own
        rule: Option<&'static str>,
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
}
impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateObjectKey { rule, key } => {
                let rule = rule_desc(*rule);
                write!(formatter, "duplicate object key `{key}`; {}", rule.summary)
            }
            Self::InvalidNumber { rule, raw } => {
                let rule = rule_desc(*rule);
                write!(formatter, "invalid number `{raw}`; {}", rule.summary)
            }
            Self::NonScalarInterpolation { rule, found } => {
                let rule = rule_desc(*rule);
                write!(formatter, "cannot interpolate {found} here; {}", rule.summary)
            }
            Self::ForGraphRefCollection { rule } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "a `for` collection cannot come from a graph reference; {}",
                    rule.summary
                )
            }
            Self::UnknownFunction { rule, name, nearest } => {
                let rule = rule_desc(*rule);
                write!(formatter, "unknown function `{name}`{nearest}; {}", rule.summary)
            }
            Self::AmbiguousFunction { name } => {
                let rule = rule_desc(spec::ids::PROVIDER_FUNCTIONS);
                write!(formatter, "function `{name}` is defined more than once; {}", rule.summary)
            }
            Self::CallArity { func, expected } => {
                let rule = rule_desc(spec::ids::FUNC_ARITY);
                write!(formatter, "function `{func}` expects {expected}; {}", rule.summary)
            }
            Self::CallArgType { func, expected, found } => {
                let rule = rule_desc(spec::ids::FUNC_ARG_TYPES);
                write!(
                    formatter,
                    "function `{func}` expects a {expected} argument, but found {found}; {}",
                    rule.summary
                )
            }
            Self::CallParam {
                func,
                param,
                expected,
                rule,
            } => {
                write!(formatter, "function `{func}` {param} must be {expected}")?;
                match rule {
                    Some(rule) => write!(formatter, "; {rule}"),
                    None => Ok(()),
                }
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
                    "for expression expects an array or object collection with a shape known before evaluation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantForFilter { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for filter expects a boolean known before evaluation, but found {found}; {}",
                    rule.summary
                )
            }
            Self::NonConstantForKey { rule, found } => {
                let rule = rule_desc(*rule);
                write!(
                    formatter,
                    "for key expects a string known before evaluation, but found {found}; {}",
                    rule.summary
                )
            }
        }
    }
}
impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
}

fn reference_head(expr: &ast::Expr) -> Option<&str> {
    match &expr.kind {
        ast::ExprKind::Ref { path } => path.first().map(String::as_str),
        ast::ExprKind::GraphRef { .. } | ast::ExprKind::GraphMember { .. } | ast::ExprKind::PatternRef(_) => Some(""),
        ast::ExprKind::Index { target, .. } | ast::ExprKind::Slice { target, .. } => reference_head(target),
        _ => None,
    }
}
fn folded_value(value: Value, range: SrcRange) -> Folded {
    match value {
        Value::Array(array) if array.elem.iter().all(|v| matches!(v, ArrayElem::Item(_))) => Folded::new(
            FoldedKind::Array(
                array
                    .elem
                    .into_iter()
                    .map(|v| match v {
                        ArrayElem::Item(v) => folded_value(v, range),
                        _ => unreachable!(),
                    })
                    .collect(),
            ),
            range,
        ),
        Value::Object(object) => Folded::new(
            FoldedKind::Object(
                object
                    .elem
                    .into_iter()
                    .map(|f| FoldedField {
                        key: f.key,
                        value: folded_value(f.value, range),
                        range,
                    })
                    .collect(),
            ),
            range,
        ),
        value => Folded::value(value, range),
    }
}
pub(crate) fn lower(expr: ast::Expr, functions: &[FunctionDesc], resolve: &mut Resolver<'_>) -> Result<Value, Diags> {
    let mut modeler = ExpressionModeler {
        scopes: Vec::new(),
        vars: HashMap::new(),
        types: HashMap::new(),
        errors: Vec::new(),
        reference_errors: Vec::new(),
        functions,
        resolve,
    };
    let value = modeler.fold_expr(expr);
    let mut errors: Diags = modeler
        .errors
        .into_iter()
        .map(|error| Diag {
            when: DiagPhase::Validation,
            what: error.kind.to_string(),
            r#where: error.range,
        })
        .collect();
    errors.extend(modeler.reference_errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(value.expect("folding failures produce diagnostics").into_value())
}
pub(crate) fn constant(value: &Value) -> bool {
    value_is_constant(value)
}

// graph handles are fixed identities even when they are not literal constants
pub(crate) fn static_value(value: &Value) -> bool {
    match value {
        Value::GraphRef { .. } => true,
        Value::Array(array) => array.elem.iter().all(|elem| match elem {
            ArrayElem::Item(value) => static_value(value),
            ArrayElem::Spread(_) => false,
        }),
        Value::Object(object) => object.elem.iter().all(|field| static_value(&field.value)),
        _ => value_is_constant(value),
    }
}

fn rule_desc(id: spec::Id) -> &'static spec::RuleDesc {
    spec::SPEC.rule(id).expect("expression rule is registered")
}

fn with_value(
    value: ReferenceValue,
    range: SrcRange,
    operation: impl FnOnce(&mut ExpressionModeler<'_>, Folded) -> Option<Folded>,
) -> Result<ReferenceValue, Diags> {
    let mut resolve = |expr: ast::Expr| {
        Err(vec![Diag {
            when: DiagPhase::Validation,
            what: "unexpected unresolved reference".to_owned(),
            r#where: expr.range,
        }])
    };
    let mut modeler = ExpressionModeler {
        scopes: Vec::new(),
        vars: HashMap::new(),
        types: HashMap::new(),
        errors: Vec::new(),
        reference_errors: Vec::new(),
        functions: &[],
        resolve: &mut resolve,
    };
    let bindings = value.bindings.clone();
    for (id, shape, ty) in &bindings {
        modeler.types.insert(*id, *ty);
        modeler.vars.insert(*id, folded_value(shape.clone(), range));
    }
    let result = operation(&mut modeler, folded_value(value.value, range));
    if !modeler.errors.is_empty() {
        return Err(modeler
            .errors
            .into_iter()
            .map(|e| Diag {
                when: DiagPhase::Validation,
                what: e.kind.to_string(),
                r#where: e.range,
            })
            .collect());
    }
    Ok(ReferenceValue {
        value: result.expect("expression checks diagnose failures").into_value(),
        bindings,
    })
}
pub(crate) fn index(value: ReferenceValue, index: Value, range: SrcRange) -> Result<ReferenceValue, Diags> {
    with_value(value, range, |modeler, value| {
        modeler.fold_index(value, folded_value(index, range), range)
    })
}
pub(crate) fn slice(
    value: ReferenceValue,
    start: Option<Value>,
    end: Option<Value>,
    range: SrcRange,
) -> Result<ReferenceValue, Diags> {
    with_value(value, range, |modeler, value| {
        modeler.slice_values(
            value,
            start.map(|v| folded_value(v, range)),
            end.map(|v| folded_value(v, range)),
            range,
        )
    })
}
fn schema_static_type(ty: FieldType) -> Option<StaticType> {
    Some(match ty {
        FieldType::Any => return None,
        FieldType::String => StaticType::String,
        FieldType::Number => StaticType::Number,
        FieldType::Boolean => StaticType::Boolean,
        FieldType::Array => StaticType::Array,
        FieldType::Object => StaticType::Object,
    })
}

pub(crate) fn schema_type(value: ReferenceValue, ty: FieldType, range: SrcRange) -> Result<(), Diags> {
    let Some(expected) = schema_static_type(ty) else {
        return Ok(());
    };
    with_value(value, range, |modeler, value| {
        modeler.check_operand(&value, expected, "schema".to_owned()).then_some(value)
    })
    .map(|_| ())
}

pub(crate) fn boolean(value: ReferenceValue, range: SrcRange) -> Result<(), Diags> {
    with_value(value, range, |modeler, value| {
        modeler
            .check_operand(&value, StaticType::Boolean, "guard".to_owned())
            .then_some(value)
    })
    .map(|_| ())
}
fn collect_template_refs(template: &Template, found: &mut Vec<BindingId>) {
    for part in &template.parts {
        match part {
            Part::Dynamic(value) => collect_refs(value, found),
            Part::BindingRef(id) => found.push(*id),
            Part::Lit(_) => {}
        }
    }
}

fn collect_refs(value: &Value, found: &mut Vec<BindingId>) {
    match value {
        Value::BindingRef(id, _) => found.push(*id),
        Value::Str(_)
        | Value::Num(_)
        | Value::Bool(_)
        | Value::Null
        | Value::GraphRef { .. }
        | Value::Input { .. }
        | Value::FieldRef(_)
        | Value::Context { .. } => {}
        Value::Template(template) => collect_template_refs(template, found),
        Value::Array(array) => {
            for elem in &array.elem {
                match elem {
                    ArrayElem::Item(value) | ArrayElem::Spread(value) => collect_refs(value, found),
                }
            }
        }
        Value::Object(object) => {
            for field in &object.elem {
                collect_refs(&field.value, found);
            }
        }
        Value::Unary { operand, .. } => collect_refs(operand, found),
        Value::Binary { lhs, rhs, .. } => {
            collect_refs(lhs, found);
            collect_refs(rhs, found);
        }
        Value::Cond { cond, then, otherwise } => {
            collect_refs(cond, found);
            collect_refs(then, found);
            collect_refs(otherwise, found);
        }
        Value::Index { target, index, .. } => {
            collect_refs(target, found);
            collect_refs(index, found);
        }
        Value::Slice { target, start, end, .. } => {
            collect_refs(target, found);
            if let Some(start) = start {
                collect_refs(start, found);
            }
            if let Some(end) = end {
                collect_refs(end, found);
            }
        }
        Value::Func { func, .. } => collect_func_refs(func, found),
    }
}

fn collect_func_refs(func: &Func, found: &mut Vec<BindingId>) {
    let mut all = |values: &[Value]| {
        for value in values {
            collect_refs(value, found);
        }
    };
    match func {
        Func::Choice(options) | Func::Min(options) | Func::Max(options) => all(options),
        Func::Weighted(options) => {
            for option in options {
                collect_refs(&option.value, found);
            }
        }
        Func::Range(_)
        | Func::Normal { .. }
        | Func::Lognormal { .. }
        | Func::Exponential { .. }
        | Func::Pareto { .. }
        | Func::Beta { .. }
        | Func::Poisson { .. }
        | Func::Chance { .. }
        | Func::Uuid
        | Func::Hex { .. }
        | Func::Alphanum { .. } => {}
        Func::Upper { text } | Func::Lower { text } | Func::Trim { text } => collect_refs(text, found),
        Func::Replace { text, from, to } => {
            collect_refs(text, found);
            collect_refs(from, found);
            collect_refs(to, found);
        }
        Func::Split { text, separator } => {
            collect_refs(text, found);
            collect_refs(separator, found);
        }
        Func::Join { array, separator }
        | Func::Compose { array, separator }
        | Func::Render {
            template: array,
            values: separator,
        }
        | Func::Sample { array, count: separator } => {
            collect_refs(array, found);
            collect_refs(separator, found);
        }
        Func::Contains { target, needle } => {
            collect_refs(target, found);
            collect_refs(needle, found);
        }
        Func::StartsWith { text, prefix } => {
            collect_refs(text, found);
            collect_refs(prefix, found);
        }
        Func::EndsWith { text, suffix } => {
            collect_refs(text, found);
            collect_refs(suffix, found);
        }
        Func::Len { target } => collect_refs(target, found),
        Func::Format { args, .. } | Func::Call { args, .. } => all(args),
        Func::Clamp { value, min, max } => {
            collect_refs(value, found);
            collect_refs(min, found);
            collect_refs(max, found);
        }
        Func::Round { value } | Func::Floor { value } | Func::Ceil { value } | Func::Abs { value } => {
            collect_refs(value, found)
        }
    }
}

pub(crate) fn dependencies(value: &Value) -> Vec<BindingId> {
    let mut ids = Vec::new();
    collect_refs(value, &mut ids);
    ids
}
