//! The task catalogue: twelve commits of this repository, four per tier,
//! and two explore tasks judged by the facts their answers state.
//!
//! The catalogue is a fixed table rather than a discovered one, because a
//! comparison whose task set can drift between runs compares two things that
//! were never the same question. Specification: `docs/ruler.md`
//! §2, which is also where each statement's derivation is recorded.

use super::model::{Fact, Task, Tier};

/// The twelve tasks, in tier then index order.
///
/// Populated by `GH-PANE-61A-SCORE` from `ruler.md` §2. `shortstat_lines` is
/// each commit's own `git show --shortstat` insertions plus deletions,
/// recorded in the worker report that filled this table. `test` is a list of
/// complete commands (program first, arguments after); four tasks (`S1`,
/// `H1`, `H2`, `H4`) name two commands in `ruler.md`'s table and carry two
/// entries here, each run in order by the ruler.
pub static CATALOGUE: &[Task] = &[
    // Tier 1 -- Leaf
    Task {
        id: "L1",
        tier: Tier::Leaf,
        commit: "fa66efc",
        statement: "a std::fs import used only by the unix permission tests breaks the Windows build after a split",
        test: &[&["cargo", "build", "-p", "glasshouse", "--tests"]],
        shortstat_lines: 6,
        rubric: &[],
    },
    Task {
        id: "L2",
        tier: Tier::Leaf,
        commit: "ca18723",
        statement: "a test pins that a referenced file cannot be stored; line 1139's producer has landed",
        test: &[&["cargo", "test", "--test", "memory_file_observer"]],
        shortstat_lines: 20,
        rubric: &[],
    },
    Task {
        id: "L3",
        tier: Tier::Leaf,
        commit: "9c1b0a5",
        statement: "re-read Codex's hook catalogue from the installed 0.153.3 and make every declaration match",
        test: &[&["cargo", "test", "--lib", "harness::codex"]],
        shortstat_lines: 123,
        rubric: &[],
    },
    Task {
        id: "L4",
        tier: Tier::Leaf,
        commit: "ad2e8f5",
        statement: "the 1836 line must print after served:; the view tests read each account's block by position",
        test: &[&["cargo", "test", "--test", "entitlement_broker"]],
        shortstat_lines: 39,
        rubric: &[],
    },
    // Tier 2 -- Standard
    Task {
        id: "S1",
        tier: Tier::Standard,
        commit: "e9178c0",
        statement: "a verbatim (\\\\?\\) project root refuses every path inside it on Windows",
        test: &[
            &["cargo", "test", "--test", "project_isolation"],
            &["cargo", "test", "--lib", "commands::context_firewall"],
        ],
        shortstat_lines: 206,
        rubric: &[],
    },
    Task {
        id: "S2",
        tier: Tier::Standard,
        commit: "045c71d",
        statement: "the relay's gzip limit differs per harness — Codex always populated, Claude Code conditional",
        test: &[&["cargo", "test", "--test", "relay_usage"]],
        shortstat_lines: 32,
        rubric: &[],
    },
    Task {
        id: "S3",
        tier: Tier::Standard,
        commit: "09e6ae9",
        statement: "map lines 2409–2410: predict a conflict, and name the distinction rather than implying it",
        test: &[&["cargo", "test", "--test", "orchestrator_conflict"]],
        shortstat_lines: 205,
        rubric: &[],
    },
    Task {
        id: "S4",
        tier: Tier::Standard,
        commit: "2bdbbc5",
        statement: "the reranking tripwire fired as designed; invert it into a four-caller census and close 1625",
        test: &[&["cargo", "test", "--test", "memory_reranker"]],
        shortstat_lines: 137,
        rubric: &[],
    },
    // Tier 3 -- Heavy
    Task {
        id: "H1",
        tier: Tier::Heavy,
        commit: "a61ba99",
        statement: "the database bootstrap straggler waits on a timer and fails every module-level run under load",
        test: &[
            &["cargo", "test", "--test", "project_isolation"],
            &["cargo", "test", "--lib", "database"],
        ],
        shortstat_lines: 641,
        rubric: &[],
    },
    Task {
        id: "H2",
        tier: Tier::Heavy,
        commit: "26fb65b",
        statement: "create the project database privately and publish it with one hard link; the race's successor",
        test: &[
            &["cargo", "test", "--lib", "database"],
            &["cargo", "test", "--test", "memory_store"],
        ],
        shortstat_lines: 1217,
        rubric: &[],
    },
    Task {
        id: "H3",
        tier: Tier::Heavy,
        commit: "ee7799b",
        statement: "main.rs is over the size ratchet; split it into commands/ with every import path kept valid",
        test: &[&["scripts/blast-radius.sh"]],
        shortstat_lines: 58943,
        rubric: &[],
    },
    Task {
        id: "H4",
        tier: Tier::Heavy,
        commit: "f2883ca",
        statement: "map lines 2402–2405: edit intent, and 2392 finally gets a producer",
        test: &[
            &["cargo", "test", "--test", "edit_intent"],
            &["cargo", "test", "--test", "file_claims"],
        ],
        shortstat_lines: 1766,
        rubric: &[],
    },
    // Explore -- no change, judged by the facts its answer states
    // (2026-09-23): what a context-handling change is for is fewer cells
    // before the model knows enough, and no test command can see that.
    Task {
        id: "X1",
        tier: Tier::Standard,
        commit: "d971e552",
        statement: "explore how `pane ruler run` runs and scores one attempt of a task, and explain it: where the attempt's tree comes from, how the harness is launched and configured, and how the outcome and the token figures are decided",
        test: &[],
        shortstat_lines: 0,
        rubric: RULER_FACTS,
    },
    Task {
        id: "X2",
        tier: Tier::Standard,
        commit: "d971e552",
        statement: "explore how Pane shows the model a value a cell returned when it is too large for the context, and explain it: how big a return may be, what happens to the part that does not fit, and how the model gets the rest",
        test: &[],
        shortstat_lines: 0,
        rubric: RETURN_FACTS,
    },
    // Editing, fixing and writing (2026-09-23): each judged by its own check
    // after the harness stops -- the ruler never reads the model's claim.
    // The four cut at `91f34c29^` start from a tree whose own tests are green.
    Task {
        id: "F1",
        tier: Tier::Standard,
        commit: "587f994b",
        statement: "`cargo test -p pane --test request_modes` fails: four tests say a read-only request no longer proposes the explore mode. Find the cause and fix it without weakening the tests.",
        test: &[&["cargo", "test", "-p", "pane", "--test", "request_modes"]],
        shortstat_lines: 12,
        rubric: &[],
    },
    Task {
        id: "I1",
        tier: Tier::Standard,
        commit: "91f34c29",
        statement: "scripts/release/next-tag.py: also accept release-candidate tags, so `v1.2.3-rc.4` gives `v1.2.3-rc.5`; every form it accepts today must keep working.",
        test: &[&[
            "sh",
            "-c",
            "t=scripts/release/next-tag.py; [ \"$(python3 $t v1.2.3-rc.4)\" = v1.2.3-rc.5 ] && [ \"$(python3 $t v0.1.0-pre.3)\" = v0.1.0-pre.4 ] && [ \"$(python3 $t v0.1.0)\" = v0.1.1-pre.1 ] && [ \"$(python3 $t)\" = v0.1.0-pre.1 ]",
        ]],
        shortstat_lines: 4,
        rubric: &[],
    },
    Task {
        id: "I2",
        tier: Tier::Standard,
        commit: "91f34c29",
        statement: "Add `pub fn is_prerelease(tag: &str) -> bool` to crates/pane/src/update.rs: true for a release tag with a `-pre.N` part, false for a plain release tag and for anything that is not a release tag. Cover it with a unit test.",
        test: &[&[
            "sh",
            "-c",
            "printf '%s\\n' '#[test] fn hidden() { use pane::update::is_prerelease as p; assert!(p(\"v0.1.0-pre.2\")); assert!(!p(\"v0.1.0\")); assert!(!p(\"v0.1.0-pre.1-1316-gf96f87f0\")); assert!(!p(\"main\")); }' > crates/pane/tests/ruler_hidden_prerelease.rs && cargo test -p pane --test ruler_hidden_prerelease && cargo test -p pane --lib update::",
        ]],
        shortstat_lines: 15,
        rubric: &[],
    },
    Task {
        id: "W1",
        tier: Tier::Standard,
        commit: "91f34c29",
        statement: "Document subscription pools in sites/INSTALLERS.md: what a pool is, the `inference-gateway subscriptions pool --entitlement <name> --include|--exclude` command, and how the model picker shows and toggles accounts.",
        test: &[&[
            "sh",
            "-c",
            "f=sites/INSTALLERS.md; grep -q 'subscriptions pool' $f && grep -q -- '--exclude' $f && grep -q -- '--include' $f && grep -qi 'picker' $f",
        ]],
        shortstat_lines: 12,
        rubric: &[],
    },
    Task {
        id: "E1",
        tier: Tier::Standard,
        commit: "91f34c29",
        statement: "Rename Pane's `/usage` slash command to `/limits` everywhere it is offered, dispatched, described and tested; the gateway's `subscriptions usage` command keeps its name.",
        test: &[&[
            "sh",
            "-c",
            "grep -q '\"limits\"' crates/pane/src/commands.rs && ! grep -q '\"usage\" =>' crates/pane/src/session/controls.rs && cargo test -p pane --test tui_look && cargo test -p pane --test project",
        ]],
        shortstat_lines: 10,
        rubric: &[],
    },
];

