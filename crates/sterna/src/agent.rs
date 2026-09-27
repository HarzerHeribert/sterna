//! A subagent: one nested turn loop, run out of band, whose answer comes back
//! as a handle — capability map Phase 64.
//!
//! **A subagent is a background job whose work is a turn loop rather than a
//! spawned command.** That is the whole design decision, and it is what makes
//! this module small. A cell cannot call the session loop directly: the
//! isolate is borrowed while the cell runs, so re-entering the loop from a
//! host callback would re-enter V8. `bg` already solved exactly that shape —
//! return a handle at once, do the work on another thread, deliver completion
//! as an event whose result is a handle — so a subagent rides it as a second
//! producer instead of inventing a second delivery path. Cancellation, the
//! deadline, the payload store, batching and dedup are all `bg`'s and are not
//! reimplemented here.
//!
//! **A subagent is a session you can read and address** (the user,
//! 2026-09-17: *"In Claude code user can talk to subagent by selecting and
//! jumping into its session in and out"*). It writes its own rollout as it
//! goes, in the same format and beside the parent's, so a person can read
//! what it is doing while it does it; and it has an inbox, so a person can
//! say something to it, delivered at its next turn boundary. The supervisor
//! may watch it on the same evidence.
//!
//! **What a subagent still is not**: it cannot start a subagent of its own,
//! it is started and owned by a cell rather than by a person, and its rollout
//! is a record to read rather than a session to restart -- nothing resumes a
//! subagent, and [`AgentRollout::for_job`] keeps the file out of the folder
//! `--sessions` lists for exactly that reason.

use std::path::{Path, PathBuf};

use crate::contract::{Conversation, Message, Role, SessionId};
use crate::prompt::{self, Budget, CellResult, ErrorSection, Extracted};
use crate::runtime::bindings::HostGlobals;
use crate::runtime::isolate::Runtime;
use crate::runtime::outcome::CellOutcome;
use crate::sandbox::profile::Profile;
use crate::tools::invoke::CancellationToken;
use crate::tools::registry;
use crate::wire::{self, Effort};

/// The most of a salvaged answer one early stop carries.
///
/// A subagent that is cancelled, times out or exhausts a turn hint still
/// returns **its own last words** rather than a sentence about having none
/// ([`run_narrowed_metered`]). That text is prose written for the parent, so
/// it is bounded the way a plan section is -- enough for a substantive draft,
/// small enough that an answer nobody reads costs about one screen.
const SALVAGED_ANSWER_BYTES: usize = 8 * 1024;

/// How far into a conversation the salvage looks for the subagent's last
/// words. The tail is where they are; a scan of a long conversation is not.
const SALVAGE_DEPTH: usize = 8;

/// What a subagent produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResult {
    /// The subagent's own answer — its top-level `return`, rendered.
    pub answer: String,
    /// Why it stopped, in one word: `returned`, `deadline`, `turns`,
    /// `cancelled`, `failed`. Only `returned` means the answer is the
    /// subagent's own `return`; the others carry its last words instead.
    pub status: String,
    /// Turns actually taken, reported with the parent's cumulative spend.
    pub turns: u64,
    /// Provider-reported tokens, summed over the turns that reported any.
    pub tokens: u64,
    /// What the loop actually did, one entry per tool call in order.
    ///
    /// A helper reports numbers it claims to have computed. Without this the
    /// claim is unfalsifiable: the caller sees an answer and no trace of the
    /// work. Tool names only -- never an argument, never a payload, so this
    /// stays the same shape §9.4's trajectory already is.
    pub trajectory: Vec<String>,
}

/// How a subagent is asked for.
///
/// **Nothing here caps the work itself.** The user's ruling of 2026-09-17:
/// *"Limits are dumb for abstract tasks … it should be wall clock. Things like
/// context length or subscription / key capacity these are real things which
/// should limit, because sterna can't change them."* So a subagent ends when it
/// returns, when [`AgentOptions::deadline`] passes, when the conversation no
/// longer fits the model's context, when the provider or the subscription
/// refuses, or when it is cancelled -- and in every one of those cases it
/// returns what it actually produced.
#[derive(Debug, Clone)]
pub struct AgentOptions {
    /// An optional turn hint for a deliberately short errand -- a helper's
    /// own `max_turns`, or a cell that wants one look and no more.
    ///
    /// `None` means *until the work is done*, which is the ordinary case: a
    /// subagent that must read before it edits cannot know in advance how
    /// many turns that takes, and a number guessed for it is the cap that
    /// threw away three subagents' work on 2026-09-17.
    pub turns: Option<u64>,
    pub model: String,
    pub effort: Effort,
    /// Wall clock, from the first turn. `None` is no deadline.
    ///
    /// This is the bound that replaces the turn cap: a person waiting is a
    /// real thing, a turn count is not. `bg` arms the same duration as the
    /// job's own deadline, so a provider call that hangs cannot outlive it;
    /// this copy is what lets the loop stop between turns and *say* that time
    /// ran out rather than reporting a bare cancellation.
    pub deadline: Option<std::time::Duration>,
}

/// What a running subagent has done so far, for a parent that looks in.
///
/// The user, 2026-09-17: *"A parent model should check on a subagent from
/// some time. But even Claude Code does not do that."* This is the cheapest
/// honest version -- turns taken and tool names, written as the loop goes, so
/// `agent.run(...).progress()` costs a lock and no provider request. Tool
/// names only, never an argument and never a payload, which is the shape
/// [`AgentResult::trajectory`] already is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentProgress {
    pub turns: u64,
    pub calls: Vec<String>,
}

