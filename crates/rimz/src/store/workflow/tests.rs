use super::*;
use serde_json::json;
use std::fs;

fn record() -> RunRecord {
    serde_json::from_value(json!({
        "version":VERSION, "id":"wf_0190b8172c0070008000000000000001", "workflow":"review",
        "spec_dir":"/definitions/review", "prompt":"Review this PR",
        "scope":{"kind":"worktree","path":"/project"}, "project_root":"/project",
        "created_at":"2026-01-01T00:00:00Z", "state":"active", "park":null, "node":"pr",
        "entered_at":"2026-01-01T00:00:00Z", "input":null, "key":null, "recheck_at":null,
        "wait":null, "visits":{"build":2},
        "key_visits":{"build":[{"key":7, "visits":1}, {"key":"7", "visits":1}]},
        "visit_grants":{"build":3},
        "deadline":null, "deadline_fired":false, "next_seq":2,
        "attempt":{"kind":"script","seq":1,"node":"pr","started_at":"2026-01-01T00:00:00Z","host":{"pid":123,"start":"token"}},
        "prev_attempt":null, "terminal_at":null
    })).unwrap()
}

#[test]
fn notes_round_trip_and_initialize_links_kept_memory() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let record = record();
    let relative = crate::store::notes::allocate(
        &state,
        crate::store::notes::Kind::Workflow {
            workflow: record.workflow.clone(),
            run: record.id.clone(),
        },
        home.path(),
        record.created_at,
    )
    .unwrap();
    let mut value = serde_json::to_value(&record).unwrap();
    value["notes"] = json!(relative);
    let record: RunRecord = serde_json::from_value(value).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &record, &guard).unwrap();
    let paths = state.workflow(&record.id);
    let kept = state.notes_run(crate::disk::paths::Class::AutomationNotes, &relative);
    assert_eq!(
        (
            serde_json::to_value(load(&paths).unwrap()).unwrap()["notes"].clone(),
            fs::read_link(paths.notes()).ok(),
        ),
        (json!(relative), Some(kept.canonicalize().unwrap()))
    );
    fs::write(paths.notes().join("build-notes.md"), "remember").unwrap();
    assert_eq!(
        fs::read_to_string(kept.join("build-notes.md")).unwrap(),
        "remember"
    );
}

#[test]
fn record_version_fence_refuses_every_other_version_before_decoding() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let record = record();
    let paths = state.workflow(&record.id);
    assert_eq!(record.header().node.as_deref(), Some("pr"));
    initialize(&state, &record, &guard).unwrap();
    assert_eq!(load(&paths).unwrap(), record);
    let id = record.id.as_str();
    let fix = format!("this RimZ reads only version {VERSION}");
    for version in (1..VERSION).chain([VERSION + 1]) {
        let mut value = serde_json::to_value(&record).unwrap();
        value["version"] = json!(version);
        // A version-2 body this binary no longer decodes still meets the refusal first.
        if version == 2 {
            value.as_object_mut().unwrap().remove("visits");
        }
        fs::write(&paths.record, serde_json::to_vec(&value).unwrap()).unwrap();
        let error = load(&paths).unwrap_err();
        let text = error.to_string();
        assert!(
            matches!(error, WorkflowStoreErr::Version { version: v, .. } if v == version)
                && text.contains(&format!("workflow run {id} at "))
                && text.contains(&format!(
                    "record version {version}, written by another RimZ version"
                ))
                && text.contains(&fix),
            "version {version}: {text}"
        );
    }
    fs::write(&paths.record, br#"{"id":"x"}"#).unwrap();
    assert!(matches!(load(&paths), Err(WorkflowStoreErr::Json { .. })));
}

/// A run a loop fire started in the fire's own fresh tree, before loop tasks stopped cutting a
/// tree each fire, carries `fire_tree`: the record still decodes at this version, and its run
/// now keys on its own scope.
#[test]
fn a_fire_tree_record_still_decodes_at_this_version() {
    let mut value = serde_json::to_value(record()).unwrap();
    value["fire_tree"] = json!(true);
    value["scope"]["path"] = json!("/project-worktrees/night-20260101-000000");
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(decoded.version, VERSION);
    assert_eq!(
        decoded.scope.path,
        Path::new("/project-worktrees/night-20260101-000000")
    );
    let header = RunHeader::read(value.as_object().unwrap());
    assert_eq!(header.version, Some(u64::from(VERSION)));
    assert_eq!(header.node.as_deref(), Some("pr"));
}

#[test]
fn the_pass_awaits_a_finished_run_only_once_it_owes_no_report() {
    let pass = |state: RunState, report: ReportMark| {
        let mut record = record();
        record.task = Some("sweep".into());
        record.state = state;
        record.report = report;
        RunClass::Current(Box::new(record)).awaits_pass()
    };
    assert!(!pass(RunState::Active, ReportMark::Owed));
    // The paced advance retries an owed report.
    assert!(!pass(RunState::Failed, ReportMark::Owed));
    assert!(pass(RunState::Failed, ReportMark::Sent));
}

#[test]
fn a_starter_owes_its_report_until_sent_and_a_run_starter_only_when_a_runs_step_joins_it() {
    let agent = RunStarter::Agent {
        kind: AgentKind::new_unchecked("claude"),
        launch: AgentSessionId::from("launch-1"),
    };
    let owes = |starter: Option<RunStarter>, task: Option<&str>, report: ReportMark| {
        let mut record = record();
        record.state = RunState::Done;
        record.starter = starter;
        record.task = task.map(Into::into);
        record.report = report;
        let header = RunHeader::read(serde_json::to_value(&record).unwrap().as_object().unwrap());
        assert_eq!(header.starter, record.starter);
        (
            record.owes_report(),
            header.owes_report(),
            record.owes_loop_report(),
            RunClass::Current(Box::new(record)).awaits_pass(),
        )
    };
    assert_eq!(
        owes(Some(agent.clone()), None, ReportMark::Owed),
        (true, true, false, false),
        "an agent starter's debt keeps the run live, and no loop collector takes it"
    );
    assert_eq!(
        owes(Some(agent), None, ReportMark::Sent),
        (false, false, false, true)
    );
    let run = RunStarter::Run {
        run: WorkflowRunId::new(),
        node: "build".into(),
    };
    assert_eq!(
        owes(Some(run.clone()), None, ReportMark::Owed),
        (true, true, false, false),
        "a runs step joins the run it started, so the run owes it the report"
    );
    assert_eq!(
        owes(Some(run), None, ReportMark::None),
        (false, false, false, true),
        "a run a command, script or check started is owed nothing"
    );
    assert_eq!(
        owes(None, Some("nightly"), ReportMark::Owed),
        (true, true, true, false)
    );
}

