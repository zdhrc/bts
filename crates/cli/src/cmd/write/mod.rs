mod experiments;
mod traces;

use std::fmt;

#[derive(Debug, clap::Args)]
#[command(about = "generate and write synthetic Braintrust data")]
pub struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Generate production traces over a time window.
    Traces(traces::Args),
    /// Generate dataset results and evaluate them with deployed scorers.
    Experiments(experiments::Args),
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Traces(args) => args.run()?,
            Cmd::Experiments(args) => args.run()?,
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    Traces(traces::Error),
    Experiments(experiments::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Traces(source) => source.fmt(formatter),
            Self::Experiments(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Traces(source) => Some(source),
            Self::Experiments(source) => Some(source),
        }
    }
}

impl From<traces::Error> for Error {
    fn from(source: traces::Error) -> Self {
        Self::Traces(source)
    }
}

impl From<experiments::Error> for Error {
    fn from(source: experiments::Error) -> Self {
        Self::Experiments(source)
    }
}
