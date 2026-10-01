//! The task's running state: its spend, its capsule and progress guard, the
//! evidence gate and salvage, and the partial-effects sentence (moved out of
//! `session.rs` for the Phase 59 size ratchet, 2026-09-13; nothing here is new).

use super::*;

/// The task's cumulative token spend and executed-cell count, and where each
/// turn's figure came from. Spend is telemetry only; only the configured cell
/// count remains a control limit.
pub(super) struct TaskSpend {
    pub(super) parent_used: u64,
    pub(super) cells_used: u64,
    pub(super) reported: bool,
    pub(super) estimated: bool,
    pub(super) cells_cap: Option<u64>,
    /// The runtime's cumulative reduction ledger as last reported, so each
    /// frame's telemetry carries only what that frame added.
    pub(super) reductions_seen: crate::runtime::observation::ReductionStats,
    /// How many estimated tokens a returned value may fill this turn, set
    /// from the context meter before each cell (`context::return_budget`).
    pub(super) return_budget: u64,
    /// What the last cell's return took of it, and the fields paged to fit.
    pub(super) last_return: Option<(u64, Vec<String>)>,
    /// Output tokens the providers reported as reasoning, over the task.
    pub(super) reasoned: u64,
}

impl TaskSpend {
    pub(super) fn new(cells_cap: Option<u64>) -> Self {
        Self {
            parent_used: 0,
            cells_used: 0,
            reported: false,
            estimated: false,
            cells_cap,
            reductions_seen: Default::default(),
            return_budget: prompt::RETURN_BUDGET_UNKNOWN,
            last_return: None,
            reasoned: 0,
        }
    }

    /// Adds the reasoning tokens a provider reported for one turn.
    pub(super) fn add_reasoning(&mut self, tokens: Option<u64>) {
        self.reasoned = self.reasoned.saturating_add(tokens.unwrap_or(0));
    }

    /// The turn about to run: its return budget follows the room the meter
    /// measured, so a cell that returns an excerpt is paged against the
    /// window it is actually in and never against a number chosen first;
    /// and a return is this cell's or nobody's, so the last figure is
    /// cleared rather than carried.
    pub(super) fn begin_turn(&mut self, return_budget: u64) {
        self.return_budget = return_budget;
        self.last_return = None;
    }

    /// A returned value for the model, within this turn's return budget:
    /// whole when it fits, paged at a line with a cursor line when it does
    /// not, and the usage line says which. The screen shows the same text.
    pub(super) fn render_return(&mut self, terminal: &crate::runtime::outcome::Terminal) -> String {
        let rendered = terminal.render_within(self.return_budget as usize);
        self.last_return = Some((rendered.tokens as u64, rendered.paged));
        rendered.text
    }

    /// §6's return figures for the usage line.
    pub(super) fn return_usage(&self) -> prompt::ReturnUsage {
        prompt::ReturnUsage {
            budget: self.return_budget,
            this_return: self.last_return.clone(),
        }
    }

    /// What the runtime's reduction ledger gained since the last frame.
    pub(super) fn reduction_delta(
        &mut self,
        current: crate::runtime::observation::ReductionStats,
    ) -> crate::runtime::observation::ReductionStats {
        let seen = std::mem::replace(&mut self.reductions_seen, current);
        crate::runtime::observation::ReductionStats {
            ruled: current.ruled.saturating_sub(seen.ruled),
            bytes_in: current.bytes_in.saturating_sub(seen.bytes_in),
            bytes_out: current.bytes_out.saturating_sub(seen.bytes_out),
        }
    }

    /// Adds one turn's cost: the gateway's own usage row when it reported
    /// one, else the Messages response's own `usage`, else `estimate`.
    ///
    /// **Which source was used is recorded, not averaged.** §6 reads a
    /// provider's figure "rather than estimated", and a total that quietly
    /// mixed a measurement with a heuristic would be a number the sidebar
    /// could not honestly label. The gateway's row is preferred over the
    /// response's own `usage` when both are present, because it is what
    /// `served_by` was built to make authoritative -- but the sidebar calls
    /// either one `reported`: a reader deciding whether to trust this figure
    /// only needs to know it did not come from `estimate_tokens`.
    pub(super) fn add(&mut self, served: &ServedBy, usage: Option<&wire::Usage>, estimate: u64) {
        match (served.input_tokens, served.output_tokens) {
            (None, None) => match usage {
                Some(usage) => {
                    self.parent_used = self.parent_used.saturating_add(usage.total_tokens());
                    self.reported = true;
                }
                None => {
                    self.parent_used = self.parent_used.saturating_add(estimate);
                    self.estimated = true;
                }
            },
            (input, output) => {
                self.parent_used = self
                    .parent_used
                    .saturating_add(input.unwrap_or(0))
                    .saturating_add(output.unwrap_or(0))
                    .saturating_add(
                        served
                            .cached_input_tokens
                            .or_else(|| usage.and_then(|row| row.cache_read_input_tokens))
                            .unwrap_or(0),
                    )
                    .saturating_add(
                        usage
                            .and_then(|row| row.cache_creation_input_tokens)
                            .unwrap_or(0),
                    );
                self.reported = true;
            }
        }
    }

