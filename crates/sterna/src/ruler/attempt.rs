//! One attempt: cut a worktree at the task's parent commit, launch the
//! harness there, run the task's own test command, meter the exchange, and
//! remove the worktree on every path -- including every failure path.
//!
//! **The invariant that is not negotiable:** neither the harness nor the
//! task's test command ever runs anywhere but the worktree cut for that
//! attempt, and that worktree is always the one that gets removed.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use super::decisions::DecisionFigures;
use super::interface::{self, Metrics};
use super::meter::Meter;
use super::model::{Attempt, Harness, Outcome, Program, RubricScore, Task, Tokens};

/// Serializes every attempt's harness-launch-through-meter-read span,
/// process-wide.
///
/// **Why this exists (a defect, not a hypothetical):** the meter has no
/// session to filter by -- the gateway records no session id for the rows
/// `routing-cost` prints, so nothing ties a row to the attempt that caused it.
/// [`super::meter`] therefore attributes tokens by time window alone. Two
/// attempts whose windows overlap would each see the other's rows (or worse,
/// silently split them), and there would be no signal that anything was
/// wrong -- the sums would just be quietly incorrect. A doc comment saying
/// "call this serially" cannot stop a future refactor from parallelizing the
/// loop in `cli::run`; this lock can.
static ATTEMPT_LOCK: Mutex<()> = Mutex::new(());

/// Process-wide, monotonically increasing, starting at 1: the `<n>` in an
/// attempt worktree's basename (`<task>-<harness>-<repeat>-<pid>-<n>`).
///
/// **Why this exists:** two `sterna ruler` processes on one machine can cut
/// attempt worktrees for the same task, harness and repeat at the same
/// moment -- two `cargo test -p sterna` gates in two worktrees, or two
/// rulers -- and git keys `.git/worktrees/<basename>` by that basename
/// alone, so a shared basename raced `commondir` reads/writes between the
/// two processes (`ruler_run::the_attempt_starts_at_the_parent_commit` red
/// twice in one week). `std::process::id()` alone is not enough: pids are
/// small and OS-recycled, so two rulers started far enough apart can still
/// collide on both task/harness/repeat *and* pid. The counter, unique
/// within this process, closes that gap.
static ATTEMPT_DIR_ORDINAL: AtomicU64 = AtomicU64::new(0);

fn next_attempt_dir_ordinal() -> u64 {
    ATTEMPT_DIR_ORDINAL.fetch_add(1, Ordering::Relaxed) + 1
}

/// One harness's executable and the argv template it is launched with.
/// `"{root}"` and `"{statement}"` are substituted by [`run_attempt_in`] when
/// an argv element is *exactly* one of those two strings -- never as a
/// substring, and never inside the statement's own text, so a statement that
/// happens to contain `{root}` or `{statement}` verbatim changes nothing
/// about what is launched. A second harness is a row here, never a branch
/// inside [`run_one`].
#[derive(Debug, Clone)]
pub struct HarnessCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Set on a `sterna:<mode>` ablation arm: the mode its argv already
    /// carries as `--interface <mode>`. Such a row's stdout is captured to
    /// [`interface::RESULT_FILE`] in the attempt's worktree and parsed into
    /// [`Attempt::metrics`]; every other row's stdout is left alone.
    pub interface: Option<String>,
    /// Set on a `sterna:decisions-<mode>` arm: the model and mode that get
    /// written into the attempt's own `<worktree>/.sterna/config.toml` right
    /// after the worktree is cut, never as a session flag -- the mode
    /// travels only through that file (`decision-model.md` §1). `model` is
    /// `None` for the `off` mode: the arm gets no `[decisions]` table at
    /// all, since unset model already means off.
    pub decisions: Option<DecisionsArm>,
}

/// What a `sterna:decisions-<mode>` arm writes into its worktree's
/// `.sterna/config.toml`.
#[derive(Debug, Clone)]
pub struct DecisionsArm {
    pub model: Option<String>,
    pub mode: String,
    /// Configuration lines the arm writes verbatim under its `[decisions]`
    /// table, which they continue until one opens a table of its own;
    /// empty for a decisions arm.
    pub lines: &'static [&'static str],
}

