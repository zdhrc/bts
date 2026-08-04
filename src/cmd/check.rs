use std::fs;

use crate::dsl::compile;
use clap::{Arg, ArgGroup, ArgMatches, Command};

pub fn command() -> Command {
    Command::new("check")
        .about("check source, file, etc for valid syntax")
        .arg(
            Arg::new("file")
                .short('f')
                .long("file")
                .value_name("PATH")
                .help("path to a source file to check"),
        )
        .arg(
            Arg::new("src")
                .short('s')
                .long("src")
                .value_name("SOURCE")
                .help("raw source text to check"),
        )
        .group(ArgGroup::new("input").args(["file", "src"]).required(true).multiple(false))
}

pub fn run(matches: &ArgMatches) -> bool {
    let (source_name, src) = match matches.get_one::<String>("file") {
        Some(path) => match fs::read_to_string(path) {
            Ok(src) => (path.as_str(), src),
            Err(error) => {
                eprintln!("error: could not read {path}: {error}");
                return false;
            }
        },
        None => {
            let src = matches
                .get_one::<String>("src")
                .expect("clap requires either --file or --src");
            ("<src>", src.clone())
        }
    };

    match compile(&src) {
        Ok(_) => {
            println!("{source_name}: valid");
            true
        }
        Err(diags) => {
            for diag in diags {
                eprintln!("{}", diag.render(source_name, &src));
            }
            false
        }
    }
}
