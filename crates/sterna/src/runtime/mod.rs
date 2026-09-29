//! The runtime a tool result lives in — map lines 2461 and 2465.
//! Specification: `docs/runtime.md` and §3.
//!
//! Composition only. [`handles`] is the table of named live values;
//! [`preview`] renders one entry, type-directed first and size-capped second.
//! The V8 isolate that executes a model's program is a later package and
//! nothing here executes anything.

pub mod bindings;
pub mod capsule;
pub mod cell;
pub mod commands;
pub(crate) mod excerpt;
pub mod handlers;
pub mod handles;
pub mod isolate;
pub mod line_shapes;
pub mod marshal;
pub mod observation;
pub mod outcome;
pub mod preview;
pub mod reduce;
pub mod reduce_rules;
pub mod repair;
pub(crate) mod rewrites;
pub mod state;

pub mod instructions;

pub(crate) mod checks;
