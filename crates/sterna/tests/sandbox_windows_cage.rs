//! What the Windows cage actually did — measured by putting a real child in
//! front of a real file, and asked in both directions.
//!
//! A test that only says "denied" proves nothing: a sandbox that refused the
//! loader itself, or a path that simply did not exist, would pass it. So every
//! executing case below has an unconfined control, and the confined half is
//! asked what it *could* do as well as what it could not.
//!
//! `sandbox-grants.md` §1 is what is being checked, quoted rather than
//! paraphrased:
//!
//! 2. *"`deny` beats `allow`, at every specificity. A path matched by any
//!    `deny` pattern is refused even when a longer, more specific `allow`
//!    names it exactly."*
//! 3. *"The project root is the only writable root by default. Not the home
//!    directory, not a temp directory, not the parent of the project."*
//! 5. *"`.claude/**` is therefore also in the deny-write set by default"* —
//!    because `.claude` lives inside the writable root, and a program that
//!    could write it would be widening its own sandbox.
//!
//! The two halves of this file are gated differently on purpose. The
//! command-line and environment builders are pure functions of UTF-16 and are
//! asserted on **every** host, because an argument-quoting defect is an
//! argument-injection defect and it should not need a Windows runner to find.
//! The cage itself is asserted where it exists.

use sterna::sandbox::windows;

// The project fixture, and everything that reads a real ACL, exists only
// where the cage does. `warnings = deny` makes an unreachable helper a build
// failure, which is the rule this file obeys rather than works around.
#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(target_os = "windows")]
use sterna::sandbox::profile::Profile;

#[cfg(target_os = "windows")]
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A throwaway project directory with a `.claude/` in it, and one directory
/// outside it, removed when the test finishes.
///
/// **What it cannot remove, said out loud:** each fixture root derives a new
/// AppContainer profile name, and the first spawn against it registers that
/// profile permanently under `%LOCALAPPDATA%\Packages`. `Drop` takes the
/// directories back and not the profiles, because
/// `DeleteAppContainerProfile` is deliberately absent from this crate
/// (`sandbox-grants.md` §7 names it a successor) and a test is not the place
/// to introduce it. So a machine that runs this file repeatedly accumulates
/// one registered profile per fixture per run. That is bounded per run and
/// visible here rather than surprising later.
#[cfg(target_os = "windows")]
struct Fixture {
    root: PathBuf,
    outside: PathBuf,
}

#[cfg(target_os = "windows")]
impl Fixture {
    fn new(label: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let stem = format!("sterna-windows-cage-{}-{label}-{n}", std::process::id());
        let root = std::env::temp_dir().join(&stem);
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        let outside = std::env::temp_dir().join(format!("{stem}-outside"));
        std::fs::create_dir_all(&outside).unwrap();
        Self { root, outside }
    }

    fn profile(&self) -> Profile {
        Profile::compile(&self.root, Some(&settings_for(&self.root)))
    }
}

#[cfg(target_os = "windows")]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.outside);
    }
}

/// The whole project readable and writable, with one `deny` inside it that a
/// longer `allow` also names — invariant 2's exact shape, not a paraphrase of
/// it.
///
/// Backslashes become forward slashes because the document is JSON: a Windows
/// path interpolated raw carries `\U`, `\A` and `\T`, the document fails to
/// parse, no rule compiles, and the implicit root grant admits everything.
/// That is a real defect this repository has already paid for once.
#[cfg(target_os = "windows")]
fn settings_for(root: &Path) -> String {
    let root = root.to_string_lossy().replace('\\', "/");
    format!(
        r#"{{"permissions":{{"allow":["Read({root}/**)","Write({root}/**)","Read({root}/secrets/token.txt)","Bash(echo*)"],"deny":["Read({root}/secrets/**)"]}}}}"#
    )
}

// --- every host: the two pure builders ---------------------------------

