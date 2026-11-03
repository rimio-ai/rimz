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
        open_permission_since: None,
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

fn append_codex_signal(
    state: &crate::StatePaths,
    session: &str,
    signal: crate::agents::LifecycleSignal,
) {
    let observation = crate::agents::AgentLifecycleObservation::new(Some(session.into()), signal);
    crate::store::event_log::append(
        &state.events_log,
        &crate::store::event::EventEnvelope::agent_lifecycle(
            state.workspace_id.clone(),
            "room",
            "codex",
            "test",
            &observation,
        ),
    )
    .unwrap();
}

fn register_codex(state: &crate::StatePaths, session: &str) {
    append_codex_signal(state, session, crate::agents::LifecycleSignal::Registered);
}

fn write_sidecar(runtime: &RuntimePaths, session: &str, source: &str) -> PathBuf {
    let mut record = AgentContextRecord::new("codex", session, context("codex"));
    record.transcript_path = Some(source.to_owned());
    let path = crate::store::agent_context::path_for(runtime, "codex", session);
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    path
}

fn hold_session(state: &crate::StatePaths, session: &str) {
    crate::store::hibernate::record(
        state,
        crate::store::hibernate::Entry {
            kind: crate::ids::AgentKind::new_unchecked("codex"),
            agent_id: session.into(),
            pane_id: "tmux:%1".parse().unwrap(),
            scope: Default::default(),
            provider_pid: 1,
            frozen_at: jiff::Timestamp::now(),
            anon_at_freeze: 0,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 0,
        },
    )
    .unwrap();
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
    register_codex(&state, "first");
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
    register_codex(&state, "second");
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
fn newly_rostered_permission_rechecks_an_already_read_completion() {
    use crate::agents::{AskKind, LifecycleSignal};
    let (dir, state, runtime) = fixture();
    register_codex(&state, "permission");
    let now = jiff::Timestamp::now();
    let call_at = now - Duration::from_secs(1);
    let result_at = now + Duration::from_secs(1);
    let path = dir.path().join("rollout.jsonl");
    let lines = [
        serde_json::json!({"timestamp": call_at, "type": "response_item", "payload": {
            "type": "custom_tool_call", "name": "exec", "call_id": "exec-1", "input": ""
        }}),
        serde_json::json!({"timestamp": result_at, "type": "response_item", "payload": {
            "type": "custom_tool_call_output", "call_id": "exec-1",
            "output": "Script completed\nWall time 0.1 seconds\nOutput:\n"
        }}),
    ];
    std::fs::write(&path, lines.map(|line| line.to_string() + "\n").concat()).unwrap();
    write_sidecar(&runtime, "permission", path.to_str().unwrap());
    crate::sidebar::refresh::refresh_session_transcript_context_from_watch(
        &runtime,
        "codex",
        "permission",
        None,
        None,
        None,
    );
    let mut cursor = crate::sidebar::consumer::RollupCursor::new();
    let mut watcher = notify::NullWatcher;
    let mut roster = Roster::default();
    reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster);
    append_codex_signal(
        &state,
        "permission",
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ask_id: Some(crate::ids::AskId::new()),
            detail: None,
            native_key: None,
        },
    );
    reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster);
    let target = roster.watched[&path].first().unwrap();
    let proof = crate::store::agent_context::read_one(&runtime, "codex", "permission")
        .unwrap()
        .context
        .permission_settle;
    assert_eq!(
        proof.map(|proof| (proof.ask_since, proof.at)),
        target.open_permission_since.map(|since| (since, result_at)),
        "roster catch-up must not need another rollout write or a heavy tick"
    );
    assert!(proof.is_some());
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
    register_codex(&state, "first");
    write_sidecar(&runtime, "first", "/t/first.jsonl");
    hold_session(&state, "first");
    let mut cursor = crate::sidebar::consumer::RollupCursor::new();
    let mut watcher = RefusingWatcher::default();
    let mut roster = Roster::default();
    let mut stats = Vec::new();
    for _ in 0..2 {
        let before = crate::testkit::sidecar_read_counts();
        assert_eq!(
            reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster),
            RosterPass::Rebuilt
        );
        assert!(roster.watched.is_empty());
        assert!(roster.stamp.is_none());
        stats.push(crate::testkit::sidecar_read_counts().0 - before.0);
    }
    assert_eq!(watcher.attempts, 2);
    assert_eq!(stats, [2, 1], "retry cached held targets without a stat");
}

#[derive(Default)]
struct RecordingWatcher {
    unwatched: usize,
}

impl Watcher for RecordingWatcher {
    fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
        Ok(Self::default())
    }

    fn watch(&mut self, _: &std::path::Path, _: RecursiveMode) -> notify::Result<()> {
        Ok(())
    }

    fn unwatch(&mut self, _: &std::path::Path) -> notify::Result<()> {
        self.unwatched += 1;
        Ok(())
    }

    fn kind() -> notify::WatcherKind {
        notify::WatcherKind::NullWatcher
    }
}

