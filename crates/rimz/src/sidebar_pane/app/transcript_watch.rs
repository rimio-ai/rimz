//! Local-source fast path: refresh the context sidecar the moment a watched
//! transcript, rollout, or telemetry file grows.
//!
//! Adapters that declare transcript-tail context reach the sidecar through hook
//! pushes after progress events plus the producer's stat-gated tick backstop.
//! The host watches each live root session's transcript with a
//! filesystem watcher and runs the same stat-gated refresh on writes, so meters
//! move mid-turn. Latency only, never truth: the refresh is idempotent behind
//! its transcript-stat gate, and a watcher that fails to start degrades to the
//! producer cadence. Hibernated roster reads and producer backstop refreshes
//! follow the per-agent hold; already registered OS watches stay armed.
//!
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use notify::{RecursiveMode, Watcher};
use tracing::debug;

use crate::agents::context::record::AgentContextRecord;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::event_log::LogExtent;
use crate::{RuntimePaths, fs_watch};

/// Backoff between watcher init attempts, so a platform refusing watches
/// (inotify limits, an unsupported filesystem) never spins the thread.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(5);
/// Cadence for reconciling watched paths against the live sidecar roster —
/// new sessions gain a watch, ended sessions drop theirs.
const ROSTER_RESCAN: Duration = Duration::from_secs(5);
/// Bound coarse directory-mtime misses even while the inputs stamp is equal.
const ROSTER_RESCAN_BACKSTOP: Duration = Duration::from_secs(60);
/// Coalescing window: a burst of rollout appends within it flushes as one
/// refresh per session, bounding refresh rate during fast token streams.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// One watched local source target and the model hint its sidecar last carried,
/// threaded into the refresh for cost pricing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct WatchTarget {
    kind: String,
    login: Option<crate::ids::LoginName>,
    session_id: String,
    model_hint: Option<String>,
    open_permission_since: Option<jiff::Timestamp>,
}

impl WatchTarget {
    fn refresh(&self, runtime: &RuntimePaths) {
        crate::sidebar::refresh::refresh_session_transcript_context_from_watch(
            runtime,
            &self.kind,
            &self.session_id,
            self.model_hint.as_deref(),
            self.login.as_ref(),
            self.open_permission_since,
        );
    }
}

#[derive(Default)]
struct Roster {
    watched: BTreeMap<PathBuf, BTreeSet<WatchTarget>>,
    stamp: Option<RosterStamp>,
    checks: HashMap<(AgentKind, AgentSessionId), Instant>,
}

struct RosterStamp {
    extent: LogExtent,
    sidecar_mtime: Option<SystemTime>,
    taken_at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum RosterPass {
    Unchanged,
    Rebuilt,
}

fn roster_unchanged(
    stamp: Option<&RosterStamp>,
    extent: LogExtent,
    sidecar_mtime: Option<SystemTime>,
    now: Instant,
) -> bool {
    stamp.is_some_and(|stamp| {
        stamp.extent == extent
            && stamp.sidecar_mtime == sidecar_mtime
            && now.duration_since(stamp.taken_at) < ROSTER_RESCAN_BACKSTOP
    })
}

/// Spawn the watcher manager thread. It runs for the process lifetime; the
/// watcher handle is dropped (releasing every OS watch) on a respawn.
pub(super) fn spawn(runtime: RuntimePaths) -> JoinHandle<()> {
    std::thread::spawn(move || watch_loop(&runtime))
}

fn watch_loop(runtime: &RuntimePaths) {
    loop {
        if let Err(err) = watch_transcripts(runtime) {
            debug!(error = %err, "transcript watch failed; tick backstop remains truth");
        }
        std::thread::sleep(RESPAWN_BACKOFF);
    }
}

/// Register live transcript paths, coalesce filesystem events, and refresh
/// their sidecars. A dead event channel returns to the respawn backoff.
fn watch_transcripts(runtime: &RuntimePaths) -> notify::Result<()> {
    let state = crate::StatePaths::for_workspace(runtime.workspace_id.clone())
        .map_err(|error| notify::Error::generic(&error.to_string()))?;
    let mut cursor = crate::sidebar::consumer::RollupCursor::new();
    let (event_tx, event_rx) = mpsc::channel::<PathBuf>();
    let mut watcher = fs_watch::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                let _ = event_tx.send(path);
            }
        }
    })?;
    let mut roster = Roster::default();
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut flush_at: Option<Instant> = None;
    let mut rescan_at = Instant::now();
    loop {
        let now = Instant::now();
        if now >= rescan_at {
            reconcile_roster(runtime, &state, &mut cursor, &mut watcher, &mut roster);
            rescan_at = now + ROSTER_RESCAN;
        }
        let wake = flush_at.map_or(rescan_at, |flush| flush.min(rescan_at));
        match event_rx.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(path) => {
                pending.insert(path);
                flush_at.get_or_insert_with(|| Instant::now() + DEBOUNCE);
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The watcher's event thread is gone; respawn through the outer loop.
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if flush_at.is_some_and(|flush| Instant::now() >= flush) {
            flush_at = None;
            for target in due_refreshes(&pending, &roster.watched) {
                target.refresh(runtime);
            }
            pending.clear();
        }
    }
}

