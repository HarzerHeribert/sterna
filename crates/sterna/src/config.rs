//! Sterna runtime settings, layered by the native settings store at startup
//! (`docs/configuration.md`). A missing file means every default
//! the runtime limits already used before this package existed
//! (`runtime-contract.md` §7), so an absent file changes no
//! existing test.

use std::path::Path;

use crate::tools::registry;

/// `[limits]` -- the runtime constants `runtime-contract.md` §7 and the cell
/// limit, now loadable. Token spend is telemetry rather than a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub cell_wall_clock_s: u64,
    pub response_bytes: usize,
    /// A ceiling on cells **only when this person set one**, and `0` in the
    /// file means the same as absent (the user, 2026-09-17: "Limits are dumb
    /// for abstract tasks").
    ///
    /// It used to default to 120, and on 2026-09-17 that default ended a
    /// four-hour session at cell 120 of 120, mid-implementation, with code
    /// that did not compile — and cancelled that session's own `cargo test`
    /// job on the way out. What ends a task now is evidence that it has
    /// stopped producing anything: `progress::Stall`'s run of empty windows.
    pub cells: Option<u64>,
    /// Whether a terminal return is held once for the deterministic
    /// final-state contract check and the no-progress guard's findings
    /// (`smarter-cheaper-roadmap.md`, *Evidence-gated completion*). On by
    /// default; off is an ablation switch, not a product mode.
    pub evidence_gate: bool,
    /// The share of a **known** context window at which the conversation is
    /// swept once, as a percentage (the user, 2026-09-17: *"should be done in
    /// one deliberate sweep … because after that you pay less again per
    /// call"*).
    ///
    /// Not a ceiling and never ends anything: over it, older cell results
    /// lose the sections the newest one restates in full. An unknown window
    /// sweeps nothing, because there is no fraction to be over.
    pub compact_above_percent: u64,
    /// Keep this many of the newest cell results in full and collapse every
    /// older one to its first line (`prompt::keep_recent_results`); `0`
    /// keeps them all, and is the default: a collapse rewrites earlier turns,
    /// which restarts the provider's cache from that point, and history is
    /// append-only except for compaction (user, 2026-09-24).
    pub keep_results: usize,
    /// Show a long instruction document as its headings and their lines
    /// (`project::instructions::root_outlined`). Off: measured worse.
    pub instructions_outline: bool,
    /// Tell the model what a turn costs (`prompt::TURN_ECONOMY`), so it
    /// plans the task in fewer, whole-step cells. Off until measured.
    pub turn_economy: bool,
    /// `prompt::AUTONOMY_BLOCK` in the system prompt. Off until measured.
    pub autonomy_block: bool,
    /// `prompt::SCOPE_BLOCK` in the system prompt. Off until measured.
    pub scope_block: bool,
    /// `prompt::BATCH_NUDGE` at the end of every cell result. Off until
    /// measured.
    pub batch_nudge: bool,
    /// The estimated size in tokens above which a command result is
    /// shortened by the reduction rules (`runtime/reduce_rules.rs`).
    pub reduce_above_tokens: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cell_wall_clock_s: 30,
            response_bytes: 16 * 1024,
            // No ceiling unless this person asks for one.
            cells: None,
            evidence_gate: true,
            // High on purpose. The user, 2026-09-17: "Compression at 252k
            // can be worth it but you pay with one call. 252k is not that
            // much by today's model though. I start compression at around
            // 800k in Claude code." A fraction carries that across models:
            // 85% of a 922k window is 784k, and 85% of a small one is small.
            compact_above_percent: 85,
            // Off since 2026-09-24: a collapse rewrites earlier turns, and
            // history is append-only except for compaction.
            keep_results: 0,
            // Measured and dropped the same day: with only headings the
            // model reread the document 30-47 times and found fewer facts.
            instructions_outline: false,
            turn_economy: false,
            autonomy_block: false,
            scope_block: false,
            batch_nudge: false,
            reduce_above_tokens: REDUCE_ABOVE_TOKENS_DEFAULT,
        }
    }
}

/// `[limits] reduce_above_tokens` when the file names none.
pub const REDUCE_ABOVE_TOKENS_DEFAULT: usize = 2048;

