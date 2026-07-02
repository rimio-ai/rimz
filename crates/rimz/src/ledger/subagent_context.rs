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
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::context::SubagentContext;
use crate::ids::{AgentKind, AgentSessionId};
use crate::ledger::atomic;
use crate::ledger::paths::RuntimePaths;
use crate::ledger::sidecar;

/// A child's context sidecar: the enrichment plus the `(kind, agent_id)` it is
/// filed under, so a read can confirm the key — and shrug off a digest collision
/// — instead of trusting the filename.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubagentContextRecord {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub context: SubagentContext,
}

impl sidecar::SidecarRecord for SubagentContextRecord {
    const FILE_PREFIX: &'static str = "sub";

    fn kind(&self) -> &str {
        self.kind.as_str()
    }

    fn agent_id(&self) -> &str {
        self.agent_id.as_str()
    }

    fn observed_at_secs(&self) -> i64 {
        self.context.observed_at.as_second()
    }
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
    sidecar::write_record(&runtime.subagent_context_dir, &record)
}

thread_local! {
    /// Per-thread parse cache. Every update lands via atomic rename of a
    /// freshly-written temp file, so `(mtime, len)` validates content; the
    /// long-lived consumer fetch thread re-reads these sidecars on every
    /// wakeup, and this caps its steady-state cost at one stat per file.
    static SUBAGENT_PARSE_CACHE: RefCell<HashMap<PathBuf, sidecar::ParsedSidecar<SubagentContextRecord>>> =
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
    SUBAGENT_PARSE_CACHE.with(|cache| {
        sidecar::read_all(
            &runtime.subagent_context_dir,
            cache,
            now.as_second(),
            CONTEXT_TTL_SECS,
        )
    })
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
}
