mod check;
mod generate;
mod setup;

use clap::Command;

pub fn command() -> Command {
    Command::new("bts")
        .about("another synthetics generator")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand(check::command())
        .subcommand(generate::command())
        .subcommand(setup::command())
}

pub fn run() -> bool {
    let matches = command().get_matches();

    match matches.subcommand() {
        Some(("check", sub_matches)) => check::run(sub_matches),
        Some(("generate", sub_matches)) => generate::run(sub_matches),
        Some(("setup", sub_matches)) => setup::run(sub_matches),
        _ => {
            let _ = command().print_help();
            println!();
            true
        }
    }
}