/// `[decisions] mode` -- whether the decision model's hold reaches the task
/// model at all. `off` and an unset `model` are both "no request is ever
/// made"; `mode` only matters once a model is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecisionMode {
    Off,
    #[default]
    Shadow,
    On,
}

impl DecisionMode {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "shadow" => Ok(Self::Shadow),
            "on" => Ok(Self::On),
            other => Err(format!(
                "config.toml: `[decisions] mode` must be \"off\", \"shadow\" or \"on\", not `{other}`"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::On => "on",
        }
    }
}

/// `[decisions]` -- the decision model's one intent question and the hold it
/// buys (`docs/decisions.md`). `model` has no default: unset means
/// decisions are off, said once at start.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionsConfig {
    pub model: Option<String>,
    pub mode: DecisionMode,
    /// Confidence at or above which a read-only intent holds an effectful
    /// cell or frame. `0.5..=1.0`.
    pub hold_above: f64,
    /// The completion question's noul at or below which a claimed completion
    /// gets a `RequestNotSatisfied` finding (2616). `0.0..=0.5`.
    pub completion_no_below: f64,
    /// A diff-hygiene `has_tests` noul at or below which the diff is read as
    /// missing tests for the behaviour it changes (2641). `0.0..=0.5`.
    pub hygiene_no_below: f64,
    /// A diff-hygiene `out_of_scope`/`debug_leftovers`/`deletes_tests`/
    /// `changes_signature` noul at or above which that question is decisive
    /// (2641). `0.5..=1.0`.
    pub hygiene_yes_above: f64,
    /// Whether Jev reads a long returned field's line shapes and, when it
    /// reads a log, shortens it by the reduction rules while the whole value
    /// stays bound. On: Jev reads a field's shape at 97 %.
    pub reduce_returns: bool,
}

impl Default for DecisionsConfig {
    fn default() -> Self {
        Self {
            model: None,
            mode: DecisionMode::default(),
            hold_above: 0.85,
            completion_no_below: 0.10,
            hygiene_no_below: 0.10,
            hygiene_yes_above: 0.90,
            reduce_returns: true,
        }
    }
}

/// `[ask] jev` -- how far the decision model gets to go on a question the
/// program put to the person.
///
/// `weight` is the default wherever a decision model exists: the person still
/// chooses, and Jev's reading of the request, the diff and the findings sits
/// beside each option as a number. `decide` lets a confident enough answer
/// stand in for the person; `off` never asks it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AskJev {
    Off,
    #[default]
    Weight,
    Decide,
}

impl AskJev {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "off" => Ok(Self::Off),
            "weight" => Ok(Self::Weight),
            "decide" => Ok(Self::Decide),
            other => Err(format!(
                "config.toml: `[ask] jev` must be \"off\", \"weight\" or \"decide\", not `{other}`"
            )),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Weight => "weight",
            Self::Decide => "decide",
        }
    }
}

/// `[ask]` -- whether a running program may put a question to the person, and
/// how much of the answer the decision model is allowed to supply.
///
/// Off by default: a question suspends the work until somebody looks at the
/// screen, so a session that never asked for the capability must not gain one
/// that can stall it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AskConfig {
    pub enabled: bool,
    pub jev: AskJev,
    /// Confidence at or above which `jev = "decide"` answers instead of the
    /// person. `0.5..=1.0`, the same span every other confidence takes.
    pub decide_above: f64,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            jev: AskJev::default(),
            decide_above: 0.85,
        }
    }
}

/// The whole of `config.toml`. `project.rs`'s own invariant -- loading edits
/// nothing -- holds here too: nothing in this module opens a path for writing.
///
/// `Eq` is not derived: [`DecisionsConfig::hold_above`] is an `f64`, and
/// nothing here needs `SternaConfig` as a map key or in a set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SternaConfig {
    pub limits: Limits,
    pub agents: AgentsConfig,
    pub model: ModelConfig,
    pub web: crate::web::WebConfig,
    pub decisions: DecisionsConfig,
    pub ask: AskConfig,
    pub wizard: WizardConfig,
}

/// `[wizard]` -- which version of Sterna's recommended settings the person
/// last saw, so an update that changes them shows the difference once
/// (`session/setup.rs`). Global only: it is about the person, not a project.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WizardConfig {
    pub seen: u32,
}

