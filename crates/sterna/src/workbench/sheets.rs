//! What each surface's sheet holds, built from that surface's own data on
//! every frame, and the few things drawn beside a sheet rather than in it:
//! the theme preview and the key form.
use super::sheet::{Item, Kind, Sheet};
use super::view::{label, row, wrap_words};
use super::{Action, Layer, Source, Tone, Workbench, chrome, document::clip};
use crate::permissions::Level;
use crate::tui::{Notebook, ScreenState, Theme};
use ratatui::{Frame, layout::Rect, widgets::Clear};

/// Rebuilds the top sheet's rows from its source and the session's state.
/// Focus, scroll and the search are the sheet's own and survive.
pub(super) fn build(ui: &mut Workbench, s: &ScreenState, n: &Notebook) {
    let depth = ui.sheets.len();
    if depth == 0 {
        return;
    }
    // A fold lists the values of a row on the layer under it.
    let fold = match &ui.sheets[depth - 1].source {
        Source::Fold(id) if depth > 1 => ui.sheets[depth - 2]
            .sheet
            .items
            .iter()
            .find(|item| item.id == *id)
            .cloned(),
        _ => None,
    };
    let turning_off = ui.turning_off.clone();
    // The undo chip rides beside the notice the newest change produced.
    let undo = (ui.offer_undo && !ui.changes.is_empty()).then_some(Action::Undo);
    let Layer { sheet, source, .. } = &mut ui.sheets[depth - 1];
    sheet.root = depth == 1;
    // A confirm, and a panel whose leaving is itself an answer (the rollback
    // preview), are decisions: armed before a key answers them.
    sheet.decision = match source {
        Source::Confirm(_) => true,
        Source::Panel(panel) => panel.back.is_some(),
        _ => false,
    };
    sheet.undo = undo;
    sheet.aside = 0;
    sheet.tools.clear();
    sheet.total = None;
    sheet.matched = None;
    let items = match source {
        Source::Sandbox => sandbox(sheet, s),
        Source::Hosts(h) => super::hosts::items(sheet, h, s),
        Source::Confirm(what) => confirm(sheet, what),
        Source::Keys => keys(sheet),
        Source::Activity => activity(sheet, s),
        Source::Themes { .. } => themes(sheet, s),
        Source::Settings(p) => super::settings::items(sheet, p, s),
        Source::Models(m) => super::models::items(sheet, m),
        Source::Panel(panel) => panel_items(sheet, panel, &turning_off),
        Source::Fold(_) => fold_items(sheet, fold),
        Source::More(controls) => more_items(sheet, controls),
    };
    let _ = n;
    // While a turn runs, a row that waits for it to end says so before it
    // is clicked, in the one sentence every such refusal uses.
    let items = if s.activity.working() {
        items.into_iter().map(between_turns).collect()
    } else {
        items
    };
    sheet.set_items(items);
}

/// Disables a row that cannot act until the turn ends: a command other than
/// a model or effort, and the model picker, which the session builds.
fn between_turns(item: Item) -> Item {
    let waits = match &item.action {
        Some(Action::Command(command)) => !super::mid_turn(command),
        Some(Action::Models) => true,
        _ => false,
    };
    if waits && item.disabled.is_none() {
        item.disabled(Some(super::voice::BETWEEN_TURNS.into()))
    } else {
        item
    }
}

/// The sandbox: its level, how it is enforced, and what was answered for
/// the session, on one sheet the level chip opens.
fn sandbox(sheet: &mut Sheet, s: &ScreenState) -> Vec<Item> {
    sheet.title = "Sandbox".into();
    sheet.crumbs = vec!["how much runs without asking".into()];
    let now = s.level.level();
    let mut items: Vec<Item> = Level::ALL
        .into_iter()
        .map(|level| {
            let id = format!("level:{}", level.name());
            let action = Action::Level(level.name().into());
            let item = if level == Level::Full && now != Level::Full {
                Item::danger(id, level.label(), action)
            } else {
                Item::choice(id, level.label(), level == now, action)
            };
            item.detail(level.sentence())
        })
        .collect();
    items.push(
        Item::info("Saved for every project. A project's own settings cannot change this.")
            .tone(Tone::Muted),
    );
    let unknown = |v: &Option<String>| v.clone().unwrap_or_else(|| "unknown".into());
    items.push(Item::heading("How it is enforced"));
    items.push(
        Item::info(format!("Child processes   {}", unknown(&s.confinement))).tone(Tone::Muted),
    );
    items.push(Item::info(format!("Pre-approved      {}", unknown(&s.sandbox))).tone(Tone::Muted));
    items.push(Item::info(format!("Host tools        {}", unknown(&s.network))).tone(Tone::Muted));
    items.push(
        Item::open("sandbox:hosts", "Allowed hosts", Action::Hosts)
            .detail("the registries and hosts commands may reach"),
    );
    // What was answered for the whole session is on this sheet too, where it
    // can be taken back: a refusal that stays must stay visibly.
    let remembered = s
        .memory
        .as_ref()
        .map(crate::approval::Memory::entries)
        .unwrap_or_default();
    for (allowed, heading) in [
        (true, "Allowed for this session"),
        (false, "Denied for this session"),
    ] {
        let rows: Vec<_> = remembered.iter().filter(|r| r.allowed == allowed).collect();
        if rows.is_empty() {
            continue;
        }
        items.push(Item::heading(heading));
        for row in rows {
            items.push(
                Item::run(
                    format!("forget:{}", row.id),
                    format!("Forget · {}", row.label),
                    Action::Forget(row.id.clone()),
                )
                .detail(if allowed {
                    "runs without asking until you forget it"
                } else {
                    "refused without asking until you forget it"
                }),
            );
        }
    }
    items.push(Item::open(
        "sandbox:settings",
        "Open settings",
        Action::Settings,
    ));
    items
}

