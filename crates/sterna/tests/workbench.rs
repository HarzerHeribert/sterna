// Building a `Workbench` or a `ScreenState` from its default and then
// setting the two fields a case is about is how every test here reads; the
// struct-literal form the lint prefers hides which field the case turns on.
#![allow(clippy::field_reassign_with_default)]
//! Acceptance of the new live renderer, input reducer and native settings store.
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use sterna::{
    contract::{Block, Conversation, Message, Role, ServedBy},
    helpers::{HelperOutcome, HelperRecord, HelperUsage},
    tui::{
        Activity, CellError, CellView, ModelGroup, Notebook, Panel, ScreenState, Theme, TierModels,
    },
    workbench::{
        self, Action, CellTab, Document, Effect, Navigator, Preferences, Source, Tone, Workbench,
        sheet::Hit,
    },
};
fn fixture() -> (Conversation, Notebook, ScreenState) {
    let mut m = Message::text(
        Role::Assistant,
        "I will inspect the motion guard before changing it.",
    );
    m.content.push(Block::ToolUse{id:"call-1".into(),name:"execute_cell".into(),input:serde_json::json!({"code":"const result = await checks.run(\"tests\");\nprint(result);"})});
    (
        Conversation {
            system: String::new(),
            messages: vec![
                Message::text(
                    Role::User,
                    "Respect reduced motion and the terminal background.",
                ),
                m,
            ],
        },
        Notebook {
            cells: vec![CellView {
                executed_source: Some(
                    "const result = await checks.run(\"tests\");\nprint(result);".into(),
                ),
                execution: Some("checks.run · completed".into()),
                output: Some("99 / 99 tests passed".into()),
                stdout: Some("Compiling dependency graph".into()),
                changes: Some("--- a/view.rs\n+++ b/view.rs\n@@ -1 +1 @@\n-old();\n+new();".into()),
                helpers: vec![HelperRecord {
                    helper: "reduce".into(),
                    verb: "reducing".into(),
                    asked: "Retain failures and source locations".into(),
                    outcome: HelperOutcome {
                        text: "No failing tests in the supplied output".into(),
                        ok: true,
                        elapsed_ms: 1200,
                        ..Default::default()
                    },
                    usage: HelperUsage {
                        model: "fixture-helper".into(),
                        ..Default::default()
                    },
                    looked: vec!["prepare failure windows".into()],
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        },
        ScreenState {
            model: Some("fixture-main".into()),
            project: Some("test-project".into()),
            activity: Activity::Complete,
            ..Default::default()
        },
    )
}
fn draw(
    c: &Conversation,
    n: &Notebook,
    s: &ScreenState,
    u: &mut Workbench,
    w: u16,
    h: u16,
) -> Buffer {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| workbench::render(f, c, n, s, &ServedBy::default(), u))
        .unwrap();
    t.backend().buffer().clone()
}
fn text(b: &Buffer) -> String {
    (0..b.area.height)
        .map(|y| {
            (0..b.area.width)
                .map(|x| b[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn key(u: &mut Workbench, s: &mut ScreenState, n: &Notebook, k: KeyCode) -> Effect {
    u.event(
        &Event::Key(KeyEvent::new(k, KeyModifiers::NONE)),
        s,
        n,
        false,
    )
}
/// Ctrl and a letter, the way a sheet reads it.
fn ctrl(u: &mut Workbench, s: &mut ScreenState, n: &Notebook, c: char) -> Effect {
    u.event(
        &Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
        s,
        n,
        false,
    )
}
fn mouse(
    u: &mut Workbench,
    s: &mut ScreenState,
    n: &Notebook,
    kind: MouseEventKind,
    x: u16,
    y: u16,
) -> Effect {
    u.event(
        &Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }),
        s,
        n,
        false,
    )
}
fn click(u: &mut Workbench, s: &mut ScreenState, n: &Notebook, a: Action) -> Effect {
    let (r, _) = u
        .geometry
        .hits
        .iter()
        .find(|(_, v)| *v == a)
        .unwrap()
        .clone();
    mouse(u, s, n, MouseEventKind::Down(MouseButton::Left), r.x, r.y);
    mouse(u, s, n, MouseEventKind::Up(MouseButton::Left), r.x, r.y)
}
/// Clicks the top sheet's row with this id.
fn click_item(u: &mut Workbench, s: &mut ScreenState, n: &Notebook, id: &str) -> Effect {
    let index = u
        .top()
        .and_then(|layer| layer.sheet.items.iter().position(|item| item.id == id))
        .unwrap_or_else(|| panic!("no row {id}"));
    click(u, s, n, Action::Sheet(Hit::Item(index)))
}
/// The top sheet has been on screen, quietly, long enough to take a key:
/// what a person waiting half a second before answering a decision gets.
fn armed(u: &mut Workbench) {
    if let Some(layer) = u.top_mut() {
        layer.sheet.shown =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(1));
    }
}
fn doc(c: &Conversation, n: &Notebook, s: &ScreenState, u: &Workbench) -> Document {
    Document::build(c, n, s, u, 100)
}
fn words(d: &Document) -> String {
    d.rows
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "sterna-workbench-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn prefs() -> (Temp, ScreenState, Preferences) {
    let t = Temp::new();
    let s = ScreenState {
        settings_root: Some(t.0.clone()),
        settings_global: Some(t.0.join("user")),
        ..Default::default()
    };
    let p = Preferences::with_global(&s, Some(t.0.join("user"))).unwrap();
    (t, s, p)
}
fn navigator() -> Navigator {
    let group = |provider: &str, available: bool, models: Vec<&str>| ModelGroup {
        provider: provider.into(),
        account: format!("{provider}-subscription"),
        scope: "subscription".into(),
        models: models.into_iter().map(str::to_string).collect(),
        selectable: Some(available),
        unavailable_reason: (!available).then(|| "No credential configured".into()),
        connect: None,
        pooled: None,
        note: None,
    };
    let panel = Panel::models(
        "Models",
        vec![
            group("A", true, vec!["fixture-main", "fixture-helper"]),
            group("OpenRouter", false, vec!["unavailable-model"]),
        ],
        TierModels {
            parent: "fixture-main".into(),
            helper: Some("fixture-helper".into()),
            subagent: None,
        },
    );
    Navigator::from_panel(&panel).unwrap()
}
#[test]
fn code_public_explanation_and_results_remain_readable() {
    let (c, n, s) = fixture();
    let d = doc(&c, &n, &s, &Workbench::default());
    for t in [
        "const result",
        "inspect the motion guard",
        "99 / 99",
        "No failing tests",
    ] {
        assert!(words(&d).contains(t));
    }
    assert!(d.rows.iter().any(|r| {
        r.text.contains("99 / 99")
            && r.spans
                .iter()
                .any(|(t, tone)| t.contains("99 / 99") && *tone == Tone::Normal)
    }));
}
#[test]
fn collapsed_cell_keeps_helper_contributions() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.collapsed.insert(1);
    let t = words(&doc(&c, &n, &s, &u));
    assert!(!t.contains("const result"));
    assert!(t.contains("No failing tests"));
}
#[test]
fn local_notices_are_not_model_conversation() {
    let (c, n, mut s) = fixture();
    s.note("SETTINGS INTERNAL MESSAGE");
    let mut u = Workbench::default();
    // A notice is kept where it happened, so it can be scrolled back to --
    // and it is marked and muted, never drawn as something the model said.
    let d = doc(&c, &n, &s, &u);
    let row = d
        .rows
        .iter()
        .find(|r| r.text.contains("SETTINGS INTERNAL"))
        .expect("the notice is in the local document");
    assert!(row.text.trim_start().starts_with('·'), "{:?}", row.text);
    assert!(
        row.spans
            .iter()
            .any(|(t, tone)| t.contains("SETTINGS INTERNAL") && *tone == Tone::Muted),
        "{:?}",
        row.spans
    );
    assert!(text(&draw(&c, &n, &s, &mut u, 100, 40)).contains("SETTINGS INTERNAL"));
    u.open(Source::Activity);
    assert!(text(&draw(&c, &n, &s, &mut u, 100, 40)).contains("SETTINGS INTERNAL"));
}
#[test]
fn final_return_visible_without_an_extra_message_and_not_duplicated() {
    let (mut c, mut n, s) = fixture();
    n.cells[0].returned = Some("The motion guard is fixed.".into());
    assert!(words(&doc(&c, &n, &s, &Workbench::default())).contains("The motion guard is fixed."));
    c.messages
        .push(Message::text(Role::Assistant, "The motion guard is fixed."));
    assert_eq!(
        words(&doc(&c, &n, &s, &Workbench::default()))
            .matches("The motion guard is fixed.")
            .count(),
        1
    );
}
#[test]
fn runtime_feedback_not_confused_with_the_next_human_message() {
    let (mut c, mut n, s) = fixture();
    n.cells[0].answered = true;
    c.messages.push(Message::tool_result(
        "call-1",
        "PRIVATE PROTOCOL NOISE",
        false,
    ));
    c.messages
        .push(Message::text(Role::User, "Now inspect the diff."));
    let t = words(&doc(&c, &n, &s, &Workbench::default()));
    assert!(!t.contains("PRIVATE PROTOCOL"));
    assert!(t.contains("Now inspect the diff."));
}
#[test]
fn diff_has_an_observed_baseline_and_semantic_colors() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.tabs.insert(1, CellTab::Diff);
    let d = doc(&c, &n, &s, &u);
    assert!(words(&d).contains("already applied"));
    // The added and removed lines keep their side's colour, and each one
    // now carries both line numbers, so the row is read by its parts.
    assert!(d.rows.iter().any(|r| {
        r.text.contains("+ new();")
            && r.spans
                .iter()
                .any(|(t, tone)| t.contains("new();") && *tone == Tone::Success)
    }));
    assert!(d.rows.iter().any(|r| {
        r.text.contains("− old();")
            && r.spans
                .iter()
                .any(|(t, tone)| t.contains("old();") && *tone == Tone::Failure)
    }));
}
#[test]
fn absent_diff_is_not_proof_of_no_changes() {
    let (c, mut n, s) = fixture();
    n.cells[0].changes = None;
    let mut u = Workbench::default();
    u.tabs.insert(1, CellTab::Diff);
    assert!(words(&doc(&c, &n, &s, &u)).contains("does not prove no files changed"));
}
#[test]
fn structured_errors_override_fold_and_noise() {
    let (c, mut n, s) = fixture();
    n.cells[0].error = Some(CellError {
        class: "TypeError".into(),
        message: "Missing result".into(),
        ..Default::default()
    });
    let mut u = Workbench::default();
    u.collapsed.insert(1);
    assert!(
        doc(&c, &n, &s, &u)
            .rows
            .iter()
            .any(|r| r.text.contains("Missing result") && r.tone == Tone::Failure)
    );
}
#[test]
fn compiler_chatter_stays_muted() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.tabs.insert(1, CellTab::Output);
    assert!(
        doc(&c, &n, &s, &u)
            .rows
            .iter()
            .any(|r| r.text.contains("dependency graph") && r.tone == Tone::Muted)
    );
}
#[test]
fn helper_details_preserve_assignment_model_and_evidence() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.helper = Some((1, 0));
    // How the helper prepared its input is one chip further in.
    u.helper_raw = Some((1, 0));
    let t = words(&doc(&c, &n, &s, &u));
    for part in [
        "Retain failures",
        "fixture-helper",
        "prepare failure windows",
    ] {
        assert!(t.contains(part), "{t}");
    }
}
/// A helper opened inside its card offers its raw record as a chip that a
/// click opens, like every other chip.
#[test]
fn a_helper_in_a_card_opens_its_raw_record_on_a_click() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.helper = Some((1, 0));
    draw(&c, &n, &s, &mut u, 100, 40);
    let raw = Action::HelperRaw(1, 0);
    assert!(
        u.geometry.hits.iter().any(|(_, a)| *a == raw),
        "the raw chip is not clickable"
    );
    click(&mut u, &mut s, &n, raw);
    assert_eq!(u.helper_raw, Some((1, 0)));
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("prepare failure windows"), "{screen}");
}
#[test]
fn long_wait_has_time_but_no_fake_percentage() {
    let (c, mut n, mut s) = fixture();
    n.cells[0].helpers[0].outcome = HelperOutcome {
        elapsed_ms: 123000,
        ..Default::default()
    };
    s.activity = Activity::Compacting;
    let mut u = Workbench::default();
    u.helper = Some((1, 0));
    let t = words(&doc(&c, &n, &s, &u));
    assert!(t.contains("123.0s"));
    assert!(t.contains("estimate unknown"));
    assert!(!t.contains("Returned:"));
    assert!(!t.contains('%'));
}
#[test]
fn failed_helper_does_not_disappear_in_a_large_roster() {
    let (c, mut n, s) = fixture();
    let h = n.cells[0].helpers[0].clone();
    n.cells[0].helpers = vec![h; 8];
    n.cells[0].helpers[7].outcome.ok = false;
    n.cells[0].helpers[7].outcome.text = "Provider unavailable".into();
    assert!(
        doc(&c, &n, &s, &Workbench::default())
            .rows
            .iter()
            .any(|r| r.text.contains("Provider unavailable") && r.tone == Tone::Failure)
    );
}
#[test]
fn host_lowered_frame_is_labeled_honestly() {
    let (c, mut n, s) = fixture();
    n.cells[0].origin = sterna::abi::Origin::DirectTool;
    assert!(words(&doc(&c, &n, &s, &Workbench::default())).contains("Host-lowered"));
}
/// A cell still being written is shown as the program it is becoming, never
/// as the protocol text that carries it -- and never as something that ran.
#[test]
fn tool_stream_is_not_misrepresented_as_executed_source() {
    let (c, n, mut s) = fixture();
    s.streaming_tool_input =
        Some("{\"code\":\"const guide = await read({path: \\\"AGENTS.md\\\"});\\n  bash({".into());
    // Default: one row per acting call, named by its first argument, with
    // the characters it spans so far -- never the unformatted program.
    assert_eq!(s.stream, sterna::tui::Stream::Actions);
    let text = words(&doc(&c, &n, &s, &Workbench::default()));
    assert!(
        text.contains("writing cell 002 · 2 actions · not executed"),
        "{text}"
    );
    assert!(text.contains("read    AGENTS.md · "), "{text}");
    assert!(text.contains("bash "), "{text}");
    assert!(!text.contains("const guide"), "{text}");
    assert!(!text.contains("{\"code"), "{text}");
    // Code: the decoded program, calls lit, with the not-executed mark.
    s.stream = sterna::tui::Stream::Code;
    let d = doc(&c, &n, &s, &Workbench::default());
    let text = words(&d);
    assert!(
        text.contains("writing cell 002 · 2 lines so far · not executed"),
        "{text}"
    );
    assert!(
        text.contains("const guide = await read({path: \"AGENTS.md\"});"),
        "{text}"
    );
    assert!(
        !text.contains("{\"code"),
        "the raw protocol text is not shown: {text}"
    );
    assert!(
        d.rows.iter().any(|r| r
            .spans
            .iter()
            .any(|(t, tone)| t == "read" && *tone == Tone::Accent)),
        "the acting call is lit"
    );
    // Raw: the protocol text, muted, for someone debugging the protocol.
    s.stream = sterna::tui::Stream::Raw;
    let d = doc(&c, &n, &s, &Workbench::default());
    assert!(words(&d).contains("not executed"));
    assert!(
        d.rows
            .iter()
            .any(|r| r.text.contains("{\"code") && r.tone == Tone::Muted)
    );
    // A fragment that carries no program yet falls back to the raw text.
    s.stream = sterna::tui::Stream::Code;
    s.streaming_tool_input = Some("{\"co".into());
    assert!(words(&doc(&c, &n, &s, &Workbench::default())).contains("Receiving cell input"));
}

/// Inside a cell, what happened is read off the record: every call, in
/// order, with how it ended -- and the program's acting calls are lit.
#[test]
fn a_cell_shows_its_chain_of_calls_and_lights_the_acting_functions() {
    let (c, mut n, s) = fixture();
    n.cells[0].execution = Some(
        "├─ read AGENTS.md · returned\n├─ bash cd demo && git status · denied · the host call gate denied this exact attempt\n└─ checks.run tests · failed · TypeError"
            .into(),
    );
    n.cells[0].output = Some("{\"loaded\":\"AGENTS.md\",\"lines\":206}".into());
    let d = doc(&c, &n, &s, &Workbench::default());
    let text = words(&d);
    for step in ["✓ read", "⊘ bash", "✕ checks.run"] {
        assert!(text.contains(step), "{step} missing from:\n{text}");
    }
    assert!(text.contains("the host call gate denied"), "{text}");
    // The result is JSON, so it is read as JSON: one field to a line.
    assert!(text.contains("\"loaded\": \"AGENTS.md\""), "{text}");
    assert!(text.contains("\"lines\": 206"), "{text}");
    // And `checks.run(` in the program is an acting call.
    assert!(
        d.rows.iter().any(|r| r
            .spans
            .iter()
            .any(|(t, tone)| t == "checks.run" && *tone == Tone::Accent)),
        "{text}"
    );
}

