#![cfg(target_arch = "wasm32")]

use serde_json::Value;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

fn validate(source: &str) -> Value {
    serde_wasm_bindgen::from_value(bts_wasm::validate(source).expect("validation must not throw")).unwrap()
}

#[wasm_bindgen_test]
fn accepts_valid_source() {
    assert_eq!(
        validate(include_str!("fixtures/valid.bt")),
        serde_json::json!({"valid": true, "diagnostics": []})
    );
}

#[wasm_bindgen_test]
fn validates_agent_entrypoints() {
    let valid = r#"agent "support" { entrypoint = trace.request } trace "request" { output = "hello" }"#;
    assert_eq!(validate(valid), serde_json::json!({"valid": true, "diagnostics": []}));
    for source in [
        r#"agent "support" {} trace "request" {}"#,
        r#"agent "support" { entrypoint = trace.missing } trace "request" {}"#,
        r#"agent "support" { entrypoint = trace.request.output } trace "request" { output = "hello" }"#,
    ] {
        let result = validate(source);
        assert_eq!(result["valid"], false, "{source}");
        assert_eq!(result["diagnostics"][0]["phase"], "validation", "{source}");
    }
}

#[wasm_bindgen_test]
fn reports_syntax_errors() {
    let result = validate(include_str!("fixtures/invalid.bt"));
    assert_eq!(result["valid"], false);
    assert_eq!(result["diagnostics"][0]["phase"], "parsing");
    assert!(!result["diagnostics"][0]["message"].as_str().unwrap().is_empty());
}

