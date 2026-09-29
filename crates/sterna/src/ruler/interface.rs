//! Interface regret: the `sterna:hybrid` arm of an ablation run compared, per
//! task and per dimension, with the best of the other `sterna:<mode>` arms.
//! Roadmap rows "Interface ablation runner" and "Hybrid interface choice".
//!
//! **Absent is never zero.** Every field of [`Metrics`] is an `Option` read
//! from Sterna's own `telemetry` document; an attempt whose document lacks a
//! figure is excluded from that dimension and counted as excluded, so a
//! missing producer can never look like a measured nought.

use std::collections::BTreeMap;

use serde_json::Value;

use super::model::Attempt;

/// The `--harness` row name of the sterna arm that is judged: `sterna:hybrid`.
pub const HYBRID_ARM: &str = "sterna:hybrid";

/// The file a sterna arm's stdout is captured to, under the attempt's own
/// worktree.
pub const RESULT_FILE: &str = "sterna-result.json";

/// The figures one sterna attempt's `telemetry` document carries that the
/// regret table compares. Absent fields stay `None`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Metrics {
    pub parent_requests: Option<u64>,
    pub parent_known_tokens: Option<u64>,
    pub execute_cell_calls: Option<u64>,
    pub direct_tool_calls: Option<u64>,
    pub frames_failed: Option<u64>,
    pub failures_by_kind: Option<BTreeMap<String, u64>>,
    pub repair_requests: Option<u64>,
    pub observation_bytes_rendered: Option<u64>,
    pub wall_time_ms: Option<u64>,
    pub completion_verified: Option<bool>,
}

impl Metrics {
    /// Reads the metrics out of what `sterna ... --output-format json` wrote to
    /// stdout: a JSON document with a top-level `telemetry` object, or a
    /// stream of JSON lines whose last `telemetry`-carrying line wins.
    /// `None` when no telemetry document is found at all -- an unmeasured
    /// attempt, distinct from one measured with every field absent.
    pub fn from_result_json(text: &str) -> Option<Metrics> {
        let telemetry = find_telemetry(text)?;
        Some(Metrics::from_telemetry(&telemetry))
    }

    /// Reads the fields by their documented paths; every miss stays `None`.
    pub fn from_telemetry(telemetry: &Value) -> Metrics {
        let at = |path: &[&str]| lookup(telemetry, path);
        Metrics {
            parent_requests: at(&["tokens", "parent", "requests"]).and_then(Value::as_u64),
            parent_known_tokens: at(&["tokens", "parent", "known_tokens"]).and_then(Value::as_u64),
            execute_cell_calls: at(&["interface", "provider_selected", "execute_cell_calls"])
                .and_then(Value::as_u64),
            direct_tool_calls: at(&["interface", "provider_selected", "direct_tool_calls"])
                .and_then(Value::as_u64),
            frames_failed: at(&["cells", "failed"]).and_then(Value::as_u64),
            failures_by_kind: at(&["failures", "by_kind"])
                .and_then(Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(kind, count)| count.as_u64().map(|n| (kind.clone(), n)))
                        .collect()
                }),
            repair_requests: at(&["recovery", "by_cause", "repair", "requests"])
                .and_then(Value::as_u64),
            observation_bytes_rendered: at(&["observation", "bytes_rendered"])
                .and_then(Value::as_u64),
            wall_time_ms: at(&["wall_time_ms"]).and_then(Value::as_u64),
            completion_verified: at(&["completion", "verified"]).and_then(Value::as_bool),
        }
    }
}

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

fn lookup<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |node, key| node.get(key))
}

/// The dimensions a hybrid arm is judged on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    VerifiedPasses,
    ParentRequests,
    RepairRequests,
    FramesFailed,
    ObservationBytes,
    Spend,
}