/// One turn of Sterna's carries its name once, however many cells it has.
#[test]
fn one_turn_of_several_cells_carries_the_name_once() {
    let (mut c, mut n, s) = fixture();
    c.messages.push(Message::tool_result("call-1", "ok", false));
    let mut m = Message::text(Role::Assistant, "Then the tests.");
    m.content.push(Block::ToolUse {
        id: "call-2".into(),
        name: "execute_cell".into(),
        input: serde_json::json!({"code":"await checks.run(\"tests\");"}),
    });
    c.messages.push(m);
    n.cells.push(n.cells[0].clone());
    let d = doc(&c, &n, &s, &Workbench::default());
    let labels = d
        .rows
        .iter()
        .filter(|r| r.kind == sterna::workbench::RowKind::Sterna)
        .count();
    assert_eq!(labels, 1, "{}", words(&d));
}
/// The terminal owns the background. The one thing the workbench paints is
/// a chip that is the current choice of a set -- filled in the accent so
/// "this is what you have now" is read at a glance -- and even that is a
/// handful of cells, never a surface.
#[test]
fn every_theme_and_local_surface_keeps_terminal_background() {
    let (c, n, s) = fixture();
    for theme in Theme::ALL {
        let mut s = s.clone();
        s.theme = theme;
        // The accent as a true-colour terminal is sent it.
        s.truecolor = true;
        for mode in 0..4 {
            let mut u = Workbench::default();
            match mode {
                1 => u.open(Source::Sandbox),
                2 => u.open(Source::Confirm("full".into())),
                3 => u.open(Source::Models(Box::new(navigator()))),
                _ => {}
            }
            let b = draw(&c, &n, &s, &mut u, 100, 40);
            // A bird's pixels are the one drawing that paints: the lower
            // half of a half block is its colour.
            let cells: Vec<_> = b
                .content
                .iter()
                .filter(|cell| !["▀", "▄"].contains(&cell.symbol()))
                .collect();
            let painted = cells.iter().filter(|cell| cell.bg != Color::Reset).count();
            assert!(
                cells
                    .iter()
                    .all(|cell| cell.bg == Color::Reset || cell.bg == theme_accent(theme)),
                "{theme:?} mode {mode}: a background other than the accent"
            );
            assert!(
                painted <= 40,
                "{theme:?} mode {mode}: {painted} painted cells"
            );
        }
    }
}
/// The accent as the chip paints it; mono paints nothing and reverses.
fn theme_accent(theme: Theme) -> Color {
    match theme {
        Theme::Neon => Color::Rgb(0xda, 0xff, 0x50),
        Theme::Amber => Color::Rgb(0xff, 0xce, 0x72),
        Theme::Ice => Color::Rgb(0x8b, 0xe3, 0xff),
        Theme::Mono => Color::Reset,
        Theme::Violet => Color::Rgb(0xd4, 0xb4, 0xff),
        Theme::Cobalt => Color::Rgb(0x9e, 0xc9, 0xff),
        Theme::Mint => Color::Rgb(0x86, 0xf1, 0xd0),
        Theme::Rose => Color::Rgb(0xff, 0xb3, 0xd4),
        Theme::Bird(bird) => {
            let rgb = bird.plumage().accent;
            Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
        }
    }
}
#[test]
fn resize_cannot_panic_or_leave_click_targets_offscreen() {
    let (c, n, s) = fixture();
    for (w, h) in [
        (1, 1),
        (8, 4),
        (30, 10),
        (60, 22),
        (80, 30),
        (120, 45),
        (200, 55),
    ] {
        for mode in 0..5 {
            let mut u = Workbench::default();
            match mode {
                1 => u.open(Source::Sandbox),
                2 => u.open(Source::Confirm("full".into())),
                3 => u.open(Source::Models(Box::new(navigator()))),
                4 => u.open(Source::Activity),
                _ => {}
            }
            draw(&c, &n, &s, &mut u, w, h);
            for (r, _) in &u.geometry.hits {
                assert!(r.right() <= w && r.bottom() <= h, "{w}x{h}: {r:?}");
            }
        }
    }
}
#[test]
fn reduced_motion_freezes_only_decoration() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Thinking;
    s.reduced_motion = true;
    let mut u = Workbench::default();
    let a = text(&draw(&c, &n, &s, &mut u, 100, 40));
    s.animation_frame = 8;
    assert_eq!(a, text(&draw(&c, &n, &s, &mut u, 100, 40)));
}
#[test]
fn active_animation_changes_at_most_three_cells() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Thinking;
    let mut u = Workbench::default();
    let a = draw(&c, &n, &s, &mut u, 100, 40);
    s.animation_frame = 8;
    let b = draw(&c, &n, &s, &mut u, 100, 40);
    let changed = a
        .content
        .iter()
        .zip(&b.content)
        .filter(|(a, b)| a != b)
        .count();
    assert!(changed > 0 && changed <= 3, "{changed}");
}
#[test]
fn click_waits_for_release_and_batched_drag_only_copies() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 100, 40);
    let (r, _) = u
        .geometry
        .hits
        .iter()
        .find(|(_, a)| *a == Action::Cell(1))
        .unwrap()
        .clone();
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Down(MouseButton::Left),
        r.x,
        r.y,
    );
    assert!(!u.collapsed.contains(&1));
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Drag(MouseButton::Left),
        r.x + 8,
        r.y,
    );
    let e = mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Up(MouseButton::Left),
        r.x + 8,
        r.y,
    );
    assert!(matches!(e, Effect::Copy(_)), "{e:?}");
    assert!(!u.collapsed.contains(&1));
    click(&mut u, &mut s, &n, Action::Cell(1));
    assert!(u.collapsed.contains(&1));
}
#[test]
fn keyboard_and_mouse_open_the_same_diff() {
    let (c, n, mut s) = fixture();
    let mut a = Workbench::default();
    draw(&c, &n, &s, &mut a, 100, 40);
    click(&mut a, &mut s, &n, Action::Tab(1, CellTab::Diff));
    let mut b = Workbench::default();
    key(&mut b, &mut s, &n, KeyCode::F(4));
    assert_eq!(a.tabs, b.tabs);
}
#[test]
fn scroll_keeps_composer_and_latest_resumes_following() {
    let (mut c, n, mut s) = fixture();
    for _ in 0..40 {
        c.messages
            .push(Message::text(Role::User, "An earlier instruction."));
    }
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 80, 20);
    let composer = u.geometry.composer;
    mouse(&mut u, &mut s, &n, MouseEventKind::ScrollUp, 10, 5);
    assert!(s.scrollback > 0);
    draw(&c, &n, &s, &mut u, 80, 20);
    assert_eq!(composer, u.geometry.composer);
    click(&mut u, &mut s, &n, Action::Latest);
    assert_eq!(s.scrollback, 0);
}
#[test]
fn modal_navigation_preserves_draft_and_transcript_position() {
    let (c, n, mut s) = fixture();
    s.scrollback = 4;
    s.input = "keep my draft".into();
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(navigator())));
    draw(&c, &n, &s, &mut u, 100, 40);
    mouse(&mut u, &mut s, &n, MouseEventKind::ScrollDown, 10, 8);
    key(&mut u, &mut s, &n, KeyCode::Char('a'));
    key(&mut u, &mut s, &n, KeyCode::Esc);
    assert_eq!(s.scrollback, 4);
    assert_eq!(s.input, "keep my draft");
}
#[test]
fn unavailable_catalogue_is_hidden_and_cannot_be_selected() {
    let mut m = navigator();
    assert_eq!(m.candidates().len(), 2);
    m.all_sources = true;
    assert_eq!(m.candidates().len(), 3);
    m.query = "unavailable".into();
    m.selected = 0;
    assert!(m.choose().unwrap_err().contains("credential"));
}
#[test]
fn model_search_spaces_do_not_stage_models() {
    let (c0, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(navigator())));
    for c in "A helper".chars() {
        key(&mut u, &mut s, &n, KeyCode::Char(c));
    }
    draw(&c0, &n, &s, &mut u, 100, 40);
    let m = u.models_mut().unwrap();
    assert_eq!(m.query, "A helper");
    m.role = 1;
    assert_eq!(m.choose().unwrap(), "/model helper fixture-helper");
}
#[test]
fn unmeasured_does_not_mean_zero() {
    let mut m = navigator();
    m.scores.insert("fixture-main".into(), 80.0);
    m.measured_order = true;
    let rows = m.candidates();
    assert_eq!(rows[0].score, Some(80.0));
    assert_eq!(rows[1].score, None);
}
#[test]
fn picker_never_offers_implicit_subagent_inheritance() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    let mut m = navigator();
    m.role = 2;
    u.open(Source::Models(Box::new(m)));
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(
        screen.contains("PINNED") && screen.contains("QUICK"),
        "{screen}"
    );
    assert!(!u.geometry.hits.iter().any(
        |(_, a)| matches!(a,Action::Command(c) if c.contains("inherit")||c.ends_with(" auto"))
    ));
}
#[test]
fn settings_browsing_creates_no_file() {
    let (t, _, p) = prefs();
    assert!(!p.path.exists());
    assert!(!t.0.join(".sterna/config.toml").exists());
}
#[test]
fn direct_save_and_undo_use_the_native_store() {
    let (_t, mut s, mut p) = prefs();
    let before = s.theme;
    p.save("ui.theme", Some("amber".into()), &mut s).unwrap();
    assert_eq!(s.theme, Theme::Amber);
    assert!(std::fs::read_to_string(&p.path).unwrap().contains("amber"));
    let mut u = Workbench::default();
    u.changes.extend(p.take_change());
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 40);
    ctrl(&mut u, &mut s, &n, 'z');
    assert!(u.preferences().unwrap().saved("ui.theme").is_none());
    assert_eq!(s.theme, before, "the screen takes it back too");
    assert!(
        u.top().unwrap().sheet.notice.starts_with("Restored: Theme"),
        "{}",
        u.top().unwrap().sheet.notice
    );
    ctrl(&mut u, &mut s, &n, 'z');
    assert_eq!(u.top().unwrap().sheet.notice, "Nothing to undo.");
}
/// Every action a click reaches on screen: the chips drawn, and the ones
/// folded into a `⟨ +N ▾ ⟩` chip's list.
fn reachable(u: &Workbench) -> Vec<Action> {
    let mut all = Vec::new();
    for (_, action) in &u.geometry.hits {
        match action {
            Action::More(folded) => all.extend(folded.iter().map(|(_, a)| a.clone())),
            other => all.push(other.clone()),
        }
    }
    all
}
/// A settings sheet narrower than its section strip folds the sections it
/// cannot draw into `⟨ +N ▾ ⟩`, as every chip row does: none is cut off,
/// and one chosen from the fold opens.
#[test]
fn every_settings_section_and_tool_is_reachable_on_a_narrow_screen() {
    let (_t, mut s, p) = prefs();
    let (c, n, _) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    for width in [80, 50] {
        draw(&c, &n, &s, &mut u, width, 30);
        let reach = reachable(&u);
        // Everyday, Display, Little helpers, Models & accounts, Subagents,
        // Advanced, Tuning.
        for i in 0..7 {
            assert!(
                reach.contains(&Action::Sheet(Hit::Section(i))),
                "{width} columns: section {i} cannot be reached"
            );
        }
        for i in 0..2 {
            assert!(
                reach.contains(&Action::Sheet(Hit::Tool(i))),
                "{width} columns: tool {i} cannot be reached"
            );
        }
    }
    draw(&c, &n, &s, &mut u, 80, 30);
    let tuning = 6;
    let fold = u
        .geometry
        .hits
        .iter()
        .find_map(|(_, a)| {
            matches!(a, Action::More(folded) if folded.iter().any(|(_, f)| *f == Action::Sheet(Hit::Section(tuning))))
                .then(|| a.clone())
        })
        .expect("Tuning is folded at 80 columns");
    click(&mut u, &mut s, &n, fold);
    draw(&c, &n, &s, &mut u, 80, 30);
    let row = u
        .top()
        .unwrap()
        .sheet
        .items
        .iter()
        .position(|item| item.title == "Tuning")
        .expect("the fold lists Tuning");
    click(&mut u, &mut s, &n, Action::Sheet(Hit::Item(row)));
    assert_eq!(u.preferences().unwrap().category, tuning);
}
#[test]
fn escape_does_not_undo_saved_settings_or_unrelated_session_overrides() {
    let (_t, mut s, mut p) = prefs();
    s.reduced_motion = true;
    p.save("ui.theme", Some("ice".into()), &mut s).unwrap();
    let path = p.path.clone();
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    key(&mut u, &mut s, &Notebook::default(), KeyCode::Esc);
    assert!(std::fs::read_to_string(path).unwrap().contains("ice"));
    assert_eq!(s.theme, Theme::Ice);
    assert!(s.reduced_motion);
}
#[test]
fn invalid_setting_leaves_disk_and_live_state_unchanged() {
    let (_t, mut s, mut p) = prefs();
    p.save("ui.theme", Some("ice".into()), &mut s).unwrap();
    let before = std::fs::read(&p.path).unwrap();
    assert!(p.save("ui.theme", Some("invalid".into()), &mut s).is_err());
    assert_eq!(before, std::fs::read(&p.path).unwrap());
    assert_eq!(s.theme, Theme::Ice);
}
/// A lifted boundary is on screen at every width the chrome is drawn at.
///
/// **An invisible mode is a mode error waiting to happen.** The session bar
/// gives controls up as the terminal narrows, and the rule it used to follow
/// -- drop the second-from-last -- dropped the boundary first, so an
/// eighty-column window running with full access looked exactly like one
/// confined to the project. A control drawn in a warning tone is now exempt
/// from that rule, and the chip carries a glyph as well as a colour, because a
/// monochrome terminal must carry the same warning.
#[test]
fn a_lifted_boundary_is_never_the_control_a_narrow_terminal_drops() {
    let (c, n, mut s) = fixture();
    s.project = Some("a-fairly-long-project-name".into());
    s.model = Some("some-long-model-identifier".into());
    s.level.set(sterna::permissions::Level::Full);
    for width in [60u16, 80, 100, 140] {
        let mut u = Workbench::default();
        let screen = text(&draw(&c, &n, &s, &mut u, width, 24));
        assert!(
            screen.contains("▲ Full access"),
            "at {width} columns:\n{screen}"
        );
    }
}

/// A picker says which option the session is on, not only where the cursor is.
///
/// **A list of choices that does not mark the current one is a quiz.** Both
/// pickers highlighted the row under the cursor and nothing else, so opening
/// one to check what the session was doing told you only where the cursor had
/// stopped. The mark is a glyph and the word `now`, so it survives a
/// monochrome terminal.
#[test]
fn a_picker_marks_the_option_the_session_is_on() {
    let (c, n, s) = fixture();
    s.level.set(sterna::permissions::Level::Ask);
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    let sheet = text(&draw(&c, &n, &s, &mut u, 120, 24));
    assert!(sheet.contains("Ask  ● now"), "{sheet}");
    assert!(
        !sheet.contains("Sandboxed  ● now"),
        "only one is current:\n{sheet}"
    );
}

#[test]
fn saved_permissions_do_not_change_running_authority() {
    let (_t, mut s, mut p) = prefs();
    // **A grant is not a level, and only one of the two may move.**
    // A denial list is authority: saving it must leave the running session
    // exactly where it was, and every `permissions` key is
    // deliberately absent from `live_command` so nothing can carry one into
    // a session that is already running.
    p.save("permissions.deny", Some("Read(secrets/**)".into()), &mut s)
        .unwrap();
    assert_eq!(p.take_live(), None, "a grant never reaches a live session");
    assert!(
        p.notice.contains("next one"),
        "and it says so: {}",
        p.notice
    );
    // The level is how much runs without asking, which `/sandbox <level>`
    // and the level chip move mid-session. The panel is a third route to the
    // same control, so it moves it too -- by handing the loop that same
    // command, never by writing the level behind the session's back.
    let before = s.level.level();
    p.save("sandbox.level", Some("ask".into()), &mut s).unwrap();
    assert_eq!(
        s.level.level(),
        before,
        "the panel does not reach into the level itself"
    );
    assert_eq!(p.take_live().as_deref(), Some("/sandbox ask"));
}
#[test]
fn concurrent_file_edits_are_not_overwritten() {
    let (_t, mut s, mut p) = prefs();
    p.save("ui.theme", Some("ice".into()), &mut s).unwrap();
    std::fs::write(&p.path, "[ui]\ntheme = \"rose\"\n").unwrap();
    assert!(p.save("ui.theme", Some("amber".into()), &mut s).is_err());
    assert!(std::fs::read_to_string(&p.path).unwrap().contains("rose"));
}
#[test]
fn every_native_key_is_searchable_but_normal_categories_are_bounded() {
    let (_t, _, mut p) = prefs();
    for category in 0..5 {
        p.category = category;
        assert!(p.rows().len() <= 8, "category {category}");
    }
    // Every key a person can choose; not Sterna's own bookkeeping, and not
    // the retired key the parser still reads.
    for spec in sterna::settings::specs()
        .iter()
        .filter(|spec| !sterna::settings::hidden(spec.key) && spec.key != "limits.task_tokens")
    {
        p.query = spec.key.into();
        assert!(p.rows().iter().any(|s| s.key == spec.key), "{}", spec.key);
    }
}
#[test]
fn full_access_requires_confirmation_that_opens_on_cancel() {
    use sterna::permissions::Level;
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    draw(&c, &n, &s, &mut u, 100, 40);
    click_item(&mut u, &mut s, &n, "level:full");
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    assert_ne!(s.level.level(), Level::Full);
    // A key typed as the confirmation appears answers nothing.
    draw(&c, &n, &s, &mut u, 100, 40);
    key(&mut u, &mut s, &n, KeyCode::Enter);
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    assert!(u.top().unwrap().sheet.notice.contains("held back"));
    // Half a second without a key arms it; it starts on Cancel, so a
    // reflexive Enter changes nothing and goes back to the Sandbox sheet.
    std::thread::sleep(sterna::workbench::sheet::ARMING + std::time::Duration::from_millis(50));
    key(&mut u, &mut s, &n, KeyCode::Enter);
    assert_ne!(s.level.level(), Level::Full);
    assert!(u.showing(|source| matches!(source, Source::Sandbox)));
    draw(&c, &n, &s, &mut u, 100, 40);
    click_item(&mut u, &mut s, &n, "level:full");
    draw(&c, &n, &s, &mut u, 100, 40);
    armed(&mut u);
    click_item(&mut u, &mut s, &n, "confirm:yes");
    assert_eq!(s.level.level(), Level::Full);
    // Going back down is not a lifted boundary: it is set at once.
    draw(&c, &n, &s, &mut u, 100, 40);
    click_item(&mut u, &mut s, &n, "level:ask");
    assert_eq!(s.level.level(), Level::Ask);
}
#[test]
fn the_live_session_uses_the_new_renderer() {
    let src = include_str!("../src/session/ui.rs");
    let live = src.split("#[cfg(test)]\nmod tests").next().unwrap();
    assert!(live.contains("crate::workbench::render"));
    assert!(!live.contains("tui::render_screen_with_geometry("));
}

#[test]
fn sidebar_visibility_is_respected_without_opaque_surfaces() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    s.sidebar = sterna::tui::SidebarVisibility::Shown;
    let with = draw(&c, &n, &s, &mut u, 140, 40);
    let shown = u.geometry.transcript.width;
    assert!(text(&with).contains("GUARDRAILS"));
    s.sidebar = sterna::tui::SidebarVisibility::Hidden;
    let without = draw(&c, &n, &s, &mut u, 140, 40);
    assert!(!text(&without).contains("GUARDRAILS"));
    assert!(u.geometry.transcript.width > shown);
}

#[test]
fn reader_anchor_survives_rows_inserted_above_it() {
    let mut u = Workbench::default();
    let mut s = ScreenState {
        scrollback: 10,
        ..Default::default()
    };
    let mut d = Document::default();
    for i in 0..30 {
        d.push(format!("row {i}"), Tone::Normal, None, 80, i);
    }
    u.anchor = Some((d.rows[12].key, d.rows[12].text.clone()));
    u.last_scrollback = 10;
    d.rows.insert(0, d.rows[0].clone());
    u.anchor_document(&d, &mut s, 8);
    assert_eq!(d.rows[d.rows.len() - 8 - s.scrollback].text, "row 12");
    s.scrollback = 0;
    u.anchor_document(&d, &mut s, 8);
    assert_eq!(s.scrollback, 0);
}

/// Turning favourites on, emptying a slot and turning a tier off change
/// the picker at once, as choosing a model does: its rows never show the
/// assignment from before the command was sent.
#[test]
fn the_picker_shows_what_its_own_commands_did() {
    let (c, n, mut s) = fixture();
    let mut m = navigator();
    m.role = 2;
    m.assignment.slots.insert(
        "quick".into(),
        sterna::config::AgentSlot {
            model: "fixture-helper".into(),
            effort: sterna::wire::Effort::Low,
        },
    );
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(m)));
    let toggle = |u: &Workbench| {
        u.top()
            .unwrap()
            .sheet
            .items
            .iter()
            .find(|item| item.id == "favourites")
            .map(|item| item.kind.clone())
    };
    draw(&c, &n, &s, &mut u, 110, 40);
    assert_eq!(toggle(&u), Some(sterna::workbench::ItemKind::Toggle(false)));
    assert_eq!(
        click_item(&mut u, &mut s, &n, "favourites"),
        Effect::Command("/subagents on".into())
    );
    draw(&c, &n, &s, &mut u, 110, 40);
    assert_eq!(toggle(&u), Some(sterna::workbench::ItemKind::Toggle(true)));
    // The one favourite emptied: the slot goes, and favourites with it.
    u.models_mut().unwrap().slot = Some("quick".into());
    draw(&c, &n, &s, &mut u, 110, 40);
    assert_eq!(
        click_item(&mut u, &mut s, &n, "off"),
        Effect::Command("/subagents quick off".into())
    );
    assert!(u.models().unwrap().assignment.slots.is_empty());
    draw(&c, &n, &s, &mut u, 110, 40);
    assert_eq!(toggle(&u), Some(sterna::workbench::ItemKind::Toggle(false)));
    // A tier turned off says so where the picker says what it is now.
    u.models_mut().unwrap().role = 1;
    u.models_mut().unwrap().slot = None;
    draw(&c, &n, &s, &mut u, 110, 40);
    assert_eq!(
        click_item(&mut u, &mut s, &n, "off"),
        Effect::Command("/model helper off".into())
    );
    let screen = text(&draw(&c, &n, &s, &mut u, 110, 40));
    assert!(screen.contains("Now: off"), "{screen}");
}

/// A locked account's models share one reason, and it is said once under
/// the first of them, not repeated under each.
#[test]
fn a_reason_shared_by_a_run_of_rows_is_said_once() {
    let (c, n, s) = fixture();
    let panel = Panel::models(
        "Models",
        vec![ModelGroup {
            provider: "B".into(),
            account: "b-subscription".into(),
            scope: "subscription".into(),
            models: vec!["b-one".into(), "b-two".into(), "b-three".into()],
            selectable: Some(false),
            unavailable_reason: Some("Not connected · sign in above".into()),
            connect: Some("b".into()),
            pooled: None,
            note: None,
        }],
        TierModels {
            parent: "b-one".into(),
            helper: None,
            subagent: None,
        },
    );
    let mut m = Navigator::from_panel(&panel).unwrap();
    // Every account, the locked ones too.
    m.all_sources = true;
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(m)));
    let screen = text(&draw(&c, &n, &s, &mut u, 110, 40));
    assert!(screen.contains("b-three"), "{screen}");
    assert_eq!(
        screen.matches("Not connected · sign in above").count(),
        1,
        "{screen}"
    );
}

