//! The system block of one task: the prompt, its facts and manifest, and
//! the scouting preflight block (moved out of `session.rs` for the Phase 59
//! size ratchet, 2026-09-13; nothing here is new).

use super::*;

/// The system block, and it is [`prompt::render_system`]'s bytes and nothing
/// else -- `model-contract.md` §1: the preamble, one declaration per
/// registered tool, then the project's own instructions.
///
/// **The joining of the instruction documents is all this function decides.**
/// Map line 2448 fixes what is loaded, not how it is joined; everything from
/// the preamble outwards is `prompt`'s, whose own golden test pins it byte for
/// byte, so there is no second spelling of the contract here to drift from it.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_system_prompt(
    limits: &crate::config::Limits,
    web: &crate::web::WebConfig,
    agents: &crate::config::AgentsConfig,
    helpers: &crate::config::HelpersConfig,
    decisions: &crate::config::DecisionsConfig,
    served: &[crate::models::RosterModel],
    profile: &Profile,
    interface: crate::abi::Interface,
    manifest: &crate::manifest::Manifest,
) -> String {
    // Configuration/grants remain session-scoped; guidance is read fresh.
    let instructions = if limits.instructions_outline {
        crate::project::instructions::root_outlined(profile)
    } else {
        crate::project::instructions::root(profile)
    };
    // The Runtime block declares `web` only when `[web]` reaches something,
    // the same predicate the runtime binds it on (map 2658).
    let web = prompt::declarations::WebReach::from_config(web);
    // The models a subagent may be sent to: the gateway's figures, resolved
    // once at session start, and `[agents]` as it stands now -- a tier
    // assignment can change the posture mid-session, the served models
    // cannot change at all.
    let agents = prompt::declarations::AgentRoster {
        posture: prompt::declarations::AgentsPosture::from_config(agents),
        models: served.to_vec(),
    };
    let mut system = prompt::render_system_reaching(
        &instructions,
        &registry::ALL.iter().collect::<Vec<_>>(),
        &session_facts_with(profile, interface, manifest),
        crate::runtime::bindings::HostGlobals::Every,
        prompt::Reach {
            web: web.as_ref(),
            agents: Some(&agents),
            // The same predicate `system_manifest` writes its `Unavailable:`
            // line on, so the roster and its refusal cannot disagree.
            helpers: Some(helpers.model.is_some() && helpers.enabled),
            // The same predicate the runtime binds `decide` on, so an
            // unconfigured session is told of no global it does not have.
            decisions: decisions.model.is_some(),
        },
    );
    if profile.os_sandbox_bypassed() {
        system.push_str(
            "\n\nDANGER: Sterna's OS process sandbox is disabled by an explicit CLI bypass. The surrounding container or VM is the only process boundary.",
        );
    }
    system.push_str("\n\n");
    system.push_str(&crate::project::orientation::collect(profile));
    if limits.turn_economy {
        system.push_str(prompt::TURN_ECONOMY);
    }
    if limits.autonomy_block {
        system.push_str(prompt::AUTONOMY_BLOCK);
    }
    if limits.scope_block {
        system.push_str(prompt::SCOPE_BLOCK);
    }
    // Last, so a note written behind one answer changes only the tail of
    // the next task's prompt and the cached prefix before it survives.
    system.push_str(&crate::learned::section(profile));
    system
}
/// Keeps the system prompt byte-stable from task to task, so a task after the
/// first reads the whole earlier conversation from the provider's cache.
///
/// It is replaced only when what it says changed -- the instructions, the
/// grants, the model, the interface -- never for its orientation timestamp or
/// a note `learned.md` gained behind the last answer: those reach the next
/// session. Whatever differs per task rides in the task's own message
/// ([`task_lines`], [`carry_task_context`]).
pub(super) fn keep_session_system(session: &Session<'_>, transcript: &mut Transcript) {
    let mut fresh = system_prompt_for(session);
    if session.config().web.enabled {
        fresh.push_str("\nHost web broker: web.fetch is enabled under the configured domain policy. Shell network access is separate. ");
        fresh.push_str(if session.config().web.search_endpoint.is_some() {
            "web.search is configured. Cite the source URLs returned by web tools.\n"
        } else {
            "web.search has no configured search endpoint and will refuse.\n"
        });
    }
    let current = &transcript.conversation.system;
    if current.is_empty() || lasting_part(current) != lasting_part(&fresh) {
        transcript.conversation.system = fresh;
    }
}

