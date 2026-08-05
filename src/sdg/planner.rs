use crate::dsl::{
    Array as ModelArray, BinOp, CtxRef as ModelCtxRef, Func as ModelFunc, Model, Number as ModelNumber, Object as ModelObject,
    ObjectField as ModelObjectField, Part as ModelPart, Range as ModelRange, Span as ModelSpan, SpanFields as ModelSpanFields,
    SpanKind as ModelSpanKind, SrcRange, Template as ModelTemplate, Trace as ModelTrace, UnaryOp, Value as ModelValue,
};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use std::ops::Range;
use thiserror::Error as Err;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Plan {
    pub(super) events: Box<[EventPlan]>,
    pub(super) traces: Box<[Range<usize>]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct EventRef(pub(super) usize);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EventPlan {
    pub(super) root: EventRef,
    pub(super) parent: Option<EventRef>,
    pub(super) name: String,
    pub(super) kind: EventKind,
    pub(super) fields: EventFields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum EventKind {
    Task,
    Llm,
}

impl EventKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            EventKind::Task => "task",
            EventKind::Llm => "llm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EventFields {
    pub(super) input: Option<JsonValue>,
    pub(super) output: Option<JsonValue>,
    pub(super) metadata: Option<JsonMap<String, JsonValue>>,
    pub(super) metrics: Option<JsonMap<String, JsonValue>>,
    pub(super) tags: Box<[String]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Planner {
    events: Vec<EventPlan>,
    traces: Vec<Range<usize>>,
}

// per-trace generation context that templates and functions resolve against
#[derive(Debug)]
struct Ctx {
    trace_index: usize,
    rng: SmallRng,
}

impl Planner {
    fn plan_trace(&mut self, trace: ModelTrace, ctx: &mut Ctx) -> Result<(), Error> {
        let start = self.events.len();
        let root = EventRef(start);

        let ModelTrace { name, fields, children } = trace;

        self.events.push(EventPlan {
            root,
            parent: None,
            name,
            kind: EventKind::Task,
            fields: lower_fields(fields, ctx)?,
        });

        for child in children {
            self.plan_span(child, root, root, ctx)?;
        }

        self.traces.push(start..self.events.len());
        Ok(())
    }

    fn plan_span(&mut self, span: ModelSpan, root: EventRef, parent: EventRef, ctx: &mut Ctx) -> Result<(), Error> {
        let event_ref = EventRef(self.events.len());

        let ModelSpan {
            name,
            kind,
            fields,
            children,
        } = span;

        self.events.push(EventPlan {
            root,
            parent: Some(parent),
            name,
            kind: match kind {
                ModelSpanKind::Task => EventKind::Task,
                ModelSpanKind::Llm => EventKind::Llm,
            },
            fields: lower_fields(fields, ctx)?,
        });

        for child in children {
            self.plan_span(child, root, event_ref, ctx)?;
        }

        Ok(())
    }
}

fn trace_len(trace: &ModelTrace) -> usize {
    1 + trace.children.iter().map(span_len).sum::<usize>()
}
fn span_len(span: &ModelSpan) -> usize {
    1 + span.children.iter().map(span_len).sum::<usize>()
}

fn lower_fields(fields: ModelSpanFields, ctx: &mut Ctx) -> Result<EventFields, Error> {
    Ok(EventFields {
        input: fields.input.map(|value| lower_value(value, ctx)).transpose()?,
        output: fields.output.map(|value| lower_value(value, ctx)).transpose()?,
        metadata: fields.metadata.map(|object| lower_object(object, ctx)).transpose()?,
        metrics: fields.metrics.map(|object| lower_object(object, ctx)).transpose()?,
        tags: fields.tags.into_iter().map(|tag| resolve_template(tag, ctx)).collect(),
    })
}

fn lower_value(value: ModelValue, ctx: &mut Ctx) -> Result<JsonValue, Error> {
    let value = match value {
        ModelValue::Str(value) => JsonValue::String(value),
        ModelValue::Template(template) => JsonValue::String(resolve_template(template, ctx)),
        ModelValue::Bool(value) => JsonValue::Bool(value),
        ModelValue::Null => JsonValue::Null,

        ModelValue::Num(ModelNumber::Int(value)) => JsonValue::Number(value.into()),

        ModelValue::Num(ModelNumber::Float(value)) => {
            let number = JsonNumber::from_f64(value).expect("modeler guarantees finite floating-point numbers");
            JsonValue::Number(number)
        }

        ModelValue::Array(ModelArray { elem }) => JsonValue::Array(
            elem.into_iter()
                .map(|value| lower_value(value, ctx))
                .collect::<Result<_, _>>()?,
        ),

        ModelValue::Object(object) => JsonValue::Object(lower_object(object, ctx)?),

        ModelValue::Func(func) => eval_func(func, ctx)?,

        ModelValue::Unary { op, operand, range } => {
            let operand = eval_operand(*operand, ctx)?;
            scalar_to_json(eval_unary(op, operand, range)?)
        }

        ModelValue::Binary { op, lhs, rhs, range } => scalar_to_json(eval_binary(op, *lhs, *rhs, range, ctx)?),

        // branches may be arbitrary values, only the taken one is evaluated
        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool() {
                then
            } else {
                otherwise
            };
            lower_value(*taken, ctx)?
        }
    };

    Ok(value)
}

fn eval_func(func: ModelFunc, ctx: &mut Ctx) -> Result<JsonValue, Error> {
    let value = match func {
        ModelFunc::Choice(options) => {
            let pick = ctx.rng.random_range(0..options.len());
            let option = options
                .into_iter()
                .nth(pick)
                .expect("modeler guarantees choice has an alternative");

            lower_value(option, ctx)?
        }

        ModelFunc::Range(ModelRange::Int { min, max }) => JsonValue::Number(ctx.rng.random_range(min..=max).into()),

        ModelFunc::Range(ModelRange::Float { min, max }) => {
            let number = JsonNumber::from_f64(ctx.rng.random_range(min..=max)).expect("modeler guarantees finite bounds");
            JsonValue::Number(number)
        }
    };

    Ok(value)
}

fn lower_object(object: ModelObject, ctx: &mut Ctx) -> Result<JsonMap<String, JsonValue>, Error> {
    object
        .elem
        .into_iter()
        .map(|ModelObjectField { key, value }| Ok((key, lower_value(value, ctx)?)))
        .collect()
}

// scalar operand of an operator expr, the modeler validated the types
#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

impl Scalar {
    fn into_bool(self) -> bool {
        match self {
            Self::Bool(value) => value,
            _ => unreachable!("modeler validated operand types"),
        }
    }

    fn as_float(&self) -> f64 {
        match self {
            Self::Int(value) => *value as f64,
            Self::Float(value) => *value,
            _ => unreachable!("modeler validated operand types"),
        }
    }
}

// finiteness is checked by eval before conversion
fn scalar_to_json(scalar: Scalar) -> JsonValue {
    match scalar {
        Scalar::Int(value) => JsonValue::Number(value.into()),
        Scalar::Float(value) => JsonValue::Number(JsonNumber::from_f64(value).expect("evaluation rejects non-finite results")),
        Scalar::Bool(value) => JsonValue::Bool(value),
        Scalar::Str(value) => JsonValue::String(value),
    }
}

fn eval_operand(value: ModelValue, ctx: &mut Ctx) -> Result<Scalar, Error> {
    let scalar = match value {
        ModelValue::Str(value) => Scalar::Str(value),
        ModelValue::Template(template) => Scalar::Str(resolve_template(template, ctx)),
        ModelValue::Num(ModelNumber::Int(value)) => Scalar::Int(value),
        ModelValue::Num(ModelNumber::Float(value)) => Scalar::Float(value),
        ModelValue::Bool(value) => Scalar::Bool(value),

        ModelValue::Func(ModelFunc::Choice(options)) => {
            let pick = ctx.rng.random_range(0..options.len());
            let option = options
                .into_iter()
                .nth(pick)
                .expect("modeler guarantees choice has an alternative");

            eval_operand(option, ctx)?
        }
        ModelValue::Func(ModelFunc::Range(ModelRange::Int { min, max })) => Scalar::Int(ctx.rng.random_range(min..=max)),
        ModelValue::Func(ModelFunc::Range(ModelRange::Float { min, max })) => Scalar::Float(ctx.rng.random_range(min..=max)),

        ModelValue::Unary { op, operand, range } => {
            let operand = eval_operand(*operand, ctx)?;
            eval_unary(op, operand, range)?
        }
        ModelValue::Binary { op, lhs, rhs, range } => eval_binary(op, *lhs, *rhs, range, ctx)?,
        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool() {
                then
            } else {
                otherwise
            };
            eval_operand(*taken, ctx)?
        }

        ModelValue::Null | ModelValue::Array(_) | ModelValue::Object(_) => {
            unreachable!("modeler validated operand types")
        }
    };

    Ok(scalar)
}

