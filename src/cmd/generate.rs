use crate::{conf::Braintrust, dsl, sdg};
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::{
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use thiserror::Error as Err;

pub fn command() -> Command {
    Command::new("generate")
        .about("generate synthetic traces from a BTS shape and write them to Braintrust")
        .arg(
            Arg::new("from")
                .long("from")
                .value_name("PATH")
                .help("BTS shape file to generate from")
                .value_parser(clap::value_parser!(PathBuf))
                .required(true),
        )
        .arg(
            Arg::new("count")
                .long("count")
                .value_name("TRACES")
                .help("exact number of top-level traces to generate")
                .value_parser(clap::value_parser!(NonZeroUsize))
                .required(true),
        )
        .arg(
            Arg::new("over")
                .long("over")
                .value_name("DURATION")
                .help("historical window over which to spread traces, such as 1h or 30m")
                .value_parser(parse_duration)
                .required(true),
        )
        .arg(
            Arg::new("dist")
                .long("dist")
                .value_name("SHAPE")
                .help("how trace volume is distributed over the window: linear or sine")
                .value_parser(parse_distribution)
                .default_value("linear"),
        )
        .arg(
            Arg::new("seed")
                .long("seed")
                .value_name("SEED")
                .help("seed for random value functions; a random seed is chosen and printed when omitted")
                .value_parser(clap::value_parser!(u64)),
        )
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .help("print the Braintrust payload without writing it")
                .action(ArgAction::SetTrue),
        )
}

pub fn run(matches: &ArgMatches) -> bool {
    match execute(matches) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("error: {error}");
            false
        }
    }
}

fn execute(matches: &ArgMatches) -> Result<(), Error> {
    let path = matches.get_one::<PathBuf>("from").expect("clap requires --from");
    let count = matches.get_one::<NonZeroUsize>("count").expect("clap requires --count").get();
    let over = *matches.get_one::<Duration>("over").expect("clap requires --over");
    let distribution = *matches.get_one::<sdg::Distribution>("dist").expect("clap defaults --dist");
    let source = fs::read_to_string(path).map_err(|source| Error::ReadShape {
        path: path.clone(),
        source,
    })?;
    let model = dsl::compile(&source).map_err(|diags| Error::InvalidShape {
        details: render_diags(path, &source, &diags),
    })?;
    let seed = match matches.get_one::<u64>("seed") {
        Some(seed) => *seed,
        None => {
            let seed = rand::random();
            eprintln!("seed: {seed} (pass --seed {seed} to reproduce)");
            seed
        }
    };
    let events = sdg::generate(model, count, over, distribution, SystemTime::now(), seed).map_err(|error| match error {
        // expression evaluation failures render like compile diagnostics with line:col
        sdg::GenerateError::Plan(plan_error) => Error::FailedGeneration {
            details: render_diags(
                path,
                &source,
                &vec![dsl::Diag {
                    when: dsl::DiagPhase::Generation,
                    what: plan_error.to_string(),
                    r#where: plan_error.range,
                }],
            ),
        },
        other => Error::Generate(other),
    })?;

    if matches.get_flag("dry-run") {
        println!("{}", serde_json::to_string_pretty(&events)?);
        return Ok(());
    }

    let config = Braintrust::from_env()?;
    let inserted = sdg::write(&config, &events)?;
    println!(
        "inserted {} traces and {} child spans into project {} ({} rows acknowledged)",
        events.trace_count(),
        events.event_count() - events.trace_count(),
        config.project_id,
        inserted.row_count(),
    );

    Ok(())
}

fn render_diags(path: &Path, src: &str, diags: &dsl::Diags) -> String {
    let source_name = path.display().to_string();

    diags
        .iter()
        .map(|diag| diag.render(&source_name, src))
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_distribution(value: &str) -> Result<sdg::Distribution, String> {
    match value {
        "linear" => Ok(sdg::Distribution::Linear),
        "sine" => Ok(sdg::Distribution::Sine),
        _ => Err("distribution must be linear or sine".to_owned()),
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000)
    } else if let Some(number) = value.strip_suffix('d') {
        (number, 86_400_000)
    } else {
        return Err("duration must end in ms, s, m, h, or d".to_owned());
    };
    let number = number
        .parse::<u64>()
        .map_err(|_| "duration must start with a whole number".to_owned())?;
    let milliseconds = number
        .checked_mul(multiplier)
        .filter(|value| *value > 0)
        .ok_or_else(|| "duration must be greater than zero and within range".to_owned())?;

    Ok(Duration::from_millis(milliseconds))
}

#[derive(Debug, Err)]
enum Error {
    #[error("could not read shape {}: {source}", path.display())]
    ReadShape {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("shape is invalid:\n{details}")]
    InvalidShape { details: String },

    #[error("generation failed:\n{details}")]
    FailedGeneration { details: String },

    #[error(transparent)]
    Generate(#[from] sdg::GenerateError),

    #[error(transparent)]
    Config(#[from] crate::conf::Error),

    #[error(transparent)]
    Write(#[from] sdg::WriteError),

    #[error("failed to encode generated events as JSON")]
    Encode(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requested_command_shape() {
        let matches = command()
            .try_get_matches_from(["generate", "--from", "simple.bt", "--count", "25", "--over", "1h"])
            .unwrap();

        assert_eq!(matches.get_one::<PathBuf>("from").unwrap(), &PathBuf::from("simple.bt"));
        assert_eq!(matches.get_one::<NonZeroUsize>("count").unwrap().get(), 25);
        assert_eq!(*matches.get_one::<Duration>("over").unwrap(), Duration::from_secs(3_600));
        assert_eq!(
            *matches.get_one::<sdg::Distribution>("dist").unwrap(),
            sdg::Distribution::Linear
        );
        assert!(!matches.get_flag("dry-run"));
    }

    #[test]
    fn parses_the_sine_distribution() {
        let matches = command()
            .try_get_matches_from([
                "generate",
                "--from",
                "simple.bt",
                "--count",
                "25",
                "--over",
                "1h",
                "--dist",
                "sine",
            ])
            .unwrap();

        assert_eq!(
            *matches.get_one::<sdg::Distribution>("dist").unwrap(),
            sdg::Distribution::Sine
        );
    }

    #[test]
    fn rejects_zero_counts_and_invalid_durations() {
        assert!(
            command()
                .try_get_matches_from(["generate", "--from", "simple.bt", "--count", "0", "--over", "1h"])
                .is_err()
        );
        assert!(
            command()
                .try_get_matches_from(["generate", "--from", "simple.bt", "--count", "1", "--over", "hour"])
                .is_err()
        );
        assert!(
            command()
                .try_get_matches_from([
                    "generate",
                    "--from",
                    "simple.bt",
                    "--count",
                    "1",
                    "--over",
                    "1h",
                    "--dist",
                    "cosine",
                ])
                .is_err()
        );
    }
}
