pub(crate) mod attachments;
pub(crate) mod writer;

use crate::conf::{Braintrust, Settings};
use reqwest::{
    Method, StatusCode,
    blocking::{Client as HttpClient, RequestBuilder as HttpRequest, Response as HttpResponse},
    header::RETRY_AFTER,
};
use serde::Serialize;
use std::{
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone)]
pub(crate) struct Client {
    http: HttpClient,
    pub(crate) config: Braintrust,
    state: Arc<(Mutex<State>, Condvar)>,
}
struct State {
    active: usize,
    cooldown: Instant,
}

impl Client {
    pub(crate) fn new(config: &Braintrust) -> Result<Self, reqwest::Error> {
        Ok(Self {
            http: HttpClient::builder().timeout(config.request_timeout).build()?,
            config: config.clone(),
            state: Arc::new((
                Mutex::new(State {
                    active: 0,
                    cooldown: Instant::now(),
                }),
                Condvar::new(),
            )),
        })
    }
    pub(crate) fn configured(mut config: Braintrust, settings: &Settings) -> Result<Self, reqwest::Error> {
        config.request_timeout = settings.request_timeout;
        config.write_concurrency = settings.write_concurrency;
        config.retry_attempts = settings.retry_attempts;
        config.retry_max_elapsed = settings.retry_max_elapsed;
        config.max_attachment_uploads = settings.max_attachment_uploads;
        config.max_attachment_file_bytes = settings.max_attachment_file_bytes;
        config.max_attachment_total_bytes = settings.max_attachment_total_bytes;
        Self::new(&config)
    }
    fn request(&self, method: Method, url: impl AsRef<str>) -> Request {
        let url = url.as_ref();
        let url = if url.starts_with('/') {
            format!("{}{}", self.config.api_url.trim_end_matches('/'), url)
        } else {
            url.to_owned()
        };
        let authenticated = match (reqwest::Url::parse(&url), reqwest::Url::parse(&self.config.api_url)) {
            (Ok(target), Ok(api)) => target.origin() == api.origin(),
            _ => false,
        };
        let safe = method == Method::GET || method == Method::HEAD;
        let mut inner = self.http.request(method, url);
        if authenticated {
            inner = inner.bearer_auth(&self.config.api_key);
        }
        Request {
            client: self.clone(),
            inner,
            safe,
            backoff: Duration::from_millis(500),
        }
    }
    pub(crate) fn get(&self, url: impl AsRef<str>) -> Request {
        self.request(Method::GET, url)
    }
    pub(crate) fn post(&self, url: impl AsRef<str>) -> Request {
        self.request(Method::POST, url)
    }
    pub(crate) fn put(&self, url: impl AsRef<str>) -> Request {
        self.request(Method::PUT, url)
    }
    pub(crate) fn signed_get(&self, url: &str) -> Request {
        Request {
            client: self.clone(),
            inner: self.http.get(url),
            safe: true,
            backoff: Duration::from_millis(500),
        }
    }
    pub(crate) fn signed_put(&self, url: &str) -> Request {
        Request {
            client: self.clone(),
            inner: self.http.put(url),
            safe: false,
            backoff: Duration::from_millis(500),
        }
    }
    fn acquire(&self, deadline: Instant) -> Result<Permit, Error> {
        let (lock, ready) = &*self.state;
        let mut state = lock.lock().unwrap();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Deadline);
            }
            let wait = state.cooldown.saturating_duration_since(Instant::now());
            if !wait.is_zero() {
                state = ready.wait_timeout(state, wait.min(remaining)).unwrap().0;
            } else if state.active < self.config.write_concurrency {
                state.active += 1;
                return Ok(Permit(self.state.clone()));
            } else {
                state = ready.wait_timeout(state, remaining).unwrap().0;
            }
        }
    }
    fn throttle(&self, delay: Duration) {
        let (lock, ready) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.cooldown = state.cooldown.max(Instant::now() + delay);
        ready.notify_all();
    }
}
struct Permit(Arc<(Mutex<State>, Condvar)>);
impl Drop for Permit {
    fn drop(&mut self) {
        let (lock, ready) = &*self.0;
        lock.lock().unwrap().active -= 1;
        ready.notify_all();
    }
}

