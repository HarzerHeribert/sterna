//! Acceptance for map line 2455's platform appliers: a compiled `Profile`
//! becomes an OS sandbox on macOS, Linux and Windows. Each test names the
//! invariant of `docs/sandbox.md` it holds.
//!
//! **Nothing model-authored runs here — map line 2457.** Every process this
//! file spawns is `/bin/cat` with a fixed argv over a file this file wrote.
//! There is no generated code, no shell string built from a template, and no
//! input from anywhere but these tests.
//!
//! And no test asserts that a sandbox works by relying on the sandbox: the
//! execution tests each prove the *same fixed argv* reaches the path without
//! confinement and fails with it. A sandbox that refused everything,
//! including the loader, would fail the unconfined half and be caught. The
//! exec-grant tests carry that further: each proves a program under the
//! executable roots **runs** under the same profile that refuses a binary
//! outside them, so a confinement that denied both would fail.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::sandbox::profile::{Access, Profile};
use sterna::sandbox::{linux, macos, windows};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The binary the profile-text tests are rendered for. A real absolute path
/// on every host, because that is what sterna hands an applier for a program
/// it resolved — `tools::invoke::exec_grant` produces the resolved case with
/// `canonicalize`, which yields nothing else. `Path::is_absolute` needs
/// a drive under Windows path semantics, which `/bin/cat` does not carry, so
/// the constant is drive-qualified there and unchanged elsewhere.
#[cfg(windows)]
const RESOLVED: &str = r"C:\bin\cat.exe";
#[cfg(not(windows))]
const RESOLVED: &str = "/bin/cat";

/// A name that resolves to no binary anywhere, for the fallback half. It is
/// relative, which is what an unresolved name is: `exec_grant` hands the
/// applier the program name back when `PATH` did not find it.
const UNRESOLVED: &str = "sterna-sandbox-apply-no-such-program";

/// A resolved binary that lies outside every [`LOADER_READ_ROOT`] and outside
/// any project root — `~/.cargo/bin/cargo` is the real one. A literal string
/// and no file, because `profile_text` is a pure function of its two
/// arguments; the executing half of this case is
/// `a_resolved_binary_outside_the_read_roots_still_starts`. Drive-qualified
/// under Windows for the same reason [`RESOLVED`] is.
#[cfg(windows)]
const OUTSIDE_READ_ROOTS: &str = r"C:\sterna-fixture\elsewhere\cat.exe";
#[cfg(not(windows))]
const OUTSIDE_READ_ROOTS: &str = "/sterna-fixture/elsewhere/cat";

/// The Windows applier's source, for the one invariant that is a property of
/// the prose rather than of any call: the job object is a lifetime primitive
/// and its documentation has to say so first, because the map line's phrase
/// "Windows job objects" reads as though it were the grant mechanism.
const WINDOWS_SOURCE: &str = include_str!("../src/sandbox/windows.rs");

/// A throwaway project directory with a `.claude/` in it, removed when the
/// test finishes.
struct Fixture {
    root: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let stem = format!("sterna-sandbox-apply-{}-{label}-{n}", std::process::id());
        let root = std::env::temp_dir().join(&stem);
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let outside = std::env::temp_dir().join(format!("{stem}-outside"));
        std::fs::create_dir_all(&outside).unwrap();
        Self { root, outside }
    }

    /// The profile for this fixture, compiled from `settings`.
    fn profile(&self, settings: Option<&str>) -> Profile {
        Profile::compile(&self.root, settings)
    }

    /// The root as `Profile` resolved it. Every assertion uses this rather
    /// than `self.root`: on macOS `temp_dir()` is `/var/folders/…` and its
    /// realpath is `/private/var/folders/…`, and a test comparing the two
    /// spellings would be testing the wrong thing.
    fn resolved(&self, profile: &Profile) -> PathBuf {
        profile.root().to_path_buf()
    }

    /// Used only by the macOS and Linux execution tests, which are the only
    /// ones that put a real file in front of a confined process. Windows has
    /// no applier it can run, so the helper has no caller there and
    /// `warnings = deny` makes a dead one a build failure.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn write(&self, path: &Path, contents: &str) -> PathBuf {
        std::fs::write(path, contents).unwrap();
        path.to_path_buf()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.outside);
    }
}

/// A settings document granting the project root and one path outside it.
fn settings_for(root: &Path) -> String {
    let root = root.to_string_lossy().replace('\\', "/");
    format!(
        r#"{{"permissions":{{"allow":["Read({root}/**)","Edit({root}/src/**/*.rs)","Bash(cargo test*)"],"deny":["Read({root}/secrets/**)"]}}}}"#
    )
}

// --- macOS: the profile text -------------------------------------------

/// One filter inside a seatbelt term: `(subpath "/usr")` is
/// `("subpath", "/usr")`, `(global-name "com.apple.x")` is
/// `("global-name", "com.apple.x")`. Unquoted filters such as
/// `(target self)` carry no path and are not collected.
#[derive(Debug, PartialEq, Eq)]
struct Filter {
    term: String,
    form: String,
    value: String,
}

/// `$HOME` as the platform names it: `HOME` on Unix, `USERPROFILE` on
/// Windows. `sandbox_profile.rs` carries the same helper for the same
/// reason -- `std::env::var("HOME")` is `NotPresent` on a Windows runner.
fn home() -> PathBuf {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            return PathBuf::from(value);
        }
    }
    panic!("neither HOME nor USERPROFILE is set")
}

/// `path` as the seatbelt renderer quotes it: wrapped in `"`, with `"` and
/// `\` escaped. On macOS and Linux no fixture path contains either, so this
/// is the spelling plus quotes; on Windows every separator is a `\` and the
/// rendered text doubles it, which is what the profile-text assertions
/// below must compare against rather than the bare `display()`.
fn quoted(path: &Path) -> String {
    let mut out = String::from("\"");
    for ch in path.to_string_lossy().chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Every `(allow …)` / `(deny …)` line of a profile, split into the term it
/// names and the quoted filters it carries.
///
/// This is what makes the allow-set assertable *positively*. A substring
/// test for `$HOME` — which is what this file used to do — passes for
/// `(subpath "/Users")`, passes for `(literal "/")`, and never looks at a
/// non-path term such as `mach-lookup` at all, so a blanket grant of every
/// Mach service on the machine sat inside a green test file. Parsing the
/// terms means a new root, a new operation, or a new service has to be
/// declared below or the test fails.
fn parse(text: &str) -> (Vec<String>, Vec<Filter>) {
    let mut names = Vec::new();
    let mut filters = Vec::new();
    for line in text.lines() {
        let Some(rest) = line
            .strip_prefix("(allow ")
            .or_else(|| line.strip_prefix("(deny "))
        else {
            assert!(
                line.starts_with("(version ") || line.is_empty(),
                "a profile line that is neither a version nor an allow/deny term: {line}"
            );
            continue;
        };
        let term: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ')')
            .collect();
        names.push(term.clone());

        // `(<form> "<value>")`, with the escaping `quote()` applies.
        let bytes: Vec<char> = rest.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != '(' {
                i += 1;
                continue;
            }
            let form: String = bytes[i + 1..]
                .iter()
                .take_while(|c| !c.is_whitespace() && **c != ')')
                .collect();
            let mut j = i + 1 + form.chars().count();
            while j < bytes.len() && bytes[j] != '"' && bytes[j] != ')' {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] == ')' {
                i += 1;
                continue;
            }
            let mut value = String::new();
            j += 1;
            while j < bytes.len() && bytes[j] != '"' {
                if bytes[j] == '\\' {
                    j += 1;
                }
                value.push(bytes[j]);
                j += 1;
            }
            filters.push(Filter {
                term: term.clone(),
                form,
                value,
            });
            i = j + 1;
        }
    }
    (names, filters)
}

/// Every term the seatbelt profile is permitted to name. A term absent from
/// here is a test failure whether or not it carries a path, which is the
/// half the old substring filter had no way to see.
const EXPECTED_TERMS: &[&str] = &[
    "default",
    "file-read-metadata",
    "file-read*",
    "process-exec*",
    "process-fork",
    "signal",
    "sysctl-read",
    "file-write-data",
    "file-ioctl",
    "file-write*",
    "network*",
];

/// Every path the profile is permitted to name when sterna resolved the
/// binary, other than the project root, its `.claude`, and the binary
/// itself. Spelled out here rather than read from the applier's own
/// constants, so adding a root there fails this test instead of travelling
/// with it.
///
/// The executable roots and package prefixes are listed apart, in
/// [`EXPECTED_EXEC_ROOTS`] and [`EXPECTED_PACKAGE_PREFIXES`], because they
/// are named by the exec term alone.
const EXPECTED_PATHS: &[&str] = &[
    "/",
    "/bin",
    "/sbin",
    "/usr",
    "/etc",
    "/private/etc",
    "/System",
    "/Library",
    "/opt/homebrew",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/dev/null",
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/dtracehelper",
];

/// The exec roots every profile names: any program under them may be started.
/// `/bin` and `/sbin` are in [`EXPECTED_PATHS`] too, because the loader reads
/// them as well.
const EXPECTED_EXEC_ROOTS: &[&str] = &[
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/usr/local/bin",
    "/opt/homebrew/bin",
];

/// The package prefixes the executable roots are symlinks into (Homebrew's
/// `bin/git` resolves under `Cellar/`), exec-granted beside them.
const EXPECTED_PACKAGE_PREFIXES: &[&str] = &["/opt/homebrew", "/usr/local"];

/// Every `(allow process-exec* …)` filter of `text`.
fn exec_filters(text: &str) -> Vec<Filter> {
    let (_, filters) = parse(text);
    filters
        .into_iter()
        .filter(|f| f.term == "process-exec*")
        .collect()
}

