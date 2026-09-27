//! One immutable profile compiled from `.claude/settings.json`'s
//! `permissions`, and the pre-call check that answers every path question
//! from it — map line 2455, specification
//! `docs/sandbox.md`.
//!
//! The invariant the whole module exists for: **a profile is built once and
//! can never be widened after session startup.** That is enforced by the type
//! rather than by a comment — [`Profile`] has no public field or shared-
//! mutable interior. Its consuming startup builders require ownership before
//! the profile is shared with a runtime. It also holds no
//! handle to the document it was compiled from, which is why re-reading
//! `.claude/settings.json` mid-session — the widening path §1.5 names, since
//! `.claude/` lives inside the writable project root — is not something a
//! caller can accidentally do.
//!
//! Two questions, answered in that order and never conflated (§2):
//! [`Profile::admits_command`] asks whether a command line may be attempted,
//! and [`Profile::check`] asks what any process may touch. A `Bash` pattern
//! answers the first and contributes nothing to the second.

use super::modes::{ModeOverlay, Narrowing, RequestMode};
use crate::contract::ProjectConfig;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// Which half of a file grant a call needs. An editing tool asks both, in
/// whichever order it performs them; there is no combined variant, because a
/// single answer would hide which half was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
        }
    }

    /// The refusal sentence for a path no grant covers, §5.
    ///
    /// In container mode a read outside every root is granted before this
    /// sentence is reached, so a container-mode read refusal always carries
    /// the never-grantable rule or the `deny` entry that decided it; the
    /// container-mode write sentence names the asymmetry instead of claiming
    /// a root is the only readable one.
    fn only_root_sentence(self, container_mode: bool) -> &'static str {
        match (self, container_mode) {
            (Access::Read, _) => {
                "no grant covers this path; the project root is the only readable root"
            }
            (Access::Write, false) => {
                "no grant covers this path; the project root is the only writable root"
            }
            (Access::Write, true) => {
                "no grant covers this path; container mode widens reads only, and the project root and the additional roots are the only writable roots"
            }
        }
    }
}

/// A refusal, as a value.
///
/// It is returned, never raised as a question: nothing in this module reads
/// from a terminal and nothing here widens anything (§1.4). `rule`
/// names the *deciding* rule — the `deny` entry that matched, the
/// never-grantable entry that matched, or the absence of any `allow` — so a
/// person reading a transcript can fix the settings file without
/// re-deriving the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDenied {
    pub tool: String,
    /// The path asked for, as it was resolved. For an argv refusal this is
    /// the command line, because that is what was refused.
    pub path: String,
    pub rule: String,
}

impl fmt::Display for PermissionDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PermissionDenied: {}(\"{}\")\n  rule: {}\n  tool: {}",
            self.tool, self.path, self.rule, self.tool
        )
    }
}

impl std::error::Error for PermissionDenied {}

/// One filesystem rule, keeping the pattern as written so a refusal can quote
/// it back.
#[derive(Debug, Clone)]
struct PathRule {
    written: String,
    glob: Vec<String>,
    read: bool,
    write: bool,
}

/// One entry of §4's never-grantable set, expressed as a resolved subtree.
#[derive(Debug, Clone)]
struct NeverRule {
    /// The subtree this rule refuses, in [`spelling`] — the one form every
    /// comparison in this module is made in, never a `Path`, because
    /// `Path::starts_with` compares a `\\?\C:` prefix and a `C:` prefix as
    /// different things while they name one directory.
    prefix: Vec<String>,
    /// `prefix` followed by `**`, so a rule of §4's set reads the same way to
    /// [`Profile::rules`] as one the document wrote. It renders the prefix
    /// test exactly: `**` matches the empty tail, so the subtree's own root
    /// is covered.
    glob: Vec<String>,
    /// The project root, when this rule's subtree contains it; for `.sterna/**`,
    /// [`SCRATCH_DIR`]; nothing otherwise. A project checked out under
    /// `~/.config` must be able to read itself, and nothing else in
    /// `~/.config`. Dropping the rule instead handed the whole directory to
    /// any pattern that named it.
    ///
    /// Kept as a path because [`Rule::exempt_subtree`] hands it to a platform
    /// applier; `except_spelling` is the same subtree in the comparison form.
    except: Option<PathBuf>,
    except_spelling: Option<Vec<String>>,
    /// `true` for `.claude/**`, which is never writable but stays readable —
    /// `settings.json` is read before the sandbox is entered (§1.5).
    write_only: bool,
    rule: String,
}

/// The compiled profile.
///
/// Every field is private. Consuming builders apply explicit host choices
/// before session startup; after the profile is shared, there is no way to
/// widen it.
#[derive(Debug, Clone)]
pub struct Profile {
    root: PathBuf,
    /// Explicit session-start acknowledgement that Sterna's native child
    /// confinement is bypassed. Admission checks and credential stripping
    /// still run; only the OS sandbox layer is skipped.
    bypass_os_sandbox: bool,
    /// Explicit host-selected directories, fixed before session start.
    additional_roots: Vec<PathBuf>,
    /// Present when the supplied project root had no unambiguous absolute
    /// identity. Every admission method checks this before implicit root or
    /// configured grants.
    invalid_root: Option<String>,
    /// `root` in [`spelling`], compiled once. Every containment question this
    /// module asks about the project root is asked against this and never
    /// against `root` itself, for the reason [`NeverRule::prefix`] gives.
    root_spelling: Vec<String>,
    home: Option<PathBuf>,
    allow: Vec<PathRule>,
    deny: Vec<PathRule>,
    never: Vec<NeverRule>,
    /// Read-only subtrees a build's own toolchain lives in, with each
    /// path's [`spelling`] beside it so [`Profile::check`] compares without
    /// recomputing one per question. Never writable, and derived from the
    /// environment rather than listed: see [`toolchain_roots`].
    toolchain: Vec<(PathBuf, Vec<String>)>,
    /// Single files in `$HOME` a build's tools read, resolved
    /// ([`TOOLCHAIN_READ_FILES`]). Read-only, and compared exactly rather
    /// than as a prefix: this grants one file, never its directory.
    toolchain_files: Vec<(PathBuf, Vec<String>)>,
    /// The real git directory of a worktree root, and the repository's common
    /// directory, read **and** write — see [`repository_dirs`].
    repository: Vec<(PathBuf, Vec<String>)>,
    command_allow: Vec<String>,
    command_deny: Vec<String>,
    mcp_allow: BTreeSet<String>,
    mcp_deny: BTreeSet<String>,
    diagnostics: Vec<String>,
    /// The request mode this clone was narrowed to; `None` is `execute`.
    narrowing: Option<Narrowing>,
}

/// The process names a shell may attempt after one complete command line has
/// passed [`Profile::admits_command`].  These are evidence for the OS sandbox,
/// not a second admission decision: every name comes from the first word of a
/// segment that the existing deny-before-allow check already accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandGrant {
    executables: Vec<String>,
    /// The segments no `Bash(...)` pattern in `permissions.allow` names.
    /// Empty for a line the allow list admits outright.
    unlisted: Vec<String>,
}

impl CommandGrant {
    pub fn executables(&self) -> &[String] {
        &self.executables
    }

    /// Whether every segment of the line is named by an allow pattern. An
    /// unlisted line is admissible only where a rung judges the call and a
    /// person can be asked about it ([`Profile::weigh_command`]).
    #[must_use]
    pub fn listed(&self) -> bool {
        self.unlisted.is_empty()
    }
}

/// The five `$HOME` directories §4.3 names, refusable by no pattern at all.
const NEVER_GRANTABLE_HOME: [&str; 5] = [".claude", ".codex", ".ssh", ".aws", ".config"];

/// Command names that re-enter the sandbox launcher, §4.6 — refused in
/// every mode, because a nested launcher is a second sandbox no profile
/// described. Matched on every word of a command line, stripped of its
/// directory part, so `sh -c "sandbox-exec …"` is the same refusal as
/// `sandbox-exec`.
const NEVER_GRANTABLE_LAUNCHERS: [&str; 3] = ["sandbox-exec", "bwrap", "bubblewrap"];

/// Command names that attach a debugger, §4.6. Refused outside container
/// mode, where attaching to a process is a way out of Sterna's own OS
/// sandbox; admitted under [`Profile::container_mode`], where that sandbox
/// is not applied and the container is the named boundary.
const NEVER_GRANTABLE_DEBUGGERS: [&str; 9] = [
    "lldb",
    "gdb",
    "strace",
    "ltrace",
    "dtrace",
    "dtruss",
    "windbg",
    "x64dbg",
    "vsjitdebugger.exe",
];

/// §4.6's table for one mode: the launchers always, the debuggers only
/// while Sterna's own sandbox is what a debugger could escape.
fn never_grantable_commands(container_mode: bool) -> impl Iterator<Item = &'static str> {
    let debuggers = if container_mode {
        &NEVER_GRANTABLE_DEBUGGERS[..0]
    } else {
        &NEVER_GRANTABLE_DEBUGGERS[..]
    };
    NEVER_GRANTABLE_LAUNCHERS
        .iter()
        .chain(debuggers.iter())
        .copied()
}

impl Profile {
    /// Compiles the profile a project's loaded configuration implies.
    ///
    /// This is the propagation path in one line: `project::load` fills
    /// `ProjectConfig::settings` with the document's exact bytes, and nothing
    /// between there and here repairs them.
    pub fn from_project(config: &ProjectConfig) -> Self {
        Self::compile(&config.root, config.settings.as_deref())
    }