    pub(super) fn used(&self) -> u64 {
        self.parent_used
    }

    pub(super) fn counted(&self) -> Option<Counted> {
        match (self.reported, self.estimated) {
            (true, true) => Some(Counted::Mixed),
            (true, false) => Some(Counted::Gateway),
            (false, true) => Some(Counted::Estimated),
            (false, false) => None,
        }
    }

    /// §6's own line, for the result block the model reads next.
    ///
    /// **The cap named here is the one the request actually carried.** It
    /// used to be `wire::MAX_TOKENS` whatever the turn asked for, and once
    /// per-model maxima landed the two parted company: a session on a model
    /// publishing 128,000 sent 128,000 and told the model it had 8,192, so
    /// the model would have cut its own work to a sixteenth of the room it
    /// had. `the_usage_line_names_the_max_tokens_actually_sent` pins the
    /// agreement rather than either figure.
    pub(super) fn line(&self, model: &str) -> Budget {
        Budget {
            turn_cap: u64::from(wire::max_tokens_for(model)),
            task_used: self.used(),
            // Kept in the wire-facing value for API compatibility. The
            // renderer deliberately ignores it: task spend has no cap.
            task_cap: 0,
            cells_used: self.cells_used,
            cells_cap: self.cells_cap,
            feedback: Some(self.return_usage()),
        }
    }

    pub(super) fn tokens(&self) -> Option<TaskTokens> {
        Some(TaskTokens {
            used: self.used(),
            counted: self.counted()?,
            reasoned: self.reasoned,
        })
    }

    /// The cell limit buys exactly one final-answer turn. Token spend is not
    /// consulted here or anywhere else in the task loop.
    /// Whether a ceiling this person set has been reached. No ceiling is no
    /// limit: `None` never ends a task.
    pub(super) fn cell_limit_reached(&self) -> bool {
        self.cells_cap.is_some_and(|cap| self.cells_used >= cap)
    }
}

/// What one cell's observation added to the next turn's feedback.
#[derive(Default)]
pub(super) struct Observed {
    /// Lines every form of the feedback carries — a no-progress notice.
    pub(super) notices: Vec<String>,
    /// The `## Task` block for the live feedback only: state the next
    /// request replaces, never history.
    pub(super) capsule_block: Option<String>,
}