/// The toolchain subtrees this machine's profile derived, as strings.
///
/// Computed from the profile rather than listed, because they come from the
/// environment: a machine with no rustup installed derives none, and an
/// expectation that spelled them would pass here and fail in CI.
fn toolchain(profile: &Profile) -> Vec<String> {
    profile
        .toolchain_roots()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

#[test]
fn the_allow_set_is_exactly_the_declared_terms() {
    let fixture = Fixture::new("default");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let text = macos::profile_text(&profile, Path::new(RESOLVED));

    // §3's shape, in the order a seatbelt profile is read.
    assert!(text.starts_with("(version 1)\n(deny default)\n"), "{text}");
    assert!(
        text.contains(&format!("(allow file-read* (subpath {}))", quoted(&root))),
        "{text}"
    );
    assert!(
        text.contains(&format!("(allow file-write* (subpath {}))", quoted(&root))),
        "{text}"
    );

    let (names, filters) = parse(&text);
    assert!(!filters.is_empty(), "the parser found no filters: {text}");

    // Positively, term by term: the set of operations this profile speaks
    // about is exactly the declared one. A new operation — `mach-lookup`,
    // `ipc-posix-shm`, `file-read-xattr` — fails here by construction.
    assert_eq!(
        sorted(names),
        sorted(EXPECTED_TERMS.iter().map(|t| t.to_string()).collect()),
        "{text}"
    );

    // Positively, path by path: the set of paths is exactly the declared
    // system machinery, the executable roots, plus the project root and its
    // `.claude`.
    let mut expected: Vec<String> = EXPECTED_PATHS
        .iter()
        .chain(EXPECTED_EXEC_ROOTS)
        .chain(EXPECTED_PACKAGE_PREFIXES)
        .map(|p| p.to_string())
        .collect();
    expected.push(RESOLVED.to_string());
    expected.push(root.to_string_lossy().into_owned());
    expected.push(root.join(".claude").to_string_lossy().into_owned());
    expected.push(root.join(".sterna").to_string_lossy().into_owned());
    expected.push(root.join(".sterna/scratch").to_string_lossy().into_owned());
    // The derived grants: the toolchain a build reads, and the cargo
    // credential file carved back out of it. Computed from the profile
    // because they come from this machine's environment.
    expected.extend(toolchain(&profile));
    expected.extend(
        profile
            .toolchain_read_files()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    expected.extend(
        profile
            .toolchain_credentials()
            .iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    assert_eq!(
        sorted(filters.iter().map(|f| f.value.clone()).collect()),
        sorted(expected),
        "{text}"
    );

    // §4.3, and it holds however the two lists above are edited: no subtree
    // grant may be an ancestor of `$HOME`. `(subpath "/Users")` is what the
    // old `$HOME`-substring filter let through; `(literal "/")` is a single
    // directory entry and not a subtree, which is why the form matters.
    let home = home();
    for filter in &filters {
        if filter.form == "subpath" {
            let granted = PathBuf::from(&filter.value);
            assert!(
                !home.starts_with(&granted) || granted.starts_with(&root),
                "a subtree grant contains $HOME: {filter:?}"
            );
        }
    }

    // §2: a `Bash` pattern grants no file access. `Bash(cargo test*)` is in
    // the document above and must leave no trace here. The *pattern* is what
    // may not appear: `$CARGO_HOME` is granted by the derived toolchain rule
    // above, which no document can ask for and which grants no command.
    assert!(!text.contains("cargo test"), "{text}");
    for filter in &filters {
        assert!(
            !filter.value.contains("cargo test"),
            "a command pattern reached the profile: {filter:?}"
        );
    }
}

#[test]
fn the_seatbelt_profile_names_every_mach_service_it_permits() {
    // §4.2: the Keychain is never grantable on any platform, and an
    // unfiltered `(allow mach-lookup)` grants it — `securityd` does the
    // keychain read on the caller's behalf, so the file rules never see it.
    // The measured base set is empty: no tool in scope needed a service.
    const EXPECTED_MACH_SERVICES: &[&str] = &[];

    let fixture = Fixture::new("mach");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let text = macos::profile_text(&profile, Path::new(RESOLVED));

    // No blanket term, in any spelling: every `mach-lookup` line must carry
    // at least one `global-name` filter.
    for line in text.lines() {
        if line.contains("mach-lookup") {
            assert!(
                line.contains("(global-name \""),
                "a mach-lookup term with no global-name filter: {line}"
            );
        }
    }

    // And the services it does name are exactly the declared ones.
    let (_, filters) = parse(&text);
    let named: Vec<String> = filters
        .iter()
        .filter(|f| f.term == "mach-lookup")
        .map(|f| f.value.clone())
        .collect();
    assert_eq!(
        named,
        EXPECTED_MACH_SERVICES
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "{text}"
    );
    // Whatever that list grows to hold, §4.2 excludes the credential store.
    for service in &named {
        let lowered = service.to_lowercase();
        assert!(
            !lowered.contains("securityd") && !lowered.contains("securityserver"),
            "a keychain endpoint is never grantable: {service}"
        );
    }
}

#[test]
fn a_confined_process_cannot_reach_the_keychain() {
    #[cfg(not(target_os = "macos"))]
    eprintln!(
        "skipped: securityd and seatbelt are macOS; §4.2's other platforms are not this test"
    );
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        // §4.2, demonstrated rather than read off the profile text. The
        // query names an item that does not exist, so no keychain ACL
        // prompt can fire and no stored secret is touched: what is being
        // watched is whether securityd answers *at all*.
        const ABSENT: &str = "__sterna-sandbox-apply-nonexistent-item__";
        /// The answer only securityd can give: the search ran and the item
        /// is not there.
        const AUTHORITATIVE: &str = "could not be found in the keychain";
        /// The client-side failure of a process that has no Mach service to
        /// ask. `security` prints this *and then* prints its own
        /// item-not-found line, which is why the presence of the
        /// authoritative sentence alone proves nothing.
        const CLIENT_SIDE: &str = "were not valid";

        /// Whether securityd itself answered: the search was created and
        /// the item was not found, with no client-side failure alongside.
        fn asked_securityd(stderr: &str) -> bool {
            stderr.contains(AUTHORITATIVE) && !stderr.contains(CLIENT_SIDE)
        }

        let fixture = Fixture::new("keychain");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));

        let security = |args: &[&str], confined: bool| {
            let mut command = Command::new("/usr/bin/security");
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if confined {
                macos::confine(&profile, Path::new("/usr/bin/security"), &mut command).unwrap();
            }
            let out = command.output().unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };

        // The unconfined halves, and they are controls rather than
        // assertions about the sandbox: they establish that securityd is
        // reachable and answering on this host at all. Where it is not — a
        // machine with no keychain in the session, or a macOS whose error
        // text has moved — the experiment cannot run, and the test says so
        // instead of passing quietly.
        let (listed, keychains, _) = security(&["list-keychains"], false);
        let (_, _, absent) = security(&["find-generic-password", "-s", ABSENT], false);
        if !listed || !keychains.contains("keychain") || !asked_securityd(&absent) {
            eprintln!(
                "skipped: securityd is not answering unconfined on this host, so the \
                 confined half would prove nothing: {keychains:?} {absent:?}"
            );
            return;
        }

        // The same two calls, confined. The keychain search list cannot be
        // read at all, and the search for the absent item never gets an
        // authoritative answer: it fails in the client, because there is no
        // Mach service to ask.
        let (listed, keychains, _) = security(&["list-keychains"], true);
        assert!(
            !listed,
            "the confined process listed the keychains: {keychains}"
        );
        assert!(
            !keychains.contains("keychain-db"),
            "the confined process read the keychain search list: {keychains}"
        );
        let (_, _, confined_absent) = security(&["find-generic-password", "-s", ABSENT], true);
        assert!(
            !asked_securityd(&confined_absent),
            "securityd answered a confined process authoritatively: {confined_absent}"
        );
        assert!(
            confined_absent.contains(CLIENT_SIDE),
            "the confined query failed for some reason other than the missing Mach service: \
             {confined_absent}"
        );
    }
}

