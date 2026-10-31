use super::Batch;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    future::Future,
    time::{Duration, SystemTime},
};

#[cfg(feature = "native")]
use crate::{
    mocking::data,
    providers::braintrust::{
        client,
        mocking::data::{Format, TraceSink},
    },
};

#[cfg(feature = "native")]
pub type WriteError = data::WriteError<super::Error, client::writer::Error>;
#[cfg(feature = "native")]
pub type RunError = data::RunError<super::Error, client::writer::Error>;
#[cfg(feature = "native")]
pub type Written = data::Written<client::writer::InsertResponse>;

/// Writes a prepared batch using the Braintrust format and delivery implementation.
#[cfg(feature = "native")]
pub fn write(client: &client::Client, events: &data::EventBatch) -> Result<client::writer::InsertResponse, WriteError> {
    data::write(events, &Format, &TraceSink::new(client))
}

/// Generates and writes traces using the supplied client.
#[cfg(feature = "native")]
pub fn run(request: data::Request<'_>, client: &client::Client) -> Result<Written, RunError> {
    data::run(request, &Format, &TraceSink::new(client))
}

/// A request to the configured Braintrust API origin.
/// The host resolves `path`, adds authentication, and enforces `timeout` through
/// response body consumption. Core owns retry decisions, so the host sends once.
pub struct HttpRequest {
    pub method: &'static str,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub timeout: Duration,
}

/// An HTTP response whose body has been fully consumed.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A host transport failure without credentials or signed URLs in its message.
#[derive(Debug)]
pub struct TransportError {
    pub message: String,
    /// Whether this represents a connection failure or timeout that can be retried.
    pub retryable: bool,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

/// Host capabilities needed to deliver traces without a native HTTP runtime.
/// Futures may remain on the calling thread. `now` must be monotonic and
/// `wall_time` supplies UTC time for HTTP-date `Retry-After` values.
pub trait Transport {
    fn send(&self, request: HttpRequest) -> impl Future<Output = Result<HttpResponse, TransportError>>;
    fn sleep(&self, delay: Duration) -> impl Future<Output = ()>;
    fn now(&self) -> Duration;
    fn wall_time(&self) -> SystemTime;
}

/// Retry limits for each insert request. Payloads are sent sequentially.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    pub request_timeout: Duration,
    pub retry_attempts: u32,
    pub retry_max_elapsed: Duration,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            retry_attempts: 8,
            retry_max_elapsed: Duration::from_secs(120),
        }
    }
}

/// Row IDs acknowledged by Braintrust, in submitted payload order.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct InsertResponse {
    pub(crate) row_ids: Box<[String]>,
}

impl InsertResponse {
    pub fn row_count(&self) -> usize {
        self.row_ids.len()
    }

    pub fn row_ids(&self) -> &[String] {
        &self.row_ids
    }
}

/// A write failure, including every row ID acknowledged before the failure.
/// A failed or interrupted request may also have inserted rows whose response
/// was not received. Writes are not rolled back.
#[derive(Debug)]
pub struct AsyncWriteError {
    pub acknowledged: InsertResponse,
    pub kind: AsyncWriteErrorKind,
}

#[derive(Debug)]
pub enum AsyncWriteErrorKind {
    InvalidOption(&'static str),
    AttachmentsUnsupported,
    Encode(serde_json::Error),
    EventTooLarge { size: usize, limit: usize },
    Transport(TransportError),
    Rejected { status: u16, body: String },
    Decode(serde_json::Error),
    UnexpectedRowCount { expected: usize, actual: usize },
    Deadline,
}

impl fmt::Display for AsyncWriteErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOption(field) => write!(f, "{field} must be greater than zero"),
            Self::AttachmentsUnsupported => f.write_str("host transport trace writes do not support attachment uploads"),
            Self::Encode(source) => write!(f, "failed to encode an event as JSON: {source}"),
            Self::EventTooLarge { size, limit } => {
                write!(f, "a single event of {size} bytes exceeds the {limit} byte payload limit")
            }
            Self::Transport(source) => write!(f, "failed to send request: {source}"),
            Self::Rejected { status, body } => write!(f, "Braintrust rejected the request with HTTP {status}: {body}"),
            Self::Decode(source) => write!(f, "failed to decode Braintrust response: {source}"),
            Self::UnexpectedRowCount { expected, actual } => write!(
                f,
                "Braintrust acknowledged {actual} rows, but {expected} events were submitted"
            ),
            Self::Deadline => f.write_str("Braintrust request exceeded retry_max_elapsed"),
        }
    }
}

impl fmt::Display for AsyncWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} rows acknowledged)", self.kind, self.acknowledged.row_count())
    }
}