pub(crate) struct Request {
    client: Client,
    inner: HttpRequest,
    safe: bool,
    backoff: Duration,
}
impl Request {
    pub(crate) fn retryable(mut self) -> Self {
        self.safe = true;
        self
    }
    pub(crate) fn retry_base(mut self, delay: Duration) -> Self {
        self.backoff = delay;
        self
    }
    pub(crate) fn query<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        self.inner = self.inner.query(value);
        self
    }
    pub(crate) fn json<T: Serialize + ?Sized>(mut self, value: &T) -> Self {
        self.inner = self.inner.json(value);
        self
    }
    pub(crate) fn header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.inner = self.inner.header(name.as_ref(), value.as_ref());
        self
    }
    pub(crate) fn body(mut self, body: Vec<u8>) -> Self {
        self.inner = self.inner.body(body);
        self
    }
    pub(crate) fn send(self) -> Result<Response, Error> {
        let start = Instant::now();
        for attempt in 1..=self.client.config.retry_attempts {
            let _permit = self.client.acquire(start + self.client.config.retry_max_elapsed)?;
            let request = self.inner.try_clone().expect("client requests use replayable bodies");
            let request = request.timeout(
                self.client
                    .config
                    .request_timeout
                    .min(self.client.config.retry_max_elapsed.saturating_sub(start.elapsed())),
            );
            let result = request.send();
            let (retry, server_delay, throttled) = match &result {
                Ok(response) => (
                    response.status() == StatusCode::TOO_MANY_REQUESTS
                        || (self.safe
                            && (response.status().is_server_error() || response.status() == StatusCode::REQUEST_TIMEOUT)),
                    retry_after(response),
                    response.status() == StatusCode::TOO_MANY_REQUESTS,
                ),
                Err(error) => (self.safe && (error.is_timeout() || error.is_connect()), None, false),
            };
            let base = self
                .backoff
                .saturating_mul(1 << (attempt - 1).min(10))
                .min(Duration::from_secs(30));
            let delay = server_delay.unwrap_or_else(|| base + Duration::from_millis(rand::random_range(0..=100)));
            if throttled {
                self.client.throttle(delay);
            }
            if !retry
                || attempt == self.client.config.retry_attempts
                || start.elapsed() + delay >= self.client.config.retry_max_elapsed
            {
                return result
                    .map(|inner| Response { inner, _permit })
                    .map_err(|error| Error::Http(error.without_url()));
            }
            tracing::warn!(attempt, ?delay, "Braintrust request will retry");
            drop(result);
            drop(_permit);
            if !throttled {
                thread::sleep(delay);
            }
        }
        unreachable!()
    }
}
fn retry_after(response: &HttpResponse) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds.min(86400.0)));
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| SystemTime::from(date).duration_since(SystemTime::now()).unwrap_or_default())
}

pub(crate) fn checked(response: Response, context: &str) -> Result<serde_json::Value, String> {
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "{context}: HTTP {status}: {}",
            response.text().unwrap_or_default().chars().take(500).collect::<String>()
        ));
    }
    response.json().map_err(|error| format!("{context}: {}", error.without_url()))
}

