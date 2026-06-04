//! Read entry points: rebuild/publish the persisted snapshot, the
//! lock-free fresh-latest fast path, and its parse cache.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use jiff::Timestamp;

use super::Result;
use super::fold::{catch_up_rollup, write_rollup_cache};
use super::view::SidebarSnapshot;
use crate::feed::AgentState;
use crate::ledger::atomic::{self};
use crate::ledger::event_log::{self};
use crate::ledger::feed_store::{self};
use crate::ledger::paths::StatePaths;
use crate::ledger::runtime::{RuntimeProjection, RuntimeScope};
use crate::ledger::workspace_record;

/// Rebuild the snapshot caches from the live ledger and persist both: the
/// rollup fold base (`rollup.json`) and the derived view (`latest.json`).
/// The resulting JSON is what `rimz sidebar snapshot --json` reads on
/// attach. Caller owns a write serialization point (the workspace lock, or
/// the publish single-flight).
///
/// Cost is O(delta bytes + pending items) per call: the fold resumes from
/// the persisted base. Archived event logs are never rescanned; rotation
/// pre-projects the agent rollup into `agents.carryover.json` and reseeds
/// the fold base, so the reducer stays bounded.
pub(crate) fn rebuild(paths: &StatePaths) -> Result<SidebarSnapshot> {
    let (rollup, agents) = catch_up_rollup(paths)?;
    let snapshot = assemble_snapshot(paths, rollup.extent, agents)?;
    // The fold base lands first: its extent always runs at or past
    // `latest.json`'s stamp, so a crash between the two leaves a stale view
    // that the next catch-up refreshes from the newer base. Both writes are
    // cache-class — crash-durability lives in the event log and the feed
    // files; a torn-after-power-cut cache parses to a miss and cold-rebuilds.
    write_rollup_cache(&paths.rollup_cache, &rollup)?;
    atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &snapshot)?;
    Ok(snapshot)
}

/// Build the snapshot view from the live ledger without persisting anything —
/// the read-only twin of [`rebuild`], safe from a lock-free reader.
pub fn build_from(paths: &StatePaths) -> Result<SidebarSnapshot> {
    let (rollup, agents) = catch_up_rollup(paths)?;
    assemble_snapshot(paths, rollup.extent, agents)
}

fn assemble_snapshot(
    paths: &StatePaths,
    extent: event_log::LogExtent,
    agents: Vec<AgentState>,
) -> Result<SidebarSnapshot> {
    // Pending items only: the view folds nothing else, and the pending scan
    // stays O(pending) regardless of feed history.
    let items = feed_store::list_pending(&paths.feed_dir)?;
    // Apply the same runtime liveness expel the live read does, so the
    // persisted `latest.json` matches what a reader would have projected —
    // never resurrecting a dead-pid agent or an ownerless-script ask.
    let projection =
        RuntimeProjection::from_parts(items, Vec::new(), agents, RuntimeScope::Runtime);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        paths.workspace_id.clone(),
        projection.items,
        projection.agents,
    );
    snapshot.reap_stale_sessions(Timestamp::now());
    snapshot.display_name = display_name_for(paths);
    let mut snapshot = snapshot.with_project_root(project_root_for(paths));
    // Stamp the extent the fold consumed. The freshness gate compares it
    // against the live log length, so a racing append can never pass a
    // stale rollup off as current.
    snapshot.reflects_log = Some(extent);
    Ok(snapshot)
}

/// Read the pre-built `latest.json` rollup when it already reflects every event
/// in the active log.
///
/// The verdict is the embedded extent stamp: the parsed snapshot must claim
/// exactly the live log's byte length. The publish runs after the workspace
/// lock releases, so file mtimes carry no ordering — the stamp is the one
/// freshness authority. A write racing this read moves the log past the
/// stamp; the guard then returns `None` and the caller folds the missing
/// delta itself, so a just-appended event is never missed. Lock-free and
/// O(snapshot): a torn or absent file deserializes to `None` and falls back,
/// and the atomic rename means a readable `latest.json` is always a complete
/// rollup.
pub fn read_fresh_latest(paths: &StatePaths) -> Option<SidebarSnapshot> {
    let meta = fs::metadata(&paths.latest_snapshot).ok()?;
    let latest_mtime = meta.modified().ok()?;
    let log_len = fs::metadata(&paths.events_log)
        .map(|meta| meta.len())
        .unwrap_or(0);
    // The freshness-vs-log check ran above on the live mtimes; only the *parse*
    // is cached. A consumer tab folds `latest.json` on every ledger delta, so
    // skipping the 100–500 KB deserialize when the file is byte-identical to this
    // thread's last read (same path, mtime, len) keeps the rollup off the CPU on a
    // delta storm — the read itself is page-cache-hot. Same (path, mtime, len)
    // trade-off the `snapshot.json` parse cache accepts; an atomic-rename republish
    // changes both mtime and len, so a stale parse cannot be served.
    let stamp_is_current = |snapshot: &SidebarSnapshot| {
        snapshot
            .reflects_log
            .is_some_and(|extent| extent.offset == log_len)
    };
    let len = meta.len();
    let path = paths.latest_snapshot.as_path();
    let cached = LATEST_PARSE_CACHE.with_borrow(|slot| {
        slot.as_ref().and_then(|entry| {
            (entry.path == path && entry.mtime == latest_mtime && entry.len == len)
                .then(|| entry.snapshot.clone())
        })
    });
    if let Some(snapshot) = cached {
        return stamp_is_current(&snapshot).then_some(snapshot);
    }
    let bytes = fs::read(&paths.latest_snapshot).ok()?;
    let snapshot: SidebarSnapshot = serde_json::from_slice(&bytes).ok()?;
    // The parse cache is identity-keyed (path, mtime, len), not a freshness
    // verdict — a stale-stamped snapshot is still worth caching so the next
    // delta skips the re-parse.
    LATEST_PARSE_CACHE.with_borrow_mut(|slot| {
        *slot = Some(ParsedLatest {
            path: path.to_path_buf(),
            mtime: latest_mtime,
            len,
            snapshot: snapshot.clone(),
        });
    });
    stamp_is_current(&snapshot).then_some(snapshot)
}

