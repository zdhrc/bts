use bts_core::mocking::{ScorerLang, code, data};
use bts_core::providers::braintrust::{Bundle, mocking::code::CodeTarget};
use bts_core::providers::{Contributor, Provider, With, world};
use bts_lang::semantics::{Source, compile_module_with_provider};
use serde_json::{Value, json};
use std::{
    fs,
    process::Command,
    time::{Duration, UNIX_EPOCH},
};

const SOURCE: &str = r#"# shared mock data and mock code case
+(patient_lookup:node) {patient_id: {type: "string"}}-|:extends|->(tool);

(intake:agent) {
    input: {patient_id: "p-123"},
    output: request.output,
};

(intake)-|:next|->(request:task) {
    input: intake.input,
    output: lookup.output,
};

(request)-|:calls, :feeds {value: source.input.patient_id, into: target.patient_id}|->(lookup:patient_lookup) {
    patient_id: <-,
    input: {patient_id: self.patient_id},
    output: {patient_id: self.patient_id, found: true},
};
"#;

// the providers a host selects: braintrust lowers, world adds its kinds and functions
const SELECTED: With<Bundle, world::Bundle> = With(Bundle, world::Bundle);

fn lower(source: &str) -> Result<bts_core::mocking::Model, String> {
    let graph = compile_module_with_provider(
        Source {
            name: "agent.bt",
            text: source,
        },
        &SELECTED.schema(),
        false,
    )
    .unwrap_or_else(|errors| panic!("{errors:#?}"));
    SELECTED.lower(&graph, SELECTED.functions()).map_err(|diag| diag.what)
}

