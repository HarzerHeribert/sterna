use ratatui::{Terminal, backend::TestBackend};
use sterna::contract::{Conversation, Message, Role, ServedBy};
use sterna::runtime::handles::HandleTable;
use sterna::tui::{
    self, CellError, CellView, Inspection, Notebook, ScreenState, SidebarVisibility,
};

fn screen(conversation: &Conversation, notebook: &Notebook, state: &ScreenState) -> String {
    let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
    terminal
        .draw(|frame| {
            tui::render_screen(
                frame,
                conversation,
                &ServedBy::default(),
                &HandleTable::new(),
                notebook,
                state,
            )
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..30)
        .map(|y| {
            (0..109)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn state() -> ScreenState {
    ScreenState {
        sidebar: SidebarVisibility::Hidden,
        ..ScreenState::default()
    }
}

#[test]
fn inspector_exposes_the_full_recorded_output_and_each_cells_own_outcome() {
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Work"),
            Message::text(Role::Assistant, "```sterna\nconsole.log('one');\n```"),
            Message::text(Role::User, "feedback"),
            Message::text(
                Role::Assistant,
                "I think this succeeded.\n```sterna\nthrow new Error('broken');\n```",
            ),
        ],
    };
    let notebook = Notebook {
        cells: vec![
            CellView {
                executed_source: Some("console.log('one');".into()),
                stdout: Some((0..150).map(|i| format!("actual-output-{i}\n")).collect()),
                execution: Some("No tool calls ran in this cell.".into()),
                table: Some("answer = 42".into()),
                answered: true,
                ..CellView::default()
            },
            CellView {
                executed_source: Some("throw new Error('broken');".into()),
                error: Some(CellError {
                    class: "Error".into(),
                    message: "broken".into(),
                    line: Some(1),
                    column: Some(0),
                }),
                execution: Some("No tool calls ran in this cell.".into()),
                ..CellView::default()
            },
        ],
        ..Notebook::default()
    };
    let mut state = state();
    state.compact = true;
    state.inspection = Inspection::open(1, &notebook);
    let first = screen(&conversation, &notebook, &state);
    assert!(first.contains("CELL 1 / 2  · executed"));
    assert!(first.contains("console.log('one');"));
    assert!(first.contains("actual-output-0"));
    assert!(!first.contains("actual-output-149"));
    state.inspection.as_mut().unwrap().scroll = usize::MAX;
    let end = screen(&conversation, &notebook, &state);
    assert!(end.contains("actual-output-149"));
    assert!(end.contains("answer = 42"));
    assert!(!end.contains("more lines"));
    state.inspection.as_mut().unwrap().adjacent(true, &notebook);
    let next = screen(&conversation, &notebook, &state);
    assert!(next.contains("CELL 2 / 2  · failed"));
    assert!(next.contains("Error: broken"));
    assert!(!next.contains("actual-output-0"));
    state.inspection = None;
    let chat = screen(&conversation, &notebook, &state);
    assert!(!chat.contains("CELL 2 / 2"));
    // The state is the header field's own word now.
    assert!(chat.contains("FAILED"), "{chat}");
}

#[test]
fn scrolling_reaches_the_first_and_last_turn_and_stays_put_when_new_text_arrives() {
    let mut conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "ORIGINAL REQUEST"),
            Message::text(
                Role::Assistant,
                (0..160)
                    .map(|i| format!("paragraph-{i}\n"))
                    .collect::<String>(),
            ),
        ],
    };
    let notebook = Notebook::default();
    let mut state = state();
    state.scrollback = usize::MAX;
    assert!(screen(&conversation, &notebook, &state).contains("ORIGINAL REQUEST"));
    state.scrollback = 0;
    assert!(screen(&conversation, &notebook, &state).contains("paragraph-159"));
    let area = ratatui::layout::Rect::new(0, 0, 110, 30);
    let regions = tui::screen_regions(area, &state);
    state.scrollback = 60;
    let previous = tui::conversation_rows(
        &conversation,
        &HandleTable::new(),
        &notebook,
        &state,
        regions.transcript.width,
    );
    let before = screen(&conversation, &notebook, &state);
    conversation.messages.push(Message::text(
        Role::Assistant,
        "NEW LIVE OUTPUT\nnew line\nmore streaming",
    ));
    let current = tui::conversation_rows(
        &conversation,
        &HandleTable::new(),
        &notebook,
        &state,
        regions.transcript.width,
    );
    state.scrollback = tui::anchor_scrollback(
        state.scrollback,
        previous,
        current,
        usize::from(regions.transcript.height),
    );
    let after = screen(&conversation, &notebook, &state);
    let body = |text: String| {
        text.lines()
            .skip(usize::from(regions.transcript.y))
            .take(usize::from(regions.transcript.height))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(body(before), body(after));
    state.scrollback = 0;
    assert!(screen(&conversation, &notebook, &state).contains("NEW LIVE OUTPUT"));
}