/// Where a running subagent writes [`AgentProgress`]; `bg` holds the other
/// end and reads it without waiting for the job.
pub type ProgressSink = std::sync::Arc<std::sync::Mutex<AgentProgress>>;

/// What a person has said to a running subagent and the loop has not read
/// yet; `bg::tell` holds the other end.
///
/// **A queue, not an interrupt.** A message arriving while the subagent is
/// inside a provider call waits for the turn boundary: cutting a request
/// short buys nothing -- the tokens are already spent and the answer would be
/// thrown away -- and a message delivered between turns is a message the
/// subagent reads with its work in front of it.
pub type InboxSink = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// The most a message to a subagent may carry, in UTF-8 bytes.
pub const MESSAGE_BYTES: usize = 65_536;

/// Where a subagent's own rollout is written, and the id its lines carry.
///
/// **In a folder beside the parent's file, not next to it.** `.sterna/sessions/`
/// is what `session::resume` lists and resumes, and every `*.jsonl` in it is
/// offered to a person as a session to go back to. A subagent's record is not
/// one: nothing resumes a subagent. Putting it under
/// `<parent id>.agents/<handle>.jsonl` keeps the pair obvious to a person
/// reading the folder while keeping it out of that listing by construction --
/// the listing filters on the `jsonl` extension, and a directory named
/// `<parent id>.agents` does not have it -- rather than by a name filter that
/// a later reader could drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRollout {
    pub path: PathBuf,
    pub id: SessionId,
}

impl AgentRollout {
    /// The record for `handle`, a job of the session `parent` in `root`.
    pub fn for_job(root: &Path, parent: &SessionId, handle: &str) -> Self {
        let dir = root
            .join(".sterna")
            .join("sessions")
            .join(format!("{}.agents", parent.as_str()));
        Self {
            path: dir.join(format!("{handle}.jsonl")),
            id: SessionId::new(format!("{}-{handle}", parent.as_str())),
        }
    }
}

/// What `bg` holds the other end of while a subagent runs: where it publishes
/// progress, where it reads what a person said, and where it writes its own
/// record.
///
/// One value rather than three parameters, because every one of them is
/// `None` for a helper's narrowed loop and `Some` for a subagent `bg`
/// started, and a caller that had to pass three `None`s would eventually pass
/// two.
#[derive(Default, Clone, Copy)]
pub struct Watch<'a> {
    pub progress: Option<&'a ProgressSink>,
    pub inbox: Option<&'a InboxSink>,
    pub record: Option<&'a AgentRollout>,
}

/// A nested loop narrowed from a subagent to a helper.
///
/// The invariant: **a helper holds only what its spec names, and is a leaf.**
/// `little-helpers.md` puts both in the runtime rather than in a helper's
/// prose, so this is what [`run_narrowed`] reads in place of `registry::ALL`
/// and in place of granting the project's `[helpers]` onward.
#[derive(Debug, Clone, Copy)]
pub struct Narrowed {
    /// Tool names, resolved against `registry::ALL`. `helpers::check_spec`
    /// has already refused an unregistered or mutating name at startup.
    pub tools: &'static [&'static str],
    /// The instructions this loop opens with, in place of
    /// [`SUBAGENT_INSTRUCTIONS`] — a helper's own `preamble`.
    pub instructions: &'static str,
}

pub(crate) struct NarrowedRun<'a> {
    narrowed: Option<&'a Narrowed>,
    helper_usage: Option<&'a crate::helpers::HelperUsageTracker>,
    config: Option<&'a crate::config::SternaConfig>,
    watch: Watch<'a>,
}

impl<'a> NarrowedRun<'a> {
    fn ordinary(narrowed: Option<&'a Narrowed>) -> Self {
        Self {
            narrowed,
            helper_usage: None,
            config: None,
            watch: Watch::default(),
        }
    }

    pub(crate) fn helper(
        narrowed: &'a Narrowed,
        usage: &'a crate::helpers::HelperUsageTracker,
    ) -> Self {
        Self {
            narrowed: Some(narrowed),
            helper_usage: Some(usage),
            config: None,
            watch: Watch::default(),
        }
    }
}

/// Runs one subagent to its end, holding every registered tool. Blocking, and
/// called on `bg`'s own worker thread — never on the thread that holds the
/// parent isolate.
pub fn run(
    profile: &Profile,
    session: &SessionId,
    task: &str,
    options: &AgentOptions,
    token: &CancellationToken,
) -> AgentResult {
    run_narrowed(profile, session, task, options, token, None)
}

pub fn run_with_config(
    profile: &Profile,
    session: &SessionId,
    task: &str,
    options: &AgentOptions,
    token: &CancellationToken,
    config: Option<&crate::config::SternaConfig>,
) -> AgentResult {
    run_watched(
        profile,
        session,
        task,
        options,
        token,
        config,
        Watch::default(),
    )
}

