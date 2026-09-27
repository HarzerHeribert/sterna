//! Recognises a familiar shell command whose meaning sterna can prove, so the
//! stronger capability runs underneath the shape the model already knows.
//!
//! `docs/tools.md` is the specification. Two
//! rules govern everything here:
//!
//! **This module executes nothing.** It maps a command line to a capability
//! and its arguments, or to nothing at all. Both answers continue into the one
//! kernel `tool-abi.md` §1 requires, so lifting cannot become a second
//! executor.
//!
//! **A recognizer has two successful states and no third.** There is no
//! confidence, no ranking and no classifier: either the semantics are proven
//! for the accepted subset, or the command runs exactly as the model wrote it.
//! [`classify`] is deliberately separate — it names a command's family for the
//! screen and for telemetry and can never change what runs.

pub mod words;

/// What kind of work a command does, for presentation and telemetry only.
///
/// Kept out of [`recognize`]'s answer on purpose: a family is a label, and a
/// label that could change execution would be the heuristic layer the
/// specification forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Search,
    Read,
    List,
    RepositoryState,
    Verification,
}

impl Family {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Read => "read",
            Self::List => "list",
            Self::RepositoryState => "repository_state",
            Self::Verification => "verification",
        }
    }
}

/// A deterministic range taken from an exact observation.
///
/// It exists because `head` and `tail` are ranges rather than summaries: the
/// result is `bounded_exact` with the whole still addressable, never a
/// derived preview (`semantic-command-lifting.md`, *File and artifact reads*).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    Head(usize),
    Tail(usize),
}

/// One proven translation: the capability to run instead, and with what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lift {
    /// A `registry::ALL` name — never a new implementation.
    pub capability: &'static str,
    /// Canonical arguments, in the capability's own spelling.
    pub args: Vec<(&'static str, String)>,
    pub family: Family,
    pub projection: Option<Projection>,
}

impl Lift {
    fn new(capability: &'static str, family: Family, args: Vec<(&'static str, String)>) -> Self {
        Self {
            capability,
            args,
            family,
            projection: None,
        }
    }

    fn with(mut self, projection: Projection) -> Self {
        self.projection = Some(projection);
        self
    }
}

/// One command family's recognizer.
type Recognize = fn(&[String]) -> Option<Lift>;

/// The registry. Adding a recognizable command extends this table; it does not
/// add an execution path.
static RECOGNIZERS: &[(&str, Recognize)] = &[
    ("rg", ripgrep),
    ("grep", grep),
    ("fd", fd),
    ("cat", cat),
    ("head", head),
    ("tail", tail),
];

/// The capability this command provably means, or `None`.
///
/// `None` is the ordinary answer and costs nothing: the caller runs the shell
/// command it already had.
#[must_use]
pub fn recognize(command: &str) -> Option<Lift> {
    let words = words::split(command)?;
    let program = program_name(&words[0])?;
    let (_, recognize) = RECOGNIZERS.iter().find(|(name, _)| *name == program)?;
    recognize(&words[1..])
}

/// The family a command belongs to, for the screen and telemetry.
///
/// Never consulted by execution. A command can be classified and not lifted —
/// `cargo test` is verification whether or not any capability replaces it —
/// and that is the normal case for the families sterna has no capability for.
#[must_use]
pub fn classify(command: &str) -> Option<Family> {
    let words = words::split(command)?;
    let program = program_name(&words[0])?;
    let rest: Vec<&str> = words[1..].iter().map(String::as_str).collect();
    match program {
        "rg" | "grep" => Some(Family::Search),
        "cat" | "head" | "tail" => Some(Family::Read),
        "fd" | "find" | "ls" => Some(Family::List),
        "git" => match rest.first() {
            Some(&"status" | &"diff" | &"show" | &"log") => Some(Family::RepositoryState),
            _ => None,
        },
        "cargo" => match rest.first() {
            Some(&"test" | &"check" | &"build" | &"clippy") => Some(Family::Verification),
            _ => None,
        },
        "pytest" => Some(Family::Verification),
        "python" | "python3" => (rest.first() == Some(&"-m") && rest.get(1) == Some(&"pytest"))
            .then_some(Family::Verification),
        "npm" | "pnpm" | "yarn" => (rest.first() == Some(&"test")).then_some(Family::Verification),
        "tsc" => Some(Family::Verification),
        _ => None,
    }
}