#[test]
fn classification_reads_newer_versions_no_further_and_strands_unfinished_old_runs() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let record = record();
    let paths = state.workflow(&record.id);
    initialize(&state, &record, &guard).unwrap();
    assert_eq!(
        classify(&paths).unwrap(),
        RunClass::Current(Box::new(record.clone()))
    );
    let value = serde_json::to_value(&record).unwrap();
    let mut ended = value.clone();
    ended["version"] = json!(VERSION - 1);
    ended["state"] = json!("done");
    ended["outcome"] = json!({"ending":{"kind":"done"}, "reason":"merged", "acted":true});
    let mut undecodable = value.clone();
    undecodable.as_object_mut().unwrap().remove("visits");
    let mut no_version = value.clone();
    no_version.as_object_mut().unwrap().remove("version");
    let mut text_version = value.clone();
    text_version["version"] = json!("four");
    let mut newer = value.clone();
    newer["version"] = json!(VERSION + 1);
    // A newer record is not read past its version, even where its state is garbage.
    newer["state"] = json!(7);
    let older = |version: u64| {
        let mut older = value.clone();
        older["version"] = json!(version);
        older
    };
    let previous = u64::from(VERSION - 1);
    let previous_cause = format!("stranded older {previous}");
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            serde_json::to_vec(&older(previous)).unwrap(),
            &previous_cause,
        ),
        (serde_json::to_vec(&older(1)).unwrap(), "stranded older 1"),
        (serde_json::to_vec(&no_version).unwrap(), "stranded missing"),
        (
            serde_json::to_vec(&text_version).unwrap(),
            "stranded invalid",
        ),
        (
            serde_json::to_vec(&undecodable).unwrap(),
            "stranded undecodable",
        ),
        (serde_json::to_vec(&ended).unwrap(), "ended"),
        (serde_json::to_vec(&newer).unwrap(), "newer"),
        (b"corrupt".to_vec(), "not object"),
        (b"[1]".to_vec(), "not object"),
    ];
    for (bytes, expected) in cases {
        fs::write(&paths.record, &bytes).unwrap();
        let class = classify(&paths).unwrap();
        let seen = match &class {
            RunClass::Stranded { cause, header } => {
                assert_eq!(header.workflow.as_deref(), Some("review"), "{expected}");
                assert_eq!(header.node.as_deref(), Some("pr"), "{expected}");
                assert_eq!(header.origin, RunOrigin::Deployed, "{expected}");
                match cause {
                    StrandedCause::Older { version } => format!("stranded older {version}"),
                    StrandedCause::MissingVersion => "stranded missing".into(),
                    StrandedCause::InvalidVersion => "stranded invalid".into(),
                    StrandedCause::Undecodable { .. } => "stranded undecodable".into(),
                }
            }
            RunClass::Ended(header) => {
                assert_eq!(header.node.as_deref(), Some("pr"));
                assert_eq!(
                    (header.state, header.reason.as_deref(), header.acted),
                    (Some(RunState::Done), Some("merged"), Some(true))
                );
                "ended".into()
            }
            RunClass::Newer { version } => {
                assert_eq!(*version, u64::from(VERSION + 1));
                "newer".into()
            }
            RunClass::NotObject => "not object".into(),
            other => format!("{other:?}"),
        };
        assert_eq!(seen, expected);
    }
    fs::remove_file(&paths.record).unwrap();
    assert_eq!(classify(&paths).unwrap(), RunClass::Recordless);
    assert!(paths.home.is_dir(), "classification never removes a home");
}