#[test]
fn favorites_picker_assigns_one_slot_and_preserves_other_roles() {
    let mut m = navigator();
    m.role = 2;
    m.slot = Some("quick".into());
    let candidate = m.candidates()[m.selected].model.clone();
    // The slot's effort travels with it: quick runs at low unless chosen.
    assert_eq!(
        m.choose().unwrap(),
        format!("/subagents quick {candidate} low")
    );
    assert!(m.assignment.slots.is_empty());
}

#[test]
fn choosing_a_model_from_global_settings_preserves_scope_and_live_assignment() {
    let (_temp, mut s, p) = prefs();
    assert_eq!(
        p.scope,
        sterna::settings::Scope::Global,
        "Settings opens on Global"
    );
    let path = p.path.clone();
    s.model = Some("live-main".into());
    let mut m = navigator();
    m.role = 1;
    m.target_key = Some("helpers.model".into());
    let chosen = m.candidates()[m.selected].clone();
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    u.push(Source::Models(Box::new(m)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 40);
    // Saved, and applied to the running session like every other setting
    // that applies now: the helper changes, the main model does not.
    assert_eq!(
        click_item(
            &mut u,
            &mut s,
            &n,
            &format!("model:{}:{}", chosen.route, chosen.model)
        ),
        Effect::Command(format!("/model helper {}", chosen.model))
    );
    let chosen = chosen.model;
    assert_eq!(s.model.as_deref(), Some("live-main"));
    let p = u.preferences().unwrap();
    assert_eq!(p.scope, sterna::settings::Scope::Global);
    assert!(std::fs::read_to_string(path).unwrap().contains(&chosen));
}

#[test]
fn settings_favorite_removal_and_undo_are_atomic() {
    let (_temp, mut s, mut p) = prefs();
    p.save(
        "agents.slots.quick.model",
        Some("small-model".into()),
        &mut s,
    )
    .unwrap();
    p.save("agents.slots.quick.effort", Some("low".into()), &mut s)
        .unwrap();
    p.save("agents.mode", Some("roster".into()), &mut s)
        .unwrap();
    p.save("agents.slots.quick.model", None, &mut s).unwrap();
    assert!(p.loaded.config.agents.slots.is_empty());
    assert_eq!(p.loaded.config.agents.mode, sterna::config::AgentsMode::Off);
    let mut u = Workbench::default();
    u.changes.extend(p.take_change());
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 40);
    ctrl(&mut u, &mut s, &n, 'z');
    let p = u.preferences().unwrap();
    assert_eq!(
        p.loaded.config.agents.mode,
        sterna::config::AgentsMode::Roster
    );
    assert_eq!(p.loaded.config.agents.slots["quick"].model, "small-model");
}

/// Development aid: prints the rendered workbench so the layout can be read.
/// `cargo test -p sterna --test workbench -- --ignored screenshot --nocapture`
#[test]
#[ignore]
fn screenshot() {
    let (c, n, mut s) = fixture();
    s.project = Some("prismMLqwen".into());
    s.model = Some("gpt-5.6-sol".into());
    s.sandbox = Some("3 path rules · 1 command pattern".into());
    s.confinement = Some("unconfined".into());
    s.network = Some("off".into());
    s.helpers_on = true;
    s.subagents = Some("off".into());
    for note in [
        "session tlqdct-yqr — resume it with:  sterna --resume tlqdct-yqr",
        "sandbox: --yolo — the project root and every command line are granted; native permission denials and the never-grantable set still apply",
        "sandbox: argv admission is a word scan over each part of a command line, not a shell",
        "sandbox: full access — Sterna applies no OS confinement to the children it spawns",
    ] {
        s.note(note);
    }
    s.startup_notes = Some(5);
    let mut u = Workbench::default();
    for (name, w, h) in [("WIDE 140x40", 140u16, 40u16), ("NARROW 80x30", 80, 30)] {
        let b = draw(&c, &n, &s, &mut u, w, h);
        println!("\n===== {name} =====\n{}", text(&b));
    }
    let (mut c2, n2, mut s2) = fixture();
    s2.activity = Activity::Executing;
    s2.input = "keep the decorative mark still".into();
    c2.messages
        .push(Message::text(Role::User, "and check the tests"));
    c2.messages.push({
        let mut m = Message::text(Role::Assistant, "Checking the guard now.");
        m.content.push(Block::ToolUse{id:"call-2".into(),name:"execute_cell".into(),input:serde_json::json!({"code":"await edit(\"motion.rs\");\nconst t = await checks.run(\"tests\");"})});
        m
    });
    let mut u = Workbench::default();
    let b = draw(&c2, &n2, &s2, &mut u, 140, 40);
    println!("\n===== RUNNING 140x40 =====\n{}", text(&b));
    let (c3, mut n3, mut s3) = fixture();
    n3.cells[0].execution = Some(
        "├─ read AGENTS.md · returned\n├─ bash cd demo && git status --short · denied · the person declined this exact call and asks for another way: \"use fd\"\n└─ checks.run tests · returned"
            .into(),
    );
    n3.cells[0].output = Some(
        "{\"loaded\":\"AGENTS.md\",\"lines\":206,\"summary\":{\"text\":\"# Agent guide\"}}".into(),
    );
    s3.activity = Activity::Streaming;
    s3.streaming_tool_input = Some("{\"code\":\"const [overview, config] = await Promise.all([\\n  read({path: \\\"README.md\\\"}),\\n  bash({command: \\\"git status\\\"}),\\n]);\\nreturn {overview".into());
    let mut u = Workbench::default();
    let b = draw(&c3, &n3, &s3, &mut u, 140, 40);
    println!("\n===== CALLS + STREAMING 140x40 =====\n{}", text(&b));
    let mut u = Workbench::default();
    u.tabs.insert(1, CellTab::Diff);
    let b = draw(&c, &n, &s, &mut u, 140, 40);
    println!("\n===== DIFF 140x40 =====\n{}", text(&b));
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    let b = draw(&c, &n, &s, &mut u, 140, 40);
    println!("\n===== SANDBOX 140x40 =====\n{}", text(&b));
    let mut u = Workbench::default();
    u.open(Source::Confirm("full".into()));
    let b = draw(&c, &n, &s, &mut u, 140, 30);
    println!("\n===== CONFIRM FULL ACCESS 140x30 =====\n{}", text(&b));
    let mut sf = s.clone();
    sf.level = sterna::permissions::LiveLevel::new(sterna::permissions::Level::Full);
    sf.network = Some("on".into());
    let mut u = Workbench::default();
    let b = draw(&c, &n, &sf, &mut u, 80, 20);
    println!("\n===== FULL ACCESS 80x20 =====\n{}", text(&b));
    let root = Temp::new();
    let mut s3 = s.clone();
    s3.settings_root = Some(root.0.clone());
    let mut u = Workbench::default();
    u.open_settings(&s3);
    let b = draw(&c, &n, &s3, &mut u, 140, 40);
    println!("\n===== SETTINGS 140x40 =====\n{}", text(&b));
}

/// `/diff` opens the last recorded cell on its own diff, which is the route
/// the transcript's "Open diff ↗" and the F4 key also take.
#[test]
fn diff_command_opens_the_last_cell_on_its_diff() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    assert!(u.local_command("/diff", &mut s, &n));
    let d = doc(&c, &n, &s, &u);
    assert!(
        d.rows.iter().any(|r| r
            .tabs
            .iter()
            .any(|(label, tab)| label.starts_with("Changes") && *tab == CellTab::Diff)
            && r.action == Some(Action::Tab(1, CellTab::Diff))),
        "{}",
        words(&d)
    );
}

/// A narrowed search leaves nothing of the wider list behind it: the row a
/// filter removed must not be legible anywhere on the surface.
#[test]
fn a_filtered_navigator_leaves_no_row_of_the_wider_list() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    let mut nav = navigator();
    nav.query = "fixture-helper".into();
    u.open(Source::Models(Box::new(nav)));
    let b = draw(&c, &n, &s, &mut u, 80, 30);
    let screen = text(&b);
    assert!(screen.contains("fixture-helper"), "{screen}");
    // The tab row still names the session's main model; the *list* holds
    // one row, and the model the query excluded is not among them.
    // `fixture-main` is on the sheet once, in the line saying what Main
    // runs on now, and nowhere in the list.
    assert_eq!(screen.matches("fixture-main").count(), 1, "{screen}");
    assert!(!screen.contains("unavailable-model"), "{screen}");
}

/// A keystroke on the settings panel is inside the perceptual "instant" limit.
///
/// **"Instant updates" is a number, not a feeling: 100 ms.** Below it a person
/// reads the change as caused by their own keypress; above it they read it as
/// the program responding. Every arrow press on a Choice row re-opens the
/// store, re-reads the target file for optimistic concurrency, re-validates
/// the whole effective configuration through the same parser a session start
/// uses, writes atomically, and reloads global-then-project -- which is the
/// right thing to do and is worth knowing the cost of.
///
/// **It judges the FASTEST pass, not the average, and the ceiling is loose.**
/// A shared CI runner descheduling this thread for 200 ms is not a fact about
/// the code, and an average lets one such steal decide the verdict -- which is
/// what it did on run 35665385746, where a mean of five passes went red against
/// a 50 ms ceiling on a machine that had measured 15 ms. The minimum of several
/// passes is the work's own cost: a real regression -- a blocking call, a tree
/// walk, a network round trip in the save path -- makes every pass slow, and no
/// amount of load makes a 15 ms operation take a quarter of a second nine times
/// running. This is a ceiling that catches an order-of-magnitude regression,
/// not a benchmark.
#[test]
fn a_settings_keystroke_stays_inside_the_instant_budget() {
    let (_t, mut s, mut p) = prefs();
    // One warm pass first: the first save pays for creating the file.
    p.save("ui.stream", Some("code".into()), &mut s).unwrap();
    let mut best = std::time::Duration::MAX;
    for word in [
        "actions", "code", "raw", "actions", "code", "raw", "actions", "code", "raw",
    ] {
        let started = std::time::Instant::now();
        p.save("ui.stream", Some(word.into()), &mut s).unwrap();
        best = best.min(started.elapsed());
    }
    println!("one settings keystroke, fastest of nine: {best:?}");
    assert!(
        best < std::time::Duration::from_millis(250),
        "a keystroke that writes and revalidates took {best:?} even at its fastest, \
         which a person reads as the program answering rather than as their own press"
    );
}

// ---------------------------------------------------------------------------
// The application pass, 2026-09-22: regions you can see, a grammar for the
// conversation, one component language, and a character.

/// An open cell is a card: its top edge, every body row and its bottom edge
/// share the same two columns, so the eye finds the cell's extent without
/// reading it.
#[test]
fn an_open_cell_is_a_card_with_both_edges_on_every_row() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    let lines: Vec<&str> = screen.lines().collect();
    let top = lines
        .iter()
        .position(|l| l.contains("╭─ 001"))
        .expect("the card's top edge names the cell");
    let bottom = lines
        .iter()
        .position(|l| l.contains("╰─ ✓ executed"))
        .expect("the card's bottom edge says how it ended");
    assert!(bottom > top + 1, "{screen}");
    let column = |line: &str, glyph: char| line.chars().position(|c| c == glyph).unwrap();
    let left = column(lines[top], '╭');
    let right = column(lines[top], '╮');
    for line in &lines[top + 1..bottom] {
        assert_eq!(line.chars().nth(left), Some('│'), "{line}");
        assert_eq!(line.chars().nth(right), Some('│'), "{line}");
    }
    assert!(lines[top].contains("✓ EXECUTED"), "{}", lines[top]);
}

/// The conversation has turns: yours under a coloured bar with your name on
/// it, Sterna's under its mark. Nothing shares a texture with what it is not.
#[test]
fn turns_are_labelled_and_the_persons_words_stand_under_a_bar() {
    let (c, n, s) = fixture();
    let d = doc(&c, &n, &s, &Workbench::default());
    let you = d
        .rows
        .iter()
        .position(|r| r.kind == sterna::workbench::RowKind::You && r.text == "you")
        .expect("the person's turn is labelled");
    assert_eq!(d.rows[you + 1].kind, sterna::workbench::RowKind::You);
    assert!(d.rows[you + 1].text.contains("Respect reduced motion"));
    assert!(
        d.rows
            .iter()
            .any(|r| r.kind == sterna::workbench::RowKind::Sterna && r.text.contains("sterna"))
    );
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("┃ you"), "{screen}");
    assert!(screen.contains("┃ Respect reduced motion"), "{screen}");
    assert!(screen.contains("⠿ sterna"), "{screen}");
}

/// Every control in the top bar is a chip, and every chip is a click target
/// for the thing it names.
#[test]
fn the_top_bar_is_chips_and_each_one_hits_its_own_control() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 40));
    let bar = screen.lines().next().unwrap();
    for chip in [
        "⟨ fixture-main ▾ ⟩",
        "⟨ ◼ Sandboxed ⟩",
        "⟨ Settings ⟩",
        "⟨ ? ⟩",
    ] {
        assert!(bar.contains(chip), "{bar}");
    }
    for action in [
        Action::Models,
        Action::Sandbox,
        Action::Settings,
        Action::Help,
    ] {
        assert!(
            u.geometry
                .hits
                .iter()
                .any(|(r, a)| *a == action && r.y == 0),
            "{action:?} is not a target on the bar"
        );
    }
}

/// `?` on an empty composer is the sheet of keys; with anything typed it is
/// a question mark and reaches the editor.
#[test]
fn a_bare_question_mark_opens_the_key_sheet_and_escape_closes_it() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    assert_eq!(
        key(&mut u, &mut s, &n, KeyCode::Char('?')),
        Effect::Consumed
    );
    let keys = |u: &Workbench| u.showing(|source| matches!(source, Source::Keys));
    assert!(keys(&u));
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("KEYS"), "{screen}");
    assert!(screen.contains("Ctrl-T"), "{screen}");
    assert!(
        !screen.contains("Shift-Tab"),
        "no key cycles the level: {screen}"
    );
    key(&mut u, &mut s, &n, KeyCode::Esc);
    assert!(!keys(&u));
    s.input = "why?".into();
    assert_eq!(key(&mut u, &mut s, &n, KeyCode::Char('?')), Effect::Pass);
    assert!(!keys(&u));
}

/// The composer is a dock: its top edge says what the session is doing and
/// its bottom edge carries the effort, which is stepped in place, and a chip
/// for each other setting changed from Sterna's own default.
#[test]
fn the_composer_dock_carries_the_status_above_and_the_chips_below() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    let top = screen
        .lines()
        .find(|l| l.starts_with("╭─"))
        .expect("the dock has a top edge");
    assert!(top.contains("✓ complete"), "{top}");
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.starts_with("╰─"), "{bottom}");
    assert!(bottom.contains("⟨ effort default ⟩"), "{bottom}");
    for default in ["helpers", "subagents", "stream"] {
        assert!(
            !bottom.contains(default),
            "a default is not a chip: {bottom}"
        );
    }
    s.effort = sterna::wire::Effort::High;
    s.helpers_on = true;
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.contains("⟨ effort high ⟩"), "{bottom}");
    assert!(bottom.contains("⟨ ◇ helpers on ⟩"), "{bottom}");
    assert!(
        u.geometry.hits.iter().any(|(_, a)| *a == Action::Effort),
        "the effort chip is a control"
    );
    // And what typing lands on is still marked the way the transcript
    // marks what was said.
    assert!(screen.contains("│ ❯ "), "{screen}");
}

/// A parrot theme speaks the one plain voice a classic theme does: the
/// plain greeting and composer line, every fact, and none of the old
/// character lines.
#[test]
fn a_parrot_theme_speaks_the_one_plain_voice() {
    let (c, n, mut s) = fixture();
    s.theme = sterna::tui::Theme::Bird(sterna::workbench::plumage::Bird::Amazon);
    s.sidebar = sterna::tui::SidebarVisibility::Shown;
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 40));
    for fact in [
        "GUARDRAILS",
        "✓ EXECUTED",
        "┃ you",
        "⟨ Settings ⟩",
        "What should we build?",
        "Describe the next step",
    ] {
        assert!(screen.contains(fact), "lacks {fact}:\n{screen}");
    }
    for remark in [
        "Back in the nest",
        "What next?",
        "ready when you are",
        "psst",
    ] {
        assert!(!screen.contains(remark), "{remark}:\n{screen}");
    }
}

/// A notice rides the dock's edge for a few seconds with the way to undo
/// it beside it, then both fade; the transcript keeps the note.
#[test]
fn a_notice_fades_from_the_dock_and_takes_its_undo_with_it() {
    let (c, n, mut s) = fixture();
    s.effort = sterna::wire::Effort::Medium;
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 100, 40);
    let Effect::Command(step) = click(&mut u, &mut s, &n, Action::Effort) else {
        panic!("the effort chip steps the effort");
    };
    // The loop hands the command on; on its way out it joins the undo list.
    u.sent(&step, &s);
    assert_eq!(
        u.changes.last().map(|c| c.was.as_str()),
        Some("effort medium"),
        "stepping the effort offers the old one back"
    );
    u.notice = "effort is now high".into();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("effort is now high"), "{screen}");
    assert!(screen.contains("undo · effort medium"), "{screen}");
    assert!(
        u.geometry.hits.iter().any(|(_, a)| *a == Action::Undo),
        "the undo chip is beside the notice"
    );
    u.notice_at = Some(std::time::Instant::now() - sterna::workbench::NOTICE_LINGER * 2);
    assert!(u.notice_expired());
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(!screen.contains("effort is now high"), "{screen}");
    assert!(u.notice.is_empty() && !u.offer_undo);
    assert!(!u.geometry.hits.iter().any(|(_, a)| *a == Action::Undo));
    // The list outlives the notice: Ctrl-Z on a sheet still reaches it.
    assert_eq!(u.changes.len(), 1);
}

/// An empty conversation offers what the project itself suggests, as chips
/// that type the message; with nothing known it still offers one thing.
#[test]
fn the_opening_offers_the_projects_own_suggestions_as_chips() {
    let (_, n, mut s) = fixture();
    let c = Conversation::default();
    s.suggestions = vec![(
        "run the tests".into(),
        "Run the tests and tell me what fails.".into(),
    )];
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("⟨ run the tests ⟩"), "{screen}");
    assert!(
        u.geometry
            .hits
            .iter()
            .any(|(_, a)| *a == Action::Draft("Run the tests and tell me what fails.".into()))
    );
    s.suggestions.clear();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("⟨ explore this project ⟩"), "{screen}");
    s.theme = sterna::tui::Theme::Bird(sterna::workbench::plumage::Bird::Amazon);
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("⟨ explore this project ⟩"), "{screen}");
}

/// A finished turn ends in an answer block: the first line as the result, a
/// line of what it cost, and -- on the latest turn only -- what to do next.
#[test]
fn the_latest_answer_offers_what_to_do_next() {
    let (c, mut n, s) = fixture();
    n.cells[0].returned = Some("The motion guard is fixed.\nNothing else changed.".into());
    let mut u = Workbench::default();
    let d = doc(&c, &n, &s, &u);
    let answer = d
        .rows
        .iter()
        .find(|r| r.kind == sterna::workbench::RowKind::Answer)
        .expect("the answer's first line is marked");
    assert_eq!(answer.text.trim(), "The motion guard is fixed.");
    assert!(
        words(&d).contains("✓ 1 file · +1 −1 · 1 helper"),
        "{}",
        words(&d)
    );
    draw(&c, &n, &s, &mut u, 100, 40);
    for action in [
        Action::Tab(1, CellTab::Diff),
        Action::Insert("commit this".into()),
        Action::Tab(1, CellTab::Output),
    ] {
        assert!(
            u.geometry.hits.iter().any(|(_, a)| *a == action),
            "{action:?} is not offered"
        );
    }
}

/// The rows of the conversation card, from its greeting down.
fn card_rows(screen: &str, greeting: &str) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(greeting))
        .unwrap_or_else(|| panic!("no card greeting `{greeting}`:\n{screen}"));
    lines[at..(at + 4).min(lines.len())]
        .iter()
        .map(|l| l.chars().take(20).collect())
        .collect()
}

fn braille(text: &str) -> bool {
    text.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
}

