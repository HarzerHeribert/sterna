//! Acceptance tests for GH-PANE-61G-WINDOW against
//! `docs/product/pane/events-contract.md` §1, §2, §3, §10.

use pane::events::window::{Accepted, DropReason, Window, WindowConfig};
use pane::events::{Event, Kind, PayloadRef, Priority, Stamp};

fn time_of_day_millis(h: i64, m: i64, s: i64, ms: i64) -> i64 {
    ((h * 60 + m) * 60 + s) * 1_000 + ms
}

fn hook_event(name: &str, source: &str, tool_call_id: &str, at_ms: i64, summary: &str) -> Event {
    Event::pending_hook(
        name,
        source,
        Stamp::from_millis(at_ms),
        PayloadRef::new(format!("payload-{tool_call_id}")),
        Priority::Batch,
        summary,
        tool_call_id,
    )
}

fn ci_cell_event(cell: &str, source: &str, conclusion: &str, at_ms: i64, summary: &str) -> Event {
    Event::pending(
        Kind::CiCell {
            cell: cell.to_string(),
            conclusion: conclusion.to_string(),
        },
        source,
        Stamp::from_millis(at_ms),
        PayloadRef::new(format!("payload-{cell}")),
        Priority::Batch,
        summary,
    )
}

fn timer_event(deadline: &str, source: &str, at_ms: i64, summary: &str) -> Event {
    Event::pending(
        Kind::Timer {
            deadline: deadline.to_string(),
        },
        source,
        Stamp::from_millis(at_ms),
        PayloadRef::new(format!("payload-{deadline}")),
        Priority::Batch,
        summary,
    )
}

fn worker_report_event(path: &str, source: &str, at_ms: i64, summary: &str) -> Event {
    Event::pending(
        Kind::WorkerReport {
            path: path.to_string(),
            mtime: format!("mtime-{path}"),
        },
        source,
        Stamp::from_millis(at_ms),
        PayloadRef::new(format!("payload-{path}")),
        Priority::Batch,
        summary,
    )
}

fn worker_quiet_event(source: &str, quiet_since: &str, at_ms: i64) -> Event {
    Event::pending(
        Kind::WorkerQuiet {
            quiet_since: quiet_since.to_string(),
        },
        source,
        Stamp::from_millis(at_ms),
        PayloadRef::new("payload-quiet"),
        Priority::Batch,
        "quiet",
    )
}

fn ci_run_interrupt(source: &str, conclusion: &str, at_ms: i64, summary: &str) -> Event {
    Event::pending(
        Kind::CiRun {
            conclusion: conclusion.to_string(),
        },
        source,
        Stamp::from_millis(at_ms),
        PayloadRef::new("payload-ci-run"),
        Priority::Interrupt,
        summary,
    )
}

/// Accepts `event` and asserts it was kept.
fn accept_kept(window: &mut Window, event: Event, now_ms: i64) {
    assert_eq!(
        window.accept(event, Stamp::from_millis(now_ms)),
        Accepted::Kept
    );
}

