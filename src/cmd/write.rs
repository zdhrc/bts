use crate::cmd::{logging, render_diags};
use crate::conf::{Braintrust, Settings, parse_duration};
use crate::{dsl, sdg};
use std::{
    env, fmt, fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
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

    /// print the final run summary as JSON on stdout
    #[arg(long, conflicts_with = "dry_run")]
    json: bool,

    /// print phase timings to stderr while running
    #[arg(long)]
    profile: bool,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let settings = Settings::load()?;
        let log_path = logging::init("write", self.profile, &settings);
        tracing::info!(
            version = env!("CARGO_PKG_VERSION"),
            argv = %env::args().skip(1).collect::<Vec<_>>().join(" "),
            "run started",
        );

        let result = self.execute(&settings, log_path.as_deref());
        if let Err(error) = &result {
            tracing::error!(%error, "run failed");
            if let Some(path) = &log_path {
                eprintln!("run log: {}", path.display());
            }
        }

        result
    }

    fn execute(self, settings: &Settings, log_path: Option<&Path>) -> Result<(), Error> {
        let started = Instant::now();
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
        tracing::info!(seed, "seed resolved");
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

        let mut config = Braintrust::from_env()?;
        config.request_timeout = settings.request_timeout;
        config.write_concurrency = settings.write_concurrency;
        tracing::info!(project_id = %config.project_id, api_url = %config.api_url, "writing to braintrust");
        let inserted = tracing::info_span!("write").in_scope(|| sdg::write(&config, &events))?;
        tracing::info!(
            traces = events.trace_count(),
            events = events.event_count(),
            rows = inserted.row_count(),
            "insert acknowledged",
        );

        if self.json {
            let summary = Summary {
                seed,
                traces: events.trace_count(),
                events: events.event_count(),
                rows: inserted.row_count(),
                project_id: config.project_id.to_string(),
                duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                log: log_path.map(|path| path.display().to_string()),
            };
            println!("{}", serde_json::to_string(&summary)?);
        } else {
            println!(
                "inserted {} traces and {} child spans into project {} ({} rows acknowledged)",
                events.trace_count(),
                events.event_count() - events.trace_count(),
                config.project_id,
                inserted.row_count(),
            );
        }

        Ok(())
    }
}

// machine-readable success summary for --json; errors stay human-readable on stderr
#[derive(serde::Serialize)]
struct Summary {
    seed: u64,
    traces: usize,
    events: usize,
    rows: usize,
    project_id: String,
    duration_ms: u64,
    log: Option<String>,
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

    fn parse_write(argv: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Cmd::Write(args) => args,
            other => panic!("expected a write command, parsed {other:?}"),
        })
    }

    #[test]
    fn parses_requested_command_shape() {
        let args = parse_write(&["bts", "write", "--from", "simple.bt", "--count", "25", "--over", "1h"]).unwrap();

        assert_eq!(args.from, PathBuf::from("simple.bt"));
        assert_eq!(args.count.get(), 25);
        assert_eq!(args.over, Duration::from_secs(3_600));
        assert_eq!(args.dist, sdg::Distribution::Linear);
        assert_eq!(args.seed, None);
        assert!(!args.dry_run);
        assert!(!args.json);
    }

    #[test]
    fn rejects_json_summaries_for_dry_runs() {
        assert!(
            parse_write(&[
                "bts",
                "write",
                "--from",
                "simple.bt",
                "--count",
                "1",
                "--over",
                "1h",
                "--dry-run",
                "--json",
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_the_sine_distribution() {
        let args = parse_write(&[
            "bts",
            "write",
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
        assert!(parse_write(&["bts", "write", "--from", "simple.bt", "--count", "0", "--over", "1h"]).is_err());
        assert!(parse_write(&["bts", "write", "--from", "simple.bt", "--count", "1", "--over", "hour"]).is_err());
        assert!(
            parse_write(&[
                "bts",
                "write",
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
