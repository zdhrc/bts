//! What a provider is, and the workflows a provider can support.
//!
//! [`Provider`] is the contract. A provider declares its kinds, its functions and
//! what it gives meaning to in a [`bts_lang::ProviderSchema`], says which elements
//! of a model it walks, supplies what runs each function, lowers a model for its
//! consumers, and supplies authoring guidance. [`conformance::check`] verifies that
//! an implementer is consistent.
//!
//! [`DataProvider`] and [`CodeProvider`] are the delivery side. Providers implement
//! only the workflows they support. Applications supply configuration and choose a
//! provider in one place, and reach it through these traits everywhere else.

pub mod braintrust;
pub mod conformance;

use crate::mocking::{Function, code, data};
use bts_lang::semantics::Source;
use bts_lang::{Diag, DiagPhase, Diags, ElementId, Model, ProviderSchema};

/// Identifies a provider whose supported workflows are declared through capability traits.
pub trait Bundle {
    const ID: &'static str;
}

/// Authoring guidance a provider contributes to a generated skill.
///
/// Each section is Markdown that starts at a second-level heading. A section a
/// provider has nothing to say in is empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Guidance {
    /// How to author source for this provider, from the request to a finished shape.
    pub workflow: &'static str,
    /// How the provider presents what a shape produces.
    pub presentation: &'static str,
    /// Notes on the provider's kinds, shown above its definitions.
    pub definitions: &'static str,
}

impl Guidance {
    /// The sections in the order a skill shows them.
    pub fn sections(&self) -> [&'static str; 3] {
        [self.workflow, self.presentation, self.definitions]
    }
}

/// Gives the language's kinds a meaning: the one contract every provider implements.
///
/// The schema is the declarative half and lives in the language crate. This trait
/// is the half that runs. A host compiles source through [`Provider::compile_module`]
/// and hands the lowered model to the provider's consumers.
pub trait Provider: Bundle {
    /// What the provider's consumers run.
    type Lowered;

    /// The provider's identity, definitions, functions and supported kinds and context keys.
    ///
    /// Its `id` equals [`Bundle::ID`].
    fn schema(&self) -> ProviderSchema<'static>;

    /// What runs each function the schema declares, one entry for each by name.
    fn functions(&self) -> &'static [Function] {
        &[]
    }

    /// The elements of a model the provider walks.
    ///
    /// Only these are checked against the schema. Other elements may belong to
    /// another provider and are left alone.
    fn elements(&self, model: &Model) -> Vec<ElementId>;

    /// Lowers a compiled model for the provider's consumers.
    ///
    /// A failure is located at the graph element it concerns when one is responsible.
    fn lower(&self, model: &Model) -> Result<Self::Lowered, Diag>;

    /// Authoring guidance for the skill.
    fn guidance(&self) -> Guidance;

    /// Reports every walked element the provider gives no meaning to, and every
    /// function call the provider cannot accept, each at its own location.
    ///
    /// A call needs an implementation, and its arguments must pass the check the
    /// implementation supplies. Whether a consumer can run the function is that
    /// consumer's check, since only it knows what it generates. It asks
    /// [`Function::runs`] and reports the call at the same location.
    fn unsupported(&self, model: &Model) -> Diags {
        let mut diags = model.unsupported(&self.schema(), &self.elements(model));
        let report = |range, what| Diag {
            when: DiagPhase::Validation,
            what,
            r#where: range,
        };
        for call in model.calls() {
            let name = call.function;
            let Some(function) = self.functions().iter().find(|function| function.name == name) else {
                diags.push(report(call.range, format!("function `{name}` has no implementation")));
                continue;
            };
            if let Some(Err(what)) = function.check.map(|check| check(call.args)) {
                diags.push(report(call.range, format!("function `{name}` {what}")));
            }
        }
        diags
    }

    /// Compiles source with the provider's definitions and lowers it.
    fn compile(&self, source: &str) -> Result<Self::Lowered, String> {
        self.compile_module(source, false).map_err(|errors| format!("{errors:?}"))
    }

    /// Compiles source while preserving diagnostics and the empty module policy.
    ///
    /// Everything the provider does not support is reported together.
    fn compile_module(&self, source: &str, allow_empty: bool) -> Result<Self::Lowered, Diags> {
        let model = bts_lang::semantics::compile_module_with_provider(
            Source {
                name: "input.bt",
                text: source,
            },
            &self.schema(),
            allow_empty,
        )?;
        let unsupported = self.unsupported(&model);
        if !unsupported.is_empty() {
            return Err(unsupported);
        }
        self.lower(&model).map_err(|diag| vec![diag])
    }
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
