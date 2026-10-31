use bts_core::mocking::data::{
    self, Attachment, AttachmentFormat, Distribution, EventBatch, Format, PrepareError, Request, RunError, Sink,
    UnsupportedAttachment, Value, WriteError,
};
use bts_core::selection::filter::WriteFilter;
use bts_lang::semantics::compile;
use serde_json::{Value as JsonValue, json};
use std::cell::{Cell, RefCell};
use std::error::Error as _;
use std::fmt;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
struct Envelope(Vec<JsonValue>);

struct ArchiveFormat {
    attachments: bool,
    projection: Option<JsonValue>,
    reject: bool,
    encodes: Cell<usize>,
}

impl Default for ArchiveFormat {
    fn default() -> Self {
        Self {
            attachments: true,
            projection: None,
            reject: false,
            encodes: Cell::new(0),
        }
    }
}

impl AttachmentFormat for ArchiveFormat {
    fn attachment(&self, attachment: &Attachment) -> Option<JsonValue> {
        self.attachments.then(|| {
            self.projection
                .clone()
                .unwrap_or_else(|| json!({ "name": attachment.filename, "reference": attachment.key }))
        })
    }
}

impl Format for ArchiveFormat {
    type Output = Envelope;
    type Error = EncodeError;

    fn encode(&self, events: &EventBatch) -> Result<Envelope, EncodeError> {
        self.encodes.set(self.encodes.get() + 1);
        if self.reject {
            return Err(EncodeError::Rejected);
        }
        events
            .events
            .iter()
            .map(|event| {
                let input = event
                    .input
                    .as_ref()
                    .map(|value| value.project(self))
                    .transpose()
                    .map_err(EncodeError::Attachment)?;
                let output = event
                    .output
                    .as_ref()
                    .map(|value| value.project(self))
                    .transpose()
                    .map_err(EncodeError::Attachment)?;
                Ok(json!({ "label": event.name, "input": input, "result": output }))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Envelope)
    }
}

#[derive(Debug)]
enum EncodeError {
    Rejected,
    Attachment(UnsupportedAttachment),
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected => f.write_str("archive encoding rejected"),
            Self::Attachment(source) => source.fmt(f),
        }
    }
}

impl std::error::Error for EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Attachment(source) => Some(source),
            Self::Rejected => None,
        }
    }
}

#[derive(Debug, PartialEq)]
struct Receipt {
    archive: usize,
    records: usize,
}

#[derive(Default)]
struct ArchiveSink {
    attempts: Cell<usize>,
    received: RefCell<Vec<Envelope>>,
    unavailable: bool,
}

impl Sink for ArchiveSink {
    type Input = Envelope;
    type Receipt = Receipt;
    type Error = ArchiveUnavailable;

    fn send(&self, input: &Envelope) -> Result<Receipt, ArchiveUnavailable> {
        self.attempts.set(self.attempts.get() + 1);
        if self.unavailable {
            return Err(ArchiveUnavailable {
                retry_after: Duration::from_secs(12),
            });
        }
        let mut received = self.received.borrow_mut();
        received.push(input.clone());
        Ok(Receipt {
            archive: received.len(),
            records: input.0.len(),
        })
    }
}

#[derive(Debug)]
struct ArchiveUnavailable {
    retry_after: Duration,
}

impl fmt::Display for ArchiveUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "archive unavailable for {} seconds", self.retry_after.as_secs())
    }
}

impl std::error::Error for ArchiveUnavailable {}

fn request<'a>(source: &str, filter: &'a WriteFilter) -> Request<'a> {
    Request {
        model: compile(source).unwrap(),
        count: 1,
        over: Duration::from_secs(60),
        distribution: Distribution::Linear,
        until: UNIX_EPOCH + Duration::from_secs(86_400),
        seed: 4,
        filter,
        source: Path::new("archive.bt"),
    }
}

