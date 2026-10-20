pub(crate) mod assembly;
pub(crate) mod emission;
mod lowering;
mod planning;
#[cfg(test)]
mod verification;

pub(crate) use assembly::{Error, assemble};
pub(crate) use emission::{build, format};
pub(crate) use planning::{resolve_slugs, slugify};
