//! A navigator over connected accounts, not a second model catalogue or policy.
use crate::spend::Tier;
use crate::tui::{ModelGroup, Panel, TierModels};
use std::collections::BTreeMap;

pub struct Navigator {
    pub role: usize,
    pub slot: Option<String>,
    pub target_key: Option<String>,
    pub assignment: crate::config::AgentsConfig,
    pub selected: usize,
    pub query: String,
    pub all_sources: bool,
    pub measured_order: bool,
    pub current: TierModels,
    pub groups: Vec<ModelGroup>,
    pub scores: BTreeMap<String, f64>,
    pub notice: String,
}
#[derive(Debug, Clone)]
pub struct Candidate {
    pub model: String,
    pub route: String,
    pub score: Option<f64>,
    pub available: bool,
    pub reason: Option<String>,
}
impl Navigator {
    pub fn from_panel(p: &Panel) -> Option<Self> {
        let (groups, scores) = p.catalogue()?;
        let assignment = p.assignment.as_ref()?;
        let mut navigator = Self {
            role: match assignment.active {
                Tier::Parent => 0,
                Tier::Helpers => 1,
                Tier::Subagents => 2,
            },
            slot: None,
            target_key: None,
            assignment: Default::default(),
            selected: 0,
            query: String::new(),
            all_sources: false,
            measured_order: false,
            current: assignment.models.clone(),
            groups: groups.to_vec(),
            scores: scores.clone(),
            notice: String::new(),
        };
        navigator.select_current();
        Some(navigator)
    }
    pub fn select_current(&mut self) {
        let current = match self.role {
            1 => self.current.helper.as_deref(),
            2 => self
                .slot
                .as_deref()
                .and_then(|name| self.assignment.slots.get(name).map(|s| s.model.as_str()))
                .or_else(|| {
                    self.slot
                        .is_none()
                        .then_some(self.current.subagent.as_deref())
                        .flatten()
                }),
            _ => Some(self.current.parent.as_str()),
        };
        self.selected = self
            .candidates()
            .iter()
            .position(|c| Some(c.model.as_str()) == current)
            .unwrap_or(0);
    }
    /// Every model in scope before the query narrowed it -- the accounts
    /// shown, chat models only: the denominator a person reads a filter
    /// against.
    pub fn catalogue_len(&self) -> usize {
        self.groups
            .iter()
            .filter(|g| self.all_sources || g.selectable != Some(false))
            .flat_map(|g| &g.models)
            .filter(|id| crate::models::chat_capable(id))
            .count()
    }
    pub fn candidates(&self) -> Vec<Candidate> {
        // `+` joins terms as well as a space does: Space stages a choice for
        // the active tier, so it never reaches this filter, and a person
        // narrowing by provider *and* account needs some separator that does.
        let terms: Vec<_> = self
            .query
            .to_lowercase()
            .split(|c: char| c.is_whitespace() || c == '+')
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect();
        let mut out = Vec::new();
        for g in &self.groups {
            if !self.all_sources && g.selectable == Some(false) {
                continue;
            }
            for id in &g.models {
                if !crate::models::chat_capable(id) {
                    continue;
                }
                let hay = format!("{} {} {} {}", id, g.provider, g.account, g.scope).to_lowercase();
                if !terms.iter().all(|t| hay.contains(t)) {
                    continue;
                }
                let score = self
                    .scores
                    .get(&crate::models::normalise(id))
                    .copied()
                    .filter(|n| n.is_finite());
                out.push(Candidate {
                    model: id.clone(),
                    route: format!("{} · {} · {}", g.provider, g.account, g.scope),
                    score,
                    available: g.selectable != Some(false),
                    reason: g.unavailable_reason.clone(),
                });
            }
        }
        out.sort_by(|a, b| {
            // A search is answered best match first; the order below breaks
            // ties within one quality of match.
            let relevance = crate::models::search_rank(&a.model, &terms)
                .cmp(&crate::models::search_rank(&b.model, &terms));
            if !terms.is_empty() && relevance != std::cmp::Ordering::Equal {
                return relevance;
            }
            let subscription_order = (!a.route.contains("account-declared"))
                .cmp(&(!b.route.contains("account-declared")));
            if !self.measured_order && subscription_order != std::cmp::Ordering::Equal {
                return subscription_order;
            }
            if self.measured_order {
                match (a.score, b.score) {
                    (Some(x), Some(y)) => y.total_cmp(&x),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    _ => std::cmp::Ordering::Equal,
                }
                .then_with(|| a.route.cmp(&b.route))
                .then_with(|| a.model.cmp(&b.model))
            } else {
                a.route.cmp(&b.route).then_with(|| a.model.cmp(&b.model))
            }
        });
        out
    }
    /// The picker's own record of a choice the session was just sent, so
    /// the mark moves at once; returns the notice that names it. On a
    /// favourite slot it moves on to the next empty one.
    pub fn chosen(&mut self, model: &str) -> String {
        use crate::config::{AgentSlot, AgentsMode, SLOT_NAMES};
        let notice = match (self.role, self.slot.clone()) {
            (2, Some(slot)) => {
                let effort = self
                    .assignment
                    .slots
                    .get(&slot)
                    .map_or_else(|| crate::config::slot_effort(&slot), |held| held.effort);
                self.assignment.slots.insert(
                    slot.clone(),
                    AgentSlot {
                        model: model.to_string(),
                        effort,
                    },
                );
                let mut notice =
                    format!("{} is now {model} · {}", slot.to_uppercase(), effort.name());
                if self.assignment.mode != AgentsMode::Roster {
                    notice.push_str(" · favourites are off: turn them on above");
                }
                self.slot = SLOT_NAMES
                    .iter()
                    .find(|name| !self.assignment.slots.contains_key(**name))
                    .map_or(Some(slot), |name| Some((*name).to_string()));
                notice
            }
            (2, None) => {
                self.current.subagent = Some(model.to_string());
                self.assignment.mode = AgentsMode::Pinned;
                self.assignment.model = Some(model.to_string());
                format!("Every subagent now runs on {model}")
            }
            (1, _) => {
                self.current.helper = Some(model.to_string());
                format!("Helper is now {model}")
            }
            _ => {
                self.current.parent = model.to_string();
                format!("Main is now {model}")
            }
        };
        self.select_current();
        notice
    }
    /// The picker's own record of one of its rows' commands, sent to the
    /// session: favourites on or off, a slot emptied, a tier turned off.
    /// Like [`chosen`](Self::chosen), it moves the marks at once; returns
    /// the notice that says what changed, or `None` for a command it does
    /// not record.
    pub fn sent(&mut self, command: &str) -> Option<String> {
        use crate::config::AgentsMode;
        let words: Vec<&str> = command.split_whitespace().collect();
        let notice = match words.as_slice() {
            ["/subagents", "on"] => {
                self.current.subagent = Some("favourites".into());
                self.assignment.mode = AgentsMode::Roster;
                self.assignment.model = None;
                "Favourites are on".to_string()
            }
            ["/subagents", "off"] => {
                self.current.subagent = Some("off".into());
                self.assignment.mode = AgentsMode::Off;
                self.assignment.model = None;
                "Subagents are off".to_string()
            }
            ["/subagents", slot, "off"] => {
                self.assignment.slots.remove(*slot);
                if self.assignment.slots.is_empty() && self.assignment.mode == AgentsMode::Roster {
                    self.current.subagent = Some("off".into());
                    self.assignment.mode = AgentsMode::Off;
                }
                format!("{} is empty", slot.to_uppercase())
            }
            ["/model", "helper", "off"] => {
                self.current.helper = Some("off".into());
                "Helpers are off".to_string()
            }
            ["/model", "subagent", "off"] => {
                self.current.subagent = Some("off".into());
                self.assignment.mode = AgentsMode::Off;
                self.assignment.model = None;
                "Subagents are off".to_string()
            }
            _ => return None,
        };
        self.select_current();
        Some(notice)
    }
    /// The model on the row the picker is on, when it can be chosen.
    pub fn selected_model(&self) -> Option<String> {
        self.candidates()
            .get(self.selected)
            .filter(|c| c.available)
            .map(|c| c.model.clone())
    }
    pub fn choose(&self) -> Result<String, String> {
        let rows = self.candidates();
        let c = rows.get(self.selected).ok_or("No selectable model.")?;
        if !c.available {
            return Err(c
                .reason
                .clone()
                .unwrap_or_else(|| "This account is not available.".into()));
        }
        Ok(self.command_for(&c.model))
    }
    /// The command that gives `model` to the tier or slot the picker is on.
    pub fn command_for(&self, model: &str) -> String {
        // The gateway still owns route selection. Never promise an account pin
        // when the serving protocol accepts only a concrete model here.
        if self.role == 2
            && let Some(slot) = &self.slot
        {
            let effort = self
                .assignment
                .slots
                .get(slot)
                .map_or_else(|| crate::config::slot_effort(slot), |held| held.effort);
            return format!("/subagents {slot} {model} {}", effort.name());
        }
        format!(
            "/model {}{model}",
            match self.role {
                1 => "helper ",
                2 => "subagent ",
                _ => "",
            }
        )
    }
}