#[test]
fn the_macos_profile_denies_network_unconditionally() {
    let fixture = Fixture::new("network");
    // §4.1: no pattern names a host, a port or a protocol, so there is no
    // document that can put a network grant in. All three of these try.
    for settings in [
        None,
        Some(r#"{"permissions":{"allow":["WebFetch(domain:example.com)"]}}"#),
        Some(r#"{"permissions":{"allow":["WebFetch","WebSearch","Bash(curl*)"]}}"#),
    ] {
        let profile = fixture.profile(settings);
        let text = macos::profile_text(&profile, Path::new(RESOLVED));
        assert!(text.contains("(deny network*)"), "{settings:?}: {text}");
        assert!(!profile.grants_network(), "{settings:?}");
        assert!(!text.contains("(allow network"), "{settings:?}: {text}");
    }
}

#[test]
fn the_macos_profile_denies_writing_dot_claude_inside_the_project() {
    let fixture = Fixture::new("dotclaude");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let text = macos::profile_text(&profile, Path::new(RESOLVED));

    // §1.5: `.claude/` is inside the writable root, so a program that could
    // write it could widen the profile it was derived from.
    let deny = format!(
        "(deny file-write* (subpath {}))",
        quoted(&root.join(".claude"))
    );
    assert!(text.contains(&deny), "{text}");
    // The deny follows the root's write allow, because seatbelt takes the
    // last matching term and the reverse order would grant what this
    // refuses.
    let allow_at = text
        .find(&format!("(allow file-write* (subpath {}))", quoted(&root)))
        .unwrap();
    assert!(text.find(&deny).unwrap() > allow_at, "{text}");
    // Reading it stays granted: `settings.json` is read before the sandbox
    // is entered.
    assert!(!text.contains("(deny file-read* (subpath"), "{text}");
    assert!(
        profile
            .check("read", Access::Read, &root.join(".claude/settings.json"))
            .is_ok()
    );
}

/// §1.5's one exemption, rendered the way the profile decides it: the
/// scratchpad's write allow follows the `.sterna` deny (seatbelt takes the last
/// matching term), and `.sterna` itself is still denied.
#[test]
fn every_applier_carves_the_scratchpad_out_of_dot_sterna() {
    let fixture = Fixture::new("scratch-text");
    std::fs::create_dir_all(fixture.root.join(".sterna/scratch")).unwrap();
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let text = macos::profile_text(&profile, Path::new(RESOLVED));
    let deny = format!(
        "(deny file-write* (subpath {}))",
        quoted(&root.join(".sterna"))
    );
    let allow = format!(
        "(allow file-write* (subpath {}))",
        quoted(&root.join(".sterna/scratch"))
    );
    let deny_at = text.find(&deny).unwrap_or_else(|| panic!("{text}"));
    let allow_at = text.find(&allow).unwrap_or_else(|| panic!("{text}"));
    assert!(deny_at < allow_at, "{text}");

    let argv: Vec<String> = linux::bwrap_argv(&profile, "/bin/cat".as_ref(), &[])
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let at = |flag: &str, path: &Path| {
        let path = path.to_string_lossy();
        argv.windows(3)
            .position(|w| w[0] == flag && w[1] == path && w[2] == path)
            .unwrap_or_else(|| panic!("{flag} {path}: {argv:?}"))
    };
    assert!(at("--ro-bind", &root.join(".sterna")) < at("--bind", &root.join(".sterna/scratch")));

    // A link in the scratchpad's place is not the scratchpad: no applier
    // grants it, and `.sterna` stays denied.
    #[cfg(unix)]
    {
        std::fs::remove_dir(fixture.root.join(".sterna/scratch")).unwrap();
        std::os::unix::fs::symlink("..", fixture.root.join(".sterna/scratch")).unwrap();
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let text = macos::profile_text(&profile, Path::new(RESOLVED));
        assert!(text.contains(&deny) && !text.contains(&allow), "{text}");
        let grants = windows::acl_grants(&profile, Path::new(RESOLVED));
        assert_eq!(grants.read_write, vec![root.clone()], "{grants:?}");
    }
}

/// The seatbelt the carve-out renders, applied: a confined child writes the
/// scratchpad and cannot write `.sterna/config.toml` directly or through a link
/// planted in the scratchpad.
#[test]
fn a_confined_child_writes_the_scratchpad_and_not_the_host_configuration() {
    #[cfg(not(target_os = "macos"))]
    eprintln!(
        "skipped: seatbelt is macOS-only; Linux's Landlock cannot carve `.sterna` at all (see landlock_alone_does_not_enforce_the_dot_claude_carve_out_and_the_mount_view_does), and Windows is the sterna (windows-latest) cell"
    );
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        let fixture = Fixture::new("scratch-exec");
        std::fs::create_dir_all(fixture.root.join(".sterna/scratch")).unwrap();
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let config = fixture.write(&root.join(".sterna/config.toml"), "host\n");
        std::os::unix::fs::symlink("../config.toml", root.join(".sterna/scratch/link")).unwrap();
        let sh = |script: &str| {
            let mut command = Command::new("/bin/bash");
            command
                .arg("-c")
                .arg(script)
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            macos::confine(&profile, Path::new("/bin/bash"), &mut command).unwrap();
            command.output().unwrap()
        };

        let scratch = sh("echo note > .sterna/scratch/x");
        assert!(scratch.status.success(), "{scratch:?}");
        assert_eq!(
            std::fs::read_to_string(root.join(".sterna/scratch/x")).unwrap(),
            "note\n"
        );
        for script in [
            "echo changed > .sterna/config.toml",
            "echo changed > .sterna/scratch/../config.toml",
            "echo changed > .sterna/scratch/link",
        ] {
            let refused = sh(script);
            assert!(!refused.status.success(), "{script}: {refused:?}");
            assert_eq!(
                std::fs::read_to_string(&config).unwrap(),
                "host\n",
                "{script}"
            );
        }
    }
}

/// The OS layer's answer to a hard link, measured against the real seatbelt
/// (docs/sandbox.md, invariant 5): a confined child cannot link a file out of a
/// `(deny file-write* (subpath …))` subtree — `link(2)` is judged on the
/// source too — so `.sterna/**` and `.claude/**` cannot gain a second name from
/// inside the sandbox. A link to an ordinary project file is created, and the
/// in-process check is then what refuses a write through it.
#[test]
fn a_confined_child_cannot_hard_link_a_never_writable_file_into_the_writable_tree() {
    #[cfg(not(target_os = "macos"))]
    eprintln!(
        "skipped: seatbelt is macOS-only; Linux and Windows are not measured here (docs/sandbox.md, invariant 5)"
    );
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};
        use sterna::sandbox::profile::Access;

        let fixture = Fixture::new("hard-link-exec");
        std::fs::create_dir_all(fixture.root.join(".sterna/scratch")).unwrap();
        std::fs::create_dir_all(fixture.root.join(".git")).unwrap();
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        fixture.write(&root.join(".sterna/config.toml"), "host\n");
        fixture.write(&root.join(".claude/settings.json"), "{}\n");
        fixture.write(&root.join(".git/config"), "git\n");
        let ln = |source: &str, target: &str| {
            let mut command = Command::new("/bin/ln");
            command
                .arg(source)
                .arg(target)
                .current_dir(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            macos::confine(&profile, Path::new("/bin/ln"), &mut command).unwrap();
            command.output().unwrap()
        };

        for (source, target) in [
            (".sterna/config.toml", ".sterna/scratch/hard"),
            (".claude/settings.json", ".sterna/scratch/hard-claude"),
            (".claude/settings.json", "hard-claude"),
        ] {
            let refused = ln(source, target);
            assert!(
                !refused.status.success(),
                "{source} -> {target}: {refused:?}"
            );
            assert!(
                String::from_utf8_lossy(&refused.stderr).contains("Operation not permitted"),
                "{source} -> {target}: {refused:?}"
            );
            assert!(!root.join(target).exists(), "{source} -> {target}");
        }

        // `.git/config` is an ordinary project file (§4 names `~/.gitconfig`,
        // not the project's own), so the seatbelt links it; the profile is
        // what refuses the write through either name.
        for target in ["hard-git", ".sterna/scratch/hard-git"] {
            let linked = ln(".git/config", target);
            assert!(linked.status.success(), "{target}: {linked:?}");
            let denied = profile
                .check_request("write", Access::Write, &root.join(target))
                .expect_err("a write through a hard link is refused in-process");
            assert!(denied.rule.starts_with("hard-linked file ("), "{denied}");
        }
        assert_eq!(
            std::fs::read_to_string(root.join(".git/config")).unwrap(),
            "git\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(".sterna/config.toml")).unwrap(),
            "host\n"
        );
    }
}

/// A plan is an in-process narrowing only: the OS layer a narrowed profile
/// renders is the session profile's, byte for byte, so everything the OS
/// refuses stays refused while planning.
#[test]
fn a_plan_leaves_the_os_sandbox_exactly_as_the_profile_renders_it() {
    use sterna::sandbox::modes::RequestMode;
    let fixture = Fixture::new("request-mode-os");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let narrowed = profile.clone().narrowed_to(RequestMode::Plan);
    assert_eq!(
        macos::profile_text(&narrowed, Path::new(RESOLVED)),
        macos::profile_text(&profile, Path::new(RESOLVED)),
    );
    assert_eq!(
        format!(
            "{:?}",
            linux::landlock_rules(&narrowed, Path::new(RESOLVED))
        ),
        format!("{:?}", linux::landlock_rules(&profile, Path::new(RESOLVED))),
    );
    // Windows refuses exec of a program the session could write; that probe
    // must keep the session's answer while planning.
    let program = fixture.root.join("tool.exe");
    assert!(narrowed.check("exec", Access::Write, &program).is_ok());
    assert!(
        narrowed
            .check_request("write", Access::Write, &program)
            .is_err()
    );
    for outside in [
        fixture.outside.join("secret.txt"),
        fixture.root.join("secrets/key"),
    ] {
        assert_eq!(
            format!("{:?}", narrowed.check("read", Access::Read, &outside)),
            format!("{:?}", profile.check("read", Access::Read, &outside)),
        );
    }
}

// --- macOS: the sandbox, actually applied ------------------------------

#[test]
fn a_sandboxed_process_cannot_read_outside_the_project_but_an_unsandboxed_one_can() {
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!(
            "skipped: seatbelt is macOS-only; the Linux equivalent is \
             a_landlocked_process_cannot_read_outside_the_project_but_an_unsandboxed_one_can"
        );
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        let fixture = Fixture::new("exec");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let inside = fixture.write(&root.join("inside.txt"), "inside-secret\n");
        let outside = fixture.write(&fixture.outside.join("outside.txt"), "outside-secret\n");

        // A fixed argv over a file this test wrote. Nothing generated.
        let cat = |path: &Path, confined: bool| {
            let mut command = Command::new("/bin/cat");
            command
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if confined {
                macos::confine(&profile, Path::new("/bin/cat"), &mut command).unwrap();
            }
            command.output().unwrap()
        };

        // The unconfined half. Without it a sandbox that refused the loader
        // itself — or a path that simply did not exist — would pass the
        // assertion below for the wrong reason.
        let free = cat(&outside, false);
        assert!(free.status.success(), "{free:?}");
        assert_eq!(String::from_utf8_lossy(&free.stdout), "outside-secret\n");

        // The same argv, confined.
        let confined = cat(&outside, true);
        assert!(!confined.status.success(), "{confined:?}");
        assert!(
            !String::from_utf8_lossy(&confined.stdout).contains("outside-secret"),
            "{confined:?}"
        );

        // And the sandbox is not simply refusing everything: the same
        // program reads the project through it.
        let granted = cat(&inside, true);
        assert!(granted.status.success(), "{granted:?}");
        assert_eq!(
            String::from_utf8_lossy(&granted.stdout),
            "inside-secret\n",
            "{granted:?}"
        );
    }
}

// --- every platform: the regime is reported ----------------------------