/// Everything the task loop learns across cells that a terminal return is
/// judged against — `smarter-cheaper-roadmap.md`'s *Structured task capsule*,
/// *Evidence-gated completion*, *No-progress guard*, *Verified checkpoint*
/// and *Cut-off salvage* rows.
///
/// The invariant: **every field is derived from the trajectory and the
/// tree, never from the model's narrative.** The capsule's facts cite cells,
/// the checkpoints hold tree digests, and the gate's findings come from the
/// filesystem the task changed.
pub(super) struct TaskState {
    pub(super) task: String,
    /// A `shadow` task decision still on its way (`system::task_decision`).
    pub(super) pending_decision: Option<super::system::PendingDecision>,
    pub(super) capsule: crate::runtime::capsule::Capsule,
    pub(super) guard: crate::progress::Guard,
    pub(super) checkpoints: crate::progress::Checkpoints,
    pub(super) files: crate::completion::TaskFiles,
    pub(super) task_start: crate::changes::Snapshot,
    pub(super) last_verification_cell: Option<u64>,
    pub(super) last_mutation_cell: Option<u64>,
    pub(super) tree_digest: Option<String>,
    pub(super) deferred_findings: Option<Vec<String>>,
    pub(super) gate_deferrals: u32,
    pub(super) last_capsule_render: String,
    pub(super) previous_frame: Option<CellRecord>,
    pub(super) previous_failed: bool,
    pub(super) evidence_gate: bool,
    /// Cells in a row that changed nothing (`progress::Stall`).
    pub(super) stall: crate::progress::Stall,
    /// The decision model's answer to this task's one intent question, asked
    /// once before the first turn -- `None` when no model is configured, the
    /// mode is off, or the request failed (`decide-model.md`).
    pub(super) intent: Option<crate::decide::Intent>,
    /// The kind answer (2026-09-23), beside the intent.
    pub(super) kind: Option<crate::decide::Kind>,
    /// The effort the kind set for this task, `None` when the person chose
    /// one or the kind did not act; `would_lower` is the shadow reading.
    pub(super) effort_set: Option<String>,
    pub(super) effort_would_set: Option<String>,
    /// How many decision requests this task attempted and did not answer --
    /// the intent question (at most one, before the first turn) and the
    /// completion question (once per distinct diff claimed at the gate;
    /// 2616).
    pub(super) decision_failures: u32,
    /// Effectful cells or frames held this task (`mode = on`); the once rule
    /// is `effect_holds == 0`.
    pub(super) effect_holds: u32,
    /// Effectful cells or frames that ran after an earlier hold this task.
    pub(super) effect_overrides: u32,
    /// Effectful cells or frames `mode = shadow` would have held.
    pub(super) would_hold: u32,
    /// Every field-shape answer this task asked for (`session/returned.rs`),
    /// as `{field, choice, confidence, latency_ms, reduced}`, so the question
    /// can be measured against always and never.
    pub(super) field_shapes: Vec<serde_json::Value>,
    /// The completion question's raw answer, keyed by the exact diff text it
    /// was asked about (2616): `None` when no model is configured, mode is
    /// off, or no request has answered yet. Re-asked whenever the diff at a
    /// later `gate` call differs from the cached key -- an identical second
    /// claim (nothing ran between the hold and the re-claim) reuses the
    /// answer and asks nothing; a changed diff (the model fixed something
    /// and claimed again) is a new question, never the stale answer to a
    /// tree that no longer exists.
    pub(super) completion_answer: Option<(String, crate::decide::CompletionAnswer)>,
    /// What the last `gate` call did with [`Self::completion_answer`] --
    /// `None` until the completion question has been asked.
    pub(super) completion_decision: Option<CompletionTelemetry>,
    /// This task's approval hints (F4, decision-model.md) that answered --
    /// synced from `approval::Gate::hint_counts` each cell, since the hint
    /// itself answers on a thread the approval seam owns, not this struct.
    pub(super) approval_hints: u32,
    /// This task's approval-hint requests that failed or timed out.
    pub(super) approval_hint_failures: u32,
    /// The profile and session a red run executes under.
    profile: Profile,
    session_id: Option<crate::contract::SessionId>,
    /// The red run in flight, and the command and tree it was asked of.
    red_check: Option<super::test_check::RedCheck>,
    red_checked: std::collections::BTreeSet<String>,
}

/// One `gate` call's completion-question telemetry (2616): the cached wire
/// answer plus what that call did with it. Rebuilt on every call from
/// [`TaskState::completion_answer`] and that call's own other findings, so a
/// held candidate's second, identical claim reports the same outcome.
#[derive(Debug, Clone)]
pub(super) struct CompletionTelemetry {
    pub(super) noul: f64,
    pub(super) latency_ms: u64,
    pub(super) truncated: bool,
    pub(super) finding_added: bool,
    /// Which state the satisfaction question was asked over (2641/2642's
    /// addendum to 2616): `"diff"` or `"answer"`.
    pub(super) state: &'static str,
    /// The five diff-hygiene nouls (2641), `None` when `state` is `"answer"`.
    pub(super) hygiene: Option<serde_json::Value>,
    /// How many of the five hygiene questions were decisive and added a
    /// finding (2641).
    pub(super) hygiene_findings: u32,
}

/// The largest file a red run puts back to its task-start bytes.
const RED_FILE_CAP: u64 = 16 * 1024 * 1024;

