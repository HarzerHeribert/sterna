//! 2449's whole contract: the two-region screen, and the sidebar collapsing
//! honestly -- by `ServedBy::is_known()` alone -- when the gateway never told
//! Sterna anything. Every test renders into a `TestBackend` buffer; none opens
//! a terminal.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use sterna::contract::{Conversation, Message, Role, ServedBy};
use sterna::helpers::{HelperOutcome, HelperRecord};
use sterna::runtime::handles::{HandleTable, render_table};
use sterna::runtime::preview::{ArrayValue, FileValue, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP, Value};
use sterna::tui::{
    CellError, CellView, Counted, HelperModelTokens, HelperTokens, Notebook, ScreenState,
    SupervisorStatus, TaskTokens, cell_ordinal, render, render_screen,
};

fn conversation(messages: Vec<Message>) -> Conversation {
    Conversation {
        system: String::new(),
        messages,
    }
}

fn known_served_by() -> ServedBy {
    ServedBy {
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-5".to_string()),
        route: None,
        quota_context: Some("pro-plan".to_string()),
        input_tokens: Some(123),
        output_tokens: Some(456),
        cached_input_tokens: None,
    }
}

/// Renders `conversation` and `served_by` into an 80x20 buffer, with no live
/// handles -- the shape every pre-notebook test still exercises.
fn rendered(conversation: &Conversation, served_by: &ServedBy) -> Buffer {
    rendered_with_handles(conversation, served_by, &HandleTable::new())
}

/// Renders `conversation`, `served_by` and `handles` into an 80x20 buffer,
/// with no cell views -- the shape every pre-runtime test still exercises.
fn rendered_with_handles(
    conversation: &Conversation,
    served_by: &ServedBy,
    handles: &HandleTable,
) -> Buffer {
    rendered_notebook(conversation, served_by, handles, &Notebook::default(), 32)
}

/// Renders everything, into a buffer `height` rows tall -- a notebook with
/// its own cell views needs more rows than the two-region tests do.
fn rendered_notebook(
    conversation: &Conversation,
    served_by: &ServedBy,
    handles: &HandleTable,
    notebook: &Notebook,
    height: u16,
) -> Buffer {
    rendered_sized(conversation, served_by, handles, notebook, 120, height)
}

/// The same render into a `width`x20 buffer with no cell views, for a
/// fixture whose lines must not wrap in the conversation column -- a handle
/// header carries its type, length and both token figures on one line
/// (`model-contract.md` §7).
fn rendered_at_width(
    conversation: &Conversation,
    served_by: &ServedBy,
    handles: &HandleTable,
    width: u16,
) -> Buffer {
    rendered_sized(
        conversation,
        served_by,
        handles,
        &Notebook::default(),
        width,
        40,
    )
}

fn rendered_sized(
    conversation: &Conversation,
    served_by: &ServedBy,
    handles: &HandleTable,
    notebook: &Notebook,
    width: u16,
    height: u16,
) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render(frame, conversation, served_by, handles, notebook))
        .unwrap();
    terminal.backend().buffer().clone()
}

/// Every cell's symbol, concatenated row by row with a newline between rows
/// -- a plain string a test can search without caring where a wrap or a
/// border fell.
fn buffer_text(buffer: &Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((x, y)) {
                text.push_str(cell.symbol());
            }
        }
        text.push('\n');
    }
    text
}

#[test]
fn the_status_meter_names_parent_and_complete_helper_spend() {
    let conversation = conversation(vec![Message::text(Role::User, "measure this task")]);
    let notebook = Notebook {
        tokens: Some(TaskTokens {
            used: 201_801,
            parent_used: 173_860,
            helpers: HelperTokens {
                calls: 3,
                usage_known_calls: 3,
                used: 27_941,
                input_tokens: 20_329,
                output_tokens: 2_492,
                requests: 6,
                reported_requests: 6,
                cache_read_input_tokens: 5_120,
                cache_creation_input_tokens: 0,
                cache_read_reported_requests: 6,
                cache_creation_reported_requests: 6,
                models: vec![HelperModelTokens {
                    model: "gpt-5.6-luna".into(),
                    calls: 3,
                    usage_known_calls: 3,
                    used: 27_941,
                    input_tokens: 20_329,
                    output_tokens: 2_492,
                    requests: 6,
                    reported_requests: 6,
                    cache_read_input_tokens: 5_120,
                    cache_creation_input_tokens: 0,
                    cache_read_reported_requests: 6,
                    cache_creation_reported_requests: 6,
                }],
            },
            counted: Counted::Gateway,
        }),
        ..Notebook::default()
    };
    let backend = TestBackend::new(180, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            render_screen(
                frame,
                &conversation,
                &known_served_by(),
                &HandleTable::new(),
                &notebook,
                &ScreenState::default(),
            )
        })
        .unwrap();
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("spent 201.8k"), "{text}");
    assert!(
        text.contains("parent 173.9k + helpers 27.9k · reported"),
        "{text}"
    );
    assert!(!text.contains("helpers 27.9k partial"), "{text}");
}

