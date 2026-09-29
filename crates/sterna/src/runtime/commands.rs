//! The command lines a cell's own source already spells out.
//!
//! **A cell is a program, and a program can be read before it is run.** The
//! permission ladder's `auto` rung otherwise meets each `bash({command: …})`
//! call at the moment it happens, one at a time, in the middle of a running
//! cell: the decision model is asked then, serially, and every answer costs
//! its own wait. Most of those lines are not computed at all — they are
//! string literals sitting in the source the model just submitted.
//!
//! So they are read out here, at submit time, and put to the decision model
//! together before the cell starts. Nothing about the decision changes: the
//! same question, the same threshold, the same memory keyed by the same
//! exact line, so a line seen here and then run answers from that memory
//! rather than asking twice. What changes is *when* the waiting happens —
//! once, in parallel, before the program runs, instead of once per call in
//! the middle of it.
//!
//! **Only what is certain is reported.** A command assembled from a variable,
//! a function call, or a template with a substitution in it is not this
//! module's business: it is left to the gate, which sees the real line when
//! the call is made. Reporting a guess here would pre-answer a question
//! about a line that never runs.

use oxc::allocator::Allocator;
use oxc::ast::ast::{Argument, CallExpression, Expression, ObjectPropertyKind, PropertyKey};
use oxc::ast_visit::{Visit, walk};
use oxc::parser::{ParseOptions, Parser};
use oxc::span::SourceType;

/// The binding whose calls this module reads.
const COMMAND_TOOL: &str = "bash";

/// The property that carries the line the shell will see.
const COMMAND_KEY: &str = "command";

/// The most lines one cell is read for.
///
/// A program that spells out more than this many literal command lines is
/// doing something the pre-judgement was not built for, and the gate still
/// meets every one of them as it happens — so this bounds the work without
/// changing any answer.
pub const MAX_LINES: usize = 32;

/// Every literal command line `source` spells out, in source order, without
/// duplicates.
///
/// Returns nothing rather than an error for a source that does not parse:
/// `cell::compile` is the authority on that and reports it with a line and a
/// column. This runs before it only in the sense that it needs no more than
/// a parse; where they disagree, `compile`'s refusal is the one the model
/// sees.
#[must_use]
pub fn literal_lines(source: &str) -> Vec<String> {
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
    let mut found = Lines::default();
    found.visit_program(&parsed.program);
    found.lines
}

#[derive(Default)]
struct Lines {
    lines: Vec<String>,
}

impl Lines {
    fn push(&mut self, line: String) {
        if line.trim().is_empty() || self.lines.len() >= MAX_LINES {
            return;
        }
        if !self.lines.iter().any(|seen| seen == &line) {
            self.lines.push(line);
        }
    }
}

impl<'a> Visit<'a> for Lines {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        if let Expression::Identifier(callee) = &it.callee
            && callee.name == COMMAND_TOOL
            && let Some(Argument::ObjectExpression(object)) = it.arguments.first()
        {
            for property in &object.properties {
                let ObjectPropertyKind::ObjectProperty(property) = property else {
                    continue;
                };
                let named = match &property.key {
                    PropertyKey::StaticIdentifier(name) => name.name == COMMAND_KEY,
                    PropertyKey::StringLiteral(name) => name.value == COMMAND_KEY,
                    _ => false,
                };
                if named && let Some(line) = certain(&property.value) {
                    self.push(line);
                }
            }
        }
        // A `bash(...)` call can sit inside another call's arguments, so the
        // walk continues in every case.
        walk::walk_call_expression(self, it);
    }
}

/// Whether `source` calls a tool the instruction gate answers with the whole
/// project index -- `bash`, `context`, `bg.run`, `bg.watch` or an `mcp` call --
/// under its own name or a dialect's. Read before the cell runs, so the gate
/// can stop the cell before any of it has (`InstructionContext::before_cell`).
///
/// A shadowed name reads as the tool here, and that is the safe direction:
/// the worst it does is deliver instructions one cell early.
#[must_use]
pub fn calls_a_broad_tool(source: &str) -> bool {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::ts())
        .with_options(ParseOptions {
            allow_return_outside_function: true,
            ..ParseOptions::default()
        })
        .parse();
    if !parsed.diagnostics.is_empty() {
        return false;
    }
    let mut found = BroadCall::default();
    found.visit_program(&parsed.program);
    found.0
}

#[derive(Default)]
struct BroadCall(bool);

