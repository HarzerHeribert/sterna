//! Local session controls and scrollable command panels.
mod picker;

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::spend::Tier;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph},
};

/// The request mode the sandbox enforces; one type, so the mode shown is the
/// mode enforced.
pub use crate::sandbox::modes::RequestMode as Mode;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StatusLine {
    #[default]
    Full,
    Compact,
    Hidden,
}
#[derive(Debug, Clone, Default)]
pub struct Panel {
    pub title: String,
    pub rows: Vec<PanelRow>,
    pub selected: usize,
    pub search: Option<PanelSearch>,
    /// Present only on the model panel: which tier a chosen model is being
    /// assigned to, and what all three run on now.
    pub assignment: Option<Assignment>,
    /// Choices made but not yet applied, one per tier — `Space` puts one
    /// here, `Enter` applies every one of them, `Esc` throws them all away.
    ///
    /// **A session is three models and the panel used to only let you change
    /// one.** `Enter` applied the highlighted row and closed, so setting a
    /// parent and a helper meant opening the panel twice and thinking about
    /// the order. Staging is also what makes `Esc` mean something: before it,
    /// there was nothing to discard.
    pub staged: BTreeMap<Tier, String>,
}

/// What picking a model in this panel will do, and to which tier.
///
/// A session is three models, not one. Without this the panel could only
/// ever set the parent, and the other two tiers existed solely in a file
/// most people never open — so most people never met them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// The tier Enter assigns to. Tab moves it.
    pub active: Tier,
    pub models: TierModels,
}

/// What each tier runs on right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TierModels {
    pub parent: String,
    /// `None` when helpers are off, which is a state rather than a missing
    /// value: no helper model means no helper ever runs.
    pub helper: Option<String>,
    /// `None` when a delegated goal inherits the parent's model.
    pub subagent: Option<String>,
}

impl TierModels {
    /// One tier's model, and the word for having none.
    #[must_use]
    pub fn describe(&self, tier: Tier) -> &str {
        match tier {
            Tier::Parent => &self.parent,
            Tier::Helpers => self.helper.as_deref().unwrap_or("off"),
            Tier::Subagents => self.subagent.as_deref().unwrap_or("auto"),
        }
    }
}
/// How the catalogue is ordered — `^O` cycles it.
///
/// **Alphabetical is the wrong default and was the shipped one.** A flat sort
/// puts `claude-3-5-haiku-20241022` above `claude-opus-5`, so the strongest
/// model in a provider's list sits below its weakest and the order carries no
/// information a person wanted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Order {
    /// By identifier. The order a person can predict without any catalogue.
    #[default]
    Name,
    /// By Artificial Analysis' intelligence index, strongest first.
    ///
    /// A model the catalogue does not measure sorts **last and prints no
    /// score**, never as a zero: an unmeasured model and a weak one are
    /// different facts and a zero would state the second.
    Intelligence,
}

impl Order {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::Intelligence,
            Self::Intelligence => Self::Name,
        }
    }
}

/// The lookup name for a model id.
///
/// Deliberately the same one line as `glasshouse::routing::analysis::normalise`,
/// which is the source of truth and which this crate cannot call: `sterna` links
/// against no part of Glasshouse and reaches it only as a binary. Duplicating
/// one `replace` is the cheaper of the two costs.
fn normalise(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace(['.', '_'], "-")
}

#[derive(Debug, Clone, Default)]
pub struct PanelSearch {
    query: String,
    source: Vec<ModelGroup>,
    matched: Vec<ModelGroup>,
    providers: Vec<String>,
    active: usize,
    choices: Vec<usize>,
    order: Order,
    /// Normalised model name to its published intelligence index. Empty when
    /// no catalogue was available, which is the state the panel ships in and
    /// which [`Order::Intelligence`] renders as an unmeasured list rather than
    /// as an empty one.
    intelligence: BTreeMap<String, f64>,
    details: BTreeMap<usize, ModelDetail>,
}
#[derive(Debug, Clone)]
struct ModelDetail {
    id: String,
    route: String,
    score: Option<f64>,
    unavailable_reason: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub struct ModelGroup {
    pub provider: String,
    pub account: String,
    pub scope: String,
    pub models: Vec<String>,
    pub selectable: Option<bool>,
    pub unavailable_reason: Option<String>,
    /// The provider whose login flow would connect this account, when it is
    /// connectable and not yet connected. `None` for every other row.
    pub connect: Option<String>,
    /// A subscription account: `Some(true)` when it is in its provider's
    /// pool, `Some(false)` when the person took it out. `None` for an
    /// account of any other kind, which is listed on its own as before.
    pub pooled: Option<bool>,
    /// The plan and the last usage reading, for a subscription's row.
    pub note: Option<String>,
}
#[derive(Debug, Clone)]
pub struct PanelRow {
    pub text: String,
    /// A user-selected local slash command, never model-supplied executable text.
    pub command: Option<String>,
}

impl Panel {
    /// Raw catalogue facts for the native workbench; no legacy picker state.
    pub(crate) fn catalogue(&self) -> Option<(&[ModelGroup], &BTreeMap<String, f64>)> {
        self.search
            .as_ref()
            .map(|s| (s.source.as_slice(), &s.intelligence))
    }

