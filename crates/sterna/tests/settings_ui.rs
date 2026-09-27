//! `SettingsPanel` is backend-independent: these tests build it from plain
//! `Row` values, never a real settings store, and check only what the panel
//! itself owns -- staged edits, Apply/Cancel/scope semantics, and rendering
//! -- per `.agent-runtime/sterna-settings-ui.md`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use sterna::settings_ui::{Action, Row, SettingsPanel};
use sterna::tui::Theme;

fn rows() -> Vec<Row> {
    vec![
        Row {
            key: "theme".into(),
            label: "Theme".into(),
            description: "Accent palette for this session.".into(),
            value: "neon".into(),
            origin: "global".into(),
            choices: vec!["neon".into(), "amber".into(), "ice".into()],
            restart: false,
        },
        Row {
            key: "model".into(),
            label: "Main model".into(),
            description: "Model ID for the parent agent.".into(),
            value: "claude-opus-5".into(),
            origin: "project".into(),
            choices: vec![],
            restart: true,
        },
        Row {
            key: "status_line".into(),
            label: "Status line".into(),
            description: "How much session status stays on screen.".into(),
            value: "full".into(),
            origin: "built-in".into(),
            choices: vec!["full".into(), "compact".into(), "hide".into()],
            restart: false,
        },
    ]
}

fn press(panel: &mut SettingsPanel, code: KeyCode) -> Action {
    panel.key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn rendered(panel: &SettingsPanel, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width.max(1), height.max(1))).unwrap();
    terminal
        .draw(|frame| panel.render(frame, Theme::Neon))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height.max(1))
        .map(|y| {
            (0..width.max(1))
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn unset_model_cycles_to_the_first_catalog_choice() {
    let mut data = rows();
    data[0].value = "unset".into();
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), data);
    press(&mut panel, KeyCode::Right);
    assert_eq!(panel.edits(), vec![("theme".into(), Some("neon".into()))]);
}

#[test]
fn left_right_cycles_a_choice_row_and_reverting_clears_the_stage() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    assert_eq!(press(&mut panel, KeyCode::Right), Action::None);
    assert!(panel.is_dirty());
    assert_eq!(panel.edits(), vec![("theme".into(), Some("amber".into()))]);
    assert_eq!(press(&mut panel, KeyCode::Left), Action::None);
    assert!(!panel.is_dirty());
    assert!(panel.edits().is_empty());
}

#[test]
fn enter_opens_free_text_editing_for_rows_without_choices() {
    let mut panel = SettingsPanel::new(1, "/tmp/project".into(), rows());
    press(&mut panel, KeyCode::Down); // select "Main model"
    press(&mut panel, KeyCode::Enter); // open the text box
    for c in "claude-sonnet-5".chars() {
        press(&mut panel, KeyCode::Char(c));
    }
    assert_eq!(press(&mut panel, KeyCode::Enter), Action::None);
    assert_eq!(
        panel.edits(),
        vec![("model".into(), Some("claude-sonnet-5".into()))]
    );
}

#[test]
fn esc_while_editing_only_discards_the_text_box_not_the_whole_panel() {
    let mut panel = SettingsPanel::new(1, "/tmp/project".into(), rows());
    press(&mut panel, KeyCode::Down);
    press(&mut panel, KeyCode::Enter);
    press(&mut panel, KeyCode::Char('x'));
    assert_eq!(press(&mut panel, KeyCode::Esc), Action::None);
    assert!(!panel.is_dirty());
    assert_eq!(press(&mut panel, KeyCode::Esc), Action::Cancel);
}

#[test]
fn esc_discards_every_staged_edit_before_closing() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    press(&mut panel, KeyCode::Right);
    assert!(panel.is_dirty());
    assert_eq!(press(&mut panel, KeyCode::Esc), Action::Cancel);
    assert!(!panel.is_dirty());
    assert!(panel.edits().is_empty());
}

