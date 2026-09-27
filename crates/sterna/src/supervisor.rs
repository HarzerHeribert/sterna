//! The look -- `docs/supervisor.md` and §3: a compressed
//! trajectory, one decision about it, and a sentence only when that decision
//! says something is wrong.
//!
//! **Never a preview's bytes and never a payload.** [`compress`] renders only
//! a program's first line, its outcome and its call trajectory -- the same
//! three things `runtime-contract.md` §9.4 already puts in the rollout -- so
//! the supervisor sees exactly what the rollout records and nothing a program
//! read or wrote.
//!
//! **Three layers, cheapest first, and each may be absent** (the user, 2026-09-17:
//! "Could supervisor be jev? ... Don't hardcode Luna I meant LLM and jev").
//! No model id is named here; every layer is whichever model its own
//! configuration points at.
//!
//! 1. The deterministic stall counter (`progress::Stall`) costs nothing and
//!    runs anyway. It is handed to layer 2 **as evidence, not as a gate**: a
//!    model that keeps writing files while repeating a failing call reads as
//!    progress to a counter that watches the tree, so gating the question on
//!    it would hide the loop most worth catching.
//! 2. The decision model (`[decisions] model`) answers one typed `Choice`
//!    question about the trajectory -- a couple of hundred tokens, bounded to
//!    two seconds, no prose. This is what decides *whether* to intervene.
//! 3. The supervisor model (`[supervisor] model`, itself falling back to
//!    `[helpers] model`) is asked for the nudge's one line **only when layer 2
//!    already said yes**, and only to phrase it.
//!
//! Measured 2026-09-17 (session `tlitep-13fv`, 247 turns, 120 cells, 11.8M
//! tokens): every look was a prose request whether anything was wrong or not,
//! so that run would have bought about thirty of them to hear "no" twenty-nine
//! times. With no decision model configured, layer 3 keeps deciding on its own
//! exactly as it did before -- the old path, not a lost one.

use serde::Deserialize;

use crate::config::{DecisionsConfig, SupervisorConfig};
use crate::contract::{Conversation, Message, Role};
use crate::decide;
use crate::runtime::outcome::{CallRecord, CellOutcomeKind, CellRecord, Ended};
use crate::wire;

/// §3's fixed system preamble, verbatim.
const PREAMBLE: &str = "You watch a coding agent's trajectory. Answer with one JSON object \
    {\"intervene\": bool, \"reason\": \"<one line>\"}: intervene only when the agent is looping, \
    repeating a failing call, or has stopped making progress toward the task.";

/// The phrasing preamble: used only after the decision model has already
/// decided to intervene, so it asks for the sentence and never for the
/// judgement. A model asked to re-decide here could quietly overturn layer 2,
/// which is the one thing this layer must not do.
const PHRASE_PREAMBLE: &str = "You watch a coding agent's trajectory. It has been judged to be \
    off track for the stated reason. Answer with one line, under twenty words, telling the agent \
    what it is doing and what to do instead. No preamble, no JSON, no apology.";

/// The look's `max_tokens` -- small, because the only valid answer is one
/// short JSON object, or one short line.
const LOOK_MAX_TOKENS: u32 = 200;

/// The header that lets the ledger tell a look apart from a task turn before
/// the gateway itself reads it (the FEASIBILITY's Class A note).
const PURPOSE_HEADER: (&str, &str) = ("x-glasshouse-purpose", "supervisor");

/// One decision from one look: intervene, or not, and why.
///
/// `ok` separates the two answers that are both *not intervene*: a look that
/// ran and said no (`true`), and a look that produced no answer at all
/// (`false`). §3 records the second **as such**, so a permanently broken
/// supervisor cannot read as a healthy one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub intervene: bool,
    pub reason: String,
    pub ok: bool,
    /// Which criterion layer 2 chose, when it was layer 2 that decided.
    ///
    /// `reason` is prose and may have been written by a model, so it varies
    /// between two looks that decided the same thing; the criterion does not.
    /// A task ended for a repeated verdict names this, never the prose — no
    /// model-written sentence is ever interpolated into a preamble.
    pub criterion: Option<String>,
}

impl Decision {
    /// The failed look: no answer arrived, so `ok` is false -- and it never
    /// intervenes, because a supervisor that could not be asked has said
    /// nothing. Both call sites are failure paths.
    fn not_intervene(reason: impl Into<String>) -> Self {
        Self {
            intervene: false,
            reason: reason.into(),
            ok: false,
            criterion: None,
        }
    }
}

