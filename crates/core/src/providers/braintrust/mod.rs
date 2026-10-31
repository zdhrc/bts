#[cfg(feature = "native")]
pub mod client;
#[cfg(feature = "native")]
pub mod config;
#[cfg(feature = "native")]
pub mod features;
pub mod mocking;
pub(crate) mod naming;
#[cfg(feature = "native")]
pub mod state;

use mocking::code;
#[cfg(feature = "native")]
use mocking::data;

/// Supplies Braintrust implementations for core mocking workflows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bundle;

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