/// `CommandLineToArgvW`'s rules, asserted rather than assumed.
///
/// `CreateProcessW` takes one string and the child re-splits it, so a quoting
/// defect here is an argument-injection defect — and the argument that would
/// be injected is a path the model chose. The three cases are the three the
/// rule is actually about: a space needs quotes, a `"` needs escaping along
/// with every backslash in front of it, and a run of backslashes at the end
/// of a quoted argument has to be doubled because the closing quote follows
/// it.
#[test]
fn an_argument_cannot_break_out_of_the_quoting_that_carries_it() {
    let quoted = |argument: &str, force: bool| {
        let units: Vec<u16> = argument.encode_utf16().collect();
        let mut out = Vec::new();
        windows::quote_argument(&units, &mut out, force);
        String::from_utf16(&out).unwrap()
    };

    // Plain, and left alone.
    assert_eq!(quoted("rg", false), "rg");
    // A space is what quotes exist for.
    assert_eq!(
        quoted(r"C:\Program Files\Git\bin\bash.exe", false),
        "\"C:\\Program Files\\Git\\bin\\bash.exe\""
    );
    // An empty argument still has to arrive as an argument.
    assert_eq!(quoted("", false), "\"\"");
    // The injection attempt: a quote the child would otherwise read as the
    // end of this argument, letting everything after it become argv of its
    // own.
    assert_eq!(quoted(r#"a" && whoami"#, false), r#""a\" && whoami""#);
    // Backslashes are literal until a quote follows them, and then the whole
    // run doubles.
    assert_eq!(quoted(r"a\b", false), r"a\b");
    assert_eq!(quoted(r#"a\"b"#, false), r#"a\\\"b"#);
    // A trailing run inside quotes doubles, because the closing quote is
    // what follows it.
    assert_eq!(quoted(r"C:\dir\", true), r#""C:\dir\\""#);

    // And the whole line: the program is quoted unconditionally, the
    // arguments are separated by one space, and it is NUL-terminated.
    let program: Vec<u16> = r"C:\bin\cat.exe".encode_utf16().collect();
    let arguments: Vec<Vec<u16>> = ["--", r"C:\a b\c.txt"]
        .iter()
        .map(|argument| argument.encode_utf16().collect())
        .collect();
    let line = windows::command_line(&program, &arguments);
    assert_eq!(line.last(), Some(&0), "the command line must be terminated");
    assert_eq!(
        String::from_utf16(&line[..line.len() - 1]).unwrap(),
        r#""C:\bin\cat.exe" -- "C:\a b\c.txt""#
    );
}

/// `cmd.exe` reads its own line, and the command tool's line is built the
/// way cmd reads it.
///
/// The injection question is asked of the shape that actually runs. With
/// `/s`, cmd pairs the first quote after `/c` with the last quote on the
/// line, so a `"` inside the command — the `findstr /c:"…"` every real
/// verification writes — reaches cmd unescaped, and a command that itself
/// ends in a quote still closes where the builder closed it. The second half
/// is the defect the shape exists to avoid: `command_line` would hand cmd
/// `\"`, an escape cmd never removes.
#[test]
fn the_shell_line_carries_the_command_verbatim_between_one_pair_of_quotes() {
    let units = |text: &str| -> Vec<u16> { text.encode_utf16().collect() };
    let cmd = units(r"C:\Windows\System32\cmd.exe");
    let switches: Vec<Vec<u16>> = ["/d", "/s", "/c"].iter().map(|s| units(s)).collect();
    let command = r#"findstr /x /c:"value = 10" src\a.py >nul && echo "done""#;
    let line = windows::shell_command_line(&cmd, &switches, &units(command));
    assert_eq!(line.last(), Some(&0), "the command line must be terminated");
    assert_eq!(
        String::from_utf16(&line[..line.len() - 1]).unwrap(),
        format!(r#""C:\Windows\System32\cmd.exe" /d /s /c "{command}""#)
    );

    let quoted = windows::command_line(&cmd, &[units("/c"), units(r#"findstr /c:"x""#)]);
    assert!(
        String::from_utf16(&quoted[..quoted.len() - 1])
            .unwrap()
            .contains(r#"\""#),
        "the argv quoting escapes a quote cmd would have read verbatim"
    );
}

/// The credential scrub, and the case-folding that makes it a scrub rather
/// than a hope.
///
/// `invoke` removes provider credentials from a confined child **by name**,
/// and on Windows that name is compared case-insensitively by the operating
/// system. A block builder that compared exactly would leave
/// `Anthropic_Api_Key` in a child's environment while reporting that it had
/// removed `ANTHROPIC_API_KEY`.
#[test]
fn the_child_environment_is_built_by_removal_and_folds_case_like_windows_does() {
    let units = |text: &str| -> Vec<u16> { text.encode_utf16().collect() };
    let entry = |name: &str, value: &str| (units(name), units(value));

    let block = windows::environment_block(
        [
            entry("Path", r"C:\Windows"),
            entry("Anthropic_Api_Key", "must-not-survive-the-scrub"),
            entry("ZULU", "z"),
            entry("alpha", "a"),
        ],
        [
            (units("ANTHROPIC_API_KEY"), None),
            (units("PATH"), Some(units(r"C:\Windows;C:\bin"))),
        ],
    );
    let text = String::from_utf16(&block).unwrap();
    let entries: Vec<&str> = text.split('\0').filter(|part| !part.is_empty()).collect();

    // The removal reached a differently-cased name, which is the whole point.
    assert!(
        !text.to_ascii_uppercase().contains("ANTHROPIC_API_KEY"),
        "the credential survived the scrub: {entries:?}"
    );
    assert!(
        !text.contains("must-not-survive-the-scrub"),
        "the credential's value survived: {entries:?}"
    );
    // A set, not an append: one PATH, and it is the changed one.
    assert_eq!(
        entries
            .iter()
            .filter(|part| part.to_ascii_uppercase().starts_with("PATH="))
            .count(),
        1,
        "{entries:?}"
    );
    // The change's own spelling of the name is what the child sees, which is
    // `std::process::Command::env`'s behaviour and not an invention here.
    assert!(entries.contains(&r"PATH=C:\Windows;C:\bin"), "{entries:?}");
    // Sorted by *folded* name, which `CreateProcessW` documents as required:
    // `alpha` sorts first because it is compared as `ALPHA`, not as `alpha`,
    // which would put it last.
    assert_eq!(
        entries,
        vec!["alpha=a", r"PATH=C:\Windows;C:\bin", "ZULU=z"]
    );
    // Double-NUL terminated, and an empty block is still two units.
    assert_eq!(&block[block.len() - 2..], &[0, 0]);
    assert_eq!(
        windows::environment_block(
            Vec::<(Vec<u16>, Vec<u16>)>::new(),
            Vec::<(Vec<u16>, Option<Vec<u16>>)>::new()
        ),
        vec![0]
    );
}

// --- Windows: the cage, with a real child in front of a real file ------

/// One confined run, reported rather than asserted on the way past.
#[cfg(target_os = "windows")]
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    /// Which container the child actually entered — the report's answer to
    /// "and with what SID", by the name the SID is derived from.
    container: String,
}

#[cfg(target_os = "windows")]
impl Ran {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

#[cfg(target_os = "windows")]
impl std::fmt::Debug for Ran {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "in {} exit {:?} stdout {:?} stderr {:?}",
            self.container,
            self.code,
            self.stdout.trim(),
            self.stderr.trim()
        )
    }
}

/// `cmd.exe`, which every Windows install has in `System32` and which carries
/// the `ALL APPLICATION PACKAGES` ACE every system binary does.
///
/// `type` and `mkdir` are its own builtins, so the whole probe needs one
/// image and no shell metacharacter: each argument below is a literal path
/// and nothing is re-parsed.
#[cfg(target_os = "windows")]
fn shell() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
    PathBuf::from(root).join(r"System32\cmd.exe")
}

/// Runs `arguments` inside the cage `profile` implies, and reports.
#[cfg(target_os = "windows")]
fn caged(profile: &Profile, arguments: &[&std::ffi::OsStr]) -> Result<Ran, String> {
    caged_program(profile, &shell(), arguments)
}

#[cfg(target_os = "windows")]
fn caged_program(
    profile: &Profile,
    program: &Path,
    arguments: &[&std::ffi::OsStr],
) -> Result<Ran, String> {
    use std::io::Read;
    let mut command = std::process::Command::new(program);
    command.args(arguments).current_dir(profile.root());
    let mut child = windows::spawn(
        profile,
        program,
        &command,
        windows::Pipes {
            stdin: false,
            stdout: true,
            stderr: true,
        },
        windows::LineShape::Argv,
    )
    .map_err(|error| error.to_string())?;
    // The outputs here are a line each, so a sequential read cannot fill the
    // other pipe. `tools::invoke` drains both on threads for the general
    // case; this file deliberately keeps the probe small enough not to need
    // that machinery in the way of what it is measuring.
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    Ok(Ran {
        code: status.code(),
        stdout,
        stderr,
        container: child.container.clone(),
    })
}

/// The same argv with no cage at all, so a refusal cannot be a missing file.
#[cfg(target_os = "windows")]
fn free(arguments: &[&std::ffi::OsStr]) -> Ran {
    let output = std::process::Command::new(shell())
        .args(arguments)
        .output()
        .unwrap();
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        container: "no container (the unconfined control)".to_string(),
    }
}

/// The decisive test: what a real child could and could not open.
///
/// Both halves, and the denial half is the one that matters — but it is only
/// worth anything beside the grant half, because a cage that refuses
/// everything is not a cage, it is a broken loader.
#[test]
fn a_caged_child_reads_the_project_and_cannot_reach_past_it() {
    #[cfg(not(target_os = "windows"))]
    eprintln!(
        "skipped: the AppContainer is a Win32 object; this host has no applier for it. The \
         command-line and environment builders in this file are checked here."
    );
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        let fixture = Fixture::new("child");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();

        let inside = root.join("inside.txt");
        std::fs::write(&inside, "inside-secret\n").unwrap();
        let denied = root.join("secrets").join("token.txt");
        std::fs::write(&denied, "denied-secret\n").unwrap();
        let outside = fixture.outside.join("outside.txt");
        std::fs::write(&outside, "outside-secret\n").unwrap();

        // Which container this project and this user derive, before any of
        // it is created. The SID is a pure function of the name, so the name
        // is the identity: `sandbox_apply.rs` asserts that two users on one
        // root do not share it, and the `icacls` dump below prints the SID
        // itself.
        let user = windows::current_user_sid().expect("this process has a user SID");
        eprintln!(
            "container: {} for user {user}",
            windows::container_name(&profile, &user)
        );

        // What the cage says it is, before any child runs.
        let regime = windows::regime(&profile);
        assert_eq!(regime, windows::Regime::AppContainer, "{regime}");
        eprintln!("regime: {regime}");
        eprintln!("network: {}", windows::network_isolation());

        let read = |path: &Path| -> Result<Ran, String> {
            caged(
                &profile,
                &[OsStr::new("/c"), OsStr::new("type"), path.as_ref()],
            )
        };

        // --- the grant half ---------------------------------------------
        //
        // Without this, every assertion below would pass on a cage that
        // refused the loader.
        let granted = read(&inside).expect("the project's own file must be spawnable");
        assert!(granted.ok(), "{granted:?}");
        assert!(
            granted.stdout.contains("inside-secret"),
            "the cage refused the project itself: {granted:?}"
        );

        // --- invariant 3: the project root is the only writable root -----
        let refused = read(&outside).expect("the spawn itself must succeed; the open must not");
        assert!(
            !refused.stdout.contains("outside-secret"),
            "a read outside the project root was admitted: {refused:?}"
        );
        // And the file is readable without the cage, so the refusal is the
        // cage's and not the filesystem's.
        let control = free(&[OsStr::new("/c"), OsStr::new("type"), outside.as_ref()]);
        assert!(
            control.stdout.contains("outside-secret"),
            "the control could not read it either, so the case proves nothing: {control:?}"
        );

        // --- invariant 2: `deny` beats a longer `allow` ------------------
        //
        // The settings document allows `secrets/token.txt` by name -- longer
        // and more specific -- and denies `secrets/**` by glob. The exact
        // allow does not win.
        //
        // Asked through `tools::invoke::run`, because that is where the
        // product decides: `Profile::check` runs before the spawn, so the
        // refusal arrives with **no child created at all**, which is a
        // stronger outcome than a child that tried and failed.
        let call = sterna::tools::invoke::run(
            &sterna::tools::invoke::ToolContext {
                profile: &profile,
                session: &sterna::contract::SessionId::new("windows-cage-deny"),
            },
            "read",
            &sterna::tools::invoke::Args::new().with("path", denied.to_string_lossy()),
        );
        match call {
            Err(sterna::tools::invoke::ToolError::Denied(refusal)) => {
                assert!(
                    refusal.rule.contains("deny"),
                    "the refusal must name the deciding rule: {refusal:?}"
                );
            }
            other => panic!("a path a `deny` names was not refused: {other:?}"),
        }

        // And what the OS layer alone does with it, reported rather than
        // asserted. ACLs are per-object: the container's grant is on the
        // project root and inherits down, so a `deny` on a subtree is
        // enforced by Sterna's pre-call check and not by the ACL --
        // `sandbox-grants.md` §3 says exactly that of both Windows and
        // Linux. Asserting either outcome here would freeze a coarseness the
        // document already records as a limitation.
        let os_layer = read(&denied).expect("the spawn must succeed whatever the open does");
        eprintln!(
            "OS layer on a denied subtree (pre-call check is the exact enforcement): {}",
            if os_layer.stdout.contains("denied-secret") {
                "read admitted, as the directory-granular ACL implies"
            } else {
                "read refused"
            }
        );

        // --- invariant 5: a program cannot widen its own profile ---------
        //
        // `.claude` lives inside the writable root, so nothing but an
        // explicit carve-out keeps a program from rewriting the document its
        // own sandbox came from.
        let make = |path: &Path| -> Result<Ran, String> {
            caged(
                &profile,
                &[OsStr::new("/c"), OsStr::new("mkdir"), path.as_ref()],
            )
        };
        let allowed_dir = root.join("made-by-the-cage");
        let made = make(&allowed_dir).expect("a write inside the project must be spawnable");
        assert!(made.ok(), "{made:?}");
        assert!(
            allowed_dir.is_dir(),
            "the cage refused a write the document grants: {made:?}"
        );

        // Which ACEs landed where. This is the report the package exists to
        // produce: not "denied", but which principal was granted what, on
        // which object, in the order an access check reads them.
        for path in [&root, &root.join(".claude"), &fixture.outside] {
            match windows::container_aces(&profile, path) {
                Ok(aces) => eprintln!("acl {}: {aces:?}", path.display()),
                Err(error) => eprintln!("acl {}: unreadable ({error})", path.display()),
            }
        }
        eprintln!(
            "rights: read {:#010x} read+write {:#010x}",
            windows::READ_RIGHTS,
            windows::READ_WRITE_RIGHTS
        );

        // The carve-out is an **absence of grant**, not a DENY: the container
        // holds exactly the read bits on `.claude` and nothing more.
        // Measured 2026-09-09 on the VM: a DENY naming an AppContainer SID
        // decides nothing, because an AppContainer's check is a grant check.
        let carved = windows::container_aces(&profile, &root.join(".claude")).unwrap();
        assert_eq!(
            carved,
            vec![windows::Ace {
                allow: true,
                inherited: false,
                mask: windows::READ_RIGHTS,
            }],
            "the carve-out must be the read grant alone, with the root's inheritable write \
             grant blocked rather than denied"
        );

        // The carve-out's own grant half: the document a session was compiled
        // from stays readable, or the session could not start.
        let settings = root.join(".claude").join("settings.json");
        std::fs::write(&settings, "{}\n").unwrap();
        let readable = read(&settings).expect("reading .claude must be spawnable");
        assert!(
            readable.ok() && readable.stdout.contains("{}"),
            "{readable:?}"
        );

        let claude_dir = root.join(".claude").join("made-by-the-cage");
        let claude = make(&claude_dir).expect("the spawn must succeed; the write must not");
        assert!(
            !claude_dir.exists(),
            "a program wrote into .claude and can now widen its own grant: {claude:?}"
        );

        let outside_dir = fixture.outside.join("made-by-the-cage");
        let past = make(&outside_dir).expect("the spawn must succeed; the write must not");
        assert!(
            !outside_dir.exists(),
            "a program wrote outside the project root: {past:?}"
        );

        // And nobody else lost anything. Blocking inheritance on `.claude`
        // copies the inherited ACEs into it first, so the developer, `SYSTEM`
        // and `Administrators` keep exactly the access they had -- sterna must
        // not leave the machine less usable than it found it any more than
        // more permissive.
        let listed = free(&[
            OsStr::new("/c"),
            OsStr::new("icacls"),
            root.join(".claude").as_ref(),
        ]);
        for principal in ["NT AUTHORITY\\SYSTEM", "BUILTIN\\Administrators"] {
            assert!(
                listed.stdout.contains(&format!("{principal}:(OI)(CI)(F)")),
                "{principal} lost its access to .claude: {listed:?}"
            );
        }
    }
}

/// Which binaries this machine's AppContainer can actually load, and the
/// refusal for the ones it cannot.
///
/// The ruling this checks: sterna does **not** write ACEs onto files outside
/// the project to make a tool runnable. A binary a package manager installed
/// without the inherited `ALL APPLICATION PACKAGES` ACE is refused by name
/// instead — a narrower cage that spawns nothing, never a wider one that
/// spawns something.
#[test]
fn a_binary_the_container_cannot_load_is_refused_and_the_report_names_it() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: an image's ACL is a Win32 object; this host has none to read");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        // The diagnostic the ruling asked for: which of the spawning tools'
        // executables this machine's container can load. It is printed rather
        // than asserted -- which tools are installed is a property of the
        // machine, and a test that asserted it would be asserting the runner.
        for tool in sterna::tools::registry::ALL.iter() {
            let name = tool.name();
            // A tool sterna performs itself names no binary, so there is no
            // image for a container to load and nothing here to report.
            let Some(program) = tool.executable() else {
                continue;
            };
            let grant = sterna::tools::invoke::exec_grant(program);
            if grant.fell_back_to_roots {
                eprintln!("tool {name} (`{program}`): not on PATH");
                continue;
            }
            match windows::image_admits_app_containers(&grant.binary) {
                Ok(true) => eprintln!("tool {name}: loadable, {}", grant.binary.display()),
                Ok(false) => eprintln!(
                    "tool {name}: REFUSED, no ALL APPLICATION PACKAGES execute ACE on {}",
                    grant.binary.display()
                ),
                Err(error) => eprintln!("tool {name}: ACL unreadable ({error})"),
            }
        }

        // `cmd.exe` is the control: a system binary the container can load.
        assert!(
            windows::image_admits_app_containers(&shell()).unwrap(),
            "a System32 binary must carry the package ACE, or this test cannot tell the two \
             outcomes apart"
        );

        // And the refusal, on a binary that provably lacks the ACE: one this
        // test writes into a directory it owns. It is a copy of `cmd.exe`, so
        // the only thing that differs from the control is the ACL -- a copy
        // does not inherit the source's explicit ACEs, only the destination
        // directory's.
        let fixture = Fixture::new("image");
        let profile = fixture.profile();
        let copy = fixture.outside.join("no-package-ace.exe");
        std::fs::copy(shell(), &copy).unwrap();
        if windows::image_admits_app_containers(&copy).unwrap() {
            eprintln!(
                "skipped the refusal half: {} inherited a package ACE from its directory, so \
                 this machine cannot produce the unloadable case",
                copy.display()
            );
            return;
        }

        let mut command = std::process::Command::new(&copy);
        command.args([OsStr::new("/c"), OsStr::new("rem")]);
        command.current_dir(profile.root());
        let refusal = windows::spawn(
            &profile,
            &copy,
            &command,
            windows::Pipes {
                stdin: false,
                stdout: true,
                stderr: true,
            },
            windows::LineShape::Argv,
        )
        .err()
        .expect("a binary the container cannot load must not be spawned");
        let text = refusal.to_string();
        assert!(
            matches!(refusal, windows::SpawnError::NotConfinable(_)),
            "an unloadable image is a refusal, not a failed start: {text}"
        );
        assert!(
            text.contains("ALL APPLICATION PACKAGES"),
            "the refusal must name why: {text}"
        );
        assert!(
            text.contains("no-package-ace.exe"),
            "the refusal must name which binary: {text}"
        );
    }
}

/// Whether the tools the registry actually names run inside the cage on this
/// machine, and what they say when they do not.
///
/// A report rather than a fixed expectation: which binaries a given Windows
/// machine has is a property of the machine. What is asserted is the part
/// that is Sterna's — a tool that resolves and whose image the container can
/// load must not be refused by the cage, because a refusal there would be
/// the cage failing rather than the machine lacking a binary.
///
/// **The list is the registry's own, never a second copy of it.** A tool
/// sterna performs in-process names no binary, enters no container and cannot
/// be "not installed": on Windows `read` and `grep` are exactly that
/// (`tools::registry`), and a hard-coded `["cat", "grep", …]` here went on
/// asking the cage about programs the registry had stopped naming — and
/// then asserted a missing-tool refusal against a `read` that had answered.
///
/// **What this reported on the Windows ARM64 host, 2026-09-09, and why it is
/// worth keeping:** Git for Windows' `cat.exe` and `grep.exe` were loadable
/// and were spawned, and then died at start-up with
/// `NtCreateDirectoryObject(\BaseNamedObjects\msys-2.0S5-…): 0xC0000022`;
/// its `bash.exe` exited `0xC0000142`. The msys runtime wants a shared
/// object-manager directory that an AppContainer's redirected namespace will
/// not give it. That is a property of Cygwin/MSYS, not of the cage — the same
/// container runs `cmd.exe` — and it is the reason `read` and `grep` do not
/// work on such a machine. `sandbox-grants.md` §3 records it; this test is
/// how the next machine's answer is obtained rather than assumed.
#[test]
fn every_installed_tool_that_the_container_can_load_actually_runs_in_it() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: there is no AppContainer on this host to run a tool inside");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        let fixture = Fixture::new("tools");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();
        let inside = root.join("inside.txt");
        std::fs::write(&inside, "inside-secret\n").unwrap();

        for tool in sterna::tools::registry::ALL.iter() {
            let name = tool.name();
            let Some(program) = tool.executable() else {
                eprintln!("tool {name}: in-process, no image to load");
                continue;
            };
            let grant = sterna::tools::invoke::exec_grant(program);
            if grant.fell_back_to_roots {
                // **A tool that is not installed is refused, and the refusal
                // says why in terms of the missing tool.** This used to
                // require the *operating system's* answer —
                // `SpawnError::NotStarted` — on the reasoning that "the
                // AppContainer could not be entered" is a frightening thing
                // to tell someone whose only problem is a missing `cat`. The
                // reasoning stands; the classification was wrong, and it cost
                // an escape: `exec_grant`'s fallback hands back the bare
                // name, and `CreateProcessW` completes a partial
                // `lpApplicationName` from Sterna's own current directory,
                // which is the writable project root. So the unresolved
                // branch is now the sandbox's own decision, and the sentence
                // a person reads is the one `tools::invoke` writes.
                let mut command = std::process::Command::new(&grant.binary);
                command.current_dir(profile.root());
                let refusal = windows::spawn(
                    &profile,
                    &grant.binary,
                    &command,
                    windows::Pipes {
                        stdin: false,
                        stdout: true,
                        stderr: true,
                    },
                    windows::LineShape::Argv,
                )
                .err()
                .expect("a name that resolves to nothing cannot start");
                assert!(
                    refusal.to_string().contains("not an absolute path"),
                    "an unresolved name must be refused for the reason it is dangerous, not as a \
                     bare container failure: {refusal}"
                );
                // And the sentence the model actually receives names the tool
                // and the machine, not a path it never chose.
                let path = inside.to_string_lossy().into_owned();
                let args = sterna::tools::invoke::Args::new();
                let args = match name {
                    "read" => args.with("path", path),
                    "jq" => args.with("filter", ".").with("path", path),
                    "bash" => args.with("command", "echo x"),
                    _ => args.with("pattern", "inside").with("path", path),
                };
                let denial = sterna::tools::invoke::run(
                    &sterna::tools::invoke::ToolContext {
                        profile: &profile,
                        session: &sterna::contract::SessionId::new("windows-missing-tool"),
                    },
                    name,
                    &args,
                );
                match &denial {
                    Err(sterna::tools::invoke::ToolError::Denied(refusal)) => assert!(
                        refusal.rule.contains("is not installed on this machine"),
                        "a missing tool must be reported as a missing tool: {refusal:?}"
                    ),
                    other => panic!("a tool that is not installed was not refused: {other:?}"),
                }
                eprintln!("tool {name} (`{program}`): not on PATH, and refused as {refusal}");
                continue;
            }
            match windows::image_admits_app_containers(&grant.binary) {
                Ok(false) => {
                    eprintln!(
                        "tool {name}: REFUSED, no package ACE on {}",
                        grant.binary.display()
                    );
                    continue;
                }
                Err(error) => {
                    eprintln!("tool {name}: ACL unreadable ({error})");
                    continue;
                }
                Ok(true) => {}
            }
            // `cat` is the one whose argv this can drive without knowing
            // the tool: everything else takes a pattern or a subcommand, and
            // `cmd` takes a command line whose one statement does nothing.
            let arguments: Vec<&OsStr> = match program {
                "cat" => vec![inside.as_ref()],
                "cmd" => vec![OsStr::new("/d"), OsStr::new("/c"), OsStr::new("rem")],
                _ => vec![OsStr::new("--version")],
            };
            let ran = caged_program(&profile, &grant.binary, &arguments);
            match ran {
                Ok(result) => eprintln!("tool {name}: ran, {result:?}"),
                Err(refusal) => panic!(
                    "the cage refused `{name}` ({}) even though the container can load it: \
                     {refusal}",
                    grant.binary.display()
                ),
            }
        }
    }
}

/// The wiring, end to end: a real tool call on Windows reaches the
/// AppContainer spawn instead of the refusal that used to stand there.
///
/// This is the user-visible half of the package. Before 2026-09-09 every
/// spawning tool on Windows came back as `PermissionDenied` naming the
/// missing applier, so a Windows user could not read a file or search a
/// project at all. What is asserted is exactly that: whatever `bash` does on
/// this machine, the answer is no longer *"sterna has no sandbox applier"*.
///
/// It does not assert success, and the reason is measured rather than
/// cautious: Git for Windows' `bash.exe` is an MSYS2 image and cannot start
/// inside any AppContainer (`sandbox-grants.md` §3), so on such a machine the
/// honest outcome is a child that started and died, not a tool that worked.
/// A machine with a native `bash` gets `cage-ok` on stdout, and the report
/// below says which happened.
#[test]
fn a_real_tool_call_reaches_the_cage_and_is_not_refused_for_want_of_one() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: the refusal this replaces was Windows-only");
    #[cfg(target_os = "windows")]
    {
        let fixture = Fixture::new("wiring");
        let profile = fixture.profile();
        let outcome = sterna::tools::invoke::run(
            &sterna::tools::invoke::ToolContext {
                profile: &profile,
                session: &sterna::contract::SessionId::new("windows-cage-wiring"),
            },
            "bash",
            &sterna::tools::invoke::Args::new().with("command", "echo cage-ok"),
        );
        match &outcome {
            Ok(result) => eprintln!(
                "bash: {} exit {:?} stdout {:?} stderr {:?}",
                result.confinement.as_str(),
                result.exit_code,
                result.stdout.trim(),
                result.stderr.trim()
            ),
            Err(error) => eprintln!("bash: {error:?}"),
        }
        if let Err(sterna::tools::invoke::ToolError::Denied(refusal)) = &outcome {
            for forbidden in ["no sandbox applier", "unconfined"] {
                assert!(
                    !refusal.rule.contains(forbidden),
                    "a tool call is still refused for want of a confinement: {refusal:?}"
                );
            }
        }
        // And when it did run, it ran in the container rather than beside it.
        if let Ok(result) = &outcome {
            assert_eq!(
                result.confinement,
                sterna::tools::invoke::Confinement::AppContainer,
                "a child ran outside the cage: {result:?}"
            );
        }
    }
}

// --- the three measured escapes, each reproduced and each closed ---------
//
// Every one of these was found by an adversarial reviewer *running a real
// program* through the cage on the Windows ARM64 VM, so every one of them
// runs a real program here too. A test that asserts a mask would have passed
// against all three defects.

/// One unconfined run of an arbitrary program, so a refusal can never be a
/// missing or unrunnable file.
#[cfg(target_os = "windows")]
fn free_program(program: &Path, arguments: &[&std::ffi::OsStr]) -> Ran {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .unwrap();
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        container: "no container (the unconfined control)".to_string(),
    }
}