/// The column each cell in row `y` holds, as one symbol per column -- used
/// to find where a border fell rather than to read prose.
fn row_symbols(buffer: &Buffer, y: u16) -> Vec<String> {
    (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell((x, y))
                .map(|cell| cell.symbol().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// The `count` rows immediately beneath the row containing `marker`, each
/// trimmed of its bordering `│` and padding -- so a cell's output region can
/// be compared against plain text without caring where the border fell.
fn rows_after(buffer: &Buffer, marker: &str, count: usize) -> Vec<String> {
    let text = buffer_text(buffer);
    let lines: Vec<&str> = text.lines().collect();
    let marker_row = lines
        .iter()
        .position(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("{marker:?} not found in buffer:\n{text}"));
    (1..=count)
        .map(|offset| {
            lines
                .get(marker_row + offset)
                .map(|line| {
                    line.strip_prefix(' ')
                        .unwrap_or(line)
                        .trim_start_matches('│')
                        .trim_end_matches(['│', ' '])
                        .to_string()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// The single row beneath the row containing `marker`, trimmed the same way
/// as [`rows_after`] -- for an output region that is exactly one line.
fn line_after(buffer: &Buffer, marker: &str) -> String {
    conversation_rows(buffer, marker, 1).remove(0)
}

/// The `count` rows beneath `marker`, cut to the **conversation column
/// alone** -- so a cell's region can be compared against plain text whatever
/// the sidebar happens to be drawing on the same rows. [`rows_after`] trims
/// borders and cannot: it keeps whatever the sidebar wrote after them.
fn conversation_rows(buffer: &Buffer, marker: &str, count: usize) -> Vec<String> {
    let text = buffer_text(buffer);
    let lines: Vec<&str> = text.lines().collect();
    let marker_row = lines
        .iter()
        .position(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("{marker:?} not found in buffer:\n{text}"));
    (1..=count)
        .map(|offset| {
            lines
                .get(marker_row + offset)
                .map(|line| {
                    let inner = line
                        .strip_prefix(' ')
                        .unwrap_or(line)
                        .trim_start_matches('\u{2502}');
                    let end = inner.find(['\u{2502}', '\u{2503}']).unwrap_or(inner.len());
                    inner[..end].trim_end().to_string()
                })
                .unwrap_or_default()
        })
        .collect()
}

/// The single conversation-column row beneath `marker`.
fn conversation_row(buffer: &Buffer, marker: &str) -> String {
    conversation_rows(buffer, marker, 1).remove(0)
}

/// The x of the sidebar's left border on the top row: the second box-drawing
/// top-left corner from the left, since the conversation column's own border
/// occupies the first.
fn sidebar_left_edge(buffer: &Buffer) -> usize {
    row_symbols(buffer, 2)
        .iter()
        .rposition(|symbol| symbol == "│")
        .expect("wide telemetry has a left separator")
}

#[test]
fn both_regions_render_with_the_conversation_in_order() {
    let conversation = conversation(vec![
        Message::text(Role::User, "hello there"),
        Message::text(Role::Assistant, "general kenobi"),
    ]);
    let text = buffer_text(&rendered(&conversation, &known_served_by()));

    let user_at = text.find("hello there").expect("user message renders");
    let assistant_at = text
        .find("general kenobi")
        .expect("assistant message renders");
    assert!(
        user_at < assistant_at,
        "messages must render in conversation order"
    );
}

#[test]
fn the_sidebar_shows_the_entitlement_that_served_the_last_request() {
    let conversation = conversation(vec![Message::text(Role::User, "hi")]);
    let text = buffer_text(&rendered(&conversation, &known_served_by()));
    assert!(
        text.contains("pro-plan"),
        "the quota_context that served the request must be on screen:\n{text}"
    );
}

#[test]
fn the_sidebar_collapses_rather_than_showing_a_zero() {
    let conversation = conversation(vec![Message::text(Role::User, "hi")]);
    let text = buffer_text(&rendered(&conversation, &ServedBy::default()));

    assert!(
        !text.contains("0 in") && !text.contains("0 out") && !text.contains("tokens: 0"),
        "an unknown ServedBy must render no fabricated zero usage:\n{text}"
    );
    assert!(
        text.contains("gateway not connected"),
        "the collapsed sidebar must say plainly that the gateway is not connected:\n{text}"
    );
}

#[test]
fn the_collapsed_sidebar_gives_its_width_to_the_conversation() {
    use sterna::tui::{ScreenState, SidebarVisibility, screen_regions};
    let area = ratatui::layout::Rect::new(0, 0, 120, 24);
    let shown = screen_regions(area, &ScreenState::default());
    let hidden = screen_regions(
        area,
        &ScreenState {
            sidebar: SidebarVisibility::Hidden,
            ..ScreenState::default()
        },
    );
    assert!(hidden.transcript.width > shown.transcript.width);
    assert_eq!(hidden.details.width, 0);
    assert_eq!(shown.details.width, 34);
}

/// Fullscreen's hide-set is header, status, sidebar and activity ribbon --
/// and never the composer. A screen that cannot be typed into has taken
/// something away rather than given room back, so `input` keeps its rows and
/// the transcript claims everything the four hidden regions gave up.
#[test]
// The *restore* half of fullscreen -- that leaving it gives back exactly the
// layout the user had -- is NOT testable here and deliberately is not tested
// here. `screen_regions` is a pure function of `(area, &ScreenState)` taking
// the state by shared reference, so it cannot write to `sidebar` or
// `status_line`, and any assertion that they survive a toggle holds by
// construction rather than by the implementation being right. The claim is
// carried where the toggle actually runs: `ctrl_f_takes_the_screen_and_gives_
// it_back_with_the_draft_intact` in tests/tui_live.rs, which a one-way-latch
// mutation of Ctrl-F kills.
fn fullscreen_hides_every_chrome_region_and_keeps_the_composer() {
    use sterna::tui::{Activity, ScreenState, screen_regions};
    let area = ratatui::layout::Rect::new(0, 0, 160, 40);
    // Every hidden region is at its widest here: Full status, a shown
    // sidebar, and a moving activity that earns the ribbon.
    let chrome = ScreenState {
        activity: Activity::Streaming,
        sidebar: sterna::tui::SidebarVisibility::Shown,
        ..ScreenState::default()
    };
    let before = screen_regions(area, &chrome);
    assert!(before.header.height > 0, "header should be drawn normally");
    assert!(before.status.height > 0, "status should be drawn normally");
    assert!(before.details.width > 0, "sidebar should be drawn normally");
    assert!(
        before.activity.height > 0,
        "ribbon should be drawn normally"
    );

    let full = screen_regions(
        area,
        &ScreenState {
            fullscreen: true,
            ..chrome.clone()
        },
    );
    assert_eq!(full.header.height, 0, "fullscreen must hide the header");
    assert_eq!(full.status.height, 0, "fullscreen must hide the status bar");
    assert_eq!(full.details.width, 0, "fullscreen must hide the sidebar");
    assert_eq!(full.activity.height, 0, "fullscreen must hide the ribbon");

    assert!(
        full.input.height >= 3,
        "fullscreen must keep the composer, got {:?}",
        full.input
    );
    assert_eq!(
        full.input.height, before.input.height,
        "the composer is not part of the hide-set and must not shrink"
    );
    assert!(
        full.transcript.height > before.transcript.height
            && full.transcript.width > before.transcript.width,
        "the transcript must claim the rows and columns the chrome gave up: \
         {:?} then {:?}",
        before.transcript,
        full.transcript
    );
    assert_eq!(full.transcript.y, area.y, "no header row is left reserved");
    assert_eq!(
        full.transcript.width, area.width,
        "no sidebar column is left reserved"
    );
}

/// The flag overrides the two presentation preferences for as long as it is
/// set and never writes to them, so leaving fullscreen restores the exact
/// layout the user had chosen before entering it.

#[test]
fn a_message_containing_terminal_escapes_cannot_repaint_the_screen() {
    let hostile = "\x1b[2J\x1b[H\x1b[?1049hpwned";
    let conversation = conversation(vec![Message::text(Role::User, hostile)]);

    let buffer = rendered(&conversation, &known_served_by());

    // The escape bytes are rendered as literal, inert content rather than
    // being interpreted -- because `render` never writes a byte to a
    // terminal itself, it only ever hands ratatui's own widgets a `String`.
    // The proof that nothing repainted the screen is that the sidebar,
    // written after the hostile message in the same frame, still renders
    // exactly as it would without it.
    let text = buffer_text(&buffer);
    assert!(
        text.contains("pwned"),
        "the hostile message's own text still renders:\n{text}"
    );
    assert!(
        text.contains("pro-plan"),
        "a message's escape sequences must not be able to blank or move the \
         sidebar drawn in the same frame:\n{text}"
    );
    assert_eq!(buffer.area.width, 120);
    assert_eq!(buffer.area.height, 32);
}

/// `is_known()` must be the predicate, not a stand-in for it.
///
/// The readout's rows are independently nullable, so a request can be metered
/// — tokens counted — while carrying no provider name. Measured: replacing
/// `served_by.is_known()` with `served_by.provider.is_some()` SURVIVED the
/// whole suite, because every other fixture here either fills both or fills
/// neither. Under that substitution this request renders "gateway not
/// connected", which is the honesty failure 2449 forbids, inverted: it tells
/// the reader nothing was connected when in fact something was and it cost
/// tokens.
#[test]
fn a_metered_request_with_no_provider_name_is_still_known() {
    let served_by = ServedBy {
        provider: None,
        model: None,
        route: None,
        quota_context: None,
        input_tokens: Some(123),
        output_tokens: Some(456),
        cached_input_tokens: None,
    };
    assert!(
        served_by.is_known(),
        "the frozen type already says this is known"
    );

    let conversation = conversation(vec![Message::text(Role::User, "hello")]);
    let text = buffer_text(&rendered(&conversation, &served_by));

    assert!(
        !text.contains("gateway not connected"),
        "a metered request rendered as not connected:\n{text}"
    );
    assert!(
        text.contains("123") && text.contains("456"),
        "the tokens that were counted are missing:\n{text}"
    );
}

/// A two-cell task, reused by every test below: a header and two numbered
/// cells, `[1]` and `[2]`.
fn two_cell_task() -> Conversation {
    conversation(vec![
        Message::text(Role::User, "the task"),
        Message::text(
            Role::Assistant,
            "```sterna\nconst first = 'first turn';\n```",
        ),
        Message::text(
            Role::Assistant,
            "```sterna\nconst second = 'second turn';\n```",
        ),
    ])
}

#[test]
fn a_turn_renders_as_a_cell_with_an_input_and_an_output_region() {
    let text = buffer_text(&rendered(&two_cell_task(), &known_served_by()));

    let in1 = text.find("[1] in").expect("cell 1's input header renders");
    let text1 = text.find("first turn").expect("cell 1's text renders");
    let out1 = text
        .find("[1] out")
        .expect("cell 1's output header renders");
    let in2 = text.find("[2] in").expect("cell 2's input header renders");
    let text2 = text.find("second turn").expect("cell 2's text renders");
    let out2 = text
        .find("[2] out")
        .expect("cell 2's output header renders");

    assert!(
        in1 < text1 && text1 < out1 && out1 < in2 && in2 < text2 && text2 < out2,
        "cells must render as [1] in, its text, [1] out, [2] in, its text, \
         [2] out, in that order:\n{text}"
    );
}

#[test]
fn the_latest_cell_shows_the_live_handle_table_through_the_one_renderer() {
    let mut handles = HandleTable::new();
    handles.declare(
        "f",
        Value::File(FileValue {
            path: "a.rs".to_string(),
            byte_len: 10,
            line_count: 1,
            mtime: "t".to_string(),
            lines: vec!["x".to_string()],
        }),
        1,
    );
    handles.declare(
        "arr",
        Value::Array(ArrayValue::sampled(
            2,
            vec![Value::Number(1.0), Value::Number(2.0)],
            None,
        )),
        1,
    );

    let expected = render_table(&handles, PREVIEW_TOKEN_CAP, TABLE_TOKEN_CAP);
    assert!(!expected.is_empty(), "the fixture table must not be empty");
    let expected_lines: Vec<String> = expected.lines().map(str::to_string).collect();

    // 120 columns: the array header is 52 characters and must reach the
    // buffer unwrapped for a line-by-line comparison to mean anything.
    let buffer = rendered_at_width(&two_cell_task(), &known_served_by(), &handles, 120);

    let actual = rows_after(&buffer, "[2] out", expected_lines.len());
    assert_eq!(
        actual, expected_lines,
        "every line of render_table's output must appear beneath [2] out, in order"
    );

    let text = buffer_text(&buffer);
    let out1 = text
        .find("[1] out")
        .expect("cell 1's output header renders");
    let out2 = text
        .find("[2] out")
        .expect("cell 2's output header renders");
    let earlier_region = &text[out1..out2];
    for line in expected_lines.iter().filter(|line| !line.is_empty()) {
        assert!(
            !earlier_region.contains(line.as_str()),
            "no handle content may appear under an earlier cell's output region:\n{earlier_region}"
        );
    }
}

#[test]
fn an_output_region_with_nothing_to_show_says_so_rather_than_collapsing() {
    let buffer = rendered(&two_cell_task(), &known_served_by());

    assert_eq!(
        line_after(&buffer, "[1] out"),
        "(no outputs)",
        "an earlier cell's output region must say so plainly, not collapse"
    );
    assert_eq!(
        line_after(&buffer, "[2] out"),
        "(no outputs)",
        "the latest cell's output region must say so when its table is empty"
    );
}

/// A source scan, not a behavioral test: `tui.rs` may call `render_table`
/// and name the two token caps, and nothing else that would let it turn a
/// handle into text on its own.
///
/// **The scan follows the file's successors.** The 2026-09-17 clickable-TUI
/// move cut `scroll.rs`, `status.rs`, `composer.rs` and — with the
/// cell descriptor, 2026-09-17 — `regions.rs` and — with the transcript's
/// bands, 2026-09-18 — `bands.rs` and — with the question a cell puts to the
/// person, 2026-09-19 — `ask.rs` out of `tui.rs` for the size ratchet; a rule that stopped at the original file would be a rule anything
/// could step around by moving one function. `regions.rs` is where every
/// bounded region of the transcript is drawn, `push_output_region` included,
/// so it is exactly the file this scan must follow.
#[test]
fn the_tui_renders_no_handle_itself() {
    let source = include_str!("../src/tui.rs");

    assert!(
        source.contains("render_table"),
        "tui.rs must draw a handle only through the one renderer, render_table:\n{source}"
    );

    for (name, source) in [
        ("tui.rs", include_str!("../src/tui.rs")),
        ("tui/scroll.rs", include_str!("../src/tui/scroll.rs")),
        ("tui/status.rs", include_str!("../src/tui/status.rs")),
        ("tui/composer.rs", include_str!("../src/tui/composer.rs")),
        ("tui/regions.rs", include_str!("../src/tui/regions.rs")),
        ("tui/bands.rs", include_str!("../src/tui/bands.rs")),
        ("tui/message.rs", include_str!("../src/tui/message.rs")),
        ("tui/poster.rs", include_str!("../src/tui/poster.rs")),
        ("tui/value.rs", include_str!("../src/tui/value.rs")),
        ("tui/ribbon.rs", include_str!("../src/tui/ribbon.rs")),
        ("tui/telemetry.rs", include_str!("../src/tui/telemetry.rs")),
    ] {
        let cleaned = source
            .replace("crate::runtime::preview::PREVIEW_TOKEN_CAP", "")
            .replace("crate::runtime::preview::TABLE_TOKEN_CAP", "")
            // `serde_json::Value` is not a handle's `Value`, and the rule
            // below is about handles: a cell's RETURNED json is rendered by
            // `tui/value.rs`, which is a different thing from the table a
            // handle is drawn in. Stripped rather than allowed by name, so a
            // file reaching for the crate's own `Value::` is still caught.
            .replace("serde_json::Value::", "");
        assert!(
            !cleaned.contains("runtime::preview::"),
            "{name} must not reach into runtime::preview beyond the two token caps:\n{source}"
        );
        assert!(
            !source.contains("render_preview"),
            "{name} must not call render_preview itself -- render_table is the one renderer"
        );
        assert!(
            !cleaned.contains("Value::"),
            "{name} must not match on a handle's Value itself"
        );
    }
}

/// The helper lane lives in `tui/lane.rs` since the Phase 59 ratchet move of
/// 2026-09-13; the scan above follows it there.
#[test]
fn the_helper_lane_renders_no_handle_itself() {
    let source = include_str!("../src/tui/lane.rs");
    assert!(!source.contains("runtime::preview::"), "{source}");
    assert!(!source.contains("render_preview"), "{source}");
    assert!(!source.contains("Value::"), "{source}");
}

#[test]
fn the_sidebar_is_unchanged_by_the_notebook() {
    let baseline = rendered(
        &conversation(vec![Message::text(Role::User, "hi")]),
        &known_served_by(),
    );
    let notebook = rendered(&two_cell_task(), &known_served_by());

    assert_eq!(
        sidebar_left_edge(&notebook),
        sidebar_left_edge(&baseline),
        "the notebook's cells must not change the sidebar's width"
    );

    let text = buffer_text(&notebook);
    assert!(
        text.contains("pro-plan"),
        "the sidebar must still show the entitlement:\n{text}"
    );
    assert!(
        text.contains("anthropic") && text.contains("claude-sonnet-5"),
        "the sidebar must still show the provider and model:\n{text}"
    );
    assert!(
        text.contains("123") && text.contains("456"),
        "the sidebar must still show the tokens:\n{text}"
    );
}

/// One cell that ran a program and ended the task with a value: the input
/// region is the **program**, not the prose the model wrapped it in, and the
/// value is the cell's return region.
///
/// The prose around the block is what makes this test decisive. A notebook
/// that drew the assistant message's whole text -- which is what it did
/// before the runtime existed -- shows the explanation and the fence too, and
/// then there is no way to tell from the screen what actually ran.
#[test]
fn a_cell_shows_its_program_as_the_input_region_and_a_return_as_the_last_cells_value() {
    let message = "Counting them now.\n\n```sterna\nconst n = hits.length;\nreturn { total: n };\n```\n\nThat should do it.";
    let conversation = conversation(vec![
        Message::text(Role::User, "the task"),
        Message::text(Role::Assistant, message),
    ]);

    let mut notebook = Notebook::default();
    notebook.set(
        1,
        CellView {
            description: None,
            helpers: Vec::new(),
            executed_source: None,
            origin: sterna::abi::Origin::AuthoredCell,
            repaired_from: None,
            changes: None,
            stdout: None,
            output: None,
            execution: None,
            call_count: None,
            table: Some("n  number  1195".to_string()),
            error: None,
            returned: Some("\"total\": number".to_string()),
            answered: false,
            yield_reason: None,
            capsule: None,
        },
    );

    let buffer = rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        32,
    );
    let text = buffer_text(&buffer);

    assert_eq!(
        conversation_rows(&buffer, "[1] in", 2),
        vec![
            "const n = hits.length;".to_string(),
            "return { total: n };".to_string()
        ],
        "the input region must be the program the message carried:\n{text}"
    );
    assert!(
        text.contains("Counting them now"),
        "existing narration must remain visible outside the exact program region:\n{text}"
    );
    assert_eq!(
        conversation_row(&buffer, "[1] out"),
        "n  number  1195",
        "the output region is the cell's own handle table:\n{text}"
    );
    assert_eq!(
        conversation_row(&buffer, "[1] return"),
        "\"total\": number",
        "a top-level return renders as the cell's return region:\n{text}"
    );
}

/// A throw is a result: it gets its own region under the cell that threw,
/// carrying the class, the message and the position inside the model's own
/// program -- `runtime-contract.md` §5's first two items and nothing else.
#[test]
fn a_throw_renders_as_the_cells_error_region() {
    let conversation = conversation(vec![
        Message::text(Role::User, "the task"),
        Message::text(Role::Assistant, "```sterna\nnosuch.field;\n```"),
    ]);

    let mut notebook = Notebook::default();
    notebook.set(
        1,
        CellView {
            description: None,
            helpers: Vec::new(),
            executed_source: None,
            origin: sterna::abi::Origin::AuthoredCell,
            repaired_from: None,
            changes: None,
            stdout: None,
            output: None,
            execution: None,
            call_count: None,
            table: Some(String::new()),
            error: Some(CellError {
                class: "ReferenceError".to_string(),
                message: "nosuch is not defined".to_string(),
                line: Some(1),
                column: Some(1),
            }),
            returned: None,
            answered: true,
            yield_reason: None,
            capsule: None,
        },
    );

    let buffer = rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        24,
    );
    let text = buffer_text(&buffer);

    assert_eq!(
        conversation_rows(&buffer, "[1] error", 2),
        vec![
            "ReferenceError: nosuch is not defined".to_string(),
            "line 1, column 1".to_string(),
        ],
        "a throw's class, message and position must be the cell's error region:\n{text}"
    );
    assert_eq!(
        conversation_row(&buffer, "[1] out"),
        "(no outputs)",
        "the cell's own empty table still says so rather than collapsing:\n{text}"
    );
}

/// The runtime's answer to a cell is not a person typing, and every section
/// of it is already drawn above as that cell's own regions. Drawing it again
/// as `you: …` puts the whole handle table on the screen twice.
#[test]
fn the_runtimes_answer_to_a_cell_is_not_drawn_as_a_person_typing() {
    let conversation = conversation(vec![
        Message::text(Role::User, "the task"),
        Message::text(Role::Assistant, "```sterna\nconst n = 1;\n```"),
        Message::text(
            Role::User,
            "[cell 1 yielded in 3 ms]\n\n## Handles\nn  number  1",
        ),
        Message::text(Role::Assistant, "```sterna\nreturn n;\n```"),
    ]);

    let mut notebook = Notebook::default();
    notebook.set(
        1,
        CellView {
            description: None,
            helpers: Vec::new(),
            executed_source: None,
            origin: sterna::abi::Origin::AuthoredCell,
            repaired_from: None,
            changes: None,
            stdout: None,
            output: None,
            execution: None,
            call_count: None,
            table: Some("n  number  1".to_string()),
            error: None,
            returned: None,
            answered: true,
            yield_reason: None,
            capsule: None,
        },
    );

    let text = buffer_text(&rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        24,
    ));

    assert!(
        !text.contains("you:"),
        "the runtime's own answer must not be drawn as a person typing:\n{text}"
    );
    assert_eq!(
        text.matches("n  number  1").count(),
        1,
        "the handle table must appear once, as the cell's output region:\n{text}"
    );
}

/// A person typing between tasks still gets a `you:` line -- the suppression
/// above is the cell's own claim about the message after it, not a blanket
/// rule about user messages.
#[test]
fn a_person_typing_after_a_task_ended_is_still_drawn() {
    let conversation = conversation(vec![
        Message::text(Role::User, "the first task"),
        Message::text(Role::Assistant, "```sterna\nreturn 1;\n```"),
        Message::text(Role::User, "the second task"),
    ]);

    let mut notebook = Notebook::default();
    notebook.set(
        1,
        CellView {
            description: None,
            helpers: Vec::new(),
            executed_source: None,
            origin: sterna::abi::Origin::AuthoredCell,
            repaired_from: None,
            changes: None,
            stdout: None,
            output: None,
            execution: None,
            call_count: None,
            table: Some(String::new()),
            error: None,
            returned: Some("1".to_string()),
            answered: false,
            yield_reason: None,
            capsule: None,
        },
    );

    let text = buffer_text(&rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        24,
    ));

    assert!(
        text.contains("you: the second task"),
        "a task typed after the previous one returned must still be drawn:\n{text}"
    );
}

/// `runtime-contract.md` §9: the terminal response is drawn as the
/// assistant's turn and as the last cell's return region, and is not counted
/// as a cell; a yield's reason is drawn in the output region beside the
/// table, never in an error region.
#[test]
fn a_terminal_response_is_the_assistants_turn_and_a_yield_reason_sits_by_the_table() {
    let conversation = conversation(vec![
        Message::text(Role::User, "the task"),
        Message::text(
            Role::Assistant,
            "```sterna\nconst n = 1;\nyieldNow(\"why\");\n```",
        ),
        Message::text(
            Role::User,
            "[cell 1 yielded in 3 ms]\nwhy\n\n## Handles\nn  number  1",
        ),
        Message::text(Role::Assistant, "```sterna\nreturn \"the answer\";\n```"),
        Message::text(Role::Assistant, "the answer"),
    ]);

    let mut notebook = Notebook::default();
    notebook.set(
        1,
        CellView {
            description: None,
            helpers: Vec::new(),
            executed_source: None,
            origin: sterna::abi::Origin::AuthoredCell,
            repaired_from: None,
            changes: None,
            stdout: None,
            output: None,
            execution: None,
            call_count: None,
            table: Some("n  number  1".to_string()),
            error: None,
            returned: None,
            answered: true,
            yield_reason: Some("why".to_string()),
            capsule: None,
        },
    );
    notebook.set(
        2,
        CellView {
            description: None,
            helpers: Vec::new(),
            executed_source: None,
            origin: sterna::abi::Origin::AuthoredCell,
            repaired_from: None,
            changes: None,
            stdout: None,
            output: None,
            execution: None,
            call_count: None,
            table: Some("n  number  1".to_string()),
            error: None,
            returned: Some("the answer".to_string()),
            answered: false,
            yield_reason: None,
            capsule: None,
        },
    );
    assert_eq!(cell_ordinal(&conversation, &notebook), 2);

    let buffer = rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        30,
    );
    let text = buffer_text(&buffer);

    assert_eq!(
        conversation_rows(&buffer, "[1] out", 2),
        vec!["n  number  1".to_string(), "yielded: why".to_string()],
        "the reason sits by the table:\n{text}"
    );
    assert!(
        !text.contains("[1] error"),
        "a yield is not an error:\n{text}"
    );
    assert_eq!(
        conversation_row(&buffer, "[2] return"),
        "the answer",
        "{text}"
    );
    assert!(
        !text.contains("assistant: the answer"),
        "the return response must not be duplicated as another assistant turn:\n{text}"
    );
    assert!(!text.contains("[3] in"), "the reply is not a cell:\n{text}");
}

// --- docs/supervisor.md §4: the sidebar's own line ----------

/// §4: the sidebar shows the supervisor's line under the spend line, and a
/// nudge's own reason is what it says.
#[test]
fn a_nudge_shows_under_the_spend_line() {
    let notebook = Notebook {
        tokens: Some(TaskTokens {
            used: 100,
            parent_used: 100,
            helpers: Default::default(),
            counted: Counted::Estimated,
        }),
        // Short enough to fit the sidebar's own width on one row -- this test
        // is about placement, not wrapping, which `notebook_lines`' own
        // `push_text_region` already covers for the conversation column.
        supervisor: Some(SupervisorStatus::Nudged("looping".to_string())),
        ..Notebook::default()
    };

    let conversation = conversation(vec![Message::text(Role::User, "hi")]);
    let buffer = rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        20,
    );
    let text = buffer_text(&buffer);

    let spend_at = text.find("Σ 100 tokens").expect("the spend line renders");
    let supervisor_at = text
        .find("supervisor: looping")
        .expect("the nudge's own reason renders under the spend line");
    assert!(
        spend_at < supervisor_at,
        "the supervisor line must sit under the spend line:\n{text}"
    );
}

/// §3: a look that produced no answer is recorded **as such**, so the sidebar
/// must not show it as the healthy "looked, no nudge" -- otherwise a
/// supervisor whose model id or endpoint is wrong reads exactly like one that
/// is watching, while spending a request every `every` cells.
#[test]
fn a_failed_look_shows_as_failed_and_names_its_cause() {
    let notebook = Notebook {
        tokens: Some(TaskTokens {
            used: 100,
            parent_used: 100,
            helpers: Default::default(),
            counted: Counted::Estimated,
        }),
        supervisor: Some(SupervisorStatus::LookFailed("unparseable".to_string())),
        ..Notebook::default()
    };

    let conversation = conversation(vec![Message::text(Role::User, "hi")]);
    let buffer = rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        20,
    );
    let text = buffer_text(&buffer);

    let spend_at = text.find("Σ 100 tokens").expect("the spend line renders");
    let supervisor_at = text
        .find("supervisor: FAILED")
        .expect("a failed look must say so, under the spend line");
    assert!(
        spend_at < supervisor_at,
        "the supervisor line must sit under the spend line:\n{text}"
    );
    assert!(
        text.contains("unparseable"),
        "the failed look must name its cause:\n{text}"
    );
    assert!(
        !text.contains("looked, no nudge"),
        "a failed look must never render as a healthy look:\n{text}"
    );
}

#[test]
fn sidebar_shows_real_inbox_and_batch_counts_without_changing_narrow_layout() {
    let notebook = Notebook {
        inbox_depth: 7,
        batches_delivered: 12,
        ..Notebook::default()
    };
    let conversation = conversation(vec![]);
    let buffer = rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        40,
    );
    let text = buffer_text(&buffer);
    assert!(text.contains("inbox 7 · batches 12"), "{text}");
    // A zero earns no room: the two live counts are drawn and the idle
    // handler count is not, so the line is worth reading on the occasion a
    // handler actually is live.
    assert!(!text.contains("handlers 0"), "{text}");
    let narrow = rendered_sized(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        40,
        12,
    );
    assert!(!buffer_text(&narrow).contains("inbox 7"));
}

