use serde_json::Value as JsonValue;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const SIMPLE_SHAPE: &str = r#"
trace "conversation" {
    task "turn" {
        llm "gpt-4o-mini" {
            input = "Hello ${trace.index}"
            output = choice(true, false) ? "Hi!" : "Hey!"
            metrics = { tokens = 2 * 2, latency = range(1, 5) * 100 }
        }
    }
}
"#;

#[test]
fn experiment_selection_errors_precede_credentials_and_seed_output() {
    let experiments = r#"
        trace "t" { input = "fallback" output = trace.input }
        dataset "cases" { case "one" { input = "question" } }
        scorer "quality" { code { score = 1 } }
        experiment "baseline" { dataset = dataset.cases task = trace.t scorers = [scorer.quality] }
        experiment "fixed" { dataset = dataset.cases task = trace.t scorers = [scorer.quality] baseline = experiment.baseline }
    "#;
    let cases = [
        (SIMPLE_SHAPE, None, "module declares no experiment blocks", false),
        (
            experiments,
            Some("dataset.cases"),
            "--select: expected an experiment traversal",
            false,
        ),
        (
            experiments,
            Some("experiment.missing"),
            "--select: unknown experiment \"missing\"",
            false,
        ),
        (
            experiments,
            Some("experiment.fixed"),
            "experiment \"fixed\" requires baseline \"baseline\" in the same write; add --select 'experiment[\"baseline\"]'",
            false,
        ),
        (
            experiments,
            Some("experiment.baseline"),
            "environment variable BRAINTRUST_API_KEY is required",
            true,
        ),
    ];
    for (source, selector, expected, selection_valid) in cases {
        let shape = write_shape(source);
        let mut command = bts();
        command
            .args(["write", "experiments", "--from"])
            .arg(&shape)
            .args(["--seed", "42"])
            .env_remove("BRAINTRUST_API_KEY")
            .env("BRAINTRUST_API_URL", "http://127.0.0.1:1")
            .env("BTS_LOG", "off");
        if let Some(selector) = selector {
            command.args(["--select", selector]);
        }
        let output = command.output().unwrap();
        fs::remove_file(shape).unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "selector {selector:?}: {stderr}");
        assert_eq!(stderr.contains("seed: 42"), selection_valid, "{stderr}");
        assert_eq!(stderr.contains("BRAINTRUST_API_KEY"), selection_valid, "{stderr}");
    }
}

#[test]
fn experiment_dry_run_selects_tasks_without_auth_or_scorer_calls() {
    let shape = write_shape(
        r#"
        trace "workflow" {
            repeat "unused" { count = range(-2, -1) tool "unused" { output = "unused" } }
            task "answer" {
                input = "fallback"
                output = self.input == "blocked" ? "Cannot recover this account" : "Recovery instructions sent"
                llm "reply" { input = task.answer.input output = task.answer.output }
                scorer "quality" { score = 0 }
            }
        }
        trace "unrelated" { repeat "unused" { count = range(-2, -1) tool "unused" { output = "unused" } } }
        scorer "quality" { code { score = output == expected ? 1 : 0 } }
        dataset "cases" {
            case "blocked" { input = "blocked" expected = "Cannot recover this account" }
            case "normal" { input = "normal" expected = "Recovery instructions sent" }
        }
        experiment "selected" { dataset = dataset.cases task = trace.workflow.task.answer scorers = [scorer.quality] }
        experiment "other" { dataset = dataset.cases task = trace.workflow scorers = [scorer.quality] }
    "#,
    );
    let output = bts()
        .args(["write", "experiments", "--from"])
        .arg(&shape)
        .args(["--select", "experiment.selected", "--seed", "42", "--dry-run"])
        .env_remove("BRAINTRUST_API_KEY")
        .env("BRAINTRUST_API_URL", "http://127.0.0.1:1")
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["scorers_executed"], false);
    assert_eq!(payload["dataset_source"], "local");
    let experiments = payload["experiments"].as_array().unwrap();
    assert_eq!(experiments.len(), 1);
    assert_eq!(experiments[0]["name"], "selected");
    let results = experiments[0]["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for result in results {
        let events = result["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["output"], events[0]["expected"]);
        assert_eq!(events[0]["output"], events[1]["output"]);
        assert!(events.iter().all(|event| event.get("scores").is_none()));
    }
}

#[test]
fn experiment_upload_failure_reports_completed_runs_in_human_and_json_output() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(
        &shape,
        r#"
        trace "t" { input = "fallback" output = trace.input }
        dataset "cases" { case "one" { input = "question" } }
        scorer "quality" { code { score = 1 } }
        experiment "baseline" { dataset = dataset.cases task = trace.t scorers = [scorer.quality] }
        experiment "fixed" { dataset = dataset.cases task = trace.t scorers = [scorer.quality] baseline = experiment.baseline }
    "#,
    )
    .unwrap();
    for json in [false, true] {
        let (api, requests) = serve_json_sequence(vec![
            serde_json::json!({"objects":[{"id":"function","project_id":project.to_string(),"slug":"quality","function_type":"scorer","_xact_id":"20"}]}),
            serde_json::json!({"objects":[{"id":"dataset","project_id":project.to_string(),"name":"cases"}]}),
            serde_json::json!({"events":[{"id":"one","input":"question","_xact_id":"10"}]}),
            serde_json::json!({"name":"quality","score":1}),
            serde_json::json!({"name":"quality","score":1}),
            serde_json::json!({"id":"baseline-id"}),
            serde_json::json!({"row_ids":["root","scorer"]}),
            serde_json::json!({}),
        ]);
        let mut command = bts();
        command
            .current_dir(&root)
            .args(["write", "experiments", "--from"])
            .arg(&shape)
            .env("BRAINTRUST_API_KEY", "test-key")
            .env("BRAINTRUST_API_URL", &api)
            .env("BTS_LOG", "off");
        if json {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("experiment \"fixed\""), "{stderr}");
        if json {
            let report: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["experiments"].as_array().unwrap().len(), 1);
            assert_eq!(report["experiments"][0]["id"], "baseline-id");
            assert_eq!(report["experiments"][0]["status"], "complete");
            assert_eq!(report["experiments"][0]["results"], 1);
            assert!(report["error"].as_str().unwrap().contains("experiment \"fixed\""));
        } else {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(stdout.contains("wrote baseline-"), "{stdout}");
            assert!(stdout.contains("experiment baseline-id"), "{stdout}");
        }
        assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 8);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_trace_write_matches_legacy_write_generation() {
    let shape = write_shape(SIMPLE_SHAPE);
    let run = |explicit: bool| {
        let mut command = bts();
        command.arg("write");
        if explicit {
            command.arg("traces");
        }
        let output = command
            .arg("--from")
            .arg(&shape)
            .args(["--count", "2", "--over", "1h", "--seed", "42", "--dry-run"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
        payload["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(json_input_output)
            .collect::<Vec<_>>()
    };
    fn json_input_output(event: &JsonValue) -> JsonValue {
        serde_json::json!([event["span_attributes"], event["input"], event["output"]])
    }
    assert_eq!(run(true), run(false));
    fs::remove_file(shape).unwrap();
}

#[test]
fn dry_run_expands_a_shape_into_the_requested_window() {
    let shape = write_shape(SIMPLE_SHAPE);
    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "25", "--over", "1h", "--dry-run"])
        .output()
        .unwrap();
    let after = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    let events = payload["events"].as_array().unwrap();
    let roots = events
        .iter()
        .filter(|event| event["span_parents"].as_array().unwrap().is_empty())
        .count();
    let starts = events
        .iter()
        .map(|event| event["metrics"]["start"].as_f64().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(events.len(), 75);
    assert_eq!(roots, 25);
    assert!(starts.iter().all(|start| *start >= before - 3_600.0 && *start <= after));

    // interpolation makes each trace unique
    let inputs = events
        .iter()
        .filter_map(|event| event["input"].as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(inputs.len(), 25);
    assert!(inputs.contains("Hello 0"));
    assert!(inputs.contains("Hello 24"));

    // operator exprs evaluate per event, constants fold and dynamics stay in range
    for event in events.iter().filter(|event| event["metrics"]["tokens"].is_number()) {
        assert_eq!(event["metrics"]["tokens"].as_i64().unwrap(), 4);
        let latency = event["metrics"]["latency"].as_i64().unwrap();
        assert!((100..=500).contains(&latency) && latency % 100 == 0);
        assert!(matches!(event["output"].as_str().unwrap(), "Hi!" | "Hey!"));
    }
}

#[test]
fn write_filter_omits_synthetic_scorers_without_changing_application_values() {
    let shape = write_shape(
        r#"
        trace "support" {
            input = "question ${trace.index}"
            output = weighted(["resolved", 3], ["escalated", 1])
            llm "Chat Completion" {
                output = trace.output
                scorer "answer-quality" {
                    score = clamp(normal(0.75, 0.1), 0, 1)
                    reason = "Observed ${trace.output}"
                }
            }
        }
        scorer "answer-quality" {
            code { score = output == "resolved" ? 1 : 0 }
        }
        "#,
    );
    let run = |filter: Option<&str>| {
        let mut command = bts();
        command.args(["write", "--from"]).arg(&shape).args([
            "--count",
            "3",
            "--start",
            "2026-09-01T00:00:00Z",
            "--end",
            "2026-09-01T01:00:00Z",
            "--seed",
            "42",
            "--dry-run",
        ]);
        if let Some(filter) = filter {
            command.args(["--filter", filter]);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice::<JsonValue>(&output.stdout).unwrap()["events"]
            .as_array()
            .unwrap()
            .clone()
    };
    let with_scores = run(None);
    let without_scores = run(Some("block.kind != \"scorer\""));
    let without_llms = run(Some("block.name != \"Chat Completion\""));
    fs::remove_file(shape).unwrap();

    assert_eq!(with_scores.len(), 9);
    assert_eq!(without_scores.len(), 6);
    assert_eq!(without_llms.len(), 3);
    let (full_chunks, _) = with_scores.as_chunks::<3>();
    let (filtered_chunks, _) = without_scores.as_chunks::<2>();
    for (full, filtered) in full_chunks.iter().zip(filtered_chunks) {
        assert_eq!(full[0]["input"], filtered[0]["input"]);
        assert_eq!(full[0]["output"], filtered[0]["output"]);
        assert_eq!(full[1]["output"], filtered[1]["output"]);
        assert_eq!(full[0]["metrics"], filtered[0]["metrics"]);
        assert_eq!(full[1]["metrics"], filtered[1]["metrics"]);
        assert_eq!(full[2]["span_attributes"]["type"], "scorer");
        assert_eq!(full[2]["span_attributes"]["purpose"], "scorer");
        assert_eq!(full[2]["span_attributes"]["name"], "answer-quality");
        assert_eq!(full[2]["span_parents"][0], full[1]["span_id"]);
        assert_eq!(full[2]["scores"]["answer-quality"], full[2]["output"]["score"]);
        assert_eq!(full[1]["scores"]["answer-quality"], full[2]["output"]["score"]);
        assert!(filtered[1].get("scores").is_none());
        assert_eq!(full[2]["input"]["output"], full[1]["output"]);
        assert!(
            full[2]["scores"]["answer-quality"]
                .as_f64()
                .is_some_and(|score| (0.0..=1.0).contains(&score))
        );
        assert!(full[2]["metadata"]["reason"].as_str().unwrap().starts_with("Observed "));
    }
}

#[test]
fn dry_run_varies_trace_shapes_through_dynamic_blocks() {
    let shape = write_shape(
        r#"trace "session" {
            repeat "turns" {
                count = range(1, 4)
                task "turn" { input = "question ${repeat.index}" llm "draft" {} }
            }
            choice { tool "get_order_status" {} function "summarize_session" {} }
            maybe { chance = 0.25 task "escalation" {} }
        }"#,
    );
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "20", "--over", "1h", "--dry-run", "--seed", "42"])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    let events = payload["events"].as_array().unwrap();

    let mut sizes = std::collections::HashSet::new();
    let mut picks = std::collections::HashMap::new();
    for event in events {
        let root = event["root_span_id"].as_str().unwrap();
        *picks.entry(root.to_owned()).or_insert(0) += match event["span_attributes"]["name"].as_str().unwrap() {
            "get_order_status" | "summarize_session" => 1,
            _ => 0,
        };
    }
    for (root, count) in &picks {
        assert_eq!(*count, 1, "trace {root} planned {count} choice children");
        sizes.insert(events.iter().filter(|event| event["root_span_id"] == *root).count());
    }

    assert_eq!(picks.len(), 20);
    assert!(sizes.len() > 1, "expected varying trace shapes, got sizes {sizes:?}");

    // repeat iterations resolve their own index
    let inputs = events
        .iter()
        .filter_map(|event| event["input"].as_str())
        .collect::<std::collections::HashSet<_>>();
    assert!(inputs.contains("question 0"));
    assert!(inputs.contains("question 1"));
}

#[test]
fn dry_run_resolves_context_references_in_expressions() {
    let shape = write_shape(
        r#"
        trace "conversation" {
            vars { messages = ["q0", "a0", "q1", "a1", "q2", "a2"] }
            repeat "turns" {
                count = 3
                llm "chat" {
                    input = var.messages[:(repeat.index * 2) + 1]
                    output = var.messages[(repeat.index * 2) + 1]
                    metrics = { turn = repeat.index + 1, of = repeat.count }
                }
            }
        }
        "#,
    );
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "1", "--over", "1h", "--dry-run"])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    let events = payload["events"].as_array().unwrap();
    let turns = events
        .iter()
        .filter(|event| event["span_attributes"]["name"] == "chat")
        .collect::<Vec<_>>();

    // history grows per iteration while the answer tracks the turn
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0]["input"], JsonValue::from(vec!["q0"]));
    assert_eq!(turns[2]["input"], JsonValue::from(vec!["q0", "a0", "q1", "a1", "q2"]));
    assert_eq!(turns[2]["output"], JsonValue::from("a2"));
    for (index, turn) in turns.iter().enumerate() {
        assert_eq!(turn["metrics"]["turn"].as_i64().unwrap(), index as i64 + 1);
        assert_eq!(turn["metrics"]["of"].as_i64().unwrap(), 3);
    }
}

