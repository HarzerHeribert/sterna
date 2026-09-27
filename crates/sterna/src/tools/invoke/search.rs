//! `read` and `grep`, performed inside sterna.
//!
//! **The invariant: where the registry declares them in-process, `read`
//! answers what `cat -- <path>` answers and `grep` what
//! `grep -r -n -E -e <pattern> -- <path>` answers — the extended dialect the
//! declaration promises and the spawned `grep` is given, so one pattern means
//! one thing on every host — and every file either one
//! opens went through `Profile::check` first** — `read`'s path in
//! `check_arguments`, and each entry of a `grep` walk here, which is the same
//! per-entry gate `glob_paths` passes, so an in-root `deny` prunes a search
//! exactly as it prunes an enumeration.
//!
//! Why it exists: an AppContainer cannot start an MSYS2 image, and on a
//! Windows machine `cat.exe` and `grep.exe` are Git for Windows' MSYS2 images
//! or nothing at all (`sandbox-grants.md` §3, measured 2026-09-09; §7 names
//! this module as the successor). It is compiled under `test` on every host
//! so the ordinary gate asserts the matcher and the walker, and used only
//! where `tools::registry` declares the two tools in-process.

use std::path::{Path, PathBuf};

use super::{Confinement, ExecGrant, ToolError, ToolResult, is_broad_search};
use crate::sandbox::profile::{Access, Profile};

/// How many directory entries one walk may visit — `glob_paths`'s bound,
/// for the same reason: a symlink cycle or a generated tree must end a call
/// rather than a process.
const MAX_VISITED: usize = 100_000;

fn answer(tool: &str, stdout: String, stderr: String, exit_code: i32) -> ToolResult {
    ToolResult {
        modified: None,
        tool: tool.to_string(),
        stdout,
        stderr,
        exit_code: Some(exit_code),
        grant: ExecGrant {
            binary: PathBuf::new(),
            fell_back_to_roots: false,
        },
        confinement: Confinement::InProcess,
    }
}

fn failure(tool: &str, error: String) -> ToolError {
    ToolError::Spawn {
        tool: tool.to_string(),
        program: PathBuf::from("(in-process)"),
        error,
    }
}

/// `cat -- <path>`: the file's bytes on stdout and exit 0, or `cat`'s own
/// shape of failure — nothing on stdout, one line naming the path and the
/// reason on stderr, exit 1 — which `call_failure` turns into the same
/// `ToolError` a child's exit 1 becomes.
pub(super) fn read_file(tool: &str, path: &Path) -> ToolResult {
    match std::fs::read(path) {
        Ok(bytes) => answer(
            tool,
            String::from_utf8_lossy(&bytes).into_owned(),
            String::new(),
            0,
        ),
        Err(error) => answer(
            tool,
            String::new(),
            format!("{tool}: {}: {error}\n", path.display()),
            1,
        ),
    }
}

/// `grep -r -n -E -e <pattern> -- <root>`.
///
/// Exit 0 when something matched — a `path:line:text` line, or only a binary
/// file's notice on stderr — exit 1 with nothing when nothing did, and exit
/// 2 with the reason on stderr for a pattern this matcher cannot compile or
/// for a directory or file it could not read. An unreadable entry does not
/// end the walk: as `grep -r` does, the matches already found stay on stdout
/// beside the error, and the exit is 2 — the three exits `call_failure`
/// already tells apart for the spawned `grep`. Files are visited in name order,
/// symlinks met during the walk are skipped as `-r` skips them, and a file
/// the profile refuses is never opened.
///
/// **A broad search prunes `.git` and every name in `skipped`, exactly where
/// `checked_call` adds `--exclude-dir=.git` and one `--exclude-dir=` per
/// name.** `skipped` is `broad::ignored_directories`' answer for this same
/// root — the project's own `.gitignore` directory rules, and only the ones
/// that transfer to a basename-wide exclusion without changing meaning — so
/// an in-process search reads what a spawned one reads and no more. Without
/// it the walk read the model downloads and virtual environments the project
/// declares generated, while the spawned form beside it skipped them.
pub(super) fn grep_tree(
    profile: &Profile,
    stopped: &dyn Fn() -> bool,
    tool: &str,
    root: &Path,
    pattern: &str,
    skipped: &[String],
) -> Result<ToolResult, ToolError> {
    let matcher = match Pattern::compile(pattern) {
        Ok(matcher) => matcher,
        Err(reason) => {
            return Ok(answer(
                tool,
                String::new(),
                format!("{tool}: {reason}\n"),
                2,
            ));
        }
    };
    let broad = is_broad_search(profile.root(), root);
    let cancelled = || ToolError::Cancelled {
        tool: tool.to_string(),
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut found = Found::default();
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    let unreadable =
        |stderr: &mut String, found: &mut Found, path: &Path, error: std::io::Error| {
            stderr.push_str(&format!("{tool}: {}: {error}\n", path.display()));
            found.errored = true;
        };
    while let Some(path) = pending.pop() {
        if stopped() {
            return Err(cancelled());
        }
        if !path.is_dir() {
            grep_file(&matcher, tool, &path, &mut stdout, &mut stderr, &mut found);
            continue;
        }
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                unreadable(&mut stderr, &mut found, &path, error);
                continue;
            }
        };
        let mut children = Vec::new();
        for entry in entries {
            visited += 1;
            if visited > MAX_VISITED {
                return Err(failure(
                    tool,
                    format!("grep stopped after {MAX_VISITED} directory entries"),
                ));
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    unreadable(&mut stderr, &mut found, &path, error);
                    continue;
                }
            };
            if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                continue;
            }
            let child = entry.path();
            if broad && child.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            // A `.gitignore` rule ending in `/` names a directory, and only a
            // directory: a file of that name is tracked, so it is read.
            if broad
                && entry.file_type().is_ok_and(|kind| kind.is_dir())
                && child
                    .file_name()
                    .is_some_and(|name| skipped.iter().any(|ignored| *name == **ignored))
            {
                continue;
            }
            let Ok(checked) = profile.check(tool, Access::Read, &child) else {
                continue;
            };
            children.push(checked);
        }
        children.sort();
        pending.extend(children.into_iter().rev());
    }
    let exit_code = if found.errored {
        2
    } else if found.matched {
        0
    } else {
        1
    };
    Ok(answer(tool, stdout, stderr, exit_code))
}