// --- docs/helpers.md, "In the TUI": the lane --------

/// One helper call, as the runtime records it once the call has resolved.
fn resolved_helper(asked: &str, gave: &str, elapsed_ms: u64) -> HelperRecord {
    HelperRecord {
        helper: "reduce".to_string(),
        verb: "reducing".to_string(),
        asked: asked.to_string(),
        outcome: HelperOutcome {
            text: gave.to_string(),
            ok: true,
            cancelled: false,
            elapsed_ms,
        },
        turns: 1,
        looked: Vec::new(),
        usage: Default::default(),
    }
}

/// A call still in flight: no answer and no failure sentence yet.
fn running_helper(asked: &str, elapsed_ms: u64) -> HelperRecord {
    HelperRecord {
        outcome: HelperOutcome {
            text: String::new(),
            ok: false,
            cancelled: false,
            elapsed_ms,
        },
        ..resolved_helper(asked, "", 0)
    }
}

/// A call that came back with a failure rather than an answer.
fn failed_helper(asked: &str, reason: &str, elapsed_ms: u64) -> HelperRecord {
    HelperRecord {
        outcome: HelperOutcome {
            text: reason.to_string(),
            ok: false,
            cancelled: false,
            elapsed_ms,
        },
        ..resolved_helper(asked, "", 0)
    }
}

