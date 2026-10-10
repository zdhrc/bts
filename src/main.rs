mod cmd;
mod conf;
mod dsl;
mod scg;
mod sdg;

fn main() -> std::process::ExitCode {
    match cmd::Cli::parse_compatible().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
