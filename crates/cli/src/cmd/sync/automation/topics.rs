use super::{Error, Options};
use crate::config::Settings;
use bts_core::providers::DataProvider;
use bts_core::providers::braintrust::features::automations::topics;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[command(flatten)]
    sync: Options,
    /// Rerun the Topics window when a facet prompt changes.
    #[arg(long)]
    regenerate: bool,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let root = crate::config::project_root().map_err(Error::Directory)?;
        let model = self.sync.compile()?;
        let client = bts_core::providers::braintrust::Bundle
            .client(&crate::providers::braintrust::config(&Settings::load()?)?)
            .map_err(Error::Client)?;
        topics::run(
            topics::Request {
                model: &model,
                root: &root,
                from: &self.sync.from,
                dry_run: self.sync.dry_run,
                select: &self.sync.select,
                regenerate: self.regenerate,
            },
            &client,
            &mut |event| println!("{}", render_progress(event)),
        )
        .map_err(Error::Topics)
    }
}

fn render_progress(event: topics::Progress) -> String {
    use topics::{Action, Progress};
    match event {
        Progress::Changed { action, name, dry_run } => {
            let verb = match (action, dry_run) {
                (Action::Unchanged, _) => "unchanged",
                (Action::Create, true) => "would create",
                (Action::Update, true) => "would update",
                (Action::Create, false) => "created",
                (Action::Update, false) => "updated",
            };
            format!("{verb} {name}")
        }
        Progress::Regeneration { name, dry_run: true } => format!("would regenerate {name:?} over its existing Topics window"),
        Progress::Regeneration { name, dry_run: false } => format!("queued regeneration {name:?}"),
        Progress::State(event) => super::super::render_state(event),
    }
}
