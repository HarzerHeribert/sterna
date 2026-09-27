//! Answer prose as it reads: the Markdown a model writes, turned into the
//! document's own tones, never shown as raw markers.
//!
//! **Tones, not colours.** The terminal renderer's Markdown paints fixed
//! colours; the workbench draws every theme, light ones included, so prose
//! here is only ever Normal, Strong, Code, Muted or a link, and the theme
//! decides what those look like.
//!
//! **A code span is one word.** Wrapping never breaks inside `inline code`
//! unless the span alone is wider than the line.

use super::Tone;

/// One run of a line: its text, its tone, and the address it links to.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Piece {
    pub text: String,
    pub tone: Tone,
    pub link: Option<String>,
}

impl Piece {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
            link: None,
        }
    }
}

/// One source line as it is laid out.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Block {
    /// A line of prose: what goes before its first row (a bullet, a
    /// number), and its runs.
    Prose {
        lead: String,
        pieces: Vec<Piece>,
    },
    /// A line inside a fence, or of a table: kept as it is.
    Verbatim(String),
    Blank,
}

/// The blocks of a text, one per source line; fences and tables verbatim.
pub(super) fn blocks(text: &str, tone: Tone) -> Vec<Block> {
    let mut fenced = false;
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || trimmed.starts_with('|') {
            out.push(Block::Verbatim(line.trim_end().to_string()));
            continue;
        }
        if trimmed.is_empty() {
            out.push(Block::Blank);
            continue;
        }
        let indent = " ".repeat(line.len() - trimmed.len());
        let (lead, rest, tone) = if let Some(rest) = heading(trimmed) {
            (String::new(), rest, Tone::Strong)
        } else if let Some(rest) = ["- ", "* ", "+ "]
            .iter()
            .find_map(|bullet| trimmed.strip_prefix(bullet))
        {
            (format!("{indent}• "), rest, tone)
        } else if let Some((number, rest)) = numbered(trimmed) {
            (format!("{indent}{number} "), rest, tone)
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            (format!("{indent}▎ "), rest, Tone::Muted)
        } else {
            (indent, trimmed, tone)
        };
        out.push(Block::Prose {
            lead,
            pieces: inline(rest, tone),
        });
    }
    out
}

fn heading(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    (1..=6)
        .contains(&hashes)
        .then(|| line[hashes..].strip_prefix(' '))
        .flatten()
}

fn numbered(line: &str) -> Option<(&str, &str)> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = line[digits..].strip_prefix(". ")?;
    Some((&line[..digits + 1], rest))
}

