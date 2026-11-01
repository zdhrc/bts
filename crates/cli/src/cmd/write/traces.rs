use crate::cmd::Distribution;
use crate::config::{Settings, parse_duration};
use crate::diag::SourceDiags;
use crate::logging;
use crate::providers::{DataAdapter, TraceRun, braintrust};
use crate::{data, dsl};
use bts_core::mocking::data::Format as _;
use bts_core::providers::DataProvider;
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
    /// The bts shape file to generate from.
    #[arg(long, value_name = "PATH")]
    from: PathBuf,

    /// The number of top-level traces to generate.
    #[arg(long, value_name = "TRACES")]
    count: Option<NonZeroUsize>,

    /// The trace rate over the window, such as `20/h` or `0.5/m`.
    #[arg(long, value_name = "RATE", value_parser = parse_rate)]
    rate: Option<f64>,

    /// The time window ending now, such as `1h` or `30m`.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    over: Option<Duration>,

    /// How far back the window ends, such as `1d`. Use with `--over`.
    #[arg(long, value_name = "DURATION", value_parser = parse_duration, requires = "over")]
    offset: Option<Duration>,

    /// The window start, including a time zone offset, such as `2026-08-27T14:00:00Z`.
    #[arg(long, value_name = "TIMESTAMP", value_parser = parse_timestamp, requires = "end")]
    start: Option<SystemTime>,

    /// The window end, including a time zone offset. Use with `--start`.
    #[arg(long, value_name = "TIMESTAMP", value_parser = parse_timestamp, requires = "start")]
    end: Option<SystemTime>,

    /// How to distribute traces over the time window.
    #[arg(long, value_name = "SHAPE", value_enum, default_value_t)]
    dist: Distribution,

    /// The seed for random values. If omitted, a random seed is chosen and printed.
    #[arg(long, value_name = "SEED")]
    seed: Option<u64>,

    /// Select generated blocks by kind or name, for example `block.kind != "scorer"`.
    #[arg(long, value_name = "EXPR", value_parser = crate::cmd::parse_filter)]
    filter: Option<bts_core::selection::filter::WriteFilter>,

    /// Print the Braintrust payload without writing it.
    #[arg(long)]
    dry_run: bool,

    /// Print the final run summary as JSON on standard output.
    #[arg(long, conflicts_with = "dry_run")]
    json: bool,

    /// Print phase timings to standard error while running.
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

        let result = self.execute(&settings, log_path.as_deref(), &braintrust::Adapter);
        if let Err(error) = &result {
            tracing::error!(%error, "run failed");
            if let Some(path) = &log_path {
                eprintln!("run log: {}", path.display());
            }
        }

        result
    }

    // turn the flags into the count and time window used by core
    fn resolve_run(&self) -> Result<(usize, Duration, SystemTime), data::request::Error> {
        use data::request::{self, Volume, Window};
        let window = match (self.over, self.start, self.end) {
            (Some(over), None, None) => Window::Relative {
                over,
                offset: self.offset.unwrap_or_default(),
            },
            (None, Some(start), Some(end)) => Window::Absolute { start, end },
            _ => unreachable!("clap enforces exactly one window form"),
        };
        let volume = match (self.count, self.rate) {
            (Some(count), None) => Volume::Count(count),
            (None, Some(rate)) => Volume::Rate(rate),
            _ => unreachable!("clap enforces exactly one volume form"),
        };
        request::resolve(window, volume, SystemTime::now())
    }

    fn execute<A: DataAdapter>(
        self,
        settings: &Settings,
        log_path: Option<&Path>,
        adapter: &A,
    ) -> Result<(), ExecutionError<A>> {
        let started = Instant::now();
        let (count, over, until) = self.resolve_run().map_err(|error| match error {
            data::request::Error::EmptyWindow => TraceError::EmptyWindow,
            data::request::Error::OffsetOutOfRange => TraceError::OffsetOutOfRange,
            data::request::Error::NoTracesAtRate => TraceError::NoTracesAtRate,
            data::request::Error::InvalidRate => TraceError::Request(error),
        })?;
        let source = fs::read_to_string(&self.from).map_err(|source| TraceError::ReadShape {
            path: self.from.clone(),
            source,
        })?;
        let source_name = self.from.display().to_string();
        let model = tracing::info_span!("compile")
            .in_scope(|| dsl::semantics::compile(&source))
            .map_err(|diags| TraceError::InvalidShape {
                details: SourceDiags::new(&source_name, &source, diags),
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
        let default_filter = bts_core::selection::filter::WriteFilter::default();
        let provider = adapter.provider();
        let format = provider.format();
        let request = data::Request {
            model,
            count,
            over,
            distribution: self.dist.into(),
            until,
            seed,
            filter: self.filter.as_ref().unwrap_or(&default_filter),
            source: &self.from,
        };
        let prepare_error = |error| match error {
            data::PrepareError::Generate(data::Error::Plan(plan_error)) => TraceError::FailedGeneration {
                details: SourceDiags::new(&source_name, &source, vec![plan_error.into()]),
            },
            data::PrepareError::Generate(error) => TraceError::Generate(error),
            data::PrepareError::Attachments(source) => TraceError::ResolveAttachmentPaths {
                path: self.from.clone(),
                source,
            },
        };
        let events = data::prepare(request, &format).map_err(prepare_error)?;
        if self.dry_run {
            let encoded = tracing::info_span!("encode").in_scope(|| {
                let batch = format.encode(&events).map_err(TraceError::Format)?;
                adapter.preview(&batch).map_err(TraceError::Adapter)
            })?;
            println!("{encoded}");
            return Ok(());
        }

        let config = adapter.config(settings).map_err(TraceError::Adapter)?;
        let client = provider.client(&config).map_err(TraceError::Client)?;
        adapter.log_destination(&client);
        let receipt = data::write(&events, &format, &provider.sink(&client)).map_err(TraceError::Write)?;
        adapter
            .report_traces(
                &client,
                &receipt,
                TraceRun {
                    seed,
                    events: &events,
                    duration: started.elapsed(),
                    log: log_path,
                    json: self.json,
                },
            )
            .map_err(TraceError::Adapter)
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

pub type Error = ExecutionError<braintrust::Adapter>;

type ExecutionError<A> = TraceError<
    <A as DataAdapter>::Error,
    <<<A as DataAdapter>::Provider as DataProvider>::Format as data::Format>::Error,
    <<A as DataAdapter>::Provider as DataProvider>::ClientError,
    <<A as DataAdapter>::Provider as DataProvider>::SendError,
>;

#[derive(Debug)]
pub enum TraceError<A, F, C, S> {
    Request(data::request::Error),
    EmptyWindow,
    OffsetOutOfRange,
    NoTracesAtRate,
    ReadShape { path: PathBuf, source: std::io::Error },
    ResolveAttachmentPaths { path: PathBuf, source: std::io::Error },
    InvalidShape { details: SourceDiags },
    FailedGeneration { details: SourceDiags },
    Generate(data::Error),
    Config(crate::config::Error),
    Adapter(A),
    Format(F),
    Client(C),
    Write(data::WriteError<F, S>),
}

impl<A: fmt::Display, F: fmt::Display, C: fmt::Display, S: fmt::Display> fmt::Display for TraceError<A, F, C, S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(source) => write!(formatter, "generation failed:\n{source}"),
            Self::EmptyWindow => formatter.write_str("window end must be after its start"),
            Self::OffsetOutOfRange => formatter.write_str("offset reaches further back than timestamps can represent"),
            Self::NoTracesAtRate => formatter.write_str("rate over this window rounds to zero traces"),
            Self::ReadShape { path, source } => {
                write!(formatter, "could not read shape {}: {source}", path.display())
            }
            Self::ResolveAttachmentPaths { path, source } => {
                write!(
                    formatter,
                    "could not resolve attachments relative to {}: {source}",
                    path.display()
                )
            }
            Self::InvalidShape { details } => write!(formatter, "shape is invalid:\n{details}"),
            Self::FailedGeneration { details } => write!(formatter, "generation failed:\n{details}"),
            Self::Generate(source) => source.fmt(formatter),
            Self::Config(source) => source.fmt(formatter),
            Self::Adapter(source) => source.fmt(formatter),
            Self::Write(source) => source.fmt(formatter),
            Self::Format(source) => source.fmt(formatter),
            Self::Client(source) => source.fmt(formatter),
        }
    }
}

