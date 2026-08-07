use crate::dsl::{
    Array as ModelArray, BinOp, Child as ModelChild, Choice as ModelChoice, CtxRef as ModelCtxRef, Func as ModelFunc,
    Maybe as ModelMaybe, Model, Number as ModelNumber, Object as ModelObject, ObjectField as ModelObjectField,
    Part as ModelPart, Range as ModelRange, Repeat as ModelRepeat, Span as ModelSpan, SpanFields as ModelSpanFields,
    SpanKind as ModelSpanKind, SrcRange, Template as ModelTemplate, Trace as ModelTrace, UnaryOp, Value as ModelValue,
};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Beta, Exp, LogNormal, Normal, Pareto, Poisson};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use std::fmt;
use std::ops::Range;
use std::sync::LazyLock;
use tiktoken_rs::CoreBPE;

// building the encoder parses the embedded vocab, do it once
static BPE: LazyLock<CoreBPE> = LazyLock::new(|| tiktoken_rs::o200k_base().expect("the embedded vocab parses"));

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
    Tool,
    Function,
}

impl EventKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            EventKind::Task => "task",
            EventKind::Llm => "llm",
            EventKind::Tool => "tool",
            EventKind::Function => "function",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EventFields {
    pub(super) input: Option<JsonValue>,
    pub(super) output: Option<JsonValue>,
    pub(super) expected: Option<JsonValue>,
    pub(super) error: Option<JsonValue>,
    pub(super) metadata: Option<JsonMap<String, JsonValue>>,
    pub(super) metrics: Option<JsonMap<String, JsonValue>>,
    pub(super) tags: Box<[String]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Planner {
    events: Vec<EventPlan>,
    traces: Vec<Range<usize>>,
}

#[derive(Debug)]
struct Ctx {
    trace_index: usize,
    rng: SmallRng,
    // iteration indexes of the enclosing repeats, innermost last
    repeat_indices: Vec<usize>,
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
            self.plan_child(child, root, root, ctx)?;
        }

        self.traces.push(start..self.events.len());
        Ok(())
    }

    // dynamic blocks draw their structural decisions before any child is planned
    fn plan_child(&mut self, child: ModelChild, root: EventRef, parent: EventRef, ctx: &mut Ctx) -> Result<(), Error> {
        match child {
            ModelChild::Span(span) => self.plan_span(span, root, parent, ctx),

            ModelChild::Repeat(ModelRepeat {
                count,
                count_range,
                children,
                ..
            }) => {
                let count = eval_count(count, count_range, ctx)?;
                for index in 0..count {
                    ctx.repeat_indices.push(index);
                    for child in &children {
                        self.plan_child(child.clone(), root, parent, ctx)?;
                    }
                    ctx.repeat_indices.pop();
                }
                Ok(())
            }

            ModelChild::Choice(ModelChoice { children, .. }) => {
                let pick = ctx.rng.random_range(0..children.len());
                let child = children.into_iter().nth(pick).expect("modeler guarantees choice has a child");

                self.plan_child(child, root, parent, ctx)
            }

            ModelChild::Maybe(ModelMaybe {
                chance,
                chance_range,
                children,
                ..
            }) => {
                let chance = eval_chance(chance, chance_range, ctx)?;
                if ctx.rng.random_bool(chance) {
                    for child in children {
                        self.plan_child(child, root, parent, ctx)?;
                    }
                }
                Ok(())
            }
        }
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
                ModelSpanKind::Tool => EventKind::Tool,
                ModelSpanKind::Function => EventKind::Function,
            },
            fields: lower_fields(fields, ctx)?,
        });

        for child in children {
            self.plan_child(child, root, event_ref, ctx)?;
        }

        Ok(())
    }
}