impl std::error::Error for AsyncWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            AsyncWriteErrorKind::Encode(source) | AsyncWriteErrorKind::Decode(source) => Some(source),
            AsyncWriteErrorKind::Transport(source) => Some(source),
            _ => None,
        }
    }
}

/// Writes an encoded trace batch through a host HTTP transport.
/// All events are packed before sending. Attachment uploads are rejected before
/// any request. Oversized server rejections are split, transient failures are
/// retried within the supplied limits, and success requires one acknowledged ID
/// per event. Earlier writes may succeed when a later payload fails.
pub async fn write_async(
    transport: &impl Transport,
    project_id: uuid::Uuid,
    events: &Batch,
    options: &WriteOptions,
) -> Result<InsertResponse, AsyncWriteError> {
    let mut row_ids = Vec::new();
    let result = async {
        for (field, valid) in [
            ("request_timeout", !options.request_timeout.is_zero()),
            ("retry_attempts", options.retry_attempts > 0),
            ("retry_max_elapsed", !options.retry_max_elapsed.is_zero()),
        ] {
            if !valid {
                return Err(AsyncWriteErrorKind::InvalidOption(field));
            }
        }
        if !events.attachments.is_empty() {
            return Err(AsyncWriteErrorKind::AttachmentsUnsupported);
        }
        let mut pending = pack(&events.events, MAX_PAYLOAD_BYTES).map_err(|error| match error {
            PackError::Encode(source) => AsyncWriteErrorKind::Encode(source),
            PackError::EventTooLarge { size, limit } => AsyncWriteErrorKind::EventTooLarge { size, limit },
        })?;
        pending.reverse();
        let path = format!("/v1/project_logs/{project_id}/insert");
        while let Some(payload) = pending.pop() {
            let response = send_payload(transport, &path, &payload, options).await?;
            if should_split(response.status, payload.event_count()) {
                let (left, right) = payload.split();
                pending.push(right);
                pending.push(left);
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(AsyncWriteErrorKind::Rejected {
                    status: response.status,
                    body: String::from_utf8_lossy(&response.body).chars().take(500).collect(),
                });
            }
            let inserted: InsertResponse = serde_json::from_slice(&response.body).map_err(AsyncWriteErrorKind::Decode)?;
            let acknowledged = check_acknowledgment(&inserted, payload.event_count());
            row_ids.extend(inserted.row_ids.into_vec());
            if let Err((expected, actual)) = acknowledged {
                return Err(AsyncWriteErrorKind::UnexpectedRowCount { expected, actual });
            }
        }
        Ok(())
    }
    .await;
    let acknowledged = InsertResponse {
        row_ids: row_ids.into_boxed_slice(),
    };
    match result {
        Ok(()) => Ok(acknowledged),
        Err(kind) => Err(AsyncWriteError { acknowledged, kind }),
    }
}

async fn send_payload(
    transport: &impl Transport,
    path: &str,
    payload: &Payload,
    options: &WriteOptions,
) -> Result<HttpResponse, AsyncWriteErrorKind> {
    let start = transport.now();
    for attempt in 1..=options.retry_attempts {
        let elapsed = transport.now().saturating_sub(start);
        let remaining = options.retry_max_elapsed.saturating_sub(elapsed);
        if remaining.is_zero() {
            return Err(AsyncWriteErrorKind::Deadline);
        }
        let result = transport
            .send(HttpRequest {
                method: "POST",
                path: path.to_owned(),
                headers: vec![("content-type".to_owned(), "application/json".to_owned())],
                body: payload.body(),
                timeout: options.request_timeout.min(remaining),
            })
            .await;
        let (retry, server_delay) = match &result {
            Ok(response) => (
                retry_response(response.status, true),
                response
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                    .and_then(|(_, value)| retry_after(value, transport.wall_time())),
            ),
            Err(error) => (error.retryable, None),
        };
        let jitter = Duration::from_millis((uuid::Uuid::new_v4().as_u128() % 101) as u64);
        let delay = server_delay.unwrap_or_else(|| retry_backoff(Duration::from_millis(500), attempt, jitter));
        if !retry
            || attempt == options.retry_attempts
            || transport.now().saturating_sub(start).saturating_add(delay) >= options.retry_max_elapsed
        {
            return result.map_err(AsyncWriteErrorKind::Transport);
        }
        transport.sleep(delay).await;
    }
    unreachable!()
}

// braintrust advertises 5 MiB, but gateway wrapping lowers the effective limit
// use half that limit, matching the official sdk
pub(crate) const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024 / 2;
const PAYLOAD_OPEN: &[u8] = b"{\"events\":[";
const PAYLOAD_CLOSE: &[u8] = b"]}";

pub(crate) fn should_split(status: u16, event_count: usize) -> bool {
    status == 413 && event_count > 1
}

