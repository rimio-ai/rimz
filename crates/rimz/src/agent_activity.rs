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
//! The touch also ferries the agent's latest per-tool plan-mode slider reading
//! (`Some(true)` while planning, `Some(false)` once it shift-tabs out). The
//! snapshot applies it as a clear-only override: a non-plan reading drops a
//! stale "thinking" sparkle the turn-grained log otherwise can't clear until
//! `Stop`. Same latency channel, same rule — a display hint, never truth.
//!
//! The file is overwritten in place (one per `(kind, agent_id)`), so a live
//! agent's touch never grows the directory, and a stale touch left by a dead
//! session is simply ignored: the rollup no longer carries that agent, so there
//! is nothing to fold it onto. `rimz gc` reaps the files ended sessions leave
//! behind, like the other runtime liveness files.

use std::fs;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::ledger::RuntimePaths;
use crate::ledger::atomic;

/// One agent's most recent progress timestamp. The identity rides inside the
/// file so the reader can fold it onto the rollup; the filename is a digest of
/// the same identity, which keeps it filesystem-safe and collision-free for
/// arbitrary agent session ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub kind: String,
    pub agent_id: String,
    pub at: Timestamp,
    /// The plan-mode slider reading the agent's most recent activity event
    /// carried (`permission_mode == "plan"`). `None` when the event reported no
    /// slider, or when read from a record an older binary wrote. A clear-only
    /// display hint, not truth: the snapshot uses it to drop a stale thinking
    /// sparkle when a tool ran with a non-plan slider — never to re-arm one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
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

/// Record that `(kind, agent_id)` just made progress, carrying the slider
/// reading the event reported (`plan_mode`; `None` when it named no mode).
/// Atomic temp-then-rename, like every other runtime liveness file.
/// Best-effort: a failed write only degrades the liveness hint, never
/// correctness, so callers log and continue.
pub fn touch(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    plan_mode: Option<bool>,
) -> Result<(), atomic::AtomicErr> {
    let record = AgentActivity {
        kind: kind.to_owned(),
        agent_id: agent_id.to_owned(),
        at: Timestamp::now(),
        plan_mode,
    };
    atomic::write_temp_then_rename(&activity_path(runtime, kind, agent_id), &record)
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
        .filter_map(|entry| {
            let bytes = fs::read(entry.path()).ok()?;
            serde_json::from_slice::<AgentActivity>(&bytes).ok()
        })
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
        touch(&runtime, "claude", "sess-1", None).expect("touch");
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "sess-1");
    }

    #[test]
    fn touch_round_trips_plan_mode() {
        let (_dir, runtime) = runtime();
        // A non-plan slider reading survives the write/read.
        touch(&runtime, "claude", "sess-1", Some(false)).expect("touch");
        assert_eq!(read_all(&runtime)[0].plan_mode, Some(false));
        // A planning slider reading round-trips as `Some(true)`.
        touch(&runtime, "claude", "sess-1", Some(true)).expect("touch again");
        assert_eq!(read_all(&runtime)[0].plan_mode, Some(true));
        // No slider reading round-trips as `None`.
        touch(&runtime, "claude", "sess-1", None).expect("touch none");
        assert_eq!(read_all(&runtime)[0].plan_mode, None);
    }

    #[test]
    fn older_json_without_plan_mode_field_reads_none() {
        let (_dir, runtime) = runtime();
        // A record an older binary wrote carries no `plan_mode` key. `serde`'s
        // `default` must read it back as `None` (ignored by the clear-only
        // override) rather than failing the whole touch.
        std::fs::create_dir_all(&runtime.agent_activity_dir).expect("create activity dir");
        std::fs::write(
            runtime.agent_activity_dir.join("legacy.json"),
            br#"{"kind":"claude","agent_id":"sess-1","at":"2026-05-30T00:00:00Z"}"#,
        )
        .expect("write legacy record");
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].plan_mode, None);
    }

    #[test]
    fn touch_overwrites_in_place_per_identity() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-1", None).expect("touch");
        touch(&runtime, "claude", "sess-1", None).expect("touch again");
        // One file per (kind, agent_id): the second touch overwrites the first.
        assert_eq!(read_all(&runtime).len(), 1);
        // A different identity gets its own file.
        touch(&runtime, "codex", "sess-1", None).expect("touch codex");
        assert_eq!(read_all(&runtime).len(), 2);
    }

    #[test]
    fn read_all_skips_interrupted_write_temp_siblings() {
        let (_dir, runtime) = runtime();
        touch(&runtime, "claude", "sess-1", None).expect("touch");
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
        touch(&runtime, "claude", "sess-1", None).expect("touch");
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