/// [`run_with_config`] watched: publishing its progress, writing its own
/// rollout, and reading what a person says to it. `bg` is the caller that
/// holds the other end of all three.
#[allow(clippy::too_many_arguments)]
pub fn run_watched(
    profile: &Profile,
    session: &SessionId,
    task: &str,
    options: &AgentOptions,
    token: &CancellationToken,
    config: Option<&crate::config::SternaConfig>,
    watch: Watch<'_>,
) -> AgentResult {
    run_narrowed_metered(
        profile,
        session,
        task,
        options,
        token,
        NarrowedRun {
            narrowed: None,
            helper_usage: None,
            config,
            watch,
        },
    )
}

/// The same loop, optionally narrowed to one helper's toolset and preamble.
///
/// **The profile is the parent's, cloned and not recompiled.** A subagent that
/// compiled its own profile could differ from its parent's by a file edited
/// mid-session, which is a widening no one asked for; `sandbox-grants.md` §1.5
/// computes a profile once per session and this honours that across the nested
/// loop too.
pub fn run_narrowed(
    profile: &Profile,
    session: &SessionId,
    task: &str,
    options: &AgentOptions,
    token: &CancellationToken,
    narrowed: Option<&Narrowed>,
) -> AgentResult {
    run_narrowed_metered(
        profile,
        session,
        task,
        options,
        token,
        NarrowedRun::ordinary(narrowed),
    )
}

