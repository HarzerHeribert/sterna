//! What a tool is — the registry's own schema, which
//! `docs/runtime.md` leaves to this sub-phase and
//! decides nothing about.
//!
//! The invariant this module exists for: **a tool's purity is declared at its
//! definition and there is no expression that omits it.** [`Purity`] has no
//! `Default`, [`Tool`] has no `Default` and no builder, and
//! [`Tool::declare`] takes purity as a positional argument, so a declaration
//! that leaves it out is a compile error rather than a silent `false`.
//! `runtime-contract.md` §4 is why it matters: a resumed handle
//! re-materialises by re-running a recorded *pure* call and comparing
//! SHA-256, so a tool wrongly declared pure would silently re-run something
//! with an effect.
//!
//! The second invariant is an absence: **a tool that needs the network is
//! not registered at all** (`sandbox-grants.md` §4.1). It is absent rather
//! than present-and-failing, and [`NEVER_REGISTERED`] names the absences so
//! a test can assert them positively instead of asserting that a list is
//! short.

use std::fmt;

/// Whether re-running a call reproduces its result without changing the
/// world.
///
/// Two variants and no third: `runtime-contract.md` §4 asks one yes/no
/// question of a recorded call, and a "probably" would be answered as `Pure`
/// by the only consumer there is.
///
/// **Deliberately not `Default`, not `Option`, and not a `bool`.** Each of
/// those would give a declaration a way to say nothing and be read as
/// something. The `bool` is the worst of the three, because `false` and
/// `true` are both plausible defaults and the reader of a call site cannot
/// tell which was meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purity {
    /// Re-running reproduces the same bytes and changes nothing. Only a
    /// `Pure` call may be re-run to re-materialise a stale handle.
    Pure,
    /// Running it may change the world. Never re-run on resume.
    Effectful,
}

impl Purity {
    pub fn as_str(self) -> &'static str {
        match self {
            Purity::Pure => "pure",
            Purity::Effectful => "effectful",
        }
    }

    /// Whether a recorded call of this kind may be re-run on resume.
    pub fn may_rematerialise(self) -> bool {
        matches!(self, Purity::Pure)
    }
}

impl fmt::Display for Purity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one argument is, which decides **which** of `sandbox-grants.md` §2's
/// two questions it is asked.
///
/// The kind is the whole of the type system here, and that is the point:
/// §2's own warning is that conflating a filesystem grant with argv
/// admission inverts the model, so an argument declares which question it
/// answers and [`crate::tools::invoke`] has no branch that can ask the other
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// A filesystem path. Goes through `Profile::check` for `Access::Read`,
    /// and the **resolved** path it returns is what reaches the child.
    Path,
    /// A filesystem path the call will **write**. The same check, asked with
    /// `Access::Write`.
    ///
    /// A separate kind rather than a flag, for §2's own reason: the kind is
    /// the whole of the type system here, so a tool that writes cannot be
    /// declared with a read-checked path by forgetting an argument.
    WritePath,
    /// An opaque string handed to the child as one argv element. It is never
    /// parsed, never expanded, and never spliced into a command line, so
    /// there is nothing in it for a shell to interpret.
    Pattern,
    /// Literal file lines. They are joined with `\n` and a final newline by
    /// the invoker, without JavaScript template interpolation or shell
    /// expansion. Only in-process file tools admit this structured value.
    Lines,
    /// An array of opaque strings, each kept as one item: never joined,
    /// parsed or expanded. The multi-hunk `edit` takes its hunks this way
    /// because a hunk spans lines and [`ArgKind::Lines`] refuses an embedded
    /// newline.
    Texts,
    /// A whole command line. Goes through `Profile::admits_command` and
    /// through nothing else — it grants no file access whatsoever (§2).
    CommandLine,
}

/// One declared argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arg {
    name: &'static str,
    kind: ArgKind,
    /// `false` only where [`Argv`] has a stated substitute for the missing
    /// value; there is no argument that is optional and then absent.
    required: bool,
    /// A missing path is replaced by the project root. Kept separate from
    /// `required`: optional scalar arguments are simply omitted.
    root_default: bool,
}

impl Arg {
    pub const fn required(name: &'static str, kind: ArgKind) -> Self {
        Self {
            name,
            kind,
            required: true,
            root_default: false,
        }
    }

