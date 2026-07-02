//! Read entry points: rebuild/publish the persisted snapshot, the
//! lock-free fresh-latest fast path, and its parse cache.

use std::fs;
use std::path::PathBuf;

use jiff::Timestamp;

use super::Result;
use super::fold::{ResumeOutcome, RollupCursor, catch_up_rollup, write_rollup_cache};
use super::view::SidebarSnapshot;
use crate::agents::AgentState;
use crate::ledger::atomic::{self};
use crate::ledger::event_log::{self};
use crate::ledger::feed_store::{self};
use crate::ledger::parse_cache::ParseCache;
use crate::ledger::paths::StatePaths;
use crate::ledger::runtime::{RuntimeProjection, RuntimeScope};
use crate::ledger::workspace_record;
use crate::workspace::RootClass;

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
    let (rollup, agents, resume_outcomes) = catch_up_rollup(paths)?;
    let snapshot = assemble_snapshot(paths, rollup.extent, agents, resume_outcomes)?;
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
    let (rollup, agents, resume_outcomes) = catch_up_rollup(paths)?;
    assemble_snapshot(paths, rollup.extent, agents, resume_outcomes)
}

/// [`build_from`] for a long-lived reader: the same projection, but the
/// rollup base rides in the caller's [`RollupCursor`] instead of being
/// re-read from `rollup.json` per call — O(new log bytes) per delta.
pub fn build_with_cursor(paths: &StatePaths, cursor: &mut RollupCursor) -> Result<SidebarSnapshot> {
    let (extent, agents, resume_outcomes) = cursor.fold(paths)?;
    assemble_snapshot(paths, extent, agents, resume_outcomes)
}

fn assemble_snapshot(
    paths: &StatePaths,
    extent: event_log::LogExtent,
    agents: Vec<AgentState>,
    resume_outcomes: Vec<ResumeOutcome>,
) -> Result<SidebarSnapshot> {
    // The one clock read this projection makes: every window verdict below
    // (reap TTLs, stall, compaction) folds against this single instant.
    let now = Timestamp::now();
    // Pending items only: the view folds nothing else, and the pending scan
    // stays O(pending) regardless of feed history.
    let items = feed_store::list_pending(&paths.feed_dir)?;
    // Apply the same runtime liveness expel the live read does, so the
    // persisted `latest.json` matches what a reader would have projected —
    // never resurrecting a dead-pid agent or an ownerless-script ask.
    let projection = RuntimeProjection::from_parts(
        items,
        std::collections::BTreeSet::new(),
        agents,
        RuntimeScope::Runtime,
    );
    let mut snapshot = SidebarSnapshot::build_with_agents(
        paths.workspace_id.clone(),
        projection.items,
        projection.agents,
        now,
    );
    snapshot.reap_stale_sessions();
    snapshot.display_name = display_name_for(paths);
    let mut snapshot = snapshot
        .with_root_class(root_class_for(paths))
        .with_project_root(project_root_for(paths));
    // Stamp the extent the fold consumed. The freshness gate compares it
    // against the live log length, so a racing append can never pass a
    // stale rollup off as current.
    snapshot.reflects_log = Some(extent);
    snapshot.resume_outcomes = Some(resume_outcomes);
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
    // The freshness-vs-log check runs below on the live mtimes; only the
    // *parse* is cached ([`ParseCache`]), keeping the 100–500 KB deserialize
    // off the CPU on a delta storm — the read itself is page-cache-hot.
    // Offset-only comparison is sound across rotations because the writer
    // retracts `latest.json` before reseeding the new generation (see
    // `rotate_event_log`), and every publish re-stamps it — so a readable
    // stamp always describes the live log, never a renamed-away one.
    let stamp_is_current = |snapshot: &SidebarSnapshot| {
        snapshot
            .reflects_log
            .is_some_and(|extent| extent.offset == log_len)
    };
    let snapshot_is_current = |snapshot: &SidebarSnapshot| {
        stamp_is_current(snapshot)
            && agent_identities_are_current(snapshot)
            && snapshot.resume_outcomes.is_some()
    };
    let len = meta.len();
    let path = paths.latest_snapshot.as_path();
    if let Some(mut snapshot) = LATEST_PARSE_CACHE.with(|cache| cache.get(path, latest_mtime, len))
    {
        // Re-stamp the projection clock at the *read* instant: the parse cache
        // can serve a clone for minutes in a quiet room, and the enrichment
        // rebuilds (stall, compaction, reset windows) must fold against the
        // reader's now, not the long-gone parse.
        snapshot.now = Timestamp::now();
        return snapshot_is_current(&snapshot).then_some(snapshot);
    }
    let bytes = fs::read(&paths.latest_snapshot).ok()?;
    let mut snapshot: SidebarSnapshot = serde_json::from_slice(&bytes).ok()?;
    snapshot.now = Timestamp::now();
    // The parse cache is identity-keyed, not a freshness verdict — a
    // stale-stamped snapshot is still worth caching so the next delta skips
    // the re-parse.
    LATEST_PARSE_CACHE.with(|cache| cache.store(path, latest_mtime, len, snapshot.clone()));
    snapshot_is_current(&snapshot).then_some(snapshot)
}