/// **Once the conversation starts, a parrot theme keeps the bird's head
/// beside the card, in colour**; a classic theme draws no bird of any kind.
/// No braille songbird is left anywhere on the card.
#[test]
fn a_parrot_theme_keeps_its_head_on_the_card_and_a_classic_one_has_no_bird() {
    let (c, n, mut s) = fixture();
    assert!(!c.messages.is_empty());
    s.truecolor = true;
    s.theme = sterna::tui::Theme::Bird(sterna::workbench::plumage::Bird::Amazon);
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    let card = card_rows(&screen, "What should we build?");
    assert!(
        card.iter()
            .any(|row| row.contains('▀') || row.contains('▄')),
        "the parrot's head is on the card:\n{}",
        card.join("\n")
    );
    assert!(!card.iter().any(|row| braille(row)), "{}", card.join("\n"));

    s.theme = sterna::tui::Theme::default();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    let card = card_rows(&screen, "What should we build?");
    for row in &card {
        assert!(
            !braille(row) && !row.contains('▀') && !row.contains('▄'),
            "a bird on a classic card:\n{}",
            card.join("\n")
        );
    }
}

/// The parrot is decoration: reduced motion holds its head still.
#[test]
fn the_parrot_holds_still_under_reduced_motion() {
    let (c, n, mut s) = fixture();
    s.truecolor = true;
    s.theme = sterna::tui::Theme::Bird(sterna::workbench::plumage::Bird::Amazon);
    s.reduced_motion = true;
    let mut u = Workbench::default();
    let a = text(&draw(&c, &n, &s, &mut u, 100, 40));
    s.animation_frame = 23;
    assert_eq!(a, text(&draw(&c, &n, &s, &mut u, 100, 40)));
}

/// The answer is shown once. A cell whose only work was `answer(...)` is
/// folded by default, its program view folds the answer's string literal
/// to its opening words, and a returned value that is the answer is not
/// printed again inside the card as a result line.
#[test]
fn the_answer_is_shown_once_under_the_card_and_not_again_inside_it() {
    let (mut c, mut n, s) = fixture();
    let answer = "The motion guard is fixed.\nNothing else changed.";
    c.messages[1].content = vec![Block::ToolUse {
        id: "call-1".into(),
        name: "execute_cell".into(),
        input: serde_json::json!({"code": format!("todo.write([]);\nanswer(\"{}\");", answer.replace('\n', "\\n"))}),
    }];
    n.cells[0].returned = Some(answer.into());
    n.cells[0].output = Some(answer.into());
    n.cells[0].executed_source = None;
    n.cells[0].call_count = Some(0);
    n.cells[0].changes = None;
    n.cells[0].execution = None;
    n.cells[0].helpers.clear();
    let d = doc(&c, &n, &s, &Workbench::default());
    let text = words(&d);
    assert_eq!(
        text.matches("The motion guard is fixed.").count(),
        1,
        "shown once:\n{text}"
    );
    assert!(
        d.rows.iter().any(|r| matches!(
            r.kind,
            sterna::workbench::RowKind::CardTop { open: false, .. }
        )),
        "an answer-only cell is folded:\n{text}"
    );
    // Opened by hand, the program folds the literal rather than repeating it.
    let mut u = Workbench::default();
    u.expanded.insert(1);
    let text = words(&doc(&c, &n, &s, &u));
    assert!(text.contains("the answer is below"), "{text}");
    assert_eq!(text.matches("Nothing else changed.").count(), 1, "{text}");
}

/// Cells that differ between two frames.
fn changed(a: &Buffer, b: &Buffer) -> usize {
    a.content
        .iter()
        .zip(&b.content)
        .filter(|(a, b)| a != b)
        .count()
}

/// A classic theme carries no bird: no face and no nest. `/bird` is gone --
/// the parrots are themes.
#[test]
fn a_classic_theme_has_no_bird_and_bird_is_no_command() {
    let (_, n, mut s) = fixture();
    let c = Conversation::default();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    for bird in ["nest", "bird", "⡔", "⠤⠤⠤"] {
        assert!(
            !screen.contains(bird),
            "{bird} in a classic theme:\n{screen}"
        );
    }
    assert!(
        !u.local_command("/bird", &mut s, &n),
        "/bird is not a workbench command any more"
    );
}

/// An idle screen is still between frames but for the heartbeat's one
/// cell, and fully still with motion off.
#[test]
fn an_idle_screen_moves_one_cell_at_most_and_none_when_off() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Idle;
    let mut u = Workbench::default();
    let a = draw(&c, &n, &s, &mut u, 100, 40);
    s.animation_frame = 1;
    let b = draw(&c, &n, &s, &mut u, 100, 40);
    assert_eq!(changed(&a, &b), 1, "the heartbeat is one cell");
    s.set_motion(sterna::tui::Motion::Off);
    let a = draw(&c, &n, &s, &mut u, 100, 40);
    s.animation_frame = 2;
    assert_eq!(changed(&a, &draw(&c, &n, &s, &mut u, 100, 40)), 0);
}

/// Prose still arriving -- the model's thinking -- is marked live by a
/// rail and ends in a caret that breathes; motion off holds the caret.
#[test]
fn arriving_prose_carries_a_rail_and_a_moving_caret() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Streaming;
    s.streaming_text = Some("Reading the guard first.".into());
    let mut u = Workbench::default();
    let a = draw(&c, &n, &s, &mut u, 100, 40);
    let line = text(&a)
        .lines()
        .find(|l| l.contains("Reading the guard first."))
        .unwrap()
        .to_string();
    assert!(line.contains("▎ Reading the guard first.▍"), "{line}");
    s.animation_frame = 1;
    let b = draw(&c, &n, &s, &mut u, 100, 40);
    assert!(changed(&a, &b) <= 3, "{}", changed(&a, &b));
    assert!(!text(&b).contains("first.▍"), "the caret breathes");
    s.set_motion(sterna::tui::Motion::Off);
    let a = text(&draw(&c, &n, &s, &mut u, 100, 40));
    s.animation_frame = 2;
    assert_eq!(a, text(&draw(&c, &n, &s, &mut u, 100, 40)));
}

/// Reasoning while it arrives is one muted line: how much has come and its
/// newest sentence, the rest of it never printed.
#[test]
fn arriving_reasoning_shows_its_size_and_newest_sentence_on_one_line() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Thinking;
    s.streaming_reasoning = Some(format!(
        "**Checking the guard**\n\n{}. Then the mode proposal is wrong.",
        "The four tests call propose ".repeat(20)
    ));
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    let line = screen
        .lines()
        .find(|l| l.contains("reasoning ·"))
        .unwrap_or_else(|| panic!("{screen}"))
        .to_string();
    assert!(line.contains("~1") && line.contains("tok ·"), "{line}");
    assert!(line.contains("Then the mode proposal is wrong"), "{line}");
    assert!(!screen.contains("Checking the guard"), "{screen}");
    assert_eq!(screen.matches("The four tests call").count(), 0, "{screen}");
}

/// A check behind the answer shows while it runs, then its verdict lands
/// emphasised for a few frames and settles to its own colour.
#[test]
fn a_check_behind_the_answer_runs_visibly_then_settles_into_its_verdict() {
    let (c, n, mut s) = fixture();
    s.lane("check", true);
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    assert!(screen.contains("checking the answer"), "{screen}");
    s.note(format!("{}holds", sterna::tui::history::CHECKED));
    s.landed_note();
    s.lane("check", false);
    let d = doc(&c, &n, &s, &u);
    let verdict = d
        .rows
        .iter()
        .find(|r| r.text.contains("checked after the answer: holds"))
        .expect("the verdict is in the transcript");
    assert_eq!(verdict.spans[0].1, Tone::Accent, "it lands emphasised");
    assert!(
        !words(&d).contains("checking the answer"),
        "the working row went"
    );
    for _ in 0..10 {
        s.advance_landing();
    }
    let d = doc(&c, &n, &s, &u);
    let verdict = d
        .rows
        .iter()
        .find(|r| r.text.contains("checked after the answer: holds"))
        .unwrap();
    assert_eq!(verdict.spans[0].1, Tone::Success, "then it settles");
}

/// A running cell under the instrument: a label and a scanner in place of
/// the bird, moving at most two cells of its own a frame; motion off holds it.
#[test]
fn a_running_cell_scans_instead_of_pecking() {
    let (mut c, mut n, mut s) = fixture();
    c.messages.push(Message::text(Role::User, "and again"));
    let mut m = Message::text(Role::Assistant, "Once more.");
    m.content.push(Block::ToolUse {
        id: "call-2".into(),
        name: "execute_cell".into(),
        input: serde_json::json!({"code": "await checks.run(\"tests\");"}),
    });
    c.messages.push(m);
    n.cells[0].execution = Some("checks.run · completed".into());
    s.activity = Activity::Executing;
    let mut u = Workbench::default();
    let a = draw(&c, &n, &s, &mut u, 100, 40);
    let screen = text(&a);
    assert!(screen.contains("◆ Executing this cell"), "{screen}");
    assert!(screen.contains("━━"), "{screen}");
    assert!(!screen.contains("⡔"), "no bird:\n{screen}");
    s.animation_frame = 1;
    let b = draw(&c, &n, &s, &mut u, 100, 40);
    // Two for the scanner, one for the dock's mark.
    assert!(changed(&a, &b) <= 3, "{}", changed(&a, &b));
    s.set_motion(sterna::tui::Motion::Off);
    let a = text(&draw(&c, &n, &s, &mut u, 100, 40));
    s.animation_frame = 5;
    assert_eq!(a, text(&draw(&c, &n, &s, &mut u, 100, 40)));
}

/// `/motion` takes the three levels and says what each one does.
#[test]
fn motion_takes_three_levels() {
    let (_, n, mut s) = fixture();
    let mut u = Workbench::default();
    for (word, level) in [
        ("calm", sterna::tui::Motion::Calm),
        ("off", sterna::tui::Motion::Off),
        ("full", sterna::tui::Motion::Full),
    ] {
        assert!(u.local_command(&format!("/motion {word}"), &mut s, &n));
        assert_eq!(s.motion, level);
        assert_eq!(s.reduced_motion, level == sterna::tui::Motion::Off);
    }
    assert!(u.local_command("/motion sideways", &mut s, &n));
    assert_eq!(s.motion, sterna::tui::Motion::Full);
}

/// Several things can be live at once -- reasoning, a cell being written,
/// work behind the answer -- and a spinner on each stacked them on screen.
/// Only the newest moves; between two frames, only its row changes.
#[test]
fn only_the_newest_live_row_moves() {
    let (c, n, mut s) = fixture();
    s.streaming_reasoning = Some("Looking at the tests first.".into());
    s.behind = vec!["checker".into()];
    s.streaming_tool_input = Some("{\"code\":\"await read({path: \\\"a.rs\\\"});".into());
    s.activity = sterna::tui::Activity::Streaming;
    let rows = |s: &ScreenState| -> Vec<String> {
        doc(&c, &n, s, &Workbench::default())
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect()
    };
    let mut changed = std::collections::BTreeSet::new();
    let first = rows(&s);
    for frame in 1..8 {
        s.animation_frame = frame;
        for (a, b) in first.iter().zip(rows(&s)) {
            if *a != b {
                changed.insert(a.clone());
            }
        }
    }
    assert!(!changed.is_empty(), "the newest live row moves");
    assert!(
        changed
            .iter()
            .all(|row| row.contains("writing cell") || row.contains("read ")),
        "only the cell being written moves: {changed:?}"
    );
}

/// A first screen says only what the header cannot: no model line, no
/// tagline, no list of commands the `/` hint already leads to.
#[test]
fn the_opening_card_repeats_nothing_the_header_says() {
    let (_, n, s) = fixture();
    let empty = Conversation::default();
    let text = words(&doc(&empty, &n, &s, &Workbench::default()));
    for gone in [
        "/model changes it",
        "code · cells",
        "/settings  ",
        "test-project",
    ] {
        assert!(!text.contains(gone), "{gone:?} is on the opening: {text}");
    }
    assert!(text.contains("What should we build?"), "{text}");
}

/// A cell's tabs are the ones with something behind them, and the model's
/// sentence that titles the card is not also printed above it.
#[test]
fn a_cell_offers_only_the_tabs_it_can_fill_and_says_its_title_once() {
    let (c, mut n, s) = fixture();
    let full = words(&doc(&c, &n, &s, &Workbench::default()));
    assert!(
        full.contains("Changes +1 −1") && full.contains("⟨ Helpers ⟩"),
        "{full}"
    );
    n.cells[0].changes = None;
    n.cells[0].helpers.clear();
    n.cells[0].description = Some("I will inspect the motion guard before changing it.".into());
    let text = words(&doc(&c, &n, &s, &Workbench::default()));
    assert!(text.contains("⟨ Cell program ⟩"), "{text}");
    for gone in ["Changes", "⟨ Helpers ⟩", "open diff"] {
        assert!(
            !text.contains(gone),
            "{gone:?} with nothing behind it: {text}"
        );
    }
    assert_eq!(
        text.matches("I will inspect the motion guard").count(),
        1,
        "{text}"
    );
}

/// A failure is said in the transcript and by the dock's own "failed"; it
/// does not ride the dock a third time.
#[test]
fn a_failure_notice_does_not_ride_the_dock() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Failed;
    s.notice = Some("ERROR: Stopped before the work was confirmed done".into());
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    let dock = screen.lines().find(|l| l.starts_with("╭─")).unwrap();
    assert!(!dock.contains("Stopped before"), "{dock}");
}

/// A bird theme perches its bird on the opening card, in its own colours,
/// where the terminal shows true colour; elsewhere the outline bird stands in.
#[test]
fn a_bird_theme_perches_its_bird_in_colour_on_the_opening_card() {
    use sterna::workbench::plumage::Bird;
    let (_, n, mut s) = fixture();
    let empty = Conversation::default();
    s.theme = Theme::Bird(Bird::Cockatoo);
    s.truecolor = true;
    let mut u = Workbench::default();
    let screen = draw(&empty, &n, &s, &mut u, 100, 40);
    let crest = Color::Rgb(0xf7, 0xd2, 0x3a);
    let painted = (0..screen.area.height)
        .flat_map(|y| (0..screen.area.width).map(move |x| (x, y)))
        .filter(|&(x, y)| screen[(x, y)].fg == crest && screen[(x, y)].symbol() != " ")
        .count();
    assert!(
        painted >= 5,
        "the cockatoo's crest is not on the card: {painted} cells"
    );
    assert!(
        text(&screen).contains("Sulphur-crested Cockatoo"),
        "{}",
        text(&screen)
    );
    // Without true colour: no sprite, and the name is not claimed beside one.
    s.truecolor = false;
    let screen = draw(&empty, &n, &s, &mut u, 100, 40);
    assert!(!text(&screen).contains("▀▀▀▀"), "{}", text(&screen));
}

/// `/theme` is a sheet: every palette listed, the chosen bird previewed by
/// its name and its Latin one.
#[test]
fn the_theme_sheet_previews_the_chosen_bird() {
    use sterna::workbench::plumage::Bird;
    let (c, n, mut s) = fixture();
    s.truecolor = true;
    s.theme = Theme::Bird(Bird::Hyacinth);
    let mut u = Workbench::default();
    u.open(Source::Themes { before: s.theme });
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    for shown in [
        "neon",
        "Sun Conure",
        "Sulphur-crested Cockatoo",
        "Anodorhynchus hyacinthinus",
        "cracks the hard ones",
    ] {
        assert!(screen.contains(shown), "{shown} is missing:\n{screen}");
    }
}

/// **Themes come in families**: the sheet heads each one -- Classic, then
/// Parrots -- with its themes under it, and choosing still steps from theme
/// to theme, never onto a heading.
#[test]
fn the_theme_sheet_groups_themes_under_their_family() {
    use sterna::workbench::plumage::Bird;
    let (c, n, mut s) = fixture();
    s.theme = Theme::Rose;
    let mut u = Workbench::default();
    u.open(Source::Themes { before: s.theme });
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 50));
    let line = |needle: &str| {
        screen
            .lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is missing:\n{screen}"))
    };
    let (classic, parrots) = (line("CLASSIC · "), line("PARROTS · "));
    assert!(classic < line("neon") && line("rose") < parrots, "{screen}");
    assert!(parrots < line("Amazon") && parrots < line("Sulphur-crested Cockatoo"));
    // The last classic theme steps straight onto the first parrot, over
    // the heading between them.
    key(&mut u, &mut s, &n, KeyCode::Down);
    let focused = u.top().unwrap().sheet.focused().unwrap().id.clone();
    assert_eq!(
        focused,
        format!("theme:{}", Theme::Bird(Bird::Amazon).name())
    );
}

/// **A key pasted into a form is bullets on the screen, never the key**, and
/// the sheet says where the paste goes and what the key looks like.
#[test]
fn a_key_form_shows_bullets_where_the_paste_went_and_never_the_key() {
    use sterna::tui::form::{Field, Form, Kind, key_shape};
    const KEY: &str = "sk-ant-api03-secret-value"; // glasshouse:not-a-secret
    let mut form = Form::new(
        "Sign in › API key · anthropic",
        "Paste your anthropic key below.",
        vec![Field::new("API key", Kind::Secret, "paste here").checked(key_shape)],
    );
    form.push(KEY);
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| {
        workbench::render_form(f, &form, Theme::default());
    })
    .unwrap();
    let screen = text(t.backend().buffer());
    assert!(
        screen.contains(&"•".repeat(KEY.chars().count())),
        "{screen}"
    );
    assert!(!screen.contains("sk-ant"), "the key rendered:\n{screen}");
    assert!(
        screen.contains("API KEY"),
        "the field is labelled:\n{screen}"
    );
    assert!(screen.contains("looks like an Anthropic key"), "{screen}");
    assert!(
        screen.contains("Ctrl-R show"),
        "the keys say how to see it:\n{screen}"
    );
}

/// **A form's warning is on the sheet before the key goes in**, in full.
#[test]
fn a_key_form_carries_its_warning_on_the_sheet() {
    use sterna::tui::form::{Field, Form, Kind};
    let form = Form::new(
        "Sign in › API key · gemini-openai",
        "Paste your gemini-openai key below.",
        vec![Field::new("API key", Kind::Secret, "paste here")],
    )
    .warn("Google's terms do not allow a subscription here.");
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| {
        workbench::render_form(f, &form, Theme::default());
    })
    .unwrap();
    let screen = text(t.backend().buffer());
    assert!(
        screen.contains("⚠ Google's terms do not allow a subscription here."),
        "{screen}"
    );
}

/// On the Subagents section the favourite slots are rows: ↓ walks them and
/// Enter picks where a model goes, so choosing a slot needs no function key.
#[test]
fn the_favourite_slots_are_rows_on_the_subagents_section() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    let mut m = navigator();
    m.role = 2;
    u.open(Source::Models(Box::new(m)));
    draw(&c, &n, &s, &mut u, 120, 40);
    // The sheet opens on the pinned row, the one the session is on.
    assert_eq!(u.top().unwrap().sheet.focused().unwrap().id, "slot:pinned");
    key(&mut u, &mut s, &n, KeyCode::Down);
    key(&mut u, &mut s, &n, KeyCode::Enter);
    assert_eq!(u.models().unwrap().slot.as_deref(), Some("quick"));
}

