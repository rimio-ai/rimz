//! The write-path contract: a mutation's critical section is the event append
//! only; the snapshot publishes off the lock through `locks/publish.lock`
//! (group commit); readers are lock-free and recover any commit the publisher
//! missed by folding the delta themselves.

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::AgentLaunchState;
use rimz::store::{AgentLaunchAppend, AgentLaunchName, AgentLaunchRequest, snapshot};
use rimz::{EventEnvelope, RuntimeScope};
use serde_json::json;

fn lifecycle(h: &crate::common::Harness, event_name: &str, agent_id: &str) -> EventEnvelope {
    crate::common::lifecycle_event(h, "rimz-test", event_name, agent_id)
}

fn lifecycle_for_workspace(
    workspace_id: rimz::WorkspaceId,
    event_name: &str,
    agent_id: &str,
) -> EventEnvelope {
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::Registered,
    );
    EventEnvelope::agent_lifecycle(
        workspace_id,
        "rimz-test",
        "claude",
        event_name,
        &observation,
    )
}

fn named_codex_lifecycle(
    h: &crate::common::Harness,
    event_name: &str,
    agent_id: &str,
    agent_name: &str,
) -> EventEnvelope {
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(agent_id)),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some(agent_name.to_owned());
    EventEnvelope::agent_lifecycle(
        h.workspace_id.clone(),
        "rimz-test",
        "codex",
        event_name,
        &observation,
    )
}

fn log_len(h: &crate::common::Harness) -> u64 {
    std::fs::metadata(&h.store.paths().events_log)
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// Drop the checkpoint cadence stamp so the next write tail publishes —
/// the test-side lever for asserting what a due publish reflects, the same
/// way tests age `abandon-sweep.stamp` to force the sweep.
fn force_next_publish(h: &crate::common::Harness) {
    let _ = std::fs::remove_file(h.store.paths().locks_dir.join("publish.stamp"));
}

#[test]
fn launch_allocation_reserves_names_owned_by_reaped_rollup_agents() {
    let h = crate::common::Harness::new();
    let mut ghost = named_codex_lifecycle(&h, "SessionStart", "ghost-session", "ghost-pet");
    ghost.timestamp = jiff::Timestamp::now() - std::time::Duration::from_secs(4 * 60 * 60);
    h.store.append_event(&ghost).expect("append ghost");
    assert!(
        !h.store
            .snapshot()
            .expect("snapshot")
            .agents
            .iter()
            .any(|agent| agent.agent_id == "ghost-session"),
        "the fixture ghost is reaped from the derived view"
    );

    let request = AgentLaunchRequest {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("launch_codex"),
        name: AgentLaunchName::Soft("ghost-pet".to_owned()),
        launch: rimz::agents::LaunchParams {
            profile: Some("codex-coder".to_owned()),
            role: Some("coder".to_owned()),
            team: Some("forge".to_owned()),
            ..rimz::agents::LaunchParams::default()
        },
        run_id: None,
    };
    let append = AgentLaunchAppend {
        workspace_id: h.workspace_id.clone(),
        session_name: "rimz-test".to_owned(),
        cwd: h.store.paths().root.clone(),
        worktree_name: Some("main".to_owned()),
        channel: None,
        prompt: Some("boot".to_owned()),
        description: None,
        state: AgentLaunchState::Bound,
        pane_id: None,
    };
    let identities = h
        .store
        .append_agent_launches_allocating(&[request], &append)
        .expect("append launch");

    assert_eq!(identities.len(), 1);
    let launched = &identities[0];
    assert_ne!(
        launched.name, "ghost-pet",
        "allocation must see unreaped rollup names"
    );
    let snapshot = h.store.snapshot().expect("snapshot with launch");
    let card = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == launched.agent_id.as_str())
        .expect("launched card is visible");
    assert_eq!(card.name.as_deref(), Some(launched.name.as_str()));
    assert_eq!(card.role.as_deref(), Some("coder"));
}

