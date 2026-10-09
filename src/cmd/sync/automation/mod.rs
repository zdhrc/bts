mod scorers;
mod topics;

use crate::{
    cmd::{
        render_diags,
        shared::{client::Client, select},
    },
    dsl, scg,
};
use serde_json::Value;
use std::{fmt, fs, path::PathBuf};

#[derive(Debug, clap::Args)]
#[command(about = "sync scorer or Topics automations from a bts shape")]
pub struct Command {
    #[command(subcommand)]
    command: Kind,
}

#[derive(Debug, clap::Subcommand)]
enum Kind {
    /// create or update online scoring rules for pushed scorers
    Scorers(Args),
    /// sync facet definitions, topic maps, and Topics automations
    Topics(TopicsArgs),
}

#[derive(Debug, clap::Args)]
pub struct Args {
    /// bts shape with automations and their scorers or facets
    #[arg(long, value_name = "PATH")]
    from: PathBuf,
    /// show the changes without writing to Braintrust
    #[arg(long)]
    dry_run: bool,
    /// select a scorer, facet, or automation, repeat for more
    #[arg(long, value_name = "TRAVERSAL")]
    select: Vec<select::ResourceSelector>,
}

#[derive(Debug, clap::Args)]
pub struct TopicsArgs {
    #[command(flatten)]
    sync: Args,
    /// rerun the Topics window when a facet prompt changes
    #[arg(long)]
    regenerate: bool,
}

impl Command {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Kind::Scorers(args) => scorers::run(args),
            Kind::Topics(args) => topics::run(args),
        }
    }
}

impl Args {
    fn compile(&self) -> Result<dsl::Model, Error> {
        let source = fs::read_to_string(&self.from).map_err(|source| Error::ReadShape {
            path: self.from.clone(),
            source,
        })?;
        dsl::compile_module(&source, true).map_err(|diags| Error::InvalidShape {
            details: render_diags(&self.from.display().to_string(), &source, &diags),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Create,
    Update,
    Unchanged,
}

fn get_objects(client: &Client, url: &str, query: &[(&str, &str)]) -> Result<Vec<Value>, Error> {
    let response = client.get(url).query(query).send().map_err(Error::Http)?;
    let value = check_response(response, url)?;
    value["objects"]
        .as_array()
        .cloned()
        .ok_or_else(|| Error::Api(format!("{url} returned no objects array")))
}

fn single(mut objects: Vec<Value>, kind: &str, name: &str) -> Result<Option<Value>, Error> {
    if objects.len() > 1 {
        return Err(Error::Api(format!("multiple {kind}s matched {name:?}")));
    }
    Ok(objects.pop())
}

fn check_response(response: crate::cmd::shared::client::Response, context: &str) -> Result<Value, Error> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().map_err(|error| Error::Http(error.into()))?;
        return Err(Error::Api(format!(
            "{context}: HTTP {status}: {}",
            body.chars().take(500).collect::<String>()
        )));
    }
    response.json().map_err(|error| Error::Http(error.into()))
}

#[derive(Debug)]
pub enum Error {
    ReadShape { path: PathBuf, source: std::io::Error },
    InvalidShape { details: String },
    NoAutomations,
    ScorerPlan(scg::Error),
    Config(crate::conf::Error),
    Http(crate::cmd::shared::client::Error),
    MissingScorer(String),
    Api(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadShape { path, source } => write!(formatter, "failed to read shape {}: {source}", path.display()),
            Self::InvalidShape { details } => write!(formatter, "invalid shape:\n{details}"),
            Self::NoAutomations => formatter.write_str("the selection contains no automations of this type"),
            Self::ScorerPlan(source) => source.fmt(formatter),
            Self::Config(source) => source.fmt(formatter),
            Self::Http(source) => write!(formatter, "Braintrust request failed: {source}"),
            Self::MissingScorer(name) => write!(
                formatter,
                "scorer {name:?} is not pushed in the selected Braintrust project; run `bts build` and `bt functions push` first"
            ),
            Self::Api(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<crate::conf::Error> for Error {
    fn from(source: crate::conf::Error) -> Self {
        Self::Config(source)
    }
}

impl From<crate::cmd::shared::state::Error> for Error {
    fn from(error: crate::cmd::shared::state::Error) -> Self {
        Self::Api(error.to_string())
    }
}
