//! `sterna ruler run`: parse the command, resolve the task and harness sets,
//! run every (task, harness, attempt) combination, print the per-tier table
//! and write one JSON line per attempt.
//!
//! **Both renderings are [`super::report`]'s and neither is spelled here.**
//! This module briefly carried its own `Serialize` record, which meant the
//! column set [`super::report::JSONL_KEYS`] pins -- map line 2432's whole
//! enforcement -- guarded a renderer the command never called, and the two
//! spellings had already drifted on four keys.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Parser;

use super::attempt::{self, HarnessCommand, RunOpts};
use super::decisions;
use super::interface::CreditRatios;
use super::meter::Meter;
use super::model::{Attempt, Harness, Task, Tier};
use super::report;
use super::score::Score;
use super::tasks;

/// The whole accepted flag set for `sterna ruler run` (map line 2432: there is
/// no flag that produces a tokens-per-turn figure -- `Attempt::turns` is
/// carried and printed, never divided into).
pub const ACCEPTED_FLAGS: &[&str] = &[
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
    "--out",
];

/// The three decision modes `--sterna-decisions` accepts, exactly
/// `decision-model.md` §1's `mode` values.
const DECISIONS_MODES: [&str; 3] = ["off", "shadow", "on"];

/// A single attempt of an agent task measures the sample, not the harness --
/// this is the minimum `--repeat` may be, and the default.
pub const MIN_REPEAT: u32 = 3;

#[derive(Parser, Debug)]
#[command(name = "sterna ruler run")]
pub struct RunArgs {
    #[arg(long)]
    pub task: Vec<String>,
    #[arg(long)]
    pub tier: Option<String>,
    #[arg(long)]
    pub harness: Vec<String>,
    #[arg(long, default_value_t = MIN_REPEAT)]
    pub repeat: u32,
    /// Sets `ANTHROPIC_BASE_URL` on the harness child, so every row talks to
    /// the same running gateway.
    #[arg(long)]
    pub gateway: Option<String>,
    /// Path to the `inference-gateway` executable to read exchange rows from
    /// (via `routing-cost --json`). Omitted means no meter: tokens and turns
    /// are absent for every attempt, never a fabricated zero.
    #[arg(long)]
    pub meter: Option<PathBuf>,
    /// Expands the `sterna` row into one `sterna:<mode>` arm per listed mode
    /// (`hybrid,cells,tools`), each launched with the row's own argv plus
    /// `--interface <mode> --output-format json`, its stdout captured for
    /// the interface-regret table. Refused unless `sterna` is a selected row.
    #[arg(long, value_delimiter = ',')]
    pub sterna_interface: Vec<String>,
    /// Helper-token credit ratios for the regret table's weighted spend,
    /// `luna=0.2,terra=0.1`. Both default to 1.0 and the table then says
    /// "assumed ratio"; nothing here is a billed figure.
    #[arg(long)]
    pub credit_ratio: Option<String>,
    /// Expands the `sterna` row into one `sterna:decisions-<mode>` arm per
    /// listed mode (`off,shadow,on`), each attempt's worktree getting a
    /// `.sterna/config.toml` written right after `cut_worktree` and before the
    /// harness launches -- the mode never travels as a session flag. Refused
    /// unless `sterna` is a selected row, together with `--sterna-interface`
    /// (one expansion at a time), or listing a mode twice or outside the
    /// three known ones.
    #[arg(long, value_delimiter = ',')]
    pub sterna_decisions: Vec<String>,
    /// The decision model named in a `shadow`/`on` arm's `.sterna/config.toml`.
    /// Required by every `--sterna-decisions` mode but `off`, which gets no
    /// `[decisions]` table at all.
    #[arg(long)]
    pub decisions_model: Option<String>,
    /// The model every `sterna` row's session runs with, written into each
    /// attempt's own `.sterna/config.toml` as `[model] parent`.
    ///
    /// **Required whenever a `sterna` row is selected.** An attempt's worktree
    /// is cut detached and carries no configuration, so without this `sterna
    /// session` refuses to start and the attempt measures nothing --
    /// silently, because the task's own tests then run against an untouched
    /// tree. The other rows configure their own model and ignore this.
    #[arg(long)]
    pub parent_model: Option<String>,
    /// Expands the `sterna` row into one `sterna:feedback-<arm>` arm per listed
    /// arm of [`attempt::FEEDBACK_ARMS`] (`bare,shadow,scout,dissect,lanes,working,outline,turns,guided,nudge,low,oneshot,reduce,prefetch,all`),
    /// each attempt's `.sterna/config.toml` carrying the decision mode and the
    /// `[helpers]` switches of its arm. The decision model is
    /// `--decisions-model`, or Jev's default. Refused beside the other two
    /// expansions: one expansion at a time.
    #[arg(long, value_delimiter = ',')]
    pub sterna_feedback: Vec<String>,
    /// The helper model every `sterna` row's session runs its Scout, its
    /// acceptance lister and its checker with, written as `[helpers] model`.
    /// Without it no helper runs in an attempt.
    #[arg(long)]
    pub helpers_model: Option<String>,
    #[arg(long)]
    pub out: PathBuf,
}

