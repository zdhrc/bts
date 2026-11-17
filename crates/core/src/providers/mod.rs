//! What a provider is, and the workflows a provider can support.
//!
//! [`Contributor`] is what every provider does: it declares its kinds, its functions
//! and what it gives meaning to in a [`bts_lang::ProviderSchema`], supplies what runs
//! each function, and supplies authoring guidance. [`Provider`] is a contributor that
//! also says which elements of a model it walks and lowers a model for its consumers.
//! [`With`] puts a provider and a contributor together, so a shape can use the kinds
//! and functions of both. A contributor may walk elements of its own, and what it
//! makes of them reaches the provider as [`Supplied`] values. [`conformance::check`]
//! verifies that an implementer is consistent.
//!
//! [`DataProvider`] and [`CodeProvider`] are the delivery side. Providers implement
//! only the workflows they support. Applications supply configuration and choose
//! their providers in one place, and reach them through these traits everywhere else.

pub mod braintrust;
pub mod conformance;
pub mod world;

use crate::mocking::{Function, code, data};
use bts_lang::semantics::Source;
use bts_lang::{Diag, DiagPhase, Diags, ElementId, Model, NodeId, ProviderSchema, Value};
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

/// What a contributor makes of the elements it walks, for the provider that lowers the model.
///
/// The values are written in the language's own model, so the provider lowers them
/// the way it lowers anything an author wrote.
#[derive(Clone, Debug, Default)]
pub struct Supplied {
    /// The elements the contributor walks. The provider reads their fields and leaves the rest alone.
    pub elements: Vec<ElementId>,
    /// The value of a context key on a node the contributor walks.
    pub context: Vec<Supply>,
}

/// The value a contributor gives one context key on one node.
#[derive(Clone, Debug)]
pub struct Supply {
    /// The node that reads the key.
    pub node: NodeId,
    /// The key, read as `context.key`.
    pub key: &'static str,
    /// What the key holds.
    pub value: Value,
}

impl Supplied {
    /// Whether a contributor walks a node.
    pub fn walks(&self, node: NodeId) -> bool {
        self.elements.contains(&ElementId::Node(node))
    }

    /// The value supplied for a context key on a node.
    pub fn value(&self, node: NodeId, key: &str) -> Option<&Value> {
        (self.context.iter())
            .find(|supply| supply.node == node && supply.key == key)
            .map(|supply| &supply.value)
    }

    fn extend(&mut self, other: Supplied) {
        self.elements.extend(other.elements);
        self.context.extend(other.context);
    }
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

    /// The names of implementations the schema does not declare.
    ///
    /// Authors cannot call these. They appear only in values the contributor
    /// supplies, so each still runs everywhere a declared function would.
    fn internal(&self) -> &'static [&'static str] {
        &[]
    }

    /// Authoring guidance for the skill.
    fn guidance(&self) -> Guidance;

    /// The elements of a model the contributor walks.
    ///
    /// Only these are checked against its schema. Other elements may belong to
    /// another contributor and are left alone. A contributor that only adds
    /// definitions and functions walks nothing.
    fn elements(&self, _model: &Model) -> Vec<ElementId> {
        Vec::new()
    }

    /// What the contributor makes of the elements it walks.
    ///
    /// A failure is located at the graph element it concerns.
    fn supply(&self, _model: &Model) -> Result<Supplied, Diag> {
        Ok(Supplied::default())
    }

    /// Reports every walked element the contributor gives no meaning to.
    fn walked(&self, model: &Model) -> Diags {
        model.unsupported(&self.schema(), &self.elements(model))
    }

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

    /// Lowers a compiled model for the provider's consumers.
    ///
    /// The functions are every implementation in use, the provider's own and those
    /// of the contributors it is joined with, so a call in the model finds what runs
    /// it. A failure is located at the graph element it concerns when one is
    /// responsible.
    fn lower(&self, model: &Model, functions: &'static [Function]) -> Result<Self::Lowered, Diag>;

    /// Lowers a compiled model that reads what contributors supply.
    ///
    /// A provider that reads nothing from a contributor keeps this as it is. Such a
    /// provider rejects a read of an element it does not walk.
    fn lower_with(&self, model: &Model, functions: &'static [Function], _supplied: &Supplied) -> Result<Self::Lowered, Diag> {
        self.lower(model, functions)
    }

    /// Reports every walked element the provider gives no meaning to, and every
    /// function call the provider cannot accept, each at its own location.
    ///
    /// A call needs an implementation, and its arguments must pass the check the
    /// implementation supplies. Whether a consumer can run the function is that
    /// consumer's check, since only it knows what it generates. It asks
    /// [`Function::runs`] and reports the call at the same location.
    fn unsupported(&self, model: &Model) -> Diags {
        let mut diags = self.walked(model);
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
/// its definitions, functions and guidance, and supplies values for the elements it
/// walks. Each checks its own elements against its own schema, and no element
/// belongs to both. Definitions compile in order, the provider's first. A function name has one definition among the two, which
/// [`conformance::check`] verifies. Joining another contributor to the result adds a
/// third, and so on.
#[derive(Clone, Copy, Debug, Default)]
pub struct With<P, C>(pub P, pub C);

// the joined schema and functions of each pairing, built once
struct Joined {
    schema: ProviderSchema<'static>,
    functions: &'static [Function],
    internal: &'static [&'static str],
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
            internal: both(self.0.internal(), self.1.internal()),
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

    fn internal(&self) -> &'static [&'static str] {
        self.joined().internal
    }

    fn guidance(&self) -> Guidance {
        self.0.guidance()
    }

    fn parts(&self) -> Vec<Part> {
        let mut parts = self.0.parts();
        parts.extend(self.1.parts());
        parts
    }

    fn elements(&self, model: &Model) -> Vec<ElementId> {
        let mut elements = self.0.elements(model);
        elements.extend(self.1.elements(model));
        elements
    }

    fn supply(&self, model: &Model) -> Result<Supplied, Diag> {
        let mut supplied = self.0.supply(model)?;
        supplied.extend(self.1.supply(model)?);
        Ok(supplied)
    }

    /// Each side answers for its own elements, and an element both walk is reported once.
    fn walked(&self, model: &Model) -> Diags {
        let mut diags = self.0.walked(model);
        diags.extend(self.1.walked(model));
        let added = self.1.elements(model);
        for element in self.0.elements(model) {
            if !added.contains(&element) {
                continue;
            }
            let range = match element {
                ElementId::Node(id) => model.nodes[id.0].range,
                ElementId::Edge(id) => model.edges[id.0].range,
                ElementId::Fork(id) => model.forks[id.0].range,
                ElementId::Junction(id) => model.junctions[id.0].range,
            };
            diags.push(Diag {
                when: DiagPhase::Validation,
                what: format!(
                    "this belongs to {} and to {}, and an element belongs to one of them",
                    self.0.schema().name,
                    self.1.schema().name
                ),
                r#where: range,
            });
        }
        diags.sort_by_key(|diag| (diag.r#where.source, diag.r#where.start));
        diags
    }
}

impl<P: Provider + 'static, C: Contributor + 'static> Provider for With<P, C> {
    type Lowered = P::Lowered;

    fn lower(&self, model: &Model, functions: &'static [Function]) -> Result<Self::Lowered, Diag> {
        self.lower_with(model, functions, &Supplied::default())
    }

    fn lower_with(&self, model: &Model, functions: &'static [Function], supplied: &Supplied) -> Result<Self::Lowered, Diag> {
        let mut supplied = supplied.clone();
        supplied.extend(self.1.supply(model)?);
        self.0.lower_with(model, functions, &supplied)
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
