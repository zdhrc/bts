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
+(patient_lookup:node) {patient_id: {type: "string"}}-[:extends]->(tool);

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

(request)-[:next, :feeds {value: source.input.patient_id, into: target.patient_id}]->(lookup:patient_lookup) {
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
    Bundle::lower(&graph).map_err(|diag| diag.what)
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
        r#"+(intake_kind:node)-[:extends]->(agent); +(lookup_kind:node)-[:extends]->(patient_lookup);
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
        ("array", "[|1|]"),
        ("object", "{}"),
    ] {
        for valid in [true, false] {
            let value = if valid { value } else { "null" };
            let source = format!(
                "+(checked_tool:node) {{patient_id: {{type: \"{ty}\"}}}}-[:extends]->(tool);\n{}",
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
            local: {token: hex(8)},
            output: {index: context.index, count: context.count, first: self.local.token, second: self.local.token},
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

#[test]
fn weighted_forks_select_once_and_allow_no_selection() {
    let source = r#"
(router:agent) {input: {enabled: true, weight: 8}, output: root.output};
(router)-[:contains]->(root);
(router)-[:contains]->(a);
(router)-[:contains]->(b);
(router)-[:next]->(root:task) {input: router.input, output: "done"};
(root) =[:weighted]=>[
    -[:next {weight: source.input.weight, when: source.input.enabled}]->(a:tool) {output: "a"},
    -[:next {weight: 2, when: source.input.enabled}]->(b:tool) {output: "b"},
];
(a)-[:child_of]->(root);
(b)-[:child_of]->(root);
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
(router)-[:contains]->(root);
(router)-[:contains]->(turn);
(router)-[:contains]->(a);
(router)-[:contains]->(b);
(router)-[:next]->(root:task) {input: router.input};
(root) =[:repeat {count: 3}]=>[-[:next]->(turn:task) {input: context.index}];
(turn) =[:weighted]=>[
    -[:next {weight: source.input == 0 ? 1 : 0}]->(a:tool) {output: "a"},
    -[:next {weight: source.input == 0 ? 0 : 1}]->(b:tool) {output: "b"},
];
(turn)-[:child_of]->(root);
(a)-[:child_of]->(turn);
(b)-[:child_of]->(turn);
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
(retrying)-[:contains]->(root);
(retrying)-[:contains]->(lookup);
(retrying)-[:contains]->(check);
(retrying)-[:contains]->(finish);
(retrying)-[:next]->(root:task) {input: retrying.input, output: finish.output};
(root)-[:next, :feeds {value: source.input, into: target.input}]->(lookup:tool) {
    input: <-,
    output: {attempt: self.input.attempt, earlier: len(context.history.lookup)},
};
(lookup)-[:next]->(check:task) {output: {retry: lookup.output.attempt < 2, next_input: {attempt: lookup.output.attempt + 1}}};
(check) =[:exclusive]=>[
    ~[:next {when: source.output.retry, max_traversals: 3}, :feeds {when: source.output.retry, value: source.output.next_input, into: target.input}]~>(lookup),
    -[:next {when: !source.output.retry}]->(finish:task) {output: lookup.output},
];
(lookup)-[:child_of]->(root);
(check)-[:child_of]->(root);
(finish)-[:child_of]->(root);
"#;

fn flow_batch(source: &str) -> data::EventBatch {
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

fn flow_code(model: &bts_core::mocking::Model, name: &str) -> Vec<Value> {
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
    let batch = flow_batch(CYCLE_SOURCE);
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
    for result in flow_code(&model, "retrying") {
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
(joined)-[:contains]->(root);
(joined)-[:contains]->(left_task);
(joined)-[:contains]->(right_task);
(joined)-[:contains]->(finish);
(joined)-[:next]->(root:task) {input: joined.input};
(root) =[:parallel]=>[
    -[a:next]->(left_task:tool) {output: 2},
    -[b:next]->(right_task:tool) {output: 3},
]
    >[:STRATEGY {observe: "result" CONFIG} [b], [a]]>>(finish:task) {output: RESULT};
(left_task)-[:child_of]->(root);
(right_task)-[:child_of]->(root);
(finish)-[:child_of]->(root);
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
        let batch = flow_batch(&source);
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
        for generated in flow_code(&model, "joined") {
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

fn flow_code_error(model: &bts_core::mocking::Model, name: &str, message: &str) {
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
(joined)-[:contains]->(root);
(joined)-[:contains]->(left_task);
(joined)-[:contains]->(right_task);
(joined)-[:contains]->(finish);
(joined)-[:next]->(root:task) {input: joined.input};
(root) =[:repeat {count: 3}]=>[
    -[a:next]->(left_task:tool) {output: context.index + 1},
    -[b:next]->(right_task:tool) {output: context.index + 10},
]
    >[:accumulate {observe: "result"} [b], [a]]>>(finish:task) {output: context.arrivals};
(left_task)-[:child_of]->(root);
(right_task)-[:child_of]->(root);
(finish)-[:child_of]->(root);
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
                .replace(":repeat {count: 3}", ":each {items: [|{a: 1, b: 2}, {b: 2, a: 1}|]}")
                .replace(
                    ":accumulate {observe: \"result\"}",
                    ":first {observe: \"result\", correlate: context.item}",
                ),
            json!([[{"edge":"a","value":1}]]),
        ),
        (
            QUEUED_SOURCE
                .replace(":repeat {count: 3}", ":each {items: [|\"a\", \"b\"|]}")
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
        let batch = flow_batch(&source);
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
        for generated in flow_code(&model, "joined") {
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
    let ambiguous = r#"
(joined:agent) {input: true, output: finish.output};
(joined)-[:contains]->(root);
(joined)-[:contains]->(shared);
(joined)-[:contains]->(finish);
(joined)-[:next]->(root:task) {input: joined.input};
(shared:tool) {output: 1};
(root) =[:parallel]=>[-[a:next]->(shared), -[b:next]->(shared)]
    >[:accumulate {observe: "result"} [a], [b]]>>(finish:task) {output: shared.output};
(shared)-[:child_of]->(root);
(finish)-[:child_of]->(root);
"#;
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
                .replace("[a:next]", "[a:next {when: false}]")
                .replace("context.index + 1", "1")
                .replace("context.index + 10", "10")
                .replace(":accumulate", ":quorum")
                .replace("observe: \"result\"", "observe: \"result\", count: 2"),
            "joined",
            "quorum group closed",
        ),
        (ambiguous.to_owned(), "joined", "ambiguous occurrence"),
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
        flow_code_error(&model, name, message);
    }
}

#[test]
fn cycle_groups_keep_limits_and_values_separate_inside_repetition() {
    let source = CYCLE_SOURCE.replace("output: finish.output", "output: \"done\"")
        .replace("(root)-[:next, :feeds {value: source.input, into: target.input}]->(lookup:tool)",
            "(retrying)-[:contains]->(turn); (root) =[:repeat {count: 2}]=>[-[:next]->(turn:task) {input: root.input}]; (turn)-[:child_of]->(root); (turn)-[:next, :feeds {value: source.input, into: target.input}]->(lookup:tool)")
        .replace("(lookup)-[:child_of]->(root)","(lookup)-[:child_of]->(turn)")
        .replace("(check)-[:child_of]->(root)","(check)-[:child_of]->(turn)")
        .replace("(finish)-[:child_of]->(root)","(finish)-[:child_of]->(turn)")
        .replace("max_traversals: 3","max_traversals: 2");
    let batch = flow_batch(&source);
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
    for generated in flow_code(&lower(&source).unwrap(), "retrying") {
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
        .replace("=[:exclusive]", "=[:weighted]")
        .replace("when: source.output.retry", "weight: 1, when: source.output.retry")
        .replace("when: !source.output.retry", "weight: 1, when: !source.output.retry")
        .replace("(lookup:tool)", "(lookup:tool {name: \"patient_lookup\"})");
    let batch = flow_batch(&source);
    assert_eq!(
        batch.events.last().unwrap().output.as_ref().unwrap().project(&()).unwrap(),
        json!({"attempt":2,"earlier":2})
    );
    for generated in flow_code(&lower(&source).unwrap(), "retrying") {
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
    flow_code_error(&model, "retrying", "execution budget");
}

#[test]
fn junction_results_wait_for_destination_payload_dependencies() {
    let source = QUEUED_SOURCE
        .replace("=[:repeat {count: 3}]", "=[:parallel]")
        .replace("output: context.index + 10", "output: 10")
        .replace("output: context.index + 1", "output: child.output");
    let source = format!(
        "{source}\n(joined)-[:contains]->(child); (left_task)-[:next]->(child:tool) {{output: 1}}; (child)-[:child_of]->(left_task);"
    );
    let expected = json!([{ "edge":"b", "value":10 },{ "edge":"a", "value":1 }]);
    let batch = flow_batch(&source);
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
    for generated in flow_code(&lower(&source).unwrap(), "joined") {
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
(route)-[:contains]->(request);
(route)-[:contains]->(answer);
(route)-[:next]->(request:task) {input: route.input, output: "done"};
(request)=[:fork]=>[-[:next]->(answer:tool)];
(answer)-[:child_of]->(request);
"#;
    for (source, located, message) in [
        (
            SOURCE.replace("(request)-[:next, :feeds", "(request)-[:next {weight: 2}, :feeds"),
            "2",
            "configured next edges are not supported by mocking yet",
        ),
        (
            SOURCE.replace("(request)-[:next, :feeds", "(request)-[:requires, :feeds"),
            ":requires",
            "edge kind \"requires\" is not supported by mocking yet",
        ),
        (
            fork.to_owned(),
            "=[:fork]=>[-[:next]->(answer:tool)]",
            "fork kind \"fork\" or its configuration is not supported by mocking yet",
        ),
    ] {
        let errors = Bundle::compile_module(&source, false).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        let range = errors[0].r#where;
        assert_eq!(errors[0].what, message);
        assert_eq!(&source[range.start..range.end], located, "{errors:#?}");
        assert_eq!(range.source, 0);
    }
    // the probe misspellings from the graph contracts plan never reach the consumer
    for (source, located, message) in [
        (
            SOURCE.replace("(request)-[:next, :feeds", "(request)-[:next {whne: true}, :feeds"),
            "whne",
            "unknown property `whne` for built-in kind `next`, did you mean `when`?",
        ),
        (
            SOURCE.replace("(request)-[:next, :feeds", "(request)-[:nxet, :feeds"),
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
        let errors = Bundle::compile_module(&source, false).unwrap_err();
        let found = errors
            .iter()
            .find(|error| error.what == message)
            .unwrap_or_else(|| panic!("{errors:#?}"));
        assert_eq!(&source[found.r#where.start..found.r#where.end], located);
    }
}
