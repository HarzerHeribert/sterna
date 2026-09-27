//! Reading one conversation message: what it says, and how it is shown.
//!
//! **A message is read here and drawn elsewhere.** Every function below
//! turns a `Message` into text a renderer can place -- its natural prose,
//! the program it carries, the tool calls its source names -- and none of
//! them draws. Split out of `tui.rs` on 2026-09-19 for the size ratchet;
//! the boundary scan in `tests/tui.rs` follows it, because a rule that
//! stopped at the original file would be a rule anything could step around
//! by moving one function.

use super::*;

/// A cell's input region: the program the message carried, or its prose
/// when it carried none. `model-contract.md` §5's parser is the one that
/// decides which -- the notebook shows what actually ran, not the
/// explanation around it, and a message with two blocks (where neither ran)
/// shows its whole text rather than picking one of them.
pub(super) fn natural_message(message: &Message) -> (String, String) {
    let text = message_text(message);
    if message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
    {
        return (text, String::new());
    }
    if !matches!(
        extract_program(&text),
        Extracted::Program(_) | Extracted::Edit(_)
    ) {
        return (String::new(), String::new());
    }
    let lines: Vec<_> = text.lines().collect();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut saw_program = false;
    let mut i = 0;
    while i < lines.len() {
        if let Some(info) = lines[i].strip_prefix("```") {
            let mut end = i + 1;
            while end < lines.len() && lines[end] != "```" {
                end += 1;
            }
            if matches!(info.trim(), "sterna" | "sterna-edit") {
                saw_program = true;
            } else {
                let target = if saw_program { &mut after } else { &mut before };
                target.extend_from_slice(&lines[i..(end + 1).min(lines.len())]);
            }
            i = end + 1;
        } else {
            if saw_program {
                after.push(lines[i]);
            } else {
                before.push(lines[i]);
            }
            i += 1;
        }
    }
    (before.join("\n"), after.join("\n"))
}

/// Syntactic candidates only: an untaken branch is never execution evidence.
pub(super) fn possible_tool_calls(source: &str) -> Vec<String> {
    use oxc::{
        allocator::Allocator,
        ast::ast::{CallExpression, Expression},
        ast_visit::{Visit, walk},
        parser::{ParseOptions, Parser},
        span::SourceType,
    };
    struct Calls(Vec<String>);
    impl<'a> Visit<'a> for Calls {
        fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
            if let Expression::Identifier(name) = &call.callee
                && crate::tools::registry::names().contains(&name.name.as_str())
            {
                self.0.push(name.name.to_string());
            }
            if let Expression::StaticMemberExpression(member) = &call.callee
                && let Expression::Identifier(owner) = &member.object
                && matches!(
                    (owner.name.as_str(), member.property.name.as_str()),
                    ("agent", "run") | ("bg", "run" | "watch" | "cancel")
                )
            {
                self.0
                    .push(format!("{}.{}", owner.name, member.property.name));
            }
            walk::walk_call_expression(self, call);
        }
    }
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts())
        .with_options(ParseOptions {
            allow_return_outside_function: true,
            ..ParseOptions::default()
        })
        .parse();
    if !parsed.diagnostics.is_empty() {
        return Vec::new();
    }
    let mut calls = Calls(Vec::new());
    calls.visit_program(&parsed.program);
    calls.0
}

pub(super) fn pretty_code(source: &str) -> String {
    use oxc::{
        allocator::Allocator,
        codegen::{Codegen, CodegenOptions, IndentChar},
        parser::{ParseOptions, Parser},
        span::SourceType,
    };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts())
        .with_options(ParseOptions {
            allow_return_outside_function: true,
            ..ParseOptions::default()
        })
        .parse();
    if !parsed.diagnostics.is_empty() {
        return source.to_string();
    }
    Codegen::new()
        .with_options(CodegenOptions {
            indent_char: IndentChar::Space,
            indent_width: 2,
            ..CodegenOptions::default()
        })
        .build(&parsed.program)
        .code
        .trim_end()
        .to_string()
}

pub(super) fn pretty_json(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) if value.is_object() || value.is_array() => {
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| text.to_string())
        }
        _ => text.to_string(),
    }
}

pub(super) fn message_program(message: &Message) -> Extracted {
    let calls: Vec<_> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { name, input, .. } => Some((name, input)),
            _ => None,
        })
        .collect();
    if calls.is_empty() {
        return extract_program(&message_text(message));
    }
    if calls.len() != 1 {
        return Extracted::Invalid("Multiple native cell calls; no cell ran.".into());
    }
    let (name, input) = calls[0];
    if name != "execute_cell" {
        return Extracted::Invalid(format!("Unknown native tool {name}"));
    }
    match input.get("code").and_then(|value| value.as_str()) {
        Some(code) => Extracted::Program(code.to_string()),
        None => Extracted::Invalid("Cell call has no valid source.".into()),
    }
}

pub(super) fn is_tool_feedback(message: &Message) -> bool {
    message
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

pub(super) fn input_region(message: &Message) -> String {
    let text = message_text(message);
    match message_program(message) {
        Extracted::Program(source) | Extracted::Edit(source) => source,
        Extracted::Prose | Extracted::TwoBlocks | Extracted::Invalid(_) => text,
    }
}

/// The prose of a reply that is still streaming, as it will read once done.
pub(super) fn streaming_message_text(text: &str) -> String {
    crate::prompt::completion_text(text).unwrap_or_else(|| text.to_string())
}

pub(super) fn message_text(message: &Message) -> String {
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) | ContentBlock::ToolResult { content: text, .. } => {
                Some(text.as_str())
            }
            ContentBlock::ToolUse { .. }
            | ContentBlock::Thinking { .. }
            | ContentBlock::RedactedThinking { .. } => None,
            ContentBlock::Image { .. } => Some("\n[image attachment]\n"),
        })
        .collect::<Vec<_>>()
        .join("");
    if message.role == Role::Assistant {
        crate::prompt::completion_text(&text).unwrap_or(text)
    } else {
        text
    }
}
