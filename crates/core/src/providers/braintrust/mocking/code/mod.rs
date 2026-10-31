mod manifests;
mod registrations;
mod target;

pub use manifests::ManifestError;
pub use target::{CodeTarget, Error, resolve_slugs};
