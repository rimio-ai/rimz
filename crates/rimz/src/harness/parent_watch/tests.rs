use super::*;

fn watchdog() -> ParentWatchdog {
    let (_dir, paths, _rt, seed) = fixture();
    ParentWatchdog::from_seed(seed, paths, |_, _| ProbeConfirm::Unknown)
}

#[test]
fn authoritative_absence_requires_three_distinct_observations() {
    let mut watch = watchdog();

    assert!(!watch.observe(ParentProbe::Absent(1)));
    assert!(!watch.observe(ParentProbe::Absent(1)));
    assert!(!watch.observe(ParentProbe::Absent(2)));
    assert!(watch.observe(ParentProbe::Absent(3)));
}

#[test]
fn presence_resets_absence_strikes_and_durable_end_is_immediate() {
    let mut watch = watchdog();

    assert!(!watch.observe(ParentProbe::Absent(1)));
    assert!(!watch.observe(ParentProbe::Absent(2)));
    assert!(!watch.observe(ParentProbe::Present(3)));
    assert!(!watch.observe(ParentProbe::Absent(4)));
    assert!(watch.observe(ParentProbe::Ended));
}

#[test]
fn parent_end_follows_current_launch_occupant_and_legacy_alias() {
    let at = jiff::Timestamp::from_second(1_000).unwrap();
    let mut old = crate::testkit::agent_state("codex", "OLD", at);
    old.launch_id = Some(AgentSessionId::from("L"));
    old.ended_at = Some(at);
    let mut new = crate::testkit::agent_state("codex", "NEW", at);
    new.launch_id = old.launch_id.clone();
    let mut child = crate::testkit::agent_state("codex", "child", at);
    child.launch_depth = Some(1);
    for parent_id in ["L", "OLD"] {
        child.parent_agent_id = Some(AgentSessionId::from(parent_id));
        for ended in [false, true] {
            new.ended_at = ended.then_some(at);
            let agents = [old.clone(), new.clone(), child.clone()];
            let (parent, _) =
                resolve_parent_and_child(&agents, &child.kind, &child.agent_id).unwrap();
            assert_eq!(parent.ended_at.is_some(), ended);
            if !ended {
                assert_eq!(parent.agent_id, new.agent_id);
            }
        }
    }
}

#[test]
fn adopted_child_and_parent_rows_resolve_by_stable_launch_identity() {
    let mut parent =
        crate::testkit::agent_state("codex", "parent-provider-session", jiff::Timestamp::now());
    parent.launch_id = Some(AgentSessionId::from("launch-parent"));
    let mut child =
        crate::testkit::agent_state("codex", "child-provider-session", jiff::Timestamp::now());
    child.launch_id = Some(AgentSessionId::from("launch-child"));
    child.parent_agent_id = Some(AgentSessionId::from("launch-parent"));
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);

    let agents = [parent.clone(), child.clone()];
    let resolved = resolve_parent_and_child(
        &agents,
        &child.kind,
        child.launch_id.as_ref().expect("child launch id"),
    )
    .expect("resolve adopted rows");

    assert_eq!(resolved.0.agent_id, parent.agent_id);
    assert_eq!(resolved.1.agent_id, child.agent_id);
}

use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::store::event::{AgentAttachPayload, EventEnvelope};
use crate::store::event_log::{self, LogExtent};
use std::sync::Mutex;

fn fixture() -> (tempfile::TempDir, StatePaths, RuntimePaths, WatchdogSeed) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(id.clone(), dir.path()).unwrap();
    let rt = RuntimePaths::under(id, dir.path()).unwrap();
    let store = crate::Store::open(paths.clone(), rt.clone()).unwrap();
    crate::testkit::fleet::seed_history_carryover(&store, 2, 512).unwrap();
    invalidate_carryover(&paths);
    append(&paths, "OLD", LifecycleSignal::Registered);
    let seed = WatchdogSeed {
        child_kind: AgentKind::new_unchecked("codex"),
        child_launch_id: "child".into(),
        parent_kind: AgentKind::new_unchecked("codex"),
        parent_refs: vec!["L".into(), "OLD".into()],
        members: BTreeMap::from([("OLD".into(), false)]),
        parent_pane: None,
        child_pane: None,
        session_name: "room".into(),
        cursor: LogExtent {
            generation: crate::store::snapshot::lifecycle_log_generation(&paths),
            offset: std::fs::metadata(&paths.events_log).unwrap().len(),
        },
    };
    drop(store);
    (dir, paths, rt, seed)
}

fn append(paths: &StatePaths, id: &str, signal: LifecycleSignal) {
    let observation = AgentLifecycleObservation::new(Some(id.into()), signal);
    event_log::append(
        &paths.events_log,
        &EventEnvelope::agent_lifecycle(
            paths.workspace_id.clone(),
            "room",
            "codex",
            "test",
            &observation,
        ),
    )
    .unwrap();
}

