use crate::{
    mocking::data,
    providers::braintrust::{
        client,
        mocking::data::{Format, TraceSink},
    },
};

pub type WriteError = data::WriteError<super::Error, client::writer::Error>;
pub type RunError = data::RunError<super::Error, client::writer::Error>;
pub type Written = data::Written<client::writer::InsertResponse>;

/// Writes a prepared batch using the Braintrust format and delivery implementation.
pub fn write(client: &client::Client, events: &data::EventBatch) -> Result<client::writer::InsertResponse, WriteError> {
    data::write(events, &Format, &TraceSink::new(client))
}

/// Generates and writes traces using the supplied client.
pub fn run(request: data::Request<'_>, client: &client::Client) -> Result<Written, RunError> {
    data::run(request, &Format, &TraceSink::new(client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::braintrust::client::tests::{Reply, serve};
    use crate::{
        mocking::data::{Distribution, Request},
        selection::filter::WriteFilter,
    };
    use serde_json::json;
    use std::time::Duration;
    use std::{path::Path, time::UNIX_EPOCH};

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
