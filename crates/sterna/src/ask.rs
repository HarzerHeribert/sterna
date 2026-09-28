//! A question the running program put to the person, and the answer that
//! comes back.
//!
//! **Asking never suspends a cell.** `ask(question, choices)` ends the cell
//! the way `yieldNow` does (`runtime-contract.md` §9.3), and the answer
//! arrives as an observation on the next turn. A person thinks for longer
//! than a cell is allowed to run, and a design where the program waits is one
//! where a program can stall the session by asking.
//!
//! **A session with nobody at the keyboard throws instead.** `ask` is a
//! capability a live terminal supplies, so `sterna session --task`, the ruler
//! and a subagent get a catchable `ToolError` at the call and carry on. That
//! is checked where the call is made, not here, so the program can `try` it.

use std::sync::mpsc;
use std::time::Duration;

/// How long the session waits for a person before answering itself. The same
/// bound [`crate::approval::MAX_APPROVAL_WAIT`] puts on a confirmation: both
/// are a person deciding, and they differ only in what is being decided.
pub const MAX_ASK_WAIT: Duration = Duration::from_secs(10 * 60);

/// The most choices one question may offer. Nine, because the picker gives
/// every choice its own digit and a tenth would need a second keystroke for
/// no gain: a question with more than nine answers is a question that has not
/// been thought through.
pub const MAX_CHOICES: usize = 9;

/// The most characters a question or a choice may carry. A question is a line
/// on a panel, not a document; the program has the whole cell to compute a
/// short one.
pub const MAX_TEXT: usize = 200;

/// What `ask` throws where there is nobody to ask: a non-interactive
/// session, a subagent, the ruler. Spelled once so the callback, the
/// declaration the model reads and the test all say the same thing.
pub const NOT_AVAILABLE: &str =
    "ask: no one is at this session to ask, so decide yourself and say what you assumed";

/// What `ask` throws where a person is present but asking is switched off.
pub const DISABLED: &str =
    "ask: asking is off for this session (`[ask] enabled`), so decide yourself";

/// One question, as the program composed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    pub choices: Vec<String>,
}

impl Question {
    /// The question a program asked, or why it is not one that can be put to
    /// a person. Bounds are checked here so the callback, the panel and the
    /// rendered observation cannot disagree about what a question is.
    pub fn new(question: &str, choices: Vec<String>) -> Result<Self, String> {
        let question = question.trim();
        if question.is_empty() {
            return Err("ask needs a question".to_string());
        }
        if question.chars().count() > MAX_TEXT {
            return Err(format!(
                "ask: the question is longer than {MAX_TEXT} characters"
            ));
        }
        if choices.len() < 2 {
            return Err("ask needs at least two choices to choose between".to_string());
        }
        if choices.len() > MAX_CHOICES {
            return Err(format!("ask takes at most {MAX_CHOICES} choices"));
        }
        for choice in &choices {
            if choice.trim().is_empty() {
                return Err("ask: every choice needs a name".to_string());
            }
            if choice.chars().count() > MAX_TEXT {
                return Err(format!(
                    "ask: a choice is longer than {MAX_TEXT} characters"
                ));
            }
        }
        Ok(Self {
            question: question.to_string(),
            choices: choices.iter().map(|c| c.trim().to_string()).collect(),
        })
    }
}

/// Who answered, which the observation says out loud: a model must never read
/// the decision model's guess as the person's word.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnsweredBy {
    /// The person chose.
    Person,
    /// `[ask] jev = "decide"` was confident enough to answer instead.
    Decision { confidence: f64 },
    /// Nobody chose: the person dismissed the question, or the session ended
    /// before they answered.
    NoOne,
}

/// The answer to one question.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// The chosen text, or `None` when nobody chose.
    pub choice: Option<String>,
    pub by: AnsweredBy,
}

impl Answer {
    #[must_use]
    pub fn dismissed() -> Self {
        Self {
            choice: None,
            by: AnsweredBy::NoOne,
        }
    }

