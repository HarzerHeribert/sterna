//! Aggregation: from a slice of `Attempt`, produce a row per `(task,
//! harness)`, a row per `(tier, harness)`, and an aggregate row per
//! `harness`. Specification: `docs/ruler.md`.
//!
//! **The one decision that matters** is in [`build_row`]: the headline sums
//! only the tokens of attempts for which [`Outcome::completed`] is `true`,
//! divides by their count, and `Outcome::completed` is the only predicate
//! that decision may be filtered by -- a second spelling here is a second
//! place for it to drift.

use std::collections::BTreeMap;
use std::time::Duration;

use super::interface::{self, CreditRatios, RegretRow};
use super::model::{Attempt, Harness, Outcome, Program, Tier, Tokens};

/// One aggregated row: the three numbers `ruler.md` §3 names, grouped over
/// whatever set of attempts the caller built it from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub attempts_completed: u32,
    pub attempts_made: u32,
    /// How many of `attempts_completed` were actually metered -- the
    /// denominator `tokens_per_completed` divides by. A completed attempt
    /// whose tokens were never counted must not act as a zero in this
    /// average, so it is excluded from this count exactly as it is already
    /// excluded from the sum (`Tokens::sum` skips it rather than reading it
    /// as nought).
    pub metered_completed: u32,
    /// Sum of metered completed attempts' tokens, divided by
    /// `metered_completed`. `None` when no completed attempt in the group
    /// was ever metered -- never `0`.
    pub tokens_per_completed: Option<u64>,
    /// Sum of tokens spent by attempts that did not complete. `Outcome::Fail`
    /// only: `Errored` never reached its test and contributes to neither
    /// token column.
    pub tokens_failed: Option<u64>,
    /// Average wall-clock of completed attempts. `None` when none completed.
    pub wall_per_completed: Option<Duration>,
    /// Sum of turns across every attempt in the group, regardless of
    /// outcome. Printed and never divided into -- map line 2432.
    pub turns: Option<u32>,
    /// Cells and calls summed across every attempt in the group that kept a
    /// rollout. `None` when none did -- an attempt whose rollout was not kept
    /// is skipped, never averaged in as nought.
    ///
    /// Held as the two counts rather than their ratio so this row stays
    /// exact: the division belongs to the renderer, and its denominator is
    /// cells. Turns remain a divisor of nothing.
    pub program: Option<Program>,
}

/// One `(task, harness)` row, tier carried alongside for grouping into tier
/// blocks without a second pass over the source attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub task: &'static str,
    pub tier: Tier,
    pub harness: Harness,
    pub row: Row,
}

/// One `(tier, harness)` row: every attempt of every task in that tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierRow {
    pub tier: Tier,
    pub harness: Harness,
    pub row: Row,
}

/// One aggregate row per harness: every attempt of every task, every tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateRow {
    pub harness: Harness,
    pub row: Row,
}

/// The three levels `ruler.md` §4 prints: a row per task, a row per tier,
/// and an aggregate row -- the tier rows never replaced by the aggregate.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Score {
    pub task_rows: Vec<TaskRow>,
    pub tier_rows: Vec<TierRow>,
    pub aggregate_rows: Vec<AggregateRow>,
    /// Interface regret rows -- empty unless some attempt is a `sterna:<mode>`
    /// ablation arm, so a run without `--sterna-interface` renders exactly as
    /// before.
    pub regret: Vec<RegretRow>,
    /// The credit ratios `regret`'s weighted spend used.
    pub ratios: CreditRatios,
}

impl Score {
    /// Scores a slice of attempts at all three levels, weighting helper
    /// tokens at the assumed 1:1 ratio.
    pub fn of(attempts: &[Attempt]) -> Score {
        Score::with_ratios(attempts, CreditRatios::default())
    }

    /// Scores a slice of attempts at all three levels, and the interface
    /// regret of any `sterna:<mode>` arms under `ratios`.
    pub fn with_ratios(attempts: &[Attempt], ratios: CreditRatios) -> Score {
        let mut by_task: BTreeMap<(Tier, &'static str), BTreeMap<Harness, Vec<&Attempt>>> =
            BTreeMap::new();
        let mut by_tier: BTreeMap<Tier, BTreeMap<Harness, Vec<&Attempt>>> = BTreeMap::new();
        let mut by_harness: BTreeMap<Harness, Vec<&Attempt>> = BTreeMap::new();

        for attempt in attempts {
            by_task
                .entry((attempt.tier, attempt.task))
                .or_default()
                .entry(attempt.harness.clone())
                .or_default()
                .push(attempt);
            by_tier
                .entry(attempt.tier)
                .or_default()
                .entry(attempt.harness.clone())
                .or_default()
                .push(attempt);
            by_harness
                .entry(attempt.harness.clone())
                .or_default()
                .push(attempt);
        }

        let task_rows = by_task
            .into_iter()
            .flat_map(|((tier, task), by_h)| {
                by_h.into_iter().map(move |(harness, group)| TaskRow {
                    task,
                    tier,
                    harness,
                    row: build_row(&group),
                })
            })
            .collect();

        let tier_rows = by_tier
            .into_iter()
            .flat_map(|(tier, by_h)| {
                by_h.into_iter().map(move |(harness, group)| TierRow {
                    tier,
                    harness,
                    row: build_row(&group),
                })
            })
            .collect();

        let aggregate_rows = by_harness
            .into_iter()
            .map(|(harness, group)| AggregateRow {
                harness,
                row: build_row(&group),
            })
            .collect();

        Score {
            task_rows,
            tier_rows,
            aggregate_rows,
            regret: interface::regret(attempts, &ratios),
            ratios,
        }
    }
}

/// Builds one [`Row`] from a group of attempts sharing whatever key the
/// caller grouped by (a task, a tier, or nothing at all).
fn build_row(group: &[&Attempt]) -> Row {
    let attempts_made = group.len() as u32;

    let completed: Vec<&&Attempt> = group.iter().filter(|a| a.outcome.completed()).collect();
    let attempts_completed = completed.len() as u32;

    let completed_tokens: Vec<Tokens> = completed.iter().map(|a| a.tokens).collect();
    let metered_completed = completed_tokens
        .iter()
        .filter(|tokens| tokens.total().is_some())
        .count() as u32;
    let tokens_per_completed =
        Tokens::sum(completed_tokens.iter()).map(|sum| sum / u64::from(metered_completed));

    let failed_tokens: Vec<Tokens> = group
        .iter()
        .filter(|a| matches!(a.outcome, Outcome::Fail))
        .map(|a| a.tokens)
        .collect();
    let tokens_failed = Tokens::sum(failed_tokens.iter());

    let wall_per_completed = if attempts_completed == 0 {
        None
    } else {
        let total: Duration = completed.iter().map(|a| a.wall_clock).sum();
        Some(total / attempts_completed)
    };

    let turns = group
        .iter()
        .filter_map(|a| a.turns)
        .fold(None, |acc: Option<u32>, n| Some(acc.unwrap_or(0) + n));

    let programs: Vec<Program> = group.iter().filter_map(|a| a.program).collect();
    let program = Program::sum(programs.iter());

    Row {
        attempts_completed,
        attempts_made,
        metered_completed,
        tokens_per_completed,
        tokens_failed,
        wall_per_completed,
        turns,
        program,
    }
}