/// X1's eight facts, each true of `ruler/attempt.rs` at X1's commit.
const RULER_FACTS: &[Fact] = &[
    Fact {
        name: "worktree",
        any: &["worktree"],
    },
    Fact {
        name: "parent commit",
        any: &[
            "parent commit",
            "parent of the",
            "commit^",
            "the parent",
            "its parent",
        ],
    },
    Fact {
        name: "pane config written",
        any: &[".pane/config.toml", "config.toml"],
    },
    Fact {
        name: "test commands after",
        any: &[
            "test command",
            "run_test_commands",
            "tests are run",
            "runs the tests",
            "runs the task's tests",
        ],
    },
    Fact {
        name: "errored outcome",
        any: &["errored"],
    },
    Fact {
        name: "suspect bound",
        any: &["suspect"],
    },
    Fact {
        name: "meter",
        any: &["meter", "routing-cost"],
    },
    Fact {
        name: "sequential",
        any: &["attempt_lock", "sequential", "serial", "one at a time"],
    },
];

/// X2's eight facts, each true of `runtime/outcome.rs` and `prompt/mod.rs`
/// at X2's commit.
const RETURN_FACTS: &[Fact] = &[
    Fact {
        name: "budget from room",
        any: &["quarter", "room / 4", "room/4", "25%", "25 %", "one fourth"],
    },
    Fact {
        name: "floor 4000",
        any: &["4000", "4_000", "4,000", "4 000", "4k"],
    },
    Fact {
        name: "ceiling 24000",
        any: &["24000", "24_000", "24,000", "24 000", "24k"],
    },
    Fact {
        name: "unknown window 8000",
        any: &["8000", "8_000", "8,000", "8 000", "8k"],
    },
    Fact {
        name: "field floor 600",
        any: &["600"],
    },
    Fact {
        name: "paged at a line",
        any: &[
            "line boundary",
            "line-boundary",
            "at a line",
            "whole lines",
            "at line",
        ],
    },
    Fact {
        name: "cursor to the rest",
        any: &[".excerpt", "excerpt(", ".slice", "cursor"],
    },
    Fact {
        name: "walk bound",
        any: &["1 mib", "1mib", "1,048,576", "1048576", "walk"],
    },
];

/// The task with this id, or `None`. Ids are case-sensitive: `L1`, not `l1`.
pub fn lookup(id: &str) -> Option<&'static Task> {
    CATALOGUE.iter().find(|task| task.id == id)
}

/// Every task in one tier, in catalogue order.
pub fn in_tier(tier: Tier) -> impl Iterator<Item = &'static Task> {
    CATALOGUE.iter().filter(move |task| task.tier == tier)
}