#[test]
fn finish_moves_only_an_ended_run_and_never_replaces_a_finished_home() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let mut record = record();
    let live = state.workflow(&record.id);
    let finished = state.finished_workflow(&record.id);
    initialize(&state, &record, &guard).unwrap();
    assert_eq!(
        finish(&state, &record.id, Some(b"h"), &guard).unwrap(),
        Finished::Kept,
        "an active run stays"
    );
    record.state = RunState::Done;
    record.task = Some("nightly".into());
    record.report = ReportMark::Owed;
    write(&live, &record, &guard).unwrap();
    assert_eq!(
        finish(&state, &record.id, Some(b"h"), &guard).unwrap(),
        Finished::Kept,
        "a run that owes its loop report stays"
    );
    assert!(!live.history().exists(), "a kept run gets no history");
    let mut older = serde_json::to_value(&record).unwrap();
    older["version"] = json!(VERSION - 1);
    older["state"] = json!("active");
    fs::write(&live.record, serde_json::to_vec(&older).unwrap()).unwrap();
    assert_eq!(
        finish(&state, &record.id, None, &guard).unwrap(),
        Finished::Kept,
        "a stranded run waits for the pass to end it"
    );
    record.report = ReportMark::Sent;
    write(&live, &record, &guard).unwrap();
    fs::create_dir_all(&finished.home).unwrap();
    assert_eq!(
        finish(&state, &record.id, Some(b"history"), &guard).unwrap(),
        Finished::Collision
    );
    assert!(live.record.is_file() && !finished.record.exists());
    assert_eq!(fs::read(live.history()).unwrap(), b"history");
    fs::remove_dir(&finished.home).unwrap();
    assert_eq!(
        finish(&state, &record.id, Some(b"history"), &guard).unwrap(),
        Finished::Moved
    );
    assert!(!live.home.exists());
    assert_eq!(load(&finished).unwrap(), record);
    assert_eq!(fs::read(finished.history()).unwrap(), b"history");
    assert_eq!(
        finish(&state, &record.id, None, &guard).unwrap(),
        Finished::Already
    );
    let found = find(&state, &record.id).unwrap().unwrap();
    assert!(found.finished && found.paths.home == finished.home);
    assert_eq!(
        state
            .finished_workflow_runs()
            .unwrap()
            .into_iter()
            .map(|paths| paths.home)
            .collect::<Vec<_>>(),
        vec![finished.home.clone()]
    );
    assert!(live_runs(&state).unwrap().is_empty());
    let other = WorkflowRunId::new();
    assert_eq!(
        finish(&state, &other, None, &guard).unwrap(),
        Finished::Gone
    );
    assert!(find(&state, &other).unwrap().is_none());
    // A record that is not a JSON object is set aside as it stands.
    let broken = state.workflow(&other);
    fs::create_dir_all(&broken.home).unwrap();
    fs::write(&broken.record, b"corrupt").unwrap();
    assert_eq!(
        finish(&state, &other, None, &guard).unwrap(),
        Finished::Moved
    );
    assert_eq!(
        fs::read(state.finished_workflow(&other).record).unwrap(),
        b"corrupt"
    );
}

#[test]
fn finish_keeps_the_live_home_when_its_history_cannot_be_written() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let mut record = record();
    record.state = RunState::Canceled;
    let live = state.workflow(&record.id);
    initialize(&state, &record, &guard).unwrap();
    // A directory at the destination fails the atomic write for any uid.
    fs::create_dir_all(live.history().join("blocker")).unwrap();
    assert!(finish(&state, &record.id, Some(b"history"), &guard).is_err());
    assert!(live.record.is_file());
    assert!(!state.finished_workflow(&record.id).home.exists());
    fs::remove_dir_all(live.history()).unwrap();
    assert_eq!(
        finish(&state, &record.id, Some(b"history"), &guard).unwrap(),
        Finished::Moved,
        "a retry completes the move"
    );
}

#[test]
fn session_agent_join_uses_the_held_attempt_and_prefers_its_root_leader() {
    use crate::agents::{WorkflowAttemptRef, WorkflowAttemptRole};
    let mut record = record();
    let mut attempt = record.attempt.clone().unwrap();
    attempt.kind = AttemptKind::Agent;
    attempt.node = "triage".into();
    attempt.run = Some(AttemptRun {
        run_id: crate::ids::RunId::new(),
        handle: "@leader".into(),
    });
    let mut leader = crate::testkit::agent_state("codex", "leader-id", record.created_at);
    leader.name = Some("leader".into());
    leader.workflow = Some(WorkflowAttemptRef {
        run: record.id.clone(),
        seq: attempt.seq,
        role: WorkflowAttemptRole::Step,
    });
    let mut companion = leader.clone();
    companion.agent_id = "companion".into();
    companion.name = Some("companion".into());
    let mut child = leader.clone();
    child.parent_agent_id = Some(leader.agent_id.clone());
    for held in [false, true] {
        record.held = held.then(|| HeldSession {
            attempt: attempt.clone(),
            entered_at: record.created_at,
            verdict: None,
        });
        record.attempt = (!held).then(|| attempt.clone());
        assert_eq!(record.session_attempt(), Some(&attempt), "held={held}");
        assert_eq!(
            record
                .step_agent(&[&companion, &child, &leader])
                .map(|agent| &agent.agent_id),
            Some(&leader.agent_id)
        );
        companion.workflow.as_mut().unwrap().seq += 1;
        assert!(record.step_agent(&[&companion, &child]).is_none());
        companion.workflow.as_mut().unwrap().seq -= 1;
    }
}

fn pin_record(
    home: &std::path::Path,
    project: &str,
    run_state: RunState,
) -> (StatePaths, RunRecord) {
    let root = home.join(project);
    let state = StatePaths::for_project_root_under(&root, home).unwrap();
    fs::create_dir_all(&state.root).unwrap();
    fs::write(
        &state.workspace_record,
        serde_json::to_vec(&json!({"workspace_id":state.workspace_id})).unwrap(),
    )
    .unwrap();
    let mut run = record();
    run.id = WorkflowRunId::new();
    run.state = run_state;
    run.project_root = root.clone();
    run.scope.path = root.join("tree");
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &run, &guard).unwrap();
    (state, run)
}

#[test]
fn unfinished_run_reader_pins_active_and_parked_but_not_terminal_states() {
    let home = tempfile::tempdir().unwrap();
    let mut expected = std::collections::BTreeSet::new();
    for run_state in [
        RunState::Active,
        RunState::Parked,
        RunState::Done,
        RunState::Failed,
        RunState::Canceled,
    ] {
        let (state, mut run) = pin_record(home.path(), "project", run_state);
        if run_state == RunState::Parked {
            run.scope.kind = ScopeKind::Project;
            let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
            write(&state.workflow(&run.id), &run, &guard).unwrap();
        }
        if !run_state.is_terminal() {
            expected.insert(run.id.to_string());
        }
    }
    for mode in [RunRead::Strict, RunRead::BestEffort] {
        let actual: std::collections::BTreeSet<_> = unfinished_under(home.path(), mode)
            .unwrap()
            .into_iter()
            .map(|run| run.id.to_string())
            .collect();
        assert_eq!(expected, actual, "{mode:?}");
    }
}