impl<A, F, C, S> std::error::Error for TraceError<A, F, C, S>
where
    A: std::error::Error + 'static,
    F: std::error::Error + 'static,
    C: std::error::Error + 'static,
    S: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FailedGeneration { details } | Self::InvalidShape { details } => Some(details),
            Self::Request(source) => Some(source),
            Self::ReadShape { source, .. } | Self::ResolveAttachmentPaths { source, .. } => Some(source),
            Self::Generate(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::Adapter(source) => Some(source),
            Self::Write(source) => Some(source),
            Self::Format(source) => Some(source),
            Self::Client(source) => Some(source),
            _ => None,
        }
    }
}

impl<A, F, C, S> From<crate::config::Error> for TraceError<A, F, C, S> {
    fn from(source: crate::config::Error) -> Self {
        Self::Config(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{Cli, Cmd};
    use clap::Parser as _;

    fn parse_write(argv: &[&str]) -> Result<Args, clap::Error> {
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Cmd::Write(super::super::Args {
                command: super::super::Cmd::Traces(args),
            }) => args,
            other => panic!("expected a write command, parsed {other:?}"),
        })
    }

    #[test]
    fn parses_requested_command_shape() {
        let args = parse_write(&[
            "bts",
            "write",
            "traces",
            "--from",
            "simple.bt",
            "--count",
            "25",
            "--over",
            "1h",
        ])
        .unwrap();

        assert_eq!(args.from, PathBuf::from("simple.bt"));
        assert_eq!(args.count.unwrap().get(), 25);
        assert_eq!(args.over, Some(Duration::from_secs(3_600)));
        assert_eq!(args.dist, Distribution::Linear);
        assert_eq!(args.seed, None);
        assert!(!args.dry_run);
        assert!(!args.json);
    }

