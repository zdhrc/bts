use super::{CodeTarget, Error, registrations};
use crate::dsl::{Model, Scorer, ScorerKind, ScorerLang};
use crate::mocking::code::{
    self, Assembly, CodeGroup, CodebaseContext, CodebasePlan, Judge, Module, SourceFile, Target, languages::Languages,
    lowering, planning,
};
use crate::providers::braintrust::naming::slugify;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

pub(super) fn assemble(model: &Model, lang: ScorerLang, project: &str, seed: u64) -> Result<Vec<SourceFile>, Error> {
    let plans = planning::evals(model).map_err(Error::Agent)?;
    if model.experiments.is_empty() {
        return Ok(Vec::new());
    }
    let target = EvaluationTarget {
        target: CodeTarget::new(project),
        exports: RefCell::default(),
    };
    let names = plans
        .iter()
        .flat_map(|plan| plan.scorers.iter().map(|scorer| &scorer.name))
        .collect::<BTreeSet<_>>();
    let group = |scorer: &crate::dsl::Scorer| {
        matches!(scorer.kind, ScorerKind::Code { .. })
            .then(|| scorer.file.clone().or_else(|| target.slug(&scorer.name)))
            .flatten()
    };
    let groups = model
        .scorers
        .iter()
        .filter(|scorer| names.contains(&scorer.name))
        .filter_map(group)
        .collect::<BTreeSet<_>>();
    let scorers = model
        .scorers
        .iter()
        .filter(|scorer| names.contains(&scorer.name) || group(scorer).is_some_and(|group| groups.contains(&group)))
        .cloned()
        .collect::<Vec<_>>();
    let selected_names = scorers.iter().map(|scorer| &scorer.name).collect::<BTreeSet<_>>();
    for selected in &scorers {
        let files = scorer_files(selected, lang);
        for other in model.scorers.iter().filter(|scorer| !selected_names.contains(&scorer.name)) {
            if let Some(file) = scorer_files(other, lang).into_iter().find(|file| files.contains(file)) {
                return Err(Error::Agent(format!(
                    "scorers {:?} and {:?} both generate {file:?}",
                    selected.name, other.name
                )));
            }
        }
    }
    let scorers = code::assemble(&scorers, Some(lang), &Languages, &target).map_err(|error| Error::Agent(error.to_string()))?;
    let exports = target.exports.into_inner();
    let mut files = Vec::new();
    for plan in &plans {
        let rows = lowering::cases(model, plan.dataset, seed, lang).map_err(Error::Agent)?;
        files.extend(evaluation(plan, &rows, lang, project, &exports));
    }
    files.push(runner(&plans, lang, project));
    files.extend(scorers.files.into_iter().map(|mut file| {
        file.name = format!("src/scorers/{}", file.name);
        file
    }));
    Ok(files)
}

fn scorer_files(scorer: &Scorer, lang: ScorerLang) -> Vec<String> {
    match &scorer.kind {
        ScorerKind::Code { .. } => scorer
            .file
            .clone()
            .or_else(|| slugify(&scorer.name))
            .map_or_else(Vec::new, |stem| {
                vec![planning::callable_name(&stem, lang), planning::source_name(&stem, lang)]
            }),
        ScorerKind::Judge { .. } => slugify(&scorer.name)
            .and_then(|slug| judge_file(&slug, lang).ok())
            .into_iter()
            .collect(),
    }
}

fn judge_file(slug: &str, lang: ScorerLang) -> Result<String, String> {
    Ok(match lang {
        ScorerLang::Python => format!("{}_judge.py", planning::module_stem(slug)?),
        ScorerLang::Typescript => format!("{slug}.judge.ts"),
    })
}

struct Callable {
    file: String,
    function: String,
}

struct EvaluationTarget {
    target: CodeTarget,
    exports: RefCell<BTreeMap<String, Callable>>,
}
impl Target for EvaluationTarget {
    type Error = Error;
    fn slug(&self, name: &str) -> Option<String> {
        slugify(name)
    }
    fn code(&self, group: CodeGroup<'_>, module: Module) -> Result<Vec<SourceFile>, Error> {
        let file = planning::callable_name(group.stem, group.lang);
        for (item, function) in group.items.iter().zip(&module.functions) {
            self.exports.borrow_mut().insert(
                item.name.to_owned(),
                Callable {
                    file: file.clone(),
                    function: function.clone(),
                },
            );
        }
        self.target.code(group, module)
    }