/// A route as a person reads it: the provider, the account, and how the
/// account is reached -- not the catalogue's bookkeeping word for where its
/// model list came from. The route itself stays the rows' identity.
fn route_words(route: &str) -> String {
    let mut parts: Vec<&str> = route.split(" · ").collect();
    if let Some(scope) = parts.last_mut() {
        *scope = match *scope {
            "account-declared" => "subscription",
            "provider-declared" => "API key",
            "unknown" => "model list not read yet",
            other => other,
        };
    }
    parts.join(" · ")
}

/// The model sheet's rows: what the tier runs on now, the favourite slots on
/// the Subagents section, then every model grouped under the account that
/// serves it, and the way to turn the tier off.
pub(super) fn items(
    sheet: &mut super::Sheet,
    m: &mut Navigator,
    effort: crate::wire::Effort,
) -> Vec<super::Item> {
    use super::{Action, Item};
    const ROLES: [(&str, &str); 3] = [
        ("Main", "answers you"),
        ("Helper", "reads and summarises for Main"),
        ("Subagents", "work in parallel"),
    ];
    if sheet.sections.is_empty() {
        sheet.query = Some(m.query.clone());
    }
    sheet.sections = if m.target_key.is_some() {
        vec![ROLES[m.role.min(2)].0.to_string()]
    } else {
        ROLES.iter().map(|(name, _)| (*name).to_string()).collect()
    };
    if m.target_key.is_some() {
        sheet.section = 0;
    } else {
        sheet.section = m.role.min(2);
    }
    let query = sheet.query.clone().unwrap_or_default();
    if query != m.query {
        m.query = query;
        m.selected = 0;
    }
    sheet.title = "Models".into();
    sheet.crumbs = vec![ROLES[m.role.min(2)].0.to_string()];
    sheet.tools = vec![
        (
            if m.all_sources {
                "all accounts".to_string()
            } else {
                "connected accounts".to_string()
            },
            Action::Sources,
            m.all_sources,
        ),
        (
            if m.measured_order {
                "order: intelligence".to_string()
            } else {
                "order: name".to_string()
            },
            Action::Scores,
            m.measured_order,
        ),
    ];
    sheet.total = Some(m.catalogue_len());
    sheet.matched = Some(m.candidates().len());
    if sheet.notice.is_empty() && !m.notice.is_empty() {
        sheet.notice = std::mem::take(&mut m.notice);
    }
    let now = match m.role {
        1 => m.current.helper.clone(),
        2 => m.current.subagent.clone(),
        _ => Some(m.current.parent.clone()).filter(|parent| !parent.is_empty()),
    }
    .unwrap_or_else(|| "not chosen yet".into());
    let (name, purpose) = ROLES[m.role.min(2)];
    let mut items = vec![Item::info(format!("{name} {purpose}. Now: {now}"))];
    // Main's effort sits under its model: how that model works, chosen in
    // the same place. A search is for a model, so the row steps aside.
    if m.role == 0 && m.target_key.is_none() && m.query.is_empty() {
        const EFFORTS: [crate::wire::Effort; 6] = [
            crate::wire::Effort::Auto,
            crate::wire::Effort::Low,
            crate::wire::Effort::Medium,
            crate::wire::Effort::High,
            crate::wire::Effort::Xhigh,
            crate::wire::Effort::Max,
        ];
        items.push(
            Item::value(
                "main:effort",
                "Effort",
                EFFORTS
                    .iter()
                    .map(|e| {
                        (
                            e.name().to_string(),
                            Action::Command(format!("/effort {}", e.name())),
                        )
                    })
                    .collect(),
                EFFORTS.iter().position(|e| *e == effort),
            )
            .detail("auto lets the model choose; higher thinks longer and costs more"),
        );
    }
    if m.role == 2 && m.target_key.is_none() {
        // The pinned model, or none: never the favourites' word.
        let pinned = m
            .current
            .subagent
            .clone()
            .filter(|word| !matches!(word.as_str(), "off" | "favourites"));
        // `● now` is what subagents run on; the row a model click fills is
        // said in words, so an empty slot never reads as the one in use.
        let next = " · the next model you choose goes here";
        items.push(
            Item::choice(
                "slot:pinned",
                "PINNED",
                pinned.is_some(),
                Action::Slot(None),
            )
            .detail(format!(
                "{} · every subagent runs on one model; Main picks the effort per job{}",
                pinned.as_deref().unwrap_or("none"),
                if m.slot.is_none() { next } else { "" }
            )),
        );
        for slot in crate::config::SLOT_NAMES {
            let held = m.assignment.slots.get(slot);
            let filling = if m.slot.as_deref() == Some(slot) {
                next
            } else {
                ""
            };
            items.push(
                Item::choice(
                    format!("slot:{slot}"),
                    slot.to_uppercase(),
                    false,
                    Action::Slot(Some(slot.to_string())),
                )
                .detail(held.map_or_else(
                    || format!("empty · choose it, then a model below{filling}"),
                    |held| format!("{}{filling}", held.model),
                )),
            );
            // Each favourite's effort sits under it, one click from any value.
            const EFFORTS: [crate::wire::Effort; 5] = [
                crate::wire::Effort::Low,
                crate::wire::Effort::Medium,
                crate::wire::Effort::High,
                crate::wire::Effort::Xhigh,
                crate::wire::Effort::Max,
            ];
            let current = held.and_then(|held| EFFORTS.iter().position(|e| *e == held.effort));
            items.push(
                Item::value(
                    format!("slot:{slot}:effort"),
                    "effort",
                    EFFORTS
                        .iter()
                        .map(|e| {
                            (
                                e.name().to_string(),
                                Action::SlotEffort(slot.to_string(), e.name().to_string()),
                            )
                        })
                        .collect(),
                    current,
                )
                .disabled(
                    held.is_none()
                        .then(|| "Choose this favourite's model first".to_string()),
                ),
            );
        }
        let enabled = m.assignment.mode == crate::config::AgentsMode::Roster;
        let empty = m.assignment.slots.is_empty();
        items.push(
            Item::toggle(
                "favourites",
                "Favourites",
                enabled,
                Action::Command(format!("/subagents {}", if enabled { "off" } else { "on" })),
            )
            .detail("Subagents choose among the filled favourites.")
            .disabled((empty && !enabled).then(|| "Fill a favourite first".to_string())),
        );
    }
    let current = match m.role {
        1 => m.current.helper.clone(),
        2 => m
            .slot
            .as_deref()
            .and_then(|name| m.assignment.slots.get(name).map(|s| s.model.clone()))
            .or_else(|| {
                m.slot
                    .is_none()
                    .then(|| m.current.subagent.clone())
                    .flatten()
            }),
        _ => Some(m.current.parent.clone()),
    };
    let rows = m.candidates();
    let mut last_route = String::new();
    let mut selected = None;
    // Without the current model in the list, the sheet opens on the first
    // model a person can choose -- never on the effort row above them, so
    // Enter chooses a model as it always has.
    let mut first = None;
    for (i, c) in rows.iter().enumerate() {
        if !m.measured_order && c.route != last_route {
            items.push(Item::heading(route_words(&c.route)));
            last_route = c.route.clone();
            // A locked account's models stay listed and muted; its one way
            // in is a row of its own, not a click on each model.
            if !c.available {
                let mut parts = c.route.split(" · ");
                let provider = parts.next().unwrap_or_default().to_string();
                let account = parts.next().unwrap_or_default().to_string();
                items.push(Item::run(
                    format!("signin:{account}"),
                    format!("Sign in to {provider}"),
                    Action::Command(format!("/login {account}")),
                ));
            }
        }
        let score = c.score.map(|v| format!(" · ★ {v:.0}")).unwrap_or_default();
        let locked = if c.available { "" } else { " · locked" };
        let via = if m.measured_order {
            format!(" · {}", route_words(&c.route))
        } else {
            String::new()
        };
        let id = format!("model:{}:{}", c.route, c.model);
        // The sheet opens on the model the tier runs on now, when that is in
        // the list; otherwise on its own current value (a slot, say).
        if i == m.selected && current.as_deref() == Some(c.model.as_str()) {
            selected = Some(id.clone());
        }
        if first.is_none() && c.available {
            first = Some(id.clone());
        }
        items.push(
            Item::choice(
                id,
                format!("{}{locked}{score}{via}", c.model),
                current.as_deref() == Some(c.model.as_str()),
                Action::Model(i),
            )
            .disabled((!c.available).then(|| {
                c.reason
                    .clone()
                    .unwrap_or_else(|| "This account is not available.".into())
            })),
        );
    }
    if rows.is_empty() {
        items.push(Item::info(
            "No models match. Backspace removes a letter; Esc clears the search.",
        ));
    }
    let off = if m.target_key.is_some() {
        Some(("Use the inherited value".to_string(), Action::UnsetModel))
    } else if m.role == 2 && m.slot.is_some() {
        Some((
            "Empty this slot".to_string(),
            Action::Command(format!(
                "/subagents {} off",
                m.slot.clone().unwrap_or_default()
            )),
        ))
    } else if m.role > 0 {
        Some((
            "Turn this tier off".to_string(),
            Action::Command(format!(
                "/model {} off",
                if m.role == 1 { "helper" } else { "subagent" }
            )),
        ))
    } else {
        None
    };
    if let Some((text, action)) = off {
        items.push(Item::run("off", text, action));
    }
    sheet.prefer = selected.or(first.filter(|_| m.role == 0));
    items
}
