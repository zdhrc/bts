mod build;
mod check;
mod init;
mod logging;
mod setup;
pub(crate) mod shared;
mod spec;
mod sync;
mod update;
mod write;

use crate::dsl;
use std::fmt;

#[derive(Debug, clap::Parser)]
#[command(name = "bts", version, about = "another synthetics generator", propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    Build(build::Args),
    Check(check::Args),
    Init(init::Args),
    Setup(setup::Args),
    Sync(sync::Args),
    Update(update::Args),
    Write(write::Args),
}

impl Cli {
    pub fn parse_compatible() -> Self {
        use clap::Parser as _;
        let mut args: Vec<_> = std::env::args_os().collect();
        if args.get(1).is_some_and(|arg| arg == "write")
            && args
                .get(2)
                .is_some_and(|arg| arg != "traces" && arg != "experiments" && arg != "--help" && arg != "-h")
        {
            args.insert(2, "traces".into());
        }
        Self::parse_from(args)
    }

    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Build(args) => args.run()?,
            Cmd::Check(args) => args.run()?,
            Cmd::Init(args) => args.run()?,
            Cmd::Setup(args) => args.run()?,
            Cmd::Sync(args) => args.run()?,
            Cmd::Update(args) => args.run()?,
            Cmd::Write(args) => args.run()?,
        }

        Ok(())
    }
}

fn render_diags(source_name: &str, src: &str, diags: &dsl::Diags) -> String {
    diags
        .iter()
        .map(|diag| diag.render(source_name, src))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug)]
pub enum Error {
    Build(build::Error),
    Check(check::Error),
    Init(init::Error),
    Setup(setup::Error),
    Sync(sync::Error),
    Update(update::Error),
    Write(write::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(source) => source.fmt(formatter),
            Self::Check(source) => source.fmt(formatter),
            Self::Init(source) => source.fmt(formatter),
            Self::Setup(source) => source.fmt(formatter),
            Self::Sync(source) => source.fmt(formatter),
            Self::Update(source) => source.fmt(formatter),
            Self::Write(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

impl From<build::Error> for Error {
    fn from(source: build::Error) -> Self {
        Self::Build(source)
    }
}

impl From<check::Error> for Error {
    fn from(source: check::Error) -> Self {
        Self::Check(source)
    }
}

impl From<init::Error> for Error {
    fn from(source: init::Error) -> Self {
        Self::Init(source)
    }
}

impl From<setup::Error> for Error {
    fn from(source: setup::Error) -> Self {
        Self::Setup(source)
    }
}

impl From<sync::Error> for Error {
    fn from(source: sync::Error) -> Self {
        Self::Sync(source)
    }
}

impl From<update::Error> for Error {
    fn from(source: update::Error) -> Self {
        Self::Update(source)
    }
}

impl From<write::Error> for Error {
    fn from(source: write::Error) -> Self {
        Self::Write(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
    }
}