#[test]
fn dry_run_fills_an_absolute_window_at_the_requested_rate() {
    let shape = write_shape(SIMPLE_SHAPE);
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args([
            "--rate",
            "30/m",
            "--start",
            "2026-08-25T12:00:00Z",
            "--end",
            "2026-08-25T13:00:00Z",
            "--dry-run",
        ])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    let events = payload["events"].as_array().unwrap();
    let roots = events
        .iter()
        .filter(|event| event["span_parents"].as_array().unwrap().is_empty())
        .count();

    // 0.5 traces per second over an hour
    assert_eq!(roots, 1_800);

    // every timestamp lands inside the requested window
    let window_start = parse_rfc3339_secs("2026-08-25T12:00:00Z");
    let window_end = parse_rfc3339_secs("2026-08-25T13:00:00Z");
    for event in events {
        let start = event["metrics"]["start"].as_f64().unwrap();
        let end = event["metrics"]["end"].as_f64().unwrap();
        assert!(start >= window_start && end <= window_end, "event escapes the window");
    }
}

#[test]
fn dry_run_honors_span_durations() {
    let shape = write_shape(
        r#"
        trace "session" {
            llm "chat" { duration = 2.5 }
        }
        "#,
    );
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "1", "--over", "1h", "--dry-run"])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    let events = payload["events"].as_array().unwrap();
    let chat = events
        .iter()
        .find(|event| event["span_attributes"]["name"] == "chat")
        .unwrap();
    let elapsed = chat["metrics"]["end"].as_f64().unwrap() - chat["metrics"]["start"].as_f64().unwrap();

    assert!((elapsed - 2.5).abs() < 1e-6, "expected a 2.5s span, got {elapsed}");
}

#[test]
fn renders_a_generation_diagnostic_for_dynamic_division_by_zero() {
    let shape = write_shape(r#"trace "t" { input = 100 / range(0, 0) }"#);
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "1", "--over", "1h", "--dry-run", "--seed", "0"])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("generation failed:"), "stderr: {stderr}");
    assert!(
        stderr.contains(":1:21: generation error: expression divides by zero"),
        "stderr: {stderr}"
    );
}

#[test]
fn threads_referenced_content_across_spans() {
    let shape = write_shape(
        r#"
        trace "support" {
            input = "Can I get invoices with our VAT number on them?"
            output = llm.chat.output.content

            llm "chat" {
                input = [{ role = "user", content = trace.input }]
                output = { role = "assistant", content = choice("Yes -- add it under Billing Settings.", "Yes, in Tax IDs.") }
                metrics = {
                    prompt_tokens = round(lognormal(400, 0.3)),
                    completion_tokens = round(lognormal(40, 0.5)),
                    tokens = self.metrics.prompt_tokens + self.metrics.completion_tokens,
                }
            }
        }
        "#,
    );
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "4", "--over", "1h", "--dry-run", "--seed", "9"])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 8);

    for pair in events.chunks(2) {
        let (root, llm) = (&pair[0], &pair[1]);
        // the sampled answer threads from the llm span up into the trace output
        assert_eq!(root["output"], llm["output"]["content"]);
        // and the question threads down into the llm's message list
        assert_eq!(llm["input"][0]["content"], root["input"]);
        // sibling metric keys sum exactly
        let metrics = &llm["metrics"];
        assert_eq!(
            metrics["tokens"].as_i64().unwrap(),
            metrics["prompt_tokens"].as_i64().unwrap() + metrics["completion_tokens"].as_i64().unwrap()
        );
    }
}

#[test]
fn writes_generated_events_to_the_configured_endpoint() {
    let shape = write_shape(SIMPLE_SHAPE);
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let (api_url, request) = serve_insert(6);
    let output = bts()
        .current_dir(&root)
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "2", "--over", "1h"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_PROJECT_ID", Uuid::new_v4().to_string())
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();
    let request = request.recv_timeout(Duration::from_secs(2)).unwrap();
    let (headers, body) = split_request(&request);
    let payload: JsonValue = serde_json::from_slice(body).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("inserted 2 traces and 4 child spans"));
    assert!(headers.starts_with(&format!("POST /v1/project_logs/{project_id}/insert HTTP/1.1\r\n")));
    assert!(headers.to_ascii_lowercase().contains("authorization: bearer test-secret\r\n"));
    assert_eq!(payload["events"].as_array().unwrap().len(), 6);
}

#[test]
fn uploads_an_attachment_before_inserting_its_reference() {
    let file = std::env::temp_dir().join(format!("bts-attachment-{}.pdf", Uuid::new_v4()));
    let contents = b"small pdf test payload";
    fs::write(&file, contents).unwrap();
    let path_literal = serde_json::to_string(&file.display().to_string()).unwrap();
    let shape = write_shape(&format!(
        "trace \"review\" {{ input = {{ document = attachment({path_literal}, \"application/pdf\") }} }}"
    ));
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let org_id = Uuid::new_v4();
    let (api_url, requests) = serve_attachment_flow(org_id, 3);

    let output = bts()
        .current_dir(&root)
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "3", "--over", "1h"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env_remove("BRAINTRUST_PROJECT_ID")
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_file(&file).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 5);
    let (project_headers, _) = split_request(&requests[0]);
    assert!(project_headers.starts_with(&format!("GET /v1/project/{project_id} HTTP/1.1\r\n")));

    let (init_headers, init_body) = split_request(&requests[1]);
    assert!(init_headers.starts_with("POST /attachment HTTP/1.1\r\n"));
    let init: JsonValue = serde_json::from_slice(init_body).unwrap();
    assert_eq!(init["org_id"], org_id.to_string());
    assert_eq!(init["filename"], file.file_name().unwrap().to_string_lossy().as_ref());
    assert_eq!(init["content_type"], "application/pdf");
    let key = init["key"].as_str().unwrap();

    let (upload_headers, upload_body) = split_request(&requests[2]);
    assert!(upload_headers.starts_with("PUT /blob HTTP/1.1\r\n"));
    assert!(!upload_headers.to_ascii_lowercase().contains("authorization:"));
    assert!(upload_headers.to_ascii_lowercase().contains("if-none-match: *\r\n"));
    assert_eq!(upload_body, contents);

    let (status_headers, status_body) = split_request(&requests[3]);
    assert!(status_headers.starts_with("POST /attachment/status HTTP/1.1\r\n"));
    let status: JsonValue = serde_json::from_slice(status_body).unwrap();
    assert_eq!(status["key"], key);
    assert_eq!(status["status"]["upload_status"], "done");

    let (insert_headers, insert_body) = split_request(&requests[4]);
    assert!(insert_headers.starts_with(&format!("POST /v1/project_logs/{project_id}/insert HTTP/1.1\r\n")));
    let inserted: JsonValue = serde_json::from_slice(insert_body).unwrap();
    assert_eq!(inserted["events"].as_array().unwrap().len(), 3);
    for event in inserted["events"].as_array().unwrap() {
        let document = &event["input"]["document"];
        assert_eq!(document["type"], "braintrust_attachment");
        assert_eq!(document["key"], key);
        assert_eq!(document["content_type"], "application/pdf");
    }
}

#[test]
fn filtered_blocks_do_not_upload_attachments_or_emit_scorer_spans() {
    let shape = write_shape(
        r#"
        trace "review" {
            task "attachment-holder" {
                output = attachment("filtered-out.pdf", "application/pdf")
            }
            task "scored" {
                output = "ok"
                scorer "inline-score" { score = 1 }
            }
        }
        scorer "inline-score" { code { score = 1 } }
        "#,
    );
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let (api_url, request) = serve_insert(2);
    let output = bts()
        .current_dir(&root)
        .args(["write", "--from"])
        .arg(&shape)
        .args([
            "--count",
            "1",
            "--over",
            "1h",
            "--filter",
            "block.name != \"attachment-holder\" && block.kind != \"scorer\"",
        ])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let request = request.recv_timeout(Duration::from_secs(2)).unwrap();
    let (headers, body) = split_request(&request);
    assert!(headers.starts_with(&format!("POST /v1/project_logs/{project_id}/insert HTTP/1.1\r\n")));
    let payload: JsonValue = serde_json::from_slice(body).unwrap();
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["span_attributes"]["name"], "review");
    assert_eq!(events[1]["span_attributes"]["name"], "scored");
    assert!(events[1].get("scores").is_none());
}