    /// Compiles the profile for `root` from `settings`, the verbatim text of
    /// `.claude/settings.json`.
    ///
    /// A document that cannot be understood grants nothing rather than
    /// everything: it is untrusted input from a directory invariant 3 makes
    /// writable, so a parse failure, an unknown pattern kind and an unknown
    /// `permissions` key each add a diagnostic and no rule.
    pub fn compile(root: impl AsRef<Path>, settings: Option<&str>) -> Self {
        let supplied_root = root.as_ref();
        // Anchor a relative CLI/project root before lexical resolution. In
        // particular, resolving `.` component-by-component produces an empty
        // PathBuf, which later makes both `read_dir("")` and
        // `Command::current_dir("")` fail with ENOENT even though Sterna's own
        // cwd is healthy. Existing roots are canonicalised so the grant and
        // the path actually opened use one symlink spelling. A missing root
        // stays anchored at its intended absolute location and therefore
        // still fails there; it never falls back to another directory.
        let (anchored, anchor_error) =
            match anchor_project_root(supplied_root, std::env::current_dir()) {
                Ok(root) => (root, None),
                Err(error) => {
                    // No relative root has a stable identity when cwd cannot
                    // be read. Use an absolute sentinel only as a display and
                    // confinement input, then return before compiling any
                    // grants below.
                    let sentinel = invalid_root_sentinel();
                    (sentinel, Some(error))
                }
            };
        let root =
            std::fs::canonicalize(&anchored).unwrap_or_else(|_| resolve(&anchored, None, None));
        let home = home_dir().map(|home| resolve(&home, None, None));
        let mut profile = Self {
            invalid_root: None,
            bypass_os_sandbox: false,
            never: never_rules(&root, home.as_deref()),
            root_spelling: spelling(&root),
            root,
            additional_roots: Vec::new(),
            home,
            allow: Vec::new(),
            deny: Vec::new(),
            toolchain: Vec::new(),
            toolchain_files: Vec::new(),
            repository: Vec::new(),
            command_allow: Vec::new(),
            command_deny: Vec::new(),
            mcp_allow: BTreeSet::new(),
            mcp_deny: BTreeSet::new(),
            diagnostics: Vec::new(),
            narrowing: None,
        };
        if let Some(error) = anchor_error {
            let reason = format!(
                "could not establish an absolute project root: {error}; no permissions were compiled"
            );
            profile.invalid_root = Some(reason.clone());
            profile.diagnostics.push(reason);
            return profile;
        }
        // Derived, not configured, and before the document is read: a build
        // reads its own toolchain whatever `.claude/settings.json` says, and
        // a worktree's git directory is the repository the root already
        // belongs to. Neither is something a pattern could ask for, and
        // neither survives the never-grantable set, which ran above.
        profile.toolchain = toolchain_roots(profile.home.as_deref())
            .into_iter()
            .map(|path| {
                let spelling = spelling(&path);
                (path, spelling)
            })
            .collect();
        for home in toolchain_credential_files(&profile.toolchain) {
            let prefix = spelling(&home);
            profile.never.push(NeverRule {
                glob: subtree_glob(&prefix),
                prefix,
                except: None,
                except_spelling: None,
                write_only: false,
                rule: format!(
                    "`{}` holds a registry token and is never readable, even though the toolchain around it is (docs/sandbox.md, never grantable 2)",
                    display(&home)
                ),
            });
        }
        profile.toolchain_files = toolchain_files(profile.home.as_deref())
            .into_iter()
            .map(|path| {
                let spelling = spelling(&path);
                (path, spelling)
            })
            .collect();
        profile.repository = repository_dirs(&profile.root)
            .into_iter()
            .map(|path| {
                let spelling = spelling(&path);
                (path, spelling)
            })
            .collect();
        let Some(text) = settings else {
            return profile;
        };
        let Ok(document) = serde_json::from_str::<serde_json::Value>(text) else {
            profile
                .diagnostics
                .push("`.claude/settings.json` is not valid JSON; it grants nothing".to_string());
            return profile;
        };
        let Some(permissions) = document.get("permissions") else {
            return profile;
        };
        let Some(permissions) = permissions.as_object() else {
            profile
                .diagnostics
                .push("`permissions` is not an object; it grants nothing".to_string());
            return profile;
        };
        for (key, value) in permissions {
            match key.as_str() {
                "allow" | "deny" => {}
                "ask" => {
                    profile.diagnostics.push(
                        "`permissions.ask` grants nothing: a refusal is a value that is returned, never a question put to a user"
                            .to_string(),
                    );
                    continue;
                }
                other => {
                    profile.diagnostics.push(format!(
                        "`permissions.{other}` is not a pattern list and grants nothing"
                    ));
                    continue;
                }
            }
            let denying = key == "deny";
            let Some(entries) = value.as_array() else {
                profile.diagnostics.push(format!(
                    "`permissions.{key}` is not an array; it grants nothing"
                ));
                continue;
            };
            for entry in entries {
                let Some(pattern) = entry.as_str() else {
                    profile.diagnostics.push(format!(
                        "a non-string entry in `permissions.{key}` grants nothing"
                    ));
                    continue;
                };
                register(&mut profile, pattern, denying);
            }
        }
        profile
    }
}

/// What a rule does to the paths it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// From `permissions.allow`.
    Allow,
    /// From `permissions.deny`, which beats every `allow` (§1.2).
    Deny,
    /// §4's set, which no pattern produced and none can undo.
    Never,
}

/// One compiled rule, as an applier may read it.
///
/// Every field is private and every accessor borrows, which is what makes
/// [`Profile::rules`] an enumeration rather than a second way in: there is no
/// constructor for this type outside the module, so nothing a caller holds
/// can become a grant.
#[derive(Debug, Clone, Copy)]
pub struct Rule<'a> {
    effect: Effect,
    written: &'a str,
    glob: &'a [String],
    read: bool,
    write: bool,
    except: Option<&'a Path>,
}

impl<'a> Rule<'a> {
    /// Allow, deny, or never-grantable.
    pub fn effect(&self) -> Effect {
        self.effect
    }

    /// The pattern as the document wrote it, or — for [`Effect::Never`] — the
    /// sentence a refusal quotes, since no document wrote those.
    pub fn written(&self) -> &'a str {
        self.written
    }

    /// The resolved path components, `*`, `?` and `**` intact. A component is
    /// matched the way [`Profile::check`] matches it: `*` and `?` do not
    /// cross a separator, and `**` spans any number of components.
    pub fn glob(&self) -> &'a [String] {
        self.glob
    }

    /// Whether the rule bears on reading, and on writing. Both are what
    /// [`Profile::check`] does with the rule rather than what its verb said,
    /// so an applier that renders these is as tight as the profile is.
    pub fn read(&self) -> bool {
        self.read
    }

    pub fn write(&self) -> bool {
        self.write
    }

    /// The one subtree this rule does not apply to: the project root, when a
    /// never-grantable directory contains it, and `.sterna/scratch` for
    /// `.sterna/**`. `None` everywhere else.
    pub fn exempt_subtree(&self) -> Option<&'a Path> {
        self.except
    }
}

/// Maps one written pattern to its kind and records it, per §2's table.
///
/// A free function rather than a method, and deliberately: [`Profile`] has
/// no method at all that takes a mutable receiver, so the only place a rule
/// can be added is inside [`Profile::compile`], before the value exists for
/// anyone else to hold.
fn register(profile: &mut Profile, pattern: &str, denying: bool) {
    let (name, argument) = split_pattern(pattern);
    let (read, write) = match name {
        "Read" => (true, false),
        "Write" => (false, true),
        "Edit" => (true, true),
        "Bash" => {
            // Argv admission, and nothing in the filesystem profile. A
            // bare `Bash` admits every command line; the profile is
            // unchanged either way.
            let admitted = argument.unwrap_or("*").to_string();
            if denying {
                profile.command_deny.push(admitted);
            } else {
                if profile.command_allow.is_empty() {
                    // Said once, where a person can read it, rather than
                    // implied: what admits a command line here is a word scan
                    // and a segment match, and neither is a shell. A
                    // diagnostic is the mechanism this module uses everywhere
                    // else for "I did not act on that", and it is a compile-
                    // time value because a built profile can be told nothing
                    // afterwards (§1.1).
                    profile.diagnostics.push(
                        "argv admission is a word scan over each part of a command line, not a shell: a line that assembles a name through a variable, a substitution or a script file is admitted here, and the OS layer is what refuses it (docs/sandbox.md, never grantable 5)"
                            .to_string(),
                    );
                }
                profile.command_allow.push(admitted);
            }
            return;
        }
        "WebFetch" | "WebSearch" => {
            // Network is never granted (§4.1). It is not registered and
            // not carried as a disabled rule: a network-needing tool is
            // absent, not present and failing.
            return;
        }
        other if other.starts_with("mcp__") => {
            if denying {
                profile.mcp_deny.insert(other.to_string());
            } else {
                profile.mcp_allow.insert(other.to_string());
            }
            return;
        }
        other => {
            profile.diagnostics.push(format!(
                "`{other}` is not a pattern kind this profile understands; `{pattern}` grants nothing"
            ));
            return;
        }
    };
    let Some(argument) = argument.map(str::trim).filter(|arg| !arg.is_empty()) else {
        profile.diagnostics.push(format!(
            "`{pattern}` names no path; a bare `{name}` grants nothing"
        ));
        return;
    };
    let glob = resolve_pattern(&profile.root, profile.home.as_deref(), argument);
    if glob.is_empty() {
        profile.diagnostics.push(format!(
            "`{pattern}` resolves to no path at all; it grants nothing"
        ));
        return;
    }
    // A pattern that names no root is anchored at the project root, and the
    // anchor is the whole of what "project-relative" means: `Read(../**)`
    // resolves to the project's parent, which is not a project-relative
    // grant by any reading. Refused rather than narrowed, and diagnosed, so
    // it is not a silent one.
    if !is_rooted(&argument.replace('\\', "/")) && !glob.starts_with(&profile.root_spelling[..]) {
        profile.diagnostics.push(format!(
            "`{pattern}` is project-relative and resolves outside the project root; it grants nothing"
        ));
        return;
    }
    let rule = PathRule {
        written: pattern.to_string(),
        glob,
        read,
        write,
    };
    if denying {
        // A `deny` entry refuses the paths it matches outright, whichever
        // verb spells it: §1.2 and §4.5 state the refusal path-wide, and
        // refusing more than was asked is the safe direction for a
        // document that is itself untrusted input.
        profile.deny.push(rule);
    } else {
        profile.allow.push(rule);
    }
}

impl Profile {
    /// Records the host-selected native-sandbox bypass before the immutable
    /// session profile is shared with any runtime.
    ///
    /// Host-only construction step, never reachable from a program: it
    /// consumes the profile, so it can only run before the profile is shared,
    /// and the session refuses the flag that reaches it without `--yolo` and
    /// on every non-Linux host. No permission pattern can spell it.
    #[must_use]
    pub fn with_os_sandbox_bypass(mut self) -> Self {
        self.bypass_os_sandbox = true;
        self
    }

    /// This profile narrowed to `mode` for one request (ruling *Request
    /// modes*). Consuming, and it only narrows: every refusal the profile
    /// makes still comes first, `execute` returns the profile unchanged, and a
    /// profile already narrowed keeps its first narrowing.
    #[must_use]
    pub fn narrowed_to(mut self, mode: RequestMode, overlay: &ModeOverlay) -> Self {
        if self.narrowing.is_none() {
            let (narrowing, dropped) = Narrowing::compile(
                mode,
                overlay,
                &self.root,
                self.home.as_deref(),
                &self.root_spelling,
            );
            self.narrowing = narrowing;
            self.diagnostics.extend(dropped);
        }
        self
    }

    /// The request mode this profile enforces.
    pub fn request_mode(&self) -> RequestMode {
        self.narrowing
            .as_ref()
            .map_or(RequestMode::Execute, Narrowing::mode)
    }

    /// Whether this session explicitly acknowledged running children without
    /// Sterna's own OS confinement layer.
    pub(crate) fn os_sandbox_bypassed(&self) -> bool {
        self.bypass_os_sandbox
    }

    /// The same fact as [`Profile::os_sandbox_bypassed`], named for what it
    /// means to the policy: the outer container is the process boundary, so
    /// reads span it and a debugger is admissible, while writes, the
    /// never-grantable set and every `deny` pattern hold unchanged.
    pub fn container_mode(&self) -> bool {
        self.bypass_os_sandbox
    }

