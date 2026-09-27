//! Opening and copying a link from the terminal UI, for the sign-in panel.

use std::io::{self, Write};
use std::process::{Command, Stdio};

use base64::Engine as _;

/// Opens an `https` link with the system's own launcher (on macOS
/// `/usr/bin/open`, never whatever a terminal put first on `PATH`), so it
/// lands in the default browser. Over SSH a browser would open on the wrong
/// screen, so nothing is opened there; anything but `https` is refused.
pub(super) fn open(url: &str) -> bool {
    let over_ssh = ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    if over_ssh || !url.starts_with("https://") {
        return false;
    }
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/open");
        command.arg(url);
        command
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Shows a file to the person, with the system's own opener.
///
/// **It opens, it never reads.** The path came off the drawn screen, so it is
/// already something this session printed; handing it to the launcher shows it
/// in whatever window the person's machine uses for that kind of file, which
/// is the whole point of the click. Nothing is executed: `open`/`xdg-open`
/// resolve a file to its editor or viewer, and a path that does not exist is
/// refused here rather than by a dialog.
///
/// Over SSH the window would open on the wrong machine, so nothing does.
pub(super) fn show(path: &std::path::Path) -> bool {
    let over_ssh = ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    if over_ssh || !path.exists() {
        return false;
    }
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/open");
        command.arg(path);
        command
    } else if cfg!(windows) {
        let mut command = Command::new("explorer.exe");
        command.arg(path);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The OSC 52 sequence asking the terminal to put `text` on the clipboard;
/// it works over SSH too, because the terminal, not this machine, holds it.
pub(super) fn copy_sequence(text: &str) -> String {
    format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text)
    )
}

/// Writes [`copy_sequence`] to the terminal. Called from the terminal thread,
/// between draws.
pub(super) fn copy(text: &str) {
    let mut stdout = io::stdout();
    let _ = stdout.write_all(copy_sequence(text).as_bytes());
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_the_whole_text_base64_encoded_in_one_osc_52_sequence() {
        assert_eq!(
            copy_sequence("https://claude.ai/oauth/authorize?scope=a&state=b"),
            "\u{1b}]52;c;aHR0cHM6Ly9jbGF1ZGUuYWkvb2F1dGgvYXV0aG9yaXplP3Njb3BlPWEmc3RhdGU9Yg==\u{7}"
        );
    }

    #[test]
    fn only_an_https_link_is_opened() {
        assert!(!open("file:///etc/passwd"));
        assert!(!open("javascript:alert(1)"));
    }
}
