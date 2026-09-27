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
use sterna::sandbox::profile::ProxyRoute;
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

    /// Used only by the macOS execution tests, which put a real file in
    /// front of a confined process; elsewhere it has no caller, and
    /// `warnings = deny` makes a dead one a build failure. (The Linux kernel
    /// tests are `wide_sandbox.rs`.)
    #[cfg(target_os = "macos")]
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
/// here is a test failure whether or not it carries a path.
const EXPECTED_TERMS: &[&str] = &[
    "default",
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

/// The fixed paths the profile names: the devices a process writes.
const EXPECTED_PATHS: &[&str] = &[
    "/dev/null",
    "/dev/tty",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/dtracehelper",
];

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

/// The wide sandbox, as text: every file readable and every program
/// runnable, writes on the writable places, the protected paths inside the
/// roots denied after them, and the secrets denied after every grant.
#[test]
fn the_allow_set_is_exactly_the_declared_terms() {
    let fixture = Fixture::new("default");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let text = macos::profile_text(&profile);

    assert!(
        text.starts_with(
            "(version 1)\n(deny default)\n(allow file-read*)\n(allow process-exec*)\n"
        ),
        "{text}"
    );
    let (names, filters) = parse(&text);
    assert_eq!(
        sorted(names),
        sorted(EXPECTED_TERMS.iter().map(|t| t.to_string()).collect()),
        "{text}"
    );

    let mut expected: Vec<String> = EXPECTED_PATHS.iter().map(|p| p.to_string()).collect();
    let text_of = |path: &Path| path.to_string_lossy().into_owned();
    expected.extend(profile.writable_places().iter().map(|p| text_of(p)));
    for name in [".sterna", ".claude", ".sterna/scratch"] {
        expected.push(text_of(&root.join(name)));
    }
    expected.extend(profile.secret_paths().iter().map(|p| text_of(p)));
    assert_eq!(
        sorted(filters.iter().map(|f| f.value.clone()).collect()),
        sorted(expected),
        "{text}"
    );

    // The secrets come after every grant: seatbelt takes the last matching
    // term, so a secret inside a writable place stays refused.
    let last_grant = text.rfind("(allow file-write* (subpath").unwrap();
    for secret in profile.secret_paths() {
        let deny = format!(
            "(deny file-read* file-write* (subpath {}))",
            quoted(&secret)
        );
        let at = text.find(&deny).unwrap_or_else(|| panic!("{deny}: {text}"));
        assert!(at > last_grant, "{deny} precedes a grant: {text}");
    }

    // §4.3: `$HOME` is never a write grant, only places inside it are.
    let home = home();
    for filter in &filters {
        if filter.term == "file-write*" && filter.form == "subpath" {
            assert_ne!(PathBuf::from(&filter.value), home, "{text}");
        }
    }

    // §2: a `Bash` pattern grants no file access.
    assert!(!text.contains("cargo test"), "{text}");

    // The repository's hooks and config are protected once it exists, and
    // not before, so a command can still `git init` a new project.
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let text = macos::profile_text(&profile);
    for name in [".git/hooks", ".git/config"] {
        let deny = format!("(deny file-write* (subpath {}))", quoted(&root.join(name)));
        assert!(text.contains(&deny), "{text}");
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
    let text = macos::profile_text(&profile);

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
                macos::confine(&profile, &mut command).unwrap();
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

/// No document can put a network grant in: without Sterna's proxy the
/// profile denies the network outright, and with it a command reaches this
/// machine only, where the proxy listens.
#[test]
fn the_macos_profile_reaches_the_network_only_through_the_proxy() {
    let fixture = Fixture::new("network");
    for settings in [
        None,
        Some(r#"{"permissions":{"allow":["WebFetch(domain:example.com)"]}}"#),
        Some(r#"{"permissions":{"allow":["WebFetch","WebSearch","Bash(curl*)"]}}"#),
    ] {
        let profile = fixture.profile(settings);
        let text = macos::profile_text(&profile);
        assert!(text.contains("(deny network*)"), "{settings:?}: {text}");
        assert!(!profile.grants_network(), "{settings:?}");
        assert!(!text.contains("(allow network"), "{settings:?}: {text}");
    }
    let proxied = fixture.profile(None).with_proxy(ProxyRoute {
        port: 1,
        unix: None,
        env: Vec::new(),
    });
    let text = macos::profile_text(&proxied);
    let network: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("(deny network") || l.starts_with("(allow network"))
        .collect();
    assert_eq!(
        network,
        [
            "(deny network*)",
            "(allow network-bind (local ip \"localhost:*\"))",
            "(allow network-inbound (local ip \"localhost:*\"))",
            "(allow network-outbound (remote ip \"localhost:*\"))",
        ],
        "{text}"
    );
    // Full access has no sandbox, so no proxy route either.
    assert!(!proxied.clone().with_os_sandbox_bypass().grants_network());
}

#[test]
fn the_macos_profile_denies_writing_dot_claude_inside_the_project() {
    let fixture = Fixture::new("dotclaude");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let root = fixture.resolved(&profile);
    let text = macos::profile_text(&profile);

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
    let text = macos::profile_text(&profile);
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

    // A link in the scratchpad's place is not the scratchpad: no applier
    // grants it, and `.sterna` stays denied.
    #[cfg(unix)]
    {
        std::fs::remove_dir(fixture.root.join(".sterna/scratch")).unwrap();
        std::os::unix::fs::symlink("..", fixture.root.join(".sterna/scratch")).unwrap();
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let text = macos::profile_text(&profile);
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
            macos::confine(&profile, &mut command).unwrap();
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
            macos::confine(&profile, &mut command).unwrap();
            command.output().unwrap()
        };

        for (source, target) in [
            (".sterna/config.toml", ".sterna/scratch/hard"),
            (".claude/settings.json", ".sterna/scratch/hard-claude"),
            (".claude/settings.json", "hard-claude"),
            // `.git/config` runs outside the sandbox later (an alias, a
            // hook path), so it is read-only too, and a link to it is
            // refused like one to `.claude`.
            (".git/config", "hard-git"),
            (".git/config", ".sterna/scratch/hard-git"),
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
        macos::profile_text(&narrowed),
        macos::profile_text(&profile),
    );
    assert_eq!(
        format!(
            "{:?}",
            linux::landlock_rules(&narrowed, sterna::sandbox::linux::Secrets::Ruleset)
        ),
        format!(
            "{:?}",
            linux::landlock_rules(&profile, sterna::sandbox::linux::Secrets::Ruleset)
        ),
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

/// The wide sandbox, applied: a confined process reads outside the project
/// but not a secret, and writes the project but not outside the writable
/// places.
#[test]
fn a_sandboxed_process_reads_widely_and_writes_only_the_writable_places() {
    #[cfg(not(target_os = "macos"))]
    eprintln!("skipped: seatbelt is macOS-only; the Linux equivalent is wide_sandbox.rs");
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        let fixture = Fixture::new("exec");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let outside = fixture.write(&fixture.outside.join("outside.txt"), "outside-text\n");
        let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("sandbox-apply-{}-made", std::process::id()));
        let _ = std::fs::remove_file(&target);

        let run = |line: &str| {
            let mut command = Command::new("/bin/sh");
            command
                .args(["-c", line])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            macos::confine(&profile, &mut command).unwrap();
            command.output().unwrap()
        };

        let read = run(&format!("cat '{}'", outside.display()));
        assert!(read.status.success(), "{read:?}");
        assert_eq!(String::from_utf8_lossy(&read.stdout), "outside-text\n");
        let wrote = run(&format!("echo x > '{}'", root.join("made").display()));
        assert!(wrote.status.success(), "{wrote:?}");
        let refused = run(&format!("echo x > '{}'", target.display()));
        assert!(!refused.status.success(), "{refused:?}");
        assert!(!target.exists());
        let secret = home().join(".ssh");
        let listed = run(&format!("ls '{}'", secret.display()));
        assert!(
            !listed.status.success() || listed.stdout.is_empty(),
            "{listed:?}"
        );
    }
}

// --- every platform: the regime is reported ----------------------------

#[test]
fn the_reported_regime_matches_what_was_applied() {
    let fixture = Fixture::new("regime");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));

    // macOS: the sentence names the wide read, the secrets, and the network
    // the profile renders.
    let said = macos::describe(&profile);
    assert!(
        said.contains("every file is readable except the secrets"),
        "{said}"
    );
    assert!(said.contains("no network"), "{said}");
    assert!(said.contains("no Mach service"), "{said}");
    let proxied = profile.clone().with_proxy(ProxyRoute {
        port: 1,
        unix: None,
        env: Vec::new(),
    });
    assert!(
        macos::describe(&proxied).contains("only through Sterna's proxy"),
        "{}",
        macos::describe(&proxied)
    );

    // Linux: only the namespaced regime reaches any host, and each says
    // what it does not enforce.
    assert!(linux::Regime::Namespaced { abi: 4 }.reaches_network());
    for regime in [
        linux::Regime::LandlockAndSeccomp { abi: 4 },
        linux::Regime::Unconfined,
    ] {
        assert!(!regime.reaches_network(), "{regime}");
    }
    assert!(
        linux::Regime::LandlockAndSeccomp { abi: 3 }
            .describe()
            .contains("no network"),
    );
    assert!(
        linux::Regime::LandlockAndSeccomp { abi: 3 }
            .describe()
            .contains("Sterna's own checks only"),
        "the unprotected paths must be said"
    );
    assert!(
        linux::Regime::Unconfined
            .describe()
            .contains("refuses to spawn tools"),
    );

    // Windows. There are two regimes and no third: the AppContainer, or a
    // refusal.
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
    assert!(cage.contains("Windows Firewall service"), "{cage}");
    assert!(cage.contains("two writable roots"), "{cage}");
    assert!(cage.contains("%LOCALAPPDATA%\\Packages"), "{cage}");
    assert!(cage.contains("this user's own SID"), "{cage}");
    assert!(
        windows::Regime::Unconfined
            .describe()
            .contains("refusal, not a degraded mode"),
        "an unconfinable host must say it spawned nothing"
    );

    #[cfg(target_os = "linux")]
    {
        // The host's own answer, whatever it is: asserting a particular
        // regime would be asserting the CI image's kernel.
        let live = linux::regime();
        let abi = linux::landlock_abi();
        let expected = if abi < 3 || !linux::seccomp_supported_arch() {
            linux::Regime::Unconfined
        } else if sterna::sandbox::linux_ns::available() {
            linux::Regime::Namespaced { abi }
        } else {
            linux::Regime::LandlockAndSeccomp { abi }
        };
        assert_eq!(live, expected, "{live:?} at ABI {abi}");
        assert_eq!(
            live.reaches_network(),
            sterna::sandbox::proxy::reachable() && live != linux::Regime::Unconfined
        );
    }
}

// --- every platform: nothing widens ------------------------------------

#[test]
fn no_runtime_input_can_widen_a_grant() {
    // (a) The profile decides, not the applier. A document whose `deny`
    // covers the project root makes the root no writable place.
    let fixture = Fixture::new("widen");
    let root_pattern = fixture.root.to_string_lossy().replace('\\', "/");
    let denied = fixture.profile(Some(&format!(
        r#"{{"permissions":{{"allow":["Write({root_pattern}/**)"],"deny":["Write({root_pattern}/**)"]}}}}"#
    )));
    let root = fixture.resolved(&denied);
    let text = macos::profile_text(&denied);
    assert!(
        !text.contains(&format!("(allow file-write* (subpath {}))", quoted(&root))),
        "{text}"
    );
    assert!(!denied.writable_places().contains(&root));
    assert!(
        !linux::landlock_rules(&denied, sterna::sandbox::linux::Secrets::Ruleset)
            .read_write
            .contains(&root),
        "{:?}",
        linux::landlock_rules(&denied, sterna::sandbox::linux::Secrets::Ruleset)
    );
    assert!(
        windows::acl_grants(&denied, Path::new(RESOLVED))
            .read_write
            .is_empty(),
        "{:?}",
        windows::acl_grants(&denied, Path::new(RESOLVED))
    );

    // (b) A project directory name cannot close a profile term and be read
    // as more profile.
    let evil = std::env::temp_dir().join(format!(
        "sterna-sbx-{}-a\"b\\c) (allow file-write* (subpath \"/",
        std::process::id()
    ));
    // Best effort: `"` is not a legal filename character on Windows.
    let _ = std::fs::create_dir_all(&evil);
    let injected = Profile::compile(&evil, None);
    let text = macos::profile_text(&injected);
    let _ = std::fs::remove_dir_all(&evil);
    let grants = text
        .lines()
        .filter(|line| line.starts_with("(allow file-write* (subpath"))
        .count();
    assert_eq!(
        grants,
        injected.writable_places().len() + 1,
        "the directory name opened a write grant of its own: {text}"
    );
    assert!(text.contains(r#"a\"b\\c"#), "not escaped: {text}");

    // (c) The text is a function of the profile and of nothing else, and a
    // command pattern changes nothing the OS layer renders: it pre-approves
    // a command line and grants no path.
    let stable = fixture.profile(Some(&settings_for(&fixture.root)));
    assert_eq!(macos::profile_text(&stable), macos::profile_text(&stable));
    for pre_approving in [
        format!(
            r#"{{"permissions":{{"allow":["Read({root_pattern}/**)","Edit({root_pattern}/src/**/*.rs)","Bash"],"deny":["Read({root_pattern}/secrets/**)"]}}}}"#
        ),
        format!(
            r#"{{"permissions":{{"allow":["Read({root_pattern}/**)","Edit({root_pattern}/src/**/*.rs)"],"deny":["Read({root_pattern}/secrets/**)"]}}}}"#
        ),
    ] {
        let other = fixture.profile(Some(&pre_approving));
        assert_eq!(macos::profile_text(&other), macos::profile_text(&stable));
        assert_eq!(
            linux::landlock_rules(&other, sterna::sandbox::linux::Secrets::Ruleset),
            linux::landlock_rules(&stable, sterna::sandbox::linux::Secrets::Ruleset)
        );
    }

    // (d) No document reaches the network on any platform: only the host's
    // own proxy route does.
    assert!(macos::profile_text(&stable).contains("(deny network*)"));
    assert!(!stable.grants_network());
    assert!(!windows::acl_grants(&stable, Path::new(RESOLVED)).internet_client);
}

// --- Linux -------------------------------------------------------------

/// The wide ruleset: everything off the way to a secret is read and run,
/// each directory on the way is listed only, and the writable places are
/// granted whole -- or write without read when one holds a secret. A
/// secret inside a temp folder is the one Landlock alone leaves to Sterna's
/// own check: carving it out would break the temp folder.
#[test]
fn the_landlock_ruleset_is_exactly_the_declared_paths() {
    let fixture = Fixture::new("landlock");
    let profile = fixture.profile(Some(&settings_for(&fixture.root)));
    let rules = linux::landlock_rules(&profile, sterna::sandbox::linux::Secrets::Ruleset);
    let temp: Vec<&Path> = profile.temp_dirs().collect();
    let secrets: Vec<PathBuf> = profile
        .secret_paths()
        .into_iter()
        .filter(|secret| !temp.iter().any(|dir| secret.starts_with(dir)))
        .collect();
    assert!(!secrets.is_empty(), "the fixture home has secrets to hide");
    for dir in &temp {
        assert!(
            rules.read_write.iter().any(|place| place == dir),
            "{} is not writable whole: {rules:?}",
            dir.display()
        );
    }

    for secret in &secrets {
        for granted in rules.read.iter().chain(rules.read_write.iter()) {
            assert!(
                !secret.starts_with(granted) && !granted.starts_with(secret),
                "{} reaches the secret {}: {rules:?}",
                granted.display(),
                secret.display()
            );
        }
    }
    for listed in &rules.list {
        assert!(
            secrets.iter().any(|secret| secret.starts_with(listed)),
            "{} is listed but leads to no secret: {rules:?}",
            listed.display()
        );
    }
    assert_eq!(rules.read_write[0], PathBuf::from("/dev/null"));
    for place in profile.writable_places() {
        let holds = secrets.iter().any(|secret| secret.starts_with(&place));
        assert_eq!(
            rules.read_write.contains(&place),
            !holds,
            "{}: {rules:?}",
            place.display()
        );
        // A place that holds a secret is still writable -- write without
        // read -- so files can be created directly in it.
        assert_eq!(
            rules.write.contains(&place),
            holds,
            "{}: {rules:?}",
            place.display()
        );
    }
    assert_eq!(linux::access::WRITE & linux::access::READ, 0);
    assert_ne!(linux::access::WRITE & linux::access::MAKE_REG, 0);
    assert!(
        !rules.read_write.contains(&home()),
        "the home folder is writable: {rules:?}"
    );
    // Where mounts cover the secrets, every writable place is granted whole,
    // so a file can be created directly in a place that holds a secret.
    let covered = linux::landlock_rules(&profile, linux::Secrets::Covered);
    for place in profile.writable_places() {
        assert!(covered.read_write.contains(&place), "{covered:?}");
    }
    assert_eq!(covered.read, [PathBuf::from("/")], "{covered:?}");
    assert!(covered.list.is_empty(), "{covered:?}");

    // A read grant is open, list and run -- never a write and never a
    // `MAKE_*`; a directory on the way to a secret is list only.
    assert_eq!(
        linux::access::READ,
        linux::access::READ_FILE | linux::access::READ_DIR | linux::access::EXECUTE
    );
    assert_eq!(linux::access::LIST, linux::access::READ_DIR);
    assert_eq!(linux::access::READ & linux::access::WRITE_FILE, 0);
    assert_eq!(linux::access::READ & linux::access::MAKE_REG, 0);
    assert_eq!(linux::access::HANDLED, linux::access::READ_WRITE);
    assert_ne!(linux::access::READ_WRITE & linux::access::TRUNCATE, 0);
    // A rule on a file may carry no directory-only right: the kernel refuses
    // the entire ruleset if it does.
    assert_eq!(linux::access::FILE & linux::access::READ_DIR, 0);
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

// --- the exec grant: any program under the roots, the project and the
// toolchains, never a binary outside them --------------------------------

/// Every program may run: the sandbox bounds what a command touches, not
/// which programs it starts.
#[test]
fn a_confined_process_runs_any_program() {
    #[cfg(not(target_os = "macos"))]
    eprintln!("skipped: seatbelt is macOS-only; the Linux equivalent is wide_sandbox.rs");
    #[cfg(target_os = "macos")]
    {
        use std::process::{Command, Stdio};

        let fixture = Fixture::new("sibling");
        let profile = fixture.profile(Some(&settings_for(&fixture.root)));
        let root = fixture.resolved(&profile);
        let inside = fixture.write(&root.join("inside.txt"), "inside-text\n");

        let run = |program: &str, arg: &Path| {
            let mut command = Command::new(program);
            command
                .arg(arg)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            macos::confine(&profile, &mut command).unwrap();
            command.output().unwrap()
        };

        let granted = run("/bin/cat", &inside);
        assert!(granted.status.success(), "{granted:?}");
        assert_eq!(String::from_utf8_lossy(&granted.stdout), "inside-text\n");
        let sibling = run("/bin/echo", Path::new("sibling-marker"));
        assert!(sibling.status.success(), "{sibling:?}");
        assert!(
            String::from_utf8_lossy(&sibling.stdout).contains("sibling-marker"),
            "{sibling:?}"
        );
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
            macos::confine(&profile, &mut command).unwrap();
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