    /// The project root, resolved.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn additional_roots(&self) -> &[PathBuf] {
        &self.additional_roots
    }

    /// The roots a cell may write under, as [`Profile::check`] would answer.
    ///
    /// **The one place that question is answered, because two derivations of
    /// it disagreed.** A root's writability is not expressed as a rule, so a
    /// reader that listed the write-`allow` rules reported "nothing is
    /// writable" for the ordinary session where `check` admits a write to the
    /// project root — the system block said that on the line above the
    /// manifest saying the opposite (measured 2026-09-18, a dogfooding
    /// session). An invalid root refuses everything, and so is no root at
    /// all here.
    pub fn writable_roots(&self) -> Vec<&Path> {
        if self.invalid_root.is_some() {
            return Vec::new();
        }
        std::iter::once(self.root.as_path())
            .chain(self.additional_roots.iter().map(PathBuf::as_path))
            .collect()
    }

    /// The read-only toolchain subtrees this profile granted, for a platform
    /// applier to render. Read and execute; never write, and never a path a
    /// refusing rule covers — [`Profile::check`] is the authority and these
    /// are the same subtrees it answers from.
    pub fn toolchain_roots(&self) -> impl Iterator<Item = &Path> {
        self.toolchain.iter().map(|(path, _)| path.as_path())
    }

    /// Single files in `$HOME` a build's tools read before they will run,
    /// granted read-only as files ([`TOOLCHAIN_READ_FILES`]).
    pub fn toolchain_read_files(&self) -> impl Iterator<Item = &Path> {
        self.toolchain_files.iter().map(|(path, _)| path.as_path())
    }

    /// The files inside those subtrees that stay unreadable, so an applier
    /// can deny them after granting the subtree around them.
    pub fn toolchain_credentials(&self) -> Vec<PathBuf> {
        toolchain_credential_files(&self.toolchain)
    }

    /// The git directories of a worktree root, read and write
    /// ([`repository_dirs`]). Empty for an ordinary checkout.
    pub fn repository_dirs(&self) -> impl Iterator<Item = &Path> {
        self.repository.iter().map(|(path, _)| path.as_path())
    }

    /// Host-only construction step. Consuming the profile keeps the active
    /// session immutable; project permission patterns cannot opt into this.
    /// Arbitrary deny globs are not approximated by OS subtree grants.
    pub fn with_additional_root(mut self, path: impl AsRef<Path>) -> Result<Self, String> {
        if cfg!(target_os = "windows") {
            return Err(
                "--add-dir is not supported by the Windows AppContainer applier yet".into(),
            );
        }
        if self.invalid_root.is_some() {
            return Err("cannot add a directory to an invalid project profile".into());
        }
        let supplied = if path.as_ref().is_absolute() {
            path.as_ref().to_path_buf()
        } else {
            self.root.join(path)
        };
        let root = std::fs::canonicalize(&supplied)
            .map_err(|error| format!("additional directory {}: {error}", supplied.display()))?;
        if !root.is_dir() {
            return Err("additional root must be an existing directory".into());
        }
        if root == self.root || self.additional_roots.contains(&root) {
            return Ok(self);
        }
        if !self.deny.is_empty() {
            return Err("--add-dir cannot be combined with filesystem deny patterns until every platform can enforce their exclusions; no additional directory was granted".into());
        }
        let candidate = spelling(&root);
        for never in &self.never {
            // Only the broad home boundary can be carved out by the host.
            // Credential stores and state remain forbidden, including when
            // the requested directory would contain one of those subtrees.
            let broad_home = self
                .home
                .as_ref()
                .is_some_and(|home| spelling(home) == never.prefix);
            if broad_home {
                continue;
            }
            if contains_refusing(&never.prefix, &candidate)
                || contains_refusing(&candidate, &never.prefix)
            {
                return Err(format!("additional directory refused: {}", never.rule));
            }
        }
        for (name, affordance) in [
            (".claude", ""),
            (
                ".sterna",
                "; write scratch files elsewhere under the project root",
            ),
        ] {
            let protected = spelling(&root.join(name));
            self.never.push(NeverRule {
                glob: subtree_glob(&protected),
                prefix: protected,
                except: None,
                except_spelling: None,
                write_only: true,
                rule: format!(
                    "`{name}/**` in an additional directory is never writable{affordance}"
                ),
            });
        }
        self.additional_roots.push(root);
        Ok(self)
    }

    /// Every rule this profile compiled, in the order [`Profile::check`]
    /// consults them: §4's never-grantable set, then `deny`, then `allow`.
    ///
    /// **A platform applier cannot hold §1.2 for a rule it cannot see.** A
    /// profile that could only be asked about one path at a time left the
    /// seatbelt profile, the Landlock ruleset and the Windows ACL to be built
    /// from the project root alone, so an in-root
    /// `deny: ["Read(<root>/secrets/**)"]` was refused in process and granted
    /// by the kernel — §1.2 holding in one layer and not in the other, and
    /// the kernel is the layer that is supposed to be the backstop when the
    /// in-process check is wrong.
    ///
    /// Read-only, and structurally rather than by promise: [`Rule`] borrows
    /// this profile, has no public field and no constructor outside this
    /// module, so it can be rendered and there is no expression that turns
    /// one back into a grant (§1.1).
    pub fn rules(&self) -> impl Iterator<Item = Rule<'_>> {
        let never = self.never.iter().map(|rule| Rule {
            effect: Effect::Never,
            written: rule.rule.as_str(),
            glob: rule.glob.as_slice(),
            // What [`Profile::check`] does with it, not what a verb said: a
            // never rule refuses both halves unless it is the write-only
            // `.claude/**`.
            read: !rule.write_only,
            write: true,
            except: rule.except.as_deref(),
        });
        let deny = self.deny.iter().map(|rule| Rule {
            effect: Effect::Deny,
            written: rule.written.as_str(),
            glob: rule.glob.as_slice(),
            // Also what [`Profile::check`] does: a `deny` refuses the paths
            // it matches whichever verb spelled it, so an applier that
            // denied only the written half would be looser than the profile.
            read: true,
            write: true,
            except: None,
        });
        let allow = self.allow.iter().map(|rule| Rule {
            effect: Effect::Allow,
            written: rule.written.as_str(),
            glob: rule.glob.as_slice(),
            read: rule.read,
            write: rule.write,
            except: None,
        });
        never.chain(deny).chain(allow)
    }

    /// What the document said that this profile did not act on. Empty for a
    /// document every entry of which mapped to a rule.
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// How many filesystem rules the document produced. A `Bash` or
    /// `WebFetch` pattern leaves this untouched, which is what makes §2's
    /// separation observable rather than asserted.
    pub fn rule_count(&self) -> usize {
        self.allow.len() + self.deny.len()
    }

    /// How many command-line patterns were admitted.
    pub fn command_pattern_count(&self) -> usize {
        self.command_allow.len()
    }

    /// Every admitted command-line pattern in its written `Bash(...)` form,
    /// in document order. A bare `Bash` is stored as `*` and renders as
    /// `Bash(*)`, which names the same grant.
    pub fn command_patterns(&self) -> Vec<String> {
        self.command_allow
            .iter()
            .map(|pattern| format!("Bash({pattern})"))
            .collect()
    }

    /// §4.6's command names as this profile's mode refuses them: the sandbox
    /// launchers always, the debuggers only outside container mode.
    pub fn never_grantable_commands(&self) -> Vec<&'static str> {
        never_grantable_commands(self.container_mode()).collect()
    }

    /// How many MCP tool patterns were admitted. A pattern may glob, so this
    /// counts patterns rather than tools.
    pub fn mcp_tool_count(&self) -> usize {
        self.mcp_allow.len()
    }

    /// Whether this profile grants any network reach. Always `false`: no
    /// `permissions` pattern names a host, a port or a protocol, so a network
    /// grant would have to be invented, and an invented capability is the one
    /// thing an allow-list must never produce (§4.1).
    pub fn grants_network(&self) -> bool {
        false
    }

    /// Whether an MCP tool is registered. A tool matched by `deny` is not,
    /// and a network-needing tool never is.
    ///
    /// A tool name is matched by the same [`match_segment`] every path
    /// component is matched by, and with the same case decision: `deny`
    /// folds, `allow` does not. Exact-string equality here would have made
    /// `deny: ["mcp__git__*"]` deny nothing at all while every path pattern
    /// beside it globbed — a grant nobody asked for and no diagnostic.
    pub fn admits_mcp_tool(&self, name: &str) -> bool {
        // An MCP tool's effect is undeclared, so a narrowing mode admits none.
        if self.invalid_root.is_some() || self.narrowing.is_some() {
            return false;
        }
        if name.eq_ignore_ascii_case("webfetch") || name.eq_ignore_ascii_case("websearch") {
            return false;
        }
        if self
            .mcp_deny
            .iter()
            .any(|pattern| match_segment(pattern, name, true))
        {
            return false;
        }
        self.mcp_allow
            .iter()
            .any(|pattern| match_segment(pattern, name, false))
    }

    /// Discovery may start only when an existing allow names this server's
    /// namespace (or a broader glob), and the candidate is not denied. This
    /// does not admit a call: every advertised tool is checked separately.
    pub fn admits_mcp_server(&self, server: &str) -> bool {
        self.narrowing.is_none()
            && self.mcp_allow.iter().any(|pattern| {
                let Some((server_pattern, tool_pattern)) = pattern
                    .strip_prefix("mcp__")
                    .and_then(|rest| rest.split_once("__"))
                else {
                    return self.admits_mcp_tool(&format!("mcp__{server}__*"));
                };
                match_segment(server_pattern, server, false)
                    && self.admits_mcp_tool(&format!("mcp__{server}__{tool_pattern}"))
            })
    }

    /// The first question of §2: may this command line be attempted at all?
    ///
    /// Answering `Ok` grants no file access whatsoever — the process it
    /// spawns gets exactly the grants the `Read`/`Write`/`Edit` patterns
    /// produced, which [`Profile::check`] is what answers.
    pub fn admits_command(&self, command_line: &str) -> Result<CommandGrant, PermissionDenied> {
        let grant = self.weigh_command(command_line)?;
        match grant.unlisted.first() {
            Some(segment) => Err(PermissionDenied {
                tool: "Bash".to_string(),
                path: command_line.to_string(),
                rule: format!("no `Bash` pattern in permissions.allow admits `{segment}`"),
            }),
            None => Ok(grant),
        }
    }

    /// [`Self::admits_command`] without its last question: a segment no
    /// allow pattern names is recorded as unlisted instead of refused.
    ///
    /// **Call-level judgement, not an OS cage** (the ruling of 2026-09-19).
    /// The foreground command tool calls this when the permission ladder
    /// judges the call and a person can answer it: there a read-only line
    /// runs on Auto, anything else asks, and nothing is refused only for
    /// being absent from a list nobody wrote. Every absolute refusal still
    /// holds here -- a `permissions.deny` match, a launcher or debugger, the
    /// request mode's narrowing. Background runs, verification and a session
    /// nobody can be asked in keep [`Self::admits_command`].
    pub fn weigh_command(&self, command_line: &str) -> Result<CommandGrant, PermissionDenied> {
        let denied = |rule: String| -> Result<CommandGrant, PermissionDenied> {
            Err(PermissionDenied {
                tool: "Bash".to_string(),
                path: command_line.to_string(),
                rule,
            })
        };
        if let Some(reason) = &self.invalid_root {
            return denied(reason.clone());
        }
        if let Some(name) = escaping_command(command_line, self.container_mode()) {
            return denied(format!(
                "`{name}` re-enters the sandbox launcher or attaches a debugger and is never grantable by any pattern (docs/sandbox.md, never grantable 5)"
            ));
        }
        // §1.2 is a rule about both of §2's questions, not only about paths.
        // A `deny` matched against the whole line lets an `allow` win by
        // concatenation — `cargo test -q; curl … | sh` is not `cargo test` —
        // so every part a shell would run as a command of its own is asked
        // separately, and one refused part refuses the line.
        let segments = command_segments(command_line);
        if segments.is_empty() {
            return denied(
                "no `Bash` pattern in permissions.allow admits this command line".to_string(),
            );
        }
        let mut executables = Vec::with_capacity(segments.len());
        let mut unlisted = Vec::new();
        for segment in &segments {
            // A leading redirect is not the command: `2>&1 cargo test` is
            // matched on `cargo test`, never on the operand that happens to
            // come first. `segment` itself is still what a refusal quotes.
            let command_word = skip_leading_redirects(segment);
            for pattern in &self.command_deny {
                if match_segment(pattern, command_word, true) {
                    return denied(format!(
                        "`Bash({pattern})` in permissions.deny matches `{segment}`"
                    ));
                }
            }
            if !self
                .command_allow
                .iter()
                .any(|pattern| match_segment(pattern, command_word, false))
            {
                unlisted.push(segment.clone());
            }
            if let Some(executable) = literal_executable(command_word) {
                executables.push(executable.to_string());
            }
        }
        if let Some(rule) = self
            .narrowing
            .as_ref()
            .and_then(|mode| mode.command_refusal(command_line))
        {
            return denied(rule);
        }
        Ok(CommandGrant {
            executables,
            unlisted,
        })
    }

    /// Whether an executable already admitted by a `Bash(...)` command is
    /// outside §4's never-grantable read roots. The command grant supplies
    /// the positive authority; this method preserves the absolute refusals
    /// when the OS layer turns that authority into a literal exec rule.
    pub fn executable_is_refused(&self, path: &Path) -> bool {
        if self.invalid_root.is_some() {
            return true;
        }
        let resolved = resolve(path, Some(&self.root), self.home.as_deref());
        let candidate = spelling(&resolved);
        if device_refusal(&resolved).is_some() {
            return true;
        }
        let never = self.never.iter().any(|never| {
            !never.write_only
                && contains_refusing(&never.prefix, &candidate)
                && !never
                    .except_spelling
                    .as_ref()
                    .is_some_and(|except| contains(except, &candidate))
                && !(self
                    .home
                    .as_ref()
                    .is_some_and(|home| spelling(home) == never.prefix)
                    && self
                        .additional_roots
                        .iter()
                        .any(|root| contains(&spelling(root), &candidate)))
        });
        let denied = self
            .deny
            .iter()
            .any(|rule| rule.read && covers(&rule.glob, &candidate, true));
        never || denied
    }

    /// Whether a bare `Bash` grant admits every command line.
    ///
    /// **Derived from the compiled profile, never from the caller's
    /// intention.** A session tells the model what its sandbox permits, and a
    /// flag asking for a grant is not the same fact as a profile holding one:
    /// a settings document that failed to parse, or a `--yolo` that never
    /// reached the compiler, would otherwise be described to the model as an
    /// open grant it does not have. Bare `Bash` is stored as the pattern `*`
    /// (see the `"Bash"` arm above), which is the whole of this answer.
    pub fn admits_every_command(&self) -> bool {
        self.command_allow.iter().any(|pattern| pattern == "*")
    }

    /// [`Profile::check`], then the request mode: the question a tool call
    /// asks. `check` itself stays the session's answer, because the platform
    /// appliers probe it to render the OS layer, and a mode must not change
    /// that layer — on Windows a program the session could write is refused
    /// exec, and a narrowed answer would lift that refusal.
    pub fn check_request(
        &self,
        tool: &str,
        access: Access,
        path: &Path,
    ) -> Result<PathBuf, PermissionDenied> {
        let resolved = self.check(tool, access, path)?;
        if access == Access::Write
            && let Some(rule) = self
                .narrowing
                .as_ref()
                .and_then(|mode| mode.write_refusal(&spelling(&resolved)))
        {
            return Err(PermissionDenied {
                tool: tool.to_string(),
                path: shown(&resolved),
                rule,
            });
        }
        Ok(resolved)
    }

    /// The second question of §2: may `tool` touch `path` for `access`, and
    /// **which path was that**?
    ///
    /// The returned `PathBuf` is the resolved path the decision was made on,
    /// and a caller must open that rather than the string it passed in. The
    /// two differ whenever the argument was spelled with a `~`, a relative
    /// prefix, a `.`, a `..` or a symlinked component, so a caller that
    /// re-opened its own argument would be opening a file this profile never
    /// examined.
    ///
    /// Decided in the only order that keeps §1.2 true — never-grantable
    /// first, then `deny`, then `allow` — so no `allow`, however exact, can
    /// reach past either. `path` is resolved before matching: `~` expands, a
    /// relative path resolves against the project root, and symlinks are
    /// resolved component by component, because two spellings of one path are
    /// how a containment check comes to disagree with itself.
    pub fn check(
        &self,
        tool: &str,
        access: Access,
        path: &Path,
    ) -> Result<PathBuf, PermissionDenied> {
        let resolved = resolve(path, Some(&self.root), self.home.as_deref());
        let shown = shown(&resolved);
        let candidate = spelling(&resolved);
        // The postcondition this function's own documentation states, held
        // where it is produced rather than where it is read: a granted path
        // is the absolute path the decision was made on, so opening it lands
        // on the file this profile examined. A root that is not absolute is
        // a literal-strings fixture — a Windows spelling on a Unix host —
        // and has no absolute form to demand (see `resolve`).
        let grant = |resolved: PathBuf| -> Result<PathBuf, PermissionDenied> {
            debug_assert!(
                resolved.is_absolute() || !self.root.is_absolute(),
                "`check` granted the non-absolute `{}`; a caller opening it would open a file this profile never examined",
                resolved.display()
            );
            Ok(resolved)
        };
        let denied = |rule: String| -> Result<PathBuf, PermissionDenied> {
            Err(PermissionDenied {
                tool: tool.to_string(),
                path: shown.clone(),
                rule,
            })
        };
        if let Some(reason) = &self.invalid_root {
            return denied(reason.clone());
        }
        // Before anything is compared, and not after: a path this module
        // cannot place inside or outside a grant is refused rather than
        // matched (§1.4).
        if let Some(rule) = device_refusal(&resolved) {
            return denied(rule);
        }
        // Every spelling a refusing rule must survive, and only the refusing
        // rules see them: one file may be written with an alternate data
        // stream and without it, and §4.5 has to hold for both. An `allow`
        // still decides on the one spelling that was asked for.
        let stripped = stream_stripped(&candidate);
        let refusable: Vec<&[String]> = std::iter::once(&candidate[..])
            .chain(stripped.as_deref())
            .collect();
        for never in &self.never {
            if never.write_only && access != Access::Write {
                continue;
            }
            if !refusable
                .iter()
                .any(|form| contains_refusing(&never.prefix, form))
            {
                continue;
            }
            // The project's own subtree, and only it, is exempt — and only
            // from a rule whose subtree contains the root. Compared exactly
            // while the rule above compares loosely, because this half is a
            // grant: a candidate whose spelling the exemption cannot confirm
            // keeps the refusal.
            if let Some(except) = &never.except_spelling
                && contains(except, &candidate)
            {
                continue;
            }
            // The broad `$HOME` boundary, and only it, is carved out by the
            // host's own derived grants: an explicit additional root, the
            // toolchain a build reads, and the repository a worktree belongs
            // to. Credential stores and state keep their own rules, which is
            // why this tests the broad rule by identity rather than skipping
            // every rule whose subtree contains the candidate.
            if self
                .home
                .as_ref()
                .is_some_and(|home| spelling(home) == never.prefix)
                && (self
                    .additional_roots
                    .iter()
                    .any(|root| contains(&spelling(root), &candidate))
                    || self
                        .repository
                        .iter()
                        .any(|(_, prefix)| contains(prefix, &candidate))
                    || (access == Access::Read
                        && (self
                            .toolchain
                            .iter()
                            .any(|(_, prefix)| contains(prefix, &candidate))
                            || self
                                .toolchain_files
                                .iter()
                                .any(|(_, file)| file == &candidate))))
            {
                continue;
            }
            return denied(never.rule.clone());
        }
        for rule in &self.deny {
            if refusable.iter().any(|form| covers(&rule.glob, form, true)) {
                return denied(format!("`{}` in permissions.deny", rule.written));
            }
        }
        // After every refusing rule, so a never-writable name is refused by
        // its own rule; before every grant, because a grant judges a name and
        // a write through a hard link reaches every other name of the file.
        if access == Access::Write
            && let Some(rule) = hard_link_refusal(&resolved)
        {
            return denied(rule);
        }
        if contains(&self.root_spelling, &candidate)
            || self
                .additional_roots
                .iter()
                .any(|root| contains(&spelling(root), &candidate))
        {
            return grant(resolved);
        }
        // The repository this root belongs to, read and write, because git
        // writes its index and refs there and a worktree keeps them outside
        // the root by construction ([`repository_dirs`]).
        if self
            .repository
            .iter()
            .any(|(_, prefix)| contains(prefix, &candidate))
        {
            return grant(resolved);
        }
        // The toolchain, read only, and after every refusing rule — so the
        // credential carve-out above has already had its turn on a path
        // inside one of these ([`toolchain_roots`]).
        if access == Access::Read
            && (self
                .toolchain
                .iter()
                .any(|(_, prefix)| contains(prefix, &candidate))
                || self
                    .toolchain_files
                    .iter()
                    .any(|(_, file)| file == &candidate))
        {
            return grant(resolved);
        }
        let granted = self.allow.iter().any(|rule| {
            let wanted = match access {
                Access::Read => rule.read,
                Access::Write => rule.write,
            };
            wanted && covers(&rule.glob, &candidate, false)
        });
        if granted {
            return grant(resolved);
        }
        // Container mode, reads only: the container is the boundary, and
        // every refusing rule — never-grantable, `deny` — has already had its
        // turn above, so a read that reaches here is one nothing refused.
        // Writes keep the roots, which is why this is the last step and not
        // an early grant.
        if access == Access::Read && self.container_mode() {
            return grant(resolved);
        }
        denied(access.only_root_sentence(self.container_mode()).to_string())
    }
}