#[test]
fn strict_run_reader_skips_other_versions_and_bad_json_but_refuses_an_unreadable_record() {
    let home = tempfile::tempdir().unwrap();
    let (_, live) = pin_record(home.path(), "live", RunState::Active);
    for version in [VERSION - 1, VERSION + 1] {
        let (state, run) = pin_record(home.path(), "old", RunState::Active);
        let mut value = serde_json::to_value(&run).unwrap();
        value["version"] = json!(version);
        fs::write(state.workflow(&run.id).record, value.to_string()).unwrap();
    }
    for mode in [RunRead::Strict, RunRead::BestEffort] {
        let ids: Vec<_> = unfinished_under(home.path(), mode)
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect();
        assert_eq!(ids, std::slice::from_ref(&live.id), "{mode:?}");
    }
    let (state, run) = pin_record(home.path(), "project", RunState::Active);
    let path = state.workflow(&run.id).record;
    fs::write(&path, "{").unwrap();
    assert_eq!(
        unfinished_under(home.path(), RunRead::Strict)
            .unwrap()
            .into_iter()
            .map(|run| run.id)
            .collect::<Vec<_>>(),
        std::slice::from_ref(&live.id)
    );
    // A directory where the record belongs fails the read for any uid.
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let result = unfinished_under(home.path(), RunRead::Strict);
    assert!(
        result
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains(path.to_str().unwrap())),
        "{result:?}"
    );
}

#[test]
fn best_effort_run_reader_keeps_both_rooms_past_bad_and_recordless_homes() {
    let home = tempfile::tempdir().unwrap();
    let (state, first) = pin_record(home.path(), "first", RunState::Active);
    let (_, second) = pin_record(home.path(), "second", RunState::Parked);
    let (bad_state, bad) = pin_record(home.path(), "first", RunState::Active);
    fs::write(bad_state.workflow(&bad.id).record, "{").unwrap();
    fs::create_dir_all(state.workflow(&WorkflowRunId::new()).home).unwrap();
    let actual: std::collections::BTreeSet<_> = unfinished_under(home.path(), RunRead::BestEffort)
        .unwrap()
        .into_iter()
        .map(|run| run.id.to_string())
        .collect();
    assert_eq!(actual, [first.id.to_string(), second.id.to_string()].into());
}

#[test]
fn strict_run_reader_refuses_an_unreadable_room_directory_but_best_effort_keeps_others() {
    let home = tempfile::tempdir().unwrap();
    let (_, first) = pin_record(home.path(), "first", RunState::Active);
    let (state, run) = pin_record(home.path(), "broken", RunState::Active);
    let dir = state.workflow(&run.id).home.parent().unwrap().to_owned();
    fs::remove_dir_all(&dir).unwrap();
    fs::write(&dir, "not a directory").unwrap();
    let strict = unfinished_under(home.path(), RunRead::Strict);
    let best = unfinished_under(home.path(), RunRead::BestEffort).unwrap();
    assert!(
        strict
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains(dir.to_str().unwrap())),
        "{strict:?}"
    );
    assert_eq!(
        best.into_iter().map(|run| run.id).collect::<Vec<_>>(),
        [first.id]
    );
}

#[test]
fn run_address_round_trips() {
    let address: RunAddress = "review#feat-x".parse().unwrap();
    assert_eq!(address.to_string(), "review#feat-x");
    assert_eq!("review#feat-x".parse::<RunAddress>().unwrap(), address);
}

#[test]
fn team_ref_lane_round_trips_external_and_reads_an_old_unnamed_lane_dashed() {
    let named = TeamRef {
        name: "forge".to_owned(),
        channel: Some(LaneName::dashed("feat-x")),
        stage: Some("Build".to_owned()),
    };
    let external = TeamRef {
        channel: None,
        ..named.clone()
    };
    for (team, lane) in [(&named, "feat-x"), (&external, "external")] {
        let value = serde_json::to_value(team).unwrap();
        assert_eq!(
            value,
            json!({"name": "forge", "channel": lane, "stage": "Build"})
        );
        assert_eq!(&serde_json::from_value::<TeamRef>(value).unwrap(), team);
    }
    let old: TeamRef =
        serde_json::from_value(json!({"name": "forge", "channel": "feat x", "stage": null}))
            .unwrap();
    assert_eq!(old.channel, Some(LaneName::dashed("feat-x")));
}

#[test]
fn run_address_rejects_missing_or_empty_parts() {
    for raw in ["review", "", "#lane", "review#", "#", "review#lane#part"] {
        assert!(raw.parse::<RunAddress>().is_err(), "{raw:?}");
    }
}

#[test]
fn run_address_uses_the_recorded_scope_without_reading_the_checkout() {
    let mut record = record();
    for (kind, path, lane) in [
        (ScopeKind::Worktree, "/project/.worktrees/feat-x", "feat-x"),
        // The project root is trunk's checkout whatever the scope's kind.
        (ScopeKind::Worktree, "/project", "main"),
        (ScopeKind::Project, "/project", "main"),
        (ScopeKind::Worktree, "/no-longer-present/deleted", "deleted"),
    ] {
        record.scope = Scope {
            kind,
            path: path.into(),
        };
        assert_eq!(record.address().to_string(), format!("review#{lane}"));
    }
}

