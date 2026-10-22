//! Room-scoped query counts that survive broker restarts and log compaction.

use super::{Result, registry};
use crate::disk::{
    atomic,
    lock::{SharedAppendLock, WorkspaceLock},
    paths::StatePaths,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

const COMPACT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Record {
    at_ms: u64,
    root: Option<PathBuf>,
    server: String,
    kind: String,
    outcome: String,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    count: u64,
}

fn one() -> u64 {
    1
}

fn is_one(count: &u64) -> bool {
    *count == 1
}

/// A room's usage log and its shared-append/exclusive-compaction lock.
pub struct Log {
    path: PathBuf,
    lock: PathBuf,
}

/// Answered and failed query targets, independent of broker lifetimes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub answered: u64,
    pub failed: u64,
}

/// One read of the room log, including counts without a current registry row.
#[derive(Default)]
pub struct Snapshot {
    counts: BTreeMap<(PathBuf, String), Counts>,
    pub total: Counts,
}

impl Log {
    /// Construct a handle without creating any files or directories.
    pub fn new(paths: &StatePaths) -> Self {
        let (path, lock) = paths.lsp_usage_paths();
        Self { path, lock }
    }

    /// Read the log once, skipping malformed rows and unknown outcome classes.
    pub fn read(&self) -> Snapshot {
        let mut snapshot = Snapshot::default();
        if let Err(error) = self.visit(|record| {
            snapshot.total.include(&record.outcome, record.count);
            if let Some(root) = record.root {
                snapshot
                    .counts
                    .entry((root, record.server))
                    .or_default()
                    .include(&record.outcome, record.count);
            }
        }) {
            tracing::debug!(%error, "cannot read language-server usage");
        }
        snapshot
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Query-client recording is wired in the next change."
        )
    )]
    fn append(&self, record: &Record) -> Result<()> {
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        {
            let _append = SharedAppendLock::acquire(&self.lock)?;
            atomic::append_record_bytes(&self.path, &bytes)?;
        }
        if std::fs::metadata(&self.path)?.len() > COMPACT_BYTES {
            self.compact()?;
        }
        Ok(())
    }

    fn compact(&self) -> Result<()> {
        let Some(_compact) = WorkspaceLock::try_acquire(&self.lock)? else {
            return Ok(());
        };
        let mut folded = BTreeMap::new();
        self.visit(|mut record| {
            record.root = record.root.filter(|root| root.is_dir());
            let key = (
                record.root.clone(),
                record.server.clone(),
                record.kind.clone(),
                record.outcome.clone(),
            );
            let row = folded.entry(key).or_insert_with(|| Record {
                count: 0,
                ..record.clone()
            });
            row.count = row.count.saturating_add(record.count);
            row.at_ms = row.at_ms.max(record.at_ms);
        })?;
        let mut bytes = Vec::new();
        for row in folded.into_values() {
            serde_json::to_writer(&mut bytes, &row)?;
            bytes.push(b'\n');
        }
        atomic::write_bytes_atomically(&self.path, &bytes)?;
        Ok(())
    }

    fn visit(&self, mut accept: impl FnMut(Record)) -> Result<()> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for line in BufReader::new(file).split(b'\n') {
            if let Ok(record) = serde_json::from_slice(&line?) {
                accept(record);
            }
        }
        Ok(())
    }
}

impl Counts {
    fn include(&mut self, outcome: &str, count: u64) {
        let total = match outcome {
            "results" | "empty" => &mut self.answered,
            "indexing" | "memory" | "error" => &mut self.failed,
            _ => return,
        };
        *total = total.saturating_add(count);
    }
}

impl Snapshot {
    /// Counts for this checkout and server, or zero when it has no records.
    pub fn for_entry(&self, entry: &registry::Entry) -> Counts {
        self.counts
            .get(&(entry.root.clone(), entry.server.clone()))
            .copied()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