/// The program name, refusing a path so `./rg` or `/usr/local/bin/rg` is not
/// silently treated as the `rg` this registry knows.
fn program_name(word: &str) -> Option<&str> {
    (!word.contains('/')).then_some(word)
}

/// Splits accepted flags from positional arguments.
///
/// Returns `None` the moment a flag outside `accepted` appears, which is what
/// makes an unsupported option a fallback rather than an approximation.
fn partition<'a>(args: &'a [String], accepted: &[&str]) -> Option<Vec<&'a str>> {
    let mut positional = Vec::new();
    let mut only_positional = false;
    for arg in args {
        if only_positional {
            positional.push(arg.as_str());
            continue;
        }
        if arg == "--" {
            only_positional = true;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            if !accepted.contains(&arg.as_str()) {
                return None;
            }
            continue;
        }
        positional.push(arg.as_str());
    }
    Some(positional)
}

/// `rg PATTERN [PATH]`.
///
/// Only flags that cannot change which lines match are accepted, and the
/// capability supplies line numbers and plain output itself. Anything that
/// filters, inverts, widens or reshapes the match set — `-i`, `-w`, `-F`,
/// `-v`, `-l`, `-c`, `-t`, `--glob`, context flags — is refused, because the
/// capability has no way to express it and a lift that dropped it would answer
/// a different question.
fn ripgrep(args: &[String]) -> Option<Lift> {
    let mut pattern: Option<&str> = None;
    let mut rest = Vec::new();
    let mut iter = args.iter().peekable();
    let mut only_positional = false;
    while let Some(arg) = iter.next() {
        if only_positional {
            rest.push(arg.as_str());
            continue;
        }
        match arg.as_str() {
            "--" => only_positional = true,
            "-e" | "--regexp" => pattern = Some(iter.next()?.as_str()),
            "-n" | "--line-number" | "--no-heading" | "--color=never" => {}
            "--color" => {
                if iter.next().map(String::as_str) != Some("never") {
                    return None;
                }
            }
            other if other.starts_with('-') && other.len() > 1 => return None,
            other => rest.push(other),
        }
    }
    let mut rest = rest.into_iter();
    let pattern = match pattern {
        Some(pattern) => pattern,
        None => rest.next()?,
    };
    let path = rest.next();
    if rest.next().is_some() {
        // The capability searches one root; two would be a different search.
        return None;
    }
    let mut lift = vec![("pattern", pattern.to_string())];
    if let Some(path) = path {
        lift.push(("path", path.to_string()));
    }
    Some(Lift::new("rg", Family::Search, lift))
}

/// `grep -r PATTERN PATH`.
///
/// Recursive only: the capability always passes `-r`, and a non-recursive
/// `grep` over a directory fails rather than searching it, so the two are not
/// the same request.
fn grep(args: &[String]) -> Option<Lift> {
    let mut recursive = false;
    let mut rest = Vec::new();
    let mut only_positional = false;
    for arg in args {
        if only_positional {
            rest.push(arg.as_str());
            continue;
        }
        match arg.as_str() {
            "--" => only_positional = true,
            "-r" | "-R" | "--recursive" => recursive = true,
            "-n" | "--line-number" => {}
            other if other.starts_with("--") => return None,
            other if other.starts_with('-') && other.len() > 1 => {
                // A combined short cluster is accepted only if every letter is.
                for letter in other[1..].chars() {
                    match letter {
                        'r' | 'R' => recursive = true,
                        'n' => {}
                        _ => return None,
                    }
                }
            }
            other => rest.push(other),
        }
    }
    if !recursive {
        return None;
    }
    let mut rest = rest.into_iter();
    let pattern = rest.next()?;
    let path = rest.next();
    if rest.next().is_some() {
        return None;
    }
    let mut lift = vec![("pattern", pattern.to_string())];
    if let Some(path) = path {
        lift.push(("path", path.to_string()));
    }
    Some(Lift::new("grep", Family::Search, lift))
}

/// `fd PATTERN [PATH]`, with no options at all.
fn fd(args: &[String]) -> Option<Lift> {
    let positional = partition(args, &[])?;
    let mut positional = positional.into_iter();
    let pattern = positional.next()?;
    let path = positional.next();
    if positional.next().is_some() {
        return None;
    }
    let mut lift = vec![("pattern", pattern.to_string())];
    if let Some(path) = path {
        lift.push(("path", path.to_string()));
    }
    Some(Lift::new("fd", Family::List, lift))
}

