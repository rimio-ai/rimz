//! Per-root-session estimated active-time accumulator.
//!
//! Hook producers bracket observable work with progress and stop signals. Each
//! `(kind, agent_id)` record keeps frozen credit plus the latest open span under
//! `active-time/`; a configurable grace caps silence so a dead or blocked turn
//! cannot accrue forever. The files are runtime-sidecar projection inputs, not
//! durable event-log truth.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId};
use crate::store::atomic;
use crate::store::paths::RuntimePaths;
use crate::store::sidecar;

/// One session's frozen credit and optional live working span.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTimeRecord {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(default)]
    pub credited_ms: u64,
    pub last_progress: Timestamp,
    #[serde(default)]
    pub active: bool,
}

impl ActiveTimeRecord {
    /// Estimated active seconds at `now`, including the grace-capped open span.
    pub fn display_secs(&self, now: Timestamp, grace_secs: u32) -> u64 {
        let live_ms = if self.active {
            bounded_elapsed_ms(now, self.last_progress, grace_secs)
        } else {
            0
        };
        self.credited_ms.saturating_add(live_ms) / 1_000
    }
}

impl sidecar::SidecarRecord for ActiveTimeRecord {
    const FILE_PREFIX: &'static str = "active";

    fn kind(&self) -> &str {
        self.kind.as_str()
    }

    fn agent_id(&self) -> &str {
        self.agent_id.as_str()
    }
}

/// Open or advance a working span. A resumed inactive record starts at `at`
/// without crediting the idle gap.
pub fn record_progress(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    at: Timestamp,
    grace_secs: u32,
) -> Result<(), atomic::AtomicErr> {
    update_record(runtime, kind, agent_id, at, |record, existed| {
        if !existed {
            record.active = true;
            return true;
        }
        if at <= record.last_progress {
            return false;
        }
        if record.active {
            record.credited_ms = record.credited_ms.saturating_add(bounded_elapsed_ms(
                at,
                record.last_progress,
                grace_secs,
            ));
        } else {
            record.active = true;
        }
        record.last_progress = at;
        true
    })
    .map(|_| ())
}

/// Close a working span at `at`. Repeated stops are idempotent.
pub fn record_stop(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    at: Timestamp,
    grace_secs: u32,
) -> Result<(), atomic::AtomicErr> {
    update_record(runtime, kind, agent_id, at, |record, existed| {
        if !existed || !record.active {
            return false;
        }
        record.credited_ms = record.credited_ms.saturating_add(bounded_elapsed_ms(
            at,
            record.last_progress,
            grace_secs,
        ));
        record.active = false;
        record.last_progress = record.last_progress.max(at);
        true
    })
    .map(|_| ())
}

fn bounded_elapsed_ms(later: Timestamp, earlier: Timestamp, grace_secs: u32) -> u64 {
    if later <= earlier {
        return 0;
    }
    let elapsed_ms = u64::try_from(
        later
            .as_millisecond()
            .saturating_sub(earlier.as_millisecond()),
    )
    .unwrap_or(0);
    elapsed_ms.min(u64::from(grace_secs).saturating_mul(1_000))
}

fn update_record(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    at: Timestamp,
    update: impl FnOnce(&mut ActiveTimeRecord, bool) -> bool,
) -> Result<bool, atomic::AtomicErr> {
    let _lock = RecordLock::acquire(runtime, kind, agent_id)?;
    let prior = sidecar::read_one(&runtime.active_time_dir, kind, agent_id);
    let existed = prior.is_some();
    let mut record = prior.unwrap_or_else(|| ActiveTimeRecord {
        kind: AgentKind::new_unchecked(kind),
        agent_id: agent_id.into(),
        credited_ms: 0,
        last_progress: at,
        active: false,
    });
    if !update(&mut record, existed) {
        return Ok(false);
    }
    sidecar::write_record(&runtime.active_time_dir, &record)?;
    Ok(true)
}

struct RecordLock {
    file: File,
}

