//! The table and the JSONL: two renderers of one [`Score`], sharing the
//! same two column sets so neither can gain a figure the other lacks.
//! Specification: `docs/ruler.md`.
//!
//! [`HEADERS`] and [`JSONL_KEYS`] are the whole column set, asserted
//! element-by-element in `crates/sterna/tests/ruler.rs`. **There is no
//! tokens-per-turn column and none is computed here** -- map line 2432;
//! [`Attempt::turns`](super::model::Attempt::turns) is read and printed, and
//! is never used as a divisor anywhere in this module.

use std::fmt::Write as _;

use super::decisions::{DecisionFigures, DecisionRow};
use super::interface::{Metrics, RegretRow};
use super::model::{Attempt, Outcome, Program, Tier};
use super::score::{AggregateRow, Row, Score, TaskRow, TierRow};

/// The rendered table's columns, in order. `render_table` emits exactly
/// these and no others.
pub const HEADERS: [&str; 9] = [
    "task",
    "harness",
    "outcome",
    "tokens/completed",
    "wall",
    "turns",
    "cells",
    "calls/cell",
    "tokens(failed)",
];

/// The JSONL's keys, in the order `render_jsonl` writes them. One line per
/// *attempt* (not per aggregated row), so these are the attempt's raw
/// fields: the three token figures the gateway metered, wall-clock, turn
/// count, and the test command's exit status. `exit_status` is derived from
/// [`Outcome`] (0 for a pass, 1 for a fail, absent for `Errored` since that
/// attempt never reached its test) -- `Attempt` carries no separate exit code.
/// `interface` and `metrics` are a `sterna:<mode>` ablation arm's mode and its
/// own telemetry figures, `null` on every other row.
pub const JSONL_KEYS: [&str; 18] = [
    "task",
    "harness",
    "commit",
    "attempt",
    "outcome",
    "tokens_input",
    "tokens_output",
    "tokens_cached_input",
    "wall_ms",
    "turns",
    "cells",
    "calls",
    "exit_status",
    "interface",
    "metrics",
    "decisions_mode",
    "decisions_figures",
    "rubric",
];

/// The decisions table's columns, in order; rendered only when some attempt
/// is a `sterna:decisions-<mode>` arm.
pub const DECISIONS_HEADERS: [&str; 11] = [
    "task",
    "arm",
    "verified",
    "findings",
    "holds",
    "overrides",
    "would_hold",
    "failed",
    "tokens(parent)",
    "wall",
    "excluded",
];

/// The interface-regret table's columns, in order; rendered only when some
/// attempt is a `sterna:<mode>` arm.
pub const REGRET_HEADERS: [&str; 6] = [
    "task",
    "dimension",
    "hybrid",
    "best(arm)",
    "regret",
    "excluded",
];

/// What an unmeasured figure renders as in the table -- never `0`.
const UNMEASURED: &str = "—";

/// Renders `ruler.md` §4's table: a row per `(task, harness)`, then one
/// block per tier, then the aggregate. The tier blocks are never skipped in
/// favour of the aggregate (map line 2431) and every row shares
/// [`HEADERS`]'s column set -- a tier or aggregate row uses the tier name or
/// `"aggregate"` in the `task` column rather than a differently shaped row.
pub fn render_table(score: &Score) -> String {
    let mut out = String::new();
    writeln!(out, "{}", HEADERS.join("  ")).expect("String write is infallible");

    for TaskRow {
        task, harness, row, ..
    } in &score.task_rows
    {
        writeln!(out, "{}", render_row(task, harness.as_str(), row))
            .expect("String write is infallible");
    }

    for tier in Tier::ALL {
        let rows: Vec<&TierRow> = score
            .tier_rows
            .iter()
            .filter(|row| row.tier == tier)
            .collect();
        if rows.is_empty() {
            continue;
        }
        writeln!(out, "-- tier {} --", tier.as_str()).expect("String write is infallible");
        for TierRow { harness, row, .. } in rows {
            writeln!(out, "{}", render_row(tier.as_str(), harness.as_str(), row))
                .expect("String write is infallible");
        }
    }

    if !score.aggregate_rows.is_empty() {
        writeln!(out, "-- aggregate --").expect("String write is infallible");
        for AggregateRow { harness, row } in &score.aggregate_rows {
            writeln!(out, "{}", render_row("aggregate", harness.as_str(), row))
                .expect("String write is infallible");
        }
    }

    if !score.regret.is_empty() {
        out.push_str(&render_regret_table(&score.regret));
    }

    out
}