/// The refusal for a write whose target is an existing regular file with
/// more than one name, or whose metadata cannot be read (sandbox-grants.md
/// §1.5). A path that does not exist, a directory and a single-name file get
/// `None`. Names, not zones: the other names are invisible from this one, so
/// no grant on this name can say where the write lands.
#[cfg(unix)]
fn hard_link_refusal(resolved: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(resolved) {
        Ok(meta) if meta.is_file() && meta.nlink() > 1 => Some(format!(
            "hard-linked file ({} names): a write here reaches every name; copy it to a new file instead",
            meta.nlink()
        )),
        Ok(_) => None,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            None
        }
        Err(error) => Some(format!(
            "the link count of this file could not be read ({error}); a write is refused rather than granted"
        )),
    }
}

/// Windows: `MetadataExt::number_of_links` is unstable on the pinned
/// toolchain, so no link count is read and nothing is refused here
/// (docs/sandbox.md, invariant 5 states the gap).
#[cfg(not(unix))]
fn hard_link_refusal(_resolved: &Path) -> Option<String> {
    None
}

/// The agent's scratchpad, relative to the project root: the one subtree of
/// `.sterna/**` the never rule exempts (docs/sandbox.md, invariant 5).
pub const SCRATCH_DIR: &str = ".sterna/scratch";