    /// An argument the project root stands in for when it is not given. Only
    /// [`ArgKind::Path`] has such a substitute, because the project root is
    /// the one path every profile grants (`sandbox-grants.md` §1.3).
    pub const fn rooted(name: &'static str) -> Self {
        Self {
            name,
            kind: ArgKind::Path,
            required: false,
            root_default: true,
        }
    }

    /// An argument whose absence has no substitute and is passed through as
    /// absent. This is intentionally distinct from [`Arg::rooted`].
    pub const fn optional(name: &'static str, kind: ArgKind) -> Self {
        Self {
            name,
            kind,
            required: false,
            root_default: false,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn kind(&self) -> ArgKind {
        self.kind
    }

    pub fn is_required(&self) -> bool {
        self.required
    }

    pub fn uses_root_default(&self) -> bool {
        self.root_default
    }
}

/// How a tool's **checked** arguments become the child's argv.
///
/// It is part of the declaration rather than a branch in the invoker so that
/// one place says everything about a tool. Every variant places the checked
/// values positionally and quotes nothing: the child is spawned through
/// `execvp`, never through a shell, so there is no string for an argument to
/// escape out of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Argv {
    /// `<exe> -- <path>`.
    ReadPath,
    /// `<exe> -r -n -e <pattern> -- <path>`.
    GrepIn,
    /// [`Argv::SearchIn`] for a `grep` call ripgrep is serving: the same
    /// fixed flags plus the two that keep `grep`'s own contract, which
    /// ripgrep's defaults do not -- a normal hidden file stays searchable,
    /// and git internals stay out of the walk rather than out of the output.
    SearchInAll,
    /// `<exe> --no-config --line-number --no-heading --color=never -e
    /// <pattern> -- <path>`.
    ///
    /// `--no-config` is part of the shape rather than a preference: ripgrep
    /// otherwise reads `RIPGREP_CONFIG_PATH`, and flags this declaration
    /// never named are the one way a call declared [`Purity::Pure`] could
    /// stop reproducing its own bytes.
    SearchIn,
    /// `<exe> --color=never -- <pattern> <path>`.
    FindIn,
    /// `<exe> -- <filter> <path>`.
    JsonFilter,
    /// `<exe> -c <command line>`. The one variant whose argument was
    /// admitted by `Profile::admits_command` rather than by `Profile::check`.
    ShellCommand,
    /// **Not an argv, and not a child.** The call is performed inside this
    /// process, so no binary is resolved, nothing is exec'd and no shell
    /// exists to quote for.
    ///
    /// The invariant this variant carries: **a tool is either a program to
    /// exec or work sterna does itself, and [`Tool::executable`] says which by
    /// returning `None`.** There is no third state and no tool that spawns
    /// without naming its binary. Reserved for work whose argument is
    /// arbitrary bytes — a file's contents cannot cross an argv or a heredoc
    /// intact, and the standard tools take content on stdin, which a confined
    /// child is not given.
    InProcess,
}

/// One tool: its name, its arguments, the executable it runs, how the two
/// become an argv, and its declared purity.
///
/// Every field is private and there is no constructor but [`Tool::declare`],
/// which takes all five. That is the mechanism behind this module's first
/// invariant — see the module header — and it is structural rather than a
/// promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    name: &'static str,
    executable: Option<&'static str>,
    args: &'static [Arg],
    argv: Argv,
    purity: Purity,
}

impl Tool {
    /// Declares one tool. `purity` is positional and has no default:
    ///
    /// ```
    /// use sterna::tools::registry::{Argv, Purity, Tool};
    /// let tool = Tool::declare("read", "cat", &[], Argv::ReadPath, Purity::Pure);
    /// assert_eq!(tool.purity(), Purity::Pure);
    /// ```
    ///
    /// Omitting it does not compile, which is the answer to "what happens to
    /// a tool that declares no purity":
    ///
    /// ```compile_fail
    /// use sterna::tools::registry::{Argv, Tool};
    /// let _ = Tool::declare("read", "cat", &[], Argv::ReadPath);
    /// ```
    ///
    /// and neither does reaching for a default:
    ///
    /// ```compile_fail
    /// use sterna::tools::registry::Purity;
    /// let _: Purity = Default::default();
    /// ```
    pub const fn declare(
        name: &'static str,
        executable: &'static str,
        args: &'static [Arg],
        argv: Argv,
        purity: Purity,
    ) -> Self {
        Self {
            name,
            executable: Some(executable),
            args,
            argv,
            purity,
        }
    }

