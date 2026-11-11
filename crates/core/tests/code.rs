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
    assert!(wrapper.contains("from .packed_impl import exported_0, exported_1"));
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
            .find(|file| file.path == Path::new(&format!("generated/src/agents/support_agent_runtime.{extension}")))
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

#[test]
fn language_functions_execute_in_generated_agents() {
    let model = compile(
        r#"
agent "language" { entrypoint = trace.language }
trace "language" {
    input = { template = "{{{name}}}: {n} {ok}", count = 2, parts = [null, "", "Hi!", "there"] }
    vars { selected = sample([0, 1, 2, 3, 4], self.input.count) }
    output = {
        text = render(self.input.template, { name = "Zoë", n = 6.0, ok = true })
        composed = compose(self.input.parts)
        selected = var.selected
        reused = var.selected
        all = sample(["same", "same"], 2)
        empty = sample([], 0)
        nested = compose(["Details:", compose(sample(["one", "two"], 2), ", ")])
    }
}
"#,
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!("bts-language-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    for (lang, program, script) in [
        (
            ScorerLang::Python,
            "python3",
            r#"
from language_agent import run
result = run(random_source=lambda: 0.25)
assert result['text'] == '{Zoë}: 6.0 true', result
assert result['composed'] == 'Hi! there'
assert result['selected'] == result['reused'] == [0, 2]
assert result['all'] == ['same', 'same'] and result['empty'] == []
assert result['nested'] == 'Details: one, two'
assert run(random_source=lambda: 0.99)['selected'] == [3, 4]
base = {'template':'{name}', 'count':2, 'parts':['hi']}
for patch in [{'template':'{missing}'}, {'template':'{'}, {'parts':[1]}, {'count':-1}, {'count':6}, {'count':1.0}]:
    try: run({**base, **patch})
    except (ValueError, TypeError): pass
    else: raise AssertionError(patch)
"#,
        ),
        (
            ScorerLang::Typescript,
            "node",
            r#"
import assert from 'node:assert/strict';
import {run} from './language_agent.mts';
const result = run(undefined, () => 0.25);
assert.equal(result.text, '{Zoë}: 6.0 true');
assert.equal(result.composed, 'Hi! there');
assert.deepEqual(result.selected, [0, 2]);
assert.deepEqual(result.reused, result.selected);
assert.deepEqual(result.all, ['same', 'same']);
assert.deepEqual(result.empty, []);
assert.equal(result.nested, 'Details: one, two');
assert.deepEqual(run(undefined, () => 0.99).selected, [3, 4]);
const base = {template:'{name}', count:2, parts:['hi']};
for (const patch of [{template:'{missing}'}, {template:'{'}, {parts:[1]}, {count:-1}, {count:6}, {count:1.5}]) {
    assert.throws(() => run({...base, ...patch}));
}
"#,
        ),
    ] {
        let assembly = code::assemble_agents(&model, lang, &PlainTarget).unwrap();
        for file in assembly.files {
            let path = root.join(&file.name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file.contents).unwrap();
        }
        let mut command = std::process::Command::new(program);
        if lang == ScorerLang::Python {
            command.arg("-c");
        } else {
            command.args(["--input-type=module", "-e"]);
        }
        let output = command.arg(script).current_dir(root.join("src/agents")).output().unwrap();
        assert!(
            output.status.success(),
            "{lang:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn english_toolkit_executes_in_packaged_python_and_typescript_agents() {
    let model = compile(include_str!("fixtures/english.bt")).unwrap();
    let root = std::env::temp_dir().join(format!("bts-english-agents-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    for (lang, program, script) in [
        (
            ScorerLang::Python,
            "python3",
            r#"
from language_agent import run
for draw in [0.0, 0.5, 0.99]:
    result = run(random_source=lambda: draw)
    assert result['action'] == 'confirm_check_in', result
    assert result['statement'] == 'The guest will arrive after midnight.', result
    assert result['combined'] == 'The guest will arrive after midnight, and the desk will be open.', result
    assert result['first_reference'] == result['reset_reference'] == 'Maya', result
    assert result['next_reference'] == 'she', result
    assert result['details'] == [{'id':'desk','text':'The desk is open.'}], result
    assert result['amount'] == '12,34,567.90', result
    assert result['date'] == '10 November 2026', result
    assert result['time'] == '12:05:09 AM', result
    assert result['structured']['selection']['message']['layout'] == 'current_desired', result
    assert result['slot_answer']['text'] == 'Friday', result
    assert result['family'] == result['procedural']['selection']['family'], result
    assert result['history_family'] == result['family'], result
    assert result['first_candidate_family'] == 'request', result
    assert result['candidate_count'] == 18, result
    assert result['procedural']['selection']['intent'] == 'change_flight', result
    assert result['followup']['text'] in ['What about Friday?', 'what about Friday?'], result
    assert result['plain_expression'] == 'What about Friday?', result
    assert len(result['followup']['context']['recent']) == 2, result
    assert result['styled'] == "I'll check the file.", result
    assert result['fragment'] == 'Checking the file.', result
    assert result['softened'] == 'Could you send the file?', result
    assert result['shorthand'] == 'Btw, the file is ready.', result
    assert result['typo'] == 'plese', result
    assert result['wording'] == result['reused'], result
    assert result['wording'] in ['The guest cannot check in.', "The guest can't check in."], result
    assert result['conditional'] in ['If the guest arrives late, can they check in?', 'Can they check in if the guest arrives late?'], result
    assert result['register'] in ["I won't leave. They cannot stay.", 'I will not leave. They cannot stay.', "I won't leave. They can't stay."], result
    assert result['identifier'] in ['api_v2 does not update résumé_ID-42.', "api_v2 doesn't update résumé_ID-42."], result
    assert run({'intent':'unknown'}, random_source=lambda: draw)['action'] == 'ask_arrival_time'
assert run(random_source=lambda: 0.99)['conditional'] == 'Can they check in if the guest arrives late?'
assert run(random_source=lambda: 0.99)['register'] == "I won't leave. They can't stay."
assert run(random_source=lambda: 0.0) == run(random_source=lambda: 0.0)
"#,
        ),
        (
            ScorerLang::Typescript,
            "node",
            r#"
import assert from 'node:assert/strict';
import {run} from './language_agent.mts';
for (const draw of [0.0, 0.5, 0.99]) {
    const result = run(undefined, () => draw);
    assert.equal(result.action, 'confirm_check_in');
    assert.equal(result.statement, 'The guest will arrive after midnight.');
    assert.equal(result.combined, 'The guest will arrive after midnight, and the desk will be open.');
    assert.equal(result.first_reference, 'Maya');
    assert.equal(result.next_reference, 'she');
    assert.equal(result.reset_reference, 'Maya');
    assert.deepEqual(result.details, [{id:'desk',text:'The desk is open.'}]);
    assert.equal(result.amount, '12,34,567.90');
    assert.equal(result.date, '10 November 2026');
    assert.equal(result.time, '12:05:09 AM');
    assert.equal(result.structured.selection.message.layout,'current_desired');
    assert.equal(result.slot_answer.text,'Friday');
    assert.equal(result.family,result.procedural.selection.family);
    assert.equal(result.history_family,result.family);
    assert.equal(result.first_candidate_family,'request');
    assert.equal(result.candidate_count,18);
    assert.equal(result.procedural.selection.intent,'change_flight');
    assert.ok(['What about Friday?', 'what about Friday?'].includes(result.followup.text));
    assert.equal(result.plain_expression,'What about Friday?');
    assert.equal(result.followup.context.recent.length,2);
    assert.equal(result.styled, "I'll check the file.");
    assert.equal(result.fragment, 'Checking the file.');
    assert.equal(result.softened, 'Could you send the file?');
    assert.equal(result.shorthand, 'Btw, the file is ready.');
    assert.equal(result.typo, 'plese');
    assert.equal(result.wording, result.reused);
    assert.ok(['The guest cannot check in.', "The guest can't check in."].includes(result.wording));
    assert.ok(['If the guest arrives late, can they check in?', 'Can they check in if the guest arrives late?'].includes(result.conditional));
    assert.ok(["I won't leave. They cannot stay.", 'I will not leave. They cannot stay.', "I won't leave. They can't stay."].includes(result.register));
    assert.ok(['api_v2 does not update résumé_ID-42.', "api_v2 doesn't update résumé_ID-42."].includes(result.identifier));
    assert.equal(run({intent:'unknown'}, () => draw).action, 'ask_arrival_time');
}
assert.equal(run(undefined, () => 0.99).conditional, 'Can they check in if the guest arrives late?');
assert.equal(run(undefined, () => 0.99).register, "I won't leave. They can't stay.");
assert.deepEqual(run(undefined, () => 0.0), run(undefined, () => 0.0));
"#,
        ),
    ] {
        let assembly = code::assemble_agents(&model, lang, &PlainTarget).unwrap();
        for file in assembly.files {
            let path = root.join(&file.name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file.contents).unwrap();
        }
        let output = std::process::Command::new(program)
            .args(if lang == ScorerLang::Python {
                vec!["-c"]
            } else {
                vec!["--input-type=module", "-e"]
            })
            .arg(script)
            .current_dir(root.join("src/agents"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{lang:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn language_numeric_contract_matches_native_agents_and_scorers() {
    use bts_core::mocking::data::{self, Distribution};
    use bts_lang::ScorerKind;
    use std::time::{Duration, UNIX_EPOCH};

    let cases = [
        ("6", "6"),
        ("6.0", "6.0"),
        ("-0.0", "-0.0"),
        ("0.0000001", "1e-7"),
        ("0.00001", "0.00001"),
        ("0.000001", "1e-6"),
        ("1000000000000000.0", "1000000000000000.0"),
        ("10000000000000000.0", "1e+16"),
        ("123000000000000000000.0", "1.23e+20"),
        ("self.input.n + 1.0", "3.0"),
        ("self.input.n / 2", "1"),
        ("self.input.n / 2.0", "1.0"),
        ("choice(6.0, 6.0)", "6.0"),
        ("weighted([6.0, 1], [6.0, 2])", "6.0"),
        ("range(6.0, 6.0)", "6.0"),
        ("-self.input.n / 3", "0"),
        ("-self.input.n % 2", "0"),
        ("min(self.input.n, 6.0)", "2.0"),
        ("max(self.input.n, 1.0)", "2.0"),
        ("clamp(self.input.n, 0, 6.0)", "2.0"),
        ("abs(-self.input.n * 1.0)", "2.0"),
        ("[6.0][0]", "6.0"),
        ("self.input.n == 2 ? 6.0 : 7", "6.0"),
        ("true", "true"),
    ];
    let render = |expression: &str| format!("render(\"{{n}}\", {{ n = {expression} }})");
    let expected: Vec<_> = cases.iter().map(|(_, expected)| *expected).collect();
    let generate = |source: &str| {
        data::generate(
            compile(source).unwrap(),
            1,
            Duration::from_secs(1),
            Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(1000),
            42,
            &(),
        )
    };
    let source = format!(
        "agent \"numeric\" {{ entrypoint = trace.numeric }} trace \"numeric\" {{ input = {{ n = 2 }} output = [{}] }}",
        cases
            .iter()
            .map(|(expression, _)| render(expression))
            .collect::<Vec<_>>()
            .join(",")
    );
    let batch = generate(&source).unwrap();
    assert_eq!(
        batch.events[0].output.as_ref().unwrap().project(&()).unwrap(),
        serde_json::json!(expected)
    );
    let model = compile(&source).unwrap();
    let invalid = [
        "1.0",
        "choice(1.0, 1.0)",
        "weighted([1.0, 1], [1.0, 2])",
        "range(1.0, 1.0)",
        "self.input.n / 2.0",
        "min(self.input.n, 1.0)",
        "[1.0][0]",
    ];
    for expression in invalid {
        assert!(
            generate(&format!(
                "trace \"t\" {{ input = {{ n = 2 }} output = sample([1, 2], {expression}) }}"
            ))
            .is_err(),
            "{expression}"
        );
    }
    let mut scorers = cases
        .iter()
        .enumerate()
        .map(|(index, (expression, expected))| {
            format!(
                "scorer \"render{index}\" {{ code {{ score = {} == {} ? 1 : 0 }} }}",
                render(&expression.replace("self.input", "input")),
                serde_json::to_string(expected).unwrap()
            )
        })
        .collect::<Vec<_>>();
    for (index, expression) in invalid.iter().enumerate() {
        scorers.push(format!(
            "scorer \"sample{index}\" {{ code {{ score = len(sample([1, 2], {})) }} }}",
            expression.replace("self.input", "input")
        ));
    }
    scorers.push("scorer \"integer\" { code { score = len(sample([1, 2], input.n / 2)) } }".into());
    let scorer_model = compile(&scorers.join("\n")).unwrap();
    let items = scorer_model
        .scorers
        .iter()
        .map(|scorer| {
            let ScorerKind::Code { score, steps } = &scorer.kind else {
                panic!("code scorer")
            };
            Item {
                name: &scorer.name,
                slug: &scorer.name,
                score,
                steps,
            }
        })
        .collect::<Vec<_>>();
    let root = std::env::temp_dir().join(format!("bts-numeric-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    for (lang, program, extension) in [(ScorerLang::Python, "python3", "py"), (ScorerLang::Typescript, "node", "mts")] {
        for file in code::assemble_agents(&model, lang, &PlainTarget).unwrap().files {
            let path = root.join(file.name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file.contents).unwrap();
        }
        let expected = serde_json::to_string(&expected).unwrap();
        let script = if lang == ScorerLang::Python {
            format!(
                r#"
import json
from numeric_agent import run
assert run() == json.loads({expected:?})
for index in range({count}):
    assert globals()['scorer_render' + str(index)](None, {{'n':2}})['score'] == 1, index
for index in range({invalid_count}):
    try: globals()['scorer_sample' + str(index)](None, {{'n':2}})
    except (TypeError, ValueError): pass
    else: raise AssertionError(index)
assert scorer_integer(None, {{'n':2}})['score'] == 1
"#,
                count = cases.len(),
                invalid_count = invalid.len()
            )
        } else {
            let render_names = (0..cases.len())
                .map(|i| format!("scorer_render{i}"))
                .collect::<Vec<_>>()
                .join(",");
            let sample_names = (0..invalid.len())
                .map(|i| format!("scorer_sample{i}"))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                r#"
import assert from 'node:assert/strict';
import {{run}} from './numeric_agent.mts';
assert.deepEqual(run(), {expected});
for (const scorer of [{render_names}]) assert.equal(scorer({{output:null,input:{{n:2}}}}).score, 1);
for (const scorer of [{sample_names}]) assert.throws(() => scorer({{output:null,input:{{n:2}}}}));
assert.equal(scorer_integer({{output:null,input:{{n:2}}}}).score, 1);
"#
            )
        };
        let emitted = code::languages::Languages.emit(lang, &items).unwrap().source;
        let path = root.join(format!("src/agents/check.{extension}"));
        fs::write(&path, format!("{emitted}\n{script}")).unwrap();
        let output = std::process::Command::new(program).arg(&path).output().unwrap();
        assert!(
            output.status.success(),
            "{lang:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
