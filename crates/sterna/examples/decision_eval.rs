//! Asks the decision model Sterna's own questions over a labelled set and
//! prints one JSON line per case -- the evaluation behind which decisions
//! Sterna delegates to Jev and at what threshold (2026-09-23).
//!
//! Usage: `decision_eval <cases.jsonl> [model]` with `ANTHROPIC_BASE_URL`
//! (and `ANTHROPIC_AUTH_TOKEN` when the gateway wants one) pointing at a
//! gateway that routes `/v1/systemone`. A case is one of:
//!
//! - `{"question":"task","request":…}` → intent and kind
//! - `{"question":"shape","name":…,"text":…}` → the field's kind of text
//!
//! Every other key (the labels) is copied to the output line untouched.

use std::io::BufRead as _;

use serde_json::{Value, json};
use sterna::decide;

/// A case's optional `session` context, as `decide::TaskContext` sends it.
#[derive(serde::Deserialize)]
struct ContextCase {
    #[serde(default)]
    earlier_requests: u32,
    #[serde(default)]
    instructions: Vec<String>,
    #[serde(default)]
    instructions_name_commands: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: decision_eval <cases.jsonl> [model]");
    let model = args
        .next()
        .unwrap_or_else(|| decide::DEFAULT_MODEL.to_string());
    let file = std::fs::File::open(&path).expect("the cases file opens");
    for line in std::io::BufReader::new(file).lines() {
        let line = line.expect("a readable line");
        if line.trim().is_empty() {
            continue;
        }
        let mut case: Value = serde_json::from_str(&line).expect("one JSON case per line");
        let text = |key: &str| case[key].as_str().unwrap_or_default().to_string();
        let answer = match case["question"].as_str() {
            Some("task") => match decide::task_questions_in(
                &model,
                &text("request"),
                serde_json::from_value::<ContextCase>(case["session"].clone())
                    .ok()
                    .map(|c| decide::TaskContext {
                        earlier_requests: c.earlier_requests,
                        instructions: c.instructions,
                        instructions_name_commands: c.instructions_name_commands,
                    })
                    .as_ref(),
            ) {
                Ok(d) => json!({
                    "intent": d.intent.choice, "intent_confidence": d.intent.confidence,
                    "kind": d.kind.as_ref().map(|k| k.choice.clone()),
                    "kind_confidence": d.kind.as_ref().map(|k| k.confidence),
                    "latency_ms": d.intent.latency_ms,
                }),
                Err(e) => json!({ "error": e.to_string() }),
            },
            Some("shape") => match decide::field_shape(&model, &text("name"), &text("text")) {
                Ok(s) => {
                    json!({ "shape": s.choice, "confidence": s.confidence, "latency_ms": s.latency_ms })
                }
                Err(e) => json!({ "error": e.to_string() }),
            },
            other => json!({ "error": format!("unknown question {other:?}") }),
        };
        case["answer"] = answer;
        println!("{case}");
    }
}
