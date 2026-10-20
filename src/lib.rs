//! In-memory BTS browser API; native CLI and publication are feature gated.
pub mod browser;
#[cfg(feature = "native")]
mod cmd;
#[cfg(feature = "native")]
mod conf;
mod dsl;
mod reference;
#[cfg(feature = "native")]
mod scg;
mod sdg;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod wasm;

/// Run the native command-line interface.
#[cfg(feature = "native")]
pub fn run_cli() -> std::process::ExitCode {
    match cmd::Cli::parse_compatible().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
