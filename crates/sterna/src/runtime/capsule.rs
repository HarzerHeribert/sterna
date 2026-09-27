//! The structured task capsule: what the task has established, from the
//! trajectory alone.
//!
//! The invariant: **every fact carries the cell and call it came from, and
//! nothing here is written by the model.** The capsule is derived from
//! `CellRecord`s deterministically, so a rendered `## Task` block never
//! claims more than the rollout can show — the defect it exists for is a
//! finalizer that believed narrative over state.

use serde::Serialize;

use crate::abi::lift::{self, Family};
use crate::runtime::outcome::{CellRecord, Ended, PlanItem, PlanStatus};

/// Facts kept verbatim; older ones are counted, not lost silently.
const FACT_CAP: usize = 8;
/// Risks kept; deduplicated by text before the cap applies.
const RISK_CAP: usize = 8;
/// Facts a rendering shows.
const RENDER_FACTS: usize = 6;
/// The rendered block's hard bound, in characters.
pub const RENDER_CAP: usize = 1_200;
const GOAL_CHARS: usize = 200;
const LINE_CHARS: usize = 110;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capsule {
    goal: String,
    state: State,
    facts: Vec<Fact>,
    /// How many facts fell off the front of `facts`.
    facts_omitted: usize,
    checkpoint: Option<Checkpoint>,
    risks: Vec<Risk>,
    risks_omitted: usize,
    next_action: Option<String>,
    last_verification: Option<Verification>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum State {
    NotStarted,
    InProgress,
    Verified { cell: u64 },
    UnverifiedSince { cell: u64 },
    Blocked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Fact {
    pub text: String,
    pub evidence: EvidenceRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceRef {
    pub cell: u64,
    pub call: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// The changed-file set's digest at the last passing verification, from
/// `changes::Snapshot::digest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checkpoint {
    pub cell: u64,
    pub tree_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Risk {
    pub text: String,
    pub since_cell: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verification {
    pub cell: u64,
    pub command: String,
    pub exit_code: Option<i32>,
    pub executed: bool,
    pub reused: bool,
}

impl Capsule {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            state: State::NotStarted,
            facts: Vec::new(),
            facts_omitted: 0,
            checkpoint: None,
            risks: Vec::new(),
            risks_omitted: 0,
            next_action: None,
            last_verification: None,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn facts(&self) -> &[Fact] {
        &self.facts
    }

    pub fn risks(&self) -> &[Risk] {
        &self.risks
    }

    pub fn checkpoint(&self) -> Option<&Checkpoint> {
        self.checkpoint.as_ref()
    }

    pub fn last_verification(&self) -> Option<&Verification> {
        self.last_verification.as_ref()
    }

    pub fn next_action(&self) -> Option<&str> {
        self.next_action.as_deref()
    }

    /// The exact facts as one line each, for `completion::fresh_checker_evidence`.
    pub fn fact_lines(&self) -> Vec<String> {
        self.facts
            .iter()
            .map(|fact| format!("{} (cell {})", fact.text, fact.evidence.cell))
            .collect()
    }

    /// Folds one finished cell in. Calls are read in trajectory order, so a
    /// verification followed by an edit in the same cell ends unverified.
    pub fn observe_cell(
        &mut self,
        record: &CellRecord,
        error: Option<(&str, &str)>,
        plan: &[PlanItem],
        tree_digest: Option<&str>,
    ) {
        if self.state == State::NotStarted {
            self.state = State::InProgress;
        }
        for (index, call) in record.calls.iter().enumerate() {
            match &call.ended {
                Ended::Denied { rule } => {
                    self.risk(format!("{} denied ({rule})", call.tool), record.cell);
                }
                Ended::Threw { .. } => {}
                Ended::Ok => {
                    if is_mutation(&call.tool) {
                        let path = call.args.get("path").map_or("<unknown>", String::as_str);
                        let sha256 = call.args.get("after_sha256").cloned();
                        let text = match &sha256 {
                            Some(hash) => format!(
                                "edited {path} (version {})",
                                hash.chars().take(12).collect::<String>()
                            ),
                            None => format!("edited {path}"),
                        };
                        self.fact(text, record.cell, index, sha256);
                        self.mutated(record.cell);
                    }
                }
            }
            if let Some(command) = verification_command(&call.tool, &call.args)
                && !matches!(call.ended, Ended::Denied { .. })
            {
                let exit_code = call.exit_code;
                self.last_verification = Some(Verification {
                    cell: record.cell,
                    command,
                    exit_code,
                    executed: call.args.get("executed").map(String::as_str) != Some("false"),
                    reused: call.args.get("reused").map(String::as_str) == Some("true"),
                });
                if exit_code == Some(0) && !matches!(self.state, State::Blocked { .. }) {
                    self.state = State::Verified { cell: record.cell };
                    self.checkpoint = tree_digest.map(|digest| Checkpoint {
                        cell: record.cell,
                        tree_digest: digest.to_string(),
                    });
                }
            }
        }
        if let Some((class, message)) = error {
            self.risk(
                format!(
                    "cell threw {class}: {}",
                    message.chars().take(120).collect::<String>()
                ),
                record.cell,
            );
        }
        if !plan.is_empty() {
            self.next_action = plan
                .iter()
                .find(|item| item.status == PlanStatus::Active)
                .map(|item| item.text.clone());
        }
    }

    /// Marks the task cut off — by the cell limit, a poisoned runtime or a
    /// provider failure — so a finalizer cannot read it as done.
    pub fn salvage(&mut self, reason: &str) {
        self.state = State::Blocked {
            reason: reason.to_string(),
        };
        let since = self.facts.last().map_or(0, |fact| fact.evidence.cell);
        self.risk(format!("cut off: {reason}"), since);
    }

    fn mutated(&mut self, cell: u64) {
        match self.state {
            State::Verified { .. } => self.state = State::UnverifiedSince { cell },
            State::NotStarted => self.state = State::InProgress,
            State::InProgress | State::UnverifiedSince { .. } | State::Blocked { .. } => {}
        }
    }

    fn fact(&mut self, text: String, cell: u64, call: usize, sha256: Option<String>) {
        self.facts.push(Fact {
            text,
            evidence: EvidenceRef { cell, call, sha256 },
        });
        if self.facts.len() > FACT_CAP {
            self.facts.remove(0);
            self.facts_omitted += 1;
        }
    }

    fn risk(&mut self, text: String, since_cell: u64) {
        if self.risks.iter().any(|risk| risk.text == text) {
            return;
        }
        if self.risks.len() >= RISK_CAP {
            self.risks_omitted += 1;
            return;
        }
        self.risks.push(Risk { text, since_cell });
    }

    /// The `## Task` block, under [`RENDER_CAP`] characters whatever the
    /// history holds: each line is capped and the whole is cut last.
    pub fn render(&self) -> String {
        let mut out = String::from("## Task\n");
        out.push_str(&clip(&self.goal, GOAL_CHARS));
        out.push('\n');
        out.push_str(&format!("state: {}\n", self.state_line()));
        let shown = self.facts.len().min(RENDER_FACTS);
        let hidden = self.facts_omitted + self.facts.len() - shown;
        for fact in &self.facts[self.facts.len() - shown..] {
            out.push_str(&format!(
                "- {} (cell {})\n",
                clip(&fact.text, LINE_CHARS),
                fact.evidence.cell
            ));
        }
        if hidden > 0 {
            out.push_str(&format!("- …and {hidden} earlier facts\n"));
        }
        if !self.risks.is_empty() {
            out.push_str("risks:\n");
            for risk in &self.risks {
                out.push_str(&format!(
                    "- {} (since cell {})\n",
                    clip(&risk.text, LINE_CHARS),
                    risk.since_cell
                ));
            }
            if self.risks_omitted > 0 {
                out.push_str(&format!("- …and {} more\n", self.risks_omitted));
            }
        }
        if let Some(next) = &self.next_action {
            out.push_str(&format!("next: {}\n", clip(next, LINE_CHARS)));
        }
        out.push_str(&format!("verified: {}\n", self.verified_line()));
        if out.chars().count() > RENDER_CAP {
            out = out.chars().take(RENDER_CAP - 1).collect();
            out.push('…');
        }
        out
    }

    fn state_line(&self) -> String {
        match &self.state {
            State::NotStarted => "not started".into(),
            State::InProgress => "in progress".into(),
            State::Verified { cell } => format!("verified at cell {cell}"),
            State::UnverifiedSince { cell } => format!("unverified since cell {cell}"),
            State::Blocked { reason } => format!("blocked — {}", clip(reason, LINE_CHARS)),
        }
    }

    fn verified_line(&self) -> String {
        match &self.last_verification {
            None => "never".into(),
            Some(v) => format!(
                "cell {} `{}` → {}{}",
                v.cell,
                clip(&v.command, 60),
                match v.exit_code {
                    Some(code) => format!("exit {code}"),
                    None => "exit unknown".into(),
                },
                if v.reused { " (reused)" } else { "" }
            ),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

fn is_mutation(tool: &str) -> bool {
    matches!(tool, "edit" | "write")
}

/// The command a call verified with: a named check's command, or a shell
/// command `lift::classify` puts in the verification family.
fn verification_command(
    tool: &str,
    args: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    match tool {
        "checks.run" => Some(
            args.get("command")
                .or_else(|| args.get("name"))
                .cloned()
                .unwrap_or_default(),
        ),
        "bash" => {
            let command = args.get("command")?;
            matches!(lift::classify(command), Some(Family::Verification)).then(|| command.clone())
        }
        _ => None,
    }
}

fn clip(text: &str, chars: usize) -> String {
    let text = text.trim().replace('\n', " ");
    if text.chars().count() <= chars {
        return text;
    }
    let mut out: String = text.chars().take(chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::outcome::{CallRecord, CellOutcomeKind};

    fn call(tool: &str, args: &[(&str, &str)], ended: Ended, exit_code: Option<i32>) -> CallRecord {
        CallRecord {
            tool: tool.into(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            evidence: None,
            lifted_from: None,
            exit_code,
            repeat_of: None,
            error: None,
            ended,
        }
    }

    fn cell(n: u64, calls: Vec<CallRecord>) -> CellRecord {
        CellRecord {
            cell: n,
            source: String::new(),
            description: None,
            outcome: CellOutcomeKind::Yielded,
            handles: Vec::new(),
            calls,
        }
    }

    #[test]
    fn edit_then_passing_check_verifies_and_a_later_edit_unverifies() {
        let mut capsule = Capsule::new("fix the parser");
        capsule.observe_cell(
            &cell(
                1,
                vec![call(
                    "edit",
                    &[
                        ("path", "/p/src/lib.rs"),
                        ("after_sha256", "abcdef0123456789"),
                    ],
                    Ended::Ok,
                    None,
                )],
            ),
            None,
            &[],
            None,
        );
        assert_eq!(capsule.state(), &State::InProgress);
        assert_eq!(
            capsule.facts()[0].text,
            "edited /p/src/lib.rs (version abcdef012345)"
        );
        capsule.observe_cell(
            &cell(
                2,
                vec![call(
                    "bash",
                    &[("command", "cargo test -p sterna")],
                    Ended::Ok,
                    Some(0),
                )],
            ),
            None,
            &[],
            Some("digest-2"),
        );
        assert_eq!(capsule.state(), &State::Verified { cell: 2 });
        assert_eq!(capsule.checkpoint().unwrap().tree_digest, "digest-2");
        capsule.observe_cell(
            &cell(
                3,
                vec![call("write", &[("path", "/p/README")], Ended::Ok, None)],
            ),
            None,
            &[],
            None,
        );
        assert_eq!(capsule.state(), &State::UnverifiedSince { cell: 3 });
        assert!(capsule.render().contains("unverified since cell 3"));
    }

    #[test]
    fn a_failing_verification_is_recorded_but_does_not_verify() {
        let mut capsule = Capsule::new("g");
        capsule.observe_cell(
            &cell(
                1,
                vec![call(
                    "checks.run",
                    &[
                        ("name", "tests"),
                        ("command", "pytest"),
                        ("executed", "true"),
                    ],
                    Ended::Ok,
                    Some(1),
                )],
            ),
            None,
            &[],
            Some("d"),
        );
        assert_eq!(capsule.state(), &State::InProgress);
        assert_eq!(capsule.last_verification().unwrap().exit_code, Some(1));
        assert!(capsule.checkpoint().is_none());
    }

    #[test]
    fn a_repeated_denial_is_one_risk() {
        let mut capsule = Capsule::new("g");
        for n in 1..=3 {
            capsule.observe_cell(
                &cell(
                    n,
                    vec![call(
                        "bash",
                        &[],
                        Ended::Denied {
                            rule: "no allow".into(),
                        },
                        None,
                    )],
                ),
                None,
                &[],
                None,
            );
        }
        assert_eq!(capsule.risks().len(), 1);
        assert_eq!(capsule.risks()[0].since_cell, 1);
    }

    #[test]
    fn render_stays_under_the_bound_with_fifty_facts_and_a_long_goal() {
        let mut capsule = Capsule::new("x".repeat(2_000));
        for n in 1..=50u64 {
            let path = format!("/very/long/path/{}/file-{n}.rs", "segment/".repeat(20));
            capsule.observe_cell(
                &cell(n, vec![call("edit", &[("path", &path)], Ended::Ok, None)]),
                Some(("TypeError", &"boom ".repeat(100))),
                &[PlanItem {
                    text: "z".repeat(500),
                    status: PlanStatus::Active,
                }],
                None,
            );
        }
        let rendered = capsule.render();
        assert!(rendered.chars().count() <= RENDER_CAP, "{rendered}");
        assert!(rendered.starts_with("## Task\n"));
        assert!(rendered.contains("earlier facts"), "{rendered}");
        assert_eq!(capsule.facts().len(), FACT_CAP);
        assert_eq!(capsule.to_json()["facts_omitted"], 42);
    }

    #[test]
    fn salvage_marks_blocked_and_names_the_reason() {
        let mut capsule = Capsule::new("g");
        capsule.observe_cell(
            &cell(
                4,
                vec![call(
                    "bash",
                    &[("command", "cargo test")],
                    Ended::Ok,
                    Some(0),
                )],
            ),
            None,
            &[],
            None,
        );
        capsule.salvage("cell limit reached");
        assert_eq!(
            capsule.state(),
            &State::Blocked {
                reason: "cell limit reached".into()
            }
        );
        assert!(capsule.render().contains("blocked — cell limit reached"));
        assert_eq!(capsule.to_json()["state"]["kind"], "blocked");
    }
}
