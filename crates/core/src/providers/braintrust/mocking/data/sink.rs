use super::Batch;
use crate::mocking::data;
use crate::providers::braintrust::client::{Client, writer};

/// Delivers formatted traces through a configured Braintrust client.
pub struct TraceSink<'a> {
    client: &'a Client,
}

impl<'a> TraceSink<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self { client }
    }
}

impl data::Sink for TraceSink<'_> {
    type Input = Batch;
    type Receipt = writer::InsertResponse;
    type Error = writer::Error;

    fn send(&self, input: &Batch) -> Result<Self::Receipt, Self::Error> {
        writer::write_with_client(self.client, input)
    }
}
