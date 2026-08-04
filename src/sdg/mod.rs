mod materializer;
mod planner;
mod writer;

use crate::{conf::Braintrust, dsl::Model};
use std::time::{Duration, SystemTime};
use thiserror::Error;

pub(crate) use materializer::EventBatch;
pub(crate) use writer::{Error as WriteError, InsertResponse};

pub(crate) fn generate(model: Model, count: usize, over: Duration, now: SystemTime) -> Result<EventBatch, GenerateError> {
    if model.traces.is_empty() {
        return Err(GenerateError::EmptyShape);
    }

    materializer::materialize(planner::plan(model, count), over, now).map_err(GenerateError::Materialize)
}

pub(crate) fn write(config: &Braintrust, events: &EventBatch) -> Result<InsertResponse, writer::Error> {
    writer::write(config, events)
}

#[derive(Debug, Error)]
pub(crate) enum GenerateError {
    #[error("shape must contain at least one trace")]
    EmptyShape,

    #[error("failed to materialize traces: {0}")]
    Materialize(#[source] materializer::Error),
}
