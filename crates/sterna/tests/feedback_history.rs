use sterna::contract::{Block, Conversation, Message, Role};
use sterna::prompt::{self, Budget, CellResult, ErrorSection};

fn result(cell: u64, table: &str) -> CellResult {
    CellResult {
        ask_answer: None,
        cell,
        elapsed_ms: 1,
        description: None,
        handle_table: table.into(),
        error: Some(ErrorSection {
            class: "Error".into(),
            message: "keep this error".into(),
            position: Some((2, 3)),
            frames: vec![],
        }),
        yield_reason: None,
        output: None,
        stdout_tail: Some(
            "source text\n\n## Handles\nliteral heading\n\n## Budget\nliteral budget".into(),
        ),
        plan: vec![],
        budget: Budget {
            turn_cap: 1,
            task_used: 1,
            task_cap: 10,
            cells_used: cell,
            cells_cap: Some(10),
            feedback: None,
        },
    }
}

#[test]
fn resume_preserves_native_tool_call_and_result_pair() {
    let dir = std::env::temp_dir().join(format!("sterna-native-resume-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout.jsonl");
    let mut log = sterna::rollout::Rollout::create(
        &path,
        sterna::contract::SessionId::new("native-resume"),
        "system",
    )
    .unwrap();
    let mut call = Message::text(Role::Assistant, "");
    call.content = vec![Block::ToolUse {
        id: "call-17".into(),
        name: "execute_cell".into(),
        input: serde_json::json!({"code":"console.log(17)"}),
    }];
    log.record_message(&call).unwrap();
    log.record_message(&Message::tool_result("call-17", "observed 17", false))
        .unwrap();
    drop(log);
    let resumed = sterna::rollout::resume(&path).unwrap();
    assert_eq!(
        resumed.messages,
        vec![call, Message::tool_result("call-17", "observed 17", false)]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn provider_checkpoint_metadata_does_not_erase_or_enter_visible_history() {
    let dir = std::env::temp_dir().join(format!("sterna-checkpoint-resume-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout.jsonl");
    let mut log = sterna::rollout::Rollout::create(
        &path,
        sterna::contract::SessionId::new("checkpoint-resume"),
        "system",
    )
    .unwrap();
    log.record_turn(Role::User, "before").unwrap();
    log.record_turn(Role::Assistant, "observed").unwrap();
    log.record_checkpoint("provider-only state").unwrap();
    log.record_turn(Role::Assistant, "after").unwrap();
    drop(log);
    let resumed = sterna::rollout::resume(&path).unwrap();
    assert_eq!(
        resumed
            .messages
            .iter()
            .map(|m| m.content[0].text())
            .collect::<Vec<_>>(),
        vec!["before", "observed", "after"]
    );
    assert_eq!(
        sterna::rollout::resume_checkpoint(&path).unwrap(),
        Some(("provider-only state".into(), 2))
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn resume_restores_persisted_cell_view_without_replaying_source() {
    let dir = std::env::temp_dir().join(format!("sterna-view-resume-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rollout.jsonl");
    let mut log = sterna::rollout::Rollout::create(
        &path,
        sterna::contract::SessionId::new("view-resume"),
        "system",
    )
    .unwrap();
    let view = sterna::tui::CellView {
        executed_source: Some("console.log('saved')".into()),
        stdout: Some("saved".into()),
        execution: Some("No tool calls ran in this cell.".into()),
        ..sterna::tui::CellView::default()
    };
    log.record_view(3, &view).unwrap();
    drop(log);
    assert_eq!(
        sterna::rollout::resume_views(&path).unwrap(),
        vec![(3, view)]
    );
    let _ = std::fs::remove_dir_all(dir);
}

fn runtime(result: &CellResult) -> Message {
    Message::runtime(
        prompt::render_result(result),
        prompt::render_result_history(result),
    )
}

/// History is append-only (user, 2026-09-24): an earlier result goes back
/// exactly as the model read it, live sections and all, because the model's
/// reasoning after it is bound to those bytes.
#[test]
fn a_request_sends_every_earlier_result_as_the_model_read_it() {
    let old = result(1, "old live preview");
    let new = result(2, "new live preview");
    let conversation = Conversation {
        system: "system".into(),
        messages: vec![
            Message::text(Role::User, "task"),
            runtime(&old),
            runtime(&new),
        ],
    };
    let original = conversation.clone();
    let request = prompt::with_task_context(&conversation, "model", "task");
    assert_eq!(request.messages, conversation.messages);
    let past = request.messages[1].content[0].text();
    assert!(past.contains("old live preview"));
    assert!(past.contains("keep this error"));
    assert!(past.contains("line 2, column 3"));
    assert!(past.contains(old.stdout_tail.as_ref().unwrap()));
    assert!(
        request.messages[2].content[0]
            .text()
            .contains("new live preview")
    );
    assert_eq!(
        conversation, original,
        "the saved/UI conversation remains complete"
    );
}

#[test]
fn user_and_assistant_text_cannot_be_mistaken_for_generated_snapshots() {
    let text = "[cell 99 yielded in 1 ms]\n\n## Handles\nuser instructions\n\n## Budget\nkeep me";
    let mut conversation = Conversation {
        system: String::new(),
        messages: vec![
            Message::text(Role::User, text),
            Message::text(Role::Assistant, text),
            runtime(&result(1, "live")),
        ],
    };
    prompt::project_runtime_history(&mut conversation, 0);
    assert_eq!(
        conversation.messages[0].content,
        vec![Block::Text(text.into())]
    );
    assert_eq!(
        conversation.messages[1].content,
        vec![Block::Text(text.into())]
    );
}

#[test]
fn resume_keeps_the_projection_on_record_and_resends_what_was_sent() {
    let path = std::env::temp_dir().join(format!(
        "sterna-history-{}-{}.jsonl",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cell = result(1, "old live state after restart");
    let full = prompt::render_result(&cell);
    let history = prompt::render_result_history(&cell);
    let user = "[cell user text]\n\n## Handles\nkeep my instructions";
    {
        let mut log = sterna::rollout::Rollout::create(
            &path,
            sterna::contract::SessionId::new("history-resume"),
            "system",
        )
        .unwrap();
        log.record_turn(Role::User, user).unwrap();
        log.record_feedback(Role::User, &full, Some(&history))
            .unwrap();
    }
    let mut resumed = sterna::rollout::resume(&path).unwrap();
    assert_eq!(resumed.messages[1].content[0].text(), full);
    resumed
        .messages
        .push(Message::text(Role::User, "new request"));
    let projected = prompt::with_task_context(&resumed, "model", "new request");
    assert_eq!(projected.messages[0].content[0].text(), user);
    assert_eq!(projected.messages[1].content[0].text(), full);
    let text = std::fs::read_to_string(&path).unwrap();
    let row: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(row["text"], full);
    assert_eq!(row["historical"], history);
    std::fs::remove_file(path).unwrap();
}