/// `[model]` -- the parent tier, the one the person talks to.
///
/// It is here for the same reason `[agents] model` is:
/// a session is three models, and the one you chose last should not be the
/// only one that forgets. There is deliberately no compiled-in fallback:
/// startup requires either this value or `--model`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelConfig {
    pub parent: Option<String>,
}

mod agents;
use agents::parse_agents;
pub use agents::{AgentSlot, AgentsConfig, AgentsMode, SLOT_NAMES, slot_effort};

/// One integer key's valid range, spelled once so the refusal sentence and
/// the check it comes from cannot drift apart.
struct Range {
    key: &'static str,
    min: i64,
    max: i64,
}

const CELL_WALL_CLOCK_S: Range = Range {
    key: "cell_wall_clock_s",
    min: 1,
    max: 600,
};
const RESPONSE_BYTES: Range = Range {
    key: "response_bytes",
    min: 1024,
    max: 1_048_576,
};
const CELLS: Range = Range {
    key: "cells",
    min: 1,
    max: 1000,
};
/// Below half a window a sweep removes little and rebuilds the cached prefix
/// for it; at 100 there is no room left to sweep into.
const COMPACT_ABOVE_PERCENT: Range = Range {
    key: "compact_above_percent",
    min: 50,
    max: 99,
};
/// `0` keeps every result; past a few dozen there is nothing left to save.
const KEEP_RESULTS: Range = Range {
    key: "keep_results",
    min: 0,
    max: 64,
};
const REDUCE_ABOVE_TOKENS: Range = Range {
    key: "reduce_above_tokens",
    min: 256,
    max: 32_768,
};

impl Range {
    fn check(&self, value: i64) -> Result<i64, String> {
        if value < self.min || value > self.max {
            Err(format!(
                "config.toml: `{}` must be between {} and {}",
                self.key, self.min, self.max
            ))
        } else {
            Ok(value)
        }
    }
}

impl SternaConfig {
    /// Loads global and `<root>/.sterna/config.toml` settings. Missing files use defaults,
    /// never an error -- most projects have none.
    pub fn load(root: &Path) -> Result<Self, String> {
        Self::load_profile(root, None)
    }

    /// Select a named configuration overlay without modifying project defaults.
    pub fn load_profile(root: &Path, profile: Option<&str>) -> Result<Self, String> {
        Ok(crate::settings::Store::new(root)?.load(profile)?.config)
    }

    pub fn parse_profile(text: &str, selected: Option<&str>) -> Result<Self, String> {
        let mut value: toml::Value =
            toml::from_str(text).map_err(|error| format!("config.toml: {error}"))?;
        let table = value.as_table_mut().ok_or("config.toml must be a table")?;
        let profiles = table.remove("profiles");
        if let Some(profiles) = &profiles {
            let profiles = profiles
                .as_table()
                .ok_or("config.toml: [profiles] must be a table")?;
            for (name, overlay) in profiles {
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    || !overlay.is_table()
                {
                    return Err(
                        "config.toml: profiles must be named tables (letters, digits, _ or -)"
                            .into(),
                    );
                }
            }
        }
        if let Some(name) = selected {
            let overlay = profiles
                .as_ref()
                .and_then(|profiles| profiles.get(name))
                .ok_or_else(|| format!("config.toml: no profile named `{name}`"))?;
            if let Some(agents) = overlay.get("agents").and_then(toml::Value::as_table)
                && matches!(
                    agents.get("mode").and_then(toml::Value::as_str),
                    Some("off" | "auto")
                )
                && !agents.contains_key("model")
                && let Some(base_agents) =
                    value.get_mut("agents").and_then(toml::Value::as_table_mut)
            {
                base_agents.remove("model");
            }
            merge_tables(&mut value, overlay);
        }
        Self::parse_base(&toml::to_string(&value).map_err(|error| format!("config.toml: {error}"))?)
    }

    /// Parses without touching the filesystem.
    ///
    /// Public so that a caller about to **write** `config.toml` can prove the
    /// text it is about to save loads — the same check `/permissions` makes
    /// by compiling a profile before saving `settings.json`. Reading stays
    /// this module's only filesystem verb; the write belongs to the command
    /// that made the edit.
    pub fn parse(text: &str) -> Result<Self, String> {
        Self::parse_profile(text, None)
    }

