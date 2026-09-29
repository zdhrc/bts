use crate::{
    conf::Braintrust,
    sdg::{materializer::EventBatch, planner::Attachment},
};
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use reqwest::{
    StatusCode,
    blocking::{Client, Response},
};
use serde::Deserialize;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fmt, thread, time::Duration};

// braintrust's lambda-backed api caps request bodies; advertised as logs3_payload_max_bytes (5MiB) on
// GET /version, but the gateway measures requests after lambda event wrapping inflates them, so bodies
// near the advertised cap still 413. the official sdk batches at half the advertised limit; match that.
const MAX_PAYLOAD_BYTES: usize = 5 * 1024 * 1024 / 2;
const PAYLOAD_OPEN: &[u8] = b"{\"events\":[";
const PAYLOAD_CLOSE: &[u8] = b"]}";

// transient failures (timeouts, 429s, 5xx) back off exponentially before giving up; the
// server's Retry-After wins over the computed backoff when present, capped so a run never stalls
const MAX_SEND_ATTEMPTS: u32 = 4;
const MAX_ATTACHMENT_STATUS_ATTEMPTS: u32 = 3;
const MAX_ATTACHMENT_RECONCILE_ATTEMPTS: u32 = 3;
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const MAX_ATTACHMENT_ERROR_CHARS: usize = 512;

#[derive(Debug, Deserialize)]
pub(crate) struct InsertResponse {
    row_ids: Box<[String]>,
}

#[derive(Deserialize)]
struct ProjectResponse {
    org_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentResponse {
    signed_url: String,
    headers: std::collections::HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttachmentMetadata {
    status: AttachmentStatus,
    content_length: Option<u64>,
    download_url: Option<String>,
}

#[derive(Deserialize)]
struct AttachmentStatus {
    upload_status: String,
    error_message: Option<String>,
}

enum ReconcileError {
    ReportedError(String),
    Unknown(String),
}

impl InsertResponse {
    pub(crate) fn row_count(&self) -> usize {
        self.row_ids.len()
    }
}

struct Writer<'config> {
    client: Client,
    config: &'config Braintrust,
    backoff_base: Duration,
}

impl<'config> Writer<'config> {
    fn new(config: &'config Braintrust) -> Result<Self, Error> {
        let client = Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| Error::new(ErrorKind::BuildClient(error)))?;

        Ok(Self {
            client,
            config,
            backoff_base: BACKOFF_BASE,
        })
    }

    // payloads are independent, so a pool of scoped worker threads sends them concurrently;
    // insert latency dominates a run, so wall time shrinks by roughly the worker count
    fn write(&self, events: &EventBatch) -> Result<InsertResponse, Error> {
        let url = format!(
            "{}/v1/project_logs/{}/insert",
            self.config.api_url.trim_end_matches('/'),
            self.config.project_id,
        );
        let payloads = payloads(events, MAX_PAYLOAD_BYTES)?;
        if !events.attachments.is_empty() {
            self.upload_attachments(&events.attachments)?;
        }
        let workers = self.config.write_concurrency.min(payloads.len()).max(1);
        // payloads are indexed so acknowledged row ids reassemble in submission
        // order no matter which worker finishes first
        let mut slots: Vec<Option<Vec<String>>> = Vec::new();
        slots.resize_with(payloads.len(), || None);
        let slots = Mutex::new(slots);
        let pending = Mutex::new(payloads.into_iter().enumerate().rev().collect::<Vec<_>>());
        let failed = AtomicBool::new(false);
        // workers re-enter the caller's span so insert logs keep their place in the tree
        let span = tracing::Span::current();

        let insert_result = thread::scope(|scope| {
            let handles: Vec<_> = (0..workers)
                .map(|_| {
                    scope.spawn(|| {
                        let _guard = span.enter();
                        self.drain(&url, &pending, &slots, &failed)
                    })
                })
                .collect();

            handles
                .into_iter()
                .try_for_each(|handle| handle.join().expect("writer worker panicked"))
        });
        if let Err(source) = insert_result {
            return Err(if events.attachments.is_empty() {
                source
            } else {
                Error::new(ErrorKind::InsertAfterAttachments {
                    uploaded: events.attachments.len(),
                    source: Box::new(source),
                })
            });
        }

        let mut row_ids = Vec::with_capacity(events.event_count());
        for slot in slots.into_inner().unwrap() {
            row_ids.extend(slot.expect("every payload has a result when no worker failed"));
        }

        Ok(InsertResponse {
            row_ids: row_ids.into_boxed_slice(),
        })
    }

