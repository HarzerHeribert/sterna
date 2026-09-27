//! Context windows this gateway has **watched a route enforce**, kept between
//! processes.
//!
//! A published catalogue states a window for a model. A provider enforces one
//! for an account on a route, and a refusal is the only place that figure is
//! ever stated out loud (`crate::gateway::context_limit`). This is where the
//! figure lives once it has been heard, so the process that answers
//! `inference-gateway models --json` — a different process from the one that
//! served the refusal — can hand it to whoever is drawing a context meter.
//!
//! **Keyed by route, which here means provider and model.** That is the
//! granularity the ruling asks for (`archive/glasshouse:docs/product/design-decisions.md`, *A
//! context window is a property of the route, not of the model*) and the
//! granularity this gateway can actually observe: `routing::Route` carries a
//! provider and an assigned model, and an account resolves to one of those
//! providers. Two accounts on the same provider therefore share a reading —
//! correct where a provider enforces one window for everyone it serves, and a
//! stated limitation where a provider enforces a window per subscription
//! tier. A tier-aware key needs the account name on the route, which is not
//! there today.
//!
//! **The newest reading wins, without comparison.** A tier can be upgraded, a
//! provider can raise a limit, an account can move. There is no regime-change
//! record here for that reason: unlike a rate-limit ceiling, where the
//! *change* is the interesting event, a window's history is of no use to
//! anyone once the current figure is known.
//!
//! Exactly [`super::GatewayQuotaCache`]'s shape otherwise:
//! [`ContextLimitCache::at`] for tests, [`ContextLimitCache::new`] for
//! production, one JSON file per provider, written
//! to a temporary file and renamed into place, and every way a read can fail
//! reads as "nothing observed here" rather than as an error.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bumped when the shape below changes; a file at any other version reads as
/// absent, exactly as the quota cache's own version guard does.
const CONTEXT_LIMIT_FORMAT_VERSION: u32 = 1;

/// The most models one provider's file will hold.
///
/// A guard against a pathological accumulation — a gateway that has served a
/// thousand model names should not grow a file without end — set far above
/// the number of models any account actually serves. Reaching it drops the
/// oldest reading, never the newest.
const MOST_MODELS_KEPT: usize = 256;

/// One route's observed window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedWindow {
    /// The figure the provider stated when it refused an over-long request.
    pub context_window_tokens: u64,
    /// When it said so, in Unix seconds — kept so a reader can tell a figure
    /// observed minutes ago from one observed last year, and so the eviction
    /// above has something to sort on.
    pub observed_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedContextLimits {
    version: u32,
    provider: String,
    #[serde(default)]
    models: BTreeMap<String, ObservedWindow>,
}

/// Where observed windows are kept between processes.
#[derive(Debug, Clone)]
pub struct ContextLimitCache {
    root: PathBuf,
}

