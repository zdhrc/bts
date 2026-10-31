mod format;
#[cfg(feature = "native")]
mod sink;
pub mod traces;

pub use format::{Batch, Error, Event, Format, SpanAttributes};
#[cfg(feature = "native")]
pub use sink::TraceSink;
