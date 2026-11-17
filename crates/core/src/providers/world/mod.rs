//! The world provider: what a shape's agents talk about and who they talk to.
//!
//! It contributes English. A [`defs`] kind writes a sentence as its grammar or an
//! intent as the ways to say it, and the [`functions`] do the same work inside an
//! expression.
//!
//! It also builds things by walking a graph. A [`walk`] stands beside an agent, and
//! the steps it leads to say how one instance of it is put together. The provider
//! turns each walk into a value and supplies it, so it joins a provider that lowers
//! a model through [`With`](super::With) and has no lowered form of its own.

pub mod defs;
pub mod english;
pub mod functions;
pub(crate) mod runtime;
pub mod skill;
pub mod walk;

/// The world provider: its definitions, its functions and its guidance.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bundle;

impl super::Bundle for Bundle {
    const ID: &'static str = "world";
}

/// What the world provider declares.
pub const SCHEMA: bts_lang::ProviderSchema<'static> = bts_lang::ProviderSchema {
    id: "world",
    name: "World",
    definitions: &[defs::SOURCE],
    // a walk, and the steps it leads to: a part, or a holder of values such as a statement
    nodes: &["walk", "part", "vars"],
    edges: &["next"],
    // a walk takes one branch at a fork
    forks: &["weighted", "exclusive"],
    junctions: &[],
    context: &[walk::STEPS],
    functions: functions::DESCRIPTORS,
};

impl super::Contributor for Bundle {
    fn schema(&self) -> bts_lang::ProviderSchema<'static> {
        SCHEMA
    }

    fn functions(&self) -> &'static [crate::mocking::Function] {
        functions::FUNCTIONS
    }

    fn internal(&self) -> &'static [&'static str] {
        &[walk::PICK]
    }

    fn elements(&self, model: &bts_lang::Model) -> Vec<bts_lang::ElementId> {
        walk::elements(model)
    }

    /// The value of `steps` on every walk.
    fn supply(&self, model: &bts_lang::Model) -> Result<super::Supplied, bts_lang::Diag> {
        walk::supply(model)
    }

    fn guidance(&self) -> super::Guidance {
        super::Guidance {
            workflow: "",
            presentation: "",
            definitions: skill::DEFINITIONS,
        }
    }
}
