//! Every test here needs a real sandbox root, so the whole file is
//! Unix-only — imports included, because `-D warnings` makes an unused
//! import an error on Windows and gating only the tests left these dead.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use sterna::contract::SessionId;
use sterna::runtime::isolate::Runtime;
use sterna::runtime::outcome::Ended;
use sterna::sandbox::profile::Profile;

#[test]
fn typed_tool_failures_and_bash_exit_codes_have_truthful_call_outcomes() {
    let root =
        std::env::temp_dir().join(format!("sterna-tool-call-outcomes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("directory")).unwrap();
    let profile = Profile::compile(
        &root,
        Some(r#"{"permissions":{"allow":["Read(**)","Bash"]}}"#),
    );
    let mut runtime = Runtime::new(&profile, &SessionId::new("tool-call-outcomes"));

    let directory = root.join("directory");
    let read = runtime.run_cell(&format!(
        r#"try {{ await read({{path:{directory:?}}}); }} catch (_) {{}}"#
    ));
    let read_calls = &read.turn().record.calls;
    assert_eq!(read_calls.len(), 1);
    assert_eq!(read_calls[0].tool, "read");
    assert_eq!(
        read_calls[0].ended,
        Ended::Threw {
            class: "ToolError".into()
        }
    );

    let bash = runtime.run_cell(r#"await bash({command:"exit 7"});"#);
    let bash_calls = &bash.turn().record.calls;
    assert_eq!(bash_calls.len(), 1);
    assert_eq!(bash_calls[0].tool, "bash");
    assert_eq!(bash_calls[0].ended, Ended::Ok);

    let _ = std::fs::remove_dir_all(root);
}

/// **A child killed by a signal did not succeed**, `bash` included.
///
/// A signal death is not an exit status, so `exit_code` is `None`; the
/// failure check early-returned on that and the call was recorded `Ended::Ok`
/// with a typed result built from whatever partial output the child had
/// managed.
#[test]
fn a_signal_killed_child_is_not_a_successful_call() {
    let root = std::env::temp_dir().join(format!("sterna-signal-outcome-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let profile = Profile::compile(&root, Some(r#"{"permissions":{"allow":["Bash"]}}"#));
    let mut runtime = Runtime::new(&profile, &SessionId::new("signal-outcome"));

    let killed = runtime.run_cell(r#"try { await bash({command:"kill -9 $$"}); } catch (_) {}"#);
    let calls = &killed.turn().record.calls;
    assert_eq!(calls.len(), 1, "{killed:?}");
    assert_eq!(calls[0].tool, "bash");
    assert_eq!(
        calls[0].ended,
        Ended::Threw {
            class: "ToolError".into()
        },
        "a signal-killed child was recorded as a successful call: {killed:?}"
    );

    let _ = std::fs::remove_dir_all(root);
}