#[test]
fn downstream_capabilities_generate_project_and_deliver_with_their_own_receipt() {
    let source = r#"
        trace "archive" {
            input = { files = [attachment("report.txt", "text/plain")] }
            output = self.input["files"][0]["name"]
            tool "copy" { input = trace.input output = trace.input["files"][0] }
        }
    "#;
    let format = ArchiveFormat::default();
    let sink = ArchiveSink::default();
    let selected_format: &dyn Format<Output = Envelope, Error = EncodeError> = &format;
    let selected_sink: &dyn Sink<Input = Envelope, Receipt = Receipt, Error = ArchiveUnavailable> = &sink;
    let written = data::run(request(source, &WriteFilter::default()), selected_format, selected_sink).unwrap();

    assert_eq!(written.receipt, Receipt { archive: 1, records: 2 });
    assert_eq!(written.events.trace_count, 1);
    let attachment = &written.events.events[0].input.as_ref().unwrap()["files"][0];
    assert!(matches!(attachment, Value::Attachment(_)));
    assert_eq!(written.events.events[1].output.as_ref(), Some(attachment));
    let projected = json!({ "name": "report.txt", "reference": written.events.attachments[0].key });
    let expected = Envelope(vec![
        json!({ "label": "archive", "input": { "files": [projected.clone()] }, "result": "report.txt" }),
        json!({ "label": "copy", "input": { "files": [projected.clone()] }, "result": projected }),
    ]);
    assert_eq!(sink.received.borrow().as_slice(), &[expected]);

    let receipt = data::write(&written.events, &format, &sink).unwrap();
    assert_eq!(receipt, Receipt { archive: 2, records: 2 });
    assert_eq!(format.encodes.get(), 2);
    assert_eq!(sink.attempts.get(), 2);
}

#[test]
fn encoding_failure_prevents_delivery() {
    let format = ArchiveFormat {
        reject: true,
        ..Default::default()
    };
    let sink = ArchiveSink::default();
    let error = data::run(
        request(r#"trace "archive" { output = "ready" }"#, &WriteFilter::default()),
        &format,
        &sink,
    )
    .err()
    .unwrap();

    assert!(matches!(error, RunError::Write(WriteError::Format(EncodeError::Rejected))));
    assert_eq!(format.encodes.get(), 1);
    assert_eq!(sink.attempts.get(), 0);
    assert!(sink.received.borrow().is_empty());
}

#[test]
fn delivery_failure_preserves_the_downstream_typed_cause() {
    let format = ArchiveFormat::default();
    let sink = ArchiveSink {
        unavailable: true,
        ..Default::default()
    };
    let error = data::run(
        request(r#"trace "archive" { output = "ready" }"#, &WriteFilter::default()),
        &format,
        &sink,
    )
    .err()
    .unwrap();

    assert!(matches!(error, RunError::Write(WriteError::Send(_))));
    let cause = error
        .source()
        .unwrap()
        .source()
        .unwrap()
        .downcast_ref::<ArchiveUnavailable>()
        .unwrap();
    assert_eq!(cause.retry_after, Duration::from_secs(12));
    assert_eq!(error.to_string(), "archive unavailable for 12 seconds");
    assert_eq!(format.encodes.get(), 1);
    assert_eq!(sink.attempts.get(), 1);
    assert!(sink.received.borrow().is_empty());
}

#[test]
fn unsupported_projection_preserves_the_source_range_and_stops_before_encoding() {
    let source = r#"trace "archive" { input = attachment("report.txt", "text/plain") output = self.input["name"] }"#;
    let format = ArchiveFormat {
        attachments: false,
        ..Default::default()
    };
    let sink = ArchiveSink::default();
    let error = data::run(request(source, &WriteFilter::default()), &format, &sink)
        .err()
        .unwrap();

    let RunError::Prepare(PrepareError::Generate(data::Error::Plan(cause))) = error else {
        panic!("expected a planning error, got {error}");
    };
    assert_eq!(cause.kind(), data::generation::EvaluationErrorKind::UnsupportedAttachment);
    assert_eq!(&source[cause.range.start..cause.range.end], r#"self.input["name"]"#);
    assert_eq!(format.encodes.get(), 0);
    assert_eq!(sink.attempts.get(), 0);
}

#[test]
fn incompatible_projection_stops_before_encoding_and_delivery() {
    let source = r#"vars { file = attachment("report.txt", "text/plain") }
        trace "archive" { input = var.file["name"] }"#;
    let format = ArchiveFormat {
        projection: Some(json!(["report.txt"])),
        ..Default::default()
    };
    let sink = ArchiveSink::default();
    let error = data::run(request(source, &WriteFilter::default()), &format, &sink)
        .err()
        .unwrap();

    let RunError::Prepare(PrepareError::Generate(data::Error::Plan(cause))) = error else {
        panic!("expected a planning error, got {error}");
    };
    assert_eq!(cause.kind(), data::generation::EvaluationErrorKind::RefShapeMismatch);
    assert_eq!(&source[cause.range.start..cause.range.end], r#"var.file["name"]"#);
    assert_eq!(format.encodes.get(), 0);
    assert_eq!(sink.attempts.get(), 0);
    assert!(sink.received.borrow().is_empty());
}