#[test]
fn intake_fixture_routes_known_and_unknown_patients() {
    let fixture = include_str!("fixtures/agent.bt");
    for (patient, route, department) in [("p-123", "schedule", "scheduling"), ("p-999", "register", "registration")] {
        let source = fixture.replacen("patient_id: \"p-123\"", &format!("patient_id: \"{patient}\""), 1);
        let model = lower(&source).unwrap();
        let batch = data::generate(
            model.clone(),
            1,
            Duration::from_secs(60),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(3600),
            42,
            &(),
        )
        .unwrap();
        assert_eq!(batch.trace_count, 1);
        assert_eq!(
            batch.events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
            ["request", "lookup", route, "reply"]
        );
        let expected = batch.events[0].output.as_ref().unwrap().project(&()).unwrap();
        assert_eq!(expected["department"], department);
        assert_eq!(batch.events[3].output, batch.events[0].output);
        for event in &batch.events[1..] {
            assert_eq!(event.span_parents.as_ref(), [batch.events[0].span_id.clone()]);
        }
        for (lang, program, file, runner) in [
            (
                ScorerLang::Python,
                "python3",
                "check.py",
                "import json\nfrom intake_agent import run_details\nprint(json.dumps(run_details()))",
            ),
            (
                ScorerLang::Typescript,
                "node",
                "check.mts",
                "import {runDetails} from './intake_agent.mts'; console.log(JSON.stringify(runDetails()));",
            ),
        ] {
            let result = execute_generated(&model, lang, program, file, runner);
            assert_eq!(result["output"], expected);
            assert_eq!(
                result["spans"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|span| span["name"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                ["request", "lookup", route, "reply"]
            );
        }
    }
}

#[test]
fn graph_agent_generates_spans_and_runnable_code() {
    check_agent(SOURCE, false);
}

#[test]
fn graph_agent_preserves_sibling_progression_and_inherited_kinds() {
    let source = SOURCE
        .replace("intake:agent", "intake:intake_kind")
        .replace("lookup:patient_lookup", "lookup:lookup_kind")
        .replace("output: lookup.output", "output: finish.output");
    let source = format!(
        r#"+(intake_kind:node)-|:extends|->(agent); +(lookup_kind:node)-|:extends|->(patient_lookup);
{source}
(lookup)-|:next|->(finish:tool) {{input: intake.input, output: {{patient_id: self.input.patient_id, found: true}}}};
"#
    );
    check_agent(&source, true);
}

fn check_agent(source: &str, sibling: bool) {
    let model = lower(source).unwrap();
    let batch = data::generate(
        model.clone(),
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap();
    assert_eq!(batch.trace_count, 1);
    assert_eq!(batch.events.len(), if sibling { 3 } else { 2 });
    let parent = &batch.events[0];
    let child = &batch.events[1];
    assert_eq!(parent.name, "request");
    assert_eq!(child.name, "lookup");
    assert_eq!(child.span_parents.as_ref(), std::slice::from_ref(&parent.span_id));
    assert_eq!(child.root_span_id, parent.span_id);
    assert!(parent.start <= child.start && parent.end >= child.end);
    assert_eq!(
        parent.output.as_ref().unwrap().project(&()).unwrap(),
        json!({"patient_id": "p-123", "found": true})
    );
    assert_eq!(child.output, parent.output);

    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let (program, file, runner) = match lang {
            ScorerLang::Python => (
                "python3",
                "check.py",
                r#"
import json
from intake_agent import run, run_details, run_traced
records = []
class Logger:
    def __init__(self, name=None): self.name = name
    def start_span(self, name, **kwargs):
        records.append(["start", name, self.name])
        return Logger(name)
    def log(self, **fields): records.append(["log", self.name, fields])
    def end(self, **kwargs): records.append(["end", self.name])
value = {"patient_id": "p-456"}
print(json.dumps({"default": run_details(), "supplied": run(value), "traced": run_traced(Logger(), value), "records": records}))
"#,
            ),
            ScorerLang::Typescript => (
                "node",
                "check.mts",
                r#"
import {run, runDetails, runTraced} from "./intake_agent.mts";
const records: any[] = [];
class Logger {
    name: string | null;
    constructor(name: string | null = null) {this.name = name;}
    startSpan({name}: {name: string}) {records.push(["start", name, this.name]); return new Logger(name);}
    log(fields: unknown) {records.push(["log", this.name, fields]);}
    end() {records.push(["end", this.name]);}
}
const value = {patient_id: "p-456"};
console.log(JSON.stringify({default: runDetails(), supplied: run(value), traced: runTraced(new Logger(), value), records}));
"#,
            ),
        };
        let result = execute_generated(&model, lang, program, file, runner);
        assert_eq!(
            result["default"]["output"],
            parent.output.as_ref().unwrap().project(&()).unwrap()
        );
        assert_eq!(result["default"]["spans"].as_array().unwrap().len(), batch.events.len());
        assert_eq!(result["default"]["spans"][1]["parent"], 0);
        assert_eq!(result["supplied"], json!({"patient_id": "p-456", "found": true}));
        assert_eq!(result["traced"], result["supplied"]);
        let records = result["records"].as_array().unwrap();
        let lifecycle: Vec<_> = records.iter().filter(|record| record[0] != "log").cloned().collect();
        let mut expected = vec![
            json!(["start", "request", null]),
            json!(["start", "lookup", "request"]),
            json!(["end", "lookup"]),
        ];
        if sibling {
            expected.extend([json!(["start", "finish", "request"]), json!(["end", "finish"])]);
            assert!(batch.events[1].end <= batch.events[2].start);
        }
        expected.push(json!(["end", "request"]));
        assert_eq!(lifecycle, expected);
    }
}

fn execute_generated(model: &bts_core::mocking::Model, lang: ScorerLang, program: &str, file: &str, runner: &str) -> Value {
    let directory = std::env::temp_dir().join(format!("bts-agent-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let assembly = code::assemble_agents(model, lang, &CodeTarget::new("local-test")).unwrap();
    for file in &assembly.files {
        let path = directory.join(&file.name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, &file.contents).unwrap();
    }
    // an llm step counts its tokens, and the tokenizer is not installed where the tests run
    // these stand-ins count words, so token metrics here are not real counts
    fs::write(directory.join("src/agents/tiktoken.py"), "def get_encoding(name):\n    class Words:\n        def encode_ordinary(self, text): return text.split()\n    return Words()\n").unwrap();
    let tokenizer = directory.join("node_modules/js-tiktoken");
    fs::create_dir_all(&tokenizer).unwrap();
    fs::write(tokenizer.join("package.json"), r#"{"type":"module","exports":"./index.mjs"}"#).unwrap();
    fs::write(
        tokenizer.join("index.mjs"),
        "export function getEncoding() { return {encode: text => text.split(/\\s+/).filter(Boolean)}; }\n",
    )
    .unwrap();
    let path = directory.join("src/agents").join(file);
    fs::write(&path, runner).unwrap();
    let output = Command::new(program).arg(&path).output().expect("local language runtime");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn graph_agent_rejects_invalid_parenting_and_unsupported_flow() {
    for (source, message) in [
        (
            SOURCE.replace("(request)-|:calls, :feeds", "(request)-|:next, :feeds"),
            "`lookup` has no parent span, go down from `request` with a `calls` edge, or indent this line under the step it follows",
        ),
        (
            format!("{SOURCE}\n(request)-|:calls|->(other:task) {{output: 1}}; (other)-|:calls|->(lookup);"),
            "`lookup` is reached under different parent spans, `request` and `other`",
        ),
        (
            format!("{SOURCE}\n(intake)-|:contains|->(lookup);"),
            "agent membership comes from the entry edge, remove this `contains` edge",
        ),
        (
            SOURCE.replace("(intake)-|:next|->(request:task)", "(intake)-|:calls|->(request:task)"),
            "an agent emits no span, enter its first step with a `next` edge",
        ),
        (
            SOURCE.replace("found: true}", "found: true, under: context.parent}"),
            "`context.parent` is supplied to the provider and cannot be read by a field",
        ),
        (format!("{SOURCE}\n(lookup)~|:next|~>(request);"), "cycle edges"),
        (format!("{SOURCE}\n(request)-|:next|->(lookup);"), "multiple next"),
        (
            SOURCE.replace(
                ":calls, :feeds {value: source.input.patient_id, into: target.patient_id}",
                ":calls",
            ),
            "no feeds edge",
        ),
    ] {
        let error = lower(&source).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn graph_bindings_cache_samples_per_invocation() {
    let source = SOURCE
        .replace("patient_id: <-,", "patient_id: <-, local: {token: hex(8)},")
        .replace(
            "found: true",
            "found: true, first: self.local.token, second: self.local.token",
        );
    let generate = || {
        let mut batch = data::generate(
            lower(&source).unwrap(),
            3,
            Duration::from_secs(60),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(3600),
            42,
            &(),
        )
        .unwrap();
        batch.assign_stable_ids("graph-sampling");
        batch
    };
    let batch = generate();
    assert_eq!(batch, generate());
    let mut tokens = std::collections::HashSet::new();
    for event in batch.events.iter().filter(|event| event.span_parents.is_empty()) {
        let output = event.output.as_ref().unwrap();
        assert_eq!(output["first"], output["second"]);
        tokens.insert(output["first"].as_str().unwrap());
    }
    assert_eq!(tokens.len(), 3);
}

#[test]
fn typed_propagation_is_checked_by_data_and_generated_code() {
    for (ty, value) in [
        ("string", "\"patient\""),
        ("number", "42"),
        ("boolean", "true"),
        ("array", "[1]"),
        ("object", "{}"),
    ] {
        for valid in [true, false] {
            let value = if valid { value } else { "null" };
            let source = format!(
                "+(checked_tool:node) {{patient_id: {{type: \"{ty}\"}}}}-|:extends|->(tool);\n{}",
                SOURCE
                    .replace("lookup:patient_lookup", "lookup:checked_tool")
                    .replace("\"p-123\"", value)
            );
            let model = lower(&source).unwrap();
            let result = data::generate(
                model.clone(),
                1,
                Duration::from_secs(60),
                data::Distribution::Linear,
                UNIX_EPOCH + Duration::from_secs(3600),
                42,
                &(),
            );
            if valid {
                assert!(result.is_ok(), "{result:?}");
            } else {
                let data::Error::Plan(error) = result.unwrap_err() else {
                    panic!("expected evaluation failure")
                };
                assert_eq!(error.kind(), data::EvaluationErrorKind::SchemaTypeMismatch);
                assert_eq!(&source[error.range.start..error.range.end], "<-");
            }
            let py = r#"
import json
from intake_agent import run
try:
    run()
    print(json.dumps({"valid": True}))
except TypeError as error:
    print(json.dumps({"valid": False, "error": str(error)}))
"#;
            let ts = r#"
import {run} from "./intake_agent.mts";
try {run(); console.log(JSON.stringify({valid: true}));}
catch(error) {if (!(error instanceof TypeError)) throw error; console.log(JSON.stringify({valid: false, error: error.message}));}
"#;
            for (lang, program, file, runner) in [
                (ScorerLang::Python, "python3", "check.py", py),
                (ScorerLang::Typescript, "node", "check.mts", ts),
            ] {
                let result = execute_generated(&model, lang, program, file, runner);
                assert_eq!(result["valid"], valid, "{ty}: {result}");
                if !valid {
                    assert!(result["error"].as_str().unwrap().contains(ty));
                }
            }
        }
    }
}

#[test]
fn agent_result_does_not_overwrite_span_output() {
    for root_output in ["output: \"request accepted\",", ""] {
        let source = SOURCE
            .replace("output: request.output", "output: {reply: lookup.output}")
            .replace("output: lookup.output,", root_output);
        let model = lower(&source).unwrap();
        let batch = data::generate(
            model.clone(),
            1,
            Duration::from_secs(60),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(3600),
            42,
            &(),
        )
        .unwrap();
        let expected = if root_output.is_empty() {
            None
        } else {
            Some(data::Value::from("request accepted"))
        };
        assert_eq!(batch.events[0].output, expected);
        let py =
            "import json\nfrom intake_agent import run_details\nprint(json.dumps(run_details({\"patient_id\": \"p-789\"})))";
        let ts =
            "import {runDetails} from './intake_agent.mts'; console.log(JSON.stringify(runDetails({patient_id: 'p-789'})));";
        for (lang, program, file, runner) in [
            (ScorerLang::Python, "python3", "check.py", py),
            (ScorerLang::Typescript, "node", "check.mts", ts),
        ] {
            let details = execute_generated(&model, lang, program, file, runner);
            assert_eq!(details["output"], json!({"reply": {"patient_id": "p-789", "found": true}}));
            if root_output.is_empty() {
                assert!(details["spans"][0].get("output").is_none());
            } else {
                assert_eq!(details["spans"][0]["output"], "request accepted");
            }
            assert_eq!(details["spans"][1]["output"], details["output"]["reply"]);
        }
    }
}

#[test]
fn guarded_forks_select_the_same_spans_in_data_and_code() {
    let source = r#"
(intake:agent) {input: {route: "lookup"}, output: request.output};
(intake)-|:next|->(request:task) {
    input: intake.input,
    output: self.input.route == "lookup" ? lookup.output: self.input.route == "fallback" ? fallback.output: "none",
}
    =|:exclusive|=>[
        -|:calls {when: source.input.route == "lookup"}|->(lookup:tool) {output: "found"},
        -|:calls {when: source.input.route == "fallback"}|->(fallback:tool) {output: "deferred"},
    ];
"#;
    for (route, result, count) in [("lookup", "found", 2), ("fallback", "deferred", 2), ("other", "none", 1)] {
        let source = source.replacen("route: \"lookup\"", &format!("route: \"{route}\""), 1);
        let model = lower(&source).unwrap();
        let batch = data::generate(
            model.clone(),
            1,
            Duration::from_secs(60),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(3600),
            42,
            &(),
        )
        .unwrap();
        assert_eq!(batch.events.len(), count);
        assert_eq!(batch.events[0].output.as_ref().unwrap(), &data::Value::from(result));
        let py = "import json\nfrom intake_agent import run_details\nprint(json.dumps(run_details()))";
        let ts = "import {runDetails} from './intake_agent.mts'; console.log(JSON.stringify(runDetails()));";
        for (lang, program, file, runner) in [
            (ScorerLang::Python, "python3", "check.py", py),
            (ScorerLang::Typescript, "node", "check.mts", ts),
        ] {
            let details = execute_generated(&model, lang, program, file, runner);
            assert_eq!(details["output"], result);
            assert_eq!(details["spans"].as_array().unwrap().len(), count);
            if count == 2 {
                assert_eq!(details["spans"][1]["name"], route);
            }
        }
    }
    let overlapping = source
        .replace("exclusive", "conditional")
        .replace("source.input.route == \"fallback\"", "true");
    let model = lower(&overlapping).unwrap();
    let batch = data::generate(
        model,
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap();
    assert_eq!(batch.events.len(), 3);
}

#[test]
fn scorer_node_supports_span_and_callable_behaviors() {
    use bts_core::mocking::ScorerKind;
    use code::Emitter;
    let source = r#"
(checker:agent) {input: "yes", output: root.output};
(checker)-|:next|->(root:task) {input: checker.input, output: self.input, expected: "yes"};
(root)-|:calls {when: source.input != "skip"}|->(quality:scorer {file: "checks"}) {
    score: context.output == context.expected ? 1: 0,
    reason: "compared output to expected",
};
"#;
    let model = lower(source).unwrap();
    let batch = data::generate(
        model.clone(),
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.events[1].name, "quality");
    assert_eq!(
        batch.events[1].output.as_ref().unwrap().project(&()).unwrap(),
        json!({"score": 1.0})
    );
    assert_eq!(model.scorers.len(), 1);
    assert_eq!(model.scorers[0].file.as_deref(), Some("checks"));
    let ScorerKind::Code { score, steps } = &model.scorers[0].kind else {
        panic!("code scorer")
    };
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let module = code::languages::Languages
            .emit(
                lang,
                &[code::Item {
                    name: "quality",
                    slug: "quality",
                    score,
                    steps,
                }],
            )
            .unwrap();
        let dir = std::env::temp_dir().join(format!("bts-scorer-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let (program, file, call) = match lang {
            ScorerLang::Python => (
                "python3",
                "check.py",
                "\nimport json\nprint(json.dumps(quality(output='yes', expected='yes')))\n",
            ),
            ScorerLang::Typescript => (
                "node",
                "check.mts",
                "\nconsole.log(JSON.stringify(quality({output: 'yes', expected: 'yes'})))\n",
            ),
        };
        let path = dir.join(file);
        let call = call.replace("quality(", &format!("{}(", module.functions[0]));
        fs::write(&path, format!("{}{call}", module.source)).unwrap();
        let output = Command::new(program).arg(path).output().unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["score"], 1);
        let (program, file, runner) = match lang {
            ScorerLang::Python => (
                "python3",
                "check.py",
                "from checker_agent import run_details\nimport json\nprint(json.dumps(run_details()))\n",
            ),
            ScorerLang::Typescript => (
                "node",
                "check.mts",
                "import {runDetails} from './checker_agent.mts'\nconsole.log(JSON.stringify(runDetails()))\n",
            ),
        };
        let value = execute_generated(&model, lang, program, file, runner);
        assert_eq!(value["output"], "yes");
        assert_eq!(value["spans"][0]["scores"]["quality"], 1);
        assert_eq!(value["spans"][1]["output"]["score"], 1);
        assert_eq!(value["spans"][1]["metadata"]["reason"], "compared output to expected");
    }
    let standalone = lower("(quality:scorer) {score: context.output == context.expected ? 1 : 0};").unwrap();
    assert!(standalone.agents.is_empty());
    assert_eq!(standalone.scorers.len(), 1);
}

#[test]
fn accumulate_groups_results_and_rejects_missing_inputs() {
    let source = r#"
(joined:agent) {input: true, output: finish.output};
(joined)-|:next|->(root:task) {input: joined.input};
(root)
    =|:conditional|=>[
        -|first:calls {when: source.input}|->(left_task:tool) {output: "first"},
        -|second:calls|->(right_task:tool) {output: "second"},
    ]
        >|:accumulate {observe: "result"} |second|, |first||>>(finish:task) {output: context.arrivals};
"#;
    let expected = json!([{"edge": "second", "value": "second"}, {"edge": "first", "value": "first"}]);
    let model = lower(source).unwrap();
    let batch = data::generate(
        model.clone(),
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap();
    assert_eq!(batch.events[3].output.as_ref().unwrap().project(&()).unwrap(), expected);
    let missing = lower(&source.replace("input: true", "input: false")).unwrap();
    let error = data::generate(
        missing,
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("missing listed input"), "{error}");
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let (program, file, runner) = match lang {
            ScorerLang::Python => (
                "python3",
                "check.py",
                r#"
from joined_agent import run
import json
ok = run(True)
try:
    run(False)
    raise AssertionError("missing arrival was accepted")
except ValueError as error:
    assert "missing listed input" in str(error)
print(json.dumps(ok))
"#,
            ),
            ScorerLang::Typescript => (
                "node",
                "check.mts",
                r#"
import {run} from './joined_agent.mts';
const ok = run(true);
try {run(false); throw new Error('missing arrival was accepted');}
catch (error) {if (!String(error).includes('missing listed input')) throw error;}
console.log(JSON.stringify(ok));
"#,
            ),
        };
        assert_eq!(execute_generated(&model, lang, program, file, runner), expected);
    }
}

#[test]
fn each_materializes_distinct_occurrences() {
    let source = r#"
(batch:agent) {input: ["a", "b", "c"], output: "done"};
(batch)-|:next|->(root:task) {input: batch.input};
(root)
    =|:each {items: source.input}|=>[
        -|:calls|->(work:tool) {input: context.item, output: {value: self.input, index: context.index}},
    ];
"#;
    let model = lower(source).unwrap();
    let batch = data::generate(
        model.clone(),
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap();
    assert_eq!(batch.events.len(), 4);
    for (index, event) in batch.events.iter().skip(1).enumerate() {
        assert_eq!(
            event.output.as_ref().unwrap().project(&()).unwrap(),
            json!({"value": (["a", "b", "c"][index]), "index": index})
        );
    }
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let (program, file, runner) = match lang {
            ScorerLang::Python => (
                "python3",
                "check.py",
                "from batch_agent import run_details\nimport json\nprint(json.dumps(run_details()))\n",
            ),
            ScorerLang::Typescript => (
                "node",
                "check.mts",
                "import {runDetails} from './batch_agent.mts';\nconsole.log(JSON.stringify(runDetails()));\n",
            ),
        };
        let result = execute_generated(&model, lang, program, file, runner);
        assert_eq!(result["spans"].as_array().unwrap().len(), 4);
        for index in 0..3 {
            assert_eq!(
                result["spans"][index + 1]["output"],
                json!({"value": (["a", "b", "c"][index]), "index": index})
            );
        }
    }
}

#[test]
fn repeat_preserves_iteration_scope_and_empty_groups() {
    let source = r#"
(batch:agent) {input: 3, output: "done"};
(batch)-|:next|->(root:task) {input: batch.input};
(root)
    =|:repeat {count: source.input}|=>[
        -|:calls|->(work:tool) {
            local: {token: hex(8)},
            output: {index: context.index, count: context.count, first: self.local.token, second: self.local.token},
        },
    ];
"#;
    let model = lower(source).unwrap();
    let batch = data::generate(
        model.clone(),
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap();
    assert_eq!(batch.events.len(), 4);
    let mut tokens = std::collections::HashSet::new();
    for (index, event) in batch.events.iter().skip(1).enumerate() {
        let output = event.output.as_ref().unwrap().project(&()).unwrap();
        assert_eq!(output["index"], index);
        assert_eq!(output["count"], 3);
        assert_eq!(output["first"], output["second"]);
        tokens.insert(output["first"].as_str().unwrap().to_owned());
    }
    assert_eq!(tokens.len(), 3);
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let (program, file, runner) = match lang {
            ScorerLang::Python => (
                "python3",
                "check.py",
                "from batch_agent import run_details\nimport json\nprint(json.dumps([run_details(3), run_details(0)]))\n",
            ),
            ScorerLang::Typescript => (
                "node",
                "check.mts",
                "import {runDetails} from './batch_agent.mts';\nconsole.log(JSON.stringify([runDetails(3), runDetails(0)]));\n",
            ),
        };
        let result = execute_generated(&model, lang, program, file, runner);
        assert_eq!(result[0]["spans"].as_array().unwrap().len(), 4);
        assert_eq!(result[1]["spans"].as_array().unwrap().len(), 1);
        let mut tokens = std::collections::HashSet::new();
        for index in 0..3 {
            let output = &result[0]["spans"][index + 1]["output"];
            assert_eq!(output["index"], index);
            assert_eq!(output["count"], 3);
            assert_eq!(output["first"], output["second"]);
            tokens.insert(output["first"].as_str().unwrap());
        }
        assert_eq!(tokens.len(), 3);
    }
}

#[test]
fn repeat_history_contains_only_completed_earlier_occurrences() {
    let source = r#"
(history_agent:agent) {input: 3, output: "done"};
(history_agent)-|:next|->(root:task) {input: history_agent.input};
(root)
    =|:repeat {count: source.input}|=>[
        -|:calls|->(turn:task) {
            input: context.history.turn,
            output: context.index,
        },
    ];
"#;
    for nested in [false, true] {
        let source = if nested {
            source.replace(
                "(root)\n    =|:repeat",
                "(root)=|:repeat {count: 2}|=>[-|:calls|->(group:task) {input: root.input}];\n(group)\n    =|:repeat",
            )
        } else {
            source.to_owned()
        };
        let model = lower(&source).unwrap();

        let batch = data::generate(
            model.clone(),
            1,
            Duration::from_secs(60),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(3600),
            42,
            &(),
        )
        .unwrap();
        assert_eq!(
            batch.events.iter().filter(|event| event.name == "turn").count(),
            if nested { 6 } else { 3 }
        );
        for (position, event) in batch.events.iter().filter(|event| event.name == "turn").enumerate() {
            let index = position % 3;
            let input = event.input.as_ref().unwrap().project(&()).unwrap();
            assert_eq!(input.as_array().unwrap().len(), index);
            for earlier in 0..index {
                assert_eq!(input[earlier]["output"], earlier);
            }
        }
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let (program, file, runner) = match lang {
                ScorerLang::Python => (
                    "python3",
                    "check.py",
                    "from history_agent import run_details\nimport json\nprint(json.dumps(run_details()))\n",
                ),
                ScorerLang::Typescript => (
                    "node",
                    "check.mts",
                    "import {runDetails} from './history_agent.mts';\nconsole.log(JSON.stringify(runDetails()));\n",
                ),
            };
            let result = execute_generated(&model, lang, program, file, runner);
            for (position, span) in result["spans"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|span| span["name"] == "turn")
                .enumerate()
            {
                let index = position % 3;
                let input = &span["input"];
                assert_eq!(input.as_array().unwrap().len(), index);
                for earlier in 0..index {
                    assert_eq!(input[earlier]["output"], earlier);
                }
            }
        }
    }
}

#[test]
fn weighted_forks_select_once_and_allow_no_selection() {
    let source = r#"
(router:agent) {input: {enabled: true, weight: 8}, output: root.output};
(router)-|:next|->(root:task) {input: router.input, output: "done"};
(root) =|:weighted|=>[
    -|:calls {weight: source.input.weight, when: source.input.enabled}|->(a:tool) {output: "a"},
    -|:calls {weight: 2, when: source.input.enabled}|->(b:tool) {output: "b"},
];
"#;
    let model = lower(source).unwrap();
    let run = |model| {
        data::generate(
            model,
            100,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            42,
            &(),
        )
        .unwrap()
    };
    let batch = run(model.clone());
    assert_eq!(batch.events.len(), 200);
    let choices: Vec<_> = batch
        .events
        .iter()
        .filter(|event| event.name != "root")
        .map(|event| event.name.clone())
        .collect();
    assert!(choices.contains(&"a".into()) && choices.contains(&"b".into()));
    assert_eq!(
        choices,
        run(model.clone())
            .events
            .iter()
            .filter(|event| event.name != "root")
            .map(|event| event.name.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        run(lower(&source.replace("enabled: true", "enabled: false")).unwrap())
            .events
            .len(),
        100
    );
    for (lang, program, file, runner) in [
        (
            ScorerLang::Python,
            "python3",
            "check.py",
            r#"
from router_agent import run_details
import json
selected = run_details({"enabled": True, "weight": 8})
assert len(selected["spans"]) == 2
empty = run_details({"enabled": False, "weight": -1})
assert len(empty["spans"]) == 1
try:
    run_details({"enabled": True, "weight": -1})
    raise AssertionError("negative weight accepted")
except ValueError:
    pass
print(json.dumps(selected["output"]))
"#,
        ),
        (
            ScorerLang::Typescript,
            "node",
            "check.mts",
            r#"
import {runDetails} from './router_agent.mts';
const selected = runDetails({enabled: true, weight: 8});
if (selected.spans.length !== 2) throw new Error('not one branch');
if (runDetails({enabled: false, weight: -1}).spans.length !== 1) throw new Error('not empty');
let failed = false;
try {runDetails({enabled: true, weight: -1});} catch {failed = true;}
if (!failed) throw new Error('negative weight accepted');
console.log(JSON.stringify(selected.output));
"#,
        ),
    ] {
        assert_eq!(execute_generated(&model, lang, program, file, runner), json!("done"));
    }
}

#[test]
fn weighted_forks_resample_in_each_repetition_scope() {
    let source = r#"
(router:agent) {input: true, output: "done"};
(router)-|:next|->(root:task) {input: router.input};
(root) =|:repeat {count: 3}|=>[-|:calls|->(turn:task) {input: context.index}];
(turn) =|:weighted|=>[
    -|:calls {weight: source.input == 0 ? 1 : 0}|->(a:tool) {output: "a"},
    -|:calls {weight: source.input == 0 ? 0 : 1}|->(b:tool) {output: "b"},
];
"#;
    let model = lower(source).unwrap();
    let batch = data::generate(
        model.clone(),
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(3600),
        42,
        &(),
    )
    .unwrap();
    let expected = ["root", "turn", "a", "turn", "b", "turn", "b"];
    assert_eq!(
        batch.events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
        expected
    );
    for (lang, program, file, runner) in [
        (
            ScorerLang::Python,
            "python3",
            "check.py",
            "from router_agent import run_details\nimport json\nprint(json.dumps([span['name'] for span in run_details()['spans']]))",
        ),
        (
            ScorerLang::Typescript,
            "node",
            "check.mts",
            "import {runDetails} from './router_agent.mts'; console.log(JSON.stringify(runDetails().spans.map(span => span.name)));",
        ),
    ] {
        assert_eq!(execute_generated(&model, lang, program, file, runner), json!(expected));
    }
}

const CYCLE_SOURCE: &str = r#"
(retrying:agent) {input: {attempt: 0}, output: finish.output};
(retrying)-|:next|->(root:task) {input: retrying.input, output: finish.output};
(root)-|:calls, :feeds {value: source.input, into: target.input}|->(lookup:tool) {
    input: <-,
    output: {attempt: self.input.attempt, earlier: len(context.history.lookup)},
};
(lookup)-|:next|->(check:task) {output: {retry: lookup.output.attempt < 2, next_input: {attempt: lookup.output.attempt + 1}}};
(check) =|:exclusive|=>[
    ~|:next {when: source.output.retry, max_traversals: 3}, :feeds {when: source.output.retry, value: source.output.next_input, into: target.input}|~>(lookup),
    -|:next {when: !source.output.retry}|->(finish:task) {output: lookup.output},
];
"#;

fn one_trace(source: &str) -> data::EventBatch {
    data::generate(
        lower(source).unwrap(),
        1,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        42,
        &(),
    )
    .unwrap()
}

fn generated_runs(model: &bts_core::mocking::Model, name: &str) -> Vec<Value> {
    [ScorerLang::Python, ScorerLang::Typescript]
        .into_iter()
        .map(|lang| match lang {
            ScorerLang::Python => execute_generated(
                model,
                lang,
                "python3",
                "check.py",
                &format!("from {name}_agent import run_details\nimport json\nprint(json.dumps(run_details()))"),
            ),
            ScorerLang::Typescript => execute_generated(
                model,
                lang,
                "node",
                "check.mts",
                &format!("import {{runDetails}} from './{name}_agent.mts'; console.log(JSON.stringify(runDetails()));"),
            ),
        })
        .collect()
}

#[test]
fn cycles_create_fresh_occurrences_carry_inputs_and_preserve_history() {
    let model = lower(CYCLE_SOURCE).unwrap();
    let batch = one_trace(CYCLE_SOURCE);
    let names = ["root", "lookup", "check", "lookup", "check", "lookup", "check", "finish"];
    assert_eq!(
        batch.events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
        names
    );
    for (index, event) in batch.events.iter().filter(|event| event.name == "lookup").enumerate() {
        assert_eq!(
            event.output.as_ref().unwrap().project(&()).unwrap(),
            json!({"attempt":index,"earlier":index})
        );
    }
    for result in generated_runs(&model, "retrying") {
        assert_eq!(result["output"], json!({"attempt":2,"earlier":2}));
        assert_eq!(
            result["spans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| span["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            names
        );
    }
    // the loop-back prints as an ordinary loop that stops when a round does not ask for another
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("retrying_agent."))
            .unwrap();
        assert!(agent.contents.contains("break"), "{}", agent.contents);
        assert!(agent.contents.contains("cycle max_traversals exceeded"), "{}", agent.contents);
        for helper in ["_loop(", "_lazy(", "_child(", "_iteration("] {
            assert!(!agent.contents.contains(helper), "{helper} in {}", agent.contents);
        }
    }
    let limited = lower(&CYCLE_SOURCE.replace("max_traversals: 3", "max_traversals: 1")).unwrap();
    let error = data::generate(
        limited,
        1,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        42,
        &(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("max_traversals"), "{error}");
}

#[test]
fn junction_strategies_execute_in_native_python_and_typescript() {
    let source = r#"
(joined:agent) {input: true, output: "done"};
(joined)-|:next|->(root:task) {input: joined.input};
(root) =|:parallel|=>[
    -|a:calls|->(left_task:tool) {output: 2},
    -|b:calls|->(right_task:tool) {output: 3},
]
    >|:STRATEGY {observe: "result" CONFIG} |b|, |a||>>(finish:task) {output: RESULT};
"#;
    for (kind, config, result, expected) in [
        (
            "accumulate",
            "",
            "context.arrivals",
            json!([[{"edge":"b","value":3},{"edge":"a","value":2}]]),
        ),
        (
            "merge",
            "",
            "context.arrivals",
            json!([[{"edge":"a","value":2}],[{"edge":"b","value":3}]]),
        ),
        ("first", "", "context.arrivals", json!([[{"edge":"a","value":2}]])),
        (
            "quorum",
            ", count: 2",
            "context.arrivals",
            json!([[{"edge":"b","value":3},{"edge":"a","value":2}]]),
        ),
        (
            "reduce",
            ", initial: 0, combine: context.accumulator + context.arrival.value",
            "context.accumulator",
            json!([5]),
        ),
    ] {
        let source = source
            .replace("STRATEGY", kind)
            .replace("CONFIG", config)
            .replace("RESULT", result);
        let model = lower(&source).unwrap();
        let batch = one_trace(&source);
        assert_eq!(
            json!(
                batch
                    .events
                    .iter()
                    .filter(|event| event.name == "finish")
                    .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
                    .collect::<Vec<_>>()
            ),
            expected,
            "{kind}"
        );
        for generated in generated_runs(&model, "joined") {
            assert_eq!(
                json!(
                    generated["spans"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|span| span["name"] == "finish")
                        .map(|span| span["output"].clone())
                        .collect::<Vec<_>>()
                ),
                expected,
                "{kind}"
            );
        }
    }
}

fn generated_failure(model: &bts_core::mocking::Model, name: &str, message: &str) {
    let python = format!(
        "from {name}_agent import run_details\nimport json\ntry:\n    run_details()\nexcept Exception as error:\n    assert {message:?} in str(error), str(error)\nelse:\n    raise AssertionError('expected failure')\nprint(json.dumps(True))"
    );
    let typescript = format!(
        "import {{runDetails}} from './{name}_agent.mts'; let failed = false; try {{runDetails();}} catch (error) {{if (!String(error).includes({message:?})) throw error; failed = true;}} if (!failed) throw new Error('expected failure'); console.log(JSON.stringify(true));"
    );
    assert_eq!(
        execute_generated(model, ScorerLang::Python, "python3", "check.py", &python),
        json!(true)
    );
    assert_eq!(
        execute_generated(model, ScorerLang::Typescript, "node", "check.mts", &typescript),
        json!(true)
    );
}

const QUEUED_SOURCE: &str = r#"
(joined:agent) {input: true, output: "done"};
(joined)-|:next|->(root:task) {input: joined.input};
(root) =|:repeat {count: 3}|=>[
    -|a:calls|->(left_task:tool) {output: context.index + 1},
    -|b:calls|->(right_task:tool) {output: context.index + 10},
]
    >|:accumulate {observe: "result"} |b|, |a||>>(finish:task) {output: context.arrivals};
"#;

#[test]
fn junction_queues_consume_repeated_inputs_and_reduce_at_closure() {
    for (source, expected) in [
        (
            QUEUED_SOURCE.to_owned(),
            json!([
                [{"edge":"b","value":10},{"edge":"a","value":1}],
                [{"edge":"b","value":11},{"edge":"a","value":2}],
                [{"edge":"b","value":12},{"edge":"a","value":3}],
            ]),
        ),
        (
            QUEUED_SOURCE
                .replace(
                    ":accumulate {observe: \"result\"}",
                    ":reduce {observe: \"result\", initial: 0, combine: context.accumulator + context.arrival.value}",
                )
                .replace("output: context.arrivals", "output: context.accumulator"),
            json!([39]),
        ),
        (
            QUEUED_SOURCE
                .replace("count: 3", "count: 0")
                .replace(
                    ":accumulate {observe: \"result\"}",
                    ":reduce {observe: \"result\", initial: 7, combine: context.accumulator + context.arrival.value}",
                )
                .replace("output: context.arrivals", "output: context.accumulator"),
            json!([7]),
        ),
        (
            QUEUED_SOURCE
                .replace(":repeat {count: 3}", ":each {items: [{a: 1, b: 2}, {b: 2, a: 1}]}")
                .replace(
                    ":accumulate {observe: \"result\"}",
                    ":first {observe: \"result\", correlate: context.item}",
                ),
            json!([[{"edge":"a","value":1}]]),
        ),
        (
            QUEUED_SOURCE
                .replace(":repeat {count: 3}", ":each {items: [\"a\", \"b\"]}")
                .replace(
                    ":accumulate {observe: \"result\"}",
                    ":accumulate {observe: \"result\", correlate: context.item}",
                ),
            json!([
                [{"edge":"b","value":10},{"edge":"a","value":1}],
                [{"edge":"b","value":11},{"edge":"a","value":2}],
            ]),
        ),
    ] {
        let model = lower(&source).unwrap();
        let batch = one_trace(&source);
        assert_eq!(
            json!(
                batch
                    .events
                    .iter()
                    .filter(|event| event.name == "finish")
                    .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
                    .collect::<Vec<_>>()
            ),
            expected
        );
        for generated in generated_runs(&model, "joined") {
            assert_eq!(
                json!(
                    generated["spans"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|span| span["name"] == "finish")
                        .map(|span| span["output"].clone())
                        .collect::<Vec<_>>()
                ),
                expected
            );
        }
    }
}

#[test]
fn cycles_and_junctions_fail_explicitly_without_partial_success() {
    // one step reached along two edges would run twice, which is refused before anything runs
    let ambiguous = r#"
(joined:agent) {input: true, output: finish.output};
(joined)-|:next|->(root:task) {input: joined.input};
(shared:tool) {output: 1};
(root) =|:parallel|=>[-|a:calls|->(shared), -|b:calls|->(shared)]
    >|:accumulate {observe: "result"} |a|, |b||>>(finish:task) {output: shared.output};
"#;
    let error = lower(ambiguous).unwrap_err();
    assert!(error.contains("multiple next edges to one target"), "{error}");
    for (source, name, message) in [
        (
            CYCLE_SOURCE.replace("max_traversals: 3", "max_traversals: 1"),
            "retrying",
            "max_traversals",
        ),
        (
            QUEUED_SOURCE.replace("count: 3", "count: 0"),
            "joined",
            "missing listed input",
        ),
        (
            QUEUED_SOURCE
                .replace(":repeat {count: 3}", ":conditional")
                .replace("|a:calls|", "|a:calls {when: false}|")
                .replace("context.index + 1", "1")
                .replace("context.index + 10", "10")
                .replace(":accumulate", ":quorum")
                .replace("observe: \"result\"", "observe: \"result\", count: 2"),
            "joined",
            "quorum group closed",
        ),
    ] {
        let model = lower(&source).unwrap();
        let error = data::generate(
            model.clone(),
            1,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            42,
            &(),
        )
        .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        generated_failure(&model, name, message);
    }
}

#[test]
fn cycle_groups_keep_limits_and_values_separate_inside_repetition() {
    let source = CYCLE_SOURCE.replace("output: finish.output", "output: \"done\"")
        .replace("(root)-|:calls, :feeds {value: source.input, into: target.input}|->(lookup:tool)",
            "(root) =|:repeat {count: 2}|=>[-|:calls|->(turn:task) {input: root.input}]; (turn)-|:calls, :feeds {value: source.input, into: target.input}|->(lookup:tool)")
        .replace("max_traversals: 3","max_traversals: 2");
    let batch = one_trace(&source);
    let outputs: Vec<_> = batch
        .events
        .iter()
        .filter(|event| event.name == "lookup")
        .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
        .collect();
    assert_eq!(
        outputs.iter().map(|value| value["attempt"].clone()).collect::<Vec<_>>(),
        vec![json!(0), json!(1), json!(2), json!(0), json!(1), json!(2)]
    );
    for generated in generated_runs(&lower(&source).unwrap(), "retrying") {
        assert_eq!(
            generated["spans"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|span| span["name"] == "lookup")
                .map(|span| span["output"].clone())
                .collect::<Vec<_>>(),
            outputs
        );
    }
}

#[test]
fn weighted_cycles_preserve_named_history_and_budget_failures() {
    let source = CYCLE_SOURCE
        .replace("=|:exclusive|", "=|:weighted|")
        .replace("when: source.output.retry", "weight: 1, when: source.output.retry")
        .replace("when: !source.output.retry", "weight: 1, when: !source.output.retry")
        .replace("(lookup:tool)", "(lookup:tool {name: \"patient_lookup\"})");
    let batch = one_trace(&source);
    assert_eq!(
        batch.events.last().unwrap().output.as_ref().unwrap().project(&()).unwrap(),
        json!({"attempt":2,"earlier":2})
    );
    for generated in generated_runs(&lower(&source).unwrap(), "retrying") {
        assert_eq!(generated["output"], json!({"attempt":2,"earlier":2}));
    }
    let infinite = CYCLE_SOURCE
        .replace("lookup.output.attempt < 2", "true")
        .replace(", max_traversals: 3", "");
    let model = lower(&infinite).unwrap();
    let error = data::generate(
        model.clone(),
        1,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        42,
        &(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("execution budget"), "{error}");
    generated_failure(&model, "retrying", "execution budget");
}

#[test]
fn junction_results_wait_for_destination_payload_dependencies() {
    let source = QUEUED_SOURCE
        .replace("=|:repeat {count: 3}|", "=|:parallel|")
        .replace("output: context.index + 10", "output: 10")
        .replace("output: context.index + 1", "output: child.output");
    let source = format!("{source}\n(left_task)-|:calls|->(child:tool) {{output: 1}};");
    let expected = json!([{ "edge":"b", "value":10 },{ "edge":"a", "value":1 }]);
    let batch = one_trace(&source);
    assert_eq!(
        batch
            .events
            .iter()
            .find(|event| event.name == "finish")
            .unwrap()
            .output
            .as_ref()
            .unwrap()
            .project(&())
            .unwrap(),
        expected
    );
    for generated in generated_runs(&lower(&source).unwrap(), "joined") {
        assert_eq!(
            generated["spans"]
                .as_array()
                .unwrap()
                .iter()
                .find(|span| span["name"] == "finish")
                .unwrap()["output"],
            expected
        );
    }
}

// valid in the language and unsupported by mocking: a consumer rejection names the offending element
#[test]
fn consumer_rejections_point_at_the_offending_element() {
    let fork = r#"
(route:agent) {input: "question", output: request.output};
(route)-|:next|->(request:task) {input: route.input, output: "done"};
(request)=|:fork|=>[-|:calls|->(answer:tool)];
"#;
    for (source, located, message) in [
        (
            format!("{SOURCE}\n(lookup)-|:next {{weight: 2}}|->(finish:tool) {{output: 1}};"),
            "2",
            "configured next edges are not supported by mocking yet",
        ),
        (
            format!("{SOURCE}\n(request)-|:requires|->(lookup);"),
            ":requires",
            "edge kind `requires` is not supported by Braintrust mocking",
        ),
        (
            fork.to_owned(),
            "=|:fork|=>[-|:calls|->(answer:tool)]",
            "fork kind `fork` is not supported by Braintrust mocking",
        ),
    ] {
        let errors = Bundle.compile_module(&source, false).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        let range = errors[0].r#where;
        assert_eq!(errors[0].what, message);
        assert_eq!(&source[range.start..range.end], located, "{errors:#?}");
        assert_eq!(range.source, 0);
    }
    // the probe misspellings from the graph contracts plan never reach the consumer
    for (source, located, message) in [
        (
            format!("{SOURCE}\n(lookup)-|:next {{whne: true}}|->(finish:tool) {{output: 1}};"),
            "whne",
            "unknown property `whne` for built-in kind `next`, did you mean `when`?",
        ),
        (
            format!("{SOURCE}\n(lookup)-|:nxet|->(finish:tool) {{output: 1}};"),
            "nxet",
            "unknown edge kind `nxet`, did you mean `next`?",
        ),
        (
            fork.replace(":fork", ":exclusiv"),
            "exclusiv",
            "unknown fork kind `exclusiv`, did you mean `exclusive`?",
        ),
        (
            SOURCE.replace("output: lookup.output,", "outptu: lookup.output,"),
            "outptu",
            "unknown field `outptu` for kind `task`, did you mean `output`?",
        ),
        (
            SOURCE.replace("(request:task)", "(request:tsak)"),
            "tsak",
            "unknown node kind `tsak`, did you mean `task`?",
        ),
    ] {
        let errors = Bundle.compile_module(&source, false).unwrap_err();
        let found = errors
            .iter()
            .find(|error| error.what == message)
            .unwrap_or_else(|| panic!("{errors:#?}"));
        assert_eq!(&source[found.r#where.start..found.r#where.end], located);
    }
    // a misspelled context key is a compile error at the read, with the keys available there
    let source = r#"(probe:agent) {input: null, output: null};
(probe)-|:next|->(a:task {name: "a"})
    =|:parallel|=>[
        -|one:next|->(b:task {name: "b"}),
        -|two:next|->(c:task {name: "c"}),
    ]
        >|:accumulate |one|, |two||>>(joined:task {name: "joined"}) {input: context.arivals};"#;
    let errors = Bundle.compile_module(source, false).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(
        errors[0].what,
        "context key `arivals` is not available here, did you mean `arrivals`? (available: arrivals, history, trace_index)"
    );
    let range = errors[0].r#where;
    assert_eq!(&source[range.start..range.end], "context.arivals");
}

// each span as (name, input, parent name, parent input), in emission order
fn span_parents(batch: &data::EventBatch) -> Vec<(String, Value, String, Value)> {
    let input = |event: &data::Event| event.input.as_ref().map_or(Value::Null, |value| value.project(&()).unwrap());
    (batch.events.iter())
        .map(|event| {
            let parent =
                (event.span_parents.first()).map(|id| batch.events.iter().find(|parent| &parent.span_id == id).unwrap());
            (
                event.name.clone(),
                input(event),
                parent.map_or(String::new(), |parent| parent.name.clone()),
                parent.map_or(Value::Null, input),
            )
        })
        .collect()
}

fn parent_names(batch: &data::EventBatch) -> Vec<(String, String)> {
    (span_parents(batch).into_iter())
        .map(|(name, _, parent, _)| (name, parent))
        .collect()
}

fn names(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, parent)| ((*name).to_owned(), (*parent).to_owned()))
        .collect()
}

#[test]
fn calls_parents_each_occurrence_of_a_repeated_step() {
    // a static plan: every reply sits under the turn of its own iteration
    let repeated = r#"
(chat:agent) {input: "hi", output: request.output};
(chat)-|:next|->(request:task) {input: chat.input, output: "done"};
(request)
    =|:repeat {count: 3}|=>[
        -|:calls|->(turn:task) {input: context.index, output: "turn"},
    ]
        ->(turn)-|:calls|->(reply:llm) {input: turn.input, output: "reply"};
"#;
    let spans = span_parents(&one_trace(repeated));
    assert_eq!(spans.len(), 7);
    for index in 0..3 {
        assert!(spans.contains(&("turn".into(), json!(index), "request".into(), json!("hi"))));
        assert!(spans.contains(&("reply".into(), json!(index), "turn".into(), json!(index))));
    }
    // each iterates over items the same way
    let iterated = r#"
(chat:agent) {input: "hi", output: request.output};
(chat)-|:next|->(request:task) {input: chat.input, output: "done", local: {files: ["a.txt", "b.txt"]}};
(request)
    =|:each {items: request.local.files}|=>[
        -|:calls|->(file:task) {input: context.item, output: "read"},
    ]
        ->(file)-|:calls|->(parse:tool) {input: file.input, output: "parsed"};
"#;
    let spans = span_parents(&one_trace(iterated));
    assert_eq!(spans.len(), 5);
    for item in ["a.txt", "b.txt"] {
        assert!(spans.contains(&("file".into(), json!(item), "request".into(), json!("hi"))));
        assert!(spans.contains(&("parse".into(), json!(item), "file".into(), json!(item))));
    }
    // a reduce junction on the rounds: each probe sits under its own round and the fold follows the loop
    let flowing = r#"
(chat:agent) {input: "hi", output: closing.output};
(chat)-|:next|->(request:task) {input: chat.input, output: closing.output};
(request)
    =|:repeat {count: 3}|=>[
        -|rounds:calls|->(inspect:task) {input: context.index, output: context.index},
    ]
        ->(inspect)-|:calls|->(probe:tool) {input: inspect.input, output: "probed"}
        >|:reduce {observe: "result", initial: [], combine: [...context.accumulator, context.arrival.value]} |rounds||>>(closing:llm) {
            input: context.accumulator,
            output: "summary",
        };
"#;
    let model = lower(flowing).unwrap();
    let spans = span_parents(&one_trace(flowing));
    assert_eq!(spans.len(), 8);
    for index in 0..3 {
        assert!(spans.contains(&("inspect".into(), json!(index), "request".into(), json!("hi"))));
        assert!(spans.contains(&("probe".into(), json!(index), "inspect".into(), json!(index))));
    }
    assert!(spans.contains(&("closing".into(), json!([0, 1, 2]), "request".into(), json!("hi"))));
    // data generation and generated code run the rounds in the same order
    let order: Vec<_> = spans
        .iter()
        .map(|(name, input, _, _)| (name.clone(), input.clone()))
        .collect();
    for details in generated_runs(&model, "chat") {
        let code: Vec<_> = (details["spans"].as_array().unwrap().iter())
            .map(|span| (span["name"].as_str().unwrap().to_owned(), span["input"].clone()))
            .collect();
        assert_eq!(code, order);
    }
}

#[test]
fn next_from_a_nested_task_returns_to_its_level() {
    let source = r#"
(chat:agent) {input: "hi", output: request.output};
(chat)-|:next|->(request:task) {input: chat.input, output: summary.output};
(request)-|:calls|->(plan:llm) {input: request.input, output: "plan"}
    -|:next|->(research:task) {input: plan.output, output: read.output}
        -|:calls|->(search:tool) {input: research.input, output: "hits"}
            -|:next|->(read:tool) {input: search.output, output: "notes"};
(research)-|:next|->(summary:llm) {input: read.output, output: "summary"};
"#;
    assert_eq!(
        parent_names(&one_trace(source)),
        names(&[
            ("request", ""),
            ("plan", "request"),
            ("research", "request"),
            ("search", "research"),
            ("read", "research"),
            ("summary", "request"),
        ])
    );
    // the steps inside a task are declared before the step that follows it
    let reordered = source.replace(
        "(chat)-|:next|->(request:task) {input: chat.input, output: summary.output};",
        "(chat)-|:next|->(request:task) {input: chat.input, output: summary.output};\n(research)-|:next|->(summary);",
    );
    let reordered = reordered.replace(
        "(research)-|:next|->(summary:llm) {input: read.output, output: \"summary\"};",
        "(summary:llm) {input: read.output, output: \"summary\"};",
    );
    assert!(
        lower(&reordered)
            .unwrap_err()
            .contains("progression reenters a completed parent span")
    );
}

#[test]
fn a_junction_target_takes_the_parent_its_inputs_carry() {
    let source = r#"
(chat:agent) {input: "hi", output: synthesis.output};
(chat)-|:next|->(request:task) {input: chat.input, output: synthesis.output};
(request)-|:calls|->(delegate:llm) {input: request.input, output: "split"}
    =|:parallel|=>[
        -|a:next|->(metrics:task) {input: "pull metrics", output: metrics_reply.output},
        -|b:next|->(diff:task) {input: "diff releases", output: diff_reply.output},
    ]
        ->(metrics)-|:calls|->(metrics_reply:llm) {input: metrics.input, output: "p95 up"}
        ->(diff)-|:calls|->(diff_reply:llm) {input: diff.input, output: "new check"}
        >|:accumulate {observe: "result"} |a|, |b||>>(synthesis:llm) {input: context.arrivals, output: "answer"};
"#;
    assert_eq!(
        parent_names(&one_trace(source)),
        names(&[
            ("request", ""),
            ("delegate", "request"),
            ("metrics", "request"),
            ("metrics_reply", "metrics"),
            ("diff", "request"),
            ("diff_reply", "diff"),
            ("synthesis", "request"),
        ])
    );
    // inputs that disagree are rejected at the junction
    let disagreeing = source.replace("-|a:next|->", "-|a:calls|->");
    let errors = Bundle.compile_module(&disagreeing, false).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(
        errors[0].what,
        "junction inputs `a` and `b` give `synthesis` different parent spans"
    );
    let range = errors[0].r#where;
    assert!(
        disagreeing[range.start..range.end].starts_with(">|:accumulate"),
        "{errors:#?}"
    );
}

#[test]
fn agents_share_a_step_under_their_own_parents() {
    let source = r#"
(first:agent) {input: "one", output: first_request.output};
(second:agent) {input: "two", output: second_request.output};
(first)-|:next|->(first_request:task) {input: first.input, output: "first done"};
(second)-|:next|->(second_request:task) {input: second.input, output: "second done"};
(first_request)-|:calls|->(lookup:tool) {input: "key", output: "value"};
(second_request)-|:calls|->(lookup);
"#;
    let model = lower(source).unwrap();
    let mut seen = Vec::new();
    for trace in 0..4 {
        let batch = data::generate(
            model.clone(),
            4,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            42 + trace,
            &(),
        )
        .unwrap();
        seen.extend(parent_names(&batch));
    }
    for pair in names(&[("lookup", "first_request"), ("lookup", "second_request")]) {
        assert!(seen.contains(&pair), "{pair:?} in {seen:?}");
    }
    assert!(
        seen.iter()
            .all(|(name, parent)| name != "lookup" || parent.ends_with("_request"))
    );
}

#[test]
fn a_step_after_a_rounds_step_runs_in_every_round() {
    // act follows think on its level, so each round runs both and act can read the round
    let source = r#"(loop:agent) {input: "x", output: request.output};
(loop)-|:next|->(request:task) {input: loop.input, output: "done"};
(request)
    =|:repeat {count: 2}|=>[
        -|:calls|->(think:llm) {input: context.index, output: "thought"},
    ]
        ->(think)-|:next|->(act:tool) {input: {round: context.index, thought: think.input}, output: "acted"};
"#;
    let expected = [
        ("request", json!("x")),
        ("think", json!(0)),
        ("act", json!({"round": 0, "thought": 0})),
        ("think", json!(1)),
        ("act", json!({"round": 1, "thought": 1})),
    ]
    .map(|(name, input)| (name.to_owned(), input));
    let batch = one_trace(source);
    let spans = span_parents(&batch);
    let data: Vec<_> = spans
        .iter()
        .map(|(name, input, _, _)| (name.clone(), input.clone()))
        .collect();
    assert_eq!(data, expected);
    assert!(
        spans.iter().skip(1).all(|(_, _, parent, _)| parent == "request"),
        "every step is a child of the request"
    );
    for details in generated_runs(&lower(source).unwrap(), "loop") {
        let code: Vec<_> = (details["spans"].as_array().unwrap().iter())
            .map(|span| (span["name"].as_str().unwrap().to_owned(), span["input"].clone()))
            .collect();
        assert_eq!(code, expected);
    }
}

#[test]
fn definitions_supply_context_values_to_data_and_code() {
    // the agent supplies its tenant to every step, and each turn supplies its own ticket to its reply
    let source = r#"
+(tenant_scoped:trait {tenant: {type: "string", required: true}})
    -|:on|->(node)
    -|:provides {tenant: {type: "string", value: self.props.tenant}}|->(context);
+(scoped_agent:node)
    -|:extends|->(agent)
    -|:has|->(tenant_scoped);
+(ticketed:trait)
    -|:on|->(node)
    -|:provides {ticket: {type: "object", value: {id: self.input, round: context.index}}}|->(context);
+(ticket_task:node)
    -|:extends|->(task)
    -|:has|->(ticketed);

(support:scoped_agent {tenant: "acme"}) {input: "refund", output: request.output};
(support)-|:next|->(request:task) {input: support.input, output: context.tenant};
(request)
    =|:repeat {count: 2}|=>[
        -|:calls|->(turn:ticket_task) {input: "T-${context.index}", output: "handled"},
    ]
        ->(turn)-|:calls|->(reply:llm) {input: {tenant: context.tenant, ticket: context.ticket}, output: "ok"};
"#;
    let reply = |round: i64| json!({"tenant": "acme", "ticket": {"id": format!("T-{round}"), "round": round}});
    let expected = [
        ("request", json!("refund"), json!("acme")),
        ("turn", json!("T-0"), json!("handled")),
        ("reply", reply(0), json!("ok")),
        ("turn", json!("T-1"), json!("handled")),
        ("reply", reply(1), json!("ok")),
    ]
    .map(|(name, input, output)| (name.to_owned(), input, output));
    let batch = one_trace(source);
    let data: Vec<_> = (batch.events.iter())
        .map(|event| {
            let project = |value: &Option<data::Value>| value.as_ref().map_or(Value::Null, |value| value.project(&()).unwrap());
            (event.name.clone(), project(&event.input), project(&event.output))
        })
        .collect();
    assert_eq!(data, expected);
    let model = lower(source).unwrap();
    for details in generated_runs(&model, "support") {
        let code: Vec<_> = (details["spans"].as_array().unwrap().iter())
            .map(|span| {
                (
                    span["name"].as_str().unwrap().to_owned(),
                    span["input"].clone(),
                    span["output"].clone(),
                )
            })
            .collect();
        assert_eq!(code, expected);
    }

    // a key that no definition gives a value is still a located consumer error
    let unsupplied = source.replace(", value: self.props.tenant", "");
    assert_eq!(
        lower(&unsupplied).unwrap_err(),
        "context key `tenant` is not supplied by Braintrust mocking"
    );
    // two suppliers on two paths leave the reader without one value
    let ambiguous = r#"
+(assigned:edge {direction: "directed"})
    -|:extends|->(calls)
    -|:provides {owner: {type: "string", value: target.input}}|->(context);
(pair:agent) {input: "x", output: request.output};
(pair)-|:next|->(request:task) {input: pair.input, output: "done"};
(request)
    =|:parallel|=>[
        -|a:assigned|->(first_owner:task) {input: "ann", output: "l"},
        -|b:assigned|->(second_owner:task) {input: "bo", output: "r"},
    ]
        >|:accumulate {observe: "result"} |a|, |b||>>(finish:task) {input: context.owner, output: "f"};
"#;
    assert_eq!(
        lower(ambiguous).unwrap_err(),
        "`context.owner` reaches this step from more than one supplier"
    );
}

// the name and output of every span, in the order the spans run
#[test]
fn kinds_compute_fields_for_data_and_code() {
    // a kind states its output once, an extension restates it, and the last step writes its own
    let source = r#"
+(clean_query:node) {output: {type: "string", value: lower(trim(self.input))}}
    -|:extends|->(tool);
+(shout_query:node) {output: {value: upper(trim(self.input))}}
    -|:extends|->(clean_query);

(help:agent) {input: "  Rotate MY Keys ", output: request.output};
(help)-|:next|->(request:task) {input: help.input, output: last.output};
(request)-|:calls|->(clean:clean_query) {input: request.input}
    -|:next|->(loud:shout_query) {input: request.input}
        -|:next|->(last:clean_query) {input: loud.output, output: "written"};
"#;
    let spans = |question: &str| {
        [
            ("request", json!(question), json!("written")),
            ("clean", json!(question), json!(question.trim().to_lowercase())),
            ("loud", json!(question), json!(question.trim().to_uppercase())),
            ("last", json!(question.trim().to_uppercase()), json!("written")),
        ]
        .map(|(name, input, output)| (name.to_owned(), input, output))
    };
    let batch = one_trace(source);
    let data: Vec<_> = (batch.events.iter())
        .map(|event| {
            let project = |value: &Option<data::Value>| value.as_ref().map_or(Value::Null, |value| value.project(&()).unwrap());
            (event.name.clone(), project(&event.input), project(&event.output))
        })
        .collect();
    assert_eq!(data, spans("  Rotate MY Keys "));
    let model = lower(source).unwrap();
    let code = |details: &Value| -> Vec<_> {
        (details["spans"].as_array().unwrap().iter())
            .map(|span| {
                (
                    span["name"].as_str().unwrap().to_owned(),
                    span["input"].clone(),
                    span["output"].clone(),
                )
            })
            .collect()
    };
    for details in generated_runs(&model, "help") {
        assert_eq!(code(&details), spans("  Rotate MY Keys "));
    }
    // the field is computed when the agent runs, from the input its caller supplies
    let python = execute_generated(
        &model,
        ScorerLang::Python,
        "python3",
        "check.py",
        "from help_agent import run_details\nimport json\nprint(json.dumps(run_details(' New QUESTION  ')))",
    );
    let typescript = execute_generated(
        &model,
        ScorerLang::Typescript,
        "node",
        "check.mts",
        "import {runDetails} from './help_agent.mts'; console.log(JSON.stringify(runDetails(' New QUESTION  ')));",
    );
    for details in [python, typescript] {
        assert_eq!(code(&details), spans(" New QUESTION  "));
    }
    // no step function shows that a value came from its kind
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let written = source.replace(
            "(clean:clean_query) {input: request.input}",
            "(clean:clean_query) {input: request.input, output: lower(trim(self.input))}",
        );
        assert_ne!(written, source);
        let files = |source: &str| -> Vec<_> {
            let assembly = code::assemble_agents(&lower(source).unwrap(), lang, &CodeTarget::new("local-test")).unwrap();
            assembly.files.into_iter().map(|file| (file.name, file.contents)).collect()
        };
        assert_eq!(files(source), files(&written));
    }
}

#[test]
fn generated_agents_count_the_tokens_of_their_llm_steps() {
    // the tokenizer stand-in counts words, so these are not real token counts
    let source = r#"(chat:agent) {input: "one two three", output: cached.output};
(chat)-|:next|->(request:task) {input: chat.input, output: cached.output};
(request)-|:calls|->(bare:llm)
    -|:next|->(timed:llm) {input: request.input, output: "four five", metrics: {time_to_first_token: 0.25}}
        -|:next|->(cached:llm) {input: request.input, output: "four five", metrics: {prompt_tokens: 0}};
"#;
    let model = lower(source).unwrap();
    for details in generated_runs(&model, "chat") {
        let metrics: Vec<_> = (details["spans"].as_array().unwrap().iter())
            .map(|span| (span["name"].as_str().unwrap().to_owned(), span["metrics"].clone()))
            .collect();
        let expected = [
            ("request", Value::Null),
            ("bare", json!({"prompt_tokens": 0, "completion_tokens": 0, "tokens": 0})),
            (
                "timed",
                json!({"prompt_tokens": 3, "completion_tokens": 2, "tokens": 5, "time_to_first_token": 0.25}),
            ),
            ("cached", json!({"prompt_tokens": 0, "completion_tokens": 2, "tokens": 2})),
        ]
        .map(|(name, metrics)| (name.to_owned(), metrics));
        assert_eq!(metrics, expected);
    }
}

fn outputs(batch: &data::EventBatch) -> Vec<Value> {
    (batch.events.iter())
        .map(|event| json!([event.name, event.output.as_ref().map(|output| output.project(&()).unwrap())]))
        .collect()
}

#[test]
fn loops_rounds_and_keyed_junctions_agree_in_data_and_code() {
    // a loop holds a fork that repeats, and each round of the loop runs every round of the fork
    let rounds_in_a_loop = r#"
(looping:agent) {input: 0, output: last.output};
(looping)-|:next|->(root:task) {input: looping.input};
(root)-|:calls|->(turn:task) {output: len(context.history.turn)}
    =|:repeat {count: 2}|=>[-|:calls|->(work:tool) {output: context.index + turn.output * 10}];
(turn)~|:next {when: source.output < 2, max_traversals: 5}|~>(turn);
(turn)-|:next {when: source.output >= 2}|->(last:task) {output: turn.output};
"#;
    // a correlated reduce closes one group for each key, in the order the keys first arrive
    let keyed_reduce = r#"
(looping:agent) {input: 0, output: "done"};
(looping)-|:next|->(root:task) {input: looping.input};
(root) =|:each {items: ["x", "y", "x"]}|=>[
    -|a:calls|->(part:tool) {output: context.index + 1},
]
    >|:reduce {observe: "result", correlate: context.item, initial: 0, combine: context.accumulator + context.arrival.value} |a||>>(finish:task) {
        output: {total: context.accumulator, arrivals: context.arrivals},
    };
"#;
    // a junction without a fork joins the edges that leave one step
    let joined_edges = r#"
(looping:agent) {input: 0, output: finish.output};
(looping)-|:next|->(root:task) {input: looping.input};
(root)-|x:calls|->(one:tool) {output: 1};
(root)-|y:calls|->(two:tool) {output: 2};
>|:accumulate {observe: "result"} |x|, |y||>>(finish:task) {output: context.arrivals};
"#;
    for (source, expected) in [
        (
            rounds_in_a_loop,
            json!([
                ["root", null],
                ["turn", 0],
                ["work", 0],
                ["work", 1],
                ["turn", 1],
                ["work", 10],
                ["work", 11],
                ["turn", 2],
                ["work", 20],
                ["work", 21],
                ["last", 2],
            ]),
        ),
        (
            keyed_reduce,
            json!([
                ["root", null],
                ["part", 1], ["part", 2], ["part", 3],
                ["finish", {"total": 4, "arrivals": [{"edge": "a", "value": 1}, {"edge": "a", "value": 3}]}],
                ["finish", {"total": 2, "arrivals": [{"edge": "a", "value": 2}]}],
            ]),
        ),
        (
            joined_edges,
            json!([
                ["root", null],
                ["one", 1],
                ["two", 2],
                ["finish", [{"edge": "x", "value": 1}, {"edge": "y", "value": 2}]],
            ]),
        ),
    ] {
        assert_eq!(json!(outputs(&one_trace(source))), expected, "{source}");
        for generated in generated_runs(&lower(source).unwrap(), "looping") {
            let spans: Vec<_> = (generated["spans"].as_array().unwrap().iter())
                .map(|span| json!([span["name"], span.get("output").cloned().unwrap_or(Value::Null)]))
                .collect();
            assert_eq!(json!(spans), expected, "{source}");
        }
    }
}

// a shape the nested plan cannot hold is refused at the element that causes it
#[test]
fn shapes_without_a_nested_form_are_refused_with_a_location() {
    let base = r#"
(looping:agent) {input: 0, output: "done"};
(looping)-|:next|->(root:task) {input: looping.input};
(root)-|:calls|->(first:tool) {output: 1};
(first)-|:next|->(second:tool) {output: 2};
"#;
    for (source, located, message) in [
        (
            format!("{base}(root)-|:calls|->(other:tool) {{output: 3}};\n(other)~|:next {{when: false}}|~>(first);"),
            ":next {when: false}",
            "a loop-back must return to a step that leads to its source",
        ),
        (
            format!("{base}(second)~|:next {{when: false}}|~>(first);\n(second)~|retry:next {{when: false}}|~>(first);"),
            "retry:next {when: false}",
            "more than one loop-back to one step is not supported by mocking yet",
        ),
        (
            format!("{base}(second)~|:next {{when: false}}|~>(root);"),
            ":next {when: false}",
            "cycle edges cannot target the trace entry span",
        ),
        (
            base.replace(
                "(first)-|:next|->(second:tool) {output: 2};",
                "(root) =|:repeat {count: 2}|=>[-|:calls|->(second:tool) {output: 2}];\n(root)-|:calls|->(reader:tool) {output: second.output};",
            ),
            "(second:tool) {output: 2}",
            "`second` runs once for each round of its fork, read it from a step in the round or through a junction",
        ),
    ] {
        let errors = Bundle.compile_module(&source, false).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        let range = errors[0].r#where;
        assert_eq!(errors[0].what, message, "{source}");
        assert_eq!(&source[range.start..range.end], located, "{errors:#?}");
    }
}

#[test]
fn the_world_example_agrees_in_data_and_code() {
    // the policies, the flight and what the traveler asks are built beside the agent, and none of it emits a span
    let source = include_str!("../../../examples/flight_change.bt");
    let model = lower(source).unwrap();
    // the last seed is past what a TypeScript number holds exactly
    let seeds: Vec<u64> = (1..=12).chain([u64::MAX]).collect();
    let project = |value: &Option<data::Value>| value.as_ref().map_or(Value::Null, |value| value.project(&()).unwrap());
    // the spans of the first trace of each seed, as an agent reports them
    let mut traces = Vec::new();
    for seed in &seeds {
        let batch = data::generate(
            model.clone(),
            1,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            *seed,
            &(),
        )
        .unwrap();
        let names: Vec<_> = batch.events.iter().map(|event| event.name.as_str()).collect();
        assert_eq!(names, ["flight-change", "search_flights", "Chat Completion"]);
        let spans: Vec<_> = (batch.events.iter())
            .map(|event| (project(&event.input), project(&event.output)))
            .collect();
        // the fee the policies state is the fee the reply quotes, and the flight the tool returns is the one it describes
        let policies = spans[2].0[0]["content"].as_str().unwrap();
        let reply = spans[2].1["content"].as_str().unwrap();
        let fee = if policies.contains("costs 75 USD") {
            "75 USD"
        } else {
            "50 USD"
        };
        assert!(policies.starts_with(&format!(
            "Follow the change policies. A same-day change costs {fee}. The change window"
        )));
        assert!(reply.starts_with(&format!(
            "I found a later flight, and a same-day change costs {fee}. It leaves on November 20, 2026 at "
        )));
        let departs = if spans[1].1["departs"] == "17:30" {
            "5:30 PM"
        } else {
            "7:05 PM"
        };
        assert!(
            reply.contains(&format!(" at {departs}. The fare difference must be paid")),
            "{reply}"
        );
        assert_eq!(spans[0].1, reply);
        // the traveler writes in lower case
        let asked = spans[2].0[1]["content"].as_str().unwrap();
        assert_eq!(asked, asked.to_lowercase());
        traces.push(spans);
    }
    let asked: std::collections::BTreeSet<_> = traces.iter().map(|spans| spans[2].0[1]["content"].to_string()).collect();
    let departures: std::collections::BTreeSet<_> = traces.iter().map(|spans| spans[1].1["departs"].to_string()).collect();
    assert!(asked.len() > 3, "{asked:?}");
    assert_eq!(departures.len(), 2);
    // a generated agent given a seed builds the world of that seed's first trace
    let runs = [
        execute_generated(
            &model,
            ScorerLang::Python,
            "python3",
            "check.py",
            &format!(
                "from flight_change_agent import run_details\nimport json\nprint(json.dumps([run_details(seed=seed) for seed in {seeds:?}]))"
            ),
        ),
        execute_generated(
            &model,
            ScorerLang::Typescript,
            "node",
            "check.mts",
            &format!(
                "import {{runDetails}} from './flight_change_agent.mts'; console.log(JSON.stringify([{}].map(seed => runDetails(undefined, Math.random, seed))));",
                (seeds.iter().map(|seed| format!("{seed}n")).collect::<Vec<_>>()).join(", ")
            ),
        ),
    ];
    for run in &runs {
        for (details, trace) in run.as_array().unwrap().iter().zip(&traces) {
            let spans = details["spans"].as_array().unwrap();
            let names: Vec<_> = spans.iter().map(|span| span["name"].as_str().unwrap()).collect();
            assert_eq!(names, ["flight-change", "search_flights", "Chat Completion"]);
            for (span, (input, output)) in spans.iter().zip(trace) {
                assert_eq!(&span["input"], input);
                assert_eq!(&span["output"], output);
            }
            assert_eq!(details["output"], trace[2].1["content"]);
        }
    }
    assert_eq!(runs[0], runs[1]);
    // an agent given a trace as well builds the world of that trace
    let later = data::generate(
        model.clone(),
        4,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        7,
        &(),
    )
    .unwrap();
    let fourth = project(&later.events[3 * 3 + 2].output)["content"].clone();
    let python = execute_generated(
        &model,
        ScorerLang::Python,
        "python3",
        "check.py",
        "from flight_change_agent import run_details\nimport json\nprint(json.dumps(run_details(seed=7, trace=3)))",
    );
    let typescript = execute_generated(
        &model,
        ScorerLang::Typescript,
        "node",
        "check.mts",
        "import {runDetails} from './flight_change_agent.mts'; console.log(JSON.stringify(runDetails(undefined, Math.random, 7, 3)));",
    );
    assert_eq!(python["output"], fourth);
    assert_eq!(typescript["output"], fourth);
    // without a seed the agent still builds one of the worlds the graph allows
    for details in generated_runs(&model, "flight_change") {
        let reply = details["output"].as_str().unwrap();
        assert!(
            reply.starts_with("I found a later flight, and a same-day change costs "),
            "{reply}"
        );
    }
    // the world leaves no function, deferred value or graph behind, and the agent takes a seed and a trace
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        for file in assembly.files.iter().filter(|file| !file.name.contains("runtime")) {
            for told in ["chance(", "included", "fee(", "FeeResult", "_lazy", "policies(", "ask("] {
                assert!(!file.contents.contains(told), "{told} in {}", file.name);
            }
        }
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("flight_change_agent.") && !file.name.contains("runtime"))
            .unwrap();
        let seeded = match lang {
            ScorerLang::Python => "ExecutionContext(logger, seeded(random_source, seed, trace))",
            ScorerLang::Typescript => "new ExecutionContext(logger, seeded(randomSource, seed, trace))",
        };
        assert!(agent.contents.contains(seeded), "{}", agent.contents);
    }
    // an agent that walks nothing takes no seed
    let plain = lower(include_str!("../../../examples/computed_fields.bt")).unwrap();
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble_agents(&plain, lang, &CodeTarget::new("local-test")).unwrap();
        assert!(assembly.files.iter().all(|file| !file.contents.contains("seed")));
    }
}

#[test]
fn a_walk_drawn_for_more_than_a_trace_agrees_in_data_and_code() {
    // the desk is drawn once for the run and the mood once for every three traces
    let source = r#"(desk:sentence {per: "run"})
    -|:next|->(:part {subject: "the desk"})
    =|:weighted|=>[
        -|:next {weight: 1}|->(:part {verb: "open", complement: "at nine"}),
        -|:next {weight: 1}|->(:part {verb: "open", complement: "at ten"}),
        -|:next {weight: 1}|->(:part {verb: "close", complement: "at noon"}),
    ];
(mood:sentence {per: floor(context.trace_index / 3)})
    -|:next|->(:part {subject: "I", person: "first"})
    =|:weighted|=>[
        -|:next {weight: 1}|->(:part {verb: "wait"}),
        -|:next {weight: 1}|->(:part {verb: "leave"}),
        -|:next {weight: 1}|->(:part {verb: "stay"}),
    ];
(desk_hours:agent) {input: "hi", output: t.output};
(desk_hours)-|:next|->(t:task {name: "t"}) {input: desk_hours.input, output: [desk.text, mood.text]};
"#;
    let model = lower(source).unwrap();
    for seed in [1u64, 2, 3, 4, 5, 6] {
        let batch = data::generate(
            model.clone(),
            6,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            seed,
            &(),
        )
        .unwrap();
        let said: Vec<_> = (batch.events.iter())
            .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
            .collect();
        assert!(said.iter().all(|turn| turn[0] == said[0][0]), "{said:?}");
        assert!(said[..3].iter().all(|turn| turn[1] == said[0][1]), "{said:?}");
        assert!(said[3..].iter().all(|turn| turn[1] == said[3][1]), "{said:?}");
        // an agent given the seed is the first trace of the run
        let python = execute_generated(
            &model,
            ScorerLang::Python,
            "python3",
            "check.py",
            &format!("from desk_hours_agent import run_details\nimport json\nprint(json.dumps(run_details(seed={seed})))"),
        );
        let typescript = execute_generated(
            &model,
            ScorerLang::Typescript,
            "node",
            "check.mts",
            &format!(
                "import {{runDetails}} from './desk_hours_agent.mts'; console.log(JSON.stringify(runDetails(undefined, Math.random, {seed})));"
            ),
        );
        assert_eq!(python["output"], said[0]);
        assert_eq!(typescript["output"], said[0]);
    }
}

#[test]
fn every_piece_of_a_world_agrees_in_data_and_code() {
    // a walk inside a walk, a fact, entities and a relation, a persona, a repeat and a read of the agent's input
    let model = lower(include_str!("fixtures/world.bt")).unwrap();
    let seeds = [1u64, 2, 3, 4, 5, 6];
    let outputs: Vec<Value> = (seeds.iter())
        .map(|seed| {
            let batch = data::generate(
                model.clone(),
                1,
                Duration::from_secs(3600),
                data::Distribution::Linear,
                UNIX_EPOCH + Duration::from_secs(7200),
                *seed,
                &(),
            )
            .unwrap();
            batch.events[0].output.as_ref().unwrap().project(&()).unwrap()
        })
        .collect();
    for output in &outputs {
        // each part of the world is read the same way wherever it is read
        assert!(
            output["policies"]
                .as_str()
                .unwrap()
                .starts_with(output["fee"].as_str().unwrap())
        );
        assert_eq!(output["holds"], json!([output["flight"]]));
        assert_eq!(output["who"], "Maya");
        assert_eq!(output["said"], json!([output["ask"]]));
        assert_eq!(output["ask"], "can i change my flight?");
        assert_eq!(output["reasons"].as_array().unwrap().len(), 3);
    }
    let distinct = |key: &str| {
        outputs
            .iter()
            .map(|output| output[key].to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    assert!(distinct("fee") > 1 && distinct("fare") > 1 && distinct("flight") > 1 && distinct("reasons") > 1);
    let python = execute_generated(
        &model,
        ScorerLang::Python,
        "python3",
        "check.py",
        &format!("from desk_agent import run\nimport json\nprint(json.dumps([run(seed=seed) for seed in {seeds:?}]))"),
    );
    let typescript = execute_generated(
        &model,
        ScorerLang::Typescript,
        "node",
        "check.mts",
        &format!(
            "import {{run}} from './desk_agent.mts'; console.log(JSON.stringify({seeds:?}.map(seed => run(undefined, Math.random, seed))));"
        ),
    );
    assert_eq!(python, Value::Array(outputs.clone()));
    assert_eq!(typescript, Value::Array(outputs));
    // nothing of the world is deferred, and its helpers print under readable names
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("desk_agent.") && !file.name.contains("runtime"))
            .unwrap();
        for told in ["_lazy", "_world"] {
            assert!(!agent.contents.contains(told), "{told} in {}", agent.contents);
        }
    }
}

#[test]
fn a_way_of_talking_that_samples_agrees_in_data_and_code() {
    // typos and a wording that may or may not apply are drawn by the seed, like a fork
    let source = r#"+(sloppy:trait)
    -|:on|->(node)
    -|:provides {speaker: {type: "object", value: {wording: {style: "text_message", probability: 0.6, typos: {probability: 0.7, max_errors: 2}}}}}|->(context);
+(texter:node) {said: {type: "array", value: context.steps}, gives: {type: "array", value: self.said}}
    -|:extends|->(walk)
    -|:has|->(sloppy);
(maya:texter)
    -|:next|->(one:sentence)
        -|:next|->(:part {subject: "I", person: "first", modal: "can", form: "question", verb: "change", complement: "my flight because the meeting moved"})
    -|:next|->(two:sentence)
        -|:next|->(:part {subject: "the people", number: "plural", verb: "think", complement: "that the ticket was really expensive"});
(texts:agent) {input: "hi", output: t.output};
(texts)-|:next|->(t:task {name: "t"}) {input: texts.input, output: maya.said};
"#;
    let model = lower(source).unwrap();
    let seeds: Vec<u64> = (1..=16).collect();
    let said: Vec<Value> = (seeds.iter())
        .map(|seed| {
            let batch = data::generate(
                model.clone(),
                1,
                Duration::from_secs(3600),
                data::Distribution::Linear,
                UNIX_EPOCH + Duration::from_secs(7200),
                *seed,
                &(),
            )
            .unwrap();
            batch.events[0].output.as_ref().unwrap().project(&()).unwrap()
        })
        .collect();
    // the sampling shows: not every seed says it the same way
    let ways: std::collections::BTreeSet<_> = said.iter().map(Value::to_string).collect();
    assert!(ways.len() > 4, "{ways:?}");
    let python = execute_generated(
        &model,
        ScorerLang::Python,
        "python3",
        "check.py",
        &format!("from texts_agent import run\nimport json\nprint(json.dumps([run(seed=seed) for seed in {seeds:?}]))"),
    );
    let typescript = execute_generated(
        &model,
        ScorerLang::Typescript,
        "node",
        "check.mts",
        &format!(
            "import {{run}} from './texts_agent.mts'; console.log(JSON.stringify({seeds:?}.map(seed => run(undefined, Math.random, seed))));"
        ),
    );
    assert_eq!(python, Value::Array(said.clone()));
    assert_eq!(typescript, Value::Array(said));
}

#[test]
fn a_series_agrees_in_data_and_code() {
    // an agent given the seed and a trace is that trace of the run, so it can be any turn of any conversation
    // each conversation runs for two to five turns
    // short conversations are the likeliest
    let source = r#"(chat:series {turns: 2, up_to: 5, weights: [4, 3, 2, 1]});
(mood:sentence {per: chat.number})
    -|:next|->(:part {subject: "I", person: "first"})
    =|:weighted|=>[
        -|:next {weight: 1}|->(:part {verb: "wait"}),
        -|:next {weight: 1}|->(:part {verb: "leave"}),
        -|:next {weight: 1}|->(:part {verb: "stay"}),
    ];
(desk:agent) {input: "hi", output: t.output};
(desk)-|:next|->(t:task {name: "turn"}) {
    input: desk.input,
    output: {id: chat.id, turn: chat.turn, length: chat.length, first: chat.first, last: chat.last, mood: mood.text},
    metadata: {conversation_id: chat.id},
};
"#;
    let model = lower(source).unwrap();
    let mut lengths = std::collections::BTreeSet::new();
    let mut later = 0;
    for seed in [1u64, 2, 3, u64::MAX] {
        let batch = data::generate(
            model.clone(),
            12,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            seed,
            &(),
        )
        .unwrap();
        let traces: Vec<_> = (batch.events.iter())
            .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
            .collect();
        let identifier = |event: &data::Event| event.metadata.as_ref().unwrap()["conversation_id"].project(&()).unwrap();
        for (event, trace) in batch.events.iter().zip(&traces) {
            assert_eq!(identifier(event), trace["id"]);
            lengths.insert(trace["length"].as_u64().unwrap());
            later += usize::from(trace["turn"] != 0);
        }
        let python = execute_generated(
            &model,
            ScorerLang::Python,
            "python3",
            "check.py",
            &format!(
                "from desk_agent import run_details\nimport json\nprint(json.dumps([run_details(seed={seed}, trace=trace) for trace in range(12)]))"
            ),
        );
        let typescript = execute_generated(
            &model,
            ScorerLang::Typescript,
            "node",
            "check.mts",
            &format!(
                "import {{runDetails}} from './desk_agent.mts'; console.log(JSON.stringify([...Array(12).keys()].map(trace => runDetails(undefined, Math.random, {seed}n, trace))));"
            ),
        );
        for (index, trace) in traces.iter().enumerate() {
            assert_eq!(&python[index]["output"], trace, "python, seed {seed}, trace {index}");
            assert_eq!(&typescript[index]["output"], trace, "typescript, seed {seed}, trace {index}");
            assert_eq!(python[index]["spans"][0]["metadata"]["conversation_id"], trace["id"]);
            assert_eq!(typescript[index]["spans"][0]["metadata"]["conversation_id"], trace["id"]);
        }
    }
    assert!(later > 12, "{later}");
    assert!(lengths.len() > 1, "{lengths:?}");
    // what the conversation computes prints under readable names
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("desk_agent.") && !file.name.contains("runtime"))
            .unwrap();
        for told in ["_floor", "_world", "_lazy"] {
            assert!(!agent.contents.contains(told), "{told} in {}", agent.contents);
        }
    }
}

#[test]
fn an_agent_input_that_reads_a_series_agrees_in_data_and_code() {
    // what the caller would say is part of the world, so the authored input reads the turn and a walk drawn for the series
    // the entry reads the series again after its input, which is where the series is sure to be read
    let source = r#"(chat:series {turns: 2, up_to: 4});
(caller:entity {per: chat.number})
    =|:weighted|=>[
        -|:next {weight: 1}|->(:given {value: {name: "Ada"}}),
        -|:next {weight: 1}|->(:given {value: {name: "Bo"}}),
        -|:next {weight: 1}|->(:given {value: {name: "Cy"}}),
    ];
(desk:agent) {input: {name: caller.attributes.name, turn: chat.turn}, output: t.output};
(desk)-|:next|->(t:task {name: "turn"}) {
    input: desk.input,
    output: t.input.name,
    metadata: {conversation_id: chat.id, name: caller.attributes.name},
};
"#;
    let model = lower(source).unwrap();
    for seed in [1u64, 2] {
        let batch = data::generate(
            model.clone(),
            8,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            seed,
            &(),
        )
        .unwrap();
        let python = execute_generated(
            &model,
            ScorerLang::Python,
            "python3",
            "check.py",
            &format!(
                "from desk_agent import run_details\nimport json\nprint(json.dumps([run_details(seed={seed}, trace=trace) for trace in range(8)]))"
            ),
        );
        let typescript = execute_generated(
            &model,
            ScorerLang::Typescript,
            "node",
            "check.mts",
            &format!(
                "import {{runDetails}} from './desk_agent.mts'; console.log(JSON.stringify([...Array(8).keys()].map(trace => runDetails(undefined, Math.random, {seed}, trace))));"
            ),
        );
        for (index, event) in batch.events.iter().enumerate() {
            let input = event.input.as_ref().unwrap().project(&()).unwrap();
            let conversation = event.metadata.as_ref().unwrap()["conversation_id"].project(&()).unwrap();
            for (lang, run) in [("python", &python), ("typescript", &typescript)] {
                assert_eq!(run[index]["spans"][0]["input"], input, "{lang}, seed {seed}, trace {index}");
                assert_eq!(
                    run[index]["spans"][0]["metadata"]["conversation_id"], conversation,
                    "{lang}, seed {seed}, trace {index}"
                );
            }
        }
    }
}

#[test]
fn a_conversation_is_the_messages_up_to_a_turn() {
    // the lists are as long as the series, which is not known until a trace is drawn
    let world = r#"(chat:series {turns: 3});
(customer:persona {per: chat.number})
    -|:next|->(:statement {subject: "I", person: "first", verb: "need", complement: "a refund"})
    -|:next|->(:statement {subject: "the order", verb: "be", complement: "late"})
    -|:next|->(:statement {subject: "that", verb: "work"});
(helper:persona {per: chat.number})
    -|:next|->(:statement {subject: "I", person: "first", modal: "can", verb: "help"})
    -|:next|->(:statement {subject: "I", person: "first", verb: "see", complement: "the order"})
    -|:next|->(:statement {subject: "the refund", verb: "be", complement: "on its way"});
(lookups:list {per: chat.number})
    -|:next|->(:given {value: [{role: "assistant", content: null, tool_calls: [{id: "call_1", name: "find_order"}]}, {role: "tool", tool_call_id: "call_1", content: {status: "late"}}]})
    -|:next|->(:given {value: []})
    -|:next|->(:given {value: []});"#;
    let traces = |read: &str| {
        let source = format!(
            "{world}\n(a:agent) {{input: \"hi\", output: t.output}};\n(a)-|:next|->(t:task {{name: \"t\"}}) {{input: a.input, output: {read}}};\n"
        );
        let model = lower(&source).unwrap_or_else(|errors| panic!("{errors}"));
        let generated = data::generate(
            model,
            3,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            1,
            &(),
        );
        generated.map(|batch| {
            (batch.events.iter())
                .map(|event| event.output.as_ref().unwrap().project(&()).unwrap())
                .collect::<Vec<_>>()
        })
    };
    let message = |role: &str, content: &str| json!({"role": role, "content": content});
    let both = r#"conversation({system: "Be brief.", user: customer.said, assistant: helper.said, turn: chat.turn})"#;
    assert_eq!(
        traces(both).unwrap(),
        [
            json!([message("system", "Be brief."), message("user", "I need a refund.")]),
            json!([
                message("system", "Be brief."),
                message("user", "I need a refund."),
                message("assistant", "I can help."),
                message("user", "The order is late."),
            ]),
            json!([
                message("system", "Be brief."),
                message("user", "I need a refund."),
                message("assistant", "I can help."),
                message("user", "The order is late."),
                message("assistant", "I see the order."),
                message("user", "That works."),
            ]),
        ]
    );
    // without the replies it is what the user said, and a null system is left out
    let alone = "conversation({system: null, user: customer.said, turn: chat.turn})";
    assert_eq!(
        traces(alone).unwrap()[2],
        json!([
            message("user", "I need a refund."),
            message("user", "The order is late."),
            message("user", "That works."),
        ])
    );
    // what passed between a message and its reply on an earlier turn sits between them, as written
    let with_calls = "conversation({user: customer.said, assistant: helper.said, between: lookups.items, turn: chat.turn})";
    let called = traces(with_calls).unwrap();
    assert_eq!(called[0], json!([message("user", "I need a refund.")]));
    assert_eq!(
        called[2],
        json!([
            message("user", "I need a refund."),
            {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "name": "find_order"}]},
            {"role": "tool", "tool_call_id": "call_1", "content": {"status": "late"}},
            message("assistant", "I can help."),
            message("user", "The order is late."),
            message("assistant", "I see the order."),
            message("user", "That works."),
        ])
    );
    // a read into a message is typed by what the provider declares
    let read = r#"upper(conversation({user: customer.said, turn: chat.turn})[0].content)"#;
    assert_eq!(traces(read).unwrap()[0], json!("I NEED A REFUND."));
    for (call, told) in [
        (
            "conversation({user: customer.said, turn: 3})",
            "turn is a whole number that counts the user's messages from zero",
        ),
        (
            "conversation({user: customer.said, turn: 0.5})",
            "turn is a whole number that counts the user's messages from zero",
        ),
        (
            "conversation({user: customer.said, assistant: helper.said[0:1], turn: 2})",
            "assistant has a reply for every turn before this one",
        ),
        ("conversation({user: [1], turn: 0})", "user is a list of strings"),
        (
            "conversation({user: customer.said, turn: 0, system: 1})",
            "system is a string",
        ),
        (
            "conversation({user: customer.said, between: [[]], turn: 2})",
            "between has an entry for every turn before this one",
        ),
        (
            "conversation({user: customer.said, between: [1, 2, 3], turn: 2})",
            "between has an entry for every turn before this one",
        ),
        ("conversation({user: customer.said})", "takes an object with user and turn"),
        (
            "conversation({user: customer.said, turn: 0, agent: helper.said})",
            "takes an object with user and turn",
        ),
    ] {
        let error = traces(call).unwrap_err().to_string();
        assert!(error.contains(told), "{call}: {error}");
    }
}

#[test]
fn json_text_and_decimal_places_agree_in_data_and_code() {
    let source = r#"(desk:agent) {input: "hi", output: answer.output};
(desk)-|:next|->(answer:task {name: "answer"}) {
    input: desk.input,
    output: {
        arguments: json({claim_id: "CLM-1", amount: 12.5, whole: 1.0, count: 3, tags: ["a", true, null], nested: {z: 1, a: 2}}),
        text: json("it's plain text"),
        nothing: json(null),
        score: decimal(0.8649, 2),
        half: decimal(7.25, 1),
        kept: decimal(3, 2),
        below: decimal(0.004, 2),
    },
};
"#;
    let model = lower(source).unwrap_or_else(|errors| panic!("{errors}"));
    let batch = data::generate(
        model.clone(),
        1,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        1,
        &(),
    )
    .unwrap();
    let output = batch.events[0].output.as_ref().unwrap().project(&()).unwrap();
    assert_eq!(
        output,
        json!({
            // compact, with the keys of each object in order
            "arguments": r#"{"amount":12.5,"claim_id":"CLM-1","count":3,"nested":{"a":2,"z":1},"tags":["a",true,null],"whole":1.0}"#,
            "text": r#""it's plain text""#,
            "nothing": "null",
            // halves round up, and the result is a float
            "score": 0.86,
            "half": 7.3,
            "kept": 3.0,
            "below": 0.0,
        })
    );
    let python = execute_generated(
        &model,
        ScorerLang::Python,
        "python3",
        "check.py",
        "from desk_agent import run_details\nimport json\nprint(json.dumps(run_details()))",
    );
    let typescript = execute_generated(
        &model,
        ScorerLang::Typescript,
        "node",
        "check.mts",
        "import {runDetails} from './desk_agent.mts'; console.log(JSON.stringify(runDetails()));",
    );
    for (lang, run) in [("python", &python), ("typescript", &typescript)] {
        for (key, expected) in output.as_object().unwrap() {
            let found = &run["output"][key];
            match expected.as_f64() {
                // a whole float prints without its point in JavaScript, so numbers are compared by value
                Some(number) => assert_eq!(found.as_f64(), Some(number), "{lang} {key}"),
                None => assert_eq!(found, expected, "{lang} {key}"),
            }
        }
    }
}

#[test]
fn a_conversation_agrees_in_data_and_code() {
    // both sides of a chat are in the world, so any turn has the messages before it
    let source = r#"(chat:series {turns: 2, up_to: 4});
(customer:persona {style: "text_message", per: chat.number})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "I", person: "first", verb: "need"})
        =|:weighted|=>[
            -|:next {weight: 1}|->(:part {complement: "a refund"}),
            -|:next {weight: 1}|->(:part {complement: "a receipt"}),
        ]
    -|:next|->(:statement {subject: "the order", verb: "be", complement: "late"})
    -|:next|->(:statement {subject: "that", verb: "work"})
    -|:next|->(:statement {subject: "I", person: "first", verb: "like", complement: "it"});