    fn parse_base(text: &str) -> Result<Self, String> {
        let value: toml::Value = toml::from_str(text).map_err(|e| format!("config.toml: {e}"))?;
        let table = value.as_table().ok_or_else(|| {
            "config.toml: must be a table of [limits], [agents] and the other sections".to_string()
        })?;

        for key in table.keys() {
            if ![
                "limits",
                "agents",
                "model",
                "web",
                "decisions",
                "ask",
                "wizard",
            ]
            .contains(&key.as_str())
            {
                return Err(format!(
                    "config.toml: unknown table `[{key}]`; only [limits], \
                     [agents], [model], [web], [decisions], [ask] and [wizard] are recognised"
                ));
            }
        }

        let limits = match table.get("limits") {
            Some(value) => parse_limits(value)?,
            None => Limits::default(),
        };

        let agents = match table.get("agents") {
            Some(value) => parse_agents(value)?,
            None => AgentsConfig::default(),
        };

        let model = match table.get("model") {
            Some(value) => parse_model(value)?,
            None => ModelConfig::default(),
        };

        let web = match table.get("web") {
            Some(value) => value
                .clone()
                .try_into::<crate::web::WebConfig>()
                .map_err(|error| format!("config.toml: [web]: {error}"))?,
            None => crate::web::WebConfig::default(),
        };
        crate::web::WebBroker::new(web.clone())?;

        let decisions = match table.get("decisions") {
            Some(value) => parse_decisions(value)?,
            None => DecisionsConfig::default(),
        };

        let ask = match table.get("ask") {
            Some(value) => parse_ask(value)?,
            None => AskConfig::default(),
        };
        let wizard = match table.get("wizard") {
            Some(value) => {
                let table = table_of(value, "wizard")?;
                if let Some(key) = table.keys().find(|key| key.as_str() != "seen") {
                    return Err(format!("config.toml: unknown key `{key}` in [wizard]"));
                }
                let seen = match table.get("seen") {
                    None => 0,
                    Some(value) => value
                        .as_integer()
                        .and_then(|seen| u32::try_from(seen).ok())
                        .ok_or("config.toml: `[wizard] seen` must be a whole number")?,
                };
                WizardConfig { seen }
            }
            None => WizardConfig::default(),
        };

        Ok(Self {
            limits,
            agents,
            model,
            web,
            decisions,
            ask,
            wizard,
        })
    }
}

fn merge_tables(base: &mut toml::Value, overlay: &toml::Value) {
    if let (Some(base), Some(overlay)) = (base.as_table_mut(), overlay.as_table()) {
        for (key, value) in overlay {
            match base.get_mut(key) {
                Some(existing) if existing.is_table() && value.is_table() => {
                    merge_tables(existing, value)
                }
                _ => {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    }
}

/// `[agents]` -- an explicit mode plus a model for `pinned`.
///
/// A legacy table containing only `model` is interpreted as `pinned`, so an
/// existing project keeps the behaviour it selected before modes existed.
/// `[model] parent` -- one optional key, refused the same way the other two
/// tiers' model names are, so one validator covers all three.
fn parse_model(value: &toml::Value) -> Result<ModelConfig, String> {
    let table = table_of(value, "model")?;
    for key in table.keys() {
        if key != "parent" {
            return Err(format!(
                "config.toml: unknown key `{key}` in [model]; only `parent` is recognised"
            ));
        }
    }
    let parent = match table.get("parent") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| "config.toml: `parent` must be a string".to_string())?;
            validate_parent_model(text)?;
            Some(text.to_string())
        }
    };
    Ok(ModelConfig { parent })
}

fn table_of<'a>(value: &'a toml::Value, name: &str) -> Result<&'a toml::value::Table, String> {
    value
        .as_table()
        .ok_or_else(|| format!("config.toml: [{name}] must be a table"))
}

fn int_field(table: &toml::value::Table, key: &str) -> Result<Option<i64>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_integer()
            .map(Some)
            .ok_or_else(|| format!("config.toml: `{key}` must be an integer")),
    }
}