impl ContextLimitCache {
    /// The cache under an installation's data directory. `gateway-context-
    /// limits` is this cache's own corner of it, beside `gateway-quota`.
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("gateway-context-limits"),
        }
    }

    /// A cache rooted at an explicit directory, for tests.
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

    fn load_raw(&self, provider: &str) -> Option<PersistedContextLimits> {
        let bytes = std::fs::read(self.path_for(provider)).ok()?;
        let stored: PersistedContextLimits = serde_json::from_slice(&bytes).ok()?;
        if stored.version != CONTEXT_LIMIT_FORMAT_VERSION || stored.provider != provider {
            return None;
        }
        Some(stored)
    }

    /// Every window observed on `provider`, keyed by the model name as the
    /// route assigned it. Empty for a provider nothing has been observed on,
    /// and for every way the read can fail.
    #[must_use]
    pub fn load(&self, provider: &str) -> BTreeMap<String, ObservedWindow> {
        self.load_raw(provider)
            .map(|stored| stored.models)
            .unwrap_or_default()
    }

    /// Every window this cache holds, across every provider, as
    /// `(provider, model, window)`.
    ///
    /// For the reader that has no provider in hand — `models --json`, which
    /// is asked about models rather than about routes.
    #[must_use]
    pub fn all(&self) -> Vec<(String, String, ObservedWindow)> {
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
            let Ok(stored) = serde_json::from_slice::<PersistedContextLimits>(&bytes) else {
                continue;
            };
            if stored.version != CONTEXT_LIMIT_FORMAT_VERSION {
                continue;
            }
            for (model, window) in stored.models {
                out.push((stored.provider.clone(), model.clone(), window));
            }
        }
        out
    }

    /// Records what `provider` enforced for `model`.
    ///
    /// Best-effort on a write failure — logged, never propagated: the accept
    /// loop this is called from cannot fail a real exchange over a full disk,
    /// the same rule [`super::GatewayQuotaCache::store`] states for itself.
    pub fn store(&self, provider: &str, model: &str, tokens: u64, observed_at_unix: i64) {
        if let Err(err) = self.try_store(provider, model, tokens, observed_at_unix) {
            tracing::debug!(
                provider,
                model,
                error = %err,
                "could not persist an observed context window"
            );
        }
    }

    fn try_store(
        &self,
        provider: &str,
        model: &str,
        tokens: u64,
        observed_at_unix: i64,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let mut models = self.load(provider);
        models.insert(
            model.to_owned(),
            ObservedWindow {
                context_window_tokens: tokens,
                observed_at_unix,
            },
        );
        while models.len() > MOST_MODELS_KEPT {
            // The oldest reading is the one worth losing: it is the most
            // likely to describe a tier or a route the account has left.
            let oldest = models
                .iter()
                .min_by_key(|(_, window)| window.observed_at_unix)
                .map(|(model, _)| model.clone());
            match oldest {
                Some(model) => {
                    models.remove(&model);
                }
                None => break,
            }
        }
        let stored = PersistedContextLimits {
            version: CONTEXT_LIMIT_FORMAT_VERSION,
            provider: provider.to_owned(),
            models,
        };
        let encoded = serde_json::to_vec_pretty(&stored)
            .map_err(|err| std::io::Error::other(err.to_string()))?;
        crate::provider::cache::write_json_atomically(&self.path_for(provider), &encoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_survives_the_process_that_observed_it() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ContextLimitCache::at(dir.path());
        cache.store("anyrouter", "claude-opus-5", 200_000, 1_789_000_000);

        let reread = ContextLimitCache::at(dir.path());
        let windows = reread.load("anyrouter");
        assert_eq!(windows["claude-opus-5"].context_window_tokens, 200_000);
        assert_eq!(windows["claude-opus-5"].observed_at_unix, 1_789_000_000);
    }

    #[test]
    fn a_later_reading_replaces_an_earlier_one_because_a_tier_can_change() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ContextLimitCache::at(dir.path());
        cache.store("openrouter", "gpt-5.6-sol", 128_000, 1_789_000_000);
        cache.store("openrouter", "gpt-5.6-sol", 922_000, 1_789_100_000);
        assert_eq!(
            cache.load("openrouter")["gpt-5.6-sol"].context_window_tokens,
            922_000,
            "the newest reading wins without comparison"
        );
    }

    #[test]
    fn two_providers_serving_one_model_are_two_readings() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ContextLimitCache::at(dir.path());
        cache.store("anthropic", "claude-sonnet-4-6", 1_000_000, 1_789_000_000);
        cache.store("snowflake", "claude-sonnet-4-6", 200_000, 1_789_000_001);
        assert_eq!(
            cache.load("anthropic")["claude-sonnet-4-6"].context_window_tokens,
            1_000_000
        );
        assert_eq!(
            cache.load("snowflake")["claude-sonnet-4-6"].context_window_tokens,
            200_000,
            "a re-host caps what it resells, and that is the point of the key"
        );
        assert_eq!(cache.all().len(), 2);
    }

    #[test]
    fn nothing_observed_reads_as_nothing_rather_than_as_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ContextLimitCache::at(dir.path());
        assert!(cache.load("never-served").is_empty());
        assert!(cache.all().is_empty());
        assert!(ContextLimitCache::at("/does/not/exist").all().is_empty());
    }

    #[test]
    fn a_file_from_another_format_version_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ContextLimitCache::at(dir.path());
        cache.store("groq", "kimi-k3", 128_000, 1_789_000_000);
        let path = cache.path_for("groq");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["version"] = serde_json::json!(CONTEXT_LIMIT_FORMAT_VERSION + 1);
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        assert!(cache.load("groq").is_empty());
        assert!(cache.all().is_empty());
    }
}
