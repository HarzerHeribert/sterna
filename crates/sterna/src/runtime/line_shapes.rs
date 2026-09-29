//! Normalised line shapes: what a text looks like, without its values.
//!
//! **A histogram of normalised line shapes says "3,907 lines look alike and
//! 12 do not" on any ecosystem**, without knowing what any of the words mean.
//! Jev reads it to tell a log from a value (`decide::field_shape`).
//!
//! **The normaliser holds no vocabulary, and that is enforceable rather than
//! promised.** It decides what is structure and what is a value from
//! [document frequency](Shapes): a word occurring on many distinct lines is
//! skeleton and is kept, a word occurring on one or two is the thing that
//! varies and becomes `‹ident›`. Numbers, paths, quoted spans and hex are
//! values by nature and are replaced wherever they appear. Nothing in this
//! module names a tool, a language or a test runner, and
//! `the_normaliser_names_no_ecosystem` fails if that stops being true.

use std::collections::BTreeMap;

/// A word must occur on at least this many distinct lines to be read as part
/// of a line's skeleton rather than as a value standing in it.
///
/// Two is too low on a long output: a failure and its echo would make the
/// failing symbol look structural and collapse two unlike lines into one
/// shape. Three is the smallest number that needs a real pattern.
const STRUCTURAL_MIN_LINES: usize = 3;

/// …but three is too high on a short one, where nothing occurs three times
/// and every line normalises to an unreadable run of `‹ident›`. A six-line
/// stack trace has two frames and they are the pattern.
///
/// The second clause is a share rather than a count, so it scales with the
/// input instead of naming a size: a word on a quarter of the lines is
/// skeleton however few lines there are, and on a long output that share is
/// unreachable by accident.
fn is_structural(frequency: usize, lines: usize) -> bool {
    frequency >= STRUCTURAL_MIN_LINES || (frequency >= 2 && frequency * 4 >= lines)
}

/// How many distinct shapes the sample names. The tail is summarised by a
/// count rather than dropped, so a reader can tell a truncated histogram from
/// a complete one.
const MAX_SHAPES: usize = 24;

/// Lines past which the histogram is computed on a prefix rather than on
/// everything. A shape needs a few thousand lines to establish itself; past
/// that the counts change and the shapes do not.
const MAX_HISTOGRAM_LINES: usize = 20_000;

/// One normalised line shape and how many lines wore it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    pub shape: String,
    pub count: usize,
}

/// The shapes of one text, most frequent first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shapes {
    pub shapes: Vec<Shape>,
    /// Distinct shapes past [`MAX_SHAPES`], and the lines they account for.
    pub tail_shapes: usize,
    pub tail_lines: usize,
    pub lines: usize,
    pub bytes: usize,
}

/// Drop ANSI escape sequences.
///
/// First, because everything downstream is wrong without it: an unstripped
/// `\x1b[32m` makes two identical lines look unalike to the histogram, and
/// makes a verbatim check compare a coloured line against an uncoloured one.
/// jest and cargo both colour their output whenever they think a terminal is
/// watching.
#[must_use]
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI — `ESC [ … final`, the form every colour code takes. Anything
        // else after ESC is a two-character sequence; drop its partner.
        match chars.peek() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '~' {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Whether a token is a filesystem path rather than a word.
///
/// A separator plus something either side of it. Deliberately coarse: a URL,
/// a Windows path and a Go package path all read as paths here, and all three
/// are values rather than structure.
fn is_path(token: &str) -> bool {
    let has_separator = token.contains('/') || token.contains('\\');
    has_separator && token.len() > 1 && !token.chars().all(|c| c == '/' || c == '\\')
}

/// Whether a token is a hexadecimal value: a `0x` literal, or a long bare run
/// of hex digits, which is what a hash, a uuid or an address looks like.
fn is_hex(token: &str) -> bool {
    let body = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
        .unwrap_or(token);
    if body.is_empty() {
        return false;
    }
    let hexish = body
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == '-' || c == '_');
    let digits = body.chars().filter(char::is_ascii_hexdigit).count();
    if token.starts_with("0x") || token.starts_with("0X") {
        return hexish && digits > 0;
    }
    // A bare run has to be long enough that it cannot be an ordinary word:
    // `add`, `face` and `decade` are all valid hex.
    hexish && digits >= 8 && body.chars().any(|c| c.is_ascii_digit())
}

/// Whether a token carries a number: a count, a duration, a line, a version.
fn is_numeric(token: &str) -> bool {
    token.chars().any(|c| c.is_ascii_digit())
        && token
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ':' | '-' | '+' | '%'))
}

/// A number with a unit suffix — `13.01s`, `250ms`, `1.2MiB` — which is one
/// value and not a number beside a word.
fn is_numeric_with_unit(token: &str) -> bool {
    let split = token
        .char_indices()
        .find(|(_, c)| c.is_ascii_alphabetic())
        .map(|(index, _)| index);
    let Some(split) = split else { return false };
    if split == 0 {
        return false;
    }
    let (number, unit) = token.split_at(split);
    is_numeric(number)
        && !unit.is_empty()
        && unit.len() <= 4
        && unit.chars().all(char::is_alphabetic)
}