impl TaskState {
    pub(super) fn new(task: &str, profile: &Profile, config: &SternaConfig) -> Self {
        Self {
            task: task.to_string(),
            capsule: crate::runtime::capsule::Capsule::new(task),
            guard: crate::progress::Guard::new(crate::progress::DEFAULT_THRESHOLD),
            checkpoints: crate::progress::Checkpoints::default(),
            files: crate::completion::TaskFiles::default(),
            task_start: crate::changes::Snapshot::capture(profile),
            last_verification_cell: None,
            last_mutation_cell: None,
            tree_digest: None,
            deferred_findings: None,
            gate_deferrals: 0,
            last_capsule_render: String::new(),
            previous_frame: None,
            previous_failed: false,
            evidence_gate: config.limits.evidence_gate,
            pending_decision: None,
            stall: crate::progress::Stall::default(),
            intent: None,
            kind: None,
            effort_set: None,
            effort_would_set: None,
            decision_failures: 0,
            effect_holds: 0,
            effect_overrides: 0,
            would_hold: 0,
            field_shapes: Vec::new(),
            completion_answer: None,
            completion_decision: None,
            approval_hints: 0,
            approval_hint_failures: 0,
            profile: profile.clone(),
            session_id: None,
            red_check: None,
            red_checked: std::collections::BTreeSet::new(),
        }
    }

    /// The session a red run executes under.
    pub(super) fn with_session(mut self, session: &crate::contract::SessionId) -> Self {
        self.session_id = Some(session.clone());
        self
    }

    /// The decision model's answer to this task's one request, asked before
    /// `TaskState` existed. `None` and `decision_failures` is `1` when the
    /// request was attempted and did not answer.
    pub(super) fn with_decision(
        mut self,
        decision: Option<crate::decide::TaskDecision>,
        decision_failures: u32,
    ) -> Self {
        self.intent = decision.as_ref().map(|decision| decision.intent.clone());
        self.kind = decision.and_then(|decision| decision.kind);
        self.decision_failures = decision_failures;
        self
    }

    /// Folds a `shadow` decision in once it is back -- `wait` bounds it by
    /// the request's own timeout -- with the would-be effects it would have
    /// had, so shadow still measures what `on` would do.
    pub(super) fn settle_decision(&mut self, session: &Session<'_>, wait: bool) {
        let Some(pending) = &self.pending_decision else {
            return;
        };
        let Some(answer) = pending.settle(wait) else {
            return;
        };
        self.pending_decision = None;
        match answer {
            Ok(decision) => {
                let lease = super::system::EffortLease::for_kind(session, Some(&decision));
                self.effort_would_set = lease.would_set.map(|effort| effort.name().to_string());
                self.intent = Some(decision.intent);
                self.kind = decision.kind;
            }
            Err(()) => self.decision_failures += 1,
        }
        output::decisions(self.decisions_telemetry(&session.config().decisions));
    }

    /// What the kind did to this task's effort (2026-09-23), computed
    /// before `TaskState` existed.
    pub(super) fn with_kind_effects(mut self, effort: &super::system::EffortLease<'_, '_>) -> Self {
        self.effort_set = effort.set.map(|effort| effort.name().to_string());
        self.effort_would_set = effort.would_set.map(|effort| effort.name().to_string());
        self
    }

    /// The `Telemetry.decisions` block: `docs/decisions.md`'s
    /// schema, `None` only when no decision model is configured at all.
    pub(super) fn decisions_telemetry(
        &self,
        config: &crate::config::DecisionsConfig,
    ) -> Option<serde_json::Value> {
        let model = config.model.as_deref()?;
        let asked = self.intent.is_some() || self.decision_failures > 0;
        let intent = self.intent.as_ref().map(|intent| {
            serde_json::json!({ "choice": intent.choice, "confidence": intent.confidence })
        });
        let kind = self.kind.as_ref().map(
            |kind| serde_json::json!({ "choice": kind.choice, "confidence": kind.confidence }),
        );
        Some(serde_json::json!({
            "model": model,
            "mode": config.mode.as_str(),
            "asked": u32::from(asked),
            "answered": u32::from(self.intent.is_some()),
            "failed": self.decision_failures,
            "latency_ms_total": self.intent.as_ref().map_or(0, |intent| intent.latency_ms),
            "intent": intent,
            "kind": kind,
            "effort": {
                "set": self.effort_set,
                "would_set": self.effort_would_set,
            },
            "would_hold": self.would_hold,
            "holds": self.effect_holds,
            "overrides": self.effect_overrides,
            "field_shapes": self.field_shapes,
            "approval_hints": self.approval_hints,
            "approval_hint_failures": self.approval_hint_failures,
            "completion": self.completion_decision.as_ref().map(|decision| serde_json::json!({
                "noul": decision.noul,
                "latency_ms": decision.latency_ms,
                "truncated": decision.truncated,
                "finding_added": decision.finding_added,
                "state": decision.state,
                "hygiene": decision.hygiene,
                "hygiene_findings": decision.hygiene_findings,
            })),
        }))
    }

