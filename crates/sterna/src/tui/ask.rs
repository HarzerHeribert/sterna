//! The panel a question is put on: what the program asked, what it will
//! accept as an answer, and — when `[ask] jev = "weight"` — how the decision
//! model reads each option.
//!
//! **Every route is a keyboard route.** `hit.rs`'s rule is that the mouse is
//! a second way to an action the keyboard already has, and a modal question
//! is exactly where breaking it would strand somebody: digits and the arrow
//! keys select, Enter confirms, Escape answers "decide yourself". Nothing
//! here is reachable by pointer alone.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::Theme;
use crate::ask::Request;

/// Draws `request` over the screen, with `selected` marked.
pub fn render(frame: &mut Frame<'_>, request: &Request, selected: usize, theme: Theme) {
    let question = request.question();
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let rows = u16::try_from(question.choices.len()).unwrap_or(u16::MAX);
    // Question, a blank line, one row per choice, a blank line, the footer.
    let height = area.height.saturating_sub(2).min(rows.saturating_add(8));
    let overlay = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, overlay);
    let block = Block::default()
        .title(" ⠿ sterna is asking ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent()));
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            question.question.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for (index, choice) in question.choices.iter().enumerate() {
        lines.push(choice_line(
            request,
            index,
            choice,
            index == selected,
            theme,
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        footer(request),
        Style::default().fg(theme.hush()),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The prompt behind `[a]` on an approval: what to do instead. `text` is
/// what has been typed so far; an empty send asks Sterna to propose.
pub fn render_redirect(frame: &mut Frame<'_>, text: &str, theme: Theme) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(100);
    let height = area.height.saturating_sub(2).min(8);
    let overlay = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, overlay);
    let block = Block::default()
        .title(" ⠿ ask Sterna to do it another way ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent()));
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            "The call is refused either way. What should Sterna do instead?",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(theme.accent())),
            Span::raw(text.to_string()),
            Span::styled("▏", Style::default().fg(theme.accent())),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Enter sends · empty asks Sterna to propose · Esc goes back to the call",
            Style::default().fg(theme.hush()),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// One choice row: its digit, its text, and the decision model's share of it
/// when there is one.
fn choice_line(
    request: &Request,
    index: usize,
    choice: &str,
    selected: bool,
    theme: Theme,
) -> Line<'static> {
    let mark = if selected { "▶" } else { " " };
    let style = if selected {
        Style::default()
            .fg(theme.accent())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let mut spans = vec![Span::styled(
        format!("{mark} {}. {choice}", index + 1),
        style,
    )];
    if let Some(weight) = request
        .weights()
        .and_then(|weights| weights.probabilities.get(index))
    {
        spans.push(Span::styled(
            format!("  {:.0}%", weight * 100.0),
            Style::default().fg(theme.hush()),
        ));
    }
    Line::from(spans)
}

/// The keys, and what the decision model would have done when it was asked.
fn footer(request: &Request) -> String {
    let keys = "1-9 or ↑/↓ choose · Enter confirms · Esc: decide yourself";
    match request.weights() {
        Some(weights) => format!(
            "decision model: {} ({:.2})\n{keys}",
            weights.choice, weights.confidence
        ),
        None => keys.to_string(),
    }
}

/// Where a key press moves the selection, or what it answers.
///
/// Returned rather than applied so the event loop owns the queue and this
/// module owns nothing: the same split `render_approval` and its loop keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Move(usize),
    Confirm,
    Dismiss,
    Ignored,
}

/// Reads one key against a question of `choices` answers, with `selected`
/// currently marked.
#[must_use]
pub fn key(code: crossterm::event::KeyCode, selected: usize, choices: usize) -> Key {
    use crossterm::event::KeyCode;
    match code {
        KeyCode::Up | KeyCode::Char('k') => Key::Move(selected.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            Key::Move((selected + 1).min(choices.saturating_sub(1)))
        }
        KeyCode::Char(digit @ '1'..='9') => {
            let index = usize::from(digit as u8 - b'1');
            if index < choices {
                Key::Move(index)
            } else {
                Key::Ignored
            }
        }
        KeyCode::Enter => Key::Confirm,
        KeyCode::Esc => Key::Dismiss,
        _ => Key::Ignored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn the_arrows_and_the_digits_reach_every_choice_and_stop_at_the_ends() {
        assert_eq!(key(KeyCode::Down, 0, 3), Key::Move(1));
        assert_eq!(key(KeyCode::Down, 2, 3), Key::Move(2), "the last row holds");
        assert_eq!(key(KeyCode::Up, 0, 3), Key::Move(0), "the first row holds");
        assert_eq!(key(KeyCode::Char('3'), 0, 3), Key::Move(2));
        assert_eq!(
            key(KeyCode::Char('4'), 0, 3),
            Key::Ignored,
            "a digit past the last choice chooses nothing"
        );
    }

    /// Escape answers the question rather than cancelling it: a model left
    /// waiting on a dismissed question would ask again.
    #[test]
    fn escape_is_an_answer_and_enter_is_the_other_one() {
        assert_eq!(key(KeyCode::Esc, 1, 3), Key::Dismiss);
        assert_eq!(key(KeyCode::Enter, 1, 3), Key::Confirm);
        assert_eq!(key(KeyCode::Char('x'), 1, 3), Key::Ignored);
    }
}