    pub fn text(title: impl Into<String>, text: impl AsRef<str>) -> Self {
        Self {
            title: title.into(),
            rows: text
                .as_ref()
                .lines()
                .map(|line| PanelRow {
                    text: line.into(),
                    command: None,
                })
                .collect(),
            selected: 0,
            search: None,
            assignment: None,
            staged: BTreeMap::new(),
        }
    }

    /// A panel whose rows the caller built, including any that are selectable
    /// because they carry a command.
    ///
    /// [`Self::text`] splits prose into inert lines; this is for a list whose
    /// entries are meant to be chosen.
    pub fn rows(title: impl Into<String>, rows: Vec<PanelRow>) -> Self {
        Self {
            title: title.into(),
            rows,
            selected: 0,
            search: None,
            assignment: None,
            staged: BTreeMap::new(),
        }
    }

    pub fn models(
        title: impl Into<String>,
        mut groups: Vec<ModelGroup>,
        models: TierModels,
    ) -> Self {
        groups.sort_by(|a, b| {
            (&a.provider, &a.account, &a.scope).cmp(&(&b.provider, &b.account, &b.scope))
        });
        for group in &mut groups {
            group.models.retain(|id| {
                !id.is_empty() && !id.chars().any(|c| c.is_whitespace() || c.is_control())
            });
            group.models.sort();
            group.models.dedup();
        }
        // The title states all three tiers, because it is the one part of the
        // panel that also reaches the piped path, where nothing rendered is
        // drawn at all.
        let summary = Tier::every()
            .map(|tier| format!("{} {}", tier.singular(), models.describe(tier)))
            .join(" · ");
        let mut panel = Self {
            title: format!("{} · {summary}", title.into()),
            search: Some(PanelSearch {
                source: groups,
                ..PanelSearch::default()
            }),
            assignment: Some(Assignment {
                active: Tier::Parent,
                models,
            }),
            ..Self::default()
        };
        panel.filter();
        if let Some(search) = panel.search.as_mut()
            && let Some(provider) = search
                .matched
                .iter()
                .find(|group| group.selectable != Some(false))
                .map(|group| group.provider.clone())
            && let Some(index) = search
                .providers
                .iter()
                .position(|candidate| candidate == &provider)
        {
            search.active = index;
            panel.provider_rows();
        }
        panel
    }

    /// Attaches the published measurements the catalogue is ordered by.
    ///
    /// A builder rather than a fourth argument to [`Self::models`]: the panel
    /// is useful without a catalogue, every existing caller predates it, and
    /// the fetch that produces one is allowed to fail.
    #[must_use]
    pub fn with_intelligence(mut self, intelligence: BTreeMap<String, f64>) -> Self {
        if let Some(search) = self.search.as_mut() {
            // **A catalogue that exists is the order the panel opens in.**
            // Leaving the alphabet as the default made the measurements a
            // thing you had to know to press `^O` for, which is the same as
            // not having them. With no catalogue there is nothing to order
            // by and the alphabet is the only honest answer.
            search.order = if intelligence.is_empty() {
                Order::Name
            } else {
                Order::Intelligence
            };
            search.intelligence = intelligence;
        }
        self.provider_rows();
        self
    }

    /// Stages the highlighted row for the active tier, applying nothing.
    ///
    /// Staging the tier's *current* model is not an edit and is dropped, so
    /// `Enter` after an accidental `Space` submits nothing rather than
    /// re-assigning what is already there.
    pub fn stage(&mut self) -> Option<String> {
        let assignment = self.assignment.as_ref()?;
        let tier = assignment.active;
        let command = self.rows.get(self.selected)?.command.clone()?;
        if !command.starts_with("/model ") {
            return None;
        }
        // The last word of the command is the value in every form the panel
        // emits -- a model id, `off`, `inherit` -- which is why the label is
        // taken from there rather than parsed back out of the row's text.
        let chosen = command.rsplit(' ').next().unwrap_or_default().to_string();
        let current = assignment.models.describe(tier).to_string();

        // **Space toggles, and every press changes something visible.** The
        // first version staged only, and silently did nothing at all when the
        // row already held the tier's current value -- so pressing Space on
        // `OFF` with helpers already off looked like a dead key. There are
        // three outcomes and each has a marker on the row it acts on:
        let said = if self.staged.get(&tier) == Some(&command) {
            // Pressing the staged row again takes it back.
            self.staged.remove(&tier);
            format!("unstaged {} · stays {current}", tier.singular())
        } else if current == chosen {
            // The row that *is* the current value means "leave this tier
            // alone", which is how a staged change is reverted without
            // remembering what it used to be.
            self.staged.remove(&tier);
            format!("{} stays {current}", tier.singular())
        } else {
            self.staged.insert(tier, command);
            format!(
                "staged {} {current} → {chosen} · ⏎ applies {}",
                tier.singular(),
                match self.staged.len() {
                    1 => "it".to_string(),
                    n => format!("all {n}"),
                }
            )
        };
        // The rows are rebuilt so the markers move, and `provider_rows`
        // resets the cursor to the first choice -- which would throw you back
        // to the top of 469 models on every press. The selection is the one
        // thing staging must not disturb.
        let was = self.selected;
        self.provider_rows();
        self.selected = was;
        Some(said)
    }

    /// Every staged assignment, in tier order. Empty when nothing is staged,
    /// which is what makes `Enter` fall back to the highlighted row.
    #[must_use]
    pub fn staged_commands(&self) -> Vec<String> {
        self.staged.values().cloned().collect()
    }