fn invalidate_carryover(paths: &StatePaths) {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&paths.agents_carryover).unwrap()).unwrap();
    crate::disk::atomic::write_temp_then_rename_cache(&paths.agents_carryover, &value).unwrap();
}

fn probe_without_fold(watch: &mut ParentWatchdog, now: Instant) -> bool {
    let before = crate::store::snapshot::fold_testkit::carryover_bytes_parsed();
    let ended = watch.probe_if_due(now);
    assert_eq!(
        crate::store::snapshot::fold_testkit::carryover_bytes_parsed(),
        before
    );
    ended
}

#[test]
fn cursor_end_requires_confirmation_without_parsing_carryover() {
    let (_dir, paths, _rt, seed) = fixture();
    append(&paths, "OLD", LifecycleSignal::Ended);
    let calls = Arc::new(Mutex::new(0));
    let seen = calls.clone();
    let mut watch = ParentWatchdog::from_seed(seed, paths, move |_, _| {
        *seen.lock().unwrap() += 1;
        ProbeConfirm::Ended
    });
    let before = crate::store::snapshot::fold_testkit::carryover_bytes_parsed();
    let due = watch.next_probe;
    assert!(probe_without_fold(&mut watch, due));
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(
        crate::store::snapshot::fold_testkit::carryover_bytes_parsed(),
        before
    );
}

#[test]
fn alive_successor_reseeds_and_later_end_is_confirmed_with_a_probe_floor() {
    let (_dir, paths, _rt, seed) = fixture();
    append(&paths, "OLD", LifecycleSignal::Ended);
    let calls = Arc::new(Mutex::new(0));
    let seen = calls.clone();
    let mut watch = ParentWatchdog::from_seed(seed, paths.clone(), move |seed, _| {
        let mut count = seen.lock().unwrap();
        *count += 1;
        if *count == 1 {
            let mut seed = seed.clone();
            seed.members = BTreeMap::from([("OLD".into(), true), ("NEW".into(), false)]);
            ProbeConfirm::Alive(Box::new(seed))
        } else {
            ProbeConfirm::Ended
        }
    });
    let before = crate::store::snapshot::fold_testkit::carryover_bytes_parsed();
    let now = watch.next_probe;
    assert!(!probe_without_fold(&mut watch, now));
    append(&paths, "NEW", LifecycleSignal::Ended);
    assert!(!probe_without_fold(&mut watch, now));
    assert_eq!(*calls.lock().unwrap(), 1);
    let due = watch.next_probe;
    assert!(probe_without_fold(&mut watch, due));
    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(
        crate::store::snapshot::fold_testkit::carryover_bytes_parsed(),
        before
    );
}

#[test]
fn attach_adds_the_parent_member_before_its_end_counts() {
    let (_dir, paths, _rt, seed) = fixture();
    append(&paths, "OLD", LifecycleSignal::Ended);
    let pane = crate::ids::PaneId::parse("tmux:%77").unwrap();
    let payload = AgentAttachPayload {
        record: None,
        tier: None,
        mode: None,
        agent_id: "NEW".into(),
        isolation: None,
        effective_isolation: None,
        launch_id: Some("L".into()),
        login: None,
        pane_id: pane.clone(),
        pane_pid: None,
        runtime_owner: crate::store::runtime::current_process_owner(
            crate::pane::RuntimeOwnerKind::Agent,
            "NEW",
        ),
    };
    event_log::append(
        &paths.events_log,
        &EventEnvelope::agent_attached(
            paths.workspace_id.clone(),
            "room",
            &AgentKind::new_unchecked("codex"),
            payload,
        ),
    )
    .unwrap();
    let calls = Arc::new(Mutex::new(0));
    let seen = calls.clone();
    let mut watch = ParentWatchdog::from_seed(seed, paths.clone(), move |_, _| {
        *seen.lock().unwrap() += 1;
        ProbeConfirm::Ended
    });
    let before = crate::store::snapshot::fold_testkit::carryover_bytes_parsed();
    let due = watch.next_probe;
    assert!(!probe_without_fold(&mut watch, due));
    assert_eq!(*calls.lock().unwrap(), 0);
    assert_eq!(watch.seed.parent_pane, Some(pane));
    append(&paths, "NEW", LifecycleSignal::Ended);
    let due = watch.next_probe;
    assert!(probe_without_fold(&mut watch, due));
    assert_eq!(
        crate::store::snapshot::fold_testkit::carryover_bytes_parsed(),
        before
    );
}

