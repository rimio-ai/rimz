//! Latest-wins per-subagent context sidecar.
//!
//! Claude's `subagentStatusLine` enrichment (a child's description, token count,
//! and start time) is written here by the feed process — one atomic file per
//! `(kind, agent_id)` child under the runtime `subagent_context/` dir — and
//! folded into the snapshot read-side by
//! [`crate::ledger::snapshot::SidebarSnapshot::with_subagent_context`]. Like its
//! [`crate::ledger::agent_context`] sibling it never touches the durable event
//! log: this is display-only latency, not truth ("Ledger first",
//! `docs/internals/ledger.md`).
//!
//! Ownership: the WRITER is always the feed process (the `rimz` CLI). The
//! sidebar renderer reads this data only through the snapshot JSON, never this
//! module, so "sidebar is read-only on the ledger" holds.

use std::fs;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::SubagentContext;
use crate::ledger::atomic::{self, write_temp_then_rename_cache};
use crate::ledger::paths::RuntimePaths;

/// A child's context sidecar: the enrichment plus the `(kind, agent_id)` it is
/// filed under, so a read can confirm the key — and shrug off a digest collision
/// — instead of trusting the filename.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentContextRecord {
    pub kind: String,
    pub agent_id: String,
    pub context: SubagentContext,
}

/// Drop a sidecar older than this even if the child's stop was missed — matched
/// to the agent-context sibling's ghost-session TTL so stale enrichment cannot
/// pin a vanished child.
const CONTEXT_TTL_SECS: i64 = 3 * 60 * 60;

/// Persist (latest-wins) one child's context. WRITER = the feed process.
/// Atomic temp+rename (no fsync — disposable sidecar) via
/// [`write_temp_then_rename_cache`].
pub fn write(
    runtime: &RuntimePaths,
    kind: &str,
    agent_id: &str,
    context: &SubagentContext,
) -> Result<(), atomic::AtomicErr> {
    let record = SubagentContextRecord {
        kind: kind.to_owned(),
        agent_id: agent_id.to_owned(),
        context: context.clone(),
    };
    write_temp_then_rename_cache(&runtime.subagent_context_path(kind, agent_id), &record)
}

/// Read every live child's context. Tolerant: an unreadable, malformed, or
/// past-TTL file is skipped, never fatal — enrichment, not correctness.
pub fn read_all(runtime: &RuntimePaths) -> Vec<SubagentContextRecord> {
    read_all_at(runtime, Timestamp::now())
}

fn read_all_at(runtime: &RuntimePaths, now: Timestamp) -> Vec<SubagentContextRecord> {
    let Ok(entries) = fs::read_dir(&runtime.subagent_context_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(record) = serde_json::from_slice::<SubagentContextRecord>(&bytes) else {
            continue;
        };
        if now.as_second() - record.context.observed_at.as_second() > CONTEXT_TTL_SECS {
            continue;
        }
        out.push(record);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn runtime() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/subctx-test"));
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    fn ctx(observed_at: Timestamp) -> SubagentContext {
        SubagentContext {
            agent_type: None,
            description: Some("locate the render seam".to_owned()),
            token_count: Some(12_400),
            started_at: Some(observed_at),
            observed_at,
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "child-1", &ctx(now)).unwrap();
        let all = read_all(&runtime);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, "claude");
        assert_eq!(all[0].agent_id, "child-1");
        assert_eq!(all[0].context.token_count, Some(12_400));
        assert_eq!(
            all[0].context.description.as_deref(),
            Some("locate the render seam")
        );
    }

    #[test]
    fn distinct_children_get_distinct_files() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "child-1", &ctx(now)).unwrap();
        write(&runtime, "claude", "child-2", &ctx(now)).unwrap();
        let mut ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["child-1".to_owned(), "child-2".to_owned()]);
    }

    #[test]
    fn corrupt_file_is_skipped() {
        let (_dir, runtime) = runtime();
        std::fs::write(
            runtime.subagent_context_dir.join("sub.bogus.json"),
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
        write(&runtime, "claude", "child-1", &ctx(stale)).unwrap();
        assert!(read_all_at(&runtime, now).is_empty());
    }
}