/// Dispatches `args` (everything after `sterna ruler`) to the `run`
/// subcommand. `args[0]` must be `"run"`; every other flag is `run`'s own.
pub fn dispatch(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("run") => run(&args[1..]),
        Some(other) => Err(format!("unknown ruler subcommand: {other}")),
        None => Err("usage: sterna ruler run [flags]".to_string()),
    }
}

fn run(flags: &[String]) -> Result<(), String> {
    let args = RunArgs::try_parse_from(
        std::iter::once("sterna ruler run".to_string()).chain(flags.iter().cloned()),
    )
    .map_err(|e| e.to_string())?;

    if args.repeat < MIN_REPEAT {
        return Err(format!(
            "--repeat must be at least {MIN_REPEAT}: a single attempt of an agent task measures the sample, not the harness"
        ));
    }
    let expansions = [
        !args.sterna_interface.is_empty(),
        !args.sterna_decisions.is_empty(),
        !args.sterna_feedback.is_empty(),
    ];
    if expansions.iter().filter(|given| **given).count() > 1 {
        return Err(
            "--sterna-interface, --sterna-decisions and --sterna-feedback cannot be combined: one expansion at a time"
                .to_string(),
        );
    }

    let selected = resolve_harnesses(&args)?;
    if args.parent_model.is_none()
        && let Some(row) = selected.iter().find(|row| attempt::is_sterna_row(row))
    {
        return Err(format!(
            "--parent-model <id> is required to run the `{row}` row: an attempt's worktree carries no .sterna/config.toml, so `sterna session` would refuse to start and the attempt would measure nothing"
        ));
    }
    if let Some(model) = &args.parent_model {
        crate::config::validate_parent_model(model)?;
    }
    let ratios = match &args.credit_ratio {
        Some(text) => CreditRatios::parse(text)?,
        None => CreditRatios::default(),
    };

    let mut harness_table = attempt::default_harnesses();
    let harnesses =
        expand_sterna_interfaces(&selected, &args.sterna_interface, &mut harness_table)?;
    let harnesses = expand_sterna_decisions(
        &harnesses,
        &args.sterna_decisions,
        args.decisions_model.as_deref(),
        &mut harness_table,
    )?;
    let harnesses = expand_sterna_feedback(
        &harnesses,
        &args.sterna_feedback,
        args.decisions_model
            .as_deref()
            .unwrap_or(crate::decide::DEFAULT_MODEL),
        &mut harness_table,
    )?;

    let tasks = resolve_tasks(&args)?;
    if tasks.is_empty() {
        return Err("no tasks selected: pass --task or --tier".to_string());
    }

    let opts = RunOpts {
        scratch: std::env::temp_dir().join("sterna-ruler"),
        gateway: args.gateway.clone(),
        meter: match &args.meter {
            Some(executable) => Meter::Command {
                executable: executable.clone(),
            },
            None => Meter::None,
        },
        harnesses: harness_table,
        parent_model: args.parent_model.clone(),
        helpers_model: args.helpers_model.clone(),
        // Created before the first attempt rather than with the records at
        // the end: an attempt writes its rollout while it runs, and a
        // missing directory would leave every one of them unstated.
        rollouts: Some(args.out.clone()),
    };
    fs::create_dir_all(&args.out)
        .map_err(|e| format!("could not create --out {}: {e}", args.out.display()))?;

    // This loop must stay a plain sequential loop: `attempt::run_one` reads
    // the meter by time window alone (`meter.rs`'s module doc comment), and
    // `attempt::ATTEMPT_LOCK` only makes concurrent calls *safe*, not
    // *meaningful* -- calling `run_one` from multiple threads would still
    // serialize their windows one at a time, silently discarding the
    // parallelism a naive refactor here would be trying to add.
    let mut attempts = Vec::new();
    for task in &tasks {
        for harness_name in &harnesses {
            let harness = Harness::new(harness_name.clone());
            for attempt_no in 1..=args.repeat {
                attempts.push(attempt::run_one(task, &harness, attempt_no, &opts));
            }
        }
    }

    print!(
        "{}",
        report::render_table(&Score::with_ratios(&attempts, ratios))
    );
    print!(
        "{}",
        report::render_decisions_table(&decisions::rows(&attempts))
    );
    print!("{}", report::render_rubric_table(&attempts));
    write_records(&args.out, &attempts)
}

