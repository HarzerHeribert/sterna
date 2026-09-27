//! One open window of events — `docs/events.md` —
//! and how it closes into a [`Batch`]. `Window` owns no clock: every `now` it
//! reasons about is a parameter, never read from the system.

use std::collections::HashSet;

use super::batch::{Batch, BatchEntry, ClosedOn, Rolled};
use super::{Event, EventId, Kind, Priority, Stamp};

/// §11's three tunables — 2,000 ms, 200 and age 4 — as a constructor
/// argument. `config.toml` naming who may change them is 61F's; this struct is
/// the one place their defaults are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowConfig {
    pub deadline_ms: i64,
    pub cap: usize,
    pub drop_age: u32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            deadline_ms: 2_000,
            cap: 200,
            drop_age: 4,
        }
    }
}

/// Why `Window::accept` refused an event — always because a first arrival of
/// the same identity already stands, never because the window is full (a
/// wider window spills at close, it does not refuse at accept).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// §1's dedup key already has a first event; this arrival is the later
    /// one the table says to drop.
    Dedup,
    /// A `worker.quiet` arrived while this window already holds that
    /// worker's `worker.report` — the quiet never displaces it.
    QuietUnderReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    Kept,
    Dropped(DropReason),
}

/// What `Window::accept` records happened to one arrival, dropped ones
/// included, so a successor can render the rollout's full history rather
/// than only what survived into a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    pub at: Stamp,
    pub outcome: ArrivalOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrivalOutcome {
    Kept(EventId),
    Dropped {
        reason: DropReason,
        dedup_key: String,
    },
}

/// One window: opens when the previous batch was delivered (or at session
/// start), accumulates events, and closes on the earlier of its deadline or
/// an interrupt. Reused across cycles — [`Window::take_batch`] resets it so
/// the very next [`Window::accept`] opens what the contract calls "a new
/// window" without the caller allocating a fresh struct.
pub struct Window {
    config: WindowConfig,
    next_id: EventId,
    first_at: Option<Stamp>,
    deadline: Option<Stamp>,
    closed_on: Option<ClosedOn>,
    /// This window's own events, oldest first: a prior close's spillover is
    /// seeded here before any fresh arrival, so spillover always leads what
    /// this window itself accepts.
    events: Vec<Event>,
    dedup_keys: HashSet<String>,
    /// Unacked events rolled forward from the batch just delivered, already
    /// capped at half the batch cap by [`Batch::roll`]. Consumed — moved
    /// into the next [`Batch`] ahead of this window's own events — the next
    /// time [`Window::take_batch`] runs.
    rolled_in: Vec<(Event, u32)>,
    /// How many events the most recent [`Window::carry_forward`] dropped,
    /// consumed by the very next [`Window::take_batch`] for that batch's
    /// `… and K dropped unacked` preview line.
    pending_dropped_count: usize,
    arrivals: Vec<Arrival>,
}

