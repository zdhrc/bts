use bts_core::providers::braintrust::client;
mod cmd;
mod diag;
mod logging;
mod providers;
mod spec;

use bts_core::{
    config,
    mocking::{code, data},
    selection,
};
use bts_lang as dsl;

fn main() -> std::process::ExitCode {
    match cmd::Cli::parse_compatible().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
