mod automation;
pub(crate) mod datasets;

#[derive(Debug)]
pub enum Error {
    Automation(automation::Error),
    Datasets(datasets::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Automation(error) => error.fmt(f),
            Self::Datasets(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Automation(source) => Some(source),
            Self::Datasets(source) => Some(source),
        }
    }
}

#[derive(Debug, clap::Args)]
#[command(about = "sync named Braintrust resources from a bts shape")]
pub struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    /// Sync scorer or Topics automations.
    Automation(automation::Args),
    /// Create or update datasets and their generated trace sources.
    Datasets(datasets::Args),
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Automation(args) => args.run().map_err(Error::Automation),
            Cmd::Datasets(args) => args.run().map_err(Error::Datasets),
        }
    }
}

fn render_state(event: bts_core::providers::braintrust::state::Progress) -> String {
    use bts_core::providers::braintrust::state::{Progress, ResourceKind};
    match event {
        Progress::Kept { name } => format!("kept shared resource {name:?}: another shape still owns it"),
        Progress::Deleted { kind, name, dry_run } => {
            let label = match kind {
                ResourceKind::Dataset => "dataset",
                ResourceKind::Facet => "facet",
                ResourceKind::TopicMap => "topic map",
                _ => "automation",
            };
            let verb = if dry_run { "would delete" } else { "deleted" };
            format!("{verb} {label} {name:?}")
        }
    }
}
