//! Local-source fast path: refresh the context sidecar the moment a watched
//! transcript, rollout, or telemetry file grows.
//!
//! Adapters that declare transcript-tail context reach the sidecar through hook
//! pushes after progress events plus the producer's stat-gated tick backstop.
//! The host watches each live root session's transcript with a
//! filesystem watcher and runs the same stat-gated refresh on writes, so meters
//! move mid-turn. Latency only, never truth: the refresh is idempotent behind
//! its transcript-stat gate, the tick backstop stays unconditional, and a
//! watcher that fails to start degrades to the producer cadence.
//!
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use notify::{RecursiveMode, Watcher};
use tracing::debug;

use crate::RuntimePaths;
use crate::agents::context::record::AgentContextRecord;
use crate::store::event_log::LogExtent;

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
}

#[derive(Default)]
struct Roster {
    watched: BTreeMap<PathBuf, BTreeSet<WatchTarget>>,
    stamp: Option<RosterStamp>,
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
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
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
                crate::sidebar::refresh::refresh_session_transcript_context_from_watch(
                    runtime,
                    &target.kind,
                    &target.session_id,
                    target.model_hint.as_deref(),
                    target.login.as_ref(),
                );
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
/// [`ROSTER_RESCAN_BACKSTOP`] old. Only a pass whose every registration
/// succeeded stores a stamp, which is what keeps that retry alive.
fn reconcile_roster<W: Watcher>(
    runtime: &RuntimePaths,
    state: &crate::StatePaths,
    cursor: &mut crate::sidebar::consumer::RollupCursor,
    watcher: &mut W,
    roster: &mut Roster,
) -> RosterPass {
    let Ok((extent, agents, _)) = cursor.fold(state) else {
        roster.stamp = None;
        return RosterPass::Unchanged;
    };
    let sidecar_mtime = std::fs::metadata(&runtime.agent_context_dir)
        .ok()
        .and_then(|meta| meta.modified().ok());
    let now = Instant::now();
    if roster_unchanged(roster.stamp.as_ref(), extent, sidecar_mtime, now) {
        return RosterPass::Unchanged;
    }
    roster.stamp = None;
    let live = transcript_targets(
        &crate::store::agent_context::read_all(runtime),
        agents.iter(),
    );
    let mut registration_failed = false;
    roster.watched.retain(|path, _| {
        if live.contains_key(path) {
            return true;
        }
        let _ = watcher.unwatch(path);
        false
    });
    for (path, targets) in live {
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
mod tests {
    use super::*;
    use crate::agents::context::AgentContext;

    fn context(kind: &str) -> AgentContext {
        AgentContext::new(kind, jiff::Timestamp::UNIX_EPOCH)
    }

    fn target(kind: &str, session_id: &str) -> WatchTarget {
        WatchTarget {
            login: None,
            kind: kind.to_owned(),
            session_id: session_id.to_owned(),
            model_hint: None,
        }
    }

    fn roster(entries: &[(&str, &str, &str)]) -> BTreeMap<PathBuf, BTreeSet<WatchTarget>> {
        let mut roster = BTreeMap::<PathBuf, BTreeSet<WatchTarget>>::new();
        for (path, kind, session) in entries {
            roster
                .entry(PathBuf::from(path))
                .or_default()
                .insert(target(kind, session));
        }
        roster
    }

    fn fixture() -> (tempfile::TempDir, crate::StatePaths, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let state = crate::StatePaths::under(id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(id, dir.path()).unwrap();
        state.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        crate::testkit::fleet::seed_fleet_store(&state, 1, 1).unwrap();
        (dir, state, runtime)
    }

    fn write_sidecar(runtime: &RuntimePaths, session: &str, source: &str) -> PathBuf {
        let mut record = AgentContextRecord::new("codex", session, context("codex"));
        record.transcript_path = Some(source.to_owned());
        let path = crate::store::agent_context::path_for(runtime, "codex", session);
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        path
    }

    fn stamp_directory(runtime: &RuntimePaths, seconds: u64) {
        std::fs::File::open(&runtime.agent_context_dir)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn roster_rescan_skips_unchanged_inputs_and_rebuilds_after_add_and_remove() {
        let (_dir, state, runtime) = fixture();
        write_sidecar(&runtime, "first", "/t/first.jsonl");
        stamp_directory(&runtime, 1);
        let mut cursor = crate::sidebar::consumer::RollupCursor::new();
        let mut watcher = notify::NullWatcher;
        let mut roster = Roster::default();
        assert_eq!(
            reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster),
            RosterPass::Rebuilt
        );
        let first = BTreeMap::from([(
            PathBuf::from("/t/first.jsonl"),
            BTreeSet::from([target("codex", "first")]),
        )]);
        assert_eq!(roster.watched, first);
        assert_eq!(
            reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster),
            RosterPass::Unchanged
        );
        assert_eq!(roster.watched, first);
        let second_path = write_sidecar(&runtime, "second", "/t/second.jsonl");
        stamp_directory(&runtime, 2);
        assert_eq!(
            reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster),
            RosterPass::Rebuilt
        );
        let mut both = first.clone();
        both.insert(
            PathBuf::from("/t/second.jsonl"),
            BTreeSet::from([target("codex", "second")]),
        );
        assert_eq!(roster.watched, both);
        std::fs::remove_file(second_path).unwrap();
        stamp_directory(&runtime, 3);
        assert_eq!(
            reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster),
            RosterPass::Rebuilt
        );
        assert_eq!(roster.watched, first);
    }

    #[test]
    fn roster_stamp_is_fresh_only_for_equal_inputs_before_the_backstop() {
        let now = Instant::now();
        let extent = LogExtent {
            generation: 2,
            offset: 10,
        };
        let mtime = SystemTime::UNIX_EPOCH;
        let stamp = RosterStamp {
            extent,
            sidecar_mtime: Some(mtime),
            taken_at: now,
        };
        assert!(roster_unchanged(Some(&stamp), extent, Some(mtime), now));
        assert!(!roster_unchanged(None, extent, Some(mtime), now));
        for changed in [
            LogExtent {
                generation: 3,
                ..extent
            },
            LogExtent {
                offset: 11,
                ..extent
            },
        ] {
            assert!(!roster_unchanged(Some(&stamp), changed, Some(mtime), now));
        }
        assert!(!roster_unchanged(Some(&stamp), extent, None, now));
        assert!(!roster_unchanged(
            Some(&stamp),
            extent,
            Some(mtime + Duration::from_secs(1)),
            now
        ));
        assert!(roster_unchanged(
            Some(&stamp),
            extent,
            Some(mtime),
            now + ROSTER_RESCAN_BACKSTOP - Duration::from_nanos(1)
        ));
        assert!(!roster_unchanged(
            Some(&stamp),
            extent,
            Some(mtime),
            now + ROSTER_RESCAN_BACKSTOP
        ));
        assert!(!roster_unchanged(
            Some(&stamp),
            extent,
            Some(mtime),
            now + ROSTER_RESCAN_BACKSTOP + Duration::from_nanos(1)
        ));
        let missing = RosterStamp {
            sidecar_mtime: None,
            ..stamp
        };
        assert!(roster_unchanged(Some(&missing), extent, None, now));
    }

    #[derive(Default)]
    struct RefusingWatcher {
        attempts: usize,
    }

    impl Watcher for RefusingWatcher {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Ok(Self::default())
        }

        fn watch(&mut self, _: &std::path::Path, _: RecursiveMode) -> notify::Result<()> {
            self.attempts += 1;
            Err(notify::Error::generic("registration refused"))
        }

        fn unwatch(&mut self, _: &std::path::Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    #[test]
    fn failed_registration_keeps_the_roster_gate_open() {
        let (_dir, state, runtime) = fixture();
        write_sidecar(&runtime, "first", "/t/first.jsonl");
        let mut cursor = crate::sidebar::consumer::RollupCursor::new();
        let mut watcher = RefusingWatcher::default();
        let mut roster = Roster::default();
        for _ in 0..2 {
            assert_eq!(
                reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster),
                RosterPass::Rebuilt
            );
            assert!(roster.watched.is_empty());
            assert!(roster.stamp.is_none());
        }
        assert_eq!(watcher.attempts, 2);
    }

    #[test]
    fn many_events_for_one_path_flush_one_refresh() {
        let roster = roster(&[("/t/a.jsonl", "codex", "sess-a")]);
        let pending: BTreeSet<PathBuf> = [PathBuf::from("/t/a.jsonl")].into();
        assert_eq!(
            due_refreshes(&pending, &roster),
            vec![target("codex", "sess-a")]
        );
    }

    #[test]
    fn two_paths_one_session_dedupe_to_one_refresh() {
        let roster = roster(&[
            ("/t/a.jsonl", "codex", "sess-a"),
            ("/t/a2.jsonl", "codex", "sess-a"),
        ]);
        let pending: BTreeSet<PathBuf> =
            [PathBuf::from("/t/a.jsonl"), PathBuf::from("/t/a2.jsonl")].into();
        assert_eq!(
            due_refreshes(&pending, &roster),
            vec![target("codex", "sess-a")]
        );
    }

    #[test]
    fn one_path_two_sessions_refreshes_each_once() {
        let roster = roster(&[
            ("/t/shared.jsonl", "copilot", "sess-a"),
            ("/t/shared.jsonl", "copilot", "sess-b"),
        ]);
        let pending: BTreeSet<PathBuf> = [PathBuf::from("/t/shared.jsonl")].into();
        assert_eq!(
            due_refreshes(&pending, &roster),
            vec![target("copilot", "sess-a"), target("copilot", "sess-b")]
        );
    }

    #[test]
    fn unrostered_path_refreshes_nothing() {
        let roster = roster(&[("/t/a.jsonl", "codex", "sess-a")]);
        let pending: BTreeSet<PathBuf> = [PathBuf::from("/t/gone.jsonl")].into();
        assert!(due_refreshes(&pending, &roster).is_empty());
    }

    #[test]
    fn empty_pending_refreshes_nothing() {
        let roster = roster(&[("/t/a.jsonl", "codex", "sess-a")]);
        assert!(due_refreshes(&BTreeSet::new(), &roster).is_empty());
    }

    #[test]
    fn targets_keep_capable_records_that_name_a_transcript() {
        let mut with_path = AgentContextRecord::new("codex", "sess-a", context("codex"));
        with_path.transcript_path = Some("/t/a.jsonl".to_owned());
        with_path.context.model_id = Some("gpt-5.5-codex".to_owned());
        let pathless = AgentContextRecord::new("codex", "sess-b", context("codex"));
        let mut copilot = AgentContextRecord::new("copilot", "sess-g", context("copilot"));
        copilot.transcript_path = Some("/t/g.jsonl".to_owned());
        let mut droid = AgentContextRecord::new("droid", "sess-d", context("droid"));
        droid.transcript_path = Some("/t/d.settings.json".to_owned());
        let mut claude = AgentContextRecord::new("claude", "sess-c", context("claude"));
        claude.transcript_path = Some("/t/c.jsonl".to_owned());

        let records = [with_path, pathless, copilot, droid, claude];
        let mut agents: Vec<_> = records
            .iter()
            .map(|record| {
                crate::testkit::agent_state(
                    record.kind.as_str(),
                    record.agent_id.as_str(),
                    jiff::Timestamp::UNIX_EPOCH,
                )
            })
            .collect();
        agents[0].login = Some("work".parse().unwrap());
        agents.retain(|agent| agent.kind != "droid");
        let targets = transcript_targets(&records, &agents);
        assert_eq!(
            targets,
            BTreeMap::from([
                (
                    PathBuf::from("/t/a.jsonl"),
                    BTreeSet::from([WatchTarget {
                        kind: "codex".to_owned(),
                        login: Some("work".parse().unwrap()),
                        session_id: "sess-a".to_owned(),
                        model_hint: Some("gpt-5.5-codex".to_owned()),
                    }])
                ),
                (
                    PathBuf::from("/t/d.settings.json"),
                    BTreeSet::from([WatchTarget {
                        kind: "droid".to_owned(),
                        login: None,
                        session_id: "sess-d".to_owned(),
                        model_hint: None,
                    }])
                ),
                (
                    PathBuf::from("/t/g.jsonl"),
                    BTreeSet::from([WatchTarget {
                        kind: "copilot".to_owned(),
                        login: None,
                        session_id: "sess-g".to_owned(),
                        model_hint: None,
                    }])
                )
            ])
        );
    }
}
