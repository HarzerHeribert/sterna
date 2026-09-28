//! The decision model: typed questions over the gateway, and the one hold a
//! read-only intent buys against an effectful cell or direct frame
//! (`docs/decisions.md`).
//!
//! A decision adds a hold or a count. It never adds a capability, grant or
//! approval; a held cell re-issued passes exactly the checks it passed
//! before. The request carries the request text and goes only to the
//! gateway's base URL with the gateway's own credential -- no secret enters
//! this module, and [`DecideError`] and every notice or telemetry string
//! carry no body beyond [`ERROR_BODY_LIMIT`] bytes.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::DecisionMode;
use crate::wire;

/// The decision endpoint's path, appended to [`wire::base_url`] exactly as
/// the Messages path is appended in `wire.rs`.
const DECISION_PATH: &str = "/v1/systemone";
/// The decision model a session uses when none is configured and the
/// gateway serves a TypeSafe account (`session/startup.rs`).
pub const DEFAULT_MODEL: &str = "jev-latest";

/// The header the gateway routes the decision request on, mirroring
/// `wire::MODEL_HEADER`'s name -- private there, so it is spelled once more
/// here rather than widening that module's visibility for one literal.
const MODEL_HEADER: &str = "x-glasshouse-model";

/// The purpose header, so the gateway and the ledger can tell a decision
/// request from a task turn or a helper call (`helpers.rs::PURPOSE_HEADER`
/// is the same shape for `"helper"`).
const PURPOSE_HEADER: (&str, &str) = ("x-glasshouse-purpose", "decision");

/// The decision request's own bound: past it an answer is lost, not late.
/// Fifteen seconds since 2026-09-23 (the user: "timing Jev out after 2
/// seconds is dumb; measuring it isn't") -- that evening Jev answered in
/// 1.8-3.0 s, and a 2 s bound threw away nearly every answer. Nothing a
/// person waits on holds for it: `shadow` collects its task decision after
/// the model's turn (`session/system.rs`).
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(15);

/// The most of a request or response body an error or notice carries.
const ERROR_BODY_LIMIT: usize = 200;

/// The most of the task diff the completion question's `state.diff` carries
/// (2616). Cut at a hunk boundary (`bound_diff`) rather than a raw byte
/// count, so a kept hunk is never split mid-way; a cut diff still gets a
/// question, with `state.diff_truncated = true`.
pub const DIFF_STATE_BYTES: usize = 64 * 1024;

/// One question sent to the decision model. `Noul` is a numeric confidence
/// question the wire protocol supports; this package asks only `Choice`
/// questions, and `Noul` stays here as the wire's other documented shape.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Noul {
        instructions: String,
    },
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
}

/// One answer, decoded from the documented response shape. `probabilities`
/// is the model's distribution over every named criterion, `confidence` its
/// probability on the returned `choice`.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Noul(f64),
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

/// One question's answer plus the round-trip latency of the request that
/// carried it -- every answer from one call shares the same latency.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub key: String,
    pub answer: Answer,
    pub latency_ms: u64,
}

/// One `decide` call's answers, keyed by the questions asked.
#[derive(Debug, Clone, PartialEq)]
pub struct Answers {
    pub model: String,
    pub decisions: Vec<Decision>,
}

/// Everything that can go wrong asking the decision model. Every variant's
/// `Display` carries no body beyond [`ERROR_BODY_LIMIT`] bytes -- the state a
/// caller sends may hold the request verbatim.
#[derive(Debug)]
pub enum DecideError {
    Status { status: u16, body_head: String },
    Transport(String),
    Timeout,
    Parse(String),
}

impl fmt::Display for DecideError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecideError::Status { status, body_head } => {
                write!(f, "http {status} — {body_head}")
            }
            DecideError::Transport(message) => write!(f, "transport error: {message}"),
            DecideError::Timeout => write!(f, "timeout after {} ms", DECISION_TIMEOUT.as_millis()),
            DecideError::Parse(message) => write!(f, "could not parse response: {message}"),
        }
    }
}

fn truncate(text: &str) -> String {
    let mut cut = text.len().min(ERROR_BODY_LIMIT);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

#[derive(Serialize)]
struct RequestBody<'a> {
    state: &'a Value,
    model: &'a str,
    questions: BTreeMap<String, Question>,
}

#[derive(Deserialize)]
struct ResponseBody {
    model: String,
    answers: BTreeMap<String, RawAnswer>,
    #[allow(dead_code)]
    usage: UsageFields,
}

#[derive(Deserialize)]
struct UsageFields {
    #[allow(dead_code)]
    input_tokens: u64,
    #[allow(dead_code)]
    output_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum RawAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

impl From<RawAnswer> for Answer {
    fn from(raw: RawAnswer) -> Self {
        match raw {
            RawAnswer::Noul { noul } => Answer::Noul(noul),
            RawAnswer::Choice {
                choice,
                probabilities,
                confidence,
            } => Answer::Choice {
                choice,
                probabilities,
                confidence,
            },
        }
    }
}

/// One metered request to the decision model: `state` plus every question,
/// answered with one [`Decision`] per question. Bounded to
/// [`DECISION_TIMEOUT`], never the 120 s side-errand timeout -- a decision is
/// asked before the model's first turn, and nothing about a task waits on it
/// past this bound.
pub fn decide(
    model: &str,
    state: Value,
    questions: &[(String, Question)],
) -> Result<Answers, DecideError> {
    let url = format!("{}{DECISION_PATH}", wire::base_url());
    let body = RequestBody {
        state: &state,
        model,
        questions: questions.iter().cloned().collect(),
    };
    let payload = serde_json::to_vec(&body)
        .map_err(|error| DecideError::Parse(truncate(&error.to_string())))?;

    let mut request = ureq::post(&url)
        .config()
        .http_status_as_error(false)
        .timeout_global(Some(DECISION_TIMEOUT))
        .build()
        .header("content-type", "application/json")
        .header(MODEL_HEADER, model)
        .header(PURPOSE_HEADER.0, PURPOSE_HEADER.1);
    if let Some((name, value)) = wire::credential_header() {
        request = request.header(name, value);
    }

    let started = Instant::now();
    let mut response = request
        .send(payload.as_slice())
        .map_err(|error| match error {
            ureq::Error::Timeout(_) => DecideError::Timeout,
            other => DecideError::Transport(truncate(&other.to_string())),
        })?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| DecideError::Transport(truncate(&error.to_string())))?;
    if !response.status().is_success() {
        return Err(DecideError::Status {
            status,
            body_head: truncate(&text),
        });
    }
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let parsed: ResponseBody = serde_json::from_str(&text)
        .map_err(|error| DecideError::Parse(truncate(&error.to_string())))?;
    // A question the model left unanswered costs that answer only: each
    // caller names the answers it cannot do without (a newer question, such
    // as `kind`, must never take the older ones down with it).
    let decisions: Vec<Decision> = questions
        .iter()
        .filter_map(|(key, _)| {
            let raw = parsed.answers.get(key).cloned()?;
            Some(Decision {
                key: key.clone(),
                answer: Answer::from(raw),
                latency_ms,
            })
        })
        .collect();
    if decisions.is_empty() {
        return Err(DecideError::Parse(
            "the response answered no question".to_string(),
        ));
    }

    Ok(Answers {
        model: parsed.model,
        decisions,
    })
}

/// The one string the intent question's answer holds when this package
/// should consider holding: `Hold::hold_for` compares against this exact
/// spelling, which is also the criterion's own key below.
/// `session/task.rs::gate` compares against it too, to choose the
/// completion question's answer state over its diff state (2641/2642's
/// addendum to 2616).
pub const READ_ONLY: &str = "read_only";

const INTENT_KEY: &str = "intent";

/// The choice `preflight::should_scout`'s decided signal fires on -- the
/// criterion's own key below, mirroring [`READ_ONLY`]'s shape for the other
/// question this package asks in the same request.
pub const NEEDS_EXPLORATION: &str = "needs_exploration";

const COMPLEXITY_KEY: &str = "complexity";
/// The kind question's key (2026-09-23).
const KIND_KEY: &str = "kind";

fn intent_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        READ_ONLY.to_string(),
        "answering, explaining, reading, searching or inspecting files, output or history; \
         nothing on disk is meant to change"
            .to_string(),
    );
    criteria.insert(
        "modify".to_string(),
        "creating, editing, deleting, moving or renaming files, or changing configuration"
            .to_string(),
    );
    criteria.insert(
        "run".to_string(),
        "building, testing, running or executing commands whose side effects are the point"
            .to_string(),
    );
    criteria.insert(
        "other".to_string(),
        "none of the above, or too unclear to say".to_string(),
    );
    Question::Choice {
        instructions: "What does this request intend?".to_string(),
        criteria,
    }
}

