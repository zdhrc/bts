mod cmd;
mod conf;
mod dsl;
mod sdg;

use clap::Parser as _;

fn main() -> std::process::ExitCode {
    match cmd::Cli::parse().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
