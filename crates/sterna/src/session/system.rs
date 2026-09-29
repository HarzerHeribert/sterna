//! The system block of one task: the prompt, its facts and manifest, and
//! the decision model's task question and holds (moved out of `session.rs`
//! for the Phase 59 size ratchet, 2026-09-13).

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
    system
}
/// Keeps the system prompt byte-stable from task to task, so a task after the
/// first reads the whole earlier conversation from the provider's cache.
///
/// It is replaced only when what it says changed -- the instructions, the
/// grants, the model, the interface -- never for its orientation timestamp:
/// that reaches the next session. Whatever differs per task rides in the
/// task's own message ([`task_lines`], [`carry_task_context`]).
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

/// A system prompt without the part that changes on its own between tasks
/// and does not warrant a new cache.
fn lasting_part(system: &str) -> String {
    system
        .lines()
        .filter(|line| !line.starts_with("task-start UTC:"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// This task's request-mode line and the approved plan, if any: task
/// context, carried in the task's message rather than the system prompt.
pub(super) fn task_lines(session: &Session<'_>) -> String {
    let mut lines = prompt::request_mode_line(session.mode.get()).unwrap_or_default();
    if session.mode.get() != RequestMode::Plan
        && let Some(plan) = session.plan.take()
    {
        lines.push_str(&prompt::plan_section(&plan));
    }
    lines
}

/// Appends the task's context -- mode, plan -- to the
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
        &session.config().decisions,
        &session.roster,
        session.profile,
        session.interface.get(),
        &session.manifest,
    )
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
        crate::decide::Hold::Run => None,
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

/// The decision model's one request, asked once per task on its own thread.
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
        // The classifier's raw answer is the telemetry's, never the
        // person's: the task's decision summary carries it.
        Some(match received {
            Ok(Ok(decision)) => Ok(decision),
            Ok(Err(_)) | Err(()) => Err(()),
        })
    }
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
            .filter(|_| session.effort.get() == wire::Effort::Auto);
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
                session_println!(
                    "Effort {} for this task: it looks like a question, not a change.",
                    effort.name()
                );
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