impl HarnessCommand {
    /// The `sterna` row expanded into one `--interface <mode>` arm, launched
    /// with `--output-format json` so its stdout is the telemetry document.
    /// The `sterna` row's own template is what gets extended, never
    /// re-spelled here.
    pub fn sterna_interface_arm(sterna: &HarnessCommand, mode: &str) -> HarnessCommand {
        let mut args = sterna.args.clone();
        args.extend([
            "--interface".to_string(),
            mode.to_string(),
            "--output-format".to_string(),
            "json".to_string(),
        ]);
        HarnessCommand {
            program: sterna.program.clone(),
            args,
            interface: Some(mode.to_string()),
            decisions: None,
        }
    }

    /// The `sterna` row expanded into one `sterna:decisions-<mode>` arm, launched
    /// with `--output-format json` so its stdout is the telemetry document --
    /// never `--interface`/`--mode`, since the decision mode travels only
    /// through the attempt's own `.sterna/config.toml`. `model` is `None` for
    /// the `off` mode.
    pub fn sterna_decisions_arm(
        sterna: &HarnessCommand,
        model: Option<&str>,
        mode: &str,
    ) -> HarnessCommand {
        let mut args = sterna.args.clone();
        args.extend(["--output-format".to_string(), "json".to_string()]);
        HarnessCommand {
            program: sterna.program.clone(),
            args,
            interface: None,
            decisions: Some(DecisionsArm {
                model: model.map(str::to_string),
                mode: mode.to_string(),
                lines: &[],
            }),
        }
    }

    /// The `sterna` row expanded into one `sterna:feedback-<arm>` arm: a
    /// decisions arm whose `.sterna/config.toml` also carries the lines `arm`
    /// names ([`FEEDBACK_ARMS`]).
    pub fn sterna_feedback_arm(
        sterna: &HarnessCommand,
        model: &str,
        arm: &FeedbackArm,
    ) -> HarnessCommand {
        let mut command = Self::sterna_decisions_arm(sterna, Some(model), arm.mode);
        if let Some(decisions) = command.decisions.as_mut() {
            decisions.lines = arm.lines;
        }
        command
    }
}

/// One `--sterna-feedback` arm: the decision mode and the configuration
/// lines it runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedbackArm {
    pub name: &'static str,
    pub mode: &'static str,
    pub lines: &'static [&'static str],
}

/// The feedback arms: `bare` asks Jev nothing; `shadow` asks every question
/// and acts on none; `on` acts on the answers (a hold, the request's kind
/// lowering the effort); `unreduced` is `on` with returned logs left whole.
/// `working`, `outline`, `turns`, `guided` and `nudge` are `shadow` plus one
/// engine change each: the newest two results kept whole, long instruction
/// documents outlined, the turn-economy line, the prompting guides' autonomy
/// and scope blocks, the batch nudge. `low` is `shadow` at low effort.
pub const FEEDBACK_ARMS: [FeedbackArm; 10] = [
    FeedbackArm {
        name: "bare",
        mode: "off",
        lines: &[],
    },
    FeedbackArm {
        name: "shadow",
        mode: "shadow",
        lines: &[],
    },
    FeedbackArm {
        name: "on",
        mode: "on",
        lines: &[],
    },
    FeedbackArm {
        name: "unreduced",
        mode: "on",
        lines: &["reduce_returns = false"],
    },
    FeedbackArm {
        name: "working",
        mode: "shadow",
        lines: &["[limits]", "keep_results = 2"],
    },
    FeedbackArm {
        name: "outline",
        mode: "shadow",
        lines: &["[limits]", "instructions_outline = true"],
    },
    FeedbackArm {
        name: "turns",
        mode: "shadow",
        lines: &["[limits]", "turn_economy = true"],
    },
    FeedbackArm {
        name: "guided",
        mode: "shadow",
        lines: &["[limits]", "autonomy_block = true", "scope_block = true"],
    },
    FeedbackArm {
        name: "nudge",
        mode: "shadow",
        lines: &["[limits]", "batch_nudge = true"],
    },
    FeedbackArm {
        name: "low",
        mode: "shadow",
        lines: &["[session]", "effort = \"low\""],
    },
];

