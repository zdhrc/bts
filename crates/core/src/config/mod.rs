mod init;
pub use init::{InitError, initialize};

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::{env, fmt, fs, time::Duration};

const CONFIG_PATH: &str = ".bt/bts/config.toml";
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_WRITE_CONCURRENCY: usize = 8;
pub(crate) const DEFAULT_MAX_ATTACHMENT_UPLOADS: usize = 16;
pub(crate) const DEFAULT_MAX_ATTACHMENT_FILE_BYTES: u64 = 20 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_ATTACHMENT_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_KEPT_RUNS: usize = 20;

// runtime behavior from .bt/bts/config.toml; an absent file means the defaults
#[derive(Debug)]
pub struct Settings {
    pub log_level: String,
    pub keep_runs: usize,
    pub request_timeout: Duration,
    pub write_concurrency: usize,
    pub retry_attempts: u32,
    pub retry_max_elapsed: Duration,
    pub max_attachment_uploads: usize,
    pub max_attachment_file_bytes: u64,
    pub max_attachment_total_bytes: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            log_level: DEFAULT_LOG_LEVEL.to_owned(),
            keep_runs: DEFAULT_KEPT_RUNS,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            write_concurrency: DEFAULT_WRITE_CONCURRENCY,
            retry_attempts: 8,
            retry_max_elapsed: Duration::from_secs(120),
            max_attachment_uploads: DEFAULT_MAX_ATTACHMENT_UPLOADS,
            max_attachment_file_bytes: DEFAULT_MAX_ATTACHMENT_FILE_BYTES,
            max_attachment_total_bytes: DEFAULT_MAX_ATTACHMENT_TOTAL_BYTES,
        }
    }
}

impl Settings {
    pub fn load() -> Result<Self, Error> {
        let root = project_root().map_err(Error::CurrentDirectory)?;
        let path = root.join(CONFIG_PATH);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(Error::ReadConfig { path, source }),
        };

        Self::parse(&raw, &path)
    }

    pub fn parse(raw: &str, path: &Path) -> Result<Self, Error> {
        let file: FileSettings = toml::from_str(raw).map_err(|source| Error::ParseConfig {
            path: path.to_owned(),
            source,
        })?;
        let mut settings = Self::default();

        if let Some(level) = file.log.level {
            // validate now so a typo fails the run instead of silently logging nothing
            validate_log_level(&level).map_err(|reason| Error::InvalidLogLevel {
                path: path.to_owned(),
                value: level.clone(),
                reason,
            })?;
            settings.log_level = level;
        }
        if let Some(keep_runs) = file.log.keep_runs {
            settings.keep_runs = keep_runs;
        }
        if let Some(timeout) = file.http.request_timeout {
            settings.request_timeout = parse_duration(&timeout).map_err(|reason| Error::InvalidTimeout {
                path: path.to_owned(),
                value: timeout.clone(),
                reason,
            })?;
        }
        if let Some(concurrency) = file.http.write_concurrency {
            if concurrency == 0 {
                return Err(Error::InvalidConcurrency { path: path.to_owned() });
            }
            settings.write_concurrency = concurrency;
        }
        if let Some(attempts) = file.http.retry_attempts {
            if attempts == 0 {
                return Err(Error::InvalidRetryAttempts { path: path.to_owned() });
            }
            settings.retry_attempts = attempts;
        }
        if let Some(timeout) = file.http.retry_max_elapsed {
            settings.retry_max_elapsed = parse_duration(&timeout).map_err(|reason| Error::InvalidTimeout {
                path: path.to_owned(),
                value: timeout,
                reason,
            })?;
        }
        if let Some(value) = file.attachments.max_uploads {
            if value == 0 {
                return Err(Error::InvalidAttachmentLimit {
                    path: path.to_owned(),
                    name: "max_uploads",
                });
            }
            settings.max_attachment_uploads = value;
        }
        for (name, value, target) in [
            (
                "max_file_bytes",
                file.attachments.max_file_bytes,
                &mut settings.max_attachment_file_bytes,
            ),
            (
                "max_total_bytes",
                file.attachments.max_total_bytes,
                &mut settings.max_attachment_total_bytes,
            ),
        ] {
            if let Some(value) = value {
                if value == 0 {
                    return Err(Error::InvalidAttachmentLimit {
                        path: path.to_owned(),
                        name,
                    });
                }
                *target = value;
            }
        }

        Ok(settings)
    }
}

