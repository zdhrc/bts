//! Generate data and code from compiled bts models and run provider workflows.
//!
//! [`mocking::data`] separates generation, encoding, and delivery.
//! [`mocking::code`] separates source assembly, codebase planning, and file writes.
//! [`providers`] supplies compatible capabilities and provider-specific features.
//! Applications own argument parsing, configuration choices, and presentation.
//! The default `native` feature enables local configuration, filesystem writes,
//! and provider clients. Disable it to use generation, encoding, and codebase
//! planning in browser applications.
//!
//! Compile graph sources with provider definitions, then lower them through
//! [`providers::braintrust::Bundle::lower`] before invoking mocking consumers.
//! Model transformations must preserve resolved references and scope invariants.

#[cfg(feature = "native")]
pub mod config;
pub mod english;
pub mod mocking;
pub mod providers;
pub mod selection;
pub mod skill;

use mocking as dsl;