/// §10's worked turn: 30 `hook.PostToolUse` from one worker's loop, 4
/// `timer`, 3 `ci.cell`, 2 `worker.report`, one failing `ci.run` that
/// closes the window 1,204 ms after the first event.
#[test]
fn forty_events_in_one_window_are_one_batch_with_the_interrupt_first() {
    let mut window = Window::new(WindowConfig::default());

    // First accepted event doubles as the window's first-event marker
    // (now = 0) and as sample [4] -- the earliest `hook.PostToolUse`.
    accept_kept(
        &mut window,
        hook_event(
            "PostToolUse",
            "worker/api-routing",
            "tc-0",
            time_of_day_millis(16, 58, 31, 6),
            "Edit routing/mod.rs",
        ),
        0,
    );
    for i in 1..30 {
        accept_kept(
            &mut window,
            hook_event(
                "PostToolUse",
                "worker/api-routing",
                &format!("tc-{i}"),
                time_of_day_millis(16, 58, 31, 6) + i,
                &format!("tool call {i}"),
            ),
            0,
        );
    }

    // 4 timer events; the first is sample [3].
    accept_kept(
        &mut window,
        timer_event(
            "m1",
            "session/glasshouse-9c",
            time_of_day_millis(16, 58, 33, 402),
            "61G drafted; the primary appends it",
        ),
        0,
    );
    accept_kept(
        &mut window,
        timer_event("m2", "session/pane-spec", 0, "noise"),
        0,
    );
    accept_kept(
        &mut window,
        timer_event("m3", "session/glasshouse-9c", 0, "noise"),
        0,
    );
    accept_kept(
        &mut window,
        timer_event("m4", "session/pane-spec", 0, "noise"),
        0,
    );

    // 3 ci.cell events, all from the same run; the first is sample [2].
    accept_kept(
        &mut window,
        ci_cell_event(
            "ubuntu-24.04 · 1.90.0",
            "github/ci-extended#4471",
            "success",
            time_of_day_millis(16, 58, 36, 744),
            "ubuntu-24.04 · 1.90.0 — success",
        ),
        0,
    );
    accept_kept(
        &mut window,
        ci_cell_event(
            "windows-11-arm · 1.90.0",
            "github/ci-extended#4471",
            "success",
            0,
            "noise",
        ),
        0,
    );
    accept_kept(
        &mut window,
        ci_cell_event(
            "macos-14 · 1.90.0",
            "github/ci-extended#4471",
            "success",
            0,
            "noise",
        ),
        0,
    );

    // 2 worker.report events; pane-events is the oldest, so it is sample
    // [1] and board-watch is the cycled-back sample [5].
    accept_kept(
        &mut window,
        worker_report_event(
            "report-pane-events.md",
            "worker/pane-events",
            time_of_day_millis(16, 58, 39, 881),
            "report-pane-events.md",
        ),
        0,
    );
    accept_kept(
        &mut window,
        worker_report_event(
            "report-board-watch.md",
            "worker/board-watch",
            time_of_day_millis(16, 58, 40, 117),
            "report-board-watch.md",
        ),
        0,
    );

    // The failing ci.run interrupt, 1,204 ms after the first event.
    accept_kept(
        &mut window,
        ci_run_interrupt(
            "github/ci-extended#4471",
            "failure",
            time_of_day_millis(16, 58, 41, 204),
            "failure — 1 of 12 cells failed",
        ),
        1_204,
    );

    assert!(window.is_closed());
    let batch = window.take_batch().expect("window closed on the interrupt");
    assert_eq!(batch.n, 40);

    let rendered = batch.preview(256);
    let golden = include_str!("fixtures/events_view1.golden").replace("\r\n", "\n");
    assert_eq!(rendered, golden.trim_end_matches('\n'));
}

#[test]
fn a_second_arrival_of_one_dedup_key_is_dropped_and_the_first_keeps_its_stamp() {
    let mut window = Window::new(WindowConfig::default());

    let first_at = time_of_day_millis(10, 0, 0, 0);
    accept_kept(
        &mut window,
        ci_cell_event("ubuntu · 1.0", "github/run#1", "success", first_at, "first"),
        0,
    );

    let second = ci_cell_event(
        "ubuntu · 1.0",
        "github/run#1",
        "success",
        first_at + 5_000,
        "second, should be dropped",
    );
    assert_eq!(
        window.accept(second, Stamp::from_millis(0)),
        Accepted::Dropped(DropReason::Dedup)
    );

    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "close it"),
        10,
    );
    let batch = window.take_batch().unwrap();
    assert_eq!(batch.n, 2, "the duplicate must not have been kept");

    let surviving = batch.where_(Some("ci.cell"), None);
    assert_eq!(surviving.len(), 1);
    assert_eq!(surviving[0].at, Stamp::from_millis(first_at));
    assert_eq!(surviving[0].summary, "first");
}

#[test]
fn worker_quiet_never_displaces_worker_report() {
    let mut window = Window::new(WindowConfig::default());

    // Quiet arriving after the report is dropped outright.
    accept_kept(
        &mut window,
        worker_report_event("report.md", "worker/a", 0, "report"),
        0,
    );
    let quiet_after = worker_quiet_event("worker/a", "q1", 0);
    assert_eq!(
        window.accept(quiet_after, Stamp::from_millis(0)),
        Accepted::Dropped(DropReason::QuietUnderReport)
    );

    // Quiet arriving *before* the report must not survive either -- the
    // reverse reading (dropping the report instead) is the mistake the
    // contract calls out by name.
    accept_kept(&mut window, worker_quiet_event("worker/b", "q2", 0), 0);
    accept_kept(
        &mut window,
        worker_report_event("report-b.md", "worker/b", 0, "report b"),
        0,
    );

    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "close it"),
        0,
    );
    let batch = window.take_batch().unwrap();

    let reports = batch.where_(Some("worker.report"), None);
    assert_eq!(reports.len(), 2, "both reports must survive");
    let quiets = batch.where_(Some("worker.quiet"), None);
    assert!(
        quiets.is_empty(),
        "no quiet may share a window with that worker's report, either order"
    );
}