/// What a walk has seen so far, which is what decides `grep`'s exit: an
/// error outranks a match, and a match — a binary file's included — outranks
/// none.
#[derive(Default)]
struct Found {
    matched: bool,
    errored: bool,
}

/// One file, as `grep -n` prints it: `path:line:text` per matching line,
/// the text as it is on disk (a `\r` before the newline included), and for
/// a file holding a NUL byte only the notice GNU grep 3.5+ writes to stderr.
fn grep_file(
    matcher: &Pattern,
    tool: &str,
    path: &Path,
    stdout: &mut String,
    stderr: &mut String,
    found: &mut Found,
) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            stderr.push_str(&format!("{tool}: {}: {error}\n", path.display()));
            found.errored = true;
            return;
        }
    };
    let binary = bytes.contains(&0);
    let text = String::from_utf8_lossy(&bytes);
    let shown = path.display();
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        if !matcher.matches(line) {
            continue;
        }
        found.matched = true;
        if binary {
            stderr.push_str(&format!("{tool}: {shown}: binary file matches\n"));
            return;
        }
        stdout.push_str(&format!("{shown}:{}:{line}\n", index + 1));
    }
}

// --- the pattern ---------------------------------------------------------

/// One compiled `grep` pattern: POSIX **extended** regular expression syntax
/// — what `grep -E` accepts, which is what the `grep` declaration promises
/// and what `rg` serves where it is installed — with the GNU extensions a
/// model actually writes — `\w`, `\W`, `\s`, `\S`, `\b`, `\B`, `\<`, `\>` —
/// and no back-references, which ERE does not have.
///
/// So `|` alternates, `(` groups, and `+`, `?` and `{m,n}` repeat, each
/// unescaped; a backslash before any of them is that character itself.
///
/// A construct this does not implement is refused at compile time with the
/// exit 2 `grep` itself uses for a bad pattern, never matched approximately:
/// a search that silently found nothing would be the default-that-looks-like-
/// success shape this project keeps finding.
#[derive(Debug)]
struct Pattern {
    /// A Thompson-construction program, run by lockstep simulation: every
    /// line costs `chars × instructions`, whatever the pattern — no
    /// backtracking, so no exponential pattern and no recursion that grows
    /// with the line, which a backtracking matcher measurably had on a
    /// 200,000-character line.
    program: Vec<Inst>,
    /// Bracket expressions, by index from [`Inst::Set`], so a counted
    /// repetition copies an index and not a vector.
    sets: Vec<(Vec<SetItem>, bool)>,
}

/// What the parser produces: an alternation of concatenations of pieces.
#[derive(Debug)]
struct Ast {
    alternatives: Vec<Vec<Piece>>,
}

#[derive(Debug)]
struct Piece {
    atom: Atom,
    min: u32,
    max: Option<u32>,
    /// Whether a quantifier has already been applied: the first one
    /// replaces the bounds, a second one (`a**`, `a{2}*`) wraps the
    /// quantified piece in a group and repeats that.
    quantified: bool,
}

#[derive(Debug)]
enum Atom {
    Char(char),
    Any,
    Set { items: Vec<SetItem>, negated: bool },
    Group(Ast),
    LineStart,
    LineEnd,
    WordStart,
    WordEnd,
    WordBoundary,
    NotWordBoundary,
    Word,
    NotWord,
    Space,
    NotSpace,
}

#[derive(Debug, Clone)]
enum SetItem {
    Char(char),
    Range(char, char),
    Class(Class),
}

#[derive(Debug, Clone, Copy)]
enum Class {
    Alpha,
    Digit,
    Alnum,
    Space,
    Upper,
    Lower,
    Punct,
    Xdigit,
    Blank,
    Cntrl,
    Print,
    Graph,
}

