//! `sterna --version` prints the release (or crate) version and nothing else, and exits 0 --
//! the one line that tells a release archive from a build (the primary's item
//! of 2026-09-06 07:16).

use std::process::Command;

#[test]
fn version_prints_the_crate_version_and_nothing_else() {
    let root = std::env::temp_dir().join(format!("sterna-version-{}", std::process::id()));
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sterna"))
            .arg(flag)
            .env("XDG_CONFIG_HOME", root.join("global-config"))
            // Piped, the tern is not drawn even where colour is on offer.
            .env("COLORTERM", "truecolor")
            .output()
            .expect("the binary runs");
        assert!(output.status.success(), "{flag}: {:?}", output.status);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("sterna {}\n", sterna::VERSION),
            "{flag} must print exactly the crate version"
        );
        assert!(output.stderr.is_empty(), "{flag} wrote to stderr");
    }
}

/// On a terminal that shows true colour the flying tern stands above the
/// version; the version is still the last line.
#[cfg(unix)]
#[test]
fn on_a_terminal_the_version_stands_under_the_tern() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("a pty");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sterna"));
    command.arg("--version");
    command.env("COLORTERM", "truecolor");
    let mut child = pair.slave.spawn_command(command).expect("the binary runs");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut output = Vec::new();
    let mut buffer = [0u8; 4096];
    // The reader ends with an error once the child is gone and the pty
    // closes; what arrived before that is the whole output.
    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..n]);
    }
    assert!(child.wait().unwrap().success());
    let output = String::from_utf8_lossy(&output);
    // The tern's red bill, in true colour, over half blocks.
    assert!(output.contains("38;2;207;43;43"), "{output:?}");
    assert!(output.contains('▀'), "{output:?}");
    assert!(
        output
            .trim_end()
            .ends_with(&format!("sterna {}", env!("CARGO_PKG_VERSION"))),
        "{output:?}"
    );
}