/// The second table: `sterna:hybrid` against the best other sterna arm, per
/// task and dimension. Spend is the parent's known tokens, never a billed
/// figure.
fn render_regret_table(rows: &[RegretRow]) -> String {
    let mut out = String::new();
    writeln!(
        out,
        "-- interface regret (spend = parent known tokens, not billed) --"
    )
    .expect("String write is infallible");
    writeln!(out, "{}", REGRET_HEADERS.join("  ")).expect("String write is infallible");
    for row in rows {
        let best = match &row.best {
            Some((arm, value)) => format!("{}({arm})", fmt_measure(*value)),
            None => UNMEASURED.to_string(),
        };
        writeln!(
            out,
            "{}  {}  {}  {}  {}  {}",
            row.task,
            row.dimension.as_str(),
            row.hybrid.map_or(UNMEASURED.to_string(), fmt_measure),
            best,
            row.regret.map_or("unmeasured".to_string(), fmt_measure),
            row.excluded,
        )
        .expect("String write is infallible");
    }
    out
}

/// The third table: one row per `(task, arm)` group of `sterna:decisions-<mode>`
/// attempts, rendered only when [`decisions::rows`](super::decisions::rows)
/// returns any -- a run without `--sterna-decisions` prints nothing here.
/// `overrides` is a proxy for a false hold (a held cell the model then
/// overrode), not a measured one, until the measurement says otherwise.
pub fn render_decisions_table(rows: &[DecisionRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    writeln!(
        out,
        "-- decisions (per task, per arm; overrides is a false-hold proxy, not a measured one) --"
    )
    .expect("String write is infallible");
    writeln!(out, "{}", DECISIONS_HEADERS.join("  ")).expect("String write is infallible");
    for row in rows {
        writeln!(
            out,
            "{}  {}  {}/{}  {}  {}  {}  {}  {}  {}  {}  {}",
            row.task,
            row.arm,
            row.verified_n,
            row.verified_m,
            row.findings_sum,
            row.holds_sum,
            row.overrides_sum,
            row.would_hold_sum,
            row.failed_sum,
            fmt_tokens(row.tokens_mean),
            fmt_ms(row.wall_mean),
            row.excluded,
        )
        .expect("String write is infallible");
    }
    out
}

fn fmt_ms(ms: Option<u64>) -> String {
    match ms {
        Some(n) => fmt_wall(Some(std::time::Duration::from_millis(n))),
        None => UNMEASURED.to_string(),
    }
}

/// A per-attempt mean: whole numbers grouped, fractions to one decimal.
fn fmt_measure(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        let grouped = group_thousands(value.abs() as u64);
        if value < 0.0 {
            format!("-{grouped}")
        } else {
            grouped
        }
    } else {
        format!("{value:.1}")
    }
}

fn render_row(label: &str, harness: &str, row: &Row) -> String {
    format!(
        "{label}  {harness}  {outcome}  {tokens}  {wall}  {turns}  {cells}  {calls_per_cell}  {failed}",
        label = label,
        harness = harness,
        outcome = fmt_outcome(row.attempts_completed, row.attempts_made),
        tokens = fmt_tokens(row.tokens_per_completed),
        wall = fmt_wall(row.wall_per_completed),
        turns = fmt_turns(row.turns),
        cells = fmt_cells(row.program),
        calls_per_cell = fmt_calls_per_cell(row.program),
        failed = fmt_tokens(row.tokens_failed),
    )
}

fn fmt_outcome(completed: u32, made: u32) -> String {
    format!("{completed}/{made} pass")
}

fn fmt_tokens(tokens: Option<u64>) -> String {
    match tokens {
        Some(n) => group_thousands(n),
        None => UNMEASURED.to_string(),
    }
}

fn fmt_turns(turns: Option<u32>) -> String {
    match turns {
        Some(n) => n.to_string(),
        None => UNMEASURED.to_string(),
    }
}

/// The group's cells, or the unmeasured marker when no attempt in it kept a
/// rollout.
fn fmt_cells(program: Option<Program>) -> String {
    match program {
        Some(program) => program.cells.to_string(),
        None => UNMEASURED.to_string(),
    }
}