#[derive(Debug)]
pub(crate) enum Error {
    Http(reqwest::Error),
    Deadline,
}
impl Error {
    pub(crate) fn without_url(self) -> Self {
        match self {
            Self::Http(error) => Self::Http(error.without_url()),
            error => error,
        }
    }
    pub(crate) fn is_timeout(&self) -> bool {
        matches!(self, Self::Deadline) || matches!(self, Self::Http(error) if error.is_timeout())
    }
    pub(crate) fn is_connect(&self) -> bool {
        matches!(self, Self::Http(error) if error.is_connect())
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http(error) => write!(f, "Braintrust request failed: {error}"),
            Self::Deadline => {
                f.write_str("Braintrust request exceeded http.retry_max_elapsed while waiting for capacity or throttling")
            }
        }
    }
}
impl std::error::Error for Error {}
impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error.without_url())
    }
}
// keep capacity until the body is consumed, including streamed downloads
pub(crate) struct Response {
    inner: HttpResponse,
    _permit: Permit,
}
impl Response {
    pub(crate) fn status(&self) -> StatusCode {
        self.inner.status()
    }
    pub(crate) fn text(self) -> Result<String, reqwest::Error> {
        self.inner.text()
    }
    pub(crate) fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, reqwest::Error> {
        self.inner.json()
    }
}
impl std::io::Read for Response {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.inner, buffer)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
    };
    pub(crate) struct Reply {
        pub(crate) status: u16,
        pub(crate) body: String,
        pub(crate) headers: String,
    }
    impl Reply {
        pub(crate) fn json(value: serde_json::Value) -> Self {
            Self {
                status: 200,
                body: value.to_string(),
                headers: String::new(),
            }
        }
    }
    pub(crate) fn serve(replies: Vec<Reply>) -> (Client, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server_url = url.clone();
        let project = uuid::Uuid::nil();
        let (send, receive) = mpsc::channel();
        thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 8192];
                let mut length = None;
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(at) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..at]);
                        let body = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|value| value.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        length = Some(at + 4 + body);
                    }
                    if length.is_some_and(|length| request.len() >= length) {
                        break;
                    }
                }
                let _ = send.send(String::from_utf8(request).unwrap());
                let body = reply
                    .body
                    .replace("$API_URL", &server_url)
                    .replace("$PROJECT", &project.to_string());
                write!(
                    stream,
                    "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{}\r\n{}",
                    reply.status,
                    body.len(),
                    reply.headers,
                    body
                )
                .unwrap();
            }
        });
        let mut config = Braintrust::new("secret".to_owned(), uuid::Uuid::nil());
        config.api_url = url;
        config.request_timeout = Duration::from_secs(3);
        config.write_concurrency = 1;
        (Client::new(&config).unwrap(), receive)
    }
    #[test]
    fn throttle_cooldown_is_shared_and_honors_fractional_retry_after() {
        let (mut client, requests) = serve(vec![
            Reply {
                status: 429,
                body: "{}".to_owned(),
                headers: "Retry-After: 0.06\r\n".to_owned(),
            },
            Reply::json(serde_json::json!({})),
        ]);
        client.config.retry_attempts = 1;
        let first = client.get("/first").send().unwrap();
        assert_eq!(first.status(), StatusCode::TOO_MANY_REQUESTS);
        drop(first);
        let start = Instant::now();
        client.clone().get("/second").send().unwrap();
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert!(requests.recv().unwrap().contains("authorization: Bearer secret"));
    }
    #[test]
    fn ambiguous_create_failures_are_not_retried() {
        let (client, requests) = serve(vec![Reply {
            status: 503,
            body: "{}".to_owned(),
            headers: String::new(),
        }]);
        assert_eq!(
            client
                .post("/v1/dataset")
                .json(&serde_json::json!({}))
                .send()
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(requests.recv().unwrap().starts_with("POST"));
        assert!(requests.recv_timeout(Duration::from_millis(20)).is_err());
    }
    #[test]
    fn throttle_wait_respects_the_retry_budget() {
        let (mut client, _) = serve(vec![Reply {
            status: 429,
            body: "{}".to_owned(),
            headers: "Retry-After: 10\r\n".to_owned(),
        }]);
        client.config.retry_max_elapsed = Duration::from_millis(50);
        drop(client.get("/first").send().unwrap());
        let start = Instant::now();
        assert!(matches!(client.get("/second").send(), Err(Error::Deadline)));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn signed_requests_omit_api_auth_even_on_the_api_origin() {
        let (client, requests) = serve(vec![Reply::json(serde_json::json!({}))]);
        client.signed_get(&format!("{}/blob", client.config.api_url)).send().unwrap();
        assert!(!requests.recv().unwrap().to_ascii_lowercase().contains("authorization:"));
    }
}