/// The runs of one line: `**strong**`, `` `code` ``, `*emphasis*` (shown
/// plain) and `[text](address)`. A marker with no partner is text.
pub(super) fn inline(text: &str, tone: Tone) -> Vec<Piece> {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    let flush = |plain: &mut String, pieces: &mut Vec<Piece>| {
        if !plain.is_empty() {
            pieces.push(Piece::new(std::mem::take(plain), tone));
        }
    };
    while let Some(c) = rest.chars().next() {
        let taken = match c {
            '`' => rest[1..]
                .find('`')
                .map(|end| (Piece::new(&rest[1..1 + end], Tone::Code), &rest[end + 2..])),
            '*' if rest.starts_with("**") => rest[2..].find("**").map(|end| {
                (
                    Piece::new(&rest[2..2 + end], Tone::Strong),
                    &rest[end + 4..],
                )
            }),
            '*' => rest[1..]
                .find('*')
                .filter(|end| *end > 0 && !rest[1..].starts_with(' '))
                .map(|end| (Piece::new(&rest[1..1 + end], tone), &rest[end + 2..])),
            '[' => link(rest).map(|(label, address, after)| {
                (
                    Piece {
                        text: label.to_string(),
                        tone: Tone::Accent,
                        link: Some(address.to_string()),
                    },
                    after,
                )
            }),
            _ => None,
        };
        match taken {
            Some((piece, after)) => {
                flush(&mut plain, &mut pieces);
                pieces.push(piece);
                rest = after;
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    flush(&mut plain, &mut pieces);
    pieces
}

/// `[label](address)` at the start of `text`: the label, the address and
/// what follows.
fn link(text: &str) -> Option<(&str, &str, &str)> {
    let close = text.find("](")?;
    let label = &text[1..close];
    if label.is_empty() || label.contains('[') {
        return None;
    }
    let after = &text[close + 2..];
    let end = after.find(')')?;
    let address = &after[..end];
    (!address.is_empty() && !address.contains(char::is_whitespace)).then_some((
        label,
        address,
        &after[end + 1..],
    ))
}

/// Lays runs out in rows of `width` columns, breaking between words. A
/// code span or a link is one word; a word wider than a row is cut.
pub(super) fn flow(pieces: &[Piece], width: usize) -> Vec<Vec<Piece>> {
    let width = width.max(1);
    // Words: runs of pieces with no space between them.
    let mut words: Vec<Vec<Piece>> = Vec::new();
    let mut word: Vec<Piece> = Vec::new();
    for piece in pieces {
        if piece.tone == Tone::Code || piece.link.is_some() {
            word.push(piece.clone());
            continue;
        }
        let mut parts = piece.text.split(' ').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                word.push(Piece {
                    text: part.to_string(),
                    ..piece.clone()
                });
            }
            if parts.peek().is_some() && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let size = |word: &[Piece]| -> usize { word.iter().map(|p| columns(&p.text)).sum() };
    let mut rows: Vec<Vec<Piece>> = Vec::new();
    let mut row: Vec<Piece> = Vec::new();
    let mut used = 0;
    for word in words {
        let wide = size(&word);
        if used > 0 && used + 1 + wide > width {
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        if wide > width {
            for piece in word {
                for c in piece.text.chars() {
                    let w = columns(&c.to_string());
                    if used + w > width && used > 0 {
                        rows.push(std::mem::take(&mut row));
                        used = 0;
                    }
                    row.push(Piece {
                        text: c.to_string(),
                        ..piece.clone()
                    });
                    used += w;
                }
            }
            continue;
        }
        if used > 0 {
            row.push(Piece::new(" ", word[0].tone));
            used += 1;
        }
        used += wide;
        row.extend(word);
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows.into_iter().map(merged).collect()
}

/// Adjacent runs of one tone and link as one.
fn merged(row: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for piece in row {
        match out.last_mut() {
            Some(last) if last.tone == piece.tone && last.link == piece.link => {
                last.text.push_str(&piece.text);
            }
            _ => out.push(piece),
        }
    }
    out
}

fn columns(text: &str) -> usize {
    ratatui::text::Span::raw(text).width()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rows: &[Vec<Piece>]) -> Vec<String> {
        rows.iter()
            .map(|row| row.iter().map(|p| p.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn markers_become_tones_and_a_link_keeps_its_address() {
        let pieces = inline(
            "Wrote **a.txt**, see `x` and [docs](https://example.com)",
            Tone::Normal,
        );
        let said: String = pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(said, "Wrote a.txt, see x and docs");
        assert!(pieces.contains(&Piece::new("a.txt", Tone::Strong)));
        assert!(pieces.contains(&Piece::new("x", Tone::Code)));
        assert!(
            pieces
                .iter()
                .any(|p| p.text == "docs" && p.link.as_deref() == Some("https://example.com"))
        );
    }

    #[test]
    fn a_lone_marker_is_text() {
        let pieces = inline("2 * 3 and a ` tick", Tone::Normal);
        let said: String = pieces.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(said, "2 * 3 and a ` tick");
    }

    #[test]
    fn a_code_span_is_not_broken_at_a_wrap() {
        let pieces = inline("run `cargo test --lib` now please", Tone::Normal);
        let rows = flow(&pieces, 16);
        assert_eq!(text(&rows), vec!["run", "cargo test --lib", "now please"]);
    }

    #[test]
    fn fences_are_verbatim_and_lists_keep_their_marks() {
        let got = blocks(
            "# Done\n- one\n2. two\n```\nlet x = **1**;\n```",
            Tone::Normal,
        );
        assert_eq!(got.len(), 4);
        assert!(matches!(&got[0], Block::Prose { pieces, .. } if pieces[0].tone == Tone::Strong));
        assert!(matches!(&got[1], Block::Prose { lead, .. } if lead == "• "));
        assert!(matches!(&got[2], Block::Prose { lead, .. } if lead == "2. "));
        assert_eq!(got[3], Block::Verbatim("let x = **1**;".into()));
    }
}