#[test]
fn the_window_closes_two_seconds_after_its_first_event_not_its_last() {
    let mut window = Window::new(WindowConfig::default());

    accept_kept(
        &mut window,
        ci_cell_event("a", "github/run#1", "success", 0, "a"),
        0,
    );
    assert!(window.close_if_due(Stamp::from_millis(1_999)).is_none());

    // Two more arrivals inside the window must not push the deadline out.
    accept_kept(
        &mut window,
        ci_cell_event("b", "github/run#1", "success", 0, "b"),
        500,
    );
    accept_kept(
        &mut window,
        ci_cell_event("c", "github/run#1", "success", 0, "c"),
        1_000,
    );
    assert!(
        window.close_if_due(Stamp::from_millis(1_999)).is_none(),
        "1,999 ms after the first event is still short of the 2,000 ms deadline"
    );

    let batch = window
        .close_if_due(Stamp::from_millis(2_000))
        .expect("2,000 ms after the first event, measured from it and not the 1,000 ms arrival");
    assert_eq!(batch.n, 3);
}

#[test]
fn an_interrupt_closes_the_window_at_once() {
    let mut window = Window::new(WindowConfig::default());

    accept_kept(
        &mut window,
        ci_cell_event("a", "github/run#1", "success", 0, "a"),
        0,
    );
    assert!(!window.is_closed());

    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "boom"),
        50,
    );
    assert!(window.is_closed(), "an interrupt closes the window at once");

    let batch = window.take_batch().unwrap();
    assert_eq!(batch.n, 2);

    // The next accept opens a new window.
    assert!(!window.is_closed());
    accept_kept(
        &mut window,
        ci_cell_event("d", "github/run#2", "success", 0, "d"),
        1_000,
    );
    assert!(!window.is_closed());
    assert!(window.close_if_due(Stamp::from_millis(1_000)).is_none());
}

#[test]
fn a_storm_fills_the_batch_oldest_first_and_spills_the_rest_in_order() {
    let mut window = Window::new(WindowConfig::default());

    for i in 0..250 {
        accept_kept(
            &mut window,
            ci_cell_event(&format!("cell-{i}"), "github/run#1", "success", 0, "storm"),
            0,
        );
    }

    let batch1 = window
        .close_if_due(Stamp::from_millis(2_000))
        .expect("the deadline is due");
    assert_eq!(batch1.n, 200);
    let kept = batch1.rest();
    assert_eq!(kept.len(), 200);
    assert_eq!(first_cell(kept[0]), "cell-0");
    assert_eq!(first_cell(kept[199]), "cell-199");

    // The 50 spilled events lead the next window, still oldest first.
    accept_kept(
        &mut window,
        ci_cell_event("fresh", "github/run#1", "success", 0, "fresh"),
        2_000,
    );
    let batch2 = window
        .close_if_due(Stamp::from_millis(4_000))
        .expect("the second window's own deadline is due");
    assert_eq!(batch2.n, 51);
    let kept2 = batch2.rest();
    assert_eq!(first_cell(kept2[0]), "cell-200");
    assert_eq!(first_cell(kept2[49]), "cell-249");
    assert_eq!(first_cell(kept2[50]), "fresh");
}

fn first_cell(event: &Event) -> &str {
    match &event.kind {
        Kind::CiCell { cell, .. } => cell,
        other => panic!("expected a ci.cell event, got {other:?}"),
    }
}

