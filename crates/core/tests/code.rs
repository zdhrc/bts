use bts_core::mocking::code::{
    self, Assembly, CodeGroup, CodebaseContext, CodebaseFile, CodebasePlan, Emitter, ExistingFiles, Item, Judge, Module,
    SourceFile, Target,
};
use bts_lang::{Number, ScorerLang, Value, semantics::compile};
use std::{collections::BTreeMap, error::Error as _, fmt, fs, path::Path};

#[derive(Debug, PartialEq)]
enum TargetError {
    UnsupportedExpression,
    UnsupportedJudge,
    Read(std::io::ErrorKind),
}

impl fmt::Display for TargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}
impl std::error::Error for TargetError {}

struct ConstantEmitter;
impl Emitter for ConstantEmitter {
    type Error = TargetError;

    fn emit(&self, _: ScorerLang, items: &[Item<'_>]) -> Result<Module, Self::Error> {
        let mut source = String::new();
        let mut functions = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let value = match item.score {
                Value::Num(Number::Int(value)) => value.to_string(),
                Value::Num(Number::Float(value)) => value.to_string(),
                _ => return Err(TargetError::UnsupportedExpression),
            };
            let function = format!("exported_{index}");
            source.push_str(&format!("def {function}(output):\n    return {value}\n"));
            functions.push(function);
        }
        Ok(Module { source, functions })
    }
}

