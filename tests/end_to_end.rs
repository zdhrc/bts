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
            output = "Hi!"
            metrics = { tokens = 4 }
        }
    }
}
"#;

#[test]
fn dry_run_expands_a_shape_into_the_requested_window() {
    let shape = write_shape();
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
}

#[test]
fn writes_generated_events_to_the_configured_endpoint() {
    let shape = write_shape();
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

fn write_shape() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("bts-generate-{}.bt", Uuid::new_v4()));
    fs::write(&path, SIMPLE_SHAPE).unwrap();
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