#[test]
fn unacked_events_roll_with_an_age_and_drop_at_four() {
    let mut window = Window::new(WindowConfig::default());

    accept_kept(
        &mut window,
        ci_cell_event("a", "worker/x", "success", 0, "a"),
        0,
    );
    accept_kept(
        &mut window,
        ci_cell_event("b", "worker/x", "success", 0, "b"),
        0,
    );
    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "close"),
        0,
    );
    let mut batch = window.take_batch().unwrap();
    let interrupt_id = batch.where_(Some("ci.run"), None)[0].id;
    batch.ack(&[interrupt_id]);

    let rolled = batch.roll();
    assert_eq!(rolled.events.len(), 2);
    assert!(
        rolled.events.iter().all(|(_, age)| *age == 1),
        "the first roll delivers both unacked events at age 1"
    );
    assert!(rolled.dropped.is_empty());

    let mut rolled = rolled;
    for expected_age in [2u32, 3] {
        window.carry_forward(rolled);
        accept_kept(
            &mut window,
            ci_run_interrupt("github/run#1", "failure", 0, "close"),
            0,
        );
        let mut batch = window.take_batch().unwrap();
        let interrupt_id = batch.where_(Some("ci.run"), None)[0].id;
        batch.ack(&[interrupt_id]);
        rolled = batch.roll();
        assert!(
            rolled.events.iter().all(|(_, age)| *age == expected_age),
            "expected age {expected_age}, got {:?}",
            rolled.events.iter().map(|(_, a)| a).collect::<Vec<_>>()
        );
    }

    // The fourth roll would make the age 4: both events are dropped instead.
    window.carry_forward(rolled);
    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "close"),
        0,
    );
    let mut batch = window.take_batch().unwrap();
    let interrupt_id = batch.where_(Some("ci.run"), None)[0].id;
    batch.ack(&[interrupt_id]);
    let rolled = batch.roll();
    assert!(rolled.events.is_empty(), "age 4 must not roll forward");
    assert_eq!(rolled.dropped.len(), 2, "both must be dropped at age 4");
}

#[test]
fn rolled_events_never_take_more_than_half_the_cap() {
    let config = WindowConfig {
        deadline_ms: 2_000,
        cap: 4,
        drop_age: 4,
    };
    let mut window = Window::new(config);

    accept_kept(
        &mut window,
        ci_cell_event("a", "worker/x", "success", 0, "a"),
        0,
    );
    accept_kept(
        &mut window,
        ci_cell_event("b", "worker/x", "success", 0, "b"),
        0,
    );
    accept_kept(
        &mut window,
        ci_cell_event("c", "worker/x", "success", 0, "c"),
        0,
    );
    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "close"),
        0,
    );
    let mut batch = window.take_batch().unwrap();
    let interrupt_id = batch.where_(Some("ci.run"), None)[0].id;
    batch.ack(&[interrupt_id]);

    let rolled = batch.roll();
    assert_eq!(
        rolled.events.len(),
        2,
        "half of a cap-4 batch is 2, even though 3 events wanted to roll"
    );
    assert_eq!(rolled.dropped.len(), 1);
}

#[test]
fn where_matches_hooks_by_prefix_and_rest_is_what_was_not_acked() {
    let mut window = Window::new(WindowConfig::default());

    accept_kept(
        &mut window,
        hook_event("PreToolUse", "worker/a", "tc-1", 0, "pre"),
        0,
    );
    accept_kept(
        &mut window,
        hook_event("PostToolUse", "worker/a", "tc-2", 0, "post"),
        0,
    );
    accept_kept(
        &mut window,
        worker_report_event("r1.md", "worker/a", 0, "report one"),
        0,
    );
    accept_kept(
        &mut window,
        worker_report_event("r2.md", "worker/b", 0, "report two"),
        0,
    );
    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "close"),
        0,
    );
    let mut batch = window.take_batch().unwrap();
    assert_eq!(batch.n, 5);

    let all_hooks = batch.where_(Some("hook.*"), None);
    assert_eq!(all_hooks.len(), 2, "the prefix matches every hook name");

    let only_post = batch.where_(Some("hook.PostToolUse"), None);
    assert_eq!(only_post.len(), 1);

    let mut to_ack: Vec<_> = batch
        .where_(Some("hook.*"), None)
        .into_iter()
        .map(|e| e.id)
        .collect();
    to_ack.extend(batch.where_(Some("ci.run"), None).into_iter().map(|e| e.id));
    to_ack.push(9_999); // an id the batch never held

    let acked = batch.ack(&to_ack);
    assert_eq!(acked.acked.len(), 3);
    assert_eq!(acked.unknown, vec![9_999]);

    let rest = batch.rest();
    assert_eq!(
        rest.len(),
        2,
        "the two worker.report events are still unacked"
    );
    assert!(
        rest.iter()
            .all(|e| matches!(e.kind, Kind::WorkerReport { .. }))
    );
}