/// **Escape 1.** The program sterna runs is never one the model could have
/// written, in any spelling.
///
/// The attack, as measured: `tools::invoke::exec_grant` returned
/// `ExecGrant { binary: PathBuf::from("grep"), fell_back_to_roots: true }`
/// for a name that was not installed, `spawn`'s only guard on it was
/// `binary.is_file()`, and `CreateProcessW` completes a partial
/// `lpApplicationName` from the **calling** process's current directory —
/// Sterna's, which during a session is the project root and is writable by
/// invariant 3. So a program wrote `<project>\grep` and sterna executed it.
///
/// Four runs. The first is the control that makes the two refusals mean
/// anything — the copied image really is a program, and it really does run
/// when nothing refuses it — and the last is the falsifying half: an
/// installed binary still spawns, so the guard did not close the cage by
/// starting nothing.
#[test]
fn a_program_the_model_could_have_written_is_never_the_program_that_runs() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: CreateProcessW's cwd completion is this platform's");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        let fixture = Fixture::new("authored");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();

        // A real, runnable PE inside the project root — which is exactly what
        // a program that can write the project can put there.
        let authored = root.join("attack.exe");
        std::fs::copy(shell(), &authored).unwrap();

        // --- the control: it is a program, and it runs ------------------
        let proof = root.join("unconfined-ran");
        let ran = free_program(
            &authored,
            &[OsStr::new("/c"), OsStr::new("mkdir"), proof.as_ref()],
        );
        assert!(
            proof.is_dir(),
            "the fixture is not a runnable program, so the refusals below prove nothing: {ran:?}"
        );
        std::fs::remove_dir(&proof).unwrap();

        // --- the measured spelling: a bare relative name ----------------
        let landed = root.join("relative-ran");
        let refusal = caged_program(
            &profile,
            Path::new("attack.exe"),
            &[OsStr::new("/c"), OsStr::new("mkdir"), landed.as_ref()],
        )
        .expect_err("a relative program name must be refused, not resolved");
        assert!(
            refusal.contains("not an absolute path"),
            "the refusal must name the rule that decided it: {refusal}"
        );
        assert!(
            !landed.exists(),
            "sterna executed a file the model wrote: {refusal}"
        );

        // --- and the general shape, not only that spelling --------------
        //
        // Absolute, canonical, present, and still refused: the question the
        // guard asks is whether this profile would let the program *write*
        // the binary, which is the property that made the relative spelling
        // dangerous in the first place.
        let landed = root.join("absolute-ran");
        let refusal = caged_program(
            &profile,
            &authored,
            &[OsStr::new("/c"), OsStr::new("mkdir"), landed.as_ref()],
        )
        .expect_err("a binary inside the writable root must be refused");
        assert!(
            refusal.contains("sits where this profile grants write"),
            "the refusal must name the rule that decided it: {refusal}"
        );
        assert!(
            !landed.exists(),
            "sterna executed a file the model wrote: {refusal}"
        );

        // --- the falsifying half: an installed binary still runs --------
        //
        // Without this the two refusals above are satisfied by a cage that
        // starts nothing at all.
        let allowed = root.join("installed-ran");
        let ran = caged(
            &profile,
            &[OsStr::new("/c"), OsStr::new("mkdir"), allowed.as_ref()],
        )
        .expect("a system binary outside every writable grant must still spawn");
        assert!(
            allowed.is_dir(),
            "the cage refused an installed binary as well: {ran:?}"
        );
    }
}

