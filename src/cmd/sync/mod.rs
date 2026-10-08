mod automations;
mod datasets;

#[derive(Debug)]
pub enum Error {
    Automations(automations::Error),
    Datasets(datasets::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Automations(error) => error.fmt(f),
            Self::Datasets(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, clap::Args)]
#[command(about = "sync named Braintrust resources from a bts shape")]
pub struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// create or update online scoring rules for pushed scorers
    Automations(automations::Args),
    /// create or update datasets and their generated trace sources
    Datasets(datasets::Args),
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Automations(args) => args.run().map_err(Error::Automations),
            Cmd::Datasets(args) => args.run().map_err(Error::Datasets),
        }
    }
}