fn resolve_tasks(args: &RunArgs) -> Result<Vec<&'static Task>, String> {
    let mut seen = HashMap::new();
    let mut resolved = Vec::new();

    if let Some(tier_name) = &args.tier {
        let tier = parse_tier(tier_name)?;
        for task in tasks::in_tier(tier) {
            if seen.insert(task.id, ()).is_none() {
                resolved.push(task);
            }
        }
    }

    for id in &args.task {
        if id == "all" {
            for task in tasks::CATALOGUE {
                if seen.insert(task.id, ()).is_none() {
                    resolved.push(task);
                }
            }
            continue;
        }
        let task = tasks::lookup(id).ok_or_else(|| format!("unknown task id: {id}"))?;
        if seen.insert(task.id, ()).is_none() {
            resolved.push(task);
        }
    }

    Ok(resolved)
}

/// Replaces the `sterna` row in `selected` with one `sterna:<mode>` row per
/// mode, adding each arm's [`HarnessCommand`] to `table`. No modes: the
/// selection is returned unchanged. Refuses a mode `Interface::parse` does
/// not know, a mode listed twice, and any mode list when `sterna` was not
/// selected -- there is no row to expand.
pub fn expand_sterna_interfaces(
    selected: &[String],
    modes: &[String],
    table: &mut HashMap<String, HarnessCommand>,
) -> Result<Vec<String>, String> {
    if modes.is_empty() {
        return Ok(selected.to_vec());
    }
    if !selected.iter().any(|row| row == "sterna") {
        return Err(
            "--sterna-interface expands the sterna row, and --harness did not select sterna"
                .to_string(),
        );
    }
    let mut parsed = Vec::new();
    for mode in modes {
        let mode = crate::abi::Interface::parse(mode)?.as_str();
        if parsed.contains(&mode) {
            return Err(format!("--sterna-interface names {mode} twice"));
        }
        parsed.push(mode);
    }
    let sterna = table
        .get("sterna")
        .cloned()
        .ok_or_else(|| "the harness table has no sterna row to expand".to_string())?;
    let mut rows = Vec::new();
    for row in selected {
        if row != "sterna" {
            rows.push(row.clone());
            continue;
        }
        for mode in &parsed {
            let name = attempt::sterna_arm_name(mode);
            table.insert(
                name.clone(),
                HarnessCommand::sterna_interface_arm(&sterna, mode),
            );
            rows.push(name);
        }
    }
    Ok(rows)
}

