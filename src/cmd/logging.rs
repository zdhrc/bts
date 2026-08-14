use crate::conf::{self, Settings};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;
use std::{env, fs};
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

// run logs park under the braintrust cli's project directory, namespaced to bts
const LOGS_DIR: &str = ".bt/bts/logs";

/// installs the global subscriber: a json-lines run log under .bt/bts/logs, plus the
/// human-readable stderr layer when --profile is set. logging must never break a run,
/// so any filesystem failure downgrades to a stderr warning and we go on without the file.
/// returns the run log path so callers can point at it when a run fails.
pub(crate) fn init(command: &str, profile: bool, settings: &Settings) -> Option<PathBuf> {
    // BTS_LOG overrides the configured level; both accept tracing filter directives
    let directives = match env::var("BTS_LOG") {
        Ok(value) if !value.trim().is_empty() => match conf::validate_log_level(value.trim()) {
            Ok(()) => value,
            Err(reason) => {
                eprintln!("warning: ignoring invalid BTS_LOG {value:?}: {reason}");
                settings.log_level.clone()
            }
        },
        _ => settings.log_level.clone(),
    };
    // "off" disables file logging entirely rather than leaving empty run logs behind
    let (path, file) = if directives.trim().eq_ignore_ascii_case("off") {
        (None, None)
    } else {
        match create_run_log(command, settings.keep_runs) {
            Ok((path, file)) => (Some(path), Some(file)),
            Err(error) => {
                eprintln!("warning: run logging disabled: {error}");
                (None, None)
            }
        }
    };
    let stderr_layer = profile.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_span_events(FmtSpan::CLOSE)
            .with_timer(tracing_subscriber::fmt::time::uptime())
            .with_target(false)
            .with_filter(LevelFilter::INFO)
    });
    let file_layer = file.map(|file| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(Mutex::new(file))
            .with_span_events(FmtSpan::CLOSE)
            // directives were validated wherever they came from, so the lossy parse is safe
            .with_filter(EnvFilter::new(&directives))
    });
    let _ = tracing_subscriber::registry().with(file_layer).with(stderr_layer).try_init();

    path
}

pub(crate) fn logs_dir() -> std::io::Result<PathBuf> {
    Ok(conf::project_root()?.join(LOGS_DIR))
}

fn create_run_log(command: &str, keep_runs: usize) -> std::io::Result<(PathBuf, fs::File)> {
    let dir = logs_dir()?;
    fs::create_dir_all(&dir)?;
    // keep run logs out of version control without asking users to edit their .gitignore
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        let _ = fs::write(&gitignore, "*\n");
    }
    prune(&dir, keep_runs);
    let timestamp = chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).format("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("{command}-{timestamp}-{}.jsonl", std::process::id()));
    let file = fs::File::create(&path)?;

    Ok((path, file))
}

// retention instead of rotation: newest keep_runs files win, counting the one about to be created
fn prune(dir: &Path, keep_runs: usize) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut runs: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "jsonl"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.0));
    for (_, path) in runs.into_iter().skip(keep_runs.saturating_sub(1)) {
        let _ = fs::remove_file(path);
    }
}