#[test]
fn tab_is_refused_while_dirty_and_keeps_the_staged_edit_intact() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    press(&mut panel, KeyCode::Right);
    assert_eq!(press(&mut panel, KeyCode::Tab), Action::None);
    assert!(panel.is_dirty(), "a blocked tab must not lose the edit");
    assert_eq!(
        panel.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        Action::Apply
    );
}

#[test]
fn tab_switches_scope_once_nothing_is_staged() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    assert_eq!(press(&mut panel, KeyCode::Tab), Action::SwitchScope(1));
    let mut other = SettingsPanel::new(1, "/tmp/project".into(), rows());
    assert_eq!(press(&mut other, KeyCode::Tab), Action::SwitchScope(0));
}

#[test]
fn ctrl_s_with_nothing_staged_applies_nothing() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    assert_eq!(
        panel.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
        Action::None
    );
}

#[test]
fn reset_to_inherited_only_takes_effect_when_this_scope_holds_the_override() {
    // Scope 0 is Global; the theme row's origin is "global", so this
    // scope owns the value and can unset it.
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    assert_eq!(press(&mut panel, KeyCode::Backspace), Action::None);
    assert_eq!(panel.edits(), vec![("theme".into(), None)]);
    // Pressing it again un-stages the pending unset rather than staging a
    // second one.
    press(&mut panel, KeyCode::Backspace);
    assert!(!panel.is_dirty());

    // The status-line row's origin is "built-in": this scope never
    // overrode it, so reset is a no-op.
    press(&mut panel, KeyCode::Down);
    press(&mut panel, KeyCode::Down);
    assert_eq!(press(&mut panel, KeyCode::Backspace), Action::None);
    assert!(!panel.is_dirty());
}

#[test]
fn renders_without_panicking_at_tiny_and_wide_terminal_sizes() {
    let panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    let _ = rendered(&panel, 8, 4);
    let _ = rendered(&panel, 1, 1);
    let wide = rendered(&panel, 160, 30);
    assert!(wide.contains("Settings"));
    assert!(wide.contains("Theme"));
    assert!(wide.contains("Main model"));
    assert!(wide.contains("↑↓ move"));
}

#[test]
fn a_dirty_choice_row_shows_the_staged_value() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    press(&mut panel, KeyCode::Right);
    let shown = rendered(&panel, 100, 20);
    assert!(shown.contains("amber"));
}

#[test]
fn statusline_row_shows_a_described_preview_not_a_shelled_render() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    press(&mut panel, KeyCode::Down);
    press(&mut panel, KeyCode::Down); // select "Status line"
    let shown = rendered(&panel, 100, 20);
    assert!(shown.contains("preview:"));
    assert!(shown.contains("wraps to two lines"));
}

#[test]
fn scrolling_keeps_the_selected_row_visible_in_a_short_list() {
    let many = (0..30)
        .map(|i| Row {
            key: format!("k{i}"),
            label: format!("Row {i}"),
            description: String::new(),
            value: "x".into(),
            origin: "built-in".into(),
            choices: vec![],
            restart: false,
        })
        .collect();
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), many);
    for _ in 0..25 {
        press(&mut panel, KeyCode::Down);
    }
    let shown = rendered(&panel, 100, 12);
    assert!(shown.contains("Row 25"));
    assert!(!shown.contains("Row 0 "));
}

#[test]
fn notice_surfaces_in_the_next_render() {
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    panel.notice("Saved to project settings.toml".into());
    let shown = rendered(&panel, 100, 20);
    assert!(shown.contains("Saved to project settings.toml"));
}

#[test]
fn a_release_event_is_ignored() {
    use crossterm::event::KeyEventKind;
    let mut panel = SettingsPanel::new(0, "/tmp/project".into(), rows());
    let mut release = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    assert_eq!(panel.key(release), Action::None);
    assert!(!panel.is_dirty());
}
