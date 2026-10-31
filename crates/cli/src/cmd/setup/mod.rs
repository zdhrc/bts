mod skill;

pub use skill::Error;

#[derive(Debug, clap::Args)]
#[command(about = "set up integrations for bts")]
pub struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    #[command(visible_alias = "skills")]
    Skill(skill::Args),
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Skill(args) => args.run(),
        }
    }
}
