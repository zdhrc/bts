use std::{env, fmt};

#[derive(Debug, clap::Args)]
#[command(about = "initialize .bt/bts in the current directory with a default config")]
pub struct Args {}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let root = env::current_dir().map_err(Error::CurrentDirectory)?;
        let (path, created) = crate::config::initialize(&root).map_err(Error::Initialize)?;
        if created {
            println!("initialized {}", path.display());
        } else {
            println!("config already exists at {}", path.display());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum Error {
    CurrentDirectory(std::io::Error),
    Initialize(crate::config::InitError),
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentDirectory(source) => write!(formatter, "could not determine the current directory: {source}"),
            Self::Initialize(source) => source.fmt(formatter),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CurrentDirectory(source) => Some(source),
            Self::Initialize(source) => Some(source),
        }
    }
}
