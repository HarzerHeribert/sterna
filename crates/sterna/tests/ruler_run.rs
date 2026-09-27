//! Acceptance tests for the ruler's runner (`ruler.rs::attempt`, `::meter`,
//! `::cli`). No test here launches a real harness: every "harness" is a
//! small shell script this file writes into its own temp directory and
//! removes at the end of the process. That is the whole point -- 61D's
//! sandbox is not built, so nothing model-authored may execute here.

#![cfg(unix)]
//! **Unix only, at file scope.** Every fake in this file -- the harness, the
//! test command, the gateway stand-in -- is a shell script with a mode
//! bit, so the module does not compile on Windows rather than failing there.
//! The Windows sterna cell added on 2026-09-05 runs the rest of the crate; a
//! `.cmd` twin for these fakes is the successor if Windows coverage of the
//! runner is wanted.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use sterna::ruler::attempt::{self, HarnessCommand, RunOpts};
use sterna::ruler::cli;
use sterna::ruler::decisions::{self, DecisionFigures};
use sterna::ruler::meter::{Meter, Readout};
use sterna::ruler::model::{Attempt, Harness, Outcome, Task, Tier, Tokens};
use sterna::ruler::report;

/// Writes an executable shell script to `dir` that appends its own working
/// directory to `record`, then exits with `exit_code`.
fn write_script(dir: &Path, name: &str, record: &Path, exit_code: i32) -> PathBuf {
    let path = dir.join(name);
    let contents = format!(
        "#!/bin/sh\npwd >> \"{}\"\nexit {}\n",
        record.display(),
        exit_code
    );
    fs::write(&path, contents).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// Writes an executable shell script to `dir` that appends its own argv --
/// NUL-separated, so an argument containing spaces, quotes or a `{` survives
/// as the one argument it was launched with -- to `argv_record`, writes its
/// `ANTHROPIC_BASE_URL` to `env_record` and its working directory to
/// `argv_record` with a `.cwd` suffix, then exits with `exit_code`.
fn write_argv_script(
    dir: &Path,
    name: &str,
    argv_record: &Path,
    env_record: &Path,
    exit_code: i32,
) -> PathBuf {
    let path = dir.join(name);
    let cwd_record = argv_record.with_extension("cwd");
    let contents = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$@\" >> \"{}\"\nprintf '%s' \"$ANTHROPIC_BASE_URL\" > \"{}\"\npwd > \"{}\"\nexit {}\n",
        argv_record.display(),
        env_record.display(),
        cwd_record.display(),
        exit_code
    );
    fs::write(&path, contents).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// Writes an executable shell script that stands in for `inference-gateway`
/// as the meter: a `routing-cost ...` invocation appends its own working
/// directory to `meter_cwd_record`; any other invocation is a silent exit 0.
fn write_meter_script(dir: &Path, name: &str, meter_cwd_record: &Path) -> PathBuf {
    let path = dir.join(name);
    let contents = format!(
        "#!/bin/sh\nif [ \"$1\" = \"routing-cost\" ]; then\n  pwd >> \"{}\"\nfi\nexit 0\n",
        meter_cwd_record.display()
    );
    fs::write(&path, contents).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// Reads a NUL-separated argv record written by [`write_argv_script`].
fn read_argv(record: &Path) -> Vec<String> {
    fs::read(record)
        .unwrap()
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Reads the `.cwd` sibling [`write_argv_script`] writes next to `record`.
fn read_argv_cwd(record: &Path) -> PathBuf {
    PathBuf::from(
        fs::read_to_string(record.with_extension("cwd"))
            .unwrap()
            .trim(),
    )
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/sterna sits two levels under the repo root")
        .to_path_buf()
}

fn git_output(args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root())
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn head_commit() -> String {
    git_output(&["rev-parse", "HEAD"])
}

fn parent_of(commit: &str) -> String {
    git_output(&["rev-parse", &format!("{commit}^")])
}

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sterna-ruler-test-{}-{}-{}",
        label,
        std::process::id(),
        unique()
    ));
    fs::create_dir_all(&dir).unwrap();
    // Canonicalize immediately: on macOS `TMPDIR` sits under a `/var` that is
    // itself a symlink to `/private/var`, and a shell's `pwd` inside the
    // worktree reports the resolved form. Comparing against that later needs
    // this path already resolved, since by then the worktree (and so the
    // only other thing we could canonicalize against) is gone.
    fs::canonicalize(&dir).unwrap_or(dir)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A `'static` copy of a runtime string, leaked deliberately: [`Task`]'s
/// fields are `&'static str` and these tests need a real commit and real
/// script paths picked at run time, not baked in at compile time.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// One `task.test` entry that runs a single script with no arguments.
fn single_command(script: &Path) -> &'static [&'static [&'static str]] {
    let program = leak(script.to_string_lossy().into_owned());
    let command: &'static [&'static str] = Box::leak(vec![program].into_boxed_slice());
    Box::leak(vec![command].into_boxed_slice())
}

/// Two `task.test` entries, run in order.
fn two_commands(first: &Path, second: &Path) -> &'static [&'static [&'static str]] {
    let a: &'static [&'static str] =
        Box::leak(vec![leak(first.to_string_lossy().into_owned())].into_boxed_slice());
    let b: &'static [&'static str] =
        Box::leak(vec![leak(second.to_string_lossy().into_owned())].into_boxed_slice());
    Box::leak(vec![a, b].into_boxed_slice())
}

fn base_task(commit: &'static str, test: &'static [&'static [&'static str]]) -> Task {
    Task {
        id: "T1",
        tier: Tier::Leaf,
        commit,
        statement: "do the thing",
        test,
        shortstat_lines: 100,
        rubric: &[],
    }
}

fn base_opts(scratch: PathBuf, harness_program: PathBuf) -> RunOpts {
    let mut harnesses = HashMap::new();
    harnesses.insert(
        "fake".to_string(),
        HarnessCommand {
            program: harness_program,
            args: vec!["{statement}".to_string()],
            interface: None,
            decisions: None,
        },
    );
    RunOpts {
        scratch,
        gateway: None,
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: None,
        helpers_model: None,
    }
}

#[test]
fn an_attempt_runs_its_test_on_the_worktree_and_never_in_the_checkout() {
    let scratch = scratch_dir("cwd");
    let harness_cwd = scratch.join("harness_cwd.txt");
    let test_cwd = scratch.join("test_cwd.txt");
    let harness_script = write_script(&scratch, "fake_harness.sh", &harness_cwd, 0);
    let test_script = write_script(&scratch, "fake_test.sh", &test_cwd, 0);

    let before = git_output(&["status", "--porcelain"]);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = base_opts(scratch.clone(), harness_script);
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);

    assert!(
        result.outcome.completed(),
        "attempt should complete: {:?}",
        result.outcome
    );

    let harness_saw = fs::read_to_string(&harness_cwd).unwrap();
    let test_saw = fs::read_to_string(&test_cwd).unwrap();
    let attempt_dir = PathBuf::from(harness_saw.trim());
    assert_eq!(PathBuf::from(test_saw.trim()), attempt_dir);
    assert!(
        attempt_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("{}-{}-{}-", task.id, harness.as_str(), 1)),
        "basename must still carry the task, harness and repeat: {attempt_dir:?}"
    );

    assert!(
        !attempt_dir.exists(),
        "worktree must be removed after the attempt"
    );

    let after = git_output(&["status", "--porcelain"]);
    assert_eq!(
        before, after,
        "checkout must be byte-unchanged after an attempt"
    );
}

