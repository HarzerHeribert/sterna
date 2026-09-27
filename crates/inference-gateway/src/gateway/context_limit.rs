//! What a provider says when a request is too long for the route it took.
//!
//! **A refusal is the only place the real window is stated.** A catalogue
//! publishes a window for a *model*; a provider enforces one for an *account
//! on a route*, and the two disagree often enough to matter — of 223 exact
//! name matches between two published catalogues on 2026-09-17, 126 disagreed,
//! because a re-host caps what it resells. The refusal is the route answering
//! for itself, so it outranks every table (`archive/glasshouse:docs/product/design-decisions.md`,
//! *A context window is a property of the route, not of the model*).
//!
//! **A shape this table does not know yields nothing.** Every reading here is
//! a literal marker followed by an integer in the provider's own sentence; a
//! message that matches no marker is not guessed at, because a wrong window
//! is worse than an admitted absence — the meter would then be confidently
//! wrong about how much room is left, which is the one failure the display
//! rule exists to prevent.

/// The smallest figure this module will believe.
///
/// Every marker below is followed by a token count, but a sentence can carry
/// other integers (a request id, a retry delay, a count of messages), and a
/// scan that reads the first integer after a marker will occasionally read
/// one of those. No route in existence enforces a context window of a few
/// hundred tokens, so a reading under this floor is a misread rather than a
/// small window, and it is discarded.
const SMALLEST_BELIEVABLE_WINDOW: u64 = 1024;

/// How much of a refusal is read at all.
///
/// A provider's error object is small; a body larger than this is not a
/// refusal message but something else wearing a 4xx, and scanning it whole
/// would make the cost of a malformed response unbounded.
pub const SCAN_LIMIT_BYTES: usize = 8 * 1024;

/// The markers, in the order they are tried, and what each is followed by.
///
/// The shapes, with the protocol whose providers state them:
///
/// - `openai-chat` and `openai-responses`: *"This model's maximum context
///   length is 128000 tokens. However, your messages resulted in …"* — the
///   limit follows the marker directly.
/// - `anthropic-messages`: *"prompt is too long: 213000 tokens > 200000
///   maximum"* and *"input length and `max_tokens` exceed context limit:
///   190000 + 20000 > 200000, decrease input length …"* — the limit follows
///   the `>`, so the marker is matched to arm the reading and the figure is
///   taken after the last `>` in the message.
///
/// A fourth family — Gemini's — is deliberately absent: this gateway speaks
/// `gemini-generate-content`, but its over-length refusal wording is not
/// confirmed by any fixture in this repository, and an unconfirmed pattern is
/// a guess with a provider's name on it.
const MARKERS: &[Marker] = &[
    Marker {
        text: "maximum context length is",
        figure: Figure::Next,
    },
    Marker {
        text: "prompt is too long",
        figure: Figure::AfterLastAngle,
    },
    Marker {
        text: "exceed context limit",
        figure: Figure::AfterLastAngle,
    },
];

#[derive(Debug, Clone, Copy)]
struct Marker {
    text: &'static str,
    figure: Figure,
}

/// Where the figure sits relative to the marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Figure {
    /// The first integer after the marker itself.
    Next,
    /// The first integer after the last `>` in the message — the shape where
    /// the sentence states what was sent before it states what is allowed.
    AfterLastAngle,
}

/// The window `body` states, or `None`.
///
/// `body` is a provider's refusal as it arrived. Nothing but the integer is
/// kept: this returns a number, never a borrowed or owned piece of the
/// provider's text, which is what keeps foreign text out of everything
/// downstream.
#[must_use]
pub fn stated_limit(body: &str) -> Option<u64> {
    // Cut at a character boundary: a provider's message may be UTF-8 and a
    // byte slice through the middle of a character would panic on a body
    // this module is supposed to read defensively.
    let mut cut = body.len().min(SCAN_LIMIT_BYTES);
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    let scanned = &body[..cut];
    // Case-insensitive because the same provider varies the capital on
    // "This model's" between endpoints, and a marker is a sentence fragment
    // rather than a protocol token.
    let lowered = scanned.to_ascii_lowercase();
    for marker in MARKERS {
        let Some(at) = lowered.find(marker.text) else {
            continue;
        };
        let rest = &lowered[at + marker.text.len()..];
        let found = match marker.figure {
            Figure::Next => first_integer(rest),
            Figure::AfterLastAngle => rest
                .rfind('>')
                .and_then(|angle| first_integer(&rest[angle..])),
        };
        if let Some(figure) = found.filter(|figure| *figure >= SMALLEST_BELIEVABLE_WINDOW) {
            return Some(figure);
        }
    }
    None
}

/// The first run of digits in `text`, as a number.
///
/// Separators are skipped rather than parsed: `128,000` and `128000` are the
/// same figure, and a provider that writes one is not stating a different
/// limit from one that writes the other.
fn first_integer(text: &str) -> Option<u64> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let mut figure: u64 = 0;
    for byte in text[start..].bytes() {
        match byte {
            b'0'..=b'9' => {
                figure = figure
                    .checked_mul(10)?
                    .checked_add(u64::from(byte - b'0'))?;
            }
            b',' | b'_' => {}
            _ => break,
        }
    }
    Some(figure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_openai_families_refusal_states_its_limit() {
        let body = r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 130512 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#;
        assert_eq!(stated_limit(body), Some(128_000));
    }

    #[test]
    fn the_anthropic_refusal_states_the_figure_after_the_angle_not_the_one_before() {
        let body = r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213000 tokens > 200000 maximum"}}"#;
        assert_eq!(
            stated_limit(body),
            Some(200_000),
            "213000 is what was sent; 200000 is what the route allows"
        );
    }

    #[test]
    fn the_other_anthropic_shape_states_it_after_the_sum() {
        let body = r#"{"error":{"message":"input length and `max_tokens` exceed context limit: 190000 + 20000 > 200000, decrease input length or max_tokens and try again"}}"#;
        assert_eq!(stated_limit(body), Some(200_000));
    }

    #[test]
    fn a_thousands_separator_is_the_same_figure() {
        assert_eq!(
            stated_limit("This model's maximum context length is 128,000 tokens."),
            Some(128_000)
        );
    }

    #[test]
    fn a_refusal_this_table_does_not_know_yields_nothing_rather_than_a_guess() {
        for body in [
            r#"{"error":{"message":"rate limit exceeded, retry after 30 seconds"}}"#,
            r#"{"error":{"message":"invalid api key"}}"#,
            r#"{"error":{"message":"the model `gpt-9` does not exist"}}"#,
            "",
            "{}",
        ] {
            assert_eq!(stated_limit(body), None, "{body} is not a window");
        }
    }

    #[test]
    fn a_figure_too_small_to_be_a_window_is_a_misread_and_is_discarded() {
        assert_eq!(
            stated_limit("prompt is too long: 5 tokens > 8 maximum"),
            None,
            "no route enforces an eight-token window; this is a misread"
        );
    }

    #[test]
    fn only_the_head_of_an_oversized_body_is_scanned() {
        let mut body = "x".repeat(SCAN_LIMIT_BYTES);
        body.push_str(" This model's maximum context length is 128000 tokens.");
        assert_eq!(
            stated_limit(&body),
            None,
            "a body this large is not a refusal message"
        );
    }

    #[test]
    fn a_capital_at_the_start_of_the_sentence_does_not_hide_the_marker() {
        assert_eq!(
            stated_limit("Maximum Context Length Is 200000 tokens"),
            Some(200_000)
        );
    }
}