fn eval_count(count: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<usize, Error> {
    match eval_operand(count, ctx)? {
        Scalar::Int(value) => usize::try_from(value).map_err(|_| Error::new(ErrorKind::NegativeRepeatCount, range)),
        Scalar::Float(_) => Err(Error::new(ErrorKind::NonIntegerRepeatCount, range)),
        _ => unreachable!("modeler validated the count as a number"),
    }
}

fn eval_chance(chance: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<f64, Error> {
    let value = match eval_operand(chance, ctx)? {
        Scalar::Int(value) => value as f64,
        Scalar::Float(value) => value,
        _ => unreachable!("modeler validated the chance as a number"),
    };

    if (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err(Error::new(ErrorKind::ChanceOutOfRange, range))
    }
}

// dynamic blocks make these estimates, only used as allocation hints
fn trace_len(trace: &ModelTrace) -> usize {
    1 + trace.children.iter().map(child_len).sum::<usize>()
}
fn child_len(child: &ModelChild) -> usize {
    match child {
        ModelChild::Span(span) => 1 + span.children.iter().map(child_len).sum::<usize>(),
        // one iteration or inclusion
        ModelChild::Repeat(repeat) => repeat.children.iter().map(child_len).sum(),
        ModelChild::Maybe(maybe) => maybe.children.iter().map(child_len).sum(),
        ModelChild::Choice(choice) => choice.children.iter().map(child_len).max().unwrap_or(0),
    }
}

fn lower_fields(fields: ModelSpanFields, ctx: &mut Ctx) -> Result<EventFields, Error> {
    Ok(EventFields {
        input: fields.input.map(|value| lower_value(value, ctx)).transpose()?,
        output: fields.output.map(|value| lower_value(value, ctx)).transpose()?,
        expected: fields.expected.map(|value| lower_value(value, ctx)).transpose()?,
        error: fields.error.map(|value| lower_value(value, ctx)).transpose()?,
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

        ModelValue::Func { func, range } => lower_value(eval_func(func, range, ctx)?, ctx)?,

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

        ModelValue::Index { target, index, range } => lower_value(eval_index(*target, *index, range, ctx)?, ctx)?,

        ModelValue::Slice {
            target,
            start,
            end,
            range,
        } => lower_value(eval_slice(*target, start, end, range, ctx)?, ctx)?,
    };

    Ok(value)
}

// generous bounds clamp like python
fn eval_slice(
    target: ModelValue,
    start: Option<Box<ModelValue>>,
    end: Option<Box<ModelValue>>,
    range: SrcRange,
    ctx: &mut Ctx,
) -> Result<ModelValue, Error> {
    let ModelValue::Array(ModelArray { elem }) = eval_container(target, ctx)? else {
        unreachable!("modeler validated the slice target as an array");
    };

    let len = elem.len();
    let start = match start {
        Some(bound) => eval_slice_bound(*bound, range, ctx)?.min(len),
        None => 0,
    };
    let end = match end {
        Some(bound) => eval_slice_bound(*bound, range, ctx)?.min(len),
        None => len,
    };

    let elem = if start >= end {
        Vec::new()
    } else {
        elem.into_iter().skip(start).take(end - start).collect()
    };

    Ok(ModelValue::Array(ModelArray { elem }))
}

fn eval_slice_bound(bound: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<usize, Error> {
    match eval_operand(bound, ctx)? {
        Scalar::Int(value) => usize::try_from(value).map_err(|_| Error::new(ErrorKind::NegativeSliceBound, range)),
        Scalar::Float(_) => Err(Error::new(ErrorKind::NonIntegerSliceBound, range)),
        _ => unreachable!("modeler validated slice bounds as numbers"),
    }
}

// siblings of the picked element stay unevaluated like an untaken branch
fn eval_index(target: ModelValue, index: ModelValue, range: SrcRange, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    let target = eval_container(target, ctx)?;
    let index = eval_operand(index, ctx)?;

    match (target, index) {
        (ModelValue::Array(ModelArray { elem }), Scalar::Int(position)) => usize::try_from(position)
            .ok()
            .filter(|&position| position < elem.len())
            .map(|position| elem.into_iter().nth(position).expect("position is in bounds"))
            .ok_or(Error::new(ErrorKind::IndexOutOfBounds, range)),
        (ModelValue::Array(_), Scalar::Float(_)) => Err(Error::new(ErrorKind::NonIntegerIndex, range)),
        (ModelValue::Object(ModelObject { elem }), Scalar::Str(key)) => elem
            .into_iter()
            .find(|field| field.key == key)
            .map(|field| field.value)
            .ok_or(Error::new(ErrorKind::MissingObjectKey, range)),
        _ => unreachable!("modeler validated index types"),
    }
}

fn eval_container(value: ModelValue, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    match value {
        ModelValue::Array(_) | ModelValue::Object(_) => Ok(value),
        ModelValue::Func { func, range } => eval_container(eval_func(func, range, ctx)?, ctx),
        ModelValue::Cond {
            cond, then, otherwise, ..
        } => {
            let taken = if eval_operand(*cond, ctx)?.into_bool() {
                then
            } else {
                otherwise
            };
            eval_container(*taken, ctx)
        }
        ModelValue::Index { target, index, range } => eval_container(eval_index(*target, *index, range, ctx)?, ctx),
        ModelValue::Slice {
            target,
            start,
            end,
            range,
        } => eval_slice(*target, start, end, range, ctx),
        _ => unreachable!("modeler validated the target as an array or object"),
    }
}

// a choice or weighted pick may itself still be dynamic, so callers recurse on the result
fn eval_func(func: ModelFunc, range: SrcRange, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    let value = match func {
        ModelFunc::Choice(options) => {
            let pick = ctx.rng.random_range(0..options.len());
            options
                .into_iter()
                .nth(pick)
                .expect("modeler guarantees choice has an alternative")
        }

        ModelFunc::Weighted(options) => {
            let total: f64 = options.iter().map(|option| option.weight).sum();
            let draw = ctx.rng.random_range(0.0..total);
            let mut acc = 0.0;
            let mut pick = None;
            let mut fallback = None;
            for (index, option) in options.iter().enumerate() {
                if option.weight > 0.0 {
                    fallback = Some(index);
                }
                acc += option.weight;
                if pick.is_none() && draw < acc && option.weight > 0.0 {
                    pick = Some(index);
                }
            }
            // float accumulation can leave the draw past every bucket, fall back
            // to the last pickable option
            let pick = pick.or(fallback).expect("modeler guarantees a positive weight");
            options.into_iter().nth(pick).expect("pick indexes into the options").value
        }

        ModelFunc::Range(ModelRange::Int { min, max }) => ModelValue::Num(ModelNumber::Int(ctx.rng.random_range(min..=max))),
        ModelFunc::Range(ModelRange::Float { min, max }) => {
            ModelValue::Num(ModelNumber::Float(ctx.rng.random_range(min..=max)))
        }

        ModelFunc::Normal { mean, stddev } => {
            let sample = ctx.rng.sample(Normal::new(mean, stddev).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Lognormal { median, sigma } => {
            let sample = ctx
                .rng
                .sample(LogNormal::new(median.ln(), sigma).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Exponential { mean } => {
            let sample = ctx.rng.sample(Exp::new(1.0 / mean).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Pareto { min, shape } => {
            let sample = ctx.rng.sample(Pareto::new(min, shape).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Beta { alpha, beta } => {
            let sample = ctx.rng.sample(Beta::new(alpha, beta).expect("modeler validated params"));
            finite_float(sample, range)?
        }
        ModelFunc::Poisson { mean } => {
            let sample: f64 = ctx.rng.sample(Poisson::new(mean).expect("modeler validated params"));
            ModelValue::Num(ModelNumber::Int(sample as i64))
        }
        ModelFunc::Chance { probability } => ModelValue::Bool(ctx.rng.random_bool(probability)),

        ModelFunc::Upper { text } => ModelValue::Str(eval_text(*text, ctx)?.to_uppercase()),
        ModelFunc::Lower { text } => ModelValue::Str(eval_text(*text, ctx)?.to_lowercase()),
        ModelFunc::Trim { text } => ModelValue::Str(eval_text(*text, ctx)?.trim().to_owned()),
        ModelFunc::Replace { text, from, to } => {
            let text = eval_text(*text, ctx)?;
            let from = eval_text(*from, ctx)?;
            let to = eval_text(*to, ctx)?;
            // replacing an empty pattern would splice `to` between every character
            ModelValue::Str(if from.is_empty() { text } else { text.replace(&from, &to) })
        }
        ModelFunc::Split { text, separator } => {
            let text = eval_text(*text, ctx)?;
            let separator = eval_text(*separator, ctx)?;
            if separator.is_empty() {
                return Err(Error::new(ErrorKind::EmptySplitSeparator, range));
            }
            ModelValue::Array(ModelArray {
                elem: text.split(&separator).map(|part| ModelValue::Str(part.to_owned())).collect(),
            })
        }
        ModelFunc::Join { array, separator } => {
            let JsonValue::Array(elems) = lower_value(*array, ctx)? else {
                unreachable!("modeler validated the argument as an array");
            };
            let separator = eval_text(*separator, ctx)?;
            let parts = elems
                .iter()
                .map(|elem| json_text(elem).ok_or(Error::new(ErrorKind::JoinElementNotScalar, range)))
                .collect::<Result<Vec<_>, _>>()?;
            ModelValue::Str(parts.join(&separator))
        }
        ModelFunc::Contains { target, needle } => match lower_value(*target, ctx)? {
            JsonValue::String(text) => {
                let needle = eval_text(*needle, ctx)?;
                ModelValue::Bool(text.contains(&needle))
            }
            JsonValue::Array(elems) => {
                let needle = eval_operand(*needle, ctx)?;
                ModelValue::Bool(elems.iter().any(|elem| json_matches_scalar(elem, &needle)))
            }
            _ => unreachable!("modeler validated the target as a string or array"),
        },
        ModelFunc::StartsWith { text, prefix } => {
            let text = eval_text(*text, ctx)?;
            let prefix = eval_text(*prefix, ctx)?;
            ModelValue::Bool(text.starts_with(&prefix))
        }
        ModelFunc::EndsWith { text, suffix } => {
            let text = eval_text(*text, ctx)?;
            let suffix = eval_text(*suffix, ctx)?;
            ModelValue::Bool(text.ends_with(&suffix))
        }
        ModelFunc::Len { target } => {
            let length = match lower_value(*target, ctx)? {
                JsonValue::String(text) => text.chars().count(),
                JsonValue::Array(elems) => elems.len(),
                _ => unreachable!("modeler validated the target as a string or array"),
            };
            ModelValue::Num(ModelNumber::Int(length as i64))
        }
        ModelFunc::Tokens { value } => {
            // containers count their compact json serialization
            let text = match lower_value(*value, ctx)? {
                JsonValue::String(text) => text,
                value => value.to_string(),
            };
            ModelValue::Num(ModelNumber::Int(BPE.encode_ordinary(&text).len() as i64))
        }
        ModelFunc::Format { template, args } => {
            let mut pieces = template.split("{}");
            let mut text = pieces.next().expect("split yields at least one piece").to_owned();
            for (arg, piece) in args.into_iter().zip(pieces) {
                text.push_str(&scalar_text(eval_operand(arg, ctx)?));
                text.push_str(piece);
            }
            ModelValue::Str(text)
        }

        ModelFunc::Clamp { value, min, max } => {
            let value = eval_operand(*value, ctx)?;
            let min = eval_operand(*min, ctx)?;
            let max = eval_operand(*max, ctx)?;
            match (value, min, max) {
                (Scalar::Int(value), Scalar::Int(min), Scalar::Int(max)) => {
                    if min > max {
                        return Err(Error::new(ErrorKind::ClampBoundsOutOfOrder, range));
                    }
                    ModelValue::Num(ModelNumber::Int(value.clamp(min, max)))
                }
                (value, min, max) => {
                    let (value, min, max) = (value.as_float(), min.as_float(), max.as_float());
                    if min > max {
                        return Err(Error::new(ErrorKind::ClampBoundsOutOfOrder, range));
                    }
                    ModelValue::Num(ModelNumber::Float(value.clamp(min, max)))
                }
            }
        }
        ModelFunc::Round { value } => eval_to_int(eval_operand(*value, ctx)?, f64::round, range)?,
        ModelFunc::Floor { value } => eval_to_int(eval_operand(*value, ctx)?, f64::floor, range)?,
        ModelFunc::Ceil { value } => eval_to_int(eval_operand(*value, ctx)?, f64::ceil, range)?,
        ModelFunc::Abs { value } => match eval_operand(*value, ctx)? {
            Scalar::Int(value) => ModelValue::Num(ModelNumber::Int(
                value.checked_abs().ok_or(Error::new(ErrorKind::NonFiniteResult, range))?,
            )),
            Scalar::Float(value) => ModelValue::Num(ModelNumber::Float(value.abs())),
            _ => unreachable!("modeler validated the argument as a number"),
        },
        ModelFunc::Min(values) => eval_extreme(values, true, ctx)?,
        ModelFunc::Max(values) => eval_extreme(values, false, ctx)?,

        ModelFunc::Uuid => {
            let bytes: [u8; 16] = ctx.rng.random();
            ModelValue::Str(uuid::Builder::from_random_bytes(bytes).into_uuid().to_string())
        }
        ModelFunc::Hex { length } => ModelValue::Str(random_text(b"0123456789abcdef", length, ctx)),
        ModelFunc::Alphanum { length } => ModelValue::Str(random_text(
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz",
            length,
            ctx,
        )),
    };

    Ok(value)
}

fn finite_float(value: f64, range: SrcRange) -> Result<ModelValue, Error> {
    if value.is_finite() {
        Ok(ModelValue::Num(ModelNumber::Float(value)))
    } else {
        Err(Error::new(ErrorKind::NonFiniteResult, range))
    }
}

fn eval_text(value: ModelValue, ctx: &mut Ctx) -> Result<String, Error> {
    match eval_operand(value, ctx)? {
        Scalar::Str(text) => Ok(text),
        _ => unreachable!("modeler validated the argument as a string"),
    }
}

fn eval_to_int(scalar: Scalar, op: fn(f64) -> f64, range: SrcRange) -> Result<ModelValue, Error> {
    let value = match scalar {
        Scalar::Int(value) => value,
        Scalar::Float(value) => {
            let value = op(value);
            if value < i64::MIN as f64 || value > i64::MAX as f64 {
                return Err(Error::new(ErrorKind::NonFiniteResult, range));
            }
            value as i64
        }
        _ => unreachable!("modeler validated the argument as a number"),
    };

    Ok(ModelValue::Num(ModelNumber::Int(value)))
}

fn eval_extreme(values: Vec<ModelValue>, minimize: bool, ctx: &mut Ctx) -> Result<ModelValue, Error> {
    let scalars = values
        .into_iter()
        .map(|value| eval_operand(value, ctx))
        .collect::<Result<Vec<_>, _>>()?;

    let number = if scalars.iter().any(|scalar| matches!(scalar, Scalar::Float(_))) {
        let floats = scalars.iter().map(Scalar::as_float);
        ModelNumber::Float(if minimize {
            floats.fold(f64::INFINITY, f64::min)
        } else {
            floats.fold(f64::NEG_INFINITY, f64::max)
        })
    } else {
        let ints = scalars.iter().map(|scalar| match scalar {
            Scalar::Int(value) => *value,
            _ => unreachable!("modeler validated the arguments as numbers"),
        });
        ModelNumber::Int(if minimize {
            ints.min().expect("modeler requires at least two arguments")
        } else {
            ints.max().expect("modeler requires at least two arguments")
        })
    };

    Ok(ModelValue::Num(number))
}

fn random_text(charset: &[u8], length: usize, ctx: &mut Ctx) -> String {
    (0..length)
        .map(|_| charset[ctx.rng.random_range(0..charset.len())] as char)
        .collect()
}

fn json_text(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) => Some(text.clone()),
        JsonValue::Number(number) => Some(number.to_string()),
        JsonValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn scalar_text(scalar: Scalar) -> String {
    match scalar {
        Scalar::Str(text) => text,
        Scalar::Int(value) => value.to_string(),
        // {:?} keeps a .0 so floats stay recognizable
        Scalar::Float(value) => format!("{value:?}"),
        Scalar::Bool(value) => value.to_string(),
    }
}

// equality between an evaluated element and the needle, ints and floats unify
fn json_matches_scalar(value: &JsonValue, needle: &Scalar) -> bool {
    match (value, needle) {
        (JsonValue::String(value), Scalar::Str(needle)) => value == needle,
        (JsonValue::Bool(value), Scalar::Bool(needle)) => value == needle,
        (JsonValue::Number(value), Scalar::Int(needle)) => match value.as_i64() {
            Some(value) => value == *needle,
            None => value.as_f64() == Some(*needle as f64),
        },
        (JsonValue::Number(value), Scalar::Float(needle)) => value.as_f64() == Some(*needle),
        _ => false,
    }
}

fn lower_object(object: ModelObject, ctx: &mut Ctx) -> Result<JsonMap<String, JsonValue>, Error> {
    object
        .elem
        .into_iter()
        .map(|ModelObjectField { key, value }| Ok((key, lower_value(value, ctx)?)))
        .collect()
}

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

        ModelValue::Func { func, range } => eval_operand(eval_func(func, range, ctx)?, ctx)?,

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
        ModelValue::Index { target, index, range } => eval_operand(eval_index(*target, *index, range, ctx)?, ctx)?,

        // a slice is always an array, which is never a scalar operand
        ModelValue::Null | ModelValue::Array(_) | ModelValue::Object(_) | ModelValue::Slice { .. } => {
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
            ModelPart::Ref(ModelCtxRef::RepeatIndex) => ctx
                .repeat_indices
                .last()
                .expect("modeler validated repeat.index is inside a repeat")
                .to_string(),
        })
        .collect()
}

// multiple trace templates cycle in source order
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
            repeat_indices: Vec::new(),
        };
        planner.plan_trace(model.traces[index % model.traces.len()].clone(), &mut ctx)?;
    }

    Ok(Plan {
        events: planner.events.into_boxed_slice(),
        traces: planner.traces.into_boxed_slice(),
    })
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct Error {
    kind: ErrorKind,
    pub(crate) range: SrcRange,
}

impl Error {
    fn new(kind: ErrorKind, range: SrcRange) -> Self {
        Self { kind, range }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ErrorKind {
    DivisionByZero,
    NonFiniteResult,
    IndexOutOfBounds,
    NonIntegerIndex,
    MissingObjectKey,
    NonIntegerSliceBound,
    NegativeSliceBound,
    NegativeRepeatCount,
    NonIntegerRepeatCount,
    ChanceOutOfRange,
    ClampBoundsOutOfOrder,
    EmptySplitSeparator,
    JoinElementNotScalar,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DivisionByZero => "expression divides by zero",
            Self::NonFiniteResult => "expression result overflowed or is not finite",
            Self::IndexOutOfBounds => "array index is out of bounds",
            Self::NonIntegerIndex => "array index is not an integer",
            Self::MissingObjectKey => "object key is not present",
            Self::NonIntegerSliceBound => "slice bound is not an integer",
            Self::NegativeSliceBound => "slice bound is negative",
            Self::NegativeRepeatCount => "repeat count is negative",
            Self::NonIntegerRepeatCount => "repeat count is not an integer",
            Self::ChanceOutOfRange => "maybe chance is not between 0 and 1",
            Self::ClampBoundsOutOfOrder => "clamp bounds are out of order",
            Self::EmptySplitSeparator => "split separator is empty",
            Self::JoinElementNotScalar => "join element is not a string, number, or boolean",
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for Error {}

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
        let model = compile(r#"trace "t" { metrics = { tokens = range(80, 400), temp = range(0.0, 1.0) } }"#).unwrap();
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
    fn evaluates_weighted_picks_proportionally_to_their_weights() {
        let model = compile(r#"trace "t" { input = weighted(["common", 9], ["rare", 1], ["never", 0]) }"#).unwrap();
        let plan = plan(model, 200, 7).unwrap();

        let mut counts = std::collections::HashMap::new();
        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            *counts.entry(input.to_owned()).or_insert(0) += 1;
        }
        assert!(!counts.contains_key("never"), "zero weights must never be picked");
        assert!(counts["common"] > counts["rare"], "weights must skew the distribution");
        assert!(counts["rare"] > 0, "positive weights must appear over 200 traces");
    }

    #[test]
    fn evaluates_weighted_picks_with_dynamic_values() {
        let model = compile(r#"trace "t" { input = weighted([range(1, 3), 1], [10, 1]) }"#).unwrap();
        let plan = plan(model, 40, 7).unwrap();

        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_i64().unwrap();
            assert!((1..=3).contains(&input) || input == 10, "unexpected pick {input}");
        }
    }

    #[test]
    fn evaluates_distributions_within_their_supports() {
        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        normal = normal(100, 10)
                        lognormal = lognormal(300, 0.5)
                        exponential = exponential(250)
                        pareto = pareto(100, 1.5)
                        beta = beta(2, 5)
                        poisson = poisson(3)
                    }
                    input = chance(0.5)
                }
            "#,
        )
        .unwrap();
        let plan_a = plan(model, 50, 7).unwrap();

        for event in plan_a.events.iter() {
            let metrics = event.fields.metrics.as_ref().unwrap();
            assert!(metrics["lognormal"].as_f64().unwrap() > 0.0);
            assert!(metrics["exponential"].as_f64().unwrap() > 0.0);
            assert!(metrics["pareto"].as_f64().unwrap() >= 100.0);
            let beta = metrics["beta"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&beta));
            let poisson = metrics["poisson"].as_i64().unwrap();
            assert!(poisson >= 0, "poisson must be a non-negative integer");
            assert!(event.fields.input.as_ref().unwrap().is_boolean());
        }

        // samples vary across traces and reproduce by seed
        let normals: std::collections::HashSet<_> = plan_a
            .events
            .iter()
            .map(|event| event.fields.metrics.as_ref().unwrap()["normal"].to_string())
            .collect();
        assert!(normals.len() > 1, "expected varying samples over 50 traces");

        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        normal = normal(100, 10)
                        lognormal = lognormal(300, 0.5)
                        exponential = exponential(250)
                        pareto = pareto(100, 1.5)
                        beta = beta(2, 5)
                        poisson = poisson(3)
                    }
                    input = chance(0.5)
                }
            "#,
        )
        .unwrap();
        let plan_b = plan(model, 50, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_chance_bounds_deterministically() {
        let always = compile(r#"trace "t" { input = chance(1) }"#).unwrap();
        let plan_a = plan(always, 10, 7).unwrap();
        assert!(
            plan_a
                .events
                .iter()
                .all(|event| event.fields.input == Some(JsonValue::Bool(true)))
        );

        let never = compile(r#"trace "t" { input = chance(0) }"#).unwrap();
        let plan_b = plan(never, 10, 7).unwrap();
        assert!(
            plan_b
                .events
                .iter()
                .all(|event| event.fields.input == Some(JsonValue::Bool(false)))
        );
    }

    #[test]
    fn evaluates_string_funcs() {
        let model = compile(
            r#"
                vars { tags = ["alpha", "beta"] }
                trace "t" {
                    input = [
                        upper("get"),
                        lower("WARN"),
                        trim("  padded  "),
                        replace("a b c", " ", "-"),
                        replace("keep", "", "x"),
                        join(var.tags, ", "),
                        join([1, 2.5, true], "-"),
                        format("model={} n={}", "gpt", 4),
                    ]
                    output = split("a,b,c", ",")
                    metadata = {
                        sub = contains("gpt-4o-mini", "mini")
                        missing = contains("gpt-4o", "mini")
                        elem = contains(var.tags, "beta")
                        absent = contains(var.tags, "gamma")
                        prefix = starts_with("gpt-4o", "gpt")
                        suffix = ends_with("gpt-4o", "4o")
                        chars = len("hello")
                        elems = len(var.tags)
                    }
                }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let fields = &plan.events[0].fields;
        assert_eq!(
            fields.input,
            Some(serde_json::json!([
                "GET",
                "warn",
                "padded",
                "a-b-c",
                "keep",
                "alpha, beta",
                "1-2.5-true",
                "model=gpt n=4",
            ]))
        );
        assert_eq!(fields.output, Some(serde_json::json!(["a", "b", "c"])));
        assert_eq!(
            fields.metadata,
            Some(
                serde_json::json!({
                    "sub": true,
                    "missing": false,
                    "elem": true,
                    "absent": false,
                    "prefix": true,
                    "suffix": true,
                    "chars": 5,
                    "elems": 2,
                })
                .as_object()
                .unwrap()
                .clone()
            )
        );
    }

    #[test]
    fn evaluates_token_counts() {
        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        text = tokens("What is the weather in Tokyo?")
                        empty = tokens("")
                        messages = tokens([{ role = "user", content = "hi" }])
                    }
                }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let metrics = plan.events[0].fields.metrics.as_ref().unwrap();
        assert_eq!(metrics["text"], JsonValue::from(7));
        assert_eq!(metrics["empty"], JsonValue::from(0));
        // containers count their compact json serialization
        let serialized = r#"[{"content":"hi","role":"user"}]"#;
        assert_eq!(metrics["messages"], JsonValue::from(BPE.encode_ordinary(serialized).len()));
    }

    #[test]
    fn evaluates_string_funcs_over_dynamic_args() {
        let model = compile(r#"trace "t" { input = upper(choice("get", "post")) }"#).unwrap();
        let planned = plan(model, 20, 7).unwrap();

        let mut seen = std::collections::HashSet::new();
        for event in planned.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "GET" | "POST"), "unexpected value {input}");
            seen.insert(input.to_owned());
        }
        assert_eq!(seen.len(), 2, "expected both alternatives over 20 traces");

        // split results behave as arrays for indexing and lens
        let model =
            compile(r#"trace "t" { input = split("a,b,c", ",")[range(0, 2)] output = len(split("a,b", ",")) }"#).unwrap();
        let planned = plan(model, 10, 7).unwrap();
        for event in planned.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "a" | "b" | "c"));
            assert_eq!(event.fields.output, Some(JsonValue::from(2)));
        }
    }

    #[test]
    fn evaluates_format_with_dynamic_args() {
        let model = compile(r#"trace "t" { input = format("n={} f={} b={}", range(1, 1), 0.5, chance(1)) }"#).unwrap();
        let plan = plan(model, 1, 0).unwrap();

        assert_eq!(plan.events[0].fields.input, Some(JsonValue::from("n=1 f=0.5 b=true")));
    }

    #[test]
    fn evaluates_numeric_funcs() {
        let model = compile(
            r#"
                trace "t" {
                    metrics = {
                        low = clamp(1, 5, 10)
                        high = clamp(50, 5, 10)
                        inside = clamp(7, 5, 10)
                        promoted = clamp(7, 5.0, 10)
                        round = round(2.5)
                        down = floor(2.9)
                        up = ceil(2.1)
                        int = round(3)
                        abs = abs(0 - 4)
                        fabs = abs(0.0 - 4.5)
                        min = min(3, 1, 2)
                        max = max(3, 1, 2)
                        fmin = min(3, 1.5)
                    }
                }
            "#,
        )
        .unwrap();
        let plan = plan(model, 1, 0).unwrap();

        let metrics = plan.events[0].fields.metrics.as_ref().unwrap();
        assert_eq!(metrics["low"], JsonValue::from(5));
        assert_eq!(metrics["high"], JsonValue::from(10));
        assert_eq!(metrics["inside"], JsonValue::from(7));
        assert_eq!(metrics["promoted"], JsonValue::from(7.0));
        assert_eq!(metrics["round"], JsonValue::from(3));
        assert_eq!(metrics["down"], JsonValue::from(2));
        assert_eq!(metrics["up"], JsonValue::from(3));
        assert_eq!(metrics["int"], JsonValue::from(3));
        assert_eq!(metrics["abs"], JsonValue::from(4));
        assert_eq!(metrics["fabs"], JsonValue::from(4.5));
        assert_eq!(metrics["min"], JsonValue::from(1));
        assert_eq!(metrics["max"], JsonValue::from(3));
        assert_eq!(metrics["fmin"], JsonValue::from(1.5));
    }

    #[test]
    fn clamps_distribution_samples_within_bounds() {
        let model = compile(r#"trace "t" { metrics = { latency = clamp(lognormal(300, 1.5), 20, 1000) } }"#).unwrap();
        let plan = plan(model, 50, 7).unwrap();

        for event in plan.events.iter() {
            let latency = event.fields.metrics.as_ref().unwrap()["latency"].as_f64().unwrap();
            assert!((20.0..=1000.0).contains(&latency), "unexpected latency {latency}");
        }
    }

    #[test]
    fn fails_on_invalid_dynamic_func_values() {
        let source = r#"trace "t" { input = clamp(1, range(5, 5), 2) }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "clamp bounds are out of order");
        assert_eq!(&source[error.range.start..error.range.end], "clamp(1, range(5, 5), 2)");

        let source = r#"trace "t" { input = split("a,b", choice(",", "")) }"#;
        let error = plan(compile(source).unwrap(), 20, 7).unwrap_err();
        assert_eq!(error.to_string(), "split separator is empty");

        let source = r#"trace "t" { input = join([{}], ",") }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "join element is not a string, number, or boolean");

        let source = r#"trace "t" { input = round(99999999999999999999.0 * range(1.0, 1.0)) }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "expression result overflowed or is not finite");
    }

    #[test]
    fn evaluates_id_funcs_reproducibly() {
        let source = r#"trace "t" { input = [uuid(), hex(16), alphanum(12), hex(0)] }"#;
        let plan_a = plan(compile(source).unwrap(), 5, 7).unwrap();

        let mut uuids = std::collections::HashSet::new();
        for event in plan_a.events.iter() {
            let JsonValue::Array(values) = event.fields.input.as_ref().unwrap() else {
                panic!("expected an array");
            };

            let uuid = values[0].as_str().unwrap();
            assert_eq!(uuid.len(), 36);
            assert_eq!(uuid.as_bytes()[14], b'4', "expected a version-4 uuid");
            uuids.insert(uuid.to_owned());

            let hex = values[1].as_str().unwrap();
            assert_eq!(hex.len(), 16);
            assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

            let alphanum = values[2].as_str().unwrap();
            assert_eq!(alphanum.len(), 12);
            assert!(alphanum.chars().all(|c| c.is_ascii_alphanumeric()));

            assert_eq!(values[3].as_str().unwrap(), "");
        }
        assert_eq!(uuids.len(), 5, "each trace draws its own uuid");

        let plan_b = plan(compile(source).unwrap(), 5, 7).unwrap();
        assert_eq!(plan_a, plan_b);
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
        let source = r#"trace "t" { metrics = { n = range(1, 5) * 100, m = -range(1, 5) } }"#;
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
    fn evaluates_dynamic_indexes_within_their_containers() {
        let source = r#"
            vars { user = { a = 1, b = 2 } }
            trace "t" {
                input = ["a", "b", "c"][range(0, 2)]
                output = var.user[choice("a", "b")]
            }
        "#;
        let plan_a = plan(compile(source).unwrap(), 20, 7).unwrap();

        let mut inputs = std::collections::HashSet::new();
        for event in plan_a.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "a" | "b" | "c"), "unexpected element {input}");
            inputs.insert(input.to_owned());
            let output = event.fields.output.as_ref().unwrap().as_i64().unwrap();
            assert!(matches!(output, 1 | 2), "unexpected value {output}");
        }
        assert!(inputs.len() > 1, "expected multiple elements over 20 traces");

        // same seed, same selections
        let plan_b = plan(compile(source).unwrap(), 20, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_nested_dynamic_indexes() {
        let source = r#"
            vars { matrix = [[1, 2], [3, 4]] }
            trace "t" { input = var.matrix[range(0, 1)][range(0, 1)] }
        "#;
        let plan = plan(compile(source).unwrap(), 20, 7).unwrap();

        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_i64().unwrap();
            assert!((1..=4).contains(&input), "unexpected element {input}");
        }
    }

    #[test]
    fn evaluates_only_the_selected_element() {
        // the division by zero sits in an unselected element, so nothing fails
        let model = compile(r#"trace "t" { input = [0, 100 / range(0, 0)][range(0, 0)] }"#).unwrap();
        let planned = plan(model, 5, 7).unwrap();

        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(0)));
    }

    #[test]
    fn fails_on_invalid_dynamic_indexes() {
        let source = r#"trace "t" { input = [1][range(1, 1)] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "array index is out of bounds");
        assert_eq!(&source[error.range.start..error.range.end], "[1][range(1, 1)]");

        let source = r#"trace "t" { input = [1, 2][range(0.0, 0.0)] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "array index is not an integer");

        let source = r#"trace "t" { input = { a = 1 }[choice("b")] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "object key is not present");
    }

    #[test]
    fn evaluates_dynamic_slices_with_clamped_bounds() {
        let source = r#"
            trace "t" {
                input = [1, 2, 3, 4][range(1, 1):range(3, 3)]
                output = [1, 2][0:range(5, 5)]
                metadata = { empty = [1, 2][range(1, 1):0] }
            }
        "#;
        let plan_a = plan(compile(source).unwrap(), 3, 7).unwrap();

        for event in plan_a.events.iter() {
            assert_eq!(event.fields.input, Some(serde_json::json!([2, 3])));
            assert_eq!(event.fields.output, Some(serde_json::json!([1, 2])));
            assert_eq!(event.fields.metadata.as_ref().unwrap()["empty"], serde_json::json!([]));
        }

        // same seed, same selections
        let plan_b = plan(compile(source).unwrap(), 3, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn evaluates_indexes_into_sliced_targets() {
        let model = compile(r#"trace "t" { input = [1, 2, 3][range(1, 1):][0] }"#).unwrap();
        let planned = plan(model, 1, 0).unwrap();

        assert_eq!(planned.events[0].fields.input, Some(JsonValue::from(2)));
    }

    #[test]
    fn fails_on_invalid_dynamic_slice_bounds() {
        let source = r#"trace "t" { input = [1, 2][0 - range(1, 1):] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "slice bound is negative");
        assert_eq!(&source[error.range.start..error.range.end], "[1, 2][0 - range(1, 1):]");

        let source = r#"trace "t" { input = [1, 2][range(0.5, 0.5):] }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "slice bound is not an integer");
    }

    #[test]
    fn evaluates_unrolled_for_bodies_per_trace() {
        let model = compile(r#"trace "t" { input = [for x in [1, 2] : x * range(1, 3)] }"#).unwrap();
        let plan = plan(model, 20, 7).unwrap();

        for event in plan.events.iter() {
            let JsonValue::Array(input) = event.fields.input.as_ref().unwrap() else {
                panic!("expected an array");
            };
            assert!((1..=3).contains(&input[0].as_i64().unwrap()));
            assert!((2..=6).contains(&input[1].as_i64().unwrap()));
        }
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

    #[test]
    fn repeats_children_a_constant_number_of_times() {
        let model = compile(r#"trace "t" { repeat { count = 3 task "turn" { llm "reply" {} } } }"#).unwrap();
        let plan = plan(model, 2, 0).unwrap();

        // per trace: root + 3 * (task + llm)
        assert_eq!(plan.events.len(), 14);
        for range in plan.traces.iter() {
            let root = EventRef(range.start);
            let events = &plan.events[range.clone()];
            assert_eq!(events.len(), 7);
            assert_eq!(events.iter().filter(|event| event.kind == EventKind::Llm).count(), 3);
            assert!(events.iter().skip(1).all(|event| event.root == root));
        }
    }

    #[test]
    fn skips_repeats_with_a_zero_count() {
        let model = compile(r#"trace "t" { repeat { count = 0 task "turn" {} } }"#).unwrap();
        let plan = plan(model, 3, 0).unwrap();

        assert_eq!(plan.events.len(), 3);
    }

    #[test]
    fn varies_repeat_counts_per_trace_and_reproduces_them_by_seed() {
        let source = r#"trace "t" { repeat { count = range(1, 4) task "turn" {} } }"#;
        let plan_a = plan(compile(source).unwrap(), 20, 7).unwrap();

        let mut lengths = std::collections::HashSet::new();
        for range in plan_a.traces.iter() {
            let turns = range.len() - 1;
            assert!((1..=4).contains(&turns), "unexpected turn count {turns}");
            lengths.insert(turns);
        }
        assert!(lengths.len() > 1, "expected varying trace shapes over 20 traces");

        let plan_b = plan(compile(source).unwrap(), 20, 7).unwrap();
        assert_eq!(plan_a, plan_b);
    }

    #[test]
    fn resolves_repeat_index_per_iteration_with_the_innermost_repeat_winning() {
        let source = r#"
            trace "t" {
                repeat {
                    count = 2
                    task "outer" {
                        input = "o ${repeat.index}"
                        repeat {
                            count = 2
                            task "inner" { input = "i ${repeat.index}" }
                        }
                    }
                }
            }
        "#;
        let plan = plan(compile(source).unwrap(), 1, 0).unwrap();

        let inputs: Vec<_> = plan
            .events
            .iter()
            .filter_map(|event| event.fields.input.as_ref().and_then(|input| input.as_str()))
            .collect();
        assert_eq!(inputs, ["o 0", "i 0", "i 1", "o 1", "i 0", "i 1"]);
    }

    #[test]
    fn plans_exactly_one_choice_child() {
        let source = r#"trace "t" { choice { tool "a" {} function "b" {} } }"#;
        let plan = plan(compile(source).unwrap(), 20, 7).unwrap();

        let mut picked = std::collections::HashSet::new();
        for range in plan.traces.iter() {
            let events = &plan.events[range.clone()];
            assert_eq!(events.len(), 2, "choice must plan exactly one child");
            picked.insert(events[1].name.clone());
        }
        assert_eq!(picked.len(), 2, "expected both alternatives over 20 traces");
    }

    #[test]
    fn includes_maybe_children_probabilistically() {
        let source = r#"trace "t" { maybe { task "extra" {} } }"#;
        let plan_a = plan(compile(source).unwrap(), 40, 7).unwrap();

        let lengths: Vec<_> = plan_a.traces.iter().map(std::ops::Range::len).collect();
        assert!(
            lengths.contains(&1) && lengths.contains(&2),
            "expected both outcomes over 40 traces"
        );

        // the bounds always include or always skip
        let always = plan(
            compile(r#"trace "t" { maybe { chance = 1 task "extra" {} } }"#).unwrap(),
            10,
            7,
        )
        .unwrap();
        assert!(always.traces.iter().all(|range| range.len() == 2));
        let never = plan(
            compile(r#"trace "t" { maybe { chance = 0 task "extra" {} } }"#).unwrap(),
            10,
            7,
        )
        .unwrap();
        assert!(never.traces.iter().all(|range| range.len() == 1));
    }

    #[test]
    fn redraws_nested_dynamic_blocks_per_iteration() {
        let source = r#"
            trace "t" {
                repeat {
                    count = 8
                    choice { tool "a" {} function "b" {} }
                }
            }
        "#;
        let plan = plan(compile(source).unwrap(), 5, 7).unwrap();

        let names: std::collections::HashSet<_> = plan
            .events
            .iter()
            .filter(|event| event.parent.is_some())
            .map(|event| event.name.clone())
            .collect();
        assert_eq!(names.len(), 2, "expected the choice to redraw across iterations");
    }

    #[test]
    fn fails_on_invalid_dynamic_counts_and_chances() {
        let source = r#"trace "t" { repeat { count = 0 - range(2, 2) task "turn" {} } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "repeat count is negative");
        assert_eq!(&source[error.range.start..error.range.end], "0 - range(2, 2)");

        let source = r#"trace "t" { repeat { count = range(0.5, 0.5) task "turn" {} } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "repeat count is not an integer");

        let source = r#"trace "t" { maybe { chance = range(2.0, 2.0) task "turn" {} } }"#;
        let error = plan(compile(source).unwrap(), 1, 0).unwrap_err();
        assert_eq!(error.to_string(), "maybe chance is not between 0 and 1");
    }

    #[test]
    fn plans_the_dynamic_fixture() {
        let model = compile(include_str!("../../tests/fixtures/dynamic.bt")).unwrap();
        let plan_a = plan(model, 10, 42).unwrap();

        for range in plan_a.traces.iter() {
            let events = &plan_a.events[range.clone()];
            // root + 1..4 turns of (task + llm) + one choice pick + maybe an escalation
            assert!((4..=11).contains(&events.len()), "unexpected trace size {}", events.len());
            let picks = events
                .iter()
                .filter(|event| matches!(event.name.as_str(), "get_order_status" | "summarize_session"))
                .count();
            assert_eq!(picks, 1);
        }

        let model = compile(include_str!("../../tests/fixtures/dynamic.bt")).unwrap();
        let plan_b = plan(model, 10, 42).unwrap();
        assert_eq!(plan_a, plan_b);
    }
}