/// The one confirmation sheet, for Full access and the other changes that
/// lift a boundary. It opens on Cancel.
fn confirm(sheet: &mut Sheet, what: &str) -> Vec<Item> {
    let (label, warning, yes) = if let Some(link) = what.strip_prefix("open:") {
        (
            "Open the page in your browser".to_string(),
            "This opens your default browser on this computer.",
            Action::OpenLink(link.to_string()),
        )
    } else if let Some(model) = what.strip_prefix("pin:") {
        (
            format!("Pin {model}"),
            "This turns favourites off: every subagent runs on this one model.",
            Action::Choose(model.to_string()),
        )
    } else {
        (
            Level::parse(what).map_or_else(|| what.to_string(), |l| l.label().to_string()),
            "Sterna will run without a sandbox, in every project. Anything it runs can change \
             any file your user can, and reach the network. Nothing is asked. Refused commands \
             stay refused. It applies from the next request.",
            Action::ConfirmLevel(what.to_string()),
        )
    };
    sheet.title = "Confirm".into();
    sheet.crumbs = vec![label.clone()];
    vec![
        Item::info(warning).tone(Tone::Warning),
        Item::info("Nothing is confirmed until you choose it below; Esc goes back unchanged."),
        Item::run("confirm:cancel", "Cancel", Action::Close).inline(),
        Item::danger("confirm:yes", format!("Yes · {label}"), yes).inline(),
    ]
}

/// Every key, and what it does: the one table the keys sheet is drawn from
/// and the tests read. A row with an action is one click from doing it.
pub fn keymap() -> Vec<(&'static str, &'static str, Option<Action>)> {
    vec![
        (
            "Enter",
            "send · Shift-Enter, Alt-Enter or Ctrl-J for a new line",
            None,
        ),
        (
            "Ctrl-Z",
            "undo an edit to the draft · Ctrl-Shift-Z redoes it",
            None,
        ),
        (
            "Ctrl-K Ctrl-U",
            "cut to the line's end or start · Ctrl-Y puts it back",
            None,
        ),
        (
            "Ctrl-W",
            "delete the word before the caret · Alt-B Alt-F move by word",
            None,
        ),
        ("@", "complete a path in this project", None),
        (
            "Esc",
            "take back a queued message · else stop after this cell",
            None,
        ),
        (
            "Ctrl-C",
            "copy a selection · else stop a turn · else clear the draft · twice quits",
            None,
        ),
        (
            "F2",
            "settings · choices save themselves; most apply now",
            Some(Action::Settings),
        ),
        ("F3", "which model answers", Some(Action::Models)),
        ("F4", "the selected cell's diff", None),
        ("F5", "the selected cell's helpers", None),
        ("Ctrl-O", "expand or collapse the selected cell", None),
        ("Alt-↑ ↓", "select the previous or next cell", None),
        (
            "Ctrl-T",
            "telemetry: the live view of requests",
            Some(Action::Telemetry),
        ),
        ("Ctrl-B", "show or hide the sidebar", None),
        ("Ctrl-F", "hide or restore the chrome", None),
        (
            "Ctrl-G",
            "release the mouse to the terminal, and take it back",
            None,
        ),
        (
            "PgUp PgDn",
            "scroll the conversation · Ctrl-Home/End to either end",
            None,
        ),
        (
            "↑ ↓",
            "the line above or below · history from the first or last line",
            None,
        ),
        ("Ctrl-A E", "start or end of the line", None),
        (
            "Ctrl-K U",
            "delete to the end or the start of the line",
            None,
        ),
        ("/", "commands · /help lists every one", None),
        ("?", "this sheet, when the composer is empty", None),
        ("click", "any chip changes the thing it names", None),
    ]
}