/// One `[supervisor]` session's look. Holds nothing today -- a type rather
/// than a free function so a later model choice has somewhere to live without
/// changing every call site.
pub struct Supervisor;

impl Supervisor {
    pub fn new() -> Self {
        Self
    }

    /// One look through the wire, against `model` -- `[supervisor] model`,
    /// the **cheaper** model map line 2469 names, never the task's own. The
    /// fixed preamble is the system block, `trajectory` the one user message,
    /// `max_tokens` small, the purpose header set. Anything unparseable, a
    /// transport error, or a refused status answers [`Decision::not_intervene`]
    /// -- never a nudge.
    pub fn look(&self, model: &str, trajectory: &str) -> Decision {
        let conversation = Conversation {
            system: PREAMBLE.to_string(),
            messages: vec![Message::text(Role::User, trajectory)],
        };
        match wire::send_turn_with(&conversation, model, LOOK_MAX_TOKENS, Some(PURPOSE_HEADER)) {
            Ok(message) => parse_decision(&message),
            Err(err) => unanswered(&err),
        }
    }

    /// Layer 3: one line for a nudge layer 2 has already decided on. `None`
    /// on any failure -- a nudge whose sentence could not be written is still
    /// a nudge, and [`consider`](Self::consider) falls back to
    /// [`criterion_reason`].
    pub fn phrase(&self, model: &str, trajectory: &str, criterion: &str) -> Option<String> {
        let conversation = Conversation {
            system: PHRASE_PREAMBLE.to_string(),
            messages: vec![Message::text(
                Role::User,
                format!("Reason: {criterion}\n\nTrajectory:\n{trajectory}"),
            )],
        };
        let message =
            wire::send_turn_with(&conversation, model, LOOK_MAX_TOKENS, Some(PURPOSE_HEADER))
                .ok()?;
        let text: String = message
            .content
            .iter()
            .map(crate::contract::Block::text)
            .collect::<Vec<_>>()
            .join("");
        let line = text.trim().lines().next().unwrap_or("").trim().to_string();
        (!line.is_empty()).then_some(line)
    }

    /// One look, layered: the decision model decides, the supervisor model
    /// phrases, and either may be absent.
    ///
    /// * A decision model configured (and `[decisions] mode` not `off`) is
    ///   asked first. Its refusal or silence is a **failed look** -- recorded
    ///   as such, never a nudge, exactly as `supervisor.md` §3 requires of an
    ///   unanswerable look.
    /// * With no decision model, the supervisor model decides on its own
    ///   through [`look`](Self::look): the path this session had before the
    ///   question existed.
    /// * With neither, there is nothing to ask and nothing is nudged.
    ///
    /// `cells_without_change` is `progress::Stall::since_progress` -- evidence
    /// for the question, never a gate on asking it.
    pub fn consider(
        &self,
        supervisor: &SupervisorConfig,
        decisions: &DecisionsConfig,
        trajectory: &str,
        cells_without_change: u32,
    ) -> Decision {
        let classifier = decisions
            .model
            .as_deref()
            .filter(|_| decisions.mode != crate::config::DecisionMode::Off);
        let Some(classifier) = classifier else {
            return match supervisor.model.as_deref() {
                Some(model) => self.look(model, trajectory),
                None => Decision::not_intervene("no supervisor model and no decision model"),
            };
        };
        let answered = match decide::supervision(classifier, trajectory, cells_without_change) {
            Ok(answer) => answer,
            Err(err) => return Decision::not_intervene(format!("decision model: {err}")),
        };
        let Some(criterion) =
            decide::supervision_for(decisions.mode, Some(&answered), decisions.supervision_above)
        else {
            return Decision {
                intervene: false,
                reason: answered.choice,
                ok: true,
                criterion: None,
            };
        };
        let reason = supervisor
            .model
            .as_deref()
            .and_then(|model| self.phrase(model, trajectory, &criterion))
            .unwrap_or_else(|| criterion_reason(&criterion));
        Decision {
            intervene: true,
            reason,
            criterion: Some(criterion),
            ok: true,
        }
    }
}

/// A criterion as a short noun phrase, for the sentence that ends a task.
///
/// Deliberately not [`criterion_reason`], which is an instruction to a model
/// mid-task; this states what was observed, and it is ours rather than any
/// model's words.
#[must_use]
pub fn criterion_phrase(criterion: &str) -> &'static str {
    match criterion {
        "repeating_a_failing_call" => "the same call keeps failing the same way",
        "looping_over_the_same_reads" => "the same files keep being read without a change",
        "stopped_without_returning" => "these cells are not advancing the task",
        _ => "this trajectory is not making progress",
    }
}