/// Reconcile OS watches with the live sidecar roster: unwatch paths whose
/// session ended, watch paths that appeared. A registration failure (the file
/// not yet on disk, an inotify limit) is logged and retried next rescan; the
/// tick backstop covers the gap.
///
/// The sidecar scan and the rebuild run only when the log extent or the
/// sidecar directory's mtime moved since the last stamp, or the stamp is
/// [`ROSTER_RESCAN_BACKSTOP`] old. Held checks wait for those rebuilds; kept
/// watches cover a thaw without reopening the gate. Only a pass whose every
/// registration succeeded stores a stamp, keeping registration retries alive
/// even while its sidecar read is deferred.
///
/// The scan reads context sidecars only for the rollup's rows that have not
/// ended (a session's sidecar is removed at `SessionEnd`), so a sidecar with
/// no live row is not watched. Deferred keys keep their cached targets and OS
/// watches. A row's arrival moves the log extent, reopening the next pass.
fn reconcile_roster<W: Watcher>(
    runtime: &RuntimePaths,
    state: &crate::StatePaths,
    cursor: &mut crate::sidebar::consumer::RollupCursor,
    watcher: &mut W,
    roster: &mut Roster,
) -> RosterPass {
    reconcile_roster_with_clock(runtime, state, cursor, watcher, roster, &Instant::now)
}

