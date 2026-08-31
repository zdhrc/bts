use crate::sdg::planner::{EventFields, EventRef, Plan};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const EVENT_SLOT: Duration = Duration::from_millis(100);

// how trace volume spreads across the window; each variant maps an even 0..=1
// ratio through its inverse cdf so placement stays deterministic
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, clap::ValueEnum)]
pub(crate) enum Distribution {
    #[default]
    Linear,
    Sine,
}

impl Distribution {
    fn position(self, ratio: f64) -> f64 {
        match self {
            Self::Linear => ratio,
            // density is a half sine wave peaking mid-window: f(t) = (pi/2)sin(pi t),
            // cdf F(t) = (1 - cos(pi t))/2, inverted here
            Self::Sine => (1.0 - 2.0 * ratio).clamp(-1.0, 1.0).acos() / std::f64::consts::PI,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct EventBatch {
    pub(super) events: Box<[Event]>,

    #[serde(skip)]
    pub(super) trace_count: usize,
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
    pub(super) expected: Option<JsonValue>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<JsonValue>,

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
    // per-event offsets from the trace anchor, from the duration layout
    starts: Box<[Duration]>,
    ends: Box<[Duration]>,
    anchors: Box<[SystemTime]>,
}

impl Materializer {
    // anchors spread traces across the past window, leaving room for the longest trace to finish by now
    fn new(plan: Plan, over: Duration, distribution: Distribution, now: SystemTime) -> Result<Self, Error> {
        let span_ids = (0..plan.events.len()).map(|_| Uuid::new_v4().to_string()).collect();
        let last_descendants = last_descendants(&plan.events);
        let mut starts = vec![Duration::ZERO; plan.events.len()];
        let mut ends = vec![Duration::ZERO; plan.events.len()];
        for trace in plan.traces.iter() {
            layout(
                &plan.events,
                &last_descendants,
                trace.start,
                Duration::ZERO,
                &mut starts,
                &mut ends,
            )?;
        }

        // the root encloses its whole trace, so its end is the trace's extent
        let max_extent = plan.traces.iter().map(|trace| ends[trace.start]).max().unwrap_or_default();
        let available = over
            .checked_sub(max_extent)
            .ok_or_else(|| Error::new(ErrorKind::WindowTooShort, EventRef(0)))?;
        let window_start = now
            .checked_sub(over)
            .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, EventRef(0)))?;
        let last_index = plan.traces.len().saturating_sub(1);
        let mut anchors = Vec::with_capacity(plan.events.len());

        for (index, trace) in plan.traces.iter().enumerate() {
            let ratio = if last_index == 0 {
                1.0
            } else {
                index as f64 / last_index as f64
            };
            let anchor = window_start
                .checked_add(available.mul_f64(distribution.position(ratio)))
                .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, EventRef(trace.start)))?;

            anchors.extend(std::iter::repeat_n(anchor, trace.len()));
        }

