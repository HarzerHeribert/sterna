//! What each turn this gateway served actually cost, kept between processes.
//!
//! The gateway already knows. `gateway::usage` reads the provider's own
//! `input`, `output` and **cached** counts out of bytes it is forwarding
//! anyway, `gateway::session` folds them into a `NewObservation`, and the
//! accept loop hands that to the observation sink. A *hosted* gateway's
//! host listens and writes the row on its own side. A standalone one was
//! handed `null_sink()`, so every figure was computed and dropped — and
//! `routing-cost --json`, the one command Sterna asks, had nothing to print.
//!
//! Measured on 2026-09-17: two dogfooding sessions of 11.8M and 20.2M tokens
//! reported `cache_read_input_tokens: 0` throughout, which said nothing about
//! caching at all. Sterna's `ServedBy` was simply always unknown, so its
//! fallback — parsing the response body, where only the Anthropic spelling is
//! understood — answered for routes that spell it `cached_tokens`.
//!
//! **This is not the routing ledger the extraction removed.** That was a
//! database inside the gateway library. This is the same small per-provider
//! JSON cache [`super::GatewayQuotaCache`] and [`super::ContextLimitCache`]
//! already are, written by the *binary* — the host of last resort for a
//! gateway that has no other — through the sink the library already reports
//! to. The library still keeps nothing and still imports no host type.
//!
//! **A gateway told to keep no telemetry keeps no rows.** The binary installs
//! this sink in the same place it installs the quota cache, so the two are
//! switched on and off together and neither is reachable from the library.
//!
//! Unlike its two siblings, this one is a **sequence**, not a latest-value
//! map: the question a reader asks is "what did the turn I just paid for
//! cost", so rows are appended and read back by window. It is bounded the
//! same way — a fixed number of rows per provider, oldest dropped first.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Serialises every append in this process — see [`TurnCostLedger::try_append`]
/// for why a lost update here is worse than a lost update in either sibling
/// cache.
static APPEND_LOCK: Mutex<()> = Mutex::new(());

/// Bumped when the shape below changes; a file at any other version reads as
/// absent, exactly as its two siblings' version guards do.
const TURN_COST_FORMAT_VERSION: u32 = 1;

/// The most rows one provider's file will hold.
///
/// A window, not an archive: the reader asks for rows since a recent second,
/// and a session that has been running for hours is the longest window anyone
/// asks about. Two hundred turns per provider covers that with room to spare,
/// and reaching it drops the oldest row rather than refusing the newest.
const MOST_ROWS_KEPT: usize = 200;

/// One served turn, as the provider stated it.
///
/// Every token field is optional and absent is never zero: a provider that
/// stated no usage, and a protocol this gateway has no usage spelling for,
/// both record nothing here rather than recording that the turn was free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCost {
    /// The wall-clock second the exchange completed — what a reader filters
    /// on, and what the eviction above sorts by.
    pub observed_at_unix: i64,
    /// The model the request asked for, as the route assigned it.
    pub model: String,
    /// The protocol slug the request was placed in.
    #[serde(default)]
    pub route: Option<String>,
    /// The credential label the route served it under.
    #[serde(default)]
    pub quota_context: Option<String>,
    /// Which kind of work this was, from the fixed purpose vocabulary.
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Input tokens the provider served from its prompt cache. **The figure
    /// this store exists for**: it is the difference between a turn costing
    /// its whole context and costing a fraction of it, and it is the one
    /// number no other path back to Sterna carries on an OpenAI-family route.
    #[serde(default)]
    pub cached_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedTurnCosts {
    version: u32,
    provider: String,
    #[serde(default)]
    rows: Vec<TurnCost>,
}

/// Where served turns are kept between processes.
#[derive(Debug, Clone)]
pub struct TurnCostLedger {
    root: PathBuf,
}

