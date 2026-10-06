use super::super::{Assembly, diff};
use super::{ManifestError, plan_manifests};
use std::path::{Path, PathBuf};
use std::{fmt, fs};

// Writes generated scorer definitions, reporting a diff immediately before
// replacing an existing file. Identical files are left untouched.
pub(crate) fn build(
    assembly: &Assembly,
    out: &Path,
    mut before_overwrite: impl FnMut(&Path, &str),
) -> Result<Vec<Built>, Error> {
    let project_files = plan_manifests(assembly, out).map_err(Error::Manifest)?;
    let scorers_dir = out.join("src/scorers");
    fs::create_dir_all(&scorers_dir).map_err(|source| Error::CreateDir {
        path: scorers_dir.clone(),
        source,
    })?;

    let mut built = Vec::with_capacity(project_files.len() + assembly.files.len());
    for file in project_files {
        let action = write_file(&file.path, &file.contents, &mut before_overwrite)?;
        built.push(Built {
            slugs: Vec::new(),
            path: file.path,
            action,
        });
    }
    for file in &assembly.files {
        let path = scorers_dir.join(&file.name);
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

pub(crate) struct Built {
    // the scorers the row covers; several when they pack into one file
    pub(crate) slugs: Vec<String>,
    pub(crate) path: PathBuf,
    pub(crate) action: Action,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Created,
    Updated,
    Unchanged,
}

#[derive(Debug)]
pub(crate) enum Error {
    Manifest(ManifestError),
    CreateDir { path: PathBuf, source: std::io::Error },
    ReadSource { path: PathBuf, source: std::io::Error },
    WriteSource { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(source) => source.fmt(formatter),
            Self::CreateDir { path, source } => {
                write!(formatter, "failed to create {}: {source}", path.display())
            }
            Self::WriteSource { path, source } => {
                write!(formatter, "failed to write {}: {source}", path.display())
            }
            Self::ReadSource { path, source } => {
                write!(formatter, "failed to read {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;
    use crate::scg::assemble;
    use uuid::Uuid;

    fn out_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("bts-build-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn writes_code_and_judge_scorers() {
        let model = compile(include_str!("../../../tests/fixtures/scorers.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        let scorers: Vec<_> = built.iter().filter(|file| !file.slugs.is_empty()).collect();
        assert_eq!(scorers.len(), 2);
        assert!(out.join("pyproject.toml").exists());
        let code = &scorers[0].path;
        assert_eq!(code.file_name().unwrap(), "answer-quality.scorer.py");
        assert!(fs::read_to_string(code).unwrap().contains("def scorer_answer_quality("));
        assert_eq!(scorers[1].slugs, ["helpfulness"]);
        assert_eq!(scorers[1].path.file_name().unwrap(), "helpfulness.scorer.py");
        assert!(fs::read_to_string(&scorers[1].path).unwrap().contains("choice_scores="));

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn packs_scorers_sharing_a_file_stem() {
        let model = compile(include_str!("../../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        let scorers: Vec<_> = built.iter().filter(|file| !file.slugs.is_empty()).collect();
        assert_eq!(scorers.len(), 1);
        assert_eq!(scorers[0].slugs, ["response-quality", "response-length"]);
        let path = &scorers[0].path;
        assert_eq!(path.file_name().unwrap(), "quality.scorer.py");
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("def scorer_response_quality(") && contents.contains("def scorer_response_length("));

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn creates_the_output_directory() {
        let model = compile(include_str!("../../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir().join("nested");

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        assert!(
            built
                .iter()
                .any(|file| file.path == out.join("src/scorers/quality.scorer.py"))
        );
        fs::remove_dir_all(out.parent().unwrap()).unwrap();
    }

    #[test]
    fn reports_diff_before_replacing_and_skips_identical_files() {
        let model = compile("scorer \"s\" { code { score = 0.5 } }").unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();
        let path = out.join("src/scorers/s.scorer.py");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "old\n").unwrap();
        let mut observed = false;
        let first = build(&assembly, &out, |candidate, diff| {
            assert_eq!(candidate, path);
            assert_eq!(fs::read_to_string(candidate).unwrap(), "old\n");
            assert!(diff.contains("- old") && diff.contains("+ import braintrust"));
            observed = true;
        })
        .unwrap();
        assert!(observed);
        assert_eq!(first.iter().find(|file| file.path == path).unwrap().action, Action::Updated);
        let second = build(&assembly, &out, |_, _| unreachable!()).unwrap();
        assert_eq!(
            second.iter().find(|file| file.path == path).unwrap().action,
            Action::Unchanged
        );
        fs::remove_dir_all(out).unwrap();
    }
}
