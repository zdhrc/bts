use super::component::{Component, Payload};
use crate::conf::Braintrust;
use crate::dsl::ScorerLang;
use reqwest::StatusCode;
use reqwest::blocking::{Client as HttpClient, RequestBuilder, Response};
use reqwest::header::RETRY_AFTER;
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use std::{fmt, thread, time::Duration};
use uuid::Uuid;

// retry policy mirrors sdg/writer.rs; extract a shared http module when a
// third consumer appears
const MAX_SEND_ATTEMPTS: u32 = 4;
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

// runtimes stamped into pushed code functions; keep in sync with what the
// braintrust playground creates
const NODE_VERSION: &str = "18";
const PYTHON_VERSION: &str = "3.11";

pub(super) struct Client {
    http: HttpClient,
    api_url: String,
    api_key: String,
    project_id: Uuid,
    backoff_base: Duration,
}

// the fields reconciliation reads; remote objects carry many more
#[derive(Debug, Deserialize)]
pub(crate) struct RemoteFunction {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) slug: String,
    #[serde(default)]
    pub(crate) function_data: JsonValue,
    #[serde(default)]
    pub(crate) prompt_data: Option<JsonValue>,
}

#[derive(Deserialize)]
struct Listing {
    objects: Vec<RemoteFunction>,
}

impl Client {
    pub(super) fn new(config: &Braintrust) -> Result<Self, Error> {
        let http = HttpClient::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|source| Error::new(ErrorKind::BuildClient(source)))?;

        Ok(Self {
            http,
            api_url: config.api_url.clone(),
            api_key: config.api_key.clone(),
            project_id: config.project_id,
            backoff_base: BACKOFF_BASE,
        })
    }

    // the function with this slug in the project, if one exists
    pub(super) fn lookup(&self, slug: &str) -> Result<Option<RemoteFunction>, Error> {
        let url = format!("{}/v1/function", self.api_url);
        let project_id = self.project_id.to_string();
        let response = self.send_with_retry(|| {
            self.http
                .get(&url)
                .query(&[("project_id", project_id.as_str()), ("slug", slug), ("limit", "1")])
        })?;
        let listing: Listing = response
            .json()
            .map_err(|source| Error::new(ErrorKind::DecodeResponse(source)))?;

        // guard against substring filtering; the slug must match exactly
        Ok(listing.objects.into_iter().find(|remote| remote.slug == slug))
    }

    // create-or-replace keyed on (project_id, slug)
    pub(super) fn upsert(&self, component: &Component) -> Result<RemoteFunction, Error> {
        let url = format!("{}/v1/function", self.api_url);
        let body = self.function_body(component);
        let response = self.send_with_retry(|| self.http.put(&url).json(&body))?;
        response
            .json()
            .map_err(|source| Error::new(ErrorKind::DecodeResponse(source)))
    }

    fn function_body(&self, component: &Component) -> JsonValue {
        let mut body = json!({
            "project_id": self.project_id.to_string(),
            "name": component.name,
            "slug": component.slug,
            "function_type": component.kind.function_type(),
        });

        match &component.payload {
            Payload::Code { lang, code } => {
                let (runtime, version) = match lang {
                    ScorerLang::Python => ("python", PYTHON_VERSION),
                    ScorerLang::Typescript => ("node", NODE_VERSION),
                };
                body["function_data"] = json!({
                    "type": "code",
                    "data": {
                        "type": "inline",
                        "runtime_context": { "runtime": runtime, "version": version },
                        "code": code,
                    },
                });
            }
            Payload::Prompt {
                model,
                content,
                choice_scores,
                use_cot,
            } => {
                let scores: serde_json::Map<String, JsonValue> = choice_scores
                    .iter()
                    .map(|(label, score)| (label.clone(), json!(score)))
                    .collect();
                body["function_data"] = json!({ "type": "prompt" });
                body["prompt_data"] = json!({
                    "prompt": { "type": "chat", "messages": [{ "role": "user", "content": content }] },
                    "options": { "model": model },
                    "parser": { "type": "llm_classifier", "use_cot": use_cot, "choice_scores": scores },
                });
            }
        }

        body
    }

    fn send_with_retry(&self, build: impl Fn() -> RequestBuilder) -> Result<Response, Error> {
        let mut backoff = self.backoff_base;

        for attempt in 1..=MAX_SEND_ATTEMPTS {
            let error = match self.send(build()) {
                Ok(response) => return Ok(response),
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
                %error,
                "transient function request failure, backing off and retrying",
            );
            thread::sleep(delay);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }

        unreachable!("the final attempt either returns its result or the exhausted error")
    }

    fn send(&self, request: RequestBuilder) -> Result<Response, Error> {
        let response = request
            .bearer_auth(&self.api_key)
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

        Ok(response)
    }
}

#[derive(Debug)]
pub(crate) struct Error {
    kind: ErrorKind,
}

#[derive(Debug)]
enum ErrorKind {
    BuildClient(reqwest::Error),
    SendRequest(reqwest::Error),
    Rejected {
        status: StatusCode,
        body: String,
        retry_after: Option<Duration>,
    },
    DecodeResponse(reqwest::Error),
    RetriesExhausted {
        attempts: u32,
        source: Box<Error>,
    },
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuildClient(source) => write!(formatter, "failed to build HTTP client: {source}"),
            Self::SendRequest(source) => write!(formatter, "failed to send request: {source}"),
            Self::Rejected { status, body, .. } => {
                write!(formatter, "Braintrust rejected the request with {status}: {body}")
            }
            Self::DecodeResponse(source) => write!(formatter, "failed to decode Braintrust response: {source}"),
            Self::RetriesExhausted { attempts, source } => {
                write!(formatter, "request failed after {attempts} attempts: {source}")
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

    // worth retrying: the request never got a verdict, the server was
    // overloaded, or it failed internally; 4xx rejections are final
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scg::testutil;
    use serde_json::json;

    fn client(api_url: String) -> Client {
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(1);
        let mut client = Client::new(&config).unwrap();
        client.backoff_base = Duration::from_millis(1);
        client
    }

    #[test]
    fn retries_transient_failures_until_success() {
        let listing = json!({ "objects": [] }).to_string();
        let (api_url, _requests) = testutil::serve(vec![
            (StatusCode::SERVICE_UNAVAILABLE, "overloaded".to_owned()),
            (StatusCode::TOO_MANY_REQUESTS, "slow down".to_owned()),
            (StatusCode::OK, listing),
        ]);

        let found = client(api_url).lookup("missing").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn rejections_are_final() {
        let (api_url, _requests) = testutil::serve(vec![(StatusCode::BAD_REQUEST, "nope".to_owned())]);

        let error = client(api_url).lookup("missing").unwrap_err();
        assert!(matches!(
            error.kind,
            ErrorKind::Rejected { status, .. } if status == StatusCode::BAD_REQUEST
        ));
    }

    #[test]
    fn lookup_guards_against_substring_matches() {
        let listing = json!({
            "objects": [{ "id": "fn-1", "name": "other", "slug": "quality-v2" }]
        })
        .to_string();
        let (api_url, _requests) = testutil::serve(vec![(StatusCode::OK, listing)]);

        let found = client(api_url).lookup("quality").unwrap();
        assert!(found.is_none());
    }
}