    fn upload_attachments(&self, attachments: &[Attachment]) -> Result<(), Error> {
        let project_url = format!(
            "{}/v1/project/{}",
            self.config.api_url.trim_end_matches('/'),
            self.config.project_id,
        );
        let response = self
            .client
            .get(project_url)
            .bearer_auth(&self.config.api_key)
            .send()
            .map_err(|source| {
                Error::new(ErrorKind::Attachment(format!(
                    "project lookup failed: {}",
                    source.without_url()
                )))
            })?;
        let project: ProjectResponse = successful_attachment_response(response)
            .and_then(|response| {
                response
                    .json()
                    .map_err(|source| format!("invalid project response: {}", source.without_url()))
            })
            .map_err(|detail| Error::new(ErrorKind::Attachment(format!("project lookup failed: {detail}"))))?;

        for (index, attachment) in attachments.iter().enumerate() {
            self.upload_attachment(attachment, &project.org_id).map_err(|detail| {
                Error::new(ErrorKind::Attachment(format!(
                    "attachment {}/{} {} (key {}): {detail}; {} earlier upload(s) completed; no log rows inserted",
                    index + 1,
                    attachments.len(),
                    attachment.path,
                    attachment.key,
                    index,
                )))
            })?;
            tracing::info!(
                key = %attachment.key,
                path = %attachment.path,
                uploaded = index + 1,
                total = attachments.len(),
                "attachment uploaded",
            );
        }
        Ok(())
    }

    fn upload_attachment(&self, attachment: &Attachment, org_id: &str) -> Result<(), String> {
        let bytes = fs::read(&attachment.path).map_err(|source| format!("file read failed: {source}"))?;
        let api_url = self.config.api_url.trim_end_matches('/');
        let response = self
            .client
            .post(format!("{api_url}/attachment"))
            .bearer_auth(&self.config.api_key)
            .json(&serde_json::json!({
                "key": attachment.key,
                "filename": attachment.filename,
                "content_type": attachment.content_type,
                "org_id": org_id,
            }))
            .send()
            .map_err(|source| {
                format!(
                    "initialization outcome unknown; request was not retried: {}",
                    source.without_url()
                )
            })?;
        let init: AttachmentResponse = successful_attachment_response(response)
            .and_then(|response| {
                response
                    .json()
                    .map_err(|source| format!("initialization outcome unknown: invalid response: {}", source.without_url()))
            })
            .map_err(|detail| format!("initialization failed: {detail}"))?;

        let mut upload = self.client.put(&init.signed_url);
        for (name, value) in init.headers {
            upload = upload.header(name, value);
        }
        let expected_bytes = bytes.len() as u64;
        match upload.body(bytes).send() {
            Ok(response) if response.status().is_success() => {}
            Ok(response) if is_definite_upload_rejection(response.status()) => {
                let failure = format!("signed upload rejected with HTTP {}", response.status());
                let status_result = self.set_attachment_status(attachment, org_id, "error", Some(&failure));
                return Err(match status_result {
                    Ok(()) => format!("{failure}; Braintrust marked the attachment as error"),
                    Err(status_error) => format!("{failure}; error status update also failed: {status_error}"),
                });
            }
            Ok(response) => {
                let failure = format!("signed upload returned HTTP {}", response.status());
                self.reconcile_attachment(attachment, org_id, expected_bytes)
                    .map_err(|error| describe_reconciliation_error(&failure, error))?;
            }
            Err(source) => {
                let failure = format!("signed upload request failed: {}", source.without_url());
                self.reconcile_attachment(attachment, org_id, expected_bytes)
                    .map_err(|error| describe_reconciliation_error(&failure, error))?;
            }
        }

        self.set_attachment_status(attachment, org_id, "done", None)
            .map_err(|detail| format!("file uploaded, but completion status update failed: {detail}"))?;
        Ok(())
    }

    fn reconcile_attachment(&self, attachment: &Attachment, org_id: &str, expected_bytes: u64) -> Result<(), ReconcileError> {
        let url = format!("{}/attachment", self.config.api_url.trim_end_matches('/'));
        let mut last_result = String::from("no attachment metadata received");
        for attempt in 1..=MAX_ATTACHMENT_RECONCILE_ATTEMPTS {
            let response = self
                .client
                .get(&url)
                .bearer_auth(&self.config.api_key)
                .query(&[
                    ("key", attachment.key.as_str()),
                    ("filename", attachment.filename.as_str()),
                    ("content_type", attachment.content_type.as_str()),
                    ("org_id", org_id),
                ])
                .send();
            last_result = match response {
                Ok(response) => match successful_attachment_response(response).and_then(|response| {
                    response
                        .json::<AttachmentMetadata>()
                        .map_err(|source| source.without_url().to_string())
                }) {
                    Ok(metadata) => {
                        if metadata.status.upload_status == "error" {
                            return Err(ReconcileError::ReportedError(safe_attachment_detail(
                                &metadata
                                    .status
                                    .error_message
                                    .unwrap_or_else(|| "no detail provided".to_owned()),
                            )));
                        }
                        if let Some(actual) = metadata.content_length {
                            if actual != expected_bytes {
                                return Err(ReconcileError::Unknown(format!(
                                    "GET /attachment reports a stored object of {actual} bytes, expected {expected_bytes}"
                                )));
                            }
                        }
                        if metadata.content_length == Some(expected_bytes)
                            || metadata.download_url.is_some_and(|url| !url.is_empty())
                        {
                            return Ok(());
                        }
                        format!(
                            "Braintrust reports attachment status {} without verifiable object metadata",
                            metadata.status.upload_status
                        )
                    }
                    Err(detail) => detail,
                },
                Err(source) => source.without_url().to_string(),
            };
            if attempt < MAX_ATTACHMENT_RECONCILE_ATTEMPTS {
                thread::sleep(self.backoff_base);
            }
        }
        Err(ReconcileError::Unknown(format!(
            "GET /attachment did not confirm the uploaded object after {MAX_ATTACHMENT_RECONCILE_ATTEMPTS} attempts: {last_result}"
        )))
    }

