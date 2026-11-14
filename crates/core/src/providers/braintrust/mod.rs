#[cfg(feature = "native")]
pub mod client;
#[cfg(feature = "native")]
pub mod config;
pub mod defs;
#[cfg(feature = "native")]
pub mod features;
pub mod mocking;
pub(crate) mod naming;
pub mod skill;
#[cfg(feature = "native")]
pub mod state;

use mocking::code;
#[cfg(feature = "native")]
use mocking::data;

/// Supplies Braintrust implementations for core mocking workflows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bundle;

impl Bundle {
    /// Supplies agent and span schemas for hosts to include explicitly during compilation.
    pub const DEFINITIONS: bts_lang::semantics::Source<'static> = defs::SOURCE;

    /// Compiles source with this provider's definitions and lowers its mocking behavior.
    pub fn compile(source: &str) -> Result<crate::mocking::Model, String> {
        let graph = bts_lang::semantics::compile_with_definitions(
            bts_lang::semantics::Source {
                name: "input.bt",
                text: source,
            },
            &[Self::DEFINITIONS],
        )
        .map_err(|errors| format!("{errors:?}"))?;
        Self::lower(&graph)
    }

    /// Lowers an agent graph for the shared mock data and mock code consumers.
    pub fn lower(model: &bts_lang::Model) -> Result<crate::mocking::Model, String> {
        mocking::lower(model)
    }
}

impl super::Bundle for Bundle {
    const ID: &'static str = "braintrust";
}

#[cfg(feature = "native")]
impl super::DataProvider for Bundle {
    type Config = config::Config;
    type Client = client::Client;
    type ClientError = client::Error;
    type Format = data::Format;
    type Receipt = client::writer::InsertResponse;
    type SendError = client::writer::Error;
    type Sink<'a> = data::TraceSink<'a>;

    fn format(&self) -> data::Format {
        data::Format
    }

    fn client(&self, config: &config::Config) -> Result<client::Client, client::Error> {
        client::Client::new(config)
    }

    fn sink<'a>(&'a self, client: &'a client::Client) -> data::TraceSink<'a> {
        data::TraceSink::new(client)
    }
}

impl super::CodeProvider for Bundle {
    type CodeConfig = String;
    type Emitter = crate::mocking::code::languages::Languages;
    type Target = code::CodeTarget;

    fn emitter(&self) -> Self::Emitter {
        crate::mocking::code::languages::Languages
    }

    fn code(&self, project: &String) -> code::CodeTarget {
        code::CodeTarget::new(project.clone())
    }
}

use crate::dsl::{RefId, Value};

/// A named topic extraction definition.
#[derive(Debug, Clone)]
pub struct Facet {
    /// The name written in source.
    pub name: String,
    /// The instructions used by the model-based operation.
    pub prompt: String,
    /// Optional descriptive text supplied in source.
    pub description: Option<String>,
    /// An optional pattern identifying responses that contain no matching topic.
    pub no_match_pattern: Option<String>,
}

/// A named rule describing when a scorer or topic extraction should run.
#[derive(Debug, Clone)]
pub struct Automation {
    /// The name written in source.
    pub name: String,
    /// The kind of operation represented by this definition.
    pub kind: AutomationKind,
    /// The fraction of eligible events selected for the automation.
    pub sampling_rate: f64,
    /// Whether the automation is enabled.
    pub enabled: bool,
}

/// The operation requested by an automation definition.
#[derive(Debug, Clone)]
pub enum AutomationKind {
    /// A scorer definition or authored score.
    Scorer {
        /// Names of the scorers bound to this automation.
        scorers: Vec<String>,
        /// Whether the rule targets root spans.
        root: bool,
        /// Names of spans targeted by the rule.
        span_names: Vec<String>,
    },
    /// Topic extraction using named facets.
    Topics {
        /// Names of the facets used by this automation.
        facets: Vec<String>,
    },
}

/// A named collection of cases and their source definitions.
#[derive(Debug, Clone)]
pub struct Dataset {
    /// The name written in source.
    pub name: String,
    /// Optional descriptive text supplied in source.
    pub description: Option<String>,
    /// Dataset cases in declaration order.
    pub cases: Vec<DatasetCase>,
}

/// A dataset evaluation definition linking a task, scorers, and optional baseline.
#[derive(Debug, Clone)]
pub struct Experiment {
    /// The name written in source.
    pub name: String,
    /// Optional descriptive text supplied in source.
    pub description: Option<String>,
    /// The name of the referenced dataset.
    pub dataset: String,
    /// A reference to the trace or static span used as the experiment task.
    pub task: RefId,
    /// Names of the scorers to invoke for each result.
    pub scorers: Vec<String>,
    /// The name of a baseline experiment, if provided.
    pub baseline: Option<String>,
}

/// One named input case, optionally derived from modeled traces or spans.
#[derive(Debug, Clone)]
pub struct DatasetCase {
    /// The name written in source.
    pub name: String,
    /// The source used to create the case.
    pub source: DatasetSource,
    /// The expected result expression, if provided.
    pub expected: Option<Value>,
    /// Additional named values attached to the definition.
    pub metadata: Option<Value>,
    /// Labels attached to the definition.
    pub tags: Option<Value>,
}

/// The authored source from which a consumer creates a dataset case.
#[derive(Debug, Clone)]
pub enum DatasetSource {
    /// Input supplied directly by the case.
    Inline(Value),
    /// A reference to an entire trace.
    Trace(RefId),
    /// A reference to one static span subtree.
    Span(RefId),
    /// An ordered group of referenced traces.
    Group(Vec<RefId>),
}
