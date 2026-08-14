use crate::cmd::logging;
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{fmt, fs};

const VERDICT_WIDTH: usize = 80;

#[derive(Debug, clap::Args)]
#[command(about = "list recent run logs, or render one to read")]
pub struct Args {
    /// run log to render, by file name from the listing
    #[arg(value_name = "RUN", conflicts_with = "last")]
    run: Option<String>,

    /// render the most recent run log
    #[arg(long)]
    last: bool,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let dir = logging::logs_dir().map_err(Error::FindLogs)?;
        let mut runs = collect_runs(&dir)?;
        runs.sort_by_key(|run| std::cmp::Reverse(run.modified));

        if self.last {
            let run = runs.first().ok_or_else(|| Error::NoRuns { dir: dir.clone() })?;
            return render(&run.path);
        }
        if let Some(name) = self.run {
            let run = runs
                .iter()
                .find(|run| run.name == name || run.name == format!("{name}.jsonl"))
                .ok_or_else(|| Error::UnknownRun { name, dir: dir.clone() })?;
            return render(&run.path);
        }

        if runs.is_empty() {
            println!("no run logs under {}", dir.display());
            return Ok(());
        }
        for run in &runs {
            println!("{}  {}", run.name, run.verdict);
        }

        Ok(())
    }
}

struct RunLog {
    name: String,
    path: PathBuf,
    modified: SystemTime,
    verdict: String,
}

fn collect_runs(dir: &Path) -> Result<Vec<RunLog>, Error> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(Error::ReadLogs {
                dir: dir.to_owned(),
                source,
            });
        }
    };
    let runs = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "jsonl"))
        .filter_map(|entry| {
            let path = entry.path();
            let content = fs::read_to_string(&path).ok()?;

            Some(RunLog {
                name: entry.file_name().to_string_lossy().into_owned(),
                modified: entry.metadata().ok()?.modified().ok()?,
                verdict: verdict(&content),
                path,
            })
        })
        .collect();

    Ok(runs)
}

// a run is ok unless it logged an error; surface the last one so the listing reads as a diagnosis
fn verdict(content: &str) -> String {
    let error = content
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<JsonValue>(line).ok())
        .find(|event| event["level"] == "ERROR")
        .map(|event| {
            let fields = &event["fields"];
            fields["error"]
                .as_str()
                .or(fields["message"].as_str())
                .unwrap_or("unknown error")
                .to_owned()
        });

    match error {
        Some(message) => format!("failed: {}", truncate(&message)),
        None => "ok".to_owned(),
    }
}

fn truncate(text: &str) -> String {
    // errors can embed multiline diagnostics; the listing gets the first line only
    let line = text.lines().next().unwrap_or(text);
    match line.char_indices().nth(VERDICT_WIDTH) {
        Some((offset, _)) => format!("{}…", &line[..offset]),
        None => line.to_owned(),
    }
}

fn render(path: &Path) -> Result<(), Error> {
    let content = fs::read_to_string(path).map_err(|source| Error::ReadRun {
        path: path.to_owned(),
        source,
    })?;
    for line in content.lines() {
        println!("{}", render_line(line));
    }

    Ok(())
}

// one readable line per event: time, level, span path, message, then the structured fields
fn render_line(raw: &str) -> String {
    let Ok(event) = serde_json::from_str::<JsonValue>(raw) else {
        return raw.to_owned();
    };
    let time = event["timestamp"]
        .as_str()
        .and_then(|timestamp| timestamp.split('T').nth(1))
        .map(|time| time.trim_end_matches('Z'))
        .unwrap_or("");
    let time = &time[..time.len().min(12)];
    let level = event["level"].as_str().unwrap_or("?");
    let mut spans: Vec<&str> = event["spans"]
        .as_array()
        .map(|spans| spans.iter().filter_map(|span| span["name"].as_str()).collect())
        .unwrap_or_default();
    if let Some(name) = event["span"]["name"].as_str() {
        spans.push(name);
    }
    let context = if spans.is_empty() {
        String::new()
    } else {
        format!("[{}] ", spans.join("/"))
    };
    let fields = event["fields"].as_object();
    let message = fields
        .and_then(|fields| fields.get("message"))
        .and_then(|message| message.as_str())
        .unwrap_or("");
    let extra: String = fields
        .map(|fields| {
            fields
                .iter()
                .filter(|(key, _)| *key != "message")
                .map(|(key, value)| format!(" {key}={value}"))
                .collect()
        })
        .unwrap_or_default();

    format!("{time}  {level:<5} {context}{message}{extra}")
}

#[derive(Debug)]
pub enum Error {
    FindLogs(std::io::Error),
    ReadLogs { dir: PathBuf, source: std::io::Error },
    ReadRun { path: PathBuf, source: std::io::Error },
    NoRuns { dir: PathBuf },
    UnknownRun { name: String, dir: PathBuf },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FindLogs(source) => {
                write!(formatter, "could not determine the current directory: {source}")
            }
            Self::ReadLogs { dir, source } => {
                write!(formatter, "could not read {}: {source}", dir.display())
            }
            Self::ReadRun { path, source } => {
                write!(formatter, "could not read run log {}: {source}", path.display())
            }
            Self::NoRuns { dir } => write!(formatter, "no run logs under {}", dir.display()),
            Self::UnknownRun { name, dir } => {
                write!(formatter, "no run log named {name} under {}", dir.display())
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_span_close_events_with_their_path() {
        let raw = r#"{"timestamp":"2026-08-12T23:45:03.205329Z","level":"INFO","fields":{"message":"close","time.busy":"70.2µs","time.idle":"3.54µs"},"target":"bts::dsl","span":{"name":"parse"},"spans":[{"name":"compile"}]}"#;

        let line = render_line(raw);

        assert_eq!(
            line,
            r#"23:45:03.205  INFO  [compile/parse] close time.busy="70.2µs" time.idle="3.54µs""#
        );
    }

    #[test]
    fn renders_plain_events_with_their_fields() {
        let raw = r#"{"timestamp":"2026-08-12T23:45:03.205846Z","level":"INFO","fields":{"message":"seed resolved","seed":42},"target":"bts::cmd::generate"}"#;

        let line = render_line(raw);

        assert_eq!(line, "23:45:03.205  INFO  seed resolved seed=42");
    }

    #[test]
    fn passes_malformed_lines_through_untouched() {
        assert_eq!(render_line("not json"), "not json");
    }

    #[test]
    fn calls_a_run_without_errors_ok() {
        let content = r#"{"timestamp":"2026-08-12T23:45:03.204945Z","level":"INFO","fields":{"message":"run started"}}"#;

        assert_eq!(verdict(content), "ok");
    }

    #[test]
    fn surfaces_the_error_of_a_failed_run() {
        let content = concat!(
            r#"{"timestamp":"2026-08-12T23:45:03.204945Z","level":"INFO","fields":{"message":"run started"}}"#,
            "\n",
            r#"{"timestamp":"2026-08-12T23:45:40.460810Z","level":"ERROR","fields":{"message":"run failed","error":"environment variable BRAINTRUST_API_KEY is required"}}"#,
        );

        assert_eq!(
            verdict(content),
            "failed: environment variable BRAINTRUST_API_KEY is required"
        );
    }

    #[test]
    fn truncates_long_and_multiline_verdicts() {
        let long = "x".repeat(100);
        assert_eq!(truncate(&long), format!("{}…", "x".repeat(VERDICT_WIDTH)));
        assert_eq!(truncate("shape is invalid:\n --> shape.bt:3:1"), "shape is invalid:");
    }
}
