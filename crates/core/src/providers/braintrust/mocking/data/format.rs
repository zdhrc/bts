use crate::mocking::data::{self, Attachment, AttachmentFormat, EventKind, Value};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{Map, Value as JsonValue};
use std::{
    collections::BTreeMap,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

/// The Braintrust representation of generated traces and attachment references.
#[derive(Clone, Copy, Debug, Default)]
pub struct Format;

impl AttachmentFormat for Format {
    fn attachment(&self, attachment: &Attachment) -> Option<JsonValue> {
        Some(serde_json::json!({
            "type": "braintrust_attachment",
            "filename": attachment.filename,
            "content_type": attachment.content_type,
            "key": attachment.key,
        }))
    }
}

impl data::Format for Format {
    type Output = Batch;
    type Error = Error;

    fn encode(&self, batch: &data::EventBatch) -> Result<Batch, Error> {
        let events = batch
            .events
            .iter()
            .map(|event| {
                let start = seconds(event.start)?;
                let end = seconds(event.end)?;
                let mut metrics = object(&event.metrics)?;
                metrics.insert("start".to_owned(), start.into());
                metrics.insert("end".to_owned(), end.into());
                Ok(Event {
                    id: event.id.clone(),
                    span_id: event.span_id.clone(),
                    root_span_id: event.root_span_id.clone(),
                    span_parents: event.span_parents.clone(),
                    created: timestamp(event.start)?,
                    span_attributes: SpanAttributes {
                        name: event.name.clone(),
                        kind: event.kind.as_str().to_owned(),
                        purpose: (event.kind == EventKind::Scorer).then_some("scorer"),
                    },
                    input: event.input.as_ref().map(project).transpose()?,
                    output: event.output.as_ref().map(project).transpose()?,
                    expected: event.expected.as_ref().map(project).transpose()?,
                    error: event.error.as_ref().map(project).transpose()?,
                    metadata: event.metadata.as_ref().map(object).transpose()?,
                    scores: event.scores.as_ref().map(object).transpose()?,
                    metrics,
                    tags: event.tags.clone(),
                })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Batch {
            events,
            attachments: batch.attachments.clone(),
            trace_count: batch.trace_count,
        })
    }
}

fn project(value: &Value) -> Result<JsonValue, Error> {
    value.project(&Format).map_err(Error::Attachment)
}

fn object(values: &BTreeMap<String, Value>) -> Result<Map<String, JsonValue>, Error> {
    values.iter().map(|(key, value)| Ok((key.clone(), project(value)?))).collect()
}

fn seconds(time: SystemTime) -> Result<f64, Error> {
    Ok(time.duration_since(UNIX_EPOCH).map_err(Error::Timestamp)?.as_secs_f64())
}

fn timestamp(time: SystemTime) -> Result<String, Error> {
    let elapsed = time.duration_since(UNIX_EPOCH).map_err(Error::Timestamp)?;
    let seconds = elapsed.as_secs().try_into().map_err(|_| Error::TimestampOutOfRange)?;
    let time = DateTime::<Utc>::from_timestamp(seconds, elapsed.subsec_nanos()).ok_or(Error::TimestampOutOfRange)?;
    Ok(time.to_rfc3339_opts(SecondsFormat::Micros, true))
}

/// A batch ready for Braintrust serialization and delivery.
#[derive(Debug, Clone, Serialize)]
pub struct Batch {
    pub events: Box<[Event]>,
    #[serde(skip)]
    pub attachments: Box<[Attachment]>,
    #[serde(skip)]
    pub trace_count: usize,
}

impl Batch {
    pub fn event_count(&self) -> usize {
        self.events.len()
    }
    pub fn trace_count(&self) -> usize {
        self.trace_count
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub id: String,
    pub span_id: String,
    pub root_span_id: String,
    pub span_parents: Box<[String]>,
    pub created: String,
    pub span_attributes: SpanAttributes,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, JsonValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scores: Option<Map<String, JsonValue>>,
    pub metrics: Map<String, JsonValue>,
    #[serde(skip_serializing_if = "tags_are_empty")]
    pub tags: Box<[String]>,
}

fn tags_are_empty(tags: &[String]) -> bool {
    tags.is_empty()
}

#[derive(Debug, Clone, Serialize)]
pub struct SpanAttributes {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<&'static str>,
}

#[derive(Debug)]
pub enum Error {
    Attachment(data::UnsupportedAttachment),
    Timestamp(std::time::SystemTimeError),
    TimestampOutOfRange,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attachment(source) => source.fmt(f),
            Self::Timestamp(_) | Self::TimestampOutOfRange => f.write_str("timestamp is out of range for Braintrust"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Attachment(source) => Some(source),
            Self::Timestamp(source) => Some(source),
            Self::TimestampOutOfRange => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::semantics::compile;
    use crate::mocking::data::Format as _;
    use serde_json::json;
    use std::{collections::HashMap, time::Duration};

    fn generate(source: &str) -> data::EventBatch {
        data::generate(
            compile(source).unwrap(),
            1,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            42,
            &Format,
        )
        .unwrap()
    }

    #[test]
    fn preserves_event_order_timestamps_and_optional_wire_fields() {
        let batch = generate(
            r#"
            trace "example" {
                duration = 2
                expected = "4"
                error = { message = "timeout" }
                tags = ["chat", "prod"]
                task "step" { input = "question" duration = 1 }
            }
            "#,
        );
        let encoded = Format.encode(&batch).unwrap();
        let json = serde_json::to_value(&encoded).unwrap();
        let events = json["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["span_attributes"], json!({"name": "example", "type": "task"}));
        assert_eq!(events[1]["span_attributes"], json!({"name": "step", "type": "task"}));
        assert_eq!(events[0]["id"], batch.events[0].id);
        assert_eq!(events[1]["span_parents"], json!([batch.events[0].span_id]));
        assert_eq!(events[0]["created"], "1970-01-01T01:59:58.000000Z");
        assert_eq!(events[0]["metrics"]["start"], 7198.0);
        assert_eq!(events[0]["metrics"]["end"], 7200.0);
        assert_eq!(events[0]["tags"], json!(["chat", "prod"]));
        assert_eq!(events[0]["expected"], "4");
        assert_eq!(events[0]["error"]["message"], "timeout");
        assert_eq!(events[1]["input"], "question");
        for key in ["expected", "error", "output", "metadata", "scores", "tags"] {
            assert!(events[1].get(key).is_none(), "unexpected field {key}");
        }
        assert!(json.get("attachments").is_none());
        assert!(json.get("trace_count").is_none());
        assert_eq!(encoded.trace_count(), 1);
    }

    #[test]
    fn projects_attachment_fields_and_token_counts_with_braintrust_encoding() {
        let batch = generate(
            r#"
            trace "example" {
                input = attachment("report.txt", "text/plain")
                output = self.input["type"]
                metrics = { count = tokens([self.input]) }
            }
            "#,
        );
        let attachment = Format.attachment(&batch.attachments[0]).unwrap();
        let expected_tokens = tiktoken_rs::o200k_base()
            .unwrap()
            .encode_ordinary(&json!([attachment.clone()]).to_string())
            .len();
        assert_eq!(
            batch.events[0].output.as_ref().and_then(Value::as_str),
            Some("braintrust_attachment")
        );
        assert_eq!(batch.events[0].metrics["count"].as_u64(), Some(expected_tokens as u64));
        let encoded = Format.encode(&batch).unwrap();
        assert_eq!(encoded.events[0].input, Some(attachment));
        assert!(matches!(batch.events[0].input, Some(Value::Attachment(_))));
    }

    #[test]
    fn attachment_reuse_does_not_rewrite_ordinary_json_with_the_same_marker_and_key() {
        let mut batch = generate(
            r#"
            trace "example" {
                input = attachment("report.txt", "text/plain")
                output = {
                    type = "braintrust_attachment"
                    filename = "report.txt"
                    content_type = "text/plain"
                    key = self.input["key"]
                }
            }
            "#,
        );
        let original = batch.attachments[0].key.clone();
        batch.reuse_attachments(&HashMap::from([(original.clone(), "stored".to_owned())]));
        let encoded = Format.encode(&batch).unwrap();
        assert_eq!(encoded.events[0].input.as_ref().unwrap()["key"], "stored");
        assert_eq!(encoded.events[0].output.as_ref().unwrap()["key"], original);
        assert!(encoded.attachments.is_empty());
    }
}