    fn judge(&self, judge: Judge<'_>) -> Result<SourceFile, Error> {
        let stem = planning::module_stem(judge.slug).map_err(Error::Agent)?;
        let function = format!("judge_{stem}");
        let file = judge_file(judge.slug, judge.lang).map_err(Error::Agent)?;
        self.exports.borrow_mut().insert(
            judge.name.to_owned(),
            Callable {
                file: file.clone(),
                function: function.clone(),
            },
        );
        Ok(SourceFile {
            name: file,
            contents: registrations::local_judge(&judge, &function),
            lang: judge.lang,
            slugs: Vec::new(),
        })
    }
    fn codebase(&self, assembly: &Assembly, context: &CodebaseContext<'_>) -> Result<CodebasePlan, Error> {
        self.target.codebase(assembly, context)
    }
}

fn evaluation(
    plan: &planning::Evaluation<'_>,
    rows: &[serde_json::Value],
    lang: ScorerLang,
    project: &str,
    exports: &BTreeMap<String, Callable>,
) -> Vec<SourceFile> {
    let experiment = plan.experiment;
    let stem = &plan.agent;
    let eval_stem = &plan.module;
    let mut functions = Vec::new();
    let mut imports = String::new();
    for scorer in &plan.scorers {
        let callable = exports.get(&scorer.name).expect("selected scorers were assembled");
        let function = &callable.function;
        let file = &callable.file;
        imports.push_str(&match lang {
            ScorerLang::Python => format!("from scorers.{} import {function}\n", file.trim_end_matches(".py")),
            ScorerLang::Typescript => format!("import {{{function}}} from \"../scorers/{}\";\n", file),
        });
        functions.push(function.as_str());
    }
    let data = serde_json::to_string_pretty(&rows).unwrap();
    let project = serde_json::to_string(project).unwrap();
    let experiment_name = serde_json::to_string(&experiment.name).unwrap();
    let description = serde_json::to_string(&experiment.description).unwrap();
    let baseline = serde_json::to_string(&experiment.baseline).unwrap();
    let scores = functions.join(", ");
    let ts_description = experiment
        .description
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap())
        .unwrap_or_else(|| "undefined".into());
    let ts_baseline = experiment
        .baseline
        .as_ref()
        .map(|value| serde_json::to_string(value).unwrap())
        .unwrap_or_else(|| "undefined".into());
    let (entrypoint, definition, contents, registration) = match lang {
        ScorerLang::Python => (
            format!("src/evals/{eval_stem}_eval.py"),
            format!("src/evals/{eval_stem}_definition.py"),
            format!(
                "# generated by bts; edits will be overwritten.\nimport json\nfrom agents.{stem} import run_traced\n{imports}\n\nCASES = json.loads({data:?})\nSCORERS = [{scores}]\n\ndef task(input, hooks):\n    return run_traced(hooks.span, input)\n\ndef evaluate(project={project}, *, no_send_logs=False, base_experiment_id=None):\n    from braintrust import Eval\n    return Eval(project, experiment_name={experiment_name}, description=json.loads({description:?}), data=CASES, task=task, scores=SCORERS, no_send_logs=no_send_logs, base_experiment_name=None if no_send_logs or base_experiment_id else json.loads({baseline:?}), base_experiment_id=None if no_send_logs else base_experiment_id)\n"
            ),
            "\n# the SDK defers execution while the CLI discovers evals\nevaluate()\n",
        ),
        ScorerLang::Typescript => (
            format!("src/evals/{eval_stem}.eval.ts"),
            format!("src/evals/{eval_stem}.definition.ts"),
            format!(
                "// generated by bts; edits will be overwritten.\nimport {{Eval}} from \"braintrust\";\nimport {{runTraced}} from \"../agents/{stem}.mts\";\n{imports}\n\nexport const cases = {data};\nexport const scorers = [{scores}];\nexport function task(input: unknown, hooks: {{span: Parameters<typeof runTraced>[0]}}): any {{\n    return runTraced(hooks.span, input);\n}}\nexport function evaluate(project={project}, noSendLogs=false, baseExperimentId?: string) {{\n    return Eval(project, {{experimentName: {experiment_name}, description: {ts_description}, data: cases, task, scores: scorers, baseExperimentName: noSendLogs || baseExperimentId ? undefined : {ts_baseline}, baseExperimentId: noSendLogs ? undefined : baseExperimentId}} , {{noSendLogs}});\n}}\n"
            ),
            "\n// the SDK defers execution while the CLI discovers evals\nevaluate();\n",
        ),
    };
    let entry_contents = match lang {
        ScorerLang::Python => format!(
            "# generated by bts; edits will be overwritten.\nfrom evals.{eval_stem}_definition import CASES, SCORERS, task, evaluate\n{registration}"
        ),
        ScorerLang::Typescript => format!(
            "// generated by bts; edits will be overwritten.\nexport {{cases, scorers, task, evaluate}} from \"./{eval_stem}.definition.ts\";\nimport {{evaluate}} from \"./{eval_stem}.definition.ts\";\n{registration}"
        ),
    };
    vec![
        SourceFile {
            name: definition,
            contents,
            lang,
            slugs: Vec::new(),
        },
        SourceFile {
            name: entrypoint,
            contents: entry_contents,
            lang,
            slugs: Vec::new(),
        },
    ]
}

