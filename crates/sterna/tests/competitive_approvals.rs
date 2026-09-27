use ratatui::{Terminal, backend::TestBackend};
use sterna::approval::Confirmation;
use sterna::tools::invoke::CheckedArgs;

#[test]
fn confirmation_escapes_terminal_and_bidi_controls_and_keeps_exact_arguments() {
    let mut arguments = CheckedArgs::new();
    arguments.insert(
        "command".into(),
        "echo \u{1b}[2J\u{202e}hidden\nnext".into(),
    );
    let confirmation = Confirmation::new("bash", "/workspace", &arguments);
    assert!(confirmation.complete);
    assert!(!confirmation.text.contains('\u{1b}'));
    assert!(!confirmation.text.contains('\u{202e}'));
    assert!(confirmation.text.contains("hidden\\nnext"));
    assert!(confirmation.text.contains("command"));
}

#[test]
fn oversized_actions_disable_approval_instead_of_showing_a_partial_action() {
    let mut arguments = CheckedArgs::new();
    arguments.insert("content".into(), "x".repeat(20_000));
    let confirmation = Confirmation::new("write", "/workspace", &arguments);
    assert!(!confirmation.complete);
    assert!(confirmation.text.contains("Approval is disabled"));
    assert!(confirmation.text.len() < 1024);
}

#[test]
fn approval_overlay_renders_decisions_and_survives_tiny_terminals() {
    let confirmation = Confirmation::new("read", "/workspace", &CheckedArgs::new());
    for (width, height) in [(100, 30), (20, 8), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| sterna::tui::render_approval(frame, &confirmation, u16::MAX, None))
            .unwrap();
        if width == 100 {
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("Allow once"));
            assert!(text.contains("Deny"));
        }
    }
}