fn keys(sheet: &mut Sheet) -> Vec<Item> {
    sheet.title = "Keys".into();
    sheet.crumbs = vec!["every key, and what it does".into()];
    keymap()
        .into_iter()
        .map(|(key, what, action)| {
            let text = format!("{key:<11}{what}");
            match action {
                Some(action) => Item::open(format!("key:{key}"), text, action),
                None => Item::info(text),
            }
        })
        .collect()
}

fn activity(sheet: &mut Sheet, s: &ScreenState) -> Vec<Item> {
    sheet.title = "Activity".into();
    sheet.crumbs = vec!["local notices, newest last".into()];
    let lines: Vec<Item> = s
        .history
        .iter()
        .flat_map(|note| note.text.lines())
        .map(|line| {
            Item::info(line).tone(if line.starts_with("ERROR:") {
                Tone::Failure
            } else {
                Tone::Normal
            })
        })
        .collect();
    if lines.is_empty() {
        vec![Item::info("Nothing has happened yet.").tone(Tone::Muted)]
    } else {
        lines
    }
}

fn themes(sheet: &mut Sheet, s: &ScreenState) -> Vec<Item> {
    sheet.title = "Themes".into();
    sheet.crumbs.clear();
    sheet.aside = 34;
    let mut items = Vec::new();
    let mut family = None;
    for theme in Theme::by_family() {
        if family != Some(theme.family()) {
            family = Some(theme.family());
            items.push(Item::heading(format!(
                "{} · {}",
                theme.family().label(),
                theme.family().blurb()
            )));
        }
        let mut item = Item::choice(
            format!("theme:{}", theme.name()),
            theme.title(),
            s.theme == theme,
            Action::Theme(theme),
        );
        // The swatch and the name are one target.
        if let Some(rgb) = super::theme::accent_value(theme) {
            item = item.swatch(rgb);
        }
        items.push(item);
    }
    items
}

fn fold_items(sheet: &mut Sheet, parent: Option<Item>) -> Vec<Item> {
    let Some(parent) = parent else {
        return vec![Item::info("That list is no longer open.")];
    };
    sheet.title = parent.title.clone();
    sheet.crumbs = vec!["every value".into()];
    let Kind::Value { values, current } = parent.kind else {
        return Vec::new();
    };
    values
        .into_iter()
        .enumerate()
        .map(|(i, (label, action))| {
            Item::choice(format!("value:{label}"), label, Some(i) == current, action)
        })
        .collect()
}

/// The controls that had no room in their row, each one a row that does
/// what the chip would have done.
fn more_items(sheet: &mut Sheet, controls: &[(String, Action)]) -> Vec<Item> {
    sheet.title = "More".into();
    controls
        .iter()
        .enumerate()
        .map(|(i, (label, action))| Item::open(format!("more:{i}"), label.clone(), action.clone()))
        .collect()
}

/// The rows of a panel the session sent, and a search once it is longer
/// than a screen is comfortable with.
fn panel_items(
    sheet: &mut Sheet,
    panel: &crate::tui::Panel,
    turning_off: &std::collections::BTreeSet<String>,
) -> Vec<Item> {
    let mut parts = panel.title.split(" › ");
    sheet.title = parts.next().unwrap_or_default().to_string();
    sheet.crumbs = parts.map(str::to_string).collect();
    if panel.rows.len() > 12 && sheet.query.is_none() {
        sheet.query = Some(String::new());
    }
    let query = sheet
        .query
        .as_deref()
        .map(str::to_lowercase)
        .unwrap_or_default();
    let mut items = Vec::new();
    let mut focus = None;
    for (i, row) in panel.rows.iter().enumerate() {
        if !query.is_empty() && !row.text.to_lowercase().contains(&query) {
            continue;
        }
        let id = row
            .id
            .clone()
            .unwrap_or_else(|| format!("row:{}", row.text));
        let (title, detail) = match row.text.split_once(" · ") {
            Some((head, rest)) if row.acts() => (head.to_string(), rest.to_string()),
            _ => (row.text.clone(), String::new()),
        };
        let mut item = match &row.kind {
            Kind::Info => Item::info(row.text.clone()),
            Kind::Heading => Item::heading(row.text.clone()),
            kind => {
                let mut item = Item::info(title);
                item.kind = kind.clone();
                item.id = id;
                item.action = row.action.clone();
                item.detail(detail)
            }
        };
        if let Some(Action::HandlerOff(name)) = &row.action
            && turning_off.contains(name)
        {
            item = item.disabled(Some("turning off…".into()));
        }
        if i == panel.selected && row.acts() {
            focus = Some(item.id.clone());
        }
        items.push(item);
    }
    if items.iter().all(|item| !item.focusable()) && !query.is_empty() {
        items.push(Item::info("Nothing matches. Backspace removes a letter.").tone(Tone::Muted));
    }
    if let Some(total) = (!query.is_empty()).then(|| panel.rows.iter().filter(|r| r.acts()).count())
    {
        sheet.total = Some(total);
    }
    sheet.prefer = focus;
    items
}