/// A system prompt without the two parts that change on their own between
/// tasks and do not warrant a new cache.
fn lasting_part(system: &str) -> String {
    let body = system
        .split("\n\n## Learned about this project")
        .next()
        .unwrap_or(system);
    body.lines()
        .filter(|line| !line.starts_with("task-start UTC:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// This task's request-mode line and the approved plan, if any: task
/// context, carried in the task's message rather than the system prompt.
pub(super) fn task_lines(session: &Session<'_>) -> String {
    let mut lines =
        prompt::request_mode_line(session.mode.get(), &session.overlay).unwrap_or_default();
    if session.mode.get() != RequestMode::Plan
        && let Some(plan) = session.plan.take()
    {
        lines.push_str(&prompt::plan_section(&plan));
    }
    lines
}

/// Appends the task's context -- mode, plan, preflight, acceptance -- to the
/// task's own message, after the request itself, which stays the first block.
pub(super) fn carry_task_context(message: &mut Message, context: String) {
    let context = context.trim_start();
    if !context.is_empty() {
        message.content.push(Block::Text(context.to_string()));
    }
}

/// [`build_system_prompt`] for a session that is already running: the same
/// block, from what the session holds.
pub(super) fn system_prompt_for(session: &Session<'_>) -> String {
    build_system_prompt(
        &session.config().limits,
        &session.config().web,
        &session.config().agents,
        &session.config().helpers,
        &session.config().decisions,
        &session.roster,
        session.profile,
        session.interface.get(),
        &session.manifest,
    )
}

/// The most files one preflight serves in full, and the most bytes one of
/// them may hold to be served at all.
///
/// **A file too large to serve whole is not served.** The section says *in
/// full*, and a truncated file under that heading is a claim the model cannot
/// check: it would read the first half as the whole of it.
pub(super) const PREFLIGHT_SERVE_FILES: usize = 6;
pub(super) const PREFLIGHT_SERVE_BYTES: u64 = 32 * 1024;

/// The acceptance lister: one toolless request that turns the task into the
/// items its completion is checked against (`acceptance.rs`). Runs once per
/// task before the first turn when `[helpers] acceptance_list` is on; a
/// request too short to need the repository gets none.
///
/// The call, ready to run on another thread: everything
/// it reads is owned or `Sync`, so it runs beside the preflight Scout rather
/// than after it -- the two are independent reads of the same request, and
/// in series the person waited for both (2026-09-23).
pub(super) struct PendingAcceptance<'s> {
    task: String,
    model: String,
    effort: crate::wire::Effort,
    profile: &'s crate::sandbox::profile::Profile,
    session_id: &'s crate::contract::SessionId,
    token: invoke::CancellationToken,
}

impl PendingAcceptance<'_> {
    pub(super) fn call(self) -> Option<crate::helpers::HelperRecord> {
        crate::helpers::acceptance_list(
            &self.task,
            crate::helpers::HelperRoute::new(&self.model, self.effort),
            self.profile,
            self.session_id,
            &self.token,
        )
    }
}

/// The lister's call for this task, or `None` when no list is derived.
pub(super) fn start_acceptance<'s>(
    task: &str,
    session: &'s Session<'_>,
) -> Option<PendingAcceptance<'s>> {
    let helpers = session.config().helpers.clone();
    if !helpers.enabled || !helpers.acceptance_list || !request_may_need_the_repository(task) {
        return None;
    }
    let model = helpers.model.clone()?;
    let effort = helpers.effort.for_helper("accept")?;
    let token = invoke::CancellationToken::new();
    session.interrupt.arm(token.clone());
    Some(PendingAcceptance {
        task: task.to_string(),
        model,
        effort,
        profile: session.profile,
        session_id: session.id,
        token,
    })
}

/// The lister's answer parsed into the block, the items and the record.
pub(super) fn finish_acceptance(
    session: &Session<'_>,
    record: Option<crate::helpers::HelperRecord>,
) -> Option<(
    String,
    Vec<crate::acceptance::Item>,
    crate::helpers::HelperRecord,
)> {
    let record = record?;
    if record.outcome.cancelled {
        session.interrupt.consumed();
    }
    output::acceptance_helper(&record);
    if !record.outcome.ok {
        session_println!(
            "acceptance: the lister did not answer ({})",
            record.outcome.text.lines().next().unwrap_or("").trim()
        );
        return None;
    }
    let items = crate::acceptance::parse(&record.outcome.text);
    if items.is_empty() {
        session_println!("acceptance: the lister named nothing verifiable");
        return None;
    }
    session_println!(
        "acceptance: {} item(s) derived from the request",
        items.len()
    );
    Some((crate::acceptance::render_list(&items), items, record))
}

