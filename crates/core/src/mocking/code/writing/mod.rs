mod diff;

use super::{CodebasePlan, ExistingFiles};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::{fmt, fs, io};

/// Reads existing codebase files from an application-selected directory.
pub struct Directory<'a> {
    root: &'a Path,
}

impl<'a> Directory<'a> {
    pub fn new(root: &'a Path) -> Self {
        Self { root }
    }
}

impl ExistingFiles for Directory<'_> {
    fn read(&self, path: &Path) -> io::Result<Option<String>> {
        check_path(path)?;
        match fs::read_to_string(self.root.join(path)) {
            Ok(contents) => Ok(Some(contents)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn exists(&self, path: &Path) -> io::Result<bool> {
        check_path(path)?;
        self.root.join(path).try_exists()
    }
}

fn check_path(path: &Path) -> io::Result<()> {
    if valid_relative_path(path) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "codebase paths must be relative to the output directory",
        ))
    }
}

fn valid_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|part| matches!(part, Component::Normal(_)))
}

/// Writes a codebase plan, reporting a diff before replacing an existing file.
///
/// All destination paths are checked before writes begin. Once writing starts,
/// an I/O failure can leave earlier files updated. The callback observes each
/// changed file before replacement and is not an approval mechanism.
pub fn build(plan: &CodebasePlan, out: &Path, mut before_overwrite: impl FnMut(&Path, &str)) -> Result<Vec<Built>, Error> {
    let mut seen = HashSet::new();
    for file in &plan.files {
        if !valid_relative_path(&file.path) {
            return Err(Error::InvalidPath { path: file.path.clone() });
        }
        if !seen.insert(&file.path) {
            return Err(Error::DuplicatePath { path: file.path.clone() });
        }
    }

    let mut built = Vec::with_capacity(plan.files.len());
    for file in &plan.files {
        let path = out.join(&file.path);
        let parent = path.parent().expect("a codebase file has an output directory");
        fs::create_dir_all(parent).map_err(|source| Error::CreateDir {
            path: parent.to_owned(),
            source,
        })?;
        let action = write_file(&path, &file.contents, &mut before_overwrite)?;
        built.push(Built {
            slugs: file.slugs.clone(),
            path,
            action,
        });
    }
    Ok(built)
}

fn write_file(path: &Path, contents: &str, before_overwrite: &mut impl FnMut(&Path, &str)) -> Result<Action, Error> {
    match fs::read_to_string(path) {
        Ok(previous) => match diff::changed(&previous, contents) {
            Some(changes) => {
                before_overwrite(path, &changes);
                fs::write(path, contents).map_err(|source| Error::WriteSource {
                    path: path.to_owned(),
                    source,
                })?;
                Ok(Action::Updated)
            }
            None => Ok(Action::Unchanged),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::write(path, contents).map_err(|source| Error::WriteSource {
                path: path.to_owned(),
                source,
            })?;
            Ok(Action::Created)
        }
        Err(source) => Err(Error::ReadSource {
            path: path.to_owned(),
            source,
        }),
    }
}

pub struct Built {
    // the scorers the row covers, including those packed into one file
    pub slugs: Vec<String>,
    pub path: PathBuf,
    pub action: Action,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Created,
    Updated,
    Unchanged,
}

#[derive(Debug)]
pub enum Error {
    InvalidPath { path: PathBuf },
    DuplicatePath { path: PathBuf },
    CreateDir { path: PathBuf, source: std::io::Error },
    ReadSource { path: PathBuf, source: std::io::Error },
    WriteSource { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path } => write!(
                formatter,
                "codebase file {} must be relative to the output directory",
                path.display()
            ),
            Self::DuplicatePath { path } => write!(formatter, "multiple codebase files would write to {}", path.display()),
            Self::CreateDir { path, source } => write!(formatter, "failed to create {}: {source}", path.display()),
            Self::WriteSource { path, source } => write!(formatter, "failed to write {}: {source}", path.display()),
            Self::ReadSource { path, source } => write!(formatter, "failed to read {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir { source, .. } | Self::ReadSource { source, .. } | Self::WriteSource { source, .. } => Some(source),
            _ => None,
        }
    }
}
