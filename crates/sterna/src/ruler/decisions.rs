//! The decisions arms' figures: what a `sterna:decisions-<mode>` attempt's own
//! `telemetry` document reported, and the `-- decisions --` table built from
//! them. Map line 2646, `docs/decisions.md`'s schema.
//!
//! **Absent is never zero** -- the same rule `interface.rs` states for the
//! ablation arms. Every field of [`DecisionFigures`] is an `Option` read
//! from Sterna's own `telemetry` document; an attempt whose document lacks a
//! figure is excluded from that figure, and from the report row's `excluded`
//! column, so a missing producer can never look like a measured nought.

use std::collections::BTreeMap;

use serde_json::Value;

use super::model::Attempt;

/// The figures one decisions attempt's `telemetry` document carries.
/// `verified` and `findings` come from the top-level `completion` object;
/// everything else but `parent_known_tokens` and `wall_time_ms` comes from
/// `decisions` (`session/task.rs::decisions_telemetry`'s schema).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecisionFigures {
    pub verified: Option<bool>,
    pub findings: Option<u64>,
    pub finding_added: Option<bool>,
    pub holds: Option<u64>,
    pub overrides: Option<u64>,
    pub would_hold: Option<u64>,
    pub asked: Option<u64>,
    pub failed: Option<u64>,
    pub latency_ms_total: Option<u64>,
    pub parent_known_tokens: Option<u64>,
    pub wall_time_ms: Option<u64>,
}

impl DecisionFigures {
    /// Reads the figures out of what `sterna ... --output-format json` wrote
    /// to stdout: a JSON document with a top-level `telemetry` object, or a
    /// stream of JSON lines whose last `telemetry`-carrying line wins.
    /// `None` when no telemetry document is found at all.
    pub fn from_result_json(text: &str) -> Option<DecisionFigures> {
        let telemetry = find_telemetry(text)?;
        Some(DecisionFigures::from_telemetry(&telemetry))
    }

    /// Reads the fields by their documented paths; every miss stays `None`.
    pub fn from_telemetry(telemetry: &Value) -> DecisionFigures {
        let at = |path: &[&str]| lookup(telemetry, path);
        DecisionFigures {
            verified: at(&["completion", "verified"]).and_then(Value::as_bool),
            findings: at(&["completion", "findings"])
                .and_then(Value::as_array)
                .map(|findings| findings.len() as u64),
            finding_added: at(&["decisions", "completion", "finding_added"])
                .and_then(Value::as_bool),
            holds: at(&["decisions", "holds"]).and_then(Value::as_u64),
            overrides: at(&["decisions", "overrides"]).and_then(Value::as_u64),
            would_hold: at(&["decisions", "would_hold"]).and_then(Value::as_u64),
            asked: at(&["decisions", "asked"]).and_then(Value::as_u64),
            failed: at(&["decisions", "failed"]).and_then(Value::as_u64),
            latency_ms_total: at(&["decisions", "latency_ms_total"]).and_then(Value::as_u64),
            parent_known_tokens: at(&["tokens", "parent", "known_tokens"]).and_then(Value::as_u64),
            wall_time_ms: at(&["wall_time_ms"]).and_then(Value::as_u64),
        }
    }
}

/// Mirrors `interface.rs::find_telemetry`'s reading of the same document --
/// not called from here, since that function is private to its own module.
fn find_telemetry(text: &str) -> Option<Value> {
    if let Ok(Value::Object(document)) = serde_json::from_str::<Value>(text)
        && let Some(telemetry @ Value::Object(_)) = document.get("telemetry")
    {
        return Some(telemetry.clone());
    }
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|line| match line.get("telemetry") {
            Some(telemetry @ Value::Object(_)) => Some(telemetry.clone()),
            _ => None,
        })
        .next_back()
}

/// Mirrors `interface.rs::lookup`, for the same reason.
fn lookup<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |node, key| node.get(key))
}

/// One `(task, decisions arm)` row of the `-- decisions --` table: sums and
/// means over that group's measured attempts, and how many were excluded
/// for lack of a telemetry document at all.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecisionRow {
    pub task: &'static str,
    pub arm: String,
    /// Verified count, out of the attempts that answered the question.
    pub verified_n: u32,
    pub verified_m: u32,
    pub findings_sum: u64,
    pub holds_sum: u64,
    pub overrides_sum: u64,
    pub would_hold_sum: u64,
    pub failed_sum: u64,
    /// Mean parent known tokens over attempts that measured it.
    pub tokens_mean: Option<u64>,
    /// Mean wall time (ms) over attempts that measured it.
    pub wall_mean: Option<u64>,
    /// Attempts of this group that carried no telemetry document at all.
    pub excluded: u32,
}

/// One row per `(task, harness)` group of attempts that ran as a
/// `sterna:decisions-<mode>` arm (`attempt.decisions_mode.is_some()`); empty
/// when none did, so a run without `--sterna-decisions` renders no table.
pub fn rows(attempts: &[Attempt]) -> Vec<DecisionRow> {
    let mut by_group: BTreeMap<(&'static str, &str), Vec<&Attempt>> = BTreeMap::new();
    for attempt in attempts.iter().filter(|a| a.decisions_mode.is_some()) {
        by_group
            .entry((attempt.task, attempt.harness.as_str()))
            .or_default()
            .push(attempt);
    }

    let mut out = Vec::new();
    for ((task, arm), group) in by_group {
        let mut row = DecisionRow {
            task,
            arm: arm.to_string(),
            ..DecisionRow::default()
        };
        let mut tokens_vals = Vec::new();
        let mut wall_vals = Vec::new();

        for attempt in &group {
            let Some(figures) = &attempt.decision_figures else {
                row.excluded += 1;
                continue;
            };
            if let Some(verified) = figures.verified {
                row.verified_m += 1;
                if verified {
                    row.verified_n += 1;
                }
            }
            if let Some(findings) = figures.findings {
                row.findings_sum += findings;
            }
            if let Some(holds) = figures.holds {
                row.holds_sum += holds;
            }
            if let Some(overrides) = figures.overrides {
                row.overrides_sum += overrides;
            }
            if let Some(would_hold) = figures.would_hold {
                row.would_hold_sum += would_hold;
            }
            if let Some(failed) = figures.failed {
                row.failed_sum += failed;
            }
            if let Some(tokens) = figures.parent_known_tokens {
                tokens_vals.push(tokens);
            }
            if let Some(wall) = figures.wall_time_ms {
                wall_vals.push(wall);
            }
        }

        row.tokens_mean = mean(&tokens_vals);
        row.wall_mean = mean(&wall_vals);
        out.push(row);
    }
    out
}

fn mean(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<u64>() / values.len() as u64)
    }
}
