use bts_core::mocking::{ScorerLang, code, data};
use bts_core::providers::braintrust::{Bundle, mocking::code::CodeTarget};
use bts_lang::semantics::{Source, compile_with_definitions};
use serde_json::{Value, json};
use std::{
    fs,
    process::Command,
    time::{Duration, UNIX_EPOCH},
};

const SOURCE: &str = r#"# shared mock data and mock code case
(intake:agent) {
    input: {patient_id: "p-123"},
    output: request.output,
};

(intake)-[:contains]->(request);
(intake)-[:contains]->(lookup);

(intake)-[:next]->(request:task) {
    input: intake.input,
    output: lookup.output,
};

(request)-[:next, :feeds {value: source.input.patient_id, into: target.patient_id}]->(lookup:tool) {
    patient_id: <-,
    input: {patient_id: self.patient_id},
    output: {patient_id: self.patient_id, found: true},
};

(lookup)-[:child_of]->(request);
"#;

fn lower(source: &str) -> Result<bts_core::mocking::Model, String> {
    let graph = compile_with_definitions(
        Source {
            name: "agent.bt",
            text: source,
        },
        &[Bundle::DEFINITIONS],
    )
    .unwrap_or_else(|errors| panic!("{errors:#?}"));
    Bundle::lower(&graph)
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
        .replace("lookup:tool", "lookup:lookup_kind")
        .replace("output: lookup.output", "output: finish.output");
    let source = format!(
        r#"+(intake_kind::agent {{}}); +(lookup_kind::tool {{}});
{source}
(intake)-[:contains]->(finish:tool) {{input: intake.input, output: {{patient_id: self.input.patient_id, found: true}}}};
(lookup)-[:next]->(finish);
(finish)-[:child_of]->(request);
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
    let path = directory.join("src/agents").join(file);
    fs::write(&path, runner).unwrap();
    let output = Command::new(program).arg(&path).output().expect("local language runtime");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn graph_agent_rejects_invalid_parenting_and_unsupported_flow() {
    for (source, message) in [
        (SOURCE.replace("(lookup)-[:child_of]->(request);", ""), "needs a child_of"),
        (
            SOURCE.replace("(lookup)-[:child_of]->(request);", "(lookup)-[:child_of]->(lookup);"),
            "before its child",
        ),
        (format!("{SOURCE}\n(request)-[:child_of]->(lookup);"), "entry span"),
        (format!("{SOURCE}\n(lookup)-[:child_of]->(request);"), "multiple child_of"),
        (format!("{SOURCE}\n(lookup)~[:next]~>(request);"), "cycle edges"),
        (format!("{SOURCE}\n(request)-[:next]->(lookup);"), "multiple next"),
        (
            SOURCE.replace(
                ":next, :feeds {value: source.input.patient_id, into: target.patient_id}",
                ":next",
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
        .replace("patient_id: <-,", "patient_id: <-, token: hex(8),")
        .replace("found: true", "found: true, first: self.token, second: self.token");
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
        ("array", "[|1|]"),
        ("object", "{}"),
    ] {
        for valid in [true, false] {
            let value = if valid { value } else { "null" };
            let source = format!(
                "+(checked_tool::tool {{fields: {{patient_id: {{type: \"{ty}\"}}}}}});\n{}",
                SOURCE
                    .replace("lookup:tool", "lookup:checked_tool")
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
(intake)-[:contains]->(request);
(intake)-[:contains]->(lookup);
(intake)-[:contains]->(fallback);
(intake)-[:next]->(request:task) {
    input: intake.input,
    output: self.input.route == "lookup" ? lookup.output: self.input.route == "fallback" ? fallback.output: "none",
}
    =[:exclusive]=>[
        -[:next {when: source.input.route == "lookup"}]->(lookup:tool) {output: "found"},
        -[:next {when: source.input.route == "fallback"}]->(fallback:tool) {output: "deferred"},
    ];
(lookup)-[:child_of]->(request);
(fallback)-[:child_of]->(request);
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
(checker)-[:contains]->(root);
(checker)-[:contains]->(quality);
(checker)-[:next]->(root:task) {input: checker.input, output: self.input, expected: "yes"};
(root)-[:next {when: source.input != "skip"}]->(quality:scorer {file: "checks"}) {
    score: context.output == context.expected ? 1: 0,
    reason: "compared output to expected",
};
(quality)-[:child_of]->(root);
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
(joined)-[:contains]->(root);
(joined)-[:contains]->(left_task);
(joined)-[:contains]->(right_task);
(joined)-[:contains]->(finish);
(joined)-[:next]->(root:task) {input: joined.input};
(root)
    =[:conditional]=>[
        -[first:next {when: source.input}]->(left_task:tool) {output: "first"},
        -[second:next]->(right_task:tool) {output: "second"},
    ]
        >[:accumulate {observe: "result"} [second], [first]]>>(finish:task) {output: context.arrivals};
(left_task)-[:child_of]->(root);
(right_task)-[:child_of]->(root);
(finish)-[:child_of]->(root);
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
(batch:agent) {input: [|"a", "b", "c"|], output: "done"};
(batch)-[:contains]->(root);
(batch)-[:contains]->(work);
(batch)-[:next]->(root:task) {input: batch.input};
(root)
    =[:each {items: source.input}]=>[
        -[:next]->(work:tool) {input: context.item, output: {value: self.input, index: context.index}},
    ];
(work)-[:child_of]->(root);
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
(batch)-[:contains]->(root);
(batch)-[:contains]->(work);
(batch)-[:next]->(root:task) {input: batch.input};
(root)
    =[:repeat {count: source.input}]=>[
        -[:next]->(work:tool) {
            token: hex(8),
            output: {index: context.index, count: context.count, first: self.token, second: self.token},
        },
    ];
(work)-[:child_of]->(root);
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
(history_agent)-[:contains]->(root);
(history_agent)-[:contains]->(turn);
(history_agent)-[:next]->(root:task) {input: history_agent.input};
(root)
    =[:repeat {count: source.input}]=>[
        -[:next]->(turn:task) {
            input: context.history.turn,
            output: context.index,
        },
    ];
(turn)-[:child_of]->(root);
"#;
    for nested in [false, true] {
        let source = if nested {
            source.replace("(root)\n    =[:repeat", "(root)=[:repeat {count: 2}]=>[-[:next]->(group:task) {input: root.input}];\n(history_agent)-[:contains]->(group);\n(group)-[:child_of]->(root);\n(group)\n    =[:repeat")
                .replace("(turn)-[:child_of]->(root)", "(turn)-[:child_of]->(group)")
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