/// The decision hold, applied right before a cell or direct frame would run
/// (`session.rs`'s `act_on`, at the `run_cell`/`run_direct_frame` site) --
/// moved out of `session.rs` for the Phase 59 size ratchet, 2026-09-16.
/// `Some(step)` means the caller returns it at once, without running
/// anything; `None` means continue exactly as `act_on` already does.
///
/// `cell::compile` here is a second **parse**, never a second run -- V8 has
/// not seen `source` yet, and `runtime.run_cell`/`run_direct_frame` parse it
/// again themselves (`runtime/cell.rs::compile` touches no isolate).
pub(super) fn apply_decision_hold(
    session: &Session<'_>,
    task_state: &mut TaskState,
    runtime: &Runtime,
    lowered: Option<&crate::abi::Lowered>,
    source: &str,
    calls: &[(&String, &String, &serde_json::Value)],
) -> Option<Step> {
    let effect = if let Some(lowered) = lowered {
        crate::decide::direct_frame_names_effect(&lowered.calls).map(|name| (name, None))
    } else {
        crate::runtime::cell::compile(source, runtime.next_cell())
            .ok()
            .and_then(|compiled| crate::decide::names_effect(&compiled.free_names))
            .map(|(name, offset)| {
                let (line, _) = crate::runtime::cell::line_and_column(source, offset);
                (name, Some(line))
            })
    };
    let decisions = &session.config().decisions;
    match crate::decide::hold_for(
        decisions.mode,
        task_state.intent.as_ref(),
        decisions.hold_above,
        effect.as_ref().map(|(name, line)| (name.as_str(), *line)),
        task_state.effect_holds > 0,
    ) {
        crate::decide::Hold::Run => {
            apply_drift_hold(session, task_state, runtime, effect.as_ref(), source, calls)
        }
        crate::decide::Hold::Overridden => {
            task_state.effect_overrides += 1;
            output::decisions(task_state.decisions_telemetry(&session.config().decisions));
            None
        }
        crate::decide::Hold::Shadow(_) => {
            task_state.would_hold += 1;
            output::decisions(task_state.decisions_telemetry(&session.config().decisions));
            None
        }
        crate::decide::Hold::Held(block) => {
            task_state.effect_holds += 1;
            output::decisions(task_state.decisions_telemetry(&session.config().decisions));
            // One `tool_result` per provider call this turn requested,
            // exactly as a refused turn answers above `act_on`'s own
            // ProtocolError case -- `calls` is the raw list scraped from the
            // assistant message either way, so this covers a native
            // `execute_cell` call and a lowered direct frame alike; a bare
            // markdown program made no call and gets none.
            let native_result = (!calls.is_empty()).then(|| Message {
                role: Role::User,
                content: calls
                    .iter()
                    .map(|(id, _, _)| Block::ToolResult {
                        tool_use_id: (*id).clone(),
                        content: block.clone(),
                        is_error: false,
                    })
                    .collect(),
                historical: None,
            });
            Some(Step {
                answer: Some(block.clone()),
                historical: Some(block),
                native_result,
                response: None,
                prose: false,
                record: None,
                rollback: None,
                view: CellView::default(),
            })
        }
    }
}

/// The drift question (2643), asked only after the intent hold above has
/// itself returned `Run` for this cell -- an effectful cell running while a
/// read-only intent is not confident enough to hold, or while the intent is
/// not read-only at all, still gets one chance to be checked against the
/// plan's own current step. `effect` is `None` for a pure cell, in which case
/// nothing is ever asked.
///
/// Unlike the intent question (asked once, before the first turn, and cached
/// on [`TaskState::intent`]), this question is asked fresh before every
/// candidate effectful cell, including the one re-issued after a hold --
/// [`crate::decide::drift_for`]'s own once rule (`already_held`) is what
/// keeps that second ask from holding the cell again, exactly as
/// [`crate::decide::hold_for`]'s `already_held` does for the intent hold.
fn apply_drift_hold(
    session: &Session<'_>,
    task_state: &mut TaskState,
    runtime: &Runtime,
    effect: Option<&(String, Option<u32>)>,
    source: &str,
    calls: &[(&String, &String, &serde_json::Value)],
) -> Option<Step> {
    effect?;
    let decisions = &session.config().decisions;
    let model = decisions.model.as_deref()?;
    if decisions.mode == crate::config::DecisionMode::Off {
        return None;
    }
    let plan = runtime.plan();
    let step = plan
        .iter()
        .find(|item| item.status == crate::runtime::outcome::PlanStatus::Active)?;
    let already_held = task_state.drift_holds > 0;
    task_state.drift_asked += 1;
    let answer = match crate::decide::drift_satisfied(model, &task_state.task, &step.text, source) {
        Ok(noul) => Some(noul),
        Err(_) => {
            task_state.drift_failed += 1;
            None
        }
    };
    match crate::decide::drift_for(
        decisions.mode,
        answer,
        decisions.drift_no_below,
        &step.text,
        already_held,
    ) {
        crate::decide::Drift::Run => None,
        crate::decide::Drift::Shadow(_) => {
            task_state.would_drift += 1;
            output::decisions(task_state.decisions_telemetry(&session.config().decisions));
            None
        }
        crate::decide::Drift::Held(block) => {
            task_state.drift_holds += 1;
            output::decisions(task_state.decisions_telemetry(&session.config().decisions));
            let native_result = (!calls.is_empty()).then(|| Message {
                role: Role::User,
                content: calls
                    .iter()
                    .map(|(id, _, _)| Block::ToolResult {
                        tool_use_id: (*id).clone(),
                        content: block.clone(),
                        is_error: false,
                    })
                    .collect(),
                historical: None,
            });
            Some(Step {
                answer: Some(block.clone()),
                historical: Some(block),
                native_result,
                response: None,
                prose: false,
                record: None,
                rollback: None,
                view: CellView::default(),
            })
        }
    }
}

