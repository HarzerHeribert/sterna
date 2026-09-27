//! Interface regret: `crates/sterna/src/ruler/interface.rs` and the second
//! table `report::render_table` grows when an attempt is a `sterna:<mode>`
//! arm. Roadmap rows "Interface ablation runner" and "Hybrid interface
//! choice".

use std::time::Duration;

use sterna::ruler::interface::{CreditRatios, Dimension, Metrics, regret};
use sterna::ruler::score::Score;
use sterna::ruler::{Attempt, Harness, Outcome, Tier, Tokens, report};

const FULL_DOCUMENT: &str = r#"{"session":"s1","telemetry":{"wall_time_ms":4321,"tokens":{"known_total":1500,"parent":{"requests":9,"known_tokens":1200,"models":[]},"helpers":{"requests":3,"known_tokens":300}},"interface":{"mode":"hybrid","provider_selected":{"execute_cell_calls":6,"direct_tool_calls":3}},"cells":{"executed":8,"failed":2},"failures":{"by_kind":{"denied":1,"timeout":1}},"recovery":{"by_cause":{"repair":{"requests":2},"retry":{"requests":1}}},"observation":{"bytes_rendered":20480},"completion":{"verified":false}}}"#;

fn arm(task: &'static str, mode: &str, attempt_no: u32, metrics: Option<Metrics>) -> Attempt {
    Attempt {
        task,
        tier: Tier::Leaf,
        harness: Harness::new(format!("sterna:{mode}")),
        base_commit: "0000000".to_string(),
        attempt: attempt_no,
        outcome: Outcome::Pass,
        tokens: Tokens::default(),
        wall_clock: Duration::from_secs(10),
        turns: None,
        changed_lines: None,
        program: None,
        interface: Some(mode.to_string()),
        metrics,
        decisions_mode: None,
        decision_figures: None,
        rubric: None,
    }
}

fn metrics(
    parent_requests: u64,
    repair: u64,
    frames_failed: u64,
    bytes: u64,
    parent_tokens: u64,
    helper_tokens: u64,
    verified: bool,
) -> Metrics {
    Metrics {
        parent_requests: Some(parent_requests),
        parent_known_tokens: Some(parent_tokens),
        helper_known_tokens: Some(helper_tokens),
        execute_cell_calls: None,
        direct_tool_calls: None,
        frames_failed: Some(frames_failed),
        failures_by_kind: None,
        repair_requests: Some(repair),
        observation_bytes_rendered: Some(bytes),
        wall_time_ms: Some(1000),
        completion_verified: Some(verified),
    }
}

fn row<'a>(
    rows: &'a [sterna::ruler::interface::RegretRow],
    task: &str,
    dimension: Dimension,
) -> &'a sterna::ruler::interface::RegretRow {
    rows.iter()
        .find(|r| r.task == task && r.dimension == dimension)
        .unwrap_or_else(|| panic!("no row for {task} / {dimension:?}"))
}

/// Contract 2: every documented field is read from its documented path.
#[test]
fn every_metric_is_parsed_from_the_telemetry_document() {
    let parsed = Metrics::from_result_json(FULL_DOCUMENT).expect("a telemetry document");
    assert_eq!(parsed.parent_requests, Some(9));
    assert_eq!(parsed.parent_known_tokens, Some(1200));
    assert_eq!(parsed.helper_known_tokens, Some(300));
    assert_eq!(parsed.execute_cell_calls, Some(6));
    assert_eq!(parsed.direct_tool_calls, Some(3));
    assert_eq!(parsed.frames_failed, Some(2));
    let by_kind = parsed.failures_by_kind.clone().expect("failures.by_kind");
    assert_eq!(by_kind.get("denied"), Some(&1));
    assert_eq!(by_kind.get("timeout"), Some(&1));
    assert_eq!(parsed.repair_requests, Some(2));
    assert_eq!(parsed.observation_bytes_rendered, Some(20480));
    assert_eq!(parsed.wall_time_ms, Some(4321));
    assert_eq!(parsed.completion_verified, Some(false));
    assert_eq!(
        parsed.weighted_spend(&CreditRatios::default()),
        Some(1500.0),
        "the assumed ratio weights helper tokens 1:1"
    );
}