#[wasm_bindgen_test]
fn reports_semantic_errors() {
    let result = validate(r#"trace "hello" { output = var.missing }"#);
    assert_eq!(result["valid"], false);
    assert_eq!(result["diagnostics"][0]["phase"], "validation");
}

#[wasm_bindgen_test]
fn preserves_unicode_byte_ranges() {
    let source = "trace \"café 🦀\" { input = }";
    let result = validate(source);
    let diagnostic = &result["diagnostics"][0];
    let start = source.find('}').unwrap();
    assert_eq!(diagnostic["phase"], "parsing");
    assert_eq!(diagnostic["range"], serde_json::json!({"start": start, "end": start + 1}));
}

#[wasm_bindgen_test]
fn calls_do_not_retain_errors() {
    assert_eq!(validate(include_str!("fixtures/invalid.bt"))["valid"], false);
    assert_eq!(validate(include_str!("fixtures/valid.bt"))["valid"], true);
}

#[wasm_bindgen_test]
fn reports_lexing_errors() {
    let result = validate("trace \"unterminated");
    assert_eq!(result["valid"], false);
    assert_eq!(result["diagnostics"][0]["phase"], "lexing");
}

#[wasm_bindgen_test]
fn empty_source_matches_cli_validation() {
    let result = validate("");
    assert_eq!(result["valid"], false);
    assert!(!result["diagnostics"].as_array().unwrap().is_empty());
}

fn build_result(source: &str, options: &Value) -> Value {
    let options = serde::Serialize::serialize(options, &serde_wasm_bindgen::Serializer::json_compatible()).unwrap();
    serde_wasm_bindgen::from_value(bts_wasm::build(source, options).expect("build must return failures as data")).unwrap()
}

struct BuildTestFiles(std::collections::BTreeMap<String, String>);

impl bts_core::mocking::code::ExistingFiles for BuildTestFiles {
    fn read(&self, path: &std::path::Path) -> std::io::Result<Option<String>> {
        Ok(self.0.get(path.to_str().unwrap()).cloned())
    }
}

#[wasm_bindgen_test]
fn build_matches_shared_core_planning_for_both_languages() {
    let source = include_str!("../../core/tests/fixtures/building.bt");
    for (language, core_language) in [(None, None), (Some("typescript"), Some(bts_lang::ScorerLang::Typescript))] {
        let mut options = serde_json::json!({ "project": "Browser Project", "root": "browser-codebase" });
        if let Some(language) = language {
            options["lang"] = language.into();
        }
        let result = build_result(source, &options);
        assert_eq!(result["ok"], true, "{result}");

        let model = bts_lang::semantics::compile(source).unwrap();
        let target = bts_core::providers::braintrust::mocking::code::CodeTarget::new("Browser Project");
        let assembly = bts_core::mocking::code::assemble(
            &model.scorers,
            core_language,
            &bts_core::mocking::code::languages::Languages,
            &target,
        )
        .unwrap();
        let existing = BuildTestFiles(Default::default());
        let context = bts_core::mocking::code::CodebaseContext {
            root: std::path::Path::new("browser-codebase"),
            files: &existing,
        };
        let plan = bts_core::mocking::code::plan_codebase(&assembly, &target, &context).unwrap();
        let files: Vec<_> = plan
            .files
            .iter()
            .map(|file| {
                serde_json::json!({
                    "path": file.path.to_str().unwrap(),
                    "contents": file.contents,
                    "slugs": file.slugs,
                })
            })
            .collect();
        assert_eq!(result["value"]["files"], serde_json::json!(files));
        assert_eq!(result["value"]["removals"], serde_json::json!([]));
    }
}

#[wasm_bindgen_test]
fn builds_callable_agents_without_a_project_in_both_languages() {
    use bts_core::mocking::code::{self, AgentTarget};
    let source = r#"
agent "hello" { entrypoint = trace.hello }
trace "hello" {
    input = { name = "Ada" }
    output = tool.greet.output
    tool "greet" { output = "Hello ${trace.input.name}" }
}
"#;
    for (lang, extension, language) in [
        ("python", "py", bts_lang::ScorerLang::Python),
        ("typescript", "mts", bts_lang::ScorerLang::Typescript),
    ] {
        let result = build_result(source, &serde_json::json!({ "target": "agent", "lang": lang }));
        assert_eq!(result["ok"], true, "{result}");
        let files = result["value"]["files"].as_array().unwrap();
        assert!(
            files
                .iter()
                .any(|file| file["path"] == format!("src/agents/hello_agent.{extension}"))
        );
        assert!(
            files
                .iter()
                .any(|file| file["path"] == format!("src/agents/hello_agent_runtime.{extension}"))
        );
        let model = bts_lang::semantics::compile(source).unwrap();
        let target = bts_core::providers::braintrust::mocking::code::CodeTarget::new("");
        let assembly = code::assemble_agents(&model, language, &target).unwrap();
        let assembly = target.agent_assembly(&model, language, assembly).unwrap();
        let existing = BuildTestFiles(Default::default());
        let context = code::CodebaseContext {
            root: std::path::Path::new("bts-scorers"),
            files: &existing,
        };
        let plan = target.agent_codebase(&assembly.files, language, &context).unwrap();
        let expected: Vec<_> = plan
            .files
            .iter()
            .map(|file| {
                serde_json::json!({
                    "path": file.path.to_str().unwrap(), "contents": file.contents, "slugs": file.slugs,
                })
            })
            .collect();
        assert_eq!(files, &expected);
    }
}

const AGENT_EVALS: &str = r#"
agent "helper" { entrypoint = trace.helper }
agent "first" { entrypoint = trace.first }
agent "second" { entrypoint = trace.second }
trace "helper" { output = "helper" }
trace "first" { input = 1 output = self.input }
trace "second" { input = 2 output = self.input }
scorer "quality" { code { score = output == expected ? 1 : 0 } }
dataset "cases" {
    case "sampled" { input = range(1, 1000000) expected = 1 }
}
experiment "first" { dataset = dataset.cases task = trace.first scorers = [scorer.quality] }
experiment "second" { dataset = dataset.cases task = trace.second scorers = [scorer.quality] }
"#;

#[wasm_bindgen_test]
fn agent_selections_limit_evals_and_seed_the_dataset_snapshot() {
    let helper = build_result(
        AGENT_EVALS,
        &serde_json::json!({ "target": "agent", "select": ["agent.helper"] }),
    );
    assert_eq!(helper["ok"], true, "{helper}");
    let files = helper["value"]["files"].as_array().unwrap();
    assert!(files.iter().any(|file| file["path"] == "src/agents/helper_agent.py"));
    assert!(
        !files
            .iter()
            .any(|file| file["path"].as_str().unwrap().starts_with("src/evals/"))
    );
    assert!(
        !files
            .iter()
            .any(|file| file["path"].as_str().unwrap().starts_with("src/scorers/"))
    );
    let helper_files = files
        .iter()
        .map(|file| (file["path"].as_str().unwrap().to_owned(), file["contents"].clone()))
        .collect::<serde_json::Map<_, _>>();
    let helper_runtime = files
        .iter()
        .find(|file| file["path"].as_str().unwrap().ends_with("_runtime.py"))
        .unwrap()["path"]
        .clone();
    for (lang, extension, eval_path, definition_path) in [
        ("python", "py", "src/evals/first_eval.py", "src/evals/first_definition.py"),
        (
            "typescript",
            "mts",
            "src/evals/first.eval.ts",
            "src/evals/first.definition.ts",
        ),
    ] {
        let options = serde_json::json!({ "target": "agent", "project": "Browser Project", "lang": lang, "select": ["experiment.first"], "files": helper_files });
        let default = build_result(AGENT_EVALS, &options);
        assert_eq!(default["ok"], true, "{default}");
        let mut seeded = options.clone();
        seeded["seed"] = "42".into();
        assert_eq!(default, build_result(AGENT_EVALS, &seeded));
        let files = default["value"]["files"].as_array().unwrap();
        assert!(!files.iter().any(|file| file["path"] == helper_runtime));
        assert!(
            files
                .iter()
                .any(|file| file["path"] == format!("src/agents/first_agent.{extension}"))
        );
        assert!(
            !files
                .iter()
                .any(|file| file["path"] == format!("src/agents/second_agent.{extension}"))
        );
        let entrypoint = files.iter().find(|file| file["path"] == eval_path).unwrap();
        assert!(entrypoint["contents"].as_str().unwrap().contains("evaluate()"));
        let evaluation = files.iter().find(|file| file["path"] == definition_path).unwrap();
        assert!(evaluation["contents"].as_str().unwrap().contains("Eval("));
        seeded["seed"] = "43".into();
        let different = build_result(AGENT_EVALS, &seeded);
        assert_eq!(different["ok"], true, "{different}");
        let changed = different["value"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == definition_path)
            .unwrap();
        assert_ne!(evaluation["contents"], changed["contents"]);
        seeded["seed"] = u64::MAX.to_string().into();
        assert_eq!(build_result(AGENT_EVALS, &seeded)["ok"], true);
    }
}

#[wasm_bindgen_test]
fn agent_builds_return_guarded_removals_with_safe_paths() {
    let previous = "# Generated by bts; edits will be overwritten.\ndef run(): return 'old'\n";
    let result = build_result(
        r#"agent "hello" { entrypoint = trace.hello } trace "hello" { output = "hello" }"#,
        &serde_json::json!({ "target": "agent", "files": { "hello_agent.py": previous, "hello_agent_agent.py": "# user code\n" } }),
    );
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        result["value"]["removals"],
        serde_json::json!([{ "path": "hello_agent.py", "expected": previous }])
    );
    let mut paths = std::collections::BTreeSet::new();
    for file in result["value"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .chain(result["value"]["removals"].as_array().unwrap())
    {
        let path = file["path"].as_str().unwrap();
        assert!(!path.is_empty() && !path.contains(['\\', ':']));
        assert!(path.split('/').all(|part| !matches!(part, "" | "." | "..")));
        assert!(paths.insert(path));
    }
}

#[wasm_bindgen_test]
fn agent_eval_snapshots_enforce_the_output_languages_integer_range() {
    let source = AGENT_EVALS.replace("range(1, 1000000)", "9007199254740993");
    for (lang, valid) in [("python", true), ("typescript", false)] {
        let result = build_result(
            &source,
            &serde_json::json!({ "target": "agent", "project": "Browser Project", "lang": lang, "select": ["experiment.first"] }),
        );
        assert_eq!(result["ok"], valid, "{result}");
        if !valid {
            assert_eq!(result["error"]["stage"], "build");
            assert!(result["error"]["message"].as_str().unwrap().contains("safe integer"));
            assert!(result.get("value").is_none());
        }
    }
}

#[wasm_bindgen_test]
fn agent_build_errors_do_not_return_partial_files() {
    for (source, options, stage) in [
        (r#"trace "unbound" {}"#, serde_json::json!({ "target": "agent" }), "build"),
        (AGENT_EVALS, serde_json::json!({ "target": "agent" }), "options"),
        (
            AGENT_EVALS,
            serde_json::json!({ "target": "agent", "select": ["agent.missing"] }),
            "build",
        ),
        (
            AGENT_EVALS,
            serde_json::json!({ "target": "agent", "select": ["scorer.quality"] }),
            "build",
        ),
        (
            AGENT_EVALS,
            serde_json::json!({ "target": "agent", "select": ["agent.first.output"] }),
            "options",
        ),
    ] {
        let result = build_result(source, &options);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["error"]["stage"], stage);
        assert!(result.get("value").is_none());
    }
}

#[wasm_bindgen_test]
fn build_merges_virtual_manifests_and_preserves_existing_tsconfig() {
    let source = r#"scorer "quality" { code { score = 0.5 } }"#;
    let package = serde_json::json!({
        "name": "existing-package",
        "scripts": { "check": "custom-check" },
        "dependencies": { "braintrust": "workspace:*", "extra": "1" }
    })
    .to_string();
    let result = build_result(
        source,
        &serde_json::json!({
            "project": "Browser Project",
            "lang": "typescript",
            "files": {
                "package.json": package,
                "tsconfig.json": "{\"compilerOptions\":{\"strict\":false}}"
            }
        }),
    );
    assert_eq!(result["ok"], true, "{result}");
    let files = result["value"]["files"].as_array().unwrap();
    assert!(!files.iter().any(|file| file["path"] == "tsconfig.json"));
    let package = files.iter().find(|file| file["path"] == "package.json").unwrap();
    let package: Value = serde_json::from_str(package["contents"].as_str().unwrap()).unwrap();
    assert_eq!(package["name"], "existing-package");
    assert_eq!(package["scripts"]["check"], "custom-check");
    assert_eq!(package["dependencies"]["braintrust"], "workspace:*");
    assert_eq!(package["dependencies"]["extra"], "1");
    assert!(package["dependencies"]["zod"].is_string());
    assert!(package["devDependencies"]["typescript"].is_string());
}

#[wasm_bindgen_test]
fn build_returns_staged_option_errors() {
    let source = r#"scorer "quality" { code { score = 0.5 } }"#;
    for options in [
        serde_json::json!({}),
        serde_json::json!({ "project": "" }),
        serde_json::json!({ "project": "Browser Project", "lang": "rust" }),
        serde_json::json!({ "project": "Browser Project", "files": { "../package.json": "{}" } }),
        serde_json::json!({ "project": "Browser Project", "files": { "C:\\package.json": "{}" } }),
        serde_json::json!({ "project": "Browser Project", "files": { "a/../package.json": "{}" } }),
        serde_json::json!({ "project": "Browser Project", "files": { "bad\u{0000}.json": "{}" } }),
        serde_json::json!({ "project": "Browser Project", "target": "agents" }),
        serde_json::json!({ "project": "Browser Project", "select": ["agent.first"] }),
        serde_json::json!({ "target": "agent", "select": "agent.first" }),
        serde_json::json!({ "target": "agent", "select": [4] }),
        serde_json::json!({ "target": "agent", "seed": 42 }),
        serde_json::json!({ "target": "agent", "seed": "-1" }),
        serde_json::json!({ "target": "agent", "seed": "+1" }),
        serde_json::json!({ "target": "agent", "seed": "18446744073709551616" }),
        serde_json::json!({ "target": "agent", "seeds": "42" }),
    ] {
        let result = build_result(source, &options);
        assert_eq!(result["ok"], false, "{options}");
        assert_eq!(result["error"]["stage"], "options", "{options}: {result}");
        assert!(!result["error"]["message"].as_str().unwrap().is_empty());
        assert!(result.get("value").is_none());
    }
}

#[wasm_bindgen_test]
fn build_returns_planning_errors_without_partial_files() {
    for (source, options) in [
        (r#"trace "run" {}"#, serde_json::json!({ "project": "Browser Project" })),
        (
            r#"scorer "quality" { code { score = 0.5 } }"#,
            serde_json::json!({
                "project": "Browser Project",
                "lang": "typescript",
                "files": { "package.json": "{" }
            }),
        ),
    ] {
        let result = build_result(source, &options);
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["stage"], "build");
        assert!(!result["error"]["message"].as_str().unwrap().is_empty());
        assert!(result.get("value").is_none());
    }
}

fn generate_result(source: &str, options: &Value) -> Value {
    let options = serde::Serialize::serialize(options, &serde_wasm_bindgen::Serializer::json_compatible()).unwrap();
    serde_wasm_bindgen::from_value(bts_wasm::generate(source, options).expect("generation must return failures as data"))
        .unwrap()
}

#[wasm_bindgen_test]
fn rejects_random_string_lengths_that_do_not_fit_wasm() {
    let options = serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" });
    for expression in ["hex(4294967296)", "alphanum(4294967297)"] {
        let source = format!(r#"trace "run" {{ output = {expression} }}"#);
        let validation = validate(&source);
        assert_eq!(validation["valid"], false, "{expression}: {validation}");
        assert_eq!(validation["diagnostics"][0]["phase"], "validation");

        let generated = generate_result(&source, &options);
        assert_eq!(generated["ok"], false, "{expression}: {generated}");
        assert_eq!(generated["error"]["stage"], "compile");
        assert!(generated.get("value").is_none());

        let source = format!(r#"scorer "quality" {{ code {{ score = {expression} == "" ? 1 : 0 }} }}"#);
        for language in ["python", "typescript"] {
            let built = build_result(
                &source,
                &serde_json::json!({ "project": "Browser Project", "lang": language }),
            );
            assert_eq!(built["ok"], false, "{expression}, {language}: {built}");
            assert_eq!(built["error"]["stage"], "compile");
            assert_eq!(built["error"]["diagnostics"][0]["phase"], "validation");
            assert!(built.get("value").is_none());
        }
    }
}

#[wasm_bindgen_test]
fn clamps_large_slice_bounds_for_literals_fields_and_iterations() {
    let options = serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" });
    for bound in [4_294_967_296_i64, i64::MAX] {
        for bound in [bound.to_string(), format!("range({bound}, {bound})")] {
            let source = format!(
                r#"
                trace "run" {{
                    input = [1, 2]
                    output = {{
                        literal_start = [1, 2][{bound}:]
                        literal_end = [1, 2][:{bound}]
                        field_start = self.input[{bound}:]
                        field_end = self.input[:{bound}]
                        iteration_start = repeat.rounds[{bound}:].task.step.output
                        iteration_end = repeat.rounds[:{bound}].task.step.output
                    }}
                    repeat "rounds" {{
                        count = 2
                        task "step" {{ output = repeat.index }}
                    }}
                }}
                "#
            );
            let validation = validate(&source);
            assert_eq!(validation["valid"], true, "{bound}: {validation}");
            let generated = generate_result(&source, &options);
            assert_eq!(generated["ok"], true, "{bound}: {generated}");
            assert_eq!(
                generated["value"]["payload"]["events"][0]["output"],
                serde_json::json!({
                    "literal_start": [], "literal_end": [1, 2],
                    "field_start": [], "field_end": [1, 2],
                    "iteration_start": [], "iteration_end": [0, 1],
                }),
                "{bound}"
            );
        }
    }
}

#[wasm_bindgen_test]
fn rejects_negative_and_noninteger_slice_bounds() {
    let options = serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" });
    for bound in ["-1", "1.5"] {
        for dynamic in [false, true] {
            let expression = if dynamic {
                format!("range({bound}, {bound})")
            } else {
                bound.to_owned()
            };
            let source = format!(r#"trace "run" {{ output = [1, 2][:{expression}] }}"#);
            let validation = validate(&source);
            assert_eq!(validation["valid"], dynamic, "{expression}: {validation}");
            let generated = generate_result(&source, &options);
            assert_eq!(generated["ok"], false, "{expression}: {generated}");
            assert_eq!(generated["error"]["stage"], if dynamic { "generate" } else { "compile" });
            assert_eq!(
                generated["error"]["diagnostics"][0]["phase"],
                if dynamic { "generation" } else { "validation" }
            );
            assert!(generated.get("value").is_none());
        }
    }
}

#[wasm_bindgen_test]
fn exposes_shared_reference_and_browser_authoring_guidance() {
    let reference: Value = serde_wasm_bindgen::from_value(bts_wasm::spec().unwrap()).unwrap();
    assert_eq!(reference, serde_json::to_value(bts_lang::spec::SPEC).unwrap());
    let skill = bts_wasm::skill();
    assert!(skill.starts_with(&bts_core::skill::render(&bts_lang::spec::SPEC)));
    assert!(skill.contains("validate(source)"));
    assert!(skill.contains("generate(source, options)"));
    assert!(skill.contains("build(source, options)"));
    assert!(!skill.contains("## CLI commands"));
    assert!(!skill.contains("bts write"));
    assert!(!skill.contains("bt functions push"));
    assert!(!skill.contains("tools()"));
}

#[wasm_bindgen_test]
fn generation_matches_shared_core_with_filtering_tokens_and_maximum_seed() {
    use bts_core::mocking::data::{self, Format as _};
    use bts_core::providers::braintrust::mocking::data::Format;
    use bts_core::selection::filter::WriteFilter;
    use std::time::{Duration, UNIX_EPOCH};

    let source = r#"
        trace "support" {
            input = {
                message = "Where can I download my invoice?"
                account = { id = range(100, 999), active = true }
                history = []
            }
            output = task.answer.output
            metadata = { channel = "web", attempt = trace.index }
            tags = ["billing"]

            task "answer" {
                output = {
                    status = "resolved"
                    message = "Open Billing and select the invoice you need."
                    follow_up = null
                }

                llm "draft" {
                    input = "hello"
                    output = "hello"
                    duration = 0.25
                    metrics = {
                        literal_tokens = tokens("hello")
                        prompt_tokens = tokens(self.input)
                        completion_tokens = tokens(self.output)
                    }
                }

                tool "lookup" {
                    input = { query = "invoice" }
                    output = { found = true }
                }
            }
        }
    "#;
    let filter = r#"block.kind != "tool""#;
    let result = generate_result(
        source,
        &serde_json::json!({
            "count": 3,
            "over_ms": 60000,
            "until_ms": 1735689600000_u64,
            "seed": "18446744073709551615",
            "distribution": "sine",
            "filter": filter,
            "id_prefix": "browser-parity"
        }),
    );
    assert_eq!(result["ok"], true, "{result}");
    let value = &result["value"];
    assert_eq!(value["provider"], "braintrust");
    assert_eq!(value["seed"], u64::MAX.to_string());
    assert_eq!(value["trace_count"], 3);
    assert_eq!(value["event_count"], 9);
    assert_eq!(value["attachments"], serde_json::json!([]));

    let mut batch = data::generate_filtered(
        bts_lang::semantics::compile(source).unwrap(),
        3,
        Duration::from_secs(60),
        data::Distribution::Sine,
        UNIX_EPOCH + Duration::from_millis(1735689600000),
        u64::MAX,
        &filter.parse::<WriteFilter>().unwrap(),
        &Format,
    )
    .unwrap();
    batch.assign_stable_ids("browser-parity");
    let encoded = Format.encode(&batch).unwrap();
    let expected: Value = serde_wasm_bindgen::from_value(
        serde::Serialize::serialize(&encoded, &serde_wasm_bindgen::Serializer::json_compatible()).unwrap(),
    )
    .unwrap();
    assert_eq!(value["payload"], expected);

    let events = value["payload"]["events"].as_array().unwrap();
    assert!(events.iter().all(|event| event["span_attributes"]["type"] != "tool"));
    let drafts: Vec<_> = events
        .iter()
        .filter(|event| event["span_attributes"]["name"] == "draft")
        .collect();
    assert_eq!(drafts.len(), 3);
    for draft in drafts {
        assert_eq!(draft["metrics"]["literal_tokens"], 1);
        assert_eq!(draft["metrics"]["prompt_tokens"], 1);
        assert_eq!(draft["metrics"]["completion_tokens"], 1);
    }
}

#[wasm_bindgen_test]
fn relative_count_and_absolute_rate_windows_generate_the_same_population() {
    let source = r#"trace "run" { output = range(1, 5) }"#;
    let relative = generate_result(
        source,
        &serde_json::json!({
            "count": 2, "over_ms": 2000, "offset_ms": 1000, "until_ms": 5000,
            "seed": "42", "id_prefix": "window-parity"
        }),
    );
    let absolute = generate_result(
        source,
        &serde_json::json!({
            "rate_per_second": 1, "start_ms": 2000, "until_ms": 4000,
            "seed": "42", "id_prefix": "window-parity"
        }),
    );
    assert_eq!(relative["ok"], true, "{relative}");
    assert_eq!(absolute["ok"], true, "{absolute}");
    assert_eq!(absolute["value"]["trace_count"], 2);
    assert_eq!(relative["value"], absolute["value"]);
}

#[wasm_bindgen_test]
fn generation_preserves_compile_diagnostics_and_unicode_byte_ranges() {
    let source = "trace \"café 🦀\" { input = }";
    let result = generate_result(
        source,
        &serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" }),
    );
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["stage"], "compile");
    let diagnostic = &result["error"]["diagnostics"][0];
    let start = source.find('}').unwrap();
    assert_eq!(diagnostic["phase"], "parsing");
    assert_eq!(diagnostic["range"], serde_json::json!({ "start": start, "end": start + 1 }));
    assert!(!diagnostic["message"].as_str().unwrap().is_empty());
    assert!(result.get("value").is_none());
}

#[wasm_bindgen_test]
fn generation_rejects_ambiguous_or_unrepresentable_options() {
    let source = r#"trace "run" {}"#;
    for overrides in [
        serde_json::json!({ "rate_per_second": 1 }),
        serde_json::json!({ "start_ms": 1000 }),
        serde_json::json!({ "over_ms": -1 }),
        serde_json::json!({ "seed": "18446744073709551616" }),
    ] {
        let mut options = serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" });
        options
            .as_object_mut()
            .unwrap()
            .extend(overrides.as_object().unwrap().clone());
        let result = generate_result(source, &options);
        assert_eq!(result["ok"], false, "{options}");
        assert_eq!(result["error"]["stage"], "options");
        assert!(!result["error"]["message"].as_str().unwrap().is_empty());
        assert!(result.get("value").is_none());
    }
}

#[wasm_bindgen_test]
fn generation_returns_authored_attachment_descriptors_without_filesystem_resolution() {
    let source = r#"
        trace "review" {
            input = attachment("browser-only/missing/report.pdf", "application/pdf")
            output = self.input
        }
    "#;
    let result = generate_result(
        source,
        &serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" }),
    );
    assert_eq!(result["ok"], true, "{result}");
    let attachments = result["value"]["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    let attachment = &attachments[0];
    assert_eq!(attachment["path"], "browser-only/missing/report.pdf");
    assert_eq!(attachment["filename"], "report.pdf");
    assert_eq!(attachment["content_type"], "application/pdf");
    assert!(!attachment["key"].as_str().unwrap().is_empty());
    let event = &result["value"]["payload"]["events"][0];
    assert_eq!(event["input"]["type"], "braintrust_attachment");
    assert_eq!(event["input"]["key"], attachment["key"]);
    assert_eq!(event["input"], event["output"]);
}

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
export function writeTestTransport(mode) {
    const fixture = { requests: [], aborted: 0 };
    fixture.send = request => {
        const payload = JSON.parse(new TextDecoder().decode(request.body));
        fixture.requests.push({
            method: request.method,
            path: request.path,
            headers: request.headers,
            payload,
            body_is_uint8: request.body instanceof Uint8Array,
            timeout_ms: request.timeout_ms,
            signal_is_abort: request.signal instanceof AbortSignal
        });
        if (mode === "timeout") {
            return new Promise((resolve, reject) => {
                request.signal.addEventListener("abort", () => {
                    fixture.aborted += 1;
                    reject(new DOMException("aborted", "AbortError"));
                }, { once: true });
            });
        }
        if (mode === "rejected") {
            return Promise.resolve({ status: 400, body: '{"message":"invalid trace"}' });
        }
        if (mode === "malformed_body") {
            return Promise.resolve({ status: 200, body: "not json" });
        }
        if (mode === "malformed_response") {
            return Promise.resolve({ status: 200, body: { unexpected: true } });
        }
        const body = new TextEncoder().encode(JSON.stringify({
            row_ids: payload.events.map((event, index) => `row-${index + 1}`)
        }));
        return new Promise(resolve => setTimeout(() => resolve({ status: 200, headers: {}, body }), 0));
    };
    return fixture;
}

export function writeTestCallback(fixture) {
    return fixture.send;
}

export function writeTestState(fixture) {
    return { requests: fixture.requests, aborted: fixture.aborted };
}
"#)]
extern "C" {
    #[wasm_bindgen::prelude::wasm_bindgen(js_name = writeTestTransport)]
    fn write_test_transport(mode: &str) -> wasm_bindgen::JsValue;
    #[wasm_bindgen::prelude::wasm_bindgen(js_name = writeTestCallback)]
    fn write_test_callback(fixture: &wasm_bindgen::JsValue) -> js_sys::Function;
    #[wasm_bindgen::prelude::wasm_bindgen(js_name = writeTestState)]
    fn write_test_state(fixture: &wasm_bindgen::JsValue) -> wasm_bindgen::JsValue;
}

fn write_generation_options() -> Value {
    serde_json::json!({
        "count": 2, "over_ms": 1000, "until_ms": 5000,
        "seed": "42", "id_prefix": "browser-write"
    })
}

fn write_options() -> Value {
    serde_json::json!({ "project_id": "00000000-0000-0000-0000-000000000000" })
}

async fn write_result(
    source: &str,
    generation_options: &Value,
    write_options: &Value,
    fixture: &wasm_bindgen::JsValue,
) -> Value {
    let to_js = |value: &Value| serde::Serialize::serialize(value, &serde_wasm_bindgen::Serializer::json_compatible()).unwrap();
    let result = bts_wasm::write(
        source,
        to_js(generation_options),
        to_js(write_options),
        write_test_callback(fixture),
    )
    .await
    .expect("write must return failures as data");
    serde_wasm_bindgen::from_value(result).unwrap()
}

fn write_state(fixture: &wasm_bindgen::JsValue) -> Value {
    serde_wasm_bindgen::from_value(write_test_state(fixture)).unwrap()
}

#[wasm_bindgen_test]
async fn write_sends_shared_payload_bytes_and_returns_acknowledged_rows() {
    let source = r#"trace "run" { output = "hello" }"#;
    let fixture = write_test_transport("success");
    let generation_options = write_generation_options();
    let result = write_result(source, &generation_options, &write_options(), &fixture).await;
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        result["value"],
        serde_json::json!({
            "provider": "braintrust", "seed": "42", "trace_count": 2, "event_count": 2,
            "row_ids": ["row-1", "row-2"]
        })
    );
    let state = write_state(&fixture);
    let requests = state["requests"].as_array().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["method"], "POST");
    assert_eq!(
        request["path"],
        "/v1/project_logs/00000000-0000-0000-0000-000000000000/insert"
    );
    assert_eq!(request["headers"]["content-type"], "application/json");
    assert_eq!(request["body_is_uint8"], true);
    assert_eq!(request["signal_is_abort"], true);
    assert_eq!(request["timeout_ms"], 30000);
    assert_eq!(
        request["payload"],
        generate_result(source, &generation_options)["value"]["payload"]
    );
    assert_eq!(state["aborted"], 0);
}

#[wasm_bindgen_test]
async fn write_reports_final_http_rejections_without_retrying() {
    let fixture = write_test_transport("rejected");
    let result = write_result(r#"trace "run" {}"#, &write_generation_options(), &write_options(), &fixture).await;
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["stage"], "write");
    assert!(result["error"]["message"].as_str().unwrap().contains("400"));
    assert_eq!(
        result["error"]["details"],
        serde_json::json!({ "row_ids": [], "row_count": 0 })
    );
    assert_eq!(write_state(&fixture)["requests"].as_array().unwrap().len(), 1);
    assert!(result.get("value").is_none());
}

#[wasm_bindgen_test]
async fn write_times_out_and_aborts_the_host_request() {
    let fixture = write_test_transport("timeout");
    let mut options = write_options();
    options["request_timeout_ms"] = 10.into();
    options["retry_attempts"] = 1.into();
    let result = write_result(r#"trace "run" {}"#, &write_generation_options(), &options, &fixture).await;
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["stage"], "write");
    assert_eq!(result["error"]["details"]["row_count"], 0);
    let state = write_state(&fixture);
    assert_eq!(state["requests"].as_array().unwrap().len(), 1);
    assert_eq!(state["requests"][0]["timeout_ms"], 10);
    assert_eq!(state["aborted"], 1);
}

#[wasm_bindgen_test]
async fn write_rejects_invalid_inputs_and_attachments_before_transport() {
    let good_source = r#"trace "run" {}"#;
    let mut missing_seed = write_generation_options();
    missing_seed.as_object_mut().unwrap().remove("seed");
    let mut invalid_project = write_options();
    invalid_project["project_id"] = "not-a-project-id".into();
    let mut invalid_retries = write_options();
    invalid_retries["retry_attempts"] = 0.into();
    let mut invalid_timeout = write_options();
    invalid_timeout["request_timeout_ms"] = 0.into();
    for (source, generation_options, options, stage) in [
        (
            r#"trace "run" { output = }"#,
            write_generation_options(),
            write_options(),
            "compile",
        ),
        (good_source, missing_seed, write_options(), "options"),
        (good_source, write_generation_options(), invalid_project, "options"),
        (good_source, write_generation_options(), invalid_retries, "options"),
        (good_source, write_generation_options(), invalid_timeout, "options"),
        (
            r#"trace "run" { input = attachment("missing/report.pdf", "application/pdf") }"#,
            write_generation_options(),
            write_options(),
            "write",
        ),
    ] {
        let fixture = write_test_transport("success");
        let result = write_result(source, &generation_options, &options, &fixture).await;
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["error"]["stage"], stage, "{result}");
        assert!(!result["error"]["message"].as_str().unwrap().is_empty());
        assert!(write_state(&fixture)["requests"].as_array().unwrap().is_empty());
        assert!(result.get("value").is_none());
    }
}

#[wasm_bindgen_test]
async fn write_reports_malformed_transport_responses_as_failures() {
    for mode in ["malformed_body", "malformed_response"] {
        let fixture = write_test_transport(mode);
        let result = write_result(r#"trace "run" {}"#, &write_generation_options(), &write_options(), &fixture).await;
        assert_eq!(result["ok"], false, "{mode}: {result}");
        assert_eq!(result["error"]["stage"], "write");
        assert_eq!(result["error"]["details"]["row_count"], 0);
        assert!(!result["error"]["message"].as_str().unwrap().is_empty());
        assert_eq!(write_state(&fixture)["requests"].as_array().unwrap().len(), 1);
        assert!(result.get("value").is_none());
    }
}

#[wasm_bindgen_test]
fn generation_returns_unsafe_javascript_integers_as_serialization_failures() {
    let result = generate_result(
        r#"trace "run" { output = 9007199254740992 }"#,
        &serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" }),
    );
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["stage"], "serialize");
    assert!(!result["error"]["message"].as_str().unwrap().is_empty());
    assert!(result.get("value").is_none());
}

#[wasm_bindgen_test]
fn generation_preserves_evaluation_diagnostics_and_unicode_byte_ranges() {
    let source = r#"trace "café 🦀" { input = [1][range(1, 1)] }"#;
    let result = generate_result(
        source,
        &serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" }),
    );
    assert_eq!(result["ok"], false, "{result}");
    assert_eq!(result["error"]["stage"], "generate");
    let diagnostics = result["error"]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    let expression = "[1][range(1, 1)]";
    let start = source.find(expression).unwrap();
    assert_eq!(diagnostic["phase"], "generation");
    assert_eq!(diagnostic["message"], "array index is out of bounds");
    assert_eq!(
        diagnostic["range"],
        serde_json::json!({ "start": start, "end": start + expression.len() })
    );
    assert!(result.get("value").is_none());
}

#[wasm_bindgen_test]
fn generation_preserves_special_json_object_keys() {
    let options = serde_json::json!({ "count": 1, "over_ms": 1000, "until_ms": 5000, "seed": "42" });
    let options = serde::Serialize::serialize(&options, &serde_wasm_bindgen::Serializer::json_compatible()).unwrap();
    let result = bts_wasm::generate(
        r#"trace "run" { output = { for key in ["__proto__"] : key => { marker = true } } }"#,
        options,
    )
    .expect("generation must return failures as data");
    let json = js_sys::JSON::stringify(&result).unwrap().as_string().unwrap();
    let result: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(
        result["value"]["payload"]["events"][0]["output"],
        serde_json::json!({ "__proto__": { "marker": true } })
    );
}

#[wasm_bindgen_test]
fn matches_native_seeded_samples() {
    let source = r#"
        trace "sampling" {
            output = {
                integer = range(-10, 10)
                choice = choice("alpha", "beta", "gamma")
                weighted = weighted(["steady", 3], ["burst", 1])
                chance = chance(0.5)
                uuid = uuid()
                hex = hex(8)
                alphanum = alphanum(8)
            }
        }
    "#;
    // captured from the original 64-bit native generator
    let expected = [
        (
            0,
            serde_json::json!([
                {
                    "integer": 10,
                    "choice": "alpha",
                    "weighted": "burst",
                    "chance": true,
                    "uuid": "0db107f7-53c0-422c-b3fd-475fe0114ae9",
                    "hex": "9daf119a",
                    "alphanum": "MjIhecBT"
                },
                {
                    "integer": -5,
                    "choice": "alpha",
                    "weighted": "steady",
                    "chance": true,
                    "uuid": "1e007435-896d-4761-874d-0067e9312c69",
                    "hex": "6b559921",
                    "alphanum": "qhDh7XAq"
                }
            ]),
        ),
        (
            42,
            serde_json::json!([
                {
                    "integer": -6,
                    "choice": "gamma",
                    "weighted": "steady",
                    "chance": false,
                    "uuid": "fa0552f9-657f-45da-8cf7-06f5fed89d5b",
                    "hex": "2a9e71e2",
                    "alphanum": "qVjkXWW7"
                },
                {
                    "integer": -6,
                    "choice": "alpha",
                    "weighted": "burst",
                    "chance": false,
                    "uuid": "35ef89eb-f8d8-48b7-8586-fda39ba3b61f",
                    "hex": "4cb85c37",
                    "alphanum": "Rl10wJDE"
                }
            ]),
        ),
        (
            u64::MAX,
            serde_json::json!([
                {
                    "integer": -8,
                    "choice": "beta",
                    "weighted": "burst",
                    "chance": true,
                    "uuid": "b09f5e2d-909c-43a1-a275-f3e3d182ea20",
                    "hex": "e544e8b6",
                    "alphanum": "GEdnZET4"
                },
                {
                    "integer": -10,
                    "choice": "beta",
                    "weighted": "steady",
                    "chance": false,
                    "uuid": "55bef189-0e4b-485f-8cbb-e060614354c9",
                    "hex": "6b1e2c38",
                    "alphanum": "NMGGYH8B"
                }
            ]),
        ),
    ];
    for (seed, expected) in expected {
        let result = generate_result(
            source,
            &serde_json::json!({
                "count": 2,
                "over_ms": 60000,
                "until_ms": 1_735_689_660_000_u64,
                "seed": seed.to_string(),
            }),
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["value"]["trace_count"], 2);
        assert_eq!(result["value"]["event_count"], 2);
        let outputs: Vec<_> = result["value"]["payload"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["output"].clone())
            .collect();
        assert_eq!(Value::Array(outputs), expected, "seed {seed}");
    }
}