struct FixedExports<'a>(&'a [&'a str]);
impl Emitter for FixedExports<'_> {
    type Error = TargetError;

    fn emit(&self, _: ScorerLang, _: &[Item<'_>]) -> Result<Module, Self::Error> {
        Ok(Module {
            source: String::new(),
            functions: self.0.iter().map(|function| (*function).to_owned()).collect(),
        })
    }
}

struct UncalledTarget;
impl Target for UncalledTarget {
    type Error = TargetError;

    fn slug(&self, name: &str) -> Option<String> {
        PlainTarget.slug(name)
    }

    fn code(&self, _: CodeGroup<'_>, _: Module) -> Result<Vec<SourceFile>, Self::Error> {
        panic!("invalid exports must fail before reaching the target")
    }

    fn judge(&self, _: Judge<'_>) -> Result<SourceFile, Self::Error> {
        panic!("this test has no judges")
    }

    fn codebase(&self, _: &Assembly, _: &CodebaseContext<'_>) -> Result<CodebasePlan, Self::Error> {
        panic!("this test does not plan a codebase")
    }
}

struct PlainTarget;
impl Target for PlainTarget {
    type Error = TargetError;

    fn slug(&self, name: &str) -> Option<String> {
        Some(name.to_ascii_lowercase().replace(' ', "_"))
    }

    fn code(&self, group: CodeGroup<'_>, module: Module) -> Result<Vec<SourceFile>, Self::Error> {
        Ok(vec![SourceFile {
            name: format!("{}.py", group.stem),
            contents: format!("{}\nexports = [{}]\n", module.source, module.functions.join(", ")),
            slugs: group.items.iter().map(|item| item.slug.to_owned()).collect(),
            lang: group.lang,
        }])
    }

    fn judge(&self, _: Judge<'_>) -> Result<SourceFile, Self::Error> {
        Err(TargetError::UnsupportedJudge)
    }

    fn codebase(&self, assembly: &Assembly, context: &CodebaseContext<'_>) -> Result<CodebasePlan, Self::Error> {
        let mut files: Vec<_> = assembly
            .files
            .iter()
            .map(|source| CodebaseFile {
                path: Path::new("generated").join(&source.name),
                contents: source.contents.clone(),
                slugs: source.slugs.clone(),
            })
            .collect();
        if let Some(contents) = context
            .files
            .read(Path::new("settings.json"))
            .map_err(|error| TargetError::Read(error.kind()))?
        {
            files.push(CodebaseFile {
                path: "settings.json".into(),
                contents,
                slugs: Vec::new(),
            });
        }
        Ok(CodebasePlan {
            files,
            removals: Vec::new(),
        })
    }
}

struct MemoryFiles(BTreeMap<String, String>);
impl ExistingFiles for MemoryFiles {
    fn read(&self, path: &Path) -> std::io::Result<Option<String>> {
        Ok(self.0.get(path.to_str().unwrap()).cloned())
    }
}

#[test]
fn external_emitter_and_target_build_without_a_provider_dependency() {
    let model = compile(r#"scorer "Answer Quality" { code { score = 0.5 } }"#).unwrap();
    let assembly = code::assemble(&model.scorers, None, &ConstantEmitter, &PlainTarget).unwrap();
    assert_eq!(assembly.files[0].slugs, ["answer_quality"]);
    let out = std::env::temp_dir().join(format!("bts-code-target-{}", uuid::Uuid::new_v4()));
    let existing = MemoryFiles(BTreeMap::from([("settings.json".to_owned(), "{\"keep\":true}".to_owned())]));
    let plan = code::plan_codebase(
        &assembly,
        &PlainTarget,
        &CodebaseContext {
            root: &out,
            files: &existing,
        },
    )
    .unwrap();
    assert!(!out.exists());
    assert_eq!(plan.files[0].path, Path::new("generated/answer_quality.py"));
    assert!(plan.files[0].contents.starts_with("def exported_0(output):\n"));
    assert!(plan.files[0].contents.ends_with("exports = [exported_0]\n"));

    code::writing::build(&plan, &out, |_, _| {}).unwrap();
    assert!(
        fs::read_to_string(out.join("generated/answer_quality.py"))
            .unwrap()
            .contains("return 0.5")
    );
    assert_eq!(fs::read_to_string(out.join("settings.json")).unwrap(), "{\"keep\":true}");
    fs::remove_dir_all(out).unwrap();
}

#[test]
fn external_target_errors_keep_their_concrete_cause() {
    let model = compile(include_str!("fixtures/building.bt")).unwrap();
    let error = code::assemble(&model.scorers, None, &code::languages::Languages, &PlainTarget)
        .err()
        .unwrap();
    assert!(matches!(error, code::AssemblyError::Target(TargetError::UnsupportedJudge)));
    assert_eq!(
        error.source().unwrap().downcast_ref::<TargetError>(),
        Some(&TargetError::UnsupportedJudge)
    );
}

#[test]
fn braintrust_target_accepts_an_external_emitters_source_and_exports() {
    let model = compile(
        r#"scorer "Quality" { file = "packed" code { score = 0.5 } }
           scorer "Coverage" { file = "packed" code { score = 0.9 } }"#,
    )
    .unwrap();
    let target = bts_core::providers::braintrust::mocking::code::CodeTarget::new("test-project");
    let assembly = code::assemble(&model.scorers, None, &ConstantEmitter, &target).unwrap();
    assert_eq!(assembly.files.len(), 2);
    assert_eq!(assembly.files[1].slugs, ["quality", "coverage"]);
    let source = &assembly.files[0].contents;
    assert!(source.contains("def exported_0(output):\n    return 0.5\n"));
    assert!(source.contains("def exported_1(output):\n    return 0.9\n"));
    let wrapper = &assembly.files[1].contents;
    assert!(wrapper.contains("from .packed import exported_0, exported_1"));
    assert!(!wrapper.contains("def exported_"));
    let registrations: Vec<_> = wrapper.split("project.scorers.create(").skip(1).collect();
    assert_eq!(registrations.len(), 2);
    assert!(registrations[0].contains("name=\"Quality\","));
    assert!(registrations[0].contains("handler=exported_0,"));
    assert!(registrations[1].contains("name=\"Coverage\","));
    assert!(registrations[1].contains("handler=exported_1,"));
}

#[test]
fn built_in_emitters_validate_slugs_for_direct_callers() {
    let score = Value::Num(Number::Float(0.5));
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        for slug in ["", "foo_bar", "foo bar", "Upper", "café"] {
            let items = [Item {
                name: "quality",
                slug,
                score: &score,
                steps: &[],
            }];
            let error = code::languages::Languages.emit(lang, &items).err().unwrap();
            assert!(matches!(error, code::EmitError::InvalidSlug { slug: rejected } if rejected == slug));
        }
        let items: Vec<_> = ["same", "same"]
            .into_iter()
            .map(|slug| Item {
                name: "quality",
                slug,
                score: &score,
                steps: &[],
            })
            .collect();
        let error = code::languages::Languages.emit(lang, &items).err().unwrap();
        assert!(matches!(error, code::EmitError::DuplicateSlug { slug } if slug == "same"));
    }
}

#[test]
fn built_in_emitters_preserve_existing_function_names() {
    let score = Value::Num(Number::Float(0.5));
    let items: Vec<_> = ["response-quality", "a-1", "a1", "2fast-2furious"]
        .into_iter()
        .map(|slug| Item {
            name: slug,
            slug,
            score: &score,
            steps: &[],
        })
        .collect();
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let module = code::languages::Languages.emit(lang, &items).unwrap();
        assert_eq!(
            module.functions,
            ["scorer_response_quality", "scorer_a_1", "scorer_a1", "scorer_2fast_2furious"]
        );
        let declaration = match lang {
            ScorerLang::Python => "def",
            ScorerLang::Typescript => "function",
        };
        for function in &module.functions {
            assert!(module.source.contains(&format!("{declaration} {function}(")));
        }
    }
}

#[test]
fn built_in_emitters_reject_custom_slugs_that_would_collide() {
    let model = compile(
        r#"scorer "foo-bar" { file = "packed" code { score = 0.1 } }
           scorer "foo_bar" { file = "packed" code { score = 0.9 } }"#,
    )
    .unwrap();
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let error = code::assemble(&model.scorers, Some(lang), &code::languages::Languages, &UncalledTarget)
            .err()
            .unwrap();
        assert!(matches!(
            error,
            code::AssemblyError::Emit(code::EmitError::InvalidSlug { slug }) if slug == "foo_bar"
        ));
    }
}

