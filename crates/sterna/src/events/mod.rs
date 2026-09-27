//! What an event is, and the dedup key deciding whether a second arrival is a
//! second event — `docs/events.md`. `window` turns a
//! stream of these into one closed window; `batch` renders the closed window
//! as the `Events.Batch` handle. This module owns none of the delivery,
//! background-job or handler machinery §4-§9 describe — only the event
//! vocabulary and the dedup rule those sections bind.

pub mod batch;
pub mod window;

use std::cell::RefCell;
use std::rc::Rc;

/// The Rust side of the one `batch` handle §4 declares, held for the life of
/// a [`crate::runtime::isolate::Runtime`] so the model's own
/// `batch.where(...)`, `batch.ack(...)` and `batch.rest()` have something to
/// call.
///
/// One slot, not a map: §4 gives the model exactly one `batch` at a time and
/// makes each delivery **replace** the previous one, so a second live batch
/// would be a name the model never wrote.
#[derive(Default)]
pub struct BatchStore {
    live: RefCell<Option<batch::Batch>>,
}

impl BatchStore {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// Installs `batch` as the live one and answers with the one it
    /// replaced — which the session rolls into the next window (§3), because
    /// nothing here knows what a window is.
    pub fn replace(&self, batch: batch::Batch) -> Option<batch::Batch> {
        self.live.borrow_mut().replace(batch)
    }

    /// Reads the live batch, if there is one. `None` before the first
    /// delivery and after [`BatchStore::clear`].
    pub fn with<T>(&self, f: impl FnOnce(&mut batch::Batch) -> T) -> Option<T> {
        self.live.borrow_mut().as_mut().map(f)
    }

    /// Frees the live batch — the task ending, which is one of
    /// `runtime-contract.md` §2's three lifetime events.
    pub fn take(&self) -> Option<batch::Batch> {
        self.live.borrow_mut().take()
    }

    pub fn clear(&self) {
        self.live.borrow_mut().take();
    }
}

/// The wall clock as a [`Stamp`], and the only reading of it this crate
/// makes for an event.
///
/// §1 makes `at` "when the runtime **accepted** it, never when the source
/// claims it happened", so producer and window must read one clock; this is
/// it. [`window::Window`] itself still takes every `now` as a parameter and
/// reads nothing.
pub fn now() -> Stamp {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0);
    Stamp::from_millis(millis)
}

/// Monotonic per [`window::Window`], starting at 1. Assigned by
/// [`window::Window::accept`], never by the event's own producer — a caller
/// building an [`Event`] before it is accepted has no id to give it yet.
pub type EventId = u64;

/// One of the nine kinds §1 enumerates, carrying the fields its own dedup key
/// needs. `hook.<name>` carries only its name — `PostToolUse`, not
/// `hook.PostToolUse` — because [`Kind::as_str`] builds the `hook.` prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Kind {
    WorkerReport {
        path: String,
        mtime: String,
    },
    WorkerQuiet {
        quiet_since: String,
    },
    Prompt,
    CiRun {
        conclusion: String,
    },
    CiCell {
        cell: String,
        conclusion: String,
    },
    BgDone {
        emission: String,
    },
    /// A subagent finished — Phase 64. Its own kind and not a `bg.done`,
    /// because a program that selects finished shell jobs must not be handed
    /// an answer to a question it asked, and the reverse.
    AgentDone {
        emission: String,
    },
    Hook(String),
    Timer {
        deadline: String,
    },
}

impl Kind {
    /// Spells the kind exactly as §1's table does: `worker.report`,
    /// `hook.PostToolUse`, and so on.
    pub fn as_str(&self) -> String {
        match self {
            Kind::WorkerReport { .. } => "worker.report".to_string(),
            Kind::WorkerQuiet { .. } => "worker.quiet".to_string(),
            Kind::Prompt => "prompt".to_string(),
            Kind::CiRun { .. } => "ci.run".to_string(),
            Kind::CiCell { .. } => "ci.cell".to_string(),
            Kind::BgDone { .. } => "bg.done".to_string(),
            Kind::AgentDone { .. } => "agent.done".to_string(),
            Kind::Hook(name) => format!("hook.{name}"),
            Kind::Timer { .. } => "timer".to_string(),
        }
    }
}

/// §1's exactly-two priorities: which kinds are `Interrupt` by default is
/// `config.toml`'s `[events] interrupt` list (61F), not this module's decision
/// — a caller building an `Event` states the priority itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    Batch,
    Interrupt,
}

/// An opaque id the successor materialises into a real payload — this module
/// never reads what it names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PayloadRef(String);

impl PayloadRef {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A UTC millisecond timestamp. `Display` renders it ISO-8601
/// (`2026-09-06T16:58:41.204Z`) with a pure function of this module — no
/// date/time crate is added for it, because `Cargo.toml` belongs to another
/// package for this task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp(i64);

impl Stamp {
    pub fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    pub fn as_millis(&self) -> i64 {
        self.0
    }

    /// Milliseconds elapsed from `earlier` to `self`, saturating at zero so a
    /// clock that runs backwards never produces a negative "closed after"
    /// duration.
    pub fn since(&self, earlier: Stamp) -> u64 {
        self.0.saturating_sub(earlier.0).max(0) as u64
    }