#[test]
fn attachment_keys_are_run_scoped_and_distinguish_full_paths() {
    let shape = write_shape("");
    let first = shape.with_file_name(format!("bts-first-{}.pdf", Uuid::new_v4()));
    let second = shape.with_file_name(format!("bts-second-{}.pdf", Uuid::new_v4()));
    fs::write(&first, b"same bytes").unwrap();
    fs::write(&second, b"same bytes").unwrap();
    let first_name = serde_json::to_string(&first.file_name().unwrap().to_string_lossy()).unwrap();
    let first_path = serde_json::to_string(&first.display().to_string()).unwrap();
    let second_path = serde_json::to_string(&second.display().to_string()).unwrap();
    fs::write(
        &shape,
        format!(
            "trace \"review\" {{ input = {{ first = attachment({first_name}, \"application/pdf\"), same = attachment({first_path}, \"application/pdf\"), second = attachment({second_path}, \"application/pdf\"), other_type = attachment({first_path}, \"text/plain\") }} }}"
        ),
    )
    .unwrap();

    let generate = || {
        let output = bts()
            .args(["write", "--from"])
            .arg(&shape)
            .args(["--count", "3", "--over", "1h", "--seed", "42", "--dry-run"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        serde_json::from_slice::<JsonValue>(&output.stdout).unwrap()
    };
    let first_run = generate();
    let second_run = generate();
    fs::remove_file(shape).unwrap();
    fs::remove_file(first).unwrap();
    fs::remove_file(second).unwrap();

    let events = first_run["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    let shared = &events[0]["input"]["first"]["key"];
    assert!(shared.is_string());
    for event in events {
        assert_eq!(&event["input"]["same"]["key"], shared);
        assert_eq!(&event["input"]["first"]["key"], shared);
        assert_ne!(&event["input"]["second"]["key"], shared);
        assert_ne!(&event["input"]["other_type"]["key"], shared);
    }
    assert_ne!(&second_run["events"][0]["input"]["first"]["key"], shared);
}

#[test]
fn resolves_relative_attachments_from_the_shape_directory() {
    let shape_dir = std::env::temp_dir().join(format!("bts-relative-{}", Uuid::new_v4()));
    fs::create_dir(&shape_dir).unwrap();
    let shape = shape_dir.join("shape.bt");
    let file = shape_dir.join("report.pdf");
    let contents = b"relative attachment payload";
    fs::write(&file, contents).unwrap();
    fs::write(
        &shape,
        "trace \"review\" { input = { document = attachment(\"report.pdf\", \"application/pdf\") } }",
    )
    .unwrap();
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let relative_shape = std::path::Path::new("..").join(shape.strip_prefix(std::env::temp_dir()).unwrap());
    let org_id = Uuid::new_v4();
    let (api_url, requests) = serve_attachment_flow(org_id, 1);

    let output = bts()
        .current_dir(&root)
        .args(["write", "--from"])
        .arg(&relative_shape)
        .args(["--count", "1", "--over", "1h"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env_remove("BRAINTRUST_PROJECT_ID")
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_dir_all(shape_dir).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 5);
    let (_, init_body) = split_request(&requests[1]);
    let init: JsonValue = serde_json::from_slice(init_body).unwrap();
    assert_eq!(init["filename"], "report.pdf");
    let (_, upload_body) = split_request(&requests[2]);
    assert_eq!(upload_body, contents);
    let (_, insert_body) = split_request(&requests[4]);
    let inserted: JsonValue = serde_json::from_slice(insert_body).unwrap();
    assert_eq!(inserted["events"][0]["input"]["document"]["key"], init["key"]);
}

#[test]
fn emits_a_json_summary_when_requested() {
    let shape = write_shape(SIMPLE_SHAPE);
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let (api_url, _request) = serve_insert(3);
    let output = bts()
        .current_dir(&root)
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "1", "--over", "1h", "--seed", "3", "--json"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env_remove("BRAINTRUST_PROJECT_ID")
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let summary: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["seed"], 3);
    assert_eq!(summary["traces"], 1);
    assert_eq!(summary["events"], 3);
    assert_eq!(summary["rows"], 3);
    assert_eq!(summary["project_id"], project_id.to_string());
    assert!(summary["duration_ms"].is_u64());
    assert!(summary["log"].as_str().unwrap().ends_with(".jsonl"));
}

// run from the temp dir so run logs land in a throwaway .bt, not the repo
#[test]
fn build_writes_scorer_sources_without_credentials() {
    let shape = write_shape(include_str!("../../../examples/scoring_automations.bt"));
    let root = write_project_context(Uuid::new_v4());
    let out = root.join("generated");
    let output = bts()
        .current_dir(&root)
        .args(["build", "--from"])
        .arg(&shape)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for (slug, file) in [
        ("reset-topic-clarity", "reset_topic_clarity_scorer.py"),
        ("no-password-collection", "no_password_collection_scorer.py"),
        ("verification-gate", "verification_gate_scorer.py"),
        ("lookup-request-binding", "lookup_request_binding_scorer.py"),
        ("reset-workflow-order", "reset_workflow_order_scorer.py"),
        ("reset-outcome-honesty", "reset_outcome_honesty_scorer.py"),
    ] {
        assert!(stdout.contains(&format!("{file} ({slug})")), "stdout:\n{stdout}");
        assert!(out.join("src/scorers").join(file).exists());
    }

    let code = fs::read_to_string(out.join("src/scorers/reset_outcome_honesty_scorer.py")).unwrap();
    assert!(code.starts_with("# generated by bts; edits will be overwritten"));
    let implementation = fs::read_to_string(out.join("src/scorers/reset_outcome_honesty.py")).unwrap();
    assert!(implementation.contains("def scorer_reset_outcome_honesty(output, input=None, expected=None, metadata=None):"));
    assert!(implementation.contains("for repeat_index in range("));
    assert!(!code.contains("def scorer_reset_outcome_honesty("));
    assert!(code.contains("handler=scorer_reset_outcome_honesty"));
    assert!(code.contains("project = braintrust.projects.create(name=\"test-project\")"));
    let pyproject: toml::Value = toml::from_str(&fs::read_to_string(out.join("pyproject.toml")).unwrap()).unwrap();
    let dependencies = pyproject["project"]["dependencies"].as_array().unwrap();
    assert!(dependencies.iter().any(|entry| entry.as_str() == Some("braintrust>=0.39,<1")));
    assert!(dependencies.iter().any(|entry| entry.as_str() == Some("pydantic>=2,<3")));
    assert!(!out.join("package.json").exists());

    // the language override flips the extension
    let shape = write_shape(include_str!("../../../examples/scoring_automations.bt"));
    let output = bts()
        .current_dir(&root)
        .args(["build", "--from"])
        .arg(&shape)
        .arg("--out")
        .arg(&out)
        .args(["--lang", "typescript"])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    for file in [
        "reset-topic-clarity",
        "no-password-collection",
        "verification-gate",
        "lookup-request-binding",
        "reset-workflow-order",
        "reset-outcome-honesty",
    ] {
        assert!(out.join("src/scorers").join(format!("{file}.scorer.ts")).exists());
    }
    let code = fs::read_to_string(out.join("src/scorers/reset-outcome-honesty.scorer.ts")).unwrap();
    let implementation = fs::read_to_string(out.join("src/scorers/reset-outcome-honesty.ts")).unwrap();
    let signature: String = implementation.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(signature.replace(",}", "}").contains("functionscorer_reset_outcome_honesty({input,output,expected,metadata}:{input?:any;output:any;expected?:any;metadata?:any;}):{name:string;score:number;metadata:Record<string,unknown>}{"));
    assert!(implementation.contains("for (let repeatIndex = 0;"));
    assert!(!code.contains("function scorer_reset_outcome_honesty("));
    assert!(code.contains("project.scorers.create({"));
    let package: JsonValue = serde_json::from_str(&fs::read_to_string(out.join("package.json")).unwrap()).unwrap();
    assert_eq!(package["dependencies"]["braintrust"], "^3.30.0");
    assert_eq!(package["dependencies"]["zod"], "^4.0.0");
    assert!(out.join("tsconfig.json").exists());
    assert!(!out.join("node_modules").exists());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scoring_example_generates_each_reset_outcome_and_scored_span() {
    let shape = write_shape(include_str!("../../../examples/scoring_automations.bt"));
    let output = bts()
        .args(["write", "--from"])
        .arg(&shape)
        .args(["--count", "6", "--over", "1h", "--dry-run", "--seed", "7"])
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let payload: JsonValue = serde_json::from_slice(&output.stdout).unwrap();
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 42);
    let roots: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| (event["span_attributes"]["name"] == "password-reset-assistant").then_some(index))
        .collect();
    assert_eq!(roots.len(), 6);
    let mut outcomes = std::collections::BTreeSet::new();
    for (position, start) in roots.iter().enumerate() {
        let end = roots.get(position + 1).copied().unwrap_or(events.len());
        let trace = &events[*start..end];
        let root = &trace[0];
        let span = |name: &str| trace.iter().find(|event| event["span_attributes"]["name"] == name);
        let status = root["output"]["status"].as_str().unwrap();
        outcomes.insert(status.to_owned());
        assert_eq!(
            root["output"]["reply"],
            span("draft-reset-reply").unwrap()["output"]["content"]
        );
        assert_eq!(
            span("verify-reset-request").unwrap()["output"]["lookup"],
            span("lookup-account").unwrap()["output"]
        );
        assert_eq!(
            span("verify-reset-request").unwrap()["input"]["email"],
            root["input"]["email"]
        );
        let steps: Vec<_> = root["output"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action["step"].clone())
            .collect();
        assert_eq!(steps, *root["metadata"]["required_steps"].as_array().unwrap());
        match status {
            "sent" => {
                assert!(span("execute-reset").is_some());
                assert_eq!(span("send-reset-email").unwrap()["output"]["status"], "sent");
            }
            "delivery_failed" => {
                assert_eq!(span("send-reset-email").unwrap()["output"]["status"], "failed");
                assert_eq!(span("send-reset-email").unwrap()["error"], "Email provider timed out");
                assert!(root["output"]["reply"].as_str().unwrap().contains("Check your email"));
            }
            "blocked" => {
                assert!(span("execute-reset").is_none());
                assert_eq!(span("verify-reset-request").unwrap()["output"]["decision"], "blocked");
            }
            other => panic!("unexpected outcome: {other}"),
        }
    }
    assert_eq!(
        outcomes,
        ["blocked", "delivery_failed", "sent"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
}

const AUTOMATION_SYNC_SHAPE: &str = r#"
scorer "reset-instructions" { code { score = 1 } }
scorer "response-helpfulness" { code { score = 1 } }
automation "score-reset-instructions" {
    type = "scorer"
    scorers = ["reset-instructions"]
    scope = "span"
    span_names = ["password-reset-answer"]
    sampling_rate = 1.0
    enabled = true
}
automation "judge-final-response" {
    type = "scorer"
    scorers = ["response-helpfulness"]
    scope = "span"
    span_names = ["password-reset-assistant"]
    sampling_rate = 1.0
    enabled = true
}
"#;

#[test]
fn sync_automations_resolves_pushed_scorers_and_creates_rules() {
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let shape = write_shape(AUTOMATION_SYNC_SHAPE);
    let scorer_ids = [Uuid::new_v4(), Uuid::new_v4()];
    let replies = vec![
        serde_json::json!({ "objects": [{ "id": scorer_ids[0].to_string(), "project_id": project_id.to_string(), "slug": "reset-instructions", "function_type": "scorer" }] }),
        serde_json::json!({ "objects": [{ "id": scorer_ids[1].to_string(), "project_id": project_id.to_string(), "slug": "response-helpfulness", "function_type": "scorer" }] }),
        serde_json::json!({ "objects": [] }),
        serde_json::json!({ "objects": [] }),
        serde_json::json!({ "id": Uuid::new_v4().to_string() }),
        serde_json::json!({ "id": Uuid::new_v4().to_string() }),
    ];
    let (api_url, requests) = serve_json_sequence(replies);
    let output = bts()
        .current_dir(&root)
        .args(["sync", "automation", "scorers", "--from"])
        .arg(&shape)
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 6);
    assert!(split_request(&requests[0]).0.starts_with("GET /v1/function?"));
    assert!(split_request(&requests[2]).0.starts_with("GET /v1/project_score?"));
    for (index, name, scorer_id, span_name) in [
        (4, "score-reset-instructions", scorer_ids[0], "password-reset-answer"),
        (5, "judge-final-response", scorer_ids[1], "password-reset-assistant"),
    ] {
        let (headers, body) = split_request(&requests[index]);
        assert!(headers.starts_with("PUT /v1/project_score HTTP/1.1\r\n"));
        assert!(headers.to_ascii_lowercase().contains("authorization: bearer test-secret\r\n"));
        let payload: JsonValue = serde_json::from_slice(body).unwrap();
        assert_eq!(payload["project_id"], project_id.to_string());
        assert_eq!(payload["name"], name);
        assert_eq!(payload["score_type"], "online");
        assert_eq!(payload["config"]["online"]["scorers"][0]["id"], scorer_id.to_string());
        assert_eq!(payload["config"]["online"]["apply_to_span_names"][0], span_name);
        assert_eq!(payload["config"]["online"]["status"], "active");
    }
}

#[test]
fn sync_automations_dry_run_does_not_put_rules() {
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let shape = write_shape(AUTOMATION_SYNC_SHAPE);
    let replies = vec![
        serde_json::json!({ "objects": [{ "id": Uuid::new_v4().to_string(), "project_id": project_id.to_string(), "slug": "reset-instructions", "function_type": "scorer" }] }),
        serde_json::json!({ "objects": [{ "id": Uuid::new_v4().to_string(), "project_id": project_id.to_string(), "slug": "response-helpfulness", "function_type": "scorer" }] }),
        serde_json::json!({ "objects": [] }),
        serde_json::json!({ "objects": [] }),
    ];
    let (api_url, requests) = serve_json_sequence(replies);
    let output = bts()
        .current_dir(&root)
        .args(["sync", "automation", "scorers", "--from"])
        .arg(&shape)
        .arg("--dry-run")
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("would create score-reset-instructions"));
    assert!(stdout.contains("would create judge-final-response"));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|request| split_request(request).0.starts_with("GET ")));
}

#[test]
fn sync_automations_skips_unchanged_rules_and_updates_changed_bindings() {
    let project_id = Uuid::new_v4();
    let root = write_project_context(project_id);
    let shape = write_shape(AUTOMATION_SYNC_SHAPE);
    let scorer_ids = [Uuid::new_v4(), Uuid::new_v4()];
    let existing = |name: &str, scorer_id: Uuid, span_name: &str, rate: f64| {
        serde_json::json!({
            "project_id": project_id.to_string(), "name": name, "score_type": "online",
            "config": { "online": {
                "sampling_rate": rate,
            "scorers": [{ "type": "function", "id": scorer_id.to_string(), "version": "latest" }],
                "status": "active", "scope": { "type": "span" },
                "apply_to_root_span": false, "apply_to_span_names": [span_name]
            }}
        })
    };
    let replies = vec![
        serde_json::json!({ "objects": [{ "id": scorer_ids[0].to_string(), "project_id": project_id.to_string(), "slug": "reset-instructions", "function_type": "scorer" }] }),
        serde_json::json!({ "objects": [{ "id": scorer_ids[1].to_string(), "project_id": project_id.to_string(), "slug": "response-helpfulness", "function_type": "scorer" }] }),
        serde_json::json!({ "objects": [existing("score-reset-instructions", scorer_ids[0], "password-reset-answer", 1.0)] }),
        serde_json::json!({ "objects": [existing("judge-final-response", scorer_ids[1], "password-reset-assistant", 0.25)] }),
        serde_json::json!({ "id": Uuid::new_v4().to_string() }),
    ];
    let (api_url, requests) = serve_json_sequence(replies);
    let output = bts()
        .current_dir(&root)
        .args(["sync", "automation", "scorers", "--from"])
        .arg(&shape)
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("unchanged score-reset-instructions"));
    assert!(stdout.contains("updated judge-final-response"));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 5);
    let (headers, body) = split_request(&requests[4]);
    assert!(headers.starts_with("PUT /v1/project_score HTTP/1.1\r\n"));
    let payload: JsonValue = serde_json::from_slice(body).unwrap();
    assert_eq!(payload["name"], "judge-final-response");
    assert_eq!(payload["config"]["online"]["sampling_rate"], 1.0);
}

fn bts() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bts"));
    command.current_dir(std::env::temp_dir());
    command
}

fn parse_rfc3339_secs(timestamp: &str) -> f64 {
    chrono::DateTime::parse_from_rfc3339(timestamp).unwrap().timestamp() as f64
}

fn write_shape(source: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("bts-write-{}.bt", Uuid::new_v4()));
    fs::write(&path, source).unwrap();
    path
}

fn write_project_context(project_id: Uuid) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("bts-context-{}", Uuid::new_v4()));
    fs::create_dir_all(root.join(".bt")).unwrap();
    let context = serde_json::json!({ "project": "test-project", "project_id": project_id.to_string() });
    fs::write(root.join(".bt/config.json"), context.to_string()).unwrap();
    root
}

fn serve_insert(row_count: usize) -> (String, Receiver<Vec<u8>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        sender.send(request).unwrap();
        let body = serde_json::json!({
            "row_ids": (0..row_count).map(|index| index.to_string()).collect::<Vec<_>>()
        })
        .to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body,
        )
        .unwrap();
    });

    (format!("http://{address}"), receiver)
}

fn serve_json_sequence(replies: Vec<JsonValue>) -> (String, Receiver<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            let body = reply.to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            )
            .unwrap();
        }
        sender.send(requests).unwrap();
    });
    (format!("http://{address}"), receiver)
}

fn serve_attachment_flow(org_id: Uuid, row_count: usize) -> (String, Receiver<Vec<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        let mut requests = Vec::new();
        for step in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_request(&mut stream));
            let body = match step {
                0 => serde_json::json!({ "org_id": org_id.to_string() }).to_string(),
                1 => serde_json::json!({
                    "signedUrl": format!("http://{address}/blob"),
                    "headers": { "If-None-Match": "*" },
                })
                .to_string(),
                4 => serde_json::json!({ "row_ids": (0..row_count).map(|index| format!("row-{index}")).collect::<Vec<_>>() })
                    .to_string(),
                _ => "{}".to_owned(),
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            )
            .unwrap();
        }
        sender.send(requests).unwrap();
    });
    (format!("http://{address}"), receiver)
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut request = Vec::new();
    let mut buffer = [0; 4096];

    loop {
        let read = stream.read(&mut buffer).unwrap();
        request.extend_from_slice(&buffer[..read]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let body_start = header_end + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .map(str::to_owned)
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_default();

        if request.len() >= body_start + content_length {
            return request;
        }
    }
}

fn split_request(request: &[u8]) -> (&str, &[u8]) {
    let header_end = request.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
    let headers = std::str::from_utf8(&request[..header_end + 4]).unwrap();
    (headers, &request[header_end + 4..])
}

const TOPICS_SYNC_SHAPE: &str = r#"
facet "Churn risk" { prompt = "Summarize churn risk." description = "Retention" no_match_pattern = "^NONE$" }
facet "Ignored" { prompt = "Do not sync this facet." }
scorer "not-pushed" { code { score = 1 } }
automation "support-topics" { type = "topics" facets = ["Churn risk"] scope = "trace" }
automation "other-topics" { type = "topics" facets = ["Ignored"] scope = "trace" }
automation "score" { type = "scorer" scorers = ["not-pushed"] scope = "span" root = true }
"#;

fn topic_function(project: Uuid, id: &str, map: bool) -> JsonValue {
    serde_json::json!({
        "id": id, "project_id": project.to_string(),
        "name": if map { "Churn risk topics" } else { "Churn risk" },
        "slug": if map { "bts-churn-risk-topic-map" } else { "churn-risk" },
        "function_type": if map { "classifier" } else { "facet" },
        "description": if map { JsonValue::Null } else { serde_json::json!("Retention") },
        "function_data": if map {
            serde_json::json!({ "type": "topic_map", "source_facet": "Churn risk",
                "source_facet_function": { "type": "function", "id": "facet-id" }, "embedding_model": "brain-embedding-1" })
        } else {
            serde_json::json!({ "type": "facet", "prompt": "Summarize churn risk.", "no_match_pattern": "^NONE$" })
        },
    })
}