(helper:persona {per: chat.number})
    -|:next|->(:statement {subject: "I", person: "first", modal: "can", verb: "help"})
    -|:next|->(:statement {subject: "I", person: "first", verb: "see", complement: "the order"})
    -|:next|->(:statement {subject: "the refund", verb: "be", complement: "on its way"})
    -|:next|->(:statement {subject: "you", person: "second", verb: "be", complement: "welcome"});
(lookups:list {per: chat.number})
    -|:next|->(:given {value: [{role: "assistant", content: null, tool_calls: [{id: "call_1", name: "find_order"}]}, {role: "tool", tool_call_id: "call_1", content: {status: "late"}}]})
    -|:next|->(:given {value: []})
    -|:next|->(:given {value: []})
    -|:next|->(:given {value: []});
(desk:agent) {input: customer.said[chat.turn], output: answer.output};
(desk)-|:next|->(answer:task {name: "answer"}) {input: desk.input, output: reply.output.content}
    -|:calls|->(reply:llm {name: "chat_completion"}) {
        input: conversation({system: "Be brief.", user: customer.said, assistant: helper.said, between: lookups.items, turn: chat.turn}),
        output: {role: "assistant", content: helper.said[chat.turn]},
    };
"#;
    let model = lower(source).unwrap();
    let mut longest = 0;
    for seed in [1u64, 2] {
        let batch = data::generate(
            model.clone(),
            8,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            seed,
            &(),
        )
        .unwrap();
        let python = execute_generated(
            &model,
            ScorerLang::Python,
            "python3",
            "check.py",
            &format!(
                "from desk_agent import run_details\nimport json\nprint(json.dumps([run_details(seed={seed}, trace=trace) for trace in range(8)]))"
            ),
        );
        let typescript = execute_generated(
            &model,
            ScorerLang::Typescript,
            "node",
            "check.mts",
            &format!(
                "import {{runDetails}} from './desk_agent.mts'; console.log(JSON.stringify([...Array(8).keys()].map(trace => runDetails(undefined, Math.random, {seed}, trace))));"
            ),
        );
        let steps: Vec<_> = batch.events.iter().filter(|event| event.name == "chat_completion").collect();
        assert_eq!(steps.len(), 8);
        for (index, step) in steps.iter().enumerate() {
            let input = step.input.as_ref().unwrap().project(&()).unwrap();
            let messages = input.as_array().unwrap();
            // a system message, a user message and a reply for each earlier turn, the tool call and its result of the first, and the message of this turn
            assert_eq!(messages.len() % 2, 0, "{input}");
            assert_eq!(messages.last().unwrap()["role"], "user");
            longest = longest.max(messages.len());
            for (lang, run) in [("python", &python), ("typescript", &typescript)] {
                let span = (run[index]["spans"].as_array().unwrap().iter())
                    .find(|span| span["name"] == "chat_completion")
                    .unwrap();
                assert_eq!(span["input"], input, "{lang}, seed {seed}, trace {index}");
            }
        }
    }
    assert!(longest >= 6, "{longest}");
    // the call reads as a library function in the generated agent
    for (lang, name) in [
        (ScorerLang::Python, "chat_messages("),
        (ScorerLang::Typescript, "chatMessages("),
    ] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("desk_agent.") && !file.name.contains("runtime"))
            .unwrap();
        assert!(agent.contents.contains(name), "{}", agent.contents);
        assert!(!agent.contents.contains("_world"), "{}", agent.contents);
    }
}