#[test]
fn an_address_takes_the_newest_run_after_local_live_and_real_runs_first() {
    let run = |id: u8, root: &str, state: RunState, once: bool, second: i64| {
        let mut run = record();
        run.id =
            WorkflowRunId::parse(&format!("wf_0190b8172c00700080000000000000{id:02}")).unwrap();
        run.project_root = root.into();
        run.scope.path = format!("{root}/.worktrees/feat-x").into();
        run.state = state;
        run.once = once;
        run.created_at = Timestamp::from_second(second).unwrap();
        run
    };
    let address: RunAddress = "review#feat-x".parse().unwrap();
    let here = Some(Path::new("/project"));
    let newest = |runs: &[RunRecord]| address.newest(runs, here).map(|run| run.id.clone());
    let older = run(1, "/project", RunState::Active, false, 10);
    let newer = run(2, "/project", RunState::Active, false, 20);
    let ended = run(3, "/project", RunState::Done, false, 30);
    let elsewhere = run(4, "/other", RunState::Active, false, 40);
    let tried = run(5, "/project", RunState::Active, true, 50);
    let mut other_lane = run(6, "/project", RunState::Active, false, 60);
    other_lane.scope.path = "/project/.worktrees/feat-y".into();
    let all = [&older, &newer, &ended, &elsewhere, &tried, &other_lane].map(Clone::clone);
    assert_eq!(newest(&all), Some(newer.id.clone()));
    assert_eq!(
        newest(&[older.clone(), ended.clone()]),
        Some(older.id.clone())
    );
    assert_eq!(newest(std::slice::from_ref(&ended)), Some(ended.id.clone()));
    assert_eq!(
        newest(&[older.clone(), elsewhere.clone()]),
        Some(older.id.clone())
    );
    assert_eq!(
        newest(&[ended.clone(), elsewhere.clone()]),
        Some(ended.id.clone())
    );
    assert_eq!(
        newest(std::slice::from_ref(&elsewhere)),
        Some(elsewhere.id.clone())
    );
    assert_eq!(newest(std::slice::from_ref(&tried)), Some(tried.id.clone()));
    assert_eq!(newest(&[other_lane]), None);
}

/// A run shows as its address only while that address, typed back, selects it: of two live runs
/// at one address (a deployed workflow and a loop task's inline run of the same name), the older
/// shows its id; a try, a run this RimZ cannot read, and an unknown id show their ids; a finished
/// run its address once no live run holds it.
#[test]
fn a_run_shows_as_its_address_only_where_the_address_selects_it() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let run = |id: u8, lane: &str, second: i64| {
        let mut run = record();
        run.id =
            WorkflowRunId::parse(&format!("wf_0190b8172c00700080000000000000{id:02}")).unwrap();
        run.scope.path = format!("/project/.worktrees/{lane}").into();
        run.created_at = Timestamp::from_second(second).unwrap();
        run
    };
    let deployed = run(1, "feat-x", 10);
    let mut task = run(2, "feat-x", 20);
    task.origin = RunOrigin::Inline;
    task.task = Some("review".into());
    let mut tried = run(3, "feat-y", 30);
    tried.once = true;
    let mut ended = run(4, "feat-z", 40);
    ended.state = RunState::Done;
    ended.terminal_at = Some(ended.created_at);
    let stranded = run(5, "feat-w", 50);
    for run in [&deployed, &task, &tried, &ended, &stranded] {
        initialize(&state, run, &guard).unwrap();
    }
    finish(&state, &ended.id, Some(b"h"), &guard).unwrap();
    let mut older = serde_json::to_value(&stranded).unwrap();
    older["version"] = json!(VERSION - 1);
    fs::write(
        &state.workflow(&stranded.id).record,
        serde_json::to_vec(&older).unwrap(),
    )
    .unwrap();
    let names = RunNames::new(&state);
    let shown = |run: &RunRecord| names.shown(&run.id);
    assert_eq!(shown(&deployed), deployed.id.to_string());
    assert_eq!(shown(&task), "review#feat-x");
    assert_eq!(shown(&tried), tried.id.to_string());
    assert_eq!(shown(&ended), "review#feat-z");
    assert_eq!(shown(&stranded), stranded.id.to_string());
    let unknown = WorkflowRunId::new();
    assert_eq!(names.shown(&unknown), unknown.to_string());
    let selected = |raw: &str| {
        names
            .select(&raw.parse().unwrap())
            .map(|run| run.id.clone())
    };
    assert_eq!(selected("review#feat-x"), Some(task.id.clone()));
    assert_eq!(selected("review#feat-z"), Some(ended.id.clone()));
    assert_eq!(selected("review#feat-w"), None);
}

#[test]
fn manual_origin_round_trips_and_stays_absent_unless_set() {
    let mut value = serde_json::to_value(record()).unwrap();
    assert!(value.get("manual").is_none());
    value["manual"] = json!(true);
    let manual: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(manual).unwrap(), value);
}

#[test]
fn start_arguments_round_trip_and_stay_absent_at_their_defaults() {
    let mut value = serde_json::to_value(record()).unwrap();
    assert!(value.get("give_up").is_none() && value.get("capacity").is_none());
    value["give_up"] = json!("2026-01-01T02:00:00Z");
    value["capacity"] = json!("skip");
    let started: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(started.capacity, StartCapacity::Skip);
    assert_eq!(serde_json::to_value(started).unwrap(), value);
}

#[test]
fn a_try_round_trips_its_flag_ending_and_untaken_route_and_a_start_entry_its_input() {
    let mut value = serde_json::to_value(record()).unwrap();
    assert!(value.get("once").is_none());
    value["once"] = json!(true);
    value["outcome"] = json!({"ending":{"kind":"tried"}, "acted":false});
    let tried: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert!(tried.once);
    assert_eq!(tried.outcome.as_ref().unwrap().ending, RunEnding::Tried);
    assert_eq!(serde_json::to_value(tried).unwrap(), value);

    let at = json!({"seq":null, "at":"2026-01-01T00:00:00Z", "node":"pr"});
    let entry = |extra: Value| {
        let mut entry = at.clone();
        entry
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let decoded: JournalEntry = serde_json::from_value(entry.clone()).unwrap();
        assert_eq!(serde_json::to_value(&decoded).unwrap(), entry);
        decoded.event
    };
    assert_eq!(
        entry(json!({"event":"terminal", "state":"done", "tried":"@failed"})),
        JournalEvent::Terminal {
            state: RunState::Done,
            tried: Some("@failed".into())
        }
    );
    assert_eq!(
        entry(json!({"event":"terminal", "state":"done"})),
        JournalEvent::Terminal {
            state: RunState::Done,
            tried: None
        }
    );
    assert_eq!(
        entry(json!({"event":"start", "input":{"outcome":"pass", "key":7}})),
        JournalEvent::Start {
            input: Some(json!({"outcome":"pass", "key":7}))
        }
    );
    assert_eq!(
        entry(json!({"event":"start"})),
        JournalEvent::Start { input: None }
    );
}