fn topic_rule(project: Uuid) -> JsonValue {
    serde_json::json!({ "id": "automation-id", "project_id": project.to_string(), "name": "support-topics",
        "description": "Keep this description",
        "config": { "event_type": "topic", "sampling_rate": 1.0, "status": "active",
            "scope": { "type": "trace", "idle_seconds": 600 }, "data_scope": { "type": "project_logs" },
            "facet_functions": [{ "type": "function", "id": "facet-id" }],
            "topic_map_functions": [{ "function": { "type": "function", "id": "map-id" } }],
            "rerun_seconds": 86400, "backfill_time_range": "1d", "relabel_overlap_seconds": 3600,
        }
    })
}

fn run_topic_sync(project: Uuid, api: &str, dry_run: bool) -> std::process::Output {
    let root = write_project_context(project);
    let shape = write_shape(TOPICS_SYNC_SHAPE);
    let mut command = bts();
    command
        .current_dir(&root)
        .args(["sync", "automation", "topics", "--from"])
        .arg(&shape)
        .args([
            "--select",
            "facet[\"Churn risk\"]",
            "--select",
            "automation[\"support-topics\"]",
        ])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api)
        .env("BRAINTRUST_APP_URL", api);
    if dry_run {
        command.arg("--dry-run");
    }
    let output = command.output().unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();
    output
}

#[test]
fn sync_topics_creates_selected_facets_maps_and_automation_with_real_ids() {
    let project = Uuid::new_v4();
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({ "objects": [] }),
        serde_json::json!({ "objects": [] }),
        serde_json::json!([]),
        topic_function(project, "facet-id", false),
        topic_function(project, "map-id", true),
        serde_json::json!({ "project_automation": topic_rule(project), "found_existing": false }),
    ]);
    let output = run_topic_sync(project, &api, false);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 6);
    for (index, path) in [
        (0, "GET /v1/function?"),
        (1, "GET /v1/function?"),
        (2, "POST /api/project_automation/get "),
        (3, "PUT /v1/function "),
        (4, "PUT /v1/function "),
        (5, "POST /api/project_automation/register "),
    ] {
        let (headers, _) = split_request(&requests[index]);
        assert!(headers.starts_with(path), "{headers}");
        assert!(headers.to_ascii_lowercase().contains("authorization: bearer test-secret"));
    }
    let map: JsonValue = serde_json::from_slice(split_request(&requests[4]).1).unwrap();
    assert_eq!(map["function_data"]["source_facet_function"]["id"], "facet-id");
    let rule: JsonValue = serde_json::from_slice(split_request(&requests[5]).1).unwrap();
    assert_eq!(rule["config"]["facet_functions"][0]["id"], "facet-id");
    assert_eq!(rule["config"]["topic_map_functions"][0]["function"]["id"], "map-id");
    assert_eq!(rule["config"]["scope"]["type"], "trace");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Ignored"));
}

#[test]
fn sync_topics_dry_run_only_reads_and_reports_all_creates() {
    let project = Uuid::new_v4();
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({ "objects": [] }),
        serde_json::json!({ "objects": [] }),
        serde_json::json!([]),
    ]);
    let output = run_topic_sync(project, &api, true);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in ["Churn risk", "Churn risk topics", "support-topics"] {
        assert!(stdout.contains(&format!("would create {name}")));
    }
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 3);
    assert!(split_request(&requests[2]).0.starts_with("POST /api/project_automation/get "));
}

#[test]
fn sync_topics_skips_unchanged_resources_and_preserves_generated_maps() {
    let project = Uuid::new_v4();
    let mut facet = topic_function(project, "facet-id", false);
    facet["function_data"]["preprocessor"] =
        serde_json::json!({ "type": "global", "name": "thread", "function_type": "preprocessor" });
    let mut map = topic_function(project, "map-id", true);
    map["function_data"]["bundle_key"] = serde_json::json!("existing-bundle");
    map["function_data"]["report_key"] = serde_json::json!("existing-report");
    map["function_data"]["generation_settings"] =
        serde_json::json!({ "algorithm": "kmeans", "dimension_reduction": "pca", "n_clusters": 8 });
    let mut rule = topic_rule(project);
    rule["config"]["sampling_rate"] = serde_json::json!(1);
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({ "objects": [facet] }),
        serde_json::json!({ "objects": [map] }),
        serde_json::json!([rule]),
    ]);
    let output = run_topic_sync(project, &api, false);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 3);
    assert_eq!(String::from_utf8_lossy(&output.stdout).matches("unchanged ").count(), 3);
}

#[test]
fn sync_topics_updates_map_references_without_losing_artifacts_or_rule_settings() {
    let project = Uuid::new_v4();
    let facet = topic_function(project, "facet-id", false);
    let mut map = topic_function(project, "map-id", true);
    map["function_data"].as_object_mut().unwrap().remove("source_facet_function");
    map["function_data"]["bundle_key"] = serde_json::json!("existing-bundle");
    map["function_data"]["report_key"] = serde_json::json!("existing-report");
    map["function_data"]["topic_names"] = serde_json::json!({ "t": "Existing topic" });
    let mut rule = topic_rule(project);
    rule["config"]["sampling_rate"] = serde_json::json!(0.25);
    rule["config"]["rerun_seconds"] = serde_json::json!(43200);
    rule["config"]["btql_filter"] = serde_json::json!("metadata.production = true");
    rule["config"]["topic_map_functions"][0]["btql_filter"] = serde_json::json!("metadata.segment = 'enterprise'");
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({ "objects": [facet] }),
        serde_json::json!({ "objects": [map] }),
        serde_json::json!([rule]),
        topic_function(project, "map-id", true),
        topic_rule(project),
    ]);
    let output = run_topic_sync(project, &api, false);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 5);
    assert!(split_request(&requests[3]).0.starts_with("PATCH /v1/function/map-id "));
    let map: JsonValue = serde_json::from_slice(split_request(&requests[3]).1).unwrap();
    assert_eq!(map["function_data"]["bundle_key"], "existing-bundle");
    assert_eq!(map["function_data"]["report_key"], "existing-report");
    assert_eq!(map["function_data"]["topic_names"]["t"], "Existing topic");
    let rule: JsonValue = serde_json::from_slice(split_request(&requests[4]).1).unwrap();
    assert_eq!(rule["config"]["sampling_rate"], 1.0);
    assert_eq!(rule["config"]["rerun_seconds"], 43200);
    assert_eq!(rule["config"]["btql_filter"], "metadata.production = true");
    assert_eq!(
        rule["config"]["topic_map_functions"][0]["btql_filter"],
        "metadata.segment = 'enterprise'"
    );
    assert!(rule.get("description").is_none());
}

#[test]
fn sync_topics_rejects_conflicts_before_writing_any_resource() {
    let project = Uuid::new_v4();
    let mut map = topic_function(project, "map-id", true);
    map["function_type"] = serde_json::json!("scorer");
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({ "objects": [] }),
        serde_json::json!({ "objects": [map] }),
    ]);
    let output = run_topic_sync(project, &api, false);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("conflicts"));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(requests.iter().all(|request| split_request(request).0.starts_with("GET ")));
}