impl Dimension {
    pub const ALL: [Dimension; 6] = [
        Dimension::VerifiedPasses,
        Dimension::ParentRequests,
        Dimension::RepairRequests,
        Dimension::FramesFailed,
        Dimension::ObservationBytes,
        Dimension::Spend,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Dimension::VerifiedPasses => "verified passes",
            Dimension::ParentRequests => "parent requests",
            Dimension::RepairRequests => "repair requests",
            Dimension::FramesFailed => "frames failed",
            Dimension::ObservationBytes => "observation bytes",
            Dimension::Spend => "spend",
        }
    }

    /// Verified passes is the one dimension where more is better; the best
    /// alternative is its maximum and regret is `best - hybrid`.
    pub fn higher_is_better(self) -> bool {
        matches!(self, Dimension::VerifiedPasses)
    }

    fn measure(self, metrics: &Metrics) -> Option<f64> {
        match self {
            Dimension::VerifiedPasses => metrics
                .completion_verified
                .map(|verified| if verified { 1.0 } else { 0.0 }),
            Dimension::ParentRequests => metrics.parent_requests.map(|n| n as f64),
            Dimension::RepairRequests => metrics.repair_requests.map(|n| n as f64),
            Dimension::FramesFailed => metrics.frames_failed.map(|n| n as f64),
            Dimension::ObservationBytes => metrics.observation_bytes_rendered.map(|n| n as f64),
            Dimension::Spend => metrics.parent_known_tokens.map(|n| n as f64),
        }
    }
}

/// One task, one dimension: what the hybrid arm measured, the best other
/// sterna arm, and hybrid's regret against it. Every figure is the mean over
/// that arm's measured attempts, so arms with a different number of
/// excluded attempts stay comparable.
#[derive(Debug, Clone, PartialEq)]
pub struct RegretRow {
    pub task: &'static str,
    pub dimension: Dimension,
    pub hybrid: Option<f64>,
    /// The best alternative arm and its figure, when any other sterna arm
    /// measured this dimension.
    pub best: Option<(String, f64)>,
    /// Positive means hybrid did worse: `hybrid - best` for a cost,
    /// `best - hybrid` for verified passes. `None` is `unmeasured`.
    pub regret: Option<f64>,
    /// Attempts of this task's sterna arms excluded from this dimension for
    /// lack of a figure.
    pub excluded: u32,
}

/// Six rows per task that has a `sterna:hybrid` arm, in task then dimension
/// order. Attempts of rows that are not `sterna:<mode>` arms are ignored.
pub fn regret(attempts: &[Attempt]) -> Vec<RegretRow> {
    let mut by_task: BTreeMap<&'static str, BTreeMap<&str, Vec<&Attempt>>> = BTreeMap::new();
    for attempt in attempts.iter().filter(|a| a.interface.is_some()) {
        by_task
            .entry(attempt.task)
            .or_default()
            .entry(attempt.harness.as_str())
            .or_default()
            .push(attempt);
    }

    let mut rows = Vec::new();
    for (task, arms) in &by_task {
        if !arms.contains_key(HYBRID_ARM) {
            continue;
        }
        for dimension in Dimension::ALL {
            let mut excluded = 0;
            let mut means: Vec<(&str, f64)> = Vec::new();
            for (arm, group) in arms {
                let measured: Vec<f64> = group
                    .iter()
                    .filter_map(|a| a.metrics.as_ref())
                    .filter_map(|m| dimension.measure(m))
                    .collect();
                excluded += (group.len() - measured.len()) as u32;
                if !measured.is_empty() {
                    means.push((arm, measured.iter().sum::<f64>() / measured.len() as f64));
                }
            }
            let hybrid = means
                .iter()
                .find(|(arm, _)| *arm == HYBRID_ARM)
                .map(|(_, v)| *v);
            let best = means
                .iter()
                .filter(|(arm, _)| *arm != HYBRID_ARM)
                .fold(None::<(&str, f64)>, |best, &(arm, v)| match best {
                    Some((_, b))
                        if (dimension.higher_is_better() && b >= v)
                            || (!dimension.higher_is_better() && b <= v) =>
                    {
                        best
                    }
                    _ => Some((arm, v)),
                })
                .map(|(arm, v)| (arm.to_string(), v));
            let regret = match (hybrid, &best) {
                (Some(h), Some((_, b))) if dimension.higher_is_better() => Some(b - h),
                (Some(h), Some((_, b))) => Some(h - b),
                _ => None,
            };
            rows.push(RegretRow {
                task,
                dimension,
                hybrid,
                best,
                regret,
                excluded,
            });
        }
    }
    rows
}
