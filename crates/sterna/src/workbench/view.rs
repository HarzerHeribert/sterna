use super::{
    Action, CellTab, Document, Geometry, Tone, Workbench, chrome,
    document::{RowKind, clip},
    theme, voice,
};
use crate::contract::{Conversation, ServedBy};
use crate::tui::{Activity, Notebook, ScreenState, StatusLine, Theme};
use ratatui::{Frame, layout::Rect, text::Span, widgets::Clear};

pub(super) fn row(f: &mut Frame<'_>, r: Rect, text: &str, tone: Tone, theme: Theme) {
    chrome::text(f, r, text, tone, theme);
}
pub(super) fn button(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    r: Rect,
    text: &str,
    action: Action,
    selected: bool,
    t: Theme,
) {
    let r = r.intersection(f.area());
    row(
        f,
        r,
        text,
        if selected { Tone::Accent } else { Tone::Normal },
        t,
    );
    if r.width > 0 && r.height > 0 {
        g.hits.push((Rect::new(r.x, r.y, r.width, 1), action));
    }
}
pub(super) fn full_row(area: Rect, y: u16) -> Rect {
    Rect::new(area.x, y, area.width, u16::from(y < area.bottom()))
}
pub(super) fn label(f: &mut Frame<'_>, a: Rect, y: u16, text: &str, tone: Tone, t: Theme) {
    if y < a.bottom() {
        row(f, full_row(a, y), text, tone, t);
    }
}

fn network_word(s: &ScreenState) -> &'static str {
    match s.network.as_deref() {
        Some("on") => "network on",
        Some("off") => "network off",
        _ => "network unknown",
    }
}
/// The level chip's tone: Full access is a warning, so it is never dropped
/// and never forgotten.
fn level_tone(s: &ScreenState) -> Tone {
    if s.full_access() {
        Tone::Warning
    } else {
        Tone::Normal
    }
}
/// Right-aligned chips that give room up in a fixed order, so a narrow
/// terminal loses the least useful control rather than the leftmost one.
///
/// Each item carries a rank: the highest rank is dropped first. A control
/// drawn in a warning tone is never dropped -- a boundary that can be lifted
/// has to be continuously visible or it is a mode error waiting to happen --
/// and the way into settings goes last, because it is the way to everything
/// else. What is dropped is not gone: it is listed behind one `⟨ ⋯ ⟩` chip
/// at the end of the row.
fn controls(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    a: Rect,
    reserved: u16,
    items: &[(String, Action, Tone, u8)],
    press: Option<(u16, u16)>,
    t: Theme,
) {
    const MORE: &str = "⋯";
    let room = a.width.saturating_sub(reserved);
    let mut keep: Vec<usize> = (0..items.len()).collect();
    // The row's width with the fold chip when anything is folded.
    let width = |keep: &Vec<usize>| -> u16 {
        let more = if keep.len() < items.len() {
            chrome::width(MORE) + 4 + 1
        } else {
            0
        };
        (keep
            .iter()
            .map(|i| chrome::width(&items[*i].0) + 4 + 1)
            .sum::<u16>()
            + more)
            .saturating_sub(1)
    };
    // Settings goes last, into the fold like the rest; a warning never does.
    while width(&keep) > room {
        let Some(drop) = keep
            .iter()
            .filter(|i| !matches!(items[**i].2, Tone::Warning))
            .max_by_key(|i| items[**i].3)
            .copied()
        else {
            break;
        };
        keep.retain(|i| *i != drop);
    }
    if width(&keep) > room {
        return;
    }
    let folded: Vec<(String, Action)> = (0..items.len())
        .filter(|i| !keep.contains(i))
        .map(|i| (items[i].0.clone(), items[i].1.clone()))
        .collect();
    let mut x = a.right().saturating_sub(width(&keep) + 1);
    for i in keep {
        let (text, action, tone, _) = &items[i];
        let w = chrome::chip(
            f,
            g,
            x,
            a.y,
            a.right(),
            text,
            action.clone(),
            false,
            *tone,
            press,
            t,
        );
        x += w + 1;
    }
    if !folded.is_empty() {
        chrome::chip(
            f,
            g,
            x,
            a.y,
            a.right(),
            MORE,
            Action::More(folded),
            false,
            Tone::Normal,
            press,
            t,
        );
    }
}
/// `⠿ STERNA  project` and the session's controls, one row, every width.
///
/// The facts are the ones a person acts on -- which model answers, how
/// often it asks, what it may do, and whether it is confined -- and each of
/// them is the chip that changes it.
fn session_bar(f: &mut Frame<'_>, g: &mut Geometry, a: Rect, s: &ScreenState, ui: &Workbench) {
    // A long project name must not cost you the session's controls: it gets
    // a quarter of what the brand leaves of the row, and clips.
    let brand = " ⠿ STERNA /";
    let project = clip(
        s.project.as_deref().unwrap_or("workspace"),
        (a.width.saturating_sub(chrome::width(brand)) as usize / 4).max(8),
    );
    row(f, a, brand, Tone::Accent, s.theme);
    row(
        f,
        Rect::new(a.x + chrome::width(brand) + 1, a.y, a.width, 1),
        &project,
        Tone::Strong,
        s.theme,
    );
    let model = clip(s.model.as_deref().unwrap_or("choose model"), 18);
    controls(
        f,
        g,
        a,
        chrome::width(brand) + 1 + chrome::width(&project) + 2,
        &[
            (format!("{model} ▾"), Action::Models, Tone::Normal, 3),
            // The level outranks the model's name: at eighty columns a
            // session must still say how much runs without asking.
            (
                super::facts::level_word(s),
                Action::Sandbox,
                level_tone(s),
                1,
            ),
            ("Settings".to_string(), Action::Settings, Tone::Normal, 0),
            ("?".to_string(), Action::Help, Tone::Normal, 5),
        ],
        ui.press,
        s.theme,
    );
}
/// The empty columns kept between the transcript and the sidebar's rule.
const SIDEBAR_GUTTER: u16 = 2;