#[test]
fn the_preview_shrinks_its_samples_before_cutting_anything_above_them() {
    let mut window = Window::new(WindowConfig::default());

    for i in 0..12 {
        accept_kept(
            &mut window,
            hook_event(
                "PostToolUse",
                "worker/a",
                &format!("tc-{i}"),
                0,
                &format!("a fairly long summary line for tool call number {i}"),
            ),
            0,
        );
    }
    accept_kept(
        &mut window,
        ci_run_interrupt("github/run#1", "failure", 0, "the interrupt summary line"),
        0,
    );
    let batch = window.take_batch().unwrap();

    let roomy = batch.preview(10_000);
    assert!(roomy.contains("[1]"), "a generous cap keeps all 5 samples");
    assert!(roomy.contains("[5]"));

    let tight = batch.preview(1);
    assert!(
        !tight.contains('['),
        "a 1-token cap must shrink samples to zero: {tight}"
    );
    assert!(
        tight.contains("batch  Events.Batch"),
        "the header is never cut: {tight}"
    );
    assert!(
        tight.contains("!  ci.run"),
        "the interrupt is never cut: {tight}"
    );
    assert!(
        tight.contains("hook.PostToolUse"),
        "the counts line is never cut: {tight}"
    );
}

// --- GH-PANE-61G-DELIVERY-AND-BG: §5's background jobs -------------------
//
// Every test below drives `pane::bg` directly, which is the module a cell's
// `bg.run` reaches through one callback. The cell-level half — the binding,
// the refusal a program catches, and the `batch` row the delivery leaves —
// is `tests/runtime_cells.rs`, where a `Runtime` is already in hand.

use pane::bg::{self, RunOptions};
// `WatchOptions` has two users now, and the second one —
// `a_watch_faster_than_the_floor_is_refused_rather_than_silently_slowed` — is ungated,
// because the floor is an argument check that happens before anything is spawned and is
// therefore the same decision on every platform. So the import is ungated too: it is dead
// on no platform, and dead is an error under `[workspace.lints.rust]`'s `-D warnings`.
use pane::bg::WatchOptions;
use pane::contract::SessionId;
use pane::sandbox::profile::Profile;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static JOBS: AtomicU64 = AtomicU64::new(0);

/// A throwaway project root and a session id nothing else in this binary
/// shares — `bg`'s board is keyed by session id and process-wide, so two
/// tests running on two threads must not name one board.
struct JobFixture {
    root: std::path::PathBuf,
    session: SessionId,
}

impl JobFixture {
    fn new(label: &str) -> Self {
        let n = JOBS.fetch_add(1, Ordering::Relaxed);
        let stem = format!("pane-bg-{}-{label}-{n}", std::process::id());
        let root = std::env::temp_dir().join(&stem);
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        Self {
            root,
            session: SessionId::new(stem),
        }
    }

    /// `Bash(...)` patterns are argv admission and grant no file access at
    /// all (`sandbox-grants.md` §2), which is why they are safe in a fixture.
    fn profile(&self) -> Profile {
        Profile::compile(
            &self.root,
            Some(
                r#"{"permissions":{"allow":["Bash(echo*)","Bash(while*)","Bash(do*)","Bash(trap*)"]}}"#,
            ),
        )
    }
}

impl Drop for JobFixture {
    fn drop(&mut self) {
        bg::shutdown(&self.session);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Whether any process on this machine is running a command line containing
/// `marker` — how `tests/tools.rs` proves a cancelled call left nothing
/// spinning, asked here of a job.
///
/// `-ww` because macOS `ps` otherwise cuts a command line at the terminal
/// width, and the marker of a job started with a long command line then never
/// appears — which reads exactly like "the job never started".
#[cfg(unix)]
fn running(marker: &str) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-A", "-ww", "-o", "command"])
        .output()
        .expect("ps is on every unix pane builds for");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.contains(marker) && !line.contains("ps -A"))
}

/// A command that runs until something stops it, carrying `marker` in its own
/// command line so [`running`] can find the process.
///
/// A **busy loop rather than `sleep`**, for the reason `tests/tools.rs`
/// records at its own fixture: `process-exec*` names the resolved binary and
/// nothing else (the 61D exec-roots ruling), so a confined `bash` cannot exec
/// `/bin/sleep` — it answers exit 126 in milliseconds, which reads exactly
/// like a job that never started. `while`, `do` and `:` are builtins and need
/// no exec at all. The marker rides in the loop body for the same reason it
/// is not a redirect: `bash` never execs a compound command, so the process
/// keeps the command line the marker is written into.
///
/// **Every caller kills it within seconds** and then asserts through `ps`
/// that it is gone, which is the whole point of the test that starts one.
#[cfg(unix)]
fn spinner(marker: &str) -> String {
    format!("while :; do : {marker}; done")
}

