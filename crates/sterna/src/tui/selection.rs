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
//! **It reads the drawn screen, not the model.** The selection is a rectangle
//! of *cells*, taken after everything else is rendered, so what is copied is
//! exactly what is on the screen — wrapping, padding and all — and no part of
//! the transcript needs to know that selecting exists.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

/// A drag in progress or finished, in screen cells.
///
/// `anchor` is where the button went down and `head` is where the pointer is
/// now; either may be the earlier one, because a person drags both ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    pub fn at(column: u16, row: u16) -> Self {
        Self {
            anchor: (column, row),
            head: (column, row),
        }
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
    fn columns(self, row: u16, area: Rect) -> Option<(u16, u16)> {
        let ((ax, ay), (hx, hy)) = self.ordered();
        if row < ay || row > hy || row < area.y || row >= area.bottom() {
            return None;
        }
        let start = if row == ay { ax.max(area.x) } else { area.x };
        let end = if row == hy {
            hx.saturating_add(1).min(area.right())
        } else {
            area.right()
        };
        (start < end).then_some((start, end))
    }
}

/// Reverses the selected cells and returns what they say.
///
/// Trailing blanks are dropped per row: a row is padded to the width of the
/// screen, and copying that padding would paste a wall of spaces.
pub(crate) fn draw(buffer: &mut Buffer, area: Rect, selection: Selection) -> String {
    let mut out = String::new();
    let ((_, top), (_, bottom)) = selection.ordered();
    for row in top..=bottom {
        let Some((start, end)) = selection.columns(row, area) else {
            continue;
        };
        if !out.is_empty() {
            out.push('\n');
        }
        for column in start..end {
            let cell = &mut buffer[(column, row)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            out.push_str(cell.symbol());
        }
        while out.ends_with(' ') {
            out.pop();
        }
    }
    out
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
        let text = draw(
            &mut buffer,
            area,
            Selection {
                anchor: (0, 0),
                head: (3, 0),
            },
        );
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
        let text = draw(
            &mut buffer,
            area,
            Selection {
                anchor: (2, 0),
                head: (2, 2),
            },
        );
        assert_eq!(text, "llo\nsecond\nthi");
    }

    /// Dragging up selects the same text as dragging down over it.
    #[test]
    fn dragging_backwards_selects_the_same_text() {
        let (mut down, area) = screen();
        let forwards = draw(
            &mut down,
            area,
            Selection {
                anchor: (2, 0),
                head: (2, 2),
            },
        );
        let (mut up, area) = screen();
        let backwards = draw(
            &mut up,
            area,
            Selection {
                anchor: (2, 2),
                head: (2, 0),
            },
        );
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn a_rows_padding_is_not_copied() {
        let (mut buffer, area) = screen();
        let text = draw(
            &mut buffer,
            area,
            Selection {
                anchor: (0, 0),
                head: (9, 0),
            },
        );
        assert_eq!(text, "hello", "the row is padded to ten cells");
    }

    #[test]
    fn a_selection_outside_the_area_covers_nothing() {
        let (mut buffer, _) = screen();
        let elsewhere = Rect::new(0, 5, 10, 3);
        let text = draw(
            &mut buffer,
            elsewhere,
            Selection {
                anchor: (0, 0),
                head: (9, 0),
            },
        );
        assert!(text.is_empty());
        assert!(
            !buffer[(0, 0)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }
}
