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
#[command(group = clap::ArgGroup::new("window").required(true).args(["over", "start"]))]
#[command(group = clap::ArgGroup::new("volume").required(true).args(["count", "rate"]))]
pub struct Args {
    /// bts shape file to generate from
    #[arg(long, value_name = "PATH")]
    from: PathBuf,

    /// exact number of top-level traces to generate
    #[arg(long, value_name = "TRACES")]
    count: Option<NonZeroUsize>,

    /// trace volume as a rate over the window, such as 20/h or 0.5/m
    #[arg(long, value_name = "RATE", value_parser = parse_rate)]
    rate: Option<f64>,

    /// historical window over which to spread traces, such as 1h or 30m; ends at now
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    over: Option<Duration>,

    /// how far back from now the window ends, such as 1d; combines with --over
    #[arg(long, value_name = "DURATION", value_parser = parse_duration, requires = "over")]
    offset: Option<Duration>,

    /// absolute window start, RFC 3339 with an offset, such as 2026-08-27T14:00:00Z
    #[arg(long, value_name = "TIMESTAMP", value_parser = parse_timestamp, requires = "end")]
    start: Option<SystemTime>,

    /// absolute window end, RFC 3339 with an offset; pairs with --start
    #[arg(long, value_name = "TIMESTAMP", value_parser = parse_timestamp, requires = "start")]
    end: Option<SystemTime>,

    /// how trace volume is distributed over the window
    #[arg(long, value_name = "SHAPE", value_enum, default_value_t)]
    dist: sdg::Distribution,

    /// seed for random value functions; a random seed is chosen and printed when omitted
    #[arg(long, value_name = "SEED")]
    seed: Option<u64>,

    /// select generated blocks by kind or name, for example 'block.kind != "scorer"'
    #[arg(long, value_name = "EXPR")]
    filter: Option<dsl::WriteFilter>,

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

    // the flag groups reduce to the (count, over, until) triple generation consumes
    fn resolve_run(&self) -> Result<(usize, Duration, SystemTime), Error> {
        let (over, until) = match (self.over, self.start, self.end) {
            (Some(over), None, None) => {
                let now = SystemTime::now();
                let until = match self.offset {
                    Some(offset) => now.checked_sub(offset).ok_or(Error::OffsetOutOfRange)?,
                    None => now,
                };
                (over, until)
            }
            (None, Some(start), Some(end)) => {
                let over = end
                    .duration_since(start)
                    .ok()
                    .filter(|over| !over.is_zero())
                    .ok_or(Error::EmptyWindow)?;
                (over, end)
            }
            _ => unreachable!("clap enforces exactly one window form"),
        };

        let count = match (self.count, self.rate) {
            (Some(count), None) => count.get(),
            (None, Some(rate)) => {
                let count = (rate * over.as_secs_f64()).round() as usize;
                if count == 0 {
                    return Err(Error::NoTracesAtRate);
                }
                count
            }
            _ => unreachable!("clap enforces exactly one volume form"),
        };

        Ok((count, over, until))
    }

