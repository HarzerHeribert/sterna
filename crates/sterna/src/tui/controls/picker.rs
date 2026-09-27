//! Agent-first model catalogue, using the session's own theme and hit geometry.
use super::*;
use crate::tui::{Theme, abbreviate};

fn line(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    if area.width > 0 && area.height > 0 {
        frame.render_widget(
            Paragraph::new(abbreviate(text, area.width as usize)).style(style),
            area,
        );
    }
}

fn take_line(area: &mut Rect) -> Rect {
    let row = Rect::new(area.x, area.y, area.width, area.height.min(1));
    area.y = area.y.saturating_add(row.height);
    area.height = area.height.saturating_sub(row.height);
    row
}

pub(super) fn render(frame: &mut Frame, area: Rect, panel: &Panel, theme: Theme) {
    let Some(search) = &panel.search else {
        return;
    };
    // Whether the mode row above the list was drawn: the list then starts
    // below the modes rather than repeating them.
    let mut modes_drawn = false;
    let accent = Style::default().fg(theme.accent());
    let muted = Style::default().fg(Color::DarkGray);
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_type(BorderType::Double)
        .border_style(accent)
        .title(" Models ")
        .title_bottom(if area.width >= 90 {
            " TAB agent · ←→ provider · ↑↓ model · SPACE select/undo · ENTER apply · ESC cancel "
        } else {
            " TAB agent · ←→ provider · SPACE select · ENTER apply · ESC cancel "
        });
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(assignment) = &panel.assignment {
        if inner.height > 2 && inner.width >= 16 {
            let tabs = take_line(&mut inner);
            let width = tabs.width / 3;
            for (i, (tier, label)) in [
                (Tier::Parent, "Main"),
                (Tier::Subagents, "Subagent"),
                (Tier::Helpers, "Helper"),
            ]
            .into_iter()
            .enumerate()
            {
                let rect = Rect::new(
                    tabs.x + i as u16 * width,
                    tabs.y,
                    if i == 2 {
                        tabs.width - width * 2
                    } else {
                        width
                    },
                    1,
                );
                let label = format!(
                    "{label}{}",
                    if panel.staged.contains_key(&tier) {
                        " ◆"
                    } else {
                        ""
                    }
                );
                let (text, style) = pill(&label, assignment.active == tier, theme);
                line(frame, rect, &text, style);
            }
        }
        if inner.height > 5 {
            let row = take_line(&mut inner);
            let width = row.width / 3;
            for (i, tier) in [Tier::Parent, Tier::Subagents, Tier::Helpers]
                .into_iter()
                .enumerate()
            {
                line(
                    frame,
                    Rect::new(row.x + i as u16 * width, row.y, width, 1),
                    assignment.models.describe(tier),
                    muted,
                );
            }
        }
        if inner.height > 3 {
            let now = assignment.models.describe(assignment.active);
            let value = panel
                .staged
                .get(&assignment.active)
                .and_then(|c| c.rsplit(' ').next())
                .map_or_else(
                    || format!("Current: {now}"),
                    |next| format!("Current: {now} → {next}  ◆ staged"),
                );
            line(frame, take_line(&mut inner), &value, accent);
        }
    }
    let mode_count = panel
        .rows
        .iter()
        .take_while(|row| {
            row.command
                .as_deref()
                .is_some_and(|c| c.ends_with(" off") || c.ends_with(" auto"))
        })
        .count();
    if inner.height > 3 && (mode_count == 0 || inner.width >= mode_count as u16 * 7) {
        let row = take_line(&mut inner);
        let mut x = row.x;
        let mode_width = if mode_count == 0 {
            0
        } else {
            ((row.width.saturating_sub(mode_count as u16 - 1)) / mode_count as u16).min(24)
        };
        for index in 0..mode_count {
            let entry = &panel.rows[index];
            let (text, style) = pill(&entry.text, panel.selected == index, theme);
            let width = mode_width.min(row.right().saturating_sub(x));
            let rect = Rect::new(x, row.y, width, 1);
            line(frame, rect, &text, style);
            modes_drawn |= width > 0;
            x = x.saturating_add(width + 1);
        }
        let hint = match panel.tier() {
            Tier::Parent => "Pinned model required",
            Tier::Helpers => "or pin a model below",
            Tier::Subagents => "Auto inherits Main · or pin below",
        };
        line(
            frame,
            Rect::new(x, row.y, row.right().saturating_sub(x), 1),
            hint,
            muted,
        );
    }
    if inner.height > 2 {
        let row = take_line(&mut inner);
        let query = if search.query.is_empty() {
            "Type to search…"
        } else {
            &search.query
        };
        let matches: usize = search.matched.iter().map(|group| group.models.len()).sum();
        let total: usize = search.source.iter().map(|group| group.models.len()).sum();
        line(
            frame,
            row,
            &format!("Search  {query}   {matches}/{total} · Ctrl-U clear"),
            accent,
        );
    }
    if inner.height > 2 {
        let row = take_line(&mut inner);
        let mut x = row.x;
        let button_width = (row.width.saturating_sub(1) / 2).min(20);
        for (order, label) in [
            (Order::Name, "Provider / name"),
            (Order::Intelligence, "Intelligence ↓"),
        ] {
            let (text, style) = pill(label, search.order == order, theme);
            let width = button_width.min(row.right().saturating_sub(x));
            let rect = Rect::new(x, row.y, width, 1);
            line(frame, rect, &text, style);
            x = x.saturating_add(width + 1);
        }
        line(
            frame,
            Rect::new(x, row.y, row.right().saturating_sub(x), 1),
            "Ctrl-O sort",
            muted,
        );
    }
    // Wide terminals get a provider rail; compact terminals get one scrolling strip.
    if inner.width >= 70 && inner.height >= 3 {
        let width = (inner.width / 4).clamp(16, 24);
        let rail = Rect::new(inner.x, inner.y, width, inner.height);
        line(
            frame,
            Rect::new(rail.x, rail.y, rail.width, 1),
            "PROVIDERS  ← →",
            muted,
        );
        let visible = usize::from(rail.height.saturating_sub(1));
        let first = search.active.saturating_sub(visible.saturating_sub(1));
        for (i, provider) in search
            .providers
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
        {
            let rect = Rect::new(
                rail.x,
                rail.y + 1 + (i - first) as u16,
                rail.width.saturating_sub(1),
                1,
            );
            let locked = provider != "All providers"
                && search
                    .matched
                    .iter()
                    .filter(|group| &group.provider == provider)
                    .all(|group| group.selectable == Some(false));
            let label = if locked {
                format!("{provider} LOCK")
            } else {
                provider.clone()
            };
            let (text, style) = pill(&label, i == search.active, theme);
            line(frame, rect, &text, style);
        }
        for y in inner.y..inner.bottom() {
            line(frame, Rect::new(inner.x + width, y, 1, 1), "│", muted);
        }
        inner.x += width + 2;
        inner.width = inner.width.saturating_sub(width + 2);
    } else if inner.width >= 10 && inner.height > 1 && !search.providers.is_empty() {
        let row = take_line(&mut inner);
        let provider = &search.providers[search.active];
        let (text, style) = pill(provider, true, theme);
        line(
            frame,
            Rect::new(row.x + 3, row.y, row.width - 6, 1),
            &text,
            style,
        );
        line(frame, Rect::new(row.x, row.y, 3, 1), " ← ", style);
        line(frame, Rect::new(row.right() - 3, row.y, 3, 1), " → ", style);
    }
    if inner.height > 3 {
        let row = take_line(&mut inner);
        let score_width = if row.width >= 40 {
            20
        } else if row.width >= 24 {
            8
        } else {
            0
        };
        line(
            frame,
            Rect::new(row.x, row.y, row.width.saturating_sub(score_width), 1),
            "MODEL",
            muted,
        );
        if score_width > 0 {
            line(
                frame,
                Rect::new(row.right() - score_width, row.y, score_width, 1),
                "AA INDEX",
                muted,
            );
        }
    }
    let footer = if inner.height > 3 {
        let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        inner.height -= 1;
        Some(footer)
    } else {
        None
    };
    let visible = usize::from(inner.height);
    let hidden_modes = if modes_drawn { mode_count } else { 0 };
    let start = panel
        .selected
        .saturating_sub(visible.saturating_sub(1))
        .max(hidden_modes);
    for (slot, (index, entry)) in panel
        .rows
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let rect = Rect::new(inner.x, inner.y + slot as u16, inner.width, 1);
        let focused = panel.selected == index;
        let style = if focused {
            Style::default().bg(theme.accent()).fg(Color::Black)
        } else if entry.command.is_some() {
            Style::default().fg(Color::White).bg(theme.dock())
        } else {
            muted
        };
        if let Some(detail) = search.details.get(&index) {
            let marker = if entry.text.contains("STAGED") {
                "◆"
            } else if entry.text.contains("NOW") {
                "●"
            } else if entry.command.is_none() {
                "×"
            } else {
                " "
            };
            let name = format!("{} {marker} {}", if focused { "›" } else { " " }, detail.id);
            let score_width = if inner.width >= 40 {
                20
            } else if inner.width >= 24 {
                8
            } else {
                0
            };
            let name_width = inner.width.saturating_sub(score_width);
            line(
                frame,
                Rect::new(rect.x, rect.y, name_width, 1),
                &name,
                style,
            );
            if score_width > 0 {
                let score = match detail.score {
                    Some(score) if score_width >= 20 => {
                        let filled = (score.clamp(0.0, 100.0) / 10.0).round() as usize;
                        format!(
                            "{}{} AA {score:>3.0}",
                            "█".repeat(filled),
                            "░".repeat(10 - filled)
                        )
                    }
                    Some(score) => format!(" AA {score:.0}"),
                    None => "— unranked".into(),
                };
                line(
                    frame,
                    Rect::new(rect.x + name_width, rect.y, score_width, 1),
                    &score,
                    style,
                );
            }
        } else {
            line(frame, rect, &entry.text, style);
        }
    }
    if let Some(footer) = footer {
        let text = search.details.get(&panel.selected).map_or_else(
            || "Choose a model to pin this agent".into(),
            |detail| {
                if panel.rows[panel.selected].command.is_none() {
                    return detail
                        .unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "Unavailable on this route".into());
                }
                let state = panel.rows[panel.selected].text.as_str();
                let state = if state.contains("STAGED") {
                    "◆ staged"
                } else if state.contains("NOW") {
                    "● current"
                } else {
                    "Space to select"
                };
                format!("{} · {state}", detail.route)
            },
        );
        line(frame, footer, &text, accent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    /// Walks the provider carousel to `index` the way the arrow keys do.
    fn to_provider(panel: &mut Panel, index: usize) {
        while panel
            .search
            .as_ref()
            .is_some_and(|search| search.active < index)
        {
            panel.move_provider(true);
        }
    }

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

    fn ranked() -> Panel {
        Panel::models(
            "Models",
            vec![
                ModelGroup {
                    provider: "alpha".into(),
                    account: "personal".into(),
                    scope: "connected".into(),
                    models: vec!["first".into(), "unmeasured".into()],
                    selectable: Some(true),
                    unavailable_reason: None,
                    connect: None,
                    pooled: None,
                    note: None,
                },
                ModelGroup {
                    provider: "beta".into(),
                    account: "work".into(),
                    scope: "connected".into(),
                    models: vec!["best".into()],
                    selectable: Some(true),
                    unavailable_reason: None,
                    connect: None,
                    pooled: None,
                    note: None,
                },
            ],
            TierModels {
                parent: "first".into(),
                ..TierModels::default()
            },
        )
        .with_intelligence(BTreeMap::from([
            ("first".into(), 35.0),
            ("best".into(), 70.0),
        ]))
    }

    #[test]
    fn sorting_keeps_duplicate_and_unavailable_route_identity() {
        let mut panel = Panel::models(
            "Models",
            vec![
                ModelGroup {
                    provider: "alpha".into(),
                    account: "one".into(),
                    scope: "connected".into(),
                    models: vec!["shared".into()],
                    selectable: Some(true),
                    unavailable_reason: None,
                    connect: None,
                    pooled: None,
                    note: None,
                },
                ModelGroup {
                    provider: "beta".into(),
                    account: "two".into(),
                    scope: "locked".into(),
                    models: vec!["shared".into()],
                    selectable: Some(false),
                    unavailable_reason: Some("Locked".into()),
                    connect: None,
                    pooled: None,
                    note: None,
                },
            ],
            TierModels::default(),
        );
        to_provider(&mut panel, 2);
        panel.move_selection(true, 1);
        assert!(panel.rows[panel.selected].command.is_none());
        panel.cycle_order();
        let detail = &panel.search.as_ref().unwrap().details[&panel.selected];
        assert_eq!(detail.route, "beta · two");
        assert_eq!(detail.id, "shared");
        assert!(panel.rows[panel.selected].command.is_none());
    }

    #[test]
    fn all_providers_rank_globally_and_sort_preserves_staging_and_cursor() {
        let mut panel = ranked();
        to_provider(&mut panel, 2);
        let ids = |panel: &Panel| {
            panel
                .rows
                .iter()
                .filter_map(|row| row.command.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&panel),
            ["/model best", "/model first", "/model unmeasured"]
        );
        panel.stage();
        panel.select_order(Order::Name);
        assert_eq!(
            ids(&panel),
            ["/model first", "/model unmeasured", "/model best"]
        );
        assert_eq!(
            panel.rows[panel.selected].command.as_deref(),
            Some("/model best")
        );
        assert_eq!(panel.staged_commands(), ["/model best"]);
        panel.stage();
        assert!(panel.staged_commands().is_empty());
    }

    #[test]
    fn ranked_picker_draws_agent_tabs_before_providers_and_honest_bars() {
        let mut panel = ranked();
        to_provider(&mut panel, 2);
        let mut terminal = Terminal::new(TestBackend::new(110, 22)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &panel, Theme::Neon))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let screen = (0..22)
            .map(|y| {
                (0..110)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("███████░░░ AA  70"), "{screen}");
        assert!(screen.contains("— unranked"), "{screen}");
        assert!(screen.find("Main").unwrap() < screen.find("PROVIDERS").unwrap());
        println!("{screen}");
    }

    #[test]
    fn empty_search_keeps_modes_available_and_the_empty_message_visible() {
        let mut panel = ranked();
        to_tier(&mut panel, Tier::Subagents);
        panel.search_insert("missing-model");
        assert_eq!(
            panel.rows[0].command.as_deref(),
            Some("/model subagent auto")
        );
        assert_eq!(
            panel.rows[1].command.as_deref(),
            Some("/model subagent off")
        );
        assert!(
            panel
                .rows
                .iter()
                .any(|row| row.text.starts_with("No models match"))
        );
        panel.move_selection(true, 1);
        panel.stage();
        assert_eq!(panel.staged_commands(), ["/model subagent off"]);
    }
}
