//! Generate data and code from compiled bts models and run provider workflows.
//!
//! [`mocking::data`] separates generation, encoding, and delivery.
//! [`mocking::code`] separates source assembly, codebase planning, and file writes.
//! [`providers`] defines what a provider is and holds the providers themselves.
//! Applications own argument parsing, configuration choices, and presentation.
//! The default `native` feature enables local configuration, filesystem writes,
//! and provider clients. Disable it to use generation, encoding, and codebase
//! planning in browser applications.
//!
//! A host names its provider in one place and reaches it through
//! [`providers::Provider`]. [`providers::Provider::compile_module`] compiles source
//! with the provider's definitions, rejects what the provider does not support and
//! lowers the result for the mocking consumers. A provider also contributes
//! functions: the schema declares each one and a [`mocking::Function`] runs it, for
//! data generation and for agents generated in each language. A consumer that cannot
//! run a function reports the call where it is written.
//! Model transformations must preserve resolved references and scope invariants.

#[cfg(feature = "native")]
pub mod config;
pub mod english;
pub mod mocking;
pub mod providers;
pub mod selection;
pub mod skill;

use mocking as dsl;
