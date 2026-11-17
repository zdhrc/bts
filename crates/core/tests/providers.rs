use bts_core::providers::braintrust::mocking::compile;
use bts_core::{
    mocking::{Consumer, Function, Helper, ScorerLang, code, data},
    providers::{Bundle, CodeProvider, Contributor, DataProvider, Guidance, Provider, With, braintrust, conformance, world},
    selection::filter::WriteFilter,
};
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
        model: compile(
            r#"(flow_root:task {name: "flow"}) {output: "ok", input: flow.input};
(flow:agent) {input: null, output: flow_root.output};
(flow)-[:next]->(flow_root);"#,
        )
        .unwrap(),
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

    fn code(&self, group: code::CodeGroup<'_>, module: code::Module) -> Result<Vec<code::SourceFile>, Self::Error> {
        Ok(vec![code::SourceFile {
            name: format!("{}.py", group.stem),
            contents: module.source,
            slugs: group.items.iter().map(|item| item.slug.to_owned()).collect(),
            lang: group.lang,
        }])
    }

    fn judge(&self, _: code::Judge<'_>) -> Result<code::SourceFile, Self::Error> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "local code does not run judges",
        ))
    }

    fn codebase(&self, assembly: &code::Assembly, _: &code::CodebaseContext<'_>) -> Result<code::CodebasePlan, Self::Error> {
        Ok(code::CodebasePlan {
            removals: Vec::new(),
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
    let model = compile(r#"("quality":scorer) {score: 0.5};"#).unwrap();
    code::assemble(&model.scorers, None, &bundle.emitter(), &bundle.code(config)).unwrap()
}

#[test]
fn one_code_workflow_accepts_braintrust_and_a_code_only_bundle() {
    let local = assemble(&LocalCode, &());
    assert_eq!(local.files[0].name, "quality.py");
    assert!(local.files[0].contents.contains("\"score\": 0.5"));

    let braintrust = assemble(&braintrust::Bundle, &"test-project".to_owned());
    assert!(braintrust.files[1].contents.contains("test-project"));
    assert!(braintrust.files[1].contents.contains("handler=scorer_quality,"));
}

#[test]
fn braintrust_conforms_to_the_provider_contract() {
    assert_eq!(conformance::check(&braintrust::Bundle), Vec::<String>::new());
}

#[test]
fn world_conforms_to_the_provider_contract() {
    assert_eq!(conformance::check(&world::Bundle), Vec::<String>::new());
    // it walks its own graphs, so it lists the walk, what a walk follows and the keys a walk reads
    // a step is any node that gives something, so no step kind is listed
    let schema = world::Bundle.schema();
    assert_eq!(schema.nodes, ["walk", "conversation", "relation", "network"]);
    assert_eq!(schema.edges, ["next", "incidence"]);
    assert_eq!(schema.forks, ["weighted", "exclusive", "repeat"]);
    assert!(schema.junctions.is_empty());
    assert_eq!(
        schema.context,
        [
            "steps",
            "trace_index",
            "place",
            "identity",
            "speaker",
            "spoken",
            "styled",
            "related",
            "members"
        ]
    );
    // the twelve English functions and merge are declared, and what a fork and a conversation lower to is internal
    assert_eq!(schema.functions.len(), 13);
    assert_eq!(world::Bundle.functions().len(), 18);
    let internal = ["world.pick", "world.turn", "world.id", "world.draws", "world.say"];
    assert_eq!(world::Bundle.internal(), internal);
    for function in world::Bundle.functions() {
        assert!(function.runs(Consumer::Data), "{}", function.name);
        assert!(function.runs(Consumer::Agent(ScorerLang::Python)), "{}", function.name);
        assert!(function.runs(Consumer::Agent(ScorerLang::Typescript)), "{}", function.name);
        // the English functions run in scorers too, and what serves a walk does not
        let walks = function.name == "merge" || internal.contains(&function.name);
        assert_eq!(function.runs(Consumer::Scorer), !walks, "{}", function.name);
    }
}

#[test]
fn a_provider_joined_by_a_contributor_has_the_kinds_and_functions_of_both() {
    let joined = With(braintrust::Bundle, world::Bundle);
    assert_eq!(conformance::check(&joined), Vec::<String>::new());
    // the provider keeps its identity, and its definitions compile first
    let schema = joined.schema();
    assert_eq!((schema.id, schema.name), ("braintrust", "Braintrust mocking"));
    let sources: Vec<_> = schema.definitions.iter().map(|source| source.name).collect();
    assert_eq!(sources, ["braintrust.bt", "world.bt"]);
    assert_eq!(schema.functions.len(), 16);
    assert_eq!(joined.functions().len(), 21);
    assert_eq!(joined.internal().len(), 5);
    let parts: Vec<_> = joined.parts().iter().map(|part| part.schema.id).collect();
    assert_eq!(parts, ["braintrust", "world"]);

    // a shape uses the kinds of both, and a call finds what runs it whichever declared it
    let source = r#"(a:agent) {input: "hi", output: t.output};
(a)-[:next]->(t:task) {input: a.input, output: {said: s.text, counted: tokens(s.text)}};
(t)-[:calls]->(s:statement {subject: "the desk", verb: "open"});"#;
    let model = joined.compile(source).unwrap();
    let batch = data::generate(
        model,
        1,
        Duration::from_secs(1),
        data::Distribution::Linear,
        UNIX_EPOCH + Duration::from_secs(10),
        1,
        &(),
    )
    .unwrap();
    let output = batch.events[0].output.as_ref().unwrap().project(&()).unwrap();
    assert_eq!(output["said"], "The desk opens.");
    assert!(output["counted"].as_i64().unwrap() > 0);

    // alone, the provider knows neither the kind nor the function
    let errors = braintrust::Bundle.compile_module(source, false).unwrap_err();
    assert!(errors.iter().any(|error| error.what.contains("statement")), "{errors:?}");
    let errors = (braintrust::Bundle)
        .compile_module(r#"(v:vars) {value: realize({})};"#, false)
        .unwrap_err();
    assert!(errors[0].what.starts_with("unknown function `realize`"), "{errors:?}");
}

#[test]
fn a_function_two_providers_declare_fails_the_conformance_check() {
    // a second contributor that declares a function the provider already has
    struct Clashing;
    impl Bundle for Clashing {
        const ID: &'static str = "clashing";
    }
    impl Contributor for Clashing {
        fn schema(&self) -> bts_lang::ProviderSchema<'static> {
            const DECLARED: &[bts_lang::FunctionDesc] = &[described("tokens", &[])];
            bts_lang::ProviderSchema {
                id: "clashing",
                name: "Clashing",
                definitions: &[],
                nodes: &[],
                edges: &[],
                forks: &[],
                junctions: &[],
                context: &[],
                functions: DECLARED,
            }
        }
        fn functions(&self) -> &'static [Function] {
            static FUNCTIONS: &[Function] = &[Function {
                name: "tokens",
                check: None,
                data: Some(|_, _| Ok(data::Value::Null)),
                python: None,
                typescript: None,
            }];
            FUNCTIONS
        }
        fn guidance(&self) -> Guidance {
            Guidance {
                workflow: "",
                presentation: "",
                definitions: "",
            }
        }
    }
    assert_eq!(conformance::check(&Clashing), Vec::<String>::new());
    let problems = conformance::check(&With(braintrust::Bundle, Clashing));
    assert_eq!(
        problems,
        [
            "function `tokens` has more than one implementation",
            "function `tokens` is declared twice",
            "function `tokens` has more than one implementation",
        ]
    );
    // and a call to it is ambiguous, whatever the order
    let errors = With(braintrust::Bundle, Clashing)
        .compile_module(r#"(v:vars) {value: tokens("a")};"#, false)
        .unwrap_err();
    assert!(
        errors[0].what.starts_with("function `tokens` is defined more than once"),
        "{errors:?}"
    );
}

const fn described(name: &'static str, params: &'static [bts_lang::ParamDesc]) -> bts_lang::FunctionDesc {
    bts_lang::FunctionDesc {
        doc: bts_lang::spec::FuncDesc {
            tags: &[],
            id: bts_lang::spec::Id::new("func.test"),
            name,
            syntax: name,
            summary: "A function declared for the conformance check.",
            examples: &[],
            rules: &[],
        },
        params,
        returns: bts_lang::FieldType::Any,
        shape: None,
    }
}

// declares a function twice, redefines a built-in, leaves a parameter without a type and one function without an implementation
const INCONSISTENT_DESCRIPTORS: &[bts_lang::FunctionDesc] = &[
    described("measure", &[]),
    described("measure", &[]),
    described("upper", &[]),
    described(
        "weigh",
        &[bts_lang::ParamDesc {
            name: "value",
            accepts: &[],
            nullable: false,
            optional: false,
            constant: false,
            bounds: None,
        }],
    ),
];

// implements a function nobody declared, one that runs nowhere, and a helper whose text defines another name
static INCONSISTENT_FUNCTIONS: &[Function] = &[
    Function {
        name: "measure",
        check: None,
        data: None,
        python: Some(Helper {
            name: "_measure",
            reads_as: None,
            random: false,
            seeded: false,
            counts: false,
            texts: false,
            plain: false,
            text: "\ndef _weigh(value):\n    return 0\n",
            operation: None,
            runtime: None,
            scorer: None,
        }),
        typescript: None,
    },
    Function {
        name: "upper",
        check: None,
        data: None,
        python: None,
        typescript: None,
    },
    Function {
        name: "orphan",
        check: Some(|_| Ok(())),
        data: None,
        python: None,
        typescript: None,
    },
];

// lists kinds it never defines, files one under the wrong construct, and says nothing about its own kind
struct Inconsistent;

impl Bundle for Inconsistent {
    const ID: &'static str = "inconsistent";
}

impl Contributor for Inconsistent {
    fn schema(&self) -> bts_lang::ProviderSchema<'static> {
        bts_lang::ProviderSchema {
            id: "other",
            name: "Inconsistent",
            definitions: &[bts_lang::semantics::Source {
                name: "inconsistent.bt",
                text: "+(widget:node);",
            }],
            nodes: &["widget", "gadget", "next"],
            edges: &["next"],
            forks: &[],
            junctions: &[],
            context: &[],
            functions: INCONSISTENT_DESCRIPTORS,
        }
    }

    fn functions(&self) -> &'static [Function] {
        INCONSISTENT_FUNCTIONS
    }

    fn guidance(&self) -> Guidance {
        Guidance {
            workflow: "## Workflow\n\nNothing about the kinds.\n",
            presentation: "",
            definitions: "",
        }
    }
}

impl Provider for Inconsistent {
    type Lowered = ();

    fn lower(&self, _: &bts_lang::Model, _: &'static [Function]) -> Result<(), bts_lang::Diag> {
        Ok(())
    }
}

#[test]
fn the_conformance_check_reports_each_inconsistency() {
    assert_eq!(
        conformance::check(&Inconsistent),
        [
            "schema id `other` is not the bundle id `inconsistent`",
            "node kind `gadget` is neither a primitive nor defined by the provider",
            "`next` is listed under node kinds and belongs to edge kinds",
            "function `measure` is declared twice",
            "function `upper` is a built-in function",
            "function `weigh` accepts no type for `value`",
            "function `weigh` has no implementation",
            "the helper text of `measure` does not define `_measure`",
            "function `upper` runs nowhere",
            "the implementation `orphan` belongs to no declared function",
            "function `orphan` runs nowhere",
            "the guidance does not mention the kind `widget`",
        ]
    );
}

#[test]
fn the_conformance_check_locates_definitions_that_do_not_compile() {
    struct Broken;
    impl Bundle for Broken {
        const ID: &'static str = "broken";
    }
    impl Contributor for Broken {
        fn schema(&self) -> bts_lang::ProviderSchema<'static> {
            bts_lang::ProviderSchema {
                id: "broken",
                name: "Broken",
                definitions: &[bts_lang::semantics::Source {
                    name: "broken.bt",
                    text: "+(widget:missing);",
                }],
                nodes: &[],
                edges: &[],
                forks: &[],
                junctions: &[],
                context: &[],
                functions: &[],
            }
        }
        fn guidance(&self) -> Guidance {
            Inconsistent.guidance()
        }
    }
    let problems = conformance::check(&Broken);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("broken.bt:"), "{problems:?}");
}

