use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

// commented values double as documentation and must stay in sync with the config defaults
const CONFIG_TEMPLATE: &str = r#"# bts runtime configuration; every key is optional and the commented values are the defaults

[log]
# verbosity of the run logs written under .bt/bts/logs: off, error, warn, info, debug,
# trace, or a tracing filter directive; the BTS_LOG environment variable overrides this
#level = "info"

# how many run log files to keep before the oldest are pruned
#keep_runs = 20

[http]
# per-request timeout for Braintrust API calls, such as 30s or 2m
#request_timeout = "30s"

# how many insert requests are in flight at once; lower this on flaky
# connections or when Braintrust rate limits, must be greater than zero
#write_concurrency = 8

[attachments]
# maximum number of distinct attachment uploads in one write run
#max_uploads = 16

# maximum size of one attachment in bytes (20 MiB)
#max_file_bytes = 20971520

# maximum combined upload size in bytes (100 MiB)
#max_total_bytes = 104857600
"#;

/// Creates the default configuration if it does not already exist.
/// Returns its path and whether a new file was created.
pub fn initialize(root: &Path) -> Result<(PathBuf, bool), InitError> {
    let dir = root.join(".bt/bts");
    fs::create_dir_all(&dir).map_err(|source| InitError::CreateDirectory {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join("config.toml");

    if path.exists() {
        return Ok((path, false));
    }

    fs::write(&path, CONFIG_TEMPLATE).map_err(|source| InitError::WriteConfig {
        path: path.clone(),
        source,
    })?;

    Ok((path, true))
}

#[derive(Debug)]
pub enum InitError {
    CreateDirectory { path: PathBuf, source: std::io::Error },
    WriteConfig { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for InitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDirectory { path, source } => {
                write!(formatter, "could not create {}: {source}", path.display())
            }
            Self::WriteConfig { path, source } => {
                write!(formatter, "could not write config {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for InitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDirectory { source, .. } => Some(source),
            Self::WriteConfig { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use std::env;

    #[test]
    fn template_parses_to_the_default_settings() {
        let defaults = Settings::default();
        let settings = Settings::parse(CONFIG_TEMPLATE, Path::new("config.toml")).unwrap();

        assert_eq!(settings.log_level, defaults.log_level);
        assert_eq!(settings.keep_runs, defaults.keep_runs);
        assert_eq!(settings.request_timeout, defaults.request_timeout);
        assert_eq!(settings.write_concurrency, defaults.write_concurrency);
    }

    #[test]
    fn writes_the_config_once_and_never_overwrites() {
        let root = env::temp_dir().join(format!("bts-init-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        let (path, created) = initialize(&root).unwrap();
        assert!(created);
        fs::write(&path, "[log]\nlevel = \"debug\"\n").unwrap();

        let (again, created) = initialize(&root).unwrap();
        assert!(!created);
        assert_eq!(again, path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[log]\nlevel = \"debug\"\n");

        fs::remove_dir_all(&root).unwrap();
    }
}