    fn set_attachment_status(
        &self,
        attachment: &Attachment,
        org_id: &str,
        upload_status: &str,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        let url = format!("{}/attachment/status", self.config.api_url.trim_end_matches('/'));
        let status = match error_message {
            Some(message) => serde_json::json!({ "upload_status": upload_status, "error_message": message }),
            None => serde_json::json!({ "upload_status": upload_status }),
        };
        for attempt in 1..=MAX_ATTACHMENT_STATUS_ATTEMPTS {
            let result = self
                .client
                .post(&url)
                .bearer_auth(&self.config.api_key)
                .json(&serde_json::json!({ "key": attachment.key, "org_id": org_id, "status": status }))
                .send();
            let (retry, failure) = match result {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    let retry = response.status().is_server_error() || response.status() == StatusCode::TOO_MANY_REQUESTS;
                    (retry, attachment_http_error(response))
                }
                Err(source) => {
                    let retry = source.is_timeout() || source.is_connect();
                    (retry, source.without_url().to_string())
                }
            };
            if !retry || attempt == MAX_ATTACHMENT_STATUS_ATTEMPTS {
                return Err(format!(
                    "POST /attachment/status failed after {attempt} attempt(s): {failure}"
                ));
            }
            thread::sleep(self.backoff_base);
        }
        unreachable!("the final status attempt either succeeds or returns an error")
    }

    // pulls the next unsent payload until the queue drains or any worker fails
    fn drain(
        &self,
        url: &str,
        pending: &Mutex<Vec<(usize, Payload)>>,
        slots: &Mutex<Vec<Option<Vec<String>>>>,
        failed: &AtomicBool,
    ) -> Result<(), Error> {
        loop {
            // fail fast: leave remaining payloads unsent once any worker errors
            if failed.load(Ordering::Relaxed) {
                return Ok(());
            }
            let Some((index, payload)) = pending.lock().unwrap().pop() else {
                return Ok(());
            };
            match self.write_payload(url, payload) {
                Ok(row_ids) => slots.lock().unwrap()[index] = Some(row_ids),
                Err(error) => {
                    failed.store(true, Ordering::Relaxed);
                    return Err(error);
                }
            }
        }
    }

    fn write_payload(&self, url: &str, payload: Payload) -> Result<Vec<String>, Error> {
        let mut row_ids = Vec::with_capacity(payload.event_count());
        let mut pending = vec![payload];

        while let Some(payload) = pending.pop() {
            let sent = tracing::info_span!("insert", events = payload.event_count(), bytes = payload.body_len)
                .in_scope(|| self.send_with_retry(url, &payload));
            match sent {
                Ok(inserted) => row_ids.extend(inserted.row_ids.into_vec()),
                // the server's effective limit can sit below our cap; split and retry until it takes
                Err(error) if error.is_payload_too_large() && payload.event_count() > 1 => {
                    tracing::warn!(
                        events = payload.event_count(),
                        bytes = payload.body_len,
                        "payload rejected as too large, splitting and retrying",
                    );
                    let (left, right) = payload.split();
                    pending.push(right);
                    pending.push(left);
                }
                Err(error) => return Err(error),
            }
        }

        Ok(row_ids)
    }

    fn send_with_retry(&self, url: &str, payload: &Payload) -> Result<InsertResponse, Error> {
        let mut backoff = self.backoff_base;

        for attempt in 1..=MAX_SEND_ATTEMPTS {
            let error = match self.send(url, payload) {
                Ok(inserted) => return Ok(inserted),
                Err(error) => error,
            };
            if !error.is_transient() {
                return Err(error);
            }
            if attempt == MAX_SEND_ATTEMPTS {
                return Err(Error::new(ErrorKind::RetriesExhausted {
                    attempts: attempt,
                    source: Box::new(error),
                }));
            }
            let delay = error.retry_after().unwrap_or(backoff).min(MAX_BACKOFF);
            tracing::warn!(
                attempt,
                max_attempts = MAX_SEND_ATTEMPTS,
                delay = ?delay,
                events = payload.event_count(),
                %error,
                "transient insert failure, backing off and retrying",
            );
            thread::sleep(delay);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }

        unreachable!("the final attempt either returns its result or the exhausted error")
    }

    fn send(&self, url: &str, payload: &Payload) -> Result<InsertResponse, Error> {
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.config.api_key)
            .header(CONTENT_TYPE, "application/json")
            .body(payload.body())
            .send()
            .map_err(|error| Error::new(ErrorKind::SendRequest(error)))?;
        let status = response.status();

        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs);
            let body = response
                .text()
                .unwrap_or_else(|error| format!("failed to read response body: {error}"));
            return Err(Error::new(ErrorKind::Rejected {
                status,
                body,
                retry_after,
            }));
        }

        let inserted: InsertResponse = response
            .json()
            .map_err(|error| Error::new(ErrorKind::DecodeResponse(error)))?;

        // partial ack means braintrust dropped events, report failure not a fake success
        if inserted.row_count() != payload.event_count() {
            return Err(Error::new(ErrorKind::UnexpectedRowCount {
                expected: payload.event_count(),
                actual: inserted.row_count(),
            }));
        }

        Ok(inserted)
    }
}

