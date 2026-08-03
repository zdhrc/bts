use crate::dsl::{
    Array as ModelArray, Model, Number as ModelNumber, Object as ModelObject, ObjectField as ModelObjectField, Span as ModelSpan, SpanFields as ModelSpanFields,
    SpanKind as ModelSpanKind, Trace as ModelTrace, Value as ModelValue,
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

impl Planner {
    fn plan_trace(&mut self, trace: ModelTrace) {
        let start = self.events.len();
        let root = EventRef(start);

        let ModelTrace { name, fields, children } = trace;

        self.events.push(EventPlan {
            root,
            parent: None,
            name,
            kind: EventKind::Task,
            fields: lower_fields(fields),
        });

        for child in children {
            self.plan_span(child, root, root);
        }

        self.traces.push(start..self.events.len());
    }

    fn plan_span(&mut self, span: ModelSpan, root: EventRef, parent: EventRef) {
        let event_ref = EventRef(self.events.len());

        let ModelSpan { name, kind, fields, children } = span;

        self.events.push(EventPlan {
            root,
            parent: Some(parent),
            name,
            kind: match kind {
                ModelSpanKind::Task => EventKind::Task,
                ModelSpanKind::Llm => EventKind::Llm,
            },
            fields: lower_fields(fields),
        });

        for child in children {
            self.plan_span(child, root, event_ref);
        }
    }
}

fn trace_len(trace: &ModelTrace) -> usize {
    1 + trace.children.iter().map(span_len).sum::<usize>()
}
fn span_len(span: &ModelSpan) -> usize {
    1 + span.children.iter().map(span_len).sum::<usize>()
}

fn lower_fields(fields: ModelSpanFields) -> EventFields {
    EventFields {
        input: fields.input.map(lower_value),
        output: fields.output.map(lower_value),
        metadata: fields.metadata.map(lower_object),
        metrics: fields.metrics.map(lower_object),
        tags: fields.tags.into_boxed_slice(),
    }
}

fn lower_value(value: ModelValue) -> JsonValue {
    match value {
        ModelValue::Str(value) => JsonValue::String(value),
        ModelValue::Bool(value) => JsonValue::Bool(value),

        ModelValue::Num(ModelNumber::Int(value)) => JsonValue::Number(value.into()),

        ModelValue::Num(ModelNumber::Float(value)) => {
            let number = JsonNumber::from_f64(value).expect("modeler guarantees finite floating-point numbers");
            JsonValue::Number(number)
        }

        ModelValue::Array(ModelArray { elem }) => JsonValue::Array(elem.into_iter().map(lower_value).collect()),

        ModelValue::Object(object) => JsonValue::Object(lower_object(object)),
    }
}

fn lower_object(object: ModelObject) -> JsonMap<String, JsonValue> {
    object.elem.into_iter().map(|ModelObjectField { key, value }| (key, lower_value(value))).collect()
}

pub(super) fn plan(model: Model) -> Plan {
    let capacity = model.traces.iter().map(trace_len).sum();

    let mut planner = Planner {
        events: Vec::with_capacity(capacity),
        traces: Vec::with_capacity(model.traces.len()),
    };

    for trace in model.traces {
        planner.plan_trace(trace);
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
        let plan = plan(model);

        println!("{plan:#?}");
    }
}