/// The `--harness` row name of the `sterna` row's `feedback-<arm>` arm.
pub fn feedback_arm_name(arm: &str) -> String {
    format!("sterna:feedback-{arm}")
}

/// The `--harness` row name of the `sterna` row's `<mode>` arm.
pub fn sterna_arm_name(mode: &str) -> String {
    format!("sterna:{mode}")
}

/// The `--harness` row name of the `sterna` row's `decisions-<mode>` arm.
pub fn decisions_arm_name(mode: &str) -> String {
    format!("sterna:decisions-{mode}")
}

/// The table's three production rows. Claude Code is run non-interactively
/// and without permission prompts, exactly as `ruler.md` §4 specifies it;
/// `sterna` is run as `sterna session --root <the attempt's worktree> --task
/// <the statement>`, `session.rs`'s non-interactive, single-shot entry point;
/// `codex` is run as `codex exec --dangerously-bypass-approvals-and-sandbox
/// <the statement>` in the attempt's own `current_dir` (Codex takes no
/// `--root`-equivalent flag; it works in its launch directory).
pub fn default_harnesses() -> HashMap<String, HarnessCommand> {
    let mut table = HashMap::new();
    table.insert(
        "claude-code".to_string(),
        HarnessCommand {
            program: PathBuf::from("claude"),
            args: vec![
                "--print".to_string(),
                "--dangerously-skip-permissions".to_string(),
                "{statement}".to_string(),
            ],
            interface: None,
            decisions: None,
        },
    );
    table.insert(
        "sterna".to_string(),
        HarnessCommand {
            program: PathBuf::from("sterna"),
            args: vec![
                "session".to_string(),
                "--root".to_string(),
                "{root}".to_string(),
                "--rollout".to_string(),
                "{rollout}".to_string(),
                "--task".to_string(),
                "{statement}".to_string(),
            ],
            interface: None,
            decisions: None,
        },
    );
    table.insert(
        "codex".to_string(),
        HarnessCommand {
            program: PathBuf::from("codex"),
            args: vec![
                "exec".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
                "{statement}".to_string(),
            ],
            interface: None,
            decisions: None,
        },
    );
    table
}

/// What one call to [`run_one`] needs beyond the task and the harness.
pub struct RunOpts {
    /// Parent directory for attempt worktrees. Refused if it resolves inside
    /// this process's own checkout.
    pub scratch: PathBuf,
    /// Passed to the harness child as `ANTHROPIC_BASE_URL` when set.
    pub gateway: Option<String>,
    pub meter: Meter,
    pub harnesses: HashMap<String, HarnessCommand>,
    /// Where an attempt's own rollout is kept, so it outlives the worktree
    /// that produced it -- `cli::run` passes `--out`, and the directory
    /// exists before the first attempt runs.
    ///
    /// **Without it the headline figure cannot be computed at all.** The
    /// attempt's `.sterna/sessions/*.jsonl` dies with `remove_worktree`, and a
    /// row's cells and calls are what say whether a cell was used as a
    /// program. `None` drops the `--rollout` pair from the argv and leaves
    /// every attempt's `program` unstated, which is what a harness row that
    /// takes no such flag already does.
    pub rollouts: Option<PathBuf>,
    /// The model a `sterna` row's session is run with, written into the
    /// attempt's own `<worktree>/.sterna/config.toml` as `[model] parent`
    /// before the harness launches.
    ///
    /// **Without it a `sterna` row cannot run at all.** An attempt's worktree
    /// is cut detached from the task's commit and carries no configuration
    /// of the developer's, so `sterna session` refuses to start with *no
    /// parent model selected*, exits non-zero, and changes nothing --
    /// whereupon the task's own tests run against an untouched tree and
    /// report whatever they reported before the benchmark existed. Two runs
    /// on 2026-09-17 were discarded by hand for that shape; `cli::run` now
    /// refuses a selected `sterna` row without this, and a non-zero harness
    /// exit is `Errored` rather than scored. This stays an `Option` because
    /// the other rows carry their own configuration.
    pub parent_model: Option<String>,
}

