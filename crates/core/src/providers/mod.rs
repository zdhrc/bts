//! What a provider is, and the workflows a provider can support.
//!
//! [`Contributor`] is what every provider does: it declares its kinds, its functions
//! and what it gives meaning to in a [`bts_lang::ProviderSchema`], supplies what runs
//! each function, and supplies authoring guidance. [`Provider`] is a contributor that
//! also says which elements of a model it walks and lowers a model for its consumers.
//! [`With`] puts a provider and a contributor together, so a shape can use the kinds
//! and functions of both. [`conformance::check`] verifies that an implementer is
//! consistent.
//!
//! [`DataProvider`] and [`CodeProvider`] are the delivery side. Providers implement
//! only the workflows they support. Applications supply configuration and choose
//! their providers in one place, and reach them through these traits everywhere else.

pub mod braintrust;
pub mod conformance;
pub mod world;

use crate::mocking::{Function, code, data};
use bts_lang::semantics::Source;
use bts_lang::{Diag, DiagPhase, Diags, ElementId, Model, ProviderSchema};
use std::any::TypeId;
use std::sync::Mutex;

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

/// One contributor's share of what a host compiles and documents.
#[derive(Clone, Copy, Debug)]
pub struct Part {
    /// What the contributor declares.
    pub schema: ProviderSchema<'static>,
    /// What the contributor tells authors.
    pub guidance: Guidance,
}

/// Adds kinds, traits, context keys and functions to the language.
///
/// The schema is the declarative half and lives in the language crate. This trait
/// is the half that runs. A contributor that lowers nothing, such as one that only
/// supplies functions and the kinds built on them, implements this alone and joins
/// a [`Provider`] through [`With`].
pub trait Contributor: Bundle {
    /// The contributor's identity, definitions, functions and supported kinds and context keys.
    ///
    /// Its `id` equals [`Bundle::ID`].
    fn schema(&self) -> ProviderSchema<'static>;

    /// What runs each function the schema declares, one entry for each by name.
    fn functions(&self) -> &'static [Function] {
        &[]
    }

    /// Authoring guidance for the skill.
    fn guidance(&self) -> Guidance;

    /// The contributors this one is made of, in the order their definitions compile.
    fn parts(&self) -> Vec<Part> {
        vec![Part {
            schema: self.schema(),
            guidance: self.guidance(),
        }]
    }
}

/// Gives the language's kinds a meaning for consumers: the contract of a provider that lowers a model.
///
/// A host compiles source through [`Provider::compile_module`] and hands the lowered
/// model to the provider's consumers.
pub trait Provider: Contributor {
    /// What the provider's consumers run.
    type Lowered;

    /// The elements of a model the provider walks.
    ///
    /// Only these are checked against the schema. Other elements may belong to
    /// another provider and are left alone.
    fn elements(&self, model: &Model) -> Vec<ElementId>;

    /// Lowers a compiled model for the provider's consumers.
    ///
    /// The functions are every implementation in use, the provider's own and those
    /// of the contributors it is joined with, so a call in the model finds what runs
    /// it. A failure is located at the graph element it concerns when one is
    /// responsible.
    fn lower(&self, model: &Model, functions: &'static [Function]) -> Result<Self::Lowered, Diag>;

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
        self.lower(&model, self.functions()).map_err(|diag| vec![diag])
    }
}

/// A provider joined by a contributor.
///
/// The provider lowers the model and delivers what it produces. The contributor adds
/// its definitions, functions and guidance. Definitions compile in order, the
/// provider's first. A function name has one definition among the two, which
/// [`conformance::check`] verifies. Joining another contributor to the result adds a
/// third, and so on.
#[derive(Clone, Copy, Debug, Default)]
pub struct With<P, C>(pub P, pub C);

// the joined schema and functions of each pairing, built once
struct Joined {
    schema: ProviderSchema<'static>,
    functions: &'static [Function],
}

impl<P: Provider + 'static, C: Contributor + 'static> With<P, C> {
    fn joined(&self) -> &'static Joined {
        static JOINED: Mutex<Vec<(TypeId, &'static Joined)>> = Mutex::new(Vec::new());
        let id = TypeId::of::<Self>();
        let mut joined = JOINED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, known)) = joined.iter().find(|(known, _)| *known == id) {
            return known;
        }
        fn both<T: Clone>(first: &[T], second: &[T]) -> &'static [T] {
            Box::leak([first, second].concat().into_boxed_slice())
        }
        let (base, added) = (self.0.schema(), self.1.schema());
        let built: &'static Joined = Box::leak(Box::new(Joined {
            schema: ProviderSchema {
                definitions: both(base.definitions, added.definitions),
                nodes: both(base.nodes, added.nodes),
                edges: both(base.edges, added.edges),
                forks: both(base.forks, added.forks),
                junctions: both(base.junctions, added.junctions),
                context: both(base.context, added.context),
                functions: both(base.functions, added.functions),
                ..base
            },
            functions: both(self.0.functions(), self.1.functions()),
        }));
        joined.push((id, built));
        built
    }
}

impl<P: Bundle, C> Bundle for With<P, C> {
    const ID: &'static str = P::ID;
}

impl<P: Provider + 'static, C: Contributor + 'static> Contributor for With<P, C> {
    /// The provider's identity with the definitions and functions of both.
    fn schema(&self) -> ProviderSchema<'static> {
        self.joined().schema
    }

    fn functions(&self) -> &'static [Function] {
        self.joined().functions
    }

    fn guidance(&self) -> Guidance {
        self.0.guidance()
    }

    fn parts(&self) -> Vec<Part> {
        let mut parts = self.0.parts();
        parts.extend(self.1.parts());
        parts
    }
}

impl<P: Provider + 'static, C: Contributor + 'static> Provider for With<P, C> {
    type Lowered = P::Lowered;

    fn elements(&self, model: &Model) -> Vec<ElementId> {
        self.0.elements(model)
    }

    fn lower(&self, model: &Model, functions: &'static [Function]) -> Result<Self::Lowered, Diag> {
        self.0.lower(model, functions)
    }
}

impl<P: DataProvider, C> DataProvider for With<P, C> {
    type Config = P::Config;
    type Client = P::Client;
    type ClientError = P::ClientError;
    type Format = P::Format;
    type Receipt = P::Receipt;
    type SendError = P::SendError;
    type Sink<'a>
        = P::Sink<'a>
    where
        Self: 'a,
        Self::Client: 'a;

    fn format(&self) -> Self::Format {
        self.0.format()
    }

    fn client(&self, config: &Self::Config) -> Result<Self::Client, Self::ClientError> {
        self.0.client(config)
    }

    fn sink<'a>(&'a self, client: &'a Self::Client) -> Self::Sink<'a> {
        self.0.sink(client)
    }
}

impl<P: CodeProvider, C> CodeProvider for With<P, C> {
    type CodeConfig = P::CodeConfig;
    type Emitter = P::Emitter;
    type Target = P::Target;

    fn emitter(&self) -> Self::Emitter {
        self.0.emitter()
    }

    fn code(&self, config: &Self::CodeConfig) -> Self::Target {
        self.0.code(config)
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