impl Class {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "alpha" => Class::Alpha,
            "digit" => Class::Digit,
            "alnum" => Class::Alnum,
            "space" => Class::Space,
            "upper" => Class::Upper,
            "lower" => Class::Lower,
            "punct" => Class::Punct,
            "xdigit" => Class::Xdigit,
            "blank" => Class::Blank,
            "cntrl" => Class::Cntrl,
            "print" => Class::Print,
            "graph" => Class::Graph,
            _ => return None,
        })
    }

    fn holds(self, c: char) -> bool {
        match self {
            Class::Alpha => c.is_alphabetic(),
            Class::Digit => c.is_ascii_digit(),
            Class::Alnum => c.is_alphanumeric(),
            Class::Space => c.is_whitespace(),
            Class::Upper => c.is_uppercase(),
            Class::Lower => c.is_lowercase(),
            Class::Punct => c.is_ascii_punctuation(),
            Class::Xdigit => c.is_ascii_hexdigit(),
            Class::Blank => c == ' ' || c == '\t',
            Class::Cntrl => c.is_control(),
            Class::Print => !c.is_control(),
            Class::Graph => !c.is_control() && !c.is_whitespace(),
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Pattern {
    fn compile(pattern: &str) -> Result<Self, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut parser = Parser {
            chars: &chars,
            at: 0,
        };
        let ast = parser.alternation(false)?;
        if parser.at < chars.len() {
            return Err("Unmatched ) or \\)".to_string());
        }
        let mut compiler = Compiler {
            program: Vec::new(),
            sets: Vec::new(),
        };
        compiler.ast(&ast)?;
        compiler.emit(Inst::Match)?;
        Ok(Pattern {
            program: compiler.program,
            sets: compiler.sets,
        })
    }

    /// Whether the pattern matches anywhere in `line`: a thread starts at
    /// every position, all threads step together over each character, and
    /// the first to reach [`Inst::Match`] answers.
    fn matches(&self, line: &str) -> bool {
        let chars: Vec<char> = line.chars().collect();
        let mut current = Threads::new(self.program.len());
        let mut next = Threads::new(self.program.len());
        let mut stack = Vec::new();
        for at in 0..=chars.len() {
            if self.add(&mut current, &mut stack, 0, &chars, at) {
                return true;
            }
            let Some(&c) = chars.get(at) else { break };
            next.clear();
            for index in 0..current.list.len() {
                let pc = current.list[index];
                let steps = match self.program[pc] {
                    Inst::Char(expected) => c == expected,
                    Inst::Any => true,
                    Inst::Set(set) => {
                        let (items, negated) = &self.sets[set];
                        set_holds(items, c) != *negated
                    }
                    Inst::Word => is_word(c),
                    Inst::NotWord => !is_word(c),
                    Inst::Space => c.is_whitespace(),
                    Inst::NotSpace => !c.is_whitespace(),
                    Inst::Assert(_) | Inst::Split(..) | Inst::Jmp(_) | Inst::Match => false,
                };
                if steps && self.add(&mut next, &mut stack, pc + 1, &chars, at + 1) {
                    return true;
                }
            }
            std::mem::swap(&mut current, &mut next);
        }
        false
    }

    /// Adds the thread at `pc`, following jumps, splits and the assertions
    /// that hold at `at`, and reports whether one of them is a match.
    /// Iterative on an explicit stack, so a chain of splits as long as the
    /// program cannot grow the call stack.
    fn add(
        &self,
        threads: &mut Threads,
        stack: &mut Vec<usize>,
        pc: usize,
        chars: &[char],
        at: usize,
    ) -> bool {
        let mut matched = false;
        stack.push(pc);
        while let Some(pc) = stack.pop() {
            if !threads.insert(pc) {
                continue;
            }
            match self.program[pc] {
                Inst::Jmp(target) => stack.push(target),
                Inst::Split(first, second) => {
                    stack.push(second);
                    stack.push(first);
                }
                Inst::Assert(assertion) => {
                    if assertion.holds(chars, at) {
                        stack.push(pc + 1);
                    }
                }
                Inst::Match => matched = true,
                _ => {}
            }
        }
        matched
    }
}

struct Parser<'a> {
    chars: &'a [char],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    /// `branch (| branch)*`, ending at the end of the pattern or, inside a
    /// group, at the `)` that closes it.
    fn alternation(&mut self, in_group: bool) -> Result<Ast, String> {
        let mut alternatives = vec![self.branch()?];
        loop {
            match self.peek(0) {
                Some('|') => {
                    self.at += 1;
                    alternatives.push(self.branch()?);
                }
                Some(')') => {
                    if !in_group {
                        return Err("Unmatched ) or \\)".to_string());
                    }
                    self.at += 1;
                    return Ok(Ast { alternatives });
                }
                None => {
                    if in_group {
                        return Err("Unmatched ( or \\(".to_string());
                    }
                    return Ok(Ast { alternatives });
                }
                _ => return Err("internal: branch stopped before a separator".to_string()),
            }
        }
    }

    /// One concatenation of pieces, up to a `|`, a `)` or the end.
    fn branch(&mut self) -> Result<Vec<Piece>, String> {
        let mut pieces: Vec<Piece> = Vec::new();
        while let Some(c) = self.peek(0) {
            let atom = match c {
                // In the extended dialect these are the operators themselves,
                // so a branch ends here and `alternation` reads the character.
                '|' | ')' => break,
                '(' => {
                    self.at += 1;
                    Atom::Group(self.alternation(true)?)
                }
                '\\' => {
                    let Some(next) = self.peek(1) else {
                        return Err("Trailing backslash".to_string());
                    };
                    match next {
                        '1'..='9' => {
                            return Err(
                                "back-references are not supported by Sterna's in-process grep"
                                    .to_string(),
                            );
                        }
                        'w' => {
                            self.at += 2;
                            Atom::Word
                        }
                        'W' => {
                            self.at += 2;
                            Atom::NotWord
                        }
                        's' => {
                            self.at += 2;
                            Atom::Space
                        }
                        'S' => {
                            self.at += 2;
                            Atom::NotSpace
                        }
                        'b' => {
                            self.at += 2;
                            Atom::WordBoundary
                        }
                        'B' => {
                            self.at += 2;
                            Atom::NotWordBoundary
                        }
                        '<' => {
                            self.at += 2;
                            Atom::WordStart
                        }
                        '>' => {
                            self.at += 2;
                            Atom::WordEnd
                        }
                        // `\|`, `\(`, `\)`, `\+`, `\?`, `\{`, `\}` — every
                        // operator of the extended dialect, escaped — and
                        // every other escaped character stand for themselves.
                        literal => {
                            self.at += 2;
                            Atom::Char(literal)
                        }
                    }
                }
                '+' | '?' | '{' if !pieces.is_empty() => {
                    self.quantify(&mut pieces)?;
                    continue;
                }
                // With nothing to repeat, `{` is the character `{` to GNU
                // grep, as `*` is `*` — so `{2}` is `{2}`, its `}` falling to
                // the literal arm below.
                '{' => {
                    self.at += 1;
                    Atom::Char('{')
                }
                // `+` and `?` with nothing before them, likewise.
                '+' | '?' => {
                    self.at += 1;
                    Atom::Char(c)
                }
                '*' if !pieces.is_empty()
                    && !matches!(
                        pieces.last(),
                        Some(Piece {
                            atom: Atom::LineStart,
                            ..
                        })
                    ) =>
                {
                    self.quantify(&mut pieces)?;
                    continue;
                }
                // A `*` with nothing to repeat is the character `*`.
                '*' => {
                    self.at += 1;
                    Atom::Char('*')
                }
                // In the extended dialect an anchor is an anchor wherever it
                // stands — `a^b` and `a$b` match nothing rather than being
                // the characters, which is what `\^` and `\$` are for.
                '^' => {
                    self.at += 1;
                    Atom::LineStart
                }
                '$' => {
                    self.at += 1;
                    Atom::LineEnd
                }
                '.' => {
                    self.at += 1;
                    Atom::Any
                }
                '[' => self.bracket()?,
                literal => {
                    self.at += 1;
                    Atom::Char(literal)
                }
            };
            pieces.push(Piece {
                atom,
                min: 1,
                max: Some(1),
                quantified: false,
            });
        }
        Ok(pieces)
    }

    /// Applies `*`, `+`, `?` or `{m,n}` to the last piece.
    fn quantify(&mut self, pieces: &mut [Piece]) -> Result<(), String> {
        let (min, max) = match self.peek(0) {
            Some('*') => {
                self.at += 1;
                (0, None)
            }
            Some('+') => {
                self.at += 1;
                (1, None)
            }
            Some('?') => {
                self.at += 1;
                (0, Some(1))
            }
            Some('{') => {
                self.at += 1;
                self.interval()?
            }
            _ => return Err("internal: not a quantifier".to_string()),
        };
        let last = pieces.last_mut().expect("a quantifier follows a piece");
        if last.quantified {
            // A second quantifier repeats the first one's whole match, as
            // GNU grep reads it: `a{2}*` is `(a{2})*`, even runs only.
            // Widening the bounds instead matched lines grep does not print.
            let inner = std::mem::replace(
                last,
                Piece {
                    atom: Atom::Char('\0'),
                    min: 1,
                    max: Some(1),
                    quantified: false,
                },
            );
            *last = Piece {
                atom: Atom::Group(Ast {
                    alternatives: vec![vec![inner]],
                }),
                min,
                max,
                quantified: true,
            };
        } else {
            last.min = min;
            last.max = max;
            last.quantified = true;
        }
        Ok(())
    }

    /// The body of `{m}`, `{m,}` or `{m,n}` after the `{`, through the `}`.
    fn interval(&mut self) -> Result<(u32, Option<u32>), String> {
        let bad = || "Invalid content of \\{\\}".to_string();
        let number = |parser: &mut Self| -> Option<u32> {
            let start = parser.at;
            while parser.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                parser.at += 1;
            }
            parser.chars[start..parser.at]
                .iter()
                .collect::<String>()
                .parse()
                .ok()
        };
        // GNU grep reads an empty minimum, `{,n}`, as `{0,n}`.
        let min = if self.peek(0) == Some(',') {
            0
        } else {
            number(self).ok_or_else(bad)?
        };
        let max = if self.peek(0) == Some(',') {
            self.at += 1;
            if self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                Some(number(self).ok_or_else(bad)?)
            } else {
                None
            }
        } else {
            Some(min)
        };
        if self.peek(0) != Some('}') {
            return Err("Unmatched \\{".to_string());
        }
        self.at += 1;
        if max.is_some_and(|max| max < min) {
            return Err(bad());
        }
        Ok((min, max))
    }

    /// A bracket expression, from its `[` through its `]`.
    fn bracket(&mut self) -> Result<Atom, String> {
        let unmatched = || "Unmatched [, [^, [:, [., or [=".to_string();
        self.at += 1;
        let negated = self.peek(0) == Some('^');
        if negated {
            self.at += 1;
        }
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(c) = self.peek(0) else {
                return Err(unmatched());
            };
            if c == ']' && !first {
                self.at += 1;
                break;
            }
            first = false;
            if c == '[' && matches!(self.peek(1), Some('.') | Some('=')) {
                return Err(
                    "collating symbols and equivalence classes are not supported by Sterna's \
                     in-process grep"
                        .to_string(),
                );
            }
            if c == '[' && self.peek(1) == Some(':') {
                let start = self.at + 2;
                let mut end = start;
                while self.chars.get(end).is_some_and(|c| *c != ':') {
                    end += 1;
                }
                if self.chars.get(end) != Some(&':') || self.chars.get(end + 1) != Some(&']') {
                    return Err(unmatched());
                }
                let name: String = self.chars[start..end].iter().collect();
                let class = Class::named(&name)
                    .ok_or_else(|| "Invalid character class name".to_string())?;
                items.push(SetItem::Class(class));
                self.at = end + 2;
                continue;
            }
            self.at += 1;
            // A range `a-z`; a `-` that begins or ends the expression is
            // itself.
            if self.peek(0) == Some('-') && self.peek(1).is_some_and(|next| next != ']') {
                let high = self.peek(1).expect("checked above");
                self.at += 2;
                if high < c {
                    return Err("Invalid range end".to_string());
                }
                items.push(SetItem::Range(c, high));
            } else {
                items.push(SetItem::Char(c));
            }
        }
        Ok(Atom::Set { items, negated })
    }
}