#[test]
fn the_reported_regime_matches_what_was_applied() {
    let fixture = Fixture::new("regime");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));

    // macOS. The count is the profile's own, and the sentence says the OS
    // layer is coarser than the pattern rather than implying it is not.
    let regime = macos::regime(&profile, Path::new(RESOLVED));
    assert_eq!(
        regime,
        macos::Regime::ProjectRootOnly {
            path_rules: profile.rule_count(),
            exec: macos::ExecScope::RootsAndProject,
        }
    );
    assert!(profile.rule_count() > 0, "the fixture has path rules");
    assert!(regime.describe().contains("directory-granular"), "{regime}");
    assert!(regime.describe().contains("pre-call check"), "{regime}");
    // §3: the coarseness is stated, not left to be discovered. Metadata is
    // readable filesystem-wide and `readlink(2)` is a metadata operation, so
    // a symlink's target is disclosed anywhere — and no Mach service is
    // reachable, which is §4.2's half of the same sentence.
    assert!(regime.describe().contains("metadata"), "{regime}");
    assert!(regime.describe().contains("symlink"), "{regime}");
    assert!(regime.describe().contains("no Mach service"), "{regime}");
    // Which exec grant is in force is in the sentence, and it is the same
    // whether or not the program name resolved.
    assert!(
        regime.describe().contains(
            "execution is granted on the declared executable roots and on the project root"
        ),
        "{regime}"
    );
    assert_eq!(macos::regime(&profile, Path::new(UNRESOLVED)), regime);

    // Linux. Every regime names what it does and does not enforce; the two
    // without a mount view say the network is still there.
    for coarse in [
        linux::Regime::LandlockOnly { abi: 3 },
        linux::Regime::Unconfined,
    ] {
        assert!(!coarse.removes_network(), "{coarse}");
    }
    for full in [
        linux::Regime::BubblewrapAndLandlock { abi: 4 },
        linux::Regime::BubblewrapOnly,
        linux::Regime::LandlockAndSeccomp { abi: 3 },
    ] {
        assert!(full.removes_network(), "{full}");
    }
    assert!(
        linux::Regime::BubblewrapOnly
            .describe()
            .contains("no Landlock"),
        "a coarser regime must say so"
    );
    assert!(
        linux::Regime::BubblewrapAndLandlock { abi: 3 }
            .describe()
            .contains("no glob"),
        "Landlock's missing glob must be stated"
    );

    // Windows. There are two regimes and no third: the AppContainer, or a
    // refusal. A `WRITE_RESTRICTED`-token-only regime used to be a variant
    // here, reported `removes_network() == false`, and isolated nothing --
    // it cannot reach a spawn now because it does not exist.
    // Said as a property of the source rather than of an array this test
    // wrote: nothing named `RestrictedToken` exists, so no half-confinement
    // can be constructed, reported, or reached from a spawn.
    assert!(
        !WINDOWS_SOURCE.contains("RestrictedToken"),
        "a restricted-token regime removes write reach without isolating \
         anything and reports a confinement it cannot back; it must not be \
         reachable"
    );
    for regime in [windows::Regime::AppContainer, windows::Regime::Unconfined] {
        assert!(!regime.describe().is_empty(), "{regime:?}");
    }
    let cage = windows::Regime::AppContainer.describe();
    // §4.1's network claim is not a claim the access check can back, so the
    // sentence names the service that does back it rather than staying silent.
    assert!(cage.contains("Windows Firewall service"), "{cage}");
    // Invariant 3 says there is one writable root. On this platform there
    // are two by construction, and the sentence says so rather than
    // pretending otherwise.
    assert!(cage.contains("two writable roots"), "{cage}");
    assert!(cage.contains("%LOCALAPPDATA%\\Packages"), "{cage}");
    // The container SID is per user as well as per project -- the security
    // property `container_name` exists for.
    assert!(cage.contains("this user's own SID"), "{cage}");
    assert!(
        windows::Regime::Unconfined
            .describe()
            .contains("refusal, not a degraded mode"),
        "an unconfinable host must say it spawned nothing"
    );

    #[cfg(target_os = "linux")]
    {
        // The host's own answer, whatever it is. Asserting a particular
        // regime here would be asserting the CI image's kernel.
        let live = linux::regime();
        assert!(!live.describe().is_empty(), "{live}");

        // What was APPLIED, not what is installed. Nothing in this package
        // spawns `bwrap` — `bwrap_argv` builds a value and the spawn path
        // that would run it is 61E's — so a mount-view regime here would
        // report an enforcement nobody installed, and `removes_network`
        // would answer `true` for a network nothing removed. That is §4.1
        // claimed as enforced while it is not, which is the specific failure
        // §3 exists to prevent.
        assert!(
            matches!(
                live,
                linux::Regime::LandlockAndSeccomp { .. } | linux::Regime::Unconfined
            ),
            "regime() reported a regime this package cannot apply: {live:?}"
        );
        assert_eq!(
            live.removes_network(),
            matches!(live, linux::Regime::LandlockAndSeccomp { .. })
        );
        let abi = linux::landlock_abi();
        assert_eq!(
            live,
            if abi >= 3 && linux::seccomp_supported_arch() {
                linux::Regime::LandlockAndSeccomp { abi }
            } else {
                linux::Regime::Unconfined
            },
            "{live:?} at ABI {abi}"
        );
        // The host-capability question still has an answer; it has a
        // different name now, and it is allowed to be the wider one.
        assert!(!linux::available_regime().describe().is_empty());
    }
    #[cfg(not(target_os = "linux"))]
    eprintln!("skipped: linux::regime() probes the running kernel's Landlock ABI");
}

// --- every platform: nothing widens ------------------------------------

#[test]
fn no_runtime_input_can_widen_a_grant() {
    // (a) The profile decides, not the applier. A document whose `deny`
    // covers the project root produces no writable directory. Linux still
    // grants the one /dev/null sink needed by ordinary command redirection.
    let fixture = Fixture::new("widen");
    let root_pattern = fixture.root.to_string_lossy().replace('\\', "/");
    let denied = fixture.profile(Some(&format!(
        r#"{{"permissions":{{"allow":["Write({root_pattern}/**)"],"deny":["Write({root_pattern}/**)"]}}}}"#
    )));
    let text = macos::profile_text(&denied, Path::new(RESOLVED));
    assert!(!text.contains("(allow file-write* (subpath"), "{text}");
    assert_eq!(
        linux::landlock_rules(&denied, Path::new(RESOLVED)).read_write,
        vec![PathBuf::from("/dev/null")],
        "{:?}",
        linux::landlock_rules(&denied, Path::new(RESOLVED))
    );
    assert!(
        windows::acl_grants(&denied, Path::new(RESOLVED))
            .read_write
            .is_empty(),
        "{:?}",
        windows::acl_grants(&denied, Path::new(RESOLVED))
    );
    let argv = linux::bwrap_argv(&denied, "/bin/cat".as_ref(), &[]);
    assert!(
        !argv.iter().any(|arg| arg == "--bind"),
        "a denied root gets no read-write bind: {argv:?}"
    );

    // (b) A project directory name cannot close a profile term and be read
    // as more profile. This is the only place an attacker-shaped string
    // reaches the generated text at all.
    let evil = std::env::temp_dir().join(format!(
        "sterna-sbx-{}-a\"b\\c) (allow file-write* (subpath \"/",
        std::process::id()
    ));
    // Best effort: `"` is not a legal filename character on Windows, so the
    // directory cannot exist there and `create_dir_all` fails with
    // ERROR_INVALID_NAME. The assertion is about the rendered text, which
    // needs no directory; on macOS it is created so the root canonicalizes
    // the same way every other fixture's does.
    let _ = std::fs::create_dir_all(&evil);
    let injected = Profile::compile(&evil, None);
    let text = macos::profile_text(&injected, Path::new(RESOLVED));
    let _ = std::fs::remove_dir_all(&evil);
    // Counted per line, not per occurrence: the escaped directory name
    // contains the phrase too, which is exactly the point — it is inside a
    // string term instead of being one.
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("(allow file-write* (subpath"))
            .count(),
        // The root's and its `.sterna/scratch` carve-out's; an injected term
        // would be a third.
        2,
        "the directory name opened a second write grant: {text}"
    );
    assert!(text.contains(r#"a\"b\\c"#), "not escaped: {text}");

    // (c) The text is a function of the profile and of nothing else: the
    // same profile renders the same bytes, and the argv a caller is about
    // to spawn never reaches the renderer — there is no parameter for it.
    let stable = fixture.profile(Some(&settings_for(&fixture.root)));
    assert_eq!(
        macos::profile_text(&stable, Path::new(RESOLVED)),
        macos::profile_text(&stable, Path::new(RESOLVED))
    );
    // And a command pattern, bare or named, changes nothing the OS layer
    // renders: it pre-approves a command line and grants no path.
    let root_pattern = fixture.root.to_string_lossy().replace('\\', "/");
    for pre_approving in [
        format!(
            r#"{{"permissions":{{"allow":["Read({root_pattern}/**)","Edit({root_pattern}/src/**/*.rs)","Bash"],"deny":["Read({root_pattern}/secrets/**)"]}}}}"#
        ),
        format!(
            r#"{{"permissions":{{"allow":["Read({root_pattern}/**)","Edit({root_pattern}/src/**/*.rs)"],"deny":["Read({root_pattern}/secrets/**)"]}}}}"#
        ),
    ] {
        let other = fixture.profile(Some(&pre_approving));
        assert_eq!(
            macos::profile_text(&other, Path::new(RESOLVED)),
            macos::profile_text(&stable, Path::new(RESOLVED))
        );
        assert_eq!(
            format!("{:?}", linux::landlock_rules(&other, Path::new(RESOLVED))),
            format!("{:?}", linux::landlock_rules(&stable, Path::new(RESOLVED)))
        );
    }

    // (d) §4.1 has no off switch on any platform.
    assert!(macos::profile_text(&stable, Path::new(RESOLVED)).contains("(deny network*)"));
    assert!(
        linux::bwrap_argv(&stable, "/bin/cat".as_ref(), &[])
            .iter()
            .any(|arg| arg == "--unshare-all")
    );
    assert!(!windows::acl_grants(&stable, Path::new(RESOLVED)).internet_client);
}

// --- Linux -------------------------------------------------------------

#[test]
fn the_bwrap_view_unshares_everything_and_rebinds_the_project_over_a_read_only_root() {
    let fixture = Fixture::new("bwrap");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let argv: Vec<String> = linux::bwrap_argv(&profile, "/bin/cat".as_ref(), &["x".into()])
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert_eq!(argv[0], "bwrap");
    assert!(argv.contains(&"--unshare-all".to_string()), "{argv:?}");
    assert!(argv.contains(&"--die-with-parent".to_string()), "{argv:?}");

    // Bind order is the policy. `/` read-only, then the project read-write
    // over it, then `.claude` read-only over that; bwrap applies binds in
    // argument order, so any reversal widens the result.
    let at = |flag: &str, path: &str| {
        argv.windows(3)
            .position(|w| w[0] == flag && w[1] == path && w[2] == path)
    };
    let slash = at("--ro-bind", "/").unwrap_or_else(|| panic!("{argv:?}"));
    let project = at("--bind", &root.to_string_lossy()).unwrap_or_else(|| panic!("{argv:?}"));
    let dot_claude = at("--ro-bind", &root.join(".claude").to_string_lossy())
        .unwrap_or_else(|| panic!("{argv:?}"));
    assert!(slash < project, "{argv:?}");
    assert!(project < dot_claude, "{argv:?}");

    // The program and its arguments come last, after `--`, so no path in
    // them can be read as a bwrap flag.
    assert_eq!(&argv[argv.len() - 3..], &["--", "/bin/cat", "x"]);
}

/// Every path a Landlock ruleset is permitted to grant read on, in order,
/// declared here rather than read from the applier's own constant — so a
/// root added there fails this test instead of travelling with it.
///
/// `/proc`, `/sys` and `/dev` are deliberately absent. A rule beneath
/// `/proc` grants `READ_FILE` on `/proc/<pid>/environ` for every process of
/// the same user, and the regime this package can actually apply is Landlock
/// alone in the host's own PID namespace — so that is the harness's whole
/// environment, §4.2's credentials reached by a route no `permissions`
/// pattern names and no `deny` entry narrows.
const EXPECTED_LANDLOCK_READ_ONLY: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc",
    "/opt",
    "/dev/null",
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/tty",
];