    /// Just the `HH:MM:SS.mmmZ` portion — what `events-contract.md` §10 view
    /// 1's table shows per event, the date being implied by the batch itself.
    /// `Display` (the full `Self` type) stays the general ISO-8601 form.
    pub(crate) fn time_of_day(&self) -> String {
        let ms_of_day = self.0.rem_euclid(86_400_000);
        let h = ms_of_day / 3_600_000;
        let min = (ms_of_day / 60_000) % 60;
        let s = (ms_of_day / 1_000) % 60;
        let millis = ms_of_day % 1_000;
        format!("{h:02}:{min:02}:{s:02}.{millis:03}Z")
    }
}

/// Days-since-epoch to `(year, month, day)`, Howard Hinnant's `civil_from_days`
/// — the standard closed-form inverse of the proleptic Gregorian calendar,
/// used here only so [`Stamp`]'s `Display` needs no date/time dependency.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

impl std::fmt::Display for Stamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ms = self.0;
        let days = ms.div_euclid(86_400_000);
        let ms_of_day = ms.rem_euclid(86_400_000);
        let (y, m, d) = civil_from_days(days);
        let h = ms_of_day / 3_600_000;
        let min = (ms_of_day / 60_000) % 60;
        let s = (ms_of_day / 1_000) % 60;
        let millis = ms_of_day % 1_000;
        write!(
            f,
            "{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}.{millis:03}Z"
        )
    }
}

/// One event, §1's five contract fields plus the `id` [`window::Window`]
/// assigns, the producer's own `summary` line §3's preview shows, and
/// `tool_call_id` — `hook.<name>`'s dedup key needs the tool call id (§1:
/// `source + hook + tool call id`), and `Kind::Hook` is pinned to carry only
/// the hook's name, so the id rides here instead rather than being parsed out
/// of `summary`. `None` for every other kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Event {
    pub id: EventId,
    pub kind: Kind,
    pub source: String,
    pub at: Stamp,
    pub payload: PayloadRef,
    pub priority: Priority,
    pub summary: String,
    pub tool_call_id: Option<String>,
}

impl Event {
    /// Builds an event with no id yet — [`window::Window::accept`] assigns
    /// the real, monotonic one when it is kept.
    #[allow(clippy::too_many_arguments)]
    pub fn pending(
        kind: Kind,
        source: impl Into<String>,
        at: Stamp,
        payload: PayloadRef,
        priority: Priority,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: 0,
            kind,
            source: source.into(),
            at,
            payload,
            priority,
            summary: summary.into(),
            tool_call_id: None,
        }
    }

    /// [`Self::pending`], additionally carrying the tool call id
    /// `Kind::Hook`'s dedup key needs.
    #[allow(clippy::too_many_arguments)]
    pub fn pending_hook(
        name: impl Into<String>,
        source: impl Into<String>,
        at: Stamp,
        payload: PayloadRef,
        priority: Priority,
        summary: impl Into<String>,
        tool_call_id: impl Into<String>,
    ) -> Self {
        let mut event = Self::pending(
            Kind::Hook(name.into()),
            source,
            at,
            payload,
            priority,
            summary,
        );
        event.tool_call_id = Some(tool_call_id.into());
        event
    }

    /// §1's dedup key, exactly: the inputs each kind's row names, read from
    /// this event's own fields, never parsed out of `summary`.
    pub fn dedup_key(&self) -> String {
        match &self.kind {
            Kind::WorkerReport { path, mtime } => {
                format!("worker.report|{}|{path}|{mtime}", self.source)
            }
            Kind::WorkerQuiet { quiet_since } => {
                format!("worker.quiet|{}|{quiet_since}", self.source)
            }
            Kind::Prompt => format!("prompt|{}", self.source),
            Kind::CiRun { conclusion } => format!("ci.run|{}|{conclusion}", self.source),
            Kind::CiCell { cell, conclusion } => {
                format!("ci.cell|{}|{cell}|{conclusion}", self.source)
            }
            Kind::BgDone { emission } => format!("bg.done|{}|{emission}", self.source),
            // A subagent completes once, so the source alone would do; the
            // emission is kept for the same reason `bg.done` keeps it, which
            // is that one shape for both is one thing to reason about.
            Kind::AgentDone { emission } => format!("agent.done|{}|{emission}", self.source),
            Kind::Hook(name) => format!(
                "hook|{}|{name}|{}",
                self.source,
                self.tool_call_id.as_deref().unwrap_or("")
            ),
            Kind::Timer { deadline } => format!("timer|{}|{deadline}", self.source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_spells_hook_with_its_name() {
        let kind = Kind::Hook("PostToolUse".to_string());
        assert_eq!(kind.as_str(), "hook.PostToolUse");
    }

    #[test]
    fn stamp_displays_iso8601_with_milliseconds() {
        // 2026-09-06T16:58:41.204Z, computed independently via a Unix
        // timestamp calculator and pinned here so the pure formatter is
        // checked against a value nobody derived from this same code.
        let stamp = Stamp::from_millis(1_788_713_921_204);
        assert_eq!(stamp.to_string(), "2026-09-06T16:58:41.204Z");
    }

    #[test]
    fn stamp_since_is_the_millisecond_difference() {
        let a = Stamp::from_millis(1_000);
        let b = Stamp::from_millis(3_204);
        assert_eq!(b.since(a), 2_204);
    }

    #[test]
    fn dedup_key_differs_by_kind_specific_field_not_summary() {
        let a = Event::pending(
            Kind::WorkerReport {
                path: "report.md".into(),
                mtime: "t1".into(),
            },
            "worker/a",
            Stamp::from_millis(0),
            PayloadRef::new("p1"),
            Priority::Batch,
            "same summary",
        );
        let b = Event::pending(
            Kind::WorkerReport {
                path: "report.md".into(),
                mtime: "t2".into(),
            },
            "worker/a",
            Stamp::from_millis(0),
            PayloadRef::new("p1"),
            Priority::Batch,
            "same summary",
        );
        assert_ne!(a.dedup_key(), b.dedup_key());
    }
}