/// Waits until `predicate` holds or `budget` runs out, and answers whether it
/// held. **Every wait in this file is bounded**: a job that never finishes
/// must fail a test rather than hang one.
fn settles(budget: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    predicate()
}

/// §5: "`bg.run` returns before the process has done anything: the model
/// never blocks on output and never polls."
///
/// Measured, not argued: the command sleeps for seconds and the call is
/// expected back in a fraction of one.
#[cfg(unix)]
#[test]
fn bg_run_returns_a_handle_before_the_process_has_done_anything() {
    let fixture = JobFixture::new("immediate");
    let command = spinner("pane-immediate-marker");
    let started = Instant::now();
    let handle = bg::run(
        &fixture.profile(),
        &fixture.session,
        &command,
        &RunOptions::default(),
    )
    .expect("`while`, `do` and `:` are admitted by this fixture's profile");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "bg.run blocked for {elapsed:?}; §5 says it returns before the process has done anything"
    );
    assert!(!handle.is_empty());
    assert_eq!(bg::live(&fixture.session), 1);
    // Nothing has been delivered yet either -- the completion is an event,
    // not this call's return value.
    assert!(bg::drain(&fixture.session).is_empty());
}

/// §5: "a `bg.run` outside the grant throws `PermissionDenied` **at the
/// call, before any handle exists**."
///
/// Both halves are asserted: the refusal, and that nothing was left behind
/// for the model to hold or for the session to drain.
#[test]
fn a_command_outside_the_grant_is_refused_before_any_handle_exists() {
    let fixture = JobFixture::new("denied");
    let denied = bg::run(
        &fixture.profile(),
        &fixture.session,
        "curl https://example.com",
        &RunOptions::default(),
    )
    .expect_err("`curl` is admitted by no rule in this fixture's profile");
    assert!(!denied.rule.is_empty(), "a refusal names its deciding rule");
    assert_eq!(
        bg::live(&fixture.session),
        0,
        "a refused bg.run left a job on the board"
    );
    assert!(
        bg::drain(&fixture.session).is_empty(),
        "a refused bg.run raised an event"
    );
}

/// `cwd` and `env` are refused rather than silently ignored, and the refusal
/// is likewise before any handle exists.
#[test]
fn an_option_this_runtime_cannot_honour_is_refused_not_ignored() {
    let fixture = JobFixture::new("options");
    let denied = bg::run(
        &fixture.profile(),
        &fixture.session,
        "echo hello",
        &RunOptions {
            cwd: Some("/".to_string()),
            ..RunOptions::default()
        },
    )
    .expect_err("cwd is not honourable through one confinement");
    assert_eq!(denied.path, "cwd");
    assert_eq!(bg::live(&fixture.session), 0);
}

/// §5: "`bg.cancel` … a cancelled or timed-out job **still emits `bg.done`
/// with `status: "cancelled"`**, so nothing waits for a dead result" — and
/// the job it cancelled ignores the polite signal, so the ladder's second
/// rung is what stops it.
#[cfg(unix)]
#[test]
fn cancelling_a_job_that_ignores_sigterm_still_stops_it_and_reports() {
    let fixture = JobFixture::new("cancel");
    let marker = "pane-cancel-marker";
    let handle = bg::run(
        &fixture.profile(),
        &fixture.session,
        &format!("trap '' TERM; {}", spinner(marker)),
        &RunOptions::default(),
    )
    .expect("`trap`, `while`, `do` and `:` are admitted by this fixture's profile");
    assert!(
        settles(Duration::from_secs(10), || running(marker)),
        "the job never started; it raised {:?}",
        bg::drain(&fixture.session)
            .iter()
            .map(|event| event.summary.clone())
            .collect::<Vec<_>>()
    );

    bg::cancel(&fixture.session, &handle);
    // Idempotent: a second cancel is not a second kill and not a panic.
    bg::cancel(&fixture.session, &handle);

    assert!(
        settles(Duration::from_secs(10), || !running(marker)),
        "a cancelled job that ignores SIGTERM was still running; the ladder stopped at the polite \
         rung"
    );
    let events = settles(Duration::from_secs(10), || bg::live(&fixture.session) == 0)
        .then(|| bg::drain(&fixture.session))
        .expect("the cancelled job's thread never finished");
    let done = events
        .iter()
        .find(|event| event.kind.as_str() == "bg.done")
        .expect("a cancelled job still emits bg.done");
    assert_eq!(done.source, format!("bg/{handle}"));
    assert!(
        done.summary.contains("cancelled"),
        "bg.done did not report the cancellation: {}",
        done.summary
    );
}

