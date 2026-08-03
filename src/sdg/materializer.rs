use crate::sdg::planner::{EventFields, EventRef, Plan};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error as Err;
use uuid::Uuid;

const EVENT_SLOT: Duration = Duration::from_millis(100);

#[derive(Debug, Serialize)]
pub(super) struct EventBatch {
    pub(super) events: Box<[Event]>,
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
    anchor: SystemTime,
}

impl Materializer {
    fn new(plan: Plan) -> Self {
        let span_ids = (0..plan.events.len()).map(|_| Uuid::new_v4().to_string()).collect();

        Self {
            plan,
            span_ids,
            anchor: SystemTime::now(),
        }
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
                let start = self.timestamp(event_ref, index)?;
                let end = self.timestamp(event_ref, last_descendants[index] + 1)?;
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
                    span_parents: event.parent.map(|parent| self.resolve(parent).to_owned()).into_iter().collect(),
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
        self.span_ids.get(event_ref.0).expect("planner guarantees that event references are in bounds")
    }

    fn timestamp(&self, event: EventRef, slot: usize) -> Result<SystemTime, Error> {
        let slot = u32::try_from(slot).map_err(|_| Error::new(ErrorKind::TimestampOutOfRange, event))?;
        let offset = EVENT_SLOT.checked_mul(slot).ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, event))?;

        self.anchor.checked_add(offset).ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, event))
    }

    fn insert_timestamp(&self, event: EventRef, metrics: &mut JsonMap<String, JsonValue>, key: &'static str, timestamp: SystemTime) -> Result<(), Error> {
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

pub(super) fn materialize(plan: Plan) -> Result<EventBatch, Error> {
    Materializer::new(plan).materialize()
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
#[error("{kind}")]
pub(super) struct Error {
    kind: ErrorKind,
    event: EventRef,
}

#[derive(Debug, Clone, Copy, Err, Eq, PartialEq)]
pub(super) enum ErrorKind {
    #[error("metric `{0}` is reserved")]
    ReservedMetric(&'static str),

    #[error("timestamp is out of range")]
    TimestampOutOfRange,
}

impl Error {
    fn new(kind: ErrorKind, event: EventRef) -> Self {
        Self { kind, event }
    }

    fn kind(&self) -> ErrorKind {
        self.kind
    }

    fn event(&self) -> EventRef {
        self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;
    use crate::sdg::planner::plan;

    #[test]
    fn materializes_fixture() {
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();
        let events = materialize(plan(model)).unwrap();

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
        assert_eq!(first_llm.output, Some(JsonValue::String("Hello!".to_owned())));
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
    fn fails_on_first_reserved_metric() {
        let model = compile(r#"trace "example" { metrics = { start = 1 end = 2 } }"#).unwrap();
        let error = materialize(plan(model)).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::ReservedMetric("start"));
        assert_eq!(error.event(), EventRef(0));
    }
}