#[test]
fn every_example_agent_reads_as_written_by_hand() {
    // no deferred values, no span bookkeeping, and no helper under a private name
    for source in [
        include_str!("../../../examples/agent_tool_loop.bt"),
        include_str!("../../../examples/computed_fields.bt"),
        include_str!("../../../examples/customer_support_agent.bt"),
        include_str!("../../../examples/dataset_cases.bt"),
        include_str!("../../../examples/error_and_escalation.bt"),
        include_str!("../../../examples/experiments.bt"),
        include_str!("../../../examples/fanout_parallel_workers.bt"),
        include_str!("../../../examples/flight_change.bt"),
        include_str!("../../../examples/multi_turn_conversation.bt"),
        include_str!("../../../examples/rag_pipeline.bt"),
        include_str!("../../../examples/scoring_automations.bt"),
        include_str!("../../../examples/supervisor_and_subagents.bt"),
        include_str!("../../../examples/topics_automations.bt"),
        include_str!("../../../examples/windowed_session.bt"),
    ] {
        let model = lower(source).unwrap();
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
            let agents: Vec<_> = (assembly.files.iter())
                .filter(|file| file.name.contains("_agent.") && !file.name.contains("runtime"))
                .collect();
            assert!(!agents.is_empty());
            for file in agents {
                // the agent's own code follows its imports
                let body = file.contents.split_once("_MISSING = ").unwrap().1;
                for told in [".fields[", ".children[", ".iterations"] {
                    assert!(!body.contains(told), "{told} in {}", file.name);
                }
                let bytes = body.as_bytes();
                for (at, _) in body.match_indices('_') {
                    let starts = at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
                    let name: String = body[at..]
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    let called = body[at + name.len()..].starts_with('(');
                    if starts && called {
                        assert_eq!(name, "_execute", "private helper {name} in {}", file.name);
                    }
                }
            }
        }
    }
}

