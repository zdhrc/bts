use super::{ManifestError, evaluation, manifests, registrations, tracing};
use crate::dsl::{Model, Scorer, ScorerLang};
use crate::mocking::code::{
    self, AgentTarget, Assembly, CodeGroup, CodebaseContext, CodebaseFile, CodebasePlan, CodebaseRemoval, Judge, Module,
    SourceFile, Target, planning,
};
use crate::providers::braintrust::naming::slugify;
use std::fmt;

/// Builds scorer registrations and codebase files for Braintrust.
pub struct CodeTarget {
    project: String,
    seed: u64,
}

impl CodeTarget {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            seed: 42,
        }
    }

    /// Sets the seed used to snapshot generated eval datasets.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }
}

impl Target for CodeTarget {
    type Error = Error;

    fn slug(&self, name: &str) -> Option<String> {
        slugify(name)
    }

    fn code(&self, group: CodeGroup<'_>, module: Module) -> Result<Vec<SourceFile>, Error> {
        let implementation = planning::callable_name(group.stem, group.lang);
        let registration = SourceFile {
            name: planning::source_name(group.stem, group.lang),
            contents: registrations::code(group.lang, &self.project, group.items, &module, &implementation),
            slugs: group.items.iter().map(|item| item.slug.to_owned()).collect(),
            lang: group.lang,
        };
        let mut contents = module.source;
        if group.lang == ScorerLang::Typescript {
            contents.push_str(&format!("\nexport {{ {} }};\n", module.functions.join(", ")));
        }
        Ok(vec![
            SourceFile {
                name: implementation,
                contents,
                slugs: Vec::new(),
                lang: group.lang,
            },
            registration,
        ])
    }