impl Window {
    pub fn new(config: WindowConfig) -> Self {
        Self {
            config,
            next_id: 1,
            first_at: None,
            deadline: None,
            closed_on: None,
            events: Vec::new(),
            dedup_keys: HashSet::new(),
            rolled_in: Vec::new(),
            pending_dropped_count: 0,
            arrivals: Vec::new(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed_on.is_some()
    }

    /// Whether this window is holding nothing at all — neither an arrival of
    /// its own nor a roll from the batch just delivered. A caller waiting for
    /// a window to close asks this first: an empty window has no deadline and
    /// can never close, so waiting on one would wait forever.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.rolled_in.is_empty()
    }

    pub fn depth(&self) -> usize {
        self.events.len() + self.rolled_in.len()
    }

    pub fn payload_ids(&self) -> std::collections::HashSet<String> {
        self.events
            .iter()
            .chain(self.rolled_in.iter().map(|(event, _)| event))
            .map(|event| event.payload.as_str().to_string())
            .collect()
    }

    pub fn arrivals(&self) -> &[Arrival] {
        &self.arrivals
    }

    /// Feeds a delivered batch's roll back in, so the unacked events it
    /// carries lead the next batch this window closes (§3: "rolled events
    /// … included first") and any it dropped are remembered for
    /// [`Batch::payload`].
    pub fn carry_forward(&mut self, rolled: Rolled) {
        self.pending_dropped_count = rolled.dropped.len();
        self.rolled_in = rolled.events;
    }

    fn record(&mut self, at: Stamp, outcome: ArrivalOutcome) {
        self.arrivals.push(Arrival { at, outcome });
    }

    /// Accepts one arrival. The first call after a reset sets the deadline
    /// from `now`; every later call in the same window leaves it alone —
    /// measuring from the first event, never the last, is what keeps a
    /// storm's debounce from re-arming on every arrival.
    pub fn accept(&mut self, mut event: Event, now: Stamp) -> Accepted {
        if self.first_at.is_none() {
            self.first_at = Some(now);
            self.deadline = Some(Stamp::from_millis(
                now.as_millis() + self.config.deadline_ms,
            ));
        }

        if let Kind::WorkerQuiet { .. } = &event.kind {
            let report_already_here = self
                .events
                .iter()
                .any(|e| matches!(e.kind, Kind::WorkerReport { .. }) && e.source == event.source);
            if report_already_here {
                let key = event.dedup_key();
                self.record(
                    now,
                    ArrivalOutcome::Dropped {
                        reason: DropReason::QuietUnderReport,
                        dedup_key: key,
                    },
                );
                return Accepted::Dropped(DropReason::QuietUnderReport);
            }
        }

        let key = event.dedup_key();
        if self.dedup_keys.contains(&key) {
            self.record(
                now,
                ArrivalOutcome::Dropped {
                    reason: DropReason::Dedup,
                    dedup_key: key,
                },
            );
            return Accepted::Dropped(DropReason::Dedup);
        }

        if let Kind::WorkerReport { .. } = &event.kind {
            let source = event.source.clone();
            self.events
                .retain(|e| !(matches!(e.kind, Kind::WorkerQuiet { .. }) && e.source == source));
        }

        event.id = self.next_id;
        self.next_id += 1;
        self.dedup_keys.insert(key);

        if event.priority == Priority::Interrupt {
            let elapsed = now.since(self.first_at.expect("set above"));
            self.closed_on = Some(ClosedOn::Interrupt(elapsed));
        }

        self.record(now, ArrivalOutcome::Kept(event.id));
        self.events.push(event);
        Accepted::Kept
    }

    /// Closes the window on its deadline, measured from the first event,
    /// when `now` has reached it — then yields exactly what
    /// [`Window::take_batch`] would. Does nothing (and returns `None`) if
    /// the window is already closed by an interrupt or the deadline has not
    /// arrived: a window with no interrupt and no due deadline stays open.
    pub fn close_if_due(&mut self, now: Stamp) -> Option<Batch> {
        if self.first_at.is_none() && !self.rolled_in.is_empty() {
            self.first_at = Some(now);
            self.deadline = Some(Stamp::from_millis(
                now.as_millis() + self.config.deadline_ms,
            ));
        }
        if !self.is_closed()
            && let Some(deadline) = self.deadline
            && now >= deadline
        {
            let elapsed = now.since(self.first_at.expect("deadline implies a first event"));
            self.closed_on = Some(ClosedOn::Deadline(elapsed));
        }
        self.take_batch()
    }

    /// Yields the batch this window closed into, or `None` if it has not
    /// closed yet. Interrupts lead, then this window's rolled-forward
    /// events, then its own oldest-first events capped at `config.cap` —
    /// the overflow becomes the seed of the window that opens next.
    pub fn take_batch(&mut self) -> Option<Batch> {
        let closed_on = self.closed_on?;

        let mut interrupts = Vec::new();
        let mut batch_events = Vec::new();
        for event in self.events.drain(..) {
            match event.priority {
                Priority::Interrupt => interrupts.push(event),
                Priority::Batch => batch_events.push(event),
            }
        }

        let rolled_in = std::mem::take(&mut self.rolled_in);
        let rolled_in_count = rolled_in.len();

        let capacity_left = self.config.cap.saturating_sub(rolled_in_count);
        let spilled = if batch_events.len() > capacity_left {
            batch_events.split_off(capacity_left)
        } else {
            Vec::new()
        };

        self.dedup_keys = spilled.iter().map(Event::dedup_key).collect();
        self.events = spilled;

        let mut entries =
            Vec::with_capacity(interrupts.len() + rolled_in_count + batch_events.len());
        entries.extend(
            interrupts
                .into_iter()
                .map(|event| BatchEntry { event, age: 0 }),
        );
        entries.extend(
            rolled_in
                .into_iter()
                .map(|(event, age)| BatchEntry { event, age }),
        );
        entries.extend(
            batch_events
                .into_iter()
                .map(|event| BatchEntry { event, age: 0 }),
        );

        let n = entries.len();
        let mut batch = Batch::new(
            n,
            closed_on,
            entries,
            rolled_in_count,
            self.config.cap,
            self.config.drop_age,
        );
        batch.set_newly_dropped(std::mem::take(&mut self.pending_dropped_count));

        self.first_at = self.events.first().map(|event| event.at);
        self.deadline = self
            .first_at
            .map(|at| Stamp::from_millis(at.as_millis() + self.config.deadline_ms));
        self.closed_on = None;

        Some(batch)
    }
}