/// The decision model's one request, asked once per task beside
/// [`preflight_block`] and [`append_acceptance`], on its own thread.
///
/// **In `on` the task waits for it, because it decides; in `shadow` it does
/// not, because it only records** -- the answer is collected after the
/// model's first turn ([`PendingDecision::settle`]), by which time it is
/// nearly always back, and at the task's end at the latest. A failed or
/// absent decision leaves the task exactly as it is; the error is recorded,
/// never surfaced as a task failure.
pub(super) fn task_decision(
    task: &str,
    session: &Session<'_>,
    has_history: bool,
) -> (
    Option<crate::decide::TaskDecision>,
    u32,
    Option<PendingDecision>,
) {
    // A resumed conversation already holds at least one earlier request.
    let earlier_requests = session.requests.get().max(u32::from(has_history));
    session.requests.set(earlier_requests.saturating_add(1));
    let decisions = session.config().decisions.clone();
    let Some(model) = decisions.model else {
        return (None, 0, None);
    };
    if decisions.mode == crate::config::DecisionMode::Off {
        return (None, 0, None);
    }
    let request = task.to_string();
    let context = crate::decide::TaskContext::of(earlier_requests, &session.project.instructions);
    let (sent, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sent.send(crate::decide::task_questions_in(
            &model,
            &request,
            Some(&context),
        ));
    });
    let pending = PendingDecision { answer };
    if decisions.mode == crate::config::DecisionMode::Shadow {
        return (None, 0, Some(pending));
    }
    match pending.settle(true) {
        Some(Ok(decision)) => (Some(decision), 0, None),
        Some(Err(())) | None => (None, 1, None),
    }
}

/// A task decision still on its way.
pub(super) struct PendingDecision {
    answer:
        std::sync::mpsc::Receiver<Result<crate::decide::TaskDecision, crate::decide::DecideError>>,
}

impl PendingDecision {
    /// The answer if it is back -- or, with `wait`, once it is, bounded by
    /// the request's own [`crate::decide::DECISION_TIMEOUT`]. `None` means
    /// not back yet; `Some(Err(()))` a request that failed, said once.
    pub(super) fn settle(&self, wait: bool) -> Option<Result<crate::decide::TaskDecision, ()>> {
        let received = if wait {
            self.answer
                .recv_timeout(crate::decide::DECISION_TIMEOUT + std::time::Duration::from_secs(1))
                .map_err(|_| ())
        } else {
            match self.answer.try_recv() {
                Ok(answer) => Ok(answer),
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(()),
            }
        };
        Some(match received {
            Ok(Ok(decision)) => {
                session_println!(
                    "decision: intent {} ({:.2}), complexity {} ({:.2}), {} ms",
                    decision.intent.choice,
                    decision.intent.confidence,
                    decision.complexity.choice,
                    decision.complexity.confidence,
                    decision.intent.latency_ms
                );
                Ok(decision)
            }
            Ok(Err(error)) => {
                session_println!("decision: no answer ({error})");
                Err(())
            }
            Err(()) => {
                session_println!("decision: no answer (the request never returned)");
                Err(())
            }
        })
    }
}

/// The acceptance list in the task's system block: derived once from the
/// request, shown beside the preflight, paid for as one helper call, and
/// returned for the task state to check when the model claims completion.
pub(super) fn append_acceptance(
    session: &Session<'_>,
    record: Option<crate::helpers::HelperRecord>,
    task_context: &mut String,
    budget: &mut TaskSpend,
) -> Vec<crate::acceptance::Item> {
    let Some((block, items, record)) = finish_acceptance(session, record) else {
        return Vec::new();
    };
    task_context.push_str(&block);
    budget.add_helpers(std::slice::from_ref(&record));
    items
}

/// The stand-in gate: a request of fewer words than this gets no preflight.
pub(super) const PREFLIGHT_MIN_WORDS: usize = 4;

/// Whether this request plausibly needs the repository at all.
///
/// A request under [`PREFLIGHT_MIN_WORDS`] words — "hi", "thanks", "carry
/// on" — gets no preflight; everything else is decided by
/// [`crate::preflight::should_scout`] on the request's own signals.
/// The whole-response cap on a dissection, in tokens: ~300 words with room
/// for the headings (memory `scout-dissection-latency-2026-09-23`).
const DISSECTION_CAP: u32 = 640;

pub(super) fn request_may_need_the_repository(task: &str) -> bool {
    task.split_whitespace().count() >= PREFLIGHT_MIN_WORDS
}