#[test]
fn the_attempt_starts_at_the_parent_commit() {
    let scratch = scratch_dir("parent");
    let noop_cwd = scratch.join("noop_cwd.txt");
    let noop = write_script(&scratch, "noop.sh", &noop_cwd, 0);

    let head = head_commit();
    let parent = parent_of(&head);

    let commit = leak(head);
    let task = base_task(commit, single_command(&noop));
    let opts = base_opts(scratch, noop);
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);

    assert_eq!(result.base_commit, parent);
}

#[test]
fn an_unreadable_meter_leaves_the_tokens_absent() {
    let scratch = scratch_dir("unreadable-meter");
    let cwd_record = scratch.join("cwd.txt");
    let noop = write_script(&scratch, "noop.sh", &cwd_record, 0);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&noop));
    let mut opts = base_opts(scratch, noop);
    opts.meter = Meter::Command {
        executable: PathBuf::from("/definitely/does/not/exist/inference-gateway"),
    };
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);

    assert!(
        result.outcome.completed(),
        "an unreadable meter must not fail the attempt"
    );
    assert_eq!(result.tokens.total(), None);
    assert_eq!(result.turns, None);
}

#[test]
fn a_null_token_column_is_not_a_zero() {
    let from = std::time::UNIX_EPOCH + std::time::Duration::from_secs(0);
    let to = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000);
    let lines = [
        r#"{"observed_at":100,"input_tokens":40,"output_tokens":10}"#,
        r#"{"observed_at":200,"input_tokens":null,"output_tokens":null}"#,
    ];

    let (tokens, turns) = Readout::for_window(lines, from, to);

    assert_eq!(
        tokens.total(),
        Some(50),
        "the null row must not zero a sum built from real rows"
    );
    assert_eq!(turns, Some(2));
}

#[test]
fn a_configured_meter_that_finds_nothing_completes_with_zero_turns_not_none() {
    // Distinguishes "meter answered with no rows" (turns: Some(0)) from "no
    // meter configured" (turns: None) end to end through `run_one`, not just
    // inside `meter.rs` -- collapsing the two used to make a broken meter
    // (session filtering that could never match) look identical to an
    // intentionally unconfigured one.
    let scratch = scratch_dir("meter-zero-rows");
    let cwd_record = scratch.join("cwd.txt");
    let noop = write_script(&scratch, "noop.sh", &cwd_record, 0);
    let fake_gateway = write_script(
        &scratch,
        "fake_gateway.sh",
        &scratch.join("gateway_calls.txt"),
        0,
    );

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&noop));
    let mut opts = base_opts(scratch, noop);
    opts.meter = Meter::Command {
        executable: fake_gateway,
    };
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);

    assert!(result.outcome.completed());
    assert_eq!(result.tokens.total(), None);
    assert_eq!(
        result.turns,
        Some(0),
        "a meter that ran and printed nothing is zero turns, not an absent meter"
    );
}

#[test]
fn concurrent_attempts_never_overlap_their_harness_launch() {
    // Enforces the fix for the session-id defect: since token attribution is
    // by time window alone (`meter.rs`'s module doc comment), two attempts'
    // harness-launch-through-meter-read spans must never overlap process-wide.
    // Each "harness" here tries to claim an exclusive lock directory; if two
    // ever run concurrently, the second one's claim fails and it records a
    // collision.
    let scratch = scratch_dir("serial-enforcement");
    let lockdir = scratch.join("lock");
    let collisions = scratch.join("collisions.txt");
    fs::write(&collisions, "").unwrap();

    let probe_script = scratch.join("mutex_probe.sh");
    let contents = format!(
        "#!/bin/sh\nif mkdir \"{lock}\" 2>/dev/null; then\n  sleep 0.15\n  rmdir \"{lock}\"\nelse\n  echo collision >> \"{log}\"\nfi\n",
        lock = lockdir.display(),
        log = collisions.display(),
    );
    fs::write(&probe_script, contents).unwrap();
    let mut perms = fs::metadata(&probe_script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&probe_script, perms).unwrap();

    let noop_a = write_script(&scratch, "noop_a.sh", &scratch.join("noop_a_cwd.txt"), 0);
    let noop_b = write_script(&scratch, "noop_b.sh", &scratch.join("noop_b_cwd.txt"), 0);

    let commit = leak(head_commit());
    let mut task_a = base_task(commit, single_command(&noop_a));
    task_a.id = "T1";
    let mut task_b = base_task(commit, single_command(&noop_b));
    task_b.id = "T2";

    let opts_a = base_opts(scratch.clone(), probe_script.clone());
    let opts_b = base_opts(scratch.clone(), probe_script);
    let harness_a = Harness::new("fake");
    let harness_b = Harness::new("fake");

    let t1 = std::thread::spawn(move || attempt::run_one(&task_a, &harness_a, 1, &opts_a));
    let t2 = std::thread::spawn(move || attempt::run_one(&task_b, &harness_b, 1, &opts_b));

    let r1 = t1.join().unwrap();
    let r2 = t2.join().unwrap();

    assert!(r1.outcome.completed(), "{:?}", r1.outcome);
    assert!(r2.outcome.completed(), "{:?}", r2.outcome);

    let collision_log = fs::read_to_string(&collisions).unwrap();
    assert!(
        collision_log.is_empty(),
        "harness launches overlapped, which would corrupt time-window token attribution: {collision_log}"
    );
}

#[test]
fn the_worktree_is_removed_even_when_the_test_command_fails() {
    let scratch = scratch_dir("removed-on-fail");
    let harness_cwd = scratch.join("harness_cwd.txt");
    let test_cwd = scratch.join("test_cwd.txt");
    let harness_script = write_script(&scratch, "fake_harness.sh", &harness_cwd, 0);
    let failing_test = write_script(&scratch, "failing_test.sh", &test_cwd, 1);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&failing_test));
    let opts = base_opts(scratch.clone(), harness_script);
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);

    assert_eq!(result.outcome, Outcome::Fail);

    let attempt_dir = PathBuf::from(fs::read_to_string(&harness_cwd).unwrap().trim());
    assert!(
        !attempt_dir.exists(),
        "worktree must be removed even when the test fails"
    );
}

#[test]
fn a_two_command_task_fails_when_the_second_command_fails() {
    let scratch = scratch_dir("second-command-fails");
    let harness_cwd = scratch.join("harness_cwd.txt");
    let first_record = scratch.join("first_cwd.txt");
    let second_record = scratch.join("second_cwd.txt");
    let harness_script = write_script(&scratch, "fake_harness.sh", &harness_cwd, 0);
    let first_command = write_script(&scratch, "first.sh", &first_record, 0);
    let second_command = write_script(&scratch, "second.sh", &second_record, 1);

    let commit = leak(head_commit());
    let task = base_task(commit, two_commands(&first_command, &second_command));
    let opts = base_opts(scratch.clone(), harness_script);
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);

    assert_eq!(result.outcome, Outcome::Fail);
    assert!(
        first_record.exists(),
        "the first command should still have run"
    );
}

