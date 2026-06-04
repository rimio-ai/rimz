//! Per-agent activity liveness heartbeat.
//!
//! A latency hint, not ledger truth. The durable event log is turn-grained —
//! an agent's `last_activity` advances only on `SessionStart`/`UserPromptSubmit`
//! /`Stop` — so the sidebar cannot tell a busy agent (running tools silently
//! between turn boundaries) from a wedged one. This file closes that gap: the
//! agent's hook touches it on every progress-proving event (a completed tool
//! call, a turn boundary), and the snapshot folds the freshest touch into the
//! agent's `last_activity`. That gives a per-tool "the agent is doing
//! something" signal without appending a durable event — and a sidebar
//! wakeup — per tool call.
//!
//! The file is overwritten in place (one per `(kind, agent_id)`), so a live
//! agent's touch never grows the directory, and a stale touch left by a dead
//! session is simply ignored: the rollup no longer carries that agent, so there
//! is nothing to fold it onto. `rimz gc` reaps the files ended sessions leave
//! behind, like the other runtime liveness files.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::ids::{AgentKind, AgentSessionId};
use crate::ledger::RuntimePaths;
use crate::ledger::atomic;

/// One agent's most recent progress timestamp. The identity rides inside the
/// file so the reader can fold it onto the rollup; the filename is a digest of
/// the same identity, which keeps it filesystem-safe and collision-free for
/// arbitrary agent session ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub at: Timestamp,
}

fn activity_path(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0u8]);
    hasher.update(agent_id.as_bytes());
    let digest = hasher.finalize();
    let name: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    runtime.agent_activity_dir.join(format!("{name}.json"))
}

/// Record that `(kind, agent_id)` just made progress. Atomic temp-then-rename,
/// like every other runtime liveness file. Best-effort: a failed write only
/// degrades the liveness hint, never correctness, so callers log and continue.
pub fn touch(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> Result<(), atomic::AtomicErr> {
    let record = AgentActivity {
        kind: AgentKind::new_unchecked(kind),
        agent_id: agent_id.into(),
        at: Timestamp::now(),
    };
    atomic::write_temp_then_rename(&activity_path(runtime, kind, agent_id), &record)
}

fn read_one(path: PathBuf) -> Option<AgentActivity> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice::<AgentActivity>(&bytes).ok()
}

/// One parsed touch, gated by the stat that validated it. `record` is `None`
/// for a file that read or parsed as garbage, so a corrupt touch costs one
/// parse attempt, not one per tick.
struct ParsedTouch {
    mtime: SystemTime,
    len: u64,
    record: Option<AgentActivity>,
}

thread_local! {
    /// Per-thread parse cache for the keyed read. Every touch lands via atomic
    /// rename of a freshly-written temp file, so `(mtime, len)` validates
    /// content; the long-lived consumer fetch thread re-reads these touches on
    /// every wakeup, and this caps its steady-state cost at one stat per key.
    /// Pruned to the queried key set on every read, so it stays bounded by
    /// the live agent set.
    static TOUCH_PARSE_CACHE: RefCell<HashMap<PathBuf, ParsedTouch>> =
        RefCell::new(HashMap::new());
}

/// Read activity touches for the live agent identities the caller already has.
/// This is the sidebar hot path: runtime activity files are disposable and may
/// outlive their sessions until `rimz gc`, so a keyed read avoids parsing a
/// directory full of stale sidecars on every renderer tick — and the per-key
/// stat gate ([`TOUCH_PARSE_CACHE`]) caps the steady-state cost at one stat
/// per key, re-parsing only a file whose stat moved.
pub fn read_for_keys<'a>(
    runtime: &RuntimePaths,
    keys: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<AgentActivity> {
    let mut seen = BTreeSet::new();
    TOUCH_PARSE_CACHE.with_borrow_mut(|cache| {
        let touches: Vec<AgentActivity> = keys
            .into_iter()
            .filter_map(|(kind, agent_id)| {
                let path = activity_path(runtime, kind, agent_id);
                if !seen.insert(path.clone()) {
                    return None;
                }
                let Ok(meta) = fs::metadata(&path) else {
                    // A vanished touch drops out of the cache with its file.
                    cache.remove(&path);
                    return None;
                };
                let mtime = meta.modified().ok()?;
                let len = meta.len();
                match cache.get(&path) {
                    Some(parsed) if parsed.mtime == mtime && parsed.len == len => {
                        parsed.record.clone()
                    }
                    _ => {
                        let record = read_one(path.clone());
                        cache.insert(
                            path,
                            ParsedTouch {
                                mtime,
                                len,
                                record: record.clone(),
                            },
                        );
                        record
                    }
                }
            })
            .collect();
        // A dead agent's key stops being queried, so without this prune its
        // entry would outlive it for the thread's lifetime. Retaining to the
        // keys just served keeps the cache bounded by the live agent set —
        // the same discipline as the context sidecar cache.
        cache.retain(|path, _| seen.contains(path));
        touches
    })
}