// unknown keys are rejected so config typos fail loudly instead of silently doing nothing
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct FileSettings {
    log: LogSection,
    http: HttpSection,
    attachments: AttachmentSection,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct AttachmentSection {
    max_uploads: Option<usize>,
    max_file_bytes: Option<u64>,
    max_total_bytes: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct LogSection {
    level: Option<String>,
    keep_runs: Option<usize>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct HttpSection {
    request_timeout: Option<String>,
    write_concurrency: Option<usize>,
    retry_attempts: Option<u32>,
    retry_max_elapsed: Option<String>,
}

// bare values must be real levels so typos fail; the env-filter parser alone would accept
// any word as a target directive. strings with '=' are genuine directives and parse as such.
pub fn validate_log_level(value: &str) -> Result<(), LogLevelError> {
    if value.contains('=') {
        return tracing_subscriber::filter::EnvFilter::try_new(value)
            .map(|_| ())
            .map_err(LogLevelError::Filter);
    }

    value
        .parse::<tracing_subscriber::filter::LevelFilter>()
        .map(|_| ())
        .map_err(|_| LogLevelError::ExpectedLevel)
}

// the project root is the nearest ancestor that already has a .bt directory, or the
// current directory when none has been set up yet
pub fn project_root() -> std::io::Result<PathBuf> {
    let current_dir = env::current_dir()?;
    let root = current_dir
        .ancestors()
        .find(|dir| dir.join(".bt").is_dir())
        .unwrap_or(&current_dir);

    Ok(root.to_path_buf())
}

pub fn parse_duration(value: &str) -> Result<Duration, DurationError> {
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
        return Err(DurationError::ExpectedUnit);
    };
    let number = number.parse::<u64>().map_err(|_| DurationError::ExpectedNumber)?;
    let milliseconds = number
        .checked_mul(multiplier)
        .filter(|value| *value > 0)
        .ok_or(DurationError::OutOfRange)?;

    Ok(Duration::from_millis(milliseconds))
}

#[derive(Debug)]
pub enum Error {
    CurrentDirectory(std::io::Error),
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    ParseConfig {
        path: PathBuf,
        source: toml::de::Error,
    },
    InvalidLogLevel {
        path: PathBuf,
        value: String,
        reason: LogLevelError,
    },
    InvalidTimeout {
        path: PathBuf,
        value: String,
        reason: DurationError,
    },
    InvalidConcurrency {
        path: PathBuf,
    },
    InvalidRetryAttempts {
        path: PathBuf,
    },
    InvalidAttachmentLimit {
        path: PathBuf,
        name: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(source) => {
                write!(formatter, "could not determine the current directory: {source}")
            }
            Self::ReadConfig { path, source } => {
                write!(formatter, "could not read config {}: {source}", path.display())
            }
            Self::ParseConfig { path, source } => {
                write!(formatter, "invalid config {}: {source}", path.display())
            }
            Self::InvalidLogLevel { path, value, reason } => {
                write!(formatter, "invalid log.level {value:?} in {}: {reason}", path.display())
            }
            Self::InvalidTimeout { path, value, reason } => {
                write!(
                    formatter,
                    "invalid http.request_timeout {value:?} in {}: {reason}",
                    path.display()
                )
            }
            Self::InvalidRetryAttempts { path } => write!(
                formatter,
                "invalid http.retry_attempts in {}: must be greater than zero",
                path.display()
            ),
            Self::InvalidConcurrency { path } => {
                write!(
                    formatter,
                    "invalid http.write_concurrency in {}: must be greater than zero",
                    path.display()
                )
            }
            Self::InvalidAttachmentLimit { path, name } => {
                write!(
                    formatter,
                    "invalid attachments.{name} in {}: must be greater than zero",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentDirectory(source) => Some(source),
            Self::InvalidLogLevel { reason, .. } => Some(reason),
            Self::InvalidTimeout { reason, .. } => Some(reason),
            Self::ReadConfig { source, .. } => Some(source),
            Self::ParseConfig { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A duration value that does not satisfy the configuration syntax or bounds.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DurationError {
    ExpectedUnit,
    ExpectedNumber,
    OutOfRange,
}
impl fmt::Display for DurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ExpectedUnit => "duration must end in ms, s, m, h, or d",
            Self::ExpectedNumber => "duration must start with a whole number",
            Self::OutOfRange => "duration must be greater than zero and within range",
        })
    }
}
impl std::error::Error for DurationError {}

/// A log level or filter directive that cannot be interpreted.
#[derive(Debug)]
pub enum LogLevelError {
    ExpectedLevel,
    Filter(tracing_subscriber::filter::ParseError),
}
impl fmt::Display for LogLevelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedLevel => formatter.write_str("expected off, error, warn, info, debug, or trace"),
            Self::Filter(source) => source.fmt(formatter),
        }
    }
}
impl std::error::Error for LogLevelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Filter(source) => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<Settings, Error> {
        Settings::parse(raw, Path::new("config.toml"))
    }

    #[test]
    fn parses_a_full_config() {
        let settings = parse(
            r#"
            [log]
            level = "debug"
            keep_runs = 5

            [http]
            request_timeout = "2m"
            write_concurrency = 4
            retry_attempts = 5
            retry_max_elapsed = "90s"

            [attachments]
            max_uploads = 3
            max_file_bytes = 1024
            max_total_bytes = 2048
            "#,
        )
        .unwrap();

        assert_eq!(settings.log_level, "debug");
        assert_eq!(settings.keep_runs, 5);
        assert_eq!(settings.request_timeout, Duration::from_secs(120));
        assert_eq!(settings.write_concurrency, 4);
        assert_eq!(settings.retry_attempts, 5);
        assert_eq!(settings.retry_max_elapsed, Duration::from_secs(90));
        assert_eq!(settings.max_attachment_uploads, 3);
        assert_eq!(settings.max_attachment_file_bytes, 1024);
        assert_eq!(settings.max_attachment_total_bytes, 2048);
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        for raw in ["", "[log]\n", "[log]\nlevel = \"warn\"\n"] {
            let settings = parse(raw).unwrap();
            assert_eq!(settings.keep_runs, DEFAULT_KEPT_RUNS);
            assert_eq!(settings.request_timeout, DEFAULT_REQUEST_TIMEOUT);
            assert_eq!(settings.write_concurrency, DEFAULT_WRITE_CONCURRENCY);
            assert_eq!(settings.max_attachment_uploads, DEFAULT_MAX_ATTACHMENT_UPLOADS);
            assert_eq!(settings.max_attachment_file_bytes, DEFAULT_MAX_ATTACHMENT_FILE_BYTES);
            assert_eq!(settings.max_attachment_total_bytes, DEFAULT_MAX_ATTACHMENT_TOTAL_BYTES);
        }
        assert_eq!(parse("").unwrap().log_level, DEFAULT_LOG_LEVEL);
    }

    #[test]
    fn rejects_unknown_keys() {
        let error = parse("[log]\nlevle = \"info\"\n").unwrap_err();
        assert!(matches!(error, Error::ParseConfig { .. }), "{error}");

        let error = parse("[logs]\n").unwrap_err();
        assert!(matches!(error, Error::ParseConfig { .. }), "{error}");
    }

    #[test]
    fn rejects_an_invalid_log_level() {
        let error = parse("[log]\nlevel = \"loud\"\n").unwrap_err();
        assert!(
            matches!(error, Error::InvalidLogLevel { value, .. } if value == "loud"),
            "not the expected error"
        );
    }

    #[test]
    fn accepts_filter_directives_as_log_levels() {
        assert_eq!(parse("[log]\nlevel = \"bts=debug\"\n").unwrap().log_level, "bts=debug");
        assert_eq!(parse("[log]\nlevel = \"off\"\n").unwrap().log_level, "off");
    }

    #[test]
    fn rejects_an_invalid_timeout() {
        let error = parse("[http]\nrequest_timeout = \"soon\"\n").unwrap_err();
        assert!(
            matches!(error, Error::InvalidTimeout { value, .. } if value == "soon"),
            "not the expected error"
        );
    }

    #[test]
    fn rejects_a_zero_write_concurrency() {
        let error = parse("[http]\nwrite_concurrency = 0\n").unwrap_err();
        assert!(matches!(error, Error::InvalidConcurrency { .. }), "{error}");
    }

    #[test]
    fn rejects_zero_attachment_limits() {
        for name in ["max_uploads", "max_file_bytes", "max_total_bytes"] {
            let error = parse(&format!("[attachments]\n{name} = 0\n")).unwrap_err();
            assert!(matches!(error, Error::InvalidAttachmentLimit { name: invalid, .. } if invalid == name));
        }
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert!(parse_duration("fast").is_err());
        assert!(parse_duration("0s").is_err());
    }
}