/// Calls per cell to two places. **The denominator is cells** -- turns are
/// printed here and divided into nowhere (map line 2432) -- and a group whose
/// rollouts were never kept, or that ran no cell, reads as unmeasured rather
/// than as `0.00`.
fn fmt_calls_per_cell(program: Option<Program>) -> String {
    match program.and_then(|program| program.calls_per_cell()) {
        Some(ratio) => format!("{ratio:.2}"),
        None => UNMEASURED.to_string(),
    }
}

fn fmt_wall(wall: Option<std::time::Duration>) -> String {
    match wall {
        Some(d) => {
            let total_secs = d.as_secs();
            let mins = total_secs / 60;
            let secs = total_secs % 60;
            if mins > 0 {
                format!("{mins}m{secs:02}s")
            } else {
                format!("{secs}s")
            }
        }
        None => UNMEASURED.to_string(),
    }
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped: Vec<char> = Vec::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    grouped.iter().rev().collect()
}

/// Renders one JSON line per attempt -- task, harness, commit, attempt,
/// outcome, the three metered token figures, wall-clock, turn count, and the
/// derived exit status -- so a later run can be diffed without re-reading a
/// table. Hand-written with `std::fmt`: `sterna`'s `Cargo.toml` names no
/// dependency and this module adds none.
pub fn render_jsonl(attempts: &[Attempt]) -> String {
    let mut out = String::new();
    for attempt in attempts {
        writeln!(out, "{}", render_jsonl_line(attempt)).expect("String write is infallible");
    }
    out
}

fn render_jsonl_line(attempt: &Attempt) -> String {
    format!(
        "{{\"task\":{task},\"harness\":{harness},\"commit\":{commit},\"attempt\":{attempt_num},\"outcome\":{outcome},\"tokens_input\":{tokens_input},\"tokens_output\":{tokens_output},\"tokens_cached_input\":{tokens_cached},\"wall_ms\":{wall_ms},\"turns\":{turns},\"cells\":{cells},\"calls\":{calls},\"exit_status\":{exit_status},\"interface\":{interface},\"metrics\":{metrics},\"decisions_mode\":{decisions_mode},\"decisions_figures\":{decisions_figures},\"rubric\":{rubric}}}",
        task = json_str(attempt.task),
        harness = json_str(attempt.harness.as_str()),
        commit = json_str(&attempt.base_commit),
        attempt_num = attempt.attempt,
        outcome = json_str(attempt.outcome.as_str()),
        tokens_input = json_opt(attempt.tokens.input),
        tokens_output = json_opt(attempt.tokens.output),
        tokens_cached = json_opt(attempt.tokens.cached_input),
        wall_ms = attempt.wall_clock.as_millis(),
        turns = json_opt(attempt.turns),
        cells = json_opt(attempt.program.map(|program| program.cells)),
        calls = json_opt(attempt.program.map(|program| program.calls)),
        exit_status = json_opt(exit_status(attempt.outcome)),
        interface = attempt
            .interface
            .as_deref()
            .map_or("null".to_string(), json_str),
        metrics = attempt
            .metrics
            .as_ref()
            .map_or("null".to_string(), render_metrics),
        decisions_mode = attempt
            .decisions_mode
            .as_deref()
            .map_or("null".to_string(), json_str),
        decisions_figures = attempt
            .decision_figures
            .as_ref()
            .map_or("null".to_string(), render_decision_figures),
        rubric = attempt.rubric.as_ref().map_or("null".to_string(), |score| {
            format!(
                "{{\"found\":[{}],\"total\":{}}}",
                score
                    .found
                    .iter()
                    .map(|name| json_str(name))
                    .collect::<Vec<_>>()
                    .join(","),
                score.total
            )
        }),
    )
}

