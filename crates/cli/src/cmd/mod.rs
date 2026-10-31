mod build;
mod check;
mod init;
mod setup;
mod sync;
mod update;
mod write;

use crate::dsl;
use std::fmt;

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, clap::ValueEnum)]
enum Distribution {
    #[default]
    Linear,
    Sine,
}

impl From<Distribution> for crate::data::Distribution {
    fn from(value: Distribution) -> Self {
        match value {
            Distribution::Linear => Self::Linear,
            Distribution::Sine => Self::Sine,
        }
    }
}

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

fn parse_selector(source: &str) -> Result<crate::selection::ResourceSelector, String> {
    use crate::selection::ParseError;
    source.parse().map_err(|error| match error {
        ParseError::Syntax(diags) => render_diags("--select", source, &diags),
        ParseError::Expected(message) => format!("--select expects {message}"),
    })
}

fn parse_filter(source: &str) -> Result<bts_core::selection::filter::WriteFilter, String> {
    use bts_core::selection::filter::ParseError;
    source.parse().map_err(|error| match error {
        ParseError::Syntax(diags) => render_diags("--filter", source, &diags),
        ParseError::Expression(error) => render_diags("--filter", source, &vec![error.into()]),
    })
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

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(source) => Some(source),
            Self::Check(source) => Some(source),
            Self::Init(source) => Some(source),
            Self::Setup(source) => Some(source),
            Self::Sync(source) => Some(source),
            Self::Update(source) => Some(source),
            Self::Write(source) => Some(source),
        }
    }
}

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
    fn labels_selector_and_filter_errors_at_the_cli_boundary() {
        assert!(parse_selector("trace.one").unwrap_err().starts_with("--select expects"));
        assert!(parse_selector("dataset[").unwrap_err().starts_with("--select:1:"));
        assert!(parse_filter("trace.output == 1").unwrap_err().starts_with("--filter:1:1:"));
        assert!(parse_filter("block.kind ==").unwrap_err().starts_with("--filter:1:"));
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory as _;
        Cli::command().debug_assert();
    }
}
