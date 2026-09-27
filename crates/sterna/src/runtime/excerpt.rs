//! Bounded, line-numbered views over text already admitted by `read`.

pub const DEFAULT_LINES: usize = 400;
pub const MAX_LINES: usize = 1_000;
/// Fits a modest source file while leaving room in the cell's console budget
/// for another useful result and explicit framing.
pub const MAX_OUTPUT_CHARS: usize = 24 * 1024;
pub const MAX_LINE_UNITS: usize = 2 * 1024;
const CONTINUATION_UNITS: usize = 1_600;

#[derive(Debug, Clone)]
pub(crate) struct SampledLine {
    pub(crate) text: String,
    pub(crate) consumed_units: usize,
    pub(crate) omitted_units: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Excerpt {
    pub(crate) text: String,
    pub(crate) start: usize,
    pub(crate) end: Option<usize>,
    pub(crate) line_count: usize,
    pub(crate) next: Option<usize>,
    pub(crate) truncated_lines: usize,
}

/// Formats sampled lines without ever exceeding [`MAX_OUTPUT_CHARS`].
pub(crate) fn render(
    line_count: usize,
    start: usize,
    requested: usize,
    samples: Vec<SampledLine>,
) -> Excerpt {
    let start = start.max(1);
    let requested = requested.clamp(1, MAX_LINES);
    if start > line_count {
        return Excerpt {
            text: format!("[lines {start}-0 of {line_count}]\n[end of file]\n"),
            start,
            end: None,
            line_count,
            next: None,
            truncated_lines: 0,
        };
    }

    let width = line_count.to_string().len();
    let mut body = String::new();
    let mut end = None;
    let mut truncated_lines = 0;
    let wanted_end = start.saturating_add(requested - 1).min(line_count);

    for (offset, sample) in samples.into_iter().enumerate() {
        let number = start + offset;
        if number > wanted_end {
            break;
        }
        let suffix = if sample.omitted_units == 0 {
            String::new()
        } else {
            let continuation_end = sample
                .consumed_units
                .saturating_add(sample.omitted_units.min(CONTINUATION_UNITS));
            format!(
                " … [line continues on this File at .lines[{}].slice({}, {})]",
                number - 1,
                sample.consumed_units,
                continuation_end
            )
        };
        let prefix = format!("{number:>width$} | ");
        // Reserve room for both metadata lines before adding another body
        // line. The exact footer is rebuilt below.
        let available = MAX_OUTPUT_CHARS
            .saturating_sub(body.chars().count())
            .saturating_sub(180);
        let full_len =
            prefix.chars().count() + sample.text.chars().count() + suffix.chars().count() + 1;
        if full_len <= available {
            truncated_lines += usize::from(sample.omitted_units > 0);
            body.push_str(&prefix);
            body.push_str(&sample.text);
            body.push_str(&suffix);
            body.push('\n');
            end = Some(number);
            continue;
        }

        // Start the next page at a whole line instead of cutting an ordinary
        // line merely to fill the remaining space on this page.
        if !body.is_empty() {
            break;
        }

        let marker_prefix = " … [line continues on this File at .lines[";
        let marker_tail = "].slice(START, END)]\n";
        let marker_room = marker_prefix.chars().count()
            + (number - 1).to_string().len()
            + marker_tail.chars().count();
        let room = available.saturating_sub(prefix.chars().count() + marker_room);
        if room > 0 {
            let shown: String = sample.text.chars().take(room).collect();
            let column = shown.encode_utf16().count();
            let continuation_end = column
                .saturating_add(CONTINUATION_UNITS)
                .min(sample.consumed_units.saturating_add(sample.omitted_units));
            let marker = format!(
                " … [line continues on this File at .lines[{}].slice({column}, {continuation_end})]\n",
                number - 1,
            );
            body.push_str(&prefix);
            body.push_str(&shown);
            body.push_str(&marker);
            end = Some(number);
            truncated_lines += 1;
        }
        break;
    }

    let next = end.and_then(|end| (end < line_count).then_some(end + 1));
    let shown_end = end.unwrap_or(start.saturating_sub(1));
    let header = format!("[lines {start}-{shown_end} of {line_count}]\n");
    let footer = next.map_or_else(
        || "[end of file]\n".to_string(),
        |next| {
            format!("[next: call .excerpt({{start: {next}, lines: {requested}}}) on this File]\n")
        },
    );
    let mut text = header;
    text.push_str(&body);
    text.push_str(&footer);
    debug_assert!(text.chars().count() <= MAX_OUTPUT_CHARS);
    Excerpt {
        text,
        start,
        end,
        line_count,
        next,
        truncated_lines,
    }
}