/// **Every sheet speaks one grammar** (the interaction model in
/// `docs/audit/tui-audit.md`). For each opener: the sheet opens on the value
/// the session is on; Space does what Enter does; the wheel scrolls without
/// moving focus; End and Home reach the last and first rows that act; and
/// Esc on a child goes back to the parent with focus on the row that opened
/// it.
#[test]
fn every_sheet_speaks_one_grammar() {
    use sterna::tui::PanelRow;
    type Opener = fn(&mut Workbench, &mut ScreenState, &Notebook, &Temp);
    let openers: [(&str, Opener); 5] = [
        ("sandbox", |u, s, n, _| {
            s.level = sterna::permissions::LiveLevel::new(sterna::permissions::Level::Ask);
            let (c, _, _) = fixture();
            draw(&c, n, s, u, 120, 40);
            click(u, s, n, Action::Sandbox);
        }),
        ("themes", |u, s, _, _| {
            s.theme = Theme::Rose;
            u.open(Source::Themes { before: s.theme });
        }),
        ("wizard", |u, s, _, _| {
            s.panel = Some(Panel::rows(
                "Setup · 0 of 3 done",
                vec![
                    PanelRow::opens("○ 1 Sign in · a subscription or an API key", "/login"),
                    PanelRow::opens(
                        "○ 2 Models for each workload · after you sign in",
                        "/wizard models",
                    ),
                    PanelRow::opens("○ 3 Jev, the decision model · needs a key", "/wizard jev"),
                ],
            ));
            u.absorb_panel(s);
        }),
        ("keys", |u, s, n, _| {
            let (c, _, _) = fixture();
            draw(&c, n, s, u, 120, 40);
            click(u, s, n, Action::Help);
        }),
        ("settings", |u, s, _, t| {
            s.settings_root = Some(t.0.clone());
            let p = Preferences::with_global(s, Some(t.0.join("user"))).unwrap();
            u.open(Source::Settings(Box::new(p)));
        }),
    ];
    let fresh = |open: Opener| {
        let (c, n, mut s) = fixture();
        let t = Temp::new();
        let mut u = Workbench::default();
        open(&mut u, &mut s, &n, &t);
        draw(&c, &n, &s, &mut u, 120, 40);
        (c, n, s, u, t)
    };
    for (name, open) in openers {
        let (c, n, mut s, mut u, _t) = fresh(open);
        let sheet = &u
            .top()
            .unwrap_or_else(|| panic!("{name}: nothing opened"))
            .sheet;
        let items = &sheet.items;
        // (a) The current value, else the first row that acts.
        let expected = items
            .iter()
            .position(|item| item.focusable() && item.is_current())
            .or_else(|| {
                items.iter().position(|item| {
                    item.focusable() && item.kind != sterna::workbench::ItemKind::Danger
                })
            })
            .unwrap();
        assert_eq!(sheet.focus, expected, "{name}: opened on the wrong row");
        let focused = sheet.focused().unwrap().id.clone();
        // (b) Space is Enter.
        let (_, n1, mut s1, mut u1, _t1) = fresh(open);
        let (_, n2, mut s2, mut u2, _t2) = fresh(open);
        let enter = key(&mut u1, &mut s1, &n1, KeyCode::Enter);
        let space = key(&mut u2, &mut s2, &n2, KeyCode::Char(' '));
        assert_eq!(enter, space, "{name}: Space did not do what Enter does");
        assert_eq!(u1.sheets.len(), u2.sheets.len(), "{name}");
        // (c) The wheel scrolls; focus stays.
        mouse(&mut u, &mut s, &n, MouseEventKind::ScrollDown, 60, 20);
        draw(&c, &n, &s, &mut u, 120, 40);
        assert_eq!(
            u.top().unwrap().sheet.focused().unwrap().id,
            focused,
            "{name}: the wheel moved focus"
        );
        // (d) End and Home reach the last and the first rows that act.
        key(&mut u, &mut s, &n, KeyCode::End);
        let sheet = &u.top().unwrap().sheet;
        let last = sheet
            .items
            .iter()
            .rposition(|item| item.focusable())
            .unwrap();
        assert_eq!(sheet.focus, last, "{name}: End");
        key(&mut u, &mut s, &n, KeyCode::Home);
        let sheet = &u.top().unwrap().sheet;
        let first = sheet
            .items
            .iter()
            .position(|item| item.focusable())
            .unwrap();
        assert_eq!(sheet.focus, first, "{name}: Home");
    }
    // (e) Esc on a child goes back to its parent, on the row that opened it.
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    draw(&c, &n, &s, &mut u, 120, 40);
    click_item(&mut u, &mut s, &n, "level:full");
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    assert_eq!(
        u.sheets.len(),
        2,
        "the confirmation opens as the Sandbox's child"
    );
    draw(&c, &n, &s, &mut u, 120, 40);
    armed(&mut u);
    key(&mut u, &mut s, &n, KeyCode::Esc);
    assert!(u.showing(|source| matches!(source, Source::Sandbox)));
    assert_eq!(u.top().unwrap().sheet.focused().unwrap().id, "level:full");
    key(&mut u, &mut s, &n, KeyCode::Esc);
    assert!(u.sheets.is_empty(), "Esc at the root closes");
}

/// A panel the session sends opens as a sheet over the conversation, and
/// nothing of the transcript behind it shows through.
#[test]
fn a_session_panel_opens_as_a_sheet_that_erases_the_transcript() {
    let (mut c, n, mut s) = fixture();
    c.messages
        .push(Message::text(Role::User, "STALE_TRANSCRIPT ".repeat(60)));
    s.panel = Some(Panel::text("Models", "one\ntwo"));
    let mut u = Workbench::default();
    u.absorb_panel(&mut s);
    let shown = text(&draw(&c, &n, &s, &mut u, 200, 40));
    assert!(shown.contains("MODELS"), "{shown}");
    assert!(!shown.contains("STALE_TRANSCRIPT"), "{shown}");
}

/// A panel the session sends again under the same title -- a sign-in that
/// redraws while it waits -- replaces itself and keeps the row a person is
/// on, even when that row's text changed.
#[test]
fn a_redrawn_panel_keeps_the_focused_row() {
    use sterna::tui::PanelRow;
    let (c, n, mut s) = fixture();
    let rows = |runs: usize| {
        vec![
            PanelRow::info("Task-scoped"),
            PanelRow::run(
                format!("first · {runs} runs"),
                Action::HandlerOff("first".into()),
            )
            .with_id("handler:first"),
            PanelRow::run(
                format!("second · {runs} runs"),
                Action::HandlerOff("second".into()),
            )
            .with_id("handler:second"),
        ]
    };
    s.panel = Some(Panel::rows("Standing handlers", rows(0)));
    let mut u = Workbench::default();
    u.absorb_panel(&mut s);
    draw(&c, &n, &s, &mut u, 120, 40);
    key(&mut u, &mut s, &n, KeyCode::Down);
    assert_eq!(
        u.top().unwrap().sheet.focused().unwrap().id,
        "handler:second"
    );
    s.panel = Some(Panel::rows("Standing handlers", rows(3)));
    u.absorb_panel(&mut s);
    draw(&c, &n, &s, &mut u, 120, 40);
    assert_eq!(u.sheets.len(), 1, "the same panel replaced itself");
    assert_eq!(
        u.top().unwrap().sheet.focused().unwrap().id,
        "handler:second"
    );
}

/// The keys sheet is drawn from the one keymap table: every key in it is on
/// the sheet, and a key that opens something is a row that opens it.
#[test]
fn the_keys_sheet_is_the_keymap() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Keys);
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 50));
    for (key, what, _) in workbench::keymap() {
        assert!(screen.contains(key), "{key} is missing:\n{screen}");
        assert!(screen.contains(what), "{what} is missing:\n{screen}");
    }
    let acting = workbench::keymap()
        .into_iter()
        .filter(|(_, _, action)| action.is_some())
        .count();
    let rows = u
        .top()
        .unwrap()
        .sheet
        .items
        .iter()
        .filter(|i| i.focusable())
        .count();
    assert_eq!(
        rows, acting,
        "every key that does something is a row that does it"
    );
}

/// **A form takes the mouse**: every field is a target, each word of a
/// choice is one, and the submit and back chips say what Enter and Esc do.
#[test]
fn a_form_is_clickable_field_by_field() {
    use sterna::tui::form::{Field, Form, Kind};
    let mut form = Form::new(
        "Sign in › Custom endpoint",
        "Any OpenAI- or Anthropic-compatible URL.",
        vec![
            Field::new("Address", Kind::Text, "https://…"),
            Field::new(
                "Protocol",
                Kind::Choice(vec!["openai".into(), "anthropic".into()]),
                "",
            ),
        ],
    );
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let mut hits = Vec::new();
    t.draw(|f| hits = workbench::render_form(f, &form, Theme::default()))
        .unwrap();
    let has = |hit: workbench::FormHit| hits.iter().any(|(_, h)| *h == hit);
    assert!(has(workbench::FormHit::Field(0)) && has(workbench::FormHit::Field(1)));
    assert!(has(workbench::FormHit::Word(1, 1)));
    assert!(has(workbench::FormHit::Submit) && has(workbench::FormHit::Back));
    form.choose_word(1, 1);
    assert_eq!(form.focus, 1);
    assert_eq!(form.take()[1], "anthropic");
}

/// **Hover owes a frame only when the target under the pointer changes**,
/// and it never moves focus.
#[test]
fn hover_redraws_only_when_the_target_changes() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    draw(&c, &n, &s, &mut u, 120, 40);
    let focused = u.top().unwrap().sheet.focused().unwrap().id.clone();
    let rects: Vec<_> = u
        .geometry
        .hits
        .iter()
        .filter(|(_, a)| matches!(a, Action::Sheet(Hit::Item(_))))
        .map(|(r, _)| *r)
        .collect();
    let (a, b) = (rects[0], rects[rects.len() - 1]);
    assert_eq!(
        mouse(&mut u, &mut s, &n, MouseEventKind::Moved, a.x + 1, a.y),
        Effect::Consumed
    );
    assert_eq!(
        mouse(&mut u, &mut s, &n, MouseEventKind::Moved, a.x + 2, a.y),
        Effect::Ignored,
        "a move within one target owes no frame"
    );
    assert_eq!(
        mouse(&mut u, &mut s, &n, MouseEventKind::Moved, b.x + 1, b.y),
        Effect::Consumed
    );
    assert_eq!(u.top().unwrap().sheet.focused().unwrap().id, focused);
}

/// The session card names the level in force now -- after the Sandbox
/// sheet or a typed command -- never the one the session started on, and
/// the line is the way to the Sandbox sheet.
#[test]
fn the_greeting_names_the_level_in_force() {
    use sterna::permissions::{Level, LiveLevel};
    let (c, n, mut s) = fixture();
    s.note("session tlqdct-yqr — resume it with:  sterna --resume tlqdct-yqr");
    s.startup_notes = Some(1);
    s.level = LiveLevel::new(Level::Full);
    s.level.set(Level::Ask);
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 42));
    let (y, card) = screen
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("Sandbox: "))
        .unwrap_or_else(|| panic!("no sandbox line on the card:\n{screen}"));
    assert!(
        card.contains(Level::Ask.label()) && card.contains(Level::Ask.asks()),
        "{card}"
    );
    assert!(!screen.contains(Level::Full.label()), "{screen}");
    assert!(
        u.geometry
            .hits
            .iter()
            .any(|(r, a)| *a == Action::Sandbox && r.y as usize == y),
        "the line opens the Sandbox sheet"
    );
}

/// The effort chip is a control stepped in place, so it stays on the dock
/// at `default` and says what a model is actually sent.
#[test]
fn the_effort_chip_stays_on_the_dock_at_default() {
    let (c, n, mut s) = fixture();
    s.effort = sterna::wire::Effort::Default;
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 140, 42);
    assert!(
        u.geometry
            .hits
            .iter()
            .any(|(r, a)| *a == Action::Effort && r.y == 41),
        "an effort chip on the dock at default"
    );
    s.model = Some("gpt-5.5".into());
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 42));
    assert!(screen.contains("effort default (low)"), "{screen}");
    // The ladder steps from the stored value, so the first step is a change.
    assert_eq!(
        click(&mut u, &mut s, &n, Action::Effort),
        Effect::Command("/effort low".into())
    );
}

/// The level has one setter: the Sandbox sheet, a typed command and a
/// settings row all reach it, each saves it globally, and each prints the
/// same notice. Full access is confirmed first on every route.
#[test]
fn every_route_to_the_level_saves_it_and_says_the_same() {
    use sterna::permissions::Level;
    let (t, mut s, p) = prefs();
    let (c, n, _) = fixture();
    let global = sterna::settings::Store::with_global(&t.0, Some(t.0.join("user")))
        .unwrap()
        .path(sterna::settings::Scope::Global);
    let saved = || std::fs::read_to_string(&global).unwrap_or_default();
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    draw(&c, &n, &s, &mut u, 100, 40);
    click_item(&mut u, &mut s, &n, "level:ask");
    assert_eq!(s.level.level(), Level::Ask);
    assert_eq!(u.top().unwrap().sheet.notice, Level::Ask.now());
    assert!(
        saved().contains("level = \"ask\""),
        "saved globally: {}",
        saved()
    );

    let mut u = Workbench::default();
    assert!(u.local_command("/sandbox sandboxed", &mut s, &n));
    assert_eq!(s.level.level(), Level::Sandboxed);
    assert_eq!(u.notice, Level::Sandboxed.now());
    assert!(saved().contains("level = \"sandboxed\""), "{}", saved());
    assert!(u.local_command("/sandbox full access", &mut s, &n));
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    assert_eq!(s.level.level(), Level::Sandboxed);

    let row = p
        .rows()
        .iter()
        .position(|spec| spec.key == "sandbox.level")
        .unwrap();
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    draw(&c, &n, &s, &mut u, 110, 40);
    let item = u
        .top()
        .unwrap()
        .sheet
        .items
        .iter()
        .position(|item| item.id == "setting:sandbox.level")
        .unwrap();
    assert!(row < item);
    // Ask, Sandboxed, Full access: the third chip.
    let chips = u.top().unwrap().sheet.items[item].clone();
    assert!(format!("{chips:?}").contains("Full access"), "{chips:?}");
    click(&mut u, &mut s, &n, Action::Sheet(Hit::Value(item, 2)));
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    assert_eq!(s.level.level(), Level::Sandboxed);
    assert!(!saved().contains("\"full\""), "{}", saved());
    draw(&c, &n, &s, &mut u, 110, 40);
    armed(&mut u);
    key(&mut u, &mut s, &n, KeyCode::Esc);
    assert!(u.showing(|source| matches!(source, Source::Settings(_))));
    draw(&c, &n, &s, &mut u, 110, 40);
    click(&mut u, &mut s, &n, Action::Sheet(Hit::Value(item, 0)));
    assert_eq!(s.level.level(), Level::Ask);
    assert!(saved().contains("level = \"ask\""), "{}", saved());
}

/// Pinned and favourites name a model. Chosen with none named, each opens
/// the surface where one is named, and nothing is saved that the settings
/// file would refuse -- no "[agents] mode = pinned requires `model`".
#[test]
fn subagent_modes_without_a_model_open_where_one_is_chosen() {
    let (c, n, _) = fixture();
    // Off, pinned, favourites: the second and third chips.
    for (chip, command, browsing) in [
        (1, "/models", Some("agents.model")),
        (2, "/subagents", None),
    ] {
        let (_t, mut s, mut p) = prefs();
        p.category = 4;
        let mut u = Workbench::default();
        u.open(Source::Settings(Box::new(p)));
        draw(&c, &n, &s, &mut u, 110, 40);
        let item = u
            .top()
            .unwrap()
            .sheet
            .items
            .iter()
            .position(|item| item.id == "setting:agents.mode")
            .unwrap();
        let effect = click(&mut u, &mut s, &n, Action::Sheet(Hit::Value(item, chip)));
        assert_eq!(effect, Effect::Command(command.into()), "chip {chip}");
        assert_eq!(u.browsing.as_deref(), browsing, "chip {chip}");
        let p = u.preferences().unwrap();
        assert_eq!(
            p.loaded.config.agents.mode,
            sterna::config::AgentsMode::Off,
            "chip {chip}: nothing is saved yet"
        );
        let notice = &u.top().unwrap().sheet.notice;
        assert!(
            !notice.contains("requires") && !notice.contains("configure"),
            "{notice}"
        );
    }
}

/// An effort command on its way to the session is saved once, whichever
/// route sent it.
#[test]
fn an_effort_on_its_way_out_is_saved() {
    let (t, s, _p) = prefs();
    let global = sterna::settings::Store::with_global(&t.0, Some(t.0.join("user")))
        .unwrap()
        .path(sterna::settings::Scope::Global);
    let mut u = Workbench::default();
    u.sent("/effort high", &s);
    let saved = std::fs::read_to_string(&global).unwrap();
    assert!(saved.contains("effort = \"high\""), "{saved}");
}

/// The sidebar's effort and helpers are two targets: stepping the effort
/// never happens from the helpers line, and a helper's own line opens it.
#[test]
fn the_sidebar_splits_effort_from_helpers() {
    let (c, n, mut s) = fixture();
    s.helpers_on = false;
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 140, 42);
    let side = |a: &Action| {
        u.geometry
            .hits
            .iter()
            .filter(|(r, act)| act == a && r.x > 100)
            .map(|(r, _)| r.y)
            .collect::<Vec<_>>()
    };
    let effort = side(&Action::Effort);
    let helpers = side(&Action::SettingsAt(2));
    assert!(!effort.is_empty() && !helpers.is_empty());
    assert!(effort.iter().all(|y| !helpers.contains(y)));
    assert!(
        !side(&Action::Helper(1, 0)).is_empty(),
        "the helper line opens its lane"
    );
}

/// Backspace edits a search and nothing else: with the search empty it does
/// nothing at all, and never unsets the row under the focus.
#[test]
fn backspace_on_an_empty_search_never_resets_a_setting() {
    let (_t, mut s, mut p) = prefs();
    p.save("ui.theme", Some("rose".into()), &mut s).unwrap();
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 40);
    for k in [
        KeyCode::Char('m'),
        KeyCode::Char('o'),
        KeyCode::Backspace,
        KeyCode::Backspace,
        KeyCode::Backspace,
    ] {
        key(&mut u, &mut s, &n, k);
        draw(&c, &n, &s, &mut u, 110, 40);
    }
    assert_eq!(
        u.preferences().unwrap().saved("ui.theme").as_deref(),
        Some("rose")
    );
}

/// Settings shows what the running session uses now: an effort a chip set,
/// never saved, is the lit value, and the row says it is not saved.
#[test]
fn settings_show_what_the_session_is_using_now() {
    let (_t, mut s, p) = prefs();
    assert_eq!(p.scope, sterna::settings::Scope::Global, "decision 6");
    s.effort = sterna::wire::Effort::Medium;
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 40);
    let sheet = &u.top().unwrap().sheet;
    let row = sheet
        .items
        .iter()
        .find(|item| item.id == "setting:session.effort")
        .unwrap();
    let workbench::ItemKind::Value { values, current } = &row.kind else {
        panic!("{row:?}");
    };
    assert_eq!(current.map(|i| values[i].0.as_str()), Some("medium"));
    u.top_mut()
        .unwrap()
        .sheet
        .focus_id("setting:session.effort");
    draw(&c, &n, &s, &mut u, 110, 40);
    let row = u
        .top()
        .unwrap()
        .sheet
        .items
        .iter()
        .find(|item| item.id == "setting:session.effort")
        .unwrap()
        .clone();
    assert!(row.detail.contains("not saved"), "{}", row.detail);
}

/// The sandbox level is global only: in Project scope its row says why it
/// cannot be set, and in Global scope stepping it to Full access is
/// confirmed first.
#[test]
fn the_sandbox_level_is_global_only_and_full_access_is_confirmed() {
    let (_t, mut s, mut p) = prefs();
    p.switch_scope().unwrap();
    assert_eq!(p.scope, sterna::settings::Scope::Local);
    p.category = 0;
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 60);
    let at = |u: &Workbench| {
        u.top()
            .unwrap()
            .sheet
            .items
            .iter()
            .position(|item| item.id == "setting:sandbox.level")
            .unwrap()
    };
    let item = at(&u);
    assert_eq!(
        u.top().unwrap().sheet.items[item].disabled.as_deref(),
        Some("Global only · F6 switches to Global")
    );
    click(&mut u, &mut s, &n, Action::Sheet(Hit::Tool(0)));
    draw(&c, &n, &s, &mut u, 110, 60);
    let item = at(&u);
    assert!(u.top().unwrap().sheet.items[item].disabled.is_none());
    // Ask, Sandboxed, Full access: the next value from the default.
    u.top_mut().unwrap().sheet.focus_id("setting:sandbox.level");
    draw(&c, &n, &s, &mut u, 110, 60);
    key(&mut u, &mut s, &n, KeyCode::Right);
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    assert_eq!(s.level.level(), sterna::permissions::Level::Sandboxed);
    assert!(
        u.preferences().unwrap().saved("sandbox.level").is_none(),
        "nothing is saved before the confirmation"
    );
}

