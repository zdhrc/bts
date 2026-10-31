//! Generate data and code from compiled bts models and run provider workflows.
//!
//! [`mocking::data`] separates generation, encoding, and delivery.
//! [`mocking::code`] separates source assembly, codebase planning, and file writes.
//! [`providers`] supplies compatible capabilities and provider-specific features.
//! Applications own argument parsing, configuration choices, and presentation.
//!
//! Pass models produced by [`bts_lang::semantics::compile`] or
//! [`bts_lang::semantics::compile_module`]. Model transformations must preserve
//! resolved references and the compiler's type and scope invariants.

pub mod config;
pub mod mocking;
pub mod providers;
pub mod selection;
pub mod skill;

use bts_lang as dsl;
