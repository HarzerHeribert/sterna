//! Local session controls, and the panels the session hands the screen.
//!
//! A panel is data: a title and rows, each row a kind and a typed action.
//! The workbench draws every panel as a sheet (`workbench/sheet.rs`); nothing
//! here draws or takes a key.
use std::collections::BTreeMap;

use crate::spend::Tier;
use crate::workbench::{Action, ItemKind};

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
    /// The row the sheet opens on, when the builder knows better than the
    /// sheet's own rule (the current value, else the first actionable row).
    pub selected: usize,
    /// Present only on the model panel: which tier a chosen model is being
    /// assigned to, and what all three run on now.
    pub assignment: Option<Assignment>,
    /// The model panel's catalogue: every account's models and the published
    /// intelligence index, for the workbench's model sheet.
    pub catalogue: Option<Catalogue>,
    /// What leaving this panel with Esc does, when leaving is itself an
    /// answer: the rollback preview's Esc cancels the pending rollback.
    pub back: Option<Action>,
}

/// Every model a model panel can offer, grouped by the account serving it.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    pub groups: Vec<ModelGroup>,
    /// Normalised model name to its published intelligence index. Empty
    /// when no catalogue could be fetched.
    pub intelligence: BTreeMap<String, f64>,
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
    /// Keeps focus on this row when the panel is rebuilt with other text
    /// (a counter that moved). `None` uses the text.
    pub id: Option<String>,
    pub text: String,
    /// What the row is, in the sheet's grammar: plain text, a heading, a way
    /// somewhere, a one-shot action or a dangerous one.
    pub kind: ItemKind,
    /// What choosing the row does. Typed, never a pseudo slash command.
    pub action: Option<Action>,
    /// What stands beside the text, in the sheet's value column: a fact's
    /// value, a setting's current word.
    pub value: Option<String>,
}

impl PanelRow {
    /// Text that is read, never chosen.
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            id: None,
            text: text.into(),
            kind: ItemKind::Info,
            action: None,
            value: None,
        }
    }
    /// A row that goes somewhere: another list, a step, a form.
    pub fn open(text: impl Into<String>, action: Action) -> Self {
        Self {
            id: None,
            text: text.into(),
            kind: ItemKind::Open,
            action: Some(action),
            value: None,
        }
    }
    /// A one-shot action.
    pub fn run(text: impl Into<String>, action: Action) -> Self {
        Self {
            id: None,
            text: text.into(),
            kind: ItemKind::Run,
            action: Some(action),
            value: None,
        }
    }
    /// A row that runs one of the session's own commands.
    pub fn command(text: impl Into<String>, command: impl Into<String>) -> Self {
        Self::run(text, Action::Command(command.into()))
    }
    /// A row that opens the list another command builds.
    pub fn opens(text: impl Into<String>, command: impl Into<String>) -> Self {
        Self::open(text, Action::Command(command.into()))
    }
    /// A heading over the rows after it.
    pub fn heading(text: impl Into<String>) -> Self {
        Self {
            id: None,
            text: text.into(),
            kind: ItemKind::Heading,
            action: None,
            value: None,
        }
    }
    /// A row whose action cannot be taken back.
    pub fn danger(text: impl Into<String>, action: Action) -> Self {
        Self {
            id: None,
            text: text.into(),
            kind: ItemKind::Danger,
            action: Some(action),
            value: None,
        }
    }
    /// The words in the value column, beside the row's text.
    #[must_use]
    pub fn shows(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
    /// The session command this row runs, if it runs one.
    #[must_use]
    pub fn command_line(&self) -> Option<&str> {
        match &self.action {
            Some(Action::Command(command)) => Some(command),
            _ => None,
        }
    }
    /// Whether choosing this row does anything.
    #[must_use]
    pub fn acts(&self) -> bool {
        self.action.is_some()
    }
}

impl Panel {
    /// The model panel's catalogue, for the workbench's model sheet.
    pub(crate) fn catalogue(&self) -> Option<(&[ModelGroup], &BTreeMap<String, f64>)> {
        self.catalogue
            .as_ref()
            .map(|c| (c.groups.as_slice(), &c.intelligence))
    }

    /// Prose laid out as plain rows, one per line.
    pub fn text(title: impl Into<String>, text: impl AsRef<str>) -> Self {
        Self::rows(title, text.as_ref().lines().map(PanelRow::info).collect())
    }

    /// A panel whose rows the caller built.
    pub fn rows(title: impl Into<String>, rows: Vec<PanelRow>) -> Self {
        Self {
            title: title.into(),
            rows,
            ..Self::default()
        }
    }

    /// The model panel: the catalogue, and what each tier runs on now.
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
        Self {
            title: format!("{} · {summary}", title.into()),
            assignment: Some(Assignment {
                active: Tier::Parent,
                models,
            }),
            catalogue: Some(Catalogue {
                groups,
                intelligence: BTreeMap::new(),
            }),
            ..Self::default()
        }
    }

    /// Attaches the published measurements the catalogue is ordered by.
    #[must_use]
    pub fn with_intelligence(mut self, intelligence: BTreeMap<String, f64>) -> Self {
        if let Some(catalogue) = self.catalogue.as_mut() {
            catalogue.intelligence = intelligence;
        }
        self
    }

    /// The tier a chosen row would be assigned to.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.assignment
            .as_ref()
            .map_or(Tier::Parent, |assignment| assignment.active)
    }
}