#[test]
fn a_loop_without_an_ordered_form_runs_in_the_deferred_form() {
    // an entry that reads its sibling takes a name of its own, which a value here already uses,
    // so the plan has no single order and the fallback prints it
    let source = CYCLE_SOURCE
        .replace(
            "input: retrying.input, output: finish.output};",
            "input: retrying.input, output: finish.output, local: {metrics_prompt_tokens: 1}};",
        )
        .replace(
            "earlier: len(context.history.lookup)},\n};",
            "earlier: len(context.history.lookup)},\n    metrics: {prompt_tokens: len(context.history.lookup) + 5, tokens: self.metrics.prompt_tokens + 1},\n};",
        );
    assert_ne!(source, CYCLE_SOURCE);
    let model = lower(&source).unwrap();
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("retrying_agent."))
            .unwrap();
        assert!(agent.contents.contains("_loop("), "{}", agent.contents);
    }
    let names = ["root", "lookup", "check", "lookup", "check", "lookup", "check", "finish"];
    for result in generated_runs(&model, "retrying") {
        assert_eq!(result["output"], json!({"attempt":2,"earlier":2}));
        assert_eq!(
            result["spans"]
                .as_array()
                .unwrap()
                .iter()
                .map(|span| span["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            names
        );
    }
}

#[test]
fn a_duration_reaches_the_step_that_logs_it() {
    // one branch of a fork draws its duration and the other writes it, as a float or from a local
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {input: a.input, output: "ok", local: {slow: a.input == "hi"}};
(t)-|:calls|->(plan:llm {name: "plan"}) {input: t.input, output: "go"};
(plan)
    =|:exclusive|=>[
        -|:next {when: !t.local.slow}|->(quick:tool {name: "lookup"}) {input: t.input, output: {found: true}, duration: range(0.1, 0.2)},
        -|:next {when: t.local.slow}|->(slow:tool {name: "lookup"}) {input: t.input, output: {found: false}, duration: 3.0},
    ];
"#;
    for (written, seconds) in [("3.0", 3.0), ("t.local.slow ? 1.5 : 2", 1.5)] {
        let model = lower(&source.replace("duration: 3.0", &format!("duration: {written}"))).unwrap();
        // a written number is handed to the step, in either language, and not kept behind a helper
        for lang in [ScorerLang::Python, ScorerLang::Typescript] {
            let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
            let agent = (assembly.files.iter())
                .find(|file| file.name.contains("a_agent.") && !file.name.contains("runtime"))
                .unwrap();
            // one that is computed is a value of the branch that runs the step, and no helper is called for it
            let helper = agent.contents.contains("def slow_duration") || agent.contents.contains("function slowDuration");
            assert!(!helper, "{}", agent.contents);
            let computed = if lang == ScorerLang::Python {
                "duration = 1.5 if turn_slow else 2"
            } else {
                "const duration = turnSlow ? 1.5 : 2"
            };
            assert_eq!(agent.contents.contains(computed), written != "3.0", "{}", agent.contents);
            // a float needs no mark where it is only read as seconds
            assert!(!agent.contents.contains("_float"), "{}", agent.contents);
            // the agent's own value that a step is also called is named for the agent, not numbered
            let own = if lang == ScorerLang::Python { "turn_slow" } else { "turnSlow" };
            assert!(agent.contents.contains(own), "{}", agent.contents);
            for numbered in ["slow2", "slow_2", "duration2", "duration_2"] {
                assert!(!agent.contents.contains(numbered), "{numbered} in {}", agent.contents);
            }
        }
        // a step that reads a name nothing gave it fails when it runs
        for details in generated_runs(&model, "a") {
            let spans = details["spans"].as_array().unwrap();
            let slow = spans.iter().find(|span| span["output"] == json!({"found": false})).unwrap();
            assert_eq!(slow["duration"].as_f64(), Some(seconds), "{details}");
        }
    }
}

#[test]
fn a_result_that_may_be_null_has_one_named_type() {
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {input: a.input, output: "ok", local: {sent: chance(0.5)}}
    -|:calls|->(mail:tool {name: "send_summary"}) {
        input: t.input,
        output: t.local.sent ? {delivered: true} : null,
    };
"#;
    let model = lower(source).unwrap();
    for (lang, declared, returned, held) in [
        (
            ScorerLang::Python,
            "class SendSummaryResult(TypedDict):\n    delivered: bool\n",
            ") -> SendSummaryResult | None:",
            "result: SendSummaryResult | None = None",
        ),
        (
            ScorerLang::Typescript,
            "export type SendSummaryResult = {\"delivered\": boolean};",
            "): SendSummaryResult | null {",
            "let result: SendSummaryResult | null = null;",
        ),
    ] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let file = |part: &str| {
            &(assembly.files.iter())
                .find(|file| file.name.contains(part))
                .unwrap()
                .contents
        };
        let types = file("a_agent_types.");
        assert!(types.contains(declared), "{types}");
        // the type is declared once, and nothing declares it again as itself or null
        assert_eq!(types.matches("SendSummaryResult").count(), 1, "{types}");
        let agent = file("a_agent.");
        assert!(agent.contains(returned) && agent.contains(held), "{agent}");
        let imports = agent
            .split_once(returned.split_once("SendSummaryResult").unwrap().0)
            .unwrap()
            .0;
        assert_eq!(
            imports.matches("SendSummaryResult").count(),
            if lang == ScorerLang::Python { 2 } else { 1 },
            "{agent}"
        );
    }
}