/// **Escape 2.** The `.claude` carve-out survives the project root's
/// inheritable grant, including on a machine where that grant has already
/// been propagated into it.
///
/// The attack, as measured: `grant_project_acl` wrote the root's inheritable
/// read-write ACE first, Windows copied it into `.claude`, and the carve-out
/// then tried to take it back with `SetEntriesInAclW`'s `REVOKE_ACCESS` —
/// which merges *explicit* entries and copies an inherited ACE through
/// untouched. `PROTECTED_DACL_SECURITY_INFORMATION` then froze the surviving
/// grant in place. One process, one fixture:
/// `spawn 1: mkdir .claude\evil1 -> exit Some(0) | landed true`.
///
/// The second half of this test is the one that would still fail with the
/// ordering fix alone: `.claude` is destroyed and recreated between spawns,
/// so it inherits the root's grant exactly as the defect produced it, and the
/// next spawn has to strip an ACE that is already there and already
/// inherited.
#[test]
fn the_claude_carve_out_is_not_reachable_through_the_roots_inherited_grant() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: ACE inheritance is this platform's");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        let fixture = Fixture::new("inherit");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();
        let claude = root.join(".claude");
        let settings = claude.join("settings.json");
        std::fs::write(&settings, "{\"permissions\":{}}\n").unwrap();

        let mkdir = |path: &Path| -> Result<Ran, String> {
            caged(
                &profile,
                &[OsStr::new("/c"), OsStr::new("mkdir"), path.as_ref()],
            )
        };
        // Every spawn applies the ACL, so this is also the first application.
        let inside = root.join("ordinary");
        let ran = mkdir(&inside).expect("a write inside the project must spawn");
        assert!(
            inside.is_dir(),
            "the cage refused the project itself: {ran:?}"
        );

        // --- the control ------------------------------------------------
        let control = claude.join("unconfined-made-this");
        std::fs::create_dir(&control).unwrap();
        std::fs::remove_dir(&control).unwrap();

        // --- first spawn: the carve-out is a grant of exactly the reads --
        let aces = windows::container_aces(&profile, &claude).unwrap();
        assert_eq!(
            aces,
            vec![windows::Ace {
                allow: true,
                inherited: false,
                mask: windows::READ_RIGHTS,
            }],
            "the container must hold the read bits on .claude and nothing else"
        );
        let evil = claude.join("evil1");
        let ran = mkdir(&evil).expect("the spawn must succeed; the write must not");
        assert!(!evil.exists(), "a program wrote into .claude: {ran:?}");

        // --- and now the upgrade case, which is the fix being tested -----
        //
        // Removing and recreating the directory is how a real `.claude`
        // acquires the root's inheritable grant: a fresh child inherits, and
        // that inherited ACE is the one `REVOKE_ACCESS` could not remove.
        std::fs::remove_dir_all(&claude).unwrap();
        std::fs::create_dir(&claude).unwrap();
        let inherited = windows::container_aces(&profile, &claude).unwrap();
        assert!(
            inherited.iter().any(|ace| ace.allow
                && ace.inherited
                && ace.mask & windows::READ_WRITE_RIGHTS == windows::READ_WRITE_RIGHTS),
            "the fixture did not reproduce the inherited grant, so the case proves nothing: \
             {inherited:?}"
        );

        let evil = claude.join("evil2");
        let ran = mkdir(&evil).expect("the spawn must succeed; the write must not");
        let aces = windows::container_aces(&profile, &claude).unwrap();
        assert_eq!(
            aces,
            vec![windows::Ace {
                allow: true,
                inherited: false,
                mask: windows::READ_RIGHTS,
            }],
            "an inherited read-write ACE survived the carve-out: {aces:?}"
        );
        assert!(
            !evil.exists(),
            "a program wrote into .claude through the root's inherited grant: {ran:?}"
        );

        // The grant half, or the carve-out is just a broken directory: the
        // document a session compiles from stays readable.
        std::fs::write(&settings, "{\"permissions\":{}}\n").unwrap();
        let read = caged(
            &profile,
            &[OsStr::new("/c"), OsStr::new("type"), settings.as_ref()],
        )
        .expect("reading .claude must spawn");
        assert!(
            read.stdout.contains("permissions"),
            "the carve-out refused the read it exists to keep: {read:?}"
        );
    }
}