    #[test]
    fn parses_the_absolute_window_and_rate_forms() {
        let args = parse_write(&[
            "bts",
            "write",
            "traces",
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
            "traces",
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
        assert!(parse_write(&["bts", "write", "traces", "--from", "simple.bt", "--count", "1"]).is_err());
        assert!(parse_write(&["bts", "write", "traces", "--from", "simple.bt", "--over", "1h"]).is_err());

        // the forms are mutually exclusive
        assert!(
            parse_write(&[
                "bts",
                "write",
                "traces",
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
                "traces",
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
                "traces",
                "--from",
                "simple.bt",
                "--count",
                "1",
                "--end",
                "2026-08-27T14:00:00Z"
            ])
            .is_err()
        );
        assert!(
            parse_write(&[
                "bts",
                "write",
                "traces",
                "--from",
                "simple.bt",
                "--count",
                "1",
                "--offset",
                "1d"
            ])
            .is_err()
        );
    }

    #[test]
    fn rejects_degenerate_resolved_runs() {
        let args = parse_write(&[
            "bts",
            "write",
            "traces",
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
        assert!(matches!(args.resolve_run(), Err(data::request::Error::EmptyWindow)));

        let args = parse_write(&[
            "bts",
            "write",
            "traces",
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
        assert!(matches!(args.resolve_run(), Err(data::request::Error::NoTracesAtRate)));
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
                "traces",
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
            "traces",
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

        assert_eq!(args.dist, Distribution::Sine);
    }

    #[test]
    fn rejects_zero_counts_and_invalid_durations() {
        assert!(
            parse_write(&[
                "bts",
                "write",
                "traces",
                "--from",
                "simple.bt",
                "--count",
                "0",
                "--over",
                "1h"
            ])
            .is_err()
        );
        assert!(
            parse_write(&[
                "bts",
                "write",
                "traces",
                "--from",
                "simple.bt",
                "--count",
                "1",
                "--over",
                "hour"
            ])
            .is_err()
        );
        assert!(
            parse_write(&[
                "bts",
                "write",
                "traces",
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