/// Helper-only metered form of [`run_narrowed`]. Keeping the tracker out of
/// the public call preserves the ordinary subagent API and prevents callers
/// from having to construct Sterna's private accounting state.
pub(crate) fn run_narrowed_metered(
    profile: &Profile,
    session: &SessionId,
    task: &str,
    options: &AgentOptions,
    token: &CancellationToken,
    narrowed_run: NarrowedRun<'_>,
) -> AgentResult {
    let NarrowedRun {
        narrowed,
        helper_usage,
        config,
        watch,
    } = narrowed_run;
    let Watch {
        progress,
        inbox,
        record,
    } = watch;
    let tools = toolset(narrowed);
    let mut facts = crate::session::session_facts(profile);
    facts.interface = crate::abi::Interface::Cells;
    let instructions = instructions_for(narrowed, &crate::project::instructions::root(profile));
    // One value decides both what the context binds and what it is told it
    // binds: a helper never receives `bg`, `send` or `mcp`, because none of
    // the three is a tool and narrowing `spec.tools` therefore left every one
    // of them installed.
    let globals = match narrowed {
        Some(narrowed) => HostGlobals::Helper(narrowed.tools),
        None => HostGlobals::Every,
    };
    let mut system = prompt::render_system_for(&instructions, &tools, &facts, globals);
    system.push_str("\n\n");
    system.push_str(&crate::project::orientation::collect(profile));
    let mut conversation = Conversation {
        system,
        messages: vec![Message::text(Role::User, task)],
    };
    // Opened before the first request so a person who attaches during turn
    // one finds the system block and the task already written.
    let mut journal = Journal::open(record, &conversation.system);

    let mut runtime = match globals {
        HostGlobals::Helper(tools) => Runtime::for_helper(profile, session, tools),
        HostGlobals::Every => Runtime::new(profile, session),
    }
    .as_subagent()
    .with_token(token.clone())
    .with_instruction_context();
    if narrowed.is_none() {
        // A subagent completes a goal, so it hits the same walls the task
        // model does and gets the same helpers; a narrowed loop is itself a
        // helper and gets none, which is what makes it a leaf. Session-started
        // agents inherit the effective configuration snapshot, including named
        // overlays and brokered web policy. Legacy direct callers without a
        // snapshot retain project-config loading with fail-closed defaults.
        let config = config.cloned().unwrap_or_else(|| {
            crate::config::SternaConfig::load(profile.root()).unwrap_or_default()
        });
        runtime = match runtime.with_config(config) {
            Ok(runtime) => runtime,
            Err(error) => return finish(&error, "failed", 0, 0, Vec::new()),
        };
    }
    let mut tokens = 0u64;
    let mut trajectory: Vec<String> = Vec::new();
    let started = std::time::Instant::now();
    let mut turn = 0u64;

    loop {
        turn += 1;
        // The three ways this loop ends without an answer of its own, asked
        // before the turn is paid for. Each salvages the subagent's last
        // words: work it did is never thrown away for the sake of a number.
        if let Some(allowed) = options.turns
            && turn > allowed
        {
            runtime.end_task();
            journal.write(&conversation);
            return finish(
                &salvage(&conversation),
                "turns",
                turn - 1,
                tokens,
                trajectory,
            );
        }
        if let Some(reason) = stopped_by(options, started, token) {
            runtime.end_task();
            journal.write(&conversation);
            return finish(
                &salvage(&conversation),
                reason,
                turn - 1,
                tokens,
                trajectory,
            );
        }
        // **The turn boundary is where a person reaches it.** Anything said
        // while the last turn ran becomes an ordinary user message here, so
        // the subagent reads it with its own work in front of it and the
        // record shows what it was told and when.
        for told in take_told(inbox) {
            conversation.messages.push(Message::text(Role::User, told));
        }
        journal.write(&conversation);
        note_progress(progress, turn, &trajectory);
        // Sent exactly as the subagent read it: append-only, like the
        // session's own requests (`prompt::with_task_context`).
        let request = conversation.clone();
        // A narrowed loop is a helper: its provider request can outlive the
        // caller that stopped waiting, so it keeps a hard bound on the wire.
        //
        // **That bound is silence, not duration.** It used to be
        // `SIDE_ERRAND_TIMEOUT` on a non-streamed request, which is a
        // whole-answer ceiling — and a whole-answer ceiling cannot tell a
        // model that is thinking from a socket that has died, so it lands on
        // the thinking model. Measured 2026-09-19: `CHECKER` died at exactly
        // 120s with its answer still arriving, and the miss it ran to catch
        // shipped. `wire::send_turn_streaming_on` bounds the same request by
        // the gap between events, and `SIDE_ERRAND_BACKSTOP` still ends the
        // thread the callback cannot reach.
        //
        // The task path is untouched: a person is watching it and stops it.
        let purpose = narrowed.map(|_| crate::helpers::PURPOSE_HEADER);
        if let Some(usage) = helper_usage {
            usage.begin_request();
        }
        let sent = match if narrowed.is_some() {
            wire::send_narrowed_turn_streaming(
                &request,
                &options.model,
                options.effort,
                purpose,
                wire::Surface::cells(),
            )
        } else {
            wire::send_turn_bounded_with(&request, &options.model, options.effort, None, purpose)
        } {
            Ok(sent) => sent,
            Err(error) => {
                journal.write(&conversation);
                return finish(&error.to_string(), "failed", turn, tokens, trajectory);
            }
        };
        if let Some(usage) = helper_usage {
            usage.record_response(sent.usage);
        }
        // A provider response can race the caller's cancellation. Do not let
        // that late response start one of the helper's read tools. The turn's
        // own message is already in hand, so the salvage includes it.
        if let Some(reason) = stopped_by(options, started, token) {
            conversation.messages.push(sent.message);
            journal.write(&conversation);
            return finish(
                &salvage(&conversation),
                reason,
                turn - 1,
                tokens,
                trajectory,
            );
        }
        if let Some(usage) = &sent.usage {
            tokens = tokens.saturating_add(usage.total_tokens());
        }
        let text = message_text(&sent.message);
        let calls: Vec<_> = sent
            .message
            .content
            .iter()
            .filter_map(|block| match block {
                crate::contract::Block::ToolUse { id, name, input } => {
                    Some((id.clone(), name.clone(), input.clone()))
                }
                _ => None,
            })
            .collect();
        if text.trim().is_empty() && calls.is_empty() {
            journal.write(&conversation);
            return finish(
                "the model returned an empty reply",
                "failed",
                turn,
                tokens,
                trajectory,
            );
        }
        conversation.messages.push(sent.message);

        let native = calls.first().cloned();
        if calls.len() > 1 || native.as_ref().is_some_and(|call| call.1 != "execute_cell") {
            let explanation = if calls.len() > 1 {
                "ProtocolError: exactly one execute_cell call is allowed; nothing ran."
            } else {
                "ProtocolError: unknown tool call; nothing ran."
            };
            conversation.messages.push(Message {
                role: Role::User,
                content: calls
                    .iter()
                    .map(|(id, _, _)| crate::contract::Block::ToolResult {
                        tool_use_id: id.clone(),
                        content: explanation.into(),
                        is_error: true,
                    })
                    .collect(),
                historical: None,
            });
            continue;
        }
        // A subagent is declared the same tool as its parent, so it may send
        // the same `description` argument; it is accepted here and carried on
        // the record for the same two readers (`legibility.md` §2).
        let described = native.as_ref().and_then(|(_, _, input)| {
            input
                .as_object()
                .and_then(|object| object.get("description"))
                .and_then(serde_json::Value::as_str)
                .and_then(crate::prompt::bound_description)
        });
        let described = described.or_else(|| {
            native
                .is_none()
                .then(|| crate::prompt::descriptor_of(&text))
                .flatten()
        });
        let program = if let Some((id, _, input)) = &native {
            match input
                .as_object()
                .filter(|object| {
                    object
                        .keys()
                        .all(|key| key == "code" || key == "description")
                })
                .and_then(|object| object.get("code"))
                .and_then(serde_json::Value::as_str)
            {
                Some(code) => code.to_string(),
                None => {
                    conversation.messages.push(Message::tool_result(
                        id.clone(),
                        "ProtocolError: execute_cell input must be {\"code\": string, \"description\": string}; nothing ran.",
                        true,
                    ));
                    continue;
                }
            }
        } else {
            match prompt::extract_program(&text) {
                Extracted::Program(source) => source,
                Extracted::Edit(json) => {
                    match runtime
                        .syntax_failure()
                        .ok_or_else(|| {
                            "No syntax-failed cell is available in this task.".to_string()
                        })
                        .and_then(|failed| failed.apply(&json))
                    {
                        Ok(source) => source,
                        Err(error) => {
                            let hint = runtime
                                .syntax_failure()
                                .map(|failed| failed.hint())
                                .unwrap_or_default();
                            conversation.messages.push(Message::text(
                                Role::User,
                                format!("CellEditError: {error} Nothing ran.\n{hint}"),
                            ));
                            continue;
                        }
                    }
                }
                Extracted::Prose => {
                    if let Some(answer) = prompt::completion_text(&text) {
                        runtime.end_task();
                        journal.write(&conversation);
                        return finish(&answer, "returned", turn, tokens, trajectory);
                    }
                    conversation
                        .messages
                        .push(Message::text(Role::User, prompt::CONTINUE_WORK));
                    continue;
                }
                Extracted::Invalid(error) => {
                    conversation.messages.push(Message::text(
                        Role::User,
                        format!("ProtocolError: {error} Nothing ran."),
                    ));
                    continue;
                }
                Extracted::TwoBlocks => {
                    conversation.messages.push(Message::text(Role::User,
                    "Mixed or multiple sterna-edit blocks are ambiguous. Send one repair or ordinary Sterna code; nothing ran."));
                    continue;
                }
            }
        };

        let outcome = runtime.run_cell(&program);
        // The runtime records the program; this loop saw the message, so the
        // descriptor is attached here — before the journal writes the record.
        let mut cell_record = outcome.turn().record.clone();
        cell_record.description = described.clone();
        journal.cell(&cell_record);
        // What this turn actually reached for, in order. Tool names only.
        trajectory.extend(
            outcome
                .turn()
                .record
                .calls
                .iter()
                .map(|call| call.tool.clone()),
        );
        note_progress(progress, turn, &trajectory);
        let instruction_boundary = runtime.pending_instructions();
        if let Some(pending) = &instruction_boundary {
            conversation.system.push_str("\n\n");
            conversation.system.push_str(&pending.text);
        }
        // A subagent ends the same way the person's task does: by saying so
        // with `answer(text)`. Reading the ending off a returned value's type
        // is the mistake `outcome.rs::ends_the_task` records.
        if outcome.ends_the_task()
            && let Some(answer) = outcome.answer().map(str::to_string)
        {
            let helpers = runtime.helper_records();
            if let Some(handoff) = checker_handoff(&helpers, &answer) {
                // A return written before the checker answered cannot have
                // evaluated that answer, even though JavaScript awaited the
                // call before reaching `return`. Preserve both pieces as an
                // observation and give the parent model one later turn to
                // decide what the evidence means. The checker prose is data:
                // no verdict spelling is parsed and no outcome is promoted to
                // approval by the host.
                let result = result_message(&outcome, turn, described.clone());
                let mut full = prompt::render_result(&result);
                full.push_str(&handoff);
                let mut historical = prompt::render_result_history(&result);
                historical.push_str(&handoff);
                if let Some((id, _, _)) = &native {
                    conversation.messages.push(Message::runtime_tool_result(
                        id.clone(),
                        full,
                        false,
                        historical,
                    ));
                } else {
                    conversation
                        .messages
                        .push(Message::runtime(full, historical));
                }
                if let Some(pending) = instruction_boundary {
                    if pending.fatal {
                        runtime.end_task();
                        journal.write(&conversation);
                        return finish(&pending.text, "failed", turn, tokens, trajectory);
                    }
                    runtime.acknowledge_instructions();
                }
                continue;
            }
            if let Some((id, _, _)) = &native {
                let result = result_message(&outcome, turn, described.clone());
                let mut feedback = prompt::render_result(&result);
                feedback.push_str("\n\n## Return\n");
                feedback.push_str(&answer);
                conversation
                    .messages
                    .push(Message::tool_result(id.clone(), feedback, false));
            }
            runtime.end_task();
            journal.write(&conversation);
            return finish(&answer, "returned", turn, tokens, trajectory);
        }
        let result = result_message(&outcome, turn, described.clone());
        let mut full = prompt::render_result(&result);
        let mut historical = prompt::render_result_history(&result);
        if let Some(failed) = runtime.syntax_failure() {
            for text in [&mut full, &mut historical] {
                text.push_str("\n\n");
                text.push_str(&failed.hint());
            }
        }
        if let Some((id, _, _)) = &native {
            let message = Message::runtime_tool_result(
                id.clone(),
                full,
                matches!(outcome, CellOutcome::Threw { .. }),
                historical,
            );
            conversation.messages.push(message);
        } else {
            conversation
                .messages
                .push(Message::runtime(full, historical));
        }
        if let Some(pending) = instruction_boundary {
            if pending.fatal {
                runtime.end_task();
                journal.write(&conversation);
                return finish(&pending.text, "failed", turn, tokens, trajectory);
            }
            runtime.acknowledge_instructions();
        }
    }
}

