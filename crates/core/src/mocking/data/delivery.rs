use super::{EventBatch, Format, PrepareError, Request, Sink};
use std::{error::Error, fmt};

/// Encodes and delivers a generated batch using compatible implementations.
///
/// Callers must resolve any local attachment paths before delivery. Encoding
/// failures prevent the sink from running. A sink failure may follow partial
/// delivery, as defined by that sink, and retains its concrete error as a source.
pub fn write<F: Format + ?Sized, S: Sink<Input = F::Output> + ?Sized>(
    events: &EventBatch,
    format: &F,
    sink: &S,
) -> Result<S::Receipt, WriteError<F::Error, S::Error>> {
    let output = format.encode(events).map_err(WriteError::Format)?;
    sink.send(&output).map_err(WriteError::Send)
}

/// Generates, encodes, and delivers traces with the supplied capabilities.
///
/// Preparation resolves local attachment paths without reading their contents.
/// Failures stop the next stage from running. A successful result includes the
/// generated batch and the sink's receipt, whose acknowledgment semantics belong
/// to that sink.
pub fn run<F: Format + ?Sized, S: Sink<Input = F::Output> + ?Sized>(
    request: Request<'_>,
    format: &F,
    sink: &S,
) -> RunResult<S::Receipt, F::Error, S::Error> {
    let events = super::prepare(request, &format).map_err(RunError::Prepare)?;
    let receipt = write(&events, format, sink).map_err(RunError::Write)?;
    Ok(Written { events, receipt })
}

/// Generated events together with the destination's delivery receipt.
pub struct Written<R> {
    pub events: EventBatch,
    pub receipt: R,
}

pub type RunResult<R, F, S> = Result<Written<R>, RunError<F, S>>;

#[derive(Debug)]
pub enum WriteError<F, S> {
    Format(F),
    Send(S),
}

impl<F: fmt::Display, S: fmt::Display> fmt::Display for WriteError<F, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(source) => source.fmt(f),
            Self::Send(source) => source.fmt(f),
        }
    }
}

impl<F: Error + 'static, S: Error + 'static> Error for WriteError<F, S> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Format(source) => Some(source),
            Self::Send(source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub enum RunError<F, S> {
    Prepare(PrepareError),
    Write(WriteError<F, S>),
}

impl<F: fmt::Display, S: fmt::Display> fmt::Display for RunError<F, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prepare(source) => source.fmt(f),
            Self::Write(source) => source.fmt(f),
        }
    }
}

impl<F: Error + 'static, S: Error + 'static> Error for RunError<F, S> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Prepare(source) => Some(source),
            Self::Write(source) => Some(source),
        }
    }
}
