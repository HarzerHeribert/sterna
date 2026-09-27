//! Regression tests for the ruler's scoring and rendering:
//! `crates/sterna/src/ruler/{tasks,score,report}.rs`. Specification:
//! `docs/ruler.md` §§2-4.

use std::collections::HashSet;
use std::time::Duration;

use sterna::ruler::score::Score;
use sterna::ruler::{Attempt, Harness, Outcome, Program, Tier, Tokens, report, tasks};

#[allow(clippy::too_many_arguments)]
fn attempt(
    task: &'static str,
    tier: Tier,
    harness: &str,
    attempt_num: u32,
    outcome: Outcome,
    tokens: Tokens,
    wall_secs: u64,
    turns: Option<u32>,
) -> Attempt {
    Attempt {
        task,
        tier,
        harness: Harness::new(harness),
        base_commit: "0000000".to_string(),
        attempt: attempt_num,
        outcome,
        tokens,
        wall_clock: Duration::from_secs(wall_secs),
        turns,
        changed_lines: None,
        program: None,
        interface: None,
        metrics: None,
        decisions_mode: None,
        decision_figures: None,
        rubric: None,
    }
}

#[test]
fn the_score_excludes_failed_attempts_and_names_the_tier() {
    // Two tiers, deliberately different token profiles: a fixture confined
    // to one tier cannot prove a tier's numbers stay its own, because
    // collapsing every tier into one is a no-op on a single-tier input.
    let harness = Harness::new("claude-code");

    let leaf_pass = attempt(
        "L1",
        Tier::Leaf,
        "claude-code",
        1,
        Outcome::Pass,
        Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        10,
        Some(3),
    );
    let leaf_fail = attempt(
        "L1",
        Tier::Leaf,
        "claude-code",
        2,
        Outcome::Fail,
        Tokens {
            input: Some(200),
            output: Some(100),
            cached_input: None,
        },
        20,
        Some(5),
    );
    let standard_pass = attempt(
        "S1",
        Tier::Standard,
        "claude-code",
        1,
        Outcome::Pass,
        Tokens {
            input: Some(1000),
            output: Some(500),
            cached_input: None,
        },
        60,
        Some(12),
    );
    let standard_fail = attempt(
        "S1",
        Tier::Standard,
        "claude-code",
        2,
        Outcome::Fail,
        Tokens {
            input: Some(2000),
            output: Some(1000),
            cached_input: None,
        },
        90,
        Some(20),
    );

    let score = Score::of(&[leaf_pass, leaf_fail, standard_pass, standard_fail]);

    let task_row = score
        .task_rows
        .iter()
        .find(|row| row.task == "L1" && row.harness == harness)
        .expect("L1/claude-code task row");
    assert_eq!(task_row.row.attempts_completed, 1);
    assert_eq!(task_row.row.attempts_made, 2);
    assert_eq!(task_row.row.tokens_per_completed, Some(150));
    assert_eq!(task_row.row.tokens_failed, Some(300));

    // (a) one tier row per tier for the harness
    let leaf_tier_row = score
        .tier_rows
        .iter()
        .find(|row| row.tier == Tier::Leaf && row.harness == harness)
        .expect("leaf/claude-code tier row");
    let standard_tier_row = score
        .tier_rows
        .iter()
        .find(|row| row.tier == Tier::Standard && row.harness == harness)
        .expect("standard/claude-code tier row");

    // (b) each row carries its own tier's numbers, and not the other's
    assert_eq!(leaf_tier_row.row.tokens_per_completed, Some(150));
    assert_eq!(leaf_tier_row.row.tokens_failed, Some(300));
    assert_eq!(standard_tier_row.row.tokens_per_completed, Some(1500));
    assert_eq!(standard_tier_row.row.tokens_failed, Some(3000));
    assert_ne!(
        leaf_tier_row.row.tokens_per_completed,
        standard_tier_row.row.tokens_per_completed
    );

    // (c) the aggregate differs from both tiers -- it is neither tier's
    // number, it is the whole harness's
    let aggregate_row = score
        .aggregate_rows
        .iter()
        .find(|row| row.harness == harness)
        .expect("claude-code aggregate row");
    assert_eq!(aggregate_row.row.tokens_per_completed, Some(825));
    assert_ne!(
        aggregate_row.row.tokens_per_completed,
        leaf_tier_row.row.tokens_per_completed
    );
    assert_ne!(
        aggregate_row.row.tokens_per_completed,
        standard_tier_row.row.tokens_per_completed
    );

    // The tier blocks survive into the rendered output alongside the
    // aggregate -- neither replaces the other (map line 2431).
    let table = report::render_table(&score);
    assert!(table.contains("-- tier leaf --"));
    assert!(table.contains("-- tier standard --"));
    assert!(table.contains("-- aggregate --"));
}