    /// The `/cell` inspector's one line for this task, `None` only when no
    /// decision model is configured at all.
    pub(super) fn decision_line(&self, config: &crate::config::DecisionsConfig) -> Option<String> {
        config.model.as_ref()?;
        Some(crate::decide::summary_line(
            self.intent.as_ref(),
            self.effect_holds,
            self.effect_overrides,
        ))
    }

    /// Why the next parent request is being made, read from the last frame.
    pub(super) fn next_cause(&self) -> crate::abi::telemetry::RequestCause {
        crate::abi::telemetry::request_cause(self.previous_frame.as_ref(), self.previous_failed)
    }

    /// Folds one finished cell into the capsule, the checkpoints and the
    /// no-progress guard.
    pub(super) fn observe(
        &mut self,
        record: &CellRecord,
        error: Option<(&str, &str)>,
        snapshots: Option<(&crate::changes::Snapshot, &crate::changes::Snapshot)>,
    ) -> Observed {
        if let Some((before, after)) = snapshots {
            let changed = before.changed_paths(after);
            if !changed.is_empty() {
                self.files.observe(&changed);
                self.last_mutation_cell = Some(record.cell);
                let digest = after.digest();
                self.checkpoints.note_mutation(record.cell, &digest);
                self.tree_digest = Some(digest);
            }
        }
        self.capsule
            .observe_cell(record, error, self.tree_digest.as_deref());
        if let Some(verification) = self.capsule.last_verification()
            && verification.cell == record.cell
        {
            self.last_verification_cell = Some(verification.cell);
            self.checkpoints.note_verification(
                verification.cell,
                self.tree_digest.as_deref().unwrap_or(""),
                verification.exit_code == Some(0),
            );
        }
        let mut notices = Vec::new();
        // Only a failing frame can repeat without progress: a denied or
        // thrown call, or a thrown cell. A successful frame ends the streak.
        let failed = error.is_some()
            || record
                .calls
                .iter()
                .any(|call| !matches!(call.ended, Ended::Ok));
        // One fingerprint, two readers: the guard asks "again?" of a failing
        // frame, the stall asks "ever?" of every frame.
        let frame = crate::progress::fingerprint(record, error, self.tree_digest.as_deref());
        if failed {
            if let Some(notice) = self.guard.observe(frame.clone()) {
                output::no_progress_notice();
                notices.push(notice);
            }
        } else {
            self.guard.reset();
        }
        // A stall is a run of cells producing nothing this task has not
        // already seen. One is a notice; `DEFAULT_STALL_LIMIT` of them in a
        // row is what ends an unattended task.
        if let Some(notice) = self.stall.observe(&frame) {
            output::stall_notice();
            notices.push(notice);
        }
        let rendered = self.capsule.render();
        let capsule_block = (rendered != self.last_capsule_render).then(|| {
            self.last_capsule_render = rendered.clone();
            rendered
        });
        self.previous_frame = Some(record.clone());
        self.consider_red_check(record, snapshots.map(|(_, after)| after));
        if let Some(finished) = self.red_check.as_ref().and_then(|check| check.poll()) {
            self.red_check = None;
            notices.extend(finished);
        }
        Observed {
            notices,
            capsule_block,
        }
    }

