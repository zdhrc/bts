use std::time::Duration;
use uuid::Uuid;

const BRAINTRUST_API_URL: &str = "https://api.braintrust.dev";
const BRAINTRUST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) braintrust: Braintrust,
}

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
}
