use super::{AgentAdapter, CodeAdapter, DataAdapter, TraceRun};
use crate::{code, config::Settings};
use bts_core::providers::braintrust;
use std::{fmt, path::Path};

pub struct Adapter;

impl DataAdapter for Adapter {
    type Provider = braintrust::Bundle;
    type Error = Error;

    fn provider(&self) -> Self::Provider {
        braintrust::Bundle
    }

    fn config(&self, settings: &Settings) -> Result<braintrust::config::Config, Error> {
        config(settings).map_err(Error::Config)
    }

    fn preview(&self, encoded: &braintrust::mocking::data::Batch) -> Result<String, Error> {
        serde_json::to_string_pretty(encoded).map_err(Error::Encode)
    }

    fn log_destination(&self, client: &braintrust::client::Client) {
        let config = client.config();
        tracing::info!(project_id = %config.project_id, api_url = %config.api_url, "writing to braintrust");
    }

    fn report_traces(
        &self,
        client: &braintrust::client::Client,
        receipt: &braintrust::client::writer::InsertResponse,
        run: TraceRun<'_>,
    ) -> Result<(), Error> {
        let config = client.config();
        tracing::info!(
            traces = run.events.trace_count(),
            events = run.events.event_count(),
            rows = receipt.row_count(),
            "insert acknowledged",
        );
        if run.json {
            let summary = Summary {
                seed: run.seed,
                traces: run.events.trace_count(),
                events: run.events.event_count(),
                rows: receipt.row_count(),
                project_id: config.project_id.to_string(),
                duration_ms: run.duration.as_millis().try_into().unwrap_or(u64::MAX),
                log: run.log.map(|path| path.display().to_string()),
            };
            println!("{}", serde_json::to_string(&summary).map_err(Error::Encode)?);
        } else {
            println!(
                "inserted {} traces and {} child spans into project {} ({} rows acknowledged)",
                run.events.trace_count(),
                run.events.event_count() - run.events.trace_count(),
                config.project_id,
                receipt.row_count(),
            );
        }
        Ok(())
    }
}

impl CodeAdapter for Adapter {
    type Provider = braintrust::Bundle;
    type Error = Error;

    fn provider(&self) -> Self::Provider {
        braintrust::Bundle
    }

    fn config(&self) -> Result<String, Error> {
        braintrust::config::ProjectContext::load()
            .map(|project| project.name)
            .map_err(Error::Config)
    }

    fn report_build(&self, out: &Path, built: &[code::writing::Built]) {
        let names = built
            .iter()
            .filter(|source| !source.slugs.is_empty())
            .map(|source| {
                source
                    .path
                    .file_name()
                    .expect("generated file has a name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "from {} run: bt functions push --if-exists replace {names}",
            out.join("src/scorers").display()
        );
    }
}

pub(crate) fn config(settings: &Settings) -> Result<braintrust::config::Config, braintrust::config::Error> {
    let mut config = braintrust::config::Config::load()?;
    config.request_timeout = settings.request_timeout;
    config.write_concurrency = settings.write_concurrency;
    config.retry_attempts = settings.retry_attempts;
    config.retry_max_elapsed = settings.retry_max_elapsed;
    config.max_attachment_uploads = settings.max_attachment_uploads;
    config.max_attachment_file_bytes = settings.max_attachment_file_bytes;
    config.max_attachment_total_bytes = settings.max_attachment_total_bytes;
    Ok(config)
}

#[derive(serde::Serialize)]
struct Summary {
    seed: u64,
    traces: usize,
    events: usize,
    rows: usize,
    project_id: String,
    duration_ms: u64,
    log: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    Config(braintrust::config::Error),
    Encode(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(source) => formatter.write_str(&crate::diag::braintrust_config_error(source)),
            Self::Encode(source) => write!(formatter, "failed to encode generated events as JSON: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(source) => Some(source),
            Self::Encode(source) => Some(source),
        }
    }
}

impl AgentAdapter for Adapter {
    fn agent_target(&self, model: &crate::dsl::Model) -> Result<braintrust::mocking::code::CodeTarget, Error> {
        let project = if model.experiments.is_empty() {
            String::new()
        } else {
            <Self as CodeAdapter>::config(self)?
        };
        Ok(braintrust::mocking::code::CodeTarget::new(project))
    }

    fn assemble_agents(
        &self,
        model: &crate::dsl::Model,
        lang: crate::dsl::ScorerLang,
        target: &braintrust::mocking::code::CodeTarget,
    ) -> Result<code::Assembly, String> {
        let assembly = code::assemble_agents(model, lang, target)?;
        target
            .agent_assembly(model, lang, assembly)
            .map_err(|error| error.to_string())
    }
}
