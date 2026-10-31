use bts_core::{
    mocking::{code, data},
    providers::{Bundle, CodeProvider, DataProvider, braintrust},
    selection::filter::WriteFilter,
};
use bts_lang::semantics::compile;
use std::{
    cell::RefCell,
    convert::Infallible,
    error::Error as _,
    fmt,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

struct Archive;

impl Bundle for Archive {
    const ID: &'static str = "archive";
}

struct ArchiveConfig {
    capacity: usize,
}

struct ArchiveClient {
    capacity: usize,
    names: RefCell<Vec<String>>,
}

struct ArchiveFormat;
struct ArchiveRows(Vec<String>);

impl data::AttachmentFormat for ArchiveFormat {}

impl data::Format for ArchiveFormat {
    type Output = ArchiveRows;
    type Error = Infallible;

    fn encode(&self, batch: &data::EventBatch) -> Result<ArchiveRows, Infallible> {
        Ok(ArchiveRows(batch.events.iter().map(|event| event.name.clone()).collect()))
    }
}

struct ArchiveSink<'a>(&'a ArchiveClient);

impl data::Sink for ArchiveSink<'_> {
    type Input = ArchiveRows;
    type Receipt = usize;
    type Error = ArchiveError;

    fn send(&self, rows: &ArchiveRows) -> Result<usize, ArchiveError> {
        let mut names = self.0.names.borrow_mut();
        if names.len() + rows.0.len() > self.0.capacity {
            return Err(ArchiveError::Full);
        }
        names.extend(rows.0.iter().cloned());
        Ok(names.len())
    }
}

#[derive(Debug, PartialEq)]
enum ArchiveError {
    InvalidCapacity,
    Full,
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity => formatter.write_str("archive capacity must be positive"),
            Self::Full => formatter.write_str("archive is full"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl DataProvider for Archive {
    type Config = ArchiveConfig;
    type Client = ArchiveClient;
    type ClientError = ArchiveError;
    type Format = ArchiveFormat;
    type Receipt = usize;
    type SendError = ArchiveError;
    type Sink<'a> = ArchiveSink<'a>;

    fn format(&self) -> ArchiveFormat {
        ArchiveFormat
    }

    fn client(&self, config: &ArchiveConfig) -> Result<ArchiveClient, ArchiveError> {
        if config.capacity == 0 {
            return Err(ArchiveError::InvalidCapacity);
        }
        Ok(ArchiveClient {
            capacity: config.capacity,
            names: RefCell::new(Vec::new()),
        })
    }

    fn sink<'a>(&'a self, client: &'a ArchiveClient) -> ArchiveSink<'a> {
        ArchiveSink(client)
    }
}

type RunResult<B> = data::RunResult<
    <B as DataProvider>::Receipt,
    <<B as DataProvider>::Format as data::Format>::Error,
    <B as DataProvider>::SendError,
>;

fn generate_and_send<B: DataProvider>(bundle: &B, client: &B::Client) -> RunResult<B> {
    let filter = WriteFilter::default();
    let request = data::Request {
        model: compile(r#"trace "flow" { output = "ok" }"#).unwrap(),
        count: 1,
        over: Duration::from_secs(60),
        distribution: data::Distribution::Linear,
        until: UNIX_EPOCH + Duration::from_secs(3600),
        seed: 4,
        filter: &filter,
        source: Path::new("shape.bt"),
    };
    data::run(request, &bundle.format(), &bundle.sink(client))
}

#[test]
fn one_data_workflow_accepts_braintrust_and_a_data_only_bundle() {
    let archive = Archive;
    let client = archive.client(&ArchiveConfig { capacity: 1 }).unwrap();
    assert_eq!(generate_and_send(&archive, &client).unwrap().receipt, 1);
    assert_eq!(&*client.names.borrow(), &["flow"]);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut config = braintrust::config::Config::new("test-key".to_owned(), uuid::Uuid::nil());
    config.api_url = format!("http://{}", listener.local_addr().unwrap());
    config.request_timeout = Duration::from_secs(3);
    config.retry_attempts = 1;
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut reader = BufReader::new(stream);
        let mut request = String::new();
        reader.read_line(&mut request).unwrap();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let response = r#"{"row_ids":["row"]}"#;
        write!(
            reader.get_mut(),
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response,
        )
        .unwrap();
        (request, serde_json::from_slice::<serde_json::Value>(&body).unwrap())
    });

    let bundle = braintrust::Bundle;
    let client = bundle.client(&config).unwrap();
    assert_eq!(generate_and_send(&bundle, &client).unwrap().receipt.row_count(), 1);
    let (request, payload) = server.join().unwrap();
    assert!(request.starts_with("POST /v1/project_logs/"));
    assert_eq!(payload["events"][0]["output"], "ok");
    assert_eq!(Archive::ID, "archive");
    assert_eq!(braintrust::Bundle::ID, "braintrust");
}

#[test]
fn provider_configuration_and_delivery_errors_keep_their_types() {
    let bundle = Archive;
    assert!(matches!(
        bundle.client(&ArchiveConfig { capacity: 0 }),
        Err(ArchiveError::InvalidCapacity)
    ));
    let client = bundle.client(&ArchiveConfig { capacity: 1 }).unwrap();
    generate_and_send(&bundle, &client).unwrap();
    let error = generate_and_send(&bundle, &client).err().unwrap();
    assert!(matches!(
        error,
        data::RunError::Write(data::WriteError::Send(ArchiveError::Full))
    ));
    assert_eq!(
        error.source().unwrap().source().unwrap().downcast_ref(),
        Some(&ArchiveError::Full)
    );
}

struct LocalCode;

impl Bundle for LocalCode {
    const ID: &'static str = "local-code";
}

impl CodeProvider for LocalCode {
    type CodeConfig = ();
    type Emitter = code::languages::Languages;
    type Target = PlainTarget;