    /// Declares a tool sterna performs itself. `purity` stays positional for
    /// the same reason it is in [`Tool::declare`], and the argv is
    /// [`Argv::InProcess`] by construction rather than by choice, so there is
    /// no way to declare an in-process tool that also names a binary.
    pub const fn declare_in_process(
        name: &'static str,
        args: &'static [Arg],
        purity: Purity,
    ) -> Self {
        Self {
            name,
            executable: None,
            args,
            argv: Argv::InProcess,
            purity,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The program this tool runs, as a name to resolve rather than a path,
    /// or `None` for one sterna performs itself.
    ///
    /// [`crate::tools::invoke`] resolves it and grants exec on the resolved
    /// binary — the 61D exec-roots ruling — so a hard-coded path here would
    /// be a second, staler answer to a question that already has one. `None`
    /// is not "resolve later": it is a tool that never becomes a child, and
    /// `spawn_confined` is unreachable for it.
    pub fn executable(&self) -> Option<&'static str> {
        self.executable
    }

    pub fn args(&self) -> &'static [Arg] {
        self.args
    }

    pub fn argv(&self) -> Argv {
        self.argv
    }

    pub fn purity(&self) -> Purity {
        self.purity
    }
}

/// `read({ path })` — `runtime-contract.md` §6's own spelling.
///
/// Pure: `cat` of a path reads bytes and writes nothing.
#[cfg(not(windows))]
const READ: Tool = Tool::declare(
    "read",
    "cat",
    &[Arg::required("path", ArgKind::Path)],
    Argv::ReadPath,
    Purity::Pure,
);

/// `read({ path })` on Windows: the same call, performed by sterna itself.
///
/// **In-process here because no program can do it inside the cage.** Windows
/// ships no `cat`; the one a machine is likely to have is Git for Windows'
/// MSYS2 image, and an AppContainer cannot start an MSYS2 image at all
/// (`sandbox-grants.md` §3, measured 2026-09-09, and §7 names this arm as
/// the successor). The path went through `Profile::check` exactly as the
/// spawning declaration's does; `tools::invoke::search` reads it.
#[cfg(windows)]
const READ: Tool = Tool::declare_in_process(
    "read",
    &[Arg::required("path", ArgKind::Path)],
    Purity::Pure,
);

/// `glob({ pattern, path? })` — names matching `pattern` beneath `path`,
/// which defaults to the project root.
///
/// Pure: sterna walks the checked tree, reads directory entries and writes
/// nothing. Each entry is checked before it is returned or traversed, so an
/// in-root deny cannot leak a name through directory enumeration.
const GLOB: Tool = Tool::declare_in_process(
    "glob",
    &[
        Arg::required("pattern", ArgKind::Pattern),
        Arg::rooted("path"),
    ],
    Purity::Pure,
);

/// `grep({ pattern, path? })` — §6's own spelling, with `glob` narrowed to a
/// path because a second glob language here would be a second answer to
/// `sandbox-grants.md` §2's pattern question.
///
/// Pure: `grep -r` reads and writes nothing.
#[cfg(not(windows))]
const GREP: Tool = Tool::declare(
    "grep",
    "grep",
    &[
        Arg::required("pattern", ArgKind::Pattern),
        Arg::rooted("path"),
    ],
    Argv::GrepIn,
    Purity::Pure,
);

/// `grep({ pattern, path? })` on Windows: the same call, performed by sterna
/// itself, for [`READ`]'s reason — Git for Windows' `grep.exe` is an MSYS2
/// image the cage cannot start. `tools::invoke::search` walks the checked
/// root, asks `Profile::check` about every entry as `glob` does, and prints
/// `grep -r -n -E`'s own `path:line:text` lines — the extended dialect,
/// because a pattern must mean one thing on every host sterna runs on, and
/// `-E` is what [`GREP`] elsewhere and `GREP_BY_RIPGREP` both read.
#[cfg(windows)]
const GREP: Tool = Tool::declare_in_process(
    "grep",
    &[
        Arg::required("pattern", ArgKind::Pattern),
        Arg::rooted("path"),
    ],
    Purity::Pure,
);