fn successful_attachment_response(response: Response) -> Result<Response, String> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(attachment_http_error(response))
    }
}

fn describe_reconciliation_error(failure: &str, error: ReconcileError) -> String {
    match error {
        ReconcileError::ReportedError(detail) => format!("{failure}; Braintrust reports attachment error: {detail}"),
        ReconcileError::Unknown(detail) => {
            format!("{failure}; upload outcome unknown: {detail}; attachment status was left unchanged")
        }
    }
}

fn is_definite_upload_rejection(status: StatusCode) -> bool {
    status.is_client_error()
        && !matches!(
            status,
            StatusCode::REQUEST_TIMEOUT
                | StatusCode::CONFLICT
                | StatusCode::PRECONDITION_FAILED
                | StatusCode::TOO_MANY_REQUESTS
        )
}

fn safe_attachment_detail(detail: &str) -> String {
    if detail.contains("http://") || detail.contains("https://") {
        "response includes a URL; detail omitted".to_owned()
    } else {
        detail.chars().take(MAX_ATTACHMENT_ERROR_CHARS).collect()
    }
}

fn attachment_http_error(response: Response) -> String {
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let detail = safe_attachment_detail(&body);
    if detail.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {detail}")
    }
}

#[derive(Debug)]
struct Payload {
    events: Vec<Vec<u8>>,
    body_len: usize,
}

impl Payload {
    fn new(events: Vec<Vec<u8>>) -> Self {
        let body_len = PAYLOAD_OPEN.len()
            + PAYLOAD_CLOSE.len()
            + events.iter().map(Vec::len).sum::<usize>()
            + events.len().saturating_sub(1);

        Self { events, body_len }
    }

    fn event_count(&self) -> usize {
        self.events.len()
    }

    fn fits(&self, encoded_length: usize, limit: usize) -> bool {
        let separator = usize::from(!self.events.is_empty());
        self.body_len + separator + encoded_length <= limit
    }

    fn push(&mut self, encoded: Vec<u8>) {
        self.body_len += usize::from(!self.events.is_empty()) + encoded.len();
        self.events.push(encoded);
    }

    fn split(mut self) -> (Self, Self) {
        let right = self.events.split_off(self.events.len() / 2);
        (Self::new(self.events), Self::new(right))
    }