/// Builds §4's never-grantable set for one project root.
///
/// **No entry is ever dropped for where the project root sits.** A rule whose
/// subtree contains the root keeps the rule and exempts the root's own
/// subtree, so a project under `~/.config` can use `~/.config/myproj/**` and
/// still cannot touch `~/.config/gh/**`; a rule inside the root is kept as
/// written, so a project rooted at `$HOME` does not acquire `~/.ssh` by being
/// there. Skipping either shape — which is what this did — reached §4.3 and
/// §4.4 from an empty `permissions` object. `.claude/**` inside the root is
/// the one deliberate exception, and it is write-only.
///
/// Order is the message, not the answer: every rule below refuses, and the
/// specific entries are pushed before the whole of `$HOME` so a refusal cites
/// the section a person would look up.
fn never_rules(root: &Path, home: Option<&Path>) -> Vec<NeverRule> {
    let root_spelling = spelling(root);
    let dot_claude = spelling(&root.join(".claude"));
    let mut rules = vec![NeverRule {
        glob: subtree_glob(&dot_claude),
        prefix: dot_claude,
        except: None,
        except_spelling: None,
        write_only: true,
        rule: "`.claude/**` is never writable: a program that could edit it could widen the profile it was derived from (docs/sandbox.md, invariant 5)".to_string(),
    }];
    let dot_sterna = spelling(&root.join(".sterna"));
    let scratch = root.join(SCRATCH_DIR);
    rules.push(NeverRule {
        glob: subtree_glob(&dot_sterna),
        prefix: dot_sterna,
        except_spelling: Some(spelling(&scratch)),
        except: Some(scratch),
        write_only: true,
        rule: "`.sterna/**` is host-owned configuration and never writable by agent tools, except `.sterna/scratch/**`, the agent's scratchpad; write scratch files elsewhere under the project root".into(),
    });
    // `Some(root)`, and it is what makes §4.2 hold on Windows: `/etc/sudoers`
    // has a root and no drive there, so a candidate spelled that way acquires
    // the project's drive from `Path::join` and becomes `C:/etc/sudoers`. A
    // prefix resolved without the same root stayed `/etc/sudoers`, matched no
    // candidate at all, and `Write(/**)` reached the file. On macOS and Linux
    // every prefix here is already absolute, so the argument is never used.
    let mut push = |prefix: PathBuf, write_only: bool, rule: String| {
        let prefix = resolve(&prefix, Some(root), None);
        let prefix = spelling(&prefix);
        let except = contains(&prefix, &root_spelling).then(|| root.to_path_buf());
        let mut glob = prefix.clone();
        glob.push("**".to_string());
        rules.push(NeverRule {
            except_spelling: except.as_ref().map(|_| root_spelling.clone()),
            prefix,
            glob,
            except,
            write_only,
            rule,
        });
    };
    let keyring = "the OS keyring or credential store is never grantable by any pattern (docs/sandbox.md, never grantable 2)";
    if let Some(home) = home {
        for name in NEVER_GRANTABLE_HOME {
            push(
                home.join(name),
                false,
                format!(
                    "`~/{name}` is never grantable by any pattern (docs/sandbox.md, never grantable 3)"
                ),
            );
        }
        for path in [
            home.join("Library").join("Keychains"),
            home.join(".local").join("share").join("keyrings"),
            home.join(".gnupg"),
        ] {
            push(path, false, keyring.to_string());
        }
    }
    push(
        PathBuf::from("/Library/Keychains"),
        false,
        keyring.to_string(),
    );
    for state in gateway_state_dirs(home) {
        push(
            state,
            false,
            "the inference gateway's state directory, which holds its database and subscription sign-ins, is never grantable by any pattern (docs/sandbox.md, never grantable 2)".to_string(),
        );
    }
    for path in system_credential_paths() {
        push(path, true, keyring.to_string());
    }
    if let Some(home) = home {
        // §4.3 as it is titled: **`$HOME` outside the project**, whatever
        // pattern names it. The five names in that sentence are the ones that
        // matter, not the whole of what it refuses — a sandbox that lets a
        // tool rewrite `~/.gitconfig` or `~/.zsh_history` is not holding a
        // boundary. Last, so the named entries above keep their own sections.
        push(
            home.to_path_buf(),
            false,
            "`$HOME` outside the project is never grantable by any pattern (docs/sandbox.md, never grantable 3)"
                .to_string(),
        );
    }
    rules
}

/// A resolved subtree as a glob: its spelling, then `**`.
fn subtree_glob(prefix: &[String]) -> Vec<String> {
    let mut glob = prefix.to_vec();
    glob.push("**".to_string());
    glob
}

/// The machine's own credential and identity store — §4.2's system half,
/// which is what stops `Write(/**)` reaching `/etc/sudoers`.
///
/// Named files and directories rather than the whole of `/etc`, because
/// `/etc/hosts` is an ordinary readable file and §3's own seatbelt shape
/// reads `/etc/passwd`; and write-only at the call site above for the same
/// reason. Computed without a `#[cfg]`, so a profile compiled on one host
/// still refuses another host's spelling.
fn system_credential_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = [
        "/etc/sudoers",
        "/etc/sudoers.d",
        "/etc/shadow",
        "/etc/gshadow",
        "/etc/passwd",
        "/etc/master.passwd",
        "/etc/group",
        "/etc/pam.d",
        "/etc/ssh",
        "/etc/security",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    for key in ["SystemRoot", "windir"] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            paths.push(PathBuf::from(value).join("System32").join("config"));
        }
    }
    paths
}

/// Every shape the inference gateway's state directory takes, on every
/// platform, computed without a `#[cfg]` so a profile compiled on one host
/// still refuses the paths another host would use. It holds the gateway's
/// database and the subscription sign-ins its brokers use, so it is part of
/// the credential store even where it lies outside `$HOME`.
fn gateway_state_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for key in [
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            roots.push(PathBuf::from(value));
        }
    }
    if let Some(home) = home {
        roots.push(home.join(".local").join("share"));
        roots.push(home.join(".local").join("state"));
        roots.push(home.join(".cache"));
        roots.push(home.join("Library").join("Application Support"));
        roots.push(home.join("Library").join("Caches"));
        roots.push(home.join("AppData").join("Roaming"));
        roots.push(home.join("AppData").join("Local"));
    }
    let mut dirs: Vec<PathBuf> = roots
        .into_iter()
        .map(|root| root.join("inference-gateway"))
        .collect();
    if let Some(value) = std::env::var_os("INFERENCE_GATEWAY_DATA_DIR")
        && !value.is_empty()
    {
        dirs.push(PathBuf::from(value));
    }
    dirs
}

/// **This scan cannot catch a name the shell assembles**: `sh -c 'S=sandbox-exec;
/// $S …'`, a `$(printf …)` that builds one, or a `sh ./run.sh` whose script
/// the project root makes writable — no word of any of those lines names an
/// escape, and no word list closes that, because the shell is a general
/// interpreter. §4.6 is held by the platform appliers (seatbelt's
/// `(deny process-exec* …)`, Landlock plus `no_new_privs`, the AppContainer);
/// this function is a cheap early refusal in front of them and nothing more.
///
/// What it does catch: the word of `command_line` naming one of §4.6's
/// escapes, if any. Every word is examined rather than the first, because
/// `sh -c "bwrap …"` and `sudo lldb` are the spellings a first-word check
/// walks straight past; a command line that merely mentions one of these
/// names is refused too, which is the direction a never-grantable set must
/// err in.
fn escaping_command(command_line: &str, container_mode: bool) -> Option<&'static str> {
    command_line.split_whitespace().find_map(|word| {
        let base = word
            .trim_matches(['"', '\'', '`', '(', ')', ';', '&', '|'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(word);
        never_grantable_commands(container_mode).find(|name| base.eq_ignore_ascii_case(name))
    })
}

/// `$HOME`, read from the environment rather than from a platform helper so
/// this stays one code path on every host.
/// The toolchain homes a build reads, as (environment variable, `$HOME`
/// fallback) pairs.
///
/// **The toolchain a build reads is part of the build.** A compiler, a
/// registry cache and a version manifest are not capabilities worth
/// withholding: they are read-only, they are the developer's own, and
/// without them `cargo test` cannot resolve a toolchain at all — measured
/// 2026-09-17 in session `tlj14m-24r`, where a model spent a large part of
/// twenty million tokens failing to verify its own work because
/// `~/.rustup/settings.toml` was unreadable.
///
/// Derived from the environment first, because a developer who moved
/// `CARGO_HOME` moved it for every tool and this one must follow. The
/// `$HOME` fallback is the default layout each of these ships with.
///
/// **Only toolchains whose read is unambiguous are here.** `~/.npm` is
/// npm's content-addressed package cache, `~/.nvm` holds whole Node
/// installations a build execs, and `~/.pyenv` the same for Python — each is
/// a toolchain's own store and nothing else. Declined: `~/.npmrc` and
/// `~/.pypirc`, which are single files holding registry auth tokens rather
/// than caches; `~/.docker`, whose `config.json` carries registry
/// credentials; `~/.gradle` and `~/.m2`, which hold `gradle.properties` and
/// `settings.xml`, both conventional homes for signing keys and repository
/// passwords. A store whose ordinary contents include a secret is not a
/// toolchain read, and admitting it here would make this list the thing §4.2
/// exists to prevent.
const TOOLCHAIN_HOMES: [(&str, &str); 6] = [
    ("CARGO_HOME", ".cargo"),
    ("RUSTUP_HOME", ".rustup"),
    ("npm_config_cache", ".npm"),
    ("NVM_DIR", ".nvm"),
    ("PYENV_ROOT", ".pyenv"),
    ("UV_CACHE_DIR", ".cache/uv"),
];

/// Single files in `$HOME` a build's tools read before they will run at all.
///
/// `~/.gitconfig` is the one measured case: under confinement `git` exits
/// with *"unable to access '/Users/…/.gitconfig': Operation not permitted"*
/// before it does anything, so a worktree grant without it buys nothing.
/// Granted as a **file**, never as a subtree, and read-only.
///
/// `~/.git-credentials` is deliberately absent and stays refused by the
/// `$HOME` rule: it is the store `credential.helper=store` writes, and it is
/// the same class as a registry token.
const TOOLCHAIN_READ_FILES: [&str; 1] = [".gitconfig"];

/// File names inside a toolchain home that hold a registry token rather than
/// a cache. `cargo login` writes the first; the second is its pre-1.78
/// spelling, and both are still read.
const TOOLCHAIN_CREDENTIAL_FILES: [&str; 2] = ["credentials.toml", "credentials"];

/// The toolchain homes that exist on this machine, resolved.
///
/// A path the environment does not name and that does not exist is not an
/// error and produces no rule: a machine without rustup simply has no
/// rustup grant. Canonicalised, because every comparison in this module is
/// made on the resolved spelling, and skipped when the resolve fails, since
/// a grant on a path that cannot be resolved cannot be compared to one.
fn toolchain_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for (variable, fallback) in TOOLCHAIN_HOMES {
        let named = std::env::var_os(variable)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let candidate = match (named, home) {
            (Some(path), _) => path,
            (None, Some(home)) => home.join(fallback),
            (None, None) => continue,
        };
        let Ok(resolved) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if !resolved.is_dir() || roots.contains(&resolved) {
            continue;
        }
        roots.push(resolved);
    }
    roots
}