pub struct Layout {
    pub transcript: Rect,
    sidebar: Option<Rect>,
    header: u16,
    footer: u16,
    /// The composer's width, the one every composer computation uses: the
    /// wrap, the drawing, the caret and a click.
    composer_width: u16,
    composer_height: u16,
    queue_height: u16,
    body_height: u16,
}
pub fn layout(a: Rect, s: &ScreenState) -> Layout {
    let chrome = !s.fullscreen;
    // The bar and the rule under it.
    let header = if chrome && a.height > 7 { 2 } else { 0 };
    // The composer dock's bottom edge, which carries the everyday chips.
    let footer = u16::from(chrome && a.height > 4);
    let textwidth = a.width.saturating_sub(2).max(1);
    let composer_width = composer_width(a, s);
    let composer_rows = composer_lines(&s.input, composer_width as usize).len();
    // The lines typed plus the dock's top edge, which is the activity line.
    let composer_height = (composer_rows as u16)
        .clamp(1, 5)
        .saturating_add(1)
        .min(a.height.saturating_sub(header + footer).max(1));
    let queue_height = (s.queued.len() as u16)
        .min(2)
        .min(a.height.saturating_sub(header + footer + composer_height));
    let body_height = a
        .height
        .saturating_sub(header + footer + composer_height + queue_height);
    let transcript = Rect::new(
        a.x + u16::from(a.width > 1),
        a.y + header,
        textwidth,
        body_height,
    );
    let side_width = if !s.fullscreen
        && body_height >= 10
        && a.width >= 100
        && match s.sidebar {
            crate::tui::SidebarVisibility::Shown => true,
            crate::tui::SidebarVisibility::Hidden => false,
            crate::tui::SidebarVisibility::Auto => a.width >= 120,
        } {
        30
    } else {
        0
    };
    let mut transcript = transcript;
    transcript.width = transcript.width.saturating_sub(side_width);
    let sidebar = (side_width > 0).then(|| {
        Rect::new(
            transcript.right() + 2,
            transcript.y,
            side_width - 2,
            body_height,
        )
    });
    // Empty columns between the transcript and the sidebar's rule: a line
    // that ran to the transcript's last column touched the rule.
    if sidebar.is_some() {
        transcript.width = transcript.width.saturating_sub(SIDEBAR_GUTTER);
    }
    // **One empty row between the conversation and the composer**: the text
    // that scrolls never runs into the box that stays put. The sidebar keeps
    // its full height, and its rule still meets the box.
    let gap = u16::from(body_height > 4);
    transcript.height = body_height - gap;
    Layout {
        header,
        footer,
        composer_width,
        composer_height,
        queue_height,
        body_height,
        transcript,
        sidebar,
    }
}