/// **Escape 3.** The carve-out cannot be moved out of the way — because
/// `FILE_DELETE_CHILD` is not in the project grant **and** `DELETE` is not in
/// the carve-out's.
///
/// The attack, as measured: `READ_WRITE_RIGHTS` was `0x001F_01FF` less one
/// bit, so the root's inheritable ACE carried `FILE_DELETE_CHILD` — on a
/// directory, the right to delete or rename a child **whose own DACL grants
/// nothing** — and escape 2's broken carve-out had left the same inheritable
/// ACE, `DELETE` and all, sitting inside `.claude` itself. A confined child
/// renamed `.claude` away, created a fresh one that inherited the root's
/// write grant, and wrote the settings document the next session compiles
/// from. Invariant 1 defeated by a `move`.
///
/// A rename needs `DELETE` on the object *or* `FILE_DELETE_CHILD` on its
/// parent, so those were two independently sufficient causes and closing one
/// would not have been enough;
/// `each_half_of_escape_threes_fix_is_independently_load_bearing` runs each
/// one and `sandbox-grants.md` §3 carries the table.
///
/// Every step of the sequence is run here, in order, against a real child.
#[test]
fn a_confined_child_cannot_move_the_carve_out_out_of_its_own_way() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: FILE_DELETE_CHILD is this platform's");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        let fixture = Fixture::new("delete-child");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();
        let claude = root.join(".claude");
        let settings = claude.join("settings.json");
        const ORIGINAL: &str = "{\"permissions\":{\"allow\":[]}}\n";
        std::fs::write(&settings, ORIGINAL).unwrap();

        // The first spawn applies the ACL. What follows it is the falsifying
        // half, and it is not decoration: `FILE_DELETE_CHILD` is a *write*
        // right, and a mask that stopped the attack by stopping every write
        // would pass the attack assertions below and be useless.
        let warm = caged(
            &profile,
            &[OsStr::new("/c"), OsStr::new("echo"), OsStr::new("x")],
        )
        .expect("a no-op must spawn");
        eprintln!("warm {warm:?}");
        eprintln!("root acl {:?}", windows::container_aces(&profile, &root));

        let run = |arguments: &[&OsStr]| -> Ran {
            caged(&profile, arguments).expect("the probe must spawn")
        };
        let file = root.join("ordinary.txt");
        let renamed = root.join("renamed.txt");
        let dir = root.join("made");
        let moved_dir = root.join("made-elsewhere");
        std::fs::write(&file, "gone soon\n").unwrap();

        // A directory the cage creates, renames and then removes. The
        // removal is the one that matters: it is a delete, and it goes
        // through `DELETE` **on the object**, which is what makes dropping
        // `FILE_DELETE_CHILD` from the parent cost nothing.
        let made = run(&[OsStr::new("/c"), OsStr::new("mkdir"), dir.as_ref()]);
        assert!(dir.is_dir(), "the cage cannot create a directory: {made:?}");
        let moved = run(&[
            OsStr::new("/c"),
            OsStr::new("move"),
            dir.as_ref(),
            moved_dir.as_ref(),
        ]);
        assert!(
            moved_dir.is_dir(),
            "the cage cannot rename a directory it made: {moved:?}"
        );
        let gone = run(&[OsStr::new("/c"), OsStr::new("rmdir"), moved_dir.as_ref()]);
        assert!(
            !moved_dir.exists(),
            "dropping FILE_DELETE_CHILD also stopped an ordinary rmdir, which it must not: \
             {gone:?}"
        );

        // And a file: renamed, then truncated.
        let moved = run(&[
            OsStr::new("/c"),
            OsStr::new("move"),
            file.as_ref(),
            renamed.as_ref(),
        ]);
        assert!(
            renamed.is_file() && !file.exists(),
            "dropping FILE_DELETE_CHILD also stopped an ordinary rename, which it must not: \
             {moved:?}"
        );
        std::fs::rename(&renamed, &file).unwrap();
        // **The truncation is asserted through a redirect, and that is a
        // measurement rather than a preference.** `copy /y nul <file>` is
        // what stood here, and on the GitHub `windows-latest` runner it
        // answered `exit 1`, *"0 file(s) copied."*, *"Access is denied."*,
        // leaving the file at its original 10 bytes — while in the same
        // container, on the same run, `mkdir`, `move`, `rmdir` and a file
        // rename all succeeded, and the root's ACE read `0x0013019F` exactly
        // as it does here. The one thing `copy` does that none of those do
        // is open the `NUL` **device**, whose name resolves through a DOS
        // device map an AppContainer in a service logon need not have. So
        // the contract — a confined child can truncate an ordinary project
        // file — is asserted through `break`, a cmd builtin that writes
        // nothing, and cmd's own `>` redirection: the same claim about the
        // same file, reached without a device.
        //
        // The `copy` probe stays, printed. It is the evidence that says
        // whether the runner's refusal was ever about the project ACL, and
        // dropping it would throw that away to make a red go quiet.
        eprintln!("file acl {}", icacls(&[file.as_ref()]));
        eprintln!(
            "list the project inside the cage {:?}",
            run(&[
                OsStr::new("/c"),
                OsStr::new("dir"),
                OsStr::new("/b"),
                root.as_ref()
            ])
        );
        eprintln!(
            "read the NUL device inside the cage {:?}",
            run(&[OsStr::new("/c"), OsStr::new("type"), OsStr::new("nul")])
        );
        let created = root.join("created-by-redirect.txt");
        eprintln!(
            "create by redirect {:?} exists={}",
            run(&[
                OsStr::new("/c"),
                OsStr::new("echo"),
                OsStr::new("x>"),
                created.as_ref(),
            ]),
            created.exists()
        );
        eprintln!(
            "copy /y nul {:?} len={:?}",
            run(&[
                OsStr::new("/c"),
                OsStr::new("copy"),
                OsStr::new("/y"),
                OsStr::new("nul"),
                file.as_ref(),
            ]),
            std::fs::metadata(&file).map(|meta| meta.len())
        );

        // And the assertion: whatever the probe above did, the file starts
        // at its ten bytes and the cage has to take them away.
        std::fs::write(&file, "gone soon\n").unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().len(), 10);
        let truncated = run(&[OsStr::new("/c"), OsStr::new("break>"), file.as_ref()]);
        assert_eq!(
            std::fs::metadata(&file).unwrap().len(),
            0,
            "the cage cannot write a project file: {truncated:?}"
        );

        // package's doing.** Measured on the Windows ARM64 VM, 2026-09-09,
        // with `FILE_DELETE_CHILD` *restored* into the mask and the file
        // carrying an inherited allow of `0x0013_01DF` — which contains
        // `DELETE` — in both the verbatim and the plain spelling of the path:
        //
        //     del: exit Some(1) stderr "Access is denied." gone=false
        //     del /f /q: exit Some(1) stderr "Access is denied." gone=false
        //
        // while the unconfined control removed the same file, and `move`,
        // `rmdir`, `mkdir` and `copy` all succeeded inside the cage. So the
        // refusal is a property of `del` under an AppContainer rather than of
        // this mask, it predates this change, and `sandbox-grants.md` §3
        // records it. It is printed rather than asserted: freezing an
        // outcome nobody has explained would make the next person's
        // measurement look like a regression.
        eprintln!(
            "del inside the cage: {:?} gone={}",
            run(&[OsStr::new("/c"), OsStr::new("del"), file.as_ref()]),
            !file.exists()
        );

        // --- the attack, step by step -----------------------------------
        let moved = root.join("claude-moved-aside");
        let renamed = caged(
            &profile,
            &[
                OsStr::new("/c"),
                OsStr::new("move"),
                claude.as_ref(),
                moved.as_ref(),
            ],
        )
        .expect("the spawn must succeed; the rename must not");
        assert!(
            !moved.exists() && claude.is_dir(),
            "a confined child renamed the carve-out away: {renamed:?}"
        );

        let removed = caged(
            &profile,
            &[
                OsStr::new("/c"),
                OsStr::new("rmdir"),
                OsStr::new("/s"),
                OsStr::new("/q"),
                claude.as_ref(),
            ],
        )
        .expect("the spawn must succeed; the removal must not");
        assert!(
            claude.is_dir(),
            "a confined child deleted the carve-out: {removed:?}"
        );

        // And the document it was all for is untouched.
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            ORIGINAL,
            "the settings document a session compiles from was rewritten"
        );

        // --- the control: the operation itself is possible here ---------
        //
        // Without it, "the rename did not happen" could mean `move` does not
        // work on this machine rather than that the cage stopped it.
        let control = free(&[
            OsStr::new("/c"),
            OsStr::new("move"),
            claude.as_ref(),
            moved.as_ref(),
        ]);
        assert!(
            moved.is_dir(),
            "the unconfined control could not rename it either, so the case proves nothing: \
             {control:?}"
        );
        std::fs::rename(&moved, &claude).unwrap();
    }
}