/// Whether this session has anything to look with: the switch, and at least
/// one of the two models [`Supervisor::consider`] can ask.
#[must_use]
pub fn active(config: &crate::config::SternaConfig) -> bool {
    config.supervisor.enabled
        && (config.supervisor.model.is_some()
            || (config.decisions.model.is_some()
                && config.decisions.mode != crate::config::DecisionMode::Off))
}

/// A look that got no answer, named by its error alone: a transport error
/// already says the request failed.
fn unanswered(err: &wire::WireError) -> Decision {
    Decision::not_intervene(err.to_string())
}

/// One look's two results: the nudge to head the next message, if any, and
/// what the sidebar and transcript show for it.
///
/// A failed look is recorded **as such** rather than as a healthy one -- §3 --
/// so a permanently broken supervisor cannot read as a quiet one.
#[must_use]
pub fn outcome(decision: Decision) -> (Option<String>, crate::tui::SupervisorStatus) {
    use crate::tui::SupervisorStatus;
    if decision.intervene {
        let reason = decision.reason;
        (Some(reason.clone()), SupervisorStatus::Nudged(reason))
    } else if decision.ok {
        (None, SupervisorStatus::LookedNoNudge)
    } else {
        (None, SupervisorStatus::LookFailed(decision.reason))
    }
}

/// §4: a nudge is the head of the next message, whichever carrier that
/// message uses.
#[must_use]
pub fn headed(reason: &str, text: &str) -> String {
    format!("supervisor: {reason}\n{text}")
}

/// The native dialect's carrier. A model answering with `execute_cell` as a
/// provider-native call gets its feedback as a `tool_result`, not as the text
/// answer, so a nudge written only into the text answer reached exactly the
/// sessions a tool-calling model does not run (found 2026-09-17 by
/// `tests/decisions.rs`).
pub fn head_tool_result(result: &mut Message, reason: &str) {
    for block in &mut result.content {
        if let crate::contract::Block::ToolResult { content, .. } = block {
            *content = headed(reason, content);
        }
    }
}

