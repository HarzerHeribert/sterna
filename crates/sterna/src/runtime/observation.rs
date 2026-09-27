//! What one turn's rendering cost the parent, and what it did not have to --
//! `smarter-cheaper-roadmap.md`, *Observation delta* and *Adaptive result
//! reduction*.
//!
//! The invariant: **every figure here is a count of bytes or rows the
//! renderer actually produced or actually suppressed.** Nothing is
//! estimated from what a model might have read; a suppressed row is one the
//! full inventory would have carried and this turn's rendering did not.

use serde::Serialize;

/// The handle-table rendering of one turn, measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ObservationStats {
    /// Handle entries rendered in full this turn.
    pub rows_rendered: usize,
    /// Live handles carried as a one-line reference instead of a full entry.
    pub rows_suppressed: usize,
    /// Bytes of the table as rendered.
    pub bytes_rendered: usize,
    /// Bytes a full inventory of every live handle would have cost.
    pub bytes_full_inventory: usize,
    /// Whether this rendering was a complete inventory (every live handle in
    /// full). The roadmap's target is zero repeated full inventories after
    /// the first turn a handle appears in.
    pub full_inventory: bool,
    /// Pure calls whose observation matched an earlier one byte for byte.
    pub repeated_observations: usize,
}

impl ObservationStats {
    /// Bytes the parent did not receive because rows were suppressed.
    #[must_use]
    pub fn bytes_suppressed(&self) -> usize {
        self.bytes_full_inventory
            .saturating_sub(self.bytes_rendered)
    }
}

/// The pushed reducer's work over one task, measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReductionStats {
    /// Results above the threshold for which a reduction was attempted.
    pub attempted: u64,
    /// Reductions that came back and were attached to the result.
    pub made: u64,
    /// Attempts that failed; the exact output stayed complete.
    pub failed: u64,
    /// Results served from the digest cache without a request.
    pub cached: u64,
    /// Results the deterministic rules brought under the threshold on their
    /// own, so no request was made and no cheap-model token was spent.
    pub ruled: u64,
    /// Reductions a filter the model wrote produced, rather than prose it
    /// retyped.
    pub filtered: u64,
    /// Reductions a filter written for an *earlier* result produced, so no
    /// request was made. This is the cache that matters: the digest one above
    /// needs the same bytes twice, and this one needs only the same shape.
    pub filter_reused: u64,
    /// Filters that came back and did not survive validation, after the one
    /// retry. The rules or today's refusal answered instead.
    pub filter_rejected: u64,
    /// Bytes handed to the reducer — the *sample*, not the output.
    ///
    /// A filter is written from the shape of an output rather than from all
    /// of it, so what travels is sizes, a histogram of line shapes, head,
    /// tail and the lines that must survive. Measured on a 600-line log:
    /// 9,492 bytes of output, 1,865 of sample.
    pub bytes_in: u64,
    /// Bytes of reduction returned.
    pub bytes_out: u64,
}

impl ReductionStats {
    pub fn add(&mut self, other: &ReductionStats) {
        self.attempted = self.attempted.saturating_add(other.attempted);
        self.made = self.made.saturating_add(other.made);
        self.failed = self.failed.saturating_add(other.failed);
        self.cached = self.cached.saturating_add(other.cached);
        self.ruled = self.ruled.saturating_add(other.ruled);
        self.filtered = self.filtered.saturating_add(other.filtered);
        self.filter_reused = self.filter_reused.saturating_add(other.filter_reused);
        self.filter_rejected = self.filter_rejected.saturating_add(other.filter_rejected);
        self.bytes_in = self.bytes_in.saturating_add(other.bytes_in);
        self.bytes_out = self.bytes_out.saturating_add(other.bytes_out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppressed_bytes_are_the_inventory_minus_the_rendering() {
        let stats = ObservationStats {
            bytes_rendered: 300,
            bytes_full_inventory: 1_200,
            ..ObservationStats::default()
        };
        assert_eq!(stats.bytes_suppressed(), 900);
        let none = ObservationStats {
            bytes_rendered: 500,
            bytes_full_inventory: 400,
            ..ObservationStats::default()
        };
        assert_eq!(none.bytes_suppressed(), 0);
    }

    #[test]
    fn reduction_stats_accumulate() {
        let mut total = ReductionStats::default();
        total.add(&ReductionStats {
            attempted: 1,
            made: 1,
            bytes_in: 10_000,
            bytes_out: 200,
            ..ReductionStats::default()
        });
        total.add(&ReductionStats {
            attempted: 1,
            failed: 1,
            bytes_in: 5_000,
            ..ReductionStats::default()
        });
        assert_eq!(total.attempted, 2);
        assert_eq!(total.made, 1);
        assert_eq!(total.failed, 1);
        assert_eq!(total.bytes_in, 15_000);
        assert_eq!(total.bytes_out, 200);
    }
}
