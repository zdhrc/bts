use super::{Assembly, diff};
use std::path::{Path, PathBuf};
use std::{fmt, fs};

// Writes generated scorer definitions, reporting a diff immediately before
// replacing an existing file. Identical files are left untouched.
pub(super) fn build(
    assembly: &Assembly,
    out: &Path,
    mut before_overwrite: impl FnMut(&Path, &str),
) -> Result<Vec<Built>, Error> {
    fs::create_dir_all(out).map_err(|source| Error::CreateDir {
        path: out.to_owned(),
        source,
    })?;

    let mut built = Vec::with_capacity(assembly.files.len());
    for file in &assembly.files {
        let path = out.join(&file.name);
        let action = match fs::read_to_string(&path) {
            Ok(previous) => match diff::changed(&previous, &file.contents) {
                Some(changes) => {
                    before_overwrite(&path, &changes);
                    fs::write(&path, &file.contents).map_err(|source| Error::WriteSource {
                        path: path.clone(),
                        source,
                    })?;
                    Action::Updated
                }
                None => Action::Unchanged,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::write(&path, &file.contents).map_err(|source| Error::WriteSource {
                    path: path.clone(),
                    source,
                })?;
                Action::Created
            }
            Err(source) => return Err(Error::ReadSource { path, source }),
        };
        built.push(Built {
            slugs: file.slugs.clone(),
            path,
            action,
        });
    }

    Ok(built)
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
    CreateDir { path: PathBuf, source: std::io::Error },
    ReadSource { path: PathBuf, source: std::io::Error },
    WriteSource { path: PathBuf, source: std::io::Error },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
        let model = compile(include_str!("../../tests/fixtures/scorers.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        assert_eq!(built.len(), 2);
        let code = &built[0].path;
        assert_eq!(code.file_name().unwrap(), "answer-quality.scorer.py");
        assert!(fs::read_to_string(code).unwrap().contains("def scorer_answer_quality("));
        assert_eq!(built[1].slugs, ["helpfulness"]);
        assert_eq!(built[1].path.file_name().unwrap(), "helpfulness.scorer.py");
        assert!(fs::read_to_string(&built[1].path).unwrap().contains("choice_scores="));

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn packs_scorers_sharing_a_file_stem() {
        let model = compile(include_str!("../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        assert_eq!(built.len(), 1);
        assert_eq!(built[0].slugs, ["response-quality", "response-length"]);
        let path = &built[0].path;
        assert_eq!(path.file_name().unwrap(), "quality.scorer.py");
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("def scorer_response_quality(") && contents.contains("def scorer_response_length("));

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn creates_the_output_directory() {
        let model = compile(include_str!("../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir().join("nested");

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        assert!(built[0].path.exists());
        fs::remove_dir_all(out.parent().unwrap()).unwrap();
    }

    #[test]
    fn reports_diff_before_replacing_and_skips_identical_files() {
        let model = compile("scorer \"s\" { code { score = 0.5 } }").unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();
        let path = out.join("s.scorer.py");
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
        assert_eq!(first[0].action, Action::Updated);
        let second = build(&assembly, &out, |_, _| unreachable!()).unwrap();
        assert_eq!(second[0].action, Action::Unchanged);
        fs::remove_dir_all(out).unwrap();
    }
}
