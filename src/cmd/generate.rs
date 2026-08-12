use crate::cmd::{parse_duration, render_diags};
use crate::{conf::Braintrust, dsl, sdg};
use std::{
    fmt, fs,
    num::NonZeroUsize,
    path::PathBuf,
    time::{Duration, SystemTime},
};

#[derive(Debug, clap::Args)]
#[command(about = "generate synthetic traces from a bts shape and write them to Braintrust")]
pub struct Args {
    /// bts shape file to generate from
    #[arg(long, value_name = "PATH")]
    from: PathBuf,

    /// exact number of top-level traces to generate
    #[arg(long, value_name = "TRACES")]
    count: NonZeroUsize,

    /// historical window over which to spread traces, such as 1h or 30m
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    over: Duration,

    /// how trace volume is distributed over the window
    #[arg(long, value_name = "SHAPE", value_enum, default_value_t)]
    dist: sdg::Distribution,

    /// seed for random value functions; a random seed is chosen and printed when omitted
    #[arg(long, value_name = "SEED")]
    seed: Option<u64>,

    /// print the Braintrust payload without writing it
    #[arg(long)]
    dry_run: bool,

    /// print phase timings to stderr while running
    #[arg(long)]
    profile: bool,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        if self.profile {
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
                .with_timer(tracing_subscriber::fmt::time::uptime())
                .with_target(false)
                .init();
        }

        let source = fs::read_to_string(&self.from).map_err(|source| Error::ReadShape {
            path: self.from.clone(),
            source,
        })?;
        let source_name = self.from.display().to_string();
        let model = tracing::info_span!("compile")
            .in_scope(|| dsl::compile(&source))
            .map_err(|diags| Error::InvalidShape {
                details: render_diags(&source_name, &source, &diags),
            })?;
        let seed = match self.seed {
            Some(seed) => seed,
            None => {
                let seed = rand::random();
                eprintln!("seed: {seed} (pass --seed {seed} to reproduce)");
                seed
            }
        };
        let events = tracing::info_span!("generate")
            .in_scope(|| sdg::generate(model, self.count.get(), self.over, self.dist, SystemTime::now(), seed))
            .map_err(|error| match error {
                // expression evaluation failures render like compile diagnostics with line:col
                sdg::Error::Plan(plan_error) => Error::FailedGeneration {
                    details: render_diags(
                        &source_name,
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

        if self.dry_run {
            let encoded = tracing::info_span!("encode").in_scope(|| serde_json::to_string_pretty(&events))?;
            println!("{encoded}");
            return Ok(());
        }

        let config = Braintrust::from_env()?;
        let inserted = tracing::info_span!("write").in_scope(|| sdg::write(&config, &events))?;
        println!(
            "inserted {} traces and {} child spans into project {} ({} rows acknowledged)",
            events.trace_count(),
            events.event_count() - events.trace_count(),
            config.project_id,
            inserted.row_count(),
        );

        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    ReadShape { path: PathBuf, source: std::io::Error },
    InvalidShape { details: String },
    FailedGeneration { details: String },
    Generate(sdg::Error),
    Config(crate::conf::Error),
    Write(sdg::writer::Error),
    Encode(serde_json::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadShape { path, source } => {
                write!(formatter, "could not read shape {}: {source}", path.display())
            }
            Self::InvalidShape { details } => write!(formatter, "shape is invalid:\n{details}"),
            Self::FailedGeneration { details } => write!(formatter, "generation failed:\n{details}"),
            Self::Generate(source) => source.fmt(formatter),
            Self::Config(source) => source.fmt(formatter),
            Self::Write(source) => source.fmt(formatter),
            Self::Encode(source) => write!(formatter, "failed to encode generated events as JSON: {source}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<sdg::Error> for Error {
    fn from(source: sdg::Error) -> Self {
        Self::Generate(source)
    }
}

impl From<crate::conf::Error> for Error {
    fn from(source: crate::conf::Error) -> Self {
        Self::Config(source)
    }
}

impl From<sdg::writer::Error> for Error {
    fn from(source: sdg::writer::Error) -> Self {
        Self::Write(source)
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Encode(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{Cli, Cmd};
    use clap::Parser as _;

    fn parse_generate(argv: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Cmd::Generate(args) => args,
            other => panic!("expected a generate command, parsed {other:?}"),
        })
    }

    #[test]
    fn parses_requested_command_shape() {
        let args = parse_generate(&["bts", "generate", "--from", "simple.bt", "--count", "25", "--over", "1h"]).unwrap();

        assert_eq!(args.from, PathBuf::from("simple.bt"));
        assert_eq!(args.count.get(), 25);
        assert_eq!(args.over, Duration::from_secs(3_600));
        assert_eq!(args.dist, sdg::Distribution::Linear);
        assert_eq!(args.seed, None);
        assert!(!args.dry_run);
    }

    #[test]
    fn parses_the_sine_distribution() {
        let args = parse_generate(&[
            "bts",
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

        assert_eq!(args.dist, sdg::Distribution::Sine);
    }

    #[test]
    fn rejects_zero_counts_and_invalid_durations() {
        assert!(parse_generate(&["bts", "generate", "--from", "simple.bt", "--count", "0", "--over", "1h"]).is_err());
        assert!(parse_generate(&["bts", "generate", "--from", "simple.bt", "--count", "1", "--over", "hour"]).is_err());
        assert!(
            parse_generate(&[
                "bts",
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