#[test]
fn the_sterna_row_launches_session_with_the_attempts_root_and_the_statement() {
    let scratch = scratch_dir("sterna-row");
    let argv_record = scratch.join("argv.txt");
    let env_record = scratch.join("env.txt");
    let fake_sterna = write_argv_script(&scratch, "fake_sterna.sh", &argv_record, &env_record, 0);
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);

    let sterna_row = attempt::default_harnesses().remove("sterna").unwrap();
    let mut harnesses = HashMap::new();
    harnesses.insert(
        "sterna".to_string(),
        HarnessCommand {
            program: fake_sterna,
            args: sterna_row.args,
            interface: None,
            decisions: None,
        },
    );

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = RunOpts {
        scratch: scratch.clone(),
        gateway: Some("http://127.0.0.1:8731".to_string()),
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: None,
        helpers_model: None,
    };
    let harness = Harness::new("sterna");

    let result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);

    let argv = read_argv(&argv_record);
    assert_eq!(argv.len(), 5, "the template's own five elements, no more");
    assert_eq!(argv[0], "session");
    assert_eq!(argv[1], "--root");
    let actual_root = PathBuf::from(&argv[2]);
    assert!(
        actual_root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("{}-{}-{}-", task.id, harness.as_str(), 1)),
        "root basename must still carry the task, harness and repeat: {actual_root:?}"
    );
    assert_eq!(argv[3], "--task");
    assert_eq!(argv[4], task.statement);

    let env = fs::read_to_string(&env_record).unwrap();
    assert_eq!(env, "http://127.0.0.1:8731");
}

#[test]
fn the_claude_code_row_still_carries_the_statement_as_a_bare_argument() {
    let scratch = scratch_dir("claude-code-row");
    let argv_record = scratch.join("argv.txt");
    let env_record = scratch.join("env.txt");
    let fake_claude = write_argv_script(&scratch, "fake_claude.sh", &argv_record, &env_record, 0);
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);

    let claude_row = attempt::default_harnesses().remove("claude-code").unwrap();
    let mut harnesses = HashMap::new();
    harnesses.insert(
        "claude-code".to_string(),
        HarnessCommand {
            program: fake_claude,
            args: claude_row.args,
            interface: None,
            decisions: None,
        },
    );

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = RunOpts {
        scratch: scratch.clone(),
        gateway: None,
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: None,
        helpers_model: None,
    };
    let harness = Harness::new("claude-code");

    let result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);

    let argv = read_argv(&argv_record);
    assert_eq!(
        argv,
        vec![
            "--print".to_string(),
            "--dangerously-skip-permissions".to_string(),
            task.statement.to_string(),
        ]
    );
}

#[test]
fn the_codex_row_runs_exec_with_the_bypass_and_the_statement() {
    let scratch = scratch_dir("codex-row");
    let argv_record = scratch.join("argv.txt");
    let env_record = scratch.join("env.txt");
    let fake_codex = write_argv_script(&scratch, "fake_codex.sh", &argv_record, &env_record, 0);
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);

    let codex_row = attempt::default_harnesses().remove("codex").unwrap();
    let mut harnesses = HashMap::new();
    harnesses.insert(
        "codex".to_string(),
        HarnessCommand {
            program: fake_codex,
            args: codex_row.args,
            interface: None,
            decisions: None,
        },
    );

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = RunOpts {
        scratch: scratch.clone(),
        gateway: None,
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: None,
        helpers_model: None,
    };
    let harness = Harness::new("codex");

    let result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);

    let argv = read_argv(&argv_record);
    assert_eq!(
        argv,
        vec![
            "exec".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
            task.statement.to_string(),
        ]
    );

    let actual_root = read_argv_cwd(&argv_record);
    assert!(
        actual_root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!("{}-{}-{}-", task.id, harness.as_str(), 1)),
        "codex takes no --root-equivalent flag; it must still launch in the attempt's own worktree: {actual_root:?}"
    );
}

#[test]
fn a_statement_with_spaces_and_braces_reaches_the_child_as_one_argument() {
    let scratch = scratch_dir("statement-braces");
    let argv_record = scratch.join("argv.txt");
    let env_record = scratch.join("env.txt");
    let fake_sterna = write_argv_script(&scratch, "fake_sterna.sh", &argv_record, &env_record, 0);
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);

    let sterna_row = attempt::default_harnesses().remove("sterna").unwrap();
    let mut harnesses = HashMap::new();
    harnesses.insert(
        "sterna".to_string(),
        HarnessCommand {
            program: fake_sterna,
            args: sterna_row.args,
            interface: None,
            decisions: None,
        },
    );

    let statement: &'static str =
        "split \"main.rs\" into {root} and {statement}, quoted 'like this'";
    let commit = leak(head_commit());
    let mut task = base_task(commit, single_command(&test_script));
    task.statement = statement;
    let opts = RunOpts {
        scratch: scratch.clone(),
        gateway: None,
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: None,
        helpers_model: None,
    };
    let harness = Harness::new("sterna");

    let result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);

    let argv = read_argv(&argv_record);
    assert_eq!(
        argv.last().map(String::as_str),
        Some(statement),
        "the statement must arrive as exactly one argv element, unsplit and unsubstituted"
    );
    assert_eq!(argv.len(), 5, "the template's own five elements, no more");
}

#[test]
fn repeat_below_three_is_refused() {
    let out = scratch_dir("repeat-refused");
    let args = vec![
        "run".to_string(),
        "--task".to_string(),
        "L1".to_string(),
        "--harness".to_string(),
        "claude-code".to_string(),
        "--repeat".to_string(),
        "1".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
    ];

    let result = cli::dispatch(&args);

    let message = result.expect_err("--repeat 1 must be refused");
    assert!(
        message.contains("--repeat"),
        "refusal message should say why: {message}"
    );
}

#[test]
fn the_accepted_flags_are_exactly_these() {
    assert_eq!(
        cli::ACCEPTED_FLAGS.to_vec(),
        vec![
            "--task",
            "--tier",
            "--harness",
            "--repeat",
            "--gateway",
            "--meter",
            "--sterna-interface",
            "--credit-ratio",
            "--sterna-decisions",
            "--decisions-model",
            "--parent-model",
            "--sterna-feedback",
            "--helpers-model",
            "--out"
        ]
    );
}

/// The seam with the gateway, pinned on the producer's own wire shape.
///
/// `meter.rs` reads four keys out of the row `inference-gateway routing-cost
/// --json` prints, and the join between the two crates is nothing but string
/// equality of those key names -- there is no shared type. So the fixture
/// below is the full row the gateway's `cost_row_json` writes, every key,
/// rather than the four-key subset the other tests use. If the producer
/// renames a token column, this fails; without it the meter would silently
/// sum nothing and report an honest-looking absent figure.
///
/// The `null`s are the other half of the contract: an absent column is
/// `null` and never `0`, so the second row here contributes a turn and no
/// tokens.
#[test]
fn the_meter_parses_the_gateways_full_routing_cost_row() {
    let counted = r#"{"provider":"anthropic","model":"claude-opus-5","route":"relay","quota_context":null,"purpose":null,"observed_at":1757100000,"input_tokens":18204,"output_tokens":3311,"cached_input_tokens":140200}"#;
    let uncounted = r#"{"provider":"anthropic","model":"claude-opus-5","route":"relay","quota_context":null,"purpose":null,"observed_at":1757100010,"input_tokens":null,"output_tokens":null,"cached_input_tokens":null}"#;

    let from = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1757099000);
    let to = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1757101000);
    let (tokens, turns) = Readout::for_window([counted, uncounted], from, to);

    assert_eq!(tokens.input, Some(18204));
    assert_eq!(tokens.output, Some(3311));
    assert_eq!(tokens.cached_input, Some(140200));
    assert_eq!(tokens.total(), Some(18204 + 3311 + 140200));
    assert_eq!(
        turns,
        Some(2),
        "both rows are exchanges; only one of them was metered"
    );
}