pub fn render(
    f: &mut Frame<'_>,
    c: &Conversation,
    n: &Notebook,
    s: &ScreenState,
    _served: &ServedBy,
    ui: &mut Workbench,
) {
    // Every style this frame is drawn with reads the terminal's ground and
    // colour depth from here.
    super::look::set(super::look::Look {
        light: s.light,
        truecolor: s.truecolor,
    });
    let a = f.area();
    f.render_widget(Clear, a);
    f.buffer_mut()
        .set_style(a, theme::style(Tone::Normal, s.theme));
    let mut g = Geometry::default();
    if a.height == 0 || a.width == 0 {
        ui.geometry = g;
        return;
    }
    let Layout {
        header,
        footer,
        composer_width,
        composer_height,
        queue_height,
        body_height,
        transcript,
        sidebar,
    } = layout(a, s);
    g.transcript = transcript;
    // The rows the conversation shows: the body less the row kept empty
    // above the composer.
    let shown = transcript.height;
    let d = Document::build(c, n, s, ui, transcript.width as usize);
    g.rows = d.rows.len();
    g.start = g
        .rows
        .saturating_sub(shown as usize)
        .saturating_sub(s.scrollback);
    ui.anchor = d.rows.get(g.start).map(|row| (row.key, row.text.clone()));
    ui.last_scrollback = s.scrollback;
    ui.age_notice();
    let gutter = sidebar.map(|side| side.x - 2);
    if header > 0 {
        session_bar(f, &mut g, Rect::new(a.x, a.y, a.width, 1), s, ui);
        chrome::rule(
            f,
            Rect::new(a.x, a.y + 1, a.width, 1),
            &gutter.map(|x| (x, "┬")).into_iter().collect::<Vec<_>>(),
            Tone::Line,
            s.theme,
        );
    }
    if let Some(x) = gutter {
        for y in transcript.y..transcript.y + body_height {
            row(f, Rect::new(x, y, 1, 1), "│", Tone::Line, s.theme);
        }
    }
    // Scrolled back, the transcript's last row is the way back down: it
    // holds the `↓ latest` chip and nothing under it, so the chip never
    // covers a card's edge or a line of text.
    let latest_row = usize::from(s.scrollback > 0 && shown > 1);
    for (j, r) in d
        .rows
        .iter()
        .skip(g.start)
        .take((shown as usize).saturating_sub(latest_row))
        .enumerate()
    {
        let area = Rect::new(
            g.transcript.x,
            g.transcript.y + j as u16,
            g.transcript.width,
            1,
        );
        draw_row(f, &mut g, area, r, s, ui);
    }
    if s.scrolling && g.rows > shown as usize && shown > 0 && g.transcript.width > 0 {
        let height = shown as usize;
        let thumb = (height * height / g.rows).max(1);
        let top = g.start.min(g.rows - height) * (height - thumb) / (g.rows - height);
        for offset in top..(top + thumb).min(height) {
            row(
                f,
                Rect::new(
                    g.transcript.right() - 1,
                    g.transcript.y + offset as u16,
                    1,
                    1,
                ),
                "▐",
                Tone::Accent,
                s.theme,
            );
        }
    }
    if latest_row > 0 {
        let text = "↓ latest";
        let w = chrome::width(text) + 4;
        // Left of the scrollbar's column, on the row kept for it.
        let (x, y, limit) = (
            g.transcript.right().saturating_sub(w + 2),
            g.transcript.bottom() - 1,
            g.transcript.right().saturating_sub(1),
        );
        chrome::chip(
            f,
            &mut g,
            x,
            y,
            limit,
            text,
            Action::Latest,
            true,
            Tone::Normal,
            ui.press,
            s.theme,
        );
    }
    g.sidebar = sidebar;
    if let Some(side) = sidebar {
        session_card(f, &mut g, side, n, s, ui);
    }
    let mut y = transcript.y + body_height;
    for q in s.queued.iter().rev().take(queue_height as usize).rev() {
        row(
            f,
            Rect::new(a.x, y, a.width, 1),
            &format!(" QUEUED › {}", q.lines().next().unwrap_or("")),
            Tone::Normal,
            s.theme,
        );
        y += 1;
    }
    let boxed = !s.fullscreen && a.width > 8;
    // The composer's window, worked out before the dock's edges are drawn,
    // because they say how much of the draft is out of view.
    let lines = composer_lines(&s.input, composer_width as usize);
    let cursor = s.cursor.unwrap_or(s.input.len()).min(s.input.len());
    let (caret_row, caret_col) = caret(&lines, &s.input, cursor);
    let skip = ui.composer_window(
        lines.len(),
        composer_height.saturating_sub(1) as usize,
        cursor,
        caret_row,
    );
    // The number the running card carries: a program being executed is in
    // the conversation already, and one being written is the next.
    let running_cell = match s.activity {
        Activity::Executing => Some(crate::tui::cell_ordinal(c, n)),
        Activity::Streaming => Some(crate::tui::cell_ordinal(c, n) + 1),
        _ => None,
    };
    if y < a.bottom() {
        dock_top(
            f,
            &mut g,
            Rect::new(a.x, y, a.width, 1),
            s,
            ui,
            gutter,
            running_cell,
            boxed,
            if sidebar.is_none() {
                &n.acceptance
            } else {
                &[]
            },
        );
        y += 1;
    }
    // `❯` marks where typing lands, exactly as it marks what was already
    // said in the transcript; the editor starts after it.
    let prompt = if boxed { 4 } else { u16::from(a.width > 6) * 3 };
    if prompt > 0 && y < a.bottom() {
        row(
            f,
            Rect::new(a.x, y, prompt, 1),
            if boxed { "│ ❯ " } else { " ❯ " },
            Tone::Accent,
            s.theme,
        );
        if boxed {
            row(f, Rect::new(a.x, y, 1, 1), "│", Tone::Line, s.theme);
        }
    }
    g.composer = Rect::new(
        a.x + prompt.max(u16::from(a.width > 1)),
        y,
        composer_width,
        composer_height
            .saturating_sub(1)
            .min(a.bottom().saturating_sub(y)),
    );
    let visible = g.composer.height as usize;
    for (i, (_, l)) in lines.iter().skip(skip).take(visible).enumerate() {
        let yy = g.composer.y + i as u16;
        row(
            f,
            Rect::new(g.composer.x, yy, g.composer.width, 1),
            l,
            Tone::Normal,
            s.theme,
        );
        if boxed {
            row(f, Rect::new(a.x, yy, 1, 1), "│", Tone::Line, s.theme);
            row(
                f,
                Rect::new(a.right() - 1, yy, 1, 1),
                "│",
                Tone::Line,
                s.theme,
            );
        }
    }
    if s.input.is_empty() {
        row(f, g.composer, voice::PLACEHOLDER, Tone::Muted, s.theme);
    }
    g.hits.push((g.composer, Action::Composer));
    if !ui.is_local() && s.form.is_none() && visible > 0 && caret_row >= skip {
        let position = (
            g.composer.x + caret_col.min(g.composer.width.saturating_sub(1) as usize) as u16,
            g.composer.y + (caret_row - skip) as u16,
        );
        if caret_row - skip < visible && super::contains(a, position.0, position.1) {
            f.set_cursor_position(position);
        }
    }
    if footer > 0 && a.height >= footer {
        dock_bottom(
            f,
            &mut g,
            Rect::new(a.x, a.bottom() - 1, a.width, 1),
            n,
            s,
            ui,
        );
    }
    // The popup: the commands a slash word matches, or the paths an `@`
    // word does. A click takes the row; the wheel moves the selection.
    if !ui.is_local() && !s.completions.is_empty() {
        let capacity = (g.transcript.height.min(7) as usize).max(1);
        let total = s.completions.len();
        // When the rows do not all fit, the last line says how many more.
        let shown = if total > capacity {
            capacity.saturating_sub(1).max(1)
        } else {
            total
        };
        let selected = s.completion_selected.min(total - 1);
        let first = selected.saturating_sub(shown.saturating_sub(1));
        let height = shown + usize::from(total > shown);
        let top = g.transcript.bottom().saturating_sub(height as u16);
        let area = Rect::new(g.transcript.x, top, g.transcript.width, height as u16);
        f.render_widget(Clear, area);
        for (i, (label, help)) in s.completions.iter().enumerate().skip(first).take(shown) {
            let line = Rect::new(area.x, top + (i - first) as u16, area.width, 1);
            let mark = if i == selected { "›" } else { " " };
            button(
                f,
                &mut g,
                line,
                &format!("{mark} {label:14} {help}"),
                Action::Completion(i),
                i == selected,
                s.theme,
            );
        }
        let rest = total - shown;
        if total > shown {
            row(
                f,
                Rect::new(area.x, area.bottom() - 1, area.width, 1),
                &format!("  ↕ {rest} more · ↑↓ or the wheel"),
                Tone::Muted,
                s.theme,
            );
        }
        g.popup = Some(area);
    }
    if s.telemetry_open && !ui.is_local() {
        // The instruments take the transcript's room, never the status line
        // that carries the context reading they are read against.
        let area = Rect::new(a.x, a.y + header, a.width, body_height + queue_height);
        f.render_widget(Clear, area);
        f.buffer_mut()
            .set_style(area, theme::style(Tone::Normal, s.theme));
        // Nothing hidden under the instruments stays clickable; the chrome
        // around them does.
        g.hits.retain(|(r, _)| !r.intersects(area));
        crate::tui::telemetry::expanded(f, area, c, _served, n, s);
        // Drawn with colours of its own: they are taken through the look.
        super::look::adopt(f.buffer_mut(), area, super::look::get());
        let back = "Esc · Back";
        let w = chrome::width(back) + 4;
        if area.width > w + 2 && area.height > 0 {
            chrome::chip(
                f,
                &mut g,
                area.right().saturating_sub(w + 1),
                area.y,
                area.right(),
                back,
                Action::Close,
                false,
                Tone::Normal,
                ui.press,
                s.theme,
            );
        }
    }
    if ui.is_local() {
        super::sheets::build(ui, s, n);
        surface(f, &mut g, a, s, ui);
    }
    g.screen = Some(f.buffer_mut().clone());
    if let Some(sel) = s.selection {
        g.copied = crate::tui::draw_selection(f.buffer_mut(), a, sel, g.start);
    }
    ui.geometry = g;
}