// Why `rg`, `fd` and `jq` and not `sed` or `awk`: a tool is `exec`'d
// directly and never through a shell, so there is no `>` to redirect with
// and a tool can only write if its own binary can. These three cannot.
// `sed -i` edits in place, and an `awk` program can `print > "file"` from
// inside its own program text — the argument a model authors — so neither is
// pure and neither belongs here.

/// `rg({ pattern, path? })` — the question [`GREP`] answers, asked of a tree
/// ripgrep already knows how to skip.
///
/// Pure: ripgrep reads and has no flag that writes a file. `--replace`
/// rewrites the output line, not the file, and `--pre` runs a preprocessor
/// per file — the one command-shaped flag it has, and unreachable, because
/// [`Argv::SearchIn`] fixes every flag at compile time, passes the pattern
/// behind `-e` and the path behind `--`. `-e` is the load-bearing half here:
/// a pattern spelled `--pre` is that option's value before it is anything
/// else.
/// The `grep` call, served by ripgrep: `grep`'s name and arguments, `rg`'s
/// binary and speed, and the two flags that keep `grep`'s own contract.
///
/// It is not in [`ALL`] and has no name of its own: a model asks for `grep`
/// and `checked_call` substitutes this when ripgrep is installed, so the
/// arguments were already checked against `GREP` and nothing new is
/// reachable by name.
pub(crate) const GREP_BY_RIPGREP: &Tool = &Tool::declare(
    "grep",
    "rg",
    &[
        Arg::required("pattern", ArgKind::Pattern),
        Arg::rooted("path"),
    ],
    Argv::SearchInAll,
    Purity::Pure,
);

const RIPGREP: Tool = Tool::declare(
    "rg",
    "rg",
    &[
        Arg::required("pattern", ArgKind::Pattern),
        Arg::rooted("path"),
    ],
    Argv::SearchIn,
    Purity::Pure,
);

/// `fd({ pattern, path? })` — names matching a regex beneath `path`.
///
/// Pure, and the one entry here where that is a claim about the argv rather
/// than about the binary: `fd -x` runs a command per match. Reaching it needs
/// a flag position and a third positional element, and [`Argv::FindIn`] emits
/// exactly two positionals, both after `--`, so `-x` arriving as `pattern` is
/// a regex.
const FD: Tool = Tool::declare(
    "fd",
    "fd",
    &[
        Arg::required("pattern", ArgKind::Pattern),
        Arg::rooted("path"),
    ],
    Argv::FindIn,
    Purity::Pure,
);

/// `jq({ filter, path })` — one value out of one JSON file.
///
/// `path` is required and not [`Arg::rooted`]: jq reads a file, and the
/// substitute the root offers is a directory. Pure: jq has no builtin that
/// writes, and the flags that read a second file (`-f`, `--rawfile`) sit in
/// the option position [`Argv::JsonFilter`]'s `--` closes.
const JQ: Tool = Tool::declare(
    "jq",
    "jq",
    &[
        Arg::required("filter", ArgKind::Pattern),
        Arg::required("path", ArgKind::Path),
    ],
    Argv::JsonFilter,
    Purity::Pure,
);

/// `bash({ command })` — a command line, admitted by
/// `Profile::admits_command` and by nothing else.
///
/// **Effectful, and it is the reason the declaration is explicit.** Nothing
/// about `bash -c` says whether the line it runs has an effect; the answer
/// cannot be inferred from the tool, the argument or the result, so it is
/// declared once here and a resumed handle is never re-materialised from it.
///
/// **`bash`, not `sh`, because the sandbox grants exec on one resolved
/// binary.** On macOS `/bin/sh` is a shim that re-execs `/bin/bash`, and a
/// grant on `/bin/sh` alone refuses that second exec (`sandbox_apply.rs`'s
/// sibling test is the rule working). `/bin/bash` is the binary itself.
#[cfg(not(windows))]
const BASH: Tool = Tool::declare(
    "bash",
    "bash",
    &[Arg::required("command", ArgKind::CommandLine)],
    Argv::ShellCommand,
    Purity::Effectful,
);