#[test]
fn assembly_rejects_invalid_exports_before_invoking_the_target() {
    let model = compile(
        r#"scorer "first" { file = "packed" code { score = 0.1 } }
           scorer "second" { file = "packed" code { score = 0.9 } }"#,
    )
    .unwrap();
    let assemble = |functions: &[&str]| {
        let error = code::assemble(&model.scorers, None, &FixedExports(functions), &UncalledTarget)
            .err()
            .unwrap();
        let code::AssemblyError::Plan(error) = error else {
            panic!("expected an assembly validation error")
        };
        error
    };
    assert!(matches!(assemble(&[""]), code::Error::ExportCount { expected: 2, actual: 1 }));
    assert!(matches!(assemble(&["", "second"]), code::Error::EmptyExport { index: 0 }));
    assert!(matches!(assemble(&["first", " \t\n"]), code::Error::EmptyExport { index: 1 }));
    assert!(matches!(
        assemble(&["same", "same"]),
        code::Error::DuplicateExport { function } if function == "same"
    ));
}

#[test]
fn codebase_paths_are_checked_before_any_file_is_written() {
    let out = std::env::temp_dir().join(format!("bts-code-target-{}", uuid::Uuid::new_v4()));
    let plan = CodebasePlan {
        removals: Vec::new(),
        files: vec![
            CodebaseFile {
                path: "valid.py".into(),
                contents: "pass\n".into(),
                slugs: Vec::new(),
            },
            CodebaseFile {
                path: "../outside.py".into(),
                contents: "pass\n".into(),
                slugs: Vec::new(),
            },
        ],
    };
    assert!(matches!(
        code::writing::build(&plan, &out, |_, _| {}),
        Err(code::writing::Error::InvalidPath { .. })
    ));
    assert!(!out.exists());
}

impl code::AgentTarget for PlainTarget {
    fn tracing(&self, lang: ScorerLang) -> &str {
        match lang {
            ScorerLang::Python => {
                "def start_trace(events, name, kind):\n    events.append({'name': name, 'kind': kind})\n    return events\ndef log_trace(events, fields):\n    events.append(fields)\ndef end_trace(events):\n    pass\n"
            }
            ScorerLang::Typescript => {
                "function startTrace(events: unknown[], name: string, kind: string): unknown[] { events.push({name, kind}); return events; }\nfunction logTrace(events: unknown[], fields: Record<string, unknown>): void { events.push(fields); }\nfunction endTrace(events: unknown[]): void {}\n"
            }
        }
    }

    fn agent_codebase(
        &self,
        sources: &[SourceFile],
        _: ScorerLang,
        context: &CodebaseContext<'_>,
    ) -> Result<CodebasePlan, Self::Error> {
        let assembly = Assembly {
            files: sources
                .iter()
                .map(|source| SourceFile {
                    name: source.name.clone(),
                    contents: source.contents.clone(),
                    slugs: source.slugs.clone(),
                    lang: source.lang,
                })
                .collect(),
        };
        self.codebase(&assembly, context)
    }
}

#[test]
fn alternate_agent_target_assembles_and_plans_without_sdk_or_filesystem_access() {
    use code::AgentTarget;

    let model = compile(r#"agent "support" { entrypoint = trace.request } trace "request" { output = "hello" }"#).unwrap();
    let out = std::env::temp_dir().join(format!("bts-agent-target-{}", uuid::Uuid::new_v4()));
    let existing = MemoryFiles(BTreeMap::from([("settings.json".to_owned(), "{\"keep\":true}".to_owned())]));
    let context = CodebaseContext {
        root: &out,
        files: &existing,
    };
    for (lang, extension, tracing) in [
        (ScorerLang::Python, "py", "events.append"),
        (ScorerLang::Typescript, "mts", "events.push"),
    ] {
        let assembly = code::assemble_agents(&model, lang, &PlainTarget).unwrap();
        let plan = PlainTarget.agent_codebase(&assembly.files, lang, &context).unwrap();
        assert!(!out.exists());
        assert!(plan.removals.is_empty());
        let source = plan
            .files
            .iter()
            .find(|file| file.path == Path::new(&format!("generated/src/agents/support_agent.{extension}")))
            .unwrap();
        assert!(source.contents.contains("hello"));
        let runtime = plan
            .files
            .iter()
            .find(|file| file.path == Path::new(&format!("generated/src/agents/_runtime.{extension}")))
            .unwrap();
        assert!(runtime.contents.contains(tracing));
        assert!(plan.files.iter().all(|file| !file.contents.contains("braintrust")));
        assert_eq!(
            plan.files
                .iter()
                .find(|file| file.path == Path::new("settings.json"))
                .unwrap()
                .contents,
            "{\"keep\":true}"
        );
    }
}
