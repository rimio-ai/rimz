//! Codex rollout fast path: refresh the context sidecar the moment the
//! transcript grows.
//!
//! Codex's tokens/cost reach the sidecar through hook pushes after progress
//! events plus the producer's stat-gated tick backstop
//! (`crate::sidebar::enrich`), so a long generation between tool calls goes
//! quiet until the next hook fires. The elected producer watches each live
//! root Codex session's rollout JSONL with a filesystem watcher and runs the
//! *same* stat-gated refresh on the write, so the token meter and `$` move
//! mid-turn. Latency only, never truth: the refresh is idempotent behind its
//! transcript-stat gate, the tick backstop stays unconditional, and a watcher
//! that fails to start (or a platform that drops events) degrades to exactly
//! the cadence the room had before this thread existed.
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

use crate::ledger::agent_context::AgentContextRecord;
use crate::{RuntimePaths, SidebarInstanceId};

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

/// One watched rollout file: the session it belongs to and the model hint its
/// sidecar last carried, threaded into the refresh for cost pricing.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WatchTarget {
    session_id: String,
    model_hint: Option<String>,
}

/// Spawn the watcher manager thread. It runs for the process lifetime; the
/// watcher handle is dropped (releasing every OS watch) whenever the instance
/// is not the producer.
pub(super) fn spawn(runtime: RuntimePaths, instance_id: SidebarInstanceId) -> JoinHandle<()> {
    std::thread::spawn(move || watch_loop(&runtime, &instance_id))
}

fn watch_loop(runtime: &RuntimePaths, instance_id: &SidebarInstanceId) {
    loop {
        if !is_producer(runtime, instance_id) {
            std::thread::sleep(ELECTION_POLL);
            continue;
        }
        if let Err(err) = watch_while_elected(runtime, instance_id) {
            debug!(error = %err, "transcript watch failed; tick backstop remains truth");
        }
        std::thread::sleep(RESPAWN_BACKOFF);
    }
}