    fn judge(&self, judge: Judge<'_>) -> Result<SourceFile, Error> {
        Ok(SourceFile {
            name: planning::source_name(judge.slug, judge.lang),
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
        Ok(CodebasePlan {
            files,
            removals: Vec::new(),
        })
    }
}

impl CodeTarget {
    pub fn agent_assembly(&self, model: &Model, lang: ScorerLang, mut assembly: Assembly) -> Result<Assembly, Error> {
        assembly
            .files
            .extend(evaluation::assemble(model, lang, &self.project, self.seed)?);
        Ok(assembly)
    }
}

impl AgentTarget for CodeTarget {
    fn tracing(&self, lang: ScorerLang) -> &str {
        tracing::runtime(lang)
    }

    fn agent_codebase(
        &self,
        sources: &[SourceFile],
        lang: ScorerLang,
        context: &CodebaseContext<'_>,
    ) -> Result<CodebasePlan, Error> {
        let modules = sources.iter().map(|source| source.name.clone()).collect::<Vec<_>>();
        let tokens = sources
            .iter()
            .any(|source| source.contents.contains("def _tokens(") || source.contents.contains("function _tokens("));
        let mut files = manifests::plan_agents(&modules, lang, context, tokens).map_err(Error::Manifest)?;
        files.extend(sources.iter().map(|source| CodebaseFile {
            path: source.name.clone().into(),
            contents: source.contents.clone(),
            slugs: source.slugs.clone(),
        }));
        let mut removals = Vec::new();
        let mut candidates = std::collections::BTreeSet::new();
        for source in sources {
            if let Some(name) = source
                .name
                .strip_prefix("src/agents/")
                .or_else(|| source.name.strip_prefix("src/evals/"))
            {
                candidates.insert(std::path::PathBuf::from(name));
                if source.name.starts_with("src/agents/")
                    && let Some((stem, extension)) = name.rsplit_once('.')
                {
                    candidates.insert(format!("{stem}_agent.{extension}").into());
                }
            }
        }
        for path in candidates {
            if let Some(contents) = context.files.read(&path).map_err(|source| {
                Error::Manifest(ManifestError::Read {
                    path: context.root.join(&path),
                    source,
                })
            })? {
                let lowercase = contents.to_ascii_lowercase();
                if lowercase.starts_with("# generated by bts;") || lowercase.starts_with("// generated by bts;") {
                    removals.push(CodebaseRemoval {
                        path,
                        expected: contents,
                    });
                }
            }
        }
        Ok(CodebasePlan { files, removals })
    }
}

pub fn resolve_slugs(scorers: &[Scorer]) -> Result<Vec<String>, code::Error> {
    code::resolve_slugs(scorers, &CodeTarget::new(""))
}

#[derive(Debug)]
pub enum Error {
    Manifest(ManifestError),
    Agent(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(source) => source.fmt(formatter),
            Self::Agent(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest(source) => Some(source),
            Self::Agent(_) => None,
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
        assert_eq!(built.files.len(), 3);
        let code = &built.files[1].contents;
        assert!(code.contains("project.scorers.create("));
        assert!(code.contains("parameters=scorer_answer_quality_params"));
        assert!(code.contains("handler=scorer_answer_quality"));
        assert!(code.contains("handler=scorer_answer_coverage"));
        assert!(code.contains("project = braintrust.projects.create(name=\"demo-project\")"));
        let judge = &built.files[2].contents;
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
        assert_eq!(built.files.len(), 3);
        assert!(built.files[1].contents.contains("parameters: z.object("));
        assert!(built.files[1].contents.contains("handler: scorer_answer_quality"));
        assert!(built.files[1].contents.contains("handler: scorer_answer_coverage"));
        assert!(built.files[2].contents.contains("choiceScores:"));
    }

    #[test]
    fn callable_names_preserve_registrations_without_keywords_or_file_collisions() {
        let model = compile(
            r#"scorer "class" { code { score = 1 } }
               scorer "foo" { code { score = 1 } }
               scorer "foo-scorer" { code { score = 1 } }
               scorer "123 Quality" { code { score = 1 } }"#,
        )
        .unwrap();
        let target = CodeTarget::new("test-project");
        for (lang, expected) in [
            (
                ScorerLang::Python,
                [
                    ("class", "class_impl.py", "class_scorer.py"),
                    ("foo", "foo_impl.py", "foo_scorer.py"),
                    ("foo-scorer", "foo_scorer_impl.py", "foo_scorer_scorer.py"),
                    ("123-quality", "_123_quality_impl.py", "_123_quality_scorer.py"),
                ],
            ),
            (
                ScorerLang::Typescript,
                [
                    ("class", "class.impl.ts", "class.scorer.ts"),
                    ("foo", "foo.impl.ts", "foo.scorer.ts"),
                    ("foo-scorer", "foo-scorer.impl.ts", "foo-scorer.scorer.ts"),
                    ("123-quality", "123-quality.impl.ts", "123-quality.scorer.ts"),
                ],
            ),
        ] {
            let assembly = assemble(&model.scorers, Some(lang), &Languages, &target).unwrap();
            assert_eq!(assembly.files.len(), expected.len() * 2);
            for (slug, implementation, registration) in expected {
                let callable = assembly.files.iter().find(|file| file.name == implementation).unwrap();
                assert!(callable.slugs.is_empty());
                let registration = assembly.files.iter().find(|file| file.name == registration).unwrap();
                assert_eq!(registration.slugs, [slug]);
                match lang {
                    ScorerLang::Python => {
                        let module = implementation.strip_suffix(".py").unwrap();
                        assert!(registration.contents.contains(&format!("from .{module} import ")));
                        assert!(registration.contents.contains(&format!("from {module} import ")));
                    }
                    ScorerLang::Typescript => {
                        let module = implementation.strip_suffix(".ts").unwrap();
                        assert!(registration.contents.contains(&format!("from \"./{module}.js\"")));
                    }
                }
            }
        }
    }
}

#[cfg(all(test, feature = "native"))]
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
        assert!(
            fs::read_to_string(out.join("src/scorers/answer_quality_impl.py"))
                .unwrap()
                .contains("def scorer_answer_quality(")
        );
        assert_eq!(scorers[0].slugs, ["answer-quality", "answer-coverage"]);
        assert!(
            fs::read_to_string(out.join("src/scorers/answer_quality_impl.py"))
                .unwrap()
                .contains("def scorer_answer_coverage(")
        );
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
        let contents = fs::read_to_string(out.join("src/scorers/quality_impl.py")).unwrap();
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

#[cfg(test)]
mod agent_tests {
    use super::*;
    use crate::dsl::semantics::compile;
    use crate::mocking::code::ExistingFiles;
    use std::{collections::BTreeMap, path::Path};

    struct MemoryFiles(BTreeMap<String, String>);
    impl ExistingFiles for MemoryFiles {
        fn read(&self, path: &Path) -> std::io::Result<Option<String>> {
            Ok(self.0.get(path.to_str().unwrap()).cloned())
        }
    }

    #[test]
    fn plans_agents_from_virtual_files_and_only_removes_generated_legacy_files() {
        let model = compile(r#"agent "hello" { entrypoint = trace.t } trace "t" { output = "hello" }"#).unwrap();
        let target = CodeTarget::new("");
        let assembly = code::assemble_agents(&model, ScorerLang::Python, &target).unwrap();
        let legacy = "# Generated by bts; edits will be overwritten.\n";
        let files = MemoryFiles(BTreeMap::from([
            ("hello_agent.py".into(), legacy.into()),
            ("hello_agent_agent.py".into(), "# user code\n".into()),
            (
                "pyproject.toml".into(),
                r#"[project]
name = "kept"
dependencies = ["braintrust==0.40.0", "requests>=2"]
"#
                .into(),
            ),
        ]));
        let context = CodebaseContext {
            root: Path::new("/virtual/agent"),
            files: &files,
        };
        let plan = target.agent_codebase(&assembly.files, ScorerLang::Python, &context).unwrap();
        assert_eq!(plan.removals.len(), 1);
        assert_eq!(plan.removals[0].path, Path::new("hello_agent.py"));
        assert_eq!(plan.removals[0].expected, legacy);
        assert!(plan.files.iter().all(|file| file.path.is_relative()));
        let manifest = &plan
            .files
            .iter()
            .find(|file| file.path == Path::new("pyproject.toml"))
            .unwrap()
            .contents;
        let parsed: toml::Value = toml::from_str(manifest).unwrap();
        assert_eq!(parsed["project"]["name"].as_str(), Some("kept"));
        assert!(
            parsed["project"]["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_str() == Some("braintrust==0.40.0"))
        );
        assert_eq!(
            parsed["tool"]["setuptools"]["package-dir"]["agents"].as_str(),
            Some("src/agents")
        );
        assert!(!manifest.contains("tiktoken"));
    }

    #[test]
    fn agent_evals_plan_their_dependencies_and_preserve_scorer_codebase_settings() {
        let model = compile(include_str!("../../../../../../../examples/customer_support_agent.bt")).unwrap();
        let target = CodeTarget::new("test-project");
        let scorer_assembly = code::assemble(
            &model.scorers,
            Some(ScorerLang::Typescript),
            &code::languages::Languages,
            &target,
        )
        .unwrap();
        let empty = MemoryFiles(BTreeMap::new());
        let context = CodebaseContext {
            root: Path::new("/virtual/agent"),
            files: &empty,
        };
        let scorer_plan = target.codebase(&scorer_assembly, &context).unwrap();
        let existing = MemoryFiles(
            scorer_plan
                .files
                .into_iter()
                .map(|file| (file.path.to_str().unwrap().to_owned(), file.contents))
                .collect(),
        );
        let context = CodebaseContext {
            root: Path::new("/virtual/agent"),
            files: &existing,
        };
        let assembly = code::assemble_agents(&model, ScorerLang::Typescript, &target).unwrap();
        let assembly = target.agent_assembly(&model, ScorerLang::Typescript, assembly).unwrap();
        let plan = target
            .agent_codebase(&assembly.files, ScorerLang::Typescript, &context)
            .unwrap();
        let package: serde_json::Value = serde_json::from_str(
            &plan
                .files
                .iter()
                .find(|file| file.path == Path::new("package.json"))
                .unwrap()
                .contents,
        )
        .unwrap();
        assert_eq!(package["dependencies"]["zod"], "^4.0.0");
        assert_eq!(package["devDependencies"]["typescript"], "^5.0.0");
        assert_eq!(package["devDependencies"]["@types/node"], "^24.2.0");
        let config: serde_json::Value = serde_json::from_str(
            &plan
                .files
                .iter()
                .find(|file| file.path == Path::new("tsconfig.json"))
                .unwrap()
                .contents,
        )
        .unwrap();
        assert_eq!(config["compilerOptions"]["rewriteRelativeImportExtensions"], true);
        assert!(
            plan.files
                .iter()
                .any(|file| file.path == Path::new("src/evals/customer_support.eval.ts"))
        );
    }
}