// the agent module of an assembly and the types it declares, which is empty when it declares none
fn agent_module(model: &bts_core::mocking::Model, lang: ScorerLang) -> (String, String) {
    let assembly = code::assemble_agents(model, lang, &CodeTarget::new("local-test")).unwrap();
    let file = |part: &str| {
        (assembly.files.iter())
            .find(|file| file.name.contains(part) && !file.name.contains("runtime"))
            .map(|file| file.contents.clone())
            .unwrap_or_default()
    };
    (file("a_agent."), file("a_agent_types."))
}

// the text of one function of an agent module

fn function_text<'a>(agent: &'a str, name: &str) -> &'a str {
    let start = (["def ", "function "].iter())
        .find_map(|keyword| agent.find(&format!("{keyword}{name}(")))
        .unwrap_or_else(|| panic!("no function {name} in {agent}"));
    let rest = &agent[start..];
    let end = (["\ndef ", "\nfunction "].iter())
        .filter_map(|keyword| rest[1..].find(keyword))
        .min()
        .map_or(rest.len(), |end| end + 1);
    &rest[..end]
}

fn step<'a>(details: &'a Value, name: &str) -> &'a Value {
    (details["spans"].as_array().unwrap().iter())
        .find(|span| span["name"] == name)
        .unwrap_or_else(|| panic!("no span {name} in {details}"))
}