/// `/settings <word>` opens on the row the word names; a word that names
/// nothing still opens Settings, and says so.
#[test]
fn a_setting_named_after_the_command_is_where_settings_opens() {
    let (_t, mut s, _p) = prefs();
    let n = Notebook::default();
    let mut u = Workbench::default();
    assert!(u.local_command("/settings theme", &mut s, &n));
    let p = u.preferences().unwrap();
    assert_eq!(p.category, 1, "Display");
    assert_eq!(
        u.top().unwrap().sheet.prefer.as_deref(),
        Some("setting:ui.theme")
    );
    assert!(u.local_command("/config nothing-here", &mut s, &n));
    assert!(u.preferences().is_some());
    assert_eq!(
        u.top().unwrap().sheet.notice,
        "No setting named nothing-here · opened Settings"
    );
    assert!(u.local_command("/motion", &mut s, &n));
    assert_eq!(
        u.top().unwrap().sheet.prefer.as_deref(),
        Some("setting:ui.motion")
    );
}

/// A field opens with its value selected, so a paste replaces it, and a
/// pasted list becomes one entry a line.
#[test]
fn a_pasted_list_replaces_the_field_one_entry_a_line() {
    let (_t, mut s, mut p) = prefs();
    p.save("web.allow_domains", Some("old.example".into()), &mut s)
        .unwrap();
    p.category = 5;
    let row = p
        .rows()
        .iter()
        .position(|spec| spec.key == "web.allow_domains")
        .unwrap();
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 60);
    assert!(row > 0);
    u.top_mut()
        .unwrap()
        .sheet
        .focus_id("setting:web.allow_domains");
    key(&mut u, &mut s, &n, KeyCode::Enter);
    draw(&c, &n, &s, &mut u, 110, 60);
    u.event(
        &Event::Paste("a.example\n\n  b.example \n".into()),
        &mut s,
        &n,
        false,
    );
    draw(&c, &n, &s, &mut u, 110, 60);
    let item = u
        .top()
        .unwrap()
        .sheet
        .items
        .iter()
        .find(|item| item.id == "setting:web.allow_domains")
        .unwrap()
        .clone();
    let workbench::ItemKind::Field(field) = &item.kind else {
        panic!("{item:?}");
    };
    assert_eq!(field.text, "a.example, b.example");
}

/// Advanced holds only what no other category does, and nothing Sterna
/// writes for itself; the thresholds are their own section.
#[test]
fn advanced_repeats_nothing_and_offers_no_bookkeeping() {
    let (_t, _, mut p) = prefs();
    let mut elsewhere = std::collections::BTreeSet::new();
    for category in [0, 1, 2, 3, 4, 6] {
        p.category = category;
        elsewhere.extend(p.rows().iter().map(|spec| spec.key));
    }
    p.category = 5;
    let advanced: Vec<_> = p.rows().iter().map(|spec| spec.key).collect();
    for key in &advanced {
        assert!(!elsewhere.contains(key), "{key} is repeated");
    }
    for key in ["wizard.seen", "legacy.imported", "limits.task_tokens"] {
        assert!(
            !advanced.contains(&key) && !elsewhere.contains(key),
            "{key}"
        );
    }
    p.category = 6;
    assert!(
        p.rows()
            .iter()
            .all(|spec| spec.kind == sterna::settings::Kind::Float)
    );
    assert!(
        p.rows()
            .iter()
            .any(|spec| spec.key == "decisions.hold_above")
    );
}

/// One undo list for the session: a level chosen on the Sandbox sheet comes
/// back with Ctrl-Z, and the notice names what came back.
#[test]
fn one_undo_list_takes_back_a_level_and_names_it() {
    use sterna::permissions::Level;
    let (_t, mut s, _p) = prefs();
    let (c, n, _) = fixture();
    let before = s.level.level();
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    draw(&c, &n, &s, &mut u, 100, 40);
    click_item(&mut u, &mut s, &n, "level:ask");
    assert_eq!(s.level.level(), Level::Ask);
    draw(&c, &n, &s, &mut u, 100, 40);
    assert!(
        u.geometry
            .hits
            .iter()
            .any(|(_, a)| *a == Action::Sheet(Hit::Undo)),
        "the undo chip rides beside the notice"
    );
    ctrl(&mut u, &mut s, &n, 'z');
    assert_eq!(s.level.level(), before);
    assert_eq!(
        u.top().unwrap().sheet.notice,
        format!("Restored: Sandbox {}.", before.label())
    );
    assert!(u.changes.is_empty(), "an undo is not itself a change");
}

/// Saved globally under a project that sets the same key, a choice does not
/// win here: the row says so before the save and the notice says so after.
#[test]
fn a_global_save_the_project_overrides_says_so() {
    let (t, mut s, mut p) = prefs();
    let project = t.0.join(".sterna");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("config.toml"), "[ui]\ntheme = \"rose\"\n").unwrap();
    p.refresh();
    assert_eq!(p.scope, sterna::settings::Scope::Global);
    p.save("ui.theme", Some("ice".into()), &mut s).unwrap();
    assert_eq!(
        p.notice,
        "Theme is saved globally; this project sets rose, which wins here."
    );
    p.category = 1;
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 40);
    let row = u
        .top()
        .unwrap()
        .sheet
        .items
        .iter()
        .find(|item| item.id == "setting:ui.theme")
        .unwrap()
        .clone();
    assert!(
        row.detail.starts_with("This project sets rose"),
        "{}",
        row.detail
    );
}

/// The opening stays under the card when a note arrives before the first
/// message, and its command chip runs the command instead of typing it.
#[test]
fn the_opening_survives_a_note_and_its_command_chip_runs() {
    let (_, n, mut s) = fixture();
    let c = Conversation::default();
    s.startup_notes = Some(0);
    s.note("Theme is now Ice");
    s.suggestions = vec![
        ("finish setup · 3 steps left".into(), "/wizard".into()),
        (
            "run the tests".into(),
            "Run the tests and tell me what fails.".into(),
        ),
    ];
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 40));
    let chip = screen
        .find("⟨ finish setup")
        .expect("the setup chip is drawn");
    let note = screen.find("Theme is now Ice").expect("the note is drawn");
    assert!(chip < note, "the opening sits above the note:\n{screen}");
    assert_eq!(
        click(&mut u, &mut s, &n, Action::Command("/wizard".into())),
        Effect::Command("/wizard".into())
    );
    draw(&c, &n, &s, &mut u, 100, 40);
    assert_eq!(
        click(
            &mut u,
            &mut s,
            &n,
            Action::Draft("Run the tests and tell me what fails.".into())
        ),
        Effect::Draft("Run the tests and tell me what fails.".into())
    );
}

/// Tab to another tier lands on that tier's current model, not row 0.
#[test]
fn switching_tier_selects_that_tiers_current_model() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(navigator())));
    draw(&c, &n, &s, &mut u, 120, 40);
    key(&mut u, &mut s, &n, KeyCode::Tab);
    draw(&c, &n, &s, &mut u, 120, 40);
    let m = u.models().unwrap();
    assert_eq!(m.role, 1);
    assert_eq!(m.candidates()[m.selected].model, "fixture-helper");
    let focused = &u.top().unwrap().sheet;
    assert!(
        focused.items[focused.focus].id.ends_with(":fixture-helper"),
        "{:?}",
        focused.items[focused.focus]
    );
}

/// A choice is sent and the picker stays open, with the mark moved and the
/// change named.
#[test]
fn a_choice_keeps_the_picker_open_and_names_the_change() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(navigator())));
    draw(&c, &n, &s, &mut u, 120, 40);
    let id = {
        let m = u.models().unwrap();
        let helper = m
            .candidates()
            .into_iter()
            .find(|c| c.model == "fixture-helper")
            .unwrap();
        format!("model:{}:{}", helper.route, helper.model)
    };
    assert_eq!(
        click_item(&mut u, &mut s, &n, &id),
        Effect::Command("/model fixture-helper".into())
    );
    let m = u.models().expect("the picker stays open");
    assert_eq!(m.current.parent, "fixture-helper");
    assert_eq!(u.top().unwrap().sheet.notice, "Main is now fixture-helper");
}

/// A favourite is filled by choosing its slot, then a model; the picker
/// moves on to the next empty slot and says favourites are still off.
#[test]
fn a_favourite_is_a_slot_then_a_model_and_the_picker_moves_on() {
    let (c, n, mut s) = fixture();
    let mut m = navigator();
    m.role = 2;
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(m)));
    draw(&c, &n, &s, &mut u, 120, 60);
    click_item(&mut u, &mut s, &n, "slot:quick");
    draw(&c, &n, &s, &mut u, 120, 60);
    let id = {
        let m = u.models().unwrap();
        let main = m
            .candidates()
            .into_iter()
            .find(|c| c.model == "fixture-main")
            .unwrap();
        format!("model:{}:{}", main.route, main.model)
    };
    assert_eq!(
        click_item(&mut u, &mut s, &n, &id),
        Effect::Command("/subagents quick fixture-main low".into())
    );
    assert_eq!(
        u.top().unwrap().sheet.notice,
        "QUICK is now fixture-main · low · favourites are off: turn them on above"
    );
    assert_eq!(u.models().unwrap().slot.as_deref(), Some("balanced"));
    // Its effort is set in place, one click from any value.
    draw(&c, &n, &s, &mut u, 120, 60);
    let effort = u
        .top()
        .unwrap()
        .sheet
        .items
        .iter()
        .position(|item| item.id == "slot:quick:effort")
        .unwrap();
    assert!(u.top().unwrap().sheet.items[effort].disabled.is_none());
    assert_eq!(
        click(&mut u, &mut s, &n, Action::Sheet(Hit::Value(effort, 2))),
        Effect::Command("/subagents quick fixture-main high".into())
    );
}

/// Pinning one model over the favourites turns them off, so it is asked
/// first; Yes sends it.
#[test]
fn pinning_over_favourites_asks_first() {
    let (c, n, mut s) = fixture();
    let mut m = navigator();
    m.role = 2;
    m.assignment.mode = sterna::config::AgentsMode::Roster;
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(m)));
    draw(&c, &n, &s, &mut u, 120, 60);
    let id = {
        let m = u.models().unwrap();
        let main = m
            .candidates()
            .into_iter()
            .find(|c| c.model == "fixture-main")
            .unwrap();
        format!("model:{}:{}", main.route, main.model)
    };
    assert_eq!(click_item(&mut u, &mut s, &n, &id), Effect::Consumed);
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    draw(&c, &n, &s, &mut u, 120, 60);
    armed(&mut u);
    assert_eq!(
        click_item(&mut u, &mut s, &n, "confirm:yes"),
        Effect::Command("/model subagent fixture-main".into())
    );
    assert!(u.models().is_some(), "back on the picker");
}

/// Search answers best match first, and a model that cannot hold a
/// conversation is never offered or counted.
#[test]
fn search_is_ranked_and_only_chat_models_are_offered() {
    let panel = Panel::models(
        "Models",
        vec![ModelGroup {
            provider: "A".into(),
            account: "a-subscription".into(),
            scope: "subscription".into(),
            models: vec![
                "claude-3-5-sonnet".into(),
                "claude-sonnet-5".into(),
                "gpt-image-1".into(),
                "whisper-1".into(),
            ],
            selectable: Some(true),
            unavailable_reason: None,
            connect: None,
            pooled: None,
            note: None,
        }],
        TierModels {
            parent: "claude-sonnet-5".into(),
            helper: None,
            subagent: None,
        },
    );
    let mut m = Navigator::from_panel(&panel).unwrap();
    assert_eq!(
        m.catalogue_len(),
        2,
        "image and speech models are not counted"
    );
    assert!(m.candidates().iter().all(|c| !c.model.contains("image")));
    m.query = "sonnet 5".into();
    assert_eq!(m.candidates()[0].model, "claude-sonnet-5");
}

/// A locked account's models stay listed, and its one way in is a row.
#[test]
fn a_locked_account_offers_its_sign_in_as_one_row() {
    let (c, n, s) = fixture();
    let mut m = navigator();
    m.all_sources = true;
    let mut u = Workbench::default();
    u.open(Source::Models(Box::new(m)));
    draw(&c, &n, &s, &mut u, 120, 60);
    let sheet = &u.top().unwrap().sheet;
    let row = sheet
        .items
        .iter()
        .find(|item| item.id == "signin:OpenRouter-subscription")
        .expect("a sign-in row for the locked account");
    assert_eq!(row.title, "Sign in to OpenRouter");
    assert_eq!(
        row.action,
        Some(Action::Command("/login OpenRouter-subscription".into()))
    );
}

/// Settings › Subagents is whether subagents run, and the way to the
/// picker where the favourites are chosen.
#[test]
fn settings_subagents_links_to_the_picker() {
    let (_t, s, mut p) = prefs();
    p.category = 4;
    assert_eq!(
        p.rows().iter().map(|spec| spec.key).collect::<Vec<_>>(),
        ["agents.mode"]
    );
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    draw(&c, &n, &s, &mut u, 110, 40);
    assert!(
        u.top()
            .unwrap()
            .sheet
            .items
            .iter()
            .any(|item| item.action == Some(Action::Command("/subagents".into())))
    );
}

/// A sign-in link kept in the chat is whole: laid over as many rows as it
/// needs, and a click on any of them copies all of it.
#[test]
fn a_link_in_a_note_is_whole_and_copies() {
    let (_, n, mut s) = fixture();
    let c = Conversation::default();
    let link = "https://claude.ai/oauth/authorize?client_id=abcdefghijklmnopqrstuvwxyz0123456789&scope=user%3Aprofile&state=s";
    s.startup_notes = Some(0);
    s.note(format!("Sign-in link for Claude:\n{link}"));
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 60, 30));
    let joined: String = screen
        .lines()
        .map(|line| line.trim().trim_start_matches('·').trim())
        .collect();
    assert!(joined.contains(link), "the link is whole:\n{screen}");
    let copies = u
        .geometry
        .hits
        .iter()
        .filter(|(_, a)| *a == Action::Copy(link.into()))
        .count();
    assert!(copies >= 2, "every row of the link copies it");
}

/// Nothing opens a browser on a single click: the row asks first, and the
/// confirmation opens on Cancel.
#[test]
fn opening_a_browser_asks_first() {
    let (c, n, mut s) = fixture();
    let link = "https://x.ai/device";
    let mut panel = Panel::rows(
        "Sign in · Grok",
        vec![sterna::tui::PanelRow::run(
            "open the page in your browser",
            Action::AskOpenLink(link.into()),
        )],
    );
    panel.selected = 0;
    let mut u = Workbench::default();
    u.open(Source::Panel(Box::new(panel)));
    draw(&c, &n, &s, &mut u, 100, 30);
    let row = u.top().unwrap().sheet.items[u.top().unwrap().sheet.focus]
        .id
        .clone();
    assert_eq!(click_item(&mut u, &mut s, &n, &row), Effect::Consumed);
    assert!(u.showing(|source| matches!(source, Source::Confirm(_))));
    draw(&c, &n, &s, &mut u, 100, 30);
    armed(&mut u);
    // Enter on the opening focus is Cancel: nothing opens.
    assert_eq!(key(&mut u, &mut s, &n, KeyCode::Enter), Effect::Consumed);
    draw(&c, &n, &s, &mut u, 100, 30);
    assert_eq!(click_item(&mut u, &mut s, &n, &row), Effect::Consumed);
    draw(&c, &n, &s, &mut u, 100, 30);
    armed(&mut u);
    assert_eq!(
        click_item(&mut u, &mut s, &n, "confirm:yes"),
        Effect::OpenLink(link.into())
    );
}

/// A sign-in running beside the session keeps a chip on the dock that
/// brings its panel back, and its panel can cancel it.
#[test]
fn a_running_sign_in_is_one_chip_away() {
    let (c, n, mut s) = fixture();
    s.signing_in = Some("Grok".into());
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains("signing in to Grok ▸"), "{screen}");
    assert_eq!(
        click(&mut u, &mut s, &n, Action::ReopenSignIn),
        Effect::ReopenSignIn
    );
    u.open(Source::Panel(Box::new(Panel::rows(
        "Sign in · Grok",
        vec![sterna::tui::PanelRow::run(
            "cancel sign-in",
            Action::CancelSignIn,
        )],
    ))));
    draw(&c, &n, &s, &mut u, 120, 40);
    assert_eq!(
        click(&mut u, &mut s, &n, Action::Sheet(Hit::Item(0))),
        Effect::CancelSignIn
    );
}

/// Clicks a hit target while a turn runs.
fn click_during_turn(u: &mut Workbench, s: &mut ScreenState, n: &Notebook, a: Action) -> Effect {
    let (r, _) = u
        .geometry
        .hits
        .iter()
        .find(|(_, v)| *v == a)
        .unwrap()
        .clone();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        let effect = u.event(
            &Event::Mouse(MouseEvent {
                kind,
                column: r.x,
                row: r.y,
                modifiers: KeyModifiers::NONE,
            }),
            s,
            n,
            true,
        );
        if !matches!(kind, MouseEventKind::Down(_)) {
            return effect;
        }
    }
    unreachable!()
}

/// While a turn runs, a row that waits for the turn to end says so before
/// it is clicked, in the one sentence every such refusal uses. A model
/// named in full applies from the turn's next request, so it does not wait;
/// a bare `/model helper` opens a sheet the session builds, so it does.
#[test]
fn mid_turn_a_row_that_waits_says_so_before_a_click() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Executing;
    let mut u = Workbench::default();
    u.open(Source::Panel(Box::new(Panel::rows(
        "Sign in",
        vec![
            sterna::tui::PanelRow::opens("Sign in with an API key", "/key"),
            sterna::tui::PanelRow::command("Use claude-x", "/model claude-x"),
        ],
    ))));
    draw(&c, &n, &s, &mut u, 100, 30);
    let row = |u: &Workbench, words: &str| {
        let items = &u.top().unwrap().sheet.items;
        let at = items.iter().position(|i| i.title.contains(words)).unwrap();
        (at, items[at].disabled.clone())
    };
    assert_eq!(
        row(&u, "API key").1.as_deref(),
        Some(workbench::voice::BETWEEN_TURNS)
    );
    let (model, waits) = row(&u, "claude-x");
    assert_eq!(waits, None, "a model applies from the next request");
    assert_eq!(
        click_during_turn(&mut u, &mut s, &n, Action::Sheet(Hit::Item(model))),
        Effect::Command("/model claude-x".into())
    );
    assert!(workbench::mid_turn("/effort high"));
    assert!(workbench::mid_turn("/model helper claude-x"));
    assert!(!workbench::mid_turn("/model helper"));
    assert!(!workbench::mid_turn("/sandbox"));
    // Between turns nothing waits.
    s.activity = Activity::Complete;
    draw(&c, &n, &s, &mut u, 100, 30);
    assert_eq!(row(&u, "API key").1, None);
}

/// "Show the diff" is a view of the cell it sits under, never a command
/// the turn gate could refuse.
#[test]
fn the_diff_chip_is_a_view_of_its_own_cell() {
    let (c, mut n, mut s) = fixture();
    n.cells[0].returned = Some("Wrote view.rs.".into());
    s.activity = Activity::Complete;
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 40));
    assert!(screen.contains("show the diff"), "{screen}");
    let actions: Vec<Action> = u.geometry.hits.iter().map(|(_, a)| a.clone()).collect();
    assert!(
        !actions.contains(&Action::Command("/diff".into())),
        "no diff chip goes through the command path"
    );
    assert!(actions.contains(&Action::Tab(1, CellTab::Diff)));
}

/// The turn is complete once its check has had its say: until then the
/// status says the answer is in and being checked.
#[test]
fn the_turn_is_complete_once_its_check_has_had_its_say() {
    let (c, n, mut s) = fixture();
    s.activity = Activity::Complete;
    s.lane("check", true);
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains(workbench::voice::CHECKING), "{screen}");
    assert!(!screen.contains("complete"), "{screen}");
    s.lane("check", false);
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains("complete"), "{screen}");
}

/// A turn waiting on the person's answer says so on its card and on the
/// dock, instead of RUNNING.
#[test]
fn a_turn_that_waits_on_you_says_so() {
    let (mut c, n, mut s) = fixture();
    s.activity = Activity::AwaitingYou;
    c.messages
        .push(Message::text(Role::User, "and check the tests"));
    c.messages.push({
        let mut m = Message::text(Role::Assistant, "Checking the guard now.");
        m.content.push(Block::ToolUse {
            id: "call-2".into(),
            name: "execute_cell".into(),
            input: serde_json::json!({"code":"await edit(\"motion.rs\");"}),
        });
        m
    });
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 40));
    assert!(screen.contains("WAITING FOR YOU"), "{screen}");
    assert!(screen.contains("waiting for you"), "{screen}");
    assert!(!screen.contains("RUNNING"), "{screen}");
}

