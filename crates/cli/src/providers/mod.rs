pub mod braintrust;

use crate::{code, config::Settings, data};
use bts_core::providers::{CodeProvider, DataProvider};
use std::{error::Error, path::Path, time::Duration};

// the one place the cli names its provider, commands reach it through the provider traits
pub type Selected = bts_core::providers::braintrust::Bundle;
pub const SELECTED: Selected = bts_core::providers::braintrust::Bundle;

pub type Encoded<P> = <<P as DataProvider>::Format as data::Format>::Output;

// adapters load cli settings and present provider results so core workflows
// don't need to know about flags, environment variables, or terminal output
pub trait DataAdapter {
    type Provider: DataProvider;
    type Error: Error + 'static;

    fn provider(&self) -> Self::Provider;
    fn config(&self, settings: &Settings) -> Result<<Self::Provider as DataProvider>::Config, Self::Error>;
    fn preview(&self, encoded: &Encoded<Self::Provider>) -> Result<String, Self::Error>;
    fn log_destination(&self, client: &<Self::Provider as DataProvider>::Client);
    fn report_traces(
        &self,
        client: &<Self::Provider as DataProvider>::Client,
        receipt: &<Self::Provider as DataProvider>::Receipt,
        run: TraceRun<'_>,
    ) -> Result<(), Self::Error>;
}

pub struct TraceRun<'a> {
    pub seed: u64,
    pub events: &'a data::EventBatch,
    pub duration: Duration,
    pub log: Option<&'a Path>,
    pub json: bool,
}

pub trait CodeAdapter {
    type Provider: CodeProvider;
    type Error: Error + 'static;

    fn provider(&self) -> Self::Provider;
    fn config(&self) -> Result<<Self::Provider as CodeProvider>::CodeConfig, Self::Error>;
    fn report_build(&self, out: &Path, built: &[code::writing::Built]);
}

/// Adds runnable agent generation without requiring scorer-only adapters to support it.
pub trait AgentAdapter: CodeAdapter
where
    <Self::Provider as CodeProvider>::Target: code::AgentTarget,
{
    fn agent_target(
        &self,
        model: &bts_core::mocking::Model,
        seed: u64,
    ) -> Result<<Self::Provider as CodeProvider>::Target, Self::Error>;

    fn assemble_agents(
        &self,
        model: &bts_core::mocking::Model,
        lang: bts_core::mocking::ScorerLang,
        target: &<Self::Provider as CodeProvider>::Target,
    ) -> Result<code::Assembly, code::AgentError> {
        code::assemble_agents(model, lang, target)
    }
}
