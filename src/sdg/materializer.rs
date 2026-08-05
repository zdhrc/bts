use crate::sdg::planner::{EventFields, EventRef, Plan};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error as Err;
use uuid::Uuid;

const EVENT_SLOT: Duration = Duration::from_millis(100);

#[derive(Debug, Serialize)]
pub(crate) struct EventBatch {
    pub(super) events: Box<[Event]>,

    #[serde(skip)]
    trace_count: usize,
}

impl EventBatch {
    pub(crate) fn event_count(&self) -> usize {
        self.events.len()
    }

    pub(crate) fn trace_count(&self) -> usize {
        self.trace_count
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Event {
    pub(super) id: String,
    pub(super) span_id: String,
    pub(super) root_span_id: String,
    pub(super) span_parents: Box<[String]>,
    pub(super) created: String,
    pub(super) span_attributes: SpanAttributes,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) input: Option<JsonValue>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) output: Option<JsonValue>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) metadata: Option<JsonMap<String, JsonValue>>,

    pub(super) metrics: JsonMap<String, JsonValue>,

    #[serde(skip_serializing_if = "tags_are_empty")]
    pub(super) tags: Box<[String]>,
}

fn tags_are_empty(tags: &[String]) -> bool {
    tags.is_empty()
}

#[derive(Debug, Serialize)]
pub(super) struct SpanAttributes {
    pub(super) name: String,

    #[serde(rename = "type")]
    pub(super) kind: String,
}

struct Materializer {
    plan: Plan,
    span_ids: Box<[String]>,
    trace_starts: Box<[usize]>,
    anchors: Box<[SystemTime]>,
}

impl Materializer {
    // anchors spread traces across the past window, leaving room for the longest trace to finish by now
    fn new(plan: Plan, over: Duration, now: SystemTime) -> Result<Self, Error> {
        let span_ids = (0..plan.events.len()).map(|_| Uuid::new_v4().to_string()).collect();
        let max_slots = plan.traces.iter().map(|trace| trace.len()).max().unwrap_or_default();
        let max_slots = u32::try_from(max_slots).map_err(|_| Error::new(ErrorKind::TimestampOutOfRange, EventRef(0)))?;
        let max_trace_duration = EVENT_SLOT
            .checked_mul(max_slots)
            .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, EventRef(0)))?;
        let available = over
            .checked_sub(max_trace_duration)
            .ok_or_else(|| Error::new(ErrorKind::WindowTooShort, EventRef(0)))?;
        let window_start = now
            .checked_sub(over)
            .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, EventRef(0)))?;
        let last_index = plan.traces.len().saturating_sub(1);
        let mut trace_starts = Vec::with_capacity(plan.events.len());
        let mut anchors = Vec::with_capacity(plan.events.len());

        for (index, trace) in plan.traces.iter().enumerate() {
            let ratio = if last_index == 0 {
                1.0
            } else {
                index as f64 / last_index as f64
            };
            let anchor = window_start
                .checked_add(available.mul_f64(ratio))
                .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, EventRef(trace.start)))?;

            trace_starts.extend(std::iter::repeat_n(trace.start, trace.len()));
            anchors.extend(std::iter::repeat_n(anchor, trace.len()));
        }

        Ok(Self {
            plan,
            span_ids,
            trace_starts: trace_starts.into_boxed_slice(),
            anchors: anchors.into_boxed_slice(),
        })
    }

    fn materialize(mut self) -> Result<EventBatch, Error> {
        let last_descendants = self.last_descendants();
        let event_plans = std::mem::take(&mut self.plan.events);
        let events = event_plans
            .into_vec()
            .into_iter()
            .enumerate()
            .map(|(index, event)| {
                let event_ref = EventRef(index);
                let trace_start = self.trace_starts[index];
                let anchor = self.anchors[index];
                let start = self.timestamp(event_ref, anchor, index - trace_start)?;
                let end = self.timestamp(event_ref, anchor, last_descendants[index] - trace_start + 1)?;
                let EventFields {
                    input,
                    output,
                    metadata,
                    metrics,
                    tags,
                } = event.fields;
                let mut metrics = metrics.unwrap_or_default();

                self.insert_timestamp(event_ref, &mut metrics, "start", start)?;
                self.insert_timestamp(event_ref, &mut metrics, "end", end)?;

                Ok(Event {
                    id: Uuid::new_v4().to_string(),
                    span_id: self.resolve(event_ref).to_owned(),
                    root_span_id: self.resolve(event.root).to_owned(),
                    span_parents: event
                        .parent
                        .map(|parent| self.resolve(parent).to_owned())
                        .into_iter()
                        .collect(),
                    created: format_timestamp(start),
                    span_attributes: SpanAttributes {
                        name: event.name,
                        kind: event.kind.as_str().to_owned(),
                    },
                    input,
                    output,
                    metadata,
                    metrics,
                    tags,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok(EventBatch {
            events: events.into_boxed_slice(),
            trace_count: self.plan.traces.len(),
        })
    }

    fn last_descendants(&self) -> Box<[usize]> {
        let mut last_descendants = (0..self.plan.events.len()).collect::<Vec<_>>();

        for index in (0..self.plan.events.len()).rev() {
            if let Some(parent) = self.plan.events[index].parent {
                last_descendants[parent.0] = last_descendants[parent.0].max(last_descendants[index]);
            }
        }

        last_descendants.into_boxed_slice()
    }

    fn resolve(&self, event_ref: EventRef) -> &str {
        self.span_ids
            .get(event_ref.0)
            .expect("planner guarantees that event references are in bounds")
    }

    fn timestamp(&self, event: EventRef, anchor: SystemTime, slot: usize) -> Result<SystemTime, Error> {
        let slot = u32::try_from(slot).map_err(|_| Error::new(ErrorKind::TimestampOutOfRange, event))?;
        let offset = EVENT_SLOT
            .checked_mul(slot)
            .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, event))?;

        anchor
            .checked_add(offset)
            .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, event))
    }

    fn insert_timestamp(
        &self,
        event: EventRef,
        metrics: &mut JsonMap<String, JsonValue>,
        key: &'static str,
        timestamp: SystemTime,
    ) -> Result<(), Error> {
        if metrics.contains_key(key) {
            return Err(Error::new(ErrorKind::ReservedMetric(key), event));
        }

        let seconds = timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::new(ErrorKind::TimestampOutOfRange, event))?
            .as_secs_f64();

        metrics.insert(key.to_owned(), JsonValue::from(seconds));
        Ok(())
    }
}

