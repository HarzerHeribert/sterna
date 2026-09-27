//! The closed window as the `Events.Batch` handle —
//! `docs/events.md`.

use std::collections::{HashMap, HashSet, VecDeque};

use super::{Event, EventId, Kind, PayloadRef, Priority};
use crate::runtime::preview::{estimate_tokens, quote, thousands};

/// How this batch's window closed, and after how many milliseconds from its
/// first event — §2's two closing conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedOn {
    Interrupt(u64),
    Deadline(u64),
}

impl ClosedOn {
    fn describe(&self) -> String {
        match self {
            ClosedOn::Interrupt(ms) => {
                format!("window closed on interrupt after {} ms", thousands(*ms))
            }
            ClosedOn::Deadline(ms) => format!("window closed after {} ms", thousands(*ms)),
        }
    }
}

/// One event carried by a batch, and the number of batches it has now
/// appeared in unacked — `0` for an event delivered for the first time.
/// [`super::window::Window`] is the only producer; this module only ever
/// reads and advances the age it is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchEntry {
    pub event: Event,
    pub age: u32,
}

/// The result of [`Batch::ack`]: which of the given ids the batch actually
/// held, and which it did not — an unknown id is reported, never silently
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Acked {
    pub acked: Vec<EventId>,
    pub unknown: Vec<EventId>,
}

/// One event [`Batch::roll`] dropped at age 4 — its identity only, for the
/// rollout's `event.dropped` record. The event itself, payload included, is
/// gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedEvent {
    pub id: EventId,
    pub kind: Kind,
    pub source: String,
}

/// What [`Batch::roll`] hands the next window: the unacked events that
/// survive another cycle, aged one more, and the ones that did not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rolled {
    pub events: Vec<(Event, u32)>,
    pub dropped: Vec<DroppedEvent>,
}

/// A dropped event's payload is gone — `Batch::payload` returns this rather
/// than pretending the id was never valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadDropped(pub EventId);

/// The batch handle itself — §3's `Events.Batch`. `n` is this batch's own
/// total event count (interrupts, rolled-in and freshly closed events
/// together); `rolled_in` is how many of them were carried forward rather
/// than newly closed into this window.
pub struct Batch {
    pub n: usize,
    pub closed_on: ClosedOn,
    entries: Vec<BatchEntry>,
    acked: HashSet<EventId>,
    pub rolled_in: usize,
    cap: usize,
    drop_age: u32,
    /// How many events [`super::window::Window::carry_forward`] dropped
    /// immediately before this batch was formed — §3's "`… and K dropped
    /// unacked (see rollout)`" line.
    newly_dropped: usize,
}

/// The widest of the nine kind spellings this crate actually renders
/// (`hook.PostToolUse`) — `events-contract.md` §10 view 1's own column
/// widths, which is the only alignment this contract specifies.
const KIND_COL_WIDTH: usize = 16;
/// The samples section's source column width, likewise pinned to view 1's
/// layout rather than derived from any stated rule.
const SOURCE_COL_WIDTH: usize = 19;
/// The column the trailing `preview N tok` right-aligns to in view 1.
const TRAILER_COLUMN: usize = 66;
/// §3: "the preview shrinks by that section's rule, five → 2 → 0" — the
/// sample counts a preview steps through before anything above them is cut.
const SAMPLE_STEPS: [usize; 3] = [5, 2, 0];