/// The complexity question (F2, map 2614/2615's paragraph): asked in the
/// same request as [`intent_question`], never a second round trip
/// (`docs/decisions.md`).
fn complexity_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        "trivial".to_string(),
        "one obvious edit or answer, no exploration needed".to_string(),
    );
    criteria.insert(
        "routine".to_string(),
        "a known shape of change in a known place".to_string(),
    );
    criteria.insert(
        NEEDS_EXPLORATION.to_string(),
        "the request needs the project read or searched before any change is safe".to_string(),
    );
    Question::Choice {
        instructions: "How much exploration does this request need before it is safe to act?"
            .to_string(),
        criteria,
    }
}

/// What the decision model answered about one request's intent.
#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    pub choice: String,
    pub confidence: f64,
    pub latency_ms: u64,
}

/// What the decision model answered about how much exploration one request
/// needs -- asked beside [`Intent`] in the same request, never on its own
/// (`preflight::should_scout`'s fifth signal, F2).
#[derive(Debug, Clone, PartialEq)]
pub struct Complexity {
    pub choice: String,
    pub confidence: f64,
}

/// What the decision model answered about what kind of work one request is
/// (2026-09-23): asked beside [`Intent`] and [`Complexity`] in the same
/// request. `explore` and `question` lower the task's effort when the
/// person chose none; `explore` briefs the Scout to dissect the request
/// (`preflight::Brief::Dissection`).
#[derive(Debug, Clone, PartialEq)]
pub struct Kind {
    pub choice: String,
    pub confidence: f64,
}

/// The confidence at or above which a [`Kind`] answer acts: lowers the
/// effort, briefs the Scout to dissect.
pub const KIND_ABOVE: f64 = 0.7;

/// Understand, survey or explain a project or an area of it.
pub const KIND_EXPLORE: &str = "explore";
/// Make a named failure stop.
pub const KIND_FIX: &str = "fix";
/// Add or change behaviour.
pub const KIND_IMPLEMENT: &str = "implement";
/// Answer from what is known, or one quick look.
pub const KIND_QUESTION: &str = "question";
/// Run, build, test or execute something and report.
pub const KIND_RUN: &str = "run";

/// All three answers to the one request asked before a task's first turn.
/// `kind` is `None` when the decision model answered the older two questions
/// and not the third.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskDecision {
    pub intent: Intent,
    pub complexity: Complexity,
    pub kind: Option<Kind>,
}

impl TaskDecision {
    /// The kind, when it was answered at or above [`KIND_ABOVE`].
    #[must_use]
    pub fn confident_kind(&self) -> Option<&str> {
        self.kind
            .as_ref()
            .filter(|kind| kind.confidence >= KIND_ABOVE)
            .map(|kind| kind.choice.as_str())
    }
}

/// The kind question (2026-09-23), asked in the same request as
/// [`intent_question`] and [`complexity_question`]: what kind of work is
/// this, which is what decides how much thinking the first turn needs and
/// what the Scout is briefed to do.
fn kind_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        KIND_EXPLORE.to_string(),
        "understand, survey or explain how a project or an area of it works; the answer is \
         what was found, not a change"
            .to_string(),
    );
    criteria.insert(
        KIND_FIX.to_string(),
        "make a named failure stop: a bug, a red test, an error".to_string(),
    );
    criteria.insert(
        KIND_IMPLEMENT.to_string(),
        "add or change behaviour that does not exist yet".to_string(),
    );
    criteria.insert(
        KIND_QUESTION.to_string(),
        "answer from what is already known or one quick look; no change and no survey".to_string(),
    );
    criteria.insert(
        KIND_RUN.to_string(),
        "run, build, test or execute something and report what happened".to_string(),
    );
    Question::Choice {
        instructions: "What kind of work is this request?".to_string(),
        criteria,
    }
}

/// Asks the intent and complexity questions about `request` in one request
/// and answers with what came back, or the reason it did not. Never
/// surfaced as a task failure -- the caller records the error in a notice
/// and proceeds exactly as if no decision model were configured.
/// What the session already has when a request arrives: how much exploring
/// a request needs depends on it (2026-09-23 -- the user: "does Jev know
/// what's already in context?"). A follow-up to requests that already read
/// the area, or a project whose instructions say how to build and test,
/// needs less than the same words in a cold session.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct TaskContext {
    /// Earlier requests in this session; `0` is a cold start.
    pub earlier_requests: u32,
    /// The project's instruction files, as `NAME (N lines)`.
    pub instructions: Vec<String>,
    /// Whether those instructions name a build or test command.
    pub instructions_name_commands: bool,
}

impl TaskContext {
    /// The context of a request in a project with these instruction files,
    /// after `earlier_requests` requests in this session.
    #[must_use]
    pub fn of(earlier_requests: u32, instructions: &[(std::path::PathBuf, String)]) -> Self {
        const COMMANDS: [&str; 9] = [
            "cargo test",
            "cargo build",
            "pytest",
            "npm test",
            "npm run",
            "make ",
            "go test",
            "pnpm ",
            "just ",
        ];
        Self {
            earlier_requests,
            instructions: instructions
                .iter()
                .map(|(path, text)| {
                    format!(
                        "{} ({} lines)",
                        path.file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                        text.lines().count()
                    )
                })
                .collect(),
            instructions_name_commands: instructions
                .iter()
                .any(|(_, text)| COMMANDS.iter().any(|command| text.contains(command))),
        }
    }
}

pub fn task_questions(model: &str, request: &str) -> Result<TaskDecision, DecideError> {
    task_questions_in(model, request, None)
}

/// [`task_questions`] with what the session already has beside the request.
pub fn task_questions_in(
    model: &str,
    request: &str,
    context: Option<&TaskContext>,
) -> Result<TaskDecision, DecideError> {
    let state = match context {
        Some(context) => serde_json::json!({ "request": request, "session": context }),
        None => serde_json::json!({ "request": request }),
    };
    let questions = [
        (INTENT_KEY.to_string(), intent_question()),
        (COMPLEXITY_KEY.to_string(), complexity_question()),
        (KIND_KEY.to_string(), kind_question()),
    ];
    let answers = decide(model, state, &questions)?;
    task_decision_of(answers)
}

/// The decoding half of [`task_questions`], pulled out so a unit test can
/// exercise it against a scripted [`Answers`] with no network involved.
fn task_decision_of(answers: Answers) -> Result<TaskDecision, DecideError> {
    let mut intent = None;
    let mut complexity = None;
    let mut kind = None;
    for decision in answers.decisions {
        let Answer::Choice {
            choice, confidence, ..
        } = decision.answer
        else {
            return Err(DecideError::Parse(format!(
                "the `{}` question was answered as a noul, not a choice",
                decision.key
            )));
        };
        match decision.key.as_str() {
            key if key == INTENT_KEY => {
                intent = Some(Intent {
                    choice,
                    confidence,
                    latency_ms: decision.latency_ms,
                });
            }
            key if key == COMPLEXITY_KEY => {
                complexity = Some(Complexity { choice, confidence });
            }
            key if key == KIND_KEY => {
                kind = Some(Kind { choice, confidence });
            }
            other => {
                return Err(DecideError::Parse(format!(
                    "unexpected answer key `{other}`"
                )));
            }
        }
    }
    let intent =
        intent.ok_or_else(|| DecideError::Parse(format!("no answer for `{INTENT_KEY}`")))?;
    let complexity = complexity
        .ok_or_else(|| DecideError::Parse(format!("no answer for `{COMPLEXITY_KEY}`")))?;
    Ok(TaskDecision {
        intent,
        complexity,
        kind,
    })
}

/// The completion question's key, mirroring [`INTENT_KEY`]'s shape for the
/// other question this package asks.
const SATISFIED_KEY: &str = "satisfied";

/// The five diff-hygiene questions' keys (2641).
const HYGIENE_HAS_TESTS_KEY: &str = "has_tests";
const HYGIENE_OUT_OF_SCOPE_KEY: &str = "out_of_scope";
const HYGIENE_DEBUG_LEFTOVERS_KEY: &str = "debug_leftovers";
const HYGIENE_DELETES_TESTS_KEY: &str = "deletes_tests";
const HYGIENE_CHANGES_SIGNATURE_KEY: &str = "changes_signature";

