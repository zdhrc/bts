mod scorers;
mod topics;

use crate::{diag::SourceDiags, dsl, selection};
use std::{fs, path::PathBuf};

#[derive(Debug, clap::Args)]
#[command(about = "sync scorer or Topics automations from a bts shape")]
pub struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Create or update online scoring rules for deployed scorers.
    Scorers(scorers::Args),
    /// Sync facet definitions, topic maps, and Topics automations.
    Topics(topics::Args),
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Scorers(args) => args.run(),
            Cmd::Topics(args) => args.run(),
        }
    }
}

#[derive(Debug, clap::Args)]
struct Options {
    /// The bts shape containing automations and their scorers or facets.
    #[arg(long, value_name = "PATH")]
    from: PathBuf,
    /// Show the changes without writing to Braintrust.
    #[arg(long)]
    dry_run: bool,
    /// Select a scorer, facet, or automation. Repeat to select more.
    #[arg(long, value_name = "TRAVERSAL", value_parser = crate::cmd::parse_selector)]
    select: Vec<selection::ResourceSelector>,
}

impl Options {
    fn compile(&self) -> Result<dsl::Model, Error> {
        let source = fs::read_to_string(&self.from).map_err(|source| Error::ReadShape {
            path: self.from.clone(),
            source,
        })?;
        dsl::semantics::compile_module(&source, true).map_err(|diags| Error::InvalidShape {
            details: SourceDiags::new(&self.from.display().to_string(), &source, diags),
        })
    }
}

#[derive(Debug)]
pub enum Error {
    ReadShape { path: PathBuf, source: std::io::Error },
    InvalidShape { details: SourceDiags },
    Directory(std::io::Error),
    Config(crate::config::Error),
    ProviderConfig(bts_core::providers::braintrust::config::Error),
    Client(crate::client::Error),
    Scorers(bts_core::providers::braintrust::features::automations::scorers::Error),
    Topics(bts_core::providers::braintrust::features::automations::topics::Error),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadShape { path, source } => write!(f, "failed to read shape {}: {source}", path.display()),
            Self::InvalidShape { details } => write!(f, "invalid shape:\n{details}"),
            Self::Directory(e) => e.fmt(f),
            Self::Config(e) => e.fmt(f),
            Self::ProviderConfig(e) => f.write_str(&crate::diag::braintrust_config_error(e)),
            Self::Client(e) => e.fmt(f),
            Self::Scorers(bts_core::providers::braintrust::features::automations::scorers::Error::Selection(message))
            | Self::Topics(bts_core::providers::braintrust::features::automations::topics::Error::Selection(message)) => {
                write!(f, "--select: {message}")
            }
            Self::Scorers(e @ bts_core::providers::braintrust::features::automations::scorers::Error::MissingScorer(_)) => {
                write!(f, "{e}; run `bts build` and `bt functions push` first")
            }
            Self::Scorers(e) => e.fmt(f),
            Self::Topics(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidShape { details } => Some(details),
            Self::ReadShape { source, .. } => Some(source),
            Self::Directory(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::ProviderConfig(source) => Some(source),
            Self::Client(source) => Some(source),
            Self::Scorers(source) => Some(source),
            Self::Topics(source) => Some(source),
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
