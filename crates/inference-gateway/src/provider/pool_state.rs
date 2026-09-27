//! Which accounts the person has taken out of the pool: `pool.toml` in the
//! gateway's data directory, `excluded = ["claude-work"]`. Every account is
//! in its pool unless named here -- the pool of a model being every account
//! whose catalogue serves it, in configuration order.
//!
//! **Read live.** A serving gateway asks [`PoolState::excluded`] per
//! request; the file is re-read only when its modification time changes, so
//! a toggle in Sterna takes effect on the next request without a restart.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{Context as _, Result};

const FILE: &str = "pool.toml";

/// The file's modification time and size, and the set it held then.
type Cached = Option<(Option<(SystemTime, u64)>, BTreeSet<String>)>;

/// The exclusion list, cached by modification time.
pub struct PoolState {
    path: PathBuf,
    cache: Mutex<Cached>,
}

impl PoolState {
    #[must_use]
    pub fn at(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(FILE),
            cache: Mutex::new(None),
        }
    }

    /// Whether `account` has been taken out of its pool.
    #[must_use]
    pub fn excluded(&self, account: &str) -> bool {
        self.excluded_set().contains(account)
    }

    /// Every excluded account, as the file says now.
    #[must_use]
    pub fn excluded_set(&self) -> BTreeSet<String> {
        let modified = std::fs::metadata(&self.path)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        let Ok(mut cache) = self.cache.lock() else {
            return read(&self.path);
        };
        if let Some((at, set)) = cache.as_ref()
            && *at == modified
        {
            return set.clone();
        }
        let set = read(&self.path);
        *cache = Some((modified, set.clone()));
        set
    }

    /// Takes `account` into its pool, or out of it.
    pub fn set(&self, account: &str, included: bool) -> Result<()> {
        let mut set = read(&self.path);
        if included {
            set.remove(account);
        } else {
            set.insert(account.to_string());
        }
        let list = set
            .iter()
            .map(|name| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!(
            "# Accounts taken out of their pool; edited by `inference-gateway subscriptions pool`.\nexcluded = [{list}]\n"
        );
        let temporary = self
            .path
            .with_extension(format!("toml.{}", std::process::id()));
        std::fs::write(&temporary, text)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        std::fs::rename(&temporary, &self.path)
            .with_context(|| format!("could not write {}", self.path.display()))
    }
}

fn read(path: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|table| table.get("excluded").cloned())
        .and_then(|value| value.as_array().cloned())
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_leaves_and_rejoins_its_pool_and_a_reader_sees_it_live() {
        let dir = tempfile::tempdir().unwrap();
        let writer = PoolState::at(dir.path());
        let reader = PoolState::at(dir.path());
        assert!(!reader.excluded("claude-work"));
        writer.set("claude-work", false).unwrap();
        // A modification time can be coarse; a second write in the same
        // tick must still be seen, so the reader here is a fresh one.
        assert!(PoolState::at(dir.path()).excluded("claude-work"));
        assert!(!PoolState::at(dir.path()).excluded("claude-max"));
        writer.set("claude-work", true).unwrap();
        assert!(!PoolState::at(dir.path()).excluded("claude-work"));
    }
}