/// What the completion question's shared `state` carries: the diff and the
/// mechanical findings already found, when there is a diff to show -- or,
/// when the diff is empty or the task's intent was [`READ_ONLY`], the
/// model's own answer text instead. Asking about a diff that does not exist
/// is the empty-diff defect Phase 66's shadow calibration measured: a
/// read-only, answer-only task scored 0.10 against a diff state that was
/// never anything but the placeholder sentence.
pub enum CompletionState<'a> {
    Diff {
        diff: &'a str,
        findings: &'a [String],
    },
    Answer {
        answer: &'a str,
    },
}

fn satisfied_question(state: &CompletionState<'_>) -> Question {
    let instructions = match state {
        CompletionState::Diff { .. } => {
            "Does the diff satisfy what the request asked for? Answer near 1.0 when \
             nothing the request asked for is missing and nothing unasked was changed; \
             answer near 0.0 when the diff clearly does not satisfy the request."
        }
        CompletionState::Answer { .. } => {
            "Does the answer satisfy what the request asked for? Answer near 1.0 when the \
             answer fully addresses the request; answer near 0.0 when it clearly does not."
        }
    };
    Question::Noul {
        instructions: instructions.to_string(),
    }
}

/// The five diff-hygiene questions (2641), asked in the same request as
/// [`satisfied_question`] whenever [`CompletionState::Diff`] is in force --
/// never for [`CompletionState::Answer`], which has no diff to ask about.
fn hygiene_questions() -> [(&'static str, Question); 5] {
    let noul = |text: &str| Question::Noul {
        instructions: text.to_string(),
    };
    [
        (
            HYGIENE_HAS_TESTS_KEY,
            noul(
                "Does the diff add or change tests for the behaviour it changes? Answer \
                 near 1.0 when it does; answer near 0.0 when it does not.",
            ),
        ),
        (
            HYGIENE_OUT_OF_SCOPE_KEY,
            noul(
                "Does the diff change files the request did not ask about? Answer near \
                 1.0 when it does; answer near 0.0 when it does not.",
            ),
        ),
        (
            HYGIENE_DEBUG_LEFTOVERS_KEY,
            noul(
                "Does the diff leave debugging artefacts: prints, commented-out code, or \
                 TODO markers? Answer near 1.0 when it does; answer near 0.0 when it does \
                 not.",
            ),
        ),
        (
            HYGIENE_DELETES_TESTS_KEY,
            noul(
                "Does the diff delete or disable tests? Answer near 1.0 when it does; \
                 answer near 0.0 when it does not.",
            ),
        ),
        (
            HYGIENE_CHANGES_SIGNATURE_KEY,
            noul(
                "Does the diff change a public function or type signature? Answer near \
                 1.0 when it does; answer near 0.0 when it does not.",
            ),
        ),
    ]
}

fn judge_key(index: usize) -> String {
    format!("judge_{index}")
}

/// One judge item's question (2642): the item's own text and whatever
/// evidence the acceptance list already gathered for it -- embedded in the
/// instructions, since [`decide`]'s `state` is shared across every question
/// in the request.
fn judge_question(item: &str, evidence: &str) -> Question {
    let instructions = if evidence.is_empty() {
        format!(
            "Does this acceptance item hold, given the diff or answer above? Answer near \
             1.0 when it clearly holds; answer near 0.0 when it clearly does not. \
             Item: {item}"
        )
    } else {
        format!(
            "Does this acceptance item hold, given the diff or answer above? Answer near \
             1.0 when it clearly holds; answer near 0.0 when it clearly does not. \
             Item: {item}\nEvidence already gathered: {evidence}"
        )
    };
    Question::Noul { instructions }
}

/// The five diff-hygiene questions' answers (2641), `None` for
/// [`CompletionState::Answer`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HygieneAnswer {
    pub has_tests: f64,
    pub out_of_scope: f64,
    pub debug_leftovers: f64,
    pub deletes_tests: f64,
    pub changes_signature: f64,
}

/// What the completion question answered (2616, extended 2641/2642): a
/// probability that the diff or answer satisfies the request, the five
/// diff-hygiene answers when a diff was asked about, and one noul per
/// acceptance judge item asked in the same request. `session/task.rs::gate`
/// decides what to do with the numbers -- this module only asks and parses.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionAnswer {
    pub noul: f64,
    pub latency_ms: u64,
    pub truncated: bool,
    pub hygiene: Option<HygieneAnswer>,
    /// One noul per judge item, in the same order they were given to
    /// [`completion_satisfied`].
    pub judge: Vec<f64>,
}

/// Cuts `diff` to at most [`DIFF_STATE_BYTES`], at the last hunk header
/// (`"\n@@ "`) at or before the bound, so a kept hunk is never split
/// mid-way. Returns the (possibly unchanged) text and whether it was cut.
fn bound_diff(diff: &str) -> (String, bool) {
    if diff.len() <= DIFF_STATE_BYTES {
        return (diff.to_string(), false);
    }
    let mut cut = DIFF_STATE_BYTES;
    while cut > 0 && !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &diff[..cut];
    match head.rfind("\n@@ ") {
        Some(newline) => (diff[..=newline].to_string(), true),
        None => (head.to_string(), true),
    }
}

fn extract_noul(answers: &mut BTreeMap<String, Answer>, key: &str) -> Result<f64, DecideError> {
    match answers.remove(key) {
        Some(Answer::Noul(value)) => Ok(value),
        Some(Answer::Choice { .. }) => Err(DecideError::Parse(format!(
            "the `{key}` question was answered as a choice, not a noul"
        ))),
        None => Err(DecideError::Parse(format!("no answer for `{key}`"))),
    }
}

/// Asks whether `state` satisfies `request` -- the diff, or the model's own
/// answer when there is no diff to show or the task's intent was read-only
/// -- together with the diff-hygiene questions (2641, only for
/// [`CompletionState::Diff`]) and one question per `judge_items` (2642),
/// all in the one request 2616 already bounds to [`DECISION_TIMEOUT`].
/// Never surfaced as a task failure -- the caller (`session/task.rs::gate`)
/// records the error and proceeds exactly as it would with no decision
/// model.
pub fn completion_satisfied(
    model: &str,
    request: &str,
    state: CompletionState<'_>,
    judge_items: &[(String, String)],
) -> Result<CompletionAnswer, DecideError> {
    let (shared_state, truncated, ask_hygiene) = match &state {
        CompletionState::Diff { diff, findings } => {
            let (bounded, truncated) = bound_diff(diff);
            let mut value = serde_json::json!({
                "request": request,
                "diff": bounded,
                "findings": findings,
            });
            if truncated {
                value["diff_truncated"] = Value::Bool(true);
            }
            (value, truncated, true)
        }
        CompletionState::Answer { answer } => (
            serde_json::json!({ "request": request, "answer": answer }),
            false,
            false,
        ),
    };

    let mut questions: Vec<(String, Question)> =
        vec![(SATISFIED_KEY.to_string(), satisfied_question(&state))];
    if ask_hygiene {
        questions.extend(
            hygiene_questions()
                .into_iter()
                .map(|(key, question)| (key.to_string(), question)),
        );
    }
    for (index, (item, evidence)) in judge_items.iter().enumerate() {
        questions.push((judge_key(index), judge_question(item, evidence)));
    }

    let answers = decide(model, shared_state, &questions)?;
    let latency_ms = answers
        .decisions
        .first()
        .map_or(0, |decision| decision.latency_ms);
    let mut by_key: BTreeMap<String, Answer> = answers
        .decisions
        .into_iter()
        .map(|decision| (decision.key, decision.answer))
        .collect();

    let noul = extract_noul(&mut by_key, SATISFIED_KEY)?;
    let hygiene = if ask_hygiene {
        Some(HygieneAnswer {
            has_tests: extract_noul(&mut by_key, HYGIENE_HAS_TESTS_KEY)?,
            out_of_scope: extract_noul(&mut by_key, HYGIENE_OUT_OF_SCOPE_KEY)?,
            debug_leftovers: extract_noul(&mut by_key, HYGIENE_DEBUG_LEFTOVERS_KEY)?,
            deletes_tests: extract_noul(&mut by_key, HYGIENE_DELETES_TESTS_KEY)?,
            changes_signature: extract_noul(&mut by_key, HYGIENE_CHANGES_SIGNATURE_KEY)?,
        })
    } else {
        None
    };
    let mut judge = Vec::with_capacity(judge_items.len());
    for index in 0..judge_items.len() {
        judge.push(extract_noul(&mut by_key, &judge_key(index))?);
    }

    Ok(CompletionAnswer {
        noul,
        latency_ms,
        truncated,
        hygiene,
        judge,
    })
}