/// One instruction of a compiled pattern.
#[derive(Debug, Clone, Copy)]
enum Inst {
    Char(char),
    Any,
    Set(usize),
    Word,
    NotWord,
    Space,
    NotSpace,
    Assert(Assertion),
    /// Try both, the first preferred — which, for a yes-or-no answer, is
    /// only an order.
    Split(usize, usize),
    Jmp(usize),
    Match,
}

#[derive(Debug, Clone, Copy)]
enum Assertion {
    LineStart,
    LineEnd,
    WordStart,
    WordEnd,
    WordBoundary,
    NotWordBoundary,
}

impl Assertion {
    fn holds(self, chars: &[char], at: usize) -> bool {
        let word_before = at > 0 && is_word(chars[at - 1]);
        let word_here = at < chars.len() && is_word(chars[at]);
        match self {
            Assertion::LineStart => at == 0,
            Assertion::LineEnd => at == chars.len(),
            Assertion::WordStart => !word_before && word_here,
            Assertion::WordEnd => word_before && !word_here,
            Assertion::WordBoundary => word_before != word_here,
            Assertion::NotWordBoundary => word_before == word_here,
        }
    }
}

fn set_holds(items: &[SetItem], c: char) -> bool {
    items.iter().any(|item| match item {
        SetItem::Char(expected) => c == *expected,
        SetItem::Range(low, high) => (*low..=*high).contains(&c),
        SetItem::Class(class) => class.holds(c),
    })
}

