pub(crate) mod materializer;
pub(crate) mod planner;

use crate::dsl::{Model, WriteFilter};
use std::fmt;
use std::time::{Duration, SystemTime};

pub(crate) use materializer::{Distribution, EventBatch};
pub(crate) use planner::Attachment;

pub(crate) fn generate(
    model: Model,
    count: usize,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
    seed: u64,
) -> Result<EventBatch, Error> {
    generate_filtered(model, count, over, distribution, now, seed, &WriteFilter::default())
}

pub(crate) fn generate_filtered(
    mut model: Model,
    count: usize,
    over: Duration,
    distribution: Distribution,
    now: SystemTime,
    seed: u64,
    filter: &WriteFilter,
) -> Result<EventBatch, Error> {
    if model.traces.is_empty() {
        return Err(Error::EmptyShape);
    }
    model.traces.retain(|trace| filter.matches("trace", Some(&trace.name)));
    if model.traces.is_empty() {
        return Err(Error::NoMatchingTraces);
    }

    let plan = tracing::info_span!("plan")
        .in_scope(|| planner::plan_with_filter(model, count, seed, filter))
        .map_err(Error::Plan)?;
    tracing::info_span!("materialize")
        .in_scope(|| materializer::materialize(plan, over, distribution, now))
        .map_err(Error::Materialize)
}

#[derive(Debug)]
pub(crate) enum Error {
    EmptyShape,
    NoMatchingTraces,
    Plan(planner::Error),
    Materialize(materializer::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyShape => formatter.write_str("shape must contain at least one trace"),
            Self::NoMatchingTraces => formatter.write_str("write filter excludes every trace block"),
            Self::Plan(source) => write!(formatter, "failed to evaluate an expression: {source}"),
            Self::Materialize(source) => write!(formatter, "failed to materialize traces: {source}"),
        }
    }
}

impl std::error::Error for Error {}