/// Every tool `registry::ALL` declares [`crate::tools::registry::Purity::Effectful`],
/// plus the non-tool doors whose own effect is not a registry fact:
/// `checks` runs a verification command, `agent` spawns a subagent, and
/// `mcp` calls an external tool. A literal, not derived: `tests::
/// effectful_names_matches_the_registry_plus_the_non_tool_doors` pins it
/// against the live registry, so a new `Purity::Effectful` tool fails that
/// test until this list says so on purpose.
pub const EFFECTFUL_NAMES: &[&str] = &["bash", "write", "edit", "checks", "agent", "mcp"];

/// The first of a cell's free names that names an effectful capability, with
/// the byte offset [`crate::runtime::cell::compile`] recorded for it.
pub fn names_effect(compiled_free_names: &[(String, u32)]) -> Option<(String, u32)> {
    compiled_free_names
        .iter()
        .find(|(name, _)| EFFECTFUL_NAMES.contains(&name.as_str()))
        .cloned()
}

/// The provider spelling of the first lowered direct call whose capability
/// id is effectful, or `None`.
pub fn direct_frame_names_effect(calls: &[crate::abi::LoweredCall]) -> Option<String> {
    calls
        .iter()
        .find(|call| EFFECTFUL_NAMES.contains(&call.capability))
        .map(|call| call.provider_name.clone())
}

/// What this turn's effectful cell or frame does, given the task's decision
/// state.
#[derive(Debug, Clone, PartialEq)]
pub enum Hold {
    /// Nothing about this turn is held.
    Run,
    /// `mode = on`, held for the first time this task: the caller does not
    /// run it and answers with this block instead.
    Held(String),
    /// `mode = on`, already held once this task: the caller runs it and
    /// counts an override.
    Overridden,
    /// `mode = shadow`: the caller runs it as it would have anyway and
    /// counts a would-be hold; this text never reaches the model.
    Shadow(String),
}

/// Decides what happens to one effectful cell or frame. `effect` is the
/// capability name found and, for a cell, the line it was found on -- `None`
/// when nothing effectful was found, in which case nothing is ever held.
pub fn hold_for(
    mode: DecisionMode,
    intent: Option<&Intent>,
    hold_above: f64,
    effect: Option<(&str, Option<u32>)>,
    already_held: bool,
) -> Hold {
    if mode == DecisionMode::Off {
        return Hold::Run;
    }
    let Some(intent) = intent else {
        return Hold::Run;
    };
    if intent.choice != READ_ONLY || intent.confidence < hold_above {
        return Hold::Run;
    }
    let Some((name, line)) = effect else {
        return Hold::Run;
    };
    match mode {
        DecisionMode::Off => Hold::Run,
        DecisionMode::Shadow => Hold::Shadow(held_block(intent.confidence, name, line)),
        DecisionMode::On if already_held => Hold::Overridden,
        DecisionMode::On => Hold::Held(held_block(intent.confidence, name, line)),
    }
}

/// The `/cell` inspector's one line for this task's decision state
/// (`session/ui.rs`'s `"/cell"` arm).
pub fn summary_line(intent: Option<&Intent>, holds: u32, overrides: u32) -> String {
    match intent {
        Some(intent) => format!(
            "decision: {} {:.2} · holds {holds} · overrides {overrides}",
            intent.choice, intent.confidence
        ),
        None => "decision: none".to_string(),
    }
}

fn effect_description(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "write" | "edit" => "changes files",
        "bash" => "runs a command",
        "agent" => "spawns a subagent",
        "mcp" => "calls an external tool",
        "checks" => "runs a verification command",
        _ => "has effects",
    }
}

fn held_block(confidence: f64, name: &str, line: Option<u32>) -> String {
    let what = match line {
        Some(line) => format!(
            "This cell calls `{name}` (line {line}), which {}.",
            effect_description(name)
        ),
        None => format!(
            "This request calls `{name}`, which {}.",
            effect_description(name)
        ),
    };
    format!(
        "## Held (decision)\n\
         The request reads as read-only (intent read_only, confidence {confidence:.2}). {what}\n\
         If the request needs it, run the cell again unchanged and it will run. Otherwise answer \
         without changing anything.\n"
    )
}

/// The drift question's key (2643), mirroring [`SATISFIED_KEY`]'s shape for
/// the other single-question request this package asks mid-task.
const DRIFT_KEY: &str = "drift";

/// The most of a cell's source the drift question's `state.cell` carries
/// (2643). Cut at the last newline at or before the bound (never
/// [`bound_diff`]'s hunk boundary -- a cell is plain source, not a diff), so
/// a kept prefix never splits a line mid-way.
pub const DRIFT_CELL_BYTES: usize = 8 * 1024;

fn bound_cell(cell: &str) -> String {
    if cell.len() <= DRIFT_CELL_BYTES {
        return cell.to_string();
    }
    let mut cut = DRIFT_CELL_BYTES;
    while cut > 0 && !cell.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &cell[..cut];
    match head.rfind('\n') {
        Some(newline) => cell[..=newline].to_string(),
        None => head.to_string(),
    }
}

fn drift_question() -> Question {
    Question::Noul {
        instructions: "The cell does what the plan's current step says and nothing else. \
                        Answer near 1.0 when it does; answer near 0.0 when it clearly does not."
            .to_string(),
    }
}

/// Asks whether `cell` does what `step` (the plan's current `Active` item)
/// says, given `request` -- one question, synchronous, bounded to
/// [`DECISION_TIMEOUT`] exactly as every other call into [`decide`]. Never
/// surfaced as a task failure -- the caller (`session/system.rs::
/// apply_decision_hold`) counts the error and runs the cell exactly as it
/// would with no decision model.
pub fn drift_satisfied(
    model: &str,
    request: &str,
    step: &str,
    cell: &str,
) -> Result<f64, DecideError> {
    let state = serde_json::json!({
        "request": request,
        "step": step,
        "cell": bound_cell(cell),
    });
    let questions = [(DRIFT_KEY.to_string(), drift_question())];
    let answers = decide(model, state, &questions)?;
    let decision = answers
        .decisions
        .into_iter()
        .next()
        .ok_or_else(|| DecideError::Parse(format!("no answer for `{DRIFT_KEY}`")))?;
    match decision.answer {
        Answer::Noul(value) => Ok(value),
        Answer::Choice { .. } => Err(DecideError::Parse(format!(
            "the `{DRIFT_KEY}` question was answered as a choice, not a noul"
        ))),
    }
}

/// What one effectful cell or frame does about the plan's current step,
/// given the task's drift state (2643).
#[derive(Debug, Clone, PartialEq)]
pub enum Drift {
    /// Nothing about this cell is held.
    Run,
    /// `mode = shadow`: the caller runs the cell as it would have anyway and
    /// counts a would-be hold; the noul that would have held it.
    Shadow(f64),
    /// `mode = on`, held for the first time this task: the caller does not
    /// run the cell and answers with this block instead.
    Held(String),
}

/// Decides what happens to one effectful cell or frame, mirroring
/// [`hold_for`]'s once rule: `already_held` (`Held` already returned once
/// this task) always runs the cell, exactly as `Hold::Overridden` does for
/// the intent hold, even if `answer` is a fresh confident no -- the once
/// rule wins over the answer, not the other way round. `answer` is `None`
/// when the request failed, which also leaves the cell running. `step` is
/// the active plan item's own text, carried through only to build the held
/// block.
pub fn drift_for(
    mode: DecisionMode,
    answer: Option<f64>,
    drift_no_below: f64,
    step: &str,
    already_held: bool,
) -> Drift {
    if mode == DecisionMode::Off || already_held {
        return Drift::Run;
    }
    let Some(noul) = answer else {
        return Drift::Run;
    };
    match mode {
        DecisionMode::Off => Drift::Run,
        DecisionMode::Shadow => Drift::Shadow(noul),
        DecisionMode::On if noul <= drift_no_below => Drift::Held(drift_block(noul, step)),
        DecisionMode::On => Drift::Run,
    }
}

fn drift_block(confidence: f64, step: &str) -> String {
    format!(
        "decision: this cell may not do what the plan's current step says ({confidence:.2}) — \
         step: {step}. Held once; run it again if it does, or update the plan first.\n"
    )
}

// --- The model half of the `auto` permission rung -------------------------

/// The key the command-permission question is asked under.
const PERMISSION_KEY: &str = "permission";

/// The criterion that means *this line only looks at things*.
pub const COMMAND_READS_ONLY: &str = "reads_only";
/// The criterion that means *this is the work a developer runs constantly*.
pub const COMMAND_ORDINARY_WORK: &str = "ordinary_development_work";
/// The criterion that means *a person should see this before it runs*.
pub const COMMAND_NEEDS_A_PERSON: &str = "needs_a_person";
/// The criterion that means *this destroys something or lowers a defence*.
pub const COMMAND_DESTRUCTIVE: &str = "destructive";

