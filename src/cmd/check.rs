use crate::cmd::render_diags;
use crate::dsl::compile;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, clap::Args)]
#[command(about = "check bts source for valid syntax")]
pub struct Args {
    /// path to a source file to check, or - to read stdin
    #[arg(value_name = "PATH")]
    path: PathBuf,
}

impl Args {
    pub fn run(self) -> Result<(), Error> {
        let (source_name, src) = if self.path == Path::new("-") {
            let src = io::read_to_string(io::stdin()).map_err(Error::ReadStdin)?;
            ("<stdin>".to_owned(), src)
        } else {
            let src = fs::read_to_string(&self.path).map_err(|source| Error::Read {
                path: self.path.clone(),
                source,
            })?;
            (self.path.display().to_string(), src)
        };

        match compile(&src) {
            Ok(_) => {
                println!("{source_name}: valid");
                Ok(())
            }
            Err(diags) => Err(Error::Invalid {
                details: render_diags(&source_name, &src, &diags),
            }),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Read { path: PathBuf, source: io::Error },
    ReadStdin(io::Error),
    Invalid { details: String },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(formatter, "could not read {}: {source}", path.display()),
            Self::ReadStdin(source) => write!(formatter, "could not read stdin: {source}"),
            Self::Invalid { details } => write!(formatter, "source is invalid:\n{details}"),
        }
    }
}

impl std::error::Error for Error {}