#[test]
fn concurrent_writers_group_commit_the_newest_state() {
    const WRITERS: usize = 8;
    const EVENTS_EACH: usize = 5;
    let h = crate::common::Harness::new();

    let handles: Vec<_> = (0..WRITERS)
        .map(|w| {
            let store = h.store.clone();
            let workspace_id = h.workspace_id.clone();
            std::thread::spawn(move || {
                for i in 0..EVENTS_EACH {
                    store
                        .append_event(&lifecycle_for_workspace(
                            workspace_id.clone(),
                            "SessionStart",
                            &format!("writer-{w}-{i}"),
                        ))
                        .expect("append");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("writer thread");
    }

    let events = h.store.read_events().expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|e| e.method == "agent.lifecycle")
            .count(),
        WRITERS * EVENTS_EACH,
        "every concurrent append landed durably"
    );
    // Freshness is the fold's job: the lock-free read reaches the log's end
    // no matter which tails the cadence gate skipped.
    let snapshot = h.store.snapshot().expect("lock-free read");
    assert_eq!(
        snapshot.reflects_log.expect("stamped").offset,
        log_len(&h),
        "the reader folds to the log's end regardless of checkpoint cadence"
    );
    // The checkpoint trails by less than the byte budget: any tail that
    // skipped saw a smaller unpublished tail, and any that crossed the
    // budget published (PUBLISH_BYTE_BUDGET, writer.rs).
    let checkpoint: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&h.store.paths().latest_snapshot).expect("checkpoint exists"),
    )
    .expect("checkpoint parses");
    let published_offset = checkpoint["reflects_log"]["offset"]
        .as_u64()
        .expect("stamped");
    assert!(
        log_len(&h) - published_offset < 64 * 1024,
        "the unpublished tail stays under the byte budget"
    );
}

#[test]
fn a_reader_recovers_a_commit_that_never_published() {
    // A writer that crashes between releasing the workspace lock and
    // publishing costs nothing: the commit is durable in the log, the stale
    // stamp declines the fast path, and the reader folds the delta itself.
    let h = crate::common::Harness::new();
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "published"))
        .expect("published write");

    // The crashed writer: a bare log append with no publish.
    rimz::store::event_log::append(
        &h.store.paths().events_log,
        &lifecycle(&h, "SessionStart", "unpublished"),
    )
    .expect("bare append");

    assert!(
        snapshot::read_fresh_latest(h.store.paths()).is_none(),
        "the stale stamp declines the fast path"
    );
    let snapshot = h.store.snapshot().expect("lock-free read");
    assert_eq!(
        snapshot.reflects_log.expect("stamped").offset,
        log_len(&h),
        "the reader folded the unpublished delta to the log's end"
    );
    let projection = h
        .store
        .runtime_projection(RuntimeScope::Runtime)
        .expect("projection");
    let ids: Vec<&str> = projection
        .agents
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect();
    assert!(
        ids.contains(&"unpublished"),
        "the recovered commit's agent is visible: {ids:?}"
    );
}

#[test]
fn rotation_bumps_the_generation_and_reseeds_the_fold() {
    let h = crate::common::Harness::new();
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "before-rotation"))
        .expect("append");

    let outcome = h.store.rotate_event_log(1, None).expect("rotate");
    assert!(outcome.rotation.is_rotated());
    assert_eq!(
        outcome.carryover_agents, 1,
        "the rotating log's rollup moved into the carryover"
    );

    h.store
        .append_event(&lifecycle(&h, "SessionStart", "after-rotation"))
        .expect("append into the fresh generation");

    let latest = snapshot::read_fresh_latest(h.store.paths()).expect("fresh");
    let extent = latest.reflects_log.expect("stamped");
    assert_eq!(extent.generation, 1, "rotation bumped the generation");
    assert_eq!(extent.offset, log_len(&h));

    let projection = h
        .store
        .runtime_projection(RuntimeScope::Runtime)
        .expect("projection");
    let mut ids: Vec<&str> = projection
        .agents
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["after-rotation", "before-rotation"],
        "carryover and fresh-generation agents both project"
    );
}

#[test]
fn rotation_retracts_the_published_snapshot_and_republishes_fresh() {
    // The published stamp describes the renamed-away log, so rotation
    // retracts `latest.json` before reseeding (a crash mid-rotation leaves
    // readers folding for themselves, never trusting an aliasable stamp) and
    // the rebuild republishes it under the new generation. A workspace that
    // never published — no `latest.json` at all — rotates the same way.
    let h = crate::common::Harness::new();
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "pre-rotation"))
        .expect("append");
    std::fs::remove_file(&h.store.paths().latest_snapshot).expect("retract published snapshot");

    let outcome = h.store.rotate_event_log(1, None).expect("rotate");
    assert!(outcome.rotation.is_rotated());

    let latest = snapshot::read_fresh_latest(h.store.paths())
        .expect("the rebuild republished a fresh snapshot");
    let extent = latest.reflects_log.expect("stamped");
    assert_eq!(extent.generation, 1, "stamped for the new generation");
    assert_eq!(extent.offset, 0, "the fresh log starts empty");
}

#[test]
fn torn_inflight_tail_does_not_drop_a_folded_agent() {
    // The structural guarantee that let reads go lock-free: the fold base
    // already holds every committed event, so racing a writer's half-written
    // tail frame can only delay that one frame — never lose a folded one.
    let h = crate::common::Harness::new();
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "folded"))
        .expect("append");
    let committed = log_len(&h);

    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&h.store.paths().events_log)
        .expect("open log")
        .write_all(b"512 {\"half\":")
        .expect("write in-flight bytes");

    let snapshot = h.store.snapshot().expect("read races the in-flight tail");
    assert_eq!(
        snapshot.reflects_log.expect("stamped").offset,
        committed,
        "the extent stops at the last complete frame"
    );
    let projection = h
        .store
        .runtime_projection(RuntimeScope::Runtime)
        .expect("projection");
    assert!(
        projection.agents.iter().any(|a| a.agent_id == "folded"),
        "the previously-folded agent survives the race"
    );
}

