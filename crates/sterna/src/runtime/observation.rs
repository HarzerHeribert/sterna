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

/// The rules' work on oversized results over one task, measured.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReductionStats {
    /// Results the rules shortened.
    pub ruled: u64,
    /// Bytes of output the rules were handed.
    pub bytes_in: u64,
    /// Bytes of reduction returned, provenance line included.
    pub bytes_out: u64,
}

impl ReductionStats {
    pub fn add(&mut self, other: &ReductionStats) {
        self.ruled = self.ruled.saturating_add(other.ruled);
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
            ruled: 1,
            bytes_in: 10_000,
            bytes_out: 200,
        });
        total.add(&ReductionStats {
            ruled: 1,
            bytes_in: 5_000,
            bytes_out: 300,
        });
        assert_eq!(total.ruled, 2);
        assert_eq!(total.bytes_in, 15_000);
        assert_eq!(total.bytes_out, 500);
    }
}