#[test]
fn pushed_scout_keeps_the_submitted_request_and_live_status_visible() {
    let task = "find where helper cancellation is handled";
    let conversation = conversation(vec![
        Message::text(Role::User, "an earlier task"),
        Message::text(Role::Assistant, "Earlier work finished."),
        Message::text(Role::User, task),
    ]);
    let mut scout = running_helper(task, 2_400);
    scout.helper = "find".into();
    scout.verb = "scanning".into();
    let notebook = Notebook {
        preflight: Some(scout),
        ..Notebook::default()
    };

    let text = buffer_text(&rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        32,
    ));

    assert!(
        text.contains(task),
        "the submitted request disappeared:\n{text}"
    );
    assert!(text.contains("PREFLIGHT · SCOUT"), "{text}");
    assert!(text.contains("find"), "{text}");
    assert!(text.contains("scanning"), "{text}");
    assert!(text.contains("2.4s"), "{text}");
    assert!(
        text.find(&format!("you: {task}")).unwrap() < text.find("PREFLIGHT · SCOUT").unwrap(),
        "preflight was attached to old history instead of the newest request:\n{text}"
    );
    assert!(
        !text.contains("Cell 1") && !text.contains("cell 1"),
        "preflight was presented as a model-authored cell:\n{text}"
    );
}