#[test]
fn the_landlock_ruleset_is_exactly_the_declared_paths() {
    let fixture = Fixture::new("landlock");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let rules = linux::landlock_rules(&profile, Path::new(RESOLVED));

    // Positively, path by path and in order: a new system root is a failure
    // by construction rather than by whether it happens to spell `$HOME`.
    // The declared system roots, then the toolchain this machine derived —
    // read-only, and after them because that is the order the ruleset builds.
    let mut expected_read_only: Vec<PathBuf> = EXPECTED_LANDLOCK_READ_ONLY
        .iter()
        .map(PathBuf::from)
        .collect();
    expected_read_only.extend(profile.toolchain_roots().map(Path::to_path_buf));
    expected_read_only.extend(profile.toolchain_read_files().map(Path::to_path_buf));
    assert_eq!(rules.read_only, expected_read_only, "{rules:?}");
    assert_eq!(
        rules.read_write,
        vec![PathBuf::from("/dev/null"), root.clone()],
        "{rules:?}"
    );
    for path in [
        "/dev",
        "/dev/zero",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
        "/dev/sda",
    ] {
        assert!(
            !rules.read_write.contains(&PathBuf::from(path)),
            "unexpected writable device grant: {rules:?}"
        );
    }

    // The three that were here and are not. Named individually because the
    // equality above would also pass if all three were added and the
    // expectation edited to match — this is the clause that has to be read
    // and argued with instead.
    for tree in ["/proc", "/sys", "/dev"] {
        assert!(
            !rules.read_only.contains(&PathBuf::from(tree)),
            "{tree} is granted as a tree: {rules:?}"
        );
    }

    // No `.claude` rule: Landlock's rules are additive, so a read-only rule
    // beneath a read-write one removes nothing. The mount view carries §1.5
    // on this platform, and
    // `landlock_alone_does_not_enforce_the_dot_claude_carve_out_and_the_mount_view_does`
    // is what measured that.
    assert!(
        !rules.read_only.contains(&root.join(".claude")),
        "{rules:?}"
    );
    // §4.3: no granted subtree may contain `$HOME`. The old form of this
    // assertion asked whether a path *started with* `$HOME`, which is false
    // for every ancestor of it — `/home` and `/` would both have passed.
    let home = home();
    for path in rules.read_only.iter().chain(rules.read_write.iter()) {
        assert!(
            !home.starts_with(path) || path.starts_with(&root),
            "a granted subtree contains $HOME: {path:?}"
        );
    }

    // A read grant is open and list — never run, never a write and never a
    // `MAKE_*`. The exec bit left this constant with the 61D ruling.
    assert_eq!(
        linux::access::READ,
        linux::access::READ_FILE | linux::access::READ_DIR
    );
    assert_eq!(linux::access::READ & linux::access::EXECUTE, 0);
    assert_eq!(linux::access::READ_WRITE & linux::access::EXECUTE, 0);
    assert_eq!(linux::access::READ & linux::access::WRITE_FILE, 0);
    assert_eq!(linux::access::READ & linux::access::MAKE_REG, 0);
    // …and the ruleset still *handles* exec. An access absent from
    // `handled_access_fs` is unrestricted, so this is the difference between
    // narrowing exec and abolishing the restriction on it.
    assert_ne!(linux::access::HANDLED & linux::access::EXECUTE, 0);
    assert_eq!(
        linux::access::HANDLED,
        linux::access::READ_WRITE | linux::access::EXECUTE
    );
    // ABI 3's `TRUNCATE` is in the write grant: without it a write grant
    // has a hole in it.
    assert_ne!(linux::access::READ_WRITE & linux::access::TRUNCATE, 0);

    // The device grants are rules on files, and a rule on a file may carry
    // no directory-only right — the kernel refuses the entire ruleset if it
    // does, which fails every confined spawn. `confine` masks with this.
    assert_eq!(linux::access::FILE & linux::access::READ_DIR, 0);
    assert_eq!(
        linux::access::READ & linux::access::FILE,
        linux::access::READ_FILE
    );
    assert_eq!(
        linux::access::EXEC & linux::access::FILE,
        linux::access::EXEC,
        "an exec rule on a file must survive the file mask intact"
    );
}

#[test]
fn a_landlocked_process_cannot_read_outside_the_project_but_an_unsandboxed_one_can() {
    #[cfg(not(target_os = "linux"))]
    eprintln!("skipped: Landlock is a Linux kernel interface; this host is not Linux");
    #[cfg(target_os = "linux")]
    {
        use std::process::{Command, Stdio};

        if linux::landlock_abi() < 3 {
            eprintln!(
                "skipped: this kernel reports Landlock ABI {} and the specification asks for 3",
                linux::landlock_abi()
            );
            return;
        }
        let fixture = Fixture::new("landlock-exec");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let inside = fixture.write(&root.join("inside.txt"), "inside-secret\n");
        let outside = fixture.write(&fixture.outside.join("outside.txt"), "outside-secret\n");

        let cat = |path: &Path, confined: bool| {
            let mut command = Command::new("/bin/cat");
            command
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if confined {
                assert!(linux::confine(&profile, Path::new("/bin/cat"), &mut command).unwrap());
            }
            command.output().unwrap()
        };

        let free = cat(&outside, false);
        assert!(free.status.success(), "{free:?}");
        assert_eq!(String::from_utf8_lossy(&free.stdout), "outside-secret\n");

        let confined = cat(&outside, true);
        assert!(!confined.status.success(), "{confined:?}");
        assert!(
            !String::from_utf8_lossy(&confined.stdout).contains("outside-secret"),
            "{confined:?}"
        );

        let granted = cat(&inside, true);
        assert!(granted.status.success(), "{granted:?}");
        assert_eq!(String::from_utf8_lossy(&granted.stdout), "inside-secret\n");
    }
}

// --- Windows -----------------------------------------------------------

#[test]
fn the_windows_acl_admits_the_capability_sid_to_the_project_and_nothing_else() {
    let fixture = Fixture::new("acl");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let grants = windows::acl_grants(&profile, Path::new(RESOLVED));

    // The scratchpad `.sterna/**`'s never rule exempts is its own read-write
    // carve-out inside the read-only `.sterna`.
    assert_eq!(
        grants.read_write,
        vec![root.clone(), root.join(".sterna/scratch")],
        "{grants:?}"
    );
    assert_eq!(
        grants.read_only,
        vec![root.join(".claude"), root.join(".sterna")],
        "{grants:?}"
    );
    // Neither grant carries `FILE_EXECUTE`: the project tree is where
    // model-authored files live and map line 2457 says none of them runs.
    // The rest of `FILE_GENERIC_EXECUTE` stays, because `SYNCHRONIZE` and
    // `FILE_READ_ATTRIBUTES` are what an ordinary open needs.
    assert_eq!(windows::READ_RIGHTS & windows::FILE_EXECUTE, 0);
    assert_eq!(windows::READ_WRITE_RIGHTS & windows::FILE_EXECUTE, 0);
    assert_ne!(windows::READ_RIGHTS & 0x0010_0000, 0, "SYNCHRONIZE");
    assert_ne!(windows::READ_RIGHTS & 0x0000_0001, 0, "FILE_READ_DATA");

    // **The whole mask, bit by bit, because `FILE_ALL_ACCESS` minus one bit
    // is not a decision — it is the absence of one, and it cost three
    // escapes.** `FILE_DELETE_CHILD` is the right to delete or rename a child
    // *whose own DACL grants nothing*, which is exactly the `.claude`
    // carve-out; `WRITE_DAC` and `WRITE_OWNER` are the right to rewrite any
    // object's security, inheritable across the entire project. All three
    // were in `0x001F_01FF`.
    assert_eq!(
        windows::READ_WRITE_RIGHTS & windows::WITHHELD_RIGHTS,
        0,
        "the project grant carries a right nothing justified"
    );
    assert_eq!(windows::READ_RIGHTS & windows::WITHHELD_RIGHTS, 0);
    assert_eq!(
        windows::WITHHELD_RIGHTS,
        windows::FILE_EXECUTE
            | windows::FILE_DELETE_CHILD
            | windows::WRITE_DAC
            | windows::WRITE_OWNER
    );
    // And the exact values, so a widening is a diff rather than a
    // re-derivation.
    assert_eq!(windows::READ_RIGHTS, 0x0012_0089, "read");
    assert_eq!(windows::READ_WRITE_RIGHTS, 0x0013_019F, "read+write");
    // The falsifying half: a narrower mask that broke ordinary work would
    // pass every assertion above. Deleting a file inside the project is
    // `DELETE` **on the file**, which is what makes dropping
    // `FILE_DELETE_CHILD` from the parent free.
    assert_ne!(windows::READ_WRITE_RIGHTS & windows::DELETE, 0, "DELETE");
    for (bit, name) in [
        (windows::FILE_WRITE_DATA, "FILE_WRITE_DATA"),
        (windows::FILE_APPEND_DATA, "FILE_APPEND_DATA"),
        (windows::FILE_WRITE_ATTRIBUTES, "FILE_WRITE_ATTRIBUTES"),
        (windows::FILE_READ_DATA, "FILE_READ_DATA"),
        (windows::FILE_READ_ATTRIBUTES, "FILE_READ_ATTRIBUTES"),
        (windows::SYNCHRONIZE, "SYNCHRONIZE"),
    ] {
        assert_ne!(windows::READ_WRITE_RIGHTS & bit, 0, "{name}");
    }
    // And the binary is recorded rather than acted on — the one platform
    // where the 61D narrow grant is not enforced, said out loud.
    assert_eq!(grants.executable, PathBuf::from(RESOLVED), "{grants:?}");
    // §4.1: an AppContainer without `internetClient` has no network, and no
    // document can add the capability because no pattern names one.
    assert!(!grants.internet_client);

    // The container name is derived from the root **and the user**, is
    // stable across calls, and fits `CreateAppContainerProfile`'s 64 UTF-16
    // limit however long the project path is.
    //
    // The user half is the security property, not decoration: an
    // AppContainer SID is a pure function of the profile name, so a name
    // derived from the path alone lets any local process -- including one
    // running as another account -- derive the SID, create the same
    // container and inherit whatever the project ACL grants it.
    const ALICE: &str = "S-1-5-21-1111111111-2222222222-3333333333-1001";
    const BOB: &str = "S-1-5-21-1111111111-2222222222-3333333333-1002";
    let name = windows::container_name(&profile, ALICE);
    assert_eq!(name, windows::container_name(&profile, ALICE));
    assert!(name.len() <= 64, "{name}");
    assert!(name.starts_with("Glasshouse.Pane."), "{name}");
    let other = Profile::compile(root.join("elsewhere"), None);
    assert_ne!(name, windows::container_name(&other, ALICE));
    assert_ne!(
        name,
        windows::container_name(&profile, BOB),
        "two users on one project root must not share a container SID"
    );
    // And the fold is length-prefixed, so a root that ends in one user's SID
    // cannot collide with a shorter root under another.
    assert_ne!(
        windows::container_name(&Profile::compile(root.join("ab"), None), "c"),
        windows::container_name(&Profile::compile(root.join("a"), None), "bc"),
    );

    #[cfg(not(target_os = "windows"))]
    eprintln!(
        "skipped: the restricted token, the AppContainer and the project ACL are Win32 calls; \
         no Windows cell exists for the sterna job, so they are compile-verified only"
    );
}