/// The rubric tasks' A/B table: one row per (task, harness) with the mean
/// facts stated, how many attempts passed, and the mean wall-clock, cells
/// and parent tokens -- what a context-handling arm is judged by. Empty when no attempt carried a rubric score.
pub fn render_rubric_table(attempts: &[Attempt]) -> String {
    let mut keys: Vec<(&str, &str)> = Vec::new();
    for attempt in attempts.iter().filter(|a| a.rubric.is_some()) {
        let key = (attempt.task, attempt.harness.as_str());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("\ntask | harness | facts | passed | wall | cells | parent tokens\n");
    for (task, harness) in keys {
        let rows: Vec<&Attempt> = attempts
            .iter()
            .filter(|a| a.task == task && a.harness.as_str() == harness)
            .collect();
        let scored: Vec<&Attempt> = rows
            .iter()
            .copied()
            .filter(|a| a.rubric.is_some())
            .collect();
        let total = scored
            .first()
            .and_then(|a| a.rubric.as_ref())
            .map_or(0, |score| score.total);
        let mean = |values: Vec<f64>| -> String {
            if values.is_empty() {
                "-".to_string()
            } else {
                format!("{:.1}", values.iter().sum::<f64>() / values.len() as f64)
            }
        };
        let facts = mean(
            scored
                .iter()
                .filter_map(|a| a.rubric.as_ref().map(|score| f64::from(score.count())))
                .collect(),
        );
        let passed = rows.iter().filter(|a| a.outcome.completed()).count();
        let wall = mean(rows.iter().map(|a| a.wall_clock.as_secs_f64()).collect());
        let cells = mean(
            rows.iter()
                .filter_map(|a| a.program.map(|p| f64::from(p.cells)))
                .collect(),
        );
        let parent = mean(
            rows.iter()
                .filter_map(|a| a.metrics.as_ref().and_then(|m| m.parent_known_tokens))
                .map(|n| n as f64)
                .collect(),
        );
        writeln!(
            out,
            "{task} | {harness} | {facts}/{total} | {passed}/{} | {wall}s | {cells} | {parent}",
            rows.len()
        )
        .expect("String write is infallible");
    }
    out
}

/// The decision figures under stable keys; an absent figure is `null`.
fn render_decision_figures(figures: &DecisionFigures) -> String {
    format!(
        "{{\"verified\":{},\"findings\":{},\"finding_added\":{},\"holds\":{},\"overrides\":{},\"would_hold\":{},\"asked\":{},\"failed\":{},\"latency_ms_total\":{},\"parent_known_tokens\":{},\"wall_time_ms\":{}}}",
        json_opt(figures.verified),
        json_opt(figures.findings),
        json_opt(figures.finding_added),
        json_opt(figures.holds),
        json_opt(figures.overrides),
        json_opt(figures.would_hold),
        json_opt(figures.asked),
        json_opt(figures.failed),
        json_opt(figures.latency_ms_total),
        json_opt(figures.parent_known_tokens),
        json_opt(figures.wall_time_ms),
    )
}

/// The telemetry figures under stable keys; an absent figure is `null`.
fn render_metrics(metrics: &Metrics) -> String {
    let by_kind = match &metrics.failures_by_kind {
        Some(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(kind, count)| format!("{}:{count}", json_str(kind)))
                .collect();
            format!("{{{}}}", entries.join(","))
        }
        None => "null".to_string(),
    };
    format!(
        "{{\"parent_requests\":{},\"parent_known_tokens\":{},\"execute_cell_calls\":{},\"direct_tool_calls\":{},\"frames_failed\":{},\"failures_by_kind\":{},\"repair_requests\":{},\"observation_bytes_rendered\":{},\"wall_time_ms\":{},\"completion_verified\":{}}}",
        json_opt(metrics.parent_requests),
        json_opt(metrics.parent_known_tokens),
        json_opt(metrics.execute_cell_calls),
        json_opt(metrics.direct_tool_calls),
        json_opt(metrics.frames_failed),
        by_kind,
        json_opt(metrics.repair_requests),
        json_opt(metrics.observation_bytes_rendered),
        json_opt(metrics.wall_time_ms),
        json_opt(metrics.completion_verified),
    )
}

/// The test command's exit status, derived from `Outcome` since `Attempt`
/// carries no raw exit code: 0 for a pass (suspect or not), 1 for a fail,
/// and absent for `Errored` -- that attempt never reached its test command.
fn exit_status(outcome: Outcome) -> Option<i32> {
    match outcome {
        Outcome::Pass | Outcome::PassSuspect { .. } => Some(0),
        Outcome::Fail => Some(1),
        Outcome::Errored => None,
    }
}

fn json_opt(value: Option<impl std::fmt::Display>) -> String {
    match value {
        Some(v) => v.to_string(),
        None => "null".to_string(),
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