#[test]
fn a_format_adds_no_empty_text() {
    // a template that starts or ends with a value has no text before or after it
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {
    input: a.input,
    output: {id: format("msg_{}", hex(12)), cost: format("{} dollars", 5), both: format("{}{}", a.input, 5), only: format("{}", 7)},
};
"#;
    let model = lower(source).unwrap();
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let (agent, _) = agent_module(&model, lang);
        let body = agent.split_once("_MISSING = ").unwrap().1;
        assert!(body.contains("\"msg_\""), "{body}");
        assert!(!body.contains("\"\""), "{body}");
    }
    for details in generated_runs(&model, "a") {
        let output = &details["output"];
        let id = output["id"].as_str().unwrap();
        assert!(id.starts_with("msg_") && id.len() == 16, "{details}");
        assert_eq!(output["cost"], "5 dollars", "{details}");
        assert_eq!(output["both"], "hi5", "{details}");
        assert_eq!(output["only"], "7", "{details}");
    }
}

#[test]
fn an_error_and_a_result_are_set_only_where_they_have_a_value() {
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {input: a.input, output: "ok", local: {sent: a.input == "hi"}}
    -|:calls|->(mail:tool {name: "send_summary"}) {
        input: t.input,
        output: t.local.sent ? {delivered: true} : null,
        error: t.local.sent ? null : "550 mailbox unavailable",
    };
"#;
    for (written, output, error) in [
        ("==", json!({"delivered": true}), None),
        ("!=", Value::Null, Some("550 mailbox unavailable")),
    ] {
        let model = lower(&source.replace("a.input ==", &format!("a.input {written}"))).unwrap();
        for (lang, name) in [(ScorerLang::Python, "send_summary"), (ScorerLang::Typescript, "sendSummary")] {
            let (agent, _) = agent_module(&model, lang);
            let mail = function_text(&agent, name);
            // the result starts as null, so no path assigns null to it again
            assert!(!mail.contains("else"), "{mail}");
            // the error is logged once, under the test for it, and never as null
            assert_eq!(mail.matches("550 mailbox unavailable").count(), 1, "{mail}");
            let plain = mail.replace([' ', '"'], "");
            assert!(!plain.contains("error:null") && !plain.contains("error=None"), "{mail}");
            assert_eq!(mail.matches("if ").count(), 2, "{mail}");
        }
        for details in generated_runs(&model, "a") {
            let mail = step(&details, "send_summary");
            assert_eq!(mail["output"], output, "{details}");
            assert_eq!(mail.get("error").and_then(Value::as_str), error, "{details}");
        }
    }
}

#[test]
fn a_step_that_returns_nothing_has_no_result_type() {
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {input: a.input, output: "ok", local: {down: a.input == "hi"}};
(t)-|:calls|->(plan:llm {name: "plan"}) {input: t.input, output: "go"};
(plan)
    =|:exclusive|=>[
        -|:next {when: !t.local.down}|->(found:tool {name: "lookup"}) {input: t.input, output: {found: true}},
        -|:next {when: t.local.down}|->(failed:tool {name: "lookup"}) {input: t.input, output: null, error: "upstream did not respond"},
    ];
"#;
    let model = lower(source).unwrap();
    for (lang, returned, path) in [
        (ScorerLang::Python, ") -> None:", "maybe_failed"),
        (ScorerLang::Typescript, "): void {", "maybeFailed"),
    ] {
        let (agent, types) = agent_module(&model, lang);
        // nothing is named for a result that is always null, and the path that may run the step does not hand one back
        assert_eq!(
            types.matches("FailedResult").count(),
            types.matches("MaybeFailedResult").count(),
            "{types}"
        );
        for nothing in ["= null", "= None", ": null", ": None"] {
            assert!(!types.contains(nothing), "{types}");
        }
        assert!(!types.contains("\"failed\"") && !types.contains("failed:"), "{types}");
        let failed = function_text(&agent, "failed");
        assert!(failed.contains(returned), "{failed}");
        assert!(!failed.contains("return"), "{failed}");
        assert!(!failed.contains("result"), "{failed}");
        // the step is called for what it does
        let maybe = function_text(&agent, path);
        assert!(!maybe.contains("= failed(") && maybe.contains("failed("), "{maybe}");
    }
    for details in generated_runs(&model, "a") {
        let spans = details["spans"].as_array().unwrap();
        let failed = spans.iter().find(|span| span["error"] == "upstream did not respond").unwrap();
        assert_eq!(failed["name"], "lookup", "{details}");
        assert_eq!(failed["output"], Value::Null, "{details}");
        assert_eq!(spans.iter().filter(|span| span["name"] == "lookup").count(), 1, "{details}");
    }
}

#[test]
fn durations_that_meet_are_named_for_their_steps() {
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {input: a.input, output: "ok", local: {known: a.input == "hi"}};
(t)-|:calls|->(plan:llm {name: "plan"}) {input: t.input, output: "go"};
(plan)
    =|:exclusive|=>[
        -|:next {when: !t.local.known}|->(greet:tool {name: "greet"}) {input: t.input, output: "hello", duration: range(0.5, 0.6)},
        -|:next {when: t.local.known}|->(screen:task {name: "screen"}) {input: t.input, output: assess.output},
    ];
(screen)-|:calls|->(history:tool {name: "history"}) {input: t.input, output: {prior: 0}, duration: range(0.1, 0.2)}
    -|:next|->(assess:tool {name: "assess"}) {input: history.output, output: "low", duration: range(0.3, 0.4)};
"#;
    let model = lower(source).unwrap();
    for (lang, both, alone) in [
        (
            ScorerLang::Python,
            ["history_duration", "assess_duration"],
            "duration: int | float",
        ),
        (
            ScorerLang::Typescript,
            ["historyDuration", "assessDuration"],
            "duration: number",
        ),
    ] {
        let (agent, _) = agent_module(&model, lang);
        // the step that runs both tells their durations apart by the steps they belong to
        let screen = function_text(&agent, "screen");
        for name in both {
            assert!(screen.contains(name), "{screen}");
        }
        // a step that sees one duration calls it that, and no name is numbered
        for name in ["greet", "history", "assess"] {
            assert!(function_text(&agent, name).contains(alone), "{agent}");
        }
        let body = agent.split_once("_MISSING = ").unwrap().1;
        for (at, _) in body.match_indices("uration") {
            let after = body[at + "uration".len()..].trim_start_matches('_');
            assert!(!after.starts_with(|c: char| c.is_ascii_digit()), "{body}");
        }
    }
    for details in generated_runs(&model, "a") {
        for (name, low, high) in [("history", 0.1, 0.2), ("assess", 0.3, 0.4)] {
            let seconds = step(&details, name)["duration"].as_f64().unwrap();
            assert!((low..=high).contains(&seconds), "{details}");
        }
    }
}

#[test]
fn a_draw_that_cannot_fail_is_a_plain_value() {
    // text around a draw and a draw held within bounds are computed where they are declared,
    // so the steps that read them take a string and a number
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {input: a.input, output: "ok", local: {known: a.input == "hi", call_id: format("call_{}", alphanum(24))}};
(t)-|:calls|->(plan:llm {name: "plan"}) {input: t.input, output: {id: t.local.call_id}};
(plan)
    =|:exclusive|=>[
        -|:next {when: !t.local.known}|->(greet:tool {name: "greet"}) {input: t.input, output: "hello"},
        -|:next {when: t.local.known}|->(screen:task {name: "screen"}) {input: {id: t.local.call_id}, output: assess.output},
    ];
(screen)-|:calls|->(history:tool {name: "history"}) {input: t.input, output: {prior: 0}, duration: range(0.1, 0.2)}
    -|:next|->(assess:llm {name: "assess"}) {input: history.output, output: "low", duration: clamp(lognormal(0.6, 0.3), 0.3, 2.5)};
"#;
    let model = lower(source).unwrap();
    for (lang, typed) in [
        (ScorerLang::Python, ["call_id: str", "assess_duration: int | float"]),
        (ScorerLang::Typescript, ["callId: string", "assessDuration: number"]),
    ] {
        let (agent, _) = agent_module(&model, lang);
        assert!(!agent.contains("_lazy("), "{agent}");
        for parameter in typed {
            assert!(agent.contains(parameter), "{parameter} in {agent}");
        }
    }
    for details in generated_runs(&model, "a") {
        // the id is drawn once, however many steps read it
        let id = step(&details, "plan")["output"]["id"].as_str().unwrap();
        assert!(id.starts_with("call_") && id.len() == 29, "{details}");
        assert_eq!(step(&details, "screen")["input"]["id"], id, "{details}");
        let seconds = step(&details, "assess")["duration"].as_f64().unwrap();
        assert!((0.3..=2.5).contains(&seconds), "{details}");
    }
}

#[test]
fn a_step_reads_the_steps_inside_an_earlier_turn() {
    // each turn after the first reads the reply of the turn before it, which sits inside that turn,
    // and the last step reads a reply that is only there on one branch
    let source = r#"(desk:agent) {input: "hi", output: conv.output};
(desk)-|:next|->(conv:task {name: "conversation"}) {
    input: desk.input,
    output: "done",
    local: {kind: weighted(["a", 1], ["b", 1])},
};
(conv)
    =|:exclusive|=>[
        -|:calls {when: conv.local.kind == "a"}|->(a1:task {name: "turn"}) {input: "first a", output: a1_reply.output},
        -|:calls {when: conv.local.kind == "b"}|->(b1:task {name: "turn"}) {input: "first b", output: "ok"},
    ];
(a1)-|:calls|->(a1_reply:llm {name: "chat"}) {
        input: [{role: "user", content: a1.input}],
        output: {role: "assistant", content: "reply one"},
    };
(a1)-|:next|->(a2:task {name: "turn"}) {input: "second a", output: a2_reply.output};
(a2)-|:calls|->(a2_reply:llm {name: "chat"}) {
        input: [...a1_reply.input, a1_reply.output, {role: "user", content: a2.input}],
        output: {role: "assistant", content: "reply two"},
    };
(a2)-|:next|->(a3:task {name: "turn"}) {input: "third a", output: a3_reply.output};
(a3)-|:calls|->(a3_reply:llm {name: "chat"}) {
        input: [...a2_reply.input, a2_reply.output, {role: "user", content: a3.input}],
        output: {role: "assistant", content: "reply three"},
    };
(conv)-|:calls|->(after:task {name: "after"}) {input: a1.input, output: a3_reply.output};
"#;
    let model = lower(source).unwrap();
    let second = json!([
        {"role": "user", "content": "first a"},
        {"role": "assistant", "content": "reply one"},
        {"role": "user", "content": "second a"},
    ]);
    let third = json!([
        {"role": "user", "content": "first a"},
        {"role": "assistant", "content": "reply one"},
        {"role": "user", "content": "second a"},
        {"role": "assistant", "content": "reply two"},
        {"role": "user", "content": "third a"},
    ]);
    // the steps of each branch as the data has them
    let batch = data::generate(
        model.clone(),
        16,
        Duration::from_secs(3600),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(7200),
        7,
        &(),
    )
    .unwrap();
    let value = |value: &Option<data::Value>| value.as_ref().map_or(Value::Null, |value| value.project(&()).unwrap());
    let mut branches: Vec<Vec<Value>> = Vec::new();
    for root in batch.events.iter().filter(|event| event.span_parents.is_empty()) {
        let mut steps: Vec<Value> = (batch.events.iter())
            .filter(|event| event.root_span_id == root.root_span_id)
            .map(|event| json!([event.name, value(&event.input), value(&event.output)]))
            .collect();
        steps.sort_by_key(|step| step.to_string());
        if !branches.contains(&steps) {
            branches.push(steps);
        }
    }
    branches.sort_by_key(|steps| steps.len());
    assert_eq!(
        branches.iter().map(|steps| steps.len()).collect::<Vec<_>>(),
        [3, 8],
        "{branches:?}"
    );
    assert!(branches[1].contains(&json!(["chat", second, {"role": "assistant", "content": "reply two"}])));
    assert!(branches[1].contains(&json!(["chat", third, {"role": "assistant", "content": "reply three"}])));
    assert!(branches[0].contains(&json!(["after", null, null])));
    let python = execute_generated(
        &model,
        ScorerLang::Python,
        "python3",
        "check.py",
        "from desk_agent import run_details\nimport json\nprint(json.dumps([run_details(random_source=lambda: draw) for draw in [0.99, 0.0]]))",
    );
    let typescript = execute_generated(
        &model,
        ScorerLang::Typescript,
        "node",
        "check.mts",
        "import {runDetails} from './desk_agent.mts'; console.log(JSON.stringify([0.99, 0].map(draw => runDetails(undefined, () => draw))));",
    );
    for (lang, runs) in [("python", &python), ("typescript", &typescript)] {
        for (run, expected) in runs.as_array().unwrap().iter().zip(&branches) {
            let mut steps: Vec<Value> = (run["spans"].as_array().unwrap().iter())
                .map(|span| json!([span["name"], span["input"], span["output"]]))
                .collect();
            steps.sort_by_key(|step| step.to_string());
            assert_eq!(&steps, expected, "{lang}");
        }
    }
    // the replies reach the later turns as values the earlier turns return
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("desk_agent.") && !file.name.contains("runtime"))
            .unwrap();
        for helper in ["_child(", "_at(", ".children"] {
            assert!(!agent.contents.contains(helper), "{helper} in {}", agent.contents);
        }
    }
}

#[test]
fn a_scorer_divides_two_integers_as_the_data_does() {
    use bts_core::mocking::ScorerKind;
    use code::Emitter;
    // 900 characters over 400 is 2, and a hundred short of a thousand over 30 is 3 short with 10 left over
    let source = r#"
(checker:agent) {input: "go", output: root.output};
(checker)-|:next|->(root:task) {input: checker.input, output: "OUTPUT"};
(root)-|:calls|->(length:scorer) {score: clamp(len(context.output) / 400, 0, 5) / 10.0};
(root)-|:calls|->(short:scorer) {score: ${${len(context.output) - 1000} / 30 + ${len(context.output) - 1000} % 30} / -100.0};
"#
    .replace("OUTPUT", &"x".repeat(900));
    let model = lower(&source).unwrap();
    let batch = one_trace(&source);
    let expected = [("length", 0.2), ("short", 0.13)];
    for (name, score) in expected {
        let event = batch.events.iter().find(|event| event.name == name).unwrap();
        assert_eq!(event.output.as_ref().unwrap().project(&()).unwrap(), json!({"score": score}));
    }
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        for (name, score) in expected {
            let scorer = model.scorers.iter().find(|scorer| scorer.name == name).unwrap();
            let ScorerKind::Code { score: value, steps } = &scorer.kind else {
                panic!("code scorer")
            };
            let module = code::languages::Languages
                .emit(
                    lang,
                    &[code::Item {
                        name,
                        slug: name,
                        score: value,
                        steps,
                    }],
                )
                .unwrap();
            let dir = std::env::temp_dir().join(format!("bts-scorer-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            let function = &module.functions[0];
            let (program, file, call) = match lang {
                ScorerLang::Python => (
                    "python3",
                    "check.py",
                    format!("\nimport json\nprint(json.dumps({function}(output='x' * 900)))\n"),
                ),
                ScorerLang::Typescript => (
                    "node",
                    "check.mts",
                    format!("\nconsole.log(JSON.stringify({function}({{output: 'x'.repeat(900)}})))\n"),
                ),
            };
            let path = dir.join(file);
            fs::write(&path, format!("{}{call}", module.source)).unwrap();
            let output = Command::new(program).arg(path).output().unwrap();
            fs::remove_dir_all(dir).unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["score"], json!(score), "{name} in {}", module.source);
        }
    }
}

