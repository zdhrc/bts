use super::{ManifestError, manifests, registrations};
use crate::dsl::{Scorer, ScorerLang};
use crate::mocking::code::{
    self, Assembly, CodeGroup, CodebaseContext, CodebaseFile, CodebasePlan, Judge, Module, SourceFile, Target,
};
use crate::providers::braintrust::naming::slugify;
use std::fmt;

/// Builds scorer registrations and codebase files for Braintrust.
pub struct CodeTarget {
    project: String,
}

impl CodeTarget {
    pub fn new(project: impl Into<String>) -> Self {
        Self { project: project.into() }
    }
}

impl Target for CodeTarget {
    type Error = Error;

    fn slug(&self, name: &str) -> Option<String> {
        slugify(name)
    }

    fn code(&self, group: CodeGroup<'_>, module: Module) -> Result<SourceFile, Error> {
        Ok(SourceFile {
            name: source_name(group.stem, group.lang),
            contents: registrations::code(group.lang, &self.project, group.items, &module),
            slugs: group.items.iter().map(|item| item.slug.to_owned()).collect(),
            lang: group.lang,
        })
    }

    fn judge(&self, judge: Judge<'_>) -> Result<SourceFile, Error> {
        Ok(SourceFile {
            name: source_name(judge.slug, judge.lang),
            contents: registrations::judge(
                judge.lang,
                &self.project,
                judge.name,
                judge.slug,
                judge.model,
                judge.prompt,
                judge
                    .options
                    .iter()
                    .map(|option| (option.label.as_str(), option.score))
                    .collect(),
            ),
            slugs: vec![judge.slug.to_owned()],
            lang: judge.lang,
        })
    }

    fn codebase(&self, assembly: &Assembly, context: &CodebaseContext<'_>) -> Result<CodebasePlan, Error> {
        let mut files = manifests::plan(assembly, context).map_err(Error::Manifest)?;
        files.extend(assembly.files.iter().map(|file| CodebaseFile {
            path: std::path::Path::new("src/scorers").join(&file.name),
            contents: file.contents.clone(),
            slugs: file.slugs.clone(),
        }));
        Ok(CodebasePlan { files })
    }
}

pub fn resolve_slugs(scorers: &[Scorer]) -> Result<Vec<String>, code::Error> {
    code::resolve_slugs(scorers, &CodeTarget::new(""))
}

fn source_name(stem: &str, lang: ScorerLang) -> String {
    match lang {
        ScorerLang::Python => {
            let mut normalized = String::new();
            for ch in stem.chars() {
                if ch.is_ascii_alphanumeric() {
                    normalized.push(ch.to_ascii_lowercase());
                } else if !normalized.ends_with('_') && !normalized.is_empty() {
                    normalized.push('_');
                }
            }
            let normalized = normalized.trim_end_matches('_');
            let prefix = if normalized.starts_with(|ch: char| ch.is_ascii_digit()) {
                "_"
            } else {
                ""
            };
            format!("{prefix}{normalized}_scorer.py")
        }
        ScorerLang::Typescript => format!("{stem}.scorer.ts"),
    }
}