/// Split a line into tokens, keeping separators so a shape reads like a line.
///
/// Quoted spans come out whole, because the text inside a quote is a value
/// however many words it contains — `expected 'foo bar baz'` is one varying
/// thing, not three.
fn tokens(line: &str) -> Vec<Token<'_>> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let rest = &line[index..];
        let c = rest.chars().next().expect("index is a char boundary");
        if matches!(c, '"' | '\'' | '`')
            && let Some(end) = rest[c.len_utf8()..].find(c)
        {
            let span = &rest[..c.len_utf8() + end + c.len_utf8()];
            out.push(Token::Quoted);
            index += span.len();
            continue;
        }
        if c.is_whitespace() {
            let run: String = rest.chars().take_while(|c| c.is_whitespace()).collect();
            out.push(Token::Space);
            index += run.len();
            continue;
        }
        if c.is_alphanumeric() || c == '_' || c == '/' || c == '\\' || c == '.' || c == '-' {
            let end = rest
                .char_indices()
                .find(|(_, c)| {
                    !(c.is_alphanumeric()
                        || *c == '_'
                        || *c == '/'
                        || *c == '\\'
                        || *c == '.'
                        || *c == '-'
                        || *c == ':'
                        || *c == '+'
                        || *c == '%'
                        || *c == ',')
                })
                .map_or(rest.len(), |(index, _)| index);
            out.push(Token::Word(&rest[..end]));
            index += end;
            continue;
        }
        out.push(Token::Punct(&rest[..c.len_utf8()]));
        index += c.len_utf8();
    }
    out
}