#[test]
fn capacity_wait_round_trips_and_other_waits_omit_its_fields() {
    let wait = json!({
        "node":"build", "outcome":"capacity", "detail":"no capacity for claude",
        "first":"2026-01-01T00:00:00Z", "last":"2026-01-01T00:00:00Z",
        "count":2, "recheck_at":null, "kinds":["claude", "codex"],
        "gives_up_at":"2026-01-01T02:00:00Z", "hold":"check_failed"
    });
    let decoded: Wait = serde_json::from_value(wait.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), wait);
    let mut other = wait;
    other.as_object_mut().unwrap().remove("kinds");
    other.as_object_mut().unwrap().remove("gives_up_at");
    other.as_object_mut().unwrap().remove("hold");
    let decoded: Wait = serde_json::from_value(other).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert!(encoded.get("kinds").is_none());
    assert!(encoded.get("gives_up_at").is_none());
    assert!(encoded.get("hold").is_none());
}

#[test]
fn a_hold_round_trips_its_stop_and_stays_absent_unless_held() {
    let mut value = serde_json::to_value(record()).unwrap();
    assert!(value["attempt"].get("held_on").is_none());
    let holds = [
        json!({"since":"2026-01-01T00:00:00Z", "handle":"@fox", "stop":"error",
               "class":"not_entitled", "text":"plan lapsed"}),
        json!({"since":"2026-01-01T00:00:00Z", "stop":"error"}),
        json!({"since":"2026-01-01T00:00:00Z", "handle":"@fox", "stop":"budget"}),
    ];
    for hold in holds {
        value["attempt"]["held_since"] = hold["since"].clone();
        value["attempt"]["held_on"] = hold;
        let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
    }
}

#[test]
fn a_held_session_round_trips_with_its_verdict() {
    let turn = json!({
        "agent_id":"leader", "started_at":"2026-01-01T00:00:00Z",
        "ended_at":"2026-01-01T00:00:01Z"
    });
    let verdict = json!({
        "node":"pr", "entered_at":"2026-01-01T00:00:01Z", "at":"2026-01-01T00:00:01Z",
        "target":"@done", "object":{"outcome":"merged"}, "reason":"user"
    });
    let mut value = serde_json::to_value(record()).unwrap();
    value["held"] = json!({
        "attempt":value["attempt"], "entered_at":"2026-01-01T00:00:00Z",
        "verdict":verdict
    });
    value["held"]["attempt"]["judged"] = turn.clone();
    value["attempt"]["judged"] = turn;
    value["attempt"]["ending"] = json!({
        "outcome":"ended", "cause":"lifecycle", "route":verdict
    });
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    let encoded = serde_json::to_value(&decoded).unwrap();
    assert_eq!(
        (&encoded["held"], &encoded["attempt"]),
        (&value["held"], &value["attempt"])
    );
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &decoded, &guard).unwrap();
    assert_eq!(load(&state.workflow(&decoded.id)).unwrap(), decoded);
}

#[test]
fn a_rest_check_turn_end_journal_preserves_each_decision() {
    let results: Vec<_> = ["kept", "closed", "discarded", "interrupted"]
        .into_iter()
        .map(|action| {
            let row = json!({
                "event":"turn_end", "seq":1, "at":"2026-01-01T00:00:01Z", "node":"watch",
                "action":action, "check":"pr", "outcome":"merged", "target":"@done",
                "reason":"leader moved on", "turn":{
                    "agent_id":"leader", "started_at":"2026-01-01T00:00:00Z",
                    "ended_at":"2026-01-01T00:00:01Z"
                }
            });
            let decoded = serde_json::from_value::<JournalEntry>(row.clone());
            (
                action,
                decoded
                    .ok()
                    .map(|entry| serde_json::to_value(entry).unwrap())
                    == Some(row),
            )
        })
        .collect();
    assert_eq!(
        results,
        vec![
            ("kept", true),
            ("closed", true),
            ("discarded", true),
            ("interrupted", true)
        ]
    );
}

#[test]
fn records_without_a_held_session_omit_rest_check_fields() {
    let value = serde_json::to_value(record()).unwrap();
    assert!(value.get("held").is_none());
    assert!(value["attempt"].get("judged").is_none());
    assert_eq!(
        serde_json::from_value::<RunRecord>(value).unwrap(),
        record()
    );
}

#[test]
fn provenance_and_window_evidence_survive_record_and_journal_round_trips() {
    let untasked = serde_json::to_value(record()).unwrap();
    let mut value = untasked.clone();
    value["task"] = json!("babysit-pr");
    value["wait"] = json!({
        "node":"pr", "outcome":"window", "detail":"5h.left=12<42",
        "first":"2026-01-01T00:00:00Z", "last":"2026-01-01T00:00:00Z",
        "count":2, "recheck_at":null
    });
    let decoded: RunRecord = serde_json::from_value(value.clone()).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(encoded["task"], value["task"]);
    assert_eq!(encoded["wait"]["detail"], value["wait"]["detail"]);
    let event = json!({
        "event":"wait", "seq":null, "at":"2026-01-01T00:00:00Z", "node":"pr",
        "outcome":"window", "detail":"5h.left=12<42", "count":2,
        "first":"2026-01-01T00:00:00Z", "last":"2026-01-01T00:00:00Z", "recheck_at":null
    });
    let decoded: JournalEntry = serde_json::from_value(event.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["detail"],
        event["detail"]
    );
    let decoded: RunRecord = serde_json::from_value(untasked).unwrap();
    assert!(serde_json::to_value(decoded).unwrap()["task"].is_null());
}

