//! Provider capabilities for data delivery and code integration.
//!
//! Providers implement only the workflows they support. Applications supply
//! configuration and choose a provider without changing the generic pipelines.

pub mod braintrust;

use crate::mocking::{code, data};

/// Identifies a provider whose supported workflows are declared through capability traits.
pub trait Bundle {
    const ID: &'static str;
}

/// Supplies compatible formatting and delivery components for generated data.
///
/// The sink's input type must match the format's output. Client construction and
/// delivery errors retain their provider-specific types. Constructing a format
/// must not require credentials or contact a service, so previews can stay local.
pub trait DataProvider: Bundle {
    type Config;
    type Client;
    type ClientError: std::error::Error + 'static;
    type Format: data::Format;
    type Receipt;
    type SendError: std::error::Error + 'static;
    type Sink<'a>: data::Sink<Input = <Self::Format as data::Format>::Output, Receipt = Self::Receipt, Error = Self::SendError>
    where
        Self: 'a,
        Self::Client: 'a;

    /// Returns a format without loading configuration or creating a client.
    fn format(&self) -> Self::Format;

    /// Creates a shared client from application-supplied configuration.
    fn client(&self, config: &Self::Config) -> Result<Self::Client, Self::ClientError>;

    /// Binds delivery to the supplied client.
    fn sink<'a>(&'a self, client: &'a Self::Client) -> Self::Sink<'a>;
}

/// Supplies code emission and integration conventions for a generated codebase.
///
/// The target's slugs must satisfy the emitter's requirements. Returned exports
/// correspond to input items in order, and any target registrations must preserve
/// that association.
pub trait CodeProvider: Bundle {
    type CodeConfig;
    type Emitter: code::Emitter;
    type Target: code::Target;

    fn emitter(&self) -> Self::Emitter;

    /// Creates a target from application-supplied code generation settings without connecting to a service.
    fn code(&self, config: &Self::CodeConfig) -> Self::Target;
}
