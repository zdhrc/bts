use super::super::{Event, EventBatch, EventKind};
use super::evaluation::{EventFields, EventPlan, EventRef, Plan};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const EVENT_SLOT: Duration = Duration::from_millis(100);

// how trace volume spreads across the window; each variant maps an even 0..=1
// ratio through its inverse cdf so placement stays deterministic
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum Distribution {
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

        // leave room after the trace for scorer spans, even when filtered out
        let max_extent = plan.traces.iter().map(|trace| ends[trace.start]).max().unwrap_or_default();
        let max_extent = if plan.scorer_postlude {
            max_extent
                .checked_add(EVENT_SLOT)
                .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, EventRef(0)))?
        } else {
            max_extent
        };
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
                let metrics = metrics.unwrap_or_default();
                for key in ["start", "end"] {
                    if metrics.contains_key(key) {
                        return Err(Error::new(ErrorKind::ReservedMetric(key), event_ref));
                    }
                }

                Ok(Event {
                    id: Uuid::new_v4().to_string(),
                    span_id: self.resolve(event_ref).to_owned(),
                    root_span_id: self.resolve(event.root).to_owned(),
                    span_parents: event
                        .parent
                        .map(|parent| self.resolve(parent).to_owned())
                        .into_iter()
                        .collect(),
                    name: event.name,
                    kind: event.kind,
                    start,
                    end,
                    input,
                    output,
                    expected,
                    error,
                    metadata,
                    scores: event.scores,
                    metrics,
                    tags,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;

        Ok(EventBatch {
            events: events.into_boxed_slice(),
            trace_count: self.plan.traces.len(),
            attachments: std::mem::take(&mut self.plan.attachments),
        })
    }

    fn resolve(&self, event_ref: EventRef) -> &str {
        self.span_ids
            .get(event_ref.0)
            .expect("planner guarantees that event references are in bounds")
    }

    fn timestamp(&self, event: EventRef, anchor: SystemTime, offset: Duration) -> Result<SystemTime, Error> {
        let timestamp = anchor
            .checked_add(offset)
            .ok_or_else(|| Error::new(ErrorKind::TimestampOutOfRange, event))?;
        timestamp
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::new(ErrorKind::TimestampOutOfRange, event))?;
        Ok(timestamp)
    }
}

fn last_descendants(events: &[EventPlan]) -> Box<[usize]> {
    let mut last_descendants = (0..events.len()).collect::<Vec<_>>();

    for index in (0..events.len()).rev() {
        if let Some(parent) = events[index].parent {
            last_descendants[parent.0] = last_descendants[parent.0].max(last_descendants[index]);
        }
    }

    last_descendants.into_boxed_slice()
}