/// The deterministic boundary between a checker call and accepting a parent
/// completion. It carries the returned candidate and the helper's unparsed
/// outcome into the next provider request; neither can be silently dropped or
/// treated as approval by host-side prose matching.
pub(crate) fn checker_handoff(
    helpers: &[crate::helpers::HelperRecord],
    candidate: &str,
) -> Option<String> {
    let checkers: Vec<_> = helpers
        .iter()
        .filter(|record| record.helper == "check")
        .collect();
    if checkers.is_empty() {
        return None;
    }
    let mut handoff = format!("\n\n## Candidate completion (deferred)\n{candidate}");
    for (index, checker) in checkers.iter().enumerate() {
        let status = if checker.outcome.cancelled {
            "cancelled"
        } else if checker.outcome.ok {
            "returned"
        } else {
            "failed"
        };
        handoff.push_str(&format!(
            "\n\n## Checker observation {} ({status})\n{}",
            index + 1,
            checker.outcome.text
        ));
    }
    handoff.push_str(
        "\n\nA checker ran in the same cell as this candidate. Evaluate every observation and \
         the candidate in this subsequent turn before deciding whether to return, revise, or \
         continue working.",
    );
    Some(handoff)
}

/// The tools this loop is declared, by name.
///
/// The invariant: **a narrowed loop is declared only what its spec names.** A
/// name that resolves to nothing is dropped rather than substituted, and
/// `helpers::check_spec` has already refused such a name at startup.
fn toolset(narrowed: Option<&Narrowed>) -> Vec<&'static registry::Tool> {
    match narrowed {
        None => registry::ALL.iter().collect(),
        Some(narrowed) => narrowed
            .tools
            .iter()
            .filter_map(|name| registry::lookup(name))
            .collect(),
    }
}