        Ok(Self {
            plan,
            span_ids,
            starts: starts.into_boxed_slice(),
            ends: ends.into_boxed_slice(),
            anchors: anchors.into_boxed_slice(),
        })
    }

    fn materialize(mut self) -> Result<EventBatch, Error> {
        let event_plans = std::mem::take(&mut self.plan.events);
        let events = event_plans
            .into_vec()
            .into_iter()
            .enumerate()
            .map(|(index, event)| {
                let event_ref = EventRef(index);
                let anchor = self.anchors[index];
                let start = self.timestamp(event_ref, anchor, self.starts[index])?;
                let end = self.timestamp(event_ref, anchor, self.ends[index])?;
                let EventFields {
                    input,
                    output,
                    expected,
                    error,
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
                    expected,
                    error,
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

    fn resolve(&self, event_ref: EventRef) -> &str {
        self.span_ids
            .get(event_ref.0)
            .expect("planner guarantees that event references are in bounds")
    }

    fn timestamp(&self, event: EventRef, anchor: SystemTime, offset: Duration) -> Result<SystemTime, Error> {
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

fn last_descendants(events: &[crate::sdg::planner::EventPlan]) -> Box<[usize]> {
    let mut last_descendants = (0..events.len()).collect::<Vec<_>>();

    for index in (0..events.len()).rev() {
        if let Some(parent) = events[index].parent {
            last_descendants[parent.0] = last_descendants[parent.0].max(last_descendants[index]);
        }
    }

    last_descendants.into_boxed_slice()
}

// lays a subtree out from its offset within the trace: children run
// sequentially after a fixed lead, and a span ends at the later of its own
// duration or its last child's end; events are pre-order, so the direct
// children of `index` chain through their last descendants
fn layout(
    events: &[crate::sdg::planner::EventPlan],
    last_descendants: &[usize],
    index: usize,
    offset: Duration,
    starts: &mut [Duration],
    ends: &mut [Duration],
) -> Result<(), Error> {
    let overflow = || Error::new(ErrorKind::TimestampOutOfRange, EventRef(index));

    starts[index] = offset;
    let mut cursor = offset.checked_add(EVENT_SLOT).ok_or_else(overflow)?;
    let mut tail = offset;
    let mut child = index + 1;
    while child <= last_descendants[index] {
        layout(events, last_descendants, child, cursor, starts, ends)?;
        cursor = ends[child];
        tail = ends[child];
        child = last_descendants[child] + 1;
    }

    let own = events[index].duration.unwrap_or(EVENT_SLOT);
    ends[index] = tail.max(offset.checked_add(own).ok_or_else(overflow)?);
    Ok(())
}

fn format_timestamp(timestamp: SystemTime) -> String {
    DateTime::<Utc>::from(timestamp).to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(super) fn materialize(
    plan: Plan,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
) -> Result<EventBatch, Error> {
    Materializer::new(plan, over, distribution, now)?.materialize()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct Error {
    kind: ErrorKind,
    event: EventRef,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum ErrorKind {
    ReservedMetric(&'static str),
    WindowTooShort,
    TimestampOutOfRange,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReservedMetric(name) => write!(formatter, "metric `{name}` is reserved"),
            Self::WindowTooShort => formatter.write_str("generation window is shorter than the longest trace"),
            Self::TimestampOutOfRange => formatter.write_str("timestamp is out of range"),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(formatter)
    }
}

impl std::error::Error for Error {}

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
        let events = materialize(
            plan(model, 1, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            now,
        )
        .unwrap();

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

        assert_eq!(first_llm.span_attributes.name, "Chat Completion");
        assert_eq!(first_llm.span_attributes.kind, "llm");
        let messages = first_llm.input.as_ref().unwrap().as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(
            messages[0]["content"],
            "You are a concise personal finance assistant.\nAnswer briefly."
        );
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "How did NVDA do today?");
        assert_eq!(
            first_llm.output,
            Some(serde_json::json!({ "role": "assistant", "content": "NVDA closed down 1.4% at $207.40." }))
        );
        assert_eq!(first_llm.metadata.as_ref().unwrap()["model"], "gpt-4o-mini");
        assert_eq!(second_llm.metadata.as_ref().unwrap()["temperature"], 0.2);
        assert_eq!(first_llm.metrics["prompt_tokens"], 612);
        assert_eq!(first_llm.metrics["completion_tokens"], 24);
        assert_eq!(first_llm.metrics["tokens"], 636);

        for event in &events.events {
            assert_eq!(Uuid::parse_str(&event.id).unwrap().get_version_num(), 4);
            assert_eq!(Uuid::parse_str(&event.span_id).unwrap().get_version_num(), 4);
            assert!(event.metrics["start"].as_f64().unwrap() < event.metrics["end"].as_f64().unwrap());
        }

        assert!(trace.metrics["start"].as_f64().unwrap() < first_turn.metrics["start"].as_f64().unwrap());
        assert!(trace.metrics["end"].as_f64().unwrap() >= second_llm.metrics["end"].as_f64().unwrap());

        let json = serde_json::to_value(events).unwrap();
        assert_eq!(json["events"][1]["input"], "How did NVDA do today?");
        assert_eq!(json["events"][0]["tags"], serde_json::json!(["chat", "prod"]));
    }

    #[test]
    fn serializes_expected_and_error_fields_only_when_set() {
        let source = r#"
            trace "example" {
                expected = "4"
                error = { message = "timeout" }
                task "step" {}
            }
        "#;
        let model = compile(source).unwrap();
        let events = materialize(
            plan(model, 1, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            SystemTime::now(),
        )
        .unwrap();

        let json = serde_json::to_value(events).unwrap();
        assert_eq!(json["events"][0]["expected"], "4");
        assert_eq!(json["events"][0]["error"]["message"], "timeout");
        assert!(json["events"][1].get("expected").is_none());
        assert!(json["events"][1].get("error").is_none());
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
                    expected: None,
                    error: None,
                    metadata: None,
                    metrics: Some(metrics),
                    tags: Box::new([]),
                },
                duration: None,
            }]),
            traces: Box::new([0..1]),
        };
        let error = materialize(plan, Duration::from_secs(3_600), Distribution::Linear, SystemTime::now()).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::ReservedMetric("start"));
        assert_eq!(error.event(), EventRef(0));
    }

    #[test]
    fn lays_spans_out_by_their_durations() {
        let source = r#"
            trace "t" {
                duration = 60
                task "a" { duration = 2 }
                task "b" {
                    llm "c" { duration = 1.5 }
                }
            }
        "#;
        let now = UNIX_EPOCH + Duration::from_secs(7_200);
        let events = materialize(
            plan(compile(source).unwrap(), 1, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            now,
        )
        .unwrap();

        let time = |index: usize, key: &str| events.events[index].metrics[key].as_f64().unwrap();
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;

        // leaves span exactly their durations
        assert!(close(time(1, "end") - time(1, "start"), 2.0));
        assert!(close(time(3, "end") - time(3, "start"), 1.5));
        // a sibling starts where the previous one ends
        assert!(close(time(2, "start"), time(1, "end")));
        // a parent outlived by its children ends with its last child
        assert!(close(time(2, "end"), time(3, "end")));
        // an explicit total longer than the children stretches the span
        assert!(close(time(0, "end") - time(0, "start"), 60.0));
        // the single trace anchors so its computed extent ends at now
        assert_eq!(time(0, "end"), 7_200.0);
    }

    #[test]
    fn rejects_windows_shorter_than_the_longest_trace() {
        let source = r#"trace "t" { duration = 7200 }"#;
        let error = materialize(
            plan(compile(source).unwrap(), 1, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            SystemTime::now(),
        )
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::WindowTooShort);
    }

    #[test]
    fn spreads_generated_traces_across_the_window() {
        let model = compile(r#"trace "example" {}"#).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(7_200);
        let events = materialize(
            plan(model, 3, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            now,
        )
        .unwrap();
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

    #[test]
    fn sine_distribution_clusters_traces_mid_window() {
        let model = compile(r#"trace "example" {}"#).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(7_200);
        let events = materialize(
            plan(model, 5, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Sine,
            now,
        )
        .unwrap();
        let starts = events
            .events
            .iter()
            .map(|event| event.metrics["start"].as_f64().unwrap())
            .collect::<Vec<_>>();
        let gaps = starts.windows(2).map(|pair| pair[1] - pair[0]).collect::<Vec<_>>();

        // endpoints match the linear spread, density peaks mid-window
        assert_eq!(starts[0], 3_600.0);
        assert_eq!(events.events[4].metrics["end"].as_f64().unwrap(), 7_200.0);
        assert!(gaps[0] > gaps[1]);
        assert!(gaps[3] > gaps[2]);
        assert!((gaps[0] - gaps[3]).abs() < 1e-3);
        assert!((gaps[1] - gaps[2]).abs() < 1e-3);
    }
}