#[test]
fn native_calls_render_as_notebook_cells_and_results_never_become_user_chat() {
    use sterna::contract::Block;
    let mut call = Message::text(Role::Assistant, "I will read the file.");
    call.content.push(Block::ToolUse { id:"call-1".into(),name:"execute_cell".into(),input:serde_json::json!({"code":"const file = await read({path: 'real.txt'});\nconsole.log(file.text);"}) });
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Read the real file."),
            call,
            Message::tool_result("call-1", "RUNTIME_FEEDBACK_ONLY", false),
            Message::text(Role::Assistant, "the actual answer"),
        ],
    };
    let notebook = Notebook {
        cells: vec![CellView {
            executed_source: Some(
                "const file = await read({path: 'real.txt'});\nconsole.log(file.text);".into(),
            ),
            execution: Some("└─ read real.txt · returned".into()),
            stdout: Some("ACTUAL_FILE_CONTENT".into()),
            returned: Some("the actual answer".into()),
            answered: true,
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    assert_eq!(tui::cell_ordinal(&conversation, &notebook), 1);
    let mut state = state();
    state.compact = true;
    let shown = screen(&conversation, &notebook, &state);
    assert!(shown.contains("EXECUTED"), "{shown}");
    assert!(shown.contains("ACTUAL_FILE_CONTENT"));
    assert_eq!(shown.matches("the actual answer").count(), 1);
    assert!(!shown.contains("RUNTIME_FEEDBACK_ONLY"));
    state.inspection = Inspection::open(1, &notebook);
    let inspected = screen(&conversation, &notebook, &state);
    assert!(inspected.contains("const file = await read"));
    assert!(inspected.contains("read real.txt · returned"));
}

#[test]
fn partial_native_input_is_marked_unexecuted_not_rendered_as_output() {
    let mut state = state();
    state.streaming_tool_input = Some("{\"code\":\"console.log('INVENTED_OUTPUT')".into());
    state.activity = tui::Activity::Streaming;
    let shown = screen(&Conversation::default(), &Notebook::default(), &state);
    assert!(shown.contains("preparing · nothing has run"));
    assert!(!shown.contains("INVENTED_OUTPUT"));
    assert!(!shown.contains("ACTUAL CALLS"));
}

#[test]
fn cell_navigation_skips_prose_turns_and_opens_the_last_actual_cell() {
    let recorded = CellView {
        execution: Some("No tool calls ran in this cell.".into()),
        ..CellView::default()
    };
    let notebook = Notebook {
        cells: vec![
            recorded.clone(),
            CellView::default(),
            recorded,
            CellView::default(),
        ],
        ..Notebook::default()
    };
    assert_eq!(Inspection::latest(&notebook), Some(3));
    assert!(Inspection::open(2, &notebook).is_none());
    let mut inspection = Inspection::open(1, &notebook).unwrap();
    inspection.adjacent(true, &notebook);
    assert_eq!(inspection.cell, 3);
    inspection.adjacent(true, &notebook);
    assert_eq!(inspection.cell, 3);
    inspection.adjacent(false, &notebook);
    assert_eq!(inspection.cell, 1);
}

#[test]
fn transcript_padding_and_scrollbar_never_overwrite_wrapped_characters() {
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "X".repeat(350)),
            Message::text(Role::Assistant, "later row\n".repeat(150)),
        ],
    };
    let mut state = state();
    state.scrollback = usize::MAX;
    let shown = screen(&conversation, &Notebook::default(), &state);
    assert_eq!(shown.matches('X').count(), 350);
}

#[test]
fn multiline_shell_source_is_one_recorded_call_not_many_display_lines() {
    let conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, "Run a script"),
            Message::text(
                Role::Assistant,
                "```sterna\nawait bash({command: 'echo a; echo b'});\n```",
            ),
        ],
    };
    let mut notebook = Notebook {
        cells: vec![CellView {
            executed_source: Some("await bash({command: 'echo a; echo b'});".into()),
            execution: Some("└─ bash\necho a\necho b\n · returned".into()),
            call_count: Some(1),
            ..CellView::default()
        }],
        ..Notebook::default()
    };
    let compact = ScreenState {
        compact: true,
        ..state()
    };
    // The call bar counts recorded calls, so a command whose *output* is
    // several lines is still one `bash` and never `bash ×3`.
    let shown = screen(&conversation, &notebook, &compact);
    assert!(shown.contains("bash"), "{shown}");
    assert!(!shown.contains("bash ×"), "{shown}");
    notebook.cells[0].execution =
        Some("├─ bash first · returned\n└─ bash second · returned".into());
    notebook.cells[0].call_count = Some(2);
    assert!(
        screen(&conversation, &notebook, &compact).contains("bash ×2"),
        "two recorded calls are not counted"
    );
}