/// What this loop opens with: a helper's own preamble and the roster's shared
/// haste line, or the subagent instructions, then the project's own.
///
/// **"Be quick" is an instruction here, not a ceiling elsewhere.** The user's
/// ruling of 2026-09-17 removed the global cap that used to enforce it, so
/// this line is what remains of it — and it is the half that can say *why*.
fn instructions_for(narrowed: Option<&Narrowed>, project: &str) -> String {
    match narrowed {
        Some(narrowed) => format!(
            "{}\n\n{}\n\n{project}",
            narrowed.instructions,
            crate::helpers::HELPER_HASTE
        ),
        None => format!("{SUBAGENT_INSTRUCTIONS}\n\n{project}"),
    }
}

/// A subagent's own rollout, written as the loop goes.
///
/// **The record is written at the turn boundary, not at the end.** The whole
/// point is that a person can read a subagent while it works, so every
/// message the conversation gained since the last look is appended before the
/// next request leaves -- and again before the loop returns, so a subagent
/// that stopped early has its last words on disk as well as in its answer.
///
/// **A broken record never stops the work.** Every failure here -- a
/// directory that cannot be created, a disk that is full, a line that will
/// not serialise -- leaves `file` as `None` and the loop runs on. A subagent
/// interrupted because nobody could write down what it was doing would be a
/// worse trade than a missing record.
struct Journal {
    file: Option<crate::rollout::Rollout>,
    /// How many of the conversation's messages are already on disk.
    written: usize,
}

impl Journal {
    fn open(record: Option<&AgentRollout>, system: &str) -> Self {
        let file = record.and_then(|record| {
            let parent = record.path.parent()?;
            std::fs::create_dir_all(parent).ok()?;
            crate::rollout::Rollout::create(&record.path, record.id.clone(), system).ok()
        });
        Self { file, written: 0 }
    }

    /// Appends every message the conversation has gained since the last call.
    fn write(&mut self, conversation: &Conversation) {
        let Some(file) = self.file.as_mut() else {
            self.written = conversation.messages.len();
            return;
        };
        for message in conversation.messages.iter().skip(self.written) {
            let _ = file.record_message(message);
        }
        self.written = conversation.messages.len();
    }

    /// Appends one cell's record, which advances no turn number.
    fn cell(&mut self, record: &crate::runtime::outcome::CellRecord) {
        if let Some(file) = self.file.as_mut() {
            let _ = file.record_cell(record);
        }
    }
}

/// What a person has said to this subagent since the last turn, taken from
/// the inbox so nothing is delivered twice.
///
/// A poisoned lock is recovered from rather than propagated, for
/// [`note_progress`]'s reason: a message must never be able to end the
/// subagent it was meant for.
fn take_told(inbox: Option<&InboxSink>) -> Vec<String> {
    let Some(inbox) = inbox else {
        return Vec::new();
    };
    let mut held = inbox
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::take(&mut *held)
}

/// Publishes what the loop has done so far, for a parent that looks in.
///
/// A poisoned lock is recovered from rather than propagated: progress is a
/// convenience for the parent and must never be able to end the subagent
/// whose work it reports.
fn note_progress(progress: Option<&ProgressSink>, turn: u64, trajectory: &[String]) {
    let Some(sink) = progress else { return };
    let mut held = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    held.turns = turn;
    held.calls = trajectory.to_vec();
}

/// Whether the wall clock this subagent was given has run out.
///
/// `bg` arms the same duration as the job's deadline, which is what stops a
/// hung provider call; this is the half the loop can see, so the parent is
/// told *time ran out* rather than *cancelled*.
fn ran_out_of_time(options: &AgentOptions, started: std::time::Instant) -> bool {
    options
        .deadline
        .is_some_and(|deadline| started.elapsed() >= deadline)
}

/// Why this loop must stop now, or `None` to take another turn.
///
/// **The deadline is asked first, and that ordering is the attribution.**
/// `bg::arm_deadline` enforces a deadline by cancelling the job's token, so
/// an expiry and a `/stop` would otherwise arrive at this loop as the same
/// flag. The token says which it was — the canceller knows, and this loop's
/// own clock cannot: it starts a thread-spawn after the one the deadline was
/// armed from, so it under-reports elapsed time by that gap and read a
/// genuine expiry as a bare cancellation whenever the expiry landed inside
/// it. The local clock stays as the answer for a deadline this loop passed
/// before the arming thread woke up. Both call sites ask through here so the
/// two cannot answer differently.
fn stopped_by(
    options: &AgentOptions,
    started: std::time::Instant,
    token: &CancellationToken,
) -> Option<&'static str> {
    if token.timed_out() || ran_out_of_time(options, started) {
        return Some("deadline");
    }
    token.is_cancelled().then_some("cancelled")
}

