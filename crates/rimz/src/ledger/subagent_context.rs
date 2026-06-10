//! Latest-wins per-subagent context sidecar.
//!
//! Claude's `subagentStatusLine` enrichment (a child's description, token count,
//! and start time) is written here by the feed process — one atomic file per
//! `(kind, agent_id)` child under the runtime `subagent_context/` dir — and
//! folded into the snapshot read-side by
//! [`crate::ledger::snapshot::SidebarSnapshot::with_subagent_context`]. Like its
//! [`crate::ledger::agent_context`] sibling it never touches the durable event
//! log: this is display-only latency, not truth ("Ledger first",
//! `docs/internals/sidebar/ledger.md`).
//!
//! Ownership: the WRITER is always the feed process (the `rimz` CLI). The
//! sidebar renderer reads this data only through the snapshot JSON, never this
//! module, so "sidebar is read-only on the ledger" holds.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::SubagentContext;
use crate::ids::{AgentKind, AgentSessionId};
use crate::ledger::atomic::{self, write_temp_then_rename_cache};
use crate::ledger::paths::RuntimePaths;

/// A child's context sidecar: the enrichment plus the `(kind, agent_id)` it is
/// filed under, so a read can confirm the key — and shrug off a digest collision
/// — instead of trusting the filename.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentContextRecord {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
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
        kind: AgentKind::new_unchecked(kind),
        agent_id: agent_id.into(),
        context: context.clone(),
    };
    write_temp_then_rename_cache(&runtime.subagent_context_path(kind, agent_id), &record)
}

/// One parsed sidecar, gated by the stat that validated it. `record` is `None`
/// for a file that read or parsed as garbage, so a corrupt sidecar costs one
/// parse attempt, not one per tick.
struct ParsedSidecar {
    mtime: SystemTime,
    len: u64,
    record: Option<SubagentContextRecord>,
}

thread_local! {
    /// Per-thread parse cache. Every update lands via atomic rename of a
    /// freshly-written temp file, so `(mtime, len)` validates content; the
    /// long-lived consumer fetch thread re-reads these sidecars on every
    /// wakeup, and this caps its steady-state cost at one stat per file.
    static SUBAGENT_PARSE_CACHE: RefCell<HashMap<PathBuf, ParsedSidecar>> =
        RefCell::new(HashMap::new());
}

/// Read every live child's context. Tolerant: an unreadable, malformed, or
/// past-TTL file is skipped, never fatal — enrichment, not correctness.
/// Steady-state cost on a long-lived thread is one stat per file; only a
/// changed file re-reads and re-parses (see [`SUBAGENT_PARSE_CACHE`]).
pub fn read_all(runtime: &RuntimePaths) -> Vec<SubagentContextRecord> {
    read_all_at(runtime, Timestamp::now())
}

fn read_all_at(runtime: &RuntimePaths, now: Timestamp) -> Vec<SubagentContextRecord> {
    let Ok(entries) = fs::read_dir(&runtime.subagent_context_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    SUBAGENT_PARSE_CACHE.with_borrow_mut(|cache| {
        let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            let len = meta.len();
            seen.insert(path.clone());
            let record = match cache.get(&path) {
                Some(parsed) if parsed.mtime == mtime && parsed.len == len => parsed.record.clone(),
                _ => {
                    let record = fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    cache.insert(
                        path,
                        ParsedSidecar {
                            mtime,
                            len,
                            record: record.clone(),
                        },
                    );
                    record
                }
            };
            let Some(record) = record else { continue };
            // The TTL is evaluated fresh per read — a cached record still ages out.
            if now.as_second() - record.context.observed_at.as_second() > CONTEXT_TTL_SECS {
                continue;
            }
            out.push(record);
        }
        // Drop cache keys whose files vanished (child stop, gc), so the cache
        // stays bounded by the live sidecar set.
        cache.retain(|path, _| seen.contains(path));
    });
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

    #[test]
    fn unchanged_stat_skips_the_reparse() {
        let (_dir, runtime) = runtime();
        let now = Timestamp::now();
        write(&runtime, "claude", "child-1", &ctx(now)).unwrap();
        let first = read_all(&runtime);
        assert_eq!(first[0].agent_id, "child-1");

        // Rewrite the file in place with a different identity but identical
        // length, restoring the original mtime: the stat gate cannot tell it
        // changed, so the cached parse is served — which is exactly the
        // contract (every real update is an atomic rename of a fresh temp
        // file, so a same-stat file is byte-identical in production).
        let path = runtime.subagent_context_path("claude", "child-1");
        let original = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let swapped = String::from_utf8(original)
            .unwrap()
            .replace("child-1", "child-9");
        std::fs::write(&path, swapped).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(mtime).unwrap();
        drop(f);
        assert_eq!(
            read_all(&runtime)[0].agent_id,
            "child-1",
            "same (mtime, len) serves the cached parse — one stat, no read"
        );

        // A moved mtime invalidates: the rewrite is now visible.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(mtime + std::time::Duration::from_secs(3))
            .unwrap();
        drop(f);
        assert_eq!(read_all(&runtime)[0].agent_id, "child-9");
    }
}
