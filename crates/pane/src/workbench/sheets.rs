//! The sheets a person fills or chooses on: a form, the theme picker, the
//! wizard's steps and the model picker. Each is drawn over the workbench's
//! surface with the same small pieces the rest of it uses.
use super::view::{add, button, label, row, wrap_words};
use super::{Action, Geometry, Tone, chrome, document::clip};
use crate::tui::{ScreenState, Theme};
use ratatui::{Frame, layout::Rect, widgets::Clear};

/// `/wizard`: each step a card -- what it is, then where it stands -- and
/// any sentence of explanation as a paragraph above them.
pub(super) fn draw_wizard(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    inner: Rect,
    panel: &crate::tui::Panel,
    t: Theme,
) {
    let mut y = inner.y + 2;
    for (i, r) in panel.rows.iter().enumerate() {
        if y + 2 >= inner.bottom() {
            break;
        }
        if r.command.is_none() {
            for line in wrap_words(&r.text, inner.width as usize) {
                label(f, inner, y, &line, Tone::Normal, t);
                y += 1;
            }
            y += 1;
            continue;
        }
        let (head, detail) = r.text.split_once(" · ").unwrap_or((r.text.as_str(), ""));
        let chosen = i == panel.selected;
        add(
            f,
            g,
            inner,
            y,
            &format!("{} {head}", if chosen { "›" } else { " " }),
            Action::PanelRow(i),
            chosen,
            t,
        );
        if !detail.is_empty() {
            label(
                f,
                Rect::new(
                    inner.x + 2,
                    inner.y,
                    inner.width.saturating_sub(2),
                    inner.height,
                ),
                y + 1,
                &clip(detail, inner.width.saturating_sub(2) as usize),
                Tone::Muted,
                t,
            );
        }
        y += 3;
    }
    label(
        f,
        inner,
        inner.bottom().saturating_sub(1),
        "↑↓ choose · Enter open · Esc finish later",
        Tone::Muted,
        t,
    );
}

/// `/theme`: the palettes on the left, each by a swatch of its accent, and
/// the chosen one on the right as the screen will wear it -- a bird theme's
/// bird, its name and its three colours.
/// One line of the theme list as drawn.
enum ThemeLine {
    /// A family's heading.
    Heading(crate::tui::Family),
    /// The space before the next family.
    Gap,
    /// A theme, with its row in the picker.
    Theme(usize, Theme),
}