/// The credential files the toolchain grant must not reach.
///
/// Cargo's home and cargo's two spellings only: this is a carve-out for a
/// known file, not a search for anything that looks like a secret, and the
/// other toolchain homes here keep their tokens outside the directory this
/// profile grants (`~/.npmrc`, not `~/.npm`). A file that does not exist
/// still earns its rule, because it is the name that is refused and `cargo
/// login` may write it tomorrow.
fn toolchain_credential_files(toolchain: &[(PathBuf, Vec<String>)]) -> Vec<PathBuf> {
    let Some(cargo) = cargo_home() else {
        return Vec::new();
    };
    if !toolchain.iter().any(|(root, _)| root == &cargo) {
        return Vec::new();
    }
    TOOLCHAIN_CREDENTIAL_FILES
        .iter()
        .map(|name| cargo.join(name))
        .collect()
}

/// The single files [`TOOLCHAIN_READ_FILES`] names, resolved, for the ones
/// that exist on this machine.
fn toolchain_files(home: Option<&Path>) -> Vec<PathBuf> {
    let Some(home) = home else {
        return Vec::new();
    };
    TOOLCHAIN_READ_FILES
        .iter()
        .filter_map(|name| std::fs::canonicalize(home.join(name)).ok())
        .filter(|path| path.is_file())
        .collect()
}

/// Cargo's home as [`toolchain_roots`] resolved it, or `None`.
fn cargo_home() -> Option<PathBuf> {
    let named = std::env::var_os("CARGO_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let candidate = match named {
        Some(path) => path,
        None => home_dir()?.join(".cargo"),
    };
    std::fs::canonicalize(candidate).ok()
}

/// The git directories a session rooted in a **worktree** must reach, read
/// and write.
///
/// A worktree's `.git` is a file whose one line reads `gitdir: <path>`,
/// pointing into the main repository's `.git/worktrees/<name>` — outside the
/// project root, and therefore outside every grant a root-based profile
/// makes. Without this, `git status` fails and so does anything built on it:
/// measured 2026-09-17, `blast-radius.sh` refused with *"is not a git
/// worktree"* because it could not read the metadata that proves what it is.
///
/// **Write, not read**, and that is the widening this function makes: git
/// writes its index, its refs and new objects there, and the common
/// directory it names is the main repository's own `.git`. A session in a
/// worktree can therefore write the repository's git directory — never its
/// working tree, which is a separate path no grant here names.
///
/// An ordinary checkout, where `.git` is a directory inside the root, needs
/// nothing: the read of that file fails and this returns empty.
fn repository_dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(root.join(".git")) else {
        return Vec::new();
    };
    let Some(named) = text
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("gitdir:"))
    else {
        return Vec::new();
    };
    let named = PathBuf::from(named.trim());
    let gitdir = if named.is_absolute() {
        named
    } else {
        root.join(named)
    };
    let Ok(gitdir) = std::fs::canonicalize(&gitdir) else {
        return Vec::new();
    };
    let mut dirs = vec![gitdir.clone()];
    // `commondir` is how a linked worktree names the repository it belongs
    // to; its own objects and refs live there, so a grant on the worktree's
    // directory alone leaves every object write refused.
    if let Ok(text) = std::fs::read_to_string(gitdir.join("commondir")) {
        let named = PathBuf::from(text.trim());
        let common = if named.is_absolute() {
            named
        } else {
            gitdir.join(named)
        };
        if let Ok(common) = std::fs::canonicalize(&common)
            && !dirs.contains(&common)
        {
            dirs.push(common);
        }
    }
    dirs
}

fn home_dir() -> Option<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            return Some(PathBuf::from(value));
        }
    }
    None
}

/// Resolves a candidate path the way every comparison in this module needs
/// it: `~` expands, a relative path resolves against `root`, and the
/// components are resolved **in the order the kernel would follow them**, so
/// a `..` is applied to the directory a call would be standing in rather than
/// to the name as written.
///
/// That order is the invariant, and it holds because [`canonical_prefix`] is
/// applied to the accumulator *before* every `ParentDir` pop rather than to
/// the whole string afterwards. Popping textually first is how
/// `<root>/link/../.ssh/id_ed25519` came to be `<root>/.ssh/id_ed25519` here
/// — a path inside the project — while every real call landed in `$HOME`.
///
/// The tail that does not exist yet — a file about to be created — is
/// appended to the resolved prefix, so a write check decides on the same
/// spelling a later read of that file would. A *dangling* symlink does not
/// exist either and yet a write through it creates its target, so
/// [`canonical_prefix`] follows it and the call is judged where it would land.
///
/// **[`Profile::check`] never returns a non-absolute path on a host whose
/// root is absolute**, and the anchoring condition is the whole of why. A
/// candidate is read in *the root's own spelling family*: a drive-rooted
/// `C:\x` is already rooted under a `\\?\C:\proj` or `C:/proj` root and
/// must not acquire the project's prefix a second time, while under
/// `/Users/…/proj` the same string is one relative filename that happens to
/// contain backslashes, and leaving it unanchored granted a **relative**
/// path — the file a caller then opened was not the file the decision was
/// made on.
fn resolve(path: &Path, root: Option<&Path>, home: Option<&Path>) -> PathBuf {
    let mut expanded = expand_tilde(path, home);
    if let Some(root) = root
        && !expanded.is_absolute()
        && !(windows_rooted(&expanded) && windows_rooted(root))
    {
        expanded = root.join(expanded);
    }
    resolve_components(&expanded, 0)
}

/// [`resolve`]'s component walk over an anchored path; `links` counts the
/// dangling links followed so far.
fn resolve_components(expanded: &Path, links: usize) -> PathBuf {
    let mut out = PathBuf::new();
    for component in expanded.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out = canonical_prefix(&out, links);
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    canonical_prefix(&out, links)
}

/// How many dangling links one resolution follows: the kernel's own `ELOOP`
/// bound, past which the write fails there too.
const DANGLING_LINK_LIMIT: usize = 40;

/// `path` with the longest prefix of it that exists replaced by its canonical
/// form, and the components that do not exist appended as written — unless
/// one of them is a dangling symlink, which is followed: `.sterna/scratch/link
/// -> ../config.toml` with no `config.toml` yet would otherwise be judged as
/// a scratch file and create the host's configuration.
fn canonical_prefix(path: &Path, links: usize) -> PathBuf {
    let mut tail = Vec::new();
    let mut prefix = path.to_path_buf();
    while !prefix.exists() {
        if links < DANGLING_LINK_LIMIT
            && let Ok(target) = std::fs::read_link(&prefix)
        {
            let mut followed = prefix
                .parent()
                .map_or_else(|| target.clone(), |parent| parent.join(&target));
            for name in tail.iter().rev() {
                followed.push(name);
            }
            return resolve_components(&followed, links + 1);
        }
        let Some(name) = prefix.file_name().map(|name| name.to_os_string()) else {
            break;
        };
        tail.push(name);
        if !prefix.pop() {
            break;
        }
    }
    let mut resolved = std::fs::canonicalize(&prefix).unwrap_or(prefix);
    for name in tail.iter().rev() {
        resolved.push(name);
    }
    resolved
}

fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix('~') else {
        return path.to_path_buf();
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return path.to_path_buf();
    }
    let Some(home) = home.map(PathBuf::from).or_else(home_dir) else {
        return path.to_path_buf();
    };
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        home
    } else {
        home.join(rest)
    }
}

/// Every part of a command line a shell would run as a command of its own:
/// the sequence and pipeline operators (`;`, `&&`, `||`, `|`, a background
/// `&` and a newline), plus the contents of a command substitution (`$(…)`,
/// backticks), which is a command line in its own right — but **not** a `&`
/// that is part of a redirect operator (`2>&1`, `>&2`, `<&0`, `&>file`,
/// `&>>file`), because that `&` never starts a new command.
///
/// Deliberately not a shell parser: quoting is not tracked, so a literal `;`
/// inside quotes splits too. That asks about more parts than a shell would
/// run, which is the refusing direction, and it is why this can be a dozen
/// lines rather than a grammar.
pub(super) fn command_segments(command_line: &str) -> Vec<String> {
    let chars: Vec<char> = command_line.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '$' if chars.get(index + 1) == Some(&'(') => {
                let (inner, next) = balanced(&chars, index + 2, Some('('), ')');
                out.extend(command_segments(&inner));
                index = next;
            }
            '`' => {
                let (inner, next) = balanced(&chars, index + 1, None, '`');
                out.extend(command_segments(&inner));
                index = next;
            }
            '&' if is_redirect_ampersand(&chars, index) => {
                current.push('&');
                index += 1;
            }
            ';' | '\n' | '&' | '|' => {
                flush_segment(&mut out, &mut current);
                index += 1;
            }
            other => {
                current.push(other);
                index += 1;
            }
        }
    }
    flush_segment(&mut out, &mut current);
    out
}