    fn execute(self, settings: &Settings, log_path: Option<&Path>) -> Result<(), Error> {
        let started = Instant::now();
        let (count, over, until) = self.resolve_run()?;
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
            .in_scope(|| {
                sdg::generate_filtered(
                    model,
                    count,
                    over,
                    self.dist,
                    until,
                    seed,
                    self.filter.as_ref().unwrap_or(&dsl::WriteFilter::default()),
                )
            })
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

        let mut config = Braintrust::load()?;
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

// rfc3339 with an explicit offset or Z, so a timestamp means the same instant everywhere
fn parse_timestamp(value: &str) -> Result<SystemTime, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(SystemTime::from)
        .map_err(|_| "timestamp must be RFC 3339 with an offset, like 2026-08-27T14:00:00Z".to_owned())
}

// traces per second from a <number>/<unit> literal like 20/h or 0.5/m
fn parse_rate(value: &str) -> Result<f64, String> {
    let (number, seconds) = if let Some(number) = value.strip_suffix("/ms") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix("/s") {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix("/m") {
        (number, 60.0)
    } else if let Some(number) = value.strip_suffix("/h") {
        (number, 3_600.0)
    } else if let Some(number) = value.strip_suffix("/d") {
        (number, 86_400.0)
    } else {
        return Err("rate must end in /ms, /s, /m, /h, or /d".to_owned());
    };
    let number = number
        .parse::<f64>()
        .map_err(|_| "rate must start with a number".to_owned())?;
    if !number.is_finite() || number <= 0.0 {
        return Err("rate must be greater than zero".to_owned());
    }

    Ok(number / seconds)
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
    EmptyWindow,
    OffsetOutOfRange,
    NoTracesAtRate,
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
            Self::EmptyWindow => formatter.write_str("window end must be after its start"),
            Self::OffsetOutOfRange => formatter.write_str("offset reaches further back than timestamps can represent"),
            Self::NoTracesAtRate => formatter.write_str("rate over this window rounds to zero traces"),
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
        assert_eq!(args.count.unwrap().get(), 25);
        assert_eq!(args.over, Some(Duration::from_secs(3_600)));
        assert_eq!(args.dist, sdg::Distribution::Linear);
        assert_eq!(args.seed, None);
        assert!(!args.dry_run);
        assert!(!args.json);
    }

    #[test]
    fn parses_the_absolute_window_and_rate_forms() {
        let args = parse_write(&[
            "bts",
            "write",
            "--from",
            "simple.bt",
            "--rate",
            "30/m",
            "--start",
            "2026-08-27T12:00:00Z",
            "--end",
            "2026-08-27T14:00:00Z",
        ])
        .unwrap();

        assert_eq!(args.rate, Some(0.5));
        assert_eq!(
            args.end.unwrap().duration_since(args.start.unwrap()).unwrap(),
            Duration::from_secs(7_200)
        );

        let (count, over, until) = args.resolve_run().unwrap();
        assert_eq!(count, 3_600);
        assert_eq!(over, Duration::from_secs(7_200));
        assert_eq!(until, args.end.unwrap());
    }

    #[test]
    fn offsets_shift_the_window_back_from_now() {
        let args = parse_write(&[
            "bts",
            "write",
            "--from",
            "simple.bt",
            "--count",
            "1",
            "--over",
            "1h",
            "--offset",
            "1d",
        ])
        .unwrap();

        let (_, _, until) = args.resolve_run().unwrap();
        let lag = SystemTime::now().duration_since(until).unwrap();
        assert!(lag >= Duration::from_secs(86_400));
        assert!(lag < Duration::from_secs(86_400 + 60));
    }

    #[test]
    fn requires_exactly_one_window_and_volume_form() {
        // a member of each group is required
        assert!(parse_write(&["bts", "write", "--from", "simple.bt", "--count", "1"]).is_err());
        assert!(parse_write(&["bts", "write", "--from", "simple.bt", "--over", "1h"]).is_err());

        // the forms are mutually exclusive
        assert!(
            parse_write(&[
                "bts",
                "write",
                "--from",
                "simple.bt",
                "--count",
                "1",
                "--rate",
                "1/h",
                "--over",
                "1h"
            ])
            .is_err()
        );
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
                "--start",
                "2026-08-27T12:00:00Z",
                "--end",
                "2026-08-27T14:00:00Z",
            ])
            .is_err()
        );

        // half of a pairing is not enough
        assert!(
            parse_write(&[
                "bts",
                "write",
                "--from",
                "simple.bt",
                "--count",
                "1",
                "--end",
                "2026-08-27T14:00:00Z"
            ])
            .is_err()
        );
        assert!(parse_write(&["bts", "write", "--from", "simple.bt", "--count", "1", "--offset", "1d"]).is_err());
    }

    #[test]
    fn rejects_degenerate_resolved_runs() {
        let args = parse_write(&[
            "bts",
            "write",
            "--from",
            "simple.bt",
            "--count",
            "1",
            "--start",
            "2026-08-27T14:00:00Z",
            "--end",
            "2026-08-27T14:00:00Z",
        ])
        .unwrap();
        assert!(matches!(args.resolve_run(), Err(Error::EmptyWindow)));

        let args = parse_write(&[
            "bts",
            "write",
            "--from",
            "simple.bt",
            "--rate",
            "1/d",
            "--start",
            "2026-08-27T13:59:59Z",
            "--end",
            "2026-08-27T14:00:00Z",
        ])
        .unwrap();
        assert!(matches!(args.resolve_run(), Err(Error::NoTracesAtRate)));
    }

    #[test]
    fn parses_timestamps() {
        let expected = parse_timestamp("2026-08-25T14:00:00Z").unwrap();
        assert_eq!(parse_timestamp("2026-08-25T09:00:00-05:00").unwrap(), expected);
        assert!(parse_timestamp("2026-08-25T14:00:00").is_err());
        assert!(parse_timestamp("2026-08-25").is_err());
        assert!(parse_timestamp("yesterday").is_err());
    }

    #[test]
    fn parses_rates() {
        assert_eq!(parse_rate("2/s").unwrap(), 2.0);
        assert_eq!(parse_rate("30/m").unwrap(), 0.5);
        assert_eq!(parse_rate("7200/h").unwrap(), 2.0);
        assert_eq!(parse_rate("0.5/s").unwrap(), 0.5);
        assert!(parse_rate("2").is_err());
        assert!(parse_rate("2/w").is_err());
        assert!(parse_rate("0/h").is_err());
        assert!(parse_rate("-1/h").is_err());
        assert!(parse_rate("fast/h").is_err());
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
