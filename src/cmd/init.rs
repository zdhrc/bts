use std::path::{Path, PathBuf};
use std::{env, fmt, fs};

// commented values double as documentation and must stay in sync with the conf defaults
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
"#;

#[derive(Debug, clap::Args)]
#[command(about = "initialize .bt/bts in the current directory with a default config")]
pub struct Args {}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let root = env::current_dir().map_err(Error::CurrentDirectory)?;
        let (path, created) = write_config(&root)?;

        if created {
            println!("initialized {}", path.display());
        } else {
            println!("config already exists at {}", path.display());
        }

        Ok(())
    }
}

fn write_config(root: &Path) -> Result<(PathBuf, bool), Error> {
    let dir = root.join(".bt/bts");
    fs::create_dir_all(&dir).map_err(|source| Error::CreateDirectory {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join("config.toml");

    if path.exists() {
        return Ok((path, false));
    }

    fs::write(&path, CONFIG_TEMPLATE).map_err(|source| Error::WriteConfig {
        path: path.clone(),
        source,
    })?;

    Ok((path, true))
}

#[derive(Debug)]
pub enum Error {
    CurrentDirectory(std::io::Error),
    CreateDirectory { path: PathBuf, source: std::io::Error },
    WriteConfig { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(source) => {
                write!(formatter, "could not determine the current directory: {source}")
            }
            Self::CreateDirectory { path, source } => {
                write!(formatter, "could not create {}: {source}", path.display())
            }
            Self::WriteConfig { path, source } => {
                write!(formatter, "could not write config {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conf::Settings;

    #[test]
    fn template_parses_to_the_default_settings() {
        let defaults = Settings::default();
        let settings = Settings::parse(CONFIG_TEMPLATE, Path::new("config.toml")).unwrap();

        assert_eq!(settings.log_level, defaults.log_level);
        assert_eq!(settings.keep_runs, defaults.keep_runs);
        assert_eq!(settings.request_timeout, defaults.request_timeout);
    }

    #[test]
    fn writes_the_config_once_and_never_overwrites() {
        let root = env::temp_dir().join(format!("bts-init-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();

        let (path, created) = write_config(&root).unwrap();
        assert!(created);
        fs::write(&path, "[log]\nlevel = \"debug\"\n").unwrap();

        let (again, created) = write_config(&root).unwrap();
        assert!(!created);
        assert_eq!(again, path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "[log]\nlevel = \"debug\"\n");

        fs::remove_dir_all(&root).unwrap();
    }
}