/// One executed cell whose view carries `helpers`, rendered at a width that
/// leaves the lane room for its own line.
fn helper_screen(helpers: Vec<HelperRecord>) -> String {
    let conversation = conversation(vec![
        Message::text(Role::User, "the task"),
        Message::text(Role::Assistant, "```sterna\nconst n = 1;\n```"),
    ]);
    let mut notebook = Notebook::default();
    notebook.set(
        1,
        CellView {
            helpers,
            execution: Some("No tool calls ran in this cell.".to_string()),
            table: Some("n  number  1".to_string()),
            ..CellView::default()
        },
    );
    buffer_text(&rendered_notebook(
        &conversation,
        &known_served_by(),
        &HandleTable::new(),
        &notebook,
        32,
    ))
}

/// The lane's whole point: while a helper runs, the user can see what is
/// happening on their behalf -- what it is doing, and what it was handed.
///
/// **Elapsed zero and still a lane.** A call that has not answered has no
/// duration yet, so the 300ms floor cannot be what decides whether it is
/// drawn; the floor is about a resolved call whose lane would vanish.
#[test]
fn a_running_helper_shows_its_verb_and_what_it_was_asked() {
    let text = helper_screen(vec![running_helper("cargo build log · 4118 lines", 0)]);

    let lane = text
        .lines()
        .find(|line| line.contains("reduce"))
        .unwrap_or_else(|| panic!("the lane renders under the cell header:\n{text}"));
    assert!(lane.contains("reducing"), "the verb renders: {lane}");
    assert!(
        lane.contains("cargo build log · 4118 lines"),
        "what it was asked renders: {lane}"
    );
    assert!(
        lane.contains("-."),
        "a running call carries the reaching-out frame: {lane}"
    );
    assert!(lane.contains("0.0s"), "elapsed is text: {lane}");
    assert!(
        !text.contains("· 1 helper"),
        "a call still in flight is not folded into the header yet:\n{text}"
    );
}