/// The effort a request's kind sets for one task, and its restoration.
///
/// **The person's choice always wins.** A kind acts only when the session's
/// effort is `default` -- one the person never set -- and what it set is
/// put back when the task ends, however the task ends, because the lease
/// restores on drop. `explore` and `question` lower the effort to `low`:
/// measured 2026-09-23, effort bought neither speed nor a better dissection
/// on an exploration, and the user's steer is to keep reasoning low where
/// it earns nothing. `mode = shadow` records what would have been set.
pub(super) struct EffortLease<'s, 'a> {
    session: &'a Session<'s>,
    restore: Option<wire::Effort>,
    pub(super) set: Option<wire::Effort>,
    pub(super) would_set: Option<wire::Effort>,
}

/// The effort a main-model turn asks for. One the person chose -- in
/// settings, a flag or `/effort` -- is sent as chosen. Left at `default`, an
/// OpenAI-family model asks for `low`, exactly what the ruler's `low` arm
/// set: on gpt-6-sol it was faster on every task of two rounds at equal
/// results and equal or fewer tokens (2026-09-24, helper-measurements §8 and
/// the Sterna-vs-Codex head-to-head). Other families keep the provider's own
/// setting; nothing measured argues for changing theirs.
pub(super) fn turn_effort(chosen: wire::Effort, model: &str) -> wire::Effort {
    chosen.sent_for(model)
}

impl<'s, 'a> EffortLease<'s, 'a> {
    pub(super) fn for_kind(
        session: &'a Session<'s>,
        decision: Option<&crate::decide::TaskDecision>,
    ) -> Self {
        let mode = session.config().decisions.mode;
        let wanted = decision
            .and_then(crate::decide::TaskDecision::confident_kind)
            .filter(|kind| {
                *kind == crate::decide::KIND_EXPLORE || *kind == crate::decide::KIND_QUESTION
            })
            .map(|_| wire::Effort::Low)
            .filter(|_| session.effort.get() == wire::Effort::Default);
        let mut lease = Self {
            session,
            restore: None,
            set: None,
            would_set: None,
        };
        match (mode, wanted) {
            (crate::config::DecisionMode::On, Some(effort)) => {
                lease.restore = Some(session.effort.replace(effort));
                lease.set = Some(effort);
                session_println!("decision: effort {} for this task", effort.name());
            }
            (crate::config::DecisionMode::Shadow, Some(effort)) => {
                lease.would_set = Some(effort);
            }
            _ => {}
        }
        lease
    }
}

impl Drop for EffortLease<'_, '_> {
    fn drop(&mut self) {
        if let Some(effort) = self.restore.take() {
            self.session.effort.set(effort);
        }
    }
}

/// [`preflight_block`]'s result: the system-prompt block, if a scout ran and
/// answered, plus what the decision model's complexity answer did to that
/// choice -- `scout_signal` and `would_scout` feed straight into
/// `TaskState::with_decision`'s telemetry.
pub(super) struct PreflightOutcome {
    pub(super) block: Option<String>,
    /// Which brief the Scout ran with, when one ran.
    pub(super) brief: Option<crate::preflight::Brief>,
    /// `mode = shadow` and the kind read `explore`: the dissection brief
    /// would have been chosen.
    pub(super) would_dissect: bool,
    /// Whether `preflight::SIGNAL_DECIDED_EXPLORATION` was one of the reasons
    /// this task actually ran a scout (`mode = on` only).
    pub(super) scout_signal: bool,
    /// Whether the complexity answer would have added that signal had
    /// `mode` been `on` (`mode = shadow` only; recorded, changes nothing).
    pub(super) would_scout: bool,
}

impl PreflightOutcome {
    const NONE: Self = Self {
        block: None,
        brief: None,
        would_dissect: false,
        scout_signal: false,
        would_scout: false,
    };
}

