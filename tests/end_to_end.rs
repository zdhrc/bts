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
    let output = Command::new(env!("CARGO_BIN_EXE_bts"))
        .args(["generate", "--from"])
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
fn dry_run_varies_trace_shapes_through_dynamic_blocks() {
    let shape = write_shape(include_str!("fixtures/dynamic.bt"));
    let output = Command::new(env!("CARGO_BIN_EXE_bts"))
        .args(["generate", "--from"])
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
    let output = Command::new(env!("CARGO_BIN_EXE_bts"))
        .args(["generate", "--from"])
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
fn renders_a_generation_diagnostic_for_dynamic_division_by_zero() {
    let shape = write_shape(r#"trace "t" { input = 100 / range(0, 0) }"#);
    let output = Command::new(env!("CARGO_BIN_EXE_bts"))
        .args(["generate", "--from"])
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
    let output = Command::new(env!("CARGO_BIN_EXE_bts"))
        .args(["generate", "--from"])
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
    let (api_url, request) = serve_insert(6);
    let output = Command::new(env!("CARGO_BIN_EXE_bts"))
        .args(["generate", "--from"])
        .arg(&shape)
        .args(["--count", "2", "--over", "1h"])
        .env("BRAINTRUST_API_KEY", "test-secret")
        .env("BRAINTRUST_PROJECT_ID", project_id.to_string())
        .env("BRAINTRUST_API_URL", api_url)
        .output()
        .unwrap();
    fs::remove_file(shape).unwrap();
    let request = request.recv_timeout(Duration::from_secs(2)).unwrap();
    let (headers, body) = split_request(&request);
    let payload: JsonValue = serde_json::from_slice(body).unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("inserted 2 traces and 4 child spans"));
    assert!(headers.starts_with(&format!("POST /v1/project_logs/{project_id}/insert HTTP/1.1\r\n")));
    assert!(headers.to_ascii_lowercase().contains("authorization: bearer test-secret\r\n"));
    assert_eq!(payload["events"].as_array().unwrap().len(), 6);
}

fn write_shape(source: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("bts-generate-{}.bt", Uuid::new_v4()));
    fs::write(&path, source).unwrap();
    path
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
