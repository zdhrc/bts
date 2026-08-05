use crate::dsl::{
    Array as ModelArray, CtxRef as ModelCtxRef, Func as ModelFunc, Model, Number as ModelNumber, Object as ModelObject,
    ObjectField as ModelObjectField, Part as ModelPart, Range as ModelRange, Span as ModelSpan, SpanFields as ModelSpanFields,
    SpanKind as ModelSpanKind, Template as ModelTemplate, Trace as ModelTrace, Value as ModelValue,
};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use std::ops::Range;

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
    fn plan_trace(&mut self, trace: ModelTrace, ctx: &mut Ctx) {
        let start = self.events.len();
        let root = EventRef(start);

        let ModelTrace { name, fields, children } = trace;

        self.events.push(EventPlan {
            root,
            parent: None,
            name,
            kind: EventKind::Task,
            fields: lower_fields(fields, ctx),
        });

        for child in children {
            self.plan_span(child, root, root, ctx);
        }

        self.traces.push(start..self.events.len());
    }

    fn plan_span(&mut self, span: ModelSpan, root: EventRef, parent: EventRef, ctx: &mut Ctx) {
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
            fields: lower_fields(fields, ctx),
        });

        for child in children {
            self.plan_span(child, root, event_ref, ctx);
        }
    }
}

fn trace_len(trace: &ModelTrace) -> usize {
    1 + trace.children.iter().map(span_len).sum::<usize>()
}
fn span_len(span: &ModelSpan) -> usize {
    1 + span.children.iter().map(span_len).sum::<usize>()
}

fn lower_fields(fields: ModelSpanFields, ctx: &mut Ctx) -> EventFields {
    EventFields {
        input: fields.input.map(|value| lower_value(value, ctx)),
        output: fields.output.map(|value| lower_value(value, ctx)),
        metadata: fields.metadata.map(|object| lower_object(object, ctx)),
        metrics: fields.metrics.map(|object| lower_object(object, ctx)),
        tags: fields.tags.into_iter().map(|tag| resolve_template(tag, ctx)).collect(),
    }
}

fn lower_value(value: ModelValue, ctx: &mut Ctx) -> JsonValue {
    match value {
        ModelValue::Str(value) => JsonValue::String(value),
        ModelValue::Template(template) => JsonValue::String(resolve_template(template, ctx)),
        ModelValue::Bool(value) => JsonValue::Bool(value),
        ModelValue::Null => JsonValue::Null,

        ModelValue::Num(ModelNumber::Int(value)) => JsonValue::Number(value.into()),

        ModelValue::Num(ModelNumber::Float(value)) => {
            let number = JsonNumber::from_f64(value).expect("modeler guarantees finite floating-point numbers");
            JsonValue::Number(number)
        }

        ModelValue::Array(ModelArray { elem }) => {
            JsonValue::Array(elem.into_iter().map(|value| lower_value(value, ctx)).collect())
        }

        ModelValue::Object(object) => JsonValue::Object(lower_object(object, ctx)),

        ModelValue::Func(func) => eval_func(func, ctx),
    }
}

fn eval_func(func: ModelFunc, ctx: &mut Ctx) -> JsonValue {
    match func {
        ModelFunc::Choice(options) => {
            let pick = ctx.rng.random_range(0..options.len());
            let option = options
                .into_iter()
                .nth(pick)
                .expect("modeler guarantees choice has an alternative");

            lower_value(option, ctx)
        }

        ModelFunc::Range(ModelRange::Int { min, max }) => JsonValue::Number(ctx.rng.random_range(min..=max).into()),

        ModelFunc::Range(ModelRange::Float { min, max }) => {
            let number = JsonNumber::from_f64(ctx.rng.random_range(min..=max)).expect("modeler guarantees finite bounds");
            JsonValue::Number(number)
        }
    }
}

fn lower_object(object: ModelObject, ctx: &mut Ctx) -> JsonMap<String, JsonValue> {
    object
        .elem
        .into_iter()
        .map(|ModelObjectField { key, value }| (key, lower_value(value, ctx)))
        .collect()
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
pub(super) fn plan(model: Model, count: usize, seed: u64) -> Plan {
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
        planner.plan_trace(model.traces[index % model.traces.len()].clone(), &mut ctx);
    }

    Plan {
        events: planner.events.into_boxed_slice(),
        traces: planner.traces.into_boxed_slice(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;

    #[test]
    fn prints_plan() {
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();
        let plan = plan(model, 1, 0);

        println!("{plan:#?}");
    }

    #[test]
    fn lowers_null_and_negative_numbers_to_json() {
        let model = compile(r#"trace "t" { input = null metrics = { delta = -0.5 } }"#).unwrap();
        let plan = plan(model, 1, 0);

        let fields = &plan.events[0].fields;
        assert_eq!(fields.input, Some(JsonValue::Null));
        assert_eq!(fields.metrics.as_ref().unwrap()["delta"], JsonValue::from(-0.5));
    }

    #[test]
    fn resolves_trace_index_per_generated_trace() {
        let model = compile(r#"trace "t" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let plan = plan(model, 3, 0);

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
        let plan = plan(model, 5, 0);

        assert_eq!(plan.traces.len(), 5);
        assert_eq!(
            plan.events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
            ["first", "second", "first", "second", "first"]
        );
    }

    #[test]
    fn evaluates_choice_within_its_alternatives() {
        let model = compile(r#"trace "t" { input = choice("a", "b") }"#).unwrap();
        let plan = plan(model, 20, 7);

        for event in plan.events.iter() {
            let input = event.fields.input.as_ref().unwrap().as_str().unwrap();
            assert!(matches!(input, "a" | "b"), "unexpected choice {input}");
        }
    }

    #[test]
    fn evaluates_ranges_within_bounds_and_keeps_integer_bounds_integral() {
        let model = compile(r#"trace "t" { metrics = { tokens = range(80, 400) temp = range(0.0, 1.0) } }"#).unwrap();
        let plan = plan(model, 20, 7);

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
        let plan = plan(model, 20, 7);

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
        let first = plan(compile(source).unwrap(), 10, 42);
        let second = plan(compile(source).unwrap(), 10, 42);

        assert_eq!(first, second);
    }

    #[test]
    fn samples_traces_independently_of_earlier_traces() {
        // trace 3 of a 5-trace run must match trace 3 of a 4-trace run
        let source = r#"trace "t" { metrics = { n = range(0, 1000000) } }"#;
        let longer = plan(compile(source).unwrap(), 5, 42);
        let shorter = plan(compile(source).unwrap(), 4, 42);

        assert_eq!(longer.events[3], shorter.events[3]);
    }
}
