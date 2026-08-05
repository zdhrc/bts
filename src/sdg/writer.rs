use crate::{conf::Braintrust, sdg::materializer::EventBatch};
use reqwest::{StatusCode, blocking::Client};
use serde::Deserialize;
use thiserror::Error as Err;

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
}

impl<'config> Writer<'config> {
    fn new(config: &'config Braintrust) -> Result<Self, Error> {
        let client = Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| Error::new(ErrorKind::BuildClient(error)))?;

        Ok(Self { client, config })
    }

    fn write(&self, events: &EventBatch) -> Result<InsertResponse, Error> {
        let url = format!(
            "{}/v1/project_logs/{}/insert",
            self.config.api_url.trim_end_matches('/'),
            self.config.project_id,
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.config.api_key)
            .json(events)
            .send()
            .map_err(|error| Error::new(ErrorKind::SendRequest(error)))?;
        let status = response.status();

        if !status.is_success() {
            let body = response
                .text()
                .unwrap_or_else(|error| format!("failed to read response body: {error}"));
            return Err(Error::new(ErrorKind::Rejected { status, body }));
        }

        let inserted: InsertResponse = response
            .json()
            .map_err(|error| Error::new(ErrorKind::DecodeResponse(error)))?;

        // partial ack means braintrust dropped events, report failure not a fake success
        if inserted.row_count() != events.event_count() {
            return Err(Error::new(ErrorKind::UnexpectedRowCount {
                expected: events.event_count(),
                actual: inserted.row_count(),
            }));
        }

        Ok(inserted)
    }
}

pub(super) fn write(config: &Braintrust, events: &EventBatch) -> Result<InsertResponse, Error> {
    Writer::new(config)?.write(events)
}

#[derive(Debug, Err)]
#[error("{kind}")]
pub(crate) struct Error {
    kind: ErrorKind,
}

#[derive(Debug, Err)]
enum ErrorKind {
    #[error("failed to build HTTP client")]
    BuildClient(#[source] reqwest::Error),

    #[error("failed to send request")]
    SendRequest(#[source] reqwest::Error),

    #[error("Braintrust rejected the request with {status}: {body}")]
    Rejected { status: StatusCode, body: String },

    #[error("failed to decode Braintrust response")]
    DecodeResponse(#[source] reqwest::Error),

    #[error("Braintrust acknowledged {actual} rows, but {expected} events were submitted")]
    UnexpectedRowCount { expected: usize, actual: usize },
}

impl Error {
    fn new(kind: ErrorKind) -> Self {
        Self { kind }
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
        materializer::{Distribution, materialize},
        planner::plan,
    };
    use serde_json::Value as JsonValue;
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
            ErrorKind::Rejected { status, body }
                if *status == StatusCode::BAD_REQUEST && body == r#"{"error":"invalid event"}"#
        ));
    }

    fn serve_once(status: StatusCode, body: String) -> (String, Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
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