impl RecordLock {
    fn acquire(
        runtime: &RuntimePaths,
        kind: &str,
        agent_id: &str,
    ) -> Result<Self, atomic::AtomicErr> {
        std::fs::create_dir_all(&runtime.active_time_dir).map_err(|source| {
            atomic::AtomicErr::Io {
                path: runtime.active_time_dir.clone(),
                source,
            }
        })?;
        let path = sidecar::lock_path(
            &runtime.active_time_dir,
            <ActiveTimeRecord as sidecar::SidecarRecord>::FILE_PREFIX,
            kind,
            agent_id,
        );
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| atomic::AtomicErr::Io {
                path: path.clone(),
                source,
            })?;
        file.lock()
            .map_err(|source| atomic::AtomicErr::Io { path, source })?;
        Ok(Self { file })
    }
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

thread_local! {
    /// The long-lived consumer pays one stat per queried live identity and
    /// reparses only records whose atomic rename changed `(mtime, len)`.
    static ACTIVE_TIME_PARSE_CACHE:
        RefCell<HashMap<PathBuf, sidecar::ParsedSidecar<ActiveTimeRecord>>> =
        RefCell::new(HashMap::new());
}

/// Read records only for identities already present in the snapshot.
pub fn read_for_keys<'a>(
    runtime: &RuntimePaths,
    keys: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<ActiveTimeRecord> {
    let mut seen = BTreeSet::new();
    ACTIVE_TIME_PARSE_CACHE.with_borrow_mut(|cache| {
        let records = keys
            .into_iter()
            .filter_map(|(kind, agent_id)| {
                let path = sidecar::path(
                    &runtime.active_time_dir,
                    <ActiveTimeRecord as sidecar::SidecarRecord>::FILE_PREFIX,
                    kind,
                    agent_id,
                );
                if !seen.insert(path.clone()) {
                    return None;
                }
                let Ok(meta) = std::fs::metadata(&path) else {
                    cache.remove(&path);
                    return None;
                };
                let mtime = meta.modified().ok()?;
                let len = meta.len();
                let record = match cache.get(&path) {
                    Some(parsed) if parsed.mtime == mtime && parsed.len == len => {
                        parsed.record.clone()
                    }
                    _ => {
                        let record = std::fs::read(&path)
                            .ok()
                            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                        cache.insert(
                            path,
                            sidecar::ParsedSidecar {
                                mtime,
                                len,
                                record: record.clone(),
                            },
                        );
                        record
                    }
                }?;
                (record.kind.as_str() == kind && record.agent_id.as_str() == agent_id)
                    .then_some(record)
            })
            .collect();
        cache.retain(|path, _| seen.contains(path));
        records
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::ids::WorkspaceId;

    const GRACE: u32 = 180;

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime");
        (dir, runtime)
    }

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).expect("fixed timestamp")
    }

    fn record(runtime: &RuntimePaths) -> ActiveTimeRecord {
        read_for_keys(runtime, [("claude", "sess-1")])
            .into_iter()
            .next()
            .expect("active-time record")
    }

    #[test]
    fn working_spans_exclude_the_idle_gap() {
        let (_dir, runtime) = runtime();
        record_progress(&runtime, "claude", "sess-1", at(0), GRACE).unwrap();
        record_stop(&runtime, "claude", "sess-1", at(10), GRACE).unwrap();
        record_progress(&runtime, "claude", "sess-1", at(30), GRACE).unwrap();
        record_stop(&runtime, "claude", "sess-1", at(45), GRACE).unwrap();

        let record = record(&runtime);
        assert_eq!(record.display_secs(at(100), GRACE), 25);
        assert!(!record.active);
    }

    #[test]
    fn live_span_counts_to_the_grace_then_freezes() {
        let (_dir, runtime) = runtime();
        record_progress(&runtime, "claude", "sess-1", at(0), GRACE).unwrap();

        assert_eq!(record(&runtime).display_secs(at(100), GRACE), 100);
        assert_eq!(record(&runtime).display_secs(at(300), GRACE), 180);
        assert_eq!(record(&runtime).display_secs(at(600), GRACE), 180);
    }

    #[test]
    fn progress_after_long_silence_resumes_without_bridging_it() {
        let (_dir, runtime) = runtime();
        record_progress(&runtime, "claude", "sess-1", at(0), GRACE).unwrap();
        record_progress(&runtime, "claude", "sess-1", at(300), GRACE).unwrap();
        record_stop(&runtime, "claude", "sess-1", at(310), GRACE).unwrap();

        assert_eq!(record(&runtime).display_secs(at(900), GRACE), 190);
    }

    #[test]
    fn stop_duplicates_and_stale_progress_do_not_change_credit() {
        let (_dir, runtime) = runtime();
        record_progress(&runtime, "claude", "sess-1", at(10), GRACE).unwrap();
        record_progress(&runtime, "claude", "sess-1", at(10), GRACE).unwrap();
        record_progress(&runtime, "claude", "sess-1", at(5), GRACE).unwrap();
        record_stop(&runtime, "claude", "sess-1", at(20), GRACE).unwrap();
        record_stop(&runtime, "claude", "sess-1", at(30), GRACE).unwrap();
        record_progress(&runtime, "claude", "sess-1", at(20), GRACE).unwrap();

        let record = record(&runtime);
        assert_eq!(record.display_secs(at(100), GRACE), 10);
        assert_eq!(record.last_progress, at(20));
        assert!(!record.active);
    }

    #[test]
    fn stale_stop_freezes_without_moving_the_clock_backwards() {
        let (_dir, runtime) = runtime();
        record_progress(&runtime, "claude", "sess-1", at(10), GRACE).unwrap();
        record_progress(&runtime, "claude", "sess-1", at(20), GRACE).unwrap();
        record_stop(&runtime, "claude", "sess-1", at(15), GRACE).unwrap();

        let record = record(&runtime);
        assert_eq!(record.credited_ms, 10_000);
        assert_eq!(record.last_progress, at(20));
        assert!(!record.active);
    }

    #[test]
    fn concurrent_progress_updates_keep_the_full_monotonic_span() {
        let (_dir, runtime) = runtime();
        record_progress(&runtime, "claude", "sess-1", at(0), GRACE).unwrap();
        let runtime = Arc::new(runtime);
        let barrier = Arc::new(Barrier::new(17));
        let threads = (1..=16)
            .map(|second| {
                let runtime = Arc::clone(&runtime);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    record_progress(&runtime, "claude", "sess-1", at(second), GRACE).unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for thread in threads {
            thread.join().unwrap();
        }

        let record = record(&runtime);
        assert_eq!(record.credited_ms, 16_000);
        assert_eq!(record.last_progress, at(16));
    }

    #[test]
    fn record_survives_a_fresh_read_and_ignores_unknown_fields() {
        let (_dir, runtime) = runtime();
        std::fs::create_dir_all(&runtime.active_time_dir).unwrap();
        let path = sidecar::path(
            &runtime.active_time_dir,
            <ActiveTimeRecord as sidecar::SidecarRecord>::FILE_PREFIX,
            "claude",
            "sess-1",
        );
        std::fs::write(
            path,
            br#"{"kind":"claude","agent_id":"sess-1","last_progress":"1970-01-01T00:00:10Z","legacy":true}"#,
        )
        .unwrap();

        let record = record(&runtime);
        assert_eq!(record.credited_ms, 0);
        assert!(!record.active);
        assert_eq!(record.last_progress, at(10));
    }

    #[test]
    fn keyed_read_uses_the_canonical_file_and_skips_temp_siblings() {
        let (_dir, runtime) = runtime();
        record_progress(&runtime, "claude", "sess-1", at(0), GRACE).unwrap();
        let orphan = runtime
            .active_time_dir
            .join("active.deadbeef.json.tmp.1234.5678");
        std::fs::write(
            orphan,
            br#"{"kind":"claude","agent_id":"sess-1","credited_ms":999000,"last_progress":"1970-01-01T00:00:00Z","active":false}"#,
        )
        .unwrap();

        assert_eq!(record(&runtime).credited_ms, 0);
    }
}