/// One preflight block to append to this task's system prompt, or `None` when
/// no scout ran or none answered.
///
/// The invariant: **a failed preflight leaves the session exactly as it is
/// today**, and **the scout never attempts the task**: it is handed a
/// scouting brief built around the verbatim request and the manifest, and
/// answers with constraints, files, tests, capabilities and risks
/// (`smarter-cheaper-roadmap.md`, *Preflight Helper*). With
/// `preflight_scope = "auto"` it runs only when the request carries an
/// uncertainty signal, so a task that names existing files and available
/// tools pays nothing -- unless `decision` names `needs_exploration` at or
/// above `scout_above` while `[decisions] mode = "on"` (F2): that signal
/// can only add a reason to run the scout, never remove one, and `mode =
/// "shadow"` only records what would have happened (`would_scout`).
pub(super) fn preflight_block(
    task: &str,
    session: &Session<'_>,
    transcript: &mut Transcript,
    decision: Option<&crate::decide::TaskDecision>,
) -> PreflightOutcome {
    let helpers = session.config().helpers.clone();
    let decisions = session.config().decisions.clone();
    // The kind (2026-09-23): a confident `explore` briefs the Scout to
    // dissect the request and is itself a reason to run it -- with or
    // without `[helpers] preflight`, which opts in the span Scout only: the
    // dissection is what `mode = "on"` asked for when it read exploration.
    let explore = decision
        .and_then(crate::decide::TaskDecision::confident_kind)
        .is_some_and(|kind| kind == crate::decide::KIND_EXPLORE);
    let dissect = explore && decisions.mode == crate::config::DecisionMode::On;
    if !helpers.enabled || !(helpers.preflight || dissect) || !request_may_need_the_repository(task)
    {
        return PreflightOutcome::NONE;
    }
    let complexity = decision.map(|decision| &decision.complexity);
    let clears_scout_above = complexity.is_some_and(|complexity| {
        complexity.choice == crate::decide::NEEDS_EXPLORATION
            && complexity.confidence >= decisions.scout_above
    });
    let would_scout = decisions.mode == crate::config::DecisionMode::Shadow && clears_scout_above;
    let decided_for_scout = if decisions.mode == crate::config::DecisionMode::On {
        complexity
    } else {
        None
    };
    let would_dissect = explore && decisions.mode == crate::config::DecisionMode::Shadow;
    let brief_kind = if dissect {
        crate::preflight::Brief::Dissection
    } else {
        crate::preflight::Brief::Spans
    };
    let checks_configured = crate::verification::load(session.profile)
        .map(|config| !config.checks.is_empty())
        .unwrap_or(false);
    let scouting_decision = crate::preflight::should_scout(
        task,
        &session.manifest,
        helpers.preflight_scope,
        checks_configured,
        decided_for_scout,
        decisions.scout_above,
    );
    let scouting_decision = match (dissect, scouting_decision) {
        (true, crate::preflight::Decision::Run(mut signals)) => {
            signals.push(crate::preflight::SIGNAL_DECIDED_EXPLORE);
            crate::preflight::Decision::Run(signals)
        }
        (true, crate::preflight::Decision::Skip(_)) => {
            crate::preflight::Decision::Run(vec![crate::preflight::SIGNAL_DECIDED_EXPLORE])
        }
        (false, decision) => decision,
    };
    let scout_signal = matches!(
        &scouting_decision,
        crate::preflight::Decision::Run(signals)
            if signals.contains(&crate::preflight::SIGNAL_DECIDED_EXPLORATION)
    );
    session_println!(
        "preflight: {}",
        crate::preflight::signals_summary(&scouting_decision)
    );
    if matches!(scouting_decision, crate::preflight::Decision::Skip(_)) {
        return PreflightOutcome {
            block: None,
            brief: None,
            would_dissect,
            scout_signal,
            would_scout,
        };
    }
    let none = PreflightOutcome {
        block: None,
        brief: None,
        would_dissect,
        scout_signal,
        would_scout,
    };
    let Some(model) = helpers.model.as_deref() else {
        return none;
    };
    let Some(effort) = helpers.effort.for_helper("find") else {
        return none;
    };
    let token = invoke::CancellationToken::new();
    session.interrupt.arm(token.clone());
    let mut brief = crate::preflight::scouting_brief_for(brief_kind, task, &session.manifest);
    // The one-shot dissection answers from the file listing in one request
    // instead of searching; without a listing the search loop runs.
    let listing = (dissect && helpers.scout_oneshot)
        .then(|| crate::preflight::listing_section(&session.project.root))
        .flatten();
    let oneshot = listing.is_some();
    if let Some(listing) = &listing {
        brief.push_str(listing);
    }
    // Ranking and judging (2644, 2645): `decisions.model` set and `mode`
    // not `off` is the same gate the intent/complexity question already
    // uses above. `apply` carries `mode = on` versus `shadow` -- shadow
    // still asks and counts, it just never reorders what the Scout is
    // served or writes a line into what it returned.
    let decisions_active = decisions.mode != crate::config::DecisionMode::Off;
    let decisions_apply = decisions.mode == crate::config::DecisionMode::On;
    let decision_model = decisions.model.clone();
    // The pool ranked is `prepare_scout`'s own term-matched walk, never a
    // separate directory walk: `helper_context.rs` does no model work by
    // its own invariant, so the ranking happens here, over what that walk
    // already found, rather than inside it.
    let scout_pool = (decisions_active && decision_model.is_some()).then(|| {
        crate::helper_context::prepare(
            crate::helper_context::HelperRole::Scout,
            task,
            session.profile,
            &token,
        )
    });
    let rank_candidates = scout_pool.as_ref().map(|prepared| {
        crate::helpers::scout_candidates_from_evidence(&prepared.evidence, session.profile, 40)
    });
    let rank = match (&rank_candidates, decision_model.as_deref()) {
        (Some(candidates), Some(decision_model)) => Some((
            candidates.as_slice(),
            crate::helpers::ScoutRankRoute {
                model: decision_model,
                floor: decisions.scout_relevance_below,
                apply: decisions_apply,
            },
        )),
        _ => None,
    };
    let judge = if decisions_active {
        decision_model
            .as_deref()
            .map(|decision_model| crate::helpers::HelperJudge {
                model: decision_model,
                floor: decisions.helper_no_below,
                apply: decisions_apply,
            })
    } else {
        None
    };
    let helper_context = crate::helpers::HelperContext {
        profile: session.profile,
        session: session.id,
        token: &token,
    };
    // A dissection is capped where it was measured right: output length is
    // the lever on its latency, and past ~300 words it was both slower and
    // malformed more often.
    let route = crate::helpers::HelperRoute {
        model,
        effort,
        cap: dissect.then_some(DISSECTION_CAP),
    };
    let Some(judged) = crate::helpers::preflight_judged(
        &brief,
        route,
        helper_context,
        rank,
        judge,
        oneshot,
        |record| {
            let Some(ui) = session.ui else {
                return;
            };
            // The real user turn is recorded below in the established rollout
            // order. This snapshot makes the submitted request and its Scout
            // visible immediately without adding a synthetic model turn.
            let mut visible = transcript.clone();
            visible
                .conversation
                .messages
                .push(Message::text(Role::User, task));
            visible.notebook.preflight = Some(record.clone());
            ui.publish(&visible, &ServedBy::default(), tui::Activity::Searching);
        },
    ) else {
        return none;
    };
    let record = judged.record;
    let ranking_note = judged
        .ranking
        .as_ref()
        .map(crate::helpers::ScoutRanking::note);
    if let Some(ranking) = &judged.ranking {
        output::helpers_ranking(ranking);
    }
    if let Some((noul, latency_ms)) = judged.judge {
        output::helpers_checked(noul, decisions.helper_no_below, latency_ms);
    }
    if record.outcome.cancelled {
        session.interrupt.consumed();
    }
    output::preflight(&record);
    // Keep the resolved Scout beside this request for every later task-frame.
    // The next task clears it before deciding whether another preflight runs.
    transcript.notebook.preflight = Some(record.clone());
    let block = record.outcome.ok.then(|| {
        let mut named = crate::preflight::spans(&record.outcome.text);
        if brief_kind == crate::preflight::Brief::Dissection {
            for (path, why) in crate::preflight::dissection_files(&record.outcome.text) {
                if !named.iter().any(|(seen, _)| *seen == path) {
                    named.push((path, why));
                }
            }
        }
        let (served, unserved) = preflight_serving(session.profile, &named);
        let served: Vec<(String, String)> = served
            .into_iter()
            .map(|(path, _why, text)| (path, text))
            .collect();
        crate::preflight::render_brief(
            brief_kind,
            task,
            &record.outcome.text,
            &served,
            &unserved,
            ranking_note.as_deref(),
        )
    });
    PreflightOutcome {
        block,
        brief: Some(brief_kind),
        would_dissect,
        scout_signal,
        would_scout,
    }
}

