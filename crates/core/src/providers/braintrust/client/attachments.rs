use super::{Client, checked};
use crate::mocking::data::Attachment;
use ring::digest::{Context, SHA256};
use serde_json::Value;
use std::{collections::HashMap, fs, io::Read};

#[derive(Default)]
pub struct Comparison {
    org_id: Option<String>,
    local: HashMap<String, (u64, Vec<u8>)>,
    remote: HashMap<String, Option<(u64, Vec<u8>)>>,
    metadata: HashMap<String, Value>,
}
impl Comparison {
    // returns replacements for equal attachments; unmatched files stay uploads
    pub fn reconcile(
        &mut self,
        client: &Client,
        desired: &mut Value,
        current: &Value,
        attachments: &[Attachment],
    ) -> Result<HashMap<String, String>, Error> {
        let by_key = attachments
            .iter()
            .map(|attachment| (attachment.key.as_str(), attachment))
            .collect();
        let mut keys = HashMap::new();
        self.walk(client, desired, current, &by_key, &mut keys)?;
        Ok(keys)
    }
    fn walk(
        &mut self,
        client: &Client,
        desired: &mut Value,
        current: &Value,
        attachments: &HashMap<&str, &Attachment>,
        keys: &mut HashMap<String, String>,
    ) -> Result<(), Error> {
        if desired["type"] == "braintrust_attachment" {
            let key = desired["key"].as_str().ok_or("attachment has no key")?.to_owned();
            if let Some(attachment) = attachments.get(key.as_str()) {
                if let Some(replacement) = keys.get(&key) {
                    desired["key"] = Value::String(replacement.clone());
                } else if current["type"] == "braintrust_attachment"
                    && desired["content_type"] == current["content_type"]
                    && desired["filename"] == current["filename"]
                    && self.equal(client, attachment, current)?
                {
                    let replacement = current["key"].as_str().ok_or("remote attachment has no key")?.to_owned();
                    desired["key"] = Value::String(replacement.clone());
                    keys.insert(key, replacement);
                }
            }
            return Ok(());
        }
        match desired {
            Value::Object(fields) => {
                for (key, value) in fields {
                    self.walk(client, value, &current[key], attachments, keys)?;
                }
            }
            Value::Array(values) => {
                for (index, value) in values.iter_mut().enumerate() {
                    self.walk(client, value, &current[index], attachments, keys)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn equal(&mut self, client: &Client, local: &Attachment, remote: &Value) -> Result<bool, Error> {
        let key = remote["key"].as_str().ok_or("remote attachment has no key")?;
        let size = fs::metadata(&local.path)
            .map_err(|source| Error::Io {
                context: local.path.clone(),
                source,
            })?
            .len();
        if !self.metadata.contains_key(key) {
            if self.org_id.is_none() {
                let project = checked(
                    client
                        .get(format!("/v1/project/{}", client.config().project_id))
                        .send()
                        .map_err(|source| Error::Request {
                            context: "attachment request".to_owned(),
                            source: source.without_url(),
                        })?,
                    "attachment project",
                )
                .map_err(Error::Response)?;
                self.org_id = Some(project["org_id"].as_str().ok_or("project has no org_id")?.to_owned());
            }
            let response = client
                .get("/attachment")
                .query(&[
                    ("key", key),
                    ("org_id", self.org_id.as_deref().unwrap()),
                    ("filename", remote["filename"].as_str().unwrap_or("")),
                    ("content_type", remote["content_type"].as_str().unwrap_or("")),
                ])
                .send()
                .map_err(|source| Error::Request {
                    context: "attachment request".to_owned(),
                    source: source.without_url(),
                })?;
            let metadata = if response.status() == reqwest::StatusCode::NOT_FOUND {
                Value::Null
            } else {
                checked(response, "compare attachment").map_err(Error::Response)?
            };
            self.metadata.insert(key.to_owned(), metadata);
        }
        let metadata = &self.metadata[key];
        if metadata.is_null() || metadata["status"]["upload_status"] == "error" {
            return Ok(false);
        }
        if metadata["contentLength"].as_u64().is_some_and(|length| length != size) {
            return Ok(false);
        }
        if !self.local.contains_key(&local.path) {
            let file = fs::File::open(&local.path).map_err(|source| Error::Io {
                context: local.path.clone(),
                source,
            })?;
            self.local.insert(local.path.clone(), hash(file)?);
        }
        if !self.remote.contains_key(key) {
            // the attachment API currently provides a download URL, not a
            // trustworthy content digest. never treat a generic ETag as one.
            let url = metadata["downloadUrl"]
                .as_str()
                .filter(|url| !url.is_empty())
                .ok_or("attachment comparison cannot obtain its bytes")?;
            let response = client.signed_get(url).send().map_err(|source| Error::Request {
                context: "attachment request".to_owned(),
                source: source.without_url(),
            })?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                self.remote.insert(key.to_owned(), None);
            } else if !response.status().is_success() {
                return Err(Error::Rejected {
                    context: "attachment download".to_owned(),
                    status: response.status(),
                    detail: String::new(),
                });
            } else {
                self.remote.insert(key.to_owned(), Some(hash(response)?));
            }
        }
        Ok(self.remote[key].as_ref() == Some(&self.local[&local.path]))
    }
}
fn hash(mut reader: impl Read) -> Result<(u64, Vec<u8>), Error> {
    let mut digest = Context::new(&SHA256);
    let mut size = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|source| Error::Io {
            context: "attachment bytes".to_owned(),
            source,
        })?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((size, digest.finish().as_ref().to_vec()))
}

/// A failure reading, transferring, or confirming an attachment.
#[derive(Debug)]
pub enum Error {
    UploadRejected {
        status: reqwest::StatusCode,
        marked_as_error: bool,
    },
    UnexpectedUploadStatus(reqwest::StatusCode),
    InvalidData(String),
    Io {
        context: String,
        source: std::io::Error,
    },
    Request {
        context: String,
        source: super::Error,
    },
    Body {
        context: String,
        source: reqwest::Error,
    },
    Response(super::ApiError),
    Rejected {
        context: String,
        status: reqwest::StatusCode,
        detail: String,
    },
    Context {
        context: String,
        source: Box<Error>,
    },
    Upload {
        index: usize,
        total: usize,
        path: String,
        key: String,
        completed: usize,
        source: Box<Error>,
    },
    StatusUpdate {
        failure: Box<Error>,
        source: Box<Error>,
    },
    ReportedError(String),
    Reconciliation {
        failure: Box<Error>,
        outcome: Box<Error>,
    },
    OutcomeUnknown {
        detail: String,
        source: Option<Box<Error>>,
    },
}
impl Error {
    pub(super) fn context(self, context: impl Into<String>) -> Self {
        Self::Context {
            context: context.into(),
            source: Box::new(self),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UploadRejected { status, marked_as_error } => {
                write!(formatter, "signed upload rejected with HTTP {status}")?;
                if *marked_as_error {
                    formatter.write_str("; Braintrust marked the attachment as error")?;
                }
                Ok(())
            }
            Self::UnexpectedUploadStatus(status) => write!(formatter, "signed upload returned HTTP {status}"),
            Self::InvalidData(message) => formatter.write_str(message),
            Self::Io { context, source } => write!(formatter, "{context}: {source}"),
            Self::Request { context, source } => write!(formatter, "{context}: {source}"),
            Self::Body { context, source } => write!(formatter, "{context}: {source}"),
            Self::Response(source) => source.fmt(formatter),
            Self::Rejected { context, status, detail } => {
                if !context.is_empty() {
                    write!(formatter, "{context}: ")?;
                }
                write!(formatter, "HTTP {status}")?;
                if !detail.is_empty() {
                    write!(formatter, ": {detail}")?;
                }
                Ok(())
            }
            Self::Context { context, source } => write!(formatter, "{context}: {source}"),
            Self::Upload {
                index,
                total,
                path,
                key,
                completed,
                source,
            } => write!(
                formatter,
                "attachment {index}/{total} {path} (key {key}): {source}; {completed} earlier upload(s) completed; no log rows inserted"
            ),
            Self::StatusUpdate { failure, source } => write!(formatter, "{failure}; error status update also failed: {source}"),
            Self::Reconciliation { failure, outcome } => write!(formatter, "{failure}; {outcome}"),
            Self::ReportedError(detail) => write!(formatter, "Braintrust reports attachment error: {detail}"),
            Self::OutcomeUnknown { detail, source } => {
                write!(formatter, "upload outcome unknown: {detail}")?;
                if let Some(source) = source {
                    write!(formatter, ": {source}")?;
                }
                formatter.write_str("; attachment status was left unchanged")
            }
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Request { source, .. } => Some(source),
            Self::Body { source, .. } => Some(source),
            Self::Response(source) => Some(source),
            Self::Reconciliation { outcome, .. } => Some(outcome.as_ref()),
            Self::Context { source, .. } | Self::Upload { source, .. } | Self::StatusUpdate { source, .. } => {
                Some(source.as_ref())
            }
            Self::OutcomeUnknown {
                source: Some(source), ..
            } => Some(source.as_ref()),
            _ => None,
        }
    }
}
impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::InvalidData(message.to_owned())
    }
}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::InvalidData(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::braintrust::client::tests::{Reply, serve};
    use serde_json::json;
    fn attachment() -> Attachment {
        let path = std::env::temp_dir().join(format!("bts-compare-{}", uuid::Uuid::new_v4()));
        fs::write(&path, b"abc").unwrap();
        Attachment {
            path: path.display().to_string(),
            filename: "file.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            key: "local".to_owned(),
        }
    }
    fn reference(key: &str) -> Value {
        json!({"type":"braintrust_attachment","key":key,"filename":"file.txt","content_type":"text/plain"})
    }
    #[test]
    fn compares_bytes_and_caches_downloads_without_sending_auth_to_blobs() {
        let (client, requests) = serve(vec![
            Reply::json(json!({"org_id":"org"})),
            Reply::json(json!({"status":{"upload_status":"done"},"contentLength":3,"downloadUrl":"$API_URL/blob"})),
            Reply {
                status: 200,
                body: "abc".to_owned(),
                headers: String::new(),
            },
        ]);
        let attachment = attachment();
        let mut comparison = Comparison::default();
        for _ in 0..2 {
            let mut desired = reference("local");
            let keys = comparison
                .reconcile(&client, &mut desired, &reference("remote"), std::slice::from_ref(&attachment))
                .unwrap();
            assert_eq!(keys["local"], "remote");
            assert_eq!(desired, reference("remote"));
        }
        requests.recv().unwrap();
        requests.recv().unwrap();
        assert!(!requests.recv().unwrap().to_ascii_lowercase().contains("authorization:"));
        assert!(requests.recv_timeout(std::time::Duration::from_millis(20)).is_err());
        fs::remove_file(attachment.path).unwrap();
    }
    #[test]
    fn equal_lengths_do_not_hide_different_content() {
        let (client, _) = serve(vec![
            Reply::json(json!({"org_id":"org"})),
            Reply::json(json!({"status":{"upload_status":"done"},"contentLength":3,"downloadUrl":"$API_URL/blob"})),
            Reply {
                status: 200,
                body: "xyz".to_owned(),
                headers: String::new(),
            },
        ]);
        let attachment = attachment();
        let keys = Comparison::default()
            .reconcile(
                &client,
                &mut reference("local"),
                &reference("remote"),
                std::slice::from_ref(&attachment),
            )
            .unwrap();
        assert!(keys.is_empty());
        fs::remove_file(attachment.path).unwrap();
    }
    #[test]
    fn different_lengths_skip_downloading_and_access_errors_fail_comparison() {
        let (client, requests) = serve(vec![
            Reply::json(json!({"org_id":"org"})),
            Reply::json(json!({"status":{"upload_status":"done"},"contentLength":2})),
        ]);
        let attachment = attachment();
        assert!(
            Comparison::default()
                .reconcile(
                    &client,
                    &mut reference("local"),
                    &reference("remote"),
                    std::slice::from_ref(&attachment)
                )
                .unwrap()
                .is_empty()
        );
        requests.recv().unwrap();
        requests.recv().unwrap();
        assert!(requests.recv_timeout(std::time::Duration::from_millis(20)).is_err());
        let (client, _) = serve(vec![
            Reply::json(json!({"org_id":"org"})),
            Reply {
                status: 403,
                body: "{}".to_owned(),
                headers: String::new(),
            },
        ]);
        assert!(
            Comparison::default()
                .reconcile(
                    &client,
                    &mut reference("local"),
                    &reference("remote"),
                    std::slice::from_ref(&attachment)
                )
                .is_err()
        );
        fs::remove_file(attachment.path).unwrap();
    }
}