/// `bash({ command })` on Windows runs `cmd.exe`, the one command
/// interpreter the cage can start.
///
/// Git for Windows' `bash.exe` is an MSYS2 image and exits `0xC0000142`
/// inside every AppContainer (`sandbox-grants.md` §3, measured 2026-09-09),
/// so declaring `bash` here would declare a tool that can never answer.
/// `cmd` is `%ComSpec%`, a native image every AppContainer can load, and
/// `tools::invoke::build_argv` gives it `/d /s /c <command>` so the model's
/// line reaches it verbatim. The name stays `bash` because it is the runtime
/// contract's and the dialects' spelling of "the command tool"; the prompt
/// says which interpreter answers it on this host.
#[cfg(windows)]
const BASH: Tool = Tool::declare(
    "bash",
    "cmd",
    &[Arg::required("command", ArgKind::CommandLine)],
    Argv::ShellCommand,
    Purity::Effectful,
);

/// `write({ path, content })` — the bytes of one file inside the project,
/// replaced whole.
///
/// **In-process, and that is forced rather than chosen.** A file's contents
/// are arbitrary bytes: through `bash` they must survive a heredoc, which a
/// line equal to the delimiter breaks and a `$` or a backtick corrupts, and
/// no standard program takes a file's contents as an argv element — `tee`
/// and `dd` read stdin, which `spawn_confined` gives a child as `/dev/null`.
/// So this one is work sterna does itself, checked by `Profile::check` for
/// `Access::Write` exactly as `read`'s path is checked for `Access::Read`.
///
/// Existing-file changes normally use [`EDIT`], which binds the mutation to
/// the hash of source the model inspected. `write` remains necessary for new
/// files and deliberate whole-file replacement.
///
/// Effectful, obviously: it is the only tool here whose whole purpose is to
/// change the world.
const WRITE: Tool = Tool::declare_in_process(
    "write",
    &[
        Arg::required("path", ArgKind::WritePath),
        Arg::optional("content", ArgKind::Pattern),
        Arg::optional("lines", ArgKind::Lines),
    ],
    Purity::Effectful,
);

/// `context({ path, symbol? })` — a bounded, versioned editing surface around
/// one file or symbol, with ranked callers and tests.
const CONTEXT: Tool = Tool::declare_in_process(
    "context",
    &[
        Arg::required("path", ArgKind::Path),
        Arg::optional("symbol", ArgKind::Pattern),
    ],
    Purity::Pure,
);

/// `edit({ path, expected_sha256?, old, replacement })` — one exact replacement tied
/// to source the model actually inspected, or `edit({ path, olds, replacements })`
/// for several hunks applied as one checked mutation.
const EDIT: Tool = Tool::declare_in_process(
    "edit",
    &[
        Arg::required("path", ArgKind::WritePath),
        Arg::optional("expected_sha256", ArgKind::Pattern),
        Arg::optional("old", ArgKind::Pattern),
        Arg::optional("oldLines", ArgKind::Lines),
        Arg::optional("replacement", ArgKind::Pattern),
        Arg::optional("replacementLines", ArgKind::Lines),
        Arg::optional("olds", ArgKind::Texts),
        Arg::optional("replacements", ArgKind::Texts),
    ],
    Purity::Effectful,
);

/// Every registered tool.
///
/// Small on purpose: each entry is a program that gets exec'd inside a
/// sandbox, so the set is the attack surface and it grows by a package, not
/// by a convenience.
pub const ALL: [Tool; 10] = [
    READ, GLOB, GREP, RIPGREP, FD, JQ, BASH, WRITE, CONTEXT, EDIT,
];

/// Tools that are **absent**, by name, and why.
///
/// `sandbox-grants.md` §4.1 says a network-needing tool is not registered
/// rather than present and failing. An absence is invisible to a test that
/// only reads what is there, so the names are written down and
/// `tests/tools.rs::no_registered_tool_needs_the_network` asserts that none
/// of them — nor any program that would reach a network — appears in
/// [`ALL`].
pub const NEVER_REGISTERED: [&str; 6] = ["webfetch", "websearch", "fetch", "curl", "wget", "http"];

/// Programs that reach a network, checked against every declared
/// [`Tool::executable`]. The companion to [`NEVER_REGISTERED`]: a tool named
/// innocuously that shells out to `curl` is the shape a name-only list
/// misses.
pub const NETWORK_PROGRAMS: [&str; 8] = [
    "curl", "wget", "nc", "netcat", "ssh", "scp", "ftp", "telnet",
];

/// The declaration for `name`, or `None`. An unknown name is not an error
/// here — [`crate::tools::invoke`] turns it into a refusal, which is a value
/// (`sandbox-grants.md` §1.4).
pub fn lookup(name: &str) -> Option<&'static Tool> {
    ALL.iter().find(|tool| tool.name == name)
}