pub(super) fn draw_themes(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    inner: Rect,
    panel: &crate::tui::Panel,
    s: &ScreenState,
    t: Theme,
) {
    use super::plumage::{Mood, ROWS, WIDTH, sprite};
    let theme_of = |row: &crate::tui::PanelRow| {
        row.command
            .as_deref()
            .and_then(|command| command.strip_prefix("/theme "))
            .and_then(Theme::parse)
    };
    let list = (inner.width / 2).clamp(24, 40);
    let rows = inner.height.saturating_sub(4) as usize;
    // The list as drawn: a heading where the family changes, then its
    // themes. Selection still counts themes only, so a heading is never
    // chosen and Up/Down step from theme to theme across families.
    let mut lines: Vec<ThemeLine> = Vec::new();
    let mut family = None;
    for (i, r) in panel.rows.iter().enumerate() {
        let Some(theme) = theme_of(r) else { continue };
        if family != Some(theme.family()) {
            if family.is_some() {
                lines.push(ThemeLine::Gap);
            }
            family = Some(theme.family());
            lines.push(ThemeLine::Heading(theme.family()));
        }
        lines.push(ThemeLine::Theme(i, theme));
    }
    let at = lines
        .iter()
        .position(|line| matches!(line, ThemeLine::Theme(i, _) if *i == panel.selected))
        .unwrap_or(0);
    let start = at.saturating_sub(rows.saturating_sub(1));
    for (n, line) in lines.iter().enumerate().skip(start).take(rows) {
        let y = inner.y + 2 + (n - start) as u16;
        match line {
            ThemeLine::Heading(heading) => {
                let text = format!("{} · {}", heading.label().to_uppercase(), heading.blurb());
                row(
                    f,
                    Rect::new(inner.x + 2, y, list.saturating_sub(2), 1),
                    &text,
                    Tone::Muted,
                    t,
                );
            }
            ThemeLine::Theme(i, theme) => {
                let (i, theme) = (*i, *theme);
                let swatch = match super::theme::accent(theme) {
                    ratatui::style::Color::Rgb(r, g, b) => {
                        Tone::Pixel(Some(u32::from_be_bytes([0, r, g, b])), None)
                    }
                    _ => Tone::Strong,
                };
                row(f, Rect::new(inner.x + 4, y, 2, 1), "██", swatch, t);
                add(
                    f,
                    g,
                    Rect::new(inner.x + 7, inner.y, list.saturating_sub(7), inner.height),
                    y,
                    &format!(
                        "{} {}",
                        if i == panel.selected { "›" } else { " " },
                        theme.title()
                    ),
                    Action::PanelRow(i),
                    i == panel.selected,
                    t,
                );
            }
            ThemeLine::Gap => {}
        }
    }
    let Some(chosen) = panel.rows.get(panel.selected).and_then(theme_of) else {
        return;
    };
    let x = inner.x + list + 3;
    let area = Rect::new(
        x,
        inner.y + 2,
        inner.right().saturating_sub(x),
        inner.height.saturating_sub(4),
    );
    let Theme::Bird(bird) = chosen else {
        label(f, area, area.y, chosen.title(), Tone::Strong, t);
        label(
            f,
            area,
            area.y + 1,
            "no bird · the palette alone",
            Tone::Muted,
            t,
        );
        return;
    };
    let plumage = bird.plumage();
    let mut y = area.y;
    if s.truecolor && area.width as usize >= WIDTH && area.height as usize >= ROWS + 5 {
        for cells in sprite(bird, Mood::Done) {
            for (dx, (glyph, fg, bg)) in cells.into_iter().enumerate() {
                row(
                    f,
                    Rect::new(area.x + dx as u16, y, 1, 1),
                    &glyph.to_string(),
                    Tone::Pixel(fg, bg),
                    t,
                );
            }
            y += 1;
        }
        y += 1;
    }
    label(f, area, y, plumage.title, Tone::Strong, t);
    label(f, area, y + 1, plumage.latin, Tone::Muted, t);
    label(f, area, y + 2, plumage.nest, Tone::Muted, t);
    for (i, colour) in [plumage.accent, plumage.second, plumage.highlight]
        .into_iter()
        .enumerate()
    {
        row(
            f,
            Rect::new(area.x + i as u16 * 3, y + 4, 2, 1),
            "██",
            Tone::Pixel(Some(colour), None),
            t,
        );
    }
    if !s.truecolor {
        label(
            f,
            area,
            y + 6,
            "This terminal shows no true colour: the outline bird stands in.",
            Tone::Muted,
            t,
        );
    }
}

