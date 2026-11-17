//! The world provider: what a shape's agents talk about and who they talk to.
//!
//! It contributes English. A [`defs`] kind writes a sentence as its grammar or an
//! intent as the ways to say it, and the [`functions`] do the same work inside an
//! expression. It lowers nothing, so it joins a provider that does through
//! [`With`](super::With).

pub mod defs;
pub mod english;
pub mod functions;
pub(crate) mod runtime;
pub mod skill;

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
    // its kinds carry values, so they derive from a kind the lowering provider already walks
    nodes: &[],
    edges: &[],
    forks: &[],
    junctions: &[],
    context: &[],
    functions: functions::DESCRIPTORS,
};

impl super::Contributor for Bundle {
    fn schema(&self) -> bts_lang::ProviderSchema<'static> {
        SCHEMA
    }

    fn functions(&self) -> &'static [crate::mocking::Function] {
        functions::FUNCTIONS
    }

    fn guidance(&self) -> super::Guidance {
        super::Guidance {
            workflow: "",
            presentation: "",
            definitions: skill::DEFINITIONS,
        }
    }
}