/// `little-helpers.md`'s order, and the reason the lane exists at all: the
/// running lane appears, resolves to what the caller got, and only then
/// folds into the cell header.
#[test]
fn a_helper_lane_runs_then_resolves_then_folds() {
    let asked = "cargo build log · 4118 lines";
    let running = helper_screen(vec![running_helper(asked, 1200)]);
    assert!(
        running.lines().any(|line| line.contains("reducing")),
        "the lane runs first:\n{running}"
    );
    assert!(
        !running.contains("· 1 helper"),
        "the header does not count a call that has not resolved:\n{running}"
    );

    let resolved = helper_screen(vec![resolved_helper(
        asked,
        "3 distinct root failures",
        1200,
    )]);
    let lane = resolved
        .lines()
        .find(|line| line.contains("reduce"))
        .unwrap_or_else(|| panic!("the resolved call keeps its lane:\n{resolved}"));
    assert!(lane.contains("OK"), "it resolves to OK: {lane}");
    assert!(
        lane.contains("3 distinct root failures"),
        "and says what the caller got: {lane}"
    );
    assert!(
        resolved.contains("· 1 helper"),
        "and only then folds into the header:\n{resolved}"
    );
}

/// A helper that failed renders ` !! ` with its reason and stays there --
/// the supervisor shipped for weeks rendering a permanently failing look as
/// a healthy one, and the collapse must not rebuild that.
#[test]
fn a_failed_helper_shows_its_reason_and_survives_the_collapse() {
    let failed = failed_helper("4118 lines", "request failed: 429", 400);
    let alone = helper_screen(vec![failed.clone()]);
    let lane = alone
        .lines()
        .find(|line| line.contains("reduce"))
        .unwrap_or_else(|| panic!("a failed call renders its own lane:\n{alone}"));
    assert!(lane.contains("!!"), "a failure is not an OK: {lane}");
    assert!(
        lane.contains("request failed: 429"),
        "the failure names its reason: {lane}"
    );
    assert!(lane.contains("4118 lines"), "and what was asked: {lane}");

    let crowded = helper_screen(vec![
        resolved_helper("first log", "1 root error", 1000),
        resolved_helper("second log", "2 root errors", 1000),
        failed,
        resolved_helper("fourth log", "4 root errors", 1000),
    ]);
    assert!(
        crowded.contains("4 helpers"),
        "four lanes collapse to a count:\n{crowded}"
    );
    assert!(
        crowded.contains("request failed: 429"),
        "a failed helper does not fold into the count:\n{crowded}"
    );
}