pub(super) fn draw_models(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    a: Rect,
    m: &super::Navigator,
    s: &ScreenState,
) {
    // **Three tabs, one list.** Which tier is being assigned is the first
    // thing on the sheet; the list below is every reachable model, grouped
    // under the account that serves it, and search narrows it -- no
    // carousel to step through to find a provider.
    let roles = [
        ("Main", "answers you", m.current.parent.clone()),
        (
            "Helper",
            "reads and summarises for Main",
            m.current.helper.clone().unwrap_or_else(|| "off".into()),
        ),
        (
            "Subagents",
            "work in parallel",
            m.current
                .subagent
                .clone()
                .unwrap_or_else(|| "favourites".into()),
        ),
    ];
    let mut x = a.x;
    for (i, (name, _, _)) in roles.iter().enumerate() {
        if m.target_key.is_some() && i != m.role {
            continue;
        }
        let w = chrome::chip(
            f,
            g,
            x,
            a.y + 2,
            a.right(),
            name,
            Action::ModelRole(i),
            i == m.role,
            Tone::Normal,
            None,
            s.theme,
        );
        x += w + 1;
    }
    if m.target_key.is_none() {
        let tab = "Tab switches";
        row(
            f,
            Rect::new(
                a.right().saturating_sub(tab.len() as u16),
                a.y + 2,
                tab.len() as u16,
                1,
            ),
            tab,
            Tone::Muted,
            s.theme,
        );
    }
    let (name, purpose, now) = &roles[m.role.min(2)];
    label(
        f,
        a,
        a.y + 3,
        &format!("{name} {purpose}. Now: {now}"),
        Tone::Muted,
        s.theme,
    );
    let mut y = a.y + 5;
    if m.role == 2 && m.target_key.is_none() {
        // The four favourites as cards, with what each holds: a slot is
        // chosen with ←→, and Enter puts the highlighted model in it.
        let slots: Vec<Option<&str>> = std::iter::once(None)
            .chain(crate::config::SLOT_NAMES.iter().copied().map(Some))
            .collect();
        let width = (a.width / slots.len() as u16).max(1);
        for (i, slot) in slots.iter().enumerate() {
            let x = a.x + i as u16 * width;
            let chosen = m.slot.as_deref() == *slot;
            let title = slot.map_or("PINNED".to_string(), str::to_uppercase);
            button(
                f,
                g,
                Rect::new(x, y, width.saturating_sub(1), 1),
                &format!("{} {title}", if chosen { "›" } else { " " }),
                Action::Slot(slot.map(str::to_owned)),
                chosen,
                s.theme,
            );
            let holds = match slot {
                Some(name) => m
                    .assignment
                    .slots
                    .get(*name)
                    .map_or("empty".to_string(), |held| {
                        format!("{} · {}", held.model, held.effort.name())
                    }),
                None => m.current.subagent.clone().unwrap_or_else(|| "none".into()),
            };
            row(
                f,
                Rect::new(x + 2, y + 1, width.saturating_sub(3), 1),
                &clip(&holds, width.saturating_sub(3) as usize),
                if holds == "empty" {
                    Tone::Muted
                } else {
                    Tone::Normal
                },
                s.theme,
            );
        }
        y += 3;
        let enabled = m.assignment.mode == crate::config::AgentsMode::Roster;
        add(
            f,
            g,
            a,
            y,
            if enabled {
                "Favourites are on · turn them off"
            } else {
                "Favourites are off · turn them on"
            },
            Action::Command(format!("/subagents {}", if enabled { "off" } else { "on" })),
            false,
            s.theme,
        );
        y += 2;
    }
    let rows = m.candidates();
    let placeholder = match (m.role, m.slot.as_deref()) {
        (2, Some(slot)) => format!("search a model for {}", slot.to_uppercase()),
        _ => "search models, providers or accounts".to_string(),
    };
    row(f, Rect::new(a.x, y, 2, 1), "┃ ", Tone::Accent, s.theme);
    let count = format!(
        "{} of {} · {}",
        rows.len(),
        m.catalogue_len(),
        if m.all_sources {
            "all accounts"
        } else {
            "connected accounts"
        }
    );
    let cw = chrome::width(&count);
    let query = if m.query.is_empty() {
        placeholder
    } else {
        format!("{}▏", m.query)
    };
    row(
        f,
        Rect::new(a.x + 2, y, a.width.saturating_sub(cw + 4), 1),
        &query,
        if m.query.is_empty() {
            Tone::Muted
        } else {
            Tone::Strong
        },
        s.theme,
    );
    row(
        f,
        Rect::new(a.right().saturating_sub(cw), y, cw, 1),
        &count,
        Tone::Muted,
        s.theme,
    );
    y += 2;
    // The list, with a header wherever the serving account changes. Sorted by
    // intelligence the accounts interleave, so each row carries its own.
    let mut lines: Vec<(Option<usize>, String)> = Vec::new();
    let mut last_route = String::new();
    for (i, c) in rows.iter().enumerate() {
        if !m.measured_order && c.route != last_route {
            lines.push((None, c.route.to_uppercase()));
            last_route = c.route.clone();
        }
        let score = c.score.map(|v| format!("★ {v:.0}")).unwrap_or_default();
        let lock = match (c.available, score.is_empty()) {
            (true, _) => "",
            (false, true) => "locked",
            (false, false) => "locked · ",
        };
        let via = if m.measured_order {
            format!("  {}", c.route)
        } else {
            String::new()
        };
        lines.push((Some(i), format!("{:<34}{lock}{score}{via}", c.model)));
    }
    let bottom = a.bottom().saturating_sub(5);
    let capacity = bottom.saturating_sub(y).max(1) as usize;
    let at = lines
        .iter()
        .position(|(i, _)| *i == Some(m.selected))
        .unwrap_or(0);
    let start = at.saturating_sub(capacity.saturating_sub(1));
    for (n, (index, text)) in lines.iter().enumerate().skip(start).take(capacity) {
        let ly = y + (n - start) as u16;
        match index {
            None => label(f, a, ly, text, Tone::Muted, s.theme),
            Some(i) => add(
                f,
                g,
                a,
                ly,
                &format!("{} {text}", if *i == m.selected { "›" } else { " " }),
                Action::Model(*i),
                *i == m.selected,
                s.theme,
            ),
        }
    }
    if rows.is_empty() {
        label(
            f,
            a,
            y,
            "No models match. Backspace removes a letter; Ctrl-U clears the search.",
            Tone::Warning,
            s.theme,
        );
    }
    if let Some(c) = rows.get(m.selected) {
        // A locked row says why before anything else: the header above it
        // already names the account.
        let detail = match c.reason.as_deref().filter(|_| !c.available) {
            Some(reason) => format!("{} is locked · {reason}", c.model),
            None => format!("{} · via {}", c.model, c.route),
        };
        label(
            f,
            a,
            bottom,
            &clip(&detail, a.width as usize),
            if c.available {
                Tone::Normal
            } else {
                Tone::Warning
            },
            s.theme,
        );
    }
    label(f, a, bottom + 1, &m.notice, Tone::Warning, s.theme);
    let enter = match (m.role, m.slot.as_deref(), m.target_key.is_some()) {
        (_, _, true) => "Enter · save to these settings".to_string(),
        (2, Some(slot), _) => format!("Enter · put in {}", slot.to_uppercase()),
        (i, _, _) => format!("Enter · use for {}", roles[i.min(2)].0),
    };
    let w = chrome::chip(
        f,
        g,
        a.x,
        bottom + 2,
        a.right(),
        &enter,
        Action::ChooseModel,
        true,
        Tone::Normal,
        None,
        s.theme,
    );
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
        chrome::chip(
            f,
            g,
            a.x + w + 1,
            bottom + 2,
            a.right(),
            &text,
            action,
            false,
            Tone::Normal,
            None,
            s.theme,
        );
    }
    let keys = if m.role == 2 && m.target_key.is_none() {
        "type to search · ↑↓ model · ←→ slot · Ctrl-A all accounts · Ctrl-O by intelligence"
    } else {
        "type to search · ↑↓ model · Ctrl-A all accounts · Ctrl-O by intelligence"
    };
    label(f, a, bottom + 3, keys, Tone::Muted, s.theme);
}