fn topic_sync_command(root: &std::path::Path, shape: &std::path::Path, api: &str) -> Command {
    let mut command = bts();
    command
        .current_dir(root)
        .args(["sync", "automation", "topics", "--from"])
        .arg(shape)
        .args(["--select", "automation[\"support-topics\"]"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api)
        .env("BRAINTRUST_APP_URL", api);
    command
}

fn existing_topics(project: Uuid) -> Vec<JsonValue> {
    vec![
        serde_json::json!({"objects":[topic_function(project, "facet-id", false)]}),
        serde_json::json!({"objects":[topic_function(project, "map-id", true)]}),
        serde_json::json!([topic_rule(project)]),
    ]
}

#[test]
fn sync_topics_prompt_changes_only_regenerate_when_requested() {
    for regenerate in [false, true] {
        let project = Uuid::new_v4();
        let root = write_project_context(project);
        let shape = root.join("shape.bt");
        fs::write(
            &shape,
            TOPICS_SYNC_SHAPE.replace("Summarize churn risk.", "Summarize current churn intent."),
        )
        .unwrap();
        let mut replies = existing_topics(project);
        replies.push(topic_function(project, "facet-id", false));
        if regenerate {
            replies.extend([serde_json::json!({"success":true}), serde_json::json!({"success":true})]);
        }
        let (api, requests) = serve_json_sequence(replies);
        let mut command = topic_sync_command(&root, &shape, &api);
        if regenerate {
            command.arg("--regenerate");
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(requests.len(), if regenerate { 6 } else { 4 });
        assert!(split_request(&requests[3]).0.starts_with("PATCH /v1/function/facet-id "));
        if regenerate {
            assert!(
                split_request(&requests[5])
                    .0
                    .starts_with("POST /brainstore/automation/reset-cursors ")
            );
            let reset: JsonValue = serde_json::from_slice(split_request(&requests[5]).1).unwrap();
            assert_eq!(reset["automation_id"], "automation-id");
            assert_eq!(reset["object_id"], format!("project_logs:{project}"));
            let xact = reset["start_xact_id"].as_str().unwrap().parse::<u64>().unwrap();
            let seconds = (xact >> 16) & 0xffffffff;
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
            assert!((86390..86410).contains(&(now - seconds)));
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn sync_topics_dry_run_does_not_save_or_queue_regeneration() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(&shape, TOPICS_SYNC_SHAPE.replace("Summarize churn risk.", "Updated prompt.")).unwrap();
    let (api, requests) = serve_json_sequence(existing_topics(project));
    let output = topic_sync_command(&root, &shape, &api)
        .args(["--dry-run", "--regenerate"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("would regenerate"));
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 3);
    assert!(!root.join(".bt/bts/state.json").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_topics_recovers_regeneration_after_the_prompt_was_saved() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(&shape, TOPICS_SYNC_SHAPE.replace("Summarize churn risk.", "Updated prompt.")).unwrap();
    let mut replies = existing_topics(project);
    replies.extend([
        topic_function(project, "facet-id", false),
        serde_json::json!({"success":true}),
        serde_json::json!({"success":false}),
    ]);
    let (api, requests) = serve_json_sequence(replies);
    let output = topic_sync_command(&root, &shape, &api).arg("--regenerate").output().unwrap();
    assert!(!output.status.success());
    let first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let first_reset: JsonValue = serde_json::from_slice(split_request(&first[5]).1).unwrap();
    let mut replies = existing_topics(project);
    replies[0]["objects"][0]["function_data"]["prompt"] = serde_json::json!("Updated prompt.");
    replies.extend([serde_json::json!({"success":true}), serde_json::json!({"success":true})]);
    let (api, requests) = serve_json_sequence(replies);
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(second.len(), 5);
    let second_reset: JsonValue = serde_json::from_slice(split_request(&second[4]).1).unwrap();
    assert_eq!(first_reset["start_xact_id"], second_reset["start_xact_id"]);
    let mut replies = existing_topics(project);
    replies[0]["objects"][0]["function_data"]["prompt"] = serde_json::json!("Updated prompt.");
    let (api, requests) = serve_json_sequence(replies);
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success());
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 3);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_topics_recovers_a_partially_created_rule_without_duplicate_functions() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(&shape, TOPICS_SYNC_SHAPE).unwrap();
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({"objects":[]}),
        serde_json::json!({"objects":[]}),
        serde_json::json!([]),
        topic_function(project, "facet-id", false),
        topic_function(project, "map-id", true),
        serde_json::json!({}),
    ]);
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 6);
    let (api, requests) = serve_json_sequence(existing_topics(project));
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 3);
    assert_eq!(String::from_utf8_lossy(&output.stdout).matches("unchanged").count(), 3);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_topics_removes_local_definitions_and_recovers_interrupted_deletions() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(&shape, TOPICS_SYNC_SHAPE).unwrap();
    let (api, requests) = serve_json_sequence(existing_topics(project));
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success());
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    fs::write(&shape, "# everything was removed\n").unwrap();
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!([topic_rule(project)]),
        serde_json::json!({"objects":[topic_function(project,"map-id",true)]}),
        serde_json::json!({"objects":[topic_function(project,"facet-id",false)]}),
    ]);
    let output = topic_sync_command(&root, &shape, &api).arg("--dry-run").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout).matches("would delete").count(), 3);
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut wrong = topic_function(project, "replacement-id", false);
    wrong["function_type"] = serde_json::json!("scorer");
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!([topic_rule(project)]),
        topic_rule(project),
        serde_json::json!({"objects":[topic_function(project,"map-id",true)]}),
        serde_json::json!({}),
        serde_json::json!({"objects":[wrong]}),
    ]);
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(!output.status.success());
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(
        split_request(&requests[1])
            .0
            .starts_with("POST /api/project_automation/delete_id ")
    );
    assert!(split_request(&requests[3]).0.starts_with("DELETE /v1/function/map-id "));
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!([]),
        serde_json::json!({"objects":[]}),
        serde_json::json!({"objects":[topic_function(project,"facet-id",false)]}),
        serde_json::json!({}),
    ]);
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 4);
    assert!(split_request(&requests[3]).0.starts_with("DELETE /v1/function/facet-id "));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_scorers_removes_owned_rules_without_deleting_pushed_functions() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(&shape, r#"scorer "brand" { code { score = 1 } } automation "rule" { type = "scorer" scorers = ["brand"] scope = "span" root = true }"#).unwrap();
    let rule = serde_json::json!({"id":"rule-id","project_id":project.to_string(),"name":"rule","score_type":"online","config":{"online":{"sampling_rate":1,"scorers":[{"type":"function","id":"fn-id"}],"status":"active","apply_to_root_span":true,"apply_to_span_names":[],"scope":{"type":"span"}}}});
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({"objects":[{"id":"fn-id","project_id":project.to_string(),"slug":"brand","function_type":"scorer"}]}),
        serde_json::json!({"objects":[rule.clone()]}),
    ]);
    let output = bts()
        .current_dir(&root)
        .args(["sync", "automation", "scorers", "--from"])
        .arg(&shape)
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", &api)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 2);
    fs::write(&shape, "").unwrap();
    let (api, requests) = serve_json_sequence(vec![serde_json::json!({"objects":[rule]}), serde_json::json!({})]);
    let output = bts()
        .current_dir(&root)
        .args(["sync", "automation", "scorers", "--from"])
        .arg(&shape)
        .args(["--select", "automation.rule"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", &api)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 2);
    assert!(split_request(&requests[1]).0.starts_with("DELETE /v1/project_score/rule-id "));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_scorer_selection_keeps_all_functions_in_shared_automation() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let source = AUTOMATION_SYNC_SHAPE.replace(
        "scorers = [\"reset-instructions\"]",
        "scorers = [\"reset-instructions\", \"response-helpfulness\"]",
    );
    let shape = write_shape(&source);
    let functions = ["reset-instructions", "response-helpfulness"].map(|slug| serde_json::json!({ "objects": [{ "id": slug, "project_id": project.to_string(), "slug": slug, "function_type": "scorer" }] }));
    let (api, requests) = serve_json_sequence(vec![
        functions[0].clone(),
        functions[1].clone(),
        serde_json::json!({ "objects": [] }),
        serde_json::json!({ "id": "rule-id" }),
    ]);
    let output = bts()
        .current_dir(&root)
        .args(["sync", "automation", "scorers", "--from"])
        .arg(&shape)
        .args(["--select", "scorer[\"reset-instructions\"]"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 4);
    let payload: JsonValue = serde_json::from_slice(split_request(&requests[3]).1).unwrap();
    assert_eq!(payload["config"]["online"]["scorers"].as_array().unwrap().len(), 2);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("judge-final-response"));
}

#[test]
fn sync_dataset_selection_skips_unselected_source_generation() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = write_shape(
        r#"
        trace "unavailable" { input = attachment("missing-selection.jpg", "image/jpeg") }
        dataset "keep" { case "inline" { input = "hello" } }
        dataset "skip" { case "source" { trace = trace.unavailable } }
    "#,
    );
    let (api, requests) = serve_json_sequence(vec![serde_json::json!({ "objects": [] })]);
    let output = bts()
        .current_dir(&root)
        .args(["sync", "datasets", "--from"])
        .arg(&shape)
        .args(["--select", "dataset.keep", "--dry-run"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 1);
    assert!(split_request(&requests[0]).0.contains("dataset_name=keep"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("skip"));
}

fn dataset_sync_command(root: &std::path::Path, shape: &std::path::Path, api: &str) -> Command {
    let mut command = bts();
    command
        .current_dir(root)
        .args(["sync", "datasets", "--from"])
        .arg(shape)
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_API_URL", api);
    command
}

#[test]
fn sync_commands_share_one_state_file_and_remove_dataset_blocks() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(
        &shape,
        format!("{TOPICS_SYNC_SHAPE}\ndataset \"cases\" {{ case \"one\" {{ input = \"hello\" }} }}"),
    )
    .unwrap();
    let (api, requests) = serve_json_sequence(existing_topics(project));
    let output = topic_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let dataset = serde_json::json!({"id":"dataset-id","name":"cases","project_id":project.to_string()});
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({"objects":[]}),
        dataset.clone(),
        serde_json::json!({"row_ids":["one"]}),
    ]);
    let output = dataset_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 3);
    let state_path = root.join(".bt/bts/state.json");
    let state: JsonValue = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let resources = state["projects"][project.to_string()]["shape.bt"]["resources"]
        .as_array()
        .unwrap();
    assert_eq!(resources.len(), 4);
    assert!(resources.iter().any(|r| r["kind"] == "dataset" && r["id"] == "dataset-id"));
    assert!(!root.join(".bt/bts/sync").exists());
    fs::write(&shape, TOPICS_SYNC_SHAPE).unwrap();
    let before = fs::read(&state_path).unwrap();
    let (api, requests) = serve_json_sequence(vec![serde_json::json!({"objects":[dataset.clone()]})]);
    let output = dataset_sync_command(&root, &shape, &api)
        .args(["--select", "dataset.cases", "--dry-run"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("would delete dataset"));
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 1);
    assert_eq!(fs::read(&state_path).unwrap(), before);
    let (api, requests) = serve_json_sequence(vec![serde_json::json!({"objects":[dataset]}), serde_json::json!({})]);
    let output = dataset_sync_command(&root, &shape, &api)
        .args(["--select", "dataset.cases"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 2);
    assert!(split_request(&requests[1]).0.starts_with("DELETE /v1/dataset/dataset-id "));
    let state: JsonValue = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let resources = state["projects"][project.to_string()]["shape.bt"]["resources"]
        .as_array()
        .unwrap();
    assert_eq!(resources.len(), 3);
    assert!(resources.iter().all(|r| r["kind"] != "dataset"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_datasets_keeps_shared_datasets_until_the_last_shape_removes_them() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let first = root.join("first.bt");
    let second = root.join("second.bt");
    let dataset = serde_json::json!({"id":"dataset-id","name":"cases","project_id":project.to_string()});
    for shape in [&first, &second] {
        fs::write(shape, "dataset \"cases\" {}").unwrap();
        let (api, requests) = serve_json_sequence(vec![
            serde_json::json!({"objects":[dataset.clone()]}),
            serde_json::json!({"events":[]}),
        ]);
        let output = dataset_sync_command(&root, shape, &api).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 2);
    }
    fs::write(&first, "").unwrap();
    let output = dataset_sync_command(&root, &first, "http://127.0.0.1:1").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("kept shared resource"));
    fs::write(&second, "# removed\n").unwrap();
    let (api, requests) = serve_json_sequence(vec![serde_json::json!({"objects":[dataset]}), serde_json::json!({})]);
    let output = dataset_sync_command(&root, &second, &api).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sync_datasets_recovers_an_ambiguous_create_without_creating_a_duplicate() {
    let project = Uuid::new_v4();
    let root = write_project_context(project);
    let shape = root.join("shape.bt");
    fs::write(&shape, "dataset \"cases\" {}").unwrap();
    let (api, requests) = serve_json_sequence(vec![serde_json::json!({"objects":[]}), serde_json::json!({})]);
    let output = dataset_sync_command(&root, &shape, &api).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(requests.recv_timeout(Duration::from_secs(2)).unwrap().len(), 2);
    let dataset = serde_json::json!({"id":"dataset-id","name":"cases","project_id":project.to_string()});
    let (api, requests) = serve_json_sequence(vec![
        serde_json::json!({"objects":[dataset]}),
        serde_json::json!({"events":[]}),
    ]);
    let output = dataset_sync_command(&root, &shape, &api).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let requests = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| split_request(r).0.starts_with("GET ")));
    let state: JsonValue = serde_json::from_slice(&fs::read(root.join(".bt/bts/state.json")).unwrap()).unwrap();
    assert_eq!(
        state["projects"][project.to_string()]["shape.bt"]["resources"][0]["id"],
        "dataset-id"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn syntax_check_validates_files_and_stdin_semantics() {
    let shape = write_shape(r#"trace "valid" { output = "hello" }"#);
    let output = bts().args(["check", "syntax"]).arg(&shape).output().unwrap();
    fs::remove_file(&shape).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).ends_with(": valid\n"));

    for (source, expected) in [
        (r#"trace "valid" { output = "hello" }"#, None),
        (
            r#"trace "invalid" { output = trace.missing.output }"#,
            Some("validation error"),
        ),
        ("trace {", Some("parsing error")),
    ] {
        let mut child = bts()
            .args(["check", "syntax", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(source.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        match expected {
            None => {
                assert!(output.status.success());
                assert_eq!(String::from_utf8_lossy(&output.stdout), "<stdin>: valid\n");
            }
            Some(phase) => {
                assert_eq!(output.status.code(), Some(1));
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains("<stdin>:"), "{stderr}");
                assert!(stderr.contains(phase), "{stderr}");
            }
        }
    }
}

const AGENT_HELLO_WORLD: &str = r#"agent "hello-world" {
    entrypoint = trace["hello-world"]
}

trace "hello-world" {
    input = "Hello, world!"
    output = self.input
}
"#;

#[test]
fn agent_hello_world_builds_and_runs_without_project_or_sdk() {
    let root = std::env::temp_dir().join(format!("bts-agent-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let shape = root.join("hello.bt");
    fs::write(&shape, AGENT_HELLO_WORLD).unwrap();
    fs::create_dir_all(root.join("generated")).unwrap();
    fs::write(
        root.join("generated/hello_world_agent.py"),
        "# Generated by bts; edits will be overwritten.\n",
    )
    .unwrap();
    fs::write(root.join("generated/notes.py"), "# keep user code\n").unwrap();
    let output = bts()
        .current_dir(&root)
        .args(["build", "agent", "--from", "hello.bt", "--out", "generated"])
        .env_remove("BRAINTRUST_API_KEY")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!root.join("generated/hello_world_agent.py").exists());
    assert_eq!(
        fs::read_to_string(root.join("generated/notes.py")).unwrap(),
        "# keep user code\n"
    );
    let module_dir = root.join("generated/src/agents");
    let script = Command::new("python3")
        .arg(module_dir.join("hello_world_agent.py"))
        .output()
        .unwrap();
    assert!(script.status.success(), "{}", String::from_utf8_lossy(&script.stderr));
    assert_eq!(String::from_utf8_lossy(&script.stdout), "Hello, world!\n");
    let imported = Command::new("python3")
        .current_dir(&module_dir)
        .args([
            "-c",
            r#"
from hello_world_agent import run
assert run() == 'Hello, world!'
assert run('Goodbye') == 'Goodbye'
assert run({'message': 'Hello again'}) == {'message': 'Hello again'}
assert run(None) is None
assert run(False) is False
"#,
        ])
        .output()
        .unwrap();
    assert!(imported.status.success(), "{}", String::from_utf8_lossy(&imported.stderr));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_typescript_builds_and_runs_without_project_or_sdk() {
    let root = std::env::temp_dir().join(format!("bts-agent-ts-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("hello.bt"),
        format!(
            "{}\n{}",
            AGENT_HELLO_WORLD,
            r#"agent "literals" { entrypoint = trace.literals }
        trace "literals" {
            output = { label = "ordinary key", values = [true, false, null, 42, 1.5, "hello\nworld"] }
        }"#,
        ),
    )
    .unwrap();
    let built = bts()
        .current_dir(&root)
        .args([
            "build",
            "agent",
            "--from",
            "hello.bt",
            "--lang",
            "typescript",
            "--out",
            "generated",
        ])
        .env_remove("BRAINTRUST_API_KEY")
        .output()
        .unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let module_dir = root.join("generated/src/agents");
    let script = Command::new("node")
        .arg(module_dir.join("hello_world_agent.mts"))
        .output()
        .unwrap();
    assert!(script.status.success(), "{}", String::from_utf8_lossy(&script.stderr));
    assert_eq!(String::from_utf8_lossy(&script.stdout), "Hello, world!\n");
    let imported = Command::new("node")
        .current_dir(&module_dir)
        .args([
            "--input-type=module",
            "-e",
            r#"
import assert from 'node:assert/strict';
import { run } from './hello_world_agent.mts';
import { run as literals } from './literals_agent.mts';
assert.equal(run(), 'Hello, world!');
assert.equal(run('Goodbye'), 'Goodbye');
assert.deepEqual(run({ message: 'Hello again' }), { message: 'Hello again' });
assert.equal(run(null), null);
assert.equal(run(false), false);
assert.equal(run(0), 0);
assert.equal(run(''), '');
assert.equal(literals().label, 'ordinary key');
assert.deepEqual(literals().values, [true, false, null, 42, 1.5, String.raw`hello\nworld`]);
const previous = literals();
previous.values.push('changed');
assert.equal(literals().values.length, 6);
"#,
        ])
        .output()
        .unwrap();
    assert!(imported.status.success(), "{}", String::from_utf8_lossy(&imported.stderr));
    assert!(imported.stdout.is_empty(), "import must not print the default output");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_example_preserves_child_data_flow_in_both_languages() {
    let root = write_project_context(Uuid::new_v4());
    fs::create_dir_all(&root).unwrap();
    let source = include_str!("../../../examples/customer_support_agent.bt");
    let shape = root.join("customer_support_agent.bt");
    fs::write(&shape, source).unwrap();
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args([
                "build",
                "agent",
                "--from",
                "customer_support_agent.bt",
                "--lang",
                lang,
                "--out",
                lang,
            ])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let extension = if lang == "python" { "py" } else { "mts" };
        let generated =
            fs::read_to_string(root.join(lang).join(format!("src/agents/customer_support_agent.{extension}"))).unwrap();
        let declaration = if lang == "python" { "def " } else { "function " };
        assert!(
            generated
                .lines()
                .filter(|line| line.trim_start().starts_with(declaration))
                .all(|line| line.starts_with(declaration)),
            "agent functions must be module-level: {generated}"
        );
        let (policy, next) = if lang == "python" {
            ("def check_arrival_policy(", "def front_desk_message(")
        } else {
            ("function checkArrivalPolicy(", "function frontDeskMessage(")
        };
        let policy = generated.split(policy).nth(1).unwrap().split(next).next().unwrap();
        assert!(!policy.contains("_at("), "policy should consume ordinary results: {policy}");
        assert!(
            !policy.contains(".children"),
            "policy should not traverse trace data: {policy}"
        );
        for scaffolding in ["_lazy(", " _at(", "(_at(", ".children", "Span_included", "span_included"] {
            assert!(
                !generated.contains(scaffolding),
                "unexpected execution scaffolding {scaffolding}: {generated}"
            );
        }
        assert!(generated.contains("context.trace("));
        assert!(generated.contains("context.choose("));
        let scorers = bts()
            .current_dir(&root)
            .args([
                "build",
                "scorers",
                "--from",
                "customer_support_agent.bt",
                "--lang",
                lang,
                "--out",
                lang,
            ])
            .output()
            .unwrap();
        assert!(scorers.status.success(), "{}", String::from_utf8_lossy(&scorers.stderr));
        assert!(root.join(lang).join("src/scorers").is_dir());
        let rebuilt = bts()
            .current_dir(&root)
            .args([
                "build",
                "agent",
                "--from",
                "customer_support_agent.bt",
                "--lang",
                lang,
                "--out",
                lang,
            ])
            .output()
            .unwrap();
        assert!(rebuilt.status.success(), "{}", String::from_utf8_lossy(&rebuilt.stderr));
        let report = String::from_utf8_lossy(&rebuilt.stdout);
        assert!(report.lines().all(|line| line.starts_with("unchanged ")), "{report}");
    }

    for lang in ["python", "typescript"] {
        let (program, args): (&str, &[&str]) = if lang == "python" {
            (
                "python3",
                &[
                    "-c",
                    r#"
from evals.customer_support_eval import CASES, SCORERS, task
from types import SimpleNamespace
class Span:
    def start_span(self, **kwargs): return self
    def log(self, **kwargs): pass
    def end(self, **kwargs): pass
assert len(CASES) == 3 and len(SCORERS) == 2
for case in CASES:
    output = task(case['input'], SimpleNamespace(span=Span()))
    for score in SCORERS:
        assert score(output=output, expected=case['expected'])['score'] == 1
        wrong = [dict(row, next_step='incorrect', needs_staff=not row['needs_staff']) for row in output]
        assert score(output=wrong, expected=case['expected'])['score'] == 0
"#,
                ],
            )
        } else {
            (
                "node",
                &[
                    "--input-type=module",
                    "-e",
                    r#"
import assert from 'node:assert/strict';
import {stripTypeScriptTypes} from 'node:module';
import {readFileSync,writeFileSync} from 'node:fs';
const {cases,scorers,task}=await import('./evals/customer_support.eval.mts');
const span={startSpan(){return this;},log(){},end(){}};
assert.equal(cases.length,3); assert.equal(scorers.length,2);
for(const row of cases){
    const output=task(row.input,{span});
    for(const score of scorers){
        assert.equal(score({output,expected:row.expected}).score,1);
        const wrong=output.map(r=>({...r,next_step:'incorrect',needs_staff:!r.needs_staff}));
        assert.equal(score({output:wrong,expected:row.expected}).score,0);
    }
}
"#,
                ],
            )
        };
        let checked = Command::new(program)
            .args(args)
            .current_dir(root.join(lang).join("src"))
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{lang}: {}",
            String::from_utf8_lossy(&checked.stderr)
        );
    }

    let cases = serde_json::json!([
        {"name": "Maya", "reservation_id": "HTL-4821", "arrival_hour": 25, "message": "My flight is delayed."},
        {"name": "Sam", "reservation_id": "UNKNOWN", "arrival_hour": 25, "message": "Can I check in after midnight?"},
        {"name": "Taylor", "reservation_id": null, "arrival_hour": 25, "message": "I lost my confirmation."},
        {"name": "Riley", "reservation_id": "HTL-7390", "arrival_hour": 26, "message": "Can I arrive at 1 a.m.?"},
        {"name": "Jordan", "reservation_id": "HTL-4821", "arrival_hour": 25, "message": "I will arrive late."},
        {"name": "Casey", "reservation_id": "HTL-7390", "arrival_hour": 23, "message": "Before midnight."},
        {"name": "Avery", "reservation_id": "HTL-9102", "arrival_hour": 25, "message": "Is my booking active?"},
        {"name": "Morgan", "reservation_id": "HTL-4821", "arrival_hour": 14, "message": "Early arrival."},
        {"name": "Quinn", "reservation_id": "HTL-4821", "arrival_hour": 30, "message": "Next morning."},
        {"name": "Lee", "reservation_id": "HTL-7390", "arrival_hour": 24, "message": "Midnight."},
        {"name": "Zoë", "reservation_id": "HTL-7390", "arrival_hour": 15, "message": "Afternoon."},
        {"name": "Drew", "reservation_id": "HTL-7390", "arrival_hour": 29, "message": "Early morning."},
    ]);
    let mut expected = Vec::new();
    for case in cases.as_array().unwrap() {
        let case_source = source
            .replacen("name = \"Maya\"", &format!("name = {}", case["name"]), 1)
            .replacen("arrival_hour = 25", &format!("arrival_hour = {}", case["arrival_hour"]), 1)
            .replacen(
                "reservation_id = \"HTL-4821\"",
                &format!("reservation_id = {}", case["reservation_id"]),
                1,
            );
        fs::write(&shape, case_source).unwrap();
        let preview = bts()
            .current_dir(&root)
            .args([
                "write",
                "traces",
                "--from",
                "customer_support_agent.bt",
                "--count",
                "1",
                "--over",
                "1h",
                "--dry-run",
            ])
            .output()
            .unwrap();
        assert!(preview.status.success(), "{}", String::from_utf8_lossy(&preview.stderr));
        let payload: JsonValue = serde_json::from_slice(&preview.stdout).unwrap();
        assert!((16..=19).contains(&payload["events"].as_array().unwrap().len()));
        let mut reply = payload["events"][0]["output"][0].clone();
        reply.as_object_mut().unwrap().remove("retried");
        reply.as_object_mut().unwrap().remove("handoff");
        expected.push(reply);
    }
    assert_eq!(expected[0]["next_step"], "check-in-on-arrival");
    assert_eq!(expected[1]["next_step"], "verify-reservation-number");
    assert_eq!(expected[2]["reservation_id"], JsonValue::Null);
    assert_eq!(expected[3]["next_step"], "contact-front-desk");
    assert_eq!(expected[3]["reservation_id"], "HTL-7390");
    assert_eq!(expected[4]["next_step"], "check-in-on-arrival");
    assert_eq!(expected[5]["next_step"], "check-in-on-arrival");
    assert_eq!(expected[6]["next_step"], "contact-reservations");
    assert_eq!(expected[7]["next_step"], "verify-arrival-time");
    assert_eq!(expected[8]["next_step"], "verify-arrival-time");
    assert_eq!(expected[9]["next_step"], "contact-front-desk");
    assert_eq!(expected[10]["next_step"], "check-in-on-arrival");
    assert_eq!(expected[11]["next_step"], "contact-front-desk");
    assert!(expected[3]["content"].as_str().unwrap().contains("120 minutes"));
    assert_eq!(expected[6]["needs_staff"], true);
    assert_eq!(expected[1]["needs_staff"], false);

    for lang in ["python", "typescript"] {
        let mut command = if lang == "python" {
            let mut command = Command::new("python3");
            command.args([
                "-c",
                r#"
import json, os, itertools
from customer_support_agent import run
cases = json.loads(os.environ['AGENT_TEST_CASES'])
def execute(batch, draw, channel_draw=None):
    channel_draw = draw if channel_draw is None else channel_draw
    draws = itertools.chain([draw, channel_draw, 0.5, 0.5, 0.5, 0.5] * 3, itertools.repeat(0.5))
    replies = run(batch, random_source=lambda: next(draws))
    for reply in replies:
        assert reply.pop('retried') == (draw < 0.15)
        handoff = reply.pop('handoff')
        assert handoff['channel'] == ('front-desk-message' if channel_draw < 0.5 else 'callback')
        assert handoff['status'] == ('queued' if reply['needs_staff'] else 'not-required')
    return replies
results = [reply for offset in range(0, len(cases), 3) for reply in execute(cases[offset:offset + 3], 0)]
assert [execute([case, case, case], 0.99)[0] for case in cases] == results
assert execute(cases[:3], 0) == results[:3]
for retry_draw in [0, 0.99]:
    for channel_draw in [0, 0.99]:
        for offset in range(0, len(cases), 3):
            assert execute(cases[offset:offset + 3], retry_draw, channel_draw) == results[offset:offset + 3]
print(json.dumps(results))
assert run()[0]['next_step'] == 'check-in-on-arrival'
for value in [{}, None, [None], [{'name': 'Maya', 'reservation_id': 123, 'message': 'hello'}]]:
    try:
        run(value)
    except (KeyError, TypeError):
        pass
    else:
        raise AssertionError('invalid input unexpectedly succeeded')
"#,
            ]);
            command
        } else {
            let mut command = Command::new("node");
            command.args([
                "--input-type=module",
                "-e",
                r#"
import assert from 'node:assert/strict';
import { run } from './customer_support_agent.mts';
const cases = JSON.parse(process.env.AGENT_TEST_CASES);
function execute(batch, draw, channelDraw = draw) {
    const draws = Array.from({length: 3}, () => [draw, channelDraw, 0.5, 0.5, 0.5, 0.5]).flat();
    const replies = run(batch, () => draws.shift() ?? 0.5);
    for (const reply of replies) {
        assert.equal(reply.retried, draw < 0.15);
        assert.equal(reply.handoff.channel, channelDraw < 0.5 ? 'front-desk-message' : 'callback');
        assert.equal(reply.handoff.status, reply.needs_staff ? 'queued' : 'not-required');
        delete reply.retried;
        delete reply.handoff;
    }
    return replies;
}
const results = cases.flatMap((_, index) => index % 3 === 0 ? execute(cases.slice(index, index + 3), 0) : []);
assert.deepEqual(cases.map(value => execute([value, value, value], 0.99)[0]), results);
assert.deepEqual(execute(cases.slice(0, 3), 0), results.slice(0, 3));
for (const retryDraw of [0, 0.99]) {
    for (const channelDraw of [0, 0.99]) {
        for (let offset = 0; offset < cases.length; offset += 3) {
            assert.deepEqual(execute(cases.slice(offset, offset + 3), retryDraw, channelDraw), results.slice(offset, offset + 3));
        }
    }
}
console.log(JSON.stringify(results));
assert.equal(run()[0].next_step, 'check-in-on-arrival');
for (const value of [{}, null, [null], [{name: 'Maya', reservation_id: 123, message: 'hello'}]]) {
    assert.throws(() => run(value));
}
"#,
            ]);
            command
        };
        let ran = command
            .current_dir(root.join(lang).join("src/agents"))
            .env("AGENT_TEST_CASES", cases.to_string())
            .output()
            .unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
        let actual: JsonValue = serde_json::from_slice(&ran.stdout).unwrap();
        assert_eq!(actual, serde_json::json!(expected), "{lang} differs from the trace planner");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_scalar_expressions_preserve_integer_and_short_circuit_semantics() {
    let root = std::env::temp_dir().join(format!("bts-agent-expressions-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let source = r#"
agent "expressions" { entrypoint = trace.values }
trace "values" {
    input = { n = -7, d = 3, guard = false }
    vars { amount = self.input.n guarded_value = self.input.missing unused_value = self.input.also_missing }
    output = {
        sum = var.amount + self.input.d
        difference = var.amount - self.input.d
        product = var.amount * self.input.d
        quotient = var.amount / self.input.d
        remainder = var.amount % self.input.d
        negated = -var.amount
        less = var.amount < self.input.d
        at_most = var.amount <= self.input.d
        greater = var.amount > self.input.d
        at_least = var.amount >= self.input.d
        unequal = var.amount != self.input.d
        guarded = self.input.guard && var.guarded_value
        fallback = !self.input.guard || var.guarded_value
        text = "n=${self.input.n}, flag=${self.input.guard}"
    }
}
"#;
    fs::write(root.join("shape.bt"), source).unwrap();
    let preview = bts()
        .current_dir(&root)
        .args([
            "write",
            "traces",
            "--from",
            "shape.bt",
            "--count",
            "1",
            "--over",
            "1h",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(preview.status.success(), "{}", String::from_utf8_lossy(&preview.stderr));
    let payload: JsonValue = serde_json::from_slice(&preview.stdout).unwrap();
    let expected = &payload["events"][0]["output"];
    assert_eq!(expected["quotient"], -2);
    assert_eq!(expected["remainder"], -1);
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args(["build", "agent", "--from", "shape.bt", "--lang", lang, "--out", lang])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let mut runner = if lang == "python" {
            let mut runner = Command::new("python3");
            runner.args([
                "-c",
                r#"
import json
from expressions_agent import run
print(json.dumps(run()))
assert run({'n': 7, 'd': -3, 'guard': False})['remainder'] == 1
assert run({'n': 1.5, 'd': 3, 'guard': False})['quotient'] == 0.5
for value in [
    {'n': 7, 'd': 0, 'guard': False},
    {'n': 9007199254740991, 'd': 3, 'guard': False},
    {'n': 7, 'd': 3, 'guard': True},
    {'n': 7, 'd': 3, 'guard': 'false'},
]:
    try:
        run(value)
    except (KeyError, TypeError, ZeroDivisionError):
        pass
    else:
        raise AssertionError('unsupported input was accepted')
"#,
            ]);
            runner
        } else {
            let mut runner = Command::new("node");
            runner.args([
                "--input-type=module",
                "-e",
                r#"
import assert from 'node:assert/strict';
import {run} from './expressions_agent.mts';
console.log(JSON.stringify(run()));
assert.equal(run({n:7,d:-3,guard:false}).remainder, 1);
assert.equal(run({n:1.5,d:3,guard:false}).quotient, 0.5);
for (const value of [
    {n:7,d:0,guard:false}, {n:9007199254740991,d:3,guard:false},
    {n:7,d:3,guard:true}, {n:7,d:3,guard:'false'},
]) assert.throws(() => run(value));
"#,
            ]);
            runner
        };
        let ran = runner.current_dir(root.join(lang).join("src/agents")).output().unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
        let actual: JsonValue = serde_json::from_slice(&ran.stdout).unwrap();
        assert_eq!(&actual, expected);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_collections_and_nested_repeats_run_in_both_languages() {
    let root = std::env::temp_dir().join(format!("bts-agent-collections-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("collections.bt"),
        r#"
agent "collections" { entrypoint = trace.t }
trace "t" {
    input = { values = [10, 20, 30], start = 1, end = 99, index = 0, text = "a😀é" }
    output = {
        names = [tool["same-name"].output, tool["same name"].output, tool["123 lookup"].output, tool["class"].output, tool["!!!"].output]
        selected = self.input.values[self.input.index]
        sliced = self.input.values[self.input.start:self.input.end]
        length = len(self.input.text)
        results = repeat.outer[:].task.row.output
        first = repeat.outer[0].task.row.output
        tail = repeat.outer[1:99].task.row.output
        empty = repeat.empty[:].tool.never.output
    }
    tool "same-name" { output = "first" }
    tool "same name" { output = "second" }
    tool "123 lookup" { output = "numeric" }
    tool "class" { output = "keyword" }
    tool "!!!" { output = "punctuation" }
    repeat "outer" {
        count = 2
        task "row" {
            output = repeat.inner[:].tool.cell.output
            repeat "inner" {
                count = 2
                tool "cell" { output = { index = repeat.index, count = repeat.count, label = "cell-${repeat.index}" } }
            }
        }
    }
    repeat "empty" { count = 0 tool "never" { output = "unreachable" } }
}
"#,
    )
    .unwrap();
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args(["build", "agent", "--from", "collections.bt", "--lang", lang, "--out", lang])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let mut command = if lang == "python" {
            let mut command = Command::new("python3");
            command.args([
                "-c",
                r#"
import json
from collections_agent import run
value = {'values': [10, 20, 30], 'start': 1, 'end': 99, 'index': 0, 'text': 'a😀é'}
print(json.dumps(run()))
assert run({**value, 'start': 3, 'end': 1})['sliced'] == []
for patch in [{'index': -1}, {'index': 3}, {'index': True}, {'start': -1}, {'end': 1.5}, {'text': {}}, {'values': 'abc'}]:
    try: run({**value, **patch})
    except (TypeError, ValueError, IndexError): pass
    else: raise AssertionError(patch)
"#,
            ]);
            command
        } else {
            let mut command = Command::new("node");
            command.args([
                "--input-type=module",
                "-e",
                r#"
import assert from 'node:assert/strict';
import { run } from './collections_agent.mts';
const value = {values: [10,20,30], start: 1, end: 99, index: 0, text: 'a😀é'};
console.log(JSON.stringify(run()));
assert.deepEqual(run({...value, start: 3, end: 1}).sliced, []);
for (const patch of [{index:-1},{index:3},{index:true},{start:-1},{end:1.5},{text:{}},{values:'abc'}]) {
    assert.throws(() => run({...value, ...patch}));
}
"#,
            ]);
            command
        };
        let ran = command.current_dir(root.join(lang).join("src/agents")).output().unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
        let actual: JsonValue = serde_json::from_slice(&ran.stdout).unwrap();
        let row = serde_json::json!([{"index":0,"count":2,"label":"cell-0"},{"index":1,"count":2,"label":"cell-1"}]);
        assert_eq!(
            actual,
            serde_json::json!({"names":["first","second","numeric","keyword","punctuation"],"selected":10,"sliced":[20,30],"length":3,"results":[row,row],"first":row,"tail":[row],"empty":[]})
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_branches_preserve_selection_nulls_and_lazy_execution() {
    let root = std::env::temp_dir().join(format!("bts-agent-branches-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("branches.bt"),
        r#"
agent "branches" { entrypoint = trace.t }
trace "t" {
    input = {}
    output = {
        chosen = choice.route.chosen
        selected = choice.route.tool.route[choice.route.chosen].output
        left = choice.route.tool.route[0].output
        right = choice.route.tool.route[1].output
        absent = maybe.never.tool.bad.output.missing
        present = maybe.always.tool.good.output
        batch = repeat.batch[:].task.row.output
    }
    choice "route" {
        tool "route" { output = { side = "left", other = choice.route.tool.route[1].output } }
        tool "route" { output = { side = "right", other = null } }
    }
    maybe "never" { chance = 0 tool "bad" { output = trace.input.missing } }
    maybe "always" { chance = 1 tool "good" { output = "yes" } }
    repeat "batch" {
        count = 2
        task "row" {
            output = {
                included = maybe.optional.included
                chosen = maybe.optional.choice.mode.chosen
                result = maybe.optional.choice.mode.tool.fast.output
                other = maybe.optional.choice.mode.tool.manual.output
            }
            maybe "optional" {
                chance = 0.5
                vars { message = "request-${repeat.index}" }
                choice "mode" {
                    tool "fast" { output = var.message }
                    tool "manual" { output = var.message }
                }
            }
        }
    }
}
"#,
    )
    .unwrap();
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args(["build", "agent", "--from", "branches.bt", "--lang", lang, "--out", lang])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let mut command = if lang == "python" {
            let mut command = Command::new("python3");
            command.args([
                "-c",
                r#"
import json
from branches_agent import run
outcomes = [run(random_source=lambda: 0), run(random_source=lambda: 0.99)]
draws = iter([0.99, 0, 0, 0, 0.99, 0.99])
mixed = run(random_source=lambda: next(draws))
assert mixed['batch'] == [
    {'included': True, 'chosen': 1, 'result': None, 'other': 'request-0'},
    {'included': False, 'chosen': None, 'result': None, 'other': None},
]
assert list(draws) == []
assert run(random_source=lambda: 0) == outcomes[0]
for value in [-1, 1, float('nan'), float('inf'), True, '0.5']:
    try: run(random_source=lambda: value)
    except (TypeError, ValueError): pass
    else: raise AssertionError(value)
print(json.dumps(outcomes))
"#,
            ]);
            command
        } else {
            let mut command = Command::new("node");
            command.args([
                "--input-type=module",
                "-e",
                r#"
import assert from 'node:assert/strict';
import {run} from './branches_agent.mts';
const outcomes = [run(undefined, () => 0), run(undefined, () => 0.99)];
const draws = [0.99, 0, 0, 0, 0.99, 0.99];
const mixed = run(undefined, () => draws.shift());
assert.deepEqual(mixed.batch, [
    {included:true, chosen:1, result:null, other:'request-0'},
    {included:false, chosen:null, result:null, other:null},
]);
assert.deepEqual(draws, []);
assert.deepEqual(run(undefined, () => 0), outcomes[0]);
for (const value of [-1, 1, NaN, Infinity, true, '0.5']) {
    assert.throws(() => run(undefined, () => value));
}
console.log(JSON.stringify(outcomes));
"#,
            ]);
            command
        };
        let ran = command.current_dir(root.join(lang).join("src/agents")).output().unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
        let actual: JsonValue = serde_json::from_slice(&ran.stdout).unwrap();
        let left = serde_json::json!({"side":"left","other":null});
        let right = serde_json::json!({"side":"right","other":null});
        assert_eq!(
            actual,
            serde_json::json!([
                {"chosen":0,"selected":left,"left":left,"right":null,"absent":null,"present":"yes","batch":[
                    {"included":true,"chosen":0,"result":"request-0","other":null},
                    {"included":true,"chosen":0,"result":"request-1","other":null}
                ]},
                {"chosen":1,"selected":right,"left":null,"right":right,"absent":null,"present":"yes","batch":[
                    {"included":false,"chosen":null,"result":null,"other":null},
                    {"included":false,"chosen":null,"result":null,"other":null}
                ]}
            ])
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_text_numeric_fields_and_forward_references_match_planner() {
    let root = std::env::temp_dir().join(format!("bts-agent-fields-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("fields.bt"),
        r#"
agent "fields" { entrypoint = trace.t }
trace "t" {
    input = { text = "  Straße|HELLO  ", number = -2.5, items = ["alpha", 2, true], separator = "|", index = 0, duration = 0.25 }
    expected = { answer = "ready" }
    error = null
    metadata = { second = self.metadata.first, first = "ready", reference = tool.later.output }
    metrics = { total = self.metrics.first + 2, first = 3 }
    tags = ["support", "fields"]
    duration = self.input.duration
    output = {
        upper = upper(trim(self.input.text))
        lower = lower(trim(self.input.text))
        replaced = replace(self.input.text, "|", "/")
        unchanged = replace(self.input.text, "", "ignored")
        parts = split(trim(self.input.text), self.input.separator)
        joined = join(self.input.items, ",")
        has = contains(self.input.items, "alpha")
        distinct = contains(self.input.items, 1)
        starts = starts_with(trim(self.input.text), "Straße")
        ends = ends_with(trim(self.input.text), "HELLO")
        formatted = format("{}:{}", self.metadata.first, self.metrics.total)
        spread = ["start", ...self.input.items, "end"]
        float_division = self.input.number / -2
        float_remainder = self.input.number % 2
        float_text = format("{}", max(self.input.number, 4))
        clamped = clamp(self.input.number, -2, 2)
        rounded = round(self.input.number)
        floored = floor(self.input.number)
        ceiling = ceil(self.input.number)
        absolute = abs(self.input.number)
        smallest = min(self.input.number, 4, -1)
        largest = max(self.input.number, 4, -1)
        forward = tool.earlier.output
        expected = self.expected.answer
        absent_error = self.error
        error_only = tool["error-only"].error
        selected = [self.input.text, self.input.does_not_exist][self.input.index]
    }
    tool "error-only" { error = "timeout" duration = 0.01 }
    tool "earlier" {
        input = tool.later.output
        output = self.input
        metadata = { status = self.output }
        duration = 0.15
    }
    tool "later" {
        input = function.inner.output
        output = format("{}", self.input)
        error = "reservation-unavailable"
        function "inner" { output = trace.metadata.first duration = 0.2 }
    }
}
"#,
    )
    .unwrap();
    let preview = bts()
        .current_dir(&root)
        .args([
            "write",
            "traces",
            "--from",
            "fields.bt",
            "--count",
            "1",
            "--over",
            "1h",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(preview.status.success(), "{}", String::from_utf8_lossy(&preview.stderr));
    let expected: JsonValue = serde_json::from_slice(&preview.stdout).unwrap();
    let expected = &expected["events"][0]["output"];
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args(["build", "agent", "--from", "fields.bt", "--lang", lang, "--out", lang])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let mut command = if lang == "python" {
            let mut command = Command::new("python3");
            command.args([
                "-c",
                r#"
import json
from fields_agent import run, run_details, run_traced
details = run_details()
assert details['spans'][0]['metadata'] == {'first':'ready','second':'ready','reference':'ready'}
assert details['spans'][0]['metrics'] == {'first':3,'total':5}
assert details['spans'][0]['tags'] == ['support','fields']
assert abs(details['spans'][0]['duration'] - 0.36) < 1e-9
assert details['spans'][3]['error'] == 'reservation-unavailable'
assert [span['parent'] for span in details['spans']] == [None,0,0,0,3]
assert details['output'] == run()
value = {'text':'x|y','number':-2.5,'items':['a',2,True],'separator':'|','index':0,'duration':0.25}
for patch in [{'duration':0},{'duration':-1},{'separator':''},{'items':'bad'},{'number':True},{'index':1}]:
    try: run({**value, **patch})
    except (ValueError, TypeError, KeyError): pass
    else: raise AssertionError(patch)

logged = []
class Logger:
    def __init__(self, index=None): self.index = index
    def start_span(self, **args):
        index = len(logged)
        logged.append(dict(args, parent=self.index))
        return Logger(index)
    def log(self, **fields): logged[self.index].update(fields)
    def end(self, *, end_time): logged[self.index]['end'] = end_time
assert run_traced(Logger()) == details['output']
assert len(logged) == len(details['spans'])
for expected in details['spans']:
    actual = next(row for row in logged if row['name'] == expected['name'])
    assert (None if actual['parent'] is None else logged[actual['parent']]['name']) == (None if expected['parent'] is None else details['spans'][expected['parent']]['name'])
    assert actual['name'] == expected['name'] and actual['type'] == expected['type']
    assert actual['end'] >= actual['start_time']
    for key in ('input','output','expected','error','metadata','metrics','tags'):
        if key in expected: assert actual[key] == expected[key]
    if actual['parent'] is not None:
        parent = logged[actual['parent']]
        assert actual['start_time'] >= parent['start_time'] - 1e-6
        assert actual['end'] <= parent['end'] + 1e-6
try: run_traced(Logger(), {'duration':0})
except (ValueError, KeyError): pass
else: raise AssertionError('expected runtime failure')
assert logged[-1]['name'] == logged[0]['name'] and logged[-1]['error']
assert 'end' in logged[-1]
print(json.dumps(details['output']))
"#,
            ]);
            command
        } else {
            let mut command = Command::new("node");
            command.args([
                "--input-type=module",
                "-e",
                r#"
import assert from 'node:assert/strict';
import {run, runDetails, runTraced} from './fields_agent.mts';
const details = runDetails();
assert.deepEqual(details.spans[0].metadata,{first:'ready',second:'ready',reference:'ready'});
assert.deepEqual(details.spans[0].metrics,{first:3,total:5});
assert.deepEqual(details.spans[0].tags,['support','fields']);
assert.ok(Math.abs(details.spans[0].duration - 0.36) < 1e-9);
assert.equal(details.spans[3].error,'reservation-unavailable');
assert.deepEqual(details.spans.map(span => span.parent),[null,0,0,0,3]);
assert.deepEqual(details.output,run());
const value = {text:'x|y',number:-2.5,items:['a',2,true],separator:'|',index:0,duration:0.25};
for(const patch of [{duration:0},{duration:-1},{separator:''},{items:'bad'},{number:true},{index:1}]) assert.throws(()=>run({...value,...patch}));

const logged = [];
class Logger {
    constructor(index = null) {this.index = index;}
    startSpan(args) {const index=logged.length; logged.push({...args,parent:this.index}); return new Logger(index);}
    log(fields) {Object.assign(logged[this.index], fields);}
    end({endTime}) {logged[this.index].end=endTime;}
}
assert.deepEqual(runTraced(new Logger()), details.output);
assert.equal(logged.length,details.spans.length);
for (const [index, expected] of details.spans.entries()) {
    const actual=logged.find(row=>row.name===expected.name);
    assert.equal(actual.parent===null?null:logged[actual.parent].name,expected.parent===null?null:details.spans[expected.parent].name);
    assert.equal(actual.name,expected.name); assert.equal(actual.type,expected.type);
    assert.ok(actual.end>=actual.startTime);
    for(const key of ['input','output','expected','error','metadata','metrics','tags']) if(key in expected) assert.deepEqual(actual[key],expected[key]);
    if(actual.parent!==null) {
        assert.ok(actual.startTime>=logged[actual.parent].startTime-1e-6);
        assert.ok(actual.end<=logged[actual.parent].end+1e-6);
    }
}
assert.throws(()=>runTraced(new Logger(),{duration:0}));
assert.equal(logged.at(-1).name,logged[0].name);
assert.ok(logged.at(-1).error && logged.at(-1).end);
console.log(JSON.stringify(details.output));
"#,
            ]);
            command
        };
        let ran = command.current_dir(root.join(lang).join("src/agents")).output().unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
        let actual: JsonValue = serde_json::from_slice(&ran.stdout).unwrap();
        // JSON.stringify emits integral floats as integers; compare their numeric values.
        fn normalize(value: &JsonValue) -> JsonValue {
            match value {
                JsonValue::Number(number) => serde_json::json!(number.as_f64().unwrap()),
                JsonValue::Array(values) => JsonValue::Array(values.iter().map(normalize).collect()),
                JsonValue::Object(values) => {
                    JsonValue::Object(values.iter().map(|(key, value)| (key.clone(), normalize(value))).collect())
                }
                _ => value.clone(),
            }
        }
        assert_eq!(normalize(&actual), normalize(expected), "{lang}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_random_values_are_cached_scoped_and_lazy() {
    let root = std::env::temp_dir().join(format!("bts-agent-random-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("random.bt"),
        r#"
agent "random" { entrypoint = trace.t }
trace "t" {
    input = {}
    vars { shared = range(10, 20) }
    output = {
        shared = var.shared
        copy = var.shared
        forward = tool.later.output
        forward_copy = tool.later.output
        chosen = choice("safe", self.input.missing)
        weighted = weighted([self.input.missing, 0], ["safe", 1])
        certain = chance(1)
        impossible = chance(0)
        float_division = range(1.0, 1.0) / 2
        float_text = format("{}", range(1.0, 1.0))
        normal = normal(10, 2)
        lognormal = lognormal(2, 0.3)
        exponential = exponential(3)
        pareto = pareto(2, 3)
        beta = beta(2, 3)
        poisson = poisson(6)
        large_poisson = poisson(1000000)
        uuid = uuid()
        hex = hex(12)
        alphanum = alphanum(16)
        noise = noise(20)
        rows = repeat.rows[:].tool.row.output
    }
    tool "later" { output = range(100, 200) }
    repeat "rows" {
        count = 2
        vars { row_value = range(1, 100) }
        tool "row" { output = { value = var.row_value, copy = var.row_value } }
    }
}
"#,
    )
    .unwrap();
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args(["build", "agent", "--from", "random.bt", "--lang", lang, "--out", lang])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let mut command = if lang == "python" {
            let mut command = Command::new("python3");
            command.args([
                "-c",
                r#"
import math, re, json
from random_agent import run
result = run(random_source=lambda: 0.25)
assert result['shared'] == result['copy'] == 12
assert result['forward'] == result['forward_copy'] == 125
assert result['chosen'] == result['weighted'] == 'safe'
assert result['certain'] is True and result['impossible'] is False
assert result['float_division'] == 0.5 and result['float_text'] == '1.0'
assert all(math.isfinite(result[key]) for key in ['normal','lognormal','exponential','pareto','beta','poisson'])
assert result['lognormal'] > 0 and result['exponential'] >= 0 and result['pareto'] >= 2
assert type(result['large_poisson']) is int and result['large_poisson'] > 0
assert 0 < result['beta'] < 1 and type(result['poisson']) is int and result['poisson'] >= 0
assert re.fullmatch(r'[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}',result['uuid'])
assert len(result['hex']) == 12 and len(result['alphanum']) == 16 and len(result['noise']) == 20
assert all(row['value'] == row['copy'] for row in result['rows'])
# Vary draws to catch accidental resampling of shared values.
counter = [0]
def draw():
    counter[0] += 1
    return (counter[0] % 20) / 100
varying = run(random_source=draw)
assert varying['shared'] == varying['copy']
assert varying['forward'] == varying['forward_copy']
assert all(row['value'] == row['copy'] for row in varying['rows'])
assert varying['rows'][0]['value'] != varying['rows'][1]['value']
print(json.dumps(result))
"#,
            ]);
            command
        } else {
            let mut command = Command::new("node");
            command.args([
                "--input-type=module",
                "-e",
                r#"
import assert from 'node:assert/strict';
import {run} from './random_agent.mts';
const result = run(undefined, () => 0.25);
assert.equal(result.shared,12); assert.equal(result.copy,12);
assert.equal(result.forward,125); assert.equal(result.forward_copy,125);
assert.equal(result.chosen,'safe'); assert.equal(result.weighted,'safe');
assert.equal(result.certain,true); assert.equal(result.impossible,false);
assert.equal(result.float_division,0.5); assert.equal(result.float_text,'1.0');
for(const key of ['normal','lognormal','exponential','pareto','beta','poisson']) assert.ok(Number.isFinite(result[key]));
assert.ok(result.lognormal > 0 && result.exponential >= 0 && result.pareto >= 2);
assert.ok(Number.isInteger(result.large_poisson) && result.large_poisson > 0);
assert.ok(result.beta > 0 && result.beta < 1 && Number.isInteger(result.poisson) && result.poisson >= 0);
assert.match(result.uuid,/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
assert.equal(result.hex.length,12); assert.equal(result.alphanum.length,16); assert.equal(result.noise.length,20);
for(const row of result.rows) assert.equal(row.value,row.copy);
let counter = 0;
const varying = run(undefined, () => (++counter % 20) / 100);
assert.equal(varying.shared,varying.copy);
assert.equal(varying.forward,varying.forward_copy);
for(const row of varying.rows) assert.equal(row.value,row.copy);
assert.notEqual(varying.rows[0].value,varying.rows[1].value);
console.log(JSON.stringify(result));
"#,
            ]);
            command
        };
        let ran = command.current_dir(root.join(lang).join("src/agents")).output().unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_traces_execution_and_preserves_partial_failures() {
    let root = std::env::temp_dir().join(format!("bts-agent-tracing-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("trace.bt"),
        r#"
agent "workflow" { entrypoint = trace.workflow }
trace "workflow" {
    input = { divisor = 2 }
    output = tool.compute.output
    tool "first" { output = range(1, 2) }
    tool "compute" { input = trace.input output = 10 / self.input.divisor }
    tool "never-on-failure" { output = "done" }
}
"#,
    )
    .unwrap();
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args(["build", "agent", "--from", "trace.bt", "--lang", lang, "--out", lang])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let (program, script) = if lang == "python" {
            (
                "python3",
                r#"
from workflow_agent import run_traced
rows=[]
class Logger:
    def __init__(self,index=None): self.index=index
    def start_span(self,**args):
        if self.index is not None: assert not rows[self.index].get('closed')
        index=len(rows); rows.append(dict(args,parent=self.index)); return Logger(index)
    def log(self,**fields):
        assert not rows[self.index].get('closed')
        if 'output' in fields: rows[self.index]['output_logs'] = rows[self.index].get('output_logs', 0) + 1
        rows[self.index].update(fields)
    def end(self,**args):
        assert not rows[self.index].get('closed'); rows[self.index]['closed']=True

def draw():
    assert rows[0]['name']=='workflow' and not rows[0].get('closed')
    assert rows[-1]['name']=='first' and not rows[-1].get('closed')
    return 0.5
assert run_traced(Logger(),random_source=draw)==5
assert all(row.get('output_logs') == 1 for row in rows), 'trace output must be logged exactly once'
assert all(row['closed'] for row in rows)
rows.clear()
try: run_traced(Logger(), {'divisor':'invalid'}, random_source=draw)
except TypeError: pass
else: raise AssertionError('expected entry validation failure')
assert rows == [], 'input validation must run before tracing or sampling'
try: run_traced(Logger(),{'divisor':0},random_source=draw)
except (ValueError,ZeroDivisionError): pass
else: raise AssertionError('expected division failure')
assert [r['name'] for r in rows]==['workflow','first','compute']
assert rows[1]['output'] in [1,2] and 'error' not in rows[1]
assert rows[0]['error'] and rows[2]['error']
assert rows[2]['input']=={'divisor':0}
assert all(row['closed'] for row in rows)
"#,
            )
        } else {
            (
                "node",
                r#"
import assert from 'node:assert/strict';
import {runTraced} from './workflow_agent.mts';
const rows=[];
class Logger {
    constructor(index=null){this.index=index;}
    startSpan(args){if(this.index!==null)assert.ok(!rows[this.index].closed);const index=rows.length;rows.push({...args,parent:this.index});return new Logger(index);}
    log(fields){assert.ok(!rows[this.index].closed);if ('output' in fields) rows[this.index].outputLogs = (rows[this.index].outputLogs ?? 0) + 1;Object.assign(rows[this.index],fields);}
    end(){assert.ok(!rows[this.index].closed);rows[this.index].closed=true;}
}
function draw(){assert.equal(rows[0].name,'workflow');assert.ok(!rows[0].closed);assert.equal(rows.at(-1).name,'first');assert.ok(!rows.at(-1).closed);return .5;}
assert.equal(runTraced(new Logger(),undefined,draw),5);
assert.ok(rows.every(row=>row.outputLogs===1), 'trace output must be logged exactly once');
assert.ok(rows.every(row=>row.closed));rows.length=0;
assert.throws(()=>runTraced(new Logger(),{divisor:'invalid'},draw));
assert.deepEqual(rows,[], 'input validation must run before tracing or sampling');
assert.throws(()=>runTraced(new Logger(),{divisor:0},draw));
assert.deepEqual(rows.map(row=>row.name),['workflow','first','compute']);
assert.ok([1,2].includes(rows[1].output));assert.ok(!rows[1].error);
assert.ok(rows[0].error && rows[2].error);assert.deepEqual(rows[2].input,{divisor:0});
assert.ok(rows.every(row=>row.closed));
"#,
            )
        };
        let mut command = Command::new(program);
        if lang == "typescript" {
            command.args(["--input-type=module", "-e", script]);
        } else {
            command.args(["-c", script]);
        }
        let ran = command.current_dir(root.join(lang).join("src/agents")).output().unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_native_values_preserve_results_names_and_lazy_failures() {
    let root = std::env::temp_dir().join(format!("bts-native-values-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("native.bt"),
        r#"
agent "native" { entrypoint = trace.t }
trace "t" {
    vars { root_request = 6 unused = self.input.missing }
    input = {}
    output = { value = tool.calculate.output, guarded = false && var.unused }
    tool "calculate" {
        input = { base = var.root_request }
        vars { next = self.input.base + 1 }
        output = { amount = var.next * 2, copy = var.next, label = upper("ready") }
    }
    repeat "checks" { count = 2 tool "check" { output = repeat.index } }
}
"#,
    )
    .unwrap();
    for lang in ["python", "typescript"] {
        let built = bts()
            .current_dir(&root)
            .args(["build", "agent", "--from", "native.bt", "--lang", lang, "--out", lang])
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let extension = if lang == "python" { "py" } else { "mts" };
        let source = fs::read_to_string(root.join(lang).join(format!("src/agents/native_agent.{extension}"))).unwrap();
        assert!(!source.contains("_arithmetic("), "{source}");
        assert!(!source.contains("_repeat("), "{source}");
        assert!(!source.contains("_scope"), "{source}");
        assert!(!source.contains("_iteration("), "{source}");
        let (program, script) = if lang == "python" {
            (
                "python3",
                r#"
from native_agent import run, run_details
expected={'value': {'amount':14,'copy':7,'label':'READY'},'guarded':False}
assert run()==expected
assert run({'unexpected':True})==expected
assert [s['output'] for s in run_details()['spans'] if s['name']=='check']==[0,1]
"#,
            )
        } else {
            (
                "node",
                r#"
import assert from 'node:assert/strict';
import {run,runDetails} from './native_agent.mts';
const expected={value:{amount:14,copy:7,label:'READY'},guarded:false};
assert.deepEqual(run(),expected);
assert.deepEqual(run({unexpected:true}),expected);
assert.deepEqual(runDetails().spans.filter(s=>s.name==='check').map(s=>s.output),[0,1]);
"#,
            )
        };
        let mut command = Command::new(program);
        if lang == "python" {
            command.args(["-c", script]);
        } else {
            command.args(["--input-type=module", "-e", script]);
        }
        let ran = command.current_dir(root.join(lang).join("src/agents")).output().unwrap();
        assert!(ran.status.success(), "{lang}: {}", String::from_utf8_lossy(&ran.stderr));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scorer_rebuild_updates_existing_agent_evals_in_both_languages() {
    let root = write_project_context(Uuid::new_v4());
    let shape = root.join("support.bt");
    let source = r#"
agent "support" { entrypoint = trace.support }
trace "support" { input = "hello" output = self.input }
dataset "cases" {
    case "hello" { input = "hello" expected = "hello" }
}
scorer "quality" { code { score = 0.25 } }
experiment "support" {
    dataset = dataset.cases
    task = trace.support
    scorers = [scorer.quality]
}
"#;
    for lang in ["python", "typescript"] {
        fs::write(&shape, source).unwrap();
        let build = |target: &str| {
            let output = bts()
                .current_dir(&root)
                .args(["build", target, "--from", "support.bt", "--lang", lang, "--out", lang])
                .env_remove("BRAINTRUST_API_KEY")
                .output()
                .unwrap();
            assert!(output.status.success(), "{lang}: {}", String::from_utf8_lossy(&output.stderr));
        };
        build("agent");
        let (agent, evaluation) = if lang == "python" {
            ("src/agents/support_agent.py", "src/evals/support_eval.py")
        } else {
            ("src/agents/support_agent.mts", "src/evals/support.eval.mts")
        };
        let agent = root.join(lang).join(agent);
        let evaluation = root.join(lang).join(evaluation);
        let original_agent = fs::read(&agent).unwrap();
        let original_evaluation = fs::read(&evaluation).unwrap();
        let check = |score: &str| {
            let (program, args): (&str, &[&str]) = if lang == "python" {
                (
                    "python3",
                    &[
                        "-c",
                        r#"
import os
from types import SimpleNamespace
from evals.support_eval import CASES, SCORERS, task
class Span:
    def start_span(self, **kwargs): return self
    def log(self, **kwargs): pass
    def end(self, **kwargs): pass
case = CASES[0]
output = task(case['input'], SimpleNamespace(span=Span()))
assert output == case['expected']
assert SCORERS[0](output=output, expected=case['expected'])['score'] == float(os.environ['EXPECTED_SCORE'])
"#,
                    ],
                )
            } else {
                (
                    "node",
                    &[
                        "--input-type=module",
                        "-e",
                        r#"
import assert from 'node:assert/strict';
import {cases, scorers, task} from './evals/support.eval.mts';
const span = {startSpan(){return this;}, log(){}, end(){}};
const row = cases[0];
const output = task(row.input, {span});
assert.equal(output, row.expected);
assert.equal(scorers[0]({output, expected: row.expected}).score, Number(process.env.EXPECTED_SCORE));
"#,
                    ],
                )
            };
            let output = Command::new(program)
                .args(args)
                .current_dir(root.join(lang).join("src"))
                .env("EXPECTED_SCORE", score)
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .env_remove("BRAINTRUST_API_KEY")
                .output()
                .unwrap();
            assert!(output.status.success(), "{lang}: {}", String::from_utf8_lossy(&output.stderr));
        };
        check("0.25");
        fs::write(&shape, source.replace("score = 0.25", "score = 0.75")).unwrap();
        build("scorers");
        assert_eq!(fs::read(&agent).unwrap(), original_agent);
        assert_eq!(fs::read(&evaluation).unwrap(), original_evaluation);
        check("0.75");
    }
    fs::remove_dir_all(root).unwrap();
}
