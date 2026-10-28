//! Read entry points: rebuild/publish the persisted snapshot, the
//! lock-free fresh-latest fast path, and its parse cache.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use jiff::Timestamp;

use super::Result;
use super::fold::{
    ResumeOutcome, RollupCursor, catch_up_rollup, lifecycle_log_generation, write_rollup_cache,
};
use super::view::{SNAPSHOT_VERSION, SidebarSnapshot};
use crate::agents::AgentState;
use crate::disk::atomic::{self};
use crate::disk::parse_cache::{ParseCache, StampedPath};
use crate::disk::paths::StatePaths;
use crate::store::event_log::{self};
use crate::store::runtime::RuntimeProjection;
use crate::workspace::RootClass;
use crate::workspace::record::{self, WorkspaceRecordErr};

/// Rebuild the snapshot caches from the live store: the rollup fold base
/// (`rollup.json`) and the derived view (`latest.json`).
/// The resulting JSON is what `rimz sidebar snapshot --json` reads on
/// attach. Caller owns a write serialization point (the workspace lock, or
/// the publish single-flight).
///
/// Cost is O(delta bytes) per call: the fold resumes from the thread's held
/// fold, or from the persisted base on the thread's first read.
/// Archived event logs are never rescanned; rotation
/// pre-projects the agent rollup into `agents.carryover.json` before advancing
/// the durable generation, so the reducer stays bounded.
pub(crate) fn rebuild(paths: &StatePaths) -> Result<SidebarSnapshot> {
    #[cfg(feature = "testkit")]
    crate::testkit::WORK_COUNTERS
        .snapshot_publishes
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (rollup, agents, resume_outcomes) = catch_up_rollup(paths)?;
    let snapshot = assemble_snapshot(paths, rollup.extent, agents.iter(), resume_outcomes)?;
    // The fold base lands first: its extent always runs at or past
    // `latest.json`'s stamp, so a crash between the two leaves a stale view
    // that the next catch-up refreshes from the newer base. Both writes are
    // cache-class — crash-durability lives in the event log; a
    // torn-after-power-cut cache parses to a miss and cold-rebuilds.
    write_rollup_cache(paths, &rollup)?;
    if let Some(path) = paths.latest_snapshot() {
        atomic::write_checkpoint(&path, &snapshot)?;
    }
    Ok(snapshot)
}

/// Build the snapshot view from the live store without persisting anything —
/// the read-only twin of [`rebuild`], safe from a lock-free reader.
pub(crate) fn build_from(paths: &StatePaths) -> Result<SidebarSnapshot> {
    let (rollup, agents, resume_outcomes) = catch_up_rollup(paths)?;
    assemble_snapshot(paths, rollup.extent, agents.iter(), resume_outcomes)
}

pub(in crate::store) fn build_from_rollup(
    paths: &StatePaths,
    rollup: &super::fold::PendingRollup,
) -> Result<SidebarSnapshot> {
    assemble_snapshot(
        paths,
        rollup.extent(),
        rollup.agents.iter(),
        rollup.resume_outcomes.clone(),
    )
}

/// Build the same projection for a long-lived reader whose rollup base rides
/// in its own [`RollupCursor`] rather than the thread's one held fold, so
/// another workspace's read on the thread cannot evict it — O(new log bytes)
/// per delta.
pub fn build_with_cursor(paths: &StatePaths, cursor: &mut RollupCursor) -> Result<SidebarSnapshot> {
    let (extent, agents, resume_outcomes) = cursor.fold(paths)?;
    assemble_snapshot(paths, extent, agents.iter(), resume_outcomes)
}

fn assemble_snapshot<'a>(
    paths: &StatePaths,
    extent: event_log::LogExtent,
    agents: impl IntoIterator<Item = &'a AgentState>,
    resume_outcomes: Vec<ResumeOutcome>,
) -> Result<SidebarSnapshot> {
    #[cfg(feature = "testkit")]
    crate::testkit::WORK_COUNTERS
        .snapshot_assemblies
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The one clock read this projection makes: every window verdict below
    // (reap TTLs, stall, compaction) folds against this single instant.
    let now = Timestamp::now();
    // Apply the same runtime liveness expel the live read does, so the
    // persisted `latest.json` matches what a reader would have projected —
    // never resurrecting a dead-pid agent.
    let RuntimeProjection {
        ended,
        expelled,
        agents,
        resume_outcomes,
    } = RuntimeProjection::runtime_from_refs(agents, resume_outcomes);
    let mut snapshot = SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), agents, now);
    snapshot.fenced_sessions = Arc::new(ended);
    Arc::make_mut(&mut snapshot.fenced_sessions).extend(expelled);
    snapshot.reap_stale_sessions();
    let identity = WorkspaceSnapshotIdentity::from_paths(paths);
    snapshot.display_name = identity.display_name.clone();
    let mut snapshot = snapshot
        .with_root_class(identity.root_class)
        .with_project_root(identity.project_root.clone());
    // Stamp the extent the fold consumed. The freshness gate compares it
    // against the live log length, so a racing append can never pass a
    // stale rollup off as current.
    snapshot.reflects_log = Some(extent);
    snapshot.resume_outcomes = Some(resume_outcomes.into());
    Ok(snapshot)
}

