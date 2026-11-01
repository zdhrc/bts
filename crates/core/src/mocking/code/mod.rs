//! Assemble scorer and agent source, plan a target's codebase, and write the resulting files.
//!
//! [`assemble`] combines a language [`Emitter`] with an integration [`Target`].
//! [`assemble_agents`] lowers trace entrypoints with an [`AgentTarget`] supplying instrumentation.
//! [`plan_codebase`] lets the target merge scorer source with application-supplied
//! existing files. The native `writing` module performs local filesystem writes.
//! Planning and assembly return inspectable results without invoking the writer.

mod assembly;
mod codebase;
mod contracts;
pub mod languages;
pub(crate) mod lowering;
pub(crate) mod planning;
#[cfg(feature = "native")]
pub mod writing;

pub use assembly::{AssemblyError, Error, assemble, assemble_agents, resolve_slugs};
pub use codebase::{CodebaseContext, CodebaseFile, CodebasePlan, CodebaseRemoval, ExistingFiles, plan_codebase};
pub use contracts::{AgentTarget, Assembly, CodeGroup, Emitter, Item, Judge, Module, SourceFile, Target, arg_name};
pub use languages::Error as EmitError;