fn eval_unary(op: UnaryOp, operand: Scalar, range: SrcRange) -> Result<Scalar, Error> {
    let scalar = match (op, operand) {
        (UnaryOp::Neg, Scalar::Int(value)) => {
            Scalar::Int(value.checked_neg().ok_or(Error::new(ErrorKind::NonFiniteResult, range))?)
        }
        (UnaryOp::Neg, Scalar::Float(value)) => Scalar::Float(-value),
        (UnaryOp::Not, Scalar::Bool(value)) => Scalar::Bool(!value),
        _ => unreachable!("modeler validated operand types"),
    };

    Ok(scalar)
}

fn eval_binary(op: BinOp, lhs: ModelValue, rhs: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<Scalar, Error> {
    // logical ops short-circuit so guard idioms never evaluate the right side
    if matches!(op, BinOp::And | BinOp::Or) {
        let left = eval_operand(lhs, ctx)?.into_bool();
        let value = match (op, left) {
            (BinOp::And, false) => false,
            (BinOp::Or, true) => true,
            _ => eval_operand(rhs, ctx)?.into_bool(),
        };
        return Ok(Scalar::Bool(value));
    }

    let lhs = eval_operand(lhs, ctx)?;
    let rhs = eval_operand(rhs, ctx)?;

    let scalar = match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => eval_arith(op, lhs, rhs, range)?,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ordering = match (&lhs, &rhs) {
                (Scalar::Int(lhs), Scalar::Int(rhs)) => lhs.partial_cmp(rhs),
                _ => lhs.as_float().partial_cmp(&rhs.as_float()),
            };
            let ordering = ordering.expect("finite numbers are always ordered");
            Scalar::Bool(match op {
                BinOp::Lt => ordering.is_lt(),
                BinOp::Le => ordering.is_le(),
                BinOp::Gt => ordering.is_gt(),
                BinOp::Ge => ordering.is_ge(),
                _ => unreachable!("operator is a comparison"),
            })
        }
        BinOp::Eq | BinOp::Ne => {
            let equal = match (&lhs, &rhs) {
                (Scalar::Str(lhs), Scalar::Str(rhs)) => lhs == rhs,
                (Scalar::Bool(lhs), Scalar::Bool(rhs)) => lhs == rhs,
                (Scalar::Int(lhs), Scalar::Int(rhs)) => lhs == rhs,
                _ => lhs.as_float() == rhs.as_float(),
            };
            Scalar::Bool(if op == BinOp::Eq { equal } else { !equal })
        }
        BinOp::And | BinOp::Or => unreachable!("logical operators short-circuit above"),
    };

    Ok(scalar)
}

