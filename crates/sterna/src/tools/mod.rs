//! The tools a sterna program can call — map lines 2455, 2463, and the
//! foundation of 2461. Specification: `docs/runtime.md`
//! §6 and §7, which leave the registry's own schema to this sub-phase.
//!
//! Composition only. [`registry`] declares what exists; [`invoke`] runs one,
//! under the sandbox, and fires the hooks around it.

pub mod exact_edit;
pub mod invoke;
pub mod mcp;
pub mod registry;

/// The cancellation facility is a property of the tools surface rather than
/// of one call site, so it is named here as well as where it is defined.
pub use invoke::CancellationToken;