    /// Starts a red run when this cell ran a test command that passed and
    /// named a test file the task changed, the task changed code too, and
    /// neither that command on this tree nor another red run is under way
    /// (`test_check`).
    fn consider_red_check(&mut self, record: &CellRecord, now: Option<&crate::changes::Snapshot>) {
        if self.red_check.is_some() || self.last_mutation_cell.is_none() {
            return;
        }
        // A cell that only ran tests changed nothing, so the session kept no
        // snapshot of it; one is taken only when a command passed.
        let captured;
        let now = match now {
            Some(now) => now,
            None if record
                .calls
                .iter()
                .any(|call| call.tool == "bash" && call.exit_code == Some(0)) =>
            {
                captured = crate::changes::Snapshot::capture(&self.profile);
                &captured
            }
            None => return,
        };
        let Some(session) = self.session_id.as_ref() else {
            return;
        };
        // Sterna's own state is not the project's code.
        let changed: Vec<_> = self
            .task_start
            .changed_paths(now)
            .into_iter()
            .filter(|(path, _)| !path.starts_with(".sterna"))
            .collect();
        let (tests, code): (Vec<_>, Vec<_>) = changed.iter().partition(|(path, _)| {
            super::test_check::is_test_path(&path.to_string_lossy().replace('\\', "/"))
        });
        if tests.is_empty() || code.is_empty() {
            return;
        }
        let digest = now.digest();
        let Some(command) = record
            .calls
            .iter()
            .filter(|call| call.tool == "bash" && call.exit_code == Some(0))
            .filter_map(|call| call.args.get("command"))
            .find(|command| {
                tests.iter().any(|(path, _)| {
                    super::test_check::runs_test(
                        command,
                        &path.to_string_lossy().replace('\\', "/"),
                    )
                })
            })
        else {
            return;
        };
        if !self.red_checked.insert(format!("{digest}\n{command}")) {
            return;
        }
        let mut before = Vec::new();
        for (path, _) in &code {
            match self.task_start.content(path, RED_FILE_CAP) {
                Some(bytes) => before.push((path.clone(), bytes)),
                // What the file held is not known, so the old code cannot
                // be rebuilt and there is nothing honest to run.
                None => return,
            }
        }
        self.red_check = Some(super::test_check::start_red_check(
            &self.profile,
            session,
            command,
            before,
        ));
    }