/// Runs one attempt of `task` by `harness` and returns its record.
///
/// Wall-clock is measured from the harness launch to the test command
/// exiting, not from "the first request leaving the gateway"
/// (`ruler.md` §3's stricter definition) -- that instant is not observable
/// from here without the exchange-row producer named in the packet's
/// limits, so this measures the widest interval it can actually see.
pub fn run_one(task: &Task, harness: &Harness, attempt_no: u32, opts: &RunOpts) -> Attempt {
    let errored = || Attempt {
        task: task.id,
        tier: task.tier,
        harness: harness.clone(),
        base_commit: String::new(),
        attempt: attempt_no,
        outcome: Outcome::Errored,
        tokens: Tokens::default(),
        wall_clock: Duration::default(),
        turns: None,
        changed_lines: None,
        program: None,
        interface: None,
        metrics: None,
        decisions_mode: None,
        decision_figures: None,
        rubric: None,
    };

    if (task.test.is_empty() && task.rubric.is_empty()) || scratch_inside_checkout(&opts.scratch) {
        return errored();
    }

    // A `sterna:<mode>` arm's colon is not a legal Windows path character.
    let dir = opts.scratch.join(format!(
        "{}-{}-{}-{}-{}",
        task.id,
        harness.as_str().replace(':', "-"),
        attempt_no,
        std::process::id(),
        next_attempt_dir_ordinal(),
    ));
    if dir.exists() {
        return errored();
    }

    if !cut_worktree(&dir, task.commit) {
        return errored();
    }

    let attempt = {
        let _serial = ATTEMPT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        run_attempt_in(&dir, task, harness, attempt_no, opts)
    };
    remove_worktree(&dir);
    attempt
}

