use crate::dsl::ast;
use crate::dsl::diag::{Diag, DiagPhase, Diags, SrcRange};
use crate::dsl::model::{
    Array, CtxRef, Func, Model, Number, Object, ObjectField, Part, Range, Span, SpanFields, SpanKind, Template, Trace, Value,
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
    Expr,
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
            // constant operator and index exprs fold to literals, only dynamic ones get here
            ast::ExprKind::Unary { .. }
            | ast::ExprKind::Binary { .. }
            | ast::ExprKind::Cond { .. }
            | ast::ExprKind::Index { .. } => Self::Expr,
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
            Self::Func => "function",
            Self::Expr => "expression",
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
            ast::ExprKind::Array(values) => {
                let mut folded = Vec::with_capacity(values.len());
                let mut valid = true;
                for value in values {
                    match self.fold_expr(value) {
                        Some(value) => folded.push(value),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Array(folded)
            }
            ast::ExprKind::Object(attrs) => {
                let mut folded = Vec::with_capacity(attrs.len());
                let mut valid = true;
                for attr in attrs {
                    match self.fold_expr(attr.value) {
                        Some(value) => folded.push(ast::Attr { value, ..attr }),
                        None => valid = false,
                    }
                }
                if !valid {
                    return None;
                }
                ast::ExprKind::Object(folded)
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
            ast::ExprKind::Object(attrs) if expr_is_constant(&index) => {
                let Const::Str(key) = self.const_scalar(&index)? else {
                    unreachable!("index was type checked as a string");
                };

                match attrs.into_iter().find(|attr| attr.key == key) {
                    Some(attr) => Some(ast::Expr::new(attr.value.kind, range)),
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
            ast::ExprKind::Func { name, args } => self.model_func(name, args, range).map(Value::Func),
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
            ast::ExprKind::VarRef(_) => unreachable!("variable references are resolved before model lowering"),
        }
    }

    fn model_func(&mut self, name: String, args: Vec<ast::Expr>, range: SrcRange) -> Option<Func> {
        if spec::SPEC.function(&name).is_none() {
            self.errors.push(Error::new(
                ErrorKind::UnknownFunction {
                    rule: spec::ids::KNOWN_FUNCTIONS,
                    name,
                },
                range,
            ));
            return None;
        }

        match name.as_str() {
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
            _ => unreachable!("function {name} does not have a model lowering"),
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
        ast::ExprKind::Func { args, .. } => args.iter().any(expr_references_vars),
        ast::ExprKind::Unary { operand, .. } => expr_references_vars(operand),
        ast::ExprKind::Binary { lhs, rhs, .. } => expr_references_vars(lhs) || expr_references_vars(rhs),
        ast::ExprKind::Cond { cond, then, otherwise } => {
            expr_references_vars(cond) || expr_references_vars(then) || expr_references_vars(otherwise)
        }
        ast::ExprKind::Index { target, index } => expr_references_vars(target) || expr_references_vars(index),
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
            "range" => Some(StaticType::Number),
            // a choice is only typed when every alternative agrees
            "choice" => unify_static_types(args.iter()),
            _ => None,
        },
        // a dynamic index is only typed when the target is a literal with agreeing elements
        ast::ExprKind::Index { target, .. } => match &target.kind {
            ast::ExprKind::Array(values) => unify_static_types(values.iter()),
            ast::ExprKind::Object(attrs) => unify_static_types(attrs.iter().map(|attr| &attr.value)),
            _ => None,
        },
        ast::ExprKind::VarRef(_) => unreachable!("variable references are resolved before type checks"),
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
        ast::ExprKind::Object(attrs) => attrs.iter().all(|attr| expr_is_constant(&attr.value)),
        ast::ExprKind::Unary { operand, .. } => expr_is_constant(operand),
        ast::ExprKind::Binary { lhs, rhs, .. } => expr_is_constant(lhs) && expr_is_constant(rhs),
        ast::ExprKind::Cond { cond, then, otherwise } => {
            expr_is_constant(cond) && expr_is_constant(then) && expr_is_constant(otherwise)
        }
        // constant indexes fold away, a residual one is dynamic
        ast::ExprKind::Index { .. } => false,
        _ => true,
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
        let model = model(r#"trace "t" { input = choice("a", 1) metrics = { n = range(1, 5) x = range(0, 1.5) } }"#).unwrap();
        let fields = &model.traces[0].fields;

        let Some(Value::Func(Func::Choice(options))) = &fields.input else {
            panic!("expected a choice");
        };
        assert!(matches!(&options[0], Value::Str(value) if value == "a"));
        assert!(matches!(options[1], Value::Num(Number::Int(1))));

        let metrics = fields.metrics.as_ref().unwrap();
        assert!(matches!(
            metrics.elem[0].value,
            Value::Func(Func::Range(Range::Int { min: 1, max: 5 }))
        ));
        assert!(matches!(
            metrics.elem[1].value,
            Value::Func(Func::Range(Range::Float { min, max })) if min == 0.0 && max == 1.5
        ));
    }

    #[test]
    fn resolves_variables_inside_func_args() {
        let model = model(r#"vars { m = "gpt" } trace "t" { input = choice(var.m, "x") }"#).unwrap();

        let Some(Value::Func(Func::Choice(options))) = &model.traces[0].fields.input else {
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

    #[test]
    fn folds_constant_arithmetic() {
        let model = model(r#"trace "t" { metrics = { a = 1 + 2 * 3 b = 7 / 2 c = 7 % 2 d = 10 - 12 } }"#).unwrap();
        let metrics = model.traces[0].fields.metrics.as_ref().unwrap();

        assert!(matches!(metrics.elem[0].value, Value::Num(Number::Int(7))));
        // integer division truncates toward zero
        assert!(matches!(metrics.elem[1].value, Value::Num(Number::Int(3))));
        assert!(matches!(metrics.elem[2].value, Value::Num(Number::Int(1))));
        assert!(matches!(metrics.elem[3].value, Value::Num(Number::Int(-2))));
    }

    #[test]
    fn folds_float_promotion_and_keeps_floats_float() {
        let model = model(r#"trace "t" { metrics = { a = 1 + 0.5 b = 3.0 / 2 c = 1.5 + 1.5 } }"#).unwrap();
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
        let model = model(r#"trace "t" { metrics = { a = -9223372036854775808 b = --5 c = -1.5 } }"#).unwrap();
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
                found: ExprType::Expr,
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
                found: ExprType::Expr,
                ..
            }
        ));
        assert!(errors[0].to_string().contains("found expression"));

        let errors = model(r#"trace "t" { metadata = 1 + range(0, 1) }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Expr,
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
        assert!(matches!(**rhs, Value::Func(Func::Range(Range::Int { min: 0, max: 1 }))));
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
                user = { name = "ada" langs = ["en", "fr"] }
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
            Some(Value::Func(Func::Range(Range::Int { min: 1, max: 5 })))
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
        assert!(matches!(**index, Value::Func(Func::Range(Range::Int { min: 0, max: 1 }))));
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
                found: ExprType::Expr,
                ..
            }
        ));

        let errors = model(r#"trace "t" { tags = [["a", "b"][range(0, 1)]] }"#).unwrap_err();
        assert!(matches!(
            errors[0].kind(),
            ErrorKind::TypeMismatch {
                found: ExprType::Expr,
                ..
            }
        ));
    }

    #[test]
    fn rejects_index_exprs_referencing_vars_in_vars() {
        let errors = model(r#"vars { a = [1] b = var.a[0] } trace "t" { input = 1 }"#).unwrap_err();

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
}
