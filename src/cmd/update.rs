use std::fmt;

#[derive(Debug, clap::Args)]
#[command(about = "update bts to the latest released version")]
pub struct Args {}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let updated = axoupdater::AxoUpdater::new_for(env!("CARGO_PKG_NAME"))
            .load_receipt()
            .map_err(|source| Error::LoadReceipt(Box::new(source)))?
            .run_sync()
            .map_err(|source| Error::Update(Box::new(source)))?;

        println!("{}", summary(updated.is_some()));

        Ok(())
    }
}

fn summary(updated: bool) -> &'static str {
    if updated { "updated bts" } else { "bts is already up to date" }
}

#[derive(Debug)]
pub enum Error {
    LoadReceipt(Box<axoupdater::AxoupdateError>),
    Update(Box<axoupdater::AxoupdateError>),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadReceipt(source) => write!(formatter, "could not load update receipt: {source}"),
            Self::Update(source) => write!(formatter, "could not update bts: {source}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{Cli, Cmd};
    use clap::Parser as _;

    fn parse_update(argv: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Cmd::Update(args) => args,
            other => panic!("expected an update command, parsed {other:?}"),
        })
    }

    #[test]
    fn parses_the_update_command() {
        parse_update(&["bts", "update"]).unwrap();
    }

    #[test]
    fn summarizes_whether_an_update_ran() {
        assert_eq!(summary(true), "updated bts");
        assert_eq!(summary(false), "bts is already up to date");
    }

    #[test]
    fn load_receipt_errors_name_the_failure() {
        let error = Error::LoadReceipt(Box::new(axoupdater::AxoupdateError::NoReceipt {
            app_name: "bts".to_owned(),
        }));

        assert_eq!(
            error.to_string(),
            "could not load update receipt: Unable to load receipt for app bts"
        );
    }

    #[test]
    fn update_errors_name_the_failure() {
        let error = Error::Update(Box::new(axoupdater::AxoupdateError::NoInstallerForPackage {}));

        assert_eq!(
            error.to_string(),
            "could not update bts: Unable to find an installer for your OS"
        );
    }
}