/// `routing-cost` runs with the attempt's own worktree as its current
/// directory.
#[test]
fn the_meter_reads_routing_cost_from_the_attempts_worktree() {
    let scratch = scratch_dir("meter-cwd-plain");
    let meter_cwd_record = scratch.join("meter_cwd.txt");
    let fake_gateway = write_meter_script(&scratch, "fake_gateway.sh", &meter_cwd_record);
    let harness_cwd = scratch.join("harness_cwd.txt");
    let harness_script = write_script(&scratch, "fake_harness.sh", &harness_cwd, 0);
    let test_cwd = scratch.join("test_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &test_cwd, 0);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let mut opts = base_opts(scratch.clone(), harness_script);
    opts.meter = Meter::Command {
        executable: fake_gateway,
    };
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);

    let expected_dir = PathBuf::from(fs::read_to_string(&harness_cwd).unwrap().trim());
    let recorded = fs::read_to_string(&meter_cwd_record).unwrap();
    assert_eq!(
        PathBuf::from(recorded.trim()),
        expected_dir,
        "the meter must read the attempt's worktree, not the ruler's own cwd"
    );
}

/// Not a standalone acceptance test: a helper this file's other tests
/// re-invoke, filtered by `--exact`, as a genuinely separate OS process.
/// Run directly (the normal full-suite case, `RULER_PROBE_SCRATCH` unset)
/// it is a no-op -- the point is exclusively the isolated re-invocation
/// below, since [`attempt::run_one`]'s attempt-dir counter is process-wide
/// and this file's other tests call it too, so only a fresh process can
/// promise "this is that process's first attempt".
#[test]
fn probe_helper_runs_one_attempt() {
    let Ok(scratch) = std::env::var("RULER_PROBE_SCRATCH") else {
        return;
    };
    let scratch = PathBuf::from(scratch);
    fs::create_dir_all(&scratch).unwrap();
    let cwd_record = scratch.join("cwd_record.txt");
    let harness_script = write_script(&scratch, "harness.sh", &cwd_record, 0);
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop.sh", &noop_cwd, 0);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = base_opts(scratch, harness_script);
    let harness = Harness::new("fake");

    let result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);
}

/// Spawns this test binary as a subprocess restricted, via `--exact`, to
/// exactly [`probe_helper_runs_one_attempt`] -- a genuinely separate
/// process whose very first (and only) call to `run_one` therefore has a
/// process-wide counter starting fresh at 1 -- and returns the worktree
/// directory that call's fake harness recorded its own `pwd` into.
/// Returns the attempt directory the probe process recorded and that
/// process's own pid, so a test can assert the name carries the *real* pid
/// rather than any number -- a constant in the pid's place survived the
/// lead's mutation until this returned it (2026-09-06).
fn run_probe_attempt_in_subprocess(label: &str) -> (PathBuf, u32) {
    let scratch = scratch_dir(label);
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("probe_helper_runs_one_attempt")
        .arg("--exact")
        .arg("--test-threads=1")
        .env("RULER_PROBE_SCRATCH", &scratch)
        .spawn()
        .unwrap();
    let pid = child.id();
    let status = child.wait_with_output().unwrap().status;
    assert!(
        status.success(),
        "probe subprocess must complete an attempt"
    );
    let dir = PathBuf::from(
        fs::read_to_string(scratch.join("cwd_record.txt"))
            .unwrap()
            .trim(),
    );
    (dir, pid)
}

/// Two ruler processes cutting an attempt of the same task, harness and
/// repeat at the same moment must never share a worktree basename --
/// the exact collision behind `.git/worktrees/<basename>/commondir`
/// failing to read while the other process is creating or removing it.
#[test]
fn two_attempts_of_one_row_in_two_processes_never_share_a_worktree_name() {
    let scratch = scratch_dir("two-process-name");
    let direct_cwd = scratch.join("direct_cwd.txt");
    let direct_harness = write_script(&scratch, "direct_harness.sh", &direct_cwd, 0);
    let direct_test = write_script(
        &scratch,
        "direct_test.sh",
        &scratch.join("direct_test.txt"),
        0,
    );

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&direct_test));
    let opts = base_opts(scratch.clone(), direct_harness);
    let harness = Harness::new("fake");

    let direct_result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(
        direct_result.outcome.completed(),
        "{:?}",
        direct_result.outcome
    );
    let direct_dir = PathBuf::from(fs::read_to_string(&direct_cwd).unwrap().trim());

    let (subprocess_dir, _pid) = run_probe_attempt_in_subprocess("two-process-name-subprocess");

    let direct_name = direct_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let subprocess_name = subprocess_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    assert_ne!(
        direct_name, subprocess_name,
        "two processes cutting the same attempt must never share a basename"
    );
    let prefix = "T1-fake-1-";
    assert!(direct_name.starts_with(prefix), "{direct_name}");
    assert!(subprocess_name.starts_with(prefix), "{subprocess_name}");
}

/// One attempt's basename ends `-<pid>-1`: the counter is 1-based and the
/// pid is the process that cut the worktree, not the ruler's own pid at
/// some other point.
#[test]
fn an_attempt_worktree_name_carries_the_pid_and_a_counter() {
    let (subprocess_dir, probe_pid) = run_probe_attempt_in_subprocess("pid-and-counter");
    let name = subprocess_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let suffix = name
        .strip_prefix("T1-fake-1-")
        .unwrap_or_else(|| panic!("expected a T1-fake-1- prefix: {name}"));
    let mut parts = suffix.splitn(2, '-');
    let pid_part = parts.next().unwrap();
    let counter_part = parts
        .next()
        .unwrap_or_else(|| panic!("expected <pid>-<n>: {name}"));

    assert_eq!(
        pid_part,
        probe_pid.to_string(),
        "the pid segment must be the probe process's own pid, not any number: {name}"
    );
    assert_eq!(
        counter_part, "1",
        "the first attempt in a fresh process must end -<pid>-1: {name}"
    );
}