#[test]
fn seed_to_first_probe_rotation_loses_no_end_frame() {
    let (_dir, paths, rt, seed) = fixture();
    append(&paths, "OLD", LifecycleSignal::Ended);
    crate::Store::open(paths.clone(), rt.clone())
        .unwrap()
        .rotate_event_log(1, None)
        .unwrap();
    invalidate_carryover(&paths);
    let mut watch = ParentWatchdog::from_seed(seed, paths, |_, _| ProbeConfirm::Ended);
    let before = crate::store::snapshot::fold_testkit::carryover_bytes_parsed();
    let due = watch.next_probe;
    assert!(probe_without_fold(&mut watch, due));
    assert_eq!(
        crate::store::snapshot::fold_testkit::carryover_bytes_parsed(),
        before
    );
}

#[test]
fn receipt_mode_wakes_a_sleeping_watch_and_drains_without_a_confirm() {
    let (_dir, paths, _rt, seed) = fixture();
    let watch = ParentWatchdog::from_seed(seed, paths.clone(), |_, _| {
        panic!("receipt drains must not confirm before the probe interval")
    })
    .start();
    watch.set_receipt_waiting(true);
    append(
        &paths,
        "OLD",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let deadline = Instant::now() + Duration::from_millis(1250);
    while !watch.take_parent_changed() {
        assert!(
            Instant::now() < deadline,
            "receipt mode must wake and drain within one second"
        );
        thread::sleep(Duration::from_millis(10));
    }
    watch.set_receipt_waiting(false);
}

#[test]
fn corrupt_archived_cursor_still_confirms_parent_end() {
    let (_dir, paths, rt, mut seed) = fixture();
    seed.cursor.offset += 1;
    append(&paths, "OLD", LifecycleSignal::Ended);
    crate::Store::open(paths.clone(), rt)
        .unwrap()
        .rotate_event_log(1, None)
        .unwrap();
    let mut watch = ParentWatchdog::from_seed(seed, paths, |_, _| ProbeConfirm::Ended);
    let due = watch.next_probe;
    assert!(
        probe_without_fold(&mut watch, due),
        "a tail error must retain the authoritative backstop"
    );
}

#[test]
fn missing_archive_still_confirms_parent_end() {
    let (_dir, paths, rt, seed) = fixture();
    append(&paths, "OLD", LifecycleSignal::Ended);
    crate::Store::open(paths.clone(), rt)
        .unwrap()
        .rotate_event_log(1, None)
        .unwrap();
    for archive in std::fs::read_dir(&paths.events_archive_dir).unwrap() {
        std::fs::remove_file(archive.unwrap().path()).unwrap();
    }
    let mut watch = ParentWatchdog::from_seed(seed, paths, |_, _| ProbeConfirm::Ended);
    let due = watch.next_probe;
    assert!(
        probe_without_fold(&mut watch, due),
        "an archive gap must nominate confirmation"
    );
}

#[test]
fn an_unseeded_watch_is_seeded_by_its_first_confirmation() {
    let (_dir, paths, _rt, mut seed) = fixture();
    seed.members.clear();
    seed.parent_refs.clear();
    let calls = Arc::new(Mutex::new(0));
    let seen = calls.clone();
    let mut watch = ParentWatchdog::from_seed(seed, paths, move |seed, _| {
        *seen.lock().unwrap() += 1;
        let mut seed = seed.clone();
        seed.parent_refs = vec!["L".into()];
        seed.members = BTreeMap::from([("NEW".into(), false)]);
        ProbeConfirm::Alive(Box::new(seed))
    });
    let due = watch.next_probe;
    assert!(!probe_without_fold(&mut watch, due));
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "an unresolved launch needs its first confirm"
    );
    assert_eq!(
        watch.seed.members.get(&AgentSessionId::from("NEW")),
        Some(&false)
    );
}

#[test]
fn an_attach_does_not_revive_an_ended_parent_member() {
    let (_dir, _paths, _rt, mut seed) = fixture();
    seed.members.insert("OLD".into(), true);
    observe_frame(
        &mut seed,
        EventEnvelope::agent_attached(
            WorkspaceId::from_project_root(std::path::Path::new("/test")),
            "room",
            &AgentKind::new_unchecked("codex"),
            AgentAttachPayload {
                agent_id: "OLD".into(),
                launch_id: Some("L".into()),
                pane_id: PaneId::parse("tmux:%77").unwrap(),
                record: None,
                tier: None,
                mode: None,
                isolation: None,
                effective_isolation: None,
                login: None,
                pane_pid: None,
                runtime_owner: crate::store::runtime::current_process_owner(
                    crate::pane::RuntimeOwnerKind::Agent,
                    "OLD",
                ),
            },
        ),
    );
    assert_eq!(
        seed.members.get(&AgentSessionId::from("OLD")),
        Some(&true),
        "attach changes placement, not lifecycle"
    );
}