/// A lane that appears and vanishes on every cheap call is the banner nobody
/// reads, so a call this short leaves only the header's folded summary.
#[test]
fn a_helper_that_resolved_under_three_hundred_milliseconds_renders_no_lane() {
    let text = helper_screen(vec![resolved_helper("a short log", "no failures", 120)]);
    assert!(
        !text.contains("no failures"),
        "a sub-300ms call renders no lane:\n{text}"
    );
    assert!(!text.contains("0.1s"), "and no elapsed of its own:\n{text}");
    assert!(
        text.contains("· 1 helper"),
        "but the header still says it happened:\n{text}"
    );
}

/// Helpers run in parallel; past three lanes the cell itself would be pushed
/// off screen.
#[test]
fn four_helpers_collapse_to_a_count_and_a_total() {
    let text = helper_screen(vec![
        resolved_helper("first log", "1 root error", 1000),
        resolved_helper("second log", "2 root errors", 1000),
        resolved_helper("third log", "3 root errors", 1000),
        resolved_helper("fourth log", "4 root errors", 1100),
    ]);
    assert!(
        text.contains("4 helpers · 4.1s"),
        "four lanes collapse to a count and a total:\n{text}"
    );
    assert!(
        !text.contains("2 root errors"),
        "the individual lanes are gone:\n{text}"
    );
}

// --- little-helpers.md, "In the TUI": the completion gate's recap ------

/// What the session actually did, and one specific next prompt -- the shape
/// `helpers::RECAP_PREAMBLE` asks the helper for.
const RECAP_TEXT: &str = "Renamed the parser entry point and updated its two callers.\n\
                          Next: run the full gate before pushing.";

/// The recap as the completion gate hands it over: one helper call that
/// either came back (`ok`) or did not.
fn recap_record(text: &str, ok: bool) -> HelperRecord {
    HelperRecord {
        helper: "recap".to_string(),
        verb: "recapping".to_string(),
        asked: "the finished task".to_string(),
        outcome: HelperOutcome {
            text: text.to_string(),
            ok,
            cancelled: false,
            elapsed_ms: 900,
        },
        turns: 1,
        looked: Vec::new(),
        usage: Default::default(),
    }
}

