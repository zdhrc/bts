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

    for removal in &plan.removals {
        if !valid_relative_path(&removal.path) {
            return Err(Error::InvalidPath {
                path: removal.path.clone(),
            });
        }
        if !seen.insert(&removal.path) {
            return Err(Error::DuplicatePath {
                path: removal.path.clone(),
            });
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
    for removal in &plan.removals {
        let path = out.join(&removal.path);
        match fs::read_to_string(&path) {
            Ok(contents) if contents == removal.expected => {
                fs::remove_file(&path).map_err(|source| Error::RemoveSource { path, source })?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::ReadSource { path, source }),
        }
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
    RemoveSource { path: PathBuf, source: std::io::Error },
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
            Self::RemoveSource { path, source } => write!(formatter, "failed to remove {}: {source}", path.display()),
            Self::ReadSource { path, source } => write!(formatter, "failed to read {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDir { source, .. }
            | Self::ReadSource { source, .. }
            | Self::WriteSource { source, .. }
            | Self::RemoveSource { source, .. } => Some(source),
            _ => None,
        }
    }
}

use crate::dsl::ScorerLang;
use std::io::Write;
use std::process::{Command, Stdio};

// Pin tools and ignore ambient style settings so regeneration is deterministic.
pub fn format(source: &str, lang: ScorerLang) -> Result<String, String> {
    let (program, args): (&str, &[&str]) = match lang {
        ScorerLang::Python => (
            "uvx",
            &[
                "--from",
                "ruff==0.14.0",
                "ruff",
                "format",
                "--isolated",
                "--line-length",
                "88",
                "--target-version",
                "py310",
                "--stdin-filename",
                "agent.py",
                "-",
            ],
        ),
        ScorerLang::Typescript => (
            "npx",
            &[
                "--yes",
                "--package=prettier@3.6.2",
                "prettier",
                "--parser",
                "typescript",
                "--no-config",
                "--no-editorconfig",
                "--print-width",
                "88",
                "--end-of-line",
                "lf",
            ],
        ),
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "could not start {program}: {error}; install {} and retry",
                if program == "uvx" { "uv" } else { "Node.js/npm" }
            )
        })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    // Drain stdout/stderr while writing; large generated files can fill both pipes.
    let (written, output) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(source.as_bytes()));
        let output = child.wait_with_output();
        (writer.join().expect("formatter writer panicked"), output)
    });
    let output = output.map_err(|error| format!("could not wait for {program}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{program} exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    written.map_err(|error| format!("could not send source to {program}: {error}"))?;
    let formatted = String::from_utf8(output.stdout).map_err(|error| format!("{program} returned invalid UTF-8: {error}"))?;
    if formatted.trim().is_empty() {
        return Err(format!("{program} returned empty source"));
    }
    Ok(formatted)
}

/// Formats generated source without invoking filesystem writes.
pub fn format_plan(plan: &mut CodebasePlan) -> Result<(), String> {
    for file in &mut plan.files {
        let lang = match file.path.extension().and_then(|value| value.to_str()) {
            Some("py") => ScorerLang::Python,
            Some("ts" | "mts") => ScorerLang::Typescript,
            _ => continue,
        };
        if !file.contents.trim().is_empty() {
            file.contents =
                format(&file.contents, lang).map_err(|error| format!("cannot format {}: {error}", file.path.display()))?;
        }
    }
    Ok(())
}