    /// The evidence gate on a terminal candidate: the deterministic
    /// final-state contract, then the decision model's completion question
    /// when one is configured. Findings hold the return once; the same
    /// findings a second time let the model finish with the completion
    /// recorded unverified.
    pub(super) fn gate(
        &mut self,
        candidate: &str,
        cell: u64,
        before: &crate::changes::Snapshot,
        after: &crate::changes::Snapshot,
        session: &Session<'_>,
    ) -> Option<String> {
        if !self.evidence_gate {
            output::completion(true, true, &[], 0);
            return None;
        }
        let mut files = self.files.clone();
        let changed = before.changed_paths(after);
        files.observe(&changed);
        let last_mutation = if changed.is_empty() {
            self.last_mutation_cell
        } else {
            Some(cell)
        };
        let root = session.profile.root();
        let contract = match crate::completion::load_contract(root) {
            Ok(contract) => contract,
            Err(error) => {
                session_println!("completion: {error}");
                crate::completion::Contract::default()
            }
        };
        let findings = crate::completion::check(
            &contract,
            root,
            &files,
            self.last_verification_cell,
            last_mutation,
        );
        let mut findings = findings;
        if let Some(verification) = self.capsule.last_verification()
            && verification.exit_code.is_some_and(|code| code != 0)
            && last_mutation.is_none_or(|cell| cell <= verification.cell)
        {
            findings.push(crate::completion::Finding {
                kind: crate::completion::FindingKind::VerificationFailed,
                path: None,
                sentence: format!(
                    "`{}` (cell {}) exited {}, and nothing changed after it: fix what it \
                     reports, or say in the answer why it fails.",
                    verification.command,
                    verification.cell,
                    verification.exit_code.unwrap_or_default()
                ),
            });
        }
        // The completion question (2616, extended 2641): does the diff or
        // the answer satisfy the request, and does the diff show the
        // hygiene a reviewer would check -- `completion_answer` caches the wire answer keyed
        // by the exact diff or answer text it was asked about, so an
        // identical second claim (the hold-once case) asks nothing, but a
        // diff or answer that changed since the cached answer is a new
        // question, never the stale answer to a tree that no longer
        // exists. Only in `mode != off` with a model configured; `mode =
        // off` or no model stays byte-identical to before this question
        // existed.
        let diff = self.task_start.diff(after);
        let decisions_config = session.config().decisions.clone();
        if let Some(model) = decisions_config.model.clone()
            && decisions_config.mode != crate::config::DecisionMode::Off
        {
            // An empty diff (nothing changed) or a read-only intent means
            // there is no diff worth asking about -- the empty-diff defect
            // Phase 66's shadow calibration measured, a diff-shaped question
            // against a task that never touched a file.
            let is_read_only_intent = self
                .intent
                .as_ref()
                .is_some_and(|intent| intent.choice == crate::decide::READ_ONLY);
            let diff_is_empty = diff.as_ref().is_none_or(|diff| diff.is_empty());
            let use_answer_state = diff_is_empty || is_read_only_intent;
            let cache_key = if use_answer_state {
                candidate.to_string()
            } else {
                diff.clone().unwrap_or_default()
            };
            let stale = self
                .completion_answer
                .as_ref()
                .is_none_or(|(asked_about, _)| asked_about != &cache_key);
            if stale {
                let finding_sentences: Vec<String> = findings
                    .iter()
                    .map(|finding| finding.sentence.clone())
                    .collect();
                let state = if use_answer_state {
                    crate::decide::CompletionState::Answer { answer: candidate }
                } else {
                    crate::decide::CompletionState::Diff {
                        diff: &cache_key,
                        findings: &finding_sentences,
                    }
                };
                match crate::decide::completion_satisfied(&model, &self.task, state) {
                    Ok(answer) => self.completion_answer = Some((cache_key, answer)),
                    Err(_) => self.decision_failures += 1,
                }
            }
            if let Some((_, answer)) = self.completion_answer.clone() {
                let decisions_on = decisions_config.mode == crate::config::DecisionMode::On;
                let mut finding_added = false;
                if answer.noul <= decisions_config.completion_no_below && decisions_on {
                    findings.push(crate::completion::Finding {
                        kind: crate::completion::FindingKind::RequestNotSatisfied,
                        path: None,
                        sentence: format!(
                            "the decision model reads {} as not satisfying the request ({:.2})",
                            if use_answer_state {
                                "the answer"
                            } else {
                                "the diff"
                            },
                            answer.noul
                        ),
                    });
                    finding_added = true;
                }

                let mut hygiene_findings = 0u32;
                if let Some(hygiene) = &answer.hygiene {
                    let checks: [(bool, &str); 5] = [
                        (
                            hygiene.has_tests <= decisions_config.hygiene_no_below,
                            "no test for the changed behaviour — add one or say why.",
                        ),
                        (
                            hygiene.out_of_scope >= decisions_config.hygiene_yes_above,
                            "the diff changes files the request did not ask about — narrow it or say why.",
                        ),
                        (
                            hygiene.debug_leftovers >= decisions_config.hygiene_yes_above,
                            "the diff leaves debugging artefacts — remove the prints, commented-out code or TODOs.",
                        ),
                        (
                            hygiene.deletes_tests >= decisions_config.hygiene_yes_above,
                            "the diff deletes or disables tests — restore them or say why.",
                        ),
                        (
                            hygiene.changes_signature >= decisions_config.hygiene_yes_above,
                            "the diff changes a public function or type signature — confirm it is intended.",
                        ),
                    ];
                    for (decisive, sentence) in checks {
                        if decisive && decisions_on {
                            hygiene_findings += 1;
                            findings.push(crate::completion::Finding {
                                kind: crate::completion::FindingKind::HygieneIssue,
                                path: None,
                                sentence: sentence.to_string(),
                            });
                        }
                    }
                }

                self.completion_decision = Some(CompletionTelemetry {
                    noul: answer.noul,
                    latency_ms: answer.latency_ms,
                    truncated: answer.truncated,
                    finding_added,
                    state: if use_answer_state { "answer" } else { "diff" },
                    hygiene: answer.hygiene.map(|hygiene| {
                        serde_json::json!({
                            "has_tests": hygiene.has_tests,
                            "out_of_scope": hygiene.out_of_scope,
                            "debug_leftovers": hygiene.debug_leftovers,
                            "deletes_tests": hygiene.deletes_tests,
                            "changes_signature": hygiene.changes_signature,
                        })
                    }),
                    hygiene_findings,
                });
            }
            output::decisions(self.decisions_telemetry(&decisions_config));
        }
        // Only a fact holds the answer (`FindingKind::holds`); every other
        // finding is a note beside it.
        let (hard, soft): (Vec<_>, Vec<_>) = findings.iter().partition(|f| f.kind.holds());
        let sentences: Vec<String> = hard.iter().map(|f| f.sentence.clone()).collect();
        let notes: Vec<String> = soft.iter().map(|f| f.sentence.clone()).collect();
        if sentences.is_empty() || self.deferred_findings.as_ref() == Some(&sentences) {
            show_notes(&notes);
        }
        if sentences.is_empty() {
            // Verified only when nothing was noted either: an answer that
            // stands beside a note stands, but is not a verified one.
            output::completion(true, notes.is_empty(), &notes, self.gate_deferrals);
            return None;
        }
        if self.deferred_findings.as_ref() == Some(&sentences) {
            output::completion(true, false, &sentences, self.gate_deferrals);
            return None;
        }
        self.deferred_findings = Some(sentences.clone());
        self.gate_deferrals += 1;
        let listed: Vec<String> = sentences.iter().map(|s| format!("- {s}")).collect();
        let text = format!(
            "## Candidate completion (deferred)\n{candidate}\n\n## Final-state findings\n{}\n\n\
             Resolve each finding, or return the same final answer again to finish with the \
             completion recorded as unverified.\n\n{}",
            listed.join("\n"),
            self.capsule.render()
        );
        Some(text)
    }

