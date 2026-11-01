use crate::config::{
    DEFAULT_MAX_ATTACHMENT_FILE_BYTES, DEFAULT_MAX_ATTACHMENT_TOTAL_BYTES, DEFAULT_MAX_ATTACHMENT_UPLOADS,
    DEFAULT_REQUEST_TIMEOUT, DEFAULT_WRITE_CONCURRENCY, project_root,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::{env, fmt, fs, time::Duration};
use uuid::Uuid;

const BRAINTRUST_API_URL: &str = "https://api.braintrust.dev";
const BT_CONTEXT_PATH: &str = ".bt/config.json";

#[derive(Clone)]
pub struct Config {
    pub api_url: String,
    pub app_url: String,
    pub api_key: String,
    pub project_id: Uuid,
    pub request_timeout: Duration,
    pub write_concurrency: usize,
    pub retry_attempts: u32,
    pub retry_max_elapsed: Duration,
    pub max_attachment_uploads: usize,
    pub max_attachment_file_bytes: u64,
    pub max_attachment_total_bytes: u64,
}

impl Config {
    pub fn new(api_key: String, project_id: Uuid) -> Self {
        Self {
            api_url: BRAINTRUST_API_URL.to_owned(),
            app_url: "https://www.braintrust.dev".to_owned(),
            api_key,
            project_id,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            write_concurrency: DEFAULT_WRITE_CONCURRENCY,
            retry_attempts: 8,
            retry_max_elapsed: Duration::from_secs(120),
            max_attachment_uploads: DEFAULT_MAX_ATTACHMENT_UPLOADS,
            max_attachment_file_bytes: DEFAULT_MAX_ATTACHMENT_FILE_BYTES,
            max_attachment_total_bytes: DEFAULT_MAX_ATTACHMENT_TOTAL_BYTES,
        }
    }

    pub fn load() -> Result<Self, Error> {
        let api_key = required_env("BRAINTRUST_API_KEY")?;
        let project = ProjectContext::load()?;
        let mut config = Self::new(api_key, project.id);

        if let Some(api_url) = env::var_os("BRAINTRUST_API_URL").filter(|value| !value.is_empty()) {
            config.api_url = api_url.to_string_lossy().into_owned();
        }

        if let Some(app_url) = env::var_os("BRAINTRUST_APP_URL").filter(|value| !value.is_empty()) {
            config.app_url = app_url.to_string_lossy().into_owned();
        }

        Ok(config)
    }
}

// The bt CLI's selected project is the target for both generated scorers and live writes.
pub struct ProjectContext {
    pub name: String,
    pub id: Uuid,
}

impl ProjectContext {
    pub fn load() -> Result<Self, Error> {
        let path = project_root().map_err(Error::CurrentDirectory)?.join(BT_CONTEXT_PATH);
        let raw = fs::read_to_string(&path).map_err(|source| Error::ReadProjectContext {
            path: path.clone(),
            source,
        })?;
        let context: BtContext = serde_json::from_str(&raw).map_err(|source| Error::ParseProjectContext {
            path: path.clone(),
            source,
        })?;
        let name = context
            .project
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| Error::MissingProjectName { path: path.clone() })?;
        let id = context
            .project_id
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| Error::MissingProjectId { path: path.clone() })?;
        let id = Uuid::parse_str(&id).map_err(|source| Error::InvalidProjectId { path, source })?;
        Ok(Self { name, id })
    }
}

#[derive(Deserialize)]
struct BtContext {
    project: Option<String>,
    project_id: Option<String>,
}

fn required_env(name: &'static str) -> Result<String, Error> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or(Error::MissingVariable(name))
}

#[derive(Debug)]
pub enum Error {
    MissingVariable(&'static str),
    InvalidProjectId { path: PathBuf, source: uuid::Error },
    CurrentDirectory(std::io::Error),
    ReadProjectContext { path: PathBuf, source: std::io::Error },
    ParseProjectContext { path: PathBuf, source: serde_json::Error },
    MissingProjectName { path: PathBuf },
    MissingProjectId { path: PathBuf },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable(name) => write!(formatter, "environment variable {name} is required"),
            Self::InvalidProjectId { path, source } => {
                write!(
                    formatter,
                    "bt project context {} has an invalid project_id: {source}",
                    path.display()
                )
            }
            Self::CurrentDirectory(source) => {
                write!(formatter, "could not determine the current directory: {source}")
            }
            Self::ReadProjectContext { path, source } => {
                write!(formatter, "could not read bt project context {}: {source}", path.display())
            }
            Self::ParseProjectContext { path, source } => {
                write!(formatter, "invalid bt project context {}: {source}", path.display())
            }
            Self::MissingProjectName { path } => {
                write!(formatter, "bt project context {} has no project name", path.display())
            }
            Self::MissingProjectId { path } => {
                write!(formatter, "bt project context {} has no project_id", path.display())
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidProjectId { source, .. } => Some(source),
            Self::CurrentDirectory(source) => Some(source),
            Self::ReadProjectContext { source, .. } => Some(source),
            Self::ParseProjectContext { source, .. } => Some(source),
            _ => None,
        }
    }
}
