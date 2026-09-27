//! Answering the question a cell asked: the decision model first, the person
//! second, and the program's own turn either way.
//!
//! **Jev never removes the person's choice; it either explains it or takes
//! it.** `weight` puts its reading beside each option and the person still
//! decides. `decide` answers instead of them, but only above
//! `[ask] decide_above`, and then it says so in the transcript — a decision
//! nobody saw being made must at least be one they can read afterwards.
//!
//! **Every failure lands on the person, never on the task.** A decision model
//! that errors or times out is a question without weights, not a question
//! that went unasked; `decide.rs` is fail-open by construction and this
//! module keeps that property.

use super::{Session, task::TaskState};
use crate::ask::{Answer, AnsweredBy, Question, Weights};
use crate::config::AskJev;

/// Why `ask` is refused in this session, or `None` when a cell may use it.
///
/// Read once per request, because all three inputs can change between
/// requests: a `/config` edit, a mode switch, and whether a terminal is
/// attached at all.
pub(super) fn refusal(
    session: &Session<'_>,
    mode: crate::sandbox::modes::RequestMode,
) -> Option<String> {
    if session.ask_gate.is_none() {
        return Some(crate::ask::NOT_AVAILABLE.to_string());
    }
    if !session.config().ask.enabled {
        return Some(crate::ask::DISABLED.to_string());
    }
    if mode == crate::sandbox::modes::RequestMode::Explore {
        return Some(crate::ask::NOT_IN_EXPLORE.to_string());
    }
    None
}

/// Answers `question`, weighing it against this task's request, diff and
/// findings first when `[ask] jev` asks for that.
pub(super) fn resolve(
    question: Question,
    after: &crate::changes::Snapshot,
    session: &Session<'_>,
    task_state: &TaskState,
) -> Answer {
    let Some(gate) = session.ask_gate.clone() else {
        return Answer::dismissed();
    };
    let config = session.config().ask;
    let weights = weigh(&question, after, session, task_state, config.jev);

    if let Some(weights) = &weights
        && weights.decides(config.jev, config.decide_above)
    {
        // Said out loud, once: a person who was not interrupted is entitled
        // to read afterwards what was decided for them, and at what odds.
        super::ui::output(format!(
            "ask: answered `{}` at {:.2} without asking you — {}",
            weights.choice, weights.confidence, question.question
        ));
        return Answer {
            choice: Some(weights.choice.clone()),
            by: AnsweredBy::Decision {
                confidence: weights.confidence,
            },
        };
    }

    gate.put(question, weights)
}

/// The decision model's reading of the question, or `None` when it was not
/// asked for, no model is configured, or the request did not answer.
fn weigh(
    question: &Question,
    after: &crate::changes::Snapshot,
    session: &Session<'_>,
    task_state: &TaskState,
    jev: AskJev,
) -> Option<Weights> {
    if jev == AskJev::Off {
        return None;
    }
    let model = session.config().decisions.model.clone()?;
    let diff = task_state.task_start.diff(after).unwrap_or_default();
    let findings = task_state.deferred_findings.clone().unwrap_or_default();
    let judged = crate::decide::asked(
        &model,
        &task_state.task,
        &diff,
        &findings,
        &question.question,
        &question.choices,
    )
    .ok()?;
    // Keyed by the choice text the program offered, so a model that answered
    // with a name the program never listed weighs nothing rather than
    // shifting the numbers beside the choices that do exist.
    let probabilities = question
        .choices
        .iter()
        .map(|choice| judged.probabilities.get(choice).copied().unwrap_or(0.0))
        .collect();
    if !question.choices.contains(&judged.choice) {
        return None;
    }
    Some(Weights {
        probabilities,
        choice: judged.choice,
        confidence: judged.confidence,
    })
}

#[cfg(test)]
mod tests {
    use crate::ask::{Answer, AnsweredBy, Question, Weights};

    /// The panel's own ordering rule, checked here because the renderer and
    /// the weigher must agree that probabilities are in choice order.
    #[test]
    fn weights_line_up_with_the_choices_they_belong_to() {
        let question = Question::new("Which?", vec!["a".into(), "b".into(), "c".into()]).unwrap();
        let weights = Weights {
            probabilities: vec![0.1, 0.7, 0.2],
            choice: "b".into(),
            confidence: 0.7,
        };
        assert_eq!(question.choices.len(), weights.probabilities.len());
        let best = weights
            .probabilities
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index)
            .unwrap();
        assert_eq!(question.choices[best], weights.choice);
    }

    #[test]
    fn a_decided_answer_never_claims_the_person_gave_it() {
        let answer = Answer {
            choice: Some("a".into()),
            by: AnsweredBy::Decision { confidence: 0.99 },
        };
        assert!(!answer.rendered().contains("the person chose"));
    }
}