#[test]
fn a_provider_compiles_source_against_its_own_definitions() {
    let errors = Inconsistent.compile_module("(a:span);", false).unwrap_err();
    assert!(errors[0].what.contains("span"), "{errors:?}");
    assert!(Inconsistent.compile_module("(a:widget);", false).is_ok());
    assert_eq!(braintrust::Bundle.schema().definition(1).unwrap().name, "braintrust.bt");
    assert!(braintrust::Bundle.schema().definition(0).is_none());
}

const TWO_AGENTS: &str = r#"(files:agent) {input: null, output: work.output};
(files)-[:next]->(work);
(work:task {name: "work"}) {
    input: files.input,
    output: {report: attachment("report.pdf", "application/pdf"), size: tokens("four words of text")},
};
(other:agent) {input: null, output: plain.output};
(other)-[:next]->(plain);
(plain:task {name: "plain"}) {input: other.input, output: noise(8)};
("length":scorer {lang: "python"}) {score: clamp(tokens(context.output) / 400, 0.1, 0.9)};
("fixed":scorer {lang: "python"}) {score: 0.5};
"#;

fn located<'a>(source: &'a str, diag: &bts_lang::Diag) -> &'a str {
    &source[diag.r#where.start..diag.r#where.end]
}

#[test]
fn braintrust_declares_and_implements_the_functions_that_left_the_language() {
    let declared: Vec<&str> = (braintrust::Bundle.schema().functions.iter())
        .map(|function| function.doc.name)
        .collect();
    assert_eq!(declared, ["tokens", "attachment", "noise"]);
    for name in &declared {
        assert!(bts_lang::spec::SPEC.function(name).is_none(), "{name}");
        assert!(bts_lang::semantics::compile(&format!("(v:vars) {{value: {name}()}};")).is_err());
    }
    let runs = |name: &str, consumer| {
        let functions = braintrust::Bundle.functions();
        functions
            .iter()
            .find(|function| function.name == name)
            .unwrap()
            .runs(consumer)
    };
    let agents = [Consumer::Agent(ScorerLang::Python), Consumer::Agent(ScorerLang::Typescript)];
    for name in ["tokens", "noise"] {
        assert!(
            runs(name, Consumer::Data) && agents.iter().all(|agent| runs(name, *agent)),
            "{name}"
        );
    }
    assert!(runs("attachment", Consumer::Data) && !agents.iter().any(|agent| runs("attachment", *agent)));
    assert!(!declared.iter().any(|name| runs(name, Consumer::Scorer)));

    // data generation runs all three
    let model = compile(TWO_AGENTS).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let events = data::generate(
        model,
        2,
        Duration::from_secs(60),
        data::Distribution::Linear,
        now,
        7,
        &braintrust::Bundle.format(),
    )
    .unwrap();
    let outputs: Vec<_> = events.events.iter().filter_map(|event| event.output.as_ref()).collect();
    assert!(
        outputs
            .iter()
            .any(|output| matches!(output["report"], data::Value::Attachment(_)))
    );
    assert!(outputs.iter().any(|output| output["size"].as_i64() == Some(4)));
    assert!(
        outputs
            .iter()
            .any(|output| output.as_str().is_some_and(|noise| noise.len() == 8))
    );
}

