use super::Distribution;
use crate::dsl::Model;
use crate::selection::filter::WriteFilter;
use std::{
    fmt,
    num::NonZeroUsize,
    time::{Duration, SystemTime},
};

/// Inputs for generating traces and resolving their attachments.
pub struct Request<'a> {
    pub model: Model,
    pub count: usize,
    pub over: Duration,
    pub distribution: Distribution,
    pub until: SystemTime,
    pub seed: u64,
    pub filter: &'a WriteFilter,
    pub source: &'a std::path::Path,
}

pub enum Window {
    Relative { over: Duration, offset: Duration },
    Absolute { start: SystemTime, end: SystemTime },
}

pub enum Volume {
    Count(NonZeroUsize),
    Rate(f64),
}

#[derive(Debug)]
pub enum Error {
    EmptyWindow,
    OffsetOutOfRange,
    NoTracesAtRate,
    InvalidRate,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyWindow => "window end must be after its start",
            Self::OffsetOutOfRange => "offset reaches further back than timestamps can represent",
            Self::NoTracesAtRate => "rate over this window rounds to zero traces",
            Self::InvalidRate => "rate must be finite, positive, and produce a representable trace count",
        })
    }
}
impl std::error::Error for Error {}

pub fn resolve(window: Window, volume: Volume, now: SystemTime) -> Result<(usize, Duration, SystemTime), Error> {
    let (over, until) = match window {
        Window::Relative { over, offset } => (over, now.checked_sub(offset).ok_or(Error::OffsetOutOfRange)?),
        Window::Absolute { start, end } => (end.duration_since(start).map_err(|_| Error::EmptyWindow)?, end),
    };
    if over.is_zero() {
        return Err(Error::EmptyWindow);
    }
    let count = match volume {
        Volume::Count(count) => count.get(),
        Volume::Rate(rate) => {
            let count = (rate * over.as_secs_f64()).round();
            if !rate.is_finite() || rate <= 0.0 || !count.is_finite() || count >= usize::MAX as f64 {
                return Err(Error::InvalidRate);
            }
            if count < 1.0 {
                return Err(Error::NoTracesAtRate);
            }
            count as usize
        }
    };
    Ok((count, over, until))
}