    /// A prose answer is a completion claim, and the evidence gate covers it
    /// exactly as it covers a terminal `return`: the same candidate rule,
    /// held once, then recorded unverified. Measured 2026-09-13 (the first
    /// hybrid Terminal-Bench trial): gpt-5.6-sol ends a task with a
    /// structured return and then prose, so a gate on returns alone gated
    /// nothing in the field.
    pub(super) fn prose_completion(
        &mut self,
        candidate: &str,
        profile: &Profile,
        session: &Session<'_>,
    ) -> Step {
        let now = crate::changes::Snapshot::capture(profile);
        let cell = self.previous_frame.as_ref().map_or(0, |frame| frame.cell);
        if let Some(gate) = self.gate(candidate, cell, &now, &now, session) {
            return Step {
                answer: Some(gate.clone()),
                historical: Some(gate.clone()),
                native_result: None,
                response: None,
                prose: false,
                record: None,
                rollback: None,
                view: CellView {
                    output: Some(gate),
                    ..CellView::default()
                },
            };
        }
        Step {
            answer: None,
            historical: None,
            native_result: None,
            response: None,
            prose: false,
            record: None,
            rollback: None,
            view: CellView::default(),
        }
    }

    /// A task that ended without completing keeps its established facts and
    /// its unfinished work in the capsule, without claiming completion.
    pub(super) fn salvage(&mut self, reason: &str) {
        self.capsule.salvage(reason);
        output::capsule(self.capsule.to_json());
    }
}

/// Once the answer stands, the notes the gate did not hold on, shown to the
/// person.
fn show_notes(notes: &[String]) {
    if !notes.is_empty() {
        super::ui::output(format!(
            "{}{}",
            crate::tui::history::NOTED,
            notes.join("\n")
        ));
    }
}

/// `model-contract.md` §5 applied to one assistant message: one `sterna` block
/// blocks form one validated program; only explicit completion ends prose.
///
/// **Nothing in `assistant_text` reaches a shell.** The one thing extracted
/// from it is a program, and the only thing that ever receives a program is
/// [`Runtime::run_cell`]; every tool that program calls goes through
/// `tools::invoke` and the session's sandbox from inside the isolate.
/// The effectful calls a thrown cell completed before it threw, one line
/// each, or `None` when nothing with an effect ran.
///
/// The invariant: **only calls the trajectory records as `Ok` on a tool
/// that changes the world appear here.** A read that completed is not an
/// effect the model must know survived; an `edit` is.
pub(super) fn partial_effects(record: &CellRecord, threw: bool) -> Option<String> {
    if !threw {
        return None;
    }
    let lines: Vec<String> = record
        .calls
        .iter()
        .filter(|call| matches!(call.ended, Ended::Ok))
        .filter_map(|call| {
            let head = |text: &str| -> String {
                let mut head: String = text.chars().take(80).collect();
                if text.chars().count() > 80 {
                    head.push('…');
                }
                head
            };
            match call.tool.as_str() {
                "edit" | "write" => call
                    .args
                    .get("path")
                    .map(|path| format!("{} {}", call.tool, head(path))),
                "bash" => call
                    .args
                    .get("command")
                    .map(|command| format!("bash `{}`", head(command))),
                "checks.run" => call
                    .args
                    .get("name")
                    .map(|name| format!("checks.run {name}")),
                _ => None,
            }
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "Completed before the throw, and their effects persist: {}. Calls after the \
         throw did not run.",
        lines.join("; ")
    ))
}