/// A job's completion arrives as an event whose payload is fetched by id —
/// §5's "its result is a handle, never blocking output".
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_finished_job_reports_through_an_event_whose_payload_is_fetched_by_id() {
    let fixture = JobFixture::new("payload");
    bg::run(
        &fixture.profile(),
        &fixture.session,
        "echo forty-megabytes-worth",
        &RunOptions::default(),
    )
    .unwrap();
    assert!(
        settles(Duration::from_secs(20), || bg::live(&fixture.session) == 0),
        "the job never finished"
    );
    let events = bg::drain(&fixture.session);
    let done = events
        .iter()
        .find(|event| event.kind.as_str() == "bg.done")
        .expect("a finished job emits bg.done");
    let payload = bg::payload(&fixture.session, done.payload.as_str())
        .expect("the payload the event names is materialisable");
    assert_eq!(payload.status, "0");
    assert!(payload.stdout.contains("forty-megabytes-worth"));
}

/// §5's watch, and §1's `bg.done` key (`bg/<handle> + emission`): two runs
/// that printed the same thing are one event, and the `until` match ends the
/// watch.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_watch_emits_per_match_and_stops_when_until_matches() {
    let fixture = JobFixture::new("watch");
    let handle = bg::watch(
        &fixture.profile(),
        &fixture.session,
        "echo still-building",
        &WatchOptions {
            // The floor, not this test's own preference: a cadence under it
            // is refused at the call now, and this watch ends on its first
            // match either way.
            every_ms: 100,
            until: Some("still-building".to_string()),
            timeout_ms: Some(10_000),
        },
    )
    .unwrap();
    assert!(
        settles(Duration::from_secs(20), || bg::live(&fixture.session) == 0),
        "the watch never stopped, so `until` never ended it"
    );
    let events = bg::drain(&fixture.session);
    let done: Vec<_> = events
        .iter()
        .filter(|event| event.kind.as_str() == "bg.done")
        .collect();
    assert_eq!(done.len(), 1, "the first match should have ended the watch");
    assert_eq!(done[0].source, format!("bg/{handle}"));
    // Two arrivals of the same output share a dedup key, which is what makes
    // §1's "identical output inside one window is one event" true of a watch
    // without the watch knowing what a window is.
    let mut window = Window::new(WindowConfig::default());
    let first = done[0].clone();
    let second = first.clone();
    assert_eq!(window.accept(first, Stamp::from_millis(0)), Accepted::Kept);
    assert_eq!(
        window.accept(second, Stamp::from_millis(1)),
        Accepted::Dropped(DropReason::Dedup)
    );
}

/// **A background job outlives no session.** `shutdown` is what `run_task`
/// and `run` both call; after it, nothing this session started is running.
#[cfg(unix)]
#[test]
fn shutdown_leaves_no_job_of_this_session_running() {
    let marker = "pane-shutdown-marker";
    let session = {
        let fixture = JobFixture::new("shutdown");
        bg::run(
            &fixture.profile(),
            &fixture.session,
            &spinner(marker),
            &RunOptions::default(),
        )
        .unwrap();
        assert!(
            settles(Duration::from_secs(10), || running(marker)),
            "the job never started"
        );
        let session = fixture.session.clone();
        bg::shutdown(&session);
        session
    };
    assert_eq!(
        bg::live(&session),
        0,
        "a job survived the session that started it"
    );
    assert!(
        settles(Duration::from_secs(10), || !running(marker)),
        "a job's process survived `bg::shutdown`"
    );
}