/// Writes an executable shell script that records its NUL-separated argv
/// to `argv_record` and prints `stdout` -- a stand-in for `sterna session
/// --output-format json` whose stdout is the telemetry document.
fn write_stdout_script(dir: &Path, name: &str, argv_record: &Path, stdout: &str) -> PathBuf {
    let path = dir.join(name);
    let contents = format!(
        "#!/bin/sh\nprintf '%s\\0' \"$@\" >> \"{}\"\ncat <<'STERNA_RESULT'\n{}\nSTERNA_RESULT\nexit 0\n",
        argv_record.display(),
        stdout
    );
    fs::write(&path, contents).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// Contract 1: `--sterna-interface hybrid,cells,tools` turns the `sterna` row
/// into three `sterna:<mode>` rows, each carrying `--interface <mode>
/// --output-format json` after the row's own template, and leaves every
/// other row alone.
#[test]
fn sterna_interface_expands_the_sterna_row_into_one_arm_per_mode() {
    let mut table = attempt::default_harnesses();
    let selected = vec!["claude-code".to_string(), "sterna".to_string()];
    let modes = vec![
        "hybrid".to_string(),
        "cells".to_string(),
        "tools".to_string(),
    ];

    let rows = cli::expand_sterna_interfaces(&selected, &modes, &mut table).unwrap();

    assert_eq!(
        rows,
        vec![
            "claude-code",
            "sterna:hybrid",
            "sterna:cells",
            "sterna:tools"
        ]
    );
    for mode in ["hybrid", "cells", "tools"] {
        let arm = &table[&format!("sterna:{mode}")];
        assert_eq!(arm.program, PathBuf::from("sterna"));
        assert_eq!(
            arm.args,
            vec![
                "session",
                "--root",
                "{root}",
                "--rollout",
                "{rollout}",
                "--task",
                "{statement}",
                "--interface",
                mode,
                "--output-format",
                "json"
            ]
        );
        assert_eq!(arm.interface.as_deref(), Some(mode));
    }
    assert_eq!(
        table["claude-code"].args,
        vec!["--print", "--dangerously-skip-permissions", "{statement}"]
    );
    assert!(table["claude-code"].interface.is_none());

    let mut untouched = attempt::default_harnesses();
    assert_eq!(
        cli::expand_sterna_interfaces(&selected, &[], &mut untouched).unwrap(),
        selected,
        "no modes: the selection is returned as it was"
    );
}

/// Contract 1: `--sterna-interface` without `sterna` among the selected rows is
/// refused, before `resolve_tasks` runs or any attempt starts, in one
/// sentence naming the flag; an unknown mode is refused too.
#[test]
fn sterna_interface_without_the_sterna_row_is_refused() {
    let out = scratch_dir("sterna-interface-refused");
    let result = cli::dispatch(&[
        "run".to_string(),
        "--task".to_string(),
        "L1".to_string(),
        "--harness".to_string(),
        "claude-code".to_string(),
        "--sterna-interface".to_string(),
        "hybrid,cells".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
    ]);
    let message = result.expect_err("--sterna-interface without sterna must be refused");
    assert!(message.contains("--sterna-interface"), "{message}");
    assert!(message.contains("sterna"), "{message}");
    assert_eq!(
        message.matches(". ").count(),
        0,
        "one sentence, not a paragraph: {message}"
    );

    let mut table = attempt::default_harnesses();
    let unknown =
        cli::expand_sterna_interfaces(&["sterna".to_string()], &["turbo".to_string()], &mut table)
            .expect_err("an unknown mode must be refused");
    assert!(unknown.contains("turbo"), "{unknown}");
}

/// Contracts 1 and 2, end to end through `run_one`: a `sterna:hybrid` arm's
/// argv carries `--interface hybrid --output-format json`, its stdout lands
/// in the attempt's `sterna-result.json`, and the attempt carries the mode
/// and the telemetry the document reported.
#[test]
fn a_sterna_arm_captures_its_stdout_and_carries_the_metrics() {
    let scratch = scratch_dir("sterna-arm-metrics");
    let argv_record = scratch.join("argv.txt");
    let document = r#"{"type":"result","telemetry":{"wall_time_ms":1234,"tokens":{"parent":{"requests":7,"known_tokens":900},"helpers":{"known_tokens":300}},"interface":{"provider_selected":{"execute_cell_calls":5,"direct_tool_calls":2}},"cells":{"executed":6,"failed":1},"failures":{"by_kind":{"denied":1}},"recovery":{"by_cause":{"repair":{"requests":2}}},"observation":{"bytes_rendered":4096},"completion":{"verified":true}}}"#;
    let fake_sterna = write_stdout_script(&scratch, "fake_sterna.sh", &argv_record, document);
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);

    let mut table = attempt::default_harnesses();
    let rows =
        cli::expand_sterna_interfaces(&["sterna".to_string()], &["hybrid".to_string()], &mut table)
            .unwrap();
    assert_eq!(rows, vec!["sterna:hybrid"]);
    let mut arm = table.remove("sterna:hybrid").unwrap();
    arm.program = fake_sterna;
    let mut harnesses = HashMap::new();
    harnesses.insert("sterna:hybrid".to_string(), arm);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = RunOpts {
        scratch: scratch.clone(),
        gateway: None,
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: None,
        helpers_model: None,
    };
    let harness = Harness::new("sterna:hybrid");

    let result = attempt::run_one(&task, &harness, 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);

    let argv = read_argv(&argv_record);
    assert_eq!(argv[0], "session");
    assert_eq!(
        &argv[5..],
        ["--interface", "hybrid", "--output-format", "json"]
    );

    assert_eq!(result.interface.as_deref(), Some("hybrid"));
    let metrics = result
        .metrics
        .clone()
        .expect("the captured document is parsed");
    assert_eq!(metrics.parent_requests, Some(7));
    assert_eq!(metrics.parent_known_tokens, Some(900));
    assert_eq!(metrics.helper_known_tokens, Some(300));
    assert_eq!(metrics.execute_cell_calls, Some(5));
    assert_eq!(metrics.direct_tool_calls, Some(2));
    assert_eq!(metrics.frames_failed, Some(1));
    assert_eq!(metrics.repair_requests, Some(2));
    assert_eq!(metrics.observation_bytes_rendered, Some(4096));
    assert_eq!(metrics.wall_time_ms, Some(1234));
    assert_eq!(metrics.completion_verified, Some(true));
    assert_eq!(
        metrics.failures_by_kind.unwrap().get("denied").copied(),
        Some(1)
    );

    let jsonl = sterna::ruler::report::render_jsonl(std::slice::from_ref(&result));
    assert!(jsonl.contains("\"interface\":\"hybrid\""), "{jsonl}");
    assert!(jsonl.contains("\"parent_requests\":7"), "{jsonl}");
}

/// A sterna arm whose stdout carries no telemetry document is unmeasured --
/// `metrics: None` -- and still a completed attempt.
#[test]
fn a_sterna_arm_without_a_telemetry_document_is_unmeasured_not_zero() {
    let scratch = scratch_dir("sterna-arm-unmeasured");
    let argv_record = scratch.join("argv.txt");
    let fake_sterna = write_stdout_script(&scratch, "fake_sterna.sh", &argv_record, "not json");
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);

    let mut table = attempt::default_harnesses();
    cli::expand_sterna_interfaces(&["sterna".to_string()], &["cells".to_string()], &mut table)
        .unwrap();
    let mut arm = table.remove("sterna:cells").unwrap();
    arm.program = fake_sterna;
    let mut harnesses = HashMap::new();
    harnesses.insert("sterna:cells".to_string(), arm);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = RunOpts {
        scratch,
        gateway: None,
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: None,
        helpers_model: None,
    };

    let result = attempt::run_one(&task, &Harness::new("sterna:cells"), 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);
    assert_eq!(result.interface.as_deref(), Some("cells"));
    assert_eq!(result.metrics, None);
}

/// Writes an executable shell script that, if `.sterna/config.toml` exists in
/// its own working directory, copies it verbatim to `config_record`, and
/// otherwise leaves `config_record` empty -- a stand-in for `sterna session`
/// that reports back what the ruler wrote into the attempt's worktree before
/// launch.
fn write_config_capture_script(dir: &Path, name: &str, config_record: &Path) -> PathBuf {
    let path = dir.join(name);
    let contents = format!(
        "#!/bin/sh\nif [ -f .sterna/config.toml ]; then cat .sterna/config.toml > \"{record}\"; else : > \"{record}\"; fi\nexit 0\n",
        record = config_record.display(),
    );
    fs::write(&path, contents).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
    path
}

/// Contract 1: `--sterna-decisions off,shadow,on` turns the `sterna` row into
/// three `sterna:decisions-<mode>` rows, each carrying `--output-format json`
/// after the row's own template and leaving every other row alone -- the
/// mode itself never reaches the argv (it travels through the worktree's
/// own config file, contract 2).
#[test]
fn sterna_decisions_expands_the_sterna_row_into_one_arm_per_mode() {
    let mut table = attempt::default_harnesses();
    let selected = vec!["claude-code".to_string(), "sterna".to_string()];
    let modes = vec!["off".to_string(), "shadow".to_string(), "on".to_string()];

    let rows =
        cli::expand_sterna_decisions(&selected, &modes, Some("jev-latest"), &mut table).unwrap();

    assert_eq!(
        rows,
        vec![
            "claude-code".to_string(),
            "sterna:decisions-off".to_string(),
            "sterna:decisions-shadow".to_string(),
            "sterna:decisions-on".to_string(),
        ]
    );
    for mode in ["off", "shadow", "on"] {
        let arm = &table[&format!("sterna:decisions-{mode}")];
        assert_eq!(
            arm.args,
            vec![
                "session",
                "--root",
                "{root}",
                "--rollout",
                "{rollout}",
                "--task",
                "{statement}",
                "--output-format",
                "json",
            ]
        );
        assert!(!arm.args.contains(&"--interface".to_string()));
        assert!(!arm.args.contains(&"--mode".to_string()));
    }

    let mut untouched = attempt::default_harnesses();
    assert_eq!(
        cli::expand_sterna_decisions(&selected, &[], None, &mut untouched).unwrap(),
        selected,
        "no modes: the selection is returned as it was"
    );
}