// --- the two halves of escape 3's fix, and the repair that keeps them ----

/// This container's SID in string form, read back from the object sterna just
/// wrote.
///
/// The tests below have to name the container as a *trustee* — `icacls` is
/// the only tool here that can put an ACE on a path sterna does not manage —
/// and nothing public hands out the SID text. The project root's DACL is
/// where it is, and after a spawn there is exactly one AppContainer SID in
/// it: `S-1-15-2-` followed by seven sub-authorities. Asserting that count is
/// what stops this quietly reading somebody else's entry.
#[cfg(target_os = "windows")]
fn container_sid_text(root: &Path) -> String {
    let output = std::process::Command::new("icacls")
        .arg(root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut found: Vec<String> = Vec::new();
    for token in text.split(|c: char| c.is_whitespace() || c == ':') {
        if token.starts_with("S-1-15-2-") && token.matches('-').count() == 10 {
            let sid = token.to_string();
            if !found.contains(&sid) {
                found.push(sid);
            }
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one AppContainer SID on the project root, got {found:?} from:\n{text}"
    );
    found.pop().unwrap()
}

#[cfg(target_os = "windows")]
fn icacls(arguments: &[&std::ffi::OsStr]) -> String {
    let output = std::process::Command::new("icacls")
        .args(arguments)
        .output()
        .unwrap();
    let report = format!(
        "icacls {arguments:?} -> exit {:?}\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    assert!(output.status.success(), "{report}");
    report
}

/// **Escape 3 had two independently sufficient causes, and this runs each
/// one.** Neither withholding would have closed it alone.
///
/// `sandbox-grants.md` §3 used to credit the withheld `FILE_DELETE_CHILD`
/// alone. Measured on the Windows ARM64 VM, 2026-09-09, against the shipping
/// build with one bit handed back at a time:
///
/// | root's grant | `.claude`'s grant | `move .claude` |
/// |---|---|---|
/// | `0x0013_019F` | `0x0012_0089` | *"Access is denied."*, 0 dir(s) moved |
/// | `0x0013_01DF` (`+FILE_DELETE_CHILD`) | `0x0012_0089` | 1 dir(s) moved |
/// | `0x0013_019F` | `0x0013_0089` (`+DELETE`) | 1 dir(s) moved |
/// | `0x001F_01FF` (the whole pre-patch mask) | `0x0012_0089` | 1 dir(s) moved |
///
/// The first two rows are `a_confined_child_cannot_move_the_carve_out_out_of_its_own_way`
/// and the mask constants. This test runs the *other* two: it hands the
/// missing right to `ALL APPLICATION PACKAGES` — a group every AppContainer
/// belongs to, and the only trustee whose ACE survives a spawn, because
/// [`windows::grant_project_acl`] rewrites every ACE naming the container
/// itself — and watches the refusal turn into a move.
///
/// So this fails if a future build stops withholding either bit, and it says
/// which one by which leg moved the directory.
#[test]
fn each_half_of_escape_threes_fix_is_independently_load_bearing() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: an AppContainer access check is this platform's");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        // --- the object half: `DELETE` on `.claude` itself ---------------
        let fixture = Fixture::new("half-object");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();
        let claude = root.join(".claude");
        std::fs::write(claude.join("settings.json"), "{}\n").unwrap();
        let warm = caged(
            &profile,
            &[OsStr::new("/c"), OsStr::new("echo"), OsStr::new("x")],
        )
        .expect("a no-op must spawn");
        assert!(warm.ok(), "the cage refused a no-op: {warm:?}");
        eprintln!(
            "{}",
            icacls(&[
                claude.as_ref(),
                OsStr::new("/grant"),
                OsStr::new("*S-1-15-2-1:(D)")
            ])
        );
        let aside = root.join("aside-object");
        let moved = caged(
            &profile,
            &[
                OsStr::new("/c"),
                OsStr::new("move"),
                claude.as_ref(),
                aside.as_ref(),
            ],
        )
        .expect("the probe must spawn");
        assert!(
            aside.is_dir() && !claude.exists(),
            "handing the container DELETE on the carve-out did NOT re-open escape 3, so the \
             withheld DELETE is not what closes it and §3 is wrong again: {moved:?}"
        );
        std::fs::rename(&aside, &claude).unwrap();

        // --- the parent half: `FILE_DELETE_CHILD` on the root ------------
        let fixture = Fixture::new("half-parent");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();
        let claude = root.join(".claude");
        std::fs::write(claude.join("settings.json"), "{}\n").unwrap();
        let warm = caged(
            &profile,
            &[OsStr::new("/c"), OsStr::new("echo"), OsStr::new("x")],
        )
        .expect("a no-op must spawn");
        assert!(warm.ok(), "the cage refused a no-op: {warm:?}");
        eprintln!(
            "{}",
            icacls(&[
                root.as_ref(),
                OsStr::new("/grant"),
                OsStr::new("*S-1-15-2-1:(DC)")
            ])
        );
        let aside = root.join("aside-parent");
        let moved = caged(
            &profile,
            &[
                OsStr::new("/c"),
                OsStr::new("move"),
                claude.as_ref(),
                aside.as_ref(),
            ],
        )
        .expect("the probe must spawn");
        assert!(
            aside.is_dir() && !claude.exists(),
            "handing the container FILE_DELETE_CHILD on the project root did NOT re-open \
             escape 3, so the withheld bit is not what closes it and §3 is wrong: {moved:?}"
        );
    }
}

/// What the repair pass takes back, and what it provably cannot — because
/// `sandbox-grants.md` §7 may promise only the first.
///
/// **Taken back:** a widened ACE on a path sterna grants, naming the container
/// itself. Every spawn rewrites those with `SET_ACCESS`, which is what stops
/// a project last caged by a build whose mask held `WRITE_DAC` from keeping
/// it for ever.
///
/// **Not taken back:** an *explicit* ACE on a descendant. Inheritance is a
/// copy performed when the parent's ACE is written, and the propagation
/// recomputes only the inherited part of a child's DACL, so an explicit entry
/// survives every walk. sterna reads the masks of the paths it grants and no
/// others, so it never sees one either. The measured cost of looking is in
/// the same file's `grant_project_acl`: propagating over a 10,000-file
/// project took 1.01s against a 47ms skip, and a read-only walk of the same
/// tree on every tool call is the same shape of stall.
///
/// The other limit is in `each_half_of_escape_threes_fix_is_independently_load_bearing`:
/// an ACE naming `ALL APPLICATION PACKAGES` widens the cage exactly as well
/// and names a trustee sterna neither writes nor reads.
#[test]
fn what_the_repair_pass_can_and_cannot_take_back() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: ACLs are this platform's");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        let fixture = Fixture::new("repair");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();
        let claude = root.join(".claude");
        std::fs::write(claude.join("settings.json"), "{}\n").unwrap();
        let warm = caged(
            &profile,
            &[OsStr::new("/c"), OsStr::new("echo"), OsStr::new("x")],
        )
        .expect("a no-op must spawn");
        assert!(warm.ok(), "the cage refused a no-op: {warm:?}");

        // Hand the container itself full control of the project root -- wider
        // than the pre-patch mask that produced escape 3, not narrower.
        let sid = container_sid_text(&root);
        eprintln!(
            "{}",
            icacls(&[
                root.as_ref(),
                OsStr::new("/grant"),
                OsStr::new(&format!("*{sid}:(OI)(CI)(F)")),
            ])
        );
        let (widened, _) = windows::container_masks(&profile, &root).unwrap();
        assert!(
            widened & windows::WITHHELD_RIGHTS != 0,
            "the fixture did not widen anything, so the case proves nothing: {widened:x}"
        );

        let aside = root.join("aside");
        let moved = caged(
            &profile,
            &[
                OsStr::new("/c"),
                OsStr::new("move"),
                claude.as_ref(),
                aside.as_ref(),
            ],
        )
        .expect("the probe must spawn");
        let (repaired, deny) = windows::container_masks(&profile, &root).unwrap();
        assert_eq!(
            (repaired, deny),
            (windows::READ_WRITE_RIGHTS, 0),
            "a hand-widened grant naming the container survived the next spawn"
        );
        assert!(
            !aside.exists() && claude.is_dir(),
            "the child moved the carve-out before the repair could run: {moved:?}"
        );

        // --- and what it cannot take back --------------------------------
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        eprintln!("{}", icacls(&[sub.as_ref(), OsStr::new("/inheritance:d")]));
        eprintln!(
            "{}",
            icacls(&[
                sub.as_ref(),
                OsStr::new("/grant"),
                OsStr::new(&format!("*{sid}:(OI)(CI)(F)")),
            ])
        );
        eprintln!("{}", icacls(&[sub.as_ref(), OsStr::new("/inheritance:e")]));
        let (before, _) = windows::container_masks(&profile, &sub).unwrap();
        assert!(
            before & windows::WITHHELD_RIGHTS == windows::WITHHELD_RIGHTS,
            "the fixture did not widen the descendant, so the case proves nothing: {before:x}"
        );

        // Take the witness away so the next spawn does the whole walk, and
        // the walk is still not enough.
        std::fs::remove_dir_all(&claude).unwrap();
        std::fs::create_dir(&claude).unwrap();
        let ran = caged(
            &profile,
            &[OsStr::new("/c"), OsStr::new("echo"), OsStr::new("x")],
        )
        .expect("a no-op must spawn");
        assert!(ran.ok(), "the cage refused a no-op: {ran:?}");
        let (after, _) = windows::container_masks(&profile, &sub).unwrap();
        assert_eq!(
            after, before,
            "the explicit descendant ACE changed under a full re-propagation, so the limit \
             this test records is not the limit any more and §7 can promise more"
        );
        assert!(
            after & windows::WITHHELD_RIGHTS == windows::WITHHELD_RIGHTS,
            "an explicit wider grant on a descendant is still not repaired, and §7 says so: \
             {after:x}"
        );
    }
}

