use crate::dsl::{
    Array as ModelArray, CtxRef as ModelCtxRef, Model, Number as ModelNumber, Object as ModelObject,
    ObjectField as ModelObjectField, Part as ModelPart, Span as ModelSpan, SpanFields as ModelSpanFields,
    SpanKind as ModelSpanKind, Template as ModelTemplate, Trace as ModelTrace, Value as ModelValue,
};
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

// per-trace generation context that templates resolve against
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Ctx {
    trace_index: usize,
}

impl Planner {
    fn plan_trace(&mut self, trace: ModelTrace, ctx: Ctx) {
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

    fn plan_span(&mut self, span: ModelSpan, root: EventRef, parent: EventRef, ctx: Ctx) {
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

fn lower_fields(fields: ModelSpanFields, ctx: Ctx) -> EventFields {
    EventFields {
        input: fields.input.map(|value| lower_value(value, ctx)),
        output: fields.output.map(|value| lower_value(value, ctx)),
        metadata: fields.metadata.map(|object| lower_object(object, ctx)),
        metrics: fields.metrics.map(|object| lower_object(object, ctx)),
        tags: fields.tags.into_iter().map(|tag| resolve_template(tag, ctx)).collect(),
    }
}

fn lower_value(value: ModelValue, ctx: Ctx) -> JsonValue {
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
    }
}

fn lower_object(object: ModelObject, ctx: Ctx) -> JsonMap<String, JsonValue> {
    object
        .elem
        .into_iter()
        .map(|ModelObjectField { key, value }| (key, lower_value(value, ctx)))
        .collect()
}

fn resolve_template(template: ModelTemplate, ctx: Ctx) -> String {
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
pub(super) fn plan(model: Model, count: usize) -> Plan {
    debug_assert!(!model.traces.is_empty());

    let capacity = (0..count)
        .map(|index| trace_len(&model.traces[index % model.traces.len()]))
        .sum();

    let mut planner = Planner {
        events: Vec::with_capacity(capacity),
        traces: Vec::with_capacity(count),
    };

    for index in 0..count {
        planner.plan_trace(model.traces[index % model.traces.len()].clone(), Ctx { trace_index: index });
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
        let plan = plan(model, 1);

        println!("{plan:#?}");
    }

    #[test]
    fn lowers_null_and_negative_numbers_to_json() {
        let model = compile(r#"trace "t" { input = null metrics = { delta = -0.5 } }"#).unwrap();
        let plan = plan(model, 1);

        let fields = &plan.events[0].fields;
        assert_eq!(fields.input, Some(JsonValue::Null));
        assert_eq!(fields.metrics.as_ref().unwrap()["delta"], JsonValue::from(-0.5));
    }

    #[test]
    fn resolves_trace_index_per_generated_trace() {
        let model = compile(r#"trace "t" { input = "q ${trace.index}" tags = ["t-${trace.index}"] }"#).unwrap();
        let plan = plan(model, 3);

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
        let plan = plan(model, 5);

        assert_eq!(plan.traces.len(), 5);
        assert_eq!(
            plan.events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
            ["first", "second", "first", "second", "first"]
        );
    }
}