/// `cat PATH` — one file, because the capability reads one.
fn cat(args: &[String]) -> Option<Lift> {
    let positional = partition(args, &[])?;
    let [path] = positional[..] else {
        return None;
    };
    Some(Lift::new(
        "read",
        Family::Read,
        vec![("path", path.to_string())],
    ))
}

fn head(args: &[String]) -> Option<Lift> {
    ranged(args, Projection::Head)
}

fn tail(args: &[String]) -> Option<Lift> {
    ranged(args, Projection::Tail)
}

/// `head -n N PATH` / `tail -n N PATH`, and the `-N` spelling.
///
/// The lift is an exact read plus a deterministic range, so the result is an
/// exact subset with the whole still reachable — never a summary.
fn ranged(args: &[String], projection: fn(usize) -> Projection) -> Option<Lift> {
    let mut count: Option<usize> = None;
    let mut rest = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-n" | "--lines" => count = Some(iter.next()?.parse().ok()?),
            other if other.starts_with("-n") => count = Some(other[2..].parse().ok()?),
            other if other.starts_with("--lines=") => {
                count = Some(other["--lines=".len()..].parse().ok()?);
            }
            // `head -20 file` is the historical spelling and means the same.
            other
                if other.starts_with('-')
                    && other.len() > 1
                    && other[1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                count = Some(other[1..].parse().ok()?);
            }
            other if other.starts_with('-') && other.len() > 1 => return None,
            other => rest.push(other),
        }
    }
    let [path] = rest[..] else {
        return None;
    };
    // The default is 10 lines for both, and it is the tool's own default
    // rather than a number chosen here.
    let count = count.unwrap_or(10);
    Some(Lift::new("read", Family::Read, vec![("path", path.to_string())]).with(projection(count)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lift(command: &str) -> Lift {
        recognize(command).unwrap_or_else(|| panic!("`{command}` should be recognised"))
    }

    fn arg<'a>(lift: &'a Lift, name: &str) -> Option<&'a str> {
        lift.args
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_plain_search_becomes_the_search_capability() {
        let lifted = lift("rg SessionManager crates/");
        assert_eq!(lifted.capability, "rg");
        assert_eq!(lifted.family, Family::Search);
        assert_eq!(arg(&lifted, "pattern"), Some("SessionManager"));
        assert_eq!(arg(&lifted, "path"), Some("crates/"));
    }

    #[test]
    fn a_search_without_a_path_keeps_the_capabilitys_own_default() {
        let lifted = lift("rg TODO");
        assert_eq!(arg(&lifted, "pattern"), Some("TODO"));
        assert_eq!(arg(&lifted, "path"), None, "the project root stands in");
    }

    #[test]
    fn the_other_search_spelling_reaches_a_search_capability_too() {
        let lifted = lift("grep -rn Session crates/");
        assert_eq!(lifted.capability, "grep");
        assert_eq!(lifted.family, Family::Search);
        assert_eq!(arg(&lifted, "pattern"), Some("Session"));
        assert_eq!(arg(&lifted, "path"), Some("crates/"));
    }

    #[test]
    fn a_non_recursive_grep_is_not_the_same_request() {
        assert!(recognize("grep Session crates/").is_none());
    }

    #[test]
    fn a_flag_that_changes_which_lines_match_is_never_dropped() {
        for command in [
            "rg -i Session crates/",
            "rg -w Session crates/",
            "rg -F Session crates/",
            "rg -v Session crates/",
            "rg -l Session crates/",
            "rg -c Session crates/",
            "rg -t rust Session crates/",
            "rg --glob '*.rs' Session crates/",
            "rg -A 3 Session crates/",
            "grep -ri Session crates/",
            "grep -rl Session crates/",
        ] {
            assert!(
                recognize(command).is_none(),
                "`{command}` changes the match set and must fall back"
            );
        }
    }

    #[test]
    fn flags_that_cannot_change_the_match_set_are_accepted() {
        assert_eq!(arg(&lift("rg -n Session src"), "pattern"), Some("Session"));
        assert_eq!(
            arg(&lift("rg --no-heading --color=never Session src"), "path"),
            Some("src")
        );
        assert_eq!(
            arg(&lift("rg --color never -e Session src"), "pattern"),
            Some("Session")
        );
    }

    #[test]
    fn a_second_search_root_is_a_different_search() {
        assert!(recognize("rg Session src crates").is_none());
    }

    #[test]
    fn a_read_keeps_its_exact_range_semantics() {
        let whole = lift("cat src/config.rs");
        assert_eq!(whole.capability, "read");
        assert_eq!(whole.projection, None);

        let first = lift("head -n 80 src/config.rs");
        assert_eq!(first.capability, "read");
        assert_eq!(first.projection, Some(Projection::Head(80)));

        let last = lift("tail -n 100 build.log");
        assert_eq!(last.projection, Some(Projection::Tail(100)));
    }

    #[test]
    fn the_historical_and_long_range_spellings_mean_the_same() {
        assert_eq!(
            lift("head -20 f.txt").projection,
            Some(Projection::Head(20))
        );
        assert_eq!(
            lift("head -n20 f.txt").projection,
            Some(Projection::Head(20))
        );
        assert_eq!(
            lift("head --lines=5 f.txt").projection,
            Some(Projection::Head(5))
        );
        assert_eq!(
            lift("head f.txt").projection,
            Some(Projection::Head(10)),
            "the default is the tool's own, not one invented here"
        );
    }

    #[test]
    fn a_multi_file_read_is_not_one_read() {
        assert!(recognize("cat a.rs b.rs").is_none());
        assert!(recognize("head -n 5 a.rs b.rs").is_none());
    }

    #[test]
    fn a_listing_is_recognised_only_in_its_plainest_form() {
        let lifted = lift("fd runtime crates");
        assert_eq!(lifted.capability, "fd");
        assert_eq!(lifted.family, Family::List);
        assert!(
            recognize("fd -e rs runtime crates").is_none(),
            "an extension filter changes the result set"
        );
        assert!(
            recognize("find . -name '*.rs' -delete").is_none(),
            "find hides a programming language in its arguments"
        );
    }

    #[test]
    fn nothing_that_mutates_is_reinterpreted() {
        for command in [
            "rm -rf build",
            "mv a.rs b.rs",
            "cp a.rs b.rs",
            "sed -i 's/a/b/' f.rs",
            "git commit -m wip",
            "npm install",
        ] {
            assert!(
                recognize(command).is_none(),
                "`{command}` must never be lifted"
            );
        }
    }

    #[test]
    fn compound_syntax_never_reaches_a_recognizer() {
        for command in [
            "rg foo | head -n 5",
            "cat a.rs > b.rs",
            "cat a.rs && rg foo",
            "rg $PATTERN src",
        ] {
            assert!(recognize(command).is_none(), "`{command}`");
        }
    }

    #[test]
    fn a_program_reached_by_path_is_not_the_one_this_registry_knows() {
        assert!(recognize("./rg foo src").is_none());
        assert!(recognize("/usr/local/bin/rg foo src").is_none());
    }

    #[test]
    fn an_unknown_command_is_simply_not_recognised() {
        assert!(recognize("cargo test -p sterna").is_none());
        assert!(recognize("git status --short").is_none());
        assert!(recognize("frobnicate --all").is_none());
    }

    /// Classification is a label. It must never imply a translation, so the
    /// families sterna has no capability for are classified and not lifted.
    #[test]
    fn a_family_can_be_named_without_being_lifted() {
        assert_eq!(
            classify("cargo test -p sterna abi"),
            Some(Family::Verification)
        );
        assert_eq!(classify("pytest tests/auth -q"), Some(Family::Verification));
        assert_eq!(
            classify("python3 -m pytest tests"),
            Some(Family::Verification)
        );
        assert_eq!(classify("npm test"), Some(Family::Verification));
        assert_eq!(
            classify("git diff -- src/runtime.rs"),
            Some(Family::RepositoryState)
        );
        assert_eq!(
            classify("git status --short"),
            Some(Family::RepositoryState)
        );

        for command in ["cargo test -p sterna abi", "git status --short"] {
            assert!(
                recognize(command).is_none(),
                "`{command}` is classified, not translated"
            );
        }
    }

    #[test]
    fn classification_refuses_the_same_compound_syntax_recognition_does() {
        assert_eq!(classify("cargo test && ./fix.sh"), None);
        assert_eq!(classify("git diff | less"), None);
        assert_eq!(
            classify("git push"),
            None,
            "a mutation is not repository state"
        );
        assert_eq!(classify("cargo run"), None);
    }
}