fn run_attempt_in(
    dir: &Path,
    task: &Task,
    harness: &Harness,
    attempt_no: u32,
    opts: &RunOpts,
) -> Attempt {
    let base_commit = git_rev_parse(dir, "HEAD").unwrap_or_default();

    let interface = opts
        .harnesses
        .get(harness.as_str())
        .and_then(|command| command.interface.clone());
    let decisions_mode = opts
        .harnesses
        .get(harness.as_str())
        .and_then(|command| command.decisions.as_ref().map(|arm| arm.mode.clone()));
    let finish =
        |outcome, tokens, wall_clock, turns, changed_lines, metrics, decision_figures| Attempt {
            task: task.id,
            tier: task.tier,
            harness: harness.clone(),
            base_commit: base_commit.clone(),
            attempt: attempt_no,
            outcome,
            tokens,
            wall_clock,
            turns,
            changed_lines,
            program: None,
            interface: interface.clone(),
            metrics,
            decisions_mode: decisions_mode.clone(),
            decision_figures,
            rubric: None,
        };

    let Some(command) = opts.harnesses.get(harness.as_str()) else {
        return finish(
            Outcome::Errored,
            Tokens::default(),
            Duration::default(),
            None,
            None,
            None,
            None,
        );
    };

    let sterna_row = is_sterna_row(harness.as_str());
    if let Err(_message) = write_sterna_config(
        dir,
        sterna_row.then_some(opts.parent_model.as_deref()).flatten(),
        command.decisions.as_ref(),
    ) {
        return finish(
            Outcome::Errored,
            Tokens::default(),
            Duration::default(),
            None,
            None,
            None,
            None,
        );
    }

    let mut launch = Command::new(&command.program);
    let rollout = opts.rollouts.as_ref().map(|out| {
        out.join(format!(
            "{}-{}-{}.jsonl",
            task.id,
            harness.as_str().replace(':', "-"),
            attempt_no
        ))
    });
    for arg in substituted_args(&command.args, dir, task.statement, rollout.as_deref()) {
        launch.arg(arg);
    }
    launch.current_dir(dir);
    if let Some(gateway) = &opts.gateway {
        launch.env("ANTHROPIC_BASE_URL", gateway);
    }
    // A rubric task is judged by the answer, so its stdout is always kept.
    let result_file =
        (command.interface.is_some() || command.decisions.is_some() || !task.rubric.is_empty())
            .then(|| dir.join(interface::RESULT_FILE));
    if let Some(path) = &result_file {
        match fs::File::create(path) {
            Ok(file) => {
                launch.stdout(file);
            }
            Err(_) => {
                return finish(
                    Outcome::Errored,
                    Tokens::default(),
                    Duration::default(),
                    None,
                    None,
                    None,
                    None,
                );
            }
        }
    }

    let start_wall = Instant::now();
    let start_time = SystemTime::now();

    // **A harness that exits non-zero never ran the task, and its tests are
    // not the benchmark's answer.** This read `is_err()` alone, which catches
    // only a harness that could not be spawned; a harness that started,
    // refused, and exited 1 looked like a harness that had worked. The tests
    // then ran against a worktree nothing had touched and reported the
    // task's own pre-existing state -- `Pass` for every task whose tests are
    // green at its base commit. `Errored` is exactly the right word for it:
    // the attempt never reached its test command, and `score.rs` counts it
    // in no denominator. Sterna itself exits non-zero only when the session
    // refuses to start, never because a task went unfinished, so nothing
    // real is lost to this rule.
    match launch.status() {
        Ok(status) if status.success() => {}
        _ => {
            return finish(
                Outcome::Errored,
                Tokens::default(),
                start_wall.elapsed(),
                None,
                None,
                None,
                None,
            );
        }
    }
    let metrics = result_file.as_deref().and_then(read_metrics);
    let decision_figures = result_file.as_deref().and_then(read_decision_figures);

    if !task.rubric.is_empty() {
        let answer = result_file
            .as_deref()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| answer_of(&text));
        let wall_clock = start_wall.elapsed();
        let (tokens, turns) = opts.meter.read(dir, start_time, SystemTime::now());
        let score = answer.map(|answer| RubricScore::of(task.rubric, &answer));
        let outcome = match &score {
            Some(score) if score.count() >= task.rubric_bound() => Outcome::Pass,
            Some(_) => Outcome::Fail,
            None => Outcome::Errored,
        };
        let mut attempt = finish(
            outcome,
            tokens,
            wall_clock,
            turns,
            diff_shortstat(dir),
            metrics,
            decision_figures,
        );
        attempt.rubric = score;
        attempt.program = rollout.as_deref().and_then(read_program);
        return attempt;
    }

    let test_result = run_test_commands(dir, task.test);

    let end_time = SystemTime::now();
    let wall_clock = start_wall.elapsed();
    let (tokens, turns) = opts.meter.read(dir, start_time, end_time);
    let changed_lines = diff_shortstat(dir);

    let outcome = match test_result {
        TestResult::Passed => {
            let bound = task.suspect_bound();
            let changed = changed_lines.unwrap_or(0);
            if changed < bound {
                Outcome::PassSuspect {
                    changed_lines: changed,
                    bound,
                }
            } else {
                Outcome::Pass
            }
        }
        TestResult::Failed => Outcome::Fail,
        TestResult::Errored => Outcome::Errored,
    };

    let mut attempt = finish(
        outcome,
        tokens,
        wall_clock,
        turns,
        changed_lines,
        metrics,
        decision_figures,
    );
    // Only this path can carry one: every early return above left before the
    // harness wrote a cell, so an absent figure there is the truth rather
    // than a gap.
    attempt.program = rollout.as_deref().and_then(read_program);
    attempt
}

/// Whether `row` is the `sterna` row or one of its arms — the rows launched
/// as the `sterna` binary, and so the rows that need a parent model written.
pub fn is_sterna_row(row: &str) -> bool {
    row.split_once(':').map_or(row, |(base, _)| base) == "sterna"
}