/// `sandbox-grants.md` §3, from the other side now that Windows has an
/// applier: a background job on a platform pane can confine **runs**, and its
/// refusal — if any — is never the confinement's.
///
/// This case used to assert the opposite, because `tools::invoke::confine`
/// refused every spawning tool on Windows and a background job refused with
/// it. The AppContainer applier landed on 2026-09-09 and that refusal is
/// gone, so what is worth guarding is that it does not come back: `bg` has no
/// spawn path of its own, so a regression in the one confined spawn would
/// surface here as this exact string.
///
/// It asserts the absence rather than a success on purpose. Whether `bash` is
/// on a given Windows runner's `PATH` is a property of the machine, and a
/// test that required it would be asserting the runner rather than the cage.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[test]
fn a_background_job_is_never_refused_for_want_of_a_confinement() {
    let fixture = JobFixture::new("confinable");
    bg::run(
        &fixture.profile(),
        &fixture.session,
        "echo hello",
        &RunOptions::default(),
    )
    .expect("the grant admits this command");
    assert!(
        settles(Duration::from_secs(20), || bg::live(&fixture.session) == 0),
        "the job never finished"
    );
    let events = bg::drain(&fixture.session);
    let done = events
        .iter()
        .find(|event| event.kind.as_str() == "bg.done")
        .expect("a finished job emits bg.done");
    let payload = bg::payload(&fixture.session, done.payload.as_str()).unwrap();
    for absent in ["unconfined", "no sandbox applier"] {
        assert!(
            !payload.stderr.contains(absent),
            "the job was refused for want of a confinement: {}",
            payload.stderr
        );
    }
}

/// Finding 2 of the lifecycle verifier's report: **an exit must not be
/// charged for work that is already done.**
///
/// `shutdown` cancels every handle on the board, and `cancel`'s settle exists
/// so a *running* job's poll loop reaches its next tick. A job that finished
/// milliseconds after it started has no tick to reach, and paying
/// `CANCEL_SETTLE` for each of them made the end of every task cost 50 ms per
/// job the task had ever started -- measured against the shipped binary at
/// 12.34 s for 200 short jobs, and paid mid-session, because `run_task` calls
/// `shutdown` at the end of every task.
///
/// The bound is generous on purpose: the point is the slope, not the
/// constant. Forty already-finished jobs cost two seconds without the check
/// and milliseconds with it.
#[test]
fn shutdown_pays_no_settle_for_jobs_that_have_already_finished() {
    const COUNT: usize = 40;
    let fixture = JobFixture::new("finished-cost");
    for _ in 0..COUNT {
        bg::run(
            &fixture.profile(),
            &fixture.session,
            "echo done",
            &RunOptions::default(),
        )
        .expect("`echo` is admitted by this fixture's profile");
    }
    assert!(
        settles(Duration::from_secs(60), || bg::live(&fixture.session) == 0),
        "the jobs never finished, so this test would measure something else"
    );

    let started = Instant::now();
    bg::shutdown(&fixture.session);
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(1),
        "shutting {COUNT} already-finished jobs down took {elapsed:?}; the settle is being paid \
         for work that was over before it was asked for"
    );
}

/// Finding 3: **`every` has a floor, and the floor is told to the program
/// rather than applied behind its back.**
///
/// A watch runs a fresh confined shell per tick, so the cadence is a resource
/// bound on model-chosen work: `{every: 1}` re-ran its command about as fast
/// as a process can be spawned and cost 13.20 s of CPU in a 24.5 s session
/// against 1.03 s for the same watch at a second -- while §1's dedup meant
/// the model saw exactly the same events either way.
///
/// It is a **refusal** rather than a clamp for the reason `cwd` and `env` are
/// refused: a program that asked for 1 ms and silently got 100 ms has been
/// told something untrue about its own polling. The refusal names the floor,
/// so a program can retry at it.
#[test]
fn a_watch_faster_than_the_floor_is_refused_rather_than_silently_slowed() {
    let fixture = JobFixture::new("floor");
    let denied = bg::watch(
        &fixture.profile(),
        &fixture.session,
        "echo tick",
        &WatchOptions {
            every_ms: 1,
            until: None,
            timeout_ms: None,
        },
    )
    .expect_err("a millisecond cadence is under the floor");
    assert_eq!(denied.tool, "bg.watch");
    assert_eq!(denied.path, "every");
    assert!(
        denied.rule.contains("100"),
        "the refusal did not name the floor a program should retry at: {}",
        denied.rule
    );
    assert_eq!(
        bg::live(&fixture.session),
        0,
        "a refused watch left a job on the board"
    );
    assert!(
        bg::drain(&fixture.session).is_empty(),
        "a refused watch raised an event"
    );

    // The floor itself is a cadence a program may ask for: this is a floor,
    // not a second default.
    bg::watch(
        &fixture.profile(),
        &fixture.session,
        "echo tick",
        &WatchOptions {
            every_ms: 100,
            until: Some("tick".to_string()),
            timeout_ms: Some(10_000),
        },
    )
    .expect("the floor itself is admitted");
}