#[test]
fn the_windows_job_object_is_documented_as_a_lifetime_primitive_and_not_a_sandbox() {
    // Requirement 4, and it is about the prose because the defect it guards
    // is a reader's: the map line says "Windows job objects" in a list of
    // sandboxes, and it is not one.
    let first = WINDOWS_SOURCE
        .lines()
        .find(|line| line.contains("job object"))
        .expect("the module documents the job object");
    assert!(
        first.contains("not a sandbox") && first.contains("grants nothing"),
        "the first sentence mentioning the job object must say it is not a sandbox: {first}"
    );
    assert!(
        WINDOWS_SOURCE.contains("lifetime"),
        "the job object's actual purpose must be named"
    );
}

#[test]
fn the_project_acl_grant_is_reached_only_through_the_spawn_that_confines() {
    // This function modifies a user's filesystem, so what guards it is no
    // longer that nothing calls it -- something does now -- but *where* it
    // is called from. The only call site is `sandbox::windows::spawn`, and
    // the ordering inside that function is the contract: the user's SID, the
    // container, the refusal for an image the container cannot load, then
    // this, and only then `CreateProcessW`. Every one is a `?`, so no child
    // is created unless all of them succeeded.
    let calls: Vec<&str> = WINDOWS_SOURCE
        .lines()
        .filter(|line| line.contains("grant_project_acl("))
        .filter(|line| !line.contains("pub fn "))
        .map(|line| line.trim())
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "the project ACL must have exactly one caller: {calls:?}"
    );
    assert!(
        calls[0].starts_with("grant_project_acl(profile, binary, &container)")
            && calls[0].ends_with("?;"),
        "the one caller must pass the container it just made and propagate the failure: {calls:?}"
    );

    // And the caller is the spawn, with the ACL grant above the process
    // creation rather than beside it. The order is the contract: refuse an
    // image the container cannot load, grant the project, and only then
    // create a process — each one a `?`, so nothing is started unless every
    // one of them succeeded.
    let spawn = WINDOWS_SOURCE
        .find("    pub fn spawn(")
        .expect("the confined spawn is gone; so is this test's subject");
    let body = &WINDOWS_SOURCE[spawn..];
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("the spawn no longer contains `{needle}`"))
    };
    let refusal = at("return Err(NotConfinable(cannot_load(binary)));");
    let acl = at("grant_project_acl(profile, binary, &container)");
    let create = at("CreateProcessW(");
    assert!(
        refusal < acl && acl < create,
        "the order must be refuse, grant, create: {refusal} {acl} {create}"
    );

    // The idempotence guard, which is not tidiness: this runs on every
    // spawn, and re-writing an ACE that is already present would both grow
    // the DACL of a person's project directory and re-walk their whole tree
    // -- measured at 1.01s for 10,000 files against a 47ms skip.
    let grant = WINDOWS_SOURCE
        .find("    pub fn grant_project_acl(")
        .expect("the grant is gone; so is this test's subject");
    let grant_body = &WINDOWS_SOURCE[grant..];
    let grant_body = &grant_body[..grant_body
        .find("\n    /// ")
        .expect("the grant is the last item in its module")];
    assert!(
        grant_body.contains("masks_for(path, container.sid())?")
            && grant_body.contains("return Ok(());"),
        "the grant must read the current masks and return having written nothing when they \
         already carry the intended rights"
    );

    // **And the skip has to be safe, which is what the three phases below it
    // are for.** `SetNamedSecurityInfoW` writes the named object's DACL and
    // then walks the tree, so an interrupted call leaves the root looking
    // finished over descendants that are not. The carve-out is therefore
    // written closed first, the roots second, and the carve-out's read grant
    // last -- so the carve-out holding exactly `READ_RIGHTS` is proof that
    // the walk before it ran to the end, and any other state re-runs the
    // whole sequence. Reordering these three collapses that proof, which no
    // mask assertion would notice.
    let phase = |needle: &str| {
        grant_body
            .find(needle)
            .unwrap_or_else(|| panic!("the grant no longer contains `{needle}`"))
    };
    let closed = phase("write_acl(path, container, None, true)?;");
    let roots = phase("write_acl(path, container, Some(*rights), false)?;");
    let granted = phase("write_acl(path, container, Some(*rights), true)?;");
    assert!(
        closed < roots && roots < granted,
        "the carve-out must be closed before the roots propagate and granted only after \
         they return: {closed} {roots} {granted}"
    );
    // And the carve-out is created rather than skipped when it is missing: a
    // project with no `.claude/` yet is one where a program could make it and
    // write the settings document its next session compiles from.
    assert!(
        grant_body.contains("std::fs::create_dir_all(path)?;"),
        "a missing carve-out must be created, not passed over"
    );
}

#[test]
fn landlock_alone_does_not_enforce_the_dot_claude_carve_out_and_the_mount_view_does() {
    #[cfg(not(target_os = "linux"))]
    eprintln!("skipped: Landlock is a Linux kernel interface; this host is not Linux");
    #[cfg(target_os = "linux")]
    {
        use std::process::{Command, Stdio};

        if linux::landlock_abi() < 3 {
            eprintln!(
                "skipped: this kernel reports Landlock ABI {} and the specification asks for 3",
                linux::landlock_abi()
            );
            return;
        }
        // The measurement behind `landlock_rules`' central caveat, and the
        // reason this test asserts a limitation rather than a protection: a
        // Landlock ruleset's rules are ADDITIVE, so a read-only rule beneath
        // a read-write one removes nothing and `.claude/` stays writable
        // under Landlock alone. §1.5's OS-level enforcement on Linux is
        // therefore the mount view's read-only bind, and `Profile::check`
        // refuses the write in every regime.
        eprintln!("measured on Landlock ABI {}", linux::landlock_abi());
        let fixture = Fixture::new("landlock-claude");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let source = fixture.write(&root.join("inside.txt"), "inside-secret\n");
        let cp = ["/bin/cp", "/usr/bin/cp"]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .expect("cp");

        let copy = |target: &Path| {
            let mut command = Command::new(cp);
            command
                .arg(&source)
                .arg(target)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            assert!(linux::confine(&profile, Path::new(cp), &mut command).unwrap());
            command.output().unwrap()
        };

        // The ruleset is applied and doing its job inside the project.
        let allowed = copy(&root.join("allowed.txt"));
        assert!(allowed.status.success(), "{allowed:?}");
        assert!(root.join("allowed.txt").exists(), "{allowed:?}");

        // And it does not carve `.claude` back out. If a future kernel makes
        // rules most-specific-wins, this assertion fails and the caveat in
        // `landlock_rules` is what needs rewriting.
        let claude = copy(&root.join(".claude/written.txt"));
        assert!(
            claude.status.success() && root.join(".claude/written.txt").exists(),
            "Landlock now enforces the carve-out; `landlock_rules` says it cannot: {claude:?}"
        );

        // The two layers that do refuse it. The mount view, by bind order:
        let argv: Vec<String> = linux::bwrap_argv(&profile, cp.as_ref(), &[])
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let claude_bind = root.join(".claude").to_string_lossy().into_owned();
        assert!(
            argv.windows(3)
                .any(|w| w[0] == "--ro-bind" && w[1] == claude_bind && w[2] == claude_bind),
            "{argv:?}"
        );
        // And Sterna's own pre-call check, in every regime:
        assert!(
            profile
                .check("write", Access::Write, &root.join(".claude/written.txt"))
                .is_err()
        );
    }
}

// --- the exec grant: any program under the roots, the project and the
// toolchains, never a binary outside them --------------------------------

/// The loader roots that keep `EXECUTE` on Linux whatever binary is
/// confined, declared here rather than read from the applier's constant so
/// that adding one fails this test instead of travelling with it.
const EXPECTED_LOADER_EXEC_ROOTS: &[&str] = &["/lib", "/lib64", "/usr/lib", "/usr/lib64"];

/// The system roots a Landlock ruleset grants `EXECUTE` beneath — the first
/// seven of [`EXPECTED_LANDLOCK_READ_ONLY`], without the character devices.
const EXPECTED_LANDLOCK_SYSTEM_ROOTS: &[&str] =
    &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt"];