/// One transcript row, drawn in the shape its kind asks for.
fn draw_row(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    area: Rect,
    r: &super::Row,
    s: &ScreenState,
    ui: &Workbench,
) {
    let t = s.theme;
    let wide = area.width >= 12;
    // Where the card's two edges stand: three columns in, and one column
    // short of the right so the gutter never touches it.
    let left = area.x + 3;
    let right = area.right().saturating_sub(2);
    let spans_at = |f: &mut Frame<'_>, g: &mut Geometry, x: u16, limit: u16| {
        let mut x = x;
        if !r.spans.is_empty() {
            for (i, (text, tone)) in r.spans.iter().enumerate() {
                let w = (chrome::width(text)).min(limit.saturating_sub(x));
                if w == 0 {
                    break;
                }
                let at = Rect::new(x, area.y, w, 1);
                row(f, at, text, *tone, t);
                // A link opens on a click, asking first; a path in any
                // other run opens its file.
                match r.links.iter().find(|(span, _)| *span == i) {
                    Some((_, address)) => {
                        f.buffer_mut().set_style(
                            at,
                            theme::style(*tone, t)
                                .add_modifier(ratatui::style::Modifier::UNDERLINED),
                        );
                        g.hits.push((at, Action::AskOpenLink(address.clone())));
                    }
                    None if *tone != Tone::Line => paths(f, g, at, text, *tone, s),
                    None => {}
                }
                x += w;
            }
        } else {
            let w = chrome::width(&r.text).min(limit.saturating_sub(x));
            row(f, Rect::new(x, area.y, w, 1), &r.text, r.tone, t);
            paths(f, g, Rect::new(x, area.y, w, 1), &r.text, r.tone, s);
        }
        x
    };
    match &r.kind {
        RowKind::You if wide => {
            row(f, Rect::new(area.x + 1, area.y, 1, 1), "┃", Tone::You, t);
            spans_at(f, g, area.x + 3, area.right());
        }
        RowKind::Sterna if wide => {
            spans_at(f, g, area.x + 1, area.right());
        }
        RowKind::CardTop { open, right: state } if wide => {
            let (lead, tone) = if *open {
                ("╭─ ", Tone::Line)
            } else {
                ("▸ ", Tone::Line)
            };
            row(
                f,
                Rect::new(left, area.y, chrome::width(lead), 1),
                lead,
                tone,
                t,
            );
            let end = spans_at(f, g, left + chrome::width(lead), right);
            let state_w: u16 = state.iter().map(|(x, _)| chrome::width(x)).sum();
            // The state word sits one space in from the corner, and the
            // fill stops one space short of it.
            let state_x = right.saturating_sub(state_w + 3);
            if *open && state_x > end + 2 {
                let fill = state_x - end - 2;
                row(
                    f,
                    Rect::new(end + 1, area.y, fill, 1),
                    &"─".repeat(fill as usize),
                    Tone::Line,
                    t,
                );
            }
            let mut x = state_x.max(end + 1);
            for (text, tone) in state {
                let w = chrome::width(text).min(right.saturating_sub(x));
                row(f, Rect::new(x, area.y, w, 1), text, *tone, t);
                x += w;
            }
            if *open {
                row(f, Rect::new(right - 2, area.y, 2, 1), "─╮", Tone::Line, t);
            }
            if let Some(act) = &r.action {
                g.hits.push((area, act.clone()));
            }
        }
        RowKind::CardBody if wide => {
            row(f, Rect::new(left, area.y, 1, 1), "│", Tone::Line, t);
            row(f, Rect::new(right - 1, area.y, 1, 1), "│", Tone::Line, t);
            let inner_x = left + 2;
            let limit = right - 2;
            if let (Some(Action::Tab(cell, current)), false) = (&r.action, r.tabs.is_empty()) {
                let (cell, current) = (*cell, *current);
                // The open tab is never the one folded.
                let tabs: Vec<(String, Action, bool)> = r
                    .tabs
                    .iter()
                    .map(|(label, tab)| (label.clone(), Action::Tab(cell, *tab), current == *tab))
                    .collect();
                let x = chrome::chips(f, g, inner_x, area.y, limit, &tabs, ui.press, t);
                // Whatever the strip left over -- the route to the whole diff.
                let rest: String = r.spans.iter().map(|(t, _)| t.as_str()).collect();
                let rest = rest.trim_start();
                let w = chrome::width(rest);
                if x + w < limit {
                    button(
                        f,
                        g,
                        Rect::new(limit - w, area.y, w, 1),
                        rest,
                        Action::Tab(cell, CellTab::Diff),
                        false,
                        t,
                    );
                }
            } else if !r.chips.is_empty() {
                chrome::chips(f, g, inner_x, area.y, limit, &r.chips, ui.press, t);
            } else {
                spans_at(f, g, inner_x, limit);
                if let Some(act) = &r.action {
                    g.hits
                        .push((Rect::new(inner_x, area.y, limit - inner_x, 1), act.clone()));
                }
            }
        }
        RowKind::CardBottom if wide => {
            row(f, Rect::new(left, area.y, 2, 1), "╰─", Tone::Line, t);
            let mut x = left + 2;
            if !r.spans.is_empty() {
                row(f, Rect::new(x, area.y, 1, 1), " ", Tone::Line, t);
                x += 1;
                x = spans_at(f, g, x, right.saturating_sub(2));
                row(f, Rect::new(x, area.y, 1, 1), " ", Tone::Line, t);
                x += 1;
            }
            let fill = right.saturating_sub(x + 1);
            row(
                f,
                Rect::new(x, area.y, fill, 1),
                &"─".repeat(fill as usize),
                Tone::Line,
                t,
            );
            row(f, Rect::new(right - 1, area.y, 1, 1), "╯", Tone::Line, t);
        }
        _ if !r.chips.is_empty() => {
            chrome::chips(
                f,
                g,
                area.x + 3,
                area.y,
                area.right(),
                &r.chips,
                ui.press,
                t,
            );
        }
        _ => {
            spans_at(f, g, area.x, area.right());
            if let Some(act) = &r.action {
                g.hits.push((area, act.clone()));
            }
        }
    }
}
/// Existing paths in a plain row are underlined and open on a click.
fn paths(f: &mut Frame<'_>, g: &mut Geometry, area: Rect, text: &str, tone: Tone, s: &ScreenState) {
    let Some(root) = &s.settings_root else {
        return;
    };
    for (start, end) in crate::tui::found_paths(text, root) {
        let prefix: String = text.chars().take(start).collect();
        let found: String = text.chars().skip(start).take(end - start).collect();
        let x = area.x.saturating_add(chrome::width(&prefix));
        let width = chrome::width(&found).min(area.right().saturating_sub(x));
        if width > 0
            && let Some(path) = crate::tui::resolve_path(&found, root)
        {
            let rect = Rect::new(x, area.y, width, 1);
            f.buffer_mut().set_style(
                rect,
                theme::style(tone, s.theme).add_modifier(ratatui::style::Modifier::UNDERLINED),
            );
            g.hits
                .push((rect, Action::Path(path.display().to_string())));
        }
    }
}
/// The standing card: what this session is, not what it did. Everything
/// here is also reachable from a control, so hiding the card never hides a
/// fact -- and every line that names a choice is the control that changes it.
fn session_card(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    side: Rect,
    n: &Notebook,
    s: &ScreenState,
    ui: &Workbench,
) {
    let t = s.theme;
    let id = s
        .history
        .iter()
        .find_map(|note| note.text.strip_prefix("session "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string);
    let files: usize = n
        .cells
        .iter()
        .map(|c| {
            c.changes
                .as_deref()
                .map_or(0, |d| d.lines().filter(|l| l.starts_with("+++ ")).count())
        })
        .sum();
    let (ok, failed) = n.program_cells().fold((0, 0), |(ok, bad), (_, c)| {
        if c.error.is_some() {
            (ok, bad + 1)
        } else if c.execution.is_some() {
            (ok + 1, bad)
        } else {
            (ok, bad)
        }
    });
    let running = usize::from(matches!(s.activity, Activity::Executing));
    let session_line = match (id, s.pulse.elapsed_ms) {
        (Some(id), 0) => id,
        (Some(id), ms) => format!("{id} · {} on this turn", clip_clock(ms)),
        (None, 0) => String::new(),
        (None, ms) => format!("{} on this turn", clip_clock(ms)),
    };
    // A heading over nothing is not drawn.
    let mut lines: Vec<(String, Tone, Option<Action>)> = if session_line.is_empty() {
        Vec::new()
    } else {
        vec![
            ("THIS SESSION".into(), Tone::Accent, None),
            (session_line, Tone::Muted, None),
            (String::new(), Tone::Normal, None),
        ]
    };
    lines.extend([
        ("GUARDRAILS".into(), Tone::Accent, None),
        (
            super::facts::level_word(s),
            level_tone(s),
            Some(Action::Sandbox),
        ),
        (
            s.level.level().asks().to_string(),
            Tone::Muted,
            Some(Action::Sandbox),
        ),
        (network_word(s).into(), Tone::Muted, Some(Action::Sandbox)),
        (String::new(), Tone::Normal, None),
        ("MODEL".into(), Tone::Accent, None),
        (
            s.model.as_deref().unwrap_or("choose model").into(),
            Tone::Normal,
            Some(Action::Models),
        ),
        // Two lines, two targets: the effort steps where it stands, and
        // the helpers open the settings that turn them on.
        (
            super::facts::effort_word(s),
            Tone::Muted,
            Some(Action::Effort),
        ),
        (
            format!("helpers {}", if s.helpers_on { "on" } else { "off" }),
            Tone::Muted,
            Some(Action::SettingsAt(2)),
        ),
    ]);
    if let Some(word) = &s.subagents {
        lines.push((
            format!("subagents {word}"),
            Tone::Muted,
            Some(Action::Command("/subagents".into())),
        ));
    }
    lines.push((String::new(), Tone::Normal, None));
    lines.push(("SO FAR".into(), Tone::Accent, None));
    let ran = n.program_cells().count();
    lines.push((
        format!(
            "{} {} · {} {}{}",
            ran,
            if ran == 1 { "cell" } else { "cells" },
            files,
            if files == 1 { "file" } else { "files" },
            n.tokens
                .as_ref()
                .map(|t| format!(" · {}", compact(t.used)))
                .unwrap_or_default()
        ),
        Tone::Normal,
        None,
    ));
    // Each count on its own line, and only when there is one: the words
    // say what the marks mean, and the narrow card never cuts them.
    for (count, said) in [(ok, "ran"), (running, "running"), (failed, "failed")] {
        if count > 0 {
            let mark = match said {
                "ran" => "✓",
                "running" => "●",
                _ => "✕",
            };
            lines.push((format!("{mark} {count} {said}"), Tone::Muted, None));
        }
    }
    // The task's acceptance list, when it has one, is one line of progress:
    // its items cut to the card's width could not be read, and the whole
    // list is one click away.
    if !n.acceptance.is_empty() {
        lines.push((
            format!("{} ▸", crate::acceptance::glance(&n.acceptance)),
            acceptance_tone(&n.acceptance, Tone::Accent),
            Some(Action::Acceptance),
        ));
    }
    lines.push((String::new(), Tone::Normal, None));
    lines.push(("◇ HELPERS".into(), Tone::Helper, None));
    // The newest cell that called a helper: the turn ending does not make
    // the sidebar forget them.
    let with_helpers = n.last_with_helpers();
    let helpers = with_helpers
        .and_then(|cell| n.cells.get(cell - 1))
        .map_or(&[][..], |c| c.helpers.as_slice());
    if helpers.is_empty() {
        lines.push((
            if s.helpers_on { "none yet" } else { "off" }.into(),
            Tone::Muted,
            Some(Action::SettingsAt(2)),
        ));
    }
    for (i, helper) in helpers.iter().enumerate() {
        lines.push((
            format!(
                "{} · {}",
                helper.helper,
                if helper.outcome.ok {
                    "returned"
                } else if helper.outcome.text.is_empty() {
                    "waiting"
                } else {
                    "failed"
                }
            ),
            if helper.outcome.ok || helper.outcome.text.is_empty() {
                Tone::Muted
            } else {
                Tone::Failure
            },
            // The same lane the card shows, opened there.
            Some(Action::Helper(with_helpers.unwrap_or(0), i)),
        ));
    }
    for (i, (text, tone, action)) in lines.iter().enumerate() {
        let y = side.y + i as u16;
        if y + 1 >= side.bottom() {
            break;
        }
        let text = clip(text, side.width as usize);
        label(f, side, y, &text, *tone, t);
        if let Some(action) = action
            && !text.is_empty()
        {
            g.hits.push((
                Rect::new(side.x, y, chrome::width(&text), 1),
                action.clone(),
            ));
        }
    }
    // The card's own routes: the notices it summarises, and the instruments
    // behind the numbers it shows.
    let y = side.bottom().saturating_sub(1);
    if y > side.y {
        chrome::chips(
            f,
            g,
            side.x,
            y,
            side.right(),
            &[
                ("activity".into(), Action::Activity, false),
                ("telemetry".into(), Action::Telemetry, false),
            ],
            ui.press,
            t,
        );
    }
}
fn clip_clock(ms: u64) -> String {
    super::document::clock(ms)
}
fn compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M tok", tokens as f64 / 1_000_000.)
    } else if tokens >= 1_000 {
        format!("{:.1}k tok", tokens as f64 / 1_000.)
    } else {
        format!("{tokens} tok")
    }
}
/// The dock's top edge: the live status, a notice that just happened, and
/// the way to undo it. This line is also the composer's top edge.
#[allow(clippy::too_many_arguments)]
fn dock_top(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    a: Rect,
    s: &ScreenState,
    ui: &Workbench,
    gutter: Option<u16>,
    cell: Option<usize>,
    boxed: bool,
    acceptance: &[crate::acceptance::Verdict],
) {
    let t = s.theme;
    let running = s.activity.working();
    // The turn is complete once its check has had its say, not before.
    let said = if s.activity == Activity::Complete && s.behind.iter().any(|lane| lane == "check") {
        voice::CHECKING.to_string()
    } else {
        voice::status(s.activity, cell, s.streaming_tool_input.is_some())
    };
    let status = format!(
        "{said}{}",
        if s.stopping { " · stop requested" } else { "" }
    );
    let lead = format!(
        "{} {} {} ",
        if boxed { "╭─" } else { "" },
        super::motion::dock_mark(s),
        status
    );
    let lead = lead.trim_start().to_string();
    let lead = format!("{}{lead}", if boxed { "" } else { " " });
    let tone = if running { Tone::Accent } else { Tone::Muted };
    let mut used = chrome::width(&lead).min(a.width);
    row(f, a, &lead, tone, t);
    if boxed {
        row(f, Rect::new(a.x, a.y, 2, 1), "╭─", Tone::Line, t);
    }
    // A notice rides this line, where the eye already is and beside the
    // composer it answers. It is also a row in the transcript, to scroll
    // back to. It fades after a few seconds; the undo beside it goes with it.
    let notice = if ui.notice_visible() {
        Some(ui.notice.clone())
    } else if let Some(notice) = &s.notice {
        Some(notice.clone())
    } else if s.mouse_off {
        Some("Mouse released · Ctrl-G captures again".to_string())
    } else if s.fullscreen {
        // With the chrome gone, the way back stays in view.
        Some("Fullscreen · Ctrl-F restores".to_string())
    } else {
        None
    }
    // A failure is already said where it is read -- the dock's own
    // "failed" and the row in the transcript -- so it does not ride the dock
    // a third time.
    .filter(|notice| !notice.starts_with("ERROR:"));
    if let Some(notice) = &notice
        && used + 8 < a.width
    {
        // The undo is the point of offering a change back, so the notice
        // is cut to leave it room, never the other way round.
        let undo = ui
            .changes
            .last()
            .filter(|_| ui.offer_undo && ui.notice_visible())
            .map(|change| format!("undo · {}", change.was));
        let room = a.width.saturating_sub(used + 6);
        let kept = undo
            .as_deref()
            .map_or(0, |undo| chrome::width(undo) + 5)
            .min(room.saturating_sub(12));
        let text = format!("── {} ", clip(notice, (room - kept) as usize));
        let w = chrome::width(&text).min(a.width - used);
        row(f, Rect::new(a.x + used, a.y, w, 1), &text, Tone::Accent, t);
        used += w;
        if let Some(undo) = undo {
            let w = chrome::chip(
                f,
                g,
                a.x + used,
                a.y,
                a.right().saturating_sub(2),
                &undo,
                Action::Undo,
                false,
                Tone::Normal,
                ui.press,
                t,
            );
            used += w + 1;
        }
    }
    if used < a.width {
        let fill = a.width - used - u16::from(boxed);
        chrome::rule(
            f,
            Rect::new(a.x + used, a.y, fill, 1),
            &gutter.map(|x| (x, "┴")).into_iter().collect::<Vec<_>>(),
            if running { Tone::Accent } else { Tone::Line },
            t,
        );
        if boxed {
            row(f, Rect::new(a.right() - 1, a.y, 1, 1), "╮", Tone::Line, t);
        }
        // What stands at the far end, right to left.
        let mut right = a.right().saturating_sub(2);
        // Rows of the draft above the composer's window say so here, on
        // its top edge.
        if ui.composer_hidden.0 > 0 {
            let above = format!(" ↑ {} more ", ui.composer_hidden.0);
            let w = chrome::width(&above);
            if used + w + 4 < a.width {
                right = right.saturating_sub(w);
                row(f, Rect::new(right, a.y, w, 1), &above, Tone::Muted, t);
            }
        }
        // The acceptance list's one line, where the sidebar that carries it
        // is not on screen: beside the turn's status, one click from the list.
        if !acceptance.is_empty() {
            let label = crate::acceptance::glance(acceptance);
            let w = chrome::width(&label) + 4;
            if right > a.x + used + w + 2 {
                chrome::chip(
                    f,
                    g,
                    right - w - 1,
                    a.y,
                    right,
                    &label,
                    Action::Acceptance,
                    false,
                    acceptance_tone(acceptance, Tone::Normal),
                    ui.press,
                    t,
                );
            }
        }
    }
}
/// The acceptance list's one line in the failure colour once a check has
/// found an item not met, and in `calm` until then.
fn acceptance_tone(acceptance: &[crate::acceptance::Verdict], calm: Tone) -> Tone {
    if crate::acceptance::failed(acceptance) > 0 {
        Tone::Failure
    } else {
        calm
    }
}
/// The dock's bottom edge: the three everyday chips, one whispered hint,
/// and the context reading at the far end.
fn dock_bottom(
    f: &mut Frame<'_>,
    g: &mut Geometry,
    a: Rect,
    n: &Notebook,
    s: &ScreenState,
    ui: &Workbench,
) {
    let t = s.theme;
    chrome::rule(f, a, &[], Tone::Line, t);
    row(f, Rect::new(a.x, a.y, 2, 1), "╰─", Tone::Line, t);
    row(f, Rect::new(a.right() - 1, a.y, 1, 1), "╯", Tone::Line, t);
    let context = n.context.map(|tokens| {
        crate::tui::status::context_summary(
            tokens,
            if a.width >= 160 { 12 } else { 7 },
            s.animation_frame,
            matches!(s.activity, Activity::Thinking | Activity::Streaming),
        )
    });
    // Rows of the draft below the composer's window say so here, on its
    // bottom edge.
    let below = match ui.composer_hidden.1 {
        0 => String::new(),
        rows => format!("↓ {rows} more"),
    };
    let right = match (context, below.is_empty()) {
        (Some(c), true) => format!(" {c} "),
        (Some(c), false) => format!(" {below} · {c} "),
        (None, false) => format!(" {below} "),
        (None, true) => String::new(),
    };
    let rw = chrome::width(&right);
    if rw > 0 && rw + 4 < a.width {
        row(
            f,
            Rect::new(a.right().saturating_sub(rw + 2), a.y, rw, 1),
            &right,
            Tone::Muted,
            t,
        );
    }
    let limit = a.right().saturating_sub(rw + 3);
    let mut x = a.x + 3;
    if s.status_line != StatusLine::Hidden {
        // **A chip says only what differs from Sterna's own default** --
        // except the effort, which is a control stepped in place: a chip
        // that vanished at `default` could not be stepped again.
        let items = [
            // A sign-in running beside the session, one click from its panel.
            (
                s.signing_in
                    .as_ref()
                    .map(|label| format!("signing in to {label} ▸"))
                    .unwrap_or_default(),
                Action::ReopenSignIn,
            ),
            (super::facts::effort_word(s), Action::Effort),
            (
                if s.helpers_on {
                    "◇ helpers on".to_string()
                } else {
                    String::new()
                },
                Action::SettingsAt(2),
            ),
            (
                s.subagents
                    .as_deref()
                    .filter(|word| *word != "off")
                    .map(|word| format!("subagents {word}"))
                    .unwrap_or_default(),
                Action::Command("/subagents".into()),
            ),
            (
                if s.stream == crate::tui::Stream::default() {
                    String::new()
                } else {
                    format!("stream {}", s.stream.name())
                },
                Action::Stream,
            ),
        ];
        // A fact nobody measured is not a control; an empty label is how
        // this strip says "no" rather than saying `unknown`.
        let chips: Vec<(String, Action, bool)> = items
            .into_iter()
            .filter(|(text, _)| !text.is_empty())
            .map(|(text, action)| (text, action, false))
            .collect();
        // The chips sit on a cleared stretch of the edge, one space apart,
        // rather than on top of the rule; with no room for one the rule
        // stays whole.
        let end = chrome::chips_end(&chips, x, limit);
        if end > x {
            row(
                f,
                Rect::new(x - 1, a.y, end - x + 1, 1),
                &" ".repeat((end - x) as usize + 1),
                Tone::Line,
                t,
            );
        }
        x = chrome::chips(f, g, x, a.y, limit, &chips, ui.press, t);
    }
    // One muted line that teaches, instead of one that counts. It turns with
    // the session rather than with the clock, so it is stable inside one
    // screenshot and inside one test.
    if s.status_line == StatusLine::Full {
        let changed = n
            .cells
            .iter()
            .any(|c| c.changes.as_deref().is_some_and(|d| !d.is_empty()));
        let hint = format!(
            " {} ",
            voice::hint(
                n.cells.len() + s.history.len(),
                s.activity.working(),
                changed
            )
        );
        let hw = chrome::width(&hint);
        if x + hw + 2 < limit {
            row(
                f,
                Rect::new(limit.saturating_sub(hw + 1), a.y, hw, 1),
                &hint,
                Tone::Muted,
                t,
            );
        }
    }
}
/// A local surface: modal and framed, drawn by the one sheet component --
/// the same head, the same foot and the same keys every time.
fn surface(f: &mut Frame<'_>, g: &mut Geometry, a: Rect, s: &ScreenState, ui: &mut Workbench) {
    let t = s.theme;
    let area = if a.width >= 100 && a.height >= 25 {
        Rect::new(a.x + 2, a.y + 1, a.width - 4, a.height - 2)
    } else {
        a
    };
    // A local surface is modal: the conversation behind it is not
    // half-visible around its edges, which would read as damage.
    f.render_widget(Clear, a);
    f.buffer_mut().set_style(a, theme::style(Tone::Normal, t));
    g.hits.clear();
    g.local = Some(area);
    let framed = area.width >= 12 && area.height >= 6;
    if framed {
        chrome::frame(f, area, Tone::Line, t);
    }
    let inner = if framed {
        Rect::new(area.x + 2, area.y + 1, area.width - 4, area.height - 2)
    } else {
        Rect::new(
            area.x + u16::from(area.width > 1),
            area.y,
            area.width.saturating_sub(2),
            area.height,
        )
    };
    let (press, hover) = (ui.press, ui.hover);
    let Some(layer) = ui.sheets.last_mut() else {
        return;
    };
    let drawn = super::sheet::draw(f, g, inner, &mut layer.sheet, t, press, hover);
    if let (super::Source::Themes { .. }, Some(aside)) = (&layer.source, drawn.aside) {
        let chosen = layer
            .sheet
            .focused()
            .and_then(|item| item.id.strip_prefix("theme:"))
            .and_then(Theme::parse)
            .unwrap_or(s.theme);
        super::sheets::draw_theme_preview(f, aside, chosen, s, t);
    }
}