/// **An interrupted propagation is detected and repeated**, because the
/// carve-out's own grant is written last and is therefore proof the walk
/// before it finished.
///
/// The defect: `SetNamedSecurityInfoW` sets the named object's DACL and
/// *then* walks the tree, so a call that dies part way leaves the project
/// root carrying the final mask over descendants that never got it. Reading
/// the root's mask cannot tell that apart from a finished propagation, and
/// the old per-path skip made the half-done state permanent — half a project
/// unreachable to the container until somebody deleted the ACE by hand.
///
/// **The walk is asserted by what it costs, and that is not laziness.**
/// Windows keeps an unprotected child's inherited ACEs in step with its
/// parent's at every opportunity — `icacls /inheritance:e` re-inherits on the
/// spot, and a directory renamed in from outside stops auto-inheriting
/// altogether — so a descendant that is stale *and* still auto-inheriting
/// cannot be fabricated from outside the process; only a real interruption
/// makes one. What can be observed is whether the walk happened at all.
/// Measured on the Windows ARM64 VM, 2026-09-09, over this test's own
/// 5,000-file project: first spawn 489ms, skipping spawn 50ms, spawn with the
/// witness removed 384ms, next spawn 51ms. The assertions below want a factor
/// of three and the measurement gives between seven and eight.
///
/// The second leg is the limit, run rather than written: while the witness is
/// in place nothing is re-walked, which is the whole reason a spawn costs
/// 47ms instead of 700.
#[test]
fn an_interrupted_propagation_is_detected_and_repeated() {
    #[cfg(not(target_os = "windows"))]
    eprintln!("skipped: ACE propagation is this platform's");
    #[cfg(target_os = "windows")]
    {
        use std::ffi::OsStr;

        let fixture = Fixture::new("witness");
        let profile = fixture.profile();
        let root = profile.root().to_path_buf();
        let claude = root.join(".claude");
        std::fs::write(claude.join("settings.json"), "{}\n").unwrap();
        // Big enough that a walk is unmistakable beside a skip, small enough
        // that building it is under a second.
        for directory in 0..100u32 {
            let directory = root.join(format!("d{directory:03}"));
            std::fs::create_dir_all(&directory).unwrap();
            for file in 0..50u32 {
                std::fs::write(directory.join(format!("f{file:03}.txt")), b"x").unwrap();
            }
        }

        // Every leg runs a real child, so a spawn that skipped the ACL for
        // the wrong reason still has to produce a working cage.
        let spawn = |label: &str| -> std::time::Duration {
            let at = std::time::Instant::now();
            let ran = caged(
                &profile,
                &[OsStr::new("/c"), OsStr::new("echo"), OsStr::new("x")],
            )
            .expect("the probe must spawn");
            let took = at.elapsed();
            assert!(ran.ok(), "the cage refused a no-op at {label}: {ran:?}");
            eprintln!("{label}: {took:?}");
            took
        };

        spawn("first, nothing applied yet");
        let leaf = root.join("d050").join("f025.txt");
        assert_eq!(
            windows::container_masks(&profile, &leaf).unwrap(),
            (windows::READ_WRITE_RIGHTS, 0),
            "the first spawn did not propagate the grant into the tree at all"
        );

        // --- the limit: everything applied, so nothing is re-walked ------
        let skipping = spawn("second, everything applied");

        // --- the witness goes missing, which is what an interrupted call
        //     leaves behind: the carve-out never got its read grant, while
        //     the root already carries the final mask.
        std::fs::remove_dir_all(&claude).unwrap();
        std::fs::create_dir(&claude).unwrap();
        assert_ne!(
            windows::container_masks(&profile, &claude).unwrap(),
            (windows::READ_RIGHTS, 0),
            "the fixture did not remove the witness, so the case proves nothing"
        );
        assert_eq!(
            windows::container_masks(&profile, &root).unwrap(),
            (windows::READ_WRITE_RIGHTS, 0),
            "the root's own mask has to be already correct, or this measures the ordinary \
             first-application path instead of the interrupted one"
        );

        let repeating = spawn("third, witness gone and the root already right");
        assert!(
            repeating > skipping * 3,
            "the missing witness did not buy a re-propagation: {repeating:?} against a \
             {skipping:?} skip, and the whole tree should have been walked again"
        );
        assert_eq!(
            windows::container_masks(&profile, &claude).unwrap(),
            (windows::READ_RIGHTS, 0),
            "the carve-out did not come back to exactly its read grant, so the next spawn \
             would walk the tree for ever"
        );

        // --- and the witness is back, so the cost goes back with it ------
        let skipping_again = spawn("fourth, everything applied again");
        assert!(
            skipping_again * 3 < repeating,
            "the witness was restored but the next spawn still walked the tree: \
             {skipping_again:?} against {repeating:?}"
        );
    }
}
