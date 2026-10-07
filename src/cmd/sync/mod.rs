mod automations;

pub use automations::Error;

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
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Automations(args) => args.run(),
        }
    }
}
