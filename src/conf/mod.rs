use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::{env, fmt, fs, time::Duration};
use uuid::Uuid;

const BRAINTRUST_API_URL: &str = "https://api.braintrust.dev";
const CONFIG_PATH: &str = ".bt/bts/config.toml";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_KEPT_RUNS: usize = 20;

#[derive(Clone)]
pub(crate) struct Braintrust {
    pub(crate) api_url: String,
    pub(crate) api_key: String,
    pub(crate) project_id: Uuid,
    pub(crate) request_timeout: Duration,
}

impl Braintrust {
    pub(crate) fn new(api_key: String, project_id: Uuid) -> Self {
        Self {
            api_url: BRAINTRUST_API_URL.to_owned(),
            api_key,
            project_id,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    pub(crate) fn from_env() -> Result<Self, Error> {
        let api_key = required_env("BRAINTRUST_API_KEY")?;
        let project_id = required_env("BRAINTRUST_PROJECT_ID")?;
        let project_id = Uuid::parse_str(&project_id).map_err(|source| Error::InvalidProjectId { source })?;
        let mut config = Self::new(api_key, project_id);

        if let Some(api_url) = env::var_os("BRAINTRUST_API_URL").filter(|value| !value.is_empty()) {
            config.api_url = api_url.to_string_lossy().into_owned();
        }

        Ok(config)
    }
}

// runtime behavior from .bt/bts/config.toml; an absent file means the defaults
#[derive(Debug)]
pub(crate) struct Settings {
    pub(crate) log_level: String,
    pub(crate) keep_runs: usize,
    pub(crate) request_timeout: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            log_level: DEFAULT_LOG_LEVEL.to_owned(),
            keep_runs: DEFAULT_KEPT_RUNS,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

impl Settings {
    pub(crate) fn load() -> Result<Self, Error> {
        let root = project_root().map_err(Error::CurrentDirectory)?;
        let path = root.join(CONFIG_PATH);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(Error::ReadConfig { path, source }),
        };

        Self::parse(&raw, &path)
    }

    pub(crate) fn parse(raw: &str, path: &Path) -> Result<Self, Error> {
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

        Ok(settings)
    }
}

// unknown keys are rejected so config typos fail loudly instead of silently doing nothing
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct FileSettings {
    log: LogSection,
    http: HttpSection,
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
}

// bare values must be real levels so typos fail; the env-filter parser alone would accept
// any word as a target directive. strings with '=' are genuine directives and parse as such.
pub(crate) fn validate_log_level(value: &str) -> Result<(), String> {
    if value.contains('=') {
        return tracing_subscriber::filter::EnvFilter::try_new(value)
            .map(|_| ())
            .map_err(|error| error.to_string());
    }

    value
        .parse::<tracing_subscriber::filter::LevelFilter>()
        .map(|_| ())
        .map_err(|_| "expected off, error, warn, info, debug, or trace".to_owned())
}

// the project root is the nearest ancestor that already has a .bt directory, or the
// current directory when none has been set up yet
pub(crate) fn project_root() -> std::io::Result<PathBuf> {
    let current_dir = env::current_dir()?;
    let root = current_dir
        .ancestors()
        .find(|dir| dir.join(".bt").is_dir())
        .unwrap_or(&current_dir);

    Ok(root.to_path_buf())
}

pub(crate) fn parse_duration(value: &str) -> Result<Duration, String> {
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

fn required_env(name: &'static str) -> Result<String, Error> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or(Error::MissingVariable(name))
}

#[derive(Debug)]
pub(crate) enum Error {
    MissingVariable(&'static str),
    InvalidProjectId { source: uuid::Error },
    CurrentDirectory(std::io::Error),
    ReadConfig { path: PathBuf, source: std::io::Error },
    ParseConfig { path: PathBuf, source: toml::de::Error },
    InvalidLogLevel { path: PathBuf, value: String, reason: String },
    InvalidTimeout { path: PathBuf, value: String, reason: String },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable(name) => write!(formatter, "environment variable {name} is required"),
            Self::InvalidProjectId { source } => write!(formatter, "BRAINTRUST_PROJECT_ID must be a UUID: {source}"),
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
        }
    }
}

impl std::error::Error for Error {}

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
            "#,
        )
        .unwrap();

        assert_eq!(settings.log_level, "debug");
        assert_eq!(settings.keep_runs, 5);
        assert_eq!(settings.request_timeout, Duration::from_secs(120));
    }

    #[test]
    fn missing_keys_fall_back_to_defaults() {
        for raw in ["", "[log]\n", "[log]\nlevel = \"warn\"\n"] {
            let settings = parse(raw).unwrap();
            assert_eq!(settings.keep_runs, DEFAULT_KEPT_RUNS);
            assert_eq!(settings.request_timeout, DEFAULT_REQUEST_TIMEOUT);
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
    fn parses_durations() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert!(parse_duration("fast").is_err());
        assert!(parse_duration("0s").is_err());
    }
}
