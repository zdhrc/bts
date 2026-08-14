use crate::cmd::render_diags;
use crate::conf::parse_duration;
use crate::{dsl, sdg};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use std::{fmt, fs, io};
use tracing::span;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;

#[derive(Debug, clap::Args)]
#[command(about = "run the generation pipeline without writing and report phase timings")]
pub struct Args {
    /// path to a source file to check, or - to read stdin
    #[arg(value_name = "PATH")]
    path: PathBuf,

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

    /// how many times to run the pipeline; more runs smooth out noise
    #[arg(long, value_name = "N", default_value_t = NonZeroUsize::MIN)]
    iterations: NonZeroUsize,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let (source_name, source) = if self.path == Path::new("-") {
            let source = io::read_to_string(io::stdin()).map_err(Error::ReadStdin)?;
            ("<stdin>".to_owned(), source)
        } else {
            let source = fs::read_to_string(&self.path).map_err(|source| Error::Read {
                path: self.path.clone(),
                source,
            })?;
            (self.path.display().to_string(), source)
        };
        let seed = match self.seed {
            Some(seed) => seed,
            None => {
                let seed = rand::random();
                eprintln!("seed: {seed} (pass --seed {seed} to reproduce)");
                seed
            }
        };
        let recorder = Arc::new(Recorder::default());
        let subscriber = tracing_subscriber::registry().with(TimingLayer {
            recorder: Arc::clone(&recorder),
        });

        let summary =
            tracing::subscriber::with_default(subscriber, || {
                let mut summary = None;

                for _ in 0..self.iterations.get() {
                    let started = Instant::now();
                    let model = tracing::info_span!("compile")
                        .in_scope(|| dsl::compile(&source))
                        .map_err(|diags| Error::Invalid {
                            details: render_diags(&source_name, &source, &diags),
                        })?;
                    let events = sdg::generate(model, self.count.get(), self.over, self.dist, SystemTime::now(), seed)
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
                    let stats = sdg::pack_stats(&events).map_err(Error::Pack)?;
                    recorder.record("total", started.elapsed());
                    summary = Some(Summary {
                        event_count: events.event_count(),
                        trace_count: events.trace_count(),
                        body_bytes: stats.body_bytes,
                        payload_count: stats.payload_count,
                    });
                }

                Ok(summary.expect("iterations is non-zero"))
            })?;

        let timings = recorder.timings.lock().expect("timings lock");
        println!("{}", render_report(&timings));
        println!();
        println!(
            "{} ({}), {} serialized, {}",
            plural(summary.event_count, "event"),
            plural(summary.trace_count, "trace"),
            human_bytes(summary.body_bytes),
            plural(summary.payload_count, "payload"),
        );

        Ok(())
    }
}

struct Summary {
    event_count: usize,
    trace_count: usize,
    body_bytes: usize,
    payload_count: usize,
}

#[derive(Default)]
struct Recorder {
    timings: Mutex<Timings>,
}

#[derive(Default)]
struct Timings {
    order: Vec<String>,
    durations: HashMap<String, Vec<Duration>>,
}

impl Recorder {
    // reserves a row so parents appear before the children that close first
    fn register(&self, path: &str) {
        let mut timings = self.timings.lock().expect("timings lock");
        if !timings.order.iter().any(|known| known == path) {
            timings.order.push(path.to_owned());
        }
    }

    fn record(&self, path: &str, elapsed: Duration) {
        self.register(path);
        let mut timings = self.timings.lock().expect("timings lock");
        timings.durations.entry(path.to_owned()).or_default().push(elapsed);
    }
}

struct TimingLayer {
    recorder: Arc<Recorder>,
}

// stashed in span extensions between open and close
struct Timing {
    path: String,
    started: Instant,
}

impl<S> Layer<S> for TimingLayer
where
    S: tracing::Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, _attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let path = span
            .scope()
            .from_root()
            .map(|ancestor| ancestor.name())
            .collect::<Vec<_>>()
            .join(":");
        self.recorder.register(&path);
        span.extensions_mut().insert(Timing {
            path,
            started: Instant::now(),
        });
    }

    fn on_close(&self, id: span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let extensions = span.extensions();
        let Some(timing) = extensions.get::<Timing>() else { return };
        self.recorder.record(&timing.path, timing.started.elapsed());
    }
}

