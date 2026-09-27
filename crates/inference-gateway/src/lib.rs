//! The inference gateway: one wire format in, many providers out.
//!
//! This crate is the **bottom** layer of the three-component split. Sterna and
//! Glasshouse both sit on it; neither is required by it, and neither is
//! required by the other. A normal Sterna session runs with no Glasshouse
//! process anywhere.
//!
//! # What lives here, and the one rule that keeps it here
//!
//! Providers, credentials, entitlements, quota, free pools, subscriptions,
//! same-model failover, protocol translation and usage accounting.
//!
//! **Nothing in this crate may name Glasshouse.** Not a type, not a path, not
//! a dependency. The gateway runs as its own process and reports outward; the
//! obvious "fix" for a boundary that a Rust trait cannot cross is to embed the
//! gateway back inside Glasshouse, and that would silently undo the whole
//! separation. `tests::the_gateway_names_no_glasshouse_path` is the scan that
//! makes the rule enforceable rather than remembered.
//!
//! # The boundary the caller relies on
//!
//! A caller sends a **model**, an **effort**, a **request** and a **fallback
//! policy**. This crate may choose the provider, the account and the
//! entitlement. It may **not** change the model or the effort unless the
//! fallback policy explicitly permits it -- `translate::canonical` and the
//! failover path both hold that line, and it is the invariant that makes a
//! gateway safe to put underneath somebody's session.
//!
//! # The standalone process, and the two modules that compose it
//!
//! `src/main.rs` is the `inference-gateway` binary: it serves until stdin
//! reaches EOF, and Sterna spawns it. [`mod@config`] is the file it reads --
//! an account catalogue and the providers those accounts name -- and
//! [`mod@pool`] turns that catalogue into the [`gateway::Upstream`] a
//! started gateway forwards through. Both are composition: they decide
//! which accounts exist and in what order they are preferred, and they add
//! no per-request decision to the ones `routing` already makes.

pub mod config;
pub mod entitlement;
pub mod gateway;
pub mod models;
pub mod pool;
pub mod provider;
pub mod routing;
pub mod secret;
pub mod subscription;