/// The most of a command line the permission question carries. A line longer
/// than this is a script, and the head of a script is enough to tell that it
/// is one.
pub const COMMAND_LINE_BYTES: usize = 4 * 1024;

fn permission_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        COMMAND_READS_ONLY.to_string(),
        "it only inspects: it prints, lists, searches, or reports, and changes no file, \
         no process and nothing outside this machine"
            .to_string(),
    );
    criteria.insert(
        COMMAND_ORDINARY_WORK.to_string(),
        "it is the ordinary work of building this project: it compiles, tests, formats, \
         lints or generates inside the project's own tree and its build directory, and \
         reaches nothing else"
            .to_string(),
    );
    criteria.insert(
        COMMAND_NEEDS_A_PERSON.to_string(),
        "it changes something outside the project's own tree, installs, publishes or \
         downloads something, touches credentials, or is a line you cannot place with \
         confidence"
            .to_string(),
    );
    criteria.insert(
        COMMAND_DESTRUCTIVE.to_string(),
        "it deletes or overwrites something that cannot be reconstructed, or it turns off \
         a protection"
            .to_string(),
    );
    Question::Choice {
        instructions: "A coding agent is about to run this shell command line in a project it \
             is working in. What kind of line is it?"
            .to_string(),
        criteria,
    }
}

// --- the shape of a returned field ---------------------------------------

const FIELD_SHAPE_KEY: &str = "field_shape";
/// Build, test or command output, where only the failures matter and a
/// filter written by the reducer keeps them.
pub const FIELD_LOG: &str = "log";
/// A listing or a table, where every row is one fact and paging keeps them.
pub const FIELD_LISTING: &str = "listing";
/// Source code or an excerpt of a file, read by its line numbers.
pub const FIELD_SOURCE: &str = "source";
/// Prose: documentation, a message, an explanation.
pub const FIELD_PROSE: &str = "prose";
/// Structured records: JSON, rows of values.
pub const FIELD_DATA: &str = "data";

/// The most lines of a field's head and tail the question's state carries.
const FIELD_HEAD_LINES: usize = 8;
const FIELD_TAIL_LINES: usize = 4;
/// The most line shapes the state carries, most frequent first.
const FIELD_SHAPES: usize = 8;
const FIELD_LINE_BYTES: usize = 200;

/// The question: what kind of text is this returned field? One `Choice`
/// over the five shapes, with the field's own head, tail and line-shape
/// histogram as the evidence.
#[must_use]
pub fn field_shape_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        FIELD_LOG.to_string(),
        "build, test or command output: many lines that look alike, and the ones that \
         matter report a failure, an error or a result"
            .to_string(),
    );
    criteria.insert(
        FIELD_LISTING.to_string(),
        "a listing or a table: file names, paths, rows of a report, where every line is one \
         fact of its own"
            .to_string(),
    );
    criteria.insert(
        FIELD_SOURCE.to_string(),
        "source code, or an excerpt of a file with line numbers".to_string(),
    );
    criteria.insert(
        FIELD_PROSE.to_string(),
        "prose: documentation, a message, an explanation, a conversation".to_string(),
    );
    criteria.insert(
        FIELD_DATA.to_string(),
        "structured records: JSON, key-value pairs, rows of values".to_string(),
    );
    Question::Choice {
        instructions: "A coding agent's program returned this field to read next. What kind of \
             text is it?"
            .to_string(),
        criteria,
    }
}

/// What the decision model answered about one returned field.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldShape {
    pub choice: String,
    pub confidence: f64,
    pub latency_ms: u64,
}

/// Asks what kind of text the returned field `name` holds -- once per large
/// field, after the cell, before the value is rendered for the model.
///
/// One `Choice` question, synchronous, bounded by [`DECISION_TIMEOUT`] like
/// every other call here. The field travels as state: its name, size, first
/// and last lines, and the line-shape histogram `reduce_sample` computes,
/// which is what tells a log from a listing without reading the whole of
/// either. The user's reading (2026-09-23): *sometimes reduction is
/// enrichment, by not flooding context with what nobody needs* -- and this
/// is the question that decides which.
pub fn field_shape(model: &str, name: &str, text: &str) -> Result<FieldShape, DecideError> {
    let lines: Vec<&str> = text.lines().collect();
    let first: Vec<String> = lines
        .iter()
        .take(FIELD_HEAD_LINES)
        .map(|line| head(line, FIELD_LINE_BYTES))
        .collect();
    let last: Vec<String> = if lines.len() > FIELD_HEAD_LINES {
        lines
            .iter()
            .rev()
            .take(FIELD_TAIL_LINES)
            .rev()
            .map(|line| head(line, FIELD_LINE_BYTES))
            .collect()
    } else {
        Vec::new()
    };
    let shapes = crate::runtime::reduce_sample::shapes_of(text);
    let histogram: Vec<Value> = shapes
        .shapes
        .iter()
        .take(FIELD_SHAPES)
        .map(|shape| {
            serde_json::json!({ "shape": head(&shape.shape, FIELD_LINE_BYTES), "lines": shape.count })
        })
        .collect();
    let state = serde_json::json!({
        "field": name,
        "lines": lines.len(),
        "tokens": crate::runtime::preview::estimate_tokens(text),
        "head": first,
        "tail": last,
        "line_shapes": histogram,
    });
    let questions = [(FIELD_SHAPE_KEY.to_string(), field_shape_question())];
    let answers = decide(model, state, &questions)?;
    let decision = answers
        .decisions
        .into_iter()
        .next()
        .ok_or_else(|| DecideError::Parse(format!("no answer for `{FIELD_SHAPE_KEY}`")))?;
    match decision.answer {
        Answer::Choice {
            choice, confidence, ..
        } => Ok(FieldShape {
            choice,
            confidence,
            latency_ms: decision.latency_ms,
        }),
        Answer::Noul(_) => Err(DecideError::Parse(format!(
            "the `{FIELD_SHAPE_KEY}` question was answered as a noul, not a choice"
        ))),
    }
}

// --- enough to go on? --------------------------------------------------------

const ENOUGH_KEY: &str = "enough";
/// The most fields, and the most head lines per field, the question's state
/// carries.
const ENOUGH_FIELDS: usize = 8;
const ENOUGH_HEAD_LINES: usize = 4;

/// The question: does this return give the agent enough to take its next
/// step, or will it first have to read the files the return names?
#[must_use]
pub fn enough_question() -> Question {
    Question::Noul {
        instructions: "A coding agent's program returned this to read next, while working on \
             the request and the plan step shown. The return names the files listed as \
             candidates. Does what it returned give the agent enough to take its next step \
             without first reading those files? Answer near 1.0 when it has enough; answer \
             near 0.0 when its next step will be to read what the return names."
            .to_string(),
    }
}

/// One returned field, as the enough question sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldGlance {
    pub name: String,
    pub tokens: usize,
    pub head: String,
}

/// What the decision model answered about a return: its probability that
/// the agent has enough, and the request's latency.
#[derive(Debug, Clone, PartialEq)]
pub struct Enough {
    pub noul: f64,
    pub latency_ms: u64,
}

/// Asks whether a cell's return is enough to go on -- once per return that
/// names files the agent has not read, after the cell, before the value is
/// rendered (`session/returned.rs`).
///
/// One `Noul` question, synchronous, bounded by [`DECISION_TIMEOUT`] like
/// every other call here. The state is the request, the active plan step,
/// a glance at each field (name, size, first lines) and the candidate paths
/// the return names; the model reads none of the files.
pub fn enough(
    model: &str,
    request: &str,
    step: Option<&str>,
    fields: &[FieldGlance],
    candidates: &[String],
) -> Result<Enough, DecideError> {
    let glance: Vec<Value> = fields
        .iter()
        .take(ENOUGH_FIELDS)
        .map(|field| {
            let head: Vec<String> = field
                .head
                .lines()
                .take(ENOUGH_HEAD_LINES)
                .map(|line| self::head(line, FIELD_LINE_BYTES))
                .collect();
            serde_json::json!({ "field": field.name, "tokens": field.tokens, "head": head })
        })
        .collect();
    let state = serde_json::json!({
        "request": head(request, COMMAND_LINE_BYTES),
        "step": step,
        "return": glance,
        "candidates": candidates,
    });
    let questions = [(ENOUGH_KEY.to_string(), enough_question())];
    let answers = decide(model, state, &questions)?;
    let decision = answers
        .decisions
        .into_iter()
        .next()
        .ok_or_else(|| DecideError::Parse(format!("no answer for `{ENOUGH_KEY}`")))?;
    match decision.answer {
        Answer::Noul(noul) => Ok(Enough {
            noul,
            latency_ms: decision.latency_ms,
        }),
        Answer::Choice { .. } => Err(DecideError::Parse(format!(
            "the `{ENOUGH_KEY}` question was answered as a choice, not a noul"
        ))),
    }
}