#[test]
fn durable_record_round_trip_and_json_shape() {
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let seed = record();
    let relative = crate::store::notes::allocate(
        &state,
        crate::store::notes::Kind::Workflow {
            workflow: seed.workflow.clone(),
            run: seed.id.clone(),
        },
        &seed.scope.path,
        seed.created_at,
    )
    .unwrap();
    let mut value = serde_json::to_value(seed).unwrap();
    value["notes"] = json!(relative);
    value["prev_attempt"] = json!({
        "step":"merge", "outcome":"died", "seq":1, "visit":1, "key":null, "routed_by":"red",
        "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:01Z",
        "handle":null, "run_id":null, "claim_path":"/work/1.claim.json",
        "head_start":"abc", "head_end":"def", "children":null,
        "code":7, "log":"/work/1.step.log"
    });
    for field in ["asked", "cost_usd"] {
        value["prev_attempt"][field] = Value::Null;
    }
    let record: RunRecord = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&record).unwrap()["prev_attempt"],
        value["prev_attempt"]
    );
    assert!(
        value.get("fire").is_none(),
        "a hand-started run keeps no fire"
    );
    for fire in [
        json!({"kind":"manual", "task":"land"}),
        json!({"kind":"scheduled", "task":"land"}),
        json!({"kind":"scheduled", "task":"land", "trigger":"on the signal `deploy.done`"}),
        json!({"kind":"manual", "task":"land",
            "booked_by":[{"kind":"claude", "agent_id":"booker"}]}),
        json!({"kind":"scheduled", "task":"land",
            "booked_by":[{"kind":"claude", "agent_id":"booker"},
                {"kind":"codex", "agent_id":"its-launcher"}]}),
    ] {
        let mut fired = value.clone();
        fired["fire"] = fire.clone();
        let kept =
            serde_json::to_value(serde_json::from_value::<RunRecord>(fired).unwrap()).unwrap();
        assert_eq!(kept["fire"], fire);
    }
    let event = json!({
        "event":"attempt", "seq":1, "at":"2026-01-01T00:00:01Z", "node":"merge",
        "outcome":"died", "visit":1, "key":null, "routed_by":"red", "digests":{},
        "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:01Z",
        "code":7, "log":"/work/1.step.log"
    });
    let decoded: JournalEntry = serde_json::from_value(event.clone()).unwrap();
    let encoded = serde_json::to_value(decoded).unwrap();
    assert_eq!(
        (&encoded["code"], &encoded["log"]),
        (&event["code"], &event["log"])
    );
    assert_eq!(encoded["routed_by"], "red");
    let signal = json!({
        "event":"signal", "seq":1, "at":"2026-01-01T00:00:01Z", "node":"build",
        "signal":"ci.failed", "message":crate::ids::MessageId::new()
    });
    let decoded: JournalEntry = serde_json::from_value(signal.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), signal);
    assert!(encoded.get("message").is_none() && value["attempt"].get("message").is_none());
    let message = crate::ids::MessageId::new();
    let mut sent = encoded.clone();
    sent["handle"] = json!("@planner");
    sent["message"] = json!(message);
    let decoded: JournalEntry = serde_json::from_value(sent.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), sent);
    let mut attempt = value["attempt"].clone();
    attempt["message"] = json!(message);
    let decoded: LiveAttempt = serde_json::from_value(attempt.clone()).unwrap();
    assert_eq!(decoded.message, Some(message));
    assert_eq!(
        serde_json::to_value(decoded).unwrap()["message"],
        attempt["message"]
    );
    let paths = state.workflow(&record.id);
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &record, &guard).unwrap();
    assert!(
        paths.record.is_file(),
        "initialization must publish the record"
    );
    assert_eq!(load(&paths).unwrap(), record);
    assert!(
        !crate::disk::paths::workflow_spec_file(&paths.home).exists(),
        "a run home holds no copy of its definition"
    );
    assert!(paths.work.is_dir());
    let children: Vec<_> = fs::read_dir(&paths.work)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        children.contains(&"notes".into()),
        "nodes need notes/ at creation"
    );
    assert!(
        children.contains(&"signs".into()),
        "judges need signs/ at creation"
    );
    assert_eq!(live_runs(&state).unwrap(), vec![record.clone()]);
    let mut next = record.clone();
    next.next_seq = 3;
    write(&paths, &next, &guard).unwrap();
    assert_eq!(load(&paths).unwrap().next_seq, 3);
    // Set, so the snapshot pins the key a task's fire writes.
    let mut record = record;
    record.loop_fire = Some(
        crate::ids::FireId::parse(
            "fire_0190b8172c0070008000000000000002_0190b8172c0070008000000000000003",
        )
        .unwrap(),
    );
    record.starter = Some(RunStarter::Agent {
        kind: AgentKind::new_unchecked("claude"),
        launch: AgentSessionId::from("launch-1"),
    });
    record.depth = 2;
    let started = StartedRun {
        id: WorkflowRunId::parse("wf_0190b8172c0070008000000000000004").unwrap(),
        workflow: "build".into(),
        ended: Some(JoinedEnding {
            ending: RunEnding::Failed,
            reason: Some("tests failed".into()),
            checkout: Some("/project-worktrees/build".into()),
            branch: Some("build".into()),
            notes: Some("/notes/build".into()),
        }),
    };
    record.attempt.as_mut().unwrap().runs = Some(StartedRuns {
        base: "0123abc".into(),
        runs: vec![started.clone()],
    });
    record.joined = vec![started];
    let value = serde_json::to_value(record).unwrap();
    insta::assert_json_snapshot!("record", value);
}

