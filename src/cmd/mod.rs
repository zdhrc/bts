mod check;
mod generate;
mod setup;

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
    Generate(generate::Args),
    Setup(setup::Args),
}

impl Cli {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Check(args) => args.run()?,
            Cmd::Generate(args) => args.run()?,
            Cmd::Setup(args) => args.run()?,
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

fn parse_duration(value: &str) -> Result<std::time::Duration, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000)
    } else if let Some(number) = value.strip_suffix('d') {
        (number, 86_400_000)
    } else {
        return Err("duration must end in ms, s, m, h, or d".to_owned());
    };
    let number = number
        .parse::<u64>()
        .map_err(|_| "duration must start with a whole number".to_owned())?;
    let milliseconds = number
        .checked_mul(multiplier)
        .filter(|value| *value > 0)
        .ok_or_else(|| "duration must be greater than zero and within range".to_owned())?;

    Ok(std::time::Duration::from_millis(milliseconds))
}

#[derive(Debug)]
pub enum Error {
    Check(check::Error),
    Generate(generate::Error),
    Setup(setup::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Check(source) => source.fmt(formatter),
            Self::Generate(source) => source.fmt(formatter),
            Self::Setup(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

impl From<check::Error> for Error {
    fn from(source: check::Error) -> Self {
        Self::Check(source)
    }
}

impl From<generate::Error> for Error {
    fn from(source: generate::Error) -> Self {
        Self::Generate(source)
    }
}

impl From<setup::Error> for Error {
    fn from(source: setup::Error) -> Self {
        Self::Setup(source)
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
