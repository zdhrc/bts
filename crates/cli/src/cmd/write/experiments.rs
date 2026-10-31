use crate::config::Settings;
use crate::diag::SourceDiags;
use crate::dsl;
use crate::logging;
use crate::selection::ResourceSelector;
use bts_core::providers::DataProvider;

use serde_json::json;
use std::{fmt, fs, path::PathBuf, time::SystemTime};
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The bts module containing experiments, datasets, tasks, and scorers.
    #[arg(long, value_name = "PATH")]
    from: PathBuf,
    /// Select an experiment. Repeat to select more, including any referenced baseline.
    #[arg(long, value_name = "TRAVERSAL", value_parser = crate::cmd::parse_selector)]
    select: Vec<ResourceSelector>,
    /// The seed for task generation. It does not control randomness in deployed scorers.
    #[arg(long)]
    seed: Option<u64>,
    /// Preview local dataset cases and generated tasks without invoking scorers.
    #[arg(long)]
    dry_run: bool,
    /// Print the final run summary as JSON.
    #[arg(long, conflicts_with = "dry_run")]
    json: bool,
    /// Print phase timings to standard error.
    #[arg(long)]
    profile: bool,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let settings = Settings::load()?;
        let log_path = logging::init("write-experiments", self.profile, &settings);
        let result = self.execute(&settings);
        if let Err(error) = &result {
            tracing::error!(%error, "experiment write failed");
            if let Some(path) = log_path {
                eprintln!("run log: {}", path.display());
            }
        }
        result
    }

    fn execute(self, settings: &Settings) -> Result<(), Error> {
        let source = fs::read_to_string(&self.from).map_err(Error::Read)?;
        let model = dsl::semantics::compile(&source)
            .map_err(|errors| Error::Invalid(SourceDiags::new(&self.from.display().to_string(), &source, errors)))?;
        bts_core::providers::braintrust::features::experiments::validate_selection(&model, &self.select)?;
        let seed = self.seed.unwrap_or_else(rand::random);
        eprintln!("seed: {seed}");
        let now = SystemTime::now();
        if self.dry_run {
            let preview =
                bts_core::providers::braintrust::features::experiments::preview(&model, &self.select, &self.from, seed, now)?;
            println!("{}", serde_json::to_string_pretty(&preview).map_err(Error::Encode)?);
            return Ok(());
        }
        let client = bts_core::providers::braintrust::Bundle
            .client(&crate::providers::braintrust::config(settings)?)
            .map_err(Error::Client)?;
        let result = bts_core::providers::braintrust::features::experiments::run(
            &model,
            &self.select,
            &self.from,
            seed,
            now,
            &client,
            &mut |experiments, calls| {
                eprintln!("generating {experiments} experiments; invoking scorers {calls} times");
            },
        )?;
        let summaries = result.summaries;
        let outcome = result.result;
        if self.json {
            let mut summary = json!({"seed":seed,"experiments":summaries});
            if let Err(error) = &outcome {
                summary["error"] = json!(render_core_error(error));
            }
            println!("{summary}");
        } else {
            for summary in summaries {
                if let bts_core::providers::braintrust::features::experiments::Status::Complete { results, spans } =
                    summary.status
                {
                    println!(
                        "wrote {}: {} results, {} spans (experiment {})",
                        summary.name, results, spans, summary.id
                    );
                } else {
                    println!(
                        "created {} (experiment {}); results may be incomplete",
                        summary.name, summary.id
                    );
                }
            }
        }
        outcome.map_err(Error::Core)
    }
}

#[derive(Debug)]
pub enum Error {
    Read(std::io::Error),
    Invalid(SourceDiags),
    Config(crate::config::Error),
    ProviderConfig(bts_core::providers::braintrust::config::Error),
    Client(crate::client::Error),
    Encode(serde_json::Error),
    Core(bts_core::providers::braintrust::features::experiments::Error),
}
fn render_core_error(error: &bts_core::providers::braintrust::features::experiments::Error) -> String {
    use bts_core::providers::braintrust::features::experiments::Error;
    match error {
        Error::Selection(message) => format!("--select: {message}"),
        Error::MissingBaseline { baseline, .. } => format!("{error}; add --select 'experiment[\"{baseline}\"]'"),
        Error::Dependency { .. } => format!("{error}; sync datasets and build/push scorers before writing experiments"),
        _ => error.to_string(),
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => e.fmt(f),
            Self::Invalid(diags) => diags.fmt(f),
            Self::Config(e) => e.fmt(f),
            Self::ProviderConfig(e) => f.write_str(&crate::diag::braintrust_config_error(e)),
            Self::Client(e) => e.fmt(f),
            Self::Encode(e) => e.fmt(f),
            Self::Core(e) => f.write_str(&render_core_error(e)),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(details) => Some(details),
            Self::Read(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::ProviderConfig(source) => Some(source),
            Self::Client(source) => Some(source),
            Self::Encode(source) => Some(source),
            Self::Core(source) => Some(source),
        }
    }
}
impl From<bts_core::providers::braintrust::config::Error> for Error {
    fn from(source: bts_core::providers::braintrust::config::Error) -> Self {
        Self::ProviderConfig(source)
    }
}

impl From<crate::config::Error> for Error {
    fn from(error: crate::config::Error) -> Self {
        Self::Config(error)
    }
}
impl From<bts_core::providers::braintrust::features::experiments::Error> for Error {
    fn from(error: bts_core::providers::braintrust::features::experiments::Error) -> Self {
        Self::Core(error)
    }
}