thread_local! {
    /// This thread's last `latest.json` parse — the rollup a long-lived
    /// consumer thread re-reads on every ledger delta.
    static LATEST_PARSE_CACHE: ParseCache<SidebarSnapshot> = const { ParseCache::new() };
}

fn agent_identities_are_current(snapshot: &SidebarSnapshot) -> bool {
    snapshot.agents.iter().all(|agent| {
        agent.name.as_deref().is_some_and(|name| {
            crate::harness::petname::valid_name(name)
                && !crate::harness::petname::collides_with_reserved_prefix(
                    name,
                    crate::agents::known_kinds(),
                )
        }) && agent.kind_ordinal.is_some()
    })
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

/// The room root's class from the workspace record, defaulting to `Repo` (the
/// prior grouping) when the record is missing or pre-dates the field.
pub(crate) fn root_class_for(paths: &StatePaths) -> RootClass {
    workspace_record::read(&paths.workspace_record)
        .ok()
        .map(|record| record.root_class)
        .unwrap_or(RootClass::Repo)
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    use crate::agents::AgentLifecycleObservation;
    use crate::agents::lifecycle::LifecycleSignal;
    use crate::ids::{AgentSessionId, WorkspaceId};
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

        // No captured pid: the owner is unknown, so the agent abstains and stays.
        let alive = lifecycle(&workspace, "sess-live", None);
        // A pid that cannot be live (u32::MAX): the rollup derives a dead owner,
        // which the runtime expel must suppress.
        let dead = lifecycle(&workspace, "sess-dead", Some(u32::MAX));
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
        event_log::append(&paths.events_log, &lifecycle(&workspace, "a", None)).unwrap();
        rebuild(&paths).unwrap();

        // The published stamp claims exactly the live log's length → served O(1).
        assert!(
            read_fresh_latest(&paths).is_some(),
            "stamp matches the live log → serve the published view"
        );

        // A write raced the read: the log moved past the stamp → stale, so the
        // guard declines and the caller folds the delta itself. Backdating the
        // log's mtime proves mtime carries no authority — only the stamp does.
        event_log::append(&paths.events_log, &lifecycle(&workspace, "b", None)).unwrap();
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

    #[test]
    fn read_fresh_latest_rejects_snapshots_without_card_identity() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        event_log::append(&paths.events_log, &lifecycle(&workspace, "a", None)).unwrap();
        rebuild(&paths).unwrap();

        let mut legacy = read_fresh_latest(&paths).expect("fresh snapshot");
        assert!(
            agent_identities_are_current(&legacy),
            "rebuilt snapshots carry card identity"
        );
        legacy.agents[0].name = None;
        legacy.agents[0].kind_ordinal = None;
        atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &legacy).unwrap();

        assert!(
            read_fresh_latest(&paths).is_none(),
            "old latest.json without name/ordinal is not fresh for the new CLI"
        );
        let rebuilt = build_from(&paths).unwrap();
        assert!(
            agent_identities_are_current(&rebuilt),
            "fallback rebuild backfills deterministic card identity"
        );
    }

    #[test]
    fn read_fresh_latest_rejects_snapshots_without_resume_outcomes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        event_log::append(&paths.events_log, &lifecycle(&workspace, "a", None)).unwrap();
        rebuild(&paths).unwrap();
        assert!(
            read_fresh_latest(&paths)
                .and_then(|snapshot| snapshot.resume_outcomes)
                .is_some(),
            "rebuilt snapshots carry the resume outcome migration marker"
        );

        let bytes = std::fs::read(&paths.latest_snapshot).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        legacy
            .as_object_mut()
            .expect("snapshot is an object")
            .remove("resume_outcomes");
        atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &legacy).unwrap();

        assert!(
            read_fresh_latest(&paths).is_none(),
            "old latest.json without resume outcomes is not fresh for the new CLI"
        );
        let rebuilt = build_from(&paths).unwrap();
        assert!(
            rebuilt.resume_outcomes.is_some(),
            "fallback rebuild stamps the resume outcome field"
        );
    }

    fn lifecycle(workspace: &WorkspaceId, agent_id: &str, agent_pid: Option<u32>) -> EventEnvelope {
        let observation = AgentLifecycleObservation {
            agent_id: Some(AgentSessionId::from(agent_id)),
            agent_name: None,
            role: None,
            team: None,
            channel: None,
            profile: None,
            kind_ordinal: None,
            signal: LifecycleSignal::Registered,
            agent_pid,
            agent_process_start: None,
            runtime_owner: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            origin: None,
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            turn_error: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            pane_id: None,
            parent_agent_id: None,
        };
        EventEnvelope::agent_lifecycle(
            workspace.clone(),
            "session",
            "claude",
            "SessionStart",
            &observation,
        )
    }
}