enum Token<'a> {
    Word(&'a str),
    /// The span itself is never read: a quoted run is a value whatever it
    /// says, and the shape stands in for all of it.
    Quoted,
    /// Likewise — indentation varies between otherwise identical lines, so a
    /// run of whitespace normalises to one space.
    Space,
    Punct(&'a str),
}

/// Count, for each word, how many distinct lines it appeared on.
fn document_frequency(lines: &[&str]) -> BTreeMap<String, usize> {
    let mut frequency: BTreeMap<String, usize> = BTreeMap::new();
    for line in lines {
        let mut seen: BTreeMap<String, ()> = BTreeMap::new();
        for token in tokens(&strip_ansi(line)) {
            if let Token::Word(word) = token
                && !is_path(word)
                && !is_hex(word)
                && !is_numeric(word)
                && !is_numeric_with_unit(word)
            {
                seen.insert(word.to_string(), ());
            }
        }
        for (word, ()) in seen {
            *frequency.entry(word).or_insert(0) += 1;
        }
    }
    frequency
}

/// Replace what varies and keep what recurs.
///
/// A word survives when [`is_structural`] says [`document_frequency`] saw it
/// often enough to be skeleton. Everything a word cannot
/// be — a path, a number, a hash, a quoted span — is replaced wherever it
/// stands, because those are values by their shape and not by how often they
/// occur.
fn normalise(line: &str, frequency: &BTreeMap<String, usize>, lines: usize) -> String {
    let stripped = strip_ansi(line);
    let mut out = String::with_capacity(stripped.len());
    for token in tokens(&stripped) {
        match token {
            Token::Quoted => out.push_str("‹str›"),
            Token::Space => out.push(' '),
            Token::Punct(punct) => out.push_str(punct),
            Token::Word(word) => {
                if is_path(word) {
                    out.push_str("‹path›");
                } else if is_hex(word) {
                    out.push_str("‹hex›");
                } else if is_numeric(word) || is_numeric_with_unit(word) {
                    out.push_str("‹num›");
                } else if is_structural(frequency.get(word).copied().unwrap_or(0), lines) {
                    out.push_str(word);
                } else {
                    out.push_str("‹ident›");
                }
            }
        }
    }
    out.trim().to_string()
}

/// The shapes of `text`, most frequent first.
#[must_use]
pub fn shapes_of(text: &str) -> Shapes {
    let all: Vec<&str> = text.lines().collect();
    let considered: Vec<&str> = all.iter().take(MAX_HISTOGRAM_LINES).copied().collect();
    let frequency = document_frequency(&considered);

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in &considered {
        let shape = normalise(line, &frequency, considered.len());
        if shape.is_empty() {
            continue;
        }
        *counts.entry(shape).or_insert(0) += 1;
    }

    let mut shapes: Vec<Shape> = counts
        .into_iter()
        .map(|(shape, count)| Shape { shape, count })
        .collect();
    // Count first so the bulk leads; the shape breaks ties so the order is
    // the same on every run and a cache key derived from it is stable.
    shapes.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.shape.cmp(&b.shape)));

    let tail_shapes = shapes.len().saturating_sub(MAX_SHAPES);
    let tail_lines: usize = shapes
        .iter()
        .skip(MAX_SHAPES)
        .map(|shape| shape.count)
        .sum();
    shapes.truncate(MAX_SHAPES);

    Shapes {
        shapes,
        tail_shapes,
        tail_lines,
        lines: all.len(),
        bytes: text.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape_of(text: &str) -> Vec<(usize, String)> {
        shapes_of(text)
            .shapes
            .into_iter()
            .map(|shape| (shape.count, shape.shape))
            .collect()
    }

    /// The acceptance criterion in one test: the same normaliser, four
    /// ecosystems, no rule written for any of them. Each fixture repeats its
    /// passing line often enough to be real output and ends with a failure
    /// the histogram must keep apart from it.
    #[test]
    fn the_histogram_collapses_repetition_in_every_ecosystem() {
        let cargo: String = (0..40)
            .map(|n| format!("test suite::case_{n} ... ok\n"))
            .chain(["test suite::broken ... FAILED\n".to_string()])
            .collect();
        let jest: String = (0..40)
            .map(|n| format!("  \u{1b}[32m✓\u{1b}[0m renders the {n}th row (12 ms)\n"))
            .chain(["  \u{1b}[31m✕\u{1b}[0m renders an empty row (4 ms)\n".to_string()])
            .collect();
        let pytest: String = (0..40)
            .map(|n| format!("tests/test_api.py::test_case_{n} PASSED               [ 50%]\n"))
            .chain(["tests/test_api.py::test_broken FAILED                 [100%]\n".to_string()])
            .collect();
        let go: String = (0..40)
            .map(|n| format!("--- PASS: TestThing_{n} (0.00s)\n"))
            .chain(["--- FAIL: TestBroken (0.01s)\n".to_string()])
            .collect();

        for (name, text, expected_top) in [
            ("cargo", &cargo, "test ‹ident› ... ok"),
            ("jest", &jest, "✓ renders the ‹num› row (‹num› ms)"),
            ("pytest", &pytest, "‹path› PASSED [ ‹num›]"),
            ("go", &go, "--- PASS: ‹ident› (‹num›)"),
        ] {
            let shapes = shape_of(text);
            let (count, shape) = shapes.first().expect("a shape").clone();
            assert_eq!(
                count, 40,
                "{name}: the forty alike lines must collapse to one shape, got {shapes:?}"
            );
            assert_eq!(shape, expected_top, "{name}: unexpected normalisation");
            assert!(
                shapes.len() >= 2,
                "{name}: the failing line must not collapse into the passing shape: {shapes:?}"
            );
        }
    }

    /// A trace has no repetition to collapse; what it has is a skeleton.
    /// Both frames must lose their path and line number and keep the words
    /// that recur, so the histogram shows a reader "these are frames" without
    /// ever having been told what a frame is.
    #[test]
    fn a_stack_trace_normalises_its_frames_to_one_skeleton() {
        let trace = "\
Traceback (most recent call last):
  File \"/app/main.py\", line 12, in <module>
    run()
  File \"/app/lib.py\", line 4, in run
    raise ValueError('boom')
ValueError: boom
";
        let shapes = shape_of(trace);
        let frames: Vec<&String> = shapes
            .iter()
            .map(|(_, shape)| shape)
            .filter(|shape| shape.starts_with("File ‹str›, line ‹num› in"))
            .collect();
        assert_eq!(
            frames.len(),
            2,
            "both frames wear the same skeleton, differing only past it: {shapes:?}",
        );
        for shape in &frames {
            assert!(
                !shape.contains(".py") && !shape.contains("12"),
                "the path and the line number are what varied: {shape}",
            );
        }
    }

    #[test]
    fn colour_codes_do_not_split_a_shape_in_two() {
        let plain = "  ok  thing passed\n".repeat(5);
        let coloured = "  \u{1b}[32mok\u{1b}[0m  thing passed\n".repeat(5);
        assert_eq!(
            shape_of(&plain).first().map(|shape| shape.1.clone()),
            shape_of(&coloured).first().map(|shape| shape.1.clone()),
            "colour is not part of a line's shape",
        );
    }

    #[test]
    fn a_word_that_recurs_is_skeleton_and_one_that_does_not_is_a_value() {
        let text = "\
alpha keeps going
alpha keeps going
alpha keeps going
alpha keeps stopping
";
        let shapes = shape_of(text);
        assert!(
            shapes
                .iter()
                .any(|(count, shape)| *count == 3 && shape == "alpha keeps going"),
            "a word on three lines is structure: {shapes:?}",
        );
        assert!(
            shapes
                .iter()
                .any(|(_, shape)| shape == "alpha keeps ‹ident›"),
            "a word on one line is a value: {shapes:?}",
        );
    }

    /// The package exists because hand-written rules only knew cargo. This
    /// fails if the normaliser learns a vocabulary of its own.
    #[test]
    fn the_normaliser_names_no_ecosystem() {
        let source = include_str!("line_shapes.rs");
        let code = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let code = code
            .split("#[cfg(test)]")
            .next()
            .expect("there is always a first split");
        for vocabulary in [
            "cargo",
            "Compiling",
            "PASSED",
            "FAILED",
            "pytest",
            "jest",
            "npm",
            "gradle",
            "test result",
            "--- PASS",
            "Traceback",
        ] {
            assert!(
                !code.contains(vocabulary),
                "the normaliser must decide from character classes and frequency, \
                 never from knowing {vocabulary:?}",
            );
        }
    }
}