pub(crate) fn check_acknowledgment(inserted: &InsertResponse, expected: usize) -> Result<(), (usize, usize)> {
    let actual = inserted.row_count();
    if actual == expected { Ok(()) } else { Err((expected, actual)) }
}

pub(crate) fn retry_response(status: u16, safe: bool) -> bool {
    status == 429 || (safe && ((500..600).contains(&status) || status == 408))
}

pub(crate) fn retry_backoff(base: Duration, attempt: u32, jitter: Duration) -> Duration {
    base.saturating_mul(1 << attempt.saturating_sub(1).min(10))
        .min(Duration::from_secs(30))
        + jitter
}

pub(crate) fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(86400.0)));
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| SystemTime::from(date).duration_since(now).unwrap_or_default())
}

#[derive(Debug)]
pub(crate) struct Payload {
    events: Vec<Vec<u8>>,
    pub(crate) body_len: usize,
}

impl Payload {
    fn new(events: Vec<Vec<u8>>) -> Self {
        let body_len = PAYLOAD_OPEN.len()
            + PAYLOAD_CLOSE.len()
            + events.iter().map(Vec::len).sum::<usize>()
            + events.len().saturating_sub(1);
        Self { events, body_len }
    }

    pub(crate) fn event_count(&self) -> usize {
        self.events.len()
    }

    fn fits(&self, encoded_length: usize, limit: usize) -> bool {
        self.body_len + usize::from(!self.events.is_empty()) + encoded_length <= limit
    }

    fn push(&mut self, encoded: Vec<u8>) {
        self.body_len += usize::from(!self.events.is_empty()) + encoded.len();
        self.events.push(encoded);
    }

    pub(crate) fn split(mut self) -> (Self, Self) {
        let right = self.events.split_off(self.events.len() / 2);
        (Self::new(self.events), Self::new(right))
    }

    pub(crate) fn body(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(self.body_len);
        body.extend_from_slice(PAYLOAD_OPEN);
        for (index, event) in self.events.iter().enumerate() {
            if index > 0 {
                body.push(b',');
            }
            body.extend_from_slice(event);
        }
        body.extend_from_slice(PAYLOAD_CLOSE);
        body
    }
}

pub(crate) enum PackError {
    Encode(serde_json::Error),
    EventTooLarge { size: usize, limit: usize },
}

