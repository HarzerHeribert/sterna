//! Explicit delegation policy. No default or legacy path inherits Main.
use super::{table_of, validate_concrete_model};
use crate::wire::Effort;
use std::collections::BTreeMap;

pub const SLOT_NAMES: [&str; 4] = ["quick", "balanced", "deep", "heavy"];
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentsMode {
    /// Accepted for migration only. Spawning is refused until the user chooses a policy.
    Auto,
    #[default]
    Off,
    Pinned,
    Roster,
}
impl AgentsMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::Pinned => "pinned",
            Self::Roster => "roster",
        }
    }
    fn parse(word: &str) -> Result<Self, String> {
        match word {
            "auto" => Ok(Self::Auto),
            "off" => Ok(Self::Off),
            "pinned" => Ok(Self::Pinned),
            "roster" => Ok(Self::Roster),
            _ => Err("[agents] mode must be off, pinned or roster (auto is migration-only)".into()),
        }
    }
}
/// The effort a favourite runs at when none is chosen for it.
pub fn slot_effort(slot: &str) -> Effort {
    match slot {
        "quick" => Effort::Low,
        "balanced" => Effort::Medium,
        "deep" => Effort::High,
        _ => Effort::Max,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSlot {
    pub model: String,
    pub effort: Effort,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentsConfig {
    pub mode: AgentsMode,
    pub model: Option<String>,
    pub deadline: Option<std::time::Duration>,
    pub slots: BTreeMap<String, AgentSlot>,
}
impl AgentsConfig {
    /// The spawn gate, not a picker filter. Templates and explicit model names
    /// cannot escape the user's selected model or slot roster.
    pub fn select(&self, model: Option<&str>, slot: Option<&str>) -> Result<AgentSlot, String> {
        match self.mode {
            AgentsMode::Off => Err(
                "subagents are off; configure an explicit model or favorite roster".into(),
            ),
            AgentsMode::Auto => Err(
                "legacy auto inheritance is disabled; choose an explicit subagent model or favorite roster".into(),
            ),
            AgentsMode::Pinned => {
                let pinned = self.model.as_ref().ok_or("pinned subagent model is missing")?;
                if slot.is_some() || model.is_some_and(|model| model != pinned) {
                    return Err("the requested model is outside the pinned subagent assignment".into());
                }
                Ok(AgentSlot { model: pinned.clone(), effort: Effort::Auto })
            }
            AgentsMode::Roster => {
                let selected = if let Some(slot) = slot {
                    self.slots.get(slot).ok_or("that favorite slot is empty or unknown")?
                } else {
                    let mut choices = self.slots.values()
                        .filter(|slot| model.is_none_or(|model| slot.model == model));
                    let first = choices.next().ok_or(
                        "no configured favorite matches; empty slots never inherit Main",
                    )?;
                    if choices.next().is_some() {
                        return Err(
                            "choose one configured favorite with agent.run(task, {slot: \"quick\"})".into(),
                        );
                    }
                    first
                };
                if model.is_some_and(|model| model != selected.model) {
                    return Err("the requested model does not match the configured favorite".into());
                }
                Ok(selected.clone())
            }
        }
    }
}
pub(super) fn parse_agents(value: &toml::Value) -> Result<AgentsConfig, String> {
    let t = table_of(value, "agents")?;
    for key in t.keys() {
        if !["mode", "model", "deadline_minutes", "slots"].contains(&key.as_str()) {
            return Err(format!("config.toml: unknown key `{key}` in [agents]"));
        }
    }
    let model = t
        .get("model")
        .map(|v| {
            let m = v.as_str().ok_or("[agents] model must be a string")?;
            validate_concrete_model("[agents] model", m)?;
            Ok::<_, String>(m.to_string())
        })
        .transpose()?;
    let mode = t
        .get("mode")
        .map(|v| AgentsMode::parse(v.as_str().ok_or("[agents] mode must be a string")?))
        .transpose()?
        .unwrap_or(if model.is_some() {
            AgentsMode::Pinned
        } else {
            AgentsMode::Off
        });
    if mode == AgentsMode::Pinned && model.is_none() {
        return Err("[agents] mode = pinned requires `model`".into());
    }
    if mode != AgentsMode::Pinned && model.is_some() {
        return Err(format!(
            "[agents] mode = {} cannot also set model",
            mode.name()
        ));
    }
    let deadline = t
        .get("deadline_minutes")
        .map(|v| {
            let n = v
                .as_integer()
                .ok_or("deadline_minutes must be whole minutes")?;
            let n = u64::try_from(n).map_err(|_| "deadline_minutes cannot be negative")?;
            let seconds = n.checked_mul(60).ok_or("deadline_minutes is too large")?;
            Ok::<_, String>((n > 0).then(|| std::time::Duration::from_secs(seconds)))
        })
        .transpose()?
        .flatten();
    let mut slots = BTreeMap::new();
    if let Some(raw) = t.get("slots") {
        for (name, value) in table_of(raw, "agents.slots")? {
            if !SLOT_NAMES.contains(&name.as_str()) {
                return Err(format!(
                    "unknown subagent slot `{name}`; use quick, balanced, deep or heavy"
                ));
            }
            let v = table_of(value, "agents.slots entry")?;
            for key in v.keys() {
                if !["model", "effort"].contains(&key.as_str()) {
                    return Err(format!("unknown favorite field `{key}`"));
                }
            }
            let m = v
                .get("model")
                .and_then(toml::Value::as_str)
                .ok_or("a configured favorite needs a concrete model")?;
            validate_concrete_model("favorite model", m)?;
            let default = slot_effort(name);
            let effort = v
                .get("effort")
                .map(|v| {
                    v.as_str()
                        .and_then(Effort::parse)
                        .filter(|e| *e != Effort::Auto)
                        .ok_or("favorite effort must be low, medium, high, xhigh or max")
                })
                .transpose()?
                .unwrap_or(default);
            slots.insert(
                name.clone(),
                AgentSlot {
                    model: m.into(),
                    effort,
                },
            );
        }
    }
    if mode == AgentsMode::Roster && slots.is_empty() {
        return Err("configure a favorite before enabling roster delegation".into());
    }
    Ok(AgentsConfig {
        mode,
        model,
        deadline,
        slots,
    })
}
