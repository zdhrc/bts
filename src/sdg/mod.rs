mod materializer;
mod planner;
pub(crate) mod writer;

use crate::{conf::Braintrust, dsl::Model};
use std::fmt;
use std::time::{Duration, SystemTime};

pub(crate) use materializer::{Distribution, EventBatch};
pub(crate) use writer::{InsertResponse, PackStats};

pub(crate) fn generate(
    model: Model,
    count: usize,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
    seed: u64,
) -> Result<EventBatch, Error> {
    if model.traces.is_empty() {
        return Err(Error::EmptyShape);
    }

    let plan = tracing::info_span!("plan")
        .in_scope(|| planner::plan(model, count, seed))
        .map_err(Error::Plan)?;
    tracing::info_span!("materialize")
        .in_scope(|| materializer::materialize(plan, over, distribution, now))
        .map_err(Error::Materialize)
}

pub(crate) fn write(config: &Braintrust, events: &EventBatch) -> Result<InsertResponse, writer::Error> {
    writer::write(config, events)
}

pub(crate) fn pack_stats(events: &EventBatch) -> Result<PackStats, writer::Error> {
    writer::pack_stats(events)
}

#[derive(Debug)]
pub(crate) enum Error {
    EmptyShape,
    Plan(planner::Error),
    Materialize(materializer::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyShape => formatter.write_str("shape must contain at least one trace"),
            Self::Plan(source) => write!(formatter, "failed to evaluate an expression: {source}"),
            Self::Materialize(source) => write!(formatter, "failed to materialize traces: {source}"),
        }
    }
}

impl std::error::Error for Error {}