/// The bound on a compiled program, so `{32767}` of a long group is
/// refused as `grep` refuses it rather than allocated.
const MAX_PROGRAM: usize = 100_000;

struct Compiler {
    program: Vec<Inst>,
    sets: Vec<(Vec<SetItem>, bool)>,
}

impl Compiler {
    fn emit(&mut self, inst: Inst) -> Result<usize, String> {
        if self.program.len() >= MAX_PROGRAM {
            return Err("Regular expression too big".to_string());
        }
        self.program.push(inst);
        Ok(self.program.len() - 1)
    }

    /// `split a, next; a; jmp end; split b, next; b; jmp end; c; end:`.
    fn ast(&mut self, ast: &Ast) -> Result<(), String> {
        let count = ast.alternatives.len();
        let mut jumps = Vec::new();
        for (index, pieces) in ast.alternatives.iter().enumerate() {
            let last = index + 1 == count;
            let split = if last {
                None
            } else {
                Some(self.emit(Inst::Split(0, 0))?)
            };
            for piece in pieces {
                self.piece(piece)?;
            }
            if !last {
                jumps.push(self.emit(Inst::Jmp(0))?);
            }
            if let Some(split) = split {
                self.program[split] = Inst::Split(split + 1, self.program.len());
            }
        }
        let end = self.program.len();
        for jump in jumps {
            self.program[jump] = Inst::Jmp(end);
        }
        Ok(())
    }

    /// `min` copies, then either a loop (`split body, exit; body; jmp
    /// split`) or `max - min` optional copies.
    fn piece(&mut self, piece: &Piece) -> Result<(), String> {
        for _ in 0..piece.min {
            self.atom(&piece.atom)?;
        }
        match piece.max {
            None => {
                let split = self.emit(Inst::Split(0, 0))?;
                self.atom(&piece.atom)?;
                self.emit(Inst::Jmp(split))?;
                self.program[split] = Inst::Split(split + 1, self.program.len());
            }
            Some(max) => {
                let mut splits = Vec::new();
                for _ in piece.min..max {
                    splits.push(self.emit(Inst::Split(0, 0))?);
                    self.atom(&piece.atom)?;
                }
                let exit = self.program.len();
                for split in splits {
                    self.program[split] = Inst::Split(split + 1, exit);
                }
            }
        }
        Ok(())
    }