/// What the decision model answered about one command line.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandJudgement {
    pub choice: String,
    pub confidence: f64,
    pub latency_ms: u64,
}

/// Asks what kind of command line `line` is -- the model half of the `auto`
/// rung, asked only about the lines the static reader could not place.
///
/// One `Choice` question, synchronous, bounded by [`DECISION_TIMEOUT`] like
/// every other call here. The line travels as state rather than folded into
/// the instructions: the question is fixed and the evidence is the line.
pub fn permission(model: &str, line: &str) -> Result<CommandJudgement, DecideError> {
    let state = serde_json::json!({ "command_line": head(line, COMMAND_LINE_BYTES) });
    let questions = [(PERMISSION_KEY.to_string(), permission_question())];
    let answers = decide(model, state, &questions)?;
    let decision = answers
        .decisions
        .into_iter()
        .next()
        .ok_or_else(|| DecideError::Parse(format!("no answer for `{PERMISSION_KEY}`")))?;
    match decision.answer {
        Answer::Choice {
            choice, confidence, ..
        } => Ok(CommandJudgement {
            choice,
            confidence,
            latency_ms: decision.latency_ms,
        }),
        Answer::Noul(_) => Err(DecideError::Parse(format!(
            "the `{PERMISSION_KEY}` question was answered as a noul, not a choice"
        ))),
    }
}

/// Whether one [`CommandJudgement`] is a reason to let the line run without
/// asking.
///
/// **The model half can vouch, and can never condemn.** Only
/// [`COMMAND_READS_ONLY`] and [`COMMAND_ORDINARY_WORK`], at or above
/// `command_runs_above`, turn a question into a run. A `needs_a_person` or
/// `destructive` answer leaves the question exactly where it was -- in front
/// of the person, now with the model's word for why -- and never becomes a
/// refusal. That is not timidity: the ladder's one structural property is
/// that a rung may only ever *remove* a question, so a model answer that
/// could deny would make `auto` stricter than the rung below it, where the
/// person is asked and may say yes.
///
/// `shadow` records and changes nothing, as everywhere else: letting a line
/// run without asking *is* changing what runs.
#[must_use]
pub fn permission_for(
    mode: DecisionMode,
    answer: Option<&CommandJudgement>,
    command_runs_above: f64,
) -> bool {
    if mode != DecisionMode::On {
        return false;
    }
    let Some(answer) = answer else {
        return false;
    };
    matches!(
        answer.choice.as_str(),
        COMMAND_READS_ONLY | COMMAND_ORDINARY_WORK
    ) && answer.confidence >= command_runs_above
}

/// The most criteria one cell-authored question may name, and the most bytes
/// its instructions and each criterion may carry.
///
/// The question travels to the decision model in a request bounded by the
/// same two-second decision timeout every gate uses, so these keep one cell
/// from turning a short errand into a slow one. Two criteria is the smallest question worth asking -- a choice
/// with one option is not a choice.
pub const JUDGEMENT_CRITERIA_MIN: usize = 2;
pub const JUDGEMENT_CRITERIA_MAX: usize = 8;
pub const JUDGEMENT_TEXT_BYTES: usize = 4 * 1024;

/// The key a cell's own question is asked under, kept distinct from every
/// harness gate's key so a rollout reader can tell the two apart.
const JUDGEMENT_KEY: &str = "judgement";

/// One judgement the running program asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Judgement {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
    pub latency_ms: u64,
}

/// Asks **the model's own** `Choice` question about state the cell already
/// holds -- the one entry point `decide.choice(...)` reaches from inside a
/// running program.
///
/// It is [`decide`] with one question and the cell's text as the state, and
/// it is deliberately nothing more: the same `/v1/systemone` route, the same
/// two-second timeout, the same rule that no error carries more than a
/// bounded head of a body. What it adds is the bound on a
/// question a *program* composed, which the harness's own gates do not need
/// because their questions are written here.
///
/// The subject is sent as `state.subject` rather than folded into the
/// instructions, so the instructions stay the question and the evidence stays
/// evidence.
pub fn judgement(
    model: &str,
    instructions: &str,
    subject: &str,
    criteria: BTreeMap<String, String>,
) -> Result<Judgement, DecideError> {
    let instructions = instructions.trim();
    if instructions.is_empty() {
        return Err(DecideError::Parse(
            "a judgement needs instructions saying what to decide".to_string(),
        ));
    }
    if criteria.len() < JUDGEMENT_CRITERIA_MIN || criteria.len() > JUDGEMENT_CRITERIA_MAX {
        return Err(DecideError::Parse(format!(
            "a judgement names between {JUDGEMENT_CRITERIA_MIN} and {JUDGEMENT_CRITERIA_MAX} criteria; this one named {}",
            criteria.len()
        )));
    }
    if let Some((name, _)) = criteria
        .iter()
        .find(|(name, text)| name.trim().is_empty() || text.trim().is_empty())
    {
        return Err(DecideError::Parse(format!(
            "criterion `{name}` needs a name and a sentence saying when it applies"
        )));
    }
    let state = serde_json::json!({
        "subject": head(subject, JUDGEMENT_TEXT_BYTES),
    });
    let questions = [(
        JUDGEMENT_KEY.to_string(),
        Question::Choice {
            instructions: head(instructions, JUDGEMENT_TEXT_BYTES),
            criteria: criteria
                .into_iter()
                .map(|(name, text)| (name, head(&text, JUDGEMENT_TEXT_BYTES)))
                .collect(),
        },
    )];
    let answers = decide(model, state, &questions)?;
    let decision = answers
        .decisions
        .into_iter()
        .next()
        .ok_or_else(|| DecideError::Parse(format!("no answer for `{JUDGEMENT_KEY}`")))?;
    match decision.answer {
        Answer::Choice {
            choice,
            confidence,
            probabilities,
        } => Ok(Judgement {
            choice,
            confidence,
            probabilities,
            latency_ms: decision.latency_ms,
        }),
        Answer::Noul(_) => Err(DecideError::Parse(format!(
            "the `{JUDGEMENT_KEY}` question was answered as a noul, not a choice"
        ))),
    }
}

/// The key the question a program put to the person is answered under.
const ASKED_KEY: &str = "asked";

/// Reads the person's question against what the session already knows: the
/// request they made, the diff so far, and the findings.
///
/// **It is the same `Choice` shape `decide.choice` uses, with the session's
/// own evidence as the state.** The point is that the model answering has
/// seen what the person would have to scroll back through -- the original
/// request, what has actually changed, and what the checkers found -- so its
/// reading of a question is grounded in the work and not in the question's
/// wording alone. The choices are the criteria, named by their own text, so
/// the probabilities come back keyed by the choice the program offered.
///
/// The diff is bounded exactly as the completion gate bounds it
/// ([`DIFF_STATE_BYTES`], cut at a hunk boundary), so one question cannot
/// cost more than the gate that already runs every task.
pub fn asked(
    model: &str,
    request: &str,
    diff: &str,
    findings: &[String],
    question: &str,
    choices: &[String],
) -> Result<Judgement, DecideError> {
    if choices.len() < JUDGEMENT_CRITERIA_MIN {
        return Err(DecideError::Parse(
            "a question needs at least two choices to weigh".to_string(),
        ));
    }
    let (bounded, truncated) = bound_diff(diff);
    let mut state = serde_json::json!({
        "request": request,
        "diff": bounded,
        "findings": findings,
        "question": head(question, JUDGEMENT_TEXT_BYTES),
    });
    if truncated {
        state["diff_truncated"] = Value::Bool(true);
    }
    let criteria: BTreeMap<String, String> = choices
        .iter()
        .map(|choice| {
            let choice = head(choice, JUDGEMENT_TEXT_BYTES);
            (choice.clone(), format!("the answer is: {choice}"))
        })
        .collect();
    let questions = [(
        ASKED_KEY.to_string(),
        Question::Choice {
            instructions: format!(
                "Given the request, the diff so far and the findings, which answer to this \
                 question best serves what was asked for? Question: {}",
                head(question, JUDGEMENT_TEXT_BYTES)
            ),
            criteria,
        },
    )];
    let answers = decide(model, state, &questions)?;
    let decision = answers
        .decisions
        .into_iter()
        .next()
        .ok_or_else(|| DecideError::Parse(format!("no answer for `{ASKED_KEY}`")))?;
    match decision.answer {
        Answer::Choice {
            choice,
            confidence,
            probabilities,
        } => Ok(Judgement {
            choice,
            confidence,
            probabilities,
            latency_ms: decision.latency_ms,
        }),
        Answer::Noul(_) => Err(DecideError::Parse(format!(
            "the `{ASKED_KEY}` question was answered as a noul, not a choice"
        ))),
    }
}