fn render_report(timings: &Timings) -> String {
    let rows: Vec<(&String, &Vec<Duration>)> = timings
        .order
        .iter()
        .filter_map(|path| timings.durations.get(path).map(|durations| (path, durations)))
        .filter(|(_, durations)| !durations.is_empty())
        .collect();
    let names: Vec<String> = rows
        .iter()
        .map(|(path, _)| {
            let depth = path.matches(':').count();
            let name = path.rsplit(':').next().unwrap_or(path);
            format!("{}{name}", "  ".repeat(depth))
        })
        .collect();
    let single = rows.iter().all(|(_, durations)| durations.len() == 1);
    let labels: &[&str] = if single { &[""] } else { &["mean ", "min ", "max "] };
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|(_, durations)| {
            if single {
                return vec![render_duration(durations[0])];
            }
            let total: Duration = durations.iter().sum();
            vec![
                render_duration(total / durations.len() as u32),
                render_duration(*durations.iter().min().expect("non-empty")),
                render_duration(*durations.iter().max().expect("non-empty")),
            ]
        })
        .collect();
    // widths in chars, not bytes, so µs cells line up
    let width = |text: &str| text.chars().count();
    let name_width = names.iter().map(|name| width(name)).max().unwrap_or(0);
    let cell_widths: Vec<usize> = (0..labels.len())
        .map(|column| cells.iter().map(|row| width(&row[column])).max().unwrap_or(0))
        .collect();

    names
        .iter()
        .zip(&cells)
        .map(|(name, row)| {
            let mut line = name.clone();
            line.push_str(&" ".repeat(name_width - width(name)));
            for ((cell, cell_width), label) in row.iter().zip(&cell_widths).zip(labels) {
                line.push_str("  ");
                line.push_str(label);
                line.push_str(&" ".repeat(cell_width - width(cell)));
                line.push_str(cell);
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    if nanos >= 1_000_000_000 {
        format!("{:.2}s", duration.as_secs_f64())
    } else if nanos >= 1_000_000 {
        format!("{:.1}ms", nanos as f64 / 1e6)
    } else if nanos >= 1_000 {
        format!("{:.1}µs", nanos as f64 / 1e3)
    } else {
        format!("{nanos}ns")
    }
}

fn human_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let exact = bytes as f64;
    if exact >= MIB {
        format!("{:.1} MiB", exact / MIB)
    } else if exact >= KIB {
        format!("{:.1} KiB", exact / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn plural(count: usize, noun: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("{} {noun}{suffix}", group_digits(count))
}

fn group_digits(value: usize) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }

    grouped
}

#[derive(Debug)]
pub enum Error {
    Read { path: PathBuf, source: io::Error },
    ReadStdin(io::Error),
    Invalid { details: String },
    FailedGeneration { details: String },
    Generate(sdg::Error),
    Pack(sdg::writer::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(formatter, "could not read {}: {source}", path.display()),
            Self::ReadStdin(source) => write!(formatter, "could not read stdin: {source}"),
            Self::Invalid { details } => write!(formatter, "shape is invalid:\n{details}"),
            Self::FailedGeneration { details } => write!(formatter, "generation failed:\n{details}"),
            Self::Generate(source) => source.fmt(formatter),
            Self::Pack(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_span_timings_in_first_seen_order() {
        let recorder = Arc::new(Recorder::default());
        let subscriber = tracing_subscriber::registry().with(TimingLayer {
            recorder: Arc::clone(&recorder),
        });

        tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!("compile").in_scope(|| {
                tracing::info_span!("lex").in_scope(|| {});
                tracing::info_span!("parse").in_scope(|| {});
            });
            tracing::info_span!("plan").in_scope(|| {});
        });
        recorder.record("total", Duration::from_millis(1));

        let timings = recorder.timings.lock().unwrap();
        assert_eq!(timings.order, ["compile", "compile:lex", "compile:parse", "plan", "total"]);
        assert_eq!(timings.durations["compile"].len(), 1);
        assert!(timings.durations["compile"][0] >= timings.durations["compile:lex"][0]);
    }

    #[test]
    fn renders_nested_rows_with_single_values() {
        let timings = Timings {
            order: vec!["compile".to_owned(), "compile:lex".to_owned(), "plan".to_owned()],
            durations: HashMap::from([
                ("compile".to_owned(), vec![Duration::from_millis(2)]),
                ("compile:lex".to_owned(), vec![Duration::from_micros(300)]),
                ("plan".to_owned(), vec![Duration::from_millis(120)]),
            ]),
        };

        let report = render_report(&timings);

        assert_eq!(report, "compile    2.0ms\n  lex    300.0µs\nplan     120.0ms");
    }

    #[test]
    fn renders_spread_columns_for_multiple_iterations() {
        let timings = Timings {
            order: vec!["plan".to_owned()],
            durations: HashMap::from([(
                "plan".to_owned(),
                vec![Duration::from_millis(100), Duration::from_millis(200)],
            )]),
        };

        let report = render_report(&timings);

        assert_eq!(report, "plan  mean 150.0ms  min 100.0ms  max 200.0ms");
    }

    #[test]
    fn formats_counts_and_sizes() {
        assert_eq!(plural(1, "payload"), "1 payload");
        assert_eq!(plural(6214, "event"), "6,214 events");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(19_188_940), "18.3 MiB");
        assert_eq!(render_duration(Duration::from_nanos(950)), "950ns");
        assert_eq!(render_duration(Duration::from_secs(2)), "2.00s");
    }
}