    fn atom(&mut self, atom: &Atom) -> Result<(), String> {
        let inst = match atom {
            Atom::Char(c) => Inst::Char(*c),
            Atom::Any => Inst::Any,
            Atom::Set { items, negated } => {
                self.sets.push((items.clone(), *negated));
                Inst::Set(self.sets.len() - 1)
            }
            Atom::Group(ast) => return self.ast(ast),
            Atom::LineStart => Inst::Assert(Assertion::LineStart),
            Atom::LineEnd => Inst::Assert(Assertion::LineEnd),
            Atom::WordStart => Inst::Assert(Assertion::WordStart),
            Atom::WordEnd => Inst::Assert(Assertion::WordEnd),
            Atom::WordBoundary => Inst::Assert(Assertion::WordBoundary),
            Atom::NotWordBoundary => Inst::Assert(Assertion::NotWordBoundary),
            Atom::Word => Inst::Word,
            Atom::NotWord => Inst::NotWord,
            Atom::Space => Inst::Space,
            Atom::NotSpace => Inst::NotSpace,
        };
        self.emit(inst)?;
        Ok(())
    }
}

/// The live threads of one step: a list for iteration and a per-instruction
/// mark for membership, so adding is constant and clearing is one increment.
struct Threads {
    list: Vec<usize>,
    mark: Vec<u32>,
    epoch: u32,
}

impl Threads {
    fn new(size: usize) -> Self {
        Self {
            list: Vec::new(),
            mark: vec![0; size],
            epoch: 1,
        }
    }

    fn clear(&mut self) {
        self.list.clear();
        self.epoch += 1;
    }

