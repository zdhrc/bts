use std::{env, time::Duration};
use thiserror::Error;
use uuid::Uuid;

const BRAINTRUST_API_URL: &str = "https://api.braintrust.dev";
const BRAINTRUST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct Braintrust {
    pub(crate) api_url: String,
    pub(crate) api_key: String,
    pub(crate) project_id: Uuid,
    pub(crate) request_timeout: Duration,
}

impl Braintrust {
    pub(crate) fn new(api_key: String, project_id: Uuid) -> Self {
        Self {
            api_url: BRAINTRUST_API_URL.to_owned(),
            api_key,
            project_id,
            request_timeout: BRAINTRUST_REQUEST_TIMEOUT,
        }
    }

    pub(crate) fn from_env() -> Result<Self, Error> {
        let api_key = required_env("BRAINTRUST_API_KEY")?;
        let project_id = required_env("BRAINTRUST_PROJECT_ID")?;
        let project_id = Uuid::parse_str(&project_id).map_err(|source| Error::InvalidProjectId { source })?;
        let mut config = Self::new(api_key, project_id);

        if let Some(api_url) = env::var_os("BRAINTRUST_API_URL").filter(|value| !value.is_empty()) {
            config.api_url = api_url.to_string_lossy().into_owned();
        }

        Ok(config)
    }
}

fn required_env(name: &'static str) -> Result<String, Error> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or(Error::MissingVariable(name))
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error("environment variable {0} is required")]
    MissingVariable(&'static str),

    #[error("BRAINTRUST_PROJECT_ID must be a UUID")]
    InvalidProjectId {
        #[source]
        source: uuid::Error,
    },
}
