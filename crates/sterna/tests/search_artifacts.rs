use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use sterna::contract::SessionId;
use sterna::sandbox::profile::Profile;
use sterna::tools::invoke::{self, Args, ToolContext};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_prefix("sterna-search-artifacts")
    }

    fn with_prefix(prefix: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for (relative, contents) in [
            ("src/lib.rs", "needle source\n"),
            (".sterna/rollout.jsonl", "needle model feedback\n"),
            (
                ".sterna/sessions/tlx7uc-2sb.jsonl",
                "needle session transcript\n",
            ),
            (
                ".sterna/sessions/tlx7uc-2sb.gateway.log",
                "needle gateway log\n",
            ),
            (".sterna/config.toml", "needle sterna config\n"),
            (".env", "needle environment\n"),
            (".settings/local.txt", "needle hidden config\n"),
            (".git/config", "needle git internals\n"),
            ("literal[1].txt", "-rf literal pattern\n"),
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        Self {
            root: std::fs::canonicalize(root).unwrap(),
        }
    }

    fn call(&self, tool: &str, args: Args) -> String {
        let profile = Profile::compile(&self.root, None);
        let session = SessionId::new("search-artifact-test");
        invoke::run(
            &ToolContext {
                profile: &profile,
                session: &session,
            },
            tool,
            &args,
        )
        .unwrap()
        .stdout
    }

    fn glob(&self, pattern: &str, path: Option<&Path>) -> String {
        let mut args = Args::new().with("pattern", pattern);
        if let Some(path) = path {
            args = args.with("path", path.to_string_lossy());
        }
        self.call("glob", args)
    }

    fn grep(&self, pattern: &str, path: Option<&Path>) -> String {
        let mut args = Args::new().with("pattern", pattern);
        if let Some(path) = path {
            args = args.with("path", path.to_string_lossy());
        }
        self.call("grep", args)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `relative` beneath `root` in the host's own separators — the spelling a
/// walker prints. `Path::join("src/lib.rs")` is not it on Windows: the `/`
/// is kept verbatim there, and no tool output ever contains that mix.
fn native(root: &Path, relative: &str) -> String {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
        .display()
        .to_string()
}

#[test]
fn broad_search_omits_generated_feedback_but_keeps_normal_hidden_files() {
    let fixture = Fixture::new();
    let glob = fixture.glob("**/*", None);
    for retained in [
        "src/lib.rs",
        ".sterna/config.toml",
        ".env",
        ".settings/local.txt",
    ] {
        assert!(
            glob.contains(&native(&fixture.root, retained)),
            "{retained}: {glob}"
        );
    }
    assert!(!glob.contains("rollout.jsonl"), "{glob}");
    assert!(!glob.contains(".git"), "{glob}");
    // Sterna's own session transcripts and logs are generated state, not the
    // project: reported from a fresh project whose first glob was all logs.
    assert!(!glob.contains("sessions"), "{glob}");

    let grep = fixture.grep("needle", None);
    for retained in [
        "src/lib.rs",
        ".sterna/config.toml",
        ".env",
        ".settings/local.txt",
    ] {
        assert!(
            grep.contains(&native(&fixture.root, retained)),
            "{retained}: {grep}"
        );
    }
    assert!(!grep.contains("model feedback"), "{grep}");
    assert!(!grep.contains("git internals"), "{grep}");
}

#[test]
fn a_large_self_matching_rollout_cannot_starve_the_real_source_match() {
    let fixture = Fixture::new();
    let rollout = fixture.root.join(".sterna/rollout.jsonl");
    std::fs::write(&rollout, "needle repeated model feedback\n".repeat(4_000)).unwrap();

    let grep = fixture.grep("needle", None);
    assert!(
        grep.contains(&native(&fixture.root, "src/lib.rs")),
        "real source match was lost: {grep}"
    );
    assert!(grep.contains("needle source"), "{grep}");
    assert!(!grep.contains("rollout.jsonl"), "{grep}");
    assert!(!grep.contains("model feedback"), "{grep}");
}

#[test]
fn a_colon_in_the_project_path_cannot_hide_the_rollout_prefix() {
    // A colon inside a path *component* is a unix-only spelling: Windows
    // reserves `:` for the drive separator and refuses the name outright
    // (`ERROR_INVALID_NAME`). The colon this test needs is already in
    // every absolute Windows path, so the default fixture supplies it.
    #[cfg(unix)]
    let fixture = Fixture::with_prefix("sterna:colon-search-artifacts");
    #[cfg(not(unix))]
    let fixture = Fixture::new();
    let grep = fixture.grep("needle", None);
    assert!(
        grep.contains(&native(&fixture.root, "src/lib.rs")),
        "{grep}"
    );
    assert!(!grep.contains("rollout.jsonl"), "{grep}");
    assert!(!grep.contains("model feedback"), "{grep}");
}

#[test]
fn explicit_hidden_targets_opt_back_in_without_changing_read_authority() {
    let fixture = Fixture::new();
    let sterna = fixture.glob(".sterna/*", None);
    assert!(sterna.contains("rollout.jsonl"), "{sterna}");
    assert!(sterna.contains("config.toml"), "{sterna}");
    assert!(
        !sterna.contains(".git"),
        "a .sterna opt-in also exposed Git: {sterna}"
    );
    let git = fixture.glob(".git/*", None);
    assert!(git.contains(&native(&fixture.root, ".git/config")), "{git}");

    let rollout = fixture.root.join(".sterna/rollout.jsonl");
    let explicit = fixture.grep("model feedback", Some(&rollout));
    assert!(explicit.contains("model feedback"), "{explicit}");

    let read = fixture.call("read", Args::new().with("path", rollout.to_string_lossy()));
    assert_eq!(read, "needle model feedback\n");
}

#[test]
fn literal_glob_and_option_shaped_grep_patterns_remain_data() {
    let fixture = Fixture::new();
    let literal = fixture.glob("literal[1].txt", None);
    assert!(literal.contains("literal[1].txt"), "{literal}");
    let option = fixture.grep("-rf", None);
    assert!(option.contains("literal[1].txt"), "{option}");
}

/// **`grep` and `rg` must read a pattern the same way.** The declaration
/// offers `grep` as "a regular expression", and a model writes one --
/// alternation, `+`, a group. `grep` without `-E` is BRE, where every one of
/// those is a literal character, so the call answers a different question
/// than the one asked and answers it with silence. Measured on 2026-09-20: a
/// real session spent 128 s and then 132 s searching a 9 GB tree for a
/// sixty-character literal with five pipes in it.
#[test]
fn grep_reads_alternation_as_alternation_and_not_as_a_literal_pipe() {
    let fixture = Fixture::new();
    let matched = fixture.grep("needle source|nothing at all", None);
    assert!(
        matched.contains(&native(&fixture.root, "src/lib.rs")),
        "alternation was searched for as a literal string: {matched:?}"
    );
}

/// The other extended forms a model reaches for, in one call each, because
/// `-E` is one flag and its absence is invisible in every one of them.
#[test]
fn grep_reads_the_extended_forms_a_model_writes() {
    let fixture = Fixture::new();
    for pattern in ["need+le", "(needle) source", "needle sourc?e"] {
        let matched = fixture.grep(pattern, None);
        assert!(
            matched.contains(&native(&fixture.root, "src/lib.rs")),
            "`{pattern}` was read as a literal: {matched:?}"
        );
    }
}

/// **A broad `grep` does not read what the project says it generates.**
/// `grep` has no notion of an ignore file, so a checkout holding a model
/// download and two virtual environments was read whole: 9.2 GB, measured at
/// 143 seconds, twice in one session, while `rg` beside it in the same
/// roster had skipped those directories all along.
///
/// The names come from the project's own `.gitignore` and from nowhere else,
/// including one a level down -- which is where the tree that mattered
/// declared itself.
///
/// **What "skipped" means is the guarantee of the backend that serves the
/// call, and this test names which one that was.** Asserting ripgrep's
/// guarantee against whatever happens to be installed is how this test came
/// to pass on a developer machine and fail on every CI runner: the runners
/// have no ripgrep, the call falls back to POSIX `grep --exclude-dir`, and a
/// rule in `sub/.gitignore` cannot be expressed as a basename-wide exclusion
/// without changing its meaning. The fallback therefore finds *more* than
/// ripgrep does, never less -- so what it owes is the rule that does
/// transfer, and every real source file still found.
#[test]
fn a_broad_grep_skips_what_the_project_says_it_generates() {
    let fixture = Fixture::with_prefix("sterna-search-ignored");
    for (relative, contents) in [
        (".gitignore", "generated/\n"),
        ("generated/big.txt", "needle generated\n"),
        ("sub/.gitignore", "models/\n.venv-vlm/\n*.so\nnotes.txt\n"),
        ("sub/models/weights.txt", "needle weights\n"),
        ("sub/.venv-vlm/pkg.py", "needle vendored\n"),
        ("sub/real.py", "needle real source\n"),
        ("sub/notes.txt", "needle notes\n"),
    ] {
        let path = fixture.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    let backend = Backend::serving();
    let matched = fixture.grep("needle", None);
    // Whichever backend served it, a search that loses real source is wrong.
    for kept in ["sub/real.py", "src/lib.rs"] {
        assert!(
            matched.contains(&native(&fixture.root, kept)),
            "{backend:?}: {kept} was lost: {matched}"
        );
    }
    // The search root's own `generated/` transfers to every backend.
    assert!(
        !matched.contains("big.txt"),
        "{backend:?}: big.txt was read although the project's own \
         .gitignore calls it generated: {matched}"
    );
    // A rule from `sub/.gitignore` binds under `sub/`, which only ripgrep
    // can express.
    for nested in ["weights.txt", "pkg.py"] {
        match backend {
            Backend::Ripgrep => assert!(
                !matched.contains(nested),
                "ripgrep: {nested} was read although sub/.gitignore calls it \
                 generated: {matched}"
            ),
            Backend::PosixGrep | Backend::InProcess => assert!(
                matched.contains(nested),
                "{backend:?}: a nested rule cannot be expressed as a \
                 basename-wide exclusion, so {nested} is expected to be read; \
                 if it is now skipped this test is what says so: {matched}"
            ),
        }
    }
}

/// Which backend `grep` actually runs on here, resolved the way `invoke`
/// resolves it: ripgrep where ripgrep is on `PATH`, otherwise Sterna's own
/// in-process walk on Windows -- where the registry declares `grep`
/// in-process because an AppContainer cannot start an MSYS2 image -- and
/// otherwise the POSIX `grep` the registry spawns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Ripgrep,
    InProcess,
    PosixGrep,
}

impl Backend {
    fn serving() -> Self {
        if which_ripgrep() {
            Backend::Ripgrep
        } else if cfg!(windows) {
            Backend::InProcess
        } else {
            Backend::PosixGrep
        }
    }
}

/// The rules are read as git reads them, not as an approximation of git.
///
/// Where ripgrep serves the call this is free and exact -- a rule from
/// `sub/.gitignore` binds under `sub/` and a file-shaped rule excludes that
/// file. Where ripgrep is absent, `--exclude-dir` can express neither, and
/// `ignored_directories` takes only the rules that transfer without changing
/// meaning: so the fallback finds *more* than this, never less, and a search
/// that is merely slower is not a search that is wrong.
#[test]
fn an_ignored_file_is_skipped_where_git_says_it_is() {
    let fixture = Fixture::with_prefix("sterna-search-file-rule");
    for (relative, contents) in [
        ("sub/.gitignore", "notes.txt\n"),
        ("sub/notes.txt", "needle notes\n"),
        ("sub/real.py", "needle real source\n"),
    ] {
        let path = fixture.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let matched = fixture.grep("needle", None);
    assert!(
        matched.contains(&native(&fixture.root, "sub/real.py")),
        "real source was lost: {matched}"
    );
    if which_ripgrep() {
        assert!(
            !matched.contains("notes.txt"),
            "a file git ignores was read anyway: {matched}"
        );
    }
}

fn which_ripgrep() -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(if cfg!(windows) { "rg.exe" } else { "rg" });
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

/// A search deliberately aimed inside a generated tree still reads it: the
/// opt-in `.git` and `.sterna` already have.
#[test]
fn naming_a_generated_directory_searches_it() {
    let fixture = Fixture::with_prefix("sterna-search-opt-in");
    for (relative, contents) in [
        (".gitignore", "models/\n"),
        ("models/weights.txt", "needle weights\n"),
    ] {
        let path = fixture.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    let inside = fixture.root.join("models");
    let matched = fixture.grep("needle", Some(&inside));
    assert!(
        matched.contains("weights.txt"),
        "a directly named directory was skipped anyway: {matched}"
    );
}