impl Batch {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        n: usize,
        closed_on: ClosedOn,
        entries: Vec<BatchEntry>,
        rolled_in: usize,
        cap: usize,
        drop_age: u32,
    ) -> Self {
        Self {
            n,
            closed_on,
            entries,
            acked: HashSet::new(),
            rolled_in,
            cap,
            drop_age,
            newly_dropped: 0,
        }
    }

    /// Sets how many events were dropped forming this batch, for the
    /// preview's rollout line. `Window::take_batch` calls this once, right
    /// after construction.
    pub(crate) fn set_newly_dropped(&mut self, count: usize) {
        self.newly_dropped = count;
    }

    /// `kind` matches `hook.*` by prefix (any hook, any name) and every
    /// other value exactly; `source`, when given, matches exactly. Both
    /// optional, and a `None` matches everything.
    pub fn where_(&self, kind: Option<&str>, source: Option<&str>) -> Vec<&Event> {
        self.entries
            .iter()
            .map(|entry| &entry.event)
            .filter(|event| match kind {
                None => true,
                Some("hook.*") => matches!(event.kind, Kind::Hook(_)),
                Some(k) => event.kind.as_str() == k,
            })
            .filter(|event| source.is_none_or(|s| event.source == s))
            .collect()
    }

    /// Acks the given ids. An id the batch does not hold is reported in
    /// [`Acked::unknown`], never silently ignored.
    pub fn ack(&mut self, ids: &[EventId]) -> Acked {
        let mut result = Acked::default();
        for &id in ids {
            if self.entries.iter().any(|entry| entry.event.id == id) {
                self.acked.insert(id);
                result.acked.push(id);
            } else {
                result.unknown.push(id);
            }
        }
        result
    }

    /// Every event this batch carries, with the number of batches it has
    /// appeared in unacked — what the runtime marshals into the `batch`
    /// handle §4 binds. `where_` and `rest` answer the model's own questions
    /// and drop the age; this is the whole batch, in order.
    pub fn events(&self) -> Vec<(&Event, u32)> {
        self.entries
            .iter()
            .map(|entry| (&entry.event, entry.age))
            .collect()
    }

    /// This batch's events not yet acked.
    pub fn rest(&self) -> Vec<&Event> {
        self.entries
            .iter()
            .filter(|entry| !self.acked.contains(&entry.event.id))
            .map(|entry| &entry.event)
            .collect()
    }

    /// Remove acknowledged handler input before the model can inspect it.
    pub(crate) fn retain_unacked(&mut self) {
        self.entries
            .retain(|entry| !self.acked.contains(&entry.event.id));
        self.acked.clear();
        self.n = self.entries.len();
        self.rolled_in = self.entries.iter().filter(|entry| entry.age > 0).count();
    }

    /// A payload by event id — `Err` when the id is not in this batch, which
    /// for any id that ever existed can only mean [`Batch::roll`] dropped it.
    pub fn payload(&self, id: EventId) -> Result<&PayloadRef, PayloadDropped> {
        self.entries
            .iter()
            .find(|entry| entry.event.id == id)
            .map(|entry| &entry.event.payload)
            .ok_or(PayloadDropped(id))
    }

    /// Consumes the batch: every unacked event ages by one, drops at
    /// `drop_age`, and the survivors never exceed half `cap` — a backlog
    /// cannot starve what the model waits for, so the oldest half-cap
    /// survive and anything past that is dropped alongside the age-4 events
    /// rather than held for a window that can never make room for it.
    /// What the next batch can carry after the age and half-cap rules.
    pub fn rolling_depth(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| !self.acked.contains(&entry.event.id) && entry.age + 1 < self.drop_age)
            .count()
            .min(self.cap / 2)
    }

    pub fn roll(self) -> Rolled {
        let Batch {
            entries,
            acked,
            cap,
            drop_age,
            ..
        } = self;
        let half_cap = cap / 2;

        let mut survivors = Vec::new();
        let mut dropped = Vec::new();
        for entry in entries {
            if acked.contains(&entry.event.id) {
                continue;
            }
            let new_age = entry.age + 1;
            if new_age >= drop_age {
                dropped.push(DroppedEvent {
                    id: entry.event.id,
                    kind: entry.event.kind,
                    source: entry.event.source,
                });
            } else {
                survivors.push((entry.event, new_age));
            }
        }

        if survivors.len() > half_cap {
            for (event, _age) in survivors.split_off(half_cap) {
                dropped.push(DroppedEvent {
                    id: event.id,
                    kind: event.kind,
                    source: event.source,
                });
            }
        }

        Rolled {
            events: survivors,
            dropped,
        }
    }

    /// §3's preview, in its exact order: every interrupt in full, then
    /// counts by kind descending, then up to 5 samples rarest-kind-first,
    /// then how many more there are — shrinking the sample count 5 → 2 → 0
    /// to fit `cap_tokens` before anything above the samples is ever cut.
    pub fn preview(&self, cap_tokens: usize) -> String {
        let mut chosen = String::new();
        for &count in &SAMPLE_STEPS {
            chosen = self.render_body(count);
            if count == 0 || estimate_tokens(&chosen) <= cap_tokens {
                break;
            }
        }
        let tokens = estimate_tokens(&chosen);
        append_trailer(chosen, tokens)
    }

    fn interrupts(&self) -> impl Iterator<Item = &BatchEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.event.priority == Priority::Interrupt)
    }

    fn non_interrupts(&self) -> impl Iterator<Item = &BatchEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.event.priority != Priority::Interrupt)
    }

    fn render_header(&self) -> String {
        let mut parts = vec![format!("n={}", self.n)];
        if self.acked.is_empty() {
            if self.rolled_in > 0 {
                let age = self
                    .entries
                    .iter()
                    .filter(|entry| entry.age > 0)
                    .map(|entry| entry.age)
                    .max()
                    .unwrap_or(0);
                parts.push(format!("{} rolled (age {age})", self.rolled_in));
            }
            parts.push(self.closed_on.describe());
        } else {
            let rolling = self.n - self.acked.len();
            parts.push(format!("{} acked", self.acked.len()));
            parts.push(format!("{rolling} rolling"));
        }
        format!("batch  Events.Batch  {}", parts.join(" · "))
    }

    fn render_counts(&self) -> Vec<String> {
        let mut order: Vec<String> = Vec::new();
        let mut counts: HashMap<String, (usize, Vec<String>)> = HashMap::new();
        for entry in self.non_interrupts() {
            let kind = entry.event.kind.as_str();
            let bucket = counts.entry(kind.clone()).or_insert_with(|| {
                order.push(kind.clone());
                (0, Vec::new())
            });
            bucket.0 += 1;
            if !bucket.1.contains(&entry.event.source) {
                bucket.1.push(entry.event.source.clone());
            }
        }

        let mut rows: Vec<(String, usize, Vec<String>)> = order
            .into_iter()
            .map(|kind| {
                let (count, sources) = counts.remove(&kind).expect("just inserted");
                (kind, count, sources)
            })
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.1));

        rows.into_iter()
            .map(|(kind, count, sources)| {
                format!(
                    "     {kind:<KIND_COL_WIDTH$}{count:>4}   {}",
                    sources.join(", ")
                )
            })
            .collect()
    }

    fn render_samples(&self, count: usize) -> Vec<String> {
        if count == 0 {
            return Vec::new();
        }

        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, VecDeque<&BatchEntry>> = HashMap::new();
        for entry in self.non_interrupts() {
            let kind = entry.event.kind.as_str();
            groups
                .entry(kind.clone())
                .or_insert_with(|| {
                    order.push(kind.clone());
                    VecDeque::new()
                })
                .push_back(entry);
        }
        order.sort_by_key(|kind| groups[kind].len());

        let mut picked: Vec<&BatchEntry> = Vec::new();
        loop {
            if picked.len() >= count {
                break;
            }
            let mut progressed = false;
            for kind in &order {
                if picked.len() >= count {
                    break;
                }
                if let Some(entry) = groups
                    .get_mut(kind)
                    .expect("kind was just inserted")
                    .pop_front()
                {
                    picked.push(entry);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }

        picked
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let mut line = format!(
                    "  [{}] {:<KIND_COL_WIDTH$}  {:<SOURCE_COL_WIDTH$}  {}",
                    index + 1,
                    entry.event.kind.as_str(),
                    entry.event.source,
                    entry.event.at.time_of_day(),
                );
                if entry.age > 0 {
                    line.push_str(&format!("  age {}", entry.age));
                }
                line.push_str(&format!("  {}", quote(&entry.event.summary)));
                line
            })
            .collect()
    }

    fn render_body(&self, sample_count: usize) -> String {
        let mut lines = vec![self.render_header()];

        for entry in self.interrupts() {
            lines.push(render_interrupt(entry));
        }

        lines.extend(self.render_counts());

        let samples = self.render_samples(sample_count);
        let shown = self.interrupts().count() + samples.len();
        lines.extend(samples);

        let more = self.n.saturating_sub(shown);
        if more > 0 {
            lines.push(format!("  … and {more} more"));
        }
        if self.newly_dropped > 0 {
            lines.push(format!(
                "  … and {} dropped unacked (see rollout)",
                self.newly_dropped
            ));
        }

        lines.join("\n")
    }
}

fn render_interrupt(entry: &BatchEntry) -> String {
    format!(
        "  !  {}   {}  {}  {}",
        entry.event.kind.as_str(),
        entry.event.source,
        entry.event.at.time_of_day(),
        quote(&entry.event.summary)
    )
}

fn append_trailer(mut body: String, tokens: usize) -> String {
    let trailer = format!("preview {tokens} tok");
    let last_line_len = body
        .lines()
        .next_back()
        .map(|line| line.chars().count())
        .unwrap_or(0);
    let padding = TRAILER_COLUMN.saturating_sub(last_line_len).max(1);
    body.push_str(&" ".repeat(padding));
    body.push_str(&trailer);
    body
}
