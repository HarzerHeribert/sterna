//! A cell's returned value, rendered by its shape rather than as JSON.
//!
//! **A diff wrapped in a JSON string is the worst rendering of a diff
//! available.** `to_string_pretty` escapes the newlines inside a string
//! value, so a unified diff returned as a field arrived on screen as one
//! enormous line of `\n` and `\"`, hard-wrapped by the terminal. The user,
//! reading their own screen (2026-09-19): *"und das als Output interessiert
//! einen Menschen auch nicht"*.
//!
//! So a value is drawn by what it is. An object becomes labelled rows; a
//! field whose text is a diff goes to [`super::regions::push_diff`], the one
//! diff renderer this crate has; any other multi-line string becomes lines.
//! Nothing here is a second diff renderer, and nothing here invents a
//! heading a value did not carry.
//!
//! **Bounds are kept, not removed.** The handle table's own
//! `…(cut at 2,048 bytes…)` marker is upstream of this and still does its
//! job; what this module fixes is the shape of what precedes it. Each field
//! is bounded here as well, because one field must not be able to push every
//! other field off the screen.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{MUTED, Theme};

/// How many lines one field of an object may draw before it says how many
/// are left. Generous enough to show a hunk of a diff, small enough that a
/// long field cannot bury the fields beneath it.
const FIELD_LINES: usize = 18;

/// How many fields an object shows before it says how many are left.
const FIELDS: usize = 12;

/// Whether `text` is a unified diff.
///
/// Deliberately strict: a multi-line string that merely begins with a dash
/// is prose, and rendering prose through a diff gutter would colour it by
/// accident. A real diff announces itself with a `diff --git` header, a hunk
/// header, or the `---`/`+++` file pair in that order.
pub(super) fn is_diff(text: &str) -> bool {
    let first = text.lines().next().unwrap_or_default();
    if first.starts_with("diff --git ") {
        return true;
    }
    let mut saw_old = false;
    for line in text.lines().take(64) {
        if line.starts_with("@@ ") && line.contains(" @@") {
            return true;
        }
        if line.starts_with("--- ") {
            saw_old = true;
        } else if saw_old && line.starts_with("+++ ") {
            return true;
        }
    }
    false
}

/// One field's heading: the key, as the transcript's signage rather than as
/// a JSON key. Uppercase and accent, matching the panels in the rail.
fn key_line(key: &str, theme: Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {}", key.to_uppercase()),
        Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD),
    ))
}

/// A scalar field as one row: `   key  value`.
fn scalar_row(key: &str, value: &str, width: usize, theme: Theme) -> Line<'static> {
    let text: String = format!("   {key}  {value}").chars().take(width).collect();
    let split = text
        .char_indices()
        .nth(3 + key.chars().count())
        .map_or(text.len(), |(index, _)| index);
    Line::from(vec![
        Span::styled(
            text[..split].to_string(),
            Style::default().fg(theme.accent()),
        ),
        Span::styled(text[split..].to_string(), Style::default().fg(MUTED)),
    ])
}

/// A scalar as the text a person reads, rather than as JSON.
///
/// A string loses its quotes, because a quoted string in a labelled row is
/// punctuation nobody needs; every other scalar is its own rendering.
fn scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) if !text.contains('\n') => Some(text.clone()),
        serde_json::Value::Null => Some("null".into()),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Pushes a multi-line string as lines, bounded, with control characters
/// neutralised the way the diff renderer does -- the same reason: a value is
/// data and must never steer the terminal.
fn push_text(lines: &mut Vec<Line<'static>>, text: &str, limit: usize) {
    for line in text.lines().take(limit) {
        let safe: String = line
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        lines.push(Line::styled(
            format!("   {safe}"),
            Style::default().fg(MUTED),
        ));
    }
    let remaining = text.lines().count().saturating_sub(limit);
    if remaining > 0 {
        lines.push(Line::styled(
            format!(
                "   … {remaining} more line{} · Ctrl-O expands",
                plural(remaining)
            ),
            Style::default().fg(MUTED),
        ));
    }
}

/// One field of an object, by what it is.
fn push_field(lines: &mut Vec<Line<'static>>, field: &serde_json::Value) {
    match field {
        serde_json::Value::String(body) if is_diff(body) => {
            super::regions::push_diff(lines, body, FIELD_LINES);
        }
        serde_json::Value::String(body) => push_text(lines, body, FIELD_LINES),
        other => push_text(
            lines,
            &serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
            FIELD_LINES,
        ),
    }
}

