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