/// Text laid into lines no wider than `width`, **keeping the spaces
/// between words**: a column laid out with spaces stays a column, and
/// indentation stays. A break drops the spaces it falls on; a word wider
/// than a line is split.
pub(super) fn wrap_spaced(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut runs: Vec<(String, bool)> = Vec::new();
    for c in text.chars() {
        let c = if c == '\t' { ' ' } else { c };
        let space = c == ' ';
        match runs.last_mut() {
            Some((run, was)) if *was == space => run.push(c),
            _ => runs.push((c.to_string(), space)),
        }
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for (run, space) in runs {
        let size = run.chars().count();
        if space {
            if used + size <= width {
                line.push_str(&run);
                used += size;
            } else {
                lines.push(std::mem::take(&mut line).trim_end().to_string());
                used = 0;
            }
            continue;
        }
        if used > 0 && used + size > width {
            lines.push(std::mem::take(&mut line).trim_end().to_string());
            used = 0;
        }
        let mut rest: Vec<char> = run.chars().collect();
        while used + rest.len() > width {
            let take = width - used;
            line.extend(rest.drain(..take));
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        used += rest.len();
        line.extend(rest);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

/// Code laid into lines no wider than `width`: every character kept, the
/// line cut where it reaches the edge.
pub(super) fn wrap_exact(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.replace('\t', "    ").chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Words laid into lines no wider than `width`.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The composer's width: the text width less the dock's edges and the
/// prompt mark. Computed here once, for every composer computation.
fn composer_width(a: Rect, s: &ScreenState) -> u16 {
    let textwidth = a.width.saturating_sub(2).max(1);
    let boxed = !s.fullscreen && a.width > 8;
    let prompt = if boxed { 4 } else { u16::from(a.width > 6) * 3 };
    textwidth
        .saturating_sub(prompt.saturating_sub(1) + u16::from(boxed) * 2)
        .max(1)
}

/// The composer's rows: the draft cut at its newlines and wherever a line
/// reaches `width` columns, after a space where one fits. **Every
/// character is kept** -- the rows joined, with the newlines between them,
/// are the draft -- and each row carries the byte it starts at.
pub(super) fn composer_lines(text: &str, width: usize) -> Vec<(usize, &str)> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        let (mut at, mut rest) = (start, line);
        loop {
            let (mut used, mut cut, mut space) = (0, rest.len(), None);
            for (i, c) in rest.char_indices() {
                let w = Span::raw(c.to_string()).width();
                if used + w > width {
                    cut = i;
                    break;
                }
                used += w;
                if c == ' ' {
                    space = Some(i + 1);
                }
            }
            if cut == rest.len() {
                rows.push((at, rest));
                break;
            }
            // A row ends after its last space, or where it is full; it
            // always takes at least one character, so a character wider
            // than the composer still moves on.
            let cut = space
                .filter(|space| *space <= cut)
                .unwrap_or(cut)
                .max(rest.chars().next().map_or(1, char::len_utf8));
            rows.push((at, &rest[..cut]));
            at += cut;
            rest = &rest[cut..];
        }
        start += line.len() + 1;
    }
    rows
}

/// The row and column the caret is drawn at: on the last row that starts
/// at or before it, so a caret at a wrap is at the start of the next row.
pub(super) fn caret(lines: &[(usize, &str)], text: &str, cursor: usize) -> (usize, usize) {
    let row = lines
        .iter()
        .rposition(|(start, _)| *start <= cursor)
        .unwrap_or(0);
    let start = lines.get(row).map_or(0, |(start, _)| *start);
    let col = Span::raw(&text[start.min(cursor)..cursor]).width();
    (row, col)
}