/// A cell's returned value, drawn by its shape.
///
/// Returns `false` when the text is not something this module improves on --
/// a bare scalar, or anything that is not JSON -- so the caller keeps
/// whatever it drew before rather than this module having an opinion about
/// every value in the transcript.
pub(super) fn push_value(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    width: usize,
    theme: Theme,
) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    match value {
        serde_json::Value::String(body) if body.contains('\n') => {
            if is_diff(&body) {
                super::regions::push_diff(lines, &body, FIELD_LINES);
            } else {
                push_text(lines, &body, FIELD_LINES);
            }
            true
        }
        serde_json::Value::Object(fields) => {
            if fields.is_empty() {
                return false;
            }
            let total = fields.len();
            for (key, field) in fields.iter().take(FIELDS) {
                if let Some(scalar) = scalar_text(field) {
                    lines.push(scalar_row(key, &scalar, width, theme));
                    continue;
                }
                lines.push(key_line(key, theme));
                push_field(lines, field);
            }
            if total > FIELDS {
                lines.push(Line::styled(
                    format!(
                        "   … {} more field{} · Ctrl-O expands",
                        total - FIELDS,
                        plural(total - FIELDS)
                    ),
                    Style::default().fg(MUTED),
                ));
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    const DIFF: &str = "diff --git a/src/mod.rs b/src/mod.rs\nindex 0b8c5190..c251f32f 100644\n--- a/src/mod.rs\n+++ b/src/mod.rs\n@@ -529,7 +529,7 @@ pub fn render(facts: &Facts) -> String {\n-    old line\n+    new line\n";

    #[test]
    fn a_returned_diff_renders_as_lines_and_never_as_escapes() {
        let value = serde_json::json!({ "diff": DIFF }).to_string();
        let mut lines = Vec::new();
        assert!(push_value(&mut lines, &value, 100, Theme::Neon));
        let drawn = text_of(&lines);
        assert!(!drawn.contains("\\n"), "a diff kept its escapes: {drawn:?}");
        assert!(
            drawn.contains("+    new line") && drawn.contains("-    old line"),
            "a diff lost its gutter lines: {drawn:?}"
        );
        assert!(drawn.contains(" DIFF"), "the field lost its label: {drawn}");
    }

    #[test]
    fn a_scalar_field_is_one_row_without_its_quotes() {
        let value = serde_json::json!({ "choice": "correct", "confidence": 0.98 }).to_string();
        let mut lines = Vec::new();
        assert!(push_value(&mut lines, &value, 80, Theme::Neon));
        let drawn = text_of(&lines);
        assert!(drawn.contains("choice  correct"), "{drawn:?}");
        assert!(
            !drawn.contains('"'),
            "a row kept JSON punctuation: {drawn:?}"
        );
    }

    #[test]
    fn prose_is_not_mistaken_for_a_diff() {
        assert!(!is_diff("- a list item\n- another one"));
        assert!(!is_diff("--- not really\njust text"));
        assert!(is_diff(DIFF));
    }

    #[test]
    fn a_long_field_is_bounded_and_says_how_much_is_left() {
        let body: String = (0..60).map(|i| format!("line {i}\n")).collect();
        let value = serde_json::json!({ "log": body }).to_string();
        let mut lines = Vec::new();
        assert!(push_value(&mut lines, &value, 80, Theme::Neon));
        let drawn = text_of(&lines);
        assert!(drawn.contains("42 more lines"), "{drawn:?}");
    }

    #[test]
    fn a_bare_scalar_is_left_to_the_caller() {
        let mut lines = Vec::new();
        assert!(!push_value(&mut lines, "42", 80, Theme::Neon));
        assert!(!push_value(&mut lines, "not json at all", 80, Theme::Neon));
        assert!(lines.is_empty());
    }

    #[test]
    fn every_theme_inks_a_field_label_with_its_own_accent() {
        for theme in Theme::ALL {
            let mut lines = Vec::new();
            let value = serde_json::json!({ "diff": DIFF }).to_string();
            assert!(push_value(&mut lines, &value, 80, theme));
            let label = lines.first().expect("a label line");
            assert_eq!(label.spans[0].style.fg, Some(theme.accent()), "{theme:?}");
        }
    }
}