/// The chosen theme beside the list, as the screen will wear it: a bird
/// theme's bird, its name and its three colours.
pub(super) fn draw_theme_preview(
    f: &mut Frame<'_>,
    area: Rect,
    chosen: Theme,
    s: &ScreenState,
    t: Theme,
) {
    use super::plumage::{Mood, sprite};
    let Theme::Bird(bird) = chosen else {
        draw_palette_preview(f, area, chosen, t);
        return;
    };
    let plumage = bird.plumage();
    let mut y = area.y;
    let drawing = sprite(bird, Mood::Done, s.light);
    if s.truecolor
        && area.width as usize >= drawing[0].len()
        && area.height as usize >= drawing.len() + 5
    {
        for cells in drawing {
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
            "This terminal shows no true colour, so the bird is not drawn.",
            Tone::Muted,
            t,
        );
    }
}

/// A palette alone, as the screen will wear it: a heading, a chosen chip
/// beside one that is not, and the roles' own colours.
fn draw_palette_preview(f: &mut Frame<'_>, area: Rect, chosen: Theme, t: Theme) {
    label(f, area, area.y, chosen.title(), Tone::Strong, t);
    label(
        f,
        area,
        area.y + 1,
        "no bird · the palette alone",
        Tone::Muted,
        t,
    );
    label(f, area, area.y + 3, "A HEADING", Tone::Accent, chosen);
    let on = "⟨ chosen ⟩";
    let w = chrome::width(on);
    if area.height > 4 && area.width > w {
        f.render_widget(
            ratatui::widgets::Paragraph::new(on).style(super::theme::chip_on(chosen)),
            Rect::new(area.x, area.y + 4, w, 1),
        );
        row(
            f,
            Rect::new(area.x + w + 1, area.y + 4, area.width - w - 1, 1),
            "⟨ another ⟩",
            Tone::Normal,
            chosen,
        );
    }
    let mut x = area.x;
    for (word, tone) in [
        ("you", Tone::You),
        ("helper", Tone::Helper),
        ("detail", Tone::Muted),
    ] {
        let w = chrome::width(word);
        if x + w > area.right() {
            break;
        }
        row(f, Rect::new(x, area.y + 6, w, 1), word, tone, chosen);
        x += w + 2;
    }
}

/// Credential display is mask-only; no plaintext reaches a render buffer.
/// A form sheet over the whole screen: its title and step, a sentence of
/// context, each field under its label with the one in focus outlined and
/// its caret showing where a paste lands, the check or the error under it,
/// and the keys that act on the sheet.
/// Where a click lands on a form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormHit {
    /// The field at this index: it takes the focus.
    Field(usize),
    /// One word of a choice field.
    Word(usize, usize),
    /// The submit chip: what Enter does.
    Submit,
    /// The header chip: what Esc does.
    Back,
}

pub fn render_form(
    f: &mut Frame<'_>,
    form: &crate::tui::Form,
    theme: Theme,
) -> Vec<(Rect, FormHit)> {
    let mut hits = Vec::new();
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
    let back = "⟨ Esc · Back ⟩";
    let back_w = chrome::width(back);
    let back_r = Rect::new(inner.right().saturating_sub(back_w), inner.y, back_w, 1);
    row(f, back_r, back, Tone::Accent, theme);
    hits.push((back_r, FormHit::Back));
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
        // The label, the value and the line under it are one target.
        hits.push((Rect::new(inner.x, y, inner.width, 3), FormHit::Field(index)));
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
                    hits.push((Rect::new(x, y, w + 1, 1), FormHit::Word(index, i)));
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
            (_, Some(Ok(crate::tui::form::Verdict::Fine(praise)))) => {
                Some((format!("✓ {praise}"), Tone::Success))
            }
            (_, Some(Ok(crate::tui::form::Verdict::Warning(doubt)))) => {
                Some((format!("! {doubt}"), Tone::Warning))
            }
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
    let submit = format!("⟨ {} ⟩", form.submit);
    let submit_w = chrome::width(&submit);
    let foot = inner.bottom().saturating_sub(1);
    let submit_r = Rect::new(inner.x, foot, submit_w.min(inner.width), 1);
    row(f, submit_r, &submit, Tone::Accent, theme);
    hits.push((submit_r, FormHit::Submit));
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
    let x = inner.x + submit_w + 2;
    row(
        f,
        Rect::new(x, foot, inner.right().saturating_sub(x), 1),
        &keys,
        Tone::Muted,
        theme,
    );
    hits
}
