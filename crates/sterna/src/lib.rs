//! `sterna` is a coding agent for your terminal: a standalone process with its
//! own binary. It reaches the inference gateway through a protocol boundary,
//! never a compile-time dependency, and builds and tests independently of the
//! rest of the workspace.

pub mod abi;
pub mod acceptance;
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
pub mod events;
pub mod excerpts;
pub mod gateway;
pub mod helper_context;
pub mod helpers;
pub mod images;
pub mod learned;
pub mod manifest;
pub mod memory;
pub mod models;
pub mod observe;
pub mod permissions;
pub mod preflight;
pub mod progress;
pub mod project;
pub mod prompt;
pub mod reader;
pub mod relocate;
pub mod rollout;
pub mod ruler;
pub mod runtime;
pub mod sandbox;
pub mod session;
pub mod settings;
pub mod settings_commands;
pub(crate) mod settings_session;
pub mod settings_ui;
pub mod spend;
pub mod supervisor;
pub mod telemetry;
pub mod tools;
pub mod tui;
pub mod update;
pub mod web;
pub mod wire;

pub mod verification;

pub mod workbench;