fn eval_arith(op: BinOp, lhs: Scalar, rhs: Scalar, range: SrcRange) -> Result<Scalar, Error> {
    let scalar = match (lhs, rhs) {
        (Scalar::Int(lhs), Scalar::Int(rhs)) => {
            if matches!(op, BinOp::Div | BinOp::Rem) && rhs == 0 {
                return Err(Error::new(ErrorKind::DivisionByZero, range));
            }
            let result = match op {
                BinOp::Add => lhs.checked_add(rhs),
                BinOp::Sub => lhs.checked_sub(rhs),
                BinOp::Mul => lhs.checked_mul(rhs),
                // checked catches i64::MIN / -1
                BinOp::Div => lhs.checked_div(rhs),
                BinOp::Rem => lhs.checked_rem(rhs),
                _ => unreachable!("operator is arithmetic"),
            };
            Scalar::Int(result.ok_or(Error::new(ErrorKind::NonFiniteResult, range))?)
        }
        (lhs, rhs) => {
            let (lhs, rhs) = (lhs.as_float(), rhs.as_float());
            if matches!(op, BinOp::Div | BinOp::Rem) && rhs == 0.0 {
                return Err(Error::new(ErrorKind::DivisionByZero, range));
            }
            let result = match op {
                BinOp::Add => lhs + rhs,
                BinOp::Sub => lhs - rhs,
                BinOp::Mul => lhs * rhs,
                BinOp::Div => lhs / rhs,
                BinOp::Rem => lhs % rhs,
                _ => unreachable!("operator is arithmetic"),
            };
            if !result.is_finite() {
                return Err(Error::new(ErrorKind::NonFiniteResult, range));
            }
            Scalar::Float(result)
        }
    };

    Ok(scalar)
}

