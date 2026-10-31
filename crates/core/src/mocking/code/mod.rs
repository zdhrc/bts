//! Assemble scorer source, plan a target's codebase, and write the resulting files.
//!
//! [`assemble`] combines a language [`Emitter`] with an integration [`Target`].
//! [`plan_codebase`] lets the target merge that source with application-supplied
//! existing files. The native `writing` module performs local filesystem writes.
//! Planning and assembly return inspectable results without invoking the writer.

mod assembly;
mod codebase;
mod contracts;
pub mod languages;
#[cfg(feature = "native")]
pub mod writing;

pub use assembly::{AssemblyError, Error, assemble, resolve_slugs};
pub use codebase::{CodebaseContext, CodebaseFile, CodebasePlan, ExistingFiles, plan_codebase};
pub use contracts::{Assembly, CodeGroup, Emitter, Item, Judge, Module, SourceFile, Target, arg_name};
pub use languages::Error as EmitError;