// application children run in order
// scorer children start after their parent ends and do not change its timing
fn layout(
    events: &[EventPlan],
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
    let mut scorers = Vec::new();
    let mut child = index + 1;
    while child <= last_descendants[index] {
        if events[child].kind == EventKind::Scorer {
            scorers.push(child);
        } else {
            layout(events, last_descendants, child, cursor, starts, ends)?;
            cursor = ends[child];
            tail = ends[child];
        }
        child = last_descendants[child] + 1;
    }

    let own = events[index].duration.unwrap_or(EVENT_SLOT);
    ends[index] = tail.max(offset.checked_add(own).ok_or_else(overflow)?);
    for scorer in scorers {
        layout(events, last_descendants, scorer, ends[index], starts, ends)?;
    }
    Ok(())
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
pub struct Error {
    kind: ErrorKind,
    event: EventRef,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ErrorKind {
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

    /// Returns the kind of materialization failure.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Returns the index of the event that could not be materialized.
    pub fn event_index(&self) -> usize {
        self.event.0
    }

    #[cfg(test)]
    fn event(&self) -> EventRef {
        self.event
    }
}

#[cfg(test)]
mod tests {
    use super::super::evaluation::plan;
    use super::*;
    use crate::dsl::semantics::compile;
    use crate::mocking::data::Value;
    use std::collections::BTreeMap;

    #[test]
    fn materializes_nested_spans_and_fields() {
        let model = compile(
            r#"
            vars {
                system = """
                    You are a concise personal finance assistant.
                    Answer briefly.
                    """
                meta = { model = "gpt-4o-mini", temperature = 0.2 }
            }
            trace "support-sessions" {
                task "turn_0" {
                    input = "How did NVDA do today?"
                    llm "Chat Completion" {
                        input = [
                            { role = "system", content = var.system },
                            { role = "user", content = task.turn_0.input },
                        ]
                        output = { role = "assistant", content = "NVDA closed down 1.4% at $207.40." }
                        metadata = var.meta
                        metrics = { prompt_tokens = 612, completion_tokens = 24,
                            tokens = self.metrics.prompt_tokens + self.metrics.completion_tokens }
                    }
                }
                task "turn_1" {
                    llm "Chat Completion" { metadata = var.meta }
                }
                tags = ["chat", "prod"]
            }
        "#,
        )
        .unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(7_200);
        let events = materialize(
            plan(model, 1, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            now,
        )
        .unwrap();

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

        assert_eq!(first_llm.name, "Chat Completion");
        assert_eq!(first_llm.kind, EventKind::Llm);
        let messages = first_llm.input.as_ref().unwrap().as_array().unwrap();
        assert_eq!(messages[0]["role"].as_str(), Some("system"));
        assert_eq!(
            messages[0]["content"].as_str(),
            Some("You are a concise personal finance assistant.\nAnswer briefly.")
        );
        assert_eq!(messages[1]["role"].as_str(), Some("user"));
        assert_eq!(messages[1]["content"].as_str(), Some("How did NVDA do today?"));
        assert_eq!(
            first_llm.output,
            Some(serde_json::json!({ "role": "assistant", "content": "NVDA closed down 1.4% at $207.40." }).into())
        );
        assert_eq!(first_llm.metadata.as_ref().unwrap()["model"].as_str(), Some("gpt-4o-mini"));
        assert_eq!(second_llm.metadata.as_ref().unwrap()["temperature"].as_f64(), Some(0.2));
        assert_eq!(first_llm.metrics["prompt_tokens"].as_i64(), Some(612));
        assert_eq!(first_llm.metrics["completion_tokens"].as_i64(), Some(24));
        assert_eq!(first_llm.metrics["tokens"].as_i64(), Some(636));

        for event in &events.events {
            assert_eq!(Uuid::parse_str(&event.id).unwrap().get_version_num(), 4);
            assert_eq!(Uuid::parse_str(&event.span_id).unwrap().get_version_num(), 4);
            assert!(event.start < event.end);
        }

        assert!(trace.start < first_turn.start);
        assert!(trace.end >= second_llm.end);

        assert_eq!(
            first_turn.input.as_ref().and_then(Value::as_str),
            Some("How did NVDA do today?")
        );
        assert_eq!(trace.tags.as_ref(), ["chat", "prod"]);
    }

    #[test]
    fn retains_expected_and_error_fields_only_when_set() {
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

        assert_eq!(events.events[0].expected.as_ref().and_then(Value::as_str), Some("4"));
        assert_eq!(events.events[0].error.as_ref().unwrap()["message"].as_str(), Some("timeout"));
        assert!(events.events[1].expected.is_none());
        assert!(events.events[1].error.is_none());
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // one trace spanning events 0..1 is the intent
    fn fails_on_reserved_metrics_that_bypass_compilation() {
        // modeler rejects reserved metric keys so a plan carrying one can only be built by hand
        let mut metrics = BTreeMap::new();
        metrics.insert("start".to_owned(), Value::from(1));
        let plan = Plan {
            attachments: Box::new([]),
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
                scores: None,
                duration: None,
            }]),
            traces: Box::new([0..1]),
            scorer_postlude: false,
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

        let root = &events.events[0];
        let a = &events.events[1];
        let b = &events.events[2];
        let c = &events.events[3];

        // leaves span exactly their durations
        assert_eq!(a.end.duration_since(a.start).unwrap(), Duration::from_secs(2));
        assert_eq!(c.end.duration_since(c.start).unwrap(), Duration::from_millis(1500));
        // a sibling starts where the previous one ends
        assert_eq!(b.start, a.end);
        // a parent outlived by its children ends with its last child
        assert_eq!(b.end, c.end);
        // an explicit total longer than the children stretches the span
        assert_eq!(root.end.duration_since(root.start).unwrap(), Duration::from_secs(60));
        // the single trace anchors so its computed extent ends at now
        assert_eq!(root.end, now);
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
        let starts = events.events.iter().map(|event| event.start).collect::<Vec<_>>();

        assert_eq!(events.trace_count(), 3);
        assert_eq!(starts[0], UNIX_EPOCH + Duration::from_secs(3_600));
        assert!(starts[1] > starts[0]);
        assert!(starts[2] > starts[1]);
        assert_eq!(events.events[2].end, now);
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
        let starts = events.events.iter().map(|event| event.start).collect::<Vec<_>>();
        let gaps = starts
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]).unwrap().as_secs_f64())
            .collect::<Vec<_>>();

        // endpoints match the linear spread, density peaks mid-window
        assert_eq!(starts[0], UNIX_EPOCH + Duration::from_secs(3_600));
        assert_eq!(events.events[4].end, now);
        assert!(gaps[0] > gaps[1]);
        assert!(gaps[3] > gaps[2]);
        assert!((gaps[0] - gaps[3]).abs() < 1e-3);
        assert!((gaps[1] - gaps[2]).abs() < 1e-3);
    }
}