/// A file the preflight serves whole: the path the scout named, why it named
/// it, and the file's complete text.
pub(super) type ServedFile = (String, String, String);

/// A file the scout named that the preflight did not serve, and the reason in
/// the person's words.
pub(super) type UnservedFile = (String, String);

/// The named files that can be served whole -- inside the grant, a regular
/// file, UTF-8, and small enough that *in full* is true of it -- and beside
/// them every named file that was **not** served, with its reason.
///
/// **The profile decides what may be served, not this function.** A scout
/// that named a path outside the grant has it refused here for the same
/// reason `read` would refuse it, and a preflight is not a way around a
/// grant.
///
/// **A file the scout named and the preflight did not serve is stated, never
/// dropped.** Not serving it is right — the section claims *in full*, and a
/// truncated file under that heading is a claim the model cannot check — but
/// silence turns a bounded offer into an invisible one: the model cannot
/// `read` a file it was never told about, so it re-derives what it was almost
/// given. Each reason names the bound so the model can act on it.
pub(super) fn preflight_serving(
    profile: &Profile,
    named: &[(String, String)],
) -> (Vec<ServedFile>, Vec<UnservedFile>) {
    let mut served = Vec::new();
    let mut unserved: Vec<(String, String)> = Vec::new();
    for (path, why) in named {
        if served.len() == PREFLIGHT_SERVE_FILES {
            unserved.push((
                path.clone(),
                format!("not served: this section serves at most {PREFLIGHT_SERVE_FILES} files"),
            ));
            continue;
        }
        let candidate = std::path::Path::new(path);
        let absolute = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            profile.root().join(candidate)
        };
        let Ok(granted) = profile.check(
            "preflight",
            crate::sandbox::profile::Access::Read,
            &absolute,
        ) else {
            unserved.push((
                path.clone(),
                "not served: outside this session's grant".into(),
            ));
            continue;
        };
        let Ok(metadata) = fs::metadata(&granted) else {
            unserved.push((path.clone(), "not served: no such file".into()));
            continue;
        };
        if !metadata.is_file() {
            unserved.push((path.clone(), "not served: not a regular file".into()));
            continue;
        }
        if metadata.len() > PREFLIGHT_SERVE_BYTES {
            unserved.push((
                path.clone(),
                format!(
                    "not served whole: {} bytes, over the {PREFLIGHT_SERVE_BYTES}-byte limit",
                    metadata.len()
                ),
            ));
            continue;
        }
        let Ok(text) = fs::read_to_string(&granted) else {
            unserved.push((path.clone(), "not served: not UTF-8 text".into()));
            continue;
        };
        served.push((path.clone(), why.clone(), text));
    }
    (served, unserved)
}