#[derive(Debug)]
pub enum Error {
    Manifest(ManifestError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::semantics::compile;
    use crate::mocking::code::{assemble, languages::Languages};

    #[test]
    fn emits_sdk_registrations_for_code_and_judge() {
        let model = compile(include_str!("../../../../../tests/fixtures/building.bt")).unwrap();
        let built = assemble(&model.scorers, None, &Languages, &CodeTarget::new("demo-project")).unwrap();
        assert_eq!(built.files.len(), 2);
        let code = &built.files[0].contents;
        assert!(code.contains("project.scorers.create("));
        assert!(code.contains("parameters=scorer_answer_quality_params"));
        assert!(code.contains("handler=scorer_answer_quality"));
        assert!(code.contains("handler=scorer_answer_coverage"));
        assert!(code.contains("project = braintrust.projects.create(name=\"demo-project\")"));
        let judge = &built.files[1].contents;
        assert!(judge.contains("choice_scores={"));
        assert!(judge.contains("messages=[{"));
    }

    #[test]
    fn emits_typescript_sdk_registrations() {
        let model = compile(include_str!("../../../../../tests/fixtures/building.bt")).unwrap();
        let built = assemble(
            &model.scorers,
            Some(ScorerLang::Typescript),
            &Languages,
            &CodeTarget::new("demo-project"),
        )
        .unwrap();
        assert_eq!(built.files.len(), 2);
        assert!(built.files[0].contents.contains("parameters: z.object("));
        assert!(built.files[0].contents.contains("handler: scorer_answer_quality"));
        assert!(built.files[0].contents.contains("handler: scorer_answer_coverage"));
        assert!(built.files[1].contents.contains("choiceScores:"));
    }
}

#[cfg(test)]
mod writing_tests {
    use super::*;
    use crate::mocking::code::{
        Assembly, CodebaseContext,
        languages::Languages,
        writing::{Action, Built, Directory},
    };
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    fn assemble(
        scorers: &[crate::dsl::Scorer],
        lang: Option<ScorerLang>,
        project: &str,
    ) -> Result<Assembly, code::AssemblyError<code::EmitError, Error>> {
        code::assemble(scorers, lang, &Languages, &CodeTarget::new(project))
    }

    fn build(
        assembly: &Assembly,
        out: &Path,
        report: impl FnMut(&Path, &str),
    ) -> Result<Vec<Built>, Box<dyn std::error::Error>> {
        let directory = Directory::new(out);
        let plan = code::plan_codebase(
            assembly,
            &CodeTarget::new("test-project"),
            &CodebaseContext {
                root: out,
                files: &directory,
            },
        )?;
        Ok(code::writing::build(&plan, out, report)?)
    }
    use crate::dsl::semantics::compile;
    use uuid::Uuid;

    fn out_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("bts-build-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn writes_code_and_judge_scorers() {
        let model = compile(include_str!("../../../../../tests/fixtures/building.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        let scorers: Vec<_> = built.iter().filter(|file| !file.slugs.is_empty()).collect();
        assert_eq!(scorers.len(), 2);
        assert!(out.join("pyproject.toml").exists());
        let code = &scorers[0].path;
        assert_eq!(code.file_name().unwrap(), "answer_quality_scorer.py");
        assert!(fs::read_to_string(code).unwrap().contains("def scorer_answer_quality("));
        assert_eq!(scorers[0].slugs, ["answer-quality", "answer-coverage"]);
        assert!(fs::read_to_string(code).unwrap().contains("def scorer_answer_coverage("));
        assert_eq!(scorers[1].slugs, ["helpfulness"]);
        assert_eq!(scorers[1].path.file_name().unwrap(), "helpfulness_scorer.py");
        assert!(fs::read_to_string(&scorers[1].path).unwrap().contains("choice_scores="));

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn packs_scorers_sharing_a_file_stem() {
        let model = compile(include_str!("../../../../../../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        let scorers: Vec<_> = built.iter().filter(|file| !file.slugs.is_empty()).collect();
        assert_eq!(scorers.len(), 1);
        assert_eq!(scorers[0].slugs, ["response-quality", "response-length"]);
        let path = &scorers[0].path;
        assert_eq!(path.file_name().unwrap(), "quality_scorer.py");
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("def scorer_response_quality(") && contents.contains("def scorer_response_length("));

        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn creates_the_output_directory() {
        let model = compile(include_str!("../../../../../../../examples/code_scorer.bt")).unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir().join("nested");

        let built = build(&assembly, &out, |_, _| unreachable!()).unwrap();

        assert!(
            built
                .iter()
                .any(|file| file.path == out.join("src/scorers/quality_scorer.py"))
        );
        fs::remove_dir_all(out.parent().unwrap()).unwrap();
    }

    #[test]
    fn reports_diff_before_replacing_and_skips_identical_files() {
        let model = compile("scorer \"s\" { code { score = 0.5 } }").unwrap();
        let assembly = assemble(&model.scorers, None, "test-project").unwrap();
        let out = out_dir();
        let path = out.join("src/scorers/s_scorer.py");
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