/// Writes `<dir>/.sterna/config.toml` — `[model] parent` for a `sterna` row and
/// `[decisions]` for a `sterna:decisions-<mode>` arm — right after the
/// worktree is cut and before the harness launches. Neither table ever
/// travels as a session flag: both are what a person's own project
/// configuration would carry, which is the thing being measured.
///
/// **One writer for one file.** These were two, and the second silently won:
/// the decisions arm wrote `[decisions]` into a file with no `[model]` table
/// in it, and every attempt of it refused to start.
///
/// The `off` decision mode contributes nothing -- unset model already means
/// off, so there is no table to write. If the task's own commit already
/// carries a `.sterna/config.toml`, the tables are appended to it; a file that
/// already has one of them refuses rather than guess which wins.
fn write_sterna_config(
    dir: &Path,
    parent_model: Option<&str>,
    arm: Option<&DecisionsArm>,
) -> Result<(), String> {
    let decisions_model = arm.and_then(|arm| arm.model.as_deref());
    if parent_model.is_none() && decisions_model.is_none() {
        return Ok(());
    }
    let config_dir = dir.join(".sterna");
    let config_path = config_dir.join("config.toml");
    let existing = fs::read_to_string(&config_path).unwrap_or_default();
    for (table, wanted) in [
        ("[model]", parent_model.is_some()),
        ("[decisions]", decisions_model.is_some()),
    ] {
        if wanted && existing.contains(table) {
            return Err(format!(
                "{} already has a {table} table",
                config_path.display()
            ));
        }
    }
    fs::create_dir_all(&config_dir)
        .map_err(|e| format!("could not create {}: {e}", config_dir.display()))?;
    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if let Some(model) = parent_model {
        content.push_str(&format!("[model]\nparent = \"{model}\"\n"));
    }
    if let Some(model) = decisions_model {
        content.push_str(&format!(
            "[decisions]\nmodel = \"{model}\"\nmode = \"{}\"\n",
            arm.map_or("", |arm| arm.mode.as_str())
        ));
        for line in arm.map_or(&[][..], |arm| arm.lines) {
            content.push_str(line);
            content.push('\n');
        }
    }
    fs::write(&config_path, content)
        .map_err(|e| format!("could not write {}: {e}", config_path.display()))
}

/// The answer a harness gave, from its captured stdout: the `answer` of
/// Sterna's last `result` line when there is one, else the whole text -- a
/// harness that prints its answer plainly is read as it printed it.
fn answer_of(stdout: &str) -> String {
    stdout
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["type"] == "result")
        .and_then(|value| value["answer"].as_str().map(str::to_string))
        .unwrap_or_else(|| stdout.to_string())
}

/// The argv for one attempt: the row's template with `{root}`, `{statement}`
/// and `{rollout}` filled in.
///
/// **A `--rollout {rollout}` pair with no path to fill it is dropped whole**,
/// flag and placeholder together, rather than handed to the harness as a
/// literal `{rollout}` it would try to open. That is why this is a loop over
/// indices and not a `map`: dropping a placeholder means dropping the flag
/// that introduced it, which the element in hand cannot know on its own.
fn substituted_args(
    args: &[String],
    dir: &Path,
    statement: &str,
    rollout: Option<&Path>,
) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "{root}" => out.push(dir.as_os_str().to_os_string()),
            "{statement}" => out.push(std::ffi::OsString::from(statement)),
            "{rollout}" => {
                if let Some(path) = rollout {
                    out.push(path.as_os_str().to_os_string());
                } else {
                    // The flag that introduced it goes with it.
                    out.pop();
                }
            }
            other => out.push(std::ffi::OsString::from(other)),
        }
        index += 1;
    }
    out
}

/// The bare runtime tools a cell calls by name. `decide.choice` is counted
/// separately in [`count_calls`], since it is reached through a receiver.
const CELL_TOOLS: [&str; 10] = [
    "read", "rg", "grep", "glob", "context", "edit", "write", "bash", "fd", "jq",
];

