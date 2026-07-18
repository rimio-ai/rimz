//! Local-source fast path: refresh the context sidecar the moment a watched
//! transcript, rollout, or telemetry file grows.
//!
//! Adapters that declare transcript-tail context reach the sidecar through hook
//! pushes after progress events plus the producer's stat-gated tick backstop.
//! The elected producer watches each live root session's transcript with a
//! filesystem watcher and runs the same stat-gated refresh on writes, so meters
//! move mid-turn. Latency only, never truth: the refresh is idempotent behind
//! its transcript-stat gate, the tick backstop stays unconditional, and a
//! watcher that fails to start degrades to the producer cadence.
//!
//! One watcher per workspace: only the eldest live instance (the same
//! election as the produce path) registers paths; the rest sleep on the
//! election poll. Demotion is rare, so it is re-checked per flush and per
//! roster rescan rather than mid-block.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use tracing::debug;

use crate::RuntimePaths;
use crate::sidebar::ProducerElectionTracker;
use crate::store::agent_context::AgentContextRecord;

/// Idle cadence for the producer-election re-check while not elected.
const ELECTION_POLL: Duration = Duration::from_secs(5);
/// Backoff between watcher init attempts, so a platform refusing watches
/// (inotify limits, an unsupported filesystem) never spins the thread.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(5);
/// Cadence for reconciling watched paths against the live sidecar roster —
/// new sessions gain a watch, ended sessions drop theirs.
const ROSTER_RESCAN: Duration = Duration::from_secs(5);
/// Coalescing window: a burst of rollout appends within it flushes as one
/// refresh per session, bounding refresh rate during fast token streams.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// One watched local source target and the model hint its sidecar last carried,
/// threaded into the refresh for cost pricing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct WatchTarget {
    kind: String,
    session_id: String,
    model_hint: Option<String>,
}

/// Spawn the watcher manager thread. It runs for the process lifetime; the
/// watcher handle is dropped (releasing every OS watch) whenever the instance
/// is not the producer.
pub(super) fn spawn(runtime: RuntimePaths, election: ProducerElectionTracker) -> JoinHandle<()> {
    std::thread::spawn(move || watch_loop(&runtime, &election))
}

fn watch_loop(runtime: &RuntimePaths, election: &ProducerElectionTracker) {
    loop {
        if !is_producer(election) {
            std::thread::sleep(ELECTION_POLL);
            continue;
        }
        if let Err(err) = watch_while_elected(runtime, election) {
            debug!(error = %err, "transcript watch failed; tick backstop remains truth");
        }
        std::thread::sleep(RESPAWN_BACKOFF);
    }
}

/// Own the watcher while elected: register live transcript paths, coalesce
/// fs events behind [`DEBOUNCE`], and run the stat-gated sidecar refresh per
/// flushed session. Returns on demotion (dropping the watcher and its OS
/// watches) or on a dead event channel (the outer loop respawns with backoff).
fn watch_while_elected(
    runtime: &RuntimePaths,
    election: &ProducerElectionTracker,
) -> notify::Result<()> {
    let (event_tx, event_rx) = mpsc::channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                let _ = event_tx.send(path);
            }
        }
    })?;
    let mut roster: BTreeMap<PathBuf, BTreeSet<WatchTarget>> = BTreeMap::new();
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut flush_at: Option<Instant> = None;
    let mut rescan_at = Instant::now();
    loop {
        let now = Instant::now();
        if now >= rescan_at {
            // Demotion check per rescan: a demoted instance releases every
            // watch by dropping the watcher and returns to the election poll.
            if !is_producer(election) {
                return Ok(());
            }
            reconcile_roster(runtime, &mut watcher, &mut roster);
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
            // Demotion check per flush, mirroring the rescan: a stray refresh
            // would be a stat-gated no-op, but a demoted instance should not
            // keep producing sidecar writes at all.
            if !is_producer(election) {
                return Ok(());
            }
            for target in due_refreshes(&pending, &roster) {
                crate::sidebar::refresh::refresh_session_transcript_context_from_watch(
                    runtime,
                    &target.kind,
                    &target.session_id,
                    target.model_hint.as_deref(),
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
fn reconcile_roster(
    runtime: &RuntimePaths,
    watcher: &mut notify::RecommendedWatcher,
    roster: &mut BTreeMap<PathBuf, BTreeSet<WatchTarget>>,
) {
    let live = transcript_targets(&crate::store::agent_context::read_all(runtime));
    roster.retain(|path, _| {
        if live.contains_key(path) {
            return true;
        }
        let _ = watcher.unwatch(path);
        false
    });
    for (path, targets) in live {
        match roster.entry(path) {
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
                        debug!(path = %entry.key().display(), error = %err, "transcript watch registration failed");
                    }
                }
            }
        }
    }
}

/// The local-source paths worth watching: every sidecar for an adapter that
/// declares transcript-tail context and names its source. Pure over the
/// records so the roster policy is testable without a watcher or a runtime dir.
fn transcript_targets(records: &[AgentContextRecord]) -> BTreeMap<PathBuf, BTreeSet<WatchTarget>> {
    let mut targets = BTreeMap::<PathBuf, BTreeSet<WatchTarget>>::new();
    for record in records.iter().filter(|record| {
        crate::agents::descriptor_by_kind(record.kind.as_str())
            .is_some_and(|descriptor| descriptor.capabilities.transcript_tail_context)
    }) {
        let Some(path) = record.transcript_path.as_deref().map(PathBuf::from) else {
            continue;
        };
        let target = WatchTarget {
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

fn is_producer(election: &ProducerElectionTracker) -> bool {
    election.elder_instance().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::context::AgentContext;
    use crate::store::agent_context::new_record;

    fn context(kind: &str) -> AgentContext {
        AgentContext::new(kind, jiff::Timestamp::UNIX_EPOCH)
    }

    fn target(kind: &str, session_id: &str) -> WatchTarget {
        WatchTarget {
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
        let mut with_path = new_record("codex", "sess-a", context("codex"));
        with_path.transcript_path = Some("/t/a.jsonl".to_owned());
        with_path.context.model_id = Some("gpt-5.5-codex".to_owned());
        let pathless = new_record("codex", "sess-b", context("codex"));
        let mut copilot = new_record("copilot", "sess-g", context("copilot"));
        copilot.transcript_path = Some("/t/g.jsonl".to_owned());
        let mut droid = new_record("droid", "sess-d", context("droid"));
        droid.transcript_path = Some("/t/d.settings.json".to_owned());
        let mut claude = new_record("claude", "sess-c", context("claude"));
        claude.transcript_path = Some("/t/c.jsonl".to_owned());

        let targets = transcript_targets(&[with_path, pathless, copilot, droid, claude]);
        assert_eq!(
            targets,
            BTreeMap::from([
                (
                    PathBuf::from("/t/a.jsonl"),
                    BTreeSet::from([WatchTarget {
                        kind: "codex".to_owned(),
                        session_id: "sess-a".to_owned(),
                        model_hint: Some("gpt-5.5-codex".to_owned()),
                    }])
                ),
                (
                    PathBuf::from("/t/d.settings.json"),
                    BTreeSet::from([WatchTarget {
                        kind: "droid".to_owned(),
                        session_id: "sess-d".to_owned(),
                        model_hint: None,
                    }])
                ),
                (
                    PathBuf::from("/t/g.jsonl"),
                    BTreeSet::from([WatchTarget {
                        kind: "copilot".to_owned(),
                        session_id: "sess-g".to_owned(),
                        model_hint: None,
                    }])
                )
            ])
        );
    }
}