// the parameters of every function of an agent module that the function calls as a function
fn called_parameters(agent: &str) -> Vec<String> {
    let mut found = Vec::new();
    let headers = ["def ", "function "];
    let starts: Vec<usize> = (agent.match_indices('\n'))
        .map(|(at, _)| at + 1)
        .filter(|at| headers.iter().any(|header| agent[*at..].starts_with(header)))
        .collect();
    for (index, start) in starts.iter().enumerate() {
        let text = &agent[*start..starts.get(index + 1).copied().unwrap_or(agent.len())];
        let (header, body) = text.split_once('\n').unwrap_or((text, ""));
        let Some((name, rest)) = header.split_once('(') else {
            continue;
        };
        // a parameter ends at a comma outside the brackets of its type
        let mut depth = 0;
        let mut parameters = vec![String::new()];
        for letter in rest.chars() {
            match letter {
                '(' | '[' | '<' | '{' => depth += 1,
                ')' if depth == 0 => break,
                ')' | ']' | '>' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    parameters.push(String::new());
                    continue;
                }
                _ => {}
            }
            parameters.last_mut().unwrap().push(letter);
        }
        for parameter in parameters {
            let parameter = parameter.trim().trim_start_matches('*');
            let end = parameter.find([':', '=', ' ']).unwrap_or(parameter.len());
            let parameter = &parameter[..end];
            let called = format!("{parameter}()");
            let read = body.match_indices(&called).any(|(at, _)| {
                !body[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|before| before.is_alphanumeric() || before == '_' || before == '.')
            });
            if !parameter.is_empty() && read {
                found.push(format!("{} in {}", parameter, name.trim()));
            }
        }
    }
    found
}

// the name, input and output of every span of each trace, in an order that does not depend on timing
fn span_records(batch: &data::EventBatch) -> Vec<Vec<String>> {
    let mut traces: Vec<(String, Vec<String>)> = Vec::new();
    for event in &batch.events {
        let record = json!([
            event.name,
            event.input.as_ref().map(|value| value.project(&()).unwrap()),
            event.output.as_ref().map(|value| value.project(&()).unwrap()),
        ])
        .to_string();
        match traces.iter_mut().find(|(root, _)| *root == event.root_span_id) {
            Some((_, records)) => records.push(record),
            None => traces.push((event.root_span_id.clone(), vec![record])),
        }
    }
    (traces.into_iter())
        .map(|(_, mut records)| {
            records.sort();
            records
        })
        .collect()
}

fn run_records(details: &Value) -> Vec<String> {
    let mut records: Vec<String> = (details["spans"].as_array().unwrap().iter())
        .map(|span| json!([span["name"], span.get("input"), span.get("output")]).to_string())
        .collect();
    records.sort();
    records
}

#[test]
fn a_world_is_computed_once_as_typed_values() {
    // a series, a fact, an entity and a persona with a fork, read by the agent's input and by two steps
    let source = r#"(chat:series {turns: 2, up_to: 4, weights: [3, 2, 1]});
(tone:fact {per: chat.number})
    =|:weighted|=>[
        -|:next {weight: 2}|->(:given {value: "conversational"}),
        -|:next {weight: 1}|->(:given {value: "professional"}),
    ];
(person:entity {per: chat.number})
    =|:weighted|=>[
        -|:next {weight: 1}|->(:given {value: {name: "Renee Castillo", customer_id: "cus_48121"}}),
        -|:next {weight: 1}|->(:given {value: {name: "Owen Brandt", customer_id: "cus_50977"}}),
    ];
(customer:persona {style: tone.value, per: chat.number})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "I", person: "first"})
        =|:weighted|=>[
            -|:next {weight: 2}|->(:part {verb: "need", complement: "to file a claim"}),
            -|:next {weight: 1}|->(:part {verb: "want", complement: "to open a claim"}),
        ]
    -|:next|->(:sentence) {defaults: {form: "question"}}
        -|:next|->(:part {subject: "my deductible", verb: "apply", complement: "to this claim"})
    -|:next|->(:sentence) {defaults: {form: "question"}}
        -|:next|->(:part {subject: "I", person: "first", modal: "can", verb: "upload", complement: "the photos"})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "that", verb: "help", complement: "a lot"});
(desk:agent) {input: {message: customer.said[chat.turn], customer_id: person.attributes.customer_id}, output: t.output};
(desk)-|:next|->(t:task {name: "turn"}) {
    input: desk.input,
    output: {id: chat.id, turn: chat.turn, tone: tone.value, name: person.attributes.name, reply: reply.output},
};
(t)-|:calls|->(lookup:tool {name: "lookup"}) {input: {customer_id: person.attributes.customer_id}, output: {holder: person.attributes.name, tone: tone.value}}
    -|:next|->(reply:llm {name: "reply"}) {input: conversation({user: customer.said, turn: chat.turn}), output: format("Thanks {}", person.attributes.name)};
"#;
    let model = lower(source).unwrap();
    for (lang, typed) in [
        (
            ScorerLang::Python,
            [
                "person_attributes: PersonAttributes",
                "tone_value: str",
                "customer_said: list[",
            ],
        ),
        (
            ScorerLang::Typescript,
            [
                "personAttributes: PersonAttributes",
                "toneValue: string",
                "customerSaid: Array<",
            ],
        ),
    ] {
        let assembly = code::assemble_agents(&model, lang, &CodeTarget::new("local-test")).unwrap();
        let agent = (assembly.files.iter())
            .find(|file| file.name.contains("desk_agent.") && !file.name.contains("runtime"))
            .unwrap();
        // nothing is deferred, and no step is handed a function to call for a value
        assert!(!agent.contents.contains("_lazy("), "{}", agent.contents);
        assert_eq!(called_parameters(&agent.contents), Vec::<String>::new(), "{}", agent.contents);
        // the steps take what the world gives as values of the types the walks give them
        for parameter in typed {
            assert!(agent.contents.contains(parameter), "{parameter} in {}", agent.contents);
        }
        let any = if lang == ScorerLang::Python { ": Any" } else { ": any" };
        for function in ["lookup", "reply"] {
            assert!(!function_text(&agent.contents, function).contains(any), "{}", agent.contents);
        }
    }
    let mut turns = std::collections::BTreeSet::new();
    for seed in [5u64, 9] {
        let batch = data::generate(
            model.clone(),
            10,
            Duration::from_secs(3600),
            data::Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            seed,
            &(),
        )
        .unwrap();
        let expected = span_records(&batch);
        assert_eq!(expected.len(), 10);
        let python = execute_generated(
            &model,
            ScorerLang::Python,
            "python3",
            "check.py",
            &format!(
                "from desk_agent import run_details\nimport json\nprint(json.dumps([run_details(seed={seed}, trace=trace) for trace in range(10)]))"
            ),
        );
        let typescript = execute_generated(
            &model,
            ScorerLang::Typescript,
            "node",
            "check.mts",
            &format!(
                "import {{runDetails}} from './desk_agent.mts'; console.log(JSON.stringify([...Array(10).keys()].map(trace => runDetails(undefined, Math.random, {seed}n, trace))));"
            ),
        );
        for (index, records) in expected.iter().enumerate() {
            assert_eq!(&run_records(&python[index]), records, "python, seed {seed}, trace {index}");
            assert_eq!(
                &run_records(&typescript[index]),
                records,
                "typescript, seed {seed}, trace {index}"
            );
            turns.insert(python[index]["output"]["turn"].as_u64().unwrap());
        }
    }
    assert!(turns.len() > 1, "{turns:?}");
}

#[test]
fn a_value_that_can_fail_is_computed_on_the_branch_that_reads_it() {
    // the history asks for a turn nobody wrote, so computing it raises, and only the step that
    // does not run for this input reads it
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {
    input: a.input,
    output: "ok",
    local: {known: a.input == "hi", history: conversation({user: ["one"], turn: 3})},
};
(t)-|:calls|->(plan:llm {name: "plan"}) {input: t.input, output: "go"};
(plan)
    =|:exclusive|=>[
        -|:next {when: t.local.known}|->(greet:tool {name: "greet"}) {input: t.input, output: "hello"},
        -|:next {when: !t.local.known}|->(recall:tool {name: "recall"}) {input: t.local.history, output: "none"},
    ];
"#;
    let model = lower(source).unwrap();
    // data generation never reads the history, so it does not raise
    let batch = one_trace(source);
    let names: Vec<&str> = batch.events.iter().map(|event| event.name.as_str()).collect();
    assert!(names.contains(&"greet") && !names.contains(&"recall"), "{names:?}");
    for (lang, call, branch) in [
        (ScorerLang::Python, "chat_messages(", "if runs_recall:"),
        (ScorerLang::Typescript, "chatMessages(", "if (runsRecall) {"),
    ] {
        let (agent, _) = agent_module(&model, lang);
        assert!(!agent.contains("_lazy("), "{agent}");
        assert_eq!(called_parameters(&agent), Vec::<String>::new(), "{agent}");
        // it is computed once, inside the branch that runs the step, and handed to the step as a value
        assert_eq!(agent.matches(call).count(), 1, "{agent}");
        let guarded = function_text(
            &agent,
            if lang == ScorerLang::Python {
                "maybe_recall"
            } else {
                "maybeRecall"
            },
        );
        let (before, inside) = guarded.split_once(branch).unwrap_or_else(|| panic!("no branch in {guarded}"));
        assert!(!before.contains(call) && inside.contains(call), "{guarded}");
        assert!(!function_text(&agent, "_execute").contains(call), "{agent}");
    }
    // the agent takes the same path without raising
    for details in generated_runs(&model, "a") {
        let spans: Vec<&str> = (details["spans"].as_array().unwrap().iter())
            .map(|span| span["name"].as_str().unwrap())
            .collect();
        assert_eq!(spans, ["turn", "plan", "greet"], "{details}");
    }
    // and raises on the path that reads the history, where the step that reads it would run
    let failed = |lang: ScorerLang, program: &str, file: &str, runner: &str| {
        let details = execute_generated(&model, lang, program, file, runner);
        assert!(
            details["error"]
                .as_str()
                .unwrap()
                .contains("conversation: turn is a whole number that counts the user's messages from zero"),
            "{details}"
        );
    };
    failed(
        ScorerLang::Python,
        "python3",
        "check.py",
        "from a_agent import run_details\nimport json\ntry:\n    run_details('bye')\n    print(json.dumps({'error': ''}))\nexcept Exception as error:\n    print(json.dumps({'error': str(error)}))",
    );
    failed(
        ScorerLang::Typescript,
        "node",
        "check.mts",
        "import {runDetails} from './a_agent.mts'; let error = ''; try { runDetails('bye'); } catch (raised) { error = String(raised); } console.log(JSON.stringify({error}));",
    );
}

#[test]
fn values_are_named_for_what_owns_them() {
    // two steps keep a value under one name that no local may take, and a walk has two sentences without names
    let source = r#"(customer:persona {style: "text_message"})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "I", person: "first", verb: "need", complement: "to file a claim"})
    -|:next|->(:sentence)
        -|:next|->(:part {subject: "that", verb: "help", complement: "a lot"});
(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {input: a.input, output: customer.said};
(t)-|:calls|->(first:llm {name: "plan"}) {
    local: {arguments: {customer_id: t.input}},
    input: t.input,
    output: {name: "lookup", arguments: self.local.arguments},
}
    -|:next|->(lookup:tool {name: "lookup"}) {input: first.local.arguments, output: "found"}
    -|:next|->(second:llm {name: "plan"}) {
        local: {arguments: {claim_id: lookup.output}},
        input: lookup.output,
        output: {name: "update", arguments: self.local.arguments},
    }
    -|:next|->(update:tool {name: "update"}) {input: second.local.arguments, output: "done"};
"#;
    let model = lower(source).unwrap();
    for (lang, owned) in [
        (
            ScorerLang::Python,
            [
                "first_arguments",
                "second_arguments",
                "customer_sentence_1_text",
                "customer_sentence_2_text",
            ],
        ),
        (
            ScorerLang::Typescript,
            [
                "firstArguments",
                "secondArguments",
                "customerSentence1Text",
                "customerSentence2Text",
            ],
        ),
    ] {
        let (agent, _) = agent_module(&model, lang);
        for name in owned {
            assert!(agent.contains(name), "{name} in {agent}");
        }
        // a number says which sentence of the persona a value is, and never that a name was taken
        for numbered in [
            "arguments2",
            "arguments_2",
            "sentenceText2",
            "sentence_text_2",
            "Text2",
            "text_2",
        ] {
            assert!(!agent.contains(numbered), "{numbered} in {agent}");
        }
    }
    for details in generated_runs(&model, "a") {
        assert_eq!(step(&details, "lookup")["input"], json!({"customer_id": "hi"}), "{details}");
        assert_eq!(step(&details, "update")["input"], json!({"claim_id": "found"}), "{details}");
        assert_eq!(details["output"].as_array().unwrap().len(), 2, "{details}");
    }
}

#[test]
fn a_value_that_can_fail_on_two_branches_is_computed_on_each() {
    // two steps that never run together read a history that raises, a ticket that is drawn,
    // and a note that both draws and may raise for what the agent is given
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-|:next|->(t:task {name: "turn"}) {
    input: a.input,
    output: "ok",
    local: {
        history: conversation({user: ["one"], turn: 3}),
        ticket: format("T-{}", range(100, 999)),
        note: paragraph([format("N-{}", range(100, 999)), a.input]),
    },
};
(t)-|:calls|->(plan:llm {name: "plan"}) {input: t.input, output: "go"};
(plan)
    =|:exclusive|=>[
        -|:next {when: t.input == "hi"}|->(greet:tool {name: "greet"}) {input: t.input, output: "hello"},
        -|:next {when: t.input == "a"}|->(recall:tool {name: "recall"}) {input: {history: t.local.history, ticket: t.local.ticket}, output: "none"},
        -|:next {when: t.input == "b"}|->(review:tool {name: "review"}) {input: {note: t.local.note, ticket: t.local.ticket}, output: "some"},
        -|:next {when: t.input == "c"}|->(revise:tool {name: "revise"}) {input: {note: t.local.note, ticket: t.local.ticket}, output: "more"},
    ];
"#;
    let model = lower(source).unwrap();
    for (lang, history, note, draw) in [
        (ScorerLang::Python, "chat_messages(", "join_sentences(", "note_draw = "),
        (ScorerLang::Typescript, "chatMessages(", "joinSentences(", "const noteDraw = "),
    ] {
        let (agent, _) = agent_module(&model, lang);
        assert!(!agent.contains("_lazy("), "{agent}");
        assert_eq!(called_parameters(&agent), Vec::<String>::new(), "{agent}");
        let entry = function_text(&agent, "_execute");
        // what can raise is computed by the one step that reads it, or once on each branch that does
        assert_eq!(agent.matches(history).count(), 1, "{agent}");
        assert_eq!(agent.matches(note).count(), 2, "{agent}");
        assert!(!entry.contains(history) && !entry.contains(note), "{agent}");
        // what is drawn is drawn once, where every branch can read it
        assert_eq!(entry.matches("ange(100, 999").count(), 2, "{agent}");
        assert!(entry.contains(draw), "{agent}");
    }
    let run = |input: &str| {
        let python = execute_generated(
            &model,
            ScorerLang::Python,
            "python3",
            "check.py",
            &format!(
                "from a_agent import run_details\nimport json\ntry:\n    print(json.dumps(run_details('{input}')))\nexcept Exception as error:\n    print(json.dumps({{'error': str(error)}}))"
            ),
        );
        let typescript = execute_generated(
            &model,
            ScorerLang::Typescript,
            "node",
            "check.mts",
            &format!(
                "import {{runDetails}} from './a_agent.mts'; let details; try {{ details = runDetails('{input}'); }} catch (raised) {{ details = {{error: String(raised)}}; }} console.log(JSON.stringify(details));"
            ),
        );
        [python, typescript]
    };
    // a path that reads neither does not raise
    for details in run("hi") {
        assert_eq!(step(&details, "greet")["output"], "hello", "{details}");
    }
    // the path that reads the history raises for it
    for details in run("a") {
        assert!(details["error"].as_str().unwrap().contains("conversation: turn"), "{details}");
    }
    // each path that reads the note joins the same draw with what the agent was given
    for input in ["b", "c"] {
        for details in run(input) {
            let name = if input == "b" { "review" } else { "revise" };
            let read = &step(&details, name)["input"];
            let note = read["note"].as_str().unwrap();
            assert!(note.starts_with("N-") && note.ends_with(&format!(". {input}")), "{details}");
            assert!(read["ticket"].as_str().unwrap().starts_with("T-"), "{details}");
        }
    }
}