/// Every command line is admitted, so a confined command may start any
/// program under the executable roots, the package prefixes, the project and
/// the toolchains: `bash` exists to exec other programs, and a shell confined
/// to exec'ing only itself runs builtins and nothing else. The scope is the
/// same for every settings document and every program name.
#[test]
fn every_profile_grants_exec_on_the_roots_the_prefixes_the_project_and_the_toolchains() {
    let fixture = Fixture::new("execgrant");
    let root_pattern = fixture.root.to_string_lossy().replace('\\', "/");
    let documents = [
        Some(settings_for(&fixture.root)),
        None,
        Some(r#"{"permissions":{}}"#.to_string()),
        Some(format!(
            r#"{{"permissions":{{"allow":["Read({root_pattern}/**)","Write({root_pattern}/**)","Bash"]}}}}"#
        )),
    ];
    for document in &documents {
        let profile = fixture.profile(document.as_deref());
        for binary in [RESOLVED, UNRESOLVED, OUTSIDE_READ_ROOTS] {
            assert_eq!(
                macos::exec_scope(&profile, Path::new(binary)),
                macos::ExecScope::RootsAndProject,
                "{document:?} {binary}"
            );
            assert_eq!(
                linux::exec_scope(&profile, Path::new(binary)),
                linux::ExecScope::RootsAndProject,
                "{document:?} {binary}"
            );
        }
    }

    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let text = macos::profile_text(&profile, Path::new(RESOLVED));

    // macOS: exactly the roots, the prefixes, the project, the toolchains,
    // and the resolved binary's own literal -- a superset, never a
    // replacement, because the shell sterna resolved may live outside the
    // roots (Homebrew's `bash` canonicalises into `Cellar/`).
    let filters = exec_filters(&text);
    let mut expected: Vec<String> = EXPECTED_EXEC_ROOTS
        .iter()
        .chain(EXPECTED_PACKAGE_PREFIXES)
        .map(|p| p.to_string())
        .collect();
    expected.push(root.to_string_lossy().into_owned());
    expected.extend(toolchain(&profile));
    assert_eq!(
        sorted(
            filters
                .iter()
                .filter(|f| f.form == "subpath")
                .map(|f| f.value.clone())
                .collect()
        ),
        sorted(expected),
        "{text}"
    );
    assert_eq!(
        filters
            .iter()
            .filter(|f| f.form == "literal")
            .map(|f| f.value.as_str())
            .collect::<Vec<_>>(),
        [RESOLVED],
        "{text}"
    );
    // §4.3: `$HOME` is not an exec root, so a binary under `~/.local/bin`
    // stays unrunnable; only the toolchain homes inside it are.
    let home = home();
    assert!(
        !filters.iter().any(|f| Path::new(&f.value) == home),
        "$HOME became an exec root: {text}"
    );

    // Linux: the same scope, as the paths the ruleset grants `EXECUTE`.
    assert!(
        EXPECTED_LANDLOCK_READ_ONLY.starts_with(EXPECTED_LANDLOCK_SYSTEM_ROOTS),
        "the declared system roots are no longer the head of the read-only list"
    );
    let rules = linux::landlock_rules(&profile, Path::new(RESOLVED));
    assert_eq!(rules.exec, linux::ExecScope::RootsAndProject, "{rules:?}");
    let mut expected: Vec<PathBuf> = EXPECTED_LANDLOCK_SYSTEM_ROOTS
        .iter()
        .map(PathBuf::from)
        .collect();
    expected.push(PathBuf::from(RESOLVED));
    expected.push(root.clone());
    expected.extend(EXPECTED_LOADER_EXEC_ROOTS.iter().map(PathBuf::from));
    expected.extend(profile.toolchain_roots().map(Path::to_path_buf));
    assert_eq!(rules.executable, expected, "{rules:?}");
    assert!(
        !rules.executable.contains(&home),
        "$HOME became an exec root: {rules:?}"
    );

    // Windows records the name and enforces nothing on it; the field is
    // there so that absence is reported rather than silent.
    assert_eq!(
        windows::acl_grants(&profile, Path::new(UNRESOLVED)).executable,
        PathBuf::from(UNRESOLVED)
    );
}

/// A name `execvp` still has to search for is not a path, so it must never
/// reach a profile term: a relative string in a `(literal …)` is a term
/// seatbelt reads against the sandbox's own working directory, not the
/// program the shell will find. The roots bound the search instead.
#[test]
fn an_unresolvable_program_name_never_reaches_the_profile() {
    let fixture = Fixture::new("fallback");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let text = macos::profile_text(&profile, Path::new(UNRESOLVED));
    assert!(!text.contains(UNRESOLVED), "{text}");
    let rules = linux::landlock_rules(&profile, Path::new(UNRESOLVED));
    assert!(
        rules.executable.iter().all(|path| path.is_absolute()),
        "{rules:?}"
    );
}

#[test]
fn a_confined_process_runs_any_program_under_the_executable_roots() {
    #[cfg(not(target_os = "macos"))]
    eprintln!(
        "skipped: seatbelt is macOS-only; the Linux equivalent is \
         a_landlocked_process_runs_the_roots_and_the_project_but_not_a_binary_outside_them"
    );
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        // The profile is rendered for `/bin/cat`, and `/bin/echo` -- its
        // sibling, which the exec roots cover -- runs under it too. The
        // refusing half, a binary outside every root, is
        // `a_resolved_binary_outside_the_read_roots_still_starts`.
        let fixture = Fixture::new("sibling");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let inside = fixture.write(&root.join("inside.txt"), "inside-secret\n");

        let run = |program: &str, arg: &Path| {
            let mut command = Command::new(program);
            command
                .arg(arg)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            macos::confine(&profile, Path::new(RESOLVED), &mut command).unwrap();
            command.output().unwrap()
        };

        let granted = run("/bin/cat", &inside);
        assert!(granted.status.success(), "{granted:?}");
        assert_eq!(String::from_utf8_lossy(&granted.stdout), "inside-secret\n");
        let sibling = run("/bin/echo", Path::new("sibling-marker"));
        assert!(sibling.status.success(), "{sibling:?}");
        assert!(
            String::from_utf8_lossy(&sibling.stdout).contains("sibling-marker"),
            "{sibling:?}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_confined_shell_runs_homebrew_python_through_the_package_prefix() {
    use std::process::{Command, Stdio};

    let Ok(python) = std::fs::canonicalize("/opt/homebrew/bin/python3") else {
        eprintln!("skipped: /opt/homebrew/bin/python3 is not installed");
        return;
    };
    let fixture = Fixture::new("python-descendant");
    let profile = fixture.profile(Some(r#"{"permissions":{}}"#));
    let shell = std::fs::canonicalize("/bin/bash").unwrap();
    let companion = macos::python_framework_companion(&python)
        .expect("Homebrew Python has its one framework launcher companion");
    let descendants = vec![python.clone(), companion];

    let mut command = Command::new(&shell);
    command
        .arg("-c")
        .arg(format!("{} -c 'print(\"python-ran\")'", python.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(profile.root());
    macos::confine_with_descendants(&profile, &shell, &descendants, &mut command).unwrap();
    let granted = command.output().expect("the confined shell starts");
    assert!(granted.status.success(), "{granted:?}");
    assert_eq!(
        String::from_utf8_lossy(&granted.stdout).trim(),
        "python-ran"
    );
}

/// §1.4, and the one shape of it a profile can defeat without ever saying
/// no: **a confined tool either runs or is refused, and never dies silently.**
/// `process-exec*` permits the exec; the image still has to be mapped, and a
/// binary the caller resolved outside the loader's read roots is named by no
/// read term the profile otherwise emits. A process killed between `exec` and
/// its first instruction carries no exit code, no stdout and no stderr, so
/// `ToolResult` reaches the model as an empty result where §1.4 promises a
/// `PermissionDenied`.
///
/// **Not a copy of `/bin/cat`.** macOS refuses to launch an Apple platform
/// binary from any path but its own — an AMFI launch-constraint violation,
/// which kills the copy identically whether or not a sandbox is in force and
/// so cannot demonstrate anything about a profile. This test copies the one
/// binary every host running this suite is guaranteed to have — itself — into
/// a directory outside both the read roots and the project, and asks it for
/// the payload test below by name.
#[test]
fn a_resolved_binary_outside_the_read_roots_still_starts() {
    #[cfg(not(target_os = "macos"))]
    eprintln!(
        "skipped: seatbelt is macOS-only; Linux grants the resolved binary its own read \
         through `access::EXEC` and is covered by \
         a_landlocked_process_runs_the_roots_and_the_project_but_not_a_binary_outside_them"
    );
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        let fixture = Fixture::new("outside-roots");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let inside = fixture.write(&root.join("inside.txt"), "inside-secret\n");

        // Outside every LOADER_READ_ROOT *and* outside the project root, so
        // no term of the rendered profile reaches it but the exec grant and
        // the read literal beside it.
        let outside = std::fs::canonicalize(&fixture.outside).unwrap();
        assert!(
            !outside.starts_with(&root),
            "the binary must sit outside the project: {outside:?}"
        );
        let binary = outside.join("resolved");
        let sibling = outside.join("sibling");
        std::fs::copy(std::env::current_exe().unwrap(), &binary).unwrap();
        std::fs::copy(&binary, &sibling).unwrap();

        let run = |program: &Path, confined: bool| {
            let mut command = Command::new(program);
            command
                .arg("--exact")
                .arg("--nocapture")
                .arg(PAYLOAD)
                .env(PAYLOAD_FILE, &inside)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if confined {
                macos::confine(&profile, &binary, &mut command).unwrap();
            }
            command.output()
        };

        // The controls: both copies run unconfined and print the file, so a
        // failure below is the profile and not a binary that cannot start.
        for control in [&binary, &sibling] {
            let free = run(control, false).unwrap();
            assert!(free.status.success(), "{control:?} does not run: {free:?}");
            assert!(
                String::from_utf8_lossy(&free.stdout).contains("inside-secret"),
                "{control:?} printed nothing: {free:?}"
            );
        }

        // The resolved binary starts under the profile rendered for it and
        // reads the project. `code()` is asserted explicitly and not merely
        // `success()`: the defect's whole signature is `None` — a child
        // killed by a signal, which every layer above renders as an empty
        // result rather than as a refusal.
        let granted = run(&binary, true).unwrap();
        assert_eq!(
            granted.status.code(),
            Some(0),
            "the resolved binary did not exit under its own profile — a `None` here is the \
             silent kill §1.4 forbids: {granted:?}"
        );
        assert!(
            String::from_utf8_lossy(&granted.stdout).contains("inside-secret"),
            "{granted:?}"
        );

        // And the grant is still one file there: its sibling, in the same
        // directory outside every executable root and the project, is not
        // executable through this profile.
        match run(&sibling, true) {
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "the sibling was refused for some reason other than the exec grant: {error:?}"
            ),
            Ok(output) => {
                assert!(!output.status.success(), "the sibling ran: {output:?}");
                assert!(
                    !String::from_utf8_lossy(&output.stdout).contains("inside-secret"),
                    "the sibling ran: {output:?}"
                );
            }
        }
    }
}

/// The read grant beside the exec grant is **one file**, and the invariant is
/// as much about what it is not: a `(subpath …)` on the binary's directory
/// would grant every neighbour it has — §4.3's `$HOME` rule survives here
/// only because the term names the binary's own bytes and nothing around
/// them. Text only, so it holds on every host.
#[test]
fn the_read_literal_names_the_binary_and_only_the_binary() {
    let fixture = Fixture::new("binary-read");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let binary = Path::new(OUTSIDE_READ_ROOTS);
    let text = macos::profile_text(&profile, binary);
    let (_, filters) = parse(&text);

    assert_eq!(
        filters
            .iter()
            .filter(|f| f.term == "file-read*"
                && f.form == "literal"
                && f.value == OUTSIDE_READ_ROOTS)
            .count(),
        1,
        "the resolved binary must be readable by the process that becomes it, exactly once: \
         {text}"
    );

    // No subtree grant reaches it, which is the half that would widen: the
    // directory, its parent, and every ancestor are absent as `subpath`.
    for filter in &filters {
        if filter.form == "subpath" {
            assert!(
                !binary.starts_with(&filter.value),
                "a subtree grant contains the binary and would carry its neighbours: {filter:?}"
            );
        }
    }

    // The read literals are the profile's own plus the binary, and nothing
    // else: rendered for another binary, only that one literal differs.
    let read_literals = |filters: &[Filter], without: &str| {
        sorted(
            filters
                .iter()
                .filter(|f| f.term == "file-read*" && f.form == "literal" && f.value != without)
                .map(|f| f.value.clone())
                .collect(),
        )
    };
    let (_, other) = parse(&macos::profile_text(&profile, Path::new(RESOLVED)));
    assert_eq!(
        read_literals(&filters, OUTSIDE_READ_ROOTS),
        read_literals(&other, RESOLVED),
        "the read literals differ by more than the binary: {text}"
    );
}

/// A descendant outside every root is granted as the one literal named,
/// never as its directory or a sibling beside it.
#[test]
fn an_admitted_descendant_is_one_literal_and_never_its_siblings() {
    let fixture = Fixture::new("descendant-literal");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let tool = Path::new(OUTSIDE_READ_ROOTS).with_file_name("python3");
    let sibling = Path::new(OUTSIDE_READ_ROOTS).with_file_name("pip3");

    let text = macos::profile_text_with_descendants(
        &profile,
        Path::new(RESOLVED),
        std::slice::from_ref(&tool),
    );
    let filters = exec_filters(&text);
    assert!(
        filters
            .iter()
            .any(|filter| filter.form == "literal" && filter.value == tool.to_string_lossy()),
        "the admitted descendant is absent: {text}"
    );
    assert!(
        !filters.iter().any(|filter| {
            filter.value == sibling.to_string_lossy()
                || (filter.form == "subpath" && tool.starts_with(&filter.value))
        }),
        "a descendant grant widened to a sibling: {text}"
    );

    let rules = linux::landlock_rules_with_descendants(
        &profile,
        Path::new(RESOLVED),
        std::slice::from_ref(&tool),
    );
    assert!(rules.executable.contains(&tool), "{rules:?}");
    assert!(!rules.executable.contains(&sibling), "{rules:?}");
    assert!(
        !rules
            .executable
            .iter()
            .any(|path| path != &tool && tool.starts_with(path)),
        "the exact descendant widened to a directory above it: {rules:?}"
    );
}

/// The name of the payload test below, and the variable that arms it.
///
/// The name is used only by the macOS test that spawns this binary, and
/// `warnings = deny` makes an unused constant a build failure elsewhere; the
/// variable is read by the payload itself and so is compiled everywhere.
#[cfg(target_os = "macos")]
const PAYLOAD: &str = "a_payload_that_prints_one_file_when_this_binary_is_the_confined_tool";
const PAYLOAD_FILE: &str = "STERNA_SANDBOX_APPLY_PRINT";

/// Not a property — the **program**
/// `a_resolved_binary_outside_the_read_roots_still_starts` confines.
///
/// That test needs a real binary outside the loader's read roots which prints
/// a file inside the project, and macOS will not launch a copy of `/bin/cat`
/// from one. This executable is the only binary every host running this suite
/// is guaranteed to have, so the test copies it and asks for this test by
/// name with [`PAYLOAD_FILE`] set. In every ordinary run the variable is
/// unset and this asserts nothing.
#[test]
fn a_payload_that_prints_one_file_when_this_binary_is_the_confined_tool() {
    let Some(path) = std::env::var_os(PAYLOAD_FILE) else {
        return;
    };
    print!("{}", std::fs::read_to_string(path).unwrap());
}

#[test]
fn a_landlocked_process_runs_the_roots_and_the_project_but_not_a_binary_outside_them() {
    #[cfg(not(target_os = "linux"))]
    eprintln!("skipped: Landlock is a Linux kernel interface; this host is not Linux");
    #[cfg(target_os = "linux")]
    {
        use std::process::{Command, Stdio};

        if linux::landlock_abi() < 3 {
            eprintln!(
                "skipped: this kernel reports Landlock ABI {} and the specification asks for 3",
                linux::landlock_abi()
            );
            return;
        }
        // Both directions, because a ruleset that refused every exec — the
        // failure mode `LOADER_EXEC_ROOTS` exists to prevent, since `execve`
        // needs `EXECUTE` on the ELF interpreter too — would pass a one-sided
        // test.
        let fixture = Fixture::new("landlock-sibling");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let inside = fixture.write(&root.join("inside.txt"), "inside-secret\n");
        let cat = ["/bin/cat", "/usr/bin/cat"]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .expect("cat");
        let echo = Path::new(cat).parent().unwrap().join("echo");
        if !echo.exists() {
            eprintln!("skipped: no sibling binary beside {cat}");
            return;
        }
        // A copy of `echo` in the project, which is an exec root, and one in
        // a directory outside every root, which is not.
        let in_project = root.join("echo-in-project");
        let outside = std::fs::canonicalize(&fixture.outside)
            .unwrap()
            .join("echo-outside");
        for copy in [&in_project, &outside] {
            std::fs::copy(&echo, copy).unwrap();
        }

        let run = |program: &Path, arg: &Path, confined: bool| {
            let mut command = Command::new(program);
            command
                .arg(arg)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if confined {
                assert!(linux::confine(&profile, Path::new(cat), &mut command).unwrap());
            }
            command.output()
        };

        // The controls: every binary runs unconfined.
        for program in [echo.as_path(), &in_project, &outside] {
            let free = run(program, Path::new("marker"), false).unwrap();
            assert!(
                free.status.success() && String::from_utf8_lossy(&free.stdout).contains("marker"),
                "{program:?} does not run: {free:?}"
            );
        }

        let granted = run(Path::new(cat), &inside, true).unwrap();
        assert!(granted.status.success(), "{granted:?}");
        assert_eq!(String::from_utf8_lossy(&granted.stdout), "inside-secret\n");
        for program in [echo.as_path(), &in_project] {
            let ran = run(program, Path::new("marker"), true).unwrap();
            assert!(
                ran.status.success() && String::from_utf8_lossy(&ran.stdout).contains("marker"),
                "{program:?} is under an exec root and did not run: {ran:?}"
            );
        }

        match run(&outside, Path::new("outside-marker"), true) {
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "the outside binary was refused for some reason other than the exec grant: {error:?}"
            ),
            Ok(output) => {
                assert!(
                    !output.status.success(),
                    "the outside binary ran: {output:?}"
                );
                assert!(
                    !String::from_utf8_lossy(&output.stdout).contains("outside-marker"),
                    "the outside binary ran: {output:?}"
                );
            }
        }
    }
}

/// The measurement this package exists for, against the kernel: a confined
/// child resolves a toolchain and runs `git` in a worktree.
///
/// Session `tlj14m-24r` (2026-09-17) could do neither. `cargo` failed on
/// `~/.rustup/settings.toml`, and the gate refused with "is not a git
/// worktree" because a linked worktree's `.git` names a directory outside
/// the project root.
#[test]
fn a_confined_child_resolves_its_toolchain_and_runs_git_in_a_worktree() {
    #[cfg(not(target_os = "macos"))]
    eprintln!("skipped: seatbelt is macOS-only; Linux is the ubuntu cell");
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        let fixture = Fixture::new("toolchain-exec");
        // A worktree, as git lays one out: `.git` is a file naming a
        // directory inside another repository.
        let repository = fixture.root.join("repo");
        let common = repository.join(".git");
        let gitdir = common.join("worktrees").join("feature");
        std::fs::create_dir_all(&gitdir).unwrap();
        let root = fixture.root.join("feature");
        std::fs::create_dir_all(&root).unwrap();

        let git = Command::new("git")
            .args(["init", "--quiet", repository.to_string_lossy().as_ref()])
            .output();
        if !git.is_ok_and(|out| out.status.success()) {
            println!("skipped: git is not runnable on this machine");
            return;
        }
        // A worktree needs a commit to detach from, and the fixture must not
        // depend on this machine's git identity.
        let commit = Command::new("git")
            .current_dir(&repository)
            .args([
                "-c",
                "user.email=sterna@example.invalid",
                "-c",
                "user.name=sterna",
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                "root",
            ])
            .output()
            .unwrap();
        if !commit.status.success() {
            println!(
                "skipped: git could not commit: {}",
                String::from_utf8_lossy(&commit.stderr)
            );
            return;
        }
        let worktree = Command::new("git")
            .current_dir(&repository)
            .args([
                "worktree",
                "add",
                "--detach",
                root.to_string_lossy().as_ref(),
            ])
            .output()
            .unwrap();
        if !worktree.status.success() {
            println!(
                "skipped: git could not create a worktree: {}",
                String::from_utf8_lossy(&worktree.stderr)
            );
            return;
        }

        // The case a developer is actually in: the project readable and
        // writable, every command line admitted.
        let resolved = std::fs::canonicalize(&root).unwrap();
        let pattern = resolved.to_string_lossy().replace('\\', "/");
        let profile = Profile::compile(
            &resolved,
            Some(&format!(
                r#"{{"permissions":{{"allow":["Read({pattern}/**)","Write({pattern}/**)","Bash"]}}}}"#
            )),
        );
        let sh = |script: &str| {
            let mut command = Command::new("/bin/bash");
            command
                .arg("-c")
                .arg(script)
                .current_dir(&resolved)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            macos::confine(&profile, Path::new("/bin/bash"), &mut command).unwrap();
            command.output().unwrap()
        };

        // The gate's own question, which is what refused on 2026-09-17.
        let status = sh("git rev-parse --is-inside-work-tree");
        assert!(
            status.status.success(),
            "git in a worktree: {}",
            String::from_utf8_lossy(&status.stderr)
        );

        // And a write through git, because the worktree grant is read *and*
        // write: an index refresh lands in the directory `.git` names.
        let add = sh("touch a.txt && git add a.txt && git status --porcelain");
        assert!(
            add.status.success() && String::from_utf8_lossy(&add.stdout).contains("a.txt"),
            "git add in a worktree: {}",
            String::from_utf8_lossy(&add.stderr)
        );

        // The toolchain, if this machine has one: reading the manifest that
        // rustup resolves a toolchain from is the exact failure measured.
        let mut roots = profile.toolchain_roots();
        if let Some(rustup) = roots.find(|path| path.ends_with(".rustup")) {
            let settings = rustup.join("settings.toml");
            if settings.exists() {
                let read = sh(&format!("cat {}", settings.to_string_lossy()));
                assert!(
                    read.status.success(),
                    "a build must read its toolchain manifest: {}",
                    String::from_utf8_lossy(&read.stderr)
                );
            }
        } else {
            println!("skipped the toolchain half: no rustup home on this machine");
        }
    }
}