/// Credential display is mask-only; no plaintext reaches a render buffer.
/// A form sheet over the whole screen: its title and step, a sentence of
/// context, each field under its label with the one in focus outlined and
/// its caret showing where a paste lands, the check or the error under it,
/// and the keys that act on the sheet.
pub fn render_form(f: &mut Frame<'_>, form: &crate::tui::Form, theme: Theme) {
    use crate::tui::form::Kind;
    let a = f.area();
    f.render_widget(Clear, a);
    let sheet = Rect::new(
        a.x + a.width.saturating_sub(a.width.min(90)) / 2,
        a.y + 1,
        a.width.min(90),
        a.height.saturating_sub(2),
    );
    chrome::frame(f, sheet, Tone::Accent, theme);
    let inner = Rect::new(
        sheet.x + 2,
        sheet.y + 1,
        sheet.width.saturating_sub(4),
        sheet.height.saturating_sub(2),
    );
    let title = match form.step {
        Some((at, of)) => format!("{} · step {at} of {of}", form.title),
        None => form.title.clone(),
    };
    row(
        f,
        Rect::new(inner.x, inner.y, inner.width, 1),
        &title,
        Tone::Accent,
        theme,
    );
    let back = "Esc · back";
    row(
        f,
        Rect::new(
            inner.right().saturating_sub(back.len() as u16),
            inner.y,
            back.len() as u16,
            1,
        ),
        back,
        Tone::Muted,
        theme,
    );
    let mut y = inner.y + 2;
    for line in wrap_words(&form.intro, inner.width as usize) {
        label(f, inner, y, &line, Tone::Normal, theme);
        y += 1;
    }
    if let Some(warning) = &form.warning {
        y += 1;
        for line in wrap_words(&format!("⚠ {warning}"), inner.width as usize) {
            label(f, inner, y, &line, Tone::Warning, theme);
            y += 1;
        }
    }
    y += 1;
    for (index, field) in form.fields.iter().enumerate() {
        if y + 3 >= inner.bottom() {
            break;
        }
        let focused = index == form.focus;
        let name = if field.optional {
            format!("{} · optional", field.label.to_uppercase())
        } else {
            field.label.to_uppercase()
        };
        label(
            f,
            inner,
            y,
            &name,
            if focused { Tone::Accent } else { Tone::Muted },
            theme,
        );
        y += 1;
        match &field.kind {
            Kind::Choice(words) => {
                let mut x = inner.x;
                for (i, word) in words.iter().enumerate() {
                    let text = format!(" {word} ");
                    let w = chrome::width(&text).min(inner.right().saturating_sub(x));
                    let tone = if i == field.chosen() {
                        Tone::Accent
                    } else {
                        Tone::Muted
                    };
                    let text = if i == field.chosen() {
                        format!("[{word}]")
                    } else {
                        text
                    };
                    row(f, Rect::new(x, y, w + 1, 1), &text, tone, theme);
                    x += w + 2;
                }
            }
            _ => {
                let value = field.shown();
                let caret = if focused { "▏" } else { "" };
                let shown = if value.is_empty() && !focused {
                    "—".to_string()
                } else {
                    format!("{value}{caret}")
                };
                let marker = if focused { "┃ " } else { "  " };
                row(f, Rect::new(inner.x, y, 2, 1), marker, Tone::Accent, theme);
                row(
                    f,
                    Rect::new(inner.x + 2, y, inner.width.saturating_sub(2), 1),
                    &clip(&shown, inner.width.saturating_sub(2) as usize),
                    Tone::Strong,
                    theme,
                );
            }
        }
        y += 1;
        let under = match (&form.error, field.verdict()) {
            (Some((at, error)), _) if *at == index => Some((format!("✕ {error}"), Tone::Failure)),
            (_, Some(Err(problem))) => Some((format!("✕ {problem}"), Tone::Warning)),
            (_, Some(Ok(praise))) => Some((format!("✓ {praise}"), Tone::Success)),
            _ if focused && !field.hint.is_empty() => Some((field.hint.clone(), Tone::Muted)),
            _ => None,
        };
        if let Some((text, tone)) = under {
            label(f, inner, y, &clip(&text, inner.width as usize), tone, theme);
        }
        y += 2;
    }
    if let Some(help) = &form.help
        && y < inner.bottom().saturating_sub(2)
    {
        label(
            f,
            inner,
            y,
            &clip(help, inner.width as usize),
            Tone::Muted,
            theme,
        );
    }
    let secret = form
        .fields
        .get(form.focus)
        .is_some_and(|field| field.kind == Kind::Secret);
    let choice = form
        .fields
        .get(form.focus)
        .is_some_and(|field| matches!(field.kind, Kind::Choice(_)));
    let mut keys = format!("Enter {}", form.submit);
    if form.fields.len() > 1 {
        keys.push_str(" · Tab next field");
    }
    if choice {
        keys.push_str(" · ←→ choose");
    }
    if secret {
        keys.push_str(" · Ctrl-R show · paste here");
    }
    keys.push_str(" · Ctrl-U clear · Esc back");
    label(
        f,
        inner,
        inner.bottom().saturating_sub(1),
        &keys,
        Tone::Muted,
        theme,
    );
}
