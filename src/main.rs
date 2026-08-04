mod cmd;
mod conf;
mod dsl;
mod sdg;

fn main() -> std::process::ExitCode {
    if cmd::run() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