/// Contract 1: `--sterna-decisions` without `sterna` among the selected rows is
/// refused, before any attempt starts, in one sentence naming the flag.
#[test]
fn sterna_decisions_without_the_sterna_row_is_refused() {
    let out = scratch_dir("sterna-decisions-refused");
    let result = cli::dispatch(&[
        "run".to_string(),
        "--task".to_string(),
        "L1".to_string(),
        "--harness".to_string(),
        "claude-code".to_string(),
        "--sterna-decisions".to_string(),
        "shadow".to_string(),
        "--decisions-model".to_string(),
        "jev-latest".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
    ]);
    let message = result.expect_err("--sterna-decisions without sterna must be refused");
    assert!(message.contains("--sterna-decisions"), "{message}");
    assert!(message.contains("sterna"), "{message}");
    assert_eq!(
        message.matches(". ").count(),
        0,
        "one sentence, not a paragraph: {message}"
    );
}

/// Contract 1: a mode outside `off,shadow,on` is refused, naming the mode.
#[test]
fn sterna_decisions_unknown_mode_is_refused() {
    let mut table = attempt::default_harnesses();
    let err = cli::expand_sterna_decisions(
        &["sterna".to_string()],
        &["turbo".to_string()],
        Some("jev-latest"),
        &mut table,
    )
    .expect_err("an unknown mode must be refused");
    assert!(err.contains("turbo"), "{err}");
}

/// Contract 1: a mode named twice is refused, naming the mode.
#[test]
fn sterna_decisions_names_a_mode_twice_is_refused() {
    let mut table = attempt::default_harnesses();
    let err = cli::expand_sterna_decisions(
        &["sterna".to_string()],
        &["shadow".to_string(), "shadow".to_string()],
        Some("jev-latest"),
        &mut table,
    )
    .expect_err("a mode named twice must be refused");
    assert!(err.contains("shadow"), "{err}");
}

/// Contract 1: `shadow` and `on` are refused without `--decisions-model`;
/// `off` alone needs none.
#[test]
fn sterna_decisions_shadow_and_on_need_a_model_but_off_does_not() {
    let mut table = attempt::default_harnesses();
    let shadow_err = cli::expand_sterna_decisions(
        &["sterna".to_string()],
        &["shadow".to_string()],
        None,
        &mut table,
    )
    .expect_err("shadow without --decisions-model must be refused");
    assert!(shadow_err.contains("--decisions-model"), "{shadow_err}");

    let mut table = attempt::default_harnesses();
    let on_err = cli::expand_sterna_decisions(
        &["sterna".to_string()],
        &["on".to_string()],
        None,
        &mut table,
    )
    .expect_err("on without --decisions-model must be refused");
    assert!(on_err.contains("--decisions-model"), "{on_err}");

    let mut table = attempt::default_harnesses();
    let rows = cli::expand_sterna_decisions(
        &["sterna".to_string()],
        &["off".to_string()],
        None,
        &mut table,
    )
    .expect("off needs no model");
    assert_eq!(rows, vec!["sterna:decisions-off".to_string()]);
}

/// `--sterna-interface` and `--sterna-decisions` cannot be combined: one
/// expansion at a time.
#[test]
fn sterna_interface_and_sterna_decisions_together_is_refused() {
    let out = scratch_dir("sterna-interface-decisions-refused");
    let result = cli::dispatch(&[
        "run".to_string(),
        "--task".to_string(),
        "L1".to_string(),
        "--harness".to_string(),
        "sterna".to_string(),
        "--sterna-interface".to_string(),
        "hybrid".to_string(),
        "--sterna-decisions".to_string(),
        "on".to_string(),
        "--decisions-model".to_string(),
        "jev-latest".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
    ]);
    let message = result.expect_err("combining the two expansions must be refused");
    assert!(message.contains("--sterna-interface"), "{message}");
    assert!(message.contains("--sterna-decisions"), "{message}");
}

/// Contract 2, end to end through `run_one`: the `off` arm's worktree gets
/// no `.sterna/config.toml` at all, while `shadow` and `on` get the exact
/// `[decisions]` table -- written after `cut_worktree` and before the
/// harness launches, so the fake harness (launched inside that worktree)
/// can read back what the ruler wrote before it ran.
#[test]
fn a_decisions_off_arm_gets_no_config_file_while_shadow_and_on_get_the_exact_toml() {
    let scratch = scratch_dir("sterna-decisions-config");
    let mut table = attempt::default_harnesses();
    let rows = cli::expand_sterna_decisions(
        &["sterna".to_string()],
        &["off".to_string(), "shadow".to_string(), "on".to_string()],
        Some("jev-latest"),
        &mut table,
    )
    .unwrap();
    assert_eq!(
        rows,
        vec![
            "sterna:decisions-off".to_string(),
            "sterna:decisions-shadow".to_string(),
            "sterna:decisions-on".to_string(),
        ]
    );

    let commit = leak(head_commit());
    for mode in ["off", "shadow", "on"] {
        let arm_name = format!("sterna:decisions-{mode}");
        let mut arm = table[&arm_name].clone();
        let config_record = scratch.join(format!("{mode}-config.txt"));
        let fake_sterna = write_config_capture_script(
            &scratch,
            &format!("fake_sterna_{mode}.sh"),
            &config_record,
        );
        arm.program = fake_sterna;
        let mut harnesses = HashMap::new();
        harnesses.insert(arm_name.clone(), arm);

        let noop_cwd = scratch.join(format!("{mode}_test_cwd.txt"));
        let test_script = write_script(&scratch, &format!("noop_test_{mode}.sh"), &noop_cwd, 0);
        let task = base_task(commit, single_command(&test_script));
        let opts = RunOpts {
            scratch: scratch.clone(),
            gateway: None,
            meter: Meter::None,
            harnesses,
            rollouts: None,
            parent_model: None,
            helpers_model: None,
        };

        let result = attempt::run_one(&task, &Harness::new(arm_name.as_str()), 1, &opts);
        assert!(result.outcome.completed(), "{mode}: {:?}", result.outcome);
        assert_eq!(result.decisions_mode.as_deref(), Some(mode));

        let content = fs::read_to_string(&config_record).unwrap_or_default();
        match mode {
            "off" => assert_eq!(content, "", "the off arm must get no config file"),
            "shadow" | "on" => assert_eq!(
                content,
                format!("[decisions]\nmodel = \"jev-latest\"\nmode = \"{mode}\"\n")
            ),
            other => unreachable!("{other}"),
        }
    }
}