    fn body(&self) -> Vec<u8> {
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

// greedily packs serialized events into payloads that stay under the byte limit
fn payloads(events: &EventBatch, limit: usize) -> Result<Vec<Payload>, Error> {
    let _span = tracing::info_span!("pack").entered();
    let mut payloads = Vec::new();
    let mut current = Payload::new(Vec::new());

    for event in &events.events {
        let encoded = serde_json::to_vec(event).map_err(|error| Error::new(ErrorKind::EncodeEvent(error)))?;

        // a single event that cannot fit in an empty payload can never be sent
        if PAYLOAD_OPEN.len() + encoded.len() + PAYLOAD_CLOSE.len() > limit {
            return Err(Error::new(ErrorKind::EventTooLarge {
                size: encoded.len(),
                limit,
            }));
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

pub(super) fn write(config: &Braintrust, events: &EventBatch) -> Result<InsertResponse, Error> {
    Writer::new(config)?.write(events)
}

// what a write would send, without sending it
pub(crate) struct PackStats {
    pub(crate) payload_count: usize,
    pub(crate) body_bytes: usize,
}

pub(super) fn pack_stats(events: &EventBatch) -> Result<PackStats, Error> {
    let payloads = payloads(events, MAX_PAYLOAD_BYTES)?;

    Ok(PackStats {
        payload_count: payloads.len(),
        body_bytes: payloads.iter().map(|payload| payload.body_len).sum(),
    })
}

#[derive(Debug)]
pub(crate) struct Error {
    kind: ErrorKind,
}

#[derive(Debug)]
enum ErrorKind {
    Attachment(String),
    InsertAfterAttachments {
        uploaded: usize,
        source: Box<Error>,
    },
    BuildClient(reqwest::Error),
    EncodeEvent(serde_json::Error),
    EventTooLarge {
        size: usize,
        limit: usize,
    },
    SendRequest(reqwest::Error),
    Rejected {
        status: StatusCode,
        body: String,
        retry_after: Option<Duration>,
    },
    DecodeResponse(reqwest::Error),
    UnexpectedRowCount {
        expected: usize,
        actual: usize,
    },
    RetriesExhausted {
        attempts: u32,
        source: Box<Error>,
    },
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attachment(message) => formatter.write_str(message),
            Self::InsertAfterAttachments { uploaded, source } => write!(
                formatter,
                "{uploaded} attachment(s) uploaded, but log insertion failed: {source}; some log rows may have been inserted"
            ),
            Self::BuildClient(source) => write!(formatter, "failed to build HTTP client: {source}"),
            Self::EncodeEvent(source) => write!(formatter, "failed to encode an event as JSON: {source}"),
            Self::EventTooLarge { size, limit } => {
                write!(
                    formatter,
                    "a single event of {size} bytes exceeds the {limit} byte payload limit"
                )
            }
            Self::SendRequest(source) => write!(formatter, "failed to send request: {source}"),
            Self::Rejected { status, body, .. } => {
                write!(formatter, "Braintrust rejected the request with {status}: {body}")
            }
            Self::DecodeResponse(source) => write!(formatter, "failed to decode Braintrust response: {source}"),
            Self::UnexpectedRowCount { expected, actual } => {
                write!(
                    formatter,
                    "Braintrust acknowledged {actual} rows, but {expected} events were submitted"
                )
            }
            Self::RetriesExhausted { attempts, source } => {
                write!(formatter, "insert failed after {attempts} attempts: {source}")
            }
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
    fn new(kind: ErrorKind) -> Self {
        Self { kind }
    }

    fn is_payload_too_large(&self) -> bool {
        matches!(&self.kind, ErrorKind::Rejected { status, .. } if *status == StatusCode::PAYLOAD_TOO_LARGE)
    }

    // worth retrying: the request never got a verdict, the server was overloaded, or it failed
    // internally; 4xx rejections (including 413, which the split path handles) are final
    fn is_transient(&self) -> bool {
        match &self.kind {
            ErrorKind::SendRequest(source) => source.is_timeout() || source.is_connect(),
            ErrorKind::Rejected { status, .. } => status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS,
            _ => false,
        }
    }

    fn retry_after(&self) -> Option<Duration> {
        match &self.kind {
            ErrorKind::Rejected { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    #[cfg(test)]
    fn kind(&self) -> &ErrorKind {
        &self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;
    use crate::sdg::{
        materializer::{Distribution, Event, SpanAttributes, materialize},
        planner::plan,
    };
    use serde_json::{Map as JsonMap, Value as JsonValue};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, SystemTime};
    use uuid::Uuid;

    #[test]
    fn writes_events_to_braintrust() {
        let project_id = Uuid::new_v4();
        let response = serde_json::json!({ "row_ids": ["1", "2", "3", "4", "5"] }).to_string();
        let (api_url, request) = serve_once(StatusCode::OK, response);
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(1);
        let model = compile(include_str!("../../tests/fixtures/simple.bt")).unwrap();
        let events = materialize(
            plan(model, 1, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            SystemTime::now(),
        )
        .unwrap();

        let inserted = write(&config, &events).unwrap();
        let request = request.recv_timeout(Duration::from_secs(1)).unwrap();
        let (headers, body) = split_request(&request);
        let payload: JsonValue = serde_json::from_slice(body).unwrap();

        assert!(headers.starts_with(&format!("POST /v1/project_logs/{project_id}/insert HTTP/1.1\r\n")));
        assert!(headers.to_ascii_lowercase().contains("authorization: bearer secret\r\n"));
        assert_eq!(payload["events"].as_array().unwrap().len(), 5);
        assert_eq!(inserted.row_ids.as_ref(), ["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn preserves_rejected_response_details() {
        let project_id = Uuid::new_v4();
        let (api_url, _request) = serve_once(StatusCode::BAD_REQUEST, r#"{"error":"invalid event"}"#.to_owned());
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(1);
        let model = compile(r#"trace "example" {}"#).unwrap();
        let events = materialize(
            plan(model, 1, 0).unwrap(),
            Duration::from_secs(3_600),
            Distribution::Linear,
            SystemTime::now(),
        )
        .unwrap();

        let error = write(&config, &events).unwrap_err();

        assert!(matches!(
            error.kind(),
            ErrorKind::Rejected { status, body, .. }
                if *status == StatusCode::BAD_REQUEST && body == r#"{"error":"invalid event"}"#
        ));
    }

    #[test]
    fn packs_events_into_payloads_under_the_limit() {
        let limit = 600;
        let events = event_batch(10, 0);

        let payloads = payloads(&events, limit).unwrap();

        assert!(payloads.len() > 1);
        let mut ids = Vec::new();
        for payload in &payloads {
            let body = payload.body();
            assert!(body.len() <= limit);
            let parsed: JsonValue = serde_json::from_slice(&body).unwrap();
            let batch = parsed["events"].as_array().unwrap();
            assert_eq!(batch.len(), payload.event_count());
            ids.extend(batch.iter().map(|event| event["id"].as_str().unwrap().to_owned()));
        }
        let expected: Vec<_> = (0..10).map(|index| format!("event-{index}")).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn rejects_events_larger_than_the_payload_limit() {
        let events = event_batch(1, 300);

        let error = payloads(&events, 200).unwrap_err();

        assert!(matches!(error.kind(), ErrorKind::EventTooLarge { size: _, limit: 200 }));
    }

    #[test]
    fn splits_writes_across_payloads() {
        let project_id = Uuid::new_v4();
        let (api_url, requests) = serve(vec![
            (StatusCode::OK, serde_json::json!({ "row_ids": ["1", "2"] }).to_string()),
            (StatusCode::OK, serde_json::json!({ "row_ids": ["3"] }).to_string()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(5);
        // one worker so the payloads arrive in order; the fake server replies by arrival
        config.write_concurrency = 1;
        // three ~1MiB events: two fit under the 2.5MiB cap, the third spills into a second payload
        let events = event_batch(3, 1024 * 1024);

        let inserted = write(&config, &events).unwrap();

        assert_eq!(inserted.row_ids.as_ref(), ["1", "2", "3"]);
        for expected_count in [2, 1] {
            let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            let (_, body) = split_request(&request);
            assert!(body.len() <= MAX_PAYLOAD_BYTES);
            let payload: JsonValue = serde_json::from_slice(body).unwrap();
            assert_eq!(payload["events"].as_array().unwrap().len(), expected_count);
        }
    }

    #[test]
    fn writes_payloads_concurrently_and_preserves_event_order() {
        let project_id = Uuid::new_v4();
        // eight ~1MiB events pack into four payloads, written by four workers at once
        let api_url = serve_matching(4);
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(5);
        let events = event_batch(8, 1024 * 1024);

        let inserted = write(&config, &events).unwrap();

        let expected: Vec<_> = (0..8).map(|index| format!("event-{index}")).collect();
        assert_eq!(inserted.row_ids.as_ref(), expected.as_slice());
    }

    #[test]
    fn splits_and_retries_when_the_server_rejects_a_payload_as_too_large() {
        let project_id = Uuid::new_v4();
        let (api_url, requests) = serve(vec![
            (StatusCode::PAYLOAD_TOO_LARGE, r#"{"message": "Request Too Long"}"#.to_owned()),
            (StatusCode::OK, serde_json::json!({ "row_ids": ["1"] }).to_string()),
            (StatusCode::OK, serde_json::json!({ "row_ids": ["2"] }).to_string()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(5);
        let events = event_batch(2, 64);

        let inserted = write(&config, &events).unwrap();

        assert_eq!(inserted.row_ids.as_ref(), ["1", "2"]);
        // one rejected request for the pair, then one per half in original order
        for expected_ids in [vec!["event-0", "event-1"], vec!["event-0"], vec!["event-1"]] {
            let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
            let (_, body) = split_request(&request);
            let payload: JsonValue = serde_json::from_slice(body).unwrap();
            let ids: Vec<_> = payload["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["id"].as_str().unwrap())
                .collect();
            assert_eq!(ids, expected_ids);
        }
    }

    #[test]
    fn retries_transient_failures_until_success() {
        let project_id = Uuid::new_v4();
        let (api_url, requests) = serve(vec![
            (StatusCode::SERVICE_UNAVAILABLE, "{}".to_owned()),
            (StatusCode::OK, serde_json::json!({ "row_ids": ["1", "2", "3"] }).to_string()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(5);
        let mut writer = Writer::new(&config).unwrap();
        writer.backoff_base = Duration::from_millis(1);
        let events = event_batch(3, 0);

        let inserted = writer.write(&events).unwrap();

        assert_eq!(inserted.row_ids.as_ref(), ["1", "2", "3"]);
        // the rejected attempt and the successful retry both reached the server
        for _ in 0..2 {
            requests.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }

    #[test]
    fn gives_up_after_exhausting_retries() {
        let project_id = Uuid::new_v4();
        let (api_url, _requests) = serve(vec![(StatusCode::BAD_GATEWAY, "{}".to_owned()); 4]);
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(5);
        let mut writer = Writer::new(&config).unwrap();
        writer.backoff_base = Duration::from_millis(1);
        let events = event_batch(1, 0);

        let error = writer.write(&events).unwrap_err();

        assert!(matches!(error.kind(), ErrorKind::RetriesExhausted { attempts: 4, .. }));
        assert!(error.to_string().contains("after 4 attempts"));
    }

    #[test]
    fn does_not_retry_final_rejections() {
        let project_id = Uuid::new_v4();
        let (api_url, requests) = serve_once(StatusCode::UNPROCESSABLE_ENTITY, "{}".to_owned());
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(5);
        let mut writer = Writer::new(&config).unwrap();
        writer.backoff_base = Duration::from_millis(1);
        let events = event_batch(1, 0);

        let error = writer.write(&events).unwrap_err();

        assert!(matches!(error.kind(), ErrorKind::Rejected { .. }));
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        // a second request would mean the 4xx was retried
        assert!(requests.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn surfaces_the_rejection_when_a_single_event_payload_is_too_large() {
        let project_id = Uuid::new_v4();
        let (api_url, _requests) = serve_once(StatusCode::PAYLOAD_TOO_LARGE, r#"{"message": "Request Too Long"}"#.to_owned());
        let mut config = Braintrust::new("secret".to_owned(), project_id);
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(5);
        let events = event_batch(1, 64);

        let error = write(&config, &events).unwrap_err();

        assert!(error.is_payload_too_large());
    }

    #[test]
    fn rejected_attachment_upload_reports_error_status_and_skips_insert() {
        let (file, events) = attachment_batch();
        let org_id = Uuid::new_v4();
        let (api_url, requests) = serve(vec![
            (
                StatusCode::OK,
                serde_json::json!({ "org_id": org_id.to_string() }).to_string(),
            ),
            (StatusCode::OK, attachment_init_response()),
            (StatusCode::FORBIDDEN, "signed-url-secret".to_owned()),
            (StatusCode::OK, "{}".to_owned()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;

        let error = write(&config, &events).unwrap_err().to_string();
        fs::remove_file(file).unwrap();

        assert!(error.contains("signed upload rejected with HTTP 403"), "{error}");
        assert!(error.contains(&events.attachments[0].path), "{error}");
        assert!(error.contains("no log rows inserted"), "{error}");
        assert!(!error.contains("signed-url-secret"), "{error}");
        for _ in 0..3 {
            requests.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let (headers, body) = split_request(&request);
        assert!(headers.starts_with("POST /attachment/status HTTP/1.1\r\n"));
        let status: JsonValue = serde_json::from_slice(body).unwrap();
        assert_eq!(status["status"]["upload_status"], "error");
        assert!(status["status"]["error_message"].as_str().unwrap().contains("HTTP 403"));
    }

    #[test]
    fn attachment_initialization_conflict_is_reported_without_retry() {
        let (file, events) = attachment_batch();
        let (api_url, requests) = serve(vec![
            (
                StatusCode::OK,
                serde_json::json!({ "org_id": Uuid::new_v4().to_string() }).to_string(),
            ),
            (StatusCode::CONFLICT, r#"{"error":"key already exists"}"#.to_owned()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;

        let error = write(&config, &events).unwrap_err().to_string();
        fs::remove_file(file).unwrap();

        assert!(error.contains("initialization failed: HTTP 409"), "{error}");
        assert!(error.contains("key already exists"), "{error}");
        assert!(error.contains("no log rows inserted"), "{error}");
        requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(split_request(&request).0.starts_with("POST /attachment HTTP/1.1\r\n"));
    }

    #[test]
    fn reconciles_an_uncertain_upload_before_inserting_logs() {
        let (file, events) = attachment_batch();
        let bytes = fs::metadata(&file).unwrap().len();
        let org_id = Uuid::new_v4();
        let (api_url, requests) = serve(vec![
            (
                StatusCode::OK,
                serde_json::json!({ "org_id": org_id.to_string() }).to_string(),
            ),
            (StatusCode::OK, attachment_init_response()),
            (StatusCode::SERVICE_UNAVAILABLE, "{}".to_owned()),
            (
                StatusCode::OK,
                serde_json::json!({ "status": { "upload_status": "uploading" }, "contentLength": bytes }).to_string(),
            ),
            (StatusCode::OK, "{}".to_owned()),
            (StatusCode::OK, serde_json::json!({ "row_ids": ["row"] }).to_string()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;

        let inserted = write(&config, &events).unwrap();
        fs::remove_file(file).unwrap();

        assert_eq!(inserted.row_count(), 1);
        for _ in 0..3 {
            requests.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let (headers, _) = split_request(&request);
        assert!(headers.starts_with("GET /attachment?"));
        assert!(headers.contains("key="));
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        let (headers, body) = split_request(&request);
        assert!(headers.starts_with("POST /attachment/status HTTP/1.1\r\n"));
        let status: JsonValue = serde_json::from_slice(body).unwrap();
        assert_eq!(status["status"]["upload_status"], "done");
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(split_request(&request).0.starts_with("POST /v1/project_logs/"));
    }

    #[test]
    fn retries_transient_attachment_status_failures() {
        let (file, events) = attachment_batch();
        let org_id = Uuid::new_v4();
        let (api_url, requests) = serve(vec![
            (
                StatusCode::OK,
                serde_json::json!({ "org_id": org_id.to_string() }).to_string(),
            ),
            (StatusCode::OK, attachment_init_response()),
            (StatusCode::OK, "{}".to_owned()),
            (StatusCode::SERVICE_UNAVAILABLE, r#"{"error":"try again"}"#.to_owned()),
            (StatusCode::OK, "{}".to_owned()),
            (StatusCode::OK, serde_json::json!({ "row_ids": ["row"] }).to_string()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;
        let mut writer = Writer::new(&config).unwrap();
        writer.backoff_base = Duration::from_millis(1);

        let inserted = writer.write(&events).unwrap();
        fs::remove_file(file).unwrap();

        assert_eq!(inserted.row_count(), 1);
        for _ in 0..3 {
            requests.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        for _ in 0..2 {
            let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(split_request(&request).0.starts_with("POST /attachment/status HTTP/1.1\r\n"));
        }
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(split_request(&request).0.starts_with("POST /v1/project_logs/"));
    }

    #[test]
    fn completion_status_failure_stops_before_log_insertion() {
        let (file, events) = attachment_batch();
        let (api_url, requests) = serve(vec![
            (
                StatusCode::OK,
                serde_json::json!({ "org_id": Uuid::new_v4().to_string() }).to_string(),
            ),
            (StatusCode::OK, attachment_init_response()),
            (StatusCode::OK, "{}".to_owned()),
            (StatusCode::BAD_REQUEST, r#"{"error":"invalid status"}"#.to_owned()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;

        let error = write(&config, &events).unwrap_err().to_string();
        fs::remove_file(file).unwrap();

        assert!(
            error.contains("file uploaded, but completion status update failed"),
            "{error}"
        );
        assert!(error.contains("HTTP 400"), "{error}");
        assert!(error.contains("no log rows inserted"), "{error}");
        for _ in 0..3 {
            requests.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(split_request(&request).0.starts_with("POST /attachment/status HTTP/1.1\r\n"));
    }

    #[test]
    fn unresolved_upload_failure_does_not_mark_status_or_insert_logs() {
        let (file, events) = attachment_batch();
        let (api_url, requests) = serve(vec![
            (
                StatusCode::OK,
                serde_json::json!({ "org_id": Uuid::new_v4().to_string() }).to_string(),
            ),
            (StatusCode::OK, attachment_init_response()),
            (StatusCode::SERVICE_UNAVAILABLE, "{}".to_owned()),
            (StatusCode::NOT_FOUND, "{}".to_owned()),
            (StatusCode::NOT_FOUND, "{}".to_owned()),
            (StatusCode::NOT_FOUND, "{}".to_owned()),
        ]);
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;
        let mut writer = Writer::new(&config).unwrap();
        writer.backoff_base = Duration::from_millis(1);

        let error = writer.write(&events).unwrap_err().to_string();
        fs::remove_file(file).unwrap();

        assert!(error.contains("upload outcome unknown"), "{error}");
        assert!(error.contains("attachment status was left unchanged"), "{error}");
        assert!(error.contains("no log rows inserted"), "{error}");
        for _ in 0..3 {
            requests.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        for _ in 0..MAX_ATTACHMENT_RECONCILE_ATTEMPTS {
            let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(split_request(&request).0.starts_with("GET /attachment?"));
        }
    }

    fn attachment_batch() -> (std::path::PathBuf, EventBatch) {
        let file = std::env::temp_dir().join(format!("bts-attachment-failure-{}", Uuid::new_v4()));
        fs::write(&file, b"sample").unwrap();
        let mut events = event_batch(1, 0);
        events.attachments = vec![Attachment {
            path: file.display().to_string(),
            filename: file.file_name().unwrap().to_string_lossy().into_owned(),
            content_type: "application/octet-stream".to_owned(),
            key: Uuid::new_v4().to_string(),
        }]
        .into_boxed_slice();
        (file, events)
    }

    fn attachment_init_response() -> String {
        serde_json::json!({
            "signedUrl": "$API_URL/blob",
            "headers": { "If-None-Match": "*" },
        })
        .to_string()
    }

    fn event_batch(count: usize, input_bytes: usize) -> EventBatch {
        let events = (0..count)
            .map(|index| Event {
                id: format!("event-{index}"),
                span_id: format!("span-{index}"),
                root_span_id: "span-0".to_owned(),
                span_parents: Box::new([]),
                created: "2026-01-01T00:00:00Z".to_owned(),
                span_attributes: SpanAttributes {
                    name: "root".to_owned(),
                    kind: "task".to_owned(),
                },
                input: (input_bytes > 0).then(|| JsonValue::String("x".repeat(input_bytes))),
                output: None,
                expected: None,
                error: None,
                metadata: None,
                metrics: JsonMap::new(),
                tags: Box::new([]),
            })
            .collect();

        EventBatch {
            attachments: Box::new([]),
            events,
            trace_count: count,
        }
    }

    fn serve_once(status: StatusCode, body: String) -> (String, Receiver<Vec<u8>>) {
        serve(vec![(status, body)])
    }

    fn serve(responses: Vec<(StatusCode, String)>) -> (String, Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                sender.send(request).unwrap();

                let body = body.replace("$API_URL", &format!("http://{address}"));
                let reason = status.canonical_reason().unwrap_or("Unknown");
                write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status.as_u16(),
                    reason,
                    body.len(),
                    body,
                )
                .unwrap();
            }
        });

        (format!("http://{address}"), receiver)
    }

    // acknowledges each request with the event ids it carried, so concurrent
    // requests get the right response regardless of arrival order
    fn serve_matching(connections: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        thread::spawn(move || {
            for _ in 0..connections {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                let (_, body) = split_request(&request);
                let payload: JsonValue = serde_json::from_slice(body).unwrap();
                let row_ids: Vec<_> = payload["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|event| event["id"].clone())
                    .collect();
                let body = serde_json::json!({ "row_ids": row_ids }).to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                )
                .unwrap();
            }
        });

        format!("http://{address}")
    }

    fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];

        loop {
            let read = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..read]);

            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let body_start = header_end + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .map(str::to_owned)
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();

            if request.len() >= body_start + content_length {
                return request;
            }
        }
    }

    fn split_request(request: &[u8]) -> (&str, &[u8]) {
        let header_end = request.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
        let headers = std::str::from_utf8(&request[..header_end + 4]).unwrap();
        (headers, &request[header_end + 4..])
    }
}
