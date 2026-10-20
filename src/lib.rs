//! BTS core library API. Optional CLI composition lives at this entry point.
pub mod api;
#[cfg(feature = "cli")]
mod cmd;
#[cfg(feature = "cli")]
mod conf;
#[cfg_attr(not(feature = "cli"), allow(dead_code, unused_imports))]
mod dsl;
mod reference;
#[cfg(feature = "cli")]
mod scg;
#[cfg_attr(not(feature = "cli"), allow(dead_code, unused_imports))]
mod sdg;

/// Run the native command-line interface.
#[cfg(feature = "cli")]
pub fn run_cli() -> std::process::ExitCode {
    match cmd::Cli::parse_compatible().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
