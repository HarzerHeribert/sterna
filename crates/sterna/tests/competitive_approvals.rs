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