    /// The card's row for this answer: what was asked and who chose what.
    #[must_use]
    pub fn row(&self, question: &str) -> String {
        match (&self.choice, self.by) {
            (Some(choice), AnsweredBy::Person) => format!("? {question} → you chose: {choice}"),
            (Some(choice), AnsweredBy::Decision { .. }) => {
                format!("? {question} → Sterna decided: {choice}")
            }
            (Some(choice), AnsweredBy::NoOne) => format!("? {question} → {choice}"),
            (None, _) => format!("? {question} → nobody chose"),
        }
    }

    /// The observation the next turn reads. It names who answered, because a
    /// program that branches on the answer is entitled to know whether a
    /// person actually looked at it.
    #[must_use]
    pub fn rendered(&self) -> String {
        match (&self.choice, self.by) {
            (Some(choice), AnsweredBy::Person) => {
                format!("{choice}\n(you asked; the person chose)")
            }
            (Some(choice), AnsweredBy::Decision { confidence }) => format!(
                "{choice}\n(you asked; the decision model chose at {confidence:.2} and the person was not interrupted)"
            ),
            (Some(choice), AnsweredBy::NoOne) => choice.clone(),
            (None, _) => "no choice — nobody answered, so decide yourself and say what you assumed"
                .to_string(),
        }
    }
}

/// The decision model's reading of a question, when one was asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Weights {
    /// Each choice's probability, in the question's own choice order.
    pub probabilities: Vec<f64>,
    /// The choice the model would make, and how sure it is.
    pub choice: String,
    pub confidence: f64,
}

impl Weights {
    /// Whether this reading answers the question instead of the person.
    ///
    /// **At the threshold it decides.** `decide_above` is the confidence a
    /// person said they would accept, and an answer that lands exactly on it
    /// is one they said was good enough; the same inclusive reading every
    /// other confidence in `[decisions]` takes.
    #[must_use]
    pub fn decides(&self, jev: crate::config::AskJev, decide_above: f64) -> bool {
        jev == crate::config::AskJev::Decide && self.confidence >= decide_above
    }
}

/// One question on its way to the person, with the answer channel it is
/// answered on.
pub struct Request {
    question: Question,
    weights: Option<Weights>,
    reply: mpsc::SyncSender<Answer>,
}

impl Request {
    #[must_use]
    pub fn question(&self) -> &Question {
        &self.question
    }

    /// The decision model's reading, when `[ask] jev` asked for one and it
    /// answered. `None` is the ordinary case and the panel shows choices
    /// alone.
    #[must_use]
    pub fn weights(&self) -> Option<&Weights> {
        self.weights.as_ref()
    }

    /// Answers the waiting session thread. `false` when it has already gone.
    pub fn respond(self, answer: Answer) -> bool {
        self.reply.send(answer).is_ok()
    }
}

/// The session thread's end of the channel: put one question, wait for one
/// answer.
#[derive(Clone)]
pub struct Gate {
    requests: mpsc::Sender<Request>,
}

impl Gate {
    /// A gate and the requests it will send. The caller owns the receiving
    /// end and is the only thing that can answer.
    #[must_use]
    pub fn channel() -> (Self, mpsc::Receiver<Request>) {
        let (requests, receiver) = mpsc::channel();
        (Self { requests }, receiver)
    }

