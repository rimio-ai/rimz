//! Latest-wins per-session agent-context sidecar.
//!
//! High-frequency enrichment (Claude's statusline) is written here by the feed
//! process — one atomic file per `(kind, agent_id)` session under the runtime
//! `agent_context/` dir — and folded into the snapshot read-side by
//! [`crate::ledger::snapshot::SidebarSnapshot::with_agent_context`]. It never
//! touches the durable event log: this is display-only latency, not truth
//! ("Ledger first", `docs/internals/ledger.md`).
//!
//! Ownership: the WRITER is always the feed process (the `rimz` CLI). The
//! sidebar renderer reads this data only through the snapshot JSON, never this
//! module, so "sidebar is read-only on the ledger" holds.

use std::fs;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::AgentContext;
use crate::ledger::atomic::{self, write_temp_then_rename_cache};
use crate::ledger::paths::RuntimePaths;

/// A session's context sidecar: the normalized record plus the
/// `(kind, agent_id)` it is filed under, so a read can confirm the key — and
/// shrug off a digest collision — instead of trusting the filename.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentContextRecord {
    pub kind: String,
    pub agent_id: String,
    pub context: AgentContext,
}

/// Drop a sidecar older than this even if its `SessionEnd` tombstone was
/// missed — matched to the snapshot's ghost-session TTL so stale cost or
/// rate-limit data cannot pin a vanished pidless session.
const CONTEXT_TTL_SECS: i64 = 3 * 60 * 60;

/// Persist (latest-wins) one session's context. WRITER = the feed process.
/// Atomic temp+rename (no fsync — disposable sidecar) via
/// [`write_temp_then_rename_cache`].
pub fn write(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    context: &AgentContext,
) -> Result<(), atomic::AtomicErr> {
    let record = AgentContextRecord {
        kind: kind.to_owned(),
        agent_id: agent_id.to_owned(),
        context: context.clone(),
    };
    write_temp_then_rename_cache(&runtime.agent_context_path(kind, agent_id), &record)
}

/// Read every live session's context. Tolerant: an unreadable, malformed, or
/// past-TTL file is skipped, never fatal — enrichment, not correctness.
pub fn read_all(runtime: &RuntimePaths) -> Vec<AgentContextRecord> {
    read_all_at(runtime, Timestamp::now())
}

fn read_all_at(runtime: &RuntimePaths, now: Timestamp) -> Vec<AgentContextRecord> {
    let Ok(entries) = fs::read_dir(&runtime.agent_context_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(record) = serde_json::from_slice::<AgentContextRecord>(&bytes) else {
            continue;
        };
        if now.as_second() - record.context.observed_at.as_second() > CONTEXT_TTL_SECS {
            continue;
        }
        out.push(record);
    }
    out
}

/// Remove a session's sidecar (a `SessionEnd` tombstone, or reap). Best-effort:
/// a missing file is success.
pub fn remove(runtime: &RuntimePaths, kind: &str, agent_id: &str) -> std::io::Result<()> {
    match fs::remove_file(runtime.agent_context_path(kind, agent_id)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/ctx-test"));
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    fn ctx(observed_at: Timestamp) -> AgentContext {
        AgentContext {
            source: "claude".to_owned(),
            session_name: None,
            model_id: Some("claude-opus-4-8".to_owned()),
            model_display_name: None,
            effort: None,
            thinking_enabled: None,
            output_style: None,
            vim_mode: None,
            agent_version: None,
            exceeds_200k_tokens: None,
            cost: None,
            tokens: None,
            rate_limits: None,
            pr: None,
            account: None,
            turn_error: None,
            observed_at,
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "sess-1");
        assert_eq!(all[0].context.model_id.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn distinct_sessions_get_distinct_files() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        write(&runtime, "claude", "sess-2", &ctx(now)).unwrap();
        let mut ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["sess-1".to_owned(), "sess-2".to_owned()]);
    }

    #[test]
    fn corrupt_file_is_skipped() {
        let (_dir, runtime) = runtime();
        std::fs::write(
            runtime.agent_context_dir.join("ctx.bogus.json"),
            b"not json",
        )
        .unwrap();
        assert!(read_all(&runtime).is_empty());
    }

    #[test]
    fn past_ttl_record_is_skipped() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::from_second(1_700_000_000).unwrap();
        let stale = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS - 60).unwrap();
        write(&runtime, "claude", "sess-1", &ctx(stale)).unwrap();
        assert!(read_all_at(&runtime, now).is_empty());
    }

    #[test]
    fn remove_targets_one_session() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
        write(&runtime, "claude", "sess-2", &ctx(now)).unwrap();
        remove(&runtime, "claude", "sess-1").unwrap();
        let ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
        assert_eq!(ids, vec!["sess-2".to_owned()]);
        // Removing an absent session is success.
        remove(&runtime, "claude", "sess-1").unwrap();
    }
}