    /// Cycles the catalogue order. `^O` rather than a letter, because this
    /// panel's plain keys are its search box.
    pub fn cycle_order(&mut self) -> bool {
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        let order = search.order.next();
        self.select_order(order)
    }

    pub(crate) fn select_order(&mut self, order: Order) -> bool {
        let selected = self
            .rows
            .get(self.selected)
            .and_then(|row| row.command.clone());
        let identity = self
            .search
            .as_ref()
            .and_then(|search| search.details.get(&self.selected))
            .map(|detail| (detail.id.clone(), detail.route.clone()));
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        search.order = order;
        self.provider_rows();
        let same_model = identity.and_then(|(id, route)| {
            self.search
                .as_ref()?
                .details
                .iter()
                .find(|(_, detail)| detail.id == id && detail.route == route)
                .map(|(index, _)| *index)
        });
        if let Some(index) = same_model.or_else(|| {
            self.rows
                .iter()
                .position(|row| row.command.is_some() && row.command == selected)
        }) {
            self.selected = index;
        }
        true
    }

    /// Moves the assignment to the next tier, wrapping.
    ///
    /// The rows do not change -- the same catalogue serves all three tiers --
    /// but the command each row carries does, which is the whole mechanism.
    pub fn cycle_tier(&mut self) -> bool {
        let Some(assignment) = self.assignment.as_mut() else {
            return false;
        };
        assignment.active = match assignment.active {
            Tier::Parent => Tier::Subagents,
            Tier::Subagents => Tier::Helpers,
            Tier::Helpers => Tier::Parent,
        };
        self.provider_rows();
        true
    }

    /// The tier a chosen row would be assigned to.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.assignment
            .as_ref()
            .map_or(Tier::Parent, |assignment| assignment.active)
    }

