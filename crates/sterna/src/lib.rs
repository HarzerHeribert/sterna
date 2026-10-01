//! `sterna` is a coding agent for your terminal: a standalone process with its
//! own binary. It reaches the inference gateway through a protocol boundary,
//! never a compile-time dependency, and builds and tests independently of the
//! rest of the workspace.

/// The version this build says it is: the release's own, which the release
/// workflow sets as `STERNA_RELEASE` when it builds a tag, or else the
/// crate's. The desktop app compares it to choose the newer of two engines.
pub const VERSION: &str = match option_env!("STERNA_RELEASE") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub mod abi;
pub mod agent;
pub mod approval;
pub mod ask;
pub mod bg;
pub mod changes;
pub mod commands;
pub mod completion;
pub mod config;
pub mod contract;
pub mod decide;
pub mod engine;
pub mod events;
pub mod gateway;
pub mod images;
pub mod manifest;
pub mod memory;
pub mod models;
pub mod observe;
pub mod permissions;
pub mod progress;
pub mod project;
pub mod prompt;
pub mod relocate;
pub mod rollout;
pub mod ruler;
pub mod runtime;
pub mod sandbox;
pub mod session;
pub mod settings;
pub mod settings_commands;
pub(crate) mod settings_session;
pub mod spend;
pub mod telemetry;
pub mod tools;
pub mod tui;
pub mod update;
pub mod web;
pub mod wire;

pub mod verification;

pub mod workbench;
