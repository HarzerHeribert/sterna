//! Local notebook inspection. Nothing here is sent to the model.
use super::lane::helper_seconds;
use super::*;

#[derive(Debug, Clone, Default)]
pub struct Inspection {
    /// One-based notebook cell number, matching the conversation.
    pub cell: usize,
    /// Wrapped rows from the top of this cell.
    pub scroll: usize,
}

impl Inspection {
    pub fn open(cell: usize, notebook: &Notebook) -> Option<Self> {
        notebook
            .cell(cell)
            .filter(|view| Self::recorded(view))
            .map(|_| Self { cell, scroll: 0 })
    }

    fn recorded(view: &CellView) -> bool {
        view.executed_source.is_some()
            || view.execution.is_some()
            || view.error.is_some()
            || view.output.is_some()
            || view.returned.is_some()
    }

    pub fn latest(notebook: &Notebook) -> Option<usize> {
        notebook
            .cells
            .iter()
            .rposition(Self::recorded)
            .map(|index| index + 1)
    }

    pub fn clamp(
        &mut self,
        conversation: &Conversation,
        notebook: &Notebook,
        width: u16,
        height: u16,
    ) {
        let count = wrap_lines(
            content(conversation, notebook, self.cell),
            width.saturating_sub(1),
        )
        .len();
        self.scroll = self
            .scroll
            .min(count.saturating_sub(usize::from(height.saturating_sub(3))));
    }

    pub fn adjacent(&mut self, forward: bool, notebook: &Notebook) {
        let next = if forward {
            ((self.cell + 1)..=notebook.cells.len())
                .find(|cell| notebook.cell(*cell).is_some_and(Self::recorded))
        } else {
            (1..self.cell)
                .rev()
                .find(|cell| notebook.cell(*cell).is_some_and(Self::recorded))
        };
        if let Some(next) = next {
            self.cell = next;
        }
        self.scroll = 0;
    }
}

fn cell_message<'a>(
    conversation: &'a Conversation,
    notebook: &Notebook,
    wanted: usize,
) -> Option<&'a Message> {
    let mut ordinal = 0;
    let mut after_return = false;
    for message in conversation.messages.iter().skip(1) {
        match message.role {
            Role::Assistant if after_return => after_return = false,
            Role::Assistant => {
                ordinal += 1;
                if ordinal == wanted {
                    return Some(message);
                }
                after_return = notebook
                    .cell(ordinal)
                    .is_some_and(|view| view.returned.is_some());
            }
            Role::User if is_tool_feedback(message) => {}
            Role::User => after_return = false,
        }
    }
    None
}

fn section(lines: &mut Vec<Line<'static>>, name: &str, text: Option<&str>) {
    lines.push(Line::default());
    lines.push(Line::styled(
        name.to_string(),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ));
    push_text_region(
        lines,
        text.filter(|text| !text.is_empty())
            .unwrap_or("None recorded."),
    );
}

