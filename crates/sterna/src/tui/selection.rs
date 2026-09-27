//! Selecting text with the pointer, inside Sterna rather than in the terminal.
//!
//! **Why Sterna does this itself.** While mouse reporting is on, a terminal
//! hands the pointer to the application and offers its own selection only
//! behind a modifier — which is why selecting used to mean Cmd-drag, or
//! releasing the mouse with Ctrl-G first. The user's ruling of 2026-09-18 is
//! that a plain click-and-drag must select. A terminal cannot be asked for
//! that, so Sterna asks for `?1002` (motion while a button is held) and draws
//! the selection itself.
//!
//! **It reads the drawn screen, not the model**, so what is copied is what is
//! on the screen, wrapping and all. But it is held to the text it was made
//! over: it stays inside the region its anchor fell in (the transcript, not
//! the sidebar beside it), a selection in the transcript moves with the
//! transcript when it scrolls, and the frame a card is drawn in is not part
//! of what it says.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

/// A drag in progress or finished.
///
/// `anchor` is where the button went down and `head` is where the pointer is
/// now; either may be the earlier one, because a person drags both ways.
/// Rows are screen rows as they stood when the transcript's top row was
/// `top`; drawn later, they move by however far the transcript has moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
    /// The region the anchor fell in; the selection never leaves it.
    /// `None` is the whole screen.
    pub region: Option<Rect>,
    /// The transcript's top document row when these rows were recorded,
    /// for a selection made in the transcript.
    pub top: Option<usize>,
}

impl Selection {
    pub fn at(column: u16, row: u16) -> Self {
        Self::span((column, row), (column, row))
    }

    /// A selection between two cells, on the whole screen.
    pub fn span(anchor: (u16, u16), head: (u16, u16)) -> Self {
        Self {
            anchor,
            head,
            region: None,
            top: None,
        }
    }

    /// Moves the head to a cell of the screen as it is now drawn, whose
    /// transcript starts at document row `top`.
    pub fn extend(&mut self, column: u16, row: u16, top: usize) {
        let shift = self.top.map_or(0, |then| top as i64 - then as i64);
        let row = (i64::from(row) + shift).clamp(0, i64::from(u16::MAX)) as u16;
        self.head = (column, row);
    }

    /// The two ends as they fall on the screen now, whose transcript starts
    /// at document row `top`: moved by however far it has scrolled since,
    /// and possibly off the screen, where they cover nothing.
    fn placed(self, top: usize) -> ((u16, i64), (u16, i64)) {
        let shift = self.top.map_or(0, |then| then as i64 - top as i64);
        let ((ax, ay), (hx, hy)) = self.ordered();
        ((ax, i64::from(ay) + shift), (hx, i64::from(hy) + shift))
    }

    /// Whether anything is actually covered. A press with no movement is a
    /// click, and a click is not a selection.
    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    /// The two ends in reading order.
    fn ordered(self) -> ((u16, u16), (u16, u16)) {
        let ((ax, ay), (hx, hy)) = (self.anchor, self.head);
        if (ay, ax) <= (hy, hx) {
            ((ax, ay), (hx, hy))
        } else {
            ((hx, hy), (ax, ay))
        }
    }

    /// The columns of `row` this selection covers, within `area`.
    ///
    /// The first row runs from the anchor to the edge, the last from the edge
    /// to the head, and every row between is whole — the shape a person
    /// expects from dragging across lines, not a rectangle.
    fn columns(
        ((ax, ay), (hx, hy)): ((u16, i64), (u16, i64)),
        row: u16,
        area: Rect,
    ) -> Option<(u16, u16)> {
        let at = i64::from(row);
        if at < ay || at > hy || row < area.y || row >= area.bottom() {
            return None;
        }
        let start = if at == ay { ax.max(area.x) } else { area.x };
        let end = if at == hy {
            hx.saturating_add(1).min(area.right())
        } else {
            area.right()
        };
        (start < end).then_some((start, end))
    }
}

/// The glyphs a card and a rule are drawn with: copied text leaves them out.
const FRAME: &[char] = &['│', '╭', '╮', '╰', '╯', '─', '┴', '┬', '├', '┤', '┃'];