/// Whether the `&` at `index` is part of a redirect operator rather than a
/// background or `&&` operator: immediately after a `>` or `<` (`2>&1`,
/// `<&0`), or immediately before a `>` (`&>file`, `&>>file`). A leading
/// file-descriptor digit needs no separate check here — it was already an
/// ordinary character pushed onto the current segment before this `&` was
/// reached.
fn is_redirect_ampersand(chars: &[char], index: usize) -> bool {
    let prev = index.checked_sub(1).and_then(|i| chars.get(i));
    let next = chars.get(index + 1);
    matches!(prev, Some('>') | Some('<')) || matches!(next, Some('>'))
}

/// Whether `word` is entirely one redirect operator with its operand
/// attached — `N>file`, `N>&M`, `<file`, `>file`, `>>file`, `&>file`,
/// `&>>file` — the same forms [`command_segments`] no longer splits on.
fn is_redirect_word(word: &str) -> bool {
    if let Some(rest) = word.strip_prefix("&>>").or_else(|| word.strip_prefix("&>")) {
        return !rest.is_empty();
    }
    let digits_end = word
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(word.len());
    let rest = &word[digits_end..];
    let operand = rest
        .strip_prefix(">>")
        .or_else(|| rest.strip_prefix('>'))
        .or_else(|| rest.strip_prefix('<'));
    matches!(operand, Some(operand) if !operand.is_empty())
}

/// `segment` with every leading redirect word removed, so matching begins at
/// the command: `2>&1 cargo test` becomes `cargo test`, while `1 cargo test`
/// — a literal word, not an operator — is returned unchanged.
pub(super) fn skip_leading_redirects(segment: &str) -> &str {
    let mut rest = segment.trim_start();
    loop {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        if word.is_empty() || !is_redirect_word(word) {
            return rest;
        }
        rest = rest[word_end..].trim_start();
    }
}

/// The first shell word where its spelling is already the executable name.
/// Substitutions and quoting stay opaque and therefore retain the OS-level
/// refusal described in §4.6 instead of being guessed into authority.
fn literal_executable(command: &str) -> Option<&str> {
    let word = command.split_whitespace().next()?;
    if word.chars().any(|c| {
        matches!(
            c,
            '$' | '`' | '\'' | '"' | '\\' | '*' | '?' | '[' | ']' | '{' | '}' | '(' | ')'
        )
    }) {
        return None;
    }
    Some(word)
}

/// The text up to the `close` that balances the one already consumed, and the
/// index just past it. `open` is `None` where the delimiter cannot nest.
fn balanced(chars: &[char], start: usize, open: Option<char>, close: char) -> (String, usize) {
    let mut depth = 1usize;
    let mut inner = String::new();
    let mut index = start;
    while index < chars.len() {
        let c = chars[index];
        if c == close {
            depth -= 1;
            if depth == 0 {
                return (inner, index + 1);
            }
        } else if Some(c) == open {
            depth += 1;
        }
        inner.push(c);
        index += 1;
    }
    (inner, index)
}

fn flush_segment(out: &mut Vec<String>, current: &mut String) {
    let segment = current.trim().to_string();
    current.clear();
    if !segment.is_empty() {
        out.push(segment);
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// One spelling of one path, and the only form this module ever compares two
/// paths in.
///
/// **Two spellings of one path are how a containment check comes to disagree
/// with itself** (§2), and on Windows one file has three: `fs::canonicalize`
/// returns the *verbatim* `\\?\C:\…`, an environment variable or a settings
/// pattern returns the ordinary `C:\…`, and either may carry an 8.3 short
/// name such as `RUNNER~1`. This function decides the first two — `\` folded
/// to `/`, a device-namespace prefix reduced to the ordinary spelling it
/// stands for, the drive letter upper-cased — and [`canonical_prefix`]
/// decides the third, because only the filesystem that issued a short name
/// can say what it is short for.
///
/// The fold is unconditional and always was: `\` is a separator here on every
/// host, which is what lets one matcher serve all three. The *reduction* is
/// not, and that condition is a containment rule rather than tidiness — see
/// [`reduced_device`].
fn spelling(path: &Path) -> Vec<String> {
    let folded = display(path).replace('\\', "/");
    let text = reduced_device(&folded).unwrap_or(folded);
    let mut parts: Vec<String> = text
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    // The **drive letter**, and not the component it begins. `C:foo` and
    // `C:FOO` are two files on a case-sensitive filesystem, and folding the
    // whole component made an `allow` match case-insensitively there —
    // against `match_segment`'s rule that an `allow` never folds, whose
    // reason is that folding lets one spelling reach a path its author never
    // wrote. Only the first character is touched; `is_drive_prefixed`
    // guarantees it is ASCII, so byte 1 is a character boundary.
    if let Some(first) = parts.first_mut()
        && is_drive_prefixed(first)
    {
        first[..1].make_ascii_uppercase();
    }
    parts
}

/// `folded` without its Windows device-namespace prefix, or `None` when it
/// carries none this module can reduce — where "can reduce" means `//?/` or
/// `//./` followed by a **rooted drive** (`C:/…`) or the `UNC/` marker.
///
/// Both prefixes reach the same object manager and name the same file:
/// `\\.\C:\proj\a.rs`, `\\?\C:\proj\a.rs` and `C:\proj\a.rs` are one path
/// spelled three ways, and reducing only the second made `\\.\C:\Windows\
/// System32\config\SAM` a component list no never-rule could meet — a §4
/// escape rather than a cosmetic gap.
///
/// **The drive must be followed by a separator**, and that is why this is not
/// `is_drive_prefixed` alone: `\\.\C:\` is the volume's root *directory*
/// while `\\.\C:` is the volume itself, opened for raw sector reads.
/// [`device_refusal`] refuses the second rather than reducing it to `C:`.
///
/// The condition is also the isolation half of the reduction. `//?/` is an
/// unusual but perfectly legal absolute path on Unix, so an unconditional
/// strip would reduce `//?/proj/a.rs` to the *relative* `proj/a.rs`, which
/// [`resolve`] then anchors inside the project root. Requiring a rooted drive
/// or the `UNC/` marker is what keeps the Windows repair from widening
/// containment on every other platform — the same test, for the same reason,
/// as `crates/glasshouse/src/commands/context_firewall.rs`.
///
/// **On Unix this reduction is reachable only from a backslash-spelled
/// candidate.** `Path::components` collapses `//` before [`spelling`] sees
/// it, so a `/`-spelled `//?/…` argument never carries the prefix this
/// function tests for by the time it is asked — which means
/// `a_verbatim_and_a_plain_spelling_of_one_path_decide_identically`
/// exercises the `\\?\` arm on this host and never the `//?/` one. It is
/// not cross-platform cover for that arm.
fn reduced_device(folded: &str) -> Option<String> {
    let rest = folded
        .strip_prefix("//?/")
        .or_else(|| folded.strip_prefix("//./"))?;
    if is_drive_prefixed(rest) && rest.as_bytes().get(2) == Some(&b'/') {
        return Some(rest.to_string());
    }
    // `\\?\UNC\srv\share` is the verbatim way of writing `\\srv\share`: the
    // marker stands in for the second leading separator.
    let marker = rest.get(..4)?;
    marker
        .eq_ignore_ascii_case("unc/")
        .then(|| format!("//{}", &rest[4..]))
}

/// The refusal a Windows device-namespace path earns when [`reduced_device`]
/// cannot reduce it to an ordinary rooted spelling, and `None` for every
/// other path.
///
/// **This is the fail-closed half of §1.4, and it is a refusal rather than a
/// repair because no lexical rule can turn one of these into a drive path.**
/// `\\?\GLOBALROOT\Device\HarddiskVolume3\Windows\…`,
/// `\\.\PhysicalDrive0` and `\\?\Volume{…}\…` all name real objects that a
/// grant was never written about; compared as ordinary components they meet
/// no never-rule and no `deny`, and a broad `allow` then covered them. A cage
/// that cannot prove a path is inside it denies.
///
/// Reachable on Unix only from a backslash-spelled argument, for the reason
/// [`reduced_device`] gives: after [`resolve`] a `/`-spelled path begins with
/// the project root, and a relative one has been joined to it.
fn device_refusal(path: &Path) -> Option<String> {
    let folded = display(path).replace('\\', "/");
    let device = folded.starts_with("//?/") || folded.starts_with("//./");
    (device && reduced_device(&folded).is_none()).then(|| {
        "a Windows device-namespace path is refused rather than compared: it names no \
         ordinary file this profile can place inside or outside a grant \
         (docs/sandbox.md, invariant 4)"
            .to_string()
    })
}

/// One path component as a **refusing** rule reads it: without the trailing
/// dots and spaces Win32 discards before it opens anything.
///
/// `C:\proj\secrets.\token` and `C:\proj\secrets \token` open
/// `C:\proj\secrets\token`, so a `deny` on `secrets/**` that compared the
/// written component let both past — measured, and the same trick worked on
/// `%SystemRoot%\System32\config` and on Glasshouse's own state directory.
/// [`canonical_prefix`] repairs this for a component that already exists,
/// which is why it was invisible until a **write** to a path that does not
/// exist yet asked the question.
///
/// Applied on every host and to both sides of a refusing comparison, which is
/// this module's standing rule for a spelling difference: `/etc/sudoers.` is
/// a different file on Unix and is refused there too, because over-refusing
/// is the direction a never-grantable set errs in. An `allow` never sees
/// this, for the reason [`match_segment`] gives.
fn refusing_form(component: &str) -> &str {
    let trimmed = component.trim_end_matches(['.', ' ']);
    // `..` and `.` trim to nothing. They are not names, and a rule that
    // matched every one of them would be looser rather than tighter.
    if trimmed.is_empty() {
        component
    } else {
        trimmed
    }
}

/// `candidate` with the alternate-data-stream suffix cut off its last
/// component, or `None` when it carries none.
///
/// **An extra form to test, never a replacement for the written one.**
/// `token.env:hidden` and `token.env::$DATA` read the same file object as
/// `token.env`, so a `deny` naming the file must refuse them; but a Unix
/// filename may legally contain a colon, and cutting `2026-09-09T12:00.log`
/// to `2026-09-09T12` would stop a `deny` on `*.log` from matching it. Both
/// spellings are therefore offered to every refusing rule and either one
/// matching refuses.
///
/// The last component only, because that is the only one Windows reads a
/// stream from, and never a lone one, because the first component may be the
/// drive (`C:`).
fn stream_stripped(candidate: &[String]) -> Option<Vec<String>> {
    let (last, head) = candidate
        .split_last()
        .filter(|(_, head)| !head.is_empty())?;
    let base = last.split(':').next().filter(|base| !base.is_empty())?;
    (base.len() != last.len()).then(|| {
        let mut out = head.to_vec();
        out.push(base.to_string());
        out
    })
}

/// Whether `text` begins with a drive letter and a colon.
fn is_drive_prefixed(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(
        (chars.next(), chars.next()),
        (Some(drive), Some(':')) if drive.is_ascii_alphabetic()
    )
}

/// Whether `path` is already rooted in a spelling only Windows produces: a
/// drive letter, a verbatim prefix, or a UNC share.
///
/// Asked on every host, and deliberately. [`is_rooted`] already asks exactly
/// this of a written *pattern* without a `#[cfg]`, and a candidate path that
/// got a different answer is the disagreement this module exists to prevent —
/// a never-rule computed for a Windows spelling could never meet a candidate
/// that had been anchored inside the project instead. On macOS and Linux the
/// question is only ever put to a path that is not already absolute, so
/// nothing beginning `/` reaches it and the only spelling that can match is a
/// literal drive letter, which no path in a Unix project is.
fn windows_rooted(path: &Path) -> bool {
    let text = display(path).replace('\\', "/");
    text.starts_with("//") || is_drive_prefixed(&text)
}

/// A Windows path with an actual root. `C:project` is drive-relative, so it
/// is rejected as ambiguous; `C:/project` and
/// UNC/verbatim paths already identify a location without process cwd.
fn windows_absolute(path: &Path) -> bool {
    let text = display(path).replace('\\', "/");
    text.starts_with("//")
        || (text.as_bytes().get(1) == Some(&b':')
            && text.as_bytes().get(2) == Some(&b'/')
            && text.as_bytes()[0].is_ascii_alphabetic())
}

fn anchor_project_root(
    supplied: &Path,
    current_dir: std::io::Result<PathBuf>,
) -> std::io::Result<PathBuf> {
    if windows_drive_relative(supplied) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a drive-relative Windows project root is ambiguous",
        ));
    }
    if supplied.is_absolute() || windows_absolute(supplied) {
        Ok(supplied.to_path_buf())
    } else {
        current_dir.map(|cwd| cwd.join(supplied))
    }
}