#[test]
fn an_unmetered_completed_attempt_is_excluded_from_the_average_denominator() {
    // A completed attempt whose tokens were never counted must not act as a
    // zero in the tokens-per-completed average: 150 metered tokens over one
    // metered attempt is 150, never 150 over two (one metered, one not).
    let metered = attempt(
        "L2",
        Tier::Leaf,
        "sterna",
        1,
        Outcome::Pass,
        Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        10,
        Some(3),
    );
    let unmetered = attempt(
        "L2",
        Tier::Leaf,
        "sterna",
        2,
        Outcome::Pass,
        Tokens::default(),
        10,
        Some(3),
    );

    let score = Score::of(&[metered, unmetered]);
    let row = &score.task_rows[0].row;

    assert_eq!(row.attempts_completed, 2);
    assert_eq!(row.metered_completed, 1);
    assert_eq!(row.tokens_per_completed, Some(150));
}

#[test]
fn an_uncounted_token_figure_never_renders_as_zero() {
    // No digit '0' appears anywhere in this fixture's measured figures, so
    // any '0' in the rendered table would have to come from a token or turn
    // figure that should instead be absent.
    let unmeasured = attempt(
        "L2",
        Tier::Leaf,
        "sterna",
        1,
        Outcome::Pass,
        Tokens::default(),
        5,
        None,
    );

    let score = Score::of(std::slice::from_ref(&unmeasured));
    let row = &score.task_rows[0].row;
    assert_eq!(row.tokens_per_completed, None);
    assert_eq!(row.tokens_failed, None);
    assert_eq!(row.turns, None);

    let table = report::render_table(&score);
    assert!(
        table.contains('\u{2014}'),
        "table should contain an em dash:\n{table}"
    );
    assert!(
        !table.contains('0'),
        "table should render no zero:\n{table}"
    );

    let jsonl = report::render_jsonl(&[unmeasured]);
    assert!(jsonl.contains("\"tokens_input\":null"));
    assert!(jsonl.contains("\"tokens_output\":null"));
    assert!(jsonl.contains("\"tokens_cached_input\":null"));
    assert!(jsonl.contains("\"turns\":null"));
}

#[test]
fn an_attempt_without_a_rollout_is_skipped_rather_than_counted_as_no_cells() {
    // The figure this whole column exists for: two real sessions of
    // 2026-09-17 ran at 1.6 and 1.98 calls per cell. An attempt whose
    // rollout was never kept knows nothing about either number, and folding
    // it in as zero cells and zero calls would drag a group's ratio toward
    // whatever the kept attempts happened to be.
    let mut kept = attempt(
        "S1",
        Tier::Standard,
        "sterna",
        1,
        Outcome::Pass,
        Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        10,
        Some(4),
    );
    kept.program = Some(Program {
        cells: 4,
        calls: 12,
    });

    let unkept = attempt(
        "S1",
        Tier::Standard,
        "sterna",
        2,
        Outcome::Pass,
        Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        10,
        Some(4),
    );
    assert_eq!(unkept.program, None);

    let score = Score::of(&[kept, unkept]);
    let row = &score.task_rows[0].row;
    assert_eq!(
        row.program,
        Some(Program {
            cells: 4,
            calls: 12
        }),
        "the kept attempt's figures stand alone; the unkept one adds nothing"
    );
    assert_eq!(
        row.program.and_then(|program| program.calls_per_cell()),
        Some(3.0),
        "12 calls over 4 cells -- not 12 over 8"
    );

    let table = report::render_table(&score);
    assert!(table.contains("  4  3.00  "), "{table}");
}

#[test]
fn a_group_that_kept_no_rollout_reads_unmeasured_in_both_program_columns() {
    let score = Score::of(&[attempt(
        "S1",
        Tier::Standard,
        "claude-code",
        1,
        Outcome::Pass,
        Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        10,
        Some(4),
    )]);
    assert_eq!(score.task_rows[0].row.program, None);

    let table = report::render_table(&score);
    let first = table.lines().nth(1).expect("a task row");
    assert!(
        first.ends_with("—  —  —"),
        "cells, calls/cell and tokens(failed) all unmeasured: {first}"
    );
}

#[test]
fn the_table_and_the_jsonl_have_exactly_these_columns() {
    assert_eq!(
        report::HEADERS,
        [
            "task",
            "harness",
            "outcome",
            "tokens/completed",
            "wall",
            "turns",
            "cells",
            "calls/cell",
            "tokens(failed)",
        ]
    );
    assert_eq!(
        report::JSONL_KEYS,
        [
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
        ]
    );
}

