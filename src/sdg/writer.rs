use crate::{conf::Braintrust, sdg::materializer::EventBatch};
use reqwest::header::{CONTENT_TYPE, RETRY_AFTER};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;
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
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
pub(crate) struct InsertResponse {
    row_ids: Box<[String]>,
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

    fn write(&self, events: &EventBatch) -> Result<InsertResponse, Error> {
        let url = format!(
            "{}/v1/project_logs/{}/insert",
            self.config.api_url.trim_end_matches('/'),
            self.config.project_id,
        );
        let mut row_ids = Vec::with_capacity(events.event_count());
        let mut pending = payloads(events, MAX_PAYLOAD_BYTES)?;
        pending.reverse();

        while let Some(payload) = pending.pop() {
            let sent = tracing::info_span!("insert", events = payload.event_count(), bytes = payload.body_len)
                .in_scope(|| self.send_with_retry(&url, &payload));
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

        Ok(InsertResponse {
            row_ids: row_ids.into_boxed_slice(),
        })
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
