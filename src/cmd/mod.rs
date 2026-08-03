mod check;

use clap::Command;

pub fn command() -> Command {
    Command::new("bts").about("another synthetics generator").subcommand(check::command())
}

pub fn run() {
    let matches = command().get_matches();

    match matches.subcommand() {
        Some(("check", sub_matches)) => check::run(sub_matches),
        _ => {
            let _ = command().print_help();
            println!();
        }
    }
}