fn parse_limits(value: &toml::Value) -> Result<Limits, String> {
    let table = table_of(value, "limits")?;
    let defaults = Limits::default();

    for key in table.keys() {
        if ![
            "cell_wall_clock_s",
            "response_bytes",
            "cells",
            "evidence_gate",
            "compact_above_percent",
            "keep_results",
            "instructions_outline",
            "turn_economy",
            "autonomy_block",
            "scope_block",
            "batch_nudge",
            "reduce_above_tokens",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("config.toml: unknown key `{key}` in [limits]"));
        }
    }
    let evidence_gate = match table.get("evidence_gate") {
        None => defaults.evidence_gate,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `evidence_gate` must be true or false".to_string())?,
    };

    let cell_wall_clock_s = match int_field(table, "cell_wall_clock_s")? {
        Some(v) => u64::try_from(CELL_WALL_CLOCK_S.check(v)?).expect("range is non-negative"),
        None => defaults.cell_wall_clock_s,
    };
    let response_bytes = match int_field(table, "response_bytes")? {
        Some(v) => usize::try_from(RESPONSE_BYTES.check(v)?).expect("range is non-negative"),
        None => defaults.response_bytes,
    };
    let compact_above_percent = match int_field(table, "compact_above_percent")? {
        Some(v) => u64::try_from(COMPACT_ABOVE_PERCENT.check(v)?).expect("range is non-negative"),
        None => defaults.compact_above_percent,
    };
    let cells = match int_field(table, "cells")? {
        // Zero is the explicit "no ceiling", as `[agents] deadline_minutes`
        // spells the same intent; absent leaves the default, which is none.
        Some(0) => None,
        Some(v) => Some(u64::try_from(CELLS.check(v)?).expect("range is non-negative")),
        None => defaults.cells,
    };

    let keep_results = match int_field(table, "keep_results")? {
        Some(v) => usize::try_from(KEEP_RESULTS.check(v)?).expect("range is non-negative"),
        None => defaults.keep_results,
    };
    let instructions_outline = match table.get("instructions_outline") {
        None => defaults.instructions_outline,
        Some(value) => value.as_bool().ok_or_else(|| {
            "config.toml: `instructions_outline` must be true or false".to_string()
        })?,
    };

    let turn_economy = match table.get("turn_economy") {
        None => defaults.turn_economy,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `turn_economy` must be true or false".to_string())?,
    };
    let switch = |key: &str, default: bool| -> Result<bool, String> {
        match table.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_bool()
                .ok_or_else(|| format!("config.toml: `{key}` must be true or false")),
        }
    };
    let autonomy_block = switch("autonomy_block", defaults.autonomy_block)?;
    let scope_block = switch("scope_block", defaults.scope_block)?;
    let batch_nudge = switch("batch_nudge", defaults.batch_nudge)?;
    let reduce_above_tokens = match int_field(table, "reduce_above_tokens")? {
        Some(v) => usize::try_from(REDUCE_ABOVE_TOKENS.check(v)?).expect("range is non-negative"),
        None => defaults.reduce_above_tokens,
    };

    Ok(Limits {
        reduce_above_tokens,
        turn_economy,
        autonomy_block,
        scope_block,
        batch_nudge,
        cell_wall_clock_s,
        response_bytes,
        cells,
        evidence_gate,
        compact_above_percent,
        keep_results,
        instructions_outline,
    })
}

const HOLD_ABOVE_MIN: f64 = 0.5;
const HOLD_ABOVE_MAX: f64 = 1.0;
const COMPLETION_NO_BELOW_MIN: f64 = 0.0;
const COMPLETION_NO_BELOW_MAX: f64 = 0.5;
const HYGIENE_NO_BELOW_MIN: f64 = 0.0;
const HYGIENE_NO_BELOW_MAX: f64 = 0.5;
const HYGIENE_YES_ABOVE_MIN: f64 = 0.5;
const HYGIENE_YES_ABOVE_MAX: f64 = 1.0;

const DECIDE_ABOVE_MIN: f64 = 0.5;
const DECIDE_ABOVE_MAX: f64 = 1.0;

/// `[ask]`, refused key by key like every other table: an unknown key is a
/// typo the person meant to have an effect, not a setting to ignore.
fn parse_ask(value: &toml::Value) -> Result<AskConfig, String> {
    let table = table_of(value, "ask")?;
    let defaults = AskConfig::default();

    for key in table.keys() {
        if !["enabled", "jev", "decide_above"].contains(&key.as_str()) {
            return Err(format!("config.toml: unknown key `{key}` in [ask]"));
        }
    }

    let enabled = match table.get("enabled") {
        None => defaults.enabled,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `[ask] enabled` must be a boolean".to_string())?,
    };
    let jev = match table.get("jev") {
        None => defaults.jev,
        Some(value) => AskJev::parse(
            value
                .as_str()
                .ok_or_else(|| "config.toml: `[ask] jev` must be a string".to_string())?,
        )?,
    };
    let decide_above = match table.get("decide_above") {
        None => defaults.decide_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `decide_above` must be a number".to_string())?;
            if !(DECIDE_ABOVE_MIN..=DECIDE_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `decide_above` must be between {DECIDE_ABOVE_MIN} and {DECIDE_ABOVE_MAX}"
                ));
            }
            number
        }
    };

    Ok(AskConfig {
        enabled,
        jev,
        decide_above,
    })
}