/// A finished task -- a request and the model's own answer -- with `recap`
/// as the session's closing output. `None` is `completion = "silent"`.
fn recap_screen(recap: Option<HelperRecord>) -> Buffer {
    let conversation = conversation(vec![
        Message::text(Role::User, "rename the parser entry point"),
        Message::text(Role::Assistant, ASSISTANT_ANSWER),
    ]);
    let state = ScreenState {
        recap,
        ..ScreenState::default()
    };
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            render_screen(
                frame,
                &conversation,
                &known_served_by(),
                &HandleTable::new(),
                &Notebook::default(),
                &state,
            )
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

const ASSISTANT_ANSWER: &str = "I renamed it and updated the callers.";

/// The foreground colour a row is drawn in: the first glyph of that row that
/// is neither blank nor part of a border.
fn fg_of_row(buffer: &Buffer, needle: &str) -> Color {
    let text = buffer_text(buffer);
    let y = text
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not found in buffer:\n{text}")) as u16;
    for x in 0..buffer.area.width {
        let cell = buffer.cell((x, y)).expect("inside the buffer");
        let symbol = cell.symbol();
        if !symbol.trim().is_empty() && !"│┃╭╰─".contains(symbol) {
            return cell.fg;
        }
    }
    panic!("row {y} carries no glyph:\n{text}");
}

/// `completion = "recap"`: one or two sentences on what the session did and
/// one suggested next prompt, rendered where the session's closing output
/// belongs -- under the transcript it summarises.
///
/// **Visually distinct, deliberately.** A recap the reader takes for the
/// assistant's answer is worse than no recap, so it carries its own header
/// naming its author and is drawn muted rather than in the model's own
/// prose style.
#[test]
fn a_recap_renders_its_sentences_and_its_next_line() {
    let buffer = recap_screen(Some(recap_record(RECAP_TEXT, true)));
    let text = buffer_text(&buffer);

    assert!(
        text.contains("Renamed the parser entry point and updated its two callers."),
        "the recap says what the session did:\n{text}"
    );
    let next = text
        .lines()
        .find(|line| line.trim_start_matches(['│', ' ']).starts_with("Next:"))
        .unwrap_or_else(|| panic!("the recap suggests one next prompt:\n{text}"));
    assert!(
        next.contains("run the full gate before pushing."),
        "and says which one: {next}"
    );

    let header = text
        .lines()
        .find(|line| line.contains("RECAP"))
        .unwrap_or_else(|| panic!("the recap carries its own header:\n{text}"));
    assert!(
        header.contains("not the assistant"),
        "which says whose words these are not: {header}"
    );

    assert_eq!(
        fg_of_row(&buffer, "Renamed the parser entry point"),
        Color::Gray,
        "the recap is muted:\n{text}"
    );
    assert_ne!(
        fg_of_row(&buffer, ASSISTANT_ANSWER),
        Color::Gray,
        "and the model's own prose is not, so the two cannot be confused:\n{text}"
    );
    assert!(
        text.contains(ASSISTANT_ANSWER),
        "the answer itself is untouched:\n{text}"
    );
}

/// The default. A line printed after every task is a line nobody reads, so
/// an accepted task says nothing at all unless it was asked to.
#[test]
fn silent_renders_nothing_at_all() {
    let text = buffer_text(&recap_screen(None));
    assert!(!text.contains("RECAP"), "no header:\n{text}");
    assert!(!text.contains("Next:"), "no suggestion:\n{text}");
    assert!(
        text.contains(ASSISTANT_ANSWER),
        "the answer still renders:\n{text}"
    );
}

/// A recap that failed prints nothing -- not its failure, not a header, not
/// a blank frame. It must never replace or delay the answer above it, and
/// the screen is the one `completion = "silent"` already draws.
#[test]
fn a_failed_recap_prints_nothing() {
    let failed = buffer_text(&recap_screen(Some(recap_record(
        "request failed: 429",
        false,
    ))));
    assert!(
        !failed.contains("429"),
        "the failure is not the answer:\n{failed}"
    );
    assert_eq!(
        failed,
        buffer_text(&recap_screen(None)),
        "a failed recap draws exactly the silent screen"
    );
}

/// A recurring notice answers one question rather than stacking: the second
/// "Copied" line replaces the first, an unrelated note between them ends the
/// run so the next copy starts a fresh line, and a note at another turn
/// boundary is never reached backwards.
#[test]
fn a_recurring_notice_replaces_its_predecessor_instead_of_piling_up() {
    let mut state = ScreenState::default();
    state.note_replacing("Copied ", "Copied 9 lines to the clipboard.");
    state.note_replacing("Copied ", "Copied 17 lines to the clipboard.");
    state.note_replacing("Copied ", "Copied 4 lines to the clipboard.");
    assert_eq!(
        state
            .history
            .iter()
            .map(|note| note.text.as_str())
            .collect::<Vec<_>>(),
        vec!["Copied 4 lines to the clipboard."],
        "three drags leave one standing answer"
    );

    state.note("Opened crates/sterna/src/tui.rs.");
    state.note_replacing("Copied ", "Copied 2 lines to the clipboard.");
    assert_eq!(state.history.len(), 3, "an unrelated note ends the run");

    state.messages_seen = 2;
    state.note_replacing("Copied ", "Copied 6 lines to the clipboard.");
    assert_eq!(
        state.history.len(),
        4,
        "a copy after a later message does not swallow the one before it"
    );
}

/// Notices are history: a note stays in the conversation after a later one,
/// is drawn at its turn boundary (before the message sent after it, after
/// the turn it followed), and an `ERROR:` note is red.
#[test]
fn notices_stay_in_the_conversation_where_they_happened() {
    let mut state = ScreenState::default();
    state.note("session s-1 — resume it with:  sterna --resume s-1");
    state.messages_seen = 2;
    state.note("model changed to claude-opus-5");
    state.note("ERROR: The `claude-max` login is no longer valid. Type /login claude-max to sign in again.");
    let conversation = conversation(vec![
        Message::text(Role::User, "first question"),
        Message::text(Role::Assistant, "first answer"),
        Message::text(Role::User, "second question"),
    ]);
    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            render_screen(
                frame,
                &conversation,
                &known_served_by(),
                &HandleTable::new(),
                &Notebook::default(),
                &state,
            )
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let text = buffer_text(&buffer);
    let at = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not on screen:\n{text}"))
    };
    assert!(at("resume it with") < at("first question"), "{text}");
    assert!(at("first answer") < at("model changed to"), "{text}");
    assert!(
        at("model changed to") < at("login is no longer valid"),
        "{text}"
    );
    assert!(
        at("login is no longer valid") < at("second question"),
        "{text}"
    );
    assert!(!text.contains(" notice "), "no transient box:\n{text}");
    assert_eq!(fg_of_row(&buffer, "login is no longer valid"), Color::Red);
}
