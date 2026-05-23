//! Per-machine resolver allowlist persisted at
//! `$XDG_CONFIG_HOME/rimz/resolvers.toml`.
//!
//! Schema (TOML):
//!
//! ```toml
//! [[resolver]]
//! id = "opus-policy"
//! order = 10
//! budget_seconds = 30
//! binary = "/home/me/bin/opus-resolver"   # optional
//! display_name = "Opus policy"            # optional
//! ```
//!
//! `order` is the chain position (low → high). `budget_seconds` is the chain
//! budget the doc specifies per link. `binary` pins the executable path the
//! heartbeat's pid must resolve to before the bridge engages.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::{InvalidResolverId, ResolverId};
use crate::ledger::atomic;
use crate::ledger::paths::config_home;

const ALLOWLIST_FILE: &str = "resolvers.toml";
const RIMZ_CONFIG_SUBDIR: &str = "rimz";

#[derive(Debug, thiserror::Error)]
pub enum AllowlistErr {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing allowlist at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serializing allowlist: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("writing allowlist: {0}")]
    Atomic(#[from] atomic::AtomicErr),
    #[error("resolver id `{0}` is already enrolled")]
    DuplicateId(ResolverId),
    #[error("resolver id `{0}` is not enrolled")]
    NotFound(ResolverId),
    #[error(transparent)]
    InvalidId(#[from] InvalidResolverId),
}

pub type Result<T> = std::result::Result<T, AllowlistErr>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowlistEntry {
    pub id: ResolverId,
    pub order: u32,
    #[serde(rename = "budget_seconds")]
    pub budget_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub binary: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allowlist {
    #[serde(default, rename = "resolver")]
    entries: Vec<AllowlistEntry>,
}

impl Allowlist {
    /// Load the allowlist from `$XDG_CONFIG_HOME/rimz/resolvers.toml`. A
    /// missing file is an empty allowlist; the bridge gates on that.
    pub fn load() -> Result<Self> {
        Self::load_from(&default_path())
    }

    /// Save the allowlist to `$XDG_CONFIG_HOME/rimz/resolvers.toml`.
    pub fn save(&self) -> Result<()> {
        self.save_to(&default_path())
    }

    /// Test-only loader. Production callers go through [`Self::load`].
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let parsed: Self = toml::from_str(&text).map_err(|source| AllowlistErr::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;
                Ok(parsed.sorted())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(AllowlistErr::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Test-only writer. Production callers go through [`Self::save`].
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let sorted = self.clone().sorted();
        let text = toml::to_string_pretty(&sorted)?;
        atomic::write_bytes_atomically(path, text.as_bytes())?;
        Ok(())
    }

    pub fn entries(&self) -> &[AllowlistEntry] {
        &self.entries
    }

    pub fn contains(&self, id: &ResolverId) -> bool {
        self.entries.iter().any(|e| &e.id == id)
    }

    pub fn get(&self, id: &ResolverId) -> Option<&AllowlistEntry> {
        self.entries.iter().find(|e| &e.id == id)
    }

    pub fn add(&mut self, entry: AllowlistEntry) -> Result<()> {
        if self.contains(&entry.id) {
            return Err(AllowlistErr::DuplicateId(entry.id));
        }
        self.entries.push(entry);
        self.entries.sort_by(sort_key);
        Ok(())
    }

    pub fn remove(&mut self, id: &ResolverId) -> Result<()> {
        let len_before = self.entries.len();
        self.entries.retain(|e| &e.id != id);
        if self.entries.len() == len_before {
            return Err(AllowlistErr::NotFound(id.clone()));
        }
        Ok(())
    }

    /// Move `target` so it sits immediately before `pivot`. The resulting
    /// `order` is the smallest integer that keeps the requested relative
    /// position; the caller-supplied `order` values are renormalised in
    /// units of 10 so future inserts can slot between siblings.
    pub fn reorder_before(&mut self, target: &ResolverId, pivot: &ResolverId) -> Result<()> {
        self.reorder(target, pivot, Position::Before)
    }

    pub fn reorder_after(&mut self, target: &ResolverId, pivot: &ResolverId) -> Result<()> {
        self.reorder(target, pivot, Position::After)
    }

    fn reorder(&mut self, target: &ResolverId, pivot: &ResolverId, pos: Position) -> Result<()> {
        if target == pivot {
            return Ok(());
        }
        let target_idx = self
            .entries
            .iter()
            .position(|e| &e.id == target)
            .ok_or_else(|| AllowlistErr::NotFound(target.clone()))?;
        let entry = self.entries.remove(target_idx);
        let pivot_idx = self
            .entries
            .iter()
            .position(|e| &e.id == pivot)
            .ok_or_else(|| AllowlistErr::NotFound(pivot.clone()))?;
        let insert_at = match pos {
            Position::Before => pivot_idx,
            Position::After => pivot_idx + 1,
        };
        self.entries.insert(insert_at, entry);
        self.renormalise_order();
        Ok(())
    }

    fn renormalise_order(&mut self) {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            entry.order = u32::try_from((i + 1) * 10).unwrap_or(u32::MAX);
        }
    }

    fn sorted(mut self) -> Self {
        self.entries.sort_by(sort_key);
        self
    }
}

enum Position {
    Before,
    After,
}

fn sort_key(a: &AllowlistEntry, b: &AllowlistEntry) -> std::cmp::Ordering {
    a.order
        .cmp(&b.order)
        .then_with(|| a.id.as_str().cmp(b.id.as_str()))
}

fn default_path() -> PathBuf {
    config_home().join(RIMZ_CONFIG_SUBDIR).join(ALLOWLIST_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(id: &str, order: u32, secs: u64) -> AllowlistEntry {
        AllowlistEntry {
            id: id.parse().unwrap(),
            order,
            budget_seconds: secs,
            binary: None,
            display_name: None,
        }
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let list = Allowlist::load_from(&path).unwrap();
        assert!(list.entries().is_empty());
    }

    #[test]
    fn round_trip_preserves_entries_sorted_by_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("resolvers.toml");
        let mut list = Allowlist::default();
        list.add(entry("slack-on-call", 20, 300)).unwrap();
        list.add(entry("opus-policy", 10, 30)).unwrap();
        list.save_to(&path).unwrap();
        let reloaded = Allowlist::load_from(&path).unwrap();
        let ids: Vec<&str> = reloaded.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["opus-policy", "slack-on-call"]);
    }

    #[test]
    fn add_rejects_duplicate_id() {
        let mut list = Allowlist::default();
        list.add(entry("opus-policy", 10, 30)).unwrap();
        let err = list.add(entry("opus-policy", 20, 60)).unwrap_err();
        assert!(matches!(err, AllowlistErr::DuplicateId(_)));
    }

    #[test]
    fn remove_returns_not_found_when_absent() {
        let mut list = Allowlist::default();
        let err = list
            .remove(&"missing".parse::<ResolverId>().unwrap())
            .unwrap_err();
        assert!(matches!(err, AllowlistErr::NotFound(_)));
    }

    #[test]
    fn reorder_before_renormalises_order_in_tens() {
        let mut list = Allowlist::default();
        list.add(entry("opus", 10, 30)).unwrap();
        list.add(entry("slack", 20, 300)).unwrap();
        list.add(entry("pager", 30, 1800)).unwrap();
        list.reorder_before(&"pager".parse().unwrap(), &"slack".parse().unwrap())
            .unwrap();
        let ids: Vec<&str> = list.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["opus", "pager", "slack"]);
        let orders: Vec<u32> = list.entries().iter().map(|e| e.order).collect();
        assert_eq!(orders, vec![10, 20, 30]);
    }
}