/// One thread's last parse of `latest.json`, keyed by path + identity (mtime,
/// len) — the read-side twin of `sidebar::snapshot`'s `snapshot.json` parse
/// cache, for the rollup a long-lived consumer thread re-reads each delta.
struct ParsedLatest {
    path: PathBuf,
    mtime: SystemTime,
    len: u64,
    snapshot: SidebarSnapshot,
}

thread_local! {
    static LATEST_PARSE_CACHE: std::cell::RefCell<Option<ParsedLatest>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn display_name_for(paths: &StatePaths) -> String {
    workspace_record::read(&paths.workspace_record)
        .ok()
        .and_then(|record| {
            record
                .project_root
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| paths.workspace_id.as_str().to_owned())
}

pub(crate) fn project_root_for(paths: &StatePaths) -> Option<PathBuf> {
    workspace_record::read(&paths.workspace_record)
        .ok()
        .map(|record| record.project_root)
}

#[cfg(test)]
mod tests {

    use super::*;

    use crate::ids::WorkspaceId;

    use crate::schema::event::EventEnvelope;

    #[cfg(target_os = "linux")]
    #[test]
    fn build_from_expels_dead_pid_agent_like_the_live_read() {
        // `latest.json` is written by `build_from`; it must apply the same
        // runtime liveness expel as `Ledger::snapshot` (`runtime_projection`),
        // or serving it O(1) would resurrect a dead-pid agent the live read
        // suppresses. A live (ownerless, abstaining) agent must survive, so the
        // filter expels without over-filtering.
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();

        let lifecycle = |params: serde_json::Value| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                params,
            )
        };
        // No captured pid: the owner is unknown, so the agent abstains and stays.
        let alive = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-live",
            "status": "idle",
        }));
        // A pid that cannot be live (u32::MAX): the rollup derives a dead owner,
        // which the runtime expel must suppress.
        let dead = lifecycle(serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-dead",
            "status": "idle",
            "agent_pid": u32::MAX,
        }));
        event_log::append(&paths.events_log, &alive).unwrap();
        event_log::append(&paths.events_log, &dead).unwrap();

        let snapshot = build_from(&paths).unwrap();
        let ids: Vec<&str> = snapshot
            .agents
            .iter()
            .map(|a| a.agent_id.as_str())
            .collect();
        assert!(
            ids.contains(&"sess-live"),
            "an ownerless (abstaining) agent must survive: {ids:?}"
        );
        assert!(
            !ids.contains(&"sess-dead"),
            "a dead-pid agent must be expelled so latest.json matches the live read: {ids:?}"
        );
    }

    #[test]
    fn read_fresh_latest_serves_only_when_it_reflects_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();

        // Absent `latest.json`: nothing to serve, so the caller re-projects.
        assert!(read_fresh_latest(&paths).is_none());

        // Seed an event log and a rebuilt `latest.json`.
        let lifecycle = |agent_id: &str| {
            EventEnvelope::new(
                workspace.clone(),
                "session",
                "claude",
                "agent-hook",
                "agent.lifecycle",
                serde_json::json!({
                    "event_name": "SessionStart",
                    "agent_id": agent_id,
                    "status": "idle",
                }),
            )
        };
        event_log::append(&paths.events_log, &lifecycle("a")).unwrap();
        rebuild(&paths).unwrap();

        // The published stamp claims exactly the live log's length → served O(1).
        assert!(
            read_fresh_latest(&paths).is_some(),
            "stamp matches the live log → serve the published view"
        );

        // A write raced the read: the log moved past the stamp → stale, so the
        // guard declines and the caller folds the delta itself. Backdating the
        // log's mtime proves mtime carries no authority — only the stamp does.
        event_log::append(&paths.events_log, &lifecycle("b")).unwrap();
        std::fs::File::open(&paths.events_log)
            .unwrap()
            .set_modified(SystemTime::now() - std::time::Duration::from_secs(60))
            .unwrap();
        assert!(
            read_fresh_latest(&paths).is_none(),
            "log outran the stamp → a just-appended event is unreflected; re-project"
        );

        // Republishing catches the stamp up; the guard serves again.
        rebuild(&paths).unwrap();
        assert!(
            read_fresh_latest(&paths).is_some(),
            "republish reflects the appended event → served again"
        );
    }
}