/// Every registered name, for a caller listing what exists.
pub fn names() -> Vec<&'static str> {
    ALL.iter().map(|tool| tool.name).collect()
}

/// MCP names are encoded component by component; punctuation and underscores
/// cannot collide with namespace separators or each other.
pub fn mcp_name(server: &str, tool: &str) -> String {
    fn encode(value: &str) -> String {
        let mut out = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() {
                out.push(char::from(byte));
            } else {
                out.push_str(&format!("_{byte:02x}"));
            }
        }
        out
    }
    format!("mcp__{}__{}", encode(server), encode(tool))
}

/// Apply the deliberate network-tool absence to raw MCP tool names too.
pub fn mcp_tool_is_absent(name: &str) -> bool {
    let normalized: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    NEVER_REGISTERED
        .iter()
        .any(|absent| normalized.eq_ignore_ascii_case(absent))
        || name.split("__").any(|part| {
            NEVER_REGISTERED
                .iter()
                .any(|absent| part.eq_ignore_ascii_case(absent))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_tool_states_a_purity() {
        for tool in ALL {
            assert!(
                matches!(tool.purity(), Purity::Pure | Purity::Effectful),
                "{} declared no purity",
                tool.name()
            );
        }
    }

    /// The effectful set is enumerated rather than counted: a resumed handle
    /// re-materialises by re-running a recorded *pure* call, so a tool that
    /// joins this list without the question being asked would be silently
    /// re-run on resume. Each is named: `bash` runs a command line, `write`
    /// replaces a file, and `edit` atomically changes an existing file.
    #[test]
    fn effectful_tools_are_named() {
        let effectful: Vec<_> = ALL
            .iter()
            .filter(|tool| tool.purity() == Purity::Effectful)
            .map(Tool::name)
            .collect();
        assert_eq!(effectful, vec!["bash", "write", "edit"]);
    }

    /// The companion claim, and the one that matters for the sandbox: a tool
    /// either names a binary to exec or is performed in this process, and
    /// four filesystem-object tools are the second — six on Windows, where
    /// `read` and `grep` join them because the cage cannot start the MSYS2
    /// images that would otherwise perform them.
    #[test]
    fn in_process_tools_are_named() {
        let in_process: Vec<_> = ALL
            .iter()
            .filter(|tool| tool.executable().is_none())
            .map(Tool::name)
            .collect();
        #[cfg(not(windows))]
        assert_eq!(in_process, vec!["glob", "write", "context", "edit"]);
        #[cfg(windows)]
        assert_eq!(
            in_process,
            vec!["read", "glob", "grep", "write", "context", "edit"]
        );
        for tool in ALL.iter().filter(|tool| tool.executable().is_none()) {
            assert_eq!(
                tool.argv(),
                Argv::InProcess,
                "{} names no binary and is not declared in-process",
                tool.name()
            );
        }
    }

    #[test]
    fn only_a_pure_call_may_rematerialise_a_handle() {
        assert!(Purity::Pure.may_rematerialise());
        assert!(!Purity::Effectful.may_rematerialise());
    }

    #[test]
    fn a_command_line_argument_belongs_to_bash_alone() {
        for tool in ALL {
            let has_command_line = tool
                .args()
                .iter()
                .any(|arg| arg.kind() == ArgKind::CommandLine);
            assert_eq!(
                has_command_line,
                tool.name() == "bash",
                "{} asks the wrong one of the two questions",
                tool.name()
            );
        }
    }

    #[test]
    fn optional_arguments_state_whether_the_root_stands_in() {
        for tool in ALL {
            for arg in tool.args() {
                if arg.uses_root_default() {
                    assert_eq!(arg.kind(), ArgKind::Path);
                    assert!(!arg.is_required());
                }
            }
        }
        let symbol = lookup("context")
            .unwrap()
            .args()
            .iter()
            .find(|arg| arg.name() == "symbol")
            .unwrap();
        assert!(!symbol.is_required());
        assert!(!symbol.uses_root_default());
    }

    #[test]
    fn lookup_answers_none_for_a_name_that_was_never_declared() {
        assert!(lookup("read").is_some());
        assert!(lookup("webfetch").is_none());
        assert!(lookup("Read").is_none());
    }
}