/// Own the watcher while elected: register live Codex rollout paths, coalesce
/// fs events behind [`DEBOUNCE`], and run the stat-gated sidecar refresh per
/// flushed session. Returns on demotion (dropping the watcher and its OS
/// watches) or on a dead event channel (the outer loop respawns with backoff).
fn watch_while_elected(
    runtime: &RuntimePaths,
    instance_id: &SidebarInstanceId,
) -> notify::Result<()> {
    let (event_tx, event_rx) = mpsc::channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                let _ = event_tx.send(path);
            }
        }
    })?;
    let mut roster: BTreeMap<PathBuf, WatchTarget> = BTreeMap::new();
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut flush_at: Option<Instant> = None;
    let mut rescan_at = Instant::now();
    loop {
        let now = Instant::now();
        if now >= rescan_at {
            // Demotion check per rescan: a demoted instance releases every
            // watch by dropping the watcher and returns to the election poll.
            if !is_producer(runtime, instance_id) {
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
            if !is_producer(runtime, instance_id) {
                return Ok(());
            }
            for target in due_refreshes(&pending, &roster) {
                crate::sidebar::enrich::refresh_codex_transcript_context(
                    runtime,
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
    roster: &mut BTreeMap<PathBuf, WatchTarget>,
) {
    let live = codex_transcript_targets(&crate::ledger::agent_context::read_all(runtime));
    roster.retain(|path, _| {
        if live.contains_key(path) {
            return true;
        }
        let _ = watcher.unwatch(path);
        false
    });
    for (path, target) in live {
        match roster.entry(path) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Keep the watch; refresh the model hint the sidecar carries.
                entry.insert(target);
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                match watcher.watch(entry.key(), RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        entry.insert(target);
                    }
                    Err(err) => {
                        debug!(path = %entry.key().display(), error = %err, "transcript watch registration failed");
                    }
                }
            }
        }
    }
}

/// The rollout paths worth watching: every live Codex sidecar that names its
/// transcript. Pure over the records so the roster policy is testable without
/// a watcher or a runtime dir.
fn codex_transcript_targets(records: &[AgentContextRecord]) -> BTreeMap<PathBuf, WatchTarget> {
    records
        .iter()
        .filter(|record| record.kind == "codex")
        .filter_map(|record| {
            let path = PathBuf::from(record.transcript_path.as_deref()?);
            let target = WatchTarget {
                session_id: record.agent_id.as_str().to_owned(),
                model_hint: record.context.model_id.clone(),
            };
            Some((path, target))
        })
        .collect()
}

/// The flush decision: map the pending event paths through the roster and
/// dedupe to one refresh per session. Pure, so the coalescing policy is
/// testable without `notify` or a clock. An un-rostered path (an event that
/// raced a session ending) refreshes nothing.
fn due_refreshes(
    pending: &BTreeSet<PathBuf>,
    roster: &BTreeMap<PathBuf, WatchTarget>,
) -> Vec<WatchTarget> {
    let mut seen = BTreeSet::new();
    pending
        .iter()
        .filter_map(|path| roster.get(path))
        .filter(|target| seen.insert(target.session_id.clone()))
        .cloned()
        .collect()
}

fn is_producer(runtime: &RuntimePaths, instance_id: &SidebarInstanceId) -> bool {
    !crate::sidebar::elder_sidebar_present(runtime, instance_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::context::AgentContext;
    use crate::ledger::agent_context::{empty_context, new_record};

    fn context(kind: &str) -> AgentContext {
        empty_context(kind, jiff::Timestamp::UNIX_EPOCH)
    }

    fn target(session_id: &str) -> WatchTarget {
        WatchTarget {
            session_id: session_id.to_owned(),
            model_hint: None,
        }
    }

    fn roster(entries: &[(&str, &str)]) -> BTreeMap<PathBuf, WatchTarget> {
        entries
            .iter()
            .map(|(path, session)| (PathBuf::from(path), target(session)))
            .collect()
    }

    #[test]
    fn many_events_for_one_path_flush_one_refresh() {
        let roster = roster(&[("/t/a.jsonl", "sess-a")]);
        let pending: BTreeSet<PathBuf> = [PathBuf::from("/t/a.jsonl")].into();
        assert_eq!(due_refreshes(&pending, &roster), vec![target("sess-a")]);
    }

    #[test]
    fn two_paths_one_session_dedupe_to_one_refresh() {
        let roster = roster(&[("/t/a.jsonl", "sess-a"), ("/t/a2.jsonl", "sess-a")]);
        let pending: BTreeSet<PathBuf> =
            [PathBuf::from("/t/a.jsonl"), PathBuf::from("/t/a2.jsonl")].into();
        assert_eq!(due_refreshes(&pending, &roster), vec![target("sess-a")]);
    }

    #[test]
    fn unrostered_path_refreshes_nothing() {
        let roster = roster(&[("/t/a.jsonl", "sess-a")]);
        let pending: BTreeSet<PathBuf> = [PathBuf::from("/t/gone.jsonl")].into();
        assert!(due_refreshes(&pending, &roster).is_empty());
    }

    #[test]
    fn empty_pending_refreshes_nothing() {
        let roster = roster(&[("/t/a.jsonl", "sess-a")]);
        assert!(due_refreshes(&BTreeSet::new(), &roster).is_empty());
    }

    #[test]
    fn targets_keep_codex_records_that_name_a_transcript() {
        let mut with_path = new_record("codex", "sess-a", context("codex"));
        with_path.transcript_path = Some("/t/a.jsonl".to_owned());
        with_path.context.model_id = Some("gpt-5.5-codex".to_owned());
        let pathless = new_record("codex", "sess-b", context("codex"));
        let mut claude = new_record("claude", "sess-c", context("claude"));
        claude.transcript_path = Some("/t/c.jsonl".to_owned());

        let targets = codex_transcript_targets(&[with_path, pathless, claude]);
        assert_eq!(
            targets,
            BTreeMap::from([(
                PathBuf::from("/t/a.jsonl"),
                WatchTarget {
                    session_id: "sess-a".to_owned(),
                    model_hint: Some("gpt-5.5-codex".to_owned()),
                }
            )])
        );
    }
}