#[test]
fn held_roster_reads_wait_for_rebuild_and_keep_watches_through_thaw() {
    let (_dir, state, runtime) = fixture();
    register_codex(&state, "held");
    register_codex(&state, "live");
    write_sidecar(&runtime, "held", "/t/held.jsonl");
    write_sidecar(&runtime, "live", "/t/live.jsonl");
    hold_session(&state, "held");
    stamp_directory(&runtime, 1);
    let mut cursor = crate::sidebar::consumer::RollupCursor::new();
    let mut watcher = RecordingWatcher::default();
    let mut roster = Roster::default();
    let now = Instant::now();
    let mut observations = Vec::new();
    for seconds in [0, 30, 60, 61, 90] {
        if seconds == 30 {
            write_sidecar(&runtime, "held", "/t/held-new.jsonl");
            stamp_directory(&runtime, 2);
        } else if seconds == 61 {
            crate::store::hibernate::withdraw(
                &state,
                &crate::ids::AgentKind::new_unchecked("codex"),
                &"held".into(),
            )
            .unwrap();
            write_sidecar(&runtime, "held", "/t/held-after-thaw.jsonl");
            stamp_directory(&runtime, 2);
        }
        let before = crate::testkit::sidecar_read_counts();
        let pass = reconcile_roster_with_clock(
            &runtime,
            &state,
            &mut cursor,
            &mut watcher,
            &mut roster,
            &|| now + Duration::from_secs(seconds),
        );
        let after = crate::testkit::sidecar_read_counts();
        assert_eq!(after.1 - before.1, 0, "no enumeration");
        let held_path = roster.watched.iter().find_map(|(path, targets)| {
            targets
                .iter()
                .any(|target| target.session_id == "held")
                .then(|| path.to_str().unwrap().to_owned())
        });
        observations.push((after.0 - before.0, pass, watcher.unwatched, held_path));
    }
    assert_eq!(
        observations,
        [
            (3, RosterPass::Rebuilt, 0, Some("/t/held.jsonl".into())),
            (2, RosterPass::Rebuilt, 0, Some("/t/held.jsonl".into())),
            (0, RosterPass::Unchanged, 0, Some("/t/held.jsonl".into())),
            (0, RosterPass::Unchanged, 0, Some("/t/held.jsonl".into())),
            (
                3,
                RosterPass::Rebuilt,
                1,
                Some("/t/held-after-thaw.jsonl".into())
            ),
        ]
    );
}

#[test]
fn rescan_reads_only_live_row_contexts_despite_orphans() {
    const LIVE: usize = 20;
    let (_dir, state, runtime) = fixture();
    let mut expected = BTreeMap::new();
    for slot in 0..LIVE {
        let session = format!("live-{slot}");
        let source = format!("/t/{session}.jsonl");
        register_codex(&state, &session);
        write_sidecar(&runtime, &session, &source);
        expected.insert(
            PathBuf::from(source),
            BTreeSet::from([target("codex", &session)]),
        );
    }
    register_codex(&state, "ended");
    append_codex_signal(&state, "ended", crate::agents::LifecycleSignal::Ended);
    write_sidecar(&runtime, "ended", "/t/ended.jsonl");
    for slot in 0..3_000 {
        write_sidecar(&runtime, &format!("orphan-{slot}"), "/t/orphan.jsonl");
    }
    let mut cursor = crate::sidebar::consumer::RollupCursor::new();
    let mut watcher = notify::NullWatcher;
    let mut roster = Roster::default();
    let before = crate::testkit::sidecar_read_counts();
    assert_eq!(
        reconcile_roster(&runtime, &state, &mut cursor, &mut watcher, &mut roster),
        RosterPass::Rebuilt
    );
    let after = crate::testkit::sidecar_read_counts();
    assert_eq!(
        after.1 - before.1,
        0,
        "a rescan must not enumerate sidecars"
    );
    // The fixture's own live row is a key too; the ended row is not.
    assert!(
        after.0 - before.0 <= LIVE + 1,
        "at most one stat per live row, got {}",
        after.0 - before.0,
    );
    assert_eq!(roster.watched, expected);
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
                    open_permission_since: None,
                    kind: "codex".to_owned(),
                    login: Some("work".parse().unwrap()),
                    session_id: "sess-a".to_owned(),
                    model_hint: Some("gpt-5.5-codex".to_owned()),
                }])
            ),
            (
                PathBuf::from("/t/d.settings.json"),
                BTreeSet::from([WatchTarget {
                    open_permission_since: None,
                    kind: "droid".to_owned(),
                    login: None,
                    session_id: "sess-d".to_owned(),
                    model_hint: None,
                }])
            ),
            (
                PathBuf::from("/t/g.jsonl"),
                BTreeSet::from([WatchTarget {
                    open_permission_since: None,
                    kind: "copilot".to_owned(),
                    login: None,
                    session_id: "sess-g".to_owned(),
                    model_hint: None,
                }])
            )
        ])
    );
}