fn parse_decisions(value: &toml::Value) -> Result<DecisionsConfig, String> {
    let table = table_of(value, "decisions")?;
    let defaults = DecisionsConfig::default();

    for key in table.keys() {
        if ![
            "model",
            "mode",
            "hold_above",
            "completion_no_below",
            "hygiene_no_below",
            "hygiene_yes_above",
            "reduce_returns",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("config.toml: unknown key `{key}` in [decisions]"));
        }
    }

    let model = match table.get("model") {
        None => None,
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| "config.toml: `[decisions] model` must be a string".to_string())?;
            check_names_no_tool_path_or_grant("model", text)?;
            Some(text.to_string())
        }
    };
    let mode = match table.get("mode") {
        None => defaults.mode,
        Some(value) => DecisionMode::parse(
            value
                .as_str()
                .ok_or_else(|| "config.toml: `mode` must be a string".to_string())?,
        )?,
    };
    let hold_above = match table.get("hold_above") {
        None => defaults.hold_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `hold_above` must be a number".to_string())?;
            if !(HOLD_ABOVE_MIN..=HOLD_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `hold_above` must be between {HOLD_ABOVE_MIN} and {HOLD_ABOVE_MAX}"
                ));
            }
            number
        }
    };
    let completion_no_below = match table.get("completion_no_below") {
        None => defaults.completion_no_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `completion_no_below` must be a number".to_string())?;
            if !(COMPLETION_NO_BELOW_MIN..=COMPLETION_NO_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `completion_no_below` must be between {COMPLETION_NO_BELOW_MIN} and {COMPLETION_NO_BELOW_MAX}"
                ));
            }
            number
        }
    };

    let hygiene_no_below = match table.get("hygiene_no_below") {
        None => defaults.hygiene_no_below,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `hygiene_no_below` must be a number".to_string())?;
            if !(HYGIENE_NO_BELOW_MIN..=HYGIENE_NO_BELOW_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `hygiene_no_below` must be between {HYGIENE_NO_BELOW_MIN} and {HYGIENE_NO_BELOW_MAX}"
                ));
            }
            number
        }
    };
    let hygiene_yes_above = match table.get("hygiene_yes_above") {
        None => defaults.hygiene_yes_above,
        Some(value) => {
            let number = value
                .as_float()
                .or_else(|| value.as_integer().map(|v| v as f64))
                .ok_or_else(|| "config.toml: `hygiene_yes_above` must be a number".to_string())?;
            if !(HYGIENE_YES_ABOVE_MIN..=HYGIENE_YES_ABOVE_MAX).contains(&number) {
                return Err(format!(
                    "config.toml: `hygiene_yes_above` must be between {HYGIENE_YES_ABOVE_MIN} and {HYGIENE_YES_ABOVE_MAX}"
                ));
            }
            number
        }
    };

    let reduce_returns = match table.get("reduce_returns") {
        None => defaults.reduce_returns,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "config.toml: `reduce_returns` must be true or false".to_string())?,
    };

    Ok(DecisionsConfig {
        reduce_returns,
        model,
        mode,
        hold_above,
        completion_no_below,
        hygiene_no_below,
        hygiene_yes_above,
    })
}

fn check_names_no_tool_path_or_grant(key: &str, value: &str) -> Result<(), String> {
    // Gateway catalogues legitimately use provider-qualified ids such as
    // `vendor/model-302`. A slash alone therefore cannot mean "path". Path
    // roots and traversal segments can, and no concrete model id needs a
    // backslash, glob, or permission-expression parenthesis.
    let slash_segments: Vec<_> = value.split('/').collect();
    let bytes = value.as_bytes();
    let windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let looks_like_a_path_or_glob = value.starts_with('/')
        || value.starts_with("~/")
        || windows_drive
        || value.contains('\\')
        || value.contains('*')
        || value.contains('?')
        || value.contains('(')
        || value.contains(')')
        || slash_segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."));
    let names_a_tool = registry::names().contains(&value);
    if looks_like_a_path_or_glob || names_a_tool {
        return Err(format!("config.toml: `{key}` names no tool, path or grant"));
    }
    Ok(())
}