#[test]
fn a_function_a_consumer_cannot_run_is_reported_where_it_is_called() {
    let model = compile(TWO_AGENTS).unwrap();
    let target = braintrust::mocking::code::CodeTarget::new("local-test");

    // an agent build reports the call in the agent it generates
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let Err(code::AgentError::Unsupported(diags)) = code::assemble_agents(&model, lang, &target) else {
            panic!("expected located diagnostics")
        };
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(
            located(TWO_AGENTS, &diags[0]),
            r#"attachment("report.pdf", "application/pdf")"#
        );
        let language = if lang == ScorerLang::Python { "Python" } else { "TypeScript" };
        assert_eq!(
            diags[0].what,
            format!("function `attachment` is not supported by {language} agent generation")
        );
        // an agent that is not generated does not stop the one that is
        let selected = bts_core::selection::agents(&model, &["agent.other".parse().unwrap()]).unwrap();
        assert!(code::assemble_agents(&selected, lang, &target).is_ok());
    }

    // a scorer build reports the call in the scorer it generates
    let emitter = braintrust::Bundle.emitter();
    let target = braintrust::Bundle.code(&"local-test".to_owned());
    let Err(code::AssemblyError::Unsupported(diags)) = code::assemble(&model.scorers, None, &emitter, &target) else {
        panic!("expected located diagnostics")
    };
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(located(TWO_AGENTS, &diags[0]), "tokens(context.output)");
    assert_eq!(diags[0].what, "function `tokens` is not supported by scorer generation");
    assert!(code::assemble(&model.scorers[1..], None, &emitter, &target).is_ok());
}