#[test]
fn rotation_serializes_with_writers_and_drops_no_append() {
    // Rotation renames the active log into the archive; every appender holds
    // the same workspace flock, so a writer can never append into a
    // just-archived file and lose its event at the rename boundary. Counted
    // across the active log plus every archive: nothing vanishes, whichever
    // interleaving the flock grants.
    const WRITERS: usize = 4;
    const EVENTS_EACH: usize = 3;
    const ROTATIONS: usize = 3;
    let h = crate::common::Harness::new();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(WRITERS + 1));
    let mut handles: Vec<std::thread::JoinHandle<()>> = (0..WRITERS)
        .map(|w| {
            let store = h.store.clone();
            let workspace_id = h.workspace_id.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for i in 0..EVENTS_EACH {
                    store
                        .append_event(&lifecycle_for_workspace(
                            workspace_id.clone(),
                            "SessionStart",
                            &format!("rot-writer-{w}-{i}"),
                        ))
                        .expect("append");
                }
            })
        })
        .collect();
    handles.push({
        let store = h.store.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..ROTATIONS {
                store.rotate_event_log(1, None).expect("rotate");
            }
        })
    });
    for handle in handles {
        handle.join().expect("thread");
    }

    let mut appended = h
        .store
        .read_events()
        .expect("active log")
        .iter()
        .filter(|event| event.method == "agent.lifecycle")
        .count();
    if let Ok(entries) = std::fs::read_dir(&h.store.paths().events_archive_dir) {
        for entry in entries {
            let path = entry.expect("archive entry").path();
            appended += rimz::store::event_log::read_all(&path)
                .expect("archived log")
                .iter()
                .filter(|event| event.method == "agent.lifecycle")
                .count();
        }
    }
    assert_eq!(
        appended,
        WRITERS * EVENTS_EACH,
        "every append survives the rename boundary, in the active log or an archive"
    );

    // The post-race read path is coherent.
    h.store
        .runtime_projection(RuntimeScope::Runtime)
        .expect("projection after the race");
}

/// Build a three-frame log (`kept`, `zeroed`, `behind-the-hole`), zero the
/// middle frame's bytes to forge a post-power-cut mid-file corpse, and drop the
/// fold bases so a read must fold the corrupt region. Returns the harness plus
/// the survivor's byte offset and the total log length.
fn corrupt_mid_frame_log() -> (crate::common::Harness, u64, u64) {
    use std::io::{Seek, SeekFrom, Write};

    let h = crate::common::Harness::new();
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "kept"))
        .expect("first event");
    let committed = log_len(&h);
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "zeroed"))
        .expect("second event");
    let corrupted = log_len(&h);
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "behind-the-hole"))
        .expect("third event");
    let total = log_len(&h);

    // Zero the middle frame's bytes (keeping its newline) — writeback loss.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&h.store.paths().events_log)
        .expect("open log");
    file.seek(SeekFrom::Start(committed)).expect("seek");
    let hole = usize::try_from(corrupted - committed - 1).expect("hole len");
    file.write_all(&vec![0u8; hole]).expect("zero the frame");
    drop(file);
    // Drop the fold bases so reads must fold the corrupt region.
    let _ = std::fs::remove_file(&h.store.paths().rollup_cache);
    let _ = std::fs::remove_file(&h.store.paths().latest_snapshot);

    (h, committed, total)
}

#[test]
fn repair_event_log_heals_a_mid_file_corpse_and_republishes() {
    // The post-power-cut recovery path end-to-end: a zeroed mid-file frame
    // hard-errors every cold read, `repair_event_log` truncates at the first
    // invalid frame under the canonical workspace → publish lock order, and
    // the republished caches reflect exactly the surviving prefix.
    let (h, committed, total) = corrupt_mid_frame_log();
    assert!(
        h.store.runtime_projection(RuntimeScope::Runtime).is_err(),
        "a torn middle frame fails reads loudly before the repair"
    );

    let outcome = h.store.repair_event_log().expect("repair");
    assert!(outcome.truncated(), "the corpse was cut");
    assert_eq!(outcome.frames_kept, 1);
    assert_eq!(outcome.bytes_truncated, total - committed);

    // Reads recover, and the republished snapshot reflects the survivor.
    let snapshot = h.store.snapshot_cached().expect("snapshot after repair");
    let ids: Vec<&str> = snapshot
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, ["kept"], "frames behind the hole are cut, not folded");

    // Idempotent: a second repair is a read-only no-op.
    let again = h.store.repair_event_log().expect("repair again");
    assert!(!again.truncated(), "second repair found an intact log");
    assert_eq!(again.frames_kept, 1);
}

