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
    let shape = write_shape(include_str!("fixtures/dynamic.bt"));
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
    let shape = write_shape(include_str!("../examples/scoring_automations.bt"));
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
    assert!(code.contains("def scorer_reset_outcome_honesty(output, input=None, expected=None, metadata=None):"));
    assert!(code.contains("for repeat_index in range("));
    assert!(code.contains("handler=scorer_reset_outcome_honesty"));
    assert!(code.contains("project = braintrust.projects.create(name=\"test-project\")"));
    let pyproject: toml::Value = toml::from_str(&fs::read_to_string(out.join("pyproject.toml")).unwrap()).unwrap();
    let dependencies = pyproject["project"]["dependencies"].as_array().unwrap();
    assert!(dependencies.iter().any(|entry| entry.as_str() == Some("braintrust>=0.39,<1")));
    assert!(dependencies.iter().any(|entry| entry.as_str() == Some("pydantic>=2,<3")));
    assert!(!out.join("package.json").exists());

    // the language override flips the extension
    let shape = write_shape(include_str!("../examples/scoring_automations.bt"));
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
    assert!(code.contains("function scorer_reset_outcome_honesty({ input, output, expected, metadata }: { input?: any; output: any; expected?: any; metadata?: any }): { name: string; score: number; metadata: Record<string, unknown> } {"));
    assert!(code.contains("for (let repeatIndex = 0;"));
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
    let shape = write_shape(include_str!("../examples/scoring_automations.bt"));
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