/// A copied row without the frame at its edges: the gutter and a card's
/// side before its text, the side, rule and corner after it.
fn unframed(row: &str) -> String {
    let row = row.trim_end_matches(|c: char| FRAME.contains(&c) || c == ' ');
    match row.trim_start().strip_prefix(|c| FRAME.contains(&c)) {
        Some(rest) => rest.to_string(),
        None => row.to_string(),
    }
}

/// The copied rows as text: the first from where the drag began, the rest
/// keeping the indentation they have relative to one another.
fn joined(rows: Vec<String>) -> String {
    let indent = rows
        .iter()
        .skip(1)
        .filter(|row| !row.trim().is_empty())
        .map(|row| row.len() - row.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    rows.iter()
        .enumerate()
        .map(|(i, row)| match i {
            0 => row.trim_start(),
            _ => row.get(indent..).unwrap_or("").trim_end(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reverses the selected cells and returns what they say.
///
/// Trailing blanks are dropped per row: a row is padded to the width of the
/// screen, and copying that padding would paste a wall of spaces.
pub(crate) fn draw(buffer: &mut Buffer, area: Rect, selection: Selection, top: usize) -> String {
    let area = selection
        .region
        .map_or(area, |region| region.intersection(area));
    let ends = selection.placed(top);
    let mut rows = Vec::new();
    for row in area.y..area.bottom() {
        let Some((start, end)) = Selection::columns(ends, row, area) else {
            continue;
        };
        let mut text = String::new();
        for column in start..end {
            let cell = &mut buffer[(column, row)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            text.push_str(cell.symbol());
        }
        rows.push(unframed(&text));
    }
    joined(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> (Buffer, Rect) {
        let area = Rect::new(0, 0, 10, 3);
        let mut buffer = Buffer::empty(area);
        for (row, text) in ["hello     ", "second    ", "third     "]
            .iter()
            .enumerate()
        {
            for (column, ch) in text.chars().enumerate() {
                buffer[(column as u16, row as u16)].set_symbol(&ch.to_string());
            }
        }
        (buffer, area)
    }

    #[test]
    fn a_press_with_no_movement_is_not_a_selection() {
        assert!(Selection::at(4, 2).is_empty());
    }

    #[test]
    fn one_rows_selection_is_the_text_between_the_ends() {
        let (mut buffer, area) = screen();
        let text = draw(&mut buffer, area, Selection::span((0, 0), (3, 0)), 0);
        assert_eq!(text, "hell");
        assert!(
            buffer[(3, 0)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !buffer[(4, 0)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    /// The shape a person expects from dragging down: to the edge, whole
    /// rows, then to the head — never a rectangle of columns.
    #[test]
    fn a_selection_across_rows_takes_whole_lines_in_between() {
        let (mut buffer, area) = screen();
        let text = draw(&mut buffer, area, Selection::span((2, 0), (2, 2)), 0);
        assert_eq!(text, "llo\nsecond\nthi");
    }

    /// Dragging up selects the same text as dragging down over it.
    #[test]
    fn dragging_backwards_selects_the_same_text() {
        let (mut down, area) = screen();
        let forwards = draw(&mut down, area, Selection::span((2, 0), (2, 2)), 0);
        let (mut up, area) = screen();
        let backwards = draw(&mut up, area, Selection::span((2, 2), (2, 0)), 0);
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn a_rows_padding_is_not_copied() {
        let (mut buffer, area) = screen();
        let text = draw(&mut buffer, area, Selection::span((0, 0), (9, 0)), 0);
        assert_eq!(text, "hello", "the row is padded to ten cells");
    }

    #[test]
    fn a_selection_outside_the_area_covers_nothing() {
        let (mut buffer, _) = screen();
        let elsewhere = Rect::new(0, 5, 10, 3);
        let text = draw(&mut buffer, elsewhere, Selection::span((0, 0), (9, 0)), 0);
        assert!(text.is_empty());
        assert!(
            !buffer[(0, 0)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }
}