fn resolve_template(template: ModelTemplate, ctx: &mut Ctx) -> String {
    template
        .parts
        .into_iter()
        .map(|part| match part {
            ModelPart::Lit(value) => value,
            ModelPart::Ref(ModelCtxRef::TraceIndex) => ctx.trace_index.to_string(),
        })
        .collect()
}

// expands into exactly count traces, multiple trace templates cycle in source order
pub(super) fn plan(model: Model, count: usize, seed: u64) -> Result<Plan, Error> {
    debug_assert!(!model.traces.is_empty());

    let capacity = (0..count)
        .map(|index| trace_len(&model.traces[index % model.traces.len()]))
        .sum();

    let mut planner = Planner {
        events: Vec::with_capacity(capacity),
        traces: Vec::with_capacity(count),
    };

    for index in 0..count {
        // each trace gets its own rng so its values don't depend on the traces planned before it
        let mut ctx = Ctx {
            trace_index: index,
            rng: SmallRng::seed_from_u64(seed.wrapping_add(index as u64)),
        };
        planner.plan_trace(model.traces[index % model.traces.len()].clone(), &mut ctx)?;
    }

    Ok(Plan {
        events: planner.events.into_boxed_slice(),
        traces: planner.traces.into_boxed_slice(),
    })
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
#[error("{kind}")]
pub(crate) struct Error {
    kind: ErrorKind,
    pub(crate) range: SrcRange,
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
enum ErrorKind {
    #[error("expression divides by zero")]
    DivisionByZero,
    #[error("expression result overflowed or is not finite")]
    NonFiniteResult,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;

    #[test]
    fn prints_plan() {
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();
        let plan = plan(model, 1, 0).unwrap();

        println!("{plan:#?}");
    }

    #[test]
    fn lowers_null_and_negative_numbers_to_json() {
        let model = compile(r#"trace "t" { input = null metrics = { delta = -0.5 } }"#).unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let fields = &plan.events[0].fields;
        assert_eq!(fields.input, Some(JsonValue::Null));
        assert_eq!(fields.metrics.as_ref().unwrap()["delta"], JsonValue::from(-0.5));
    }

    #[test]
    fn resolves_trace_index_per_generated_trace() {
        let model = compile(r#"trace "t" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let plan = plan(model, 3, 0).unwrap();

        let inputs = plan
            .events
            .iter()
            .map(|event| event.fields.input.clone().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            inputs,
            [JsonValue::from("q 0"), JsonValue::from("q 1"), JsonValue::from("q 2")]
        );
        assert_eq!(plan.events[2].fields.tags.as_ref(), ["t-2"]);
    }

    #[test]
    fn cycles_templates_to_the_requested_trace_count() {
        let model = compile(
            r#"
                trace "first" {}
                trace "second" {}
            "#,
        )
        .unwrap();
        let plan = plan(model, 5, 0).unwrap();

        assert_eq!(plan.traces.len(), 5);
        assert_eq!(
            plan.events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
            ["first", "second", "first", "second", "first"]
        );
    }

    #[test]
    fn evaluates_choice_within_its_alternatives() {
        let model = compile(r#"trace "t" { input = choice("a", "b") }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "a" | "b"), "unexpected choice {input}");
        }
    }

    #[test]
    fn evaluates_ranges_within_bounds_and_keeps_integer_bounds_integral() {
        let model = compile(r#"trace "t" { metrics = { tokens = range(80, 400) temp = range(0.0, 1.0) } }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let metrics = event.fields.metrics.as_ref().unwrap();
            let tokens = metrics["tokens"].as_i64().unwrap();
            assert!((80..=400).contains(&tokens));
            let temp = metrics["temp"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&temp));
        }
    }

    #[test]
    fn evaluates_nested_funcs_and_funcs_from_vars() {
        let model = compile(
            r#"
                vars { style = choice("clear", "vague") }
                trace "t" { input = [choice(range(1, 3), "x"), var.style] }
            "#,
        )
        .unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let JsonValue::Array(input) = event.fields.input.as_ref().unwrap() else {
                panic!("expected an array");
            };
            match &input[0] {
                JsonValue::Number(number) => assert!((1..=3).contains(&number.as_i64().unwrap())),
                JsonValue::String(value) => assert_eq!(value, "x"),
                other => panic!("unexpected value {other}"),
            }
            assert!(matches!(input[1].as_str().unwrap(), "clear" | "vague"));
        }
    }

    #[test]
    fn reproduces_the_same_plan_for_the_same_seed() {
        let source = r#"trace "t" { input = choice("a", "b", "c") metrics = { n = range(0, 1000000) } }"#;
        let first = plan(compile(source).unwrap(), 10, 42).unwrap();
        let second = plan(compile(source).unwrap(), 10, 42).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn samples_traces_independently_of_earlier_traces() {
        // trace 3 of a 5-trace run must match trace 3 of a 4-trace run
        let source = r#"trace "t" { metrics = { n = range(0, 1000000) } }"#;
        let longer = plan(compile(source).unwrap(), 5, 42).unwrap();
        let shorter = plan(compile(source).unwrap(), 4, 42).unwrap();

        assert_eq!(longer.events[3], shorter.events[3]);
    }

    #[test]
    fn evaluates_dynamic_arithmetic_within_bounds() {
        let source = r#"trace "t" { metrics = { n = range(1, 5) * 100 m = -range(1, 5) } }"#;
        let plan_a = plan(compile(source).unwrap(), 20, 7).unwrap();

        for event in plan_a.events.iter() {
            let metrics = event.fields.metrics.as_ref().unwrap();
            let n = metrics["n"].as_i64().unwrap();
            assert!((100..=500).contains(&n) && n % 100 == 0, "unexpected n {n}");
            let m = metrics["m"].as_i64().unwrap();
            assert!((-5..=-1).contains(&m), "unexpected m {m}");
        }

        // same seed, same values
        let plan_b = plan(compile(source).unwrap(), 20, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_dynamic_ternaries_per_trace() {
        let model = compile(r#"trace "t" { input = choice(true, false) ? "a" : "b" }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        let mut seen = std::collections::HashSet::new();
        for event in plan.events.iter() {
            seen.insert(event.fields.input.as_ref().unwrap().as_str().unwrap().to_owned());
        }
        assert_eq!(seen.len(), 2, "expected both branches over 20 traces");
    }

    #[test]
    fn compares_resolved_templates_per_trace() {
        let model = compile(r#"trace "t" { input = "i ${trace.index}" == "i 1" ? 1 : 0 }"#).unwrap();
        let plan = plan(model, 3, 0).unwrap();

        let inputs: Vec<_> = plan
            .events
            .iter()
            .map(|event| event.fields.input.as_ref().unwrap().as_i64().unwrap())
            .collect();
        assert_eq!(inputs, [0, 1, 0]);
    }

    #[test]
    fn short_circuits_guarded_divisions() {
        // the zero divisor is never evaluated, so neither shape fails
        let model = compile(r#"trace "t" { input = range(0, 0) == 0 ? 0 : 100 / range(0, 0) }"#).unwrap();
        let planned = plan(model, 5, 7).unwrap();
        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(0)));

        let model = compile(r#"trace "t" { input = range(0, 0) != 0 && 100 / range(0, 0) > 1 }"#).unwrap();
        let planned = plan(model, 5, 7).unwrap();
        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(false)));
    }

    #[test]
    fn fails_on_dynamic_division_by_zero() {
        let source = r#"trace "t" { input = 100 / range(0, 0) }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();

        assert_eq!(error.to_string(), "expression divides by zero");
        assert_eq!(&source[error.range.start..error.range.end], "100 / range(0, 0)");
    }

    #[test]
    fn fails_on_dynamic_overflow_and_non_finite_results() {
        let source = r#"trace "t" { metrics = { n = 9223372036854775807 + range(1, 1) } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "expression result overflowed or is not finite");

        // ~9.99e307 is finite on its own, doubling it overflows f64
        let big = format!("{}.0", "9".repeat(308));
        let source = format!(r#"trace "t" {{ input = {big} * range(2.0, 2.0) }}"#);
        let error = plan(compile(&source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "expression result overflowed or is not finite");
    }
}