fn windows_drive_relative(path: &Path) -> bool {
    let text = display(path).replace('\\', "/");
    text.as_bytes().get(1) == Some(&b':')
        && text.as_bytes()[0].is_ascii_alphabetic()
        && text.as_bytes().get(2) != Some(&b'/')
}

#[cfg(windows)]
fn invalid_root_sentinel() -> PathBuf {
    PathBuf::from(r"C:\.sterna-invalid-relative-root")
}

#[cfg(not(windows))]
fn invalid_root_sentinel() -> PathBuf {
    PathBuf::from("/.sterna-invalid-relative-root")
}

/// `path` as one string, for the sentence a person reads.
///
/// The verbatim prefix is reduced here too, so a refusal quotes the spelling
/// the settings file would use rather than the one the kernel handed back.
/// Nothing else is touched: a path carrying no verbatim prefix — which is
/// every path on macOS and Linux — is returned exactly as it was, separators
/// included.
fn shown(path: &Path) -> String {
    let text = display(path);
    reduced_device(&text.replace('\\', "/")).unwrap_or(text)
}

/// Whether `candidate` is `prefix` or lies beneath it, in [`spelling`].
fn contains(prefix: &[String], candidate: &[String]) -> bool {
    prefix.len() <= candidate.len() && prefix.iter().zip(candidate).all(|(a, b)| a == b)
}

/// [`contains`], asked the way §4's never-grantable set has to ask it: case
/// is folded and [`refusing_form`] is applied, on every host.
///
/// The reason is [`match_segment`]'s, one layer up. A never-rule is a
/// refusal, and a refusal that a differing case walks past is not one:
/// `%LOCALAPPDATA%\GLASSHOUSE\state.db` reached Glasshouse's own state
/// directory on Windows while `…\glasshouse\state.db` was refused, because
/// only the second spelling of a directory that does not exist yet survives
/// [`canonical_prefix`]. Erring towards refusing more is the direction §4
/// errs in; `contains` itself stays exact, because the two places that call
/// it — the implicit root grant and a never-rule's exemption — are grants.
fn contains_refusing(prefix: &[String], candidate: &[String]) -> bool {
    prefix.len() <= candidate.len()
        && prefix
            .iter()
            .zip(candidate)
            .all(|(a, b)| same_component(refusing_form(a), refusing_form(b)))
}

/// Two components, compared the way a refusing rule compares them: without
/// regard to case, on every host, for [`match_segment`]'s reason.
fn same_component(expected: &str, actual: &str) -> bool {
    expected.eq_ignore_ascii_case(actual) || expected.to_lowercase() == actual.to_lowercase()
}

/// Whether a written pattern names an absolute path, a `~`-rooted one, or a
/// Windows drive — the three that are not resolved against the project root.
pub(super) fn is_rooted(pattern: &str) -> bool {
    pattern.starts_with('/')
        || pattern == "~"
        || pattern.starts_with("~/")
        || pattern
            .as_bytes()
            .get(1)
            .is_some_and(|byte| *byte == b':' && pattern.as_bytes()[0].is_ascii_alphabetic())
}

/// Resolves a written pattern's literal prefix and keeps its glob tail.
///
/// The prefix is resolved so a pattern and a candidate spelled differently —
/// `/tmp/…` against `/private/tmp/…` — cannot disagree; the tail is left
/// alone because a glob names no single path to resolve. A pattern that
/// names no root is anchored at the project root before either step; a
/// pattern whose resolved form then leaves the root is refused by
/// [`register`] rather than compiled, which is what makes it true that a
/// project-relative glob cannot match its way out of the project.
pub(super) fn resolve_pattern(root: &Path, home: Option<&Path>, pattern: &str) -> Vec<String> {
    // Both halves are reduced before they are spliced, and the pattern's own
    // half matters as much as the root's: `?` is a glob metacharacter here, so
    // a verbatim `//?/C:/…` left in either one splits at the `?` and anchors
    // the pattern under a doubled drive prefix that matches no path at all.
    // That is what made every project-relative pattern — `Read(**)`,
    // `Read(src/**)` — register nothing on Windows.
    let normalized = ordinary(&pattern.replace('\\', "/"));
    let anchored = if is_rooted(&normalized) {
        normalized
    } else {
        format!(
            "{}/{normalized}",
            ordinary(&display(root).replace('\\', "/"))
        )
    };
    let mut literal = Vec::new();
    let mut rest = Vec::new();
    for part in anchored.split('/') {
        if rest.is_empty() && !part.contains(['*', '?']) {
            literal.push(part.to_string());
        } else {
            rest.push(part.to_string());
        }
    }
    let literal_path = literal.join("/");
    let mut out = if literal_path.is_empty() {
        Vec::new()
    } else {
        spelling(&resolve(Path::new(&literal_path), Some(root), home))
    };
    out.extend(rest.into_iter().filter(|part| !part.is_empty()));
    out
}

/// `folded` with a Windows device-namespace prefix reduced, and unchanged
/// otherwise — including when it carries one this module refuses to reduce,
/// which [`Profile::check`] then refuses outright.
fn ordinary(folded: &str) -> String {
    reduced_device(folded).unwrap_or_else(|| folded.to_string())
}

/// Splits `Name(argument)` into its parts; a bare `Name` has no argument.
fn split_pattern(pattern: &str) -> (&str, Option<&str>) {
    let pattern = pattern.trim();
    let Some(open) = pattern.find('(') else {
        return (pattern, None);
    };
    if !pattern.ends_with(')') {
        return (pattern, None);
    }
    (
        &pattern[..open],
        Some(&pattern[open + 1..pattern.len() - 1]),
    )
}

/// Whether `glob` matches `candidate` or names one of its ancestors.
///
/// Naming an ancestor is a match because a pattern that names a directory
/// covers its subtree — the `(subpath …)` term §3's seatbelt shape uses, and
/// the "realpath closure of the glob" §2's table names.
pub(super) fn covers(glob: &[String], candidate: &[String], fold: bool) -> bool {
    (0..=candidate.len()).any(|end| match_components(glob, &candidate[..end], fold))
}

fn match_components(glob: &[String], candidate: &[String], fold: bool) -> bool {
    let Some((head, tail)) = glob.split_first() else {
        return candidate.is_empty();
    };
    if head == "**" {
        return (0..=candidate.len()).any(|skip| match_components(tail, &candidate[skip..], fold));
    }
    let Some((first, rest)) = candidate.split_first() else {
        return false;
    };
    match_segment(head, first, fold) && match_components(tail, rest, fold)
}

/// Matches one component, where `*` and `?` do not cross a separator.
///
/// `fold` says the caller is a **refusing** rule, and what that buys is
/// deliberate rather than the host's: **a `deny` pattern matches
/// case-insensitively and ignores the trailing dots and spaces Win32
/// discards, on every platform; an `allow` pattern matches exactly, on every
/// platform.** A case-insensitive filesystem would otherwise let `SECRET.ENV`
/// walk past a `deny` written as `secret.env`, and `secrets.\token` would
/// open `secrets\token` past a `deny` on `secrets/**`; folding `allow`
/// instead would let either trick reach a path its author never spelled.
/// Both errors are made in the refusing direction, and the answer is the same
/// on macOS, Linux and Windows rather than three answers.
pub(super) fn match_segment(pattern: &str, text: &str, fold: bool) -> bool {
    let (pattern, text) = if fold {
        (refusing_form(pattern), refusing_form(text))
    } else {
        (pattern, text)
    };
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    match_chars(&pattern, &text, fold)
}

fn match_chars(pattern: &[char], text: &[char], fold: bool) -> bool {
    let Some((head, tail)) = pattern.split_first() else {
        return text.is_empty();
    };
    match head {
        '*' => (0..=text.len()).any(|skip| match_chars(tail, &text[skip..], fold)),
        '?' => !text.is_empty() && match_chars(tail, &text[1..], fold),
        expected => match text.split_first() {
            Some((actual, rest)) if same(*expected, *actual, fold) => match_chars(tail, rest, fold),
            _ => false,
        },
    }
}

fn same(expected: char, actual: char, fold: bool) -> bool {
    if expected == actual {
        return true;
    }
    fold && expected.to_lowercase().eq(actual.to_lowercase())
}

#[cfg(test)]
mod relative_root_tests {
    use super::*;

    #[test]
    fn unavailable_cwd_does_not_return_the_relative_root_as_a_location() {
        let error = std::io::Error::new(std::io::ErrorKind::NotFound, "deleted cwd");
        assert!(anchor_project_root(Path::new("."), Err(error)).is_err());
    }

    #[test]
    fn an_invalid_root_refuses_implicit_paths_commands_and_mcp() {
        let mut profile = Profile::compile(
            std::env::temp_dir(),
            Some(r#"{"permissions":{"allow":["Read(**)","Bash","mcp__demo__*"]}}"#),
        );
        profile.invalid_root = Some("invalid project root".into());
        assert!(
            profile
                .check("Read", Access::Read, Path::new("file"))
                .is_err()
        );
        assert!(profile.admits_command("pwd").is_err());
        assert!(!profile.admits_mcp_tool("mcp__demo__read"));
    }
}