pub(crate) fn pack<T: Serialize>(events: &[T], limit: usize) -> Result<Vec<Payload>, PackError> {
    let mut payloads = Vec::new();
    let mut current = Payload::new(Vec::new());
    for event in events {
        let encoded = serde_json::to_vec(event).map_err(PackError::Encode)?;
        if PAYLOAD_OPEN.len() + encoded.len() + PAYLOAD_CLOSE.len() > limit {
            return Err(PackError::EventTooLarge {
                size: encoded.len(),
                limit,
            });
        }
        if !current.fits(encoded.len(), limit) {
            payloads.push(current);
            current = Payload::new(Vec::new());
        }
        current.push(encoded);
    }
    if current.event_count() > 0 {
        payloads.push(current);
    }
    Ok(payloads)
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use crate::providers::braintrust::client::tests::{Reply, serve};
    use crate::{
        mocking::data::{Distribution, Request},
        selection::filter::WriteFilter,
    };
    use serde_json::json;
    use std::time::Duration;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
    };
    use std::{path::Path, time::UNIX_EPOCH};

    struct Host {
        responses: RefCell<VecDeque<Result<HttpResponse, TransportError>>>,
        requests: RefCell<Vec<HttpRequest>>,
        elapsed: Cell<Duration>,
    }

    impl Host {
        fn new(responses: Vec<HttpResponse>) -> Self {
            Self {
                responses: RefCell::new(responses.into_iter().map(Ok).collect()),
                requests: RefCell::new(Vec::new()),
                elapsed: Cell::new(Duration::ZERO),
            }
        }
    }

    impl Transport for Host {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
            self.requests.borrow_mut().push(request);
            self.responses.borrow_mut().pop_front().expect("unexpected request")
        }

        async fn sleep(&self, delay: Duration) {
            self.elapsed.set(self.elapsed.get() + delay);
        }

        fn now(&self) -> Duration {
            self.elapsed.get()
        }
        fn wall_time(&self) -> SystemTime {
            UNIX_EPOCH + self.elapsed.get()
        }
    }

    fn ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("test transport should complete without waiting"),
        }
    }

    fn response(status: u16, body: serde_json::Value) -> HttpResponse {
        HttpResponse {
            status,
            headers: Vec::new(),
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn batch(count: usize) -> Batch {
        use crate::mocking::data::Format as _;
        let events = data::generate(
            crate::dsl::semantics::compile(r#"trace "run" { output = "hello" }"#).unwrap(),
            count,
            Duration::from_secs(60),
            Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(3600),
            42,
            &Format,
        )
        .unwrap();
        Format.encode(&events).unwrap()
    }

    #[test]
    fn async_trace_write_splits_rejected_payloads_and_preserves_receipt_order() {
        let host = Host::new(vec![
            response(413, json!({})),
            response(200, json!({"row_ids":["one"]})),
            response(200, json!({"row_ids":["two"]})),
        ]);
        let project = uuid::Uuid::nil();
        let receipt = ready(write_async(&host, project, &batch(2), &WriteOptions::default())).unwrap();
        assert_eq!(receipt.row_ids(), ["one", "two"]);
        let requests = host.requests.borrow();
        assert!(
            requests
                .iter()
                .all(|request| request.method == "POST" && request.path == format!("/v1/project_logs/{project}/insert"))
        );
        let payloads: Vec<serde_json::Value> = requests
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        assert_eq!(payloads[0]["events"].as_array().unwrap().len(), 2);
        assert_eq!(payloads[1]["events"][0], payloads[0]["events"][0]);
        assert_eq!(payloads[2]["events"][0], payloads[0]["events"][1]);
    }

    #[test]
    fn async_trace_write_rejects_attachments_before_any_request() {
        let host = Host::new(Vec::new());
        let mut events = batch(1);
        events.attachments = vec![data::Attachment {
            path: "input.pdf".to_owned(),
            filename: "input.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            key: "attachment-key".to_owned(),
        }]
        .into_boxed_slice();
        let error = ready(write_async(&host, uuid::Uuid::nil(), &events, &WriteOptions::default())).unwrap_err();
        assert!(matches!(error.kind, AsyncWriteErrorKind::AttachmentsUnsupported));
        assert_eq!(error.acknowledged.row_count(), 0);
        assert!(host.requests.borrow().is_empty());
    }

    #[test]
    fn async_trace_write_honors_retry_after_and_request_budget() {
        let mut throttled = response(429, json!({}));
        throttled.headers.push(("Retry-After".to_owned(), "0.25".to_owned()));
        let host = Host::new(vec![throttled, response(200, json!({"row_ids":["one"]}))]);
        let options = WriteOptions {
            retry_max_elapsed: Duration::from_secs(1),
            ..WriteOptions::default()
        };
        ready(write_async(&host, uuid::Uuid::nil(), &batch(1), &options)).unwrap();
        assert_eq!(host.elapsed.get(), Duration::from_millis(250));
        let requests = host.requests.borrow();
        assert_eq!(requests[0].timeout, Duration::from_secs(1));
        assert_eq!(requests[1].timeout, Duration::from_millis(750));
    }

    #[test]
    fn async_trace_write_retains_acknowledgments_when_a_later_payload_fails() {
        let host = Host::new(vec![
            response(413, json!({})),
            response(200, json!({"row_ids":["one"]})),
            response(400, json!({"message":"invalid"})),
        ]);
        let error = ready(write_async(&host, uuid::Uuid::nil(), &batch(2), &WriteOptions::default())).unwrap_err();
        assert!(matches!(error.kind, AsyncWriteErrorKind::Rejected { status: 400, .. }));
        assert_eq!(error.acknowledged.row_ids(), ["one"]);
        assert_eq!(host.requests.borrow().len(), 3);
        assert_eq!(host.elapsed.get(), Duration::ZERO);
    }

    #[test]
    fn async_trace_write_reports_partial_acknowledgment_as_failure() {
        let host = Host::new(vec![response(200, json!({"row_ids":["one"]}))]);
        let error = ready(write_async(&host, uuid::Uuid::nil(), &batch(2), &WriteOptions::default())).unwrap_err();
        assert!(matches!(
            error.kind,
            AsyncWriteErrorKind::UnexpectedRowCount { expected: 2, actual: 1 }
        ));
        assert_eq!(error.acknowledged.row_ids(), ["one"]);
        assert_eq!(host.requests.borrow().len(), 1);
    }

    #[test]
    fn runs_generation_and_writing_through_the_core_api() {
        let (client, requests) = serve(vec![Reply::json(json!({"row_ids":["one","two"]}))]);
        let written = run(
            Request {
                model: crate::dsl::semantics::compile(r#"trace "run" { output = "hello" }"#).unwrap(),
                count: 2,
                over: Duration::from_secs(60),
                distribution: Distribution::Linear,
                until: UNIX_EPOCH + Duration::from_secs(3600),
                seed: 42,
                filter: &WriteFilter::default(),
                source: Path::new("shape.bt"),
            },
            &client,
        )
        .unwrap();
        assert_eq!(written.events.trace_count(), 2);
        assert_eq!(written.receipt.row_count(), 2);
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("POST /v1/project_logs/"));
        let (_, body) = request.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["events"].as_array().unwrap().len(), 2);
        assert_eq!(body["events"][0]["output"], "hello");
    }
}