    /// Puts `question` to the person and waits [`MAX_ASK_WAIT`] for an
    /// answer.
    ///
    /// **It cannot fail into waiting.** A closed channel, a dropped terminal
    /// and a person who never answers all produce [`Answer::dismissed`], so a
    /// question is at worst one observation saying nobody chose.
    #[must_use]
    pub fn put(&self, question: Question, weights: Option<Weights>) -> Answer {
        let (reply, answers) = mpsc::sync_channel(1);
        let request = Request {
            question,
            weights,
            reply,
        };
        if self.requests.send(request).is_err() {
            return Answer::dismissed();
        }
        answers
            .recv_timeout(MAX_ASK_WAIT)
            .unwrap_or_else(|_| Answer::dismissed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_is_bounded_in_length_and_in_how_many_answers_it_offers() {
        assert!(Question::new("Which?", vec!["a".into(), "b".into()]).is_ok());
        assert!(Question::new("  ", vec!["a".into(), "b".into()]).is_err());
        assert!(Question::new("Which?", vec!["only".into()]).is_err());
        let ten: Vec<String> = (0..10).map(|n| n.to_string()).collect();
        assert!(Question::new("Which?", ten).is_err());
        let long = "x".repeat(MAX_TEXT + 1);
        assert!(Question::new(&long, vec!["a".into(), "b".into()]).is_err());
        assert!(Question::new("Which?", vec!["a".into(), long]).is_err());
        assert!(Question::new("Which?", vec!["a".into(), " ".into()]).is_err());
    }

    #[test]
    fn a_dismissed_question_tells_the_model_to_decide_for_itself() {
        let rendered = Answer::dismissed().rendered();
        assert!(rendered.contains("decide yourself"), "{rendered}");
    }

    /// The observation never lets the decision model's guess read as the
    /// person's word.
    #[test]
    fn an_answer_says_who_gave_it() {
        let person = Answer {
            choice: Some("rename only".into()),
            by: AnsweredBy::Person,
        };
        assert!(person.rendered().contains("the person chose"));
        let model = Answer {
            choice: Some("rename only".into()),
            by: AnsweredBy::Decision { confidence: 0.93 },
        };
        let rendered = model.rendered();
        assert!(
            rendered.contains("decision model chose at 0.93"),
            "{rendered}"
        );
        assert!(!rendered.contains("the person chose"), "{rendered}");
    }

    /// The threshold is inclusive, and `weight` never decides however sure
    /// the model is.
    #[test]
    fn only_decide_mode_answers_for_the_person_and_only_at_or_above_the_bar() {
        let weights = |confidence| Weights {
            probabilities: vec![1.0 - confidence, confidence],
            choice: "b".into(),
            confidence,
        };
        assert!(weights(0.85).decides(crate::config::AskJev::Decide, 0.85));
        assert!(weights(0.86).decides(crate::config::AskJev::Decide, 0.85));
        assert!(!weights(0.84).decides(crate::config::AskJev::Decide, 0.85));
        assert!(
            !weights(1.0).decides(crate::config::AskJev::Weight, 0.85),
            "weighting shows the reading; it never takes the choice"
        );
        assert!(!weights(1.0).decides(crate::config::AskJev::Off, 0.85));
    }

    #[test]
    fn a_gate_whose_receiver_is_gone_answers_dismissed_rather_than_waiting() {
        let (gate, receiver) = Gate::channel();
        drop(receiver);
        let answer = gate.put(
            Question::new("Which?", vec!["a".into(), "b".into()]).unwrap(),
            None,
        );
        assert_eq!(answer, Answer::dismissed());
    }

    #[test]
    fn a_question_reaches_the_receiver_with_its_weights_and_is_answered() {
        let (gate, receiver) = Gate::channel();
        let answering = std::thread::spawn(move || {
            let request = receiver.recv().unwrap();
            assert_eq!(request.question().choices, vec!["a", "b"]);
            assert_eq!(request.weights().map(|w| w.confidence), Some(0.7));
            request.respond(Answer {
                choice: Some("b".into()),
                by: AnsweredBy::Person,
            });
        });
        let answer = gate.put(
            Question::new("Which?", vec!["a".into(), "b".into()]).unwrap(),
            Some(Weights {
                probabilities: vec![0.3, 0.7],
                choice: "b".into(),
                confidence: 0.7,
            }),
        );
        answering.join().unwrap();
        assert_eq!(answer.choice.as_deref(), Some("b"));
    }
}
