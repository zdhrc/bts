use crate::config::Settings;
use crate::diag::SourceDiags;
use crate::dsl;
use crate::selection;
use bts_core::providers::DataProvider;
use std::{fs, path::PathBuf};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The bts module containing datasets and their source traces.
    #[arg(long, value_name = "PATH")]
    from: PathBuf,
    /// Show the planned changes without writing to Braintrust.
    #[arg(long)]
    dry_run: bool,
    /// Select a dataset. Repeat to select more.
    #[arg(long, value_name = "TRAVERSAL", value_parser = crate::cmd::parse_selector)]
    select: Vec<selection::ResourceSelector>,
}
impl Args {
    pub fn run(self) -> Result<(), Error> {
        let source = fs::read_to_string(&self.from).map_err(Error::Read)?;
        let model = dsl::semantics::compile_module(&source, true)
            .map_err(|diags| Error::Invalid(SourceDiags::new(&self.from.display().to_string(), &source, diags)))?;
        let client = bts_core::providers::braintrust::Bundle
            .client(&crate::providers::braintrust::config(&Settings::load()?)?)
            .map_err(Error::Client)?;
        bts_core::providers::braintrust::features::datasets::sync(
            bts_core::providers::braintrust::features::datasets::Request {
                model: &model,
                path: &self.from,
                root: &crate::config::project_root().map_err(Error::Read)?,
                selectors: &self.select,
                dry_run: self.dry_run,
                now: std::time::SystemTime::now(),
            },
            &client,
            &mut |event| println!("{}", render_progress(event)),
        )
        .map_err(Error::Core)
    }
}
#[derive(Debug)]
pub enum Error {
    Read(std::io::Error),
    Invalid(SourceDiags),
    Config(crate::config::Error),
    ProviderConfig(bts_core::providers::braintrust::config::Error),
    Client(crate::client::Error),
    Core(bts_core::providers::braintrust::features::datasets::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => e.fmt(f),
            Self::Invalid(diags) => diags.fmt(f),
            Self::Config(e) => e.fmt(f),
            Self::ProviderConfig(e) => f.write_str(&crate::diag::braintrust_config_error(e)),
            Self::Client(e) => e.fmt(f),
            Self::Core(bts_core::providers::braintrust::features::datasets::Error::Selection(message)) => {
                write!(f, "--select: {message}")
            }
            Self::Core(e) => e.fmt(f),
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

fn render_progress(event: bts_core::providers::braintrust::features::datasets::Progress) -> String {
    use bts_core::providers::braintrust::features::datasets::{CaseAction, Progress};
    match event {
        Progress::Created { name, dry_run } => format!("{} dataset {name:?}", if dry_run { "would create" } else { "created" }),
        Progress::DescriptionUpdated { name, dry_run } => format!(
            "{} dataset description {name:?}",
            if dry_run { "would update" } else { "updated" }
        ),
        Progress::SourceWritten { name, dry_run } => {
            format!("{} source trace {name:?}", if dry_run { "would write" } else { "wrote" })
        }
        Progress::CasePlanned { dataset, name, action } => {
            let verb = match action {
                CaseAction::Unchanged => "unchanged",
                CaseAction::Create => "would create case",
                CaseAction::Update => "would update case",
            };
            format!("{verb} {dataset:?} / {name:?}")
        }
        Progress::CaseSynced { dataset, id } => format!("synced case {dataset:?} / {}", serde_json::json!(id)),
        Progress::CaseDeleted { dataset, id, dry_run } => format!(
            "{} case {dataset:?} / {}",
            if dry_run { "would delete" } else { "deleted" },
            serde_json::json!(id)
        ),
        Progress::State(event) => super::render_state(event),
    }
}
