use super::{Attachment, EventBatch};
use serde_json::Value as JsonValue;
use std::error::Error;

/// Supplies the attachment representation used by value operations.
pub trait AttachmentFormat {
    /// Projects attachment metadata without reading files or making requests.
    /// Any JSON value can be projected for serialization and token counting.
    /// Where source permits runtime selection, objects support string keys and
    /// arrays support integer indexes and slices. Incompatible selections return
    /// an evaluation error. Scalar projections cannot be indexed, sliced, or spread.
    /// Returns `None` when this format does not support attachments.
    fn attachment(&self, _attachment: &Attachment) -> Option<JsonValue> {
        None
    }
}

impl AttachmentFormat for () {}

impl<T: AttachmentFormat + ?Sized> AttachmentFormat for &T {
    fn attachment(&self, attachment: &Attachment) -> Option<JsonValue> {
        (**self).attachment(attachment)
    }
}

/// Converts generated events into a destination's representation without sending them.
pub trait Format: AttachmentFormat {
    type Output;
    type Error: Error + 'static;

    fn encode(&self, events: &EventBatch) -> Result<Self::Output, Self::Error>;
}

/// Delivers an encoded batch and reports the acknowledged result.
pub trait Sink {
    type Input;
    type Receipt;
    type Error: Error + 'static;

    fn send(&self, input: &Self::Input) -> Result<Self::Receipt, Self::Error>;
}