#[test]
fn the_catalogue_is_twelve_commit_tasks_four_per_tier_and_two_explore_tasks() {
    assert_eq!(tasks::CATALOGUE.len(), 19);

    let mut ids = HashSet::new();
    for task in tasks::CATALOGUE {
        assert!(ids.insert(task.id), "duplicate task id {}", task.id);
    }

    for tier in Tier::ALL {
        assert_eq!(
            tasks::in_tier(tier)
                .filter(|task| task.id.starts_with(['L', 'S', 'H']))
                .count(),
            4,
            "tier {tier:?} should have four commit tasks"
        );
    }

    // An explore task is judged by its answer alone: eight facts, no test
    // command, and a pass bound of three quarters.
    for id in ["X1", "X2"] {
        let task = tasks::lookup(id).unwrap_or_else(|| panic!("task {id} should exist"));
        assert_eq!(task.rubric.len(), 8, "{id}");
        assert!(task.test.is_empty(), "{id}");
        assert_eq!(task.rubric_bound(), 6, "{id}");
    }
}

#[test]
fn the_two_command_tasks_carry_exactly_two_commands_each() {
    for id in ["S1", "H1", "H2", "H4"] {
        let task = tasks::lookup(id).unwrap_or_else(|| panic!("task {id} should exist"));
        assert_eq!(
            task.test.len(),
            2,
            "task {id} should carry exactly two commands, got {:?}",
            task.test
        );
    }
}

#[test]
fn an_errored_attempt_counts_in_neither_token_column() {
    let errored = attempt(
        "L3",
        Tier::Leaf,
        "codex",
        1,
        Outcome::Errored,
        Tokens {
            input: Some(999),
            output: Some(999),
            cached_input: None,
        },
        1,
        Some(1),
    );

    let score = Score::of(&[errored]);
    let row = &score.task_rows[0].row;
    assert_eq!(row.attempts_completed, 0);
    assert_eq!(row.attempts_made, 1);
    assert_eq!(row.tokens_per_completed, None);
    assert_eq!(row.tokens_failed, None);
}

/// Map line 2432 is about what a comparison **presents**, so this asserts
/// the bytes the command actually emits, not the constants it declares.
///
/// Its predecessor, `the_table_and_the_jsonl_have_exactly_these_columns`,
/// compares `HEADERS` and `JSONL_KEYS` against literals -- which pins the
/// declaration and leaves the renderer free. Measured: appending
/// `tokens/turn` to `render_table`'s own header line, with both constants
/// untouched, SURVIVED the whole suite. A per-turn column could therefore
/// have been presented while every test stayed green, which is the one
/// outcome 2432 forbids.
#[test]
fn the_rendered_table_header_is_exactly_the_declared_columns() {
    let score = Score::of(&[attempt(
        "L1",
        Tier::Leaf,
        "claude-code",
        1,
        Outcome::Pass,
        Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        10,
        Some(3),
    )]);

    let table = report::render_table(&score);
    let header: Vec<&str> = table
        .lines()
        .next()
        .expect("render_table emits a header line")
        .split_whitespace()
        .collect();

    assert_eq!(header, report::HEADERS.to_vec());
    for line in table.lines() {
        assert!(
            !line.contains("tokens/turn") && !line.to_lowercase().contains("per turn"),
            "a tokens-per-turn column reached the rendered table: {line}"
        );
    }
}

/// The JSONL half of the same question: the keys `render_jsonl` actually
/// writes, in order, rather than the array it is supposed to write them from.
#[test]
fn the_rendered_jsonl_keys_are_exactly_the_declared_keys() {
    let rendered = report::render_jsonl(&[attempt(
        "L1",
        Tier::Leaf,
        "claude-code",
        1,
        Outcome::Pass,
        Tokens {
            input: Some(100),
            output: Some(50),
            cached_input: None,
        },
        10,
        Some(3),
    )]);

    let line = rendered.lines().next().expect("one attempt, one line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("render_jsonl emits JSON");
    let object = parsed.as_object().expect("each line is a JSON object");

    // Membership both ways: no declared key missing, and no key emitted that
    // was never declared -- the second half is what a smuggled per-turn
    // figure would trip.
    let emitted: HashSet<&str> = object.keys().map(String::as_str).collect();
    let declared: HashSet<&str> = report::JSONL_KEYS.iter().copied().collect();
    assert_eq!(emitted, declared);

    // Order, read off the bytes rather than the parse: `serde_json::Value`
    // holds a `BTreeMap`, so its iteration order is alphabetical and says
    // nothing about what `render_jsonl` wrote.
    let mut previous = 0;
    for key in report::JSONL_KEYS {
        let at = line
            .find(&format!("\"{key}\":"))
            .unwrap_or_else(|| panic!("{key} is not written as a key: {line}"));
        assert!(
            at >= previous,
            "{key} is emitted out of declared order: {line}"
        );
        previous = at;
    }
}