impl TurnCostLedger {
    /// The ledger under an installation's data directory. `gateway-turn-costs`
    /// is this store's own corner of it, beside `gateway-quota` and
    /// `gateway-context-limits`.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("gateway-turn-costs"),
        }
    }

    /// A ledger rooted at an explicit directory, for tests.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, provider: &str) -> PathBuf {
        crate::provider::cache::provider_json_path(&self.root, provider)
    }

    fn load_raw(&self, provider: &str) -> Option<PersistedTurnCosts> {
        let bytes = std::fs::read(self.path_for(provider)).ok()?;
        let stored: PersistedTurnCosts = serde_json::from_slice(&bytes).ok()?;
        if stored.version != TURN_COST_FORMAT_VERSION || stored.provider != provider {
            return None;
        }
        Some(stored)
    }

    /// Every row kept for `provider`, oldest first. Empty for a provider
    /// nothing has been served on, and for every way the read can fail.
    #[must_use]
    pub fn load(&self, provider: &str) -> Vec<TurnCost> {
        self.load_raw(provider)
            .map(|stored| stored.rows)
            .unwrap_or_default()
    }

    /// Every row this ledger holds at or after `since`, across every
    /// provider, ascending by the second it completed and paired with the
    /// provider that served it.
    ///
    /// Ascending because that is the order `routing-cost --json` promises and
    /// Sterna's reader depends on: it takes the **last** row in the window as
    /// the one closest to the request it is answering for.
    #[must_use]
    pub fn since(&self, since: i64) -> Vec<(String, TurnCost)> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<PersistedTurnCosts>(&bytes) else {
                continue;
            };
            if stored.version != TURN_COST_FORMAT_VERSION {
                continue;
            }
            for row in stored.rows {
                if row.observed_at_unix >= since {
                    out.push((stored.provider.clone(), row));
                }
            }
        }
        out.sort_by_key(|(provider, row)| (row.observed_at_unix, provider.clone()));
        out
    }

    /// Records one served turn.
    ///
    /// Best-effort on a write failure — logged, never propagated: the sink
    /// this is called from runs on a connection thread that cannot fail a
    /// real exchange over a full disk, the same rule
    /// [`super::GatewayQuotaCache::store`] states for itself.
    pub fn append(&self, provider: &str, row: TurnCost) {
        if let Err(err) = self.try_append(provider, row) {
            tracing::debug!(
                provider,
                error = %err,
                "could not persist a served turn's cost"
            );
        }
    }

    fn try_append(&self, provider: &str, row: TurnCost) -> std::io::Result<()> {
        // **Appending is read-modify-write, and the callers are the accept
        // loop's connection threads.** Without this, two turns completing
        // together both read the same file, both add their own row, and the
        // second write loses the first — measured 2026-09-17 against the
        // shipped binary, which kept two rows for three served exchanges.
        // Its two siblings are latest-value-wins maps where a lost update
        // costs one reading; here it costs the row a reader is about to ask
        // for, because the newest row is the answer.
        //
        // One lock for every ledger in the process, not one per provider: a
        // write is a few kilobytes and happens once per turn, so contention
        // is not a cost anyone can measure, and a lock keyed by anything
        // would be a map that itself needs guarding.
        let _serialised = APPEND_LOCK.lock().unwrap_or_else(|poisoned| {
            // A panic in another thread's append says nothing about this
            // file: the data is whatever is on disk, and refusing to record
            // from here on would lose more than it protects.
            poisoned.into_inner()
        });
        std::fs::create_dir_all(&self.root)?;
        let mut rows = self.load(provider);
        rows.push(row);
        rows.sort_by_key(|row| row.observed_at_unix);
        if rows.len() > MOST_ROWS_KEPT {
            // The oldest rows are the ones worth losing: nobody asks about a
            // window that has already scrolled out of every reader's `since`.
            rows.drain(..rows.len() - MOST_ROWS_KEPT);
        }
        let stored = PersistedTurnCosts {
            version: TURN_COST_FORMAT_VERSION,
            provider: provider.to_owned(),
            rows,
        };
        let encoded = serde_json::to_vec_pretty(&stored)
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        crate::provider::cache::write_json_atomically(&self.path_for(provider), &encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: i64, cached: Option<u64>) -> TurnCost {
        TurnCost {
            observed_at_unix: at,
            model: "gpt-5.6-sol".to_owned(),
            route: Some("openai-responses".to_owned()),
            quota_context: Some("chatgpt-subscription".to_owned()),
            purpose: Some("harness-turn".to_owned()),
            input_tokens: Some(52_000),
            output_tokens: Some(900),
            cached_input_tokens: cached,
        }
    }

    #[test]
    fn a_served_turn_survives_the_process_that_served_it() {
        let dir = tempfile::tempdir().unwrap();
        TurnCostLedger::at(dir.path())
            .append("chatgpt-subscription", row(1_789_000_000, Some(48_000)));

        let reread = TurnCostLedger::at(dir.path());
        let rows = reread.load("chatgpt-subscription");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cached_input_tokens, Some(48_000));
        assert_eq!(rows[0].input_tokens, Some(52_000));
    }

    #[test]
    fn a_window_holds_what_it_covers_and_nothing_older() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = TurnCostLedger::at(dir.path());
        ledger.append("chatgpt-subscription", row(1_789_000_000, Some(1)));
        ledger.append("chatgpt-subscription", row(1_789_000_500, Some(2)));

        let window = ledger.since(1_789_000_100);
        assert_eq!(window.len(), 1, "the older row is outside the window");
        assert_eq!(window[0].0, "chatgpt-subscription");
        assert_eq!(window[0].1.cached_input_tokens, Some(2));
    }

    #[test]
    fn rows_come_back_oldest_first_across_providers() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = TurnCostLedger::at(dir.path());
        ledger.append("claude-max", row(1_789_000_200, Some(10)));
        ledger.append("chatgpt-subscription", row(1_789_000_100, Some(20)));
        ledger.append("claude-max", row(1_789_000_300, Some(30)));

        let seconds: Vec<i64> = ledger
            .since(0)
            .iter()
            .map(|(_, row)| row.observed_at_unix)
            .collect();
        assert_eq!(
            seconds,
            vec![1_789_000_100, 1_789_000_200, 1_789_000_300],
            "the reader takes the last row as the newest, so order is the contract"
        );
    }

    #[test]
    fn an_unstated_figure_stays_absent_rather_than_becoming_zero() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = TurnCostLedger::at(dir.path());
        ledger.append("groq", row(1_789_000_000, None));
        assert_eq!(ledger.load("groq")[0].cached_input_tokens, None);
    }

    #[test]
    fn the_oldest_rows_are_dropped_once_the_window_is_full() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = TurnCostLedger::at(dir.path());
        for n in 0..(MOST_ROWS_KEPT as i64 + 5) {
            ledger.append(
                "chatgpt-subscription",
                row(1_789_000_000 + n, Some(n as u64)),
            );
        }
        let rows = ledger.load("chatgpt-subscription");
        assert_eq!(rows.len(), MOST_ROWS_KEPT);
        assert_eq!(
            rows[0].observed_at_unix, 1_789_000_005,
            "the five oldest rows left, not the five newest"
        );
    }

    /// Turns completing together all reach the ledger.
    ///
    /// The regression for a real lost update: before the append lock, the
    /// shipped binary kept two rows for three exchanges served back to back,
    /// because each connection thread read the file, added its row, and wrote
    /// over whatever another had written meanwhile.
    #[test]
    fn turns_that_complete_together_are_all_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = TurnCostLedger::at(dir.path());
        std::thread::scope(|scope| {
            for n in 0..8 {
                let ledger = ledger.clone();
                scope.spawn(move || {
                    ledger.append(
                        "chatgpt-subscription",
                        row(1_789_000_000 + n, Some(n as u64)),
                    );
                });
            }
        });
        assert_eq!(
            ledger.load("chatgpt-subscription").len(),
            8,
            "every thread's row survived"
        );
    }

    #[test]
    fn nothing_served_reads_as_nothing_rather_than_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = TurnCostLedger::at(dir.path());
        assert!(ledger.load("never-served").is_empty());
        assert!(ledger.since(0).is_empty());
        assert!(TurnCostLedger::at("/does/not/exist").since(0).is_empty());
    }

    #[test]
    fn a_file_from_another_format_version_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = TurnCostLedger::at(dir.path());
        ledger.append("groq", row(1_789_000_000, Some(5)));
        let path = ledger.path_for("groq");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["version"] = serde_json::json!(TURN_COST_FORMAT_VERSION + 1);
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        assert!(ledger.load("groq").is_empty());
        assert!(ledger.since(0).is_empty());
    }
}
