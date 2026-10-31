//! Generate typed events, then encode and deliver them through supplied capabilities.
//!
//! [`generate`] evaluates a compiled model without file or network access.
//! [`prepare`] also resolves attachment paths relative to the source file, without
//! reading their contents. [`write()`] encodes a prepared batch and sends it, while
//! [`run`] combines preparation and delivery. Errors preserve the failing stage
//! and its concrete cause.

mod contracts;
pub mod delivery;
pub mod events;
pub mod generation;
pub mod request;
pub mod value;

pub use contracts::{AttachmentFormat, Format, Sink};
pub use delivery::{RunError, RunResult, WriteError, Written, run, write};
pub use events::{Event, EventBatch, EventKind, stable_uuid};
pub use generation::{
    Distribution, Error, EvaluationError, EvaluationErrorKind, MaterializationError, MaterializationErrorKind, PrepareError,
    case_data, experiment_result, generate, generate_filtered, prepare,
};
pub use request::Request;
pub use value::{Attachment, UnsupportedAttachment, Value};