/// Replaces the `sterna` row in `selected` with one `sterna:decisions-<mode>`
/// row per mode, adding each arm's [`HarnessCommand`] to `table`. No modes:
/// the selection is returned unchanged. Refuses a mode outside
/// [`DECISIONS_MODES`], a mode listed twice, any mode list when `sterna` was
/// not selected, and `shadow`/`on` without `model` -- all before any
/// worktree is cut. `off` never needs `model`: unset model already means
/// off, and the arm gets no `[decisions]` table.
pub fn expand_sterna_decisions(
    selected: &[String],
    modes: &[String],
    model: Option<&str>,
    table: &mut HashMap<String, HarnessCommand>,
) -> Result<Vec<String>, String> {
    if modes.is_empty() {
        return Ok(selected.to_vec());
    }
    if !selected.iter().any(|row| row == "sterna") {
        return Err(
            "--sterna-decisions expands the sterna row, and --harness did not select sterna"
                .to_string(),
        );
    }
    let mut parsed = Vec::new();
    for mode in modes {
        if !DECISIONS_MODES.contains(&mode.as_str()) {
            return Err(format!(
                "--sterna-decisions knows off, shadow and on, not `{mode}`"
            ));
        }
        if parsed.contains(mode) {
            return Err(format!("--sterna-decisions names {mode} twice"));
        }
        parsed.push(mode.clone());
    }
    for mode in &parsed {
        if mode != "off" && model.is_none() {
            return Err(format!(
                "--sterna-decisions {mode} needs --decisions-model <name>"
            ));
        }
    }
    let sterna = table
        .get("sterna")
        .cloned()
        .ok_or_else(|| "the harness table has no sterna row to expand".to_string())?;
    let mut rows = Vec::new();
    for row in selected {
        if row != "sterna" {
            rows.push(row.clone());
            continue;
        }
        for mode in &parsed {
            let name = attempt::decisions_arm_name(mode);
            let arm_model = if mode == "off" { None } else { model };
            table.insert(
                name.clone(),
                HarnessCommand::sterna_decisions_arm(&sterna, arm_model, mode),
            );
            rows.push(name);
        }
    }
    Ok(rows)
}

/// Replaces the `sterna` row in `selected` with one `sterna:feedback-<arm>` row
/// per arm, adding each arm's [`HarnessCommand`] to `table`. No arms: the
/// selection is returned unchanged. Refuses an arm outside
/// [`attempt::FEEDBACK_ARMS`], an arm listed twice, and any arm list when
/// `sterna` was not selected -- all before any worktree is cut.
pub fn expand_sterna_feedback(
    selected: &[String],
    arms: &[String],
    model: &str,
    table: &mut HashMap<String, HarnessCommand>,
) -> Result<Vec<String>, String> {
    if arms.is_empty() {
        return Ok(selected.to_vec());
    }
    if !selected.iter().any(|row| row == "sterna") {
        return Err(
            "--sterna-feedback expands the sterna row, and --harness did not select sterna"
                .to_string(),
        );
    }
    let mut parsed = Vec::new();
    for name in arms {
        let arm = attempt::FEEDBACK_ARMS
            .iter()
            .find(|arm| arm.name == name)
            .ok_or_else(|| {
                format!(
                    "--sterna-feedback knows bare, shadow, scout, dissect, oneshot, reduce, prefetch and all, not `{name}`"
                )
            })?;
        if parsed.contains(&arm) {
            return Err(format!("--sterna-feedback names {name} twice"));
        }
        parsed.push(arm);
    }
    let sterna = table
        .get("sterna")
        .cloned()
        .ok_or_else(|| "the harness table has no sterna row to expand".to_string())?;
    let mut rows = Vec::new();
    for row in selected {
        if row != "sterna" {
            rows.push(row.clone());
            continue;
        }
        for arm in &parsed {
            let name = attempt::feedback_arm_name(arm.name);
            table.insert(
                name.clone(),
                HarnessCommand::sterna_feedback_arm(&sterna, model, arm),
            );
            rows.push(name);
        }
    }
    Ok(rows)
}

fn resolve_harnesses(args: &RunArgs) -> Result<Vec<String>, String> {
    if args.harness.is_empty() {
        return Err("no harness selected: pass at least one --harness".to_string());
    }
    Ok(args.harness.clone())
}

fn parse_tier(name: &str) -> Result<Tier, String> {
    match name {
        "leaf" => Ok(Tier::Leaf),
        "standard" => Ok(Tier::Standard),
        "heavy" => Ok(Tier::Heavy),
        other => Err(format!(
            "unknown tier: {other} (want leaf, standard or heavy)"
        )),
    }
}

fn write_records(out_dir: &Path, attempts: &[Attempt]) -> Result<(), String> {
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("could not create --out {}: {e}", out_dir.display()))?;
    let path = out_dir.join("attempts.jsonl");
    fs::write(&path, report::render_jsonl(attempts))
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}
