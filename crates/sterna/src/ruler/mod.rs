//! The ruler: run one fixed task set through two or more harnesses against
//! one meter, and report tokens per completed task, wall-clock and outcome
//! per workload tier. Map lines 2430-2432; specification
//! `docs/ruler.md`.
//!
//! Composition only — every type this module names lives in [`model`], every
//! decision in one of the siblings below.

pub mod attempt;
pub mod cli;
pub mod decisions;
pub mod interface;
pub mod meter;
pub mod model;
pub mod report;
pub mod score;
pub mod tasks;

pub use model::{Attempt, Harness, Outcome, Program, Task, Tier, Tokens};