fn runner(plans: &[planning::Evaluation<'_>], lang: ScorerLang, project: &str) -> SourceFile {
    let project = serde_json::to_string(project).unwrap();
    let mut imports = String::new();
    let mut steps = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        let name = serde_json::to_string(&plan.experiment.name).unwrap();
        let baseline = plan
            .experiment
            .baseline
            .as_ref()
            .map(|name| serde_json::to_string(name).unwrap());
        match lang {
            ScorerLang::Python => {
                imports.push_str(&format!(
                    "from evals.{}_definition import evaluate as evaluate_{index}\n",
                    plan.module
                ));
                steps.push(format!(
                    "({name}, {}, evaluate_{index})",
                    baseline.unwrap_or_else(|| "None".into())
                ));
            }
            ScorerLang::Typescript => {
                imports.push_str(&format!(
                    "import {{evaluate as evaluate{index}}} from \"./{}.definition.ts\";\n",
                    plan.module
                ));
                steps.push(format!(
                    "{{name: {name}, baseline: {}, evaluate: evaluate{index}}}",
                    baseline.unwrap_or_else(|| "undefined".into())
                ));
            }
        }
    }
    let steps = steps.join(",\n    ");
    let (name, contents) = match lang {
        ScorerLang::Python => (
            "src/evals/run_evals.py",
            format!(
                r#"# generated by bts; edits will be overwritten.
import argparse
import sys
{imports}

EVALUATIONS = [
    {steps}
]

def run(project={project}, *, no_send_logs=False):
    if no_send_logs:
        print("Local run: experiment comparisons are unavailable without uploads.", file=sys.stderr)
    results = {{}}
    for name, baseline, evaluate in EVALUATIONS:
        baseline_id = None
        if baseline is not None and not no_send_logs:
            baseline_id = results[baseline].summary.experiment_id
            if not baseline_id:
                raise RuntimeError(f"baseline {{baseline!r}} did not return an experiment ID")
        results[name] = evaluate(project, no_send_logs=no_send_logs, base_experiment_id=baseline_id)
    return results

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Run generated evaluations in baseline order.")
    parser.add_argument("--project", default={project})
    parser.add_argument("--no-send-logs", action="store_true")
    args = parser.parse_args()
    run(args.project, no_send_logs=args.no_send_logs)
"#
            ),
        ),
        ScorerLang::Typescript => (
            "src/evals/run-evals.mts",
            format!(
                r#"// generated by bts; edits will be overwritten.
import {{parseArgs}} from "node:util";
import type {{EvalResultWithSummary}} from "braintrust";
{imports}

const evaluations = [
    {steps}
];

export async function run(project={project}, noSendLogs=false) {{
    if (noSendLogs) console.error("Local run: experiment comparisons are unavailable without uploads.");
    const results = new Map<string, EvalResultWithSummary<any, any, any>>();
    for (const {{name, baseline, evaluate}} of evaluations) {{
        let baselineId: string | undefined;
        if (baseline !== undefined && !noSendLogs) {{
            baselineId = results.get(baseline)?.summary.experimentId;
            if (!baselineId) throw new Error("baseline " + baseline + " did not return an experiment ID");
        }}
        results.set(name, await evaluate(project, noSendLogs, baselineId));
    }}
    return results;
}}

if (import.meta.main) {{
    const {{values}} = parseArgs({{options: {{project: {{type: "string"}}, "no-send-logs": {{type: "boolean", default: false}}}}}});
    await run(values.project, values["no-send-logs"]);
}}
"#
            ),
        ),
    };
    SourceFile {
        name: name.into(),
        contents,
        lang,
        slugs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{Number, Value};
    use code::Item;

    #[test]
    fn selected_scorers_reject_output_collisions_with_unselected_definitions() {
        let model = crate::dsl::semantics::compile(
            r#"
trace "task" { input = "hello" output = self.input }
agent "task" { entrypoint = trace.task }
dataset "cases" { case "one" { input = "hello" } }
scorer "first" { file = "a-b" code { score = 1 } }
scorer "second" { file = "a_b" code { score = 0.5 } }
scorer "judge-a" { judge { model = "gpt-4o-mini" prompt = "Check ${output}" options = { yes = 1, no = 0 } } }
scorer "judge_a" { judge { model = "gpt-4o-mini" prompt = "Check ${output}" options = { yes = 1, no = 0 } } }
scorer "safe" { code { score = 1 } }
experiment "code" { task = trace.task dataset = dataset.cases scorers = [scorer.first] }
experiment "judge" { task = trace.task dataset = dataset.cases scorers = [scorer["judge-a"]] }
experiment "safe" { task = trace.task dataset = dataset.cases scorers = [scorer.safe] }
"#,
        )
        .unwrap();
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let select = |name| crate::selection::agents(&model, &[format!("experiment.{name}").parse().unwrap()]).unwrap();
            let error = assemble(&select("judge"), lang, "test", 42).err().unwrap();
            assert!(error.to_string().contains("both generate"));
            assert!(error.to_string().contains("judge-a"));
            assert!(error.to_string().contains("judge_a"));
            assert!(assemble(&select("safe"), lang, "test", 42).is_ok());
            let code = assemble(&select("code"), lang, "test", 42);
            if lang == ScorerLang::Python {
                assert!(code.err().unwrap().to_string().contains("a_b_impl.py"));
            } else {
                assert!(code.is_ok());
            }
        }
    }

    #[test]
    fn selected_evaluations_preserve_complete_packed_scorer_modules() {
        let model = crate::dsl::semantics::compile(
            r#"
trace "task" { input = "hello" output = self.input }
agent "task" { entrypoint = trace.task }
dataset "cases" { case "one" { input = "hello" } }
scorer "quality" { code { score = 1 } }
scorer "coverage" { file = "quality" code { score = 0.5 } }
scorer "length" { file = "quality" code { score = 0.75 } }
scorer "unrelated" { code { score = 0 } }
experiment "first" { task = trace.task dataset = dataset.cases scorers = [scorer.quality] }
experiment "second" { task = trace.task dataset = dataset.cases scorers = [scorer.coverage] }
"#,
        )
        .unwrap();
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let callable = format!("src/scorers/{}", planning::callable_name("quality", lang));
            let mut existing = BTreeMap::new();
            let mut previous = None;
            for name in ["first", "second"] {
                let selected = crate::selection::agents(&model, &[format!("experiment.{name}").parse().unwrap()]).unwrap();
                let built = assemble(&selected, lang, "test", 42).unwrap();
                assert!(built.iter().all(|file| !file.name.contains("unrelated")));
                let source = &built.iter().find(|file| file.name == callable).unwrap().contents;
                for function in ["scorer_quality", "scorer_coverage", "scorer_length"] {
                    assert!(source.contains(function), "missing {function}");
                }
                if let Some(previous) = &previous {
                    assert_eq!(source, previous);
                }
                previous = Some(source.clone());
                existing.extend(built.into_iter().map(|file| (file.name, file.contents)));
            }
            let definition = if lang == ScorerLang::Python {
                "_definition.py"
            } else {
                ".definition.ts"
            };
            assert!(existing.contains_key(&format!("src/evals/first{definition}")));
            assert!(existing.contains_key(&format!("src/evals/second{definition}")));
            assert_eq!(existing[&callable], previous.unwrap());
        }
    }

    #[test]
    fn callable_module_exports_the_emitters_declared_function() {
        let target = EvaluationTarget {
            target: CodeTarget::new("test"),
            exports: RefCell::default(),
        };
        let score = Value::Num(Number::Int(1));
        let items = [Item {
            name: "quality",
            slug: "quality",
            score: &score,
            steps: &[],
        }];
        let source = target
            .code(
                CodeGroup {
                    lang: ScorerLang::Typescript,
                    stem: "checks",
                    items: &items,
                },
                Module {
                    source: "function assess(output: unknown) { return 1; }\n".into(),
                    functions: vec!["assess".into()],
                },
            )
            .unwrap();
        assert!(source[0].contents.contains("export { assess };"));
        assert!(source[1].contents.contains("import {assess} from \"./checks.impl.js\";"));
        assert!(!source[1].contents.contains("function assess"));
        let exports = target.exports.into_inner();
        assert_eq!(exports["quality"].function, "assess");
        assert_eq!(exports["quality"].file, "checks.impl.ts");
    }

    #[cfg(feature = "native")]
    #[test]
    fn generated_suites_order_baselines_and_pass_completed_experiment_ids() {
        use std::{fs, process::Command};

        let model = crate::dsl::semantics::compile(
            r#"
agent "baseline" { entrypoint = trace.baseline }
agent "candidate" { entrypoint = trace.candidate }
trace "baseline" { input = "question" output = "baseline" }
trace "candidate" { input = "question" output = "candidate" }
dataset "cases" { case "one" { input = "question" expected = "answer" } }
scorer "helpful" {
    judge { model = "gpt-4o-mini" prompt = "Check ${output} for ${input}" options = { yes = 1, no = 0 } }
}
experiment "candidate" {
    dataset = dataset.cases task = trace.candidate scorers = [scorer.helpful]
    baseline = experiment.baseline
}
experiment "baseline" { dataset = dataset.cases task = trace.baseline scorers = [scorer.helpful] }
"#,
        )
        .unwrap();
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let root = std::env::temp_dir().join(format!("bts-eval-order-{}", uuid::Uuid::new_v4()));
            let files = assemble(&model, lang, "test-project", 42).unwrap();
            for file in files {
                let path = root.join(file.name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, file.contents).unwrap();
            }
            let agents = root.join("src/agents");
            fs::create_dir_all(&agents).unwrap();
            let (program, script) = match lang {
                ScorerLang::Python => {
                    for package in ["agents", "evals", "scorers"] {
                        fs::write(root.join("src").join(package).join("__init__.py"), "").unwrap();
                    }
                    for agent in ["baseline", "candidate"] {
                        fs::write(
                            agents.join(format!("{agent}_agent.py")),
                            "def run_traced(span, input):\n    return input\n",
                        )
                        .unwrap();
                    }
                    fs::write(
                        root.join("src/autoevals.py"),
                        "class LLMClassifier:\n    def __init__(self, **config):\n        self.config = config\n",
                    )
                    .unwrap();
                    fs::write(
                        root.join("src/braintrust.py"),
                        r#"from types import SimpleNamespace
calls = []
missing_id = False
fail = False
def Eval(project, **definition):
    calls.append(definition)
    if fail:
        raise ValueError("baseline failed")
    id = None if missing_id or definition['no_send_logs'] else f"run-{len(calls)}"
    return SimpleNamespace(summary=SimpleNamespace(experiment_id=id))
"#,
                    )
                    .unwrap();
                    (
                        "python3",
                        r#"
import braintrust
from evals.run_evals import run
from scorers.helpful_judge import judge_helpful

assert braintrust.calls == []
assert judge_helpful.config['model'] == 'gpt-4o-mini'
assert judge_helpful.config['prompt_template'] == 'Check {{output}} for {{input}}'
assert judge_helpful.config['choice_scores'] == {'yes': 1, 'no': 0}
assert judge_helpful.config['use_cot'] is True
run()
assert [call['experiment_name'] for call in braintrust.calls] == ['baseline', 'candidate']
assert braintrust.calls[1]['base_experiment_id'] == 'run-1'
assert braintrust.calls[1]['base_experiment_name'] is None
braintrust.calls.clear()
run(no_send_logs=True)
assert len(braintrust.calls) == 2
assert all(call['base_experiment_id'] is None and call['base_experiment_name'] is None for call in braintrust.calls)
braintrust.calls.clear()
braintrust.missing_id = True
try:
    run()
    raise AssertionError('missing baseline ID was accepted')
except RuntimeError as error:
    assert 'experiment ID' in str(error)
assert len(braintrust.calls) == 1
braintrust.calls.clear()
braintrust.fail = True
try:
    run()
    raise AssertionError('baseline failure was ignored')
except ValueError:
    pass
assert len(braintrust.calls) == 1
braintrust.fail = False
braintrust.missing_id = False
braintrust.calls.clear()
import evals.candidate_eval
assert len(braintrust.calls) == 1
assert braintrust.calls[0]['base_experiment_name'] == 'baseline'
"#,
                    )
                }
                ScorerLang::Typescript => {
                    fs::write(root.join("package.json"), r#"{"type":"module"}"#).unwrap();
                    for agent in ["baseline", "candidate"] {
                        fs::write(
                            agents.join(format!("{agent}_agent.mts")),
                            "export function runTraced(span: any, input: any) { return input; }\n",
                        )
                        .unwrap();
                    }
                    for package in ["braintrust", "autoevals"] {
                        let path = root.join("node_modules").join(package);
                        fs::create_dir_all(&path).unwrap();
                        fs::write(path.join("package.json"), r#"{"type":"module","exports":"./index.mjs"}"#).unwrap();
                    }
                    fs::write(
                        root.join("node_modules/autoevals/index.mjs"),
                        "export function LLMClassifierFromTemplate(config) { return {config}; }\n",
                    )
                    .unwrap();
                    fs::write(
                        root.join("node_modules/braintrust/index.mjs"),
                        r#"export const calls = [];
export const state = {missingId: false, fail: false};
export async function Eval(project, definition, options) {
    calls.push({...definition, ...options});
    if (state.fail) throw new Error("baseline failed");
    await new Promise(resolve => setTimeout(resolve, 2));
    return {summary: {experimentId: state.missingId || options.noSendLogs ? undefined : "run-" + calls.length}};
}
"#,
                    )
                    .unwrap();
                    (
                        "node",
                        r#"
import assert from 'node:assert/strict';
import {calls, state} from 'braintrust';
import {run} from './evals/run-evals.mts';
import {judge_helpful} from './scorers/helpful.judge.ts';

assert.deepEqual(calls, []);
assert.equal(judge_helpful.config.model, 'gpt-4o-mini');
assert.equal(judge_helpful.config.promptTemplate, 'Check {{output}} for {{input}}');
assert.deepEqual(judge_helpful.config.choiceScores, {yes: 1, no: 0});
assert.equal(judge_helpful.config.useCoT, true);
await run();
assert.deepEqual(calls.map(call => call.experimentName), ['baseline', 'candidate']);
assert.equal(calls[1].baseExperimentId, 'run-1');
assert.equal(calls[1].baseExperimentName, undefined);
calls.length = 0;
await run(undefined, true);
assert.equal(calls.length, 2);
assert.ok(calls.every(call => call.baseExperimentId === undefined && call.baseExperimentName === undefined));
calls.length = 0;
state.missingId = true;
await assert.rejects(run(), /experiment ID/);
assert.equal(calls.length, 1);
calls.length = 0;
state.fail = true;
await assert.rejects(run(), /baseline failed/);
assert.equal(calls.length, 1);
state.fail = false;
state.missingId = false;
calls.length = 0;
await import('./evals/candidate.eval.ts');
assert.equal(calls.length, 1);
assert.equal(calls[0].baseExperimentName, 'baseline');
"#,
                    )
                }
            };
            let mut command = Command::new(program);
            if lang == ScorerLang::Python {
                command.arg("-c");
            } else {
                command.args(["--input-type=module", "-e"]);
            }
            let output = command.arg(script).current_dir(root.join("src")).output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            fs::remove_dir_all(root).unwrap();
        }
    }
}