fn format_timestamp(timestamp: SystemTime) -> String {
    DateTime::<Utc>::from(timestamp).to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(super) fn materialize(plan: Plan, over: Duration, now: SystemTime) -> Result<EventBatch, Error> {
    Materializer::new(plan, over, now)?.materialize()
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
#[error("{kind}")]
pub(crate) struct Error {
    kind: ErrorKind,
    event: EventRef,
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
pub(super) enum ErrorKind {
    #[error("metric `{0}` is reserved")]
    ReservedMetric(&'static str),

    #[error("generation window is shorter than the longest trace")]
    WindowTooShort,

    #[error("timestamp is out of range")]
    TimestampOutOfRange,
}

impl Error {
    fn new(kind: ErrorKind, event: EventRef) -> Self {
        Self { kind, event }
    }

    #[cfg(test)]
    fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[cfg(test)]
    fn event(&self) -> EventRef {
        self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;
    use crate::sdg::planner::{EventKind, EventPlan, plan};

    #[test]
    fn materializes_fixture() {
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(7_200);
        let events = materialize(plan(model, 1), Duration::from_secs(3_600), now).unwrap();

        println!("{}", serde_json::to_string_pretty(&events).unwrap());

        assert_eq!(events.events.len(), 5);

        let trace = &events.events[0];
        let first_turn = &events.events[1];
        let first_llm = &events.events[2];
        let second_turn = &events.events[3];
        let second_llm = &events.events[4];

        assert_eq!(trace.root_span_id, trace.span_id);
        assert!(trace.span_parents.is_empty());
        assert_eq!(first_turn.root_span_id, trace.span_id);
        assert_eq!(first_turn.span_parents.as_ref(), std::slice::from_ref(&trace.span_id));
        assert_eq!(first_llm.span_parents.as_ref(), std::slice::from_ref(&first_turn.span_id));
        assert_eq!(second_turn.span_parents.as_ref(), std::slice::from_ref(&trace.span_id));
        assert_eq!(second_llm.span_parents.as_ref(), std::slice::from_ref(&second_turn.span_id));

        assert_eq!(first_llm.span_attributes.name, "gpt-4o-mini");
        assert_eq!(first_llm.span_attributes.kind, "llm");
        assert_eq!(first_llm.input, Some(JsonValue::String("Hey".to_owned())));
        assert_eq!(first_llm.output, Some(JsonValue::String("Hello! I'm Eugene.".to_owned())));
        assert_eq!(first_llm.metadata.as_ref().unwrap()["model"], "gpt-4o-mini");
        assert_eq!(second_llm.metadata.as_ref().unwrap()["temperature"], 0.2);
        assert_eq!(first_llm.metrics["tokens"], 4);

        for event in &events.events {
            assert_eq!(Uuid::parse_str(&event.id).unwrap().get_version_num(), 4);
            assert_eq!(Uuid::parse_str(&event.span_id).unwrap().get_version_num(), 4);
            assert!(event.metrics["start"].as_f64().unwrap() < event.metrics["end"].as_f64().unwrap());
        }

        assert!(trace.metrics["start"].as_f64().unwrap() < first_turn.metrics["start"].as_f64().unwrap());
        assert!(trace.metrics["end"].as_f64().unwrap() >= second_llm.metrics["end"].as_f64().unwrap());

        let json = serde_json::to_value(events).unwrap();
        assert!(json["events"][1].get("input").is_none());
        assert_eq!(json["events"][0]["tags"], serde_json::json!(["chat", "prod"]));
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // one trace spanning events 0..1 is the intent
    fn fails_on_reserved_metrics_that_bypass_compilation() {
        // modeler rejects reserved metric keys so a plan carrying one can only be built by hand
        let mut metrics = JsonMap::new();
        metrics.insert("start".to_owned(), JsonValue::from(1));
        let plan = Plan {
            events: Box::new([EventPlan {
                root: EventRef(0),
                parent: None,
                name: "example".to_owned(),
                kind: EventKind::Task,
                fields: EventFields {
                    input: None,
                    output: None,
                    metadata: None,
                    metrics: Some(metrics),
                    tags: Box::new([]),
                },
            }]),
            traces: Box::new([0..1]),
        };
        let error = materialize(plan, Duration::from_secs(3_600), SystemTime::now()).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::ReservedMetric("start"));
        assert_eq!(error.event(), EventRef(0));
    }

    #[test]
    fn spreads_generated_traces_across_the_window() {
        let model = compile(r#"trace "example" {}"#).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(7_200);
        let events = materialize(plan(model, 3), Duration::from_secs(3_600), now).unwrap();
        let starts = events
            .events
            .iter()
            .map(|event| event.metrics["start"].as_f64().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(events.trace_count(), 3);
        assert_eq!(starts[0], 3_600.0);
        assert!(starts[1] > starts[0]);
        assert!(starts[2] > starts[1]);
        assert_eq!(events.events[2].metrics["end"].as_f64().unwrap(), 7_200.0);
    }
}