/// Contract 3: [`DecisionFigures`] reads every documented key, and
/// `decisions` absent from the telemetry document leaves the whole
/// `decisions.*` family `None` rather than reading a missing spare as
/// "not spared".
#[test]
fn the_decision_figures_reader_reads_every_key_and_leaves_decisions_absent_as_none() {
    let full = r#"{"telemetry":{"wall_time_ms":555,"tokens":{"parent":{"known_tokens":900}},"completion":{"verified":true,"findings":["a","b"]},"decisions":{"asked":1,"answered":1,"failed":0,"latency_ms_total":180,"would_hold":2,"holds":1,"overrides":1,"completion":{"noul":0.93,"latency_ms":40,"truncated":false,"finding_added":false,"checker_skipped":"decision 0.93"}}}}"#;
    let figures = DecisionFigures::from_result_json(full).expect("a telemetry document is present");
    assert_eq!(figures.verified, Some(true));
    assert_eq!(figures.findings, Some(2));
    assert_eq!(figures.checker_skipped, Some(true));
    assert_eq!(figures.finding_added, Some(false));
    assert_eq!(figures.holds, Some(1));
    assert_eq!(figures.overrides, Some(1));
    assert_eq!(figures.would_hold, Some(2));
    assert_eq!(figures.asked, Some(1));
    assert_eq!(figures.failed, Some(0));
    assert_eq!(figures.latency_ms_total, Some(180));
    assert_eq!(figures.parent_known_tokens, Some(900));
    assert_eq!(figures.wall_time_ms, Some(555));

    let absent = r#"{"telemetry":{"wall_time_ms":10,"tokens":{"parent":{"known_tokens":5}},"completion":{"verified":false,"findings":[]}}}"#;
    let figures =
        DecisionFigures::from_result_json(absent).expect("a telemetry document is present");
    assert_eq!(figures.verified, Some(false));
    assert_eq!(figures.findings, Some(0));
    assert_eq!(
        figures.checker_skipped, None,
        "no decisions object means never asked, not spared=false"
    );
    assert_eq!(figures.finding_added, None);
    assert_eq!(figures.holds, None);
    assert_eq!(figures.overrides, None);
    assert_eq!(figures.would_hold, None);
    assert_eq!(figures.asked, None);
    assert_eq!(figures.failed, None);
    assert_eq!(figures.latency_ms_total, None);

    assert_eq!(
        DecisionFigures::from_result_json("not json"),
        None,
        "no telemetry document at all is unmeasured, not a figure of nought"
    );
}

fn decision_attempt(
    task: &'static str,
    arm: &str,
    attempt_no: u32,
    decision_figures: Option<DecisionFigures>,
) -> Attempt {
    Attempt {
        task,
        tier: Tier::Leaf,
        harness: Harness::new(arm),
        base_commit: "0000000".to_string(),
        attempt: attempt_no,
        outcome: Outcome::Pass,
        tokens: Tokens::default(),
        wall_clock: std::time::Duration::from_secs(1),
        turns: None,
        changed_lines: None,
        program: None,
        interface: None,
        metrics: None,
        decisions_mode: Some(arm.rsplit('-').next().unwrap().to_string()),
        decision_figures,
        rubric: None,
    }
}

/// Contract 4: the `checker spared` column counts attempts whose completion
/// decision was a confident yes, `Some(true)` -- three attempts with
/// `true, true, false` spare exactly two, not three.
#[test]
fn the_decisions_table_counts_checker_spared_from_three_attempts() {
    let figures = |checker_skipped: Option<bool>| DecisionFigures {
        checker_skipped,
        ..DecisionFigures::default()
    };
    let attempts = vec![
        decision_attempt("L1", "sterna:decisions-on", 1, Some(figures(Some(true)))),
        decision_attempt("L1", "sterna:decisions-on", 2, Some(figures(Some(true)))),
        decision_attempt("L1", "sterna:decisions-on", 3, Some(figures(Some(false)))),
    ];

    let rows = decisions::rows(&attempts);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].checker_spared, 2);
    assert_eq!(rows[0].excluded, 0);

    let table = report::render_decisions_table(&rows);
    assert!(table.contains("-- decisions"), "{table}");
    assert!(
        table.contains("L1  sterna:decisions-on  0/0  0  2  0  0  0  0"),
        "{table}"
    );
}

/// An attempt whose stdout carried no telemetry document is excluded from
/// the decisions table, never read as a zero.
#[test]
fn a_decisions_attempt_without_a_telemetry_document_is_excluded_not_zero() {
    let attempts = vec![decision_attempt("L1", "sterna:decisions-on", 1, None)];
    let rows = decisions::rows(&attempts);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].excluded, 1);
    assert_eq!(rows[0].checker_spared, 0);
    assert_eq!(rows[0].tokens_mean, None);
}

/// Without `--sterna-decisions` the decisions table renders nothing at all.
#[test]
fn no_decisions_arm_renders_no_decisions_table() {
    let attempts: Vec<Attempt> = Vec::new();
    assert_eq!(
        report::render_decisions_table(&decisions::rows(&attempts)),
        ""
    );
}

// --- The two ways an attempt can measure nothing and look like a result ----
//
// A `sterna` row's worktree is cut detached and carries no `.sterna/config.toml`,
// so `sterna session` refuses to start and exits non-zero. The run used to read
// only whether the harness could be *spawned*, so it then ran the task's own
// tests against a tree nothing had touched and scored what those tests said
// at the base commit -- green, for most tasks. Two runs on 2026-09-17 were
// discarded by hand for that shape, which is a thing a person noticed rather
// than a thing the ruler reported.

/// A `sterna` row's attempt carries the parent model into the worktree the
/// session will read it from — the one place a session looks, since no flag
/// on the row's argv names a model.
#[test]
fn a_sterna_attempt_writes_the_parent_model_into_its_own_project_config() {
    let scratch = scratch_dir("sterna-parent-model");
    let argv_record = scratch.join("argv.txt");
    let env_record = scratch.join("env.txt");
    // The harness prints the config it was given, so the assertion is about
    // what the *session* would have read, not about what this test wrote.
    let config_echo = scratch.join("config.txt");
    let fake_sterna = scratch.join("fake_sterna.sh");
    fs::write(
        &fake_sterna,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" >> \"{}\"\nprintf '%s' \"$ANTHROPIC_BASE_URL\" > \"{}\"\ncat .sterna/config.toml > \"{}\"\nexit 0\n",
            argv_record.display(),
            env_record.display(),
            config_echo.display(),
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&fake_sterna).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&fake_sterna, perms).unwrap();

    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);
    let sterna_row = attempt::default_harnesses().remove("sterna").unwrap();
    let mut harnesses = HashMap::new();
    harnesses.insert(
        "sterna".to_string(),
        HarnessCommand {
            program: fake_sterna,
            args: sterna_row.args,
            interface: None,
            decisions: None,
        },
    );

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = RunOpts {
        scratch: scratch.clone(),
        gateway: None,
        meter: Meter::None,
        harnesses,
        rollouts: None,
        parent_model: Some("gpt-5-6-sol".to_string()),
        helpers_model: None,
    };

    let result = attempt::run_one(&task, &Harness::new("sterna"), 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);
    let config = fs::read_to_string(&config_echo).unwrap();
    assert!(
        config.contains("[model]") && config.contains("parent = \"gpt-5-6-sol\""),
        "the session's own config must name the parent model: {config}"
    );
}