/// Contract 2: an absent section stays `None` -- never a zero -- while the
/// sections that are present still parse, and a stream-JSON stdout is read
/// off its last `telemetry`-carrying line.
#[test]
fn absent_telemetry_fields_stay_none() {
    let partial = r#"{"telemetry":{"wall_time_ms":10,"tokens":{"parent":{"requests":4}},"cells":{"executed":1}}}"#;
    let parsed = Metrics::from_result_json(partial).expect("a telemetry document");
    assert_eq!(parsed.parent_requests, Some(4));
    assert_eq!(parsed.wall_time_ms, Some(10));
    assert_eq!(parsed.parent_known_tokens, None);
    assert_eq!(parsed.helper_known_tokens, None);
    assert_eq!(parsed.execute_cell_calls, None);
    assert_eq!(parsed.direct_tool_calls, None);
    assert_eq!(parsed.frames_failed, None, "cells.failed absent is not 0");
    assert_eq!(parsed.failures_by_kind, None);
    assert_eq!(parsed.repair_requests, None);
    assert_eq!(parsed.observation_bytes_rendered, None);
    assert_eq!(parsed.completion_verified, None);
    assert_eq!(
        parsed.weighted_spend(&CreditRatios::default()),
        None,
        "a spend with one lane unread is not a spend"
    );

    assert_eq!(Metrics::from_result_json("not json"), None);
    assert_eq!(Metrics::from_result_json(r#"{"tokens":{}}"#), None);
    assert_eq!(Metrics::from_result_json(""), None);

    let stream = "{\"type\":\"turn\"}\n{\"type\":\"result\",\"telemetry\":{\"wall_time_ms\":7}}\n";
    assert_eq!(
        Metrics::from_result_json(stream).map(|m| m.wall_time_ms),
        Some(Some(7))
    );
}

/// Contract 4: three arms of one task. Hybrid is worse than the best
/// alternative on parent requests and better on observation bytes; one arm
/// has an unmeasured attempt, which is excluded and counted rather than
/// read as zero.
#[test]
fn regret_compares_hybrid_with_the_best_other_arm_per_dimension() {
    let attempts = vec![
        // hybrid: 10 requests, 2 repairs, 1 frame failed, 1000 bytes, verified
        arm(
            "L1",
            "hybrid",
            1,
            Some(metrics(10, 2, 1, 1000, 800, 100, true)),
        ),
        arm(
            "L1",
            "hybrid",
            2,
            Some(metrics(12, 2, 1, 1200, 800, 100, true)),
        ),
        // cells: fewer requests, more bytes, one attempt unmeasured
        arm(
            "L1",
            "cells",
            1,
            Some(metrics(6, 1, 0, 5000, 900, 200, true)),
        ),
        arm("L1", "cells", 2, None),
        // tools: most requests, most bytes, never verified
        arm(
            "L1",
            "tools",
            1,
            Some(metrics(20, 4, 3, 9000, 2000, 0, false)),
        ),
        arm(
            "L1",
            "tools",
            2,
            Some(metrics(20, 4, 3, 9000, 2000, 0, false)),
        ),
    ];

    let rows = regret(&attempts, &CreditRatios::default());
    assert_eq!(rows.len(), Dimension::ALL.len(), "one row per dimension");

    let requests = row(&rows, "L1", Dimension::ParentRequests);
    assert_eq!(requests.hybrid, Some(11.0));
    assert_eq!(requests.best, Some(("sterna:cells".to_string(), 6.0)));
    assert_eq!(
        requests.regret,
        Some(5.0),
        "hybrid spent five more requests"
    );
    assert_eq!(requests.excluded, 1, "the unmeasured cells attempt");

    let bytes = row(&rows, "L1", Dimension::ObservationBytes);
    assert_eq!(bytes.hybrid, Some(1100.0));
    assert_eq!(bytes.best, Some(("sterna:cells".to_string(), 5000.0)));
    assert_eq!(bytes.regret, Some(-3900.0), "hybrid exposed fewer bytes");

    let repairs = row(&rows, "L1", Dimension::RepairRequests);
    assert_eq!(repairs.regret, Some(1.0));

    let frames = row(&rows, "L1", Dimension::FramesFailed);
    assert_eq!(frames.best, Some(("sterna:cells".to_string(), 0.0)));
    assert_eq!(frames.regret, Some(1.0));

    // Verified passes: more is better, so the best alternative is the
    // maximum and regret is best minus hybrid -- zero here, both verified.
    let passes = row(&rows, "L1", Dimension::VerifiedPasses);
    assert_eq!(passes.hybrid, Some(1.0));
    assert_eq!(passes.best, Some(("sterna:cells".to_string(), 1.0)));
    assert_eq!(passes.regret, Some(0.0));

    // Weighted spend at the assumed 1:1 ratio: hybrid 900, cells 1100,
    // tools 2000 -- hybrid is the cheapest, so its regret is negative.
    let spend = row(&rows, "L1", Dimension::WeightedSpend);
    assert_eq!(spend.hybrid, Some(900.0));
    assert_eq!(spend.best, Some(("sterna:cells".to_string(), 1100.0)));
    assert_eq!(spend.regret, Some(-200.0));
}

/// Contract 4: `--credit-ratio luna=0.2` weights helper tokens into the
/// spend, and a task whose hybrid arm is wholly unmeasured reports
/// `unmeasured`, not a zero regret.
#[test]
fn weighted_spend_uses_the_credit_ratio_and_unmeasured_stays_unmeasured() {
    let ratios = CreditRatios::parse("luna=0.2,terra=0.1").unwrap();
    assert_eq!(ratios.luna, 0.2);
    assert_eq!(ratios.terra, 0.1);
    assert!(!ratios.is_assumed());
    assert!(CreditRatios::parse("sol=1").is_err());
    assert!(CreditRatios::parse("luna=-1").is_err());
    assert!(CreditRatios::parse("luna=0.2,luna=0.3").is_err());

    let attempts = vec![
        arm(
            "S1",
            "hybrid",
            1,
            Some(metrics(5, 0, 0, 100, 1000, 1000, true)),
        ),
        arm("S1", "cells", 1, Some(metrics(5, 0, 0, 100, 1100, 0, true))),
        arm("S2", "hybrid", 1, None),
        arm("S2", "cells", 1, Some(metrics(5, 0, 0, 100, 1100, 0, true))),
    ];
    let rows = regret(&attempts, &ratios);

    let spend = row(&rows, "S1", Dimension::WeightedSpend);
    assert_eq!(spend.hybrid, Some(1200.0), "1000 + 0.2 x 1000");
    assert_eq!(spend.best, Some(("sterna:cells".to_string(), 1100.0)));
    assert_eq!(spend.regret, Some(100.0));

    let unmeasured = row(&rows, "S2", Dimension::WeightedSpend);
    assert_eq!(unmeasured.hybrid, None);
    assert_eq!(unmeasured.regret, None);
    assert_eq!(unmeasured.excluded, 1);

    let table = report::render_table(&Score::with_ratios(&attempts, ratios));
    assert!(table.contains("-- interface regret"), "{table}");
    assert!(table.contains("given ratio luna=0.2 terra=0.1"), "{table}");
    assert!(table.contains("not billed"), "{table}");
    assert!(
        !table.replace("not billed", "").contains("billed"),
        "{table}"
    );
    assert!(
        table.contains("S2  weighted spend  —  1,100(sterna:cells)  unmeasured  1"),
        "{table}"
    );
    assert!(
        table.contains("S1  weighted spend  1,200  1,100(sterna:cells)  100  0"),
        "{table}"
    );
}

/// Contract 5: without an interface arm the rendered table is byte-identical
/// to the pre-ablation rendering, and with one the default ratio is called
/// assumed.
#[test]
fn render_table_without_interfaces_is_unchanged() {
    let plain = Attempt {
        task: "L1",
        tier: Tier::Leaf,
        harness: Harness::new("claude-code"),
        base_commit: "0000000".to_string(),
        attempt: 1,
        outcome: Outcome::Pass,
        tokens: Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        wall_clock: Duration::from_secs(10),
        turns: Some(3),
        changed_lines: None,
        program: None,
        interface: None,
        metrics: None,
        decisions_mode: None,
        decision_figures: None,
        rubric: None,
    };
    let table = report::render_table(&Score::of(std::slice::from_ref(&plain)));
    // An attempt that kept no rollout reads unmeasured in both program
    // columns -- never `0` cells and never `0.00` calls per cell.
    let expected = "task  harness  outcome  tokens/completed  wall  turns  cells  calls/cell  tokens(failed)\n\
L1  claude-code  1/1 pass  150  10s  3  —  —  —\n\
-- tier leaf --\n\
leaf  claude-code  1/1 pass  150  10s  3  —  —  —\n\
-- aggregate --\n\
aggregate  claude-code  1/1 pass  150  10s  3  —  —  —\n";
    assert_eq!(table, expected);

    let jsonl = report::render_jsonl(std::slice::from_ref(&plain));
    assert!(
        jsonl.contains(
            "\"interface\":null,\"metrics\":null,\"decisions_mode\":null,\"decisions_figures\":null,\"rubric\":null}"
        ),
        "{jsonl}"
    );

    let with_arm = [
        plain.clone(),
        arm("L1", "hybrid", 1, Some(metrics(1, 0, 0, 10, 10, 10, true))),
    ];
    let table = report::render_table(&Score::of(&with_arm));
    assert!(
        table.starts_with(expected.lines().next().unwrap()),
        "{table}"
    );
    assert!(table.contains("assumed ratio luna=1 terra=1"), "{table}");
    assert!(
        table.contains("task  dimension  hybrid  best(arm)  regret  excluded"),
        "{table}"
    );
}