/// The subagent's own last words, for a stop it did not choose.
///
/// **A subagent that stops early returns what it produced.** Until
/// 2026-09-17 every early exit answered `"the subagent used every turn it was
/// given without returning"` and dropped the work: measured that day, three
/// subagents came back with an empty answer and the parent started the same
/// doomed delegation twice more. The prose of each turn is already in the
/// conversation; this is where it is read back out.
fn salvage(conversation: &Conversation) -> String {
    let words = conversation
        .messages
        .iter()
        .rev()
        .take(SALVAGE_DEPTH)
        .find(|message| message.role == Role::Assistant)
        .map(message_text)
        .unwrap_or_default();
    bound(words.trim())
}

/// [`SALVAGED_ANSWER_BYTES`] of `text`, cut at a character boundary and said
/// to be cut. The head, not the tail: prose answers state their finding
/// first, and a reader who needs the rest can ask the subagent again.
fn bound(text: &str) -> String {
    if text.len() <= SALVAGED_ANSWER_BYTES {
        return text.to_string();
    }
    let mut cut = SALVAGED_ANSWER_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… [truncated]", &text[..cut])
}

fn finish(
    answer: &str,
    status: &str,
    turns: u64,
    tokens: u64,
    trajectory: Vec<String>,
) -> AgentResult {
    AgentResult {
        answer: answer.to_string(),
        status: status.to_string(),
        turns,
        tokens,
        trajectory,
    }
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            crate::contract::Block::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// The subagent's own result message, which is the parent's renderer with no
/// usage line: a subagent has no token budget of its own, and a figure it
/// cannot act on is prompt it pays for.
fn result_message(outcome: &CellOutcome, cell: u64, description: Option<String>) -> CellResult {
    let turn = outcome.turn();
    let error = match outcome {
        CellOutcome::Threw { error, .. } => Some(ErrorSection {
            class: error.class.clone(),
            message: error.message.clone(),
            position: error
                .line
                .zip(error.column)
                .map(|(line, column)| (u64::from(line), u64::from(column))),
            frames: Vec::new(),
        }),
        _ => None,
    };
    CellResult {
        // A subagent has nobody at the keyboard, so `ask` is refused at the
        // call and no answer can ever reach this result.
        ask_answer: None,
        cell,
        elapsed_ms: turn.elapsed_ms,
        // A subagent's own result carries the descriptor its turn supplied,
        // so a subagent that said what it was doing reads the same way its
        // parent does after compaction.
        description,
        error,
        yield_reason: turn.yield_reason.clone(),
        output: match outcome {
            // A subagent's window is not metered here, so its return gets
            // the unknown budget rather than none.
            CellOutcome::Returned { terminal, .. } => Some(
                terminal
                    .render_within(prompt::RETURN_BUDGET_UNKNOWN as usize)
                    .text,
            ),
            _ => None,
        },
        handle_table: turn.table.clone(),
        stdout_tail: (!turn.stdout_tail.is_empty()).then(|| turn.stdout_tail.clone()),
        budget: Budget {
            turn_cap: 0,
            task_used: 0,
            task_cap: 0,
            cells_used: cell,
            cells_cap: None,
            feedback: None,
        },
        plan: turn.plan.clone(),
    }
}

/// What a subagent is told about itself, appended to the ordinary system
/// block. It is short on purpose: everything else it needs is the same
/// contract its parent works under.
const SUBAGENT_INSTRUCTIONS: &str = "You are a subagent. Another session asked you one question and is waiting \
for the answer; there is no person here to ask for more.\n\n\
Return the answer as a string with a top-level `return`, and return as soon as \
you have it — the session that started you is waiting and pays for your work. \
Nothing counts your turns, so take the ones the work needs and no more. You \
have no inbox, no messages, and you cannot start a subagent of your own. If the \
question cannot be answered with the grant you have, return that plainly \
instead of working around it.\n\n\
If you are stopped before you return — time runs out, or the session cancels \
you — your last message is what reaches the parent, so keep it worth reading: \
say what you have established and what is left.";

#[cfg(test)]
mod tests {
    use super::*;

    /// `little-helpers.md`'s first build cost: the tool list comes from the
    /// caller instead of `registry::ALL`. The mutating tools are the point --
    /// a narrowed loop is never declared them, so "a helper never writes" is
    /// a list it does not have rather than a rule it might disregard.
    #[test]
    fn a_narrowed_loop_is_declared_only_the_tools_it_named() {
        let every: Vec<&str> = toolset(None).iter().map(|tool| tool.name()).collect();
        assert_eq!(
            every.len(),
            registry::ALL.len(),
            "a subagent still holds every registered tool: {every:?}"
        );

        let scout = Narrowed {
            tools: &["read", "grep"],
            instructions: "",
        };
        let named: Vec<&str> = toolset(Some(&scout))
            .iter()
            .map(|tool| tool.name())
            .collect();
        assert_eq!(named, ["read", "grep"], "in the order the spec named them");
        for forbidden in crate::helpers::FORBIDDEN_TOOLS {
            assert!(
                !named.contains(&forbidden),
                "a narrowed loop must not be declared `{forbidden}`"
            );
        }
    }

    /// A helper is *told* to move quickly now that nothing caps it, and a
    /// subagent is told the opposite thing about its turns: take what the
    /// work needs. Both sentences are load-bearing, so both are pinned.
    #[test]
    fn a_helper_is_instructed_to_be_quick_and_a_subagent_is_not_counted() {
        let spec = Narrowed {
            tools: &["read"],
            instructions: "You find things.",
        };
        let helper = instructions_for(Some(&spec), "PROJECT");
        assert!(helper.starts_with("You find things."), "{helper}");
        assert!(helper.contains(crate::helpers::HELPER_HASTE), "{helper}");
        assert!(helper.ends_with("PROJECT"), "{helper}");

        let subagent = instructions_for(None, "PROJECT");
        assert!(subagent.contains("Nothing counts your turns"), "{subagent}");
        assert!(
            !subagent.contains(crate::helpers::HELPER_HASTE),
            "a subagent owns a goal, not a question: {subagent}"
        );
    }

    /// The salvage is the whole of the user's objection to a cap: whatever
    /// stops a subagent, its own last words are the answer. Before
    /// 2026-09-17 every early exit answered with a sentence about having
    /// returned nothing and dropped the work.
    #[test]
    fn an_early_stop_answers_with_the_subagents_own_last_words() {
        let conversation = Conversation {
            system: String::new(),
            messages: vec![
                Message::text(Role::User, "the task"),
                Message::text(Role::Assistant, "I found the parser in config.rs."),
                Message::runtime("a cell result", "a cell result"),
            ],
        };
        assert_eq!(salvage(&conversation), "I found the parser in config.rs.");

        // Nothing said yet is empty, not a sentence claiming there was
        // nothing: the note on `stderr` is what says why it stopped.
        let silent = Conversation {
            system: String::new(),
            messages: vec![Message::text(Role::User, "the task")],
        };
        assert_eq!(salvage(&silent), "");
    }

    /// A long draft is cut and says so, rather than being dropped whole.
    #[test]
    fn a_long_last_message_is_bounded_and_marked() {
        let long = "x".repeat(SALVAGED_ANSWER_BYTES + 500);
        let bounded = bound(&long);
        assert!(bounded.len() < long.len());
        assert!(bounded.ends_with("… [truncated]"), "{}", &bounded[..40]);
        assert_eq!(bound("short"), "short");
    }

    /// The wall clock is the person's to set, and absent by default: a
    /// subagent doing correct work for an hour is not interrupted by a
    /// number nobody chose (user, 2026-09-17).
    #[test]
    fn a_subagent_with_no_configured_deadline_is_not_time_bounded() {
        let unbounded = AgentOptions {
            turns: None,
            model: "m".into(),
            effort: Effort::default(),
            deadline: None,
        };
        let long_ago = std::time::Instant::now() - std::time::Duration::from_secs(3600);
        assert!(
            !ran_out_of_time(&unbounded, long_ago),
            "an hour of correct work is not a reason to stop"
        );
        assert_eq!(crate::config::AgentsConfig::default().deadline, None);

        let bounded = AgentOptions {
            deadline: Some(std::time::Duration::from_millis(1)),
            ..unbounded
        };
        assert!(ran_out_of_time(&bounded, long_ago));
    }

    /// **A deadline arrives as a cancellation, and must not read as one.**
    /// `bg::arm_deadline` enforces the clock by cancelling the job's token,
    /// so by the time the loop looks, both are true; asking the clock first
    /// is what tells the parent to give the next subagent longer instead of
    /// leaving it to guess. The ordering is invisible to an end-to-end test
    /// -- the loop usually notices its own clock before the deadline thread
    /// fires -- so it is pinned here, where both can be true at once.
    #[test]
    fn a_deadline_that_cancelled_the_token_still_reads_as_the_deadline() {
        let long_ago = std::time::Instant::now() - std::time::Duration::from_secs(3600);
        let timed = AgentOptions {
            turns: None,
            model: "m".into(),
            effort: Effort::default(),
            deadline: Some(std::time::Duration::from_millis(1)),
        };
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(stopped_by(&timed, long_ago, &token), Some("deadline"));

        // With no clock configured, the same cancelled token is what it is.
        let untimed = AgentOptions {
            deadline: None,
            ..timed
        };
        assert_eq!(stopped_by(&untimed, long_ago, &token), Some("cancelled"));

        // And a healthy loop is stopped by neither.
        let fresh = CancellationToken::new();
        assert_eq!(
            stopped_by(&untimed, std::time::Instant::now(), &fresh),
            None
        );
    }

    /// The race the Linux sweep found on 2026-09-18: `arm_deadline`'s clock
    /// starts a thread-spawn before this loop's, so an expiry can arrive
    /// while the loop's own clock still has time left. The reason comes off
    /// the token, so the gap no longer decides what the parent is told.
    #[test]
    fn an_expiry_this_loops_own_clock_has_not_reached_is_still_a_deadline() {
        let timed = AgentOptions {
            turns: None,
            deadline: Some(std::time::Duration::from_secs(3600)),
            model: "m".to_string(),
            effort: crate::wire::Effort::default(),
        };
        let token = CancellationToken::new();
        token.cancel_for_deadline();
        // The loop started just now: `ran_out_of_time` is false by an hour.
        assert_eq!(
            stopped_by(&timed, std::time::Instant::now(), &token),
            Some("deadline")
        );

        // A `/stop` on the same job is still a plain cancellation.
        let stopped = CancellationToken::new();
        stopped.cancel();
        assert_eq!(
            stopped_by(&timed, std::time::Instant::now(), &stopped),
            Some("cancelled")
        );
    }
}