/// The nudge when no model wrote one: the criterion itself, in words. Worse
/// than a sentence written about this trajectory, and never a lost
/// intervention -- which is the trade this function exists to make.
#[must_use]
pub fn criterion_reason(criterion: &str) -> String {
    match criterion {
        "repeating_a_failing_call" => {
            "the same call keeps failing the same way; change the approach or say what blocks it"
        }
        "looping_over_the_same_reads" => {
            "the same files are being read again without a change following from them; \
             act on what you have or say what is missing"
        }
        "stopped_without_returning" => {
            "these cells are not advancing the task and not ending it; \
             do the next real step or finish with what holds"
        }
        _ => "this trajectory is not making progress; change the approach or say what blocks it",
    }
    .to_string()
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct RawDecision {
    intervene: bool,
    reason: String,
}

fn parse_decision(message: &Message) -> Decision {
    let text: String = message
        .content
        .iter()
        .map(crate::contract::Block::text)
        .collect::<Vec<_>>()
        .join("");
    match serde_json::from_str::<RawDecision>(text.trim()) {
        Ok(raw) => Decision {
            intervene: raw.intervene,
            reason: raw.reason,
            ok: true,
            // The prose look decides in a sentence rather than by criterion;
            // a task ended on a run of these names the generic phrase.
            criterion: None,
        },
        Err(_) => Decision::not_intervene("unparseable"),
    }
}

/// §2: one line per cell since the last look, in cell order.
pub fn compress(cells: &[CellRecord]) -> String {
    cells
        .iter()
        .map(compress_one)
        .collect::<Vec<_>>()
        .join("\n")
}

fn compress_one(cell: &CellRecord) -> String {
    let head = cell.source.lines().next().unwrap_or("").trim();
    // What the model *said* it was doing, beside what it did. The gap between
    // the two is the sharpest loop signal the question can be given — five
    // cells running that say "implementing the parser" while every call is a
    // read — and it costs nothing, because the line was already written
    // (`legibility.md` §2, `runtime::outcome::CellRecord::description`).
    let said = cell
        .description
        .as_deref()
        .map(|said| format!("said: {said} · "))
        .unwrap_or_default();
    let outcome = match cell.outcome {
        CellOutcomeKind::Yielded => "yielded",
        CellOutcomeKind::Returned => "returned",
        CellOutcomeKind::Threw => "threw",
    };
    let calls = if cell.calls.is_empty() {
        "(none)".to_string()
    } else {
        cell.calls
            .iter()
            .map(render_call)
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "cell {} {outcome} · {said}{head} · calls: {calls}",
        cell.cell
    )
}

fn render_call(call: &CallRecord) -> String {
    let args: String = call
        .args
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ");
    let ended = match &call.ended {
        Ended::Ok => "ended".to_string(),
        Ended::Threw { class } => format!("threw {class}"),
        Ended::Denied { rule } => format!("denied {rule}"),
    };
    format!("{}({args})→{ended}", call.tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn a_look_that_never_reached_the_endpoint_says_so_once() {
        let err = wire::WireError::Http(Box::new(ureq::Error::Io(std::io::Error::other(
            "connection refused",
        ))));
        let reason = unanswered(&err).reason;
        assert_eq!(reason.matches("request failed").count(), 1, "{reason}");
        assert!(reason.contains("connection refused"), "{reason}");
    }

    #[test]
    fn compress_renders_the_head_the_outcome_and_the_calls_never_a_payload() {
        let cell = CellRecord {
            cell: 1,
            source: "const hits = await grep({ pattern: \"x\" });\nconst n = hits.length;".into(),
            description: None,
            outcome: CellOutcomeKind::Yielded,
            handles: Vec::new(),
            calls: vec![CallRecord {
                tool: "grep".into(),
                args: BTreeMap::from([("pattern".to_string(), "x".to_string())]),
                evidence: None,
                lifted_from: None,
                exit_code: None,
                repeat_of: None,
                error: None,
                ended: Ended::Ok,
            }],
        };
        let line = compress(&[cell]);
        assert_eq!(
            line,
            "cell 1 yielded · const hits = await grep({ pattern: \"x\" }); · calls: \
             grep(pattern=x)→ended"
        );
    }

    /// The trajectory carries what the model *said* beside what it did, so the
    /// question can see the gap between them — five cells saying "implementing
    /// the parser" while every call is a read. Still no payload and still no
    /// preview bytes: the sentence is the model's own, already in the rollout.
    #[test]
    fn the_trajectory_says_what_the_model_said_it_was_doing() {
        let cell = CellRecord {
            cell: 7,
            source: "await rg({ pattern: \"refuse\" });".into(),
            description: Some("Searching for the command-refusal grammar.".into()),
            outcome: CellOutcomeKind::Yielded,
            handles: Vec::new(),
            calls: vec![CallRecord {
                tool: "rg".into(),
                args: BTreeMap::from([("pattern".to_string(), "refuse".to_string())]),
                evidence: None,
                lifted_from: None,
                exit_code: None,
                repeat_of: None,
                error: None,
                ended: Ended::Ok,
            }],
        };
        assert_eq!(
            compress(&[cell]),
            "cell 7 yielded · said: Searching for the command-refusal grammar. · \
             await rg({ pattern: \"refuse\" }); · calls: rg(pattern=refuse)→ended"
        );
    }

    #[test]
    fn with_neither_model_nothing_is_asked_and_nothing_is_nudged() {
        let supervisor = Supervisor::new();
        let decision = supervisor.consider(
            &SupervisorConfig {
                model: None,
                ..SupervisorConfig::default()
            },
            &DecisionsConfig::default(),
            "cell 1 yielded · const x = 1; · calls: (none)",
            3,
        );
        assert!(!decision.intervene);
        assert!(!decision.ok, "nothing was asked, so no look succeeded");
    }

    #[test]
    fn an_unknown_criterion_still_says_something_a_model_can_act_on() {
        assert_eq!(
            criterion_reason("something_new_the_question_grew"),
            "this trajectory is not making progress; change the approach or say what blocks it"
        );
        assert!(criterion_reason("repeating_a_failing_call").contains("same call keeps failing"));
    }

    #[test]
    fn an_unparseable_answer_is_not_intervene() {
        let message = Message::text(Role::Assistant, "not json");
        assert_eq!(
            parse_decision(&message),
            Decision::not_intervene("unparseable")
        );
    }

    #[test]
    fn a_well_formed_answer_carries_its_own_reason() {
        let message = Message::text(
            Role::Assistant,
            r#"{"intervene": true, "reason": "looping"}"#,
        );
        assert_eq!(
            parse_decision(&message),
            Decision {
                intervene: true,
                reason: "looping".to_string(),
                ok: true,
                criterion: None,
            }
        );
    }
}