/// A row that is not a `sterna` row configures its own model and gets no
/// `.sterna/config.toml` written under it.
#[test]
fn a_foreign_row_gets_no_sterna_config_written_beneath_it() {
    let scratch = scratch_dir("foreign-row-config");
    let launched = scratch.join("launched.txt");
    let harness_script = write_script(&scratch, "fake.sh", &launched, 0);
    let noop_cwd = scratch.join("noop_cwd.txt");
    let test_script = write_script(&scratch, "noop_test.sh", &noop_cwd, 0);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let mut opts = base_opts(scratch.clone(), harness_script);
    opts.parent_model = Some("gpt-5-6-sol".to_string());

    let result = attempt::run_one(&task, &Harness::new("fake"), 1, &opts);
    assert!(result.outcome.completed(), "{:?}", result.outcome);
    let where_it_ran = PathBuf::from(fs::read_to_string(&launched).unwrap().trim());
    assert!(
        !where_it_ran.join(".sterna").exists(),
        "a foreign row's worktree must stay untouched: {where_it_ran:?}"
    );
}

/// The decisive one: a harness that starts, refuses and exits non-zero is an
/// errored attempt, whatever the task's own tests would have said about the
/// tree it never touched.
#[test]
fn a_harness_that_exits_non_zero_is_errored_and_its_tests_are_not_the_answer() {
    let scratch = scratch_dir("refusing-harness");
    let launched = scratch.join("launched.txt");
    // Exit 1, exactly as `sterna session` does when it has no model to run.
    let refusing = write_script(&scratch, "refusing.sh", &launched, 1);
    let passed = scratch.join("passed.txt");
    // A test command that passes on an untouched tree — the shape that turns
    // a refusal into a pass.
    let test_script = write_script(&scratch, "always_passes.sh", &passed, 0);

    let commit = leak(head_commit());
    let task = base_task(commit, single_command(&test_script));
    let opts = base_opts(scratch.clone(), refusing);

    let result = attempt::run_one(&task, &Harness::new("fake"), 1, &opts);
    assert_eq!(
        result.outcome,
        Outcome::Errored,
        "a refusing harness never reached its test command"
    );
    assert!(
        launched.exists(),
        "the harness must really have been launched"
    );
    assert!(
        !passed.exists(),
        "the task's tests must not run once the harness has refused"
    );
}

/// A selected `sterna` row with no `--parent-model` is refused before any
/// attempt runs, rather than producing attempts that measure nothing.
#[test]
fn a_sterna_row_without_a_parent_model_is_refused_before_any_attempt() {
    let out = scratch_dir("sterna-row-no-model");

    let result = cli::dispatch(&[
        "run".to_string(),
        "--task".to_string(),
        "L1".to_string(),
        "--harness".to_string(),
        "sterna".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
    ]);

    let message = result.expect_err("a sterna row with no model must be refused");
    assert!(
        message.contains("--parent-model") && message.contains("sterna"),
        "refusal should name the flag and the row: {message}"
    );
    assert!(
        !out.join("attempts.jsonl").exists(),
        "no attempt may have run"
    );
}

/// The same run with a model passes the flag check — the refusal is about
/// the missing model and nothing else.
#[test]
fn a_foreign_row_alone_needs_no_parent_model() {
    let out = scratch_dir("foreign-row-no-model");

    let result = cli::dispatch(&[
        "run".to_string(),
        "--task".to_string(),
        "no-such-task".to_string(),
        "--harness".to_string(),
        "claude-code".to_string(),
        "--out".to_string(),
        out.to_string_lossy().into_owned(),
    ]);

    // Refused by task resolution, which runs *after* the parent-model check:
    // reaching it at all is the proof that no row here needed a model.
    let message = result.expect_err("an unknown task id is refused");
    assert!(
        message.contains("no-such-task"),
        "the parent-model check must not fire for a foreign row: {message}"
    );
}

/// An explore task is judged by the facts its answer states, and a
/// `--sterna-feedback` arm's worktree carries the decision mode and the
/// `[helpers]` switches of its arm: the `reduce` arm reads `on` with
/// `reduce_returns`, and every sterna row gets the helper model.
#[test]
fn a_rubric_task_is_scored_on_its_answer_and_a_feedback_arm_writes_its_switches() {
    use sterna::ruler::model::Fact;
    let scratch = scratch_dir("feedback-rubric");
    let argv_record = scratch.join("argv.txt");
    // The fake reads its own worktree's config into the answer, so the
    // assertions below see exactly what the attempt was launched with.
    let path = scratch.join("fake_sterna.sh");
    let seen = scratch.join("config.seen");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" >> \"{}\"\ncp .sterna/config.toml \"{}\"\nprintf '%s\\n' '{{\"type\":\"text\",\"text\":\"working\"}}'\nprintf '%s\\n' '{{\"type\":\"result\",\"answer\":\"It cuts a worktree at the parent commit; the meter reads tokens.\"}}'\nexit 0\n",
            argv_record.display(),
            seen.display()
        ),
    )
    .unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();

    let mut table = attempt::default_harnesses();
    let rows = cli::expand_sterna_feedback(
        &["sterna".to_string()],
        &["reduce".to_string()],
        "jev-latest",
        &mut table,
    )
    .unwrap();
    assert_eq!(rows, vec!["sterna:feedback-reduce"]);
    let mut arm = table.remove("sterna:feedback-reduce").unwrap();
    arm.program = path;
    let mut harnesses = HashMap::new();
    harnesses.insert("sterna:feedback-reduce".to_string(), arm);

    const FACTS: &[Fact] = &[
        Fact {
            name: "worktree",
            any: &["worktree"],
        },
        Fact {
            name: "parent",
            any: &["parent commit"],
        },
        Fact {
            name: "meter",
            any: &["meter"],
        },
        Fact {
            name: "suspect",
            any: &["suspect"],
        },
    ];
    let task = Task {
        test: &[],
        rubric: FACTS,
        ..base_task(leak(head_commit()), &[])
    };
    let opts = RunOpts {
        parent_model: Some("gpt-5-6-sol".to_string()),
        helpers_model: Some("gpt-5.6-luna".to_string()),
        ..base_opts(scratch.clone(), PathBuf::from("unused"))
    };
    let opts = RunOpts { harnesses, ..opts };

    let result = attempt::run_one(&task, &Harness::new("sterna:feedback-reduce"), 1, &opts);
    let score = result.rubric.clone().expect("the answer was scored");
    assert_eq!(
        score.found,
        vec!["worktree", "parent", "meter"],
        "{score:?}"
    );
    assert_eq!(score.total, 4);
    assert_eq!(task.rubric_bound(), 3);
    assert!(
        result.outcome.completed(),
        "3 of 4 meets the bound: {:?}",
        result.outcome
    );
    assert_eq!(result.decisions_mode.as_deref(), Some("on"));

    let jsonl = sterna::ruler::report::render_jsonl(std::slice::from_ref(&result));
    assert!(
        jsonl.contains("\"rubric\":{\"found\":[\"worktree\",\"parent\",\"meter\"],\"total\":4}"),
        "{jsonl}"
    );
    let table = sterna::ruler::report::render_rubric_table(std::slice::from_ref(&result));
    assert!(
        table.contains("T1 | sterna:feedback-reduce | 3.0/4 | 1/1"),
        "{table}"
    );

    // The config the attempt was launched with: decisions on with Jev, the
    // reducer switched on, the parent and helper models written.
    let config = fs::read_to_string(&seen).unwrap();
    for expected in [
        "[model]\nparent = \"gpt-5-6-sol\"",
        "[decisions]\nmodel = \"jev-latest\"\nmode = \"on\"",
        "[helpers]\nenabled = true\nmodel = \"gpt-5.6-luna\"\nreduce_returns = true",
    ] {
        assert!(config.contains(expected), "{expected} in {config}");
    }
    let argv = read_argv(&argv_record);
    assert!(
        argv.ends_with(&["--output-format".to_string(), "json".to_string()]),
        "{argv:?}"
    );
}