/// The compiled profile, as the model needs to read it.
///
/// **The invariant: this reports the profile that is actually in force, never
/// the one the configuration asked for.** It is built from `Profile`'s own
/// accessors for that reason — a settings document that failed to parse
/// grants nothing, and a model told otherwise would plan against grants it
/// does not have.
///
/// `pub` so `tests/session.rs`'s byte-equality test can build the same facts
/// the binary did rather than spelling them a second time — the same reason
/// that test calls [`prompt::render_system`] instead of quoting its output.
pub fn session_facts(profile: &Profile) -> prompt::SessionFacts {
    // The roots come from `Profile::writable_roots`, which is the same
    // answer `Profile::check` gives and the same one the manifest renders.
    // Listing only the write-`allow` rules said "nothing is writable" for an
    // ordinary session, on the line above the manifest naming the project
    // root as writable.
    let mut writable: Vec<String> = profile
        .writable_roots()
        .into_iter()
        .map(|root| root.display().to_string())
        .collect();
    writable.extend(
        profile
            .rules()
            .filter(|rule| rule.write() && rule.effect() == crate::sandbox::profile::Effect::Allow)
            .map(|rule| rule.written().to_string()),
    );
    writable.sort();
    writable.dedup();
    prompt::SessionFacts {
        root: profile.root().display().to_string(),
        writable,
        command_patterns: profile.command_pattern_count(),
        // Not `args.yolo`: the flag is a request, the profile is the grant.
        // A mutation that stopped `--yolo` reaching the compiler survived
        // while this read the flag, because the model was still told the
        // grant was open (2026-09-06).
        all_commands: profile.admits_every_command(),
        network: profile.grants_network(),
        interface: crate::abi::Interface::default(),
        manifest: None,
    }
}

/// [`session_facts`] for the interface this session declares and the
/// manifest it collected — the facts the binary actually renders.
pub fn session_facts_with(
    profile: &Profile,
    interface: crate::abi::Interface,
    manifest: &crate::manifest::Manifest,
) -> prompt::SessionFacts {
    let mut facts = session_facts(profile);
    facts.interface = interface;
    facts.manifest = Some(manifest.render());
    facts
}

/// The executables the manifest looks for on `PATH`, so the model knows
/// before acting which of the tools a task usually names are absent.
pub const MANIFEST_PROBE: [&str; 24] = [
    "bash", "sh", "python3", "python", "git", "cargo", "rustc", "gcc", "g++", "clang", "make",
    "cmake", "node", "npm", "rg", "fd", "jq", "gdb", "lldb", "valgrind", "pytest", "go", "java",
    "docker",
];

/// The effective capability and environment manifest for one session
/// (`smarter-cheaper-roadmap.md`, *Capability/environment manifest*): the
/// compiled profile's roots and policies, the probed executables, and the
/// capabilities this configuration cannot provide.
pub fn system_manifest(profile: &Profile, config: &SternaConfig) -> crate::manifest::Manifest {
    let mut manifest = crate::manifest::Manifest::collect(profile, &MANIFEST_PROBE);
    if !config.web.enabled {
        manifest
            .unavailable
            .push("web.fetch and web.search: the host web broker is disabled".into());
    } else if !config.web.search_configured() {
        manifest
            .unavailable
            .push("web.search: no search provider is configured".into());
    }
    if config.helpers.model.is_none() || !config.helpers.enabled {
        manifest
            .unavailable
            .push("helper.*: no helper model is configured".into());
    }
    if matches!(config.agents.mode, crate::config::AgentsMode::Off) {
        manifest
            .unavailable
            .push("agent.run: subagents are off in this configuration".into());
    }
    manifest
}

/// The tokens a request for `conversation` would carry, estimated from its
/// wire body (moved from `session.rs` for the size ratchet, 2026-09-23).
pub(super) fn estimate_request_tokens(conversation: &Conversation, model: &str) -> u64 {
    estimate_task_request_tokens(conversation, model, "")
}

pub(super) fn estimate_task_request_tokens(
    conversation: &Conversation,
    model: &str,
    task: &str,
) -> u64 {
    let request = prompt::with_task_context(conversation, model, task);
    let body = wire::request_body_on_model(&request, model);
    preview::estimate_tokens(&String::from_utf8_lossy(&body)) as u64
}