    pub fn move_provider(&mut self, forward: bool) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        search.active = if forward {
            (search.active + 1).min(search.providers.len().saturating_sub(1))
        } else {
            search.active.saturating_sub(1)
        };
        self.provider_rows();
    }

    pub fn search_insert(&mut self, text: &str) -> bool {
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        search
            .query
            .extend(text.chars().filter(|c| !c.is_control()));
        self.filter();
        true
    }

    pub fn search_backspace(&mut self) {
        if let Some(search) = self.search.as_mut() {
            search.query.pop();
            self.filter();
        }
    }

    pub fn search_clear(&mut self) {
        if let Some(search) = self.search.as_mut() {
            search.query.clear();
            self.filter();
        }
    }

    fn filter(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        let previous = search.providers.get(search.active).cloned();
        let query = search.query.to_lowercase();
        // Terms split on `+` as well as whitespace, because `Space` now
        // stages a choice and no longer reaches this box. Multi-term
        // filtering is not a nicety here: two accounts on one provider can
        // offer the same model id, and the account name is the only thing
        // that tells them apart -- `openrouter+work+303`.
        let terms: Vec<_> = query
            .split(|c: char| c.is_whitespace() || c == '+')
            .filter(|term| !term.is_empty())
            .collect();
        search.matched = search
            .source
            .iter()
            .filter_map(|group| {
                let prefix = format!("{} {}", group.provider, group.account).to_lowercase();
                let models: Vec<_> = group
                    .models
                    .iter()
                    .filter(|id| {
                        let haystack = format!("{prefix} {}", id.to_lowercase());
                        terms.iter().all(|term| haystack.contains(term))
                    })
                    .cloned()
                    .collect();
                (!models.is_empty()).then(|| ModelGroup {
                    models,
                    ..group.clone()
                })
            })
            .collect();
        search.providers = search
            .matched
            .iter()
            .map(|group| group.provider.clone())
            .collect();
        search.providers.dedup();
        search.providers.push("All providers".into());
        search.active = previous
            .and_then(|name| search.providers.iter().position(|p| p == &name))
            .unwrap_or(0);
        self.provider_rows();
    }

    fn provider_rows(&mut self) {
        let tier = self.tier();
        // Read before the search borrow: a row says whether it is what the
        // tier runs now and whether it is what `Enter` would apply, so that
        // `Space` has an effect under the cursor and not only in a notice
        // drawn below the panel, where it was easy to miss entirely.
        let current = self
            .assignment
            .as_ref()
            .map(|assignment| assignment.models.describe(tier).to_string());
        let staged_here = self.staged.get(&tier).cloned();
        let mark = |command: &str| match (&staged_here, &current) {
            (Some(staged), _) if staged == command => "  ◆ STAGED",
            (_, Some(now)) if Some(now.as_str()) == command.rsplit(' ').next() => "  · NOW",
            _ => "",
        };
        let Some(search) = &mut self.search else {
            return;
        };
        self.rows.clear();
        search.choices.clear();
        search.details.clear();
        let clearing: &[(&str, &str)] = match tier {
            Tier::Parent => &[],
            Tier::Helpers => &[("Off", "/model helper off")],
            Tier::Subagents => &[
                ("Auto", "/model subagent auto"),
                ("Off", "/model subagent off"),
            ],
        };
        for &(text, command) in clearing {
            search.choices.push(self.rows.len());
            self.rows.push(PanelRow {
                text: format!("{text}{}", mark(command)),
                command: Some(command.into()),
            });
        }
        let mut entries = Vec::new();
        // A provider's subscription accounts are one pool (2026-09-23): each
        // account is a row that ⏎ takes in or out of it, and the models are
        // listed once, for every account in it -- under a synthetic group
        // built here, since no single account is the route.
        let mut pools: BTreeMap<String, ModelGroup> = BTreeMap::new();
        let mut pool_header: Option<String> = None;
        for group in search.matched.iter().filter(|group| {
            search
                .providers
                .get(search.active)
                .is_some_and(|provider| provider == "All providers" || provider == &group.provider)
        }) {
            if let Some(provider) = &group.connect {
                search.choices.push(self.rows.len());
                self.rows.push(PanelRow {
                    text: format!("Connect {} · {provider}", group.account),
                    command: Some(format!("/login {}", group.account)),
                });
                continue;
            }
            if let Some(in_pool) = group.pooled {
                if pool_header.as_deref() != Some(group.provider.as_str()) {
                    self.rows.push(PanelRow {
                        text: format!(
                            "{} · accounts · ⏎ takes one in or out of the pool",
                            group.provider
                        ),
                        command: None,
                    });
                    pool_header = Some(group.provider.clone());
                }
                search.choices.push(self.rows.len());
                self.rows.push(PanelRow {
                    text: format!(
                        "  {} {}{}",
                        if in_pool { "●" } else { "○" },
                        group.account,
                        match (&group.note, in_pool) {
                            (_, false) => " — out of the pool".to_string(),
                            (Some(note), true) => format!(" — {note}"),
                            (None, true) => String::new(),
                        }
                    ),
                    command: Some(format!(
                        "/pool {} {}",
                        group.account,
                        if in_pool { "exclude" } else { "include" }
                    )),
                });
                if in_pool {
                    let pool = pools
                        .entry(group.provider.clone())
                        .or_insert_with(|| ModelGroup {
                            provider: group.provider.clone(),
                            account: "pool".into(),
                            scope: "served by every account in the pool".into(),
                            selectable: Some(true),
                            ..ModelGroup::default()
                        });
                    pool.models.extend(group.models.iter().cloned());
                }
                continue;
            }
            for id in &group.models {
                let score = search
                    .intelligence
                    .get(&normalise(id))
                    .copied()
                    .filter(|score| score.is_finite() && *score >= 0.0);
                entries.push((group, id, score));
            }
        }
        for pool in pools.values_mut() {
            pool.models.sort();
            pool.models.dedup();
        }
        for pool in pools.values() {
            for id in &pool.models {
                let score = search
                    .intelligence
                    .get(&normalise(id))
                    .copied()
                    .filter(|score| score.is_finite() && *score >= 0.0);
                entries.push((pool, id, score));
            }
        }
        entries.sort_by(|(a, x, sx), (b, y, sy)| {
            let name = (&a.provider, &a.account, x).cmp(&(&b.provider, &b.account, y));
            if search.order == Order::Name {
                return name;
            }
            match (sx, sy) {
                (Some(x), Some(y)) => y.partial_cmp(x).unwrap_or(Ordering::Equal).then(name),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => name,
            }
        });
        let mut last_group = None;
        for (group, id, score) in entries {
            let route = format!("{} · {}", group.provider, group.account);
            if search.order == Order::Name && last_group.as_ref() != Some(&route) {
                self.rows.push(PanelRow {
                    text: format!(
                        "{route} · {}",
                        group.unavailable_reason.as_deref().unwrap_or(&group.scope)
                    ),
                    command: None,
                });
                last_group = Some(route.clone());
            }
            let command = (group.selectable != Some(false)).then(|| match tier {
                Tier::Parent => format!("/model {id}"),
                assigned => format!("/model {} {id}", assigned.singular()),
            });
            let index = self.rows.len();
            search.choices.push(index);
            search.details.insert(
                index,
                ModelDetail {
                    id: id.clone(),
                    route,
                    score,
                    unavailable_reason: group.unavailable_reason.clone(),
                },
            );
            self.rows.push(PanelRow {
                text: format!(
                    "  {}{id}{}{}",
                    if command.is_none() { "× " } else { "" },
                    score.map_or_else(String::new, |score| format!("   AA {score:.0}")),
                    command.as_deref().map_or("", &mark)
                ),
                command,
            });
        }
        if self.rows.len() == clearing.len() {
            self.rows.push(PanelRow {
                text: "No models match. Ctrl-U clears search.".into(),
                command: None,
            });
        }
        self.selected = search.choices.first().copied().unwrap_or(0);
    }

    pub fn move_selection(&mut self, forward: bool, count: usize) {
        for _ in 0..count {
            let next = if forward {
                (self.selected + 1..self.rows.len()).find(|&i| {
                    self.search
                        .as_ref()
                        .is_none_or(|search| search.choices.contains(&i))
                })
            } else {
                (0..self.selected).rev().find(|&i| {
                    self.search
                        .as_ref()
                        .is_none_or(|search| search.choices.contains(&i))
                })
            };
            if let Some(next) = next {
                self.selected = next
            } else {
                break;
            }
        }
    }
}
/// The pill vocabulary, the twin of Glasshouse's `shell::hotspot`.
///
/// **A resting affordance, not only a selected one.** Every surface in either
/// product already drew *selection*; none drew "this is a thing you can
/// press", so an unselected row and a line of prose were the same pixels.
/// A pill is `[`, a one-cell marker, the label, a pad and `]`: the marker slot
/// is why the width never changes as the cursor moves, and brackets rather
/// than half blocks because both half blocks are East Asian *Ambiguous* and a
/// CJK terminal may draw them two cells wide.
///
/// Design: `archive/glasshouse:docs/product/glasshouse/tui-actionables.md`.
const CAP_LEFT: &str = "[";
const CAP_RIGHT: &str = "]";
const MARK_FOCUSED: &str = "▸";
const MARK_RESTING: &str = " ";

