//! Room-scoped query counts that survive broker restarts and log compaction.

use super::{
    Result,
    query::{Output, QueryErr},
    registry,
};
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

fn outcome(result: &std::result::Result<Output, QueryErr>) -> &'static str {
    match result {
        Ok(Output::Answer { result, .. })
            if result.is_null() || result.as_array().is_some_and(Vec::is_empty) =>
        {
            "empty"
        }
        Ok(Output::NotFound { .. }) => "empty",
        Ok(_) => "results",
        Err(QueryErr::Indexing { .. }) => "indexing",
        Err(QueryErr::MemoryShort { .. } | QueryErr::MemoryPressure { .. }) => "memory",
        Err(_) => "error",
    }
}

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
    workspace_record: PathBuf,
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
        Self {
            path,
            lock,
            workspace_record: paths.workspace_record.clone(),
        }
    }

    /// Record one selected target without changing the query's result on a write failure.
    pub fn record(
        &self,
        entry: &registry::Entry,
        kind: &str,
        result: &std::result::Result<Output, QueryErr>,
    ) {
        if let Err(error) = self.append(&Record {
            at_ms: crate::utils::time::unix_now_ms(),
            root: Some(entry.root.clone()),
            server: entry.server.clone(),
            kind: kind.into(),
            outcome: outcome(result).into(),
            count: 1,
        }) {
            tracing::debug!(%error, "cannot record language-server usage");
        }
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

    fn append(&self, record: &Record) -> Result<()> {
        if !self.room_exists() {
            return Ok(());
        }
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        {
            let _append = SharedAppendLock::acquire(&self.lock)?;
            if !self.room_exists() {
                return Ok(());
            }
            atomic::append_record_bytes(&self.path, &bytes)?;
        }
        if std::fs::metadata(&self.path)?.len() > COMPACT_BYTES {
            self.compact()?;
        }
        Ok(())
    }

    fn compact(&self) -> Result<()> {
        if !self.room_exists() {
            return Ok(());
        }
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
        if self.room_exists() {
            atomic::write_bytes_atomically(&self.path, &bytes)?;
        }
        Ok(())
    }

    fn room_exists(&self) -> bool {
        crate::workspace::record::read(&self.workspace_record).is_ok()
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
