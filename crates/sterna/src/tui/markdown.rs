//! Local presentation of common Markdown; never rewrites model messages.
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

fn inline(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next = ["**", "`", "*"]
            .iter()
            .filter_map(|marker| rest.find(marker).map(|i| (i, *marker)))
            .min_by_key(|(i, marker)| (*i, usize::MAX - marker.len()));
        let Some((i, marker)) = next else {
            spans.push(Span::raw(rest.to_owned()));
            break;
        };
        if i > 0 {
            spans.push(Span::raw(rest[..i].to_owned()));
        }
        let tail = &rest[i + marker.len()..];
        if let Some(end) = tail.find(marker) {
            let style = match marker {
                "**" => Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                "`" => Style::default().fg(Color::LightCyan),
                _ => Style::default().add_modifier(Modifier::ITALIC),
            };
            spans.push(Span::styled(tail[..end].to_owned(), style));
            rest = &tail[end + marker.len()..];
        } else {
            spans.push(Span::raw(format!("{marker}{tail}")));
            break;
        }
    }
    spans
}

fn flow(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut words: Vec<Vec<Span<'static>>> = Vec::new();
    let mut word = Vec::new();
    for span in line.spans {
        for glyph in span.styled_graphemes(line.style) {
            if glyph.symbol.chars().all(char::is_whitespace) {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else {
                word.push(Span::styled(glyph.symbol.to_string(), glyph.style));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let mut out = Vec::new();
    let mut row = Vec::new();
    let mut used = 0;
    for word in words {
        let size = Line::from(word.clone()).width();
        if used > 0 && used + 1 + size > width {
            out.push(Line::from(std::mem::take(&mut row)));
            used = 0;
        }
        if size > width {
            out.extend(super::wrap_lines(vec![Line::from(word)], width as u16));
            continue;
        }
        if used > 0 {
            row.push(Span::raw(" "));
            used += 1;
        }
        row.extend(word);
        used += size;
    }
    if !row.is_empty() || out.is_empty() {
        out.push(Line::from(row));
    }
    out
}

pub(super) fn code(source: &str) -> Vec<Line<'static>> {
    source
        .lines()
        .map(|line| {
            let chars: Vec<char> = line.chars().collect();
            let mut spans = Vec::new();
            let mut i = 0;
            while i < chars.len() {
                let start = i;
                let color = if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
                    i = chars.len();
                    Color::Gray
                } else if matches!(chars[i], '\'' | '"' | '`') {
                    let quote = chars[i];
                    i += 1;
                    while i < chars.len() {
                        if chars[i] == '\\' {
                            i = (i + 2).min(chars.len());
                        } else if chars[i] == quote {
                            i += 1;
                            break;
                        } else {
                            i += 1;
                        }
                    }
                    Color::LightYellow
                } else if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
                    i += 1;
                    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    if [
                        "const",
                        "let",
                        "var",
                        "await",
                        "return",
                        "if",
                        "else",
                        "for",
                        "while",
                        "function",
                        "async",
                        "throw",
                        "new",
                        "try",
                        "catch",
                        "true",
                        "false",
                        "null",
                        "undefined",
                    ]
                    .contains(&word.as_str())
                    {
                        Color::LightMagenta
                    } else {
                        Color::White
                    }
                } else if chars[i].is_ascii_digit() {
                    i += 1;
                    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                        i += 1;
                    }
                    Color::LightCyan
                } else {
                    i += 1;
                    Color::White
                };
                spans.push(Span::styled(
                    chars[start..i].iter().collect::<String>(),
                    Style::default().fg(color),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

fn cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}
fn delimiter(line: &str) -> bool {
    line.contains('|')
        && cells(line).iter().all(|s| {
            let s = s.trim_matches(':');
            s.len() >= 3 && s.chars().all(|c| c == '-')
        })
}

/// Tables keep their columns when they fit; narrow screens use labelled rows.
fn table(out: &mut Vec<Line<'static>>, header: &[&str], rows: &[Vec<&str>], width: usize) {
    let mut widths: Vec<usize> = header
        .iter()
        .map(|s| Line::from(inline(s)).width())
        .collect();
    for row in rows {
        for (i, value) in row.iter().take(widths.len()).enumerate() {
            widths[i] = widths[i].max(Line::from(inline(value)).width());
        }
    }
    if widths.iter().sum::<usize>() + widths.len().saturating_sub(1) * 3 > width {
        for (n, row) in rows.iter().enumerate() {
            if n > 0 {
                out.push(Line::default());
            }
            for (i, value) in row.iter().enumerate() {
                let label = header.get(i).copied().unwrap_or("Value");
                let mut spans = vec![Span::styled(
                    format!("{label}: "),
                    Style::default().fg(Color::LightCyan),
                )];
                spans.extend(inline(value));
                out.push(Line::from(spans));
            }
        }
        return;
    }
    for (n, row) in std::iter::once(header.to_vec())
        .chain(rows.iter().cloned())
        .enumerate()
    {
        let mut spans = Vec::new();
        for (i, col_width) in widths.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
            }
            let value = row.get(i).copied().unwrap_or("");
            let content = inline(value);
            let used = Line::from(content.clone()).width();
            spans.extend(content);
            spans.push(Span::raw(" ".repeat(col_width.saturating_sub(used))));
        }
        let mut line = Line::from(spans);
        if n == 0 {
            line = line.style(
                Style::default()
                    .fg(Color::LightCyan)
                    .add_modifier(Modifier::BOLD),
            );
        }
        out.push(line);
        if n == 0 {
            out.push(Line::styled(
                widths
                    .iter()
                    .map(|w| "─".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("─┼─"),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
}

pub(super) fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let source: Vec<_> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut fence = false;
    while i < source.len() {
        let line = source[i];
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            fence = !fence;
            if fence {
                let language = trimmed.trim_start_matches('`').trim();
                out.push(Line::styled(
                    if language.is_empty() {
                        "CODE".into()
                    } else {
                        language.to_owned()
                    },
                    Style::default().fg(Color::Gray),
                ));
            } else {
                out.push(Line::default());
            }
        } else if fence {
            out.push(Line::styled(
                format!("  {line}"),
                Style::default().fg(Color::LightCyan),
            ));
        } else if i + 1 < source.len() && line.contains('|') && delimiter(source[i + 1]) {
            let header = cells(line);
            i += 2;
            let mut rows = Vec::new();
            while i < source.len() && source[i].contains('|') && !source[i].trim().is_empty() {
                rows.push(cells(source[i]));
                i += 1;
            }
            table(&mut out, &header, &rows, width);
            continue;
        } else if let Some(title) = trimmed.strip_prefix('#').and_then(|s| {
            let s = s.trim_start_matches('#');
            s.strip_prefix(' ')
        }) {
            if out.last().is_some_and(|line: &Line<'_>| line.width() > 0) {
                out.push(Line::default());
            }
            out.push(
                Line::from(inline(title)).style(
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                ),
            );
        } else if matches!(trimmed, "---" | "***" | "___") {
            out.push(Line::styled(
                "─".repeat(width.min(48)),
                Style::default().fg(Color::DarkGray),
            ));
        } else if let Some(item) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let indent = line.len() - line.trim_start().len();
            let mut spans = vec![Span::styled(
                format!("{}• ", " ".repeat(indent)),
                Style::default().fg(Color::LightCyan),
            )];
            spans.extend(inline(item));
            out.push(Line::from(spans));
        } else if let Some(quote) = trimmed.strip_prefix("> ") {
            let mut spans = vec![Span::styled("│ ", Style::default().fg(Color::DarkGray))];
            spans.extend(inline(quote));
            out.push(Line::from(spans));
        } else {
            // **The leading whitespace is kept.** `flow` reads the line as
            // prose: it splits on whitespace and rejoins with single spaces,
            // which is right for a paragraph and wrong for everything that
            // carries structure in its indentation. Pretty-printed JSON is
            // the case that showed it -- every key of an `OUTPUT` block came
            // out on column zero, so the nesting the pretty-printer had just
            // computed was invisible (measured 2026-09-18, from a screen).
            let indent = line.len() - line.trim_start().len();
            if indent == 0 {
                out.extend(flow(Line::from(inline(line)), width.min(110)));
            } else {
                let pad = " ".repeat(indent);
                let body = flow(
                    Line::from(inline(trimmed)),
                    width.min(110).saturating_sub(indent).max(1),
                );
                out.extend(body.into_iter().map(|row| {
                    let mut spans = vec![Span::raw(pad.clone())];
                    spans.extend(row.spans);
                    Line::from(spans).style(row.style)
                }));
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn headings_emphasis_and_code_have_distinct_styles() {
        let lines = render(
            "## Findings\n**Actual result** and `read`\n```ts\nconst x = '**literal**';\n```",
            80,
        );
        assert_eq!(
            text(&lines),
            "Findings\nActual result and read\nts\n  const x = '**literal**';\n"
        );
        assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.style.fg == Some(Color::LightCyan))
        );
    }
    #[test]
    fn tables_align_and_reflow_without_losing_values() {
        let source =
            "| Feature | Result |\n|---|---|\n| Read | Correct |\n| Write | Nested files work |";
        let wide = render(source, 80);
        assert!(text(&wide).contains("Read    │ Correct"));
        let narrow = render(source, 20);
        assert!(text(&narrow).contains("Result: Nested files work"));
        assert!(!text(&narrow).contains("|---|"));
    }
    #[test]
    fn prose_wraps_at_words_and_code_keeps_its_exact_text() {
        let lines = render("One **important finding** deserves attention.", 20);
        assert_eq!(
            text(&lines),
            "One important
finding deserves
attention."
        );
        let source = "const result = await read({path: 'roman.py'}); // inspect";
        assert_eq!(text(&code(source)), source);
    }
}