    fn insert(&mut self, pc: usize) -> bool {
        if self.mark[pc] == self.epoch {
            return false;
        }
        self.mark[pc] = self.epoch;
        self.list.push(pc);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(pattern: &str, line: &str) -> bool {
        Pattern::compile(pattern)
            .unwrap_or_else(|error| panic!("{pattern}: {error}"))
            .matches(line)
    }

    #[test]
    fn literals_dots_and_stars_match_as_grep_matches_them() {
        assert!(found("needle", "a needle in here"));
        assert!(!found("needle", "nothing"));
        assert!(found("n.edle", "needle"));
        assert!(found("ne*dle", "ndle"));
        assert!(found("ne*dle", "neeeedle"));
        assert!(found("-rf", "-rf literal pattern"));
        // A `*` with nothing before it, or after `^`, is the character.
        assert!(found("*x", "a *x"));
        assert!(found("^*x", "*x"));
        assert!(!found("^*x", "a*x"));
        // Escaped metacharacters are themselves.
        assert!(found(r"a\.b", "a.b"));
        assert!(!found(r"a\.b", "axb"));
        assert!(found(r"\[1\]", "literal[1].txt"));
        assert!(found(r"C:\\Users", r"C:\Users\x"));
    }

    #[test]
    fn anchors_apply_everywhere_ere_says_they_do() {
        assert!(found("^fn ", "fn main() {}"));
        assert!(!found("^fn ", "  fn main() {}"));
        assert!(found("}$", "fn main() {}"));
        assert!(!found("}$", "} // trailing"));
        // In the extended dialect an anchor is an anchor wherever it stands,
        // so these match nothing at all; the characters are `\^` and `\$`.
        assert!(!found("a^b", "a^b"));
        assert!(!found("a$b", "a$b"));
        assert!(found(r"a\^b", "a^b"));
        assert!(found(r"a\$b", "a$b"));
        assert!(found("^$", ""));
        assert!(!found("^$", "x"));
        // An anchor inside a group and before an alternation is still one.
        assert!(found("(^fn|^struct) ", "struct X"));
        assert!(!found("(^fn|^struct) ", " struct X"));
        assert!(found("(;$|,$)", "let x = 1;"));
    }

    #[test]
    fn brackets_ranges_and_classes() {
        assert!(found("[abc]x", "cx"));
        assert!(!found("[abc]x", "dx"));
        assert!(found("[^abc]x", "dx"));
        assert!(found("[a-c]x", "bx"));
        assert!(found("[[:digit:]][[:digit:]]", "line 42"));
        assert!(!found("[[:digit:]][[:digit:]]", "line 4"));
        assert!(found("[[:space:]]", "a b"));
        // A `]` first is itself, and a `-` at either end is itself.
        assert!(found("[]a]", "]"));
        assert!(found("[-a]", "-"));
        assert!(found("[a-]", "-"));
        assert!(found("x[[:alpha:]_]*", "x_ab1"));
    }

    #[test]
    fn extended_groups_alternation_intervals_and_the_gnu_escapes() {
        assert!(found("ab+c", "abbbc"));
        assert!(!found("ab+c", "ac"));
        assert!(found("ab?c", "ac"));
        assert!(found("(ab){2}", "xabab"));
        assert!(!found("(ab){2}", "xab"));
        assert!(found("a{2,3}", "aaa"));
        assert!(!found("^a{2,3}$", "aaaa"));
        assert!(found("a{2,}", "aaaaa"));
        assert!(found("needle|haystack", "a haystack"));
        assert!(found("^(fn|struct) ", "struct X"));
        assert!(!found("^(fn|struct) ", "enum X"));
        assert!(found(r"\w+", "  word"));
        assert!(!found(r"^\w+$", "two words"));
        assert!(found(r"\s", "two words"));
        assert!(found(r"\bfoo\b", "a foo b"));
        assert!(!found(r"\bfoo\b", "afoob"));
        assert!(found(r"\<foo\>", "(foo)"));
        assert!(found(r"\Boo", "foo"));
        // An empty group iteration does not loop for ever.
        assert!(found("(a*)*b", "aaab"));
        assert!(found("(a*){2}b", "b"));
        // Escaped, every operator of the dialect is the character itself.
        assert!(found(r"a\|b", "a|b"));
        assert!(!found(r"a\|b", "a"));
        assert!(found(r"\(ab\)", "(ab)"));
        assert!(!found(r"\(ab\)", "ab"));
        assert!(found(r"a\+", "a+"));
        assert!(found(r"a\?", "a?"));
        assert!(found(r"a\{2\}", "a{2}"));
    }

    #[test]
    fn a_long_line_does_not_recurse_per_character() {
        let line = "x".repeat(200_000);
        assert!(found(".*y", &format!("{line}y")));
        assert!(!found(".*y", &line));
        assert!(found("x*$", &line));
    }

    #[test]
    fn what_this_matcher_refuses_it_refuses_out_loud() {
        for (pattern, expected) in [
            (r"(a)\1", "back-references"),
            (r"[abc", "Unmatched ["),
            ("(ab", "Unmatched ("),
            ("ab)", "Unmatched )"),
            ("a{2", "Unmatched \\{"),
            ("a{3,2}", "Invalid content"),
            (r"[[:nosuch:]]", "Invalid character class"),
            (r"[z-a]", "Invalid range end"),
            (r"ab\", "Trailing backslash"),
        ] {
            let error = Pattern::compile(pattern).expect_err(pattern);
            assert!(error.contains(expected), "{pattern}: {error}");
        }
    }

    /// Verify Finding 1: a second quantifier repeats the first one's match.
    /// `a{2}*` is `(aa)*` to GNU grep 3.11 — even runs only — and the
    /// widest-bound reading this replaced also printed the odd ones.
    #[test]
    fn finding_1_a_doubled_quantifier_repeats_the_quantified_piece() {
        for (line, expected) in [
            ("", true),
            ("aa", true),
            ("aaaa", true),
            ("aaa", false),
            ("aaaaa", false),
        ] {
            assert_eq!(found("^a{2}*$", line), expected, "a{{2}}* against {line:?}");
        }
        for line in ["", "a", "aaa"] {
            assert!(found("^a**$", line), "a** against {line:?}");
            assert!(found("^(a)**$", line), "(a)** against {line:?}");
            assert!(found("^a*{2}$", line), "a*{{2}} against {line:?}");
        }
        assert!(!found("^a**$", "ab"));
        assert!(found("^a{2}{3}$", "aaaaaa"));
        assert!(!found("^a{2}{3}$", "aaaaa"));
    }

    /// Verify Finding 2: GNU grep reads `{,n}` as `{0,n}`.
    #[test]
    fn finding_2_an_open_minimum_interval_is_zero_to_n() {
        assert!(found("^a{,3}$", ""));
        assert!(found("^a{,3}$", "aaa"));
        assert!(!found("^a{,3}$", "aaaa"));
        assert!(found("^a{,}$", "aaaaaaa"));
        assert!(found("a{,2}b", "xb"));
    }

    /// Verify Finding 3: with nothing before it, `{n}` is the literal
    /// `{n}` to GNU grep, as a leading `*` is `*`.
    #[test]
    fn finding_3_an_interval_with_nothing_before_it_is_literal() {
        assert!(found("{2}", "{2}"));
        assert!(!found("{2}", "2"));
        assert!(!found("{2}x", "{2}"));
        assert!(found("({2})", "x{2}x"));
    }

    /// Verify Finding 4: a tree whose only match is in a binary file is a
    /// match — GNU `grep -r` exits 0 beside its stderr notice.
    #[test]
    fn finding_4_a_binary_only_match_exits_zero() {
        let root = fixture("binary-only");
        let profile = profile(&root);
        let blob = root.join("blob.bin");
        let result = grep_tree(&profile, &|| false, "grep", &blob, "needle", &[]).unwrap();
        assert_eq!(result.stdout, "", "{result:?}");
        assert!(
            result.stderr.contains("blob.bin: binary file matches"),
            "{result:?}"
        );
        assert_eq!(
            result.exit_code,
            Some(0),
            "a binary match is a match: {result:?}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Verify Finding 5: one unreadable directory does not lose the matches
    /// already found — they stay on stdout, the error is named on stderr,
    /// and the exit is 2 as GNU `grep -r` answers.
    #[cfg(unix)]
    #[test]
    fn finding_5_an_unreadable_directory_keeps_the_matches_beside_the_error() {
        use std::os::unix::fs::PermissionsExt;
        let root = fixture("unreadable");
        let profile = profile(&root);
        let src = root.join("src");
        let locked = src.join("z-locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("hidden.rs"), "needle hidden\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = grep_tree(&profile, &|| false, "grep", &src, "needle", &[]);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let result = result.expect("an unreadable directory does not end the walk");
        assert!(
            result
                .stdout
                .contains(&format!("{}:1:needle source", src.join("lib.rs").display())),
            "{result:?}"
        );
        assert!(!result.stdout.contains("hidden"), "{result:?}");
        assert!(
            result
                .stderr
                .contains(&format!("grep: {}:", locked.display())),
            "{result:?}"
        );
        assert_eq!(result.exit_code, Some(2), "{result:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    fn fixture(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "sterna-in-process-search-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (relative, contents) in [
            ("src/lib.rs", "needle source\nplain\n"),
            ("src/deep/more.rs", "another needle\r\n"),
            (".git/config", "needle git internals\n"),
            (".sterna/rollout.jsonl", "needle model feedback\n"),
            ("secrets/token", "needle secret\n"),
            ("blob.bin", "needle\0binary\n"),
        ] {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        std::fs::canonicalize(&root).unwrap()
    }

    fn profile(root: &Path) -> Profile {
        let root_text = root.to_string_lossy().replace('\\', "/");
        Profile::compile(
            root,
            Some(&format!(
                r#"{{"permissions":{{"deny":["Read({root_text}/secrets/**)"]}}}}"#
            )),
        )
    }

    #[test]
    fn read_answers_the_bytes_or_cats_own_failure_shape() {
        let root = fixture("read");
        let ok = read_file("read", &root.join("src").join("lib.rs"));
        assert_eq!(ok.exit_code, Some(0));
        assert_eq!(ok.stdout, "needle source\nplain\n");
        assert_eq!(ok.confinement, Confinement::InProcess);

        let missing = read_file("read", &root.join("absent.rs"));
        assert_eq!(missing.exit_code, Some(1));
        assert!(missing.stdout.is_empty());
        assert!(
            missing.stderr.starts_with("read: ") && missing.stderr.contains("absent.rs"),
            "{}",
            missing.stderr
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn grep_prints_located_lines_prunes_git_skips_denied_files_and_reports_binaries() {
        let root = fixture("grep");
        let profile = profile(&root);
        let result = grep_tree(&profile, &|| false, "grep", &root, "needle", &[]).unwrap();
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        let lib = root.join("src").join("lib.rs");
        let more = root.join("src").join("deep").join("more.rs");
        assert_eq!(
            result.stdout,
            format!(
                "{}:1:needle model feedback\n{}:1:another needle\r\n{}:1:needle source\n",
                root.join(".sterna").join("rollout.jsonl").display(),
                more.display(),
                lib.display()
            ),
            "name order, the CR kept, .git pruned, secrets/ never opened"
        );
        assert!(
            result.stderr.contains("blob.bin: binary file matches"),
            "{}",
            result.stderr
        );
        assert!(!result.stderr.contains("secrets"), "{}", result.stderr);

        // Rooted inside `.git`, the search is not broad and reads it.
        let git = grep_tree(
            &profile,
            &|| false,
            "grep",
            &root.join(".git"),
            "needle",
            &[],
        )
        .unwrap();
        assert!(git.stdout.contains("git internals"), "{git:?}");

        // A single file, and no match.
        let none = grep_tree(&profile, &|| false, "grep", &lib, "absent", &[]).unwrap();
        assert_eq!((none.exit_code, none.stdout.as_str()), (Some(1), ""));

        // A pattern grep cannot compile is exit 2 with the reason.
        let bad = grep_tree(&profile, &|| false, "grep", &lib, "[", &[]).unwrap();
        assert_eq!(bad.exit_code, Some(2));
        assert!(
            bad.stderr.starts_with("grep: Unmatched ["),
            "{}",
            bad.stderr
        );

        // The stop predicate ends the walk.
        let stopped = grep_tree(&profile, &|| true, "grep", &root, "needle", &[]);
        assert!(matches!(stopped, Err(ToolError::Cancelled { .. })));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The names `checked_call` would have spelled `--exclude-dir=` are the
    /// names this walk prunes, and a file of that name is still read.
    #[test]
    fn a_broad_walk_prunes_the_names_the_spawned_form_excludes() {
        let root = fixture("ignored");
        let profile = profile(&root);
        for (relative, contents) in [
            ("holder/generated/big.txt", "needle generated\n"),
            ("other/generated", "needle a file, not the directory\n"),
        ] {
            let path = relative
                .split('/')
                .fold(root.clone(), |path, part| path.join(part));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        let skipped = ["generated".to_string()];
        let pruned = grep_tree(&profile, &|| false, "grep", &root, "needle", &skipped).unwrap();
        assert!(!pruned.stdout.contains("big.txt"), "{}", pruned.stdout);
        assert!(
            pruned.stdout.contains("a file, not the directory"),
            "a rule ending in `/` names a directory only: {}",
            pruned.stdout
        );
        assert!(
            pruned.stdout.contains("needle source"),
            "real source was lost: {}",
            pruned.stdout
        );

        // Without the names, the same walk reads the generated tree -- which
        // is what it did on Windows while the spawned form skipped it.
        let whole = grep_tree(&profile, &|| false, "grep", &root, "needle", &[]).unwrap();
        assert!(whole.stdout.contains("big.txt"), "{}", whole.stdout);

        // A search aimed into the tree reads it, as `.git` already works.
        let inside = root.join("holder").join("generated");
        let named = grep_tree(&profile, &|| false, "grep", &inside, "needle", &skipped).unwrap();
        assert!(named.stdout.contains("big.txt"), "{}", named.stdout);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn grep_skips_a_symlink_met_during_the_walk_as_grep_r_does() {
        let root = fixture("symlink");
        std::os::unix::fs::symlink(root.join("src"), root.join("linked")).unwrap();
        let profile = profile(&root);
        let result = grep_tree(&profile, &|| false, "grep", &root, "needle", &[]).unwrap();
        assert!(!result.stdout.contains("linked"), "{}", result.stdout);
        let _ = std::fs::remove_dir_all(root);
    }
}