/// Read every recorded activity touch. A missing dir, an unreadable file, or
/// malformed JSON is skipped — a liveness hint never blocks a snapshot.
pub fn read_all(runtime: &RuntimePaths) -> Vec<AgentActivity> {
    let entries = match fs::read_dir(&runtime.agent_activity_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            debug!(path = %runtime.agent_activity_dir.display(), error = %err, "agent-activity dir unreadable");
            return Vec::new();
        }
    };
    entries
        .flatten()
        .filter(|entry| {
            // Read only the canonical `<digest>.json`; skip the
            // `<digest>.json.tmp.<pid>.<nonce>` sibling an interrupted atomic
            // write can leave, which would otherwise deserialize as a second,
            // possibly staler, touch for the same identity.
            entry.path().extension().is_some_and(|ext| ext == "json")
        })
        .filter_map(|entry| read_one(entry.path()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use tempfile::TempDir;

    fn runtime() -> (TempDir, RuntimePaths) {
        let dir = TempDir::new().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime");
        (dir, runtime)
    }

    #[test]
    fn touch_then_read_round_trips_the_identity() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-1").expect("touch");
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "sess-1");
    }

    #[test]
    fn legacy_posture_fields_are_ignored() {
        let (_dir, runtime) = runtime();
        // Records older binaries wrote may carry posture-ish fields. They are
        // ignored so old runtime files still deserialize as liveness hints.
        std::fs::create_dir_all(&runtime.agent_activity_dir).expect("create activity dir");
        std::fs::write(
            runtime.agent_activity_dir.join("legacy.json"),
            br#"{"kind":"claude","agent_id":"sess-1","at":"2026-05-30T00:00:00Z","plan_mode":true,"posture":"plan"}"#,
        )
        .expect("write legacy record");
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "sess-1");
    }

    #[test]
    fn touch_overwrites_in_place_per_identity() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-1").expect("touch");
        touch(&runtime, "claude", "sess-1").expect("touch again");
        // One file per (kind, agent_id): the second touch overwrites the first.
        assert_eq!(read_all(&runtime).len(), 1);
        // A different identity gets its own file.
        touch(&runtime, "codex", "sess-1").expect("touch codex");
        assert_eq!(read_all(&runtime).len(), 2);
    }

    #[test]
    fn read_for_keys_reads_only_requested_identities() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-live").expect("touch live");
        touch(&runtime, "claude", "sess-stale").expect("touch stale");
        touch(&runtime, "codex", "sess-stale").expect("touch other stale");

        let all = read_for_keys(&runtime, [("claude", "sess-live")]);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "sess-live");
    }

    #[test]
    fn keyed_read_serves_an_unchanged_stat_from_cache() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-1").expect("touch");
        let first = read_for_keys(&runtime, [("claude", "sess-1")]);
        assert_eq!(first.len(), 1);
        let original_at = first[0].at;

        // Rewrite the touch in place with a different identity but identical
        // length, restoring the original mtime: the stat gate serves the
        // cached parse — the contract, since every real touch is an atomic
        // rename of a fresh temp file.
        let path = activity_path(&runtime, "claude", "sess-1");
        let bytes = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let swapped = String::from_utf8(bytes)
            .unwrap()
            .replace("sess-1", "sess-9");
        std::fs::write(&path, swapped).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(mtime).unwrap();
        drop(f);
        let cached = read_for_keys(&runtime, [("claude", "sess-1")]);
        assert_eq!(cached[0].at, original_at);
        assert_eq!(
            cached[0].agent_id, "sess-1",
            "same (mtime, len) serves the cached parse — one stat, no read"
        );

        // A moved mtime invalidates: the rewrite is now visible.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(mtime + std::time::Duration::from_secs(3))
            .unwrap();
        drop(f);
        let fresh = read_for_keys(&runtime, [("claude", "sess-1")]);
        assert_eq!(fresh[0].agent_id, "sess-9");
    }

    #[test]
    fn keyed_read_prunes_cache_entries_for_keys_no_longer_queried() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-dead").expect("touch dead");
        touch(&runtime, "claude", "sess-live").expect("touch live");
        // Prime the cache with both keys, then drop the dead one from the
        // queried set — a dead agent leaving the snapshot.
        read_for_keys(&runtime, [("claude", "sess-dead"), ("claude", "sess-live")]);
        read_for_keys(&runtime, [("claude", "sess-live")]);

        // Rewrite the dead touch in place with identical (mtime, len) — the
        // same gate `keyed_read_serves_an_unchanged_stat_from_cache` proves
        // would serve a *cached* entry. Seeing the rewrite proves the entry
        // was pruned when its key left the queried set.
        let path = activity_path(&runtime, "claude", "sess-dead");
        let bytes = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let swapped = String::from_utf8(bytes)
            .unwrap()
            .replace("sess-dead", "sess-201x");
        std::fs::write(&path, swapped).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(mtime).unwrap();
        drop(f);

        let reread = read_for_keys(&runtime, [("claude", "sess-dead")]);
        assert_eq!(
            reread[0].agent_id, "sess-201x",
            "the pruned key re-parses from disk instead of serving the stale cache"
        );
    }

    #[test]
    fn read_all_skips_interrupted_write_temp_siblings() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-1").expect("touch");
        let canonical = read_all(&runtime);
        assert_eq!(canonical.len(), 1);
        // An interrupted atomic write can leave a fully-valid-JSON
        // `<digest>.json.tmp.<pid>.<nonce>` sibling. It must not read back as a
        // second (possibly staler) touch for the same identity.
        let orphan = runtime
            .agent_activity_dir
            .join("deadbeefdeadbeef.json.tmp.1234.5678");
        std::fs::write(
            &orphan,
            serde_json::to_vec(&canonical[0]).expect("serialize"),
        )
        .expect("write orphan temp sibling");
        assert_eq!(
            read_all(&runtime).len(),
            1,
            "the .tmp sibling is filtered out by extension"
        );
    }

    #[test]
    fn read_all_skips_unreadable_files() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-1").expect("touch");
        std::fs::write(
            runtime.agent_activity_dir.join("garbage.json"),
            b"{ not json",
        )
        .expect("write garbage");
        // The valid touch still reads back; the garbage file is skipped.
        assert_eq!(read_all(&runtime).len(), 1);
    }

    #[test]
    fn absent_dir_reads_empty() {
        let (_dir, runtime) = runtime();
        assert!(read_all(&runtime).is_empty());
    }
}