fn cell_turn(id: &str, code: &str) -> Message {
    let mut m = Message::text(Role::Assistant, "Working on it.");
    m.content.push(Block::ToolUse {
        id: id.into(),
        name: "execute_cell".into(),
        input: serde_json::json!({ "code": code }),
    });
    m
}

fn ran(source: &str) -> CellView {
    CellView {
        executed_source: Some(source.into()),
        execution: Some("No tool calls ran in this cell.".into()),
        ..CellView::default()
    }
}

/// The person's message after a turn that ran a cell is shown, even when
/// the runtime's feedback to that cell was kept only as history.
#[test]
fn a_prompt_after_a_cell_turn_is_shown() {
    let (_, _, s) = fixture();
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "message 1"),
            Message::text(Role::Assistant, "Plain one."),
            Message::text(Role::User, "message 2"),
            cell_turn("call-1", "await write(\"a\", \"x\");"),
            Message::runtime("[cell 2 feedback]", "[cell 2 feedback, as history]"),
            Message::text(Role::Assistant, "Wrote a."),
            Message::text(Role::User, "message 3"),
            Message::text(Role::Assistant, "Plain three."),
        ],
    };
    let n = Notebook {
        cells: vec![
            CellView::default(),
            CellView {
                answered: true,
                returned: Some("Wrote a.".into()),
                ..ran("await write(\"a\", \"x\");")
            },
            CellView::default(),
        ],
        ..Notebook::default()
    };
    let u = Workbench::default();
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(shown.contains("message 3"), "{shown}");
    assert!(shown.contains("Plain three."), "{shown}");
}

/// The answer a cell returned is the latest one when the only message after
/// it is the session's echo of that same answer: its next steps are offered.
#[test]
fn an_answer_is_latest_even_with_its_echo_after_it() {
    let (_, _, mut s) = fixture();
    s.activity = Activity::Complete;
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "fix the guard"),
            cell_turn("call-1", "answer(\"The guard is fixed.\");"),
            Message::text(Role::Assistant, "The guard is fixed."),
        ],
    };
    let n = Notebook {
        cells: vec![CellView {
            returned: Some("The guard is fixed.".into()),
            changes: Some("--- a/g.rs\n+++ b/g.rs\n@@ -1 +1 @@\n-a\n+b".into()),
            ..ran("answer(\"The guard is fixed.\");")
        }],
        ..Notebook::default()
    };
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 120, 40);
    assert!(
        u.geometry
            .hits
            .iter()
            .any(|(_, a)| *a == Action::Insert("commit this".into())),
        "the latest answer offers what to do next"
    );
}

/// F4, F5, Ctrl-O and the sidebar read the cells that ran a program; the
/// entry a prose answer leaves in the notebook is not one of them.
#[test]
fn the_cell_keys_and_the_sidebar_skip_a_prose_entry() {
    let (c, mut n, s) = fixture();
    n.cells.push(CellView::default());
    let mut u = Workbench::default();
    let mut s = s;
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 40));
    assert!(screen.contains("1 cell ·"), "{screen}");
    assert!(screen.contains("reduce · returned"), "{screen}");
    assert!(
        u.geometry
            .hits
            .iter()
            .any(|(_, a)| *a == Action::Helper(1, 0))
    );
    key(&mut u, &mut s, &n, KeyCode::F(4));
    assert_eq!(u.tabs.get(&1), Some(&CellTab::Diff));
    key(&mut u, &mut s, &n, KeyCode::F(5));
    assert_eq!(u.tabs.get(&1), Some(&CellTab::Helpers));
    assert!(!u.tabs.contains_key(&2), "no key acted on the prose entry");
}

/// Alt-↑ and Alt-↓ move the selection between cards, and a selected card
/// shows it even when it failed.
#[test]
fn alt_arrows_select_cards_and_a_failed_card_shows_it() {
    let (_, _, mut s) = fixture();
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "go"),
            cell_turn("call-1", "1;"),
            Message::text(Role::User, "again"),
            cell_turn("call-2", "throw new Error(\"no\");"),
        ],
    };
    let n = Notebook {
        cells: vec![
            ran("1;"),
            CellView {
                error: Some(CellError {
                    class: "Error".into(),
                    message: "no".into(),
                    line: None,
                    column: None,
                }),
                ..ran("throw new Error(\"no\");")
            },
        ],
        ..Notebook::default()
    };
    let mut u = Workbench::default();
    let alt = |u: &mut Workbench, s: &mut ScreenState, k: KeyCode| {
        u.event(
            &Event::Key(KeyEvent::new(k, KeyModifiers::ALT)),
            s,
            &n,
            false,
        )
    };
    alt(&mut u, &mut s, KeyCode::Up);
    assert_eq!(
        u.selected_cell,
        Some(2),
        "the first Alt-↑ selects the latest"
    );
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(
        screen.contains("› 002"),
        "a selected failed card shows it:\n{screen}"
    );
    alt(&mut u, &mut s, KeyCode::Up);
    assert_eq!(u.selected_cell, Some(1));
    alt(&mut u, &mut s, KeyCode::Up);
    assert_eq!(u.selected_cell, Some(1), "the first card is the top");
    alt(&mut u, &mut s, KeyCode::Down);
    assert_eq!(u.selected_cell, Some(2));
}

/// Bare /cell expands the latest cell that ran; a word is not a number.
#[test]
fn bare_cell_expands_the_latest_cell_that_ran() {
    let (_, mut n, mut s) = fixture();
    n.cells.push(CellView::default());
    let mut u = Workbench::default();
    assert!(u.local_command("/cell", &mut s, &n));
    assert!(u.expanded.contains(&1));
    assert_eq!(u.selected_cell, Some(1));
    assert!(u.local_command("/cell abc", &mut s, &n));
    assert_eq!(u.notice, "Use /cell <number>, as in /cell 1.");
    let empty = Notebook::default();
    assert!(u.local_command("/cell", &mut s, &empty));
    assert_eq!(u.notice, "No cell has run yet.");
}

/// A cell that asked a question shows what was asked and what was chosen.
#[test]
fn a_card_shows_what_was_asked_and_what_was_chosen() {
    let (c, mut n, s) = fixture();
    n.cells[0].asked = Some("? Which way? → you chose: left".into());
    let u = Workbench::default();
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(shown.contains("? Which way? → you chose: left"), "{shown}");
}

/// The dock names the running cell by its card's number, even when a
/// helper's snapshot already holds that cell in the notebook.
#[test]
fn the_running_cell_is_named_by_its_card() {
    let (_, _, mut s) = fixture();
    s.activity = Activity::Executing;
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "go"),
            cell_turn("call-1", "await scout(\"where\");"),
        ],
    };
    let n = Notebook {
        cells: vec![CellView {
            helpers: vec![HelperRecord {
                helper: "scout".into(),
                ..HelperRecord::default()
            }],
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains("executing cell 001"), "{screen}");
}

/// The composer keeps every character: the rows it draws, joined, are the
/// draft, and a click on a wrapped row puts the caret under the pointer.
#[test]
fn the_composer_draws_every_character_and_a_click_lands_where_it_points() {
    let (c, n, mut s) = fixture();
    s.input = (0..75).map(|i| format!("{i:03},")).collect();
    s.cursor = Some(0);
    let mut u = Workbench::default();
    let b = draw(&c, &n, &s, &mut u, 140, 42);
    let r = u.geometry.composer;
    let drawn: String = (r.y..r.bottom())
        .map(|y| {
            (r.x..r.right())
                .map(|x| b[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    assert_eq!(drawn, s.input);
    // With no space to break at, a row is full: the second row starts at
    // the composer's width.
    let first = r.width as usize;
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Down(MouseButton::Left),
        r.x,
        r.y + 1,
    );
    let effect = mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Up(MouseButton::Left),
        r.x,
        r.y + 1,
    );
    assert_eq!(effect, Effect::Cursor(first));
}

/// A draft taller than its window says how much is out of view, and the
/// window does not jump when the caret stays inside it.
#[test]
fn a_tall_draft_says_what_is_out_of_view() {
    let (c, n, mut s) = fixture();
    s.input = (1..=9)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    s.cursor = Some(s.input.len());
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains("↑ 4 more"), "{screen}");
    assert!(screen.contains("line 9"), "{screen}");
    assert!(!screen.contains("line 4"), "{screen}");
    // A click on the window's first row lands on that row, not on the
    // draft's first line.
    let r = u.geometry.composer;
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Down(MouseButton::Left),
        r.x,
        r.y,
    );
    assert_eq!(
        mouse(
            &mut u,
            &mut s,
            &n,
            MouseEventKind::Up(MouseButton::Left),
            r.x,
            r.y
        ),
        Effect::Cursor(s.input.find("line 5").unwrap())
    );
    // The wheel scrolls the draft, and the window stays where it was put
    // while the caret does not move.
    mouse(&mut u, &mut s, &n, MouseEventKind::ScrollUp, r.x, r.y);
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains("line 4"), "{screen}");
    assert!(screen.contains("↑ 3 more"), "{screen}");
    s.cursor = Some(s.input.find("line 1").unwrap());
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains("line 1"), "{screen}");
    assert!(screen.contains("↓ 4 more"), "{screen}");
}

/// With nothing typed, Home and End go to the conversation's ends.
#[test]
fn home_and_end_on_an_empty_composer_scroll_the_conversation() {
    let (mut c, n, mut s) = fixture();
    for i in 0..40 {
        c.messages
            .push(Message::text(Role::User, format!("question {i}")));
        c.messages
            .push(Message::text(Role::Assistant, format!("answer {i}")));
    }
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 120, 30);
    key(&mut u, &mut s, &n, KeyCode::Home);
    assert!(s.scrollback > 0, "Home goes to the top");
    key(&mut u, &mut s, &n, KeyCode::End);
    assert_eq!(s.scrollback, 0, "End comes back to the latest");
}

/// The popup marks its row with `›`, says when rows do not fit, and a
/// click takes the row it lands on.
#[test]
fn the_popup_marks_its_row_and_a_click_takes_it() {
    let (c, n, mut s) = fixture();
    s.input = "/".into();
    s.completions = (0..12)
        .map(|i| (format!("/command{i}"), format!("does thing {i}")))
        .collect();
    s.completion_selected = 1;
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    assert!(screen.contains("› /command1"), "{screen}");
    assert!(screen.contains("more · ↑↓ or the wheel"), "{screen}");
    assert_eq!(
        click(&mut u, &mut s, &n, Action::Completion(2)),
        Effect::Completion(2)
    );
}

/// Ctrl-C over a selection copies it and puts the selection away, so the
/// next Ctrl-C does what it does without one.
#[test]
fn a_second_ctrl_c_after_a_copy_passes_through() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    let b = draw(&c, &n, &s, &mut u, 120, 40);
    let screen = text(&b);
    let y = screen
        .lines()
        .position(|line| line.contains("Respect reduced motion"))
        .unwrap() as u16;
    let x = screen
        .lines()
        .nth(y as usize)
        .unwrap()
        .find("Respect")
        .unwrap() as u16;
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
    );
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Drag(MouseButton::Left),
        x + 6,
        y,
    );
    draw(&c, &n, &s, &mut u, 120, 40);
    assert!(matches!(ctrl(&mut u, &mut s, &n, 'c'), Effect::Copy(_)));
    assert_eq!(ctrl(&mut u, &mut s, &n, 'c'), Effect::Pass);
    assert!(s.selection.is_none());
}

/// A selection across an open card copies its text, not its frame, and
/// stays inside the transcript: the sidebar beside it is not copied.
#[test]
fn a_selection_across_a_card_copies_no_frame() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    u.expanded.insert(1);
    let b = draw(&c, &n, &s, &mut u, 140, 40);
    let screen = text(&b);
    let rows: Vec<&str> = screen.lines().collect();
    let top = rows
        .iter()
        .position(|l| l.contains("const result"))
        .unwrap() as u16;
    let t = u.geometry.transcript;
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Down(MouseButton::Left),
        t.x + 3,
        top,
    );
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Drag(MouseButton::Left),
        139,
        top + 2,
    );
    draw(&c, &n, &s, &mut u, 140, 40);
    let Effect::Copy(copied) = ctrl(&mut u, &mut s, &n, 'c') else {
        panic!("nothing was copied");
    };
    assert!(!copied.contains('│'), "{copied}");
    assert_eq!(copied.lines().nth(1), Some("print(result);"), "{copied}");
    assert!(
        !copied.contains("SO FAR") && !copied.contains("helpers"),
        "the sidebar is not copied: {copied}"
    );
}

/// A resize puts a selection away: its cells no longer hold its text.
#[test]
fn a_resize_puts_the_selection_away() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    draw(&c, &n, &s, &mut u, 120, 40);
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Down(MouseButton::Left),
        5,
        5,
    );
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Drag(MouseButton::Left),
        20,
        6,
    );
    assert!(s.selection.is_some());
    u.event(&Event::Resize(100, 30), &mut s, &n, false);
    assert!(s.selection.is_none());
}

/// A selection in the transcript stays on its text while new rows arrive
/// and the transcript follows them.
#[test]
fn a_selection_stays_on_its_text_as_the_transcript_moves() {
    let (mut c, n, mut s) = fixture();
    for i in 0..30 {
        c.messages
            .push(Message::text(Role::User, format!("question {i}")));
        c.messages
            .push(Message::text(Role::Assistant, format!("answer {i}")));
    }
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 100, 30));
    let y = screen
        .lines()
        .position(|l| l.contains("answer 28"))
        .unwrap() as u16;
    let x = u.geometry.transcript.x;
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
    );
    mouse(
        &mut u,
        &mut s,
        &n,
        MouseEventKind::Drag(MouseButton::Left),
        x + 60,
        y,
    );
    c.messages
        .push(Message::text(Role::User, "one more question"));
    c.messages
        .push(Message::text(Role::Assistant, "one more answer"));
    draw(&c, &n, &s, &mut u, 100, 30);
    let Effect::Copy(copied) = ctrl(&mut u, &mut s, &n, 'c') else {
        panic!("nothing was copied");
    };
    assert!(copied.contains("answer 28"), "{copied}");
}

/// Answer prose reads as Markdown: no raw markers, and a link is a link.
#[test]
fn answer_markdown_is_rendered() {
    let (_, n, s) = fixture();
    let c = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "what did you do"),
            Message::text(
                Role::Assistant,
                "Wrote **a.txt**, see `x` and [docs](https://example.com)",
            ),
        ],
    };
    let n = Notebook {
        cells: Vec::new(),
        ..n
    };
    let mut u = Workbench::default();
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(shown.contains("Wrote a.txt, see x and docs"), "{shown}");
    for marker in ["**", "`", "]("] {
        assert!(!shown.contains(marker), "{marker} is raw: {shown}");
    }
    draw(&c, &n, &s, &mut u, 120, 40);
    assert!(
        u.geometry
            .hits
            .iter()
            .any(|(_, a)| *a == Action::AskOpenLink("https://example.com".into())),
        "the link opens, asking first"
    );
}

/// A cell's helper calls are drawn inside its card, before its bottom edge.
#[test]
fn helper_rows_are_drawn_inside_their_card() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.expanded.insert(1);
    let d = doc(&c, &n, &s, &u);
    let helper = d
        .rows
        .iter()
        .position(|r| r.text.contains("◇ reduce"))
        .expect("the helper's row");
    let bottom = d
        .rows
        .iter()
        .position(|r| r.kind == sterna::workbench::RowKind::CardBottom)
        .expect("the card's bottom edge");
    assert!(helper < bottom, "the lane is inside the card");
    assert_eq!(d.rows[helper].kind, sterna::workbench::RowKind::CardBody);
}

/// A path in a styled row is clickable, and the diff's file header opens
/// the file it names.
#[test]
fn paths_in_styled_rows_and_the_diff_header_open_their_file() {
    let t = Temp::new();
    std::fs::create_dir_all(t.0.join("src")).unwrap();
    std::fs::write(t.0.join("src/guard.rs"), "fn guard() {}").unwrap();
    let (c, mut n, mut s) = fixture();
    s.settings_root = Some(t.0.clone());
    n.cells[0].changes =
        Some("--- a/src/guard.rs\n+++ b/src/guard.rs\n@@ -1 +1 @@\n-old();\n+new();".into());
    n.cells[0].returned = Some("Fixed the guard in src/guard.rs as asked.".into());
    let mut u = Workbench::default();
    u.expanded.insert(1);
    u.tabs.insert(1, CellTab::Diff);
    draw(&c, &n, &s, &mut u, 140, 50);
    let wanted = Action::Path(t.0.join("src/guard.rs").display().to_string());
    let opens = u.geometry.hits.iter().filter(|(_, a)| *a == wanted).count();
    assert!(
        opens >= 2,
        "the header and the answer's path both open it ({opens})"
    );
}

/// A card's copy reads right: one line is one line, empty sections are not
/// shown, the full output includes the answer, and a short answer() call is
/// shown whole.
#[test]
fn a_cards_copy_reads_right() {
    let (mut c, mut n, s) = fixture();
    c.messages[1] = {
        let mut m = Message::text(Role::Assistant, "Done.");
        m.content.push(Block::ToolUse {
            id: "call-1".into(),
            name: "execute_cell".into(),
            input: serde_json::json!({"code": "answer(\"done\");"}),
        });
        m
    };
    n.cells[0].executed_source = Some("answer(\"done\");".into());
    n.cells[0].description = None;
    n.cells[0].table = Some(String::new());
    n.cells[0].returned = Some("done".into());
    let mut u = Workbench::default();
    u.expanded.insert(1);
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(shown.contains("1 line"), "{shown}");
    assert!(!shown.contains("1 lines"), "{shown}");
    assert!(shown.contains("answer(\"done\")"), "{shown}");
    assert!(!shown.contains("done\");…"), "{shown}");
    u.tabs.insert(1, CellTab::Output);
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(!shown.contains("Handles"), "{shown}");
    assert!(shown.contains("Answer"), "{shown}");
}

/// A cell whose change was rolled back says so and claims no change.
#[test]
fn a_rolled_back_cell_says_so() {
    let (c, mut n, s) = fixture();
    n.cells[0].rolled_back = true;
    let mut u = Workbench::default();
    u.expanded.insert(1);
    let shown = text(&draw(&c, &n, &s, &mut u, 140, 40));
    assert!(shown.contains("↶ ROLLED BACK"), "{shown}");
    assert!(!shown.contains("1 file changed"), "{shown}");
}

/// A helper's account is what was asked, what came back and what it cites;
/// its preparation and the excerpts written for the model are one chip away.
#[test]
fn a_helpers_account_is_for_the_person() {
    let (c, mut n, s) = fixture();
    n.cells[0].helpers[0].looked = vec!["listed src/".into()];
    n.cells[0].helpers[0].outcome.text = format!(
        "The guard is in src/motion.rs.\n\n{}\n### src/motion.rs:1-3\n1 | fn guard() {{}}",
        sterna::excerpts::HEADING
    );
    let mut u = Workbench::default();
    u.expanded.insert(1);
    u.helper = Some((1, 0));
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(
        shown.contains("Answer: The guard is in src/motion.rs."),
        "{shown}"
    );
    assert!(shown.contains("Cited: src/motion.rs:1-3"), "{shown}");
    assert!(!shown.contains("## Excerpts"), "{shown}");
    assert!(!shown.contains("listed src/"), "{shown}");
    u.helper_raw = Some((1, 0));
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(shown.contains("listed src/"), "{shown}");
    assert!(shown.contains("## Excerpts"), "{shown}");
}

/// The sidebar has no heading over nothing, and its tally says what each
/// count is.
#[test]
fn the_sidebar_says_what_its_counts_are() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 140, 40));
    assert!(!screen.contains("THIS SESSION"), "{screen}");
    assert!(screen.contains("✓ 1 ran "), "{screen}");
    assert!(
        !screen.contains("0 running") && !screen.contains("0 failed"),
        "{screen}"
    );
}