/// `HELPERS · what was asked and what came back` -- one block per call the
/// cell made, in call order.
///
/// The toolset comes from the roster rather than from the record, so a
/// helper that held tools is visible as having held them. A failed call is
/// labelled a failure here too: what came back was not an answer.
fn helpers_block(view: &CellView) -> Option<String> {
    if view.helpers.is_empty() {
        return None;
    }
    let mut block = String::new();
    for record in &view.helpers {
        let tools = match crate::helpers::lookup(&record.helper) {
            Some(spec) if spec.tools.is_empty() => "no tools".to_string(),
            Some(spec) => spec.tools.join(" "),
            None => "toolset unknown".to_string(),
        };
        block.push_str(&format!(
            "  {} · {} · {} turn{} · {tools}\n",
            record.helper,
            helper_seconds(record.outcome.elapsed_ms),
            record.turns,
            if record.turns == 1 { "" } else { "s" },
        ));
        push_labelled(&mut block, "asked", &record.asked);
        // `looked` is what earns this section: a helper reports numbers it
        // says it computed, and this is the only place that claim can be
        // checked. Counted rather than listed, so a long trajectory stays one
        // line -- the shape §9.4 already uses.
        if !record.looked.is_empty() {
            let mut counts: std::collections::BTreeMap<&str, usize> =
                std::collections::BTreeMap::new();
            for tool in &record.looked {
                *counts.entry(tool.as_str()).or_default() += 1;
            }
            let summary = counts
                .iter()
                .map(|(tool, n)| {
                    if *n == 1 {
                        (*tool).to_string()
                    } else {
                        format!("{tool} x{n}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" · ");
            push_labelled(&mut block, "looked", &summary);
        }
        let (label, text) = if record.outcome.ok {
            ("gave", record.outcome.text.as_str())
        } else if record.outcome.text.is_empty() {
            ("running", "no answer yet")
        } else {
            ("failed", record.outcome.text.as_str())
        };
        push_labelled(&mut block, label, text);
    }
    Some(block)
}

/// One labelled entry, its continuation lines under the value's own column.
fn push_labelled(block: &mut String, label: &str, text: &str) {
    for (row, line) in text.lines().enumerate() {
        if row == 0 {
            block.push_str(&format!("    {label:<8} {line}\n"));
        } else {
            block.push_str(&format!("             {line}\n"));
        }
    }
}

fn content(conversation: &Conversation, notebook: &Notebook, cell: usize) -> Vec<Line<'static>> {
    let Some(view) = notebook.cell(cell) else {
        return vec![Line::from("No recorded cell at this number.")];
    };
    let source = view
        .executed_source
        .clone()
        .or_else(|| cell_message(conversation, notebook, cell).map(input_region));
    // The heading says who wrote what is shown. A lowered frame's source is
    // Sterna's spelling of the model's own direct calls, and calling it the
    // model's original source would be the screen misreporting the turn
    // (`tool-abi.md` §19).
    let heading = match view.origin {
        crate::abi::Origin::DirectTool => "CODE · lowered from direct tool calls",
        crate::abi::Origin::LittleHelper => "CODE · helper request",
        crate::abi::Origin::AuthoredCell => "CODE · original source",
    };
    let mut lines = vec![Line::styled(
        heading,
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    )];
    if let Some(source) = source {
        let digits = source.lines().count().max(1).to_string().len();
        for (i, mut line) in markdown::code(&source).into_iter().enumerate() {
            line.spans.insert(
                0,
                Span::styled(format!("{:>digits$} │ ", i + 1), Style::default().fg(MUTED)),
            );
            lines.push(line);
        }
    } else {
        lines.push(Line::from("Source not available in this notebook."));
    }
    section(
        &mut lines,
        "ACTUAL CALLS · runtime evidence",
        view.execution.as_deref(),
    );
    if let Some(helpers) = helpers_block(view) {
        section(
            &mut lines,
            "HELPERS · what was asked and what came back",
            Some(&helpers),
        );
    }
    section(
        &mut lines,
        "CONSOLE OUTPUT · recorded cell output",
        view.stdout.as_deref(),
    );
    section(
        &mut lines,
        "NOTEBOOK OUTPUT · task continues",
        view.output.as_deref(),
    );
    section(
        &mut lines,
        "OBJECTS · snapshot when this cell ended",
        view.table.as_deref(),
    );
    if let Some(changes) = &view.changes {
        push_changes(&mut lines, changes, false);
    }
    if let Some(error) = &view.error {
        section(&mut lines, "ERROR", None);
        lines.pop();
        push_error_region(&mut lines, error);
    }
    if let Some(reason) = &view.yield_reason {
        section(&mut lines, "YIELDED", Some(reason));
    }
    if let Some(returned) = &view.returned {
        section(&mut lines, "RETURN VALUE", Some(returned));
    }
    lines
}

pub(super) fn render(
    frame: &mut Frame,
    area: Rect,
    conversation: &Conversation,
    notebook: &Notebook,
    inspection: &Inspection,
) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Reset)),
        area,
    );
    if area.height < 3 || area.width < 8 {
        return;
    }
    let view = notebook.cell(inspection.cell);
    let (status, color) = if view.is_some_and(|v| v.error.is_some()) {
        ("failed", Color::Red)
    } else if view.is_some_and(|v| v.returned.is_some()) {
        ("returned", ACCENT)
    } else if view.is_some_and(|v| v.execution.is_some()) {
        ("executed", ACCENT)
    } else {
        ("not executed", Color::LightYellow)
    };
    frame.render_widget(
        Paragraph::new(format!(
            "CELL {} / {}  · {status}",
            inspection.cell,
            notebook.cells.len()
        ))
        .style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let body = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(3),
    );
    let rows = wrap_lines(
        content(conversation, notebook, inspection.cell),
        body.width.saturating_sub(1),
    );
    let max_scroll = rows.len().saturating_sub(usize::from(body.height));
    let start = inspection.scroll.min(max_scroll);
    let count = rows.len();
    frame.render_widget(
        Paragraph::new(
            rows.into_iter()
                .skip(start)
                .take(usize::from(body.height))
                .collect::<Vec<_>>(),
        ),
        body,
    );
    // The inspector states its position in words on the hint line below, so
    // it never needs the thumb: the quiet marker for "there is more under
    // this" is the whole of what the overlay adds here.
    super::scroll::render_scrollbar(frame, body, count, start, false);
    let hint = if area.width >= 100 {
        format!(
            "←/→ cell · ↑/↓ or wheel scroll · Home/End · Esc chat   rows {}–{} / {count}",
            start + 1,
            (start + usize::from(body.height)).min(count)
        )
    } else {
        "←/→ cell · ↑/↓ scroll · Esc chat".into()
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(MUTED)),
        Rect::new(area.x, area.bottom() - 1, area.width, 1),
    );
}