#[test]
fn publish_tail_self_heals_a_corrupt_event_log() {
    // The same corpse, healed without an operator: the next mutation's
    // off-lock publish tail hits the corrupt frame, repairs, and republishes
    // the surviving prefix — `rimz gc` is the fallback, not the only way
    // back. Frames behind the cut are gone, including the very append whose
    // tail healed the log: truncate-at-first-invalid is the documented
    // semantic, and resyncing past a hole would need frame magic the format
    // deliberately omits.
    let (h, _, _) = corrupt_mid_frame_log();
    // Drop the cadence stamp too, so the publish runs in the next write tail.
    force_next_publish(&h);
    assert!(
        h.store.runtime_projection(RuntimeScope::Runtime).is_err(),
        "a torn middle frame fails reads loudly before the heal"
    );

    // The next mutation commits blind, then its tail repairs and republishes.
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "behind-the-cut"))
        .expect("a mutation on a corrupt log still commits");

    let events = h.store.read_events().expect("reads recover after the heal");
    assert_eq!(events.len(), 1, "the log is the surviving prefix");
    let snapshot = h.store.snapshot_cached().expect("snapshot after the heal");
    let ids: Vec<&str> = snapshot
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, ["kept"], "the republished view reflects the survivor");

    // The workspace is healthy again: the next mutation lands and folds.
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "after-the-heal"))
        .expect("post-heal append");
    assert_eq!(h.store.read_events().expect("post-heal read").len(), 2);
}

#[test]
fn checkpoint_publish_is_debounced_and_reads_stay_event_fresh() {
    // The checkpoint cadence contract: under sustained writes the publish
    // runs at most once per interval (or per byte budget of unpublished
    // tail), and the skips cost readers nothing — the wakeup-then-fold path
    // is the freshness channel, the checkpoint a catch-up accelerator.
    let h = crate::common::Harness::new();
    let stamp = h.store.paths().locks_dir.join("publish.stamp");

    // First mutation on a quiet workspace: no cadence stamp, so the tail
    // publishes and seeds it.
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "first"))
        .expect("first append");
    let first_checkpoint = log_len(&h);
    let latest = snapshot::read_fresh_latest(h.store.paths()).expect("first publish lands");
    assert_eq!(
        latest.reflects_log.expect("stamped").offset,
        first_checkpoint
    );

    // Pin the stamp fresh so the next tail sits mid-interval even on a
    // stalled runner (mtime only; the stamped extent stays truthful).
    std::fs::File::options()
        .write(true)
        .open(&stamp)
        .expect("stamp exists")
        .set_modified(std::time::SystemTime::now())
        .expect("refresh stamp");
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "second"))
        .expect("second append");
    assert!(
        snapshot::read_fresh_latest(h.store.paths()).is_none(),
        "inside the interval the checkpoint stays put"
    );
    // …while reads stay event-fresh by folding the unpublished tail.
    let folded = h.store.snapshot_cached().expect("fold read");
    assert_eq!(folded.reflects_log.expect("stamped").offset, log_len(&h));
    let ids: Vec<&str> = folded.agents.iter().map(|a| a.agent_id.as_str()).collect();
    assert!(
        ids.contains(&"second"),
        "the skipped checkpoint hides nothing: {ids:?}"
    );

    // Age the stamp past the interval: the next tail publishes.
    std::fs::File::options()
        .write(true)
        .open(&stamp)
        .expect("stamp exists")
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2))
        .expect("age stamp");
    h.store
        .append_event(&lifecycle(&h, "SessionStart", "third"))
        .expect("third append");
    let latest = snapshot::read_fresh_latest(h.store.paths())
        .expect("an aged stamp makes the next tail publish");
    assert_eq!(latest.reflects_log.expect("stamped").offset, log_len(&h));

    // Crossing the byte budget escapes the interval early, bounding a cold
    // reader's catch-up fold whatever the stamp's age.
    let big = EventEnvelope::new(
        h.workspace_id.clone(),
        "rimz-test",
        "claude",
        "agent-hook",
        "agent.lifecycle",
        json!({
            "event_name": "SessionStart",
            "agent_id": "big",
            "signal": { "signal": "registered" },
            "blob": "x".repeat(70 * 1024),
        }),
    );
    h.store.append_event(&big).expect("big append");
    let latest = snapshot::read_fresh_latest(h.store.paths())
        .expect("crossing the byte budget forces an early checkpoint");
    assert_eq!(latest.reflects_log.expect("stamped").offset, log_len(&h));
}