/// What a disabled ask throws is written for the model; the card says
/// where to turn asking on.
#[test]
fn a_disabled_ask_reads_as_where_to_turn_it_on() {
    let (c, mut n, s) = fixture();
    n.cells[0].error = Some(CellError {
        class: "ToolError".into(),
        message: sterna::ask::DISABLED.into(),
        line: None,
        column: None,
    });
    let mut u = Workbench::default();
    u.expanded.insert(1);
    let shown = words(&doc(&c, &n, &s, &u));
    assert!(
        shown.contains("asking is off (Settings › Advanced › Ask the person)"),
        "{shown}"
    );
    assert!(!shown.contains("[ask] enabled"), "{shown}");
}

/// The Theme row keeps the theme in force on screen, however many themes
/// there are: drawn as the chip that is on, or held by the fold chip.
#[test]
fn the_active_theme_chip_is_always_drawn() {
    let (_t, mut s, mut p) = prefs();
    p.save("ui.theme", Some("cockatoo".into()), &mut s).unwrap();
    p.category = 1;
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let (c, n, _) = fixture();
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 40));
    let row = screen
        .lines()
        .find(|line| line.contains("Theme"))
        .unwrap_or_else(|| panic!("no Theme row:\n{screen}"));
    assert!(row.contains("cockatoo ●"), "{screen}");
}

/// What the top bar has no room for is one chip away, never gone: the
/// `⋯` chip lists it, and a row of that list does what the chip would have.
#[test]
fn the_top_bar_folds_what_does_not_fit_into_one_chip() {
    let (c, n, mut s) = fixture();
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 60, 24));
    let bar = screen.lines().next().unwrap().to_string();
    assert!(bar.contains("⟨ ⋯ ⟩"), "{bar}");
    let folded = u
        .geometry
        .hits
        .iter()
        .find_map(|(_, action)| match action {
            Action::More(list) => Some(list.clone()),
            _ => None,
        })
        .expect("a fold chip");
    for (label, _) in &folded {
        assert!(
            !bar.contains(&format!("⟨ {label} ⟩")),
            "{label} is drawn and folded: {bar}"
        );
    }
    let help = folded
        .iter()
        .position(|(label, action)| label == "?" && *action == Action::Help)
        .unwrap_or_else(|| panic!("? is neither drawn nor folded: {folded:?}"));
    click(&mut u, &mut s, &n, Action::More(folded));
    draw(&c, &n, &s, &mut u, 60, 24);
    assert!(u.showing(|source| matches!(source, Source::More(_))));
    click_item(&mut u, &mut s, &n, &format!("more:{help}"));
    assert!(
        u.showing(|source| matches!(source, Source::Keys)),
        "the row did what ? does, and the list went"
    );
    assert_eq!(u.sheets.len(), 1);
}

/// A card's tab strip keeps the open tab when it cannot show them all;
/// the rest are one `+N ▾` chip away.
#[test]
fn a_chip_row_keeps_its_open_tab_and_folds_the_rest() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.expanded.insert(1);
    u.tabs.insert(1, CellTab::Helpers);
    let screen = text(&draw(&c, &n, &s, &mut u, 50, 30));
    let strip = screen
        .lines()
        .find(|line| line.contains("▾ ⟩"))
        .unwrap_or_else(|| panic!("no fold chip:\n{screen}"));
    assert!(strip.contains("⟨ Helpers ⟩"), "{screen}");
    let folded = u
        .geometry
        .hits
        .iter()
        .find_map(|(_, action)| match action {
            Action::More(list) if matches!(list[0].1, Action::Tab(..)) => Some(list.clone()),
            _ => None,
        })
        .expect("a fold chip");
    assert!(
        folded
            .iter()
            .all(|(label, action)| matches!(action, Action::Tab(1, _))
                && !strip.contains(&format!("⟨ {label} ⟩"))),
        "{folded:?}\n{strip}"
    );
}

/// Scrolled back, `↓ latest` has a row of its own: it covers no card edge
/// and no line of text.
#[test]
fn the_latest_chip_covers_nothing() {
    let (mut c, n, mut s) = fixture();
    for _ in 0..40 {
        c.messages
            .push(Message::text(Role::User, "An earlier instruction."));
    }
    // Every offset of a few rows, so the row the chip keeps would have
    // held text at some of them.
    for back in 1..6 {
        s.scrollback = back;
        let mut u = Workbench::default();
        let screen = text(&draw(&c, &n, &s, &mut u, 80, 24));
        let row = screen
            .lines()
            .find(|line| line.contains("↓ latest"))
            .unwrap_or_else(|| panic!("no latest chip:\n{screen}"));
        let rest = row.replace("⟨ ↓ latest ⟩", "");
        assert!(
            rest.chars().all(|ch| ch == ' ' || ch == '▐'),
            "the chip shares its row {back} rows back: {row:?}"
        );
    }
}

/// The composer's bottom edge is one unbroken rule where it has no room
/// for a chip.
#[test]
fn the_composer_edge_has_no_gap_without_chips() {
    let (c, mut n, s) = fixture();
    n.context = Some(sterna::tui::ContextTokens {
        used: 7_400,
        cap: None,
        cap_source: sterna::models::WindowSource::Unknown,
        counted: sterna::tui::Counted::Estimated,
    });
    let mut u = Workbench::default();
    let screen = text(&draw(&c, &n, &s, &mut u, 34, 20));
    let edge = screen.lines().last().unwrap();
    assert!(edge.starts_with("╰──"), "{edge:?}");
}

/// A narrow sheet's foot gives up its middle hints before it cuts a word:
/// what Enter does and what Esc does stay whole.
#[test]
fn a_narrow_sheet_foot_keeps_its_first_and_last_hints_whole() {
    let (c, n, _) = fixture();
    let (_t, s, p) = prefs();
    let mut u = Workbench::default();
    u.open(Source::Settings(Box::new(p)));
    let screen = text(&draw(&c, &n, &s, &mut u, 50, 15));
    let foot = screen.lines().nth(13).unwrap();
    assert!(foot.contains("Enter open"), "{screen}");
    assert!(foot.contains("Esc close"), "{screen}");
}

/// The Arctic tern is a theme of its own family: it perches on an empty
/// conversation's card, which is as tall as its drawing, and once the
/// conversation starts its head stays beside the greeting.
#[test]
fn the_arctic_tern_perches_and_its_head_stays_beside_the_greeting() {
    use sterna::workbench::plumage::Bird;
    let drawn = |screen: &str| {
        screen
            .lines()
            .skip(2)
            .take_while(|line| !line.trim_start().starts_with('─'))
            .filter(|line| line.contains(['▀', '▄']))
            .count()
    };
    let (c, n, mut s) = fixture();
    s.truecolor = true;
    let empty = Conversation {
        system: String::new(),
        messages: vec![],
    };
    s.theme = Theme::Bird(Bird::ArcticTern);
    let mut u = Workbench::default();
    let perched = text(&draw(&empty, &Notebook::default(), &s, &mut u, 100, 30));
    assert!(
        perched.contains("Arctic Tern · the longest migration of any bird"),
        "{perched}"
    );
    // Twenty-four pixel rows, the empty ones above the bird left out.
    assert_eq!(drawn(&perched), 10, "{perched}");
    s.theme = Theme::Bird(Bird::Amazon);
    let mut u = Workbench::default();
    let parrot = text(&draw(&empty, &Notebook::default(), &s, &mut u, 100, 30));
    assert_eq!(drawn(&parrot), 12, "{parrot}");
    s.theme = Theme::Bird(Bird::ArcticTern);
    let mut u = Workbench::default();
    let started = text(&draw(&c, &n, &s, &mut u, 100, 50));
    let greeting = started
        .lines()
        .find(|line| line.contains("What should we build?"))
        .unwrap_or_else(|| panic!("{started}"));
    assert!(
        greeting.contains(['▀', '▄']),
        "the head is beside it: {started}"
    );
    assert_eq!(drawn(&started), 4, "{started}");
}

/// The theme picker has a Seabirds family, and the tern is in it.
#[test]
fn the_theme_picker_has_a_seabirds_family() {
    let (c, n, s) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Themes { before: s.theme });
    let screen = text(&draw(&c, &n, &s, &mut u, 110, 40));
    let heading = screen
        .lines()
        .position(|line| line.contains("SEABIRDS"))
        .unwrap_or_else(|| panic!("{screen}"));
    assert!(
        screen
            .lines()
            .nth(heading + 1)
            .unwrap()
            .contains("Arctic Tern"),
        "{screen}"
    );
}

fn rgb_of(colour: Color) -> Option<u32> {
    match colour {
        Color::Rgb(r, g, b) => Some(u32::from_be_bytes([0, r, g, b])),
        _ => None,
    }
}

/// A terminal that shows no true colour is sent none: every colour is one
/// of its 256.
#[test]
fn without_true_colour_no_rgb_is_emitted() {
    let (c, n, mut s) = fixture();
    s.truecolor = false;
    s.theme = Theme::Neon;
    let mut u = Workbench::default();
    let b = draw(&c, &n, &s, &mut u, 140, 42);
    for cell in b.content() {
        assert!(
            rgb_of(cell.fg).is_none() && rgb_of(cell.bg).is_none(),
            "{:?} is sent as 24-bit colour",
            cell.symbol()
        );
    }
    u.open(Source::Themes { before: s.theme });
    let b = draw(&c, &n, &s, &mut u, 140, 42);
    assert!(b.content().iter().all(|cell| rgb_of(cell.fg).is_none()));
}

/// On a light terminal every coloured word reads against its white -- the
/// roles, the accent of a pale theme, the ink on a chosen chip.
#[test]
fn a_light_terminal_gets_colours_that_read_on_it() {
    use sterna::workbench::look::{LIGHT_GROUND, contrast};
    let (c, n, mut s) = fixture();
    s.truecolor = true;
    s.light = true;
    s.sidebar = sterna::tui::SidebarVisibility::Shown;
    for theme in [
        Theme::Neon,
        Theme::Amber,
        Theme::Bird(sterna::workbench::plumage::Bird::Cockatoo),
    ] {
        s.theme = theme;
        let mut u = Workbench::default();
        let mut buffers = vec![draw(&c, &n, &s, &mut u, 140, 40)];
        u.open(Source::Sandbox);
        buffers.push(draw(&c, &n, &s, &mut u, 140, 40));
        for b in buffers {
            // Plumage is drawing, not text: its pixels keep their own rule.
            for cell in b
                .content()
                .iter()
                .filter(|cell| !["▀", "▄", "█"].contains(&cell.symbol()))
            {
                let Some(fg) = rgb_of(cell.fg) else { continue };
                match rgb_of(cell.bg) {
                    // Ink on a filled chip reads on the chip.
                    Some(bg) => assert!(
                        contrast(fg, bg) >= 4.5,
                        "{theme:?}: {:?} is {fg:06x} on {bg:06x}",
                        cell.symbol()
                    ),
                    None => assert!(
                        contrast(fg, LIGHT_GROUND) >= 3.0,
                        "{theme:?}: {:?} is {fg:06x} on a light ground",
                        cell.symbol()
                    ),
                }
            }
        }
    }
}

/// The instruments are drawn by older code with colours of their own; they
/// read on a light terminal like everything else, and a terminal without
/// true colour is sent none of theirs either.
#[test]
fn the_instruments_read_on_a_light_terminal_and_send_no_rgb_without_it() {
    use sterna::workbench::look::{LIGHT_GROUND, contrast};
    let (c, n, mut s) = fixture();
    s.telemetry_open = true;
    s.activity = Activity::Thinking;
    s.theme = Theme::Neon;
    s.truecolor = true;
    s.light = true;
    let mut u = Workbench::default();
    let b = draw(&c, &n, &s, &mut u, 140, 40);
    let mut checked = 0;
    for cell in b
        .content()
        .iter()
        .filter(|cell| !cell.symbol().trim().is_empty())
    {
        let on_ground = matches!(cell.bg, Color::Reset);
        match cell.fg {
            Color::Reset => {}
            Color::Rgb(..) if on_ground => {
                let fg = rgb_of(cell.fg).unwrap();
                assert!(
                    contrast(fg, LIGHT_GROUND) >= 3.0,
                    "{:?} is {fg:06x} on a light ground",
                    cell.symbol()
                );
                checked += 1;
            }
            Color::Rgb(..) => {}
            // A named colour is the terminal's own, drawn for its dark
            // ground: on a light one it is not left to chance.
            named if on_ground => panic!("{:?} is drawn in {named:?}", cell.symbol()),
            _ => {}
        }
    }
    assert!(checked > 20, "the instruments were drawn: {checked}");
    s.light = false;
    s.truecolor = false;
    let b = draw(&c, &n, &s, &mut u, 140, 40);
    for cell in b.content() {
        assert!(
            rgb_of(cell.fg).is_none() && rgb_of(cell.bg).is_none(),
            "{:?} is sent as 24-bit colour",
            cell.symbol()
        );
    }
}

/// On a light terminal a bird wears its light palette: the tern's white
/// is the grey its drawing names for a light ground, shaded just enough to
/// keep its outline there.
#[test]
fn a_bird_on_a_light_terminal_wears_its_light_palette() {
    let (_, _, mut s) = fixture();
    s.truecolor = true;
    s.theme = Theme::Bird(sterna::workbench::plumage::Bird::ArcticTern);
    let empty = Conversation {
        system: String::new(),
        messages: vec![],
    };
    let colours = |s: &ScreenState| {
        let mut u = Workbench::default();
        let b = draw(&empty, &Notebook::default(), s, &mut u, 100, 30);
        b.content()
            .iter()
            .filter(|cell| ["▀", "▄"].contains(&cell.symbol()))
            .flat_map(|cell| [rgb_of(cell.fg), rgb_of(cell.bg)])
            .flatten()
            .collect::<std::collections::BTreeSet<u32>>()
    };
    let dark = colours(&s);
    s.light = true;
    let light = colours(&s);
    let named = sterna::workbench::look::readable(0xdce1e6, true, 1.4);
    assert!(dark.contains(&0xeef0f2) && !dark.contains(&named));
    assert!(light.contains(&named) && !light.contains(&0xeef0f2));
    assert!(sterna::workbench::look::contrast(named, sterna::workbench::look::LIGHT_GROUND) >= 1.4);
}

/// Mono is monochrome: the person and the helpers are told apart by
/// weight, not hue.
#[test]
fn mono_draws_the_person_and_the_helpers_without_hue() {
    let (c, n, mut s) = fixture();
    s.theme = Theme::Mono;
    let mut u = Workbench::default();
    let b = draw(&c, &n, &s, &mut u, 140, 40);
    let cell_of = |needle: &str| {
        let screen = text(&b);
        let (y, line) = screen
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle}:\n{screen}"));
        let x = line.chars().position(|_| true).unwrap()
            + line[..line.find(needle).unwrap()].chars().count();
        b[(x as u16, y as u16)].clone()
    };
    for needle in ["you", "◇ reduce"] {
        let cell = cell_of(needle);
        assert_eq!(cell.fg, Color::Reset, "{needle} has a hue in mono");
    }
}

/// The background setting is in force the moment it is chosen.
#[test]
fn choosing_a_light_background_applies_now() {
    let (_t, mut s, mut p) = prefs();
    assert!(!s.light);
    p.save("ui.background", Some("light".into()), &mut s)
        .unwrap();
    assert!(s.light);
    p.save("ui.background", Some("dark".into()), &mut s)
        .unwrap();
    assert!(!s.light);
}

/// A panel whose leaving is itself an answer -- the rollback preview -- is
/// a decision: a key typed as it appears answers nothing.
#[test]
fn the_rollback_preview_holds_back_a_key_typed_as_it_appears() {
    let (c, n, mut s) = fixture();
    let mut panel = Panel::rows(
        "Rollback",
        vec![
            sterna::tui::PanelRow::run("Cancel", Action::Command("/rollback cancel".into())),
            sterna::tui::PanelRow::danger("Roll back", Action::Command("/rollback confirm".into())),
        ],
    );
    panel.back = Some(Action::Command("/rollback cancel".into()));
    let mut u = Workbench::default();
    u.open(Source::Panel(Box::new(panel)));
    draw(&c, &n, &s, &mut u, 100, 30);
    assert_eq!(key(&mut u, &mut s, &n, KeyCode::Enter), Effect::Consumed);
    assert!(u.top().unwrap().sheet.notice.contains("held back"));
    std::thread::sleep(sterna::workbench::sheet::ARMING + std::time::Duration::from_millis(50));
    assert_eq!(
        key(&mut u, &mut s, &n, KeyCode::Enter),
        Effect::Command("/rollback cancel".into())
    );
}

/// The allowed hosts are one sheet away from the Sandbox sheet: every
/// ecosystem is a switch, the person's own hosts can be added (a pasted URL
/// is refused, not half-allowed) and removed, each change is saved to the
/// global settings, and it reaches the running proxy's list at once.
#[test]
fn the_hosts_sheet_switches_ecosystems_and_adds_and_removes_hosts() {
    use sterna::sandbox::proxy::{Allowed, ECOSYSTEMS};
    let (t, mut s, _) = prefs();
    let (c, n, _) = fixture();
    let allowed = Allowed::defaults();
    s.allowed = Some(allowed.clone());
    let global = sterna::settings::Store::with_global(&t.0, Some(t.0.join("user")))
        .unwrap()
        .path(sterna::settings::Scope::Global);
    let saved = || std::fs::read_to_string(&global).unwrap_or_default();
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    draw(&c, &n, &s, &mut u, 120, 60);
    click_item(&mut u, &mut s, &n, "sandbox:hosts");
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 60));
    assert!(screen.contains("ALLOWED HOSTS"), "{screen}");
    assert!(
        screen.contains("apply to this session's next command"),
        "{screen}"
    );
    for ecosystem in ECOSYSTEMS {
        assert!(
            screen.contains(ecosystem.label),
            "{}: {screen}",
            ecosystem.label
        );
    }

    click_item(&mut u, &mut s, &n, "eco:rust");
    assert!(!allowed.permits("crates.io"), "switched off, still reached");
    assert!(allowed.permits("registry.npmjs.org"));
    assert!(saved().contains("ecosystems = ["), "{}", saved());
    assert!(!saved().contains("\"rust\""), "{}", saved());
    draw(&c, &n, &s, &mut u, 120, 60);
    click_item(&mut u, &mut s, &n, "eco:rust");
    assert!(allowed.permits("crates.io"));
    assert!(saved().contains("\"rust\""), "{}", saved());

    let type_host = |u: &mut Workbench, s: &mut ScreenState, host: &str| {
        draw(&c, &n, s, u, 120, 60);
        u.top_mut().unwrap().sheet.focus_id("host:new");
        for ch in host.chars() {
            key(u, s, &n, KeyCode::Char(ch));
        }
        key(u, s, &n, KeyCode::Enter);
    };
    type_host(&mut u, &mut s, "https://api.example.com/v1");
    assert!(
        u.top().unwrap().sheet.notice.contains("is not a host name"),
        "{}",
        u.top().unwrap().sheet.notice
    );
    assert!(!saved().contains("api.example.com"), "{}", saved());
    ctrl(&mut u, &mut s, &n, 'u');
    type_host(&mut u, &mut s, "api.example.com");
    assert!(allowed.permits("api.example.com"));
    assert!(
        saved().contains("hosts = [\"api.example.com\"]"),
        "{}",
        saved()
    );
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 60));
    assert!(screen.contains("Remove · api.example.com"), "{screen}");

    click_item(&mut u, &mut s, &n, "host:api.example.com");
    assert!(!allowed.permits("api.example.com"));
    assert!(saved().contains("hosts = []"), "{}", saved());
}

/// With no proxy running, the sheet says a change waits for the next
/// session, and still saves it.
#[test]
fn the_hosts_sheet_says_when_a_change_applies_without_a_proxy() {
    let (_t, mut s, _) = prefs();
    let (c, n, _) = fixture();
    let mut u = Workbench::default();
    u.open(Source::Sandbox);
    draw(&c, &n, &s, &mut u, 120, 60);
    click_item(&mut u, &mut s, &n, "sandbox:hosts");
    let screen = text(&draw(&c, &n, &s, &mut u, 120, 60));
    assert!(screen.contains("apply from the next session"), "{screen}");
    click_item(&mut u, &mut s, &n, "eco:go");
    assert_eq!(
        u.top().unwrap().sheet.notice,
        "Go is off from the next session."
    );
}