/// `text`'s first `limit` bytes, cut on a character boundary.
fn head(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut cut = limit;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

#[cfg(test)]
mod judgement_tests {
    use super::*;

    fn criteria(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, text)| ((*name).to_string(), (*text).to_string()))
            .collect()
    }

    /// The bounds are checked before any request is built, so a malformed
    /// question costs nothing and says what was wrong with it.
    #[test]
    fn a_question_with_one_criterion_is_refused_before_it_is_sent() {
        let error = judgement(
            "jev-latest",
            "Is this a rename?",
            "diff",
            criteria(&[("rename_only", "every hunk renames one symbol")]),
        )
        .expect_err("one criterion is not a choice");
        let said = error.to_string();
        assert!(said.contains("between 2 and 8"), "{said}");
    }

    #[test]
    fn a_question_with_no_instructions_is_refused_before_it_is_sent() {
        let error = judgement(
            "jev-latest",
            "   ",
            "diff",
            criteria(&[("a", "one"), ("b", "two")]),
        )
        .expect_err("a question needs a question");
        assert!(error.to_string().contains("instructions"));
    }

    #[test]
    fn a_criterion_without_a_sentence_is_refused_and_named() {
        let error = judgement(
            "jev-latest",
            "Is this a rename?",
            "diff",
            criteria(&[
                ("rename_only", "every hunk renames one symbol"),
                ("wider", ""),
            ]),
        )
        .expect_err("a criterion the model cannot apply");
        assert!(error.to_string().contains("wider"), "{error}");
    }

    #[test]
    fn a_subject_longer_than_the_bound_is_cut_on_a_character_boundary() {
        let text = "é".repeat(JUDGEMENT_TEXT_BYTES);
        let cut = head(&text, JUDGEMENT_TEXT_BYTES);
        assert!(cut.len() <= JUDGEMENT_TEXT_BYTES);
        assert!(text.starts_with(&cut), "a prefix of what was given");
        assert_eq!(head("short", JUDGEMENT_TEXT_BYTES), "short");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry;

    #[test]
    fn effectful_names_matches_the_registry_plus_the_non_tool_doors() {
        let mut expected: Vec<&str> = registry::ALL
            .iter()
            .filter(|tool| tool.purity() == registry::Purity::Effectful)
            .map(|tool| tool.name())
            .collect();
        expected.extend(["checks", "agent", "mcp"]);
        expected.sort_unstable();
        let mut actual: Vec<&str> = EFFECTFUL_NAMES.to_vec();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn names_effect_finds_write_and_nothing_for_read() {
        let free = vec![("write".to_string(), 12u32), ("cwd".to_string(), 0)];
        assert_eq!(names_effect(&free), Some(("write".to_string(), 12)));
        let free = vec![("read".to_string(), 3u32)];
        assert_eq!(names_effect(&free), None);
    }

    #[test]
    fn request_body_matches_the_documented_shape() {
        let mut criteria = BTreeMap::new();
        criteria.insert("read_only".to_string(), "reads only".to_string());
        let questions: BTreeMap<String, Question> = [(
            "intent".to_string(),
            Question::Choice {
                instructions: "What does this request intend?".to_string(),
                criteria,
            },
        )]
        .into_iter()
        .collect();
        let body = RequestBody {
            state: &serde_json::json!({"request": "read the file"}),
            model: "jev-latest",
            questions,
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["model"], "jev-latest");
        assert_eq!(value["state"]["request"], "read the file");
        assert_eq!(value["questions"]["intent"]["type"], "choice");
        assert_eq!(
            value["questions"]["intent"]["instructions"],
            "What does this request intend?"
        );
        assert_eq!(
            value["questions"]["intent"]["criteria"]["read_only"],
            "reads only"
        );
    }

    #[test]
    fn the_task_questions_request_carries_intent_and_complexity_in_one_map() {
        let questions: BTreeMap<String, Question> = [
            (INTENT_KEY.to_string(), intent_question()),
            (COMPLEXITY_KEY.to_string(), complexity_question()),
        ]
        .into_iter()
        .collect();
        let body = RequestBody {
            state: &serde_json::json!({"request": "read the file"}),
            model: "jev-latest",
            questions,
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(
            value["questions"].as_object().unwrap().len(),
            2,
            "one request, both questions: {value}"
        );
        assert_eq!(value["questions"]["intent"]["type"], "choice");
        assert_eq!(value["questions"]["complexity"]["type"], "choice");
        assert!(
            value["questions"]["complexity"]["criteria"][NEEDS_EXPLORATION]
                .as_str()
                .is_some()
        );
    }

    #[test]
    fn task_decision_of_pairs_both_answers_by_key() {
        let answers = Answers {
            model: "jev-latest".to_string(),
            decisions: vec![
                Decision {
                    key: INTENT_KEY.to_string(),
                    answer: Answer::Choice {
                        choice: "read_only".to_string(),
                        probabilities: BTreeMap::new(),
                        confidence: 0.94,
                    },
                    latency_ms: 640,
                },
                Decision {
                    key: COMPLEXITY_KEY.to_string(),
                    answer: Answer::Choice {
                        choice: "routine".to_string(),
                        probabilities: BTreeMap::new(),
                        confidence: 0.81,
                    },
                    latency_ms: 640,
                },
            ],
        };
        let decision = task_decision_of(answers).unwrap();
        assert_eq!(decision.intent.choice, "read_only");
        assert_eq!(decision.intent.confidence, 0.94);
        assert_eq!(decision.complexity.choice, "routine");
        assert_eq!(decision.complexity.confidence, 0.81);
    }

    #[test]
    fn task_decision_of_missing_complexity_is_a_parse_error() {
        let answers = Answers {
            model: "jev-latest".to_string(),
            decisions: vec![Decision {
                key: INTENT_KEY.to_string(),
                answer: Answer::Choice {
                    choice: "read_only".to_string(),
                    probabilities: BTreeMap::new(),
                    confidence: 0.94,
                },
                latency_ms: 640,
            }],
        };
        let error = task_decision_of(answers).unwrap_err();
        assert!(error.to_string().contains("complexity"), "{error}");
    }

    #[test]
    fn the_documented_choice_response_parses() {
        let text = serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "intent": {
                    "type": "choice",
                    "choice": "read_only",
                    "probabilities": {"read_only": 0.94, "modify": 0.04, "run": 0.01, "other": 0.01},
                    "confidence": 0.94,
                }
            },
            "usage": {"input_tokens": 40, "output_tokens": 12},
        })
        .to_string();
        let parsed: ResponseBody = serde_json::from_str(&text).unwrap();
        let answer = Answer::from(parsed.answers.get("intent").cloned().unwrap());
        match answer {
            Answer::Choice {
                choice, confidence, ..
            } => {
                assert_eq!(choice, "read_only");
                assert_eq!(confidence, 0.94);
            }
            Answer::Noul(_) => panic!("expected a choice answer"),
        }
    }

    #[test]
    fn a_response_missing_confidence_is_a_parse_error() {
        let text = serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "intent": {
                    "type": "choice",
                    "choice": "read_only",
                    "probabilities": {"read_only": 0.94},
                }
            },
            "usage": {"input_tokens": 40, "output_tokens": 12},
        })
        .to_string();
        assert!(serde_json::from_str::<ResponseBody>(&text).is_err());
    }

    #[test]
    fn hold_for_applies_the_threshold_and_the_once_rule() {
        let intent = Intent {
            choice: "read_only".to_string(),
            confidence: 0.94,
            latency_ms: 180,
        };
        let effect = Some(("write", Some(3)));
        assert!(matches!(
            hold_for(DecisionMode::On, Some(&intent), 0.85, effect, false),
            Hold::Held(_)
        ));
        assert!(matches!(
            hold_for(DecisionMode::On, Some(&intent), 0.85, effect, true),
            Hold::Overridden
        ));
        assert!(matches!(
            hold_for(DecisionMode::Shadow, Some(&intent), 0.85, effect, false),
            Hold::Shadow(_)
        ));
        let low_confidence = Intent {
            confidence: 0.80,
            ..intent.clone()
        };
        assert_eq!(
            hold_for(DecisionMode::On, Some(&low_confidence), 0.85, effect, false),
            Hold::Run
        );
        let modify = Intent {
            choice: "modify".to_string(),
            ..intent.clone()
        };
        assert_eq!(
            hold_for(DecisionMode::On, Some(&modify), 0.85, effect, false),
            Hold::Run
        );
        assert_eq!(
            hold_for(DecisionMode::On, Some(&intent), 0.85, None, false),
            Hold::Run
        );
    }

    #[test]
    fn drift_for_applies_the_threshold_and_the_once_rule() {
        assert!(matches!(
            drift_for(
                DecisionMode::On,
                Some(0.06),
                0.10,
                "write the README",
                false
            ),
            Drift::Held(_)
        ));
        assert_eq!(
            drift_for(DecisionMode::On, Some(0.06), 0.10, "write the README", true),
            Drift::Run,
            "the once rule wins even if a caller mistakenly asks again"
        );
        assert_eq!(
            drift_for(
                DecisionMode::On,
                Some(0.50),
                0.10,
                "write the README",
                false
            ),
            Drift::Run,
            "an in-between answer is not confident enough to hold"
        );
        assert_eq!(
            drift_for(
                DecisionMode::Shadow,
                Some(0.06),
                0.10,
                "write the README",
                false
            ),
            Drift::Shadow(0.06)
        );
        assert_eq!(
            drift_for(DecisionMode::On, None, 0.10, "write the README", false),
            Drift::Run,
            "no answer (skipped or failed) leaves the cell running"
        );
        assert_eq!(
            drift_for(
                DecisionMode::Off,
                Some(0.06),
                0.10,
                "write the README",
                false
            ),
            Drift::Run
        );
    }

    #[test]
    fn drift_block_names_the_step_and_the_confidence() {
        let block = drift_block(0.06, "write the README");
        assert!(block.contains("write the README"), "{block}");
        assert!(block.contains("0.06"), "{block}");
        assert!(
            block.contains("this cell may not do what the plan's current step says"),
            "{block}"
        );
    }

    #[test]
    fn the_satisfied_body_serializes_to_the_documented_shape() {
        let (bounded, truncated) = bound_diff("+one line\n");
        assert!(!truncated);
        let state = CompletionState::Diff {
            diff: "+one line\n",
            findings: &[],
        };
        let questions: BTreeMap<String, Question> =
            [(SATISFIED_KEY.to_string(), satisfied_question(&state))]
                .into_iter()
                .collect();
        let body = RequestBody {
            state: &serde_json::json!({"request": "fix the bug", "diff": bounded, "findings": Vec::<String>::new()}),
            model: "jev-latest",
            questions,
        };
        let value = serde_json::to_value(&body).unwrap();
        assert_eq!(value["questions"]["satisfied"]["type"], "noul");
        assert!(
            value["questions"]["satisfied"]["instructions"]
                .as_str()
                .unwrap()
                .contains("satisfy")
        );
        assert_eq!(value["state"]["request"], "fix the bug");
        assert_eq!(value["state"]["diff"], "+one line\n");
    }

    #[test]
    fn the_answer_state_question_asks_about_the_answer_not_a_diff() {
        let state = CompletionState::Answer { answer: "done" };
        let question = satisfied_question(&state);
        let Question::Noul { instructions } = question else {
            panic!("expected a noul question");
        };
        assert!(instructions.contains("answer"), "{instructions}");
        assert!(!instructions.contains("diff"), "{instructions}");
    }

    #[test]
    fn hygiene_questions_cover_the_five_keys() {
        let questions = hygiene_questions();
        let keys: Vec<&str> = questions.iter().map(|(key, _)| *key).collect();
        assert_eq!(
            keys,
            vec![
                "has_tests",
                "out_of_scope",
                "debug_leftovers",
                "deletes_tests",
                "changes_signature",
            ]
        );
    }

    #[test]
    fn a_judge_question_embeds_the_item_and_its_evidence() {
        let Question::Noul { instructions } =
            judge_question("the tone is friendly", "no evidence gathered")
        else {
            panic!("expected a noul question");
        };
        assert!(
            instructions.contains("the tone is friendly"),
            "{instructions}"
        );
        assert!(
            instructions.contains("no evidence gathered"),
            "{instructions}"
        );
        assert_eq!(judge_key(0), "judge_0");
        assert_eq!(judge_key(3), "judge_3");
    }

    fn command_answer(choice: &str, confidence: f64) -> CommandJudgement {
        CommandJudgement {
            choice: choice.to_string(),
            confidence,
            latency_ms: 7,
        }
    }

    /// The two criteria that vouch, and only those two.
    #[test]
    fn only_a_reading_or_ordinary_line_is_vouched_for() {
        for choice in [COMMAND_READS_ONLY, COMMAND_ORDINARY_WORK] {
            assert!(
                permission_for(DecisionMode::On, Some(&command_answer(choice, 0.9)), 0.85),
                "{choice} must let the line run"
            );
        }
        for choice in [COMMAND_NEEDS_A_PERSON, COMMAND_DESTRUCTIVE] {
            assert!(
                !permission_for(DecisionMode::On, Some(&command_answer(choice, 0.99)), 0.85),
                "{choice} must leave the question with the person"
            );
        }
    }

    #[test]
    fn a_vouch_below_the_threshold_is_not_decisive_and_at_it_is() {
        let below = command_answer(COMMAND_READS_ONLY, 0.84);
        assert!(!permission_for(DecisionMode::On, Some(&below), 0.85));
        let at = command_answer(COMMAND_READS_ONLY, 0.85);
        assert!(
            permission_for(DecisionMode::On, Some(&at), 0.85),
            "at the threshold is decisive, as every other `_above` here is"
        );
    }

    /// **This one changes what runs**, so
    /// `shadow` must not act on it — that is the whole difference between
    /// the two modes.
    #[test]
    fn shadow_asks_and_changes_nothing_and_off_does_not_act_either() {
        let answer = command_answer(COMMAND_READS_ONLY, 0.99);
        assert!(
            !permission_for(DecisionMode::Shadow, Some(&answer), 0.85),
            "letting a line run without asking is changing what runs"
        );
        assert!(!permission_for(DecisionMode::Off, Some(&answer), 0.85));
        assert!(!permission_for(DecisionMode::On, None, 0.85));
    }

    /// Every criterion the question offers is one of the four this module
    /// names, so a model answering the question can only ever produce a
    /// choice `permission_for` knows how to read.
    #[test]
    fn the_permission_question_offers_exactly_its_four_criteria() {
        let Question::Choice { criteria, .. } = permission_question() else {
            panic!("the permission question is a choice");
        };
        let mut names: Vec<&str> = criteria.keys().map(String::as_str).collect();
        names.sort_unstable();
        let mut expected = vec![
            COMMAND_READS_ONLY,
            COMMAND_ORDINARY_WORK,
            COMMAND_NEEDS_A_PERSON,
            COMMAND_DESTRUCTIVE,
        ];
        expected.sort_unstable();
        assert_eq!(names, expected);
        assert!(
            criteria.values().all(|text| !text.trim().is_empty()),
            "every criterion says when it applies"
        );
    }

    #[test]
    fn bound_diff_cuts_at_a_hunk_boundary_and_sets_the_flag() {
        let hunk = format!("@@ -1,1 +1,1 @@\n-{}\n+after\n", "x".repeat(2_000));
        let mut diff = String::new();
        while diff.len() <= DIFF_STATE_BYTES {
            diff.push_str(&hunk);
        }
        let (bounded, truncated) = bound_diff(&diff);
        assert!(truncated);
        assert!(bounded.len() <= DIFF_STATE_BYTES);
        assert!(diff.starts_with(&bounded), "a prefix of the original diff");
        assert_eq!(
            bounded.len() % hunk.len(),
            0,
            "kept only whole hunks, none split mid-way: {} of {}",
            bounded.len(),
            hunk.len()
        );

        let (short, truncated) = bound_diff("@@ -1,1 +1,1 @@\n-a\n+b\n");
        assert!(!truncated);
        assert_eq!(short, "@@ -1,1 +1,1 @@\n-a\n+b\n");
    }

    #[test]
    fn a_completion_answer_missing_a_noul_is_a_parse_error() {
        let text = serde_json::json!({
            "model": "jev-latest",
            "answers": {
                "satisfied": {
                    "type": "choice",
                    "choice": "yes",
                    "probabilities": {"yes": 0.9},
                    "confidence": 0.9,
                }
            },
            "usage": {"input_tokens": 10, "output_tokens": 4},
        })
        .to_string();
        let parsed: ResponseBody = serde_json::from_str(&text).unwrap();
        let answer = Answer::from(parsed.answers.get("satisfied").cloned().unwrap());
        assert!(matches!(answer, Answer::Choice { .. }));
    }
}
