mod check;
mod init;
mod logging;
mod setup;
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
    Check(check::Args),
    Init(init::Args),
    Setup(setup::Args),
    Write(write::Args),
}

impl Cli {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Check(args) => args.run()?,
            Cmd::Init(args) => args.run()?,
            Cmd::Setup(args) => args.run()?,
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
    Check(check::Error),
    Init(init::Error),
    Setup(setup::Error),
    Write(write::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check(source) => source.fmt(formatter),
            Self::Init(source) => source.fmt(formatter),
            Self::Setup(source) => source.fmt(formatter),
            Self::Write(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

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