/// How many tool calls one cell's source makes.
///
/// An identifier counts when it is one of [`CELL_TOOLS`], stands on its own
/// rather than after a `.`, and is followed by `(`; or when it is
/// `decide.choice`. A method of the same name on
/// something else -- `results.read(...)` -- is not a tool call and is not
/// counted.
fn count_calls(source: &str) -> u32 {
    let bytes = source.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut calls = 0;
    let mut index = 0;
    while index < bytes.len() {
        if !is_ident(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_ident(bytes[index]) {
            index += 1;
        }
        if start > 0 && is_ident(bytes[start - 1]) {
            continue;
        }
        let name = &source[start..index];
        // The next non-space character decides whether this is a call at all.
        let mut after = index;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if after >= bytes.len() || bytes[after] != b'(' {
            continue;
        }
        let dotted = start > 0 && bytes[start - 1] == b'.';
        if dotted {
            let mut receiver_end = start - 1;
            while receiver_end > 0 && bytes[receiver_end - 1].is_ascii_whitespace() {
                receiver_end -= 1;
            }
            let mut receiver_start = receiver_end;
            while receiver_start > 0 && is_ident(bytes[receiver_start - 1]) {
                receiver_start -= 1;
            }
            if &source[receiver_start..receiver_end] == "decide" && name == "choice" {
                calls += 1;
            }
        } else if CELL_TOOLS.contains(&name) {
            calls += 1;
        }
    }
    calls
}

/// What one kept rollout says about the cells the attempt ran, or `None`
/// when it was never kept or cannot be read -- unmeasured, never a zero, and
/// never an inference from the record's absence.
fn read_program(path: &Path) -> Option<Program> {
    let text = fs::read_to_string(path).ok()?;
    let mut program = Program::default();
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("kind").and_then(serde_json::Value::as_str) != Some("cell") {
            continue;
        }
        program.cells += 1;
        if let Some(source) = value.get("source").and_then(serde_json::Value::as_str) {
            program.calls += count_calls(source);
        }
    }
    (program.cells > 0).then_some(program)
}

/// The captured telemetry document, or `None` when the file is unreadable
/// or carries no `telemetry` -- unmeasured, never an empty measurement.
fn read_metrics(path: &Path) -> Option<Metrics> {
    Metrics::from_result_json(&fs::read_to_string(path).ok()?)
}

/// The captured telemetry document's decision figures, or `None` when the
/// file is unreadable or carries no `telemetry` -- unmeasured, never an
/// empty measurement.
fn read_decision_figures(path: &Path) -> Option<DecisionFigures> {
    DecisionFigures::from_result_json(&fs::read_to_string(path).ok()?)
}

enum TestResult {
    Passed,
    Failed,
    Errored,
}

/// Runs every command in `commands` in order in `dir`, stopping at the first
/// one that does not exit 0. The task completes only if every command in it
/// exits 0 -- one non-zero exit is [`TestResult::Failed`] regardless of how
/// many commands came before it.
fn run_test_commands(dir: &Path, commands: &[&[&str]]) -> TestResult {
    for command in commands {
        let Some((program, args)) = command.split_first() else {
            continue;
        };
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd.current_dir(dir);
        match cmd.status() {
            Ok(status) if status.success() => continue,
            Ok(_) => return TestResult::Failed,
            Err(_) => return TestResult::Errored,
        }
    }
    TestResult::Passed
}

/// Refuses a scratch directory that resolves inside this process's own
/// working directory -- best-effort: a scratch path that does not exist yet
/// is compared lexically rather than canonically, since there is nothing on
/// disk yet to canonicalize.
fn scratch_inside_checkout(scratch: &Path) -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return false;
    };
    let cwd = cwd.canonicalize().unwrap_or(cwd);
    let scratch_abs = scratch
        .canonicalize()
        .unwrap_or_else(|_| scratch.to_path_buf());
    scratch_abs.starts_with(&cwd)
}