#[test]
fn listing_skips_unreadable_records_but_named_loads_fail() {
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let record = record();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    initialize(&state, &record, &guard).unwrap();
    let mut future = record.clone();
    future.version = VERSION + 1;
    for contents in [
        None,
        Some(b"corrupt".to_vec()),
        Some(serde_json::to_vec(&future).unwrap()),
    ] {
        let paths = state.workflow(&WorkflowRunId::new());
        fs::create_dir_all(&paths.home).unwrap();
        if let Some(contents) = contents {
            fs::write(&paths.record, contents).unwrap();
        }
        assert!(load(&paths).is_err(), "named loads must still fail");
    }
    assert_eq!(
        live_runs(&state).ok(),
        Some(vec![record]),
        "unreadable records must not hide healthy runs"
    );
}

#[test]
fn journal_preserves_entries_and_tolerates_torn_or_unknown_rows() {
    let home = tempfile::tempdir().unwrap();
    let state =
        StatePaths::for_project_root_under(std::path::Path::new("/project"), home.path()).unwrap();
    let record = record();
    let paths = state.workflow(&record.id);
    fs::create_dir_all(&paths.home).unwrap();
    let entry = JournalEntry {
        seq: Some(1),
        at: record.created_at,
        node: "pr".into(),
        event: JournalEvent::Route {
            outcome: "timeout".into(),
            input: json!({"outcome":"timeout"}),
        },
    };
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    append(&paths, &entry, &guard).unwrap();
    assert_eq!(journal(&paths).unwrap(), vec![entry.clone()]);
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&paths.journal)
        .unwrap();
    writeln!(file, "{{\"event\":\"future\"}}").unwrap();
    writeln!(file, "corrupt row").unwrap();
    let mut future_field = serde_json::to_value(&entry).unwrap();
    future_field["future"] = json!(true);
    writeln!(file, "{future_field}").unwrap();
    write!(file, "{{\"event\":").unwrap();
    assert_eq!(journal(&paths).unwrap(), vec![entry.clone(), entry]);
}

#[test]
fn a_moved_home_reads_its_recorded_logs_under_the_home_it_was_read_from() {
    let home = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root_under(home.path(), home.path()).unwrap();
    let guard = WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    let mut record = record();
    let live = state.workflow(&record.id);
    let finished = state.finished_workflow(&record.id);
    initialize(&state, &record, &guard).unwrap();
    let step = live.step_log(1);
    let check = live.check_log(2);
    let outside = std::path::Path::new("/elsewhere/build.log");
    let failure = json!({"outcome":"failed", "cause":"died", "detail":{"log":step, "code":3}});
    let events = [
        json!({"event":"attempt", "outcome":"died", "visit":1, "key":null, "digests":{},
            "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:01Z",
            "log":step}),
        json!({"event":"evaluation", "outcome":"fail", "object":{"outcome":"fail", "log":check},
            "digests":{}, "started_at":"2026-01-01T00:00:00Z",
            "ended_at":"2026-01-01T00:00:01Z"}),
        json!({"event":"evaluation", "origin":"engine", "outcome":"died", "object":failure,
            "digests":{}, "started_at":"2026-01-01T00:00:00Z",
            "ended_at":"2026-01-01T00:00:01Z"}),
        json!({"event":"route", "outcome":"died", "input":failure}),
        json!({"event":"attempt", "outcome":"ended", "visit":1, "key":null, "digests":{},
            "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:01Z",
            "log":outside}),
    ];
    let entries: Vec<JournalEntry> = events
        .into_iter()
        .map(|mut event| {
            event["seq"] = json!(1);
            event["at"] = json!("2026-01-01T00:00:00Z");
            event["node"] = json!("build");
            serde_json::from_value(event).unwrap()
        })
        .collect();
    for entry in &entries {
        append(&live, entry, &guard).unwrap();
    }
    record.input = Some(failure.clone());
    record.prev_attempt = Some(
        serde_json::from_value(json!({"step":"build", "outcome":"died", "seq":1, "visit":1,
            "key":null, "started_at":"2026-01-01T00:00:00Z", "ended_at":"2026-01-01T00:00:01Z",
            "handle":null, "run_id":null, "claim_path":live.claim(1), "head_start":null,
            "head_end":null, "log":step, "children":null}))
        .unwrap(),
    );
    record.attempt = None;
    record.state = RunState::Failed;
    write(&live, &record, &guard).unwrap();
    assert_eq!(
        journal(&live).unwrap(),
        entries,
        "the live home reads as recorded"
    );
    assert_eq!(
        load(&live).unwrap(),
        record,
        "the live home reads as recorded"
    );
    fs::create_dir_all(finished.home.parent().unwrap()).unwrap();
    fs::rename(&live.home, &finished.home).unwrap();
    let raw = fs::read(&finished.journal).unwrap();

    let moved_step = json!(finished.step_log(1));
    let moved_check = json!(finished.check_log(2));
    let logs = journal(&finished)
        .unwrap()
        .into_iter()
        .map(|entry| match entry.event {
            JournalEvent::Attempt { log, .. } => json!(log),
            JournalEvent::Evaluation { object, .. } => object,
            JournalEvent::Route { input, .. } => input,
            event => panic!("unexpected {event:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(logs[0], moved_step);
    assert_eq!(logs[1]["log"], moved_check);
    assert_eq!(logs[2]["detail"]["log"], moved_step);
    assert_eq!(logs[3]["detail"]["log"], moved_step);
    assert_eq!(logs[3]["detail"]["code"], 3);
    assert_eq!(
        logs[4],
        json!(outside),
        "a path outside the run's homes reads as recorded"
    );
    for read in [
        load(&finished).unwrap(),
        match classify(&finished).unwrap() {
            RunClass::Current(record) => *record,
            class => panic!("unexpected {class:?}"),
        },
    ] {
        assert_eq!(read.input.as_ref().unwrap()["detail"]["log"], moved_step);
        assert_eq!(json!(read.prev_attempt.unwrap().log), moved_step);
    }
    assert_eq!(
        fs::read(&finished.journal).unwrap(),
        raw,
        "reading writes nothing back"
    );
}