impl<'a> Visit<'a> for BroadCall {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        match &it.callee {
            Expression::Identifier(callee) => {
                let name = crate::abi::dialect::lookup_any(&callee.name)
                    .map_or(callee.name.as_str(), |shape| shape.target.callee());
                self.0 |= matches!(name, "bash" | "context");
            }
            Expression::StaticMemberExpression(member) => {
                if let Expression::Identifier(object) = &member.object {
                    self.0 |= object.name == "mcp"
                        || (object.name == "bg"
                            && matches!(member.property.name.as_str(), "run" | "watch"));
                }
            }
            _ => {}
        }
        walk::walk_call_expression(self, it);
    }
}

/// The string this expression certainly is, or `None`.
///
/// A template literal counts only when it has no substitution at all: one
/// `${…}` and the line is computed, whatever the rest of it looks like.
fn certain(value: &Expression<'_>) -> Option<String> {
    match value {
        Expression::StringLiteral(text) => Some(text.value.to_string()),
        Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
            .quasis
            .first()
            .and_then(|quasi| quasi.value.cooked.as_ref())
            .map(|cooked| cooked.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broad_tool_is_found_by_name_member_or_dialect_and_nothing_else_is() {
        for source in [
            "const [a, b] = await Promise.all([read({path: 'x'}), bash({command: 'git status'})]);",
            "await context({path: 'src/a.rs', symbol: 'f'});",
            "const job = bg.run('cargo test');",
            "await mcp.call('server', 'tool', {});",
            "await Bash({command: 'ls'});",
        ] {
            assert!(calls_a_broad_tool(source), "{source}");
        }
        for source in [
            "await read({path: 'src/a.rs'});",
            "await rg({pattern: 'bash', path: 'src'});",
            "const text = 'bash({command: 1})';",
            "bg.list();",
        ] {
            assert!(!calls_a_broad_tool(source), "{source}");
        }
    }

    #[test]
    fn a_literal_command_is_read_out_of_the_source() {
        assert_eq!(
            literal_lines(r#"const r = await bash({command: "cargo test -p sterna"});"#),
            vec!["cargo test -p sterna".to_string()]
        );
    }

    #[test]
    fn several_calls_are_read_in_source_order_without_repeats() {
        let source = r#"
            await bash({command: "git status"});
            await bash({command: "cargo fmt"});
            await bash({command: "git status"});
        "#;
        assert_eq!(
            literal_lines(source),
            vec!["git status".to_string(), "cargo fmt".to_string()]
        );
    }

    /// A template with nothing substituted into it is as certain as a
    /// string, and cells are written with backticks constantly.
    #[test]
    fn a_template_with_no_substitution_is_certain() {
        assert_eq!(
            literal_lines("await bash({command: `git diff --stat`});"),
            vec!["git diff --stat".to_string()]
        );
    }

    /// **The decisive one.** A line assembled at run time is not this
    /// module's business; pre-answering a question about a line that never
    /// runs is worse than asking nothing.
    #[test]
    fn a_computed_command_is_not_reported() {
        for source in [
            "await bash({command: `git show ${rev}`});",
            "await bash({command: line});",
            "await bash({command: \"git show \" + rev});",
            "await bash({command: choose()});",
        ] {
            assert!(
                literal_lines(source).is_empty(),
                "a computed line must not be reported: {source}"
            );
        }
    }

    #[test]
    fn another_tools_argument_is_not_a_command_line() {
        assert!(literal_lines(r#"await read({path: "src/main.rs"});"#).is_empty());
        assert!(literal_lines(r#"await rg({command: "not a shell"});"#).is_empty());
    }

    /// A shadowed `bash` is still spelled `bash` here — and that is exactly
    /// why this pre-judges and never *grants*: the worst a wrong reading can
    /// do is ask the model about a line nothing runs.
    #[test]
    fn a_nested_call_is_still_found() {
        let source = r#"
            const all = await Promise.all([
                bash({command: "cargo check"}),
                bash({command: "cargo clippy"}),
            ]);
        "#;
        assert_eq!(
            literal_lines(source),
            vec!["cargo check".to_string(), "cargo clippy".to_string()]
        );
    }

    #[test]
    fn a_source_that_does_not_parse_reports_nothing() {
        assert!(literal_lines("const = = =;").is_empty());
    }

    #[test]
    fn the_reading_is_bounded() {
        let source: String = (0..MAX_LINES + 10)
            .map(|n| format!("await bash({{command: \"echo {n}\"}});\n"))
            .collect();
        assert_eq!(literal_lines(&source).len(), MAX_LINES);
    }

    #[test]
    fn an_empty_command_is_not_a_line() {
        assert!(literal_lines(r#"await bash({command: "   "});"#).is_empty());
    }
}