fn cut_worktree(dir: &Path, commit: &str) -> bool {
    Command::new("git")
        .arg("worktree")
        .arg("add")
        .arg("--detach")
        .arg(dir)
        .arg(format!("{commit}^"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn remove_worktree(dir: &Path) {
    let _ = Command::new("git")
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(dir)
        .status();
}

fn git_rev_parse(dir: &Path, rev: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("rev-parse")
        .arg(rev)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// `git diff --shortstat`'s insertions plus deletions, or `None` if the
/// command itself could not be read.
fn diff_shortstat(dir: &Path) -> Option<u32> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("diff")
        .arg("--shortstat")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_shortstat(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_shortstat(text: &str) -> u32 {
    text.trim()
        .split(',')
        .filter(|part| part.contains("insertion") || part.contains("deletion"))
        .filter_map(|part| {
            part.trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .ok()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_three_harness_rows_are_pinned_by_their_argv_template() {
        let table = default_harnesses();

        let claude_code = &table["claude-code"];
        assert_eq!(
            claude_code.args,
            vec!["--print", "--dangerously-skip-permissions", "{statement}"]
        );

        let sterna = &table["sterna"];
        assert_eq!(
            sterna.args,
            vec![
                "session",
                "--root",
                "{root}",
                "--rollout",
                "{rollout}",
                "--task",
                "{statement}"
            ]
        );

        let codex = &table["codex"];
        assert_eq!(
            codex.args,
            vec![
                "exec",
                "--dangerously-bypass-approvals-and-sandbox",
                "{statement}"
            ]
        );
    }

    #[test]
    fn a_rollout_with_nowhere_to_go_drops_its_flag_with_it() {
        let args: Vec<String> = default_harnesses()["sterna"].args.clone();
        let root = Path::new("/tmp/attempt-root");

        let kept = substituted_args(
            &args,
            root,
            "do the thing",
            Some(Path::new("/out/S1.jsonl")),
        );
        assert_eq!(
            kept,
            vec![
                "session",
                "--root",
                "/tmp/attempt-root",
                "--rollout",
                "/out/S1.jsonl",
                "--task",
                "do the thing"
            ]
        );

        // Not a literal `{rollout}` and not a bare `--rollout` with the
        // statement swallowed as its value: the pair goes together.
        let dropped = substituted_args(&args, root, "do the thing", None);
        assert_eq!(
            dropped,
            vec![
                "session",
                "--root",
                "/tmp/attempt-root",
                "--task",
                "do the thing"
            ]
        );
    }

    #[test]
    fn a_cells_calls_are_counted_by_name_and_by_receiver() {
        // Every shape that counts, and three that must not: a method of the
        // same name on a value, an identifier that merely starts with a tool
        // name, and a mention that is not a call.
        let source = "const a = await read({path: \"x\"});\n\
             const b = await rg({pattern: \"y\"});\n\
             const c = context({path: \"z\", symbol: \"S\"});\n\
             const e = await decide.choice(\"q\", {a: \"1\", b: \"2\"});\n\
             const f = results.read(0);\n\
             const g = readme(1);\n\
             const h = \"bash is a word here\";\n";
        assert_eq!(count_calls(source), 4);
    }

    #[test]
    fn a_cell_that_calls_nothing_is_a_cell_all_the_same() {
        // Twenty of the 120 cells in the session of 2026-09-17 made no call
        // at all; they are exactly what drags calls-per-cell down, so they
        // must count in the denominator.
        assert_eq!(count_calls("return {done: true};"), 0);
    }

    #[test]
    fn an_unreadable_rollout_is_unstated_rather_than_a_cellless_attempt() {
        assert_eq!(read_program(Path::new("/nonexistent/attempt.jsonl")), None);
    }

    #[test]
    fn a_kept_rollout_reports_its_cells_and_their_calls() {
        let dir = std::env::temp_dir().join(format!(
            "ruler-program-{}-{}",
            std::process::id(),
            next_attempt_dir_ordinal()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("S1-sterna-1.jsonl");
        fs::write(
            &path,
            "{\"kind\":\"system\",\"text\":\"read( in the preamble is not a cell\"}\n\
             {\"kind\":\"cell\",\"cell\":1,\"source\":\"const a = await read({path: 'a'}); const b = await rg({pattern: 'b'});\"}\n\
             {\"kind\":\"turn\",\"role\":\"assistant\",\"text\":\"prose\"}\n\
             {\"kind\":\"cell\",\"cell\":2,\"source\":\"return 1;\"}\n\
             not json at all\n",
        )
        .expect("write rollout");

        let program = read_program(&path).expect("a kept rollout carries a figure");
        assert_eq!(program.cells, 2);
        assert_eq!(program.calls, 2);
        // One call in one cell and none in the other: the empty cell is in
        // the denominator, so this is 1.00 and not 2.00.
        assert_eq!(program.calls_per_cell(), Some(1.0));

        fs::remove_dir_all(&dir).ok();
    }
}