fn reconcile_roster_with_clock<W: Watcher>(
    runtime: &RuntimePaths,
    state: &crate::StatePaths,
    cursor: &mut crate::sidebar::consumer::RollupCursor,
    watcher: &mut W,
    roster: &mut Roster,
    clock: &dyn Fn() -> Instant,
) -> RosterPass {
    let Ok((extent, agents, _)) = cursor.fold(state) else {
        roster.stamp = None;
        return RosterPass::Unchanged;
    };
    let sidecar_mtime = std::fs::metadata(&runtime.agent_context_dir)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let now = clock();
    if roster_unchanged(roster.stamp.as_ref(), extent, sidecar_mtime, now) {
        return RosterPass::Unchanged;
    }
    roster.stamp = None;
    let entries = crate::store::hibernate::read_cached(state);
    let active: HashSet<_> = agents
        .iter()
        .filter(|agent| agent.ended_at.is_none())
        .map(|agent| (&agent.kind, &agent.agent_id))
        .collect();
    roster
        .checks
        .retain(|(kind, id), _| active.contains(&(kind, id)));
    let held: HashSet<_> = agents
        .iter()
        .filter(|agent| crate::harness::hibernate::hibernation_entry(agent, &entries).is_some())
        .map(|agent| (agent.kind.clone(), agent.agent_id.clone()))
        .collect();
    let mut deferred = HashSet::new();
    let keys = agents
        .iter()
        .filter(|agent| agent.ended_at.is_none())
        .map(|agent| {
            let key = (agent.kind.clone(), agent.agent_id.clone());
            let skip = !crate::sidebar::cache::hibernated_check_due(
                &mut roster.checks,
                key.clone(),
                held.contains(&key),
                clock(),
            );
            if skip {
                deferred.insert(key);
            }
            (agent.kind.as_str(), agent.agent_id.as_str(), skip)
        });
    let mut live = transcript_targets(
        &crate::store::agent_context::read_for_keys_with_holds(runtime, keys),
        agents.iter(),
    );
    for (path, targets) in &roster.watched {
        for target in targets {
            let key = (
                AgentKind::new_unchecked(target.kind.clone()),
                target.session_id.clone().into(),
            );
            if deferred.contains(&key) {
                live.entry(path.clone()).or_default().insert(target.clone());
            }
        }
    }
    let mut registration_failed = false;
    roster.watched.retain(|path, _| {
        if live.contains_key(path) {
            return true;
        }
        let _ = watcher.unwatch(path);
        false
    });
    for (path, targets) in live {
        for target in &targets {
            if target.open_permission_since.is_some()
                && !roster
                    .watched
                    .get(&path)
                    .is_some_and(|prior| prior.contains(target))
            {
                // A file event may have consumed the result before its ask folded.
                target.refresh(runtime);
            }
        }
        match roster.watched.entry(path) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Keep the one OS watch; refresh every session target sharing it.
                entry.insert(targets);
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                match watcher.watch(entry.key(), RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        entry.insert(targets);
                    }
                    Err(err) => {
                        registration_failed = true;
                        debug!(path = %entry.key().display(), error = %err, "transcript watch registration failed");
                    }
                }
            }
        }
    }
    if !registration_failed {
        roster.stamp = Some(RosterStamp {
            extent,
            sidecar_mtime,
            taken_at: now,
        });
    }
    RosterPass::Rebuilt
}

/// The local-source paths worth watching: every sidecar for an adapter that
/// declares transcript-tail context and names its source. Pure over the
/// records so the roster policy is testable without a watcher or a runtime dir.
fn transcript_targets<'a>(
    records: &[AgentContextRecord],
    agents: impl IntoIterator<Item = &'a crate::agents::AgentState> + Clone,
) -> BTreeMap<PathBuf, BTreeSet<WatchTarget>> {
    let mut targets = BTreeMap::<PathBuf, BTreeSet<WatchTarget>>::new();
    for record in records.iter().filter(|record| {
        crate::agents::spec_by_kind(record.kind.as_str())
            .is_some_and(|definition| definition.capabilities.transcript_tail_context)
    }) {
        let agent = agents
            .clone()
            .into_iter()
            .find(|agent| agent.kind == record.kind && agent.agent_id == record.agent_id);
        let Some(path) = record.transcript_path.as_deref().map(PathBuf::from) else {
            continue;
        };
        let target = WatchTarget {
            login: agent.and_then(|agent| agent.login.clone()),
            kind: record.kind.as_str().to_owned(),
            session_id: record.agent_id.as_str().to_owned(),
            model_hint: record.context.model_id.clone(),
            open_permission_since: agent.and_then(crate::agents::AgentState::open_permission_since),
        };
        targets.entry(path).or_default().insert(target);
    }
    targets
}

/// The flush decision: map the pending event paths through the roster and
/// dedupe to one refresh per session. Pure, so the coalescing policy is
/// testable without `notify` or a clock. An un-rostered path (an event that
/// raced a session ending) refreshes nothing.
fn due_refreshes(
    pending: &BTreeSet<PathBuf>,
    roster: &BTreeMap<PathBuf, BTreeSet<WatchTarget>>,
) -> Vec<WatchTarget> {
    let mut seen = BTreeSet::new();
    pending
        .iter()
        .filter_map(|path| roster.get(path))
        .flatten()
        .filter(|target| seen.insert((target.kind.clone(), target.session_id.clone())))
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "transcript_watch/tests.rs"]
mod tests;