/// One actionable drawn as a button: focused fills with the accent, resting
/// fills with the dock so it still reads as pressable.
fn pill(label: &str, focused: bool, theme: super::Theme) -> (String, Style) {
    let marker = if focused { MARK_FOCUSED } else { MARK_RESTING };
    let style = if focused {
        Style::default().bg(theme.accent()).fg(Color::Black)
    } else {
        Style::default().bg(theme.dock()).fg(theme.accent())
    };
    (format!("{CAP_LEFT}{marker}{label} {CAP_RIGHT}"), style)
}

pub(super) fn render_panel(frame: &mut Frame, area: Rect, panel: &Panel, theme: super::Theme) {
    if panel.search.is_some() {
        return picker::render(frame, area, panel, theme);
    }
    // Two double rules, not a box. A closed frame was tried and boxed the
    // panel off from the session it is drawn over; the weight belongs in the
    // rules themselves.
    let block = if panel.search.is_some() {
        let hint = if area.width >= 100 {
            " ↑↓/CLICK MOVE · ⎵ STAGE/UNSTAGE · ⏎ APPLY · TAB TIER · ^O ORDER · ESC DISCARDS · SELECT TEXT: SHIFT "
        } else {
            " ↑↓ MOVE · ⎵ STAGE/UNSTAGE · ⏎ APPLY · TAB TIER · ^O ORDER · ESC DISCARDS "
        };
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(theme.accent()))
            .title(format!(" {} ", panel.title))
            .title_bottom(hint)
    } else {
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(theme.accent()))
            .title(format!(
                " {} · ↑↓ SCROLL · ⏎ SELECT · ESC CLOSE ",
                panel.title
            ))
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let start = panel
        .selected
        .saturating_sub(usize::from(inner.height).saturating_sub(1));
    let rows: Vec<_> = panel
        .rows
        .iter()
        .enumerate()
        .skip(start)
        .take(usize::from(inner.height))
        .map(|(i, row)| {
            let focused = i == panel.selected;
            // A row that carries a command is a button; a group heading, a
            // "no models match" note and a locked entry are prose, and
            // drawing prose as a button is exactly the confusion the pill
            // exists to remove. The theme rows keep their own swatch colour,
            // which is the one place the label is the value.
            let Some(command) = row.command.as_deref() else {
                let text = format!("{} {}", if focused { "›" } else { " " }, row.text);
                return Line::styled(
                    super::abbreviate(&text, inner.width as usize),
                    Style::default().fg(if focused {
                        theme.accent()
                    } else {
                        Color::White
                    }),
                );
            };
            let (text, mut style) = pill(&row.text, focused, theme);
            if let Some(swatch) = command
                .strip_prefix("/theme ")
                .and_then(super::Theme::parse)
                && !focused
            {
                style = style.fg(swatch.accent());
            }
            Line::styled(super::abbreviate(&text, inner.width as usize), style)
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tabs to `tier` the way the keyboard does.
    fn to_tier(panel: &mut Panel, tier: Tier) -> bool {
        for _ in 0..3 {
            if panel.tier() == tier {
                return true;
            }
            panel.cycle_tier();
        }
        panel.tier() == tier
    }
    use ratatui::{Terminal, backend::TestBackend};

    fn catalogue() -> Panel {
        Panel::models(
            "Models",
            (0..5)
                .map(|i| ModelGroup {
                    provider: format!("provider-{i}"),
                    account: format!("account-{i}"),
                    scope: "declared".into(),
                    models: vec![format!("model-{i}/exact"), "shared/flash".into()],
                    selectable: None,
                    unavailable_reason: None,
                    connect: None,
                    pooled: None,
                    note: None,
                })
                .collect(),
            TierModels::default(),
        )
    }

    #[test]
    fn carousel_search_crosses_providers_and_preserves_exact_selection() {
        let mut panel = catalogue();
        assert!(panel.rows[0].text.contains("provider-0"));
        panel.move_provider(true);
        assert!(panel.rows[0].text.contains("provider-1"));
        // Both separators reach the same AND, which is what keeps the panel
        // usable after `Space` stopped being a character.
        panel.search_insert("ACCOUNT-4+exact");
        assert_eq!(
            panel.search.as_ref().unwrap().providers,
            ["provider-4", "All providers"]
        );
        panel.search_clear();
        panel.search_insert("ACCOUNT-4 exact");
        assert_eq!(
            panel.search.as_ref().unwrap().providers,
            ["provider-4", "All providers"]
        );
        assert_eq!(
            panel.rows[panel.selected].command.as_deref(),
            Some("/model model-4/exact")
        );
        panel.search_insert("x");
        assert!(panel.rows[0].text.starts_with("No models match"));
        panel.search_backspace();
        assert_eq!(panel.rows.len(), 2);
        panel.search_clear();
        panel.search_insert("flash");
        assert_eq!(panel.search.as_ref().unwrap().providers.len(), 6);
        panel.move_provider(true);
        assert_eq!(
            panel.rows[panel.selected].command.as_deref(),
            Some("/model shared/flash")
        );
    }

    #[test]
    fn locked_accounts_are_searchable_but_have_no_apply_command() {
        let mut panel = Panel::models(
            "Models",
            vec![ModelGroup {
                provider: "google".into(),
                account: "other-sub".into(),
                scope: "subscription".into(),
                models: vec!["gemini/exact".into(), "gemini/second".into()],
                selectable: Some(false),
                unavailable_reason: Some("Pinned to another account".into()),
                connect: None,
                pooled: None,
                note: None,
            }],
            TierModels::default(),
        );
        assert!(panel.rows[0].text.contains("Pinned to another account"));
        assert!(panel.rows.iter().all(|row| row.command.is_none()));
        assert_eq!(panel.selected, 1);
        panel.move_selection(true, 1);
        assert_eq!(panel.selected, 2, "locked models remain inspectable");
        panel.search_insert("other-sub exact");
        assert_eq!(panel.rows.len(), 2);
        assert!(panel.rows[panel.selected].command.is_none());
    }

    /// The panel sets three models, not one.
    ///
    /// Same catalogue, same rows, same keys -- the tier changes only what the
    /// chosen row *does*, which is why a person can discover the ladder
    /// without being taught it.
    #[test]
    fn tab_moves_which_tier_a_chosen_model_is_assigned_to() {
        let mut panel = catalogue();
        assert_eq!(panel.tier(), Tier::Parent);
        assert!(panel.rows.iter().all(|row| {
            !row.command
                .as_deref()
                .is_some_and(|c| c.ends_with(" off") || c.ends_with(" auto"))
        }));
        panel.cycle_tier();
        assert_eq!(panel.tier(), Tier::Subagents);
        assert_eq!(
            panel.rows[0].command.as_deref(),
            Some("/model subagent auto")
        );
        assert_eq!(
            panel.rows[1].command.as_deref(),
            Some("/model subagent off")
        );
        panel.cycle_tier();
        assert_eq!(panel.tier(), Tier::Helpers);
        assert_eq!(panel.rows[0].command.as_deref(), Some("/model helper off"));
        assert!(
            !panel
                .rows
                .iter()
                .any(|row| row.command.as_deref() == Some("/model helper auto"))
        );
        panel.cycle_tier();
        assert_eq!(panel.tier(), Tier::Parent);
    }

    /// Opening the panel is how a person finds out the other two tiers exist,
    /// so it states all three whether or not they are set.
    #[test]
    fn the_panel_states_every_tier_including_the_ones_that_are_unset() {
        let panel = Panel::models(
            "Models",
            vec![ModelGroup {
                provider: "openai".into(),
                account: "chatgpt-subscription".into(),
                scope: "subscription".into(),
                models: vec!["gpt-5.6-luna".into()],
                selectable: Some(true),
                unavailable_reason: None,
                connect: None,
                pooled: None,
                note: None,
            }],
            TierModels {
                parent: "opus-5".into(),
                helper: None,
                subagent: Some("claude-sonnet-5".into()),
            },
        );
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal
            .draw(|frame| {
                render_panel(frame, frame.area(), &panel, super::super::Theme::Neon);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen: String = (0..24)
            .map(|y| (0..90).map(|x| buffer[(x, y)].symbol()).collect::<String>() + "\n")
            .collect();
        assert!(screen.contains("[▸Main ]"), "{screen}");
        assert!(screen.contains("[ Subagent ]"), "{screen}");
        assert!(screen.contains("[ Helper ]"), "{screen}");
        assert!(screen.contains("Current: opus-5"), "{screen}");
        assert!(screen.contains("claude-sonnet-5"), "{screen}");
        assert!(screen.contains("off"), "{screen}");
    }

    #[test]
    fn the_current_selectable_provider_opens_ahead_of_locked_subscriptions() {
        let panel = Panel::models(
            "Models",
            vec![
                ModelGroup {
                    provider: "anthropic".into(),
                    account: "claude-sub".into(),
                    scope: "account-declared".into(),
                    models: vec!["claude/exact".into()],
                    selectable: Some(false),
                    unavailable_reason: Some("another route is active".into()),
                    connect: None,
                    pooled: None,
                    note: None,
                },
                ModelGroup {
                    provider: "google".into(),
                    account: "gemini-sub".into(),
                    scope: "account-declared".into(),
                    models: vec!["gemini/exact".into()],
                    selectable: Some(true),
                    unavailable_reason: None,
                    connect: None,
                    pooled: None,
                    note: None,
                },
            ],
            TierModels::default(),
        );
        assert!(panel.rows[0].text.starts_with("google · gemini-sub"));
        assert_eq!(
            panel.rows[panel.selected].command.as_deref(),
            Some("/model gemini/exact")
        );
    }

    /// **The finding this vocabulary exists for: a resting actionable still
    /// looks pressable.**
    ///
    /// Before this, an unselected panel row and a line of prose were the same
    /// pixels — selection was drawn, "you can press this" was not. Asserted on
    /// the background, because that is the affordance: a row that carries a
    /// command is filled with `dock` when resting and `accent` when focused,
    /// and a heading is filled with neither.
    ///
    /// Rows are located by the text they drew rather than by arithmetic on
    /// `inner`: a models panel opens with its provider carousel and search
    /// lines above the list, so counting from the top asserts about the wrong
    /// row and passes for the wrong reason.
    #[test]
    fn an_actionable_row_looks_pressable_even_when_it_is_not_selected() {
        let theme = super::super::Theme::Neon;
        let panel = Panel::models(
            "Models",
            vec![ModelGroup {
                provider: "google".into(),
                account: "gemini-sub".into(),
                scope: "declared".into(),
                models: vec!["gemini/one".into(), "gemini/two".into()],
                selectable: Some(true),
                unavailable_reason: None,
                connect: None,
                pooled: None,
                note: None,
            }],
            TierModels::default(),
        );
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| {
                render_panel(frame, frame.area(), &panel, theme);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let lines: Vec<String> = (0..20)
            .map(|y| (0..60).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();

        // The background of the first non-blank cell of the row that drew
        // `needle` — the pill's own left cap when there is one.
        let fill = |needle: &str| -> Color {
            let y = lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("`{needle}` was not drawn:\n{}", lines.join("\n")));
            let x = lines[y]
                .chars()
                .position(|c| c != ' ' && c != '║')
                .expect("a drawn row has a first character");
            buffer[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].bg
        };

        assert_eq!(
            panel.selected, 1,
            "the fixture opens on the first model, so row 2 is the resting one"
        );
        assert_eq!(
            fill("gemini/one"),
            theme.accent(),
            "the focused button is filled with the accent"
        );
        assert_eq!(
            fill("gemini/two"),
            theme.dock(),
            "and an unselected button is still filled — this is the whole point"
        );
        assert_eq!(
            fill("gemini-sub · declared"),
            Color::Reset,
            "a heading is prose and must not be filled"
        );
    }

    /// `^O` is the answer to a 469-entry list whose alphabetical order put
    /// `claude-3-5-haiku-20241022` above `claude-opus-5`.
    #[test]
    fn the_catalogue_orders_by_published_index_and_sinks_the_unmeasured() {
        let mut panel = Panel::models(
            "Models",
            vec![ModelGroup {
                provider: "anthropic".into(),
                account: "one".into(),
                scope: "declared".into(),
                models: vec![
                    "claude-3-5-haiku-20241022".into(),
                    "claude-opus-5".into(),
                    "unlisted-model".into(),
                ],
                selectable: Some(true),
                unavailable_reason: None,
                connect: None,
                pooled: None,
                note: None,
            }],
            TierModels::default(),
        )
        .with_intelligence(BTreeMap::from([
            ("claude-opus-5".to_string(), 71.0),
            ("claude-3-5-haiku-20241022".to_string(), 34.0),
        ]));

        let ids = |panel: &Panel| {
            panel
                .rows
                .iter()
                .filter_map(|row| row.command.as_deref())
                .map(|command| command.trim_start_matches("/model ").to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            ids(&panel),
            [
                "claude-opus-5",
                "claude-3-5-haiku-20241022",
                "unlisted-model"
            ],
            "a catalogue that exists is the order it opens in: strongest first, \
             and a model with no measurement sinks rather than sorting as a zero"
        );
        // The score reaches the row, so the order is inspectable rather than
        // something a person has to take on trust.
        assert!(
            panel.rows.iter().any(|row| row.text.contains("AA 71")),
            "{:?}",
            panel.rows
        );

        assert!(panel.cycle_order());
        assert_eq!(
            ids(&panel),
            [
                "claude-3-5-haiku-20241022",
                "claude-opus-5",
                "unlisted-model"
            ],
            "^O still goes back to the alphabet"
        );
        assert!(panel.cycle_order());
        assert!(
            !panel
                .rows
                .iter()
                .any(|row| row.text.contains("unlisted-model") && row.text.contains("AA")),
            "an unmeasured model prints no score at all: {:?}",
            panel.rows
        );

        assert_eq!(ids(&panel)[0], "claude-opus-5", "^O cycles round");
    }

    /// With no catalogue there is nothing to order by, so the alphabet is the
    /// only honest answer and `^O` must not pretend otherwise.
    #[test]
    fn a_panel_with_no_catalogue_opens_on_the_alphabet() {
        let panel = catalogue().with_intelligence(BTreeMap::new());
        assert_eq!(panel.search.as_ref().unwrap().order, Order::Name);
        assert!(
            !panel.rows.iter().any(|row| row.text.contains("AA ")),
            "no measurement, no score column"
        );
    }

    /// A provider's subscriptions are one pool: each account is a row whose
    /// ⏎ takes it in or out, with its plan and usage; the models are listed
    /// once, and only those an account in the pool serves.
    #[test]
    fn subscriptions_of_one_provider_are_a_pool_with_a_toggle_per_account() {
        let account = |name: &str, pooled: bool, models: &[&str]| ModelGroup {
            provider: "claude".into(),
            account: name.into(),
            scope: "account-declared".into(),
            models: models.iter().map(|m| (*m).to_string()).collect(),
            selectable: Some(true),
            pooled: Some(pooled),
            note: Some("Max 20x · 5h 4% · week 16%".into()),
            ..ModelGroup::default()
        };
        let panel = Panel::models(
            "Models",
            vec![
                account("claude-a", true, &["claude-opus-5-5", "claude-sonnet-5"]),
                account("claude-b", true, &["claude-opus-5-5"]),
                account("claude-c", false, &["claude-only-on-c"]),
            ],
            TierModels {
                parent: "claude-opus-5-5".into(),
                helper: None,
                subagent: None,
            },
        )
        .with_intelligence(BTreeMap::new());
        let row = |needle: &str| {
            panel
                .rows
                .iter()
                .find(|row| row.text.contains(needle))
                .unwrap_or_else(|| panic!("no row with {needle}: {:#?}", panel.rows))
        };
        assert_eq!(
            row("● claude-a — Max 20x").command.as_deref(),
            Some("/pool claude-a exclude")
        );
        assert_eq!(
            row("○ claude-c — out of the pool").command.as_deref(),
            Some("/pool claude-c include")
        );
        let listed = |id: &str| {
            panel
                .rows
                .iter()
                .filter(|row| row.command.as_deref() == Some(&format!("/model {id}")))
                .count()
        };
        assert_eq!(
            listed("claude-opus-5-5"),
            1,
            "one row for a model two accounts serve"
        );
        assert_eq!(listed("claude-sonnet-5"), 1);
        assert_eq!(
            listed("claude-only-on-c"),
            0,
            "an account out of the pool serves nothing"
        );
    }

    /// Three tiers in one visit: stage each, apply once, or throw the lot
    /// away. Before this, `Enter` applied one row and closed.
    #[test]
    fn space_stages_every_tier_and_enter_has_one_list_to_apply() {
        let mut panel = Panel::models(
            "Models",
            vec![ModelGroup {
                provider: "anthropic".into(),
                account: "one".into(),
                scope: "declared".into(),
                models: vec!["big".into(), "small".into()],
                selectable: Some(true),
                unavailable_reason: None,
                connect: None,
                pooled: None,
                note: None,
            }],
            TierModels {
                parent: "big".into(),
                helper: None,
                subagent: None,
            },
        );

        assert!(
            panel.staged_commands().is_empty(),
            "nothing is staged on open"
        );

        // The row that IS the tier's current value says so, and staging it
        // means "leave this tier alone" rather than queueing a no-op.
        assert_eq!(panel.tier(), Tier::Parent);
        assert!(
            panel.rows[panel.selected].text.contains("· NOW"),
            "{:?}",
            panel.rows
        );
        assert!(panel.stage().unwrap().contains("parent stays big"));
        assert!(panel.staged_commands().is_empty());

        panel.move_selection(true, 1);
        assert!(panel.stage().unwrap().contains("big → small"));
        assert_eq!(panel.staged_commands(), ["/model small"]);
        // Every press moves a marker under the cursor, so `Space` is never a
        // key that appears to do nothing.
        assert!(
            panel.rows[panel.selected].text.contains("◆ STAGED"),
            "{:?}",
            panel.rows
        );

        // Pressing it again on the same row takes it back.
        assert!(panel.stage().unwrap().contains("unstaged parent"));
        assert!(panel.staged_commands().is_empty());
        assert!(!panel.rows[panel.selected].text.contains("STAGED"));
        assert!(panel.stage().is_some());

        // A second tier in the same visit, which is the point.
        assert!(to_tier(&mut panel, Tier::Helpers));
        panel.move_selection(true, 1);
        assert!(panel.stage().is_some());

        assert!(to_tier(&mut panel, Tier::Subagents));
        panel.move_selection(true, 1);
        assert!(panel.stage().unwrap().contains("staged subagent"));

        let applied = panel.staged_commands();
        assert_eq!(applied.len(), 3, "{applied:?}");
        assert!(applied.iter().any(|c| c.starts_with("/model helper ")));
        assert!(applied.iter().any(|c| c.starts_with("/model subagent ")));

        // A different row for a tier that already has one replaces that
        // tier's choice rather than adding a second.
        assert!(to_tier(&mut panel, Tier::Helpers));
        panel.move_selection(true, 2);
        panel.stage();
        assert_eq!(panel.staged_commands().len(), 3, "one choice per tier");
        assert!(
            panel.staged_commands().iter().any(|c| c.ends_with("small")),
            "{:?}",
            panel.staged_commands()
        );

        // And the helper's `off` row -- what it already runs -- reverts that
        // tier rather than queueing a no-op.
        panel.move_selection(false, 9);
        assert!(panel.stage().unwrap().contains("helper stays off"));
        assert_eq!(panel.staged_commands().len(), 2);
    }

    /// A staged change is visible on the row it will change, not only in a
    /// notice that the next keystroke replaces.
    #[test]
    fn the_roster_shows_a_staged_tier_as_now_then_next() {
        let mut panel = catalogue();
        to_tier(&mut panel, Tier::Helpers);
        // Past the `off` row, which is what this tier already runs.
        panel.move_selection(true, 1);
        panel.stage();

        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal
            .draw(|frame| {
                render_panel(frame, frame.area(), &panel, super::super::Theme::Neon);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen: String = (0..24)
            .map(|y| (0..90).map(|x| buffer[(x, y)].symbol()).collect::<String>() + "\n")
            .collect();
        assert!(screen.contains("off → "), "{screen}");
    }
}