#[test]
fn a_computed_field_a_consumer_cannot_run_is_reported_at_its_call() {
    // the kind states the call once, so the report points into the kind for the agent that uses it
    let source = r#"+(reporting:node) {output: {value: {report: attachment("report.pdf", "application/pdf")}}}
    -[:extends]->(task);
(files:agent) {input: null, output: work.output};
(files)-[:next]->(work:reporting {name: "work"}) {input: files.input};
(other:agent) {input: null, output: plain.output};
(other)-[:next]->(plain:task {name: "plain"}) {input: other.input, output: "plain"};
"#;
    let model = compile(source).unwrap();
    let target = braintrust::mocking::code::CodeTarget::new("local-test");
    for lang in [ScorerLang::Python, ScorerLang::Typescript] {
        let Err(code::AgentError::Unsupported(diags)) = code::assemble_agents(&model, lang, &target) else {
            panic!("expected located diagnostics")
        };
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(located(source, &diags[0]), r#"attachment("report.pdf", "application/pdf")"#);
        assert_eq!(diags[0].r#where.source, 0);
        // an agent that does not use the kind is still generated
        let selected = bts_core::selection::agents(&model, &["agent.other".parse().unwrap()]).unwrap();
        assert!(code::assemble_agents(&selected, lang, &target).is_ok());
    }
}

#[test]
fn an_llm_span_counts_its_own_tokens() {
    use braintrust::functions::count_tokens;
    // no step writes a token count: the llm kind computes them, and an object in metrics is merged over them
    let source = r#"(chat:agent) {input: "How do I rotate API keys?", output: full.output};
(chat)-[:next]->(request:task {name: "request"}) {input: chat.input, output: full.output};
(request)-[:calls]->(bare:llm {name: "bare"})
    -[:next]->(full:llm {name: "full"}) {input: request.input, output: "Under Settings."}
        -[:next]->(timed:llm {name: "timed"}) {input: request.input, output: "Under Settings.", metrics: {time_to_first_token: 0.25}}
            -[:next]->(cached:llm {name: "cached"}) {input: request.input, output: "Under Settings.", metrics: {prompt_tokens: 0}};
"#;
    let model = compile(source).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let batch = data::generate(
        model,
        1,
        Duration::from_secs(60),
        data::Distribution::Linear,
        now,
        7,
        &braintrust::Bundle.format(),
    )
    .unwrap();
    let metric = |span: &str, key: &str| -> Option<data::Value> {
        let event = batch.events.iter().find(|event| event.name == span).unwrap();
        event.metrics.get(key).cloned()
    };
    let (prompt, completion) = (
        count_tokens("How do I rotate API keys?") as i64,
        count_tokens("Under Settings.") as i64,
    );
    for span in ["full", "timed"] {
        assert_eq!(metric(span, "prompt_tokens"), Some(data::Value::from(prompt)), "{span}");
        assert_eq!(
            metric(span, "completion_tokens"),
            Some(data::Value::from(completion)),
            "{span}"
        );
        assert_eq!(metric(span, "tokens"), Some(data::Value::from(prompt + completion)), "{span}");
    }
    // what a step adds sits beside the counts, and what it replaces feeds the total
    assert_eq!(metric("timed", "time_to_first_token"), Some(data::Value::from(0.25)));
    assert_eq!(metric("full", "time_to_first_token"), None);
    assert_eq!(metric("cached", "prompt_tokens"), Some(data::Value::from(0)));
    assert_eq!(metric("cached", "tokens"), Some(data::Value::from(completion)));
    // a step with no input or output has nothing to count
    for key in ["prompt_tokens", "completion_tokens", "tokens"] {
        assert_eq!(metric("bare", key), Some(data::Value::from(0)), "{key}");
    }
    // a task counts nothing unless it asks
    assert_eq!(metric("request", "tokens"), None);
}

#[test]
fn a_provider_checks_the_arguments_its_descriptor_cannot_declare() {
    for (call, message) in [
        (
            r#"attachment("", "text/plain")"#,
            "function `attachment` path must be a constant file path",
        ),
        (
            r#"attachment("..", "text/plain")"#,
            "function `attachment` path must be a constant file path",
        ),
        (
            r#"attachment("a.txt", "")"#,
            "function `attachment` content_type must be a non-empty constant MIME type",
        ),
    ] {
        let source = format!("(v:vars) {{value: {call}}};");
        let errors = braintrust::Bundle.compile_module(&source, false).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!((errors[0].what.as_str(), located(&source, &errors[0])), (message, call));
    }
    // what the descriptor declares is checked by the language, at the argument
    let source = "(v:vars) {value: noise(8388609)};";
    let errors = braintrust::Bundle.compile_module(source, false).unwrap_err();
    assert!(
        errors[0]
            .what
            .starts_with("function `noise` size must be an integer between 0 and 8388608")
    );
    assert_eq!(located(source, &errors[0]), "8388609");
}

#[test]
fn the_skill_lists_the_functions_a_provider_contributes() {
    let skill = bts_core::skill::render(&bts_lang::spec::SPEC, &braintrust::Bundle);
    let heading = skill.find("## Braintrust mocking functions").unwrap();
    assert!(heading > skill.find("## Global rules").unwrap());
    for function in braintrust::Bundle.schema().functions {
        let entry = skill[heading..].find(&format!("### `{}`", function.doc.name)).unwrap();
        assert!(skill[heading + entry..].contains(function.doc.summary));
        assert!(skill[heading + entry..].contains(function.doc.syntax));
    }
}

#[test]
fn the_examples_of_a_provider_function_compile_against_the_provider() {
    let settings = r#"(settings:vars) {prompt: "hello"};"#;
    for function in braintrust::Bundle.schema().functions {
        assert!(!function.doc.examples.is_empty(), "{}", function.doc.name);
        for example in function.doc.examples {
            let text = format!("{settings} (example:vars) {{value: {example}}};");
            let source = bts_lang::semantics::Source {
                name: "example.bt",
                text: &text,
            };
            let schema = braintrust::Bundle.schema();
            let model = bts_lang::semantics::compile_module_with_provider(source, &schema, false);
            let model = model.unwrap_or_else(|errors| panic!("{example}: {errors:?}"));
            assert_eq!(braintrust::Bundle.unsupported(&model), [], "{example}");
        }
    }
}