    fn emitter(&self) -> Self::Emitter {
        code::languages::Languages
    }

    fn code(&self, _: &()) -> PlainTarget {
        PlainTarget
    }
}

struct PlainTarget;

impl code::Target for PlainTarget {
    type Error = std::io::Error;

    fn slug(&self, name: &str) -> Option<String> {
        let slug = name.to_ascii_lowercase();
        (!slug.is_empty()
            && slug
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
        .then_some(slug)
    }

    fn code(&self, group: code::CodeGroup<'_>, module: code::Module) -> Result<code::SourceFile, Self::Error> {
        Ok(code::SourceFile {
            name: format!("{}.py", group.stem),
            contents: module.source,
            slugs: group.items.iter().map(|item| item.slug.to_owned()).collect(),
            lang: group.lang,
        })
    }

    fn judge(&self, _: code::Judge<'_>) -> Result<code::SourceFile, Self::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "local code does not run judges",
        ))
    }

    fn codebase(&self, assembly: &code::Assembly, _: &code::CodebaseContext<'_>) -> Result<code::CodebasePlan, Self::Error> {
        Ok(code::CodebasePlan {
            files: assembly
                .files
                .iter()
                .map(|source| code::CodebaseFile {
                    path: source.name.clone().into(),
                    contents: source.contents.clone(),
                    slugs: source.slugs.clone(),
                })
                .collect(),
        })
    }
}

fn assemble<B: CodeProvider>(bundle: &B, config: &B::CodeConfig) -> code::Assembly {
    let model = compile(r#"scorer "quality" { code { score = 0.5 } }"#).unwrap();
    code::assemble(&model.scorers, None, &bundle.emitter(), &bundle.code(config)).unwrap()
}

#[test]
fn one_code_workflow_accepts_braintrust_and_a_code_only_bundle() {
    let local = assemble(&LocalCode, &());
    assert_eq!(local.files[0].name, "quality.py");
    assert!(local.files[0].contents.contains("\"score\": 0.5"));

    let braintrust = assemble(&braintrust::Bundle, &"test-project".to_owned());
    assert!(braintrust.files[0].contents.contains("test-project"));
    assert!(braintrust.files[0].contents.contains("handler=scorer_quality,"));
}