/// Validates a model used by the parent. Mode words are controls, never
/// concrete request model identifiers.
pub fn validate_parent_model(value: &str) -> Result<(), String> {
    validate_concrete_model("[model] parent", value)
}

fn validate_concrete_model(key: &str, value: &str) -> Result<(), String> {
    if value.is_empty() || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "config.toml: `{key}` must be one concrete model id"
        ));
    }
    if matches!(value, "auto" | "off" | "inherit") {
        return Err(format!(
            "config.toml: `{key}` must be a concrete model id, not `{value}`"
        ));
    }
    check_names_no_tool_path_or_grant(key, value)
}

#[cfg(test)]
mod completion_threshold_tests {
    use super::*;

    #[test]
    fn the_completion_thresholds_default_without_a_decisions_table() {
        let defaults = DecisionsConfig::default();
        assert_eq!(defaults.completion_no_below, 0.10);
        let config = SternaConfig::parse_profile("", None).unwrap();
        assert_eq!(config.decisions.completion_no_below, 0.10);
    }

    #[test]
    fn the_completion_thresholds_parse_and_are_refused_out_of_range() {
        let config = SternaConfig::parse_profile(
            "[decisions]\nmodel = \"jev-latest\"\ncompletion_no_below = 0.2\n",
            None,
        )
        .unwrap();
        assert_eq!(config.decisions.completion_no_below, 0.2);

        let error = SternaConfig::parse_profile("[decisions]\ncompletion_no_below = 0.6\n", None)
            .unwrap_err();
        assert!(error.contains("completion_no_below"), "{error}");
    }

    #[test]
    fn the_hygiene_thresholds_default_and_are_refused_out_of_range() {
        let defaults = DecisionsConfig::default();
        assert_eq!(defaults.hygiene_no_below, 0.10);
        assert_eq!(defaults.hygiene_yes_above, 0.90);

        let config = SternaConfig::parse_profile(
            "[decisions]\nmodel = \"jev-latest\"\nhygiene_no_below = 0.2\nhygiene_yes_above = 0.8\n",
            None,
        )
        .unwrap();
        assert_eq!(config.decisions.hygiene_no_below, 0.2);
        assert_eq!(config.decisions.hygiene_yes_above, 0.8);

        let error =
            SternaConfig::parse_profile("[decisions]\nhygiene_no_below = 0.6\n", None).unwrap_err();
        assert!(error.contains("hygiene_no_below"), "{error}");
        let error = SternaConfig::parse_profile("[decisions]\nhygiene_yes_above = 0.4\n", None)
            .unwrap_err();
        assert!(error.contains("hygiene_yes_above"), "{error}");
    }

    /// The two settings that outlived `[helpers]`: the size above which a
    /// command result is shortened by the rules, under `[limits]`, and Jev's
    /// reading of a long returned field, under `[decisions]`.
    #[test]
    fn the_reduction_settings_live_in_limits_and_decisions() {
        let config = SternaConfig::parse_profile("", None).unwrap();
        assert_eq!(
            config.limits.reduce_above_tokens,
            REDUCE_ABOVE_TOKENS_DEFAULT
        );
        assert!(config.decisions.reduce_returns);

        let config = SternaConfig::parse_profile(
            "[limits]\nreduce_above_tokens = 4096\n[decisions]\nreduce_returns = false\n",
            None,
        )
        .unwrap();
        assert_eq!(config.limits.reduce_above_tokens, 4096);
        assert!(!config.decisions.reduce_returns);

        let error =
            SternaConfig::parse_profile("[limits]\nreduce_above_tokens = 100\n", None).unwrap_err();
        assert!(error.contains("reduce_above_tokens"), "{error}");
        let error = SternaConfig::parse_profile("[decisions]\nreduce_returns = \"yes\"\n", None)
            .unwrap_err();
        assert!(error.contains("reduce_returns"), "{error}");
    }
}
