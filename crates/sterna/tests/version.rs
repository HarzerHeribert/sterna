//! `sterna --version` prints the crate version and nothing else, and exits 0 --
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
            .output()
            .expect("the binary runs");
        assert!(output.status.success(), "{flag}: {:?}", output.status);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("sterna {}\n", env!("CARGO_PKG_VERSION")),
            "{flag} must print exactly the crate version"
        );
        assert!(output.stderr.is_empty(), "{flag} wrote to stderr");
    }
}
