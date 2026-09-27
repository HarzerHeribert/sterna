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
    /// Every model this role could reach before the query narrowed it: the
    /// denominator a person reads a filter against.
    pub fn catalogue_len(&self) -> usize {
        self.groups.iter().map(|g| g.models.len()).sum()
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
    pub fn choose(&self) -> Result<String, String> {
        let rows = self.candidates();
        let c = rows.get(self.selected).ok_or("No selectable model.")?;
        if !c.available {
            return Err(c
                .reason
                .clone()
                .unwrap_or_else(|| "This account is not available.".into()));
        }
        // The gateway still owns route selection. Never promise an account pin
        // when the serving protocol accepts only a concrete model here.
        if self.role == 2
            && let Some(slot) = &self.slot
        {
            return Ok(format!("/subagents {slot} {}", c.model));
        }
        Ok(format!(
            "/model {}{}",
            match self.role {
                1 => "helper ",
                2 => "subagent ",
                _ => "",
            },
            c.model
        ))
    }
}

/// The model sheet's rows: what the tier runs on now, the favourite slots on
/// the Subagents section, then every model grouped under the account that
/// serves it, and the way to turn the tier off.
pub(super) fn items(sheet: &mut super::Sheet, m: &mut Navigator) -> Vec<super::Item> {
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
        1 => m.current.helper.clone().unwrap_or_else(|| "off".into()),
        2 => m
            .current
            .subagent
            .clone()
            .unwrap_or_else(|| "favourites".into()),
        _ => m.current.parent.clone(),
    };
    let (name, purpose) = ROLES[m.role.min(2)];
    let mut items = vec![Item::info(format!("{name} {purpose}. Now: {now}"))];
    if m.role == 2 && m.target_key.is_none() {
        let slots =
            std::iter::once(None).chain(crate::config::SLOT_NAMES.iter().copied().map(Some));
        for slot in slots {
            let holds = match slot {
                Some(name) => m
                    .assignment
                    .slots
                    .get(name)
                    .map_or("empty".to_string(), |held| {
                        format!("{} · {}", held.model, held.effort.name())
                    }),
                None => m.current.subagent.clone().unwrap_or_else(|| "none".into()),
            };
            items.push(
                Item::choice(
                    format!("slot:{}", slot.unwrap_or("pinned")),
                    slot.map_or("PINNED".to_string(), str::to_uppercase),
                    m.slot.as_deref() == slot,
                    Action::Slot(slot.map(str::to_owned)),
                )
                .detail(holds),
            );
        }
        let enabled = m.assignment.mode == crate::config::AgentsMode::Roster;
        items.push(Item::toggle(
            "favourites",
            "Favourites",
            enabled,
            Action::Command(format!("/subagents {}", if enabled { "off" } else { "on" })),
        ));
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
    for (i, c) in rows.iter().enumerate() {
        if !m.measured_order && c.route != last_route {
            items.push(Item::heading(c.route.clone()));
            last_route = c.route.clone();
        }
        let score = c.score.map(|v| format!(" · ★ {v:.0}")).unwrap_or_default();
        let locked = if c.available { "" } else { " · locked" };
        let via = if m.measured_order {
            format!(" · {}", c.route)
        } else {
            String::new()
        };
        let id = format!("model:{}:{}", c.route, c.model);
        // The sheet opens on the model the tier runs on now, when that is in
        // the list; otherwise on its own current value (a slot, say).
        if i == m.selected && current.as_deref() == Some(c.model.as_str()) {
            selected = Some(id.clone());
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
    sheet.prefer = selected;
    items
}
