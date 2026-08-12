mod perf;
mod syntax;

use std::fmt;

#[derive(Debug, clap::Args)]
#[command(about = "check a bts shape before generating from it")]
pub struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, clap::Subcommand)]
enum Cmd {
    Syntax(syntax::Args),
    Perf(perf::Args),
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        match self.command {
            Cmd::Syntax(args) => args.run()?,
            Cmd::Perf(args) => args.run()?,
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    Syntax(syntax::Error),
    Perf(perf::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(source) => source.fmt(formatter),
            Self::Perf(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

impl From<syntax::Error> for Error {
    fn from(source: syntax::Error) -> Self {
        Self::Syntax(source)
    }
}

impl From<perf::Error> for Error {
    fn from(source: perf::Error) -> Self {
        Self::Perf(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{Cli, Cmd as RootCmd};
    use clap::Parser as _;

    fn parse_check(argv: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            RootCmd::Check(args) => args,
            other => panic!("expected a check command, parsed {other:?}"),
        })
    }

    #[test]
    fn parses_the_syntax_subcommand() {
        let args = parse_check(&["bts", "check", "syntax", "shape.bt"]).unwrap();

        assert!(matches!(args.command, Cmd::Syntax(_)));
    }

    #[test]
    fn parses_the_perf_subcommand() {
        let args = parse_check(&["bts", "check", "perf", "shape.bt", "--count", "10", "--over", "1h"]).unwrap();

        assert!(matches!(args.command, Cmd::Perf(_)));
    }

    #[test]
    fn requires_a_subcommand() {
        assert!(parse_check(&["bts", "check", "shape.bt"]).is_err());
        assert!(parse_check(&["bts", "check", "perf", "shape.bt"]).is_err());
    }
}
