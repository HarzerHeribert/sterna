//! Splits a command line into words, and refuses anything that is a program.
//!
//! The asymmetry this module exists to enforce (`semantic-command-lifting.md`,
//! *Parsing and recognition safety*): **a false negative costs an
//! optimisation, a false positive changes what the model asked for.** So every
//! construct whose meaning depends on the shell — a pipeline, a redirection,
//! a substitution, an expansion, an environment prefix — returns `None` rather
//! than a best guess, and the caller runs the original command unchanged.

/// The characters that make a command line a program rather than one command.
///
/// `*`, `?` and `[` are here because the shell expands them before the
/// program ever sees them, so a lifted capability receiving the literal
/// pattern would be searching for something else entirely. Inside quotes they
/// are ordinary characters and the tokenizer keeps them.
const SHELL_OPERATORS: [char; 14] = [
    '|', '&', ';', '<', '>', '`', '$', '(', ')', '{', '}', '*', '?', '[',
];

/// One simple command's words, or `None` if the line is anything more.
///
/// Quoting is honoured because a quoted argument is exactly the case where a
/// metacharacter is *not* an operator: `rg "a|b" src` is one command whose
/// pattern contains a pipe, and refusing it would be a false negative for no
/// reason. A double-quoted string containing `$`, a backtick or a backslash is
/// refused instead of unescaped, because those still expand inside them.
#[must_use]
pub fn split(command: &str) -> Option<Vec<String>> {
    if command.contains('\n') || command.contains('\r') {
        return None;
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            ' ' | '\t' => {
                if has_word {
                    words.push(std::mem::take(&mut word));
                    has_word = false;
                }
            }
            '\'' => {
                has_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        // An unterminated quote is not a command line.
                        None => return None,
                        Some(other) => word.push(other),
                    }
                }
            }
            '"' => {
                has_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        None => return None,
                        // Still expanded inside double quotes, so the word is
                        // not the literal it looks like.
                        Some('$' | '`' | '\\') => return None,
                        Some(other) => word.push(other),
                    }
                }
            }
            '\\' => return None,
            '~' if !has_word => return None,
            other if SHELL_OPERATORS.contains(&other) => return None,
            other => {
                has_word = true;
                word.push(other);
            }
        }
    }
    if has_word {
        words.push(word);
    }
    if words.is_empty() {
        return None;
    }
    // `FOO=bar cmd` runs `cmd` in a different environment, which is a meaning
    // no capability reproduces.
    if is_assignment(&words[0]) {
        return None;
    }
    Some(words)
}

/// Whether a word is a `NAME=value` environment prefix.
fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_command_splits_into_its_words() {
        assert_eq!(
            split("rg SessionManager crates/").unwrap(),
            ["rg", "SessionManager", "crates/"]
        );
    }

    #[test]
    fn quoting_keeps_a_metacharacter_as_an_ordinary_character() {
        assert_eq!(
            split("rg 'a|b' src").unwrap(),
            ["rg", "a|b", "src"],
            "a quoted pipe is part of the pattern, not an operator"
        );
        assert_eq!(split("rg \"a b\" src").unwrap(), ["rg", "a b", "src"]);
    }

    #[test]
    fn every_compound_form_is_refused() {
        for line in [
            "rg foo | awk '{print $2}'",
            "cargo test && ./fix.sh",
            "cargo test || echo broken",
            "rg foo; ls",
            "rg foo > out.txt",
            "rg foo < in.txt",
            "echo `date`",
            "echo $(date)",
            "echo $HOME",
            "(cd src && ls)",
            "ls &",
            "for f in *; do echo $f; done",
        ] {
            assert!(split(line).is_none(), "`{line}` must not be recognised");
        }
    }

    #[test]
    fn an_unquoted_glob_is_refused_because_the_shell_would_expand_it() {
        assert!(split("rg foo src/*.rs").is_none());
        assert!(split("cat file?.txt").is_none());
        assert!(split("ls [ab]").is_none());
        // Quoted, it is a literal and safe to pass through.
        assert_eq!(split("rg 'foo*' src").unwrap(), ["rg", "foo*", "src"]);
    }

    #[test]
    fn an_environment_prefix_is_refused() {
        assert!(split("FOO=bar rg foo src").is_none());
        assert!(split("RUST_LOG=debug cargo test").is_none());
        // A bare `=` in an argument is not an assignment prefix.
        assert_eq!(
            split("rg foo=bar src").unwrap(),
            ["rg", "foo=bar", "src"],
            "an argument containing `=` is not an environment prefix"
        );
    }

    #[test]
    fn an_escape_or_tilde_is_refused_rather_than_interpreted() {
        assert!(split("cat foo\\ bar").is_none());
        assert!(split("cat ~/notes.txt").is_none());
        assert!(split("rg \"a\\tb\" src").is_none());
    }

    #[test]
    fn an_unterminated_quote_is_not_a_command_line() {
        assert!(split("rg 'unterminated src").is_none());
        assert!(split("rg \"unterminated src").is_none());
    }

    #[test]
    fn an_empty_line_is_not_a_command() {
        assert!(split("").is_none());
        assert!(split("   ").is_none());
    }

    #[test]
    fn a_newline_makes_it_a_script() {
        assert!(split("rg foo src\nls").is_none());
    }
}
