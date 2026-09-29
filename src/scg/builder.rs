use super::Assembly;
use std::path::{Path, PathBuf};
use std::{fmt, fs};

// writes assembled scorer sources to disk, the local counterpart of the
// pusher's upsert; judges have no source and come back without a path
pub(super) fn build(assembly: &Assembly, out: &Path) -> Result<Vec<Built>, Error> {
    fs::create_dir_all(out).map_err(|source| Error::CreateDir {
        path: out.to_owned(),
        source,
    })?;

    let mut built = Vec::with_capacity(assembly.files.len() + assembly.judges.len());
    for file in &assembly.files {
        let path = out.join(&file.name);
        fs::write(&path, &file.contents).map_err(|source| Error::WriteSource {
            path: path.clone(),
            source,
        })?;
        built.push(Built {
            slugs: file.slugs.clone(),
            path: Some(path),
        });
    }
    for judge in &assembly.judges {
        built.push(Built {
            slugs: vec![judge.clone()],
            path: None,
        });
    }

    Ok(built)
}

pub(crate) struct Built {
    // the scorers the row covers; several when they packed into one file
    pub(crate) slugs: Vec<String>,
    // none for judges, which push as prompt functions and have no source
    pub(crate) path: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum Error {
    CreateDir { path: PathBuf, source: std::io::Error },
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
    fn writes_code_scorers_and_skips_judges() {
        let model = compile(include_str!("../../tests/fixtures/scorers.bt")).unwrap();
        let assembly = assemble(&model.scorers, None).unwrap();
        let out = out_dir();

        let built = build(&assembly, &out).unwrap();

        assert_eq!(built.len(), 2);
        let code = built[0].path.as_ref().unwrap();
        assert_eq!(code.file_name().unwrap(), "answer-quality.scorer.py");
        assert!(fs::read_to_string(code).unwrap().contains("def scorer_answer_quality("));
        assert_eq!(built[1].slugs, ["helpfulness"]);
        assert!(built[1].path.is_none());

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn packs_scorers_sharing_a_file_stem() {
        let model = compile(include_str!("../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None).unwrap();
        let out = out_dir();

        let built = build(&assembly, &out).unwrap();

        assert_eq!(built.len(), 1);
        assert_eq!(built[0].slugs, ["response-quality", "response-length"]);
        let path = built[0].path.as_ref().unwrap();
        assert_eq!(path.file_name().unwrap(), "quality.scorer.py");
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("def scorer_response_quality(") && contents.contains("def scorer_response_length("));

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn creates_the_output_directory() {
        let model = compile(include_str!("../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None).unwrap();
        let out = out_dir().join("nested");

        let built = build(&assembly, &out).unwrap();

        assert!(built[0].path.as_ref().unwrap().exists());
        fs::remove_dir_all(out.parent().unwrap()).unwrap();
    }
}