/// Read the pre-built `latest.json` rollup when it already reflects every event
/// in the active log.
///
/// The verdict is the embedded extent stamp: the parsed snapshot must claim
/// the durable generation and exactly the live log's byte length. The publish
/// runs after the workspace lock releases, so file mtimes carry no ordering —
/// the stamp is the one freshness authority. A write racing this read moves
/// the log past the stamp; the guard then returns `None` and the caller folds
/// the missing delta itself, so a just-appended event is never missed.
/// Lock-free and O(snapshot): a torn or absent file deserializes to `None` and
/// falls back, and the atomic rename means a readable `latest.json` is always
/// a complete rollup.
pub fn read_fresh_latest(paths: &StatePaths) -> Option<SidebarSnapshot> {
    let path = paths.latest_snapshot()?;
    fs::metadata(&path).ok()?;
    let generation = lifecycle_log_generation(paths);
    let log_len = fs::metadata(&paths.events_log)
        .map(|meta| meta.len())
        .unwrap_or(0);
    // The freshness-vs-log check runs below on the live extent; only the
    // *parse* is cached ([`ParseCache`]), keeping the 100–500 KB deserialize
    // off the CPU on a delta storm — the read itself is page-cache-hot.
    // Each runtime domain owns its copy, so generation validation also
    // rejects views another domain's boundary could not retract.
    let stamp_is_current = |snapshot: &SidebarSnapshot| {
        snapshot
            .reflects_log
            .is_some_and(|extent| extent.generation == generation && extent.offset == log_len)
    };
    let snapshot_is_current = |snapshot: &SidebarSnapshot| {
        stamp_is_current(snapshot) && snapshot.snapshot_version == SNAPSHOT_VERSION
    };
    // `latest.json` is only ever replaced by atomic rename, so the full stamp
    // (device and inode included) keys the parse: a same-tick, equal-length
    // republish is a distinct inode and never serves the prior parse, whose
    // stale extent would decline the fast path until the next publish.
    let stamped = StampedPath::of(&path);
    if let Some(snapshot) = LATEST_PARSE_CACHE.with(|cache| cache.get_stamped(&stamped)) {
        // The snapshot's projection clock is reader-local, so a shared cached
        // parse becomes owned at this mutation point.
        let mut snapshot = Arc::unwrap_or_clone(snapshot);
        // Re-stamp the projection clock at the *read* instant: the parse cache
        // can serve a clone for minutes in a quiet room, and the enrichment
        // rebuilds (stall, compaction, reset windows) must fold against the
        // reader's now, not the long-gone parse.
        snapshot.now = Timestamp::now();
        return snapshot_is_current(&snapshot).then_some(snapshot);
    }
    let bytes = fs::read(&path).ok()?;
    #[cfg(feature = "testkit")]
    crate::testkit::WORK_COUNTERS
        .latest_bytes_parsed
        .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let mut snapshot: SidebarSnapshot = serde_json::from_slice(&bytes).ok()?;
    snapshot.now = Timestamp::now();
    // The parse cache is identity-keyed, not a freshness verdict — a
    // stale-stamped snapshot is still worth caching so the next delta skips
    // the re-parse.
    LATEST_PARSE_CACHE.with(|cache| {
        cache.store_stamped(&stamped, Arc::new(snapshot.clone()));
    });
    snapshot_is_current(&snapshot).then_some(snapshot)
}

thread_local! {
    /// This thread's last `latest.json` parse — the rollup a long-lived
    /// consumer thread re-reads on every store delta.
    static LATEST_PARSE_CACHE: ParseCache<SidebarSnapshot> = const { ParseCache::new() };
    static WORKSPACE_IDENTITY_PARSE_CACHE: ParseCache<WorkspaceSnapshotIdentity> = const { ParseCache::new() };
}

pub(super) struct WorkspaceSnapshotIdentity {
    display_name: String,
    pub(super) project_root: Option<PathBuf>,
    root_class: RootClass,
}

impl WorkspaceSnapshotIdentity {
    fn fallback(paths: &StatePaths) -> Self {
        Self {
            display_name: paths.workspace_id.as_str().to_owned(),
            project_root: None,
            root_class: RootClass::Repo,
        }
    }

    pub(super) fn from_paths(paths: &StatePaths) -> Arc<Self> {
        let stamped = StampedPath::of(&paths.workspace_record);
        if let Some(identity) =
            WORKSPACE_IDENTITY_PARSE_CACHE.with(|cache| cache.get_stamped(&stamped))
        {
            return identity;
        }
        let record = match record::read(&paths.workspace_record) {
            Ok(record) => record,
            Err(WorkspaceRecordErr::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Arc::new(Self::fallback(paths));
            }
            Err(err) => {
                tracing::debug!(
                    path = %paths.workspace_record.display(),
                    error = %err,
                    "workspace record is unreadable while resolving the display name",
                );
                return Arc::new(Self::fallback(paths));
            }
        };
        let root = crate::utils::path::normalize_path_lexical(&record.project_root);
        let display_name = root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                tracing::debug!(
                    root = %root.display(),
                    "workspace project root has no usable display name",
                );
                paths.workspace_id.as_str().to_owned()
            });
        let identity = Arc::new(Self {
            display_name,
            project_root: Some(root),
            root_class: record.root_class,
        });
        WORKSPACE_IDENTITY_PARSE_CACHE
            .with(|cache| cache.store_stamped(&stamped, Arc::clone(&identity)));
        identity
    }
}

#[cfg(test)]
mod tests;
