use super::super::catalog::TaskSource;
use super::super::tests::{seconds_before, zdt};
use super::*;

use crate::config::TaskEntry;

mod holds;

const NAME: &str = "task";

#[test]
fn workflow_ticks_honor_backoff_pace_due_edges_and_retire_inactive_runs() {
    let root = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&state).unwrap();
    let mut record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version":crate::store::workflow::VERSION,"id":"wf_0190b8172c0070008000000000000001","workflow":"review",
        "spec_dir":"/definitions/review","spec_digest":"abc","scope":{"kind":"worktree","path":root.path()},
        "project_root":root.path(),"created_at":"2026-01-01T00:00:00Z","state":"active","node":"pr",
        "entered_at":"2026-01-01T00:00:00Z","visits":{},"visit_grant":0,"deadline_fired":false,"next_seq":1
    })).unwrap();
    let paths = state.workflow(&record.id);
    std::fs::create_dir_all(&paths.home).unwrap();
    let cache = runtime.lane_path("workflow-tick.json");
    let mut actual = Vec::new();
    let mut expected_stamps = Vec::new();
    for (now, recheck, held, expected) in [
        (999, Some(1000), false, 0),
        (1000, None, false, 1000),
        (1029, Some(1030), false, 1000),
        (1030, Some(1030), false, 1030),
        (1040, None, false, 1030),
        (1090, None, false, 1090),
        (1150, Some(10000), false, 1090),
        (1160, Some(10000), true, 1160),
        (1190, Some(10000), true, 1160),
        (1220, Some(10000), true, 1220),
    ] {
        record.recheck_at = recheck.map(|second| Timestamp::from_second(second).unwrap());
        let mut value = serde_json::to_value(&record).unwrap();
        value["held"] = if held {
            serde_json::json!({
                "attempt":{"kind":"agent", "lifecycle":"session", "seq":1, "node":"work",
                    "started_at":record.created_at, "host":{"pid":1,"start":null},
                    "run":{"run_id":crate::ids::RunId::new(),"handle":"@worker"},
                    "judged":{"agent_id":"leader", "started_at":record.created_at,
                        "ended_at":record.created_at}},
                "entered_at":record.created_at
            })
        } else {
            serde_json::Value::Null
        };
        record = serde_json::from_value(value).unwrap();
        std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
        tick_workflows(
            &runtime,
            root.path(),
            Timestamp::from_second(now).unwrap(),
            LoopRunHost::Detached,
        );
        let stamps: BTreeMap<crate::ids::WorkflowRunId, Timestamp> =
            crate::disk::atomic::read_json_cache(&cache);
        actual.push((now, stamps.get(&record.id).copied()));
        expected_stamps.push((
            now,
            (expected != 0).then(|| Timestamp::from_second(expected).unwrap()),
        ));
    }
    assert_eq!(actual, expected_stamps);
    record.held = None;
    record.state = crate::store::workflow::RunState::Parked;
    std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    tick_workflows(
        &runtime,
        root.path(),
        Timestamp::from_second(1100).unwrap(),
        LoopRunHost::Detached,
    );
    let stamps: BTreeMap<crate::ids::WorkflowRunId, Timestamp> =
        crate::disk::atomic::read_json_cache(&cache);
    assert!(stamps.is_empty());
    record.state = crate::store::workflow::RunState::Failed;
    record.task = Some("nightly".to_owned());
    record.recheck_at = Some(Timestamp::from_second(10000).unwrap());
    for (report, now, expected) in [
        (crate::store::workflow::LoopReport::Owed, 1200, Some(1200)),
        (crate::store::workflow::LoopReport::Sent, 1300, None),
    ] {
        record.loop_report = report;
        std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
        tick_workflows(
            &runtime,
            root.path(),
            Timestamp::from_second(now).unwrap(),
            LoopRunHost::Detached,
        );
        let stamps: BTreeMap<crate::ids::WorkflowRunId, Timestamp> =
            crate::disk::atomic::read_json_cache(&cache);
        assert_eq!(
            stamps.get(&record.id).copied(),
            expected.map(|second| Timestamp::from_second(second).unwrap()),
            "a finished run is due only while it owes its report: {report:?}"
        );
    }
}

#[test]
fn an_owed_reports_retry_gap_doubles_from_one_tick_up_to_an_hour() {
    let root = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&state).unwrap();
    let cache = runtime.lane_path("workflow-tick.json");
    let start = 1_000_000;
    for (ended, ticks) in [
        // Ten minutes finished at its last attempt: the next waits ten minutes, not a tick.
        (
            start - 600,
            [
                (start, start),
                (start + 60, start),
                (start + 600, start + 600),
            ],
        ),
        // Days finished: the gap stops at an hour.
        (
            start - 3 * 86_400,
            [
                (start, start),
                (start + 3599, start),
                (start + 3600, start + 3600),
            ],
        ),
    ] {
        let _ = std::fs::remove_file(&cache);
        let record: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
            "version":crate::store::workflow::VERSION,"id":"wf_0190b8172c0070008000000000000002","workflow":"review",
            "spec_dir":"/definitions/review","spec_digest":"abc",
            "scope":{"kind":"worktree","path":root.path()},"project_root":root.path(),
            "created_at":"2026-01-01T00:00:00Z","state":"failed","node":"pr",
            "entered_at":"2026-01-01T00:00:00Z","visits":{},"visit_grant":0,
            "deadline_fired":false,"next_seq":1,"task":"nightly","loop_report":"owed",
            "terminal_at":Timestamp::from_second(ended).unwrap()
        }))
        .unwrap();
        assert!(record.owes_loop_report());
        let paths = state.workflow(&record.id);
        std::fs::create_dir_all(&paths.home).unwrap();
        std::fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
        let mut actual = Vec::new();
        for (now, _) in ticks {
            tick_workflows(
                &runtime,
                root.path(),
                Timestamp::from_second(now).unwrap(),
                LoopRunHost::Detached,
            );
            let stamps: BTreeMap<crate::ids::WorkflowRunId, Timestamp> =
                crate::disk::atomic::read_json_cache(&cache);
            actual.push((now, stamps.get(&record.id).map(|at| at.as_second())));
        }
        let expected = ticks.map(|(now, stamp)| (now, Some(stamp)));
        assert_eq!(actual, expected, "finished at {ended}");
    }
}

#[test]
fn a_home_awaiting_the_pass_spawns_it_at_most_once_a_tick() {
    let root = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&state).unwrap();
    let cache = runtime.lane_path("workflow-reconcile.json");
    let paths = state.workflow(&"wf_0190b8172c0070008000000000000003".parse().unwrap());
    std::fs::create_dir_all(&paths.home).unwrap();
    let tick = |now: i64| {
        tick_workflows(
            &runtime,
            root.path(),
            Timestamp::from_second(now).unwrap(),
            LoopRunHost::Detached,
        );
        crate::disk::atomic::read_json_cache::<Option<Timestamp>>(&cache).map(|at| at.as_second())
    };
    // A home without a record leaves the pass alone.
    assert_eq!(tick(1000), None);
    let older = crate::store::workflow::VERSION - 1;
    std::fs::write(
        &paths.record,
        format!(r#"{{"version":{older},"state":"active"}}"#),
    )
    .unwrap();
    let actual: Vec<_> = [1000, 1059, 1060, 1100, 1120]
        .into_iter()
        .map(|now| (now, tick(now)))
        .collect();
    assert_eq!(
        actual,
        [
            (1000, Some(1000)),
            (1059, Some(1000)),
            (1060, Some(1060)),
            (1100, Some(1060)),
            (1120, Some(1120))
        ]
    );
}

fn worktree_tasks(
    hold: Option<&str>,
) -> (
    tempfile::TempDir,
    BTreeMap<String, LoadedTask>,
    Vec<PathBuf>,
) {
    let root = tempfile::tempdir().unwrap();
    let owned: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|name| root.path().join(name))
        .collect();
    for path in &owned {
        std::fs::create_dir(path).unwrap();
    }
    let tasks = one(loaded(TaskEntry {
        root: root.path().to_owned(),
        agent: Some("claude".into()),
        prompt: Some("repair".into()),
        stay: true,
        each_worktree: true,
        when: Some(vec!["team.stage=Done".into()]),
        hold: hold.map(str::to_owned),
        ..Default::default()
    }));
    (root, tasks, owned)
}

#[test]
fn worktree_scopes_normalize_marker_components() {
    let (root, tasks, _) = worktree_tasks(None);
    let owned = vec![root.path().join("b/../a")];
    let scoped = scoped_tasks(&tasks, &Default::default(), &owned);
    let scope = scoped.values().next().unwrap();
    assert_eq!(
        scope.checkout.as_deref(),
        Some(root.path().join("a").as_path())
    );
}

/// A launch the resident path remembered before every fire became a workflow run.
fn resident(at: Timestamp) -> super::super::launch_ledger::WorkflowLaunchRecord {
    super::super::launch_ledger::WorkflowLaunchRecord {
        at,
        run: None,
        scope: crate::store::workflow::ScopeKind::Worktree,
        leader: Some("otter".into()),
        destination: None,
    }
}

#[test]
fn resident_memory_holds_a_checkout_only_for_a_remembering_task() {
    let (_root, tasks, owned) = worktree_tasks(None);
    let now = Timestamp::from_second(1000).unwrap();
    let mut workflows = super::super::launch_ledger::WorkflowLedger::default();
    workflows.tasks.insert(
        NAME.into(),
        BTreeMap::from([(owned[0].clone(), resident(now))]),
    );
    let mut entry = tasks[NAME].entry().clone();
    let stay = scoped_tasks(&one(loaded(entry.clone())), &workflows, &owned).len();
    entry.agent = None;
    entry.stay = false;
    entry.workflow = Some("review".into());
    entry.when = None;
    entry.every = Some("1h".into());
    let scheduled = scoped_tasks(&one(loaded(entry)), &workflows, &owned).len();
    assert_eq!((stay, scheduled), (1, 2));
}

#[test]
fn condition_workflow_scopes_use_only_workflow_memory_and_project_entries_cover_all() {
    use super::super::launch_ledger::{WorkflowLaunchRecord, WorkflowLedger};
    use crate::store::workflow::ScopeKind;
    let (root, tasks, owned) = worktree_tasks(None);
    let now = Timestamp::now();
    let mut workflows = WorkflowLedger::default();
    let record = WorkflowLaunchRecord {
        at: now,
        run: Some(crate::ids::WorkflowRunId::new()),
        scope: ScopeKind::Worktree,
        leader: None,
        destination: None,
    };
    let mut results = Vec::new();
    for (stay, condition, each_worktree, project) in [
        (false, true, true, false),
        (false, true, false, false),
        (false, true, true, true),
        (false, false, true, false),
        (true, true, true, false),
    ] {
        let entry = TaskEntry {
            stay,
            agent: stay.then(|| "claude".into()),
            workflow: (!stay).then(|| "review".into()),
            when: condition.then(|| vec!["team.stage=Done".into()]),
            every: (!condition).then(|| "1h".into()),
            each_worktree,
            ..tasks[NAME].entry().clone()
        };
        let key = if project || !each_worktree {
            root.path().to_owned()
        } else {
            owned[0].clone()
        };
        workflows.tasks.insert(
            NAME.into(),
            BTreeMap::from([(
                key,
                WorkflowLaunchRecord {
                    scope: if project {
                        ScopeKind::Project
                    } else {
                        ScopeKind::Worktree
                    },
                    ..record.clone()
                },
            )]),
        );
        results.push(scoped_tasks(&one(loaded(entry)), &workflows, &owned).len());
    }
    // A stay task runs as a workflow, so its run's memory holds its checkout too.
    assert_eq!(results, vec![1, 0, 0, 2, 1]);
}

#[test]
fn malformed_launch_memory_suppresses_every_remembering_task() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let mut tasks: BTreeMap<String, LoadedTask> = BTreeMap::new();
    for (name, stay, workflow) in [
        ("workflow", false, true),
        ("resident", true, false),
        ("ordinary", false, false),
    ] {
        tasks.insert(
            name.into(),
            loaded(TaskEntry {
                root: root.path().to_owned(),
                agent: (!workflow).then(|| "claude".into()),
                prompt: Some("repair".into()),
                stay,
                workflow: workflow.then(|| "review".into()),
                when: Some(vec!["team.stage=Done".into()]),
                ..Default::default()
            }),
        );
    }
    std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
    std::fs::write(
        root.path().join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    std::fs::create_dir_all(
        super::super::launch_ledger::workflow_path(&paths)
            .parent()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        super::super::launch_ledger::workflow_path(&paths),
        "malformed",
    )
    .unwrap();
    write_temp_then_rename_cache(
        &state_path(&runtime),
        &tasks
            .keys()
            .map(|name| (name.clone(), seconds_before(now.timestamp(), 60)))
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    assert_eq!(
        fire_tasks(
            &runtime,
            Some(root.path()),
            tasks,
            &now,
            LoopRunHost::Detached,
            None
        ),
        // A resident task records its runs there too, so it fails closed beside the workflow.
        vec!["ordinary"]
    );
}

#[test]
fn workflow_launch_memory_accepts_a_silent_scope_handoff() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    crate::disk::atomic::write_temp_then_rename(
        &super::super::launch_ledger::workflow_path(&paths),
        &serde_json::json!({"version":1,"tasks":{"land":{root.path().to_str().unwrap():{
            "at":Timestamp::now(),"run":crate::ids::WorkflowRunId::new(),"scope":"project"
        }}}}),
    )
    .unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        workflow: Some("review".into()),
        when: Some(vec!["pr=open".into()]),
        ..Default::default()
    };
    assert!(recorded_launch(
        &runtime,
        Some(root.path()),
        "land",
        &entry,
        Some(&root.path().join("later"))
    ));
}

#[test]
fn worktree_conditions_select_the_checkout_and_skip_ledgered_edges() {
    let (_root, tasks, owned) = worktree_tasks(None);
    let now = zdt(2026, 6, 24, 8, 0, 0);
    for checkout in &owned {
        std::fs::create_dir_all(checkout.join(".rimz/team")).unwrap();
    }
    std::fs::write(owned[0].join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    std::fs::write(owned[1].join(".rimz/team/blackboard.md"), "Stage: Review\n").unwrap();
    let mut ledger = super::super::launch_ledger::WorkflowLedger::default();
    for stage in [None, Some("Review"), Some("Done")] {
        if let Some(stage) = stage {
            std::fs::write(
                owned[0].join(".rimz/team/blackboard.md"),
                format!("Stage: {stage}\n"),
            )
            .unwrap();
        }
        let scoped = scoped_tasks(&tasks, &ledger, &owned);
        assert_eq!(scoped.len(), if ledger.tasks.is_empty() { 2 } else { 1 });
        let planned: BTreeMap<_, _> = scoped
            .iter()
            .map(|(key, scoped)| (key.clone(), scoped.task.clone()))
            .collect();
        let windows = when::WindowReadings::new(None, now.timestamp());
        let expr = when::WhenExpr::parse(&["team.stage=Done".into()]).unwrap();
        let verdicts = scoped
            .iter()
            .map(|(key, scoped)| {
                (
                    key.clone(),
                    when::evaluate(
                        &expr,
                        scoped.checkout.as_deref().unwrap(),
                        None,
                        None,
                        None,
                        &windows,
                    ),
                )
            })
            .collect();
        let (_, stamps, _) = plan(
            &planned,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        let (actions, _, _) = plan(
            &planned,
            &stamps,
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
            &verdicts,
            &BTreeSet::new(),
        );
        if ledger.tasks.is_empty() {
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0].1, Action::Fire);
            assert_eq!(scoped[&actions[0].0].checkout.as_ref(), Some(&owned[0]));
            ledger.tasks.insert(
                NAME.into(),
                BTreeMap::from([(owned[0].clone(), resident(now.timestamp()))]),
            );
        } else {
            assert!(actions.is_empty());
            assert!(
                scoped
                    .values()
                    .all(|task| task.checkout.as_ref() != Some(&owned[0]))
            );
        }
    }
}

#[test]
fn worktree_retries_use_each_checkouts_last_fire() {
    let (_root, tasks, owned) = worktree_tasks(None);
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let scoped = scoped_tasks(&tasks, &Default::default(), &owned);
    let planned = scoped
        .iter()
        .map(|(key, scope)| (key.clone(), scope.task.clone()))
        .collect();
    let mut stamps = BTreeMap::new();
    let mut states = BTreeMap::new();
    let mut verdicts = BTreeMap::new();
    for (key, scope) in &scoped {
        stamps.insert(
            key.clone(),
            seconds_before(
                now.timestamp(),
                if scope.checkout.as_ref() == Some(&owned[0]) {
                    300
                } else {
                    299
                },
            ),
        );
        states.insert(
            key.clone(),
            WhenState {
                fingerprint: Some(("team.stage=Done".into(), None, scope.task.entry().run_dir())),
                since: seconds_before(now.timestamp(), 600),
                fired: true,
            },
        );
        verdicts.insert(
            key.clone(),
            Verdict {
                truth: Some(true),
                readings: BTreeMap::new(),
                unread: Default::default(),
            },
        );
    }
    let (actions, next_stamps, _) = plan(
        &planned,
        &stamps,
        &BTreeMap::new(),
        &now,
        &states,
        &verdicts,
        &BTreeSet::new(),
    );
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].1, Action::Fire);
    assert_eq!(scoped[&actions[0].0].checkout.as_ref(), Some(&owned[0]));
    for (key, scope) in &scoped {
        assert_eq!(
            next_stamps[key],
            if scope.checkout.as_ref() == Some(&owned[0]) {
                now.timestamp()
            } else {
                stamps[key]
            }
        );
    }
}

#[test]
fn worktree_holds_have_independent_clocks() {
    let (_root, tasks, owned) = worktree_tasks(Some("30m"));
    let now = zdt(2026, 6, 24, 8, 0, 0);
    let scoped = scoped_tasks(&tasks, &Default::default(), &owned);
    assert_eq!(scoped.len(), 2);
    let planned: BTreeMap<_, _> = scoped
        .iter()
        .map(|(key, scoped)| (key.clone(), scoped.task.clone()))
        .collect();
    let (_, mut stamps, mut states) = plan(
        &planned,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &now,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    for (minute, expected) in [
        (0, vec![]),
        (20, vec![]),
        (30, vec![&owned[0]]),
        (50, vec![&owned[0], &owned[1]]),
    ] {
        let tick = zdt(2026, 6, 24, 8, minute, 0);
        let verdicts = scoped
            .iter()
            .map(|(key, scope)| {
                (
                    key.clone(),
                    Verdict {
                        truth: Some(minute >= 20 || scope.checkout.as_ref() == Some(&owned[0])),
                        readings: BTreeMap::new(),
                        unread: Default::default(),
                    },
                )
            })
            .collect();
        let (actions, next_stamps, next_states) = plan(
            &planned,
            &stamps,
            &BTreeMap::new(),
            &tick,
            &states,
            &verdicts,
            &BTreeSet::new(),
        );
        assert!(actions.iter().all(|(_, action)| *action == Action::Fire));
        assert_eq!(
            actions
                .iter()
                .map(|(key, _)| scoped[key].checkout.as_ref().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        stamps = next_stamps;
        states = next_states;
    }
}

#[test]
fn condition_write_failure_preserves_clock_fires() {
    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    std::fs::create_dir(when_state_path(&runtime)).unwrap();
    std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
    std::fs::write(
        root.path().join(".rimz/team/blackboard.md"),
        "Stage: Plan\n",
    )
    .unwrap();
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 60);
    write_temp_then_rename_cache(
        &state_path(&runtime),
        &BTreeMap::from([("clock".to_owned(), prior), ("condition".to_owned(), prior)]),
    )
    .unwrap();
    let entry = TaskEntry {
        root: root.path().to_owned(),
        check: Some("true".into()),
        ..TaskEntry::default()
    };
    let tasks = BTreeMap::from([
        (
            "clock".to_owned(),
            loaded(TaskEntry {
                at: Some("08:05".to_owned()),
                ..entry.clone()
            }),
        ),
        (
            "condition".to_owned(),
            loaded(TaskEntry {
                when: Some(vec!["!team.stage=Done".to_owned()]),
                ..entry
            }),
        ),
    ]);
    for task in tasks.values() {
        assert!(task.trigger().is_ok(), "{:?}", task.trigger());
    }
    assert_eq!(
        fire_tasks(
            &runtime,
            Some(root.path()),
            tasks.clone(),
            &now,
            LoopRunHost::Detached,
            None
        ),
        vec!["clock"]
    );
    std::fs::remove_dir(when_state_path(&runtime)).unwrap();
    assert_eq!(
        fire_tasks(
            &runtime,
            Some(root.path()),
            tasks,
            &now,
            LoopRunHost::Detached,
            None
        ),
        vec!["condition"]
    );
}

#[test]
fn window_condition_reads_the_provider_window_under_the_elder() {
    use crate::agents::{
        AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache, WindowSpan,
    };

    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let publish = |used| {
        write_temp_then_rename_cache(
            &RateLimitsCache::shared_path(&runtime),
            &RateLimitsCache {
                entries: BTreeMap::from([(
                    "claude@default".parse().unwrap(),
                    RateLimitCacheEntry {
                        limits: AgentRateLimits {
                            windows: vec![RateLimitWindow {
                                used_percentage: Some(used),
                                resets_at: Some(seconds_before(now.timestamp(), -3_600)),
                                duration_mins: Some(WindowSpan::FiveHour.minutes()),
                                ..RateLimitWindow::default()
                            }],
                        },
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            },
        )
        .unwrap();
    };
    write_temp_then_rename_cache(
        &state_path(&runtime),
        &one(seconds_before(now.timestamp(), 60)),
    )
    .unwrap();
    let tasks = one(loaded(TaskEntry {
        root: root.path().to_owned(),
        check: Some("true".into()),
        when: Some(vec!["window.5h.left>=40".to_owned()]),
        provider: Some(crate::ids::AgentKind::new_unchecked("claude")),
        ..TaskEntry::default()
    }));
    let tick = || {
        fire_tasks(
            &runtime,
            Some(root.path()),
            tasks.clone(),
            &now,
            LoopRunHost::Detached,
            None,
        )
    };
    publish(92);
    assert!(tick().is_empty());
    publish(10);
    assert_eq!(tick(), vec![NAME]);
}

#[test]
fn window_conditions_read_each_tasks_own_account_in_one_pass() {
    use crate::agents::{
        AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache, WindowSpan,
    };

    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let entry = |used| RateLimitCacheEntry {
        limits: AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(used),
                resets_at: Some(seconds_before(now.timestamp(), -3_600)),
                duration_mins: Some(WindowSpan::FiveHour.minutes()),
                ..RateLimitWindow::default()
            }],
        },
        ..Default::default()
    };
    let prior = seconds_before(now.timestamp(), 60);
    write_temp_then_rename_cache(
        &state_path(&runtime),
        &BTreeMap::from([("pinned".to_owned(), prior), ("room".to_owned(), prior)]),
    )
    .unwrap();
    let task = |account: Option<&str>| {
        loaded(TaskEntry {
            root: root.path().to_owned(),
            agent: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            when: Some(vec!["window.5h.left>=40".to_owned()]),
            provider: Some(crate::ids::AgentKind::new_unchecked("claude")),
            account: account.map(|name| name.parse().unwrap()),
            ..TaskEntry::default()
        })
    };
    let tasks = BTreeMap::from([
        ("pinned".to_owned(), task(Some("work"))),
        ("room".to_owned(), task(None)),
    ]);
    for (room_used, work_used, fired) in [(92, 10, "pinned"), (10, 92, "room")] {
        write_temp_then_rename_cache(
            &RateLimitsCache::shared_path(&runtime),
            &RateLimitsCache {
                entries: BTreeMap::from([
                    ("claude@default".parse().unwrap(), entry(room_used)),
                    ("claude@work".parse().unwrap(), entry(work_used)),
                ]),
                ..Default::default()
            },
        )
        .unwrap();
        std::fs::remove_file(when_state_path(&runtime)).ok();
        assert_eq!(
            fire_tasks(
                &runtime,
                Some(root.path()),
                tasks.clone(),
                &now,
                LoopRunHost::Detached,
                None,
            ),
            vec![fired]
        );
    }
}

fn condition_state(hold: Option<u64>, since: Timestamp, fired: bool) -> WhenState {
    WhenState {
        fingerprint: Some((
            "team.stage=Done".to_owned(),
            hold.map(std::time::Duration::from_secs),
            TaskEntry::default().run_dir(),
        )),
        since,
        fired,
    }
}

#[test]
fn condition_replacement_resets_fired_and_hold_state() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 29 * 60);
    for hold in [None, Some("30m")] {
        let (_, _, old) = condition_tick(hold, Some(prior), None, Some(true), false, &now);
        for change in ["expression", "hold", "scope", "legacy"] {
            let mut entry = TaskEntry {
                check: Some("true".into()),
                when: Some(vec!["team.stage=Done".to_owned()]),
                hold: hold.map(str::to_owned),
                ..TaskEntry::default()
            };
            let mut old = old.clone();
            old.get_mut(NAME).unwrap().since = prior;
            match change {
                "expression" => entry.when = Some(vec!["ci=passed".to_owned()]),
                "hold" => entry.hold = Some("31m".to_owned()),
                "scope" => entry.dir = Some(PathBuf::from("/another-checkout")),
                "legacy" => {
                    old = serde_json::from_value(serde_json::json!({
                        NAME: {"since": prior, "fired": hold.is_none()}
                    }))
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let should_fire = entry.hold.is_none();
            let (actions, _, states) = plan(
                &one(loaded(entry)),
                &one(prior),
                &BTreeMap::new(),
                &now,
                &old,
                &one(Verdict {
                    truth: Some(true),
                    readings: BTreeMap::new(),
                    unread: Default::default(),
                }),
                &BTreeSet::new(),
            );
            assert_eq!(states[NAME].since, now.timestamp(), "{change}");
            assert_eq!(states[NAME].fired, should_fire, "{change}");
            assert_eq!(actions.len(), usize::from(should_fire), "{change}");
        }
    }
}

fn condition_tick(
    hold: Option<&str>,
    stamp: Option<Timestamp>,
    state: Option<WhenState>,
    verdict: Option<bool>,
    paused: bool,
    now: &Zoned,
) -> PlannedTasks {
    let task = loaded(TaskEntry {
        check: Some("true".into()),
        when: Some(vec!["team.stage=Done".to_owned()]),
        hold: hold.map(str::to_owned),
        ..TaskEntry::default()
    });
    let arming = if paused {
        BTreeMap::from([(task.key(NAME), until(seconds_before(now.timestamp(), -60)))])
    } else {
        BTreeMap::new()
    };
    plan(
        &one(task),
        &stamp.map(one).unwrap_or_default(),
        &arming,
        now,
        &state.map(one).unwrap_or_default(),
        &verdict
            .map(|ok| {
                one(Verdict {
                    truth: Some(ok),
                    readings: BTreeMap::new(),
                    unread: Default::default(),
                })
            })
            .unwrap_or_default(),
        &BTreeSet::new(),
    )
}

#[test]
fn resident_conditions_retry_at_five_minutes_until_ledgered() {
    let root = tempfile::tempdir().unwrap();
    let now = zdt(2026, 6, 24, 8, 0, 0);
    for (stay, workflow) in [(true, false), (false, false), (false, true)] {
        let workflow_entry: TaskEntry = toml::from_str("workflow='review'").unwrap();
        let tasks = one(loaded(TaskEntry {
            root: root.path().to_owned(),
            agent: (!workflow).then(|| "claude".into()),
            prompt: Some("repair".into()),
            stay,
            when: Some(vec!["team.stage=Done".into()]),
            ..if workflow {
                workflow_entry
            } else {
                Default::default()
            }
        }));
        let verdicts = one(Verdict {
            truth: Some(true),
            readings: BTreeMap::new(),
            unread: Default::default(),
        });
        let (_, mut stamps, mut states) = plan(
            &tasks,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeSet::new(),
        );
        for (minute, second, expected) in [
            (0, 0, true),
            (4, 59, false),
            (5, 0, stay || workflow),
            (9, 59, false),
            (10, 0, stay || workflow),
        ] {
            let tick = zdt(2026, 6, 24, 8, minute, second);
            // Named lost, a resident row keeps its own retry timing.
            let (actions, next_stamps, next_states) = plan(
                &tasks,
                &stamps,
                &BTreeMap::new(),
                &tick,
                &states,
                &verdicts,
                &BTreeSet::from([NAME.to_owned()]),
            );
            assert_eq!(
                actions,
                if expected {
                    vec![(NAME.into(), Action::Fire)]
                } else {
                    vec![]
                },
                "stay={stay}, {tick}"
            );
            assert_eq!(next_states[NAME].since, now.timestamp());
            assert!(next_states[NAME].fired);
            stamps = next_stamps;
            states = next_states;
        }
        if !stay {
            continue;
        }
        let paths = StatePaths::for_project_root(root.path()).unwrap();
        let runtime = RuntimePaths::for_state(&paths).unwrap();
        crate::disk::atomic::write_temp_then_rename(
            &super::super::launch_ledger::resident_path(&paths),
            &one(BTreeMap::from([(
                root.path().to_owned(),
                serde_json::json!({"at": now.timestamp(), "leader": "otter"}),
            )])),
        )
        .unwrap();
        write_temp_then_rename_cache(&state_path(&runtime), &stamps).unwrap();
        write_temp_then_rename_cache(&when_state_path(&runtime), &states).unwrap();
        std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
        std::fs::write(
            root.path().join(".rimz/team/blackboard.md"),
            "Stage: Done\n",
        )
        .unwrap();
        for hour in [9, 10] {
            assert!(
                fire_tasks(
                    &runtime,
                    Some(root.path()),
                    tasks.clone(),
                    &zdt(2026, 6, 24, hour, 0, 0),
                    LoopRunHost::Detached,
                    None
                )
                .is_empty()
            );
        }
    }
}

#[test]
fn condition_first_sight_arms_without_a_verdict() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let (actions, stamps, states) = condition_tick(None, None, None, None, false, &now);
    assert_eq!(actions, vec![(NAME.to_owned(), Action::Arm)]);
    assert_eq!(stamps, one(now.timestamp()));
    assert!(states.is_empty());
}

#[test]
fn condition_true_without_hold_fires_and_stamps() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let (actions, stamps, states) = condition_tick(
        None,
        Some(seconds_before(now.timestamp(), 1)),
        None,
        Some(true),
        false,
        &now,
    );
    assert_eq!(actions, vec![(NAME.to_owned(), Action::Fire)]);
    assert_eq!(stamps, one(now.timestamp()));
    assert_eq!(states, one(condition_state(None, now.timestamp(), true)));
}

#[test]
fn condition_hold_counts_from_first_true_and_fires_at_boundary() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 600);
    let (actions, stamps, states) =
        condition_tick(Some("30m"), Some(prior), None, Some(true), false, &now);
    assert!(actions.is_empty());
    assert_eq!(stamps, one(prior));
    let state = states[NAME].clone();
    assert_eq!(state, condition_state(Some(1800), now.timestamp(), false));
    let early = zdt(2026, 6, 24, 8, 34, 59);
    assert!(
        condition_tick(
            Some("30m"),
            Some(prior),
            Some(state.clone()),
            Some(true),
            false,
            &early
        )
        .0
        .is_empty()
    );
    let boundary = zdt(2026, 6, 24, 8, 35, 0);
    let (actions, stamps, states) = condition_tick(
        Some("30m"),
        Some(prior),
        Some(state),
        Some(true),
        false,
        &boundary,
    );
    assert_eq!(actions, vec![(NAME.to_owned(), Action::Fire)]);
    assert_eq!(stamps, one(boundary.timestamp()));
    assert!(states[NAME].fired);
}

#[test]
fn condition_false_drops_hold_state() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 60);
    let (actions, stamps, states) = condition_tick(
        Some("30m"),
        Some(prior),
        Some(condition_state(Some(1800), prior, false)),
        Some(false),
        false,
        &now,
    );
    assert!(states.is_empty());
    assert!(actions.is_empty());
    assert_eq!(stamps, one(prior));
}

#[test]
fn condition_fired_period_does_not_refire() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 60);
    let state = condition_state(None, prior, true);
    let (actions, stamps, states) = condition_tick(
        None,
        Some(prior),
        Some(state.clone()),
        Some(true),
        false,
        &now,
    );
    assert_eq!(stamps, one(prior));
    assert!(actions.is_empty());
    assert_eq!(states, one(state));
}

/// A fired one-shot condition named lost fires again only past the grace and on a true
/// reading; a repeating condition named lost never does.
#[test]
fn lost_once_condition_retries_past_the_grace_on_a_true_reading() {
    let now = zdt(2026, 6, 24, 9, 0, 0);
    let since = seconds_before(now.timestamp(), 3600);
    let state = condition_state(None, since, true);
    let lost = BTreeSet::from([NAME.to_owned()]);
    let tick = |once: bool, age: i64, truth: Option<bool>| {
        plan(
            &one(loaded(TaskEntry {
                check: Some("true".into()),
                when: Some(vec!["team.stage=Done".to_owned()]),
                once: once.then_some(true),
                ..TaskEntry::default()
            })),
            &one(seconds_before(now.timestamp(), age)),
            &BTreeMap::new(),
            &now,
            &one(state.clone()),
            &one(Verdict {
                truth,
                readings: BTreeMap::new(),
                unread: Default::default(),
            }),
            &lost,
        )
    };

    let (actions, stamps, states) = tick(true, FIRE_AT_RETRY_SECS, Some(true));
    assert_eq!(
        actions,
        vec![(NAME.to_owned(), Action::Fire)],
        "a lost runner past the grace is dispatched again"
    );
    assert_eq!(stamps, one(now.timestamp()));
    assert_eq!(
        states,
        one(state.clone()),
        "the latch and hold start are kept"
    );

    let young = seconds_before(now.timestamp(), FIRE_AT_RETRY_SECS - 1);
    let (actions, stamps, states) = tick(true, FIRE_AT_RETRY_SECS - 1, Some(true));
    assert!(actions.is_empty(), "a lost runner inside the grace waits");
    assert_eq!((stamps, states), (one(young), one(state.clone())));

    let old = seconds_before(now.timestamp(), FIRE_AT_RETRY_SECS);
    let (actions, stamps, states) = tick(true, FIRE_AT_RETRY_SECS, Some(false));
    assert!(
        actions.is_empty() && states.is_empty(),
        "false drops the latch"
    );
    assert_eq!(stamps, one(old));
    let (actions, _, states) = tick(true, FIRE_AT_RETRY_SECS, None);
    assert!(actions.is_empty(), "unknown holds");
    assert_eq!(states, one(state.clone()));
    let (actions, _, states) = tick(false, FIRE_AT_RETRY_SECS, Some(true));
    assert!(actions.is_empty(), "a repeating condition is not retried");
    assert_eq!(states, one(state));
}

#[test]
fn condition_false_then_true_rearms() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 60);
    let (_, _, states) = condition_tick(
        None,
        Some(prior),
        Some(condition_state(None, prior, true)),
        Some(false),
        false,
        &now,
    );
    assert!(states.is_empty());
    let (actions, _, states) = condition_tick(None, Some(prior), None, Some(true), false, &now);
    assert_eq!(actions, vec![(NAME.to_owned(), Action::Fire)]);
    assert!(states[NAME].fired);
}

#[test]
fn condition_unknown_of_any_cause_holds_the_stamp_and_clock_and_false_drops_them() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 60);
    let state = condition_state(None, seconds_before(now.timestamp(), 120), false);
    let tasks = one(loaded(TaskEntry {
        check: Some("true".into()),
        when: Some(vec!["team.stage=Done".to_owned()]),
        ..TaskEntry::default()
    }));
    let tick = |truth: Option<bool>, unread: &[(&str, &str)]| {
        plan(
            &tasks,
            &one(prior),
            &BTreeMap::new(),
            &now,
            &one(state.clone()),
            &one(Verdict {
                truth,
                readings: BTreeMap::new(),
                unread: unread
                    .iter()
                    .map(|(key, reason)| ((*key).to_owned(), (*reason).to_owned()))
                    .collect(),
            }),
            &BTreeSet::new(),
        )
    };
    // A forge key held unread and a stage or window that reads nothing are both unknown.
    for unread in [
        &[("ci", "probed before the last fire or first sight")][..],
        &[],
    ] {
        let (actions, stamps, states) = tick(None, unread);
        assert!(actions.is_empty(), "{unread:?}");
        assert_eq!(stamps, one(prior), "{unread:?}");
        assert_eq!(states, one(state.clone()), "{unread:?}");
    }
    let (actions, stamps, states) = tick(Some(false), &[]);
    assert!(actions.is_empty());
    assert_eq!(stamps, one(prior));
    assert!(states.is_empty(), "false drops the clock");
    // Kleene truth decides: an unread key under a true disjunction does not veto the fire.
    let (actions, stamps, states) = tick(
        Some(true),
        &[("ci", "probed before the last fire or first sight")],
    );
    assert_eq!(actions, vec![(NAME.to_owned(), Action::Fire)]);
    assert_eq!(stamps, one(now.timestamp()));
    assert_eq!(states[NAME].since, state.since);
    assert!(states[NAME].fired);
}

#[test]
fn forge_conditions_hold_until_a_successful_probe_at_or_after_the_fire_stamp() {
    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let stamp = seconds_before(now.timestamp(), 60);
    let entry = TaskEntry {
        root: root.path().to_owned(),
        check: Some("true".into()),
        when: Some(vec!["!ci=failed".to_owned()]),
        ..TaskEntry::default()
    };
    let scope = entry.run_dir().to_string_lossy().into_owned();
    let held = WhenState {
        fingerprint: Some(("!ci=failed".into(), None, entry.run_dir())),
        since: seconds_before(now.timestamp(), 120),
        fired: false,
    };
    let tasks = one(loaded(entry));
    let probe = |at: Timestamp, ok: bool| {
        write_temp_then_rename_cache(
            &runtime.pr_state_path(),
            &serde_json::json!({
                "path_repos": {&scope: "repo"},
                "repos": {"repo": {"ok": ok, "refreshed_at_ms": at.as_millisecond()}},
                "branch_ci": {&scope: "passing"},
            }),
        )
        .unwrap();
        Some(CiSource::read(&runtime))
    };
    let tick = |source: Option<CiSource>| {
        write_temp_then_rename_cache(&state_path(&runtime), &one(stamp)).unwrap();
        write_temp_then_rename_cache(&when_state_path(&runtime), &one(held.clone())).unwrap();
        fire_tasks(
            &runtime,
            Some(root.path()),
            tasks.clone(),
            &now,
            LoopRunHost::Detached,
            source.as_ref(),
        )
    };
    for (label, source) in [
        ("no source", None),
        (
            "probed before the stamp",
            probe(seconds_before(stamp, 1), true),
        ),
        ("latest probe failed", probe(now.timestamp(), false)),
    ] {
        assert!(tick(source).is_empty(), "{label}");
        assert_eq!(last_stamps(&runtime), one(stamp), "{label}");
        assert_eq!(last_when_states(&runtime), one(held.clone()), "{label}");
    }
    assert_eq!(tick(probe(stamp, true)), vec![NAME]);
    let states = last_when_states(&runtime);
    assert_eq!(states[NAME].since, held.since);
    assert!(states[NAME].fired);
}

#[test]
fn worktree_inspection_holds_a_forge_condition_probed_before_its_fire_stamp() {
    let temp = tempfile::tempdir().unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "--allow-empty", "-qm", "initial"]);
    let checkout = temp.path().join("lane");
    git(
        &repo,
        &["worktree", "add", "-qb", "lane", checkout.to_str().unwrap()],
    );
    // Discovery reports git's resolved path, which keys the probe and the fire stamp.
    let checkout = checkout.canonicalize().unwrap();
    let marker = crate::worktree::WorktreeMarker {
        version: 4,
        name: "lane".into(),
        branch: "lane".into(),
        base_branch: Some("main".into()),
        from_pr: None,
        base_ref: "main".into(),
        repo_root: repo.clone(),
        worktree_path: checkout.clone(),
        created_at: Timestamp::now(),
        loop_task: None,
    };
    let git_dir = PathBuf::from(git(&checkout, &["rev-parse", "--absolute-git-dir"]));
    std::fs::write(
        git_dir.join("rimz-worktree.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(&repo), &repo).unwrap();
    runtime.ensure_dirs().unwrap();
    let now = zdt(2026, 6, 24, 8, 5, 0).timestamp();
    let stamp = seconds_before(now, 60);
    let task = loaded(TaskEntry {
        root: repo.clone(),
        check: Some("true".into()),
        when: Some(vec!["ci=none".to_owned()]),
        each_worktree: true,
        ..TaskEntry::default()
    });
    write_temp_then_rename_cache(
        &state_path(&runtime),
        &BTreeMap::from([(scope_key(NAME, &checkout), stamp)]),
    )
    .unwrap();
    let lane = checkout.to_string_lossy().into_owned();
    let inspect = |probed: Timestamp| {
        write_temp_then_rename_cache(
            &runtime.pr_state_path(),
            &serde_json::json!({
                "path_repos": {&lane: "repo"},
                "repos": {"repo": {"ok": true, "refreshed_at_ms": probed.as_millisecond()}},
                "branch_ci": {},
            }),
        )
        .unwrap();
        let rows = super::super::fanout::inspect(
            NAME,
            &task,
            &runtime,
            &Default::default(),
            Some(&CiSource::read(&runtime)),
            now,
        )
        .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        rows.into_iter().next().unwrap().state
    };
    // A fresh probe that found no CI reads `none`; a stale one leaves `ci` unread, so unknown.
    let super::super::fanout::CheckoutState::Unknown { verdict } =
        inspect(seconds_before(stamp, 1))
    else {
        panic!("a probe before the fire stamp leaves the checkout unknown");
    };
    assert_eq!(verdict.truth, None);
    assert_eq!(
        verdict.unread,
        BTreeMap::from([(
            "ci".into(),
            "probed before the last fire or first sight".into()
        )])
    );
    assert!(matches!(
        inspect(stamp),
        super::super::fanout::CheckoutState::Ready { .. }
    ));
}

#[test]
fn a_task_fired_on_a_true_reading_skips_the_commit_fence_git_reads() {
    let root = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(root.path()), root.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let stamp = seconds_before(now.timestamp(), 60);
    let entry = TaskEntry {
        root: root.path().to_owned(),
        check: Some("true".into()),
        when: Some(vec!["!ci=failed".to_owned()]),
        ..TaskEntry::default()
    };
    let scope = entry.run_dir().to_string_lossy().into_owned();
    let tasks = one(loaded(entry));
    // The CI verdict describes another commit than the PR head, so the fence holds `ci`.
    write_temp_then_rename_cache(
        &runtime.pr_state_path(),
        &serde_json::json!({
            "path_repos": {&scope: "repo"},
            "repos": {"repo": {"ok": true, "refreshed_at_ms": now.timestamp().as_millisecond()}},
            "branch_ci": {&scope: "passing"},
            "states": {&scope: {
                "state": "open", "ci": "passing", "ci_sha": "aaaaaaa1",
                "open": {"head": "bbbbbbb2"},
            }},
        }),
    )
    .unwrap();
    let source = CiSource::read(&runtime);
    let spawns = |tasks: &BTreeMap<String, LoadedTask>, stamp: Timestamp, fired: bool| {
        let state = WhenState {
            fingerprint: Some((
                tasks[NAME].entry().when.as_ref().unwrap()[0].clone(),
                None,
                tasks[NAME].entry().run_dir(),
            )),
            since: seconds_before(stamp, 60),
            fired,
        };
        write_temp_then_rename_cache(&state_path(&runtime), &one(stamp)).unwrap();
        write_temp_then_rename_cache(&when_state_path(&runtime), &one(state.clone())).unwrap();
        let before = crate::proc::testkit::spawn_count();
        let fires = fire_tasks(
            &runtime,
            Some(root.path()),
            tasks.clone(),
            &now,
            LoopRunHost::Detached,
            Some(&source),
        );
        let spawned = crate::proc::testkit::spawn_count() - before;
        assert!(fires.is_empty(), "fired={fired}");
        assert_eq!(last_when_states(&runtime), one(state), "fired={fired}");
        spawned
    };
    assert_eq!(
        spawns(&tasks, stamp, true),
        0,
        "a fired task runs no fence git read"
    );
    assert!(
        spawns(&tasks, stamp, false) > 0,
        "before its first fire the fence still reads git"
    );

    // A fired one-shot whose runner is lost retries as a fire, so the fence holds it.
    let once = loaded(TaskEntry {
        root: root.path().to_owned(),
        check: Some("true".into()),
        when: Some(vec!["ci=passed".to_owned()]),
        once: Some(true),
        ..TaskEntry::default()
    });
    let lost = seconds_before(now.timestamp(), FIRE_AT_RETRY_SECS);
    let _ = run_log::record_transition(
        &once,
        &LoopRunRecord::new(NAME, LoopRunResult::StartFailed, LoopRunMode::Scheduled, 0),
    );
    assert!(
        spawns(&one(once), lost, true) > 0,
        "a lost runner's retry reads the fence and holds on its unread `ci`"
    );
}

#[test]
fn condition_not_live_carries_both_maps() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 60);
    let state = condition_state(None, prior, false);
    let (actions, stamps, states) = condition_tick(
        None,
        Some(prior),
        Some(state.clone()),
        Some(false),
        true,
        &now,
    );
    assert_eq!(stamps, one(prior));
    assert_eq!(states, one(state));
    assert!(actions.is_empty());
}

#[test]
fn workflow_tick_advances_leave_the_timer_service_like_loop_runs() {
    let exe = Path::new("/opt/rimz");
    let id = crate::ids::WorkflowRunId::new();
    let advance = crate::harness::workflow::advance_args(Path::new("/repo"), Some(&id), None, None);
    assert_eq!(
        workflow_advance_command(LoopRunHost::Detached, exe, Path::new("/repo"), &id),
        (exe.as_os_str().to_owned(), advance.clone()),
    );
    let (program, args) =
        workflow_advance_command(LoopRunHost::TransientScope, exe, Path::new("/repo"), &id);
    assert_eq!(program, "systemd-run");
    assert_eq!(&args[..2], ["--user", "--scope"].map(OsString::from));
    assert!(args.ends_with(&advance), "{args:?}");
}

#[test]
fn encoded_signal_carries_its_firing_time_and_unchanged_payload() {
    let signal: Signal = serde_json::from_value(serde_json::json!({
        "name": "build.moved", "source": "cli", "payload": {"from": "a", "to": "b"}
    }))
    .unwrap();
    let fired_at = Timestamp::from_second(1).unwrap();
    let encoded: serde_json::Value =
        serde_json::from_str(&encode_signal(Some((&signal, fired_at))).unwrap().unwrap()).unwrap();
    assert_eq!(encoded["arrived_at"], serde_json::json!(fired_at));
    let decoded: Signal = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.name, signal.name);
    assert_eq!(decoded.payload, signal.payload);
    assert!(encode_signal(None).unwrap().is_none());
}

#[test]
fn loop_run_hosts_render_precise_argv() {
    let exe = Path::new("/opt/RimZ build/rimz");
    let args = loop_run_args(
        Some(Path::new("/workspace with spaces")),
        NAME,
        Some(r#"{"name":"deploy.done"}"#),
        None,
        None,
    );
    assert_eq!(
        loop_run_command(LoopRunHost::Detached, exe, &args, NAME),
        (exe.as_os_str().to_owned(), args.clone()),
    );
    assert_eq!(
        loop_run_command(LoopRunHost::IsolatedProcessGroup, exe, &args, NAME),
        (exe.as_os_str().to_owned(), args.clone()),
    );
    assert_eq!(
        loop_run_command(LoopRunHost::TransientScope, exe, &args, NAME),
        (
            OsString::from("systemd-run"),
            [
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--description",
                "RimZ loop run task",
                "--",
                "/opt/RimZ build/rimz",
                "--root",
                "/workspace with spaces",
                "loop",
                "run",
                "task",
                "--signal-json",
                r#"{"name":"deploy.done"}"#,
            ]
            .map(OsString::from)
            .to_vec(),
        ),
    );
}

#[test]
fn scope_handoff_requires_observed_distinct_cgroups() {
    for (parent, child) in [
        (
            b"0::/user.slice/loop.service\n".as_slice(),
            b"0::/user.slice/run-test.scope\n".as_slice(),
        ),
        (
            b"1:name=systemd:/user.slice/loop.service\n".as_slice(),
            b"1:name=systemd:/user.slice/run-test.scope\n".as_slice(),
        ),
    ] {
        assert!(cgroup_changed(Some(parent), Some(child)));
        assert!(!cgroup_changed(Some(parent), Some(parent)));
        assert!(!cgroup_changed(Some(parent), None));
        assert!(!cgroup_changed(None, Some(child)));
        assert!(!cgroup_changed(Some(parent), Some(b"")));
        assert!(!cgroup_changed(Some(b""), Some(child)));
    }
    assert!(!cgroup_changed(None, None));
}

#[test]
fn scope_handoff_distinguishes_exit_from_pending_migration() {
    let parent = Some(b"0::/user.slice/loop.service\n".as_slice());
    let scoped = Some(b"0::/user.slice/run-test.scope\n".as_slice());
    for child in [parent, scoped, None, Some(b"".as_slice())] {
        assert_eq!(scope_handoff(parent, child, false), ScopeHandoff::ChildGone);
    }
    for child in [parent, None, Some(b"".as_slice())] {
        assert_eq!(scope_handoff(parent, child, true), ScopeHandoff::Pending);
    }
    assert_eq!(scope_handoff(parent, scoped, true), ScopeHandoff::HandedOff);
    assert_eq!(scope_handoff(None, scoped, true), ScopeHandoff::Pending);
    assert_eq!(scope_handoff(None, None, false), ScopeHandoff::ChildGone);
}

#[test]
fn all_loop_run_hosts_suppress_subprocesses_in_unit_tests() {
    let root = Path::new("/missing-loop-test-root");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(root), root).unwrap();
    for host in [
        LoopRunHost::Detached,
        LoopRunHost::IsolatedProcessGroup,
        LoopRunHost::TransientScope,
    ] {
        assert_eq!(
            spawn_loop_run(
                &runtime,
                &task("/missing-loop-test-root", "1m"),
                Some(root),
                NAME,
                None,
                None,
                None,
                host
            ),
            LaunchOutcome::Started
        );
    }
}

fn one<T>(value: T) -> BTreeMap<String, T> {
    BTreeMap::from([(NAME.to_owned(), value)])
}

#[test]
fn backlog_backstop_paces_failed_spawns_and_does_not_silence_other_tasks() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    let at = Timestamp::from_second(100).unwrap();
    let trigger = serde_json::json!({"triggers":[{"arrived_at":at,"signal":null,"absorbed":0}]});
    crate::disk::atomic::write_temp_then_rename(
        &super::super::backlog::path(&paths),
        &serde_json::json!({"version":1,"tasks":{"first":trigger,"second":trigger}}),
    )
    .unwrap();
    let mut calls = Vec::new();
    for seconds in [100, 159, 160] {
        tick_backlog_with(
            &runtime,
            root.path(),
            Timestamp::from_second(seconds).unwrap(),
            |name| {
                calls.push(name.to_owned());
                if name == "first" {
                    Err(std::io::Error::other("spawn failed"))
                } else {
                    Ok(())
                }
            },
        );
    }
    assert_eq!(calls, ["first", "second", "first", "second"]);
}

#[test]
fn backlog_backstop_read_failure_without_prior_state_is_also_paced() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    crate::disk::atomic::write_bytes_atomically(&super::super::backlog::path(&paths), b"invalid")
        .unwrap();
    let mut calls = 0;
    tick_backlog_with(
        &runtime,
        root.path(),
        Timestamp::from_second(100).unwrap(),
        |_| {
            calls += 1;
            Ok(())
        },
    );
    let at = Timestamp::from_second(100).unwrap();
    crate::disk::atomic::write_temp_then_rename(&super::super::backlog::path(&paths), &serde_json::json!({"version":1,"tasks":{"busy":{"triggers":[{"arrived_at":at,"signal":null,"absorbed":0}]}}})).unwrap();
    tick_backlog_with(
        &runtime,
        root.path(),
        Timestamp::from_second(159).unwrap(),
        |_| {
            calls += 1;
            Ok(())
        },
    );
    assert_eq!(calls, 0);
    tick_backlog_with(
        &runtime,
        root.path(),
        Timestamp::from_second(160).unwrap(),
        |_| {
            calls += 1;
            Ok(())
        },
    );
    assert_eq!(
        calls, 1,
        "retry a failed room read after the pace, not on every tick"
    );
}

fn loaded(entry: TaskEntry) -> LoadedTask {
    LoadedTask::new(NAME, entry, TaskSource::Config)
}

fn loaded_from(entry: TaskEntry, source: TaskSource) -> LoadedTask {
    LoadedTask::new(NAME, entry, source)
}

fn task(root: &str, every: &str) -> LoadedTask {
    loaded(TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("do it".to_owned()),
        root: PathBuf::from(root),
        every: Some(every.to_owned()),
        ..TaskEntry::default()
    })
}

fn until(stamp: Timestamp) -> Arming {
    Arming {
        enabled: true,
        at: Timestamp::from_second(0).ok(),
        pause_until: Some(stamp),
        strikes: None,
    }
}

/// Durable inputs for one elder tick over a single task. The default is the
/// never-seen, live case.
#[derive(Default)]
struct Tick {
    state: Option<Timestamp>,
    arming: Option<Arming>,
    lost: bool,
}

impl Tick {
    /// A task the elder has already stamped once.
    fn armed(state: Timestamp) -> Self {
        Self {
            state: Some(state),
            ..Self::default()
        }
    }

    fn held(self, arming: Arming) -> Self {
        Self {
            arming: Some(arming),
            ..self
        }
    }

    /// A task whose dispatched runner holds no run lock and recorded nothing.
    fn lost(self) -> Self {
        Self { lost: true, ..self }
    }

    /// The task's action this tick and the stamp carried into the next one.
    fn run(self, task: &LoadedTask, now: &Zoned) -> (Option<Action>, Option<Timestamp>) {
        let (actions, next, _) = plan(
            &one(task.clone()),
            &self.state.map(one).unwrap_or_default(),
            &self
                .arming
                .map(|arming| BTreeMap::from([(task.key(NAME), arming)]))
                .unwrap_or_default(),
            now,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &if self.lost {
                BTreeSet::from([NAME.to_owned()])
            } else {
                BTreeSet::new()
            },
        );
        (
            actions.first().map(|(_, action)| *action),
            next.get(NAME).copied(),
        )
    }
}

fn arm(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
    (Some(Action::Arm), Some(stamp))
}

fn fire(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
    (Some(Action::Fire), Some(stamp))
}

fn watch_lost(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
    (Some(Action::WatchLost), Some(stamp))
}

fn carry(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
    (None, Some(stamp))
}

/// Arm on first sight, fire once due, otherwise carry the prior stamp. A row
/// the elder cannot schedule leaves no stamp at all.
#[test]
fn plan_arms_fires_and_carries_each_task_state() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let stamp = now.timestamp();
    let due = seconds_before(stamp, 300);
    let early = seconds_before(stamp, 240);
    let prior = seconds_before(stamp, 600);
    let manual = Arming {
        enabled: false,
        at: Some(stamp),
        pause_until: None,
        strikes: Some(3),
    };
    let every_5m = &task("/repo", "5m");
    // `every` and `at` together are a time conflict, so this row never parses.
    let malformed = &loaded(TaskEntry {
        agent: Some("claude".to_owned()),
        every: Some("5m".to_owned()),
        at: Some("07:00".to_owned()),
        ..TaskEntry::default()
    });

    let run = |tick: Tick, task| tick.run(task, &now);
    assert_eq!(run(Tick::default(), every_5m), arm(stamp), "first sight");
    assert_eq!(run(Tick::armed(due), every_5m), fire(stamp), "due");
    assert_eq!(
        run(Tick::armed(early), every_5m),
        carry(early),
        "not yet due"
    );
    assert_eq!(
        run(Tick::default().held(manual), every_5m),
        arm(stamp),
        "armed while paused"
    );
    assert_eq!(
        run(Tick::armed(prior).held(manual), every_5m),
        carry(prior),
        "held by a pause"
    );
    assert_eq!(
        run(Tick::armed(due), malformed),
        (None, None),
        "malformed is skipped"
    );

    // State for a task the catalog no longer holds is dropped, not carried.
    let (actions, next, _) = plan(
        &BTreeMap::new(),
        &one(due),
        &BTreeMap::new(),
        &now,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &BTreeSet::new(),
    );
    assert!(actions.is_empty());
    assert!(next.is_empty());
}

#[test]
fn signal_tasks_only_arm_while_missing_watchers_fire_after_the_grace() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let signal = loaded(TaskEntry {
        agent: Some("claude".to_owned()),
        signal: Some("ci.failed".to_owned()),
        ..TaskEntry::default()
    });
    assert_eq!(Tick::default().run(&signal, &now), arm(now.timestamp()));
    let prior = seconds_before(now.timestamp(), 300);
    assert_eq!(Tick::armed(prior).run(&signal, &now), carry(prior));

    let root = tempfile::tempdir().unwrap();
    let watch = loaded(TaskEntry {
        agent: Some("claude".to_owned()),
        root: root.path().to_path_buf(),
        watch: Some(crate::config::WatchSpec::Command("cargo test".to_owned())),
        ..TaskEntry::default()
    });
    assert_eq!(Tick::default().run(&watch, &now), arm(now.timestamp()));
    let recent = seconds_before(now.timestamp(), WATCH_LOST_GRACE_SECS);
    assert_eq!(Tick::armed(recent).run(&watch, &now), carry(recent));

    let stale = seconds_before(now.timestamp(), WATCH_LOST_GRACE_SECS + 1);
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).expect("watch runtime");
    std::fs::create_dir_all(&runtime.locks_dir).expect("runtime root");
    let guard = super::super::signal::acquire_watch_lock(&runtime, NAME)
        .unwrap()
        .expect("watch lock");
    assert_eq!(Tick::armed(stale).run(&watch, &now), carry(stale));
    drop(guard);
    assert_eq!(
        Tick::armed(stale).run(&watch, &now),
        watch_lost(now.timestamp())
    );
    let outcome = lost_watch_outcome(&watch, NAME, stale, now.timestamp()).unwrap();
    assert!(outcome.verdict.elapsed_ms() >= WATCH_LOST_GRACE_SECS as u64 * 1_000);
    assert!(outcome.output.is_empty());
    assert_eq!(
        outcome.summary,
        crate::disk::summary::FileSummary::default()
    );
    assert_eq!(
        outcome.output_path,
        Some(super::super::signal::wait_output_path(
            &paths,
            NAME,
            watch.entry()
        ))
    );
    let watch = loaded(TaskEntry {
        wait_meta: Some(crate::config::WaitMeta {
            armed_at: prior,
            delay: None,
            reader: Some("armer".to_owned()),
        }),
        ..watch.entry().clone()
    });
    let path = paths
        .out_reader_dir(Some("armer"))
        .join(format!("{NAME}.output"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "watcher failed before launching command").unwrap();
    let outcome = lost_watch_outcome(&watch, NAME, stale, now.timestamp()).unwrap();
    assert_eq!(
        outcome.output_path.as_deref(),
        Some(path.as_path()),
        "the lost-watcher outcome names the file the armer's watcher wrote"
    );
    assert_eq!(outcome.verdict.elapsed_ms(), 300_000);
    assert_eq!(outcome.output, "watcher failed before launching command");
    assert_eq!(outcome.summary.bytes, 39);
    assert_eq!(outcome.summary.lines, 1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn invalid_renamed_watcher_signal_does_not_stop_elder_fire() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).expect("watch runtime");
    std::fs::create_dir_all(&runtime.locks_dir).expect("runtime root");
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let stale = seconds_before(now.timestamp(), WATCH_LOST_GRACE_SECS + 1);
    write_temp_then_rename_cache(
        &state_path(&runtime),
        &BTreeMap::from([("Renamed".to_owned(), stale), ("valid".to_owned(), stale)]),
    )
    .expect("fire state");
    let watch = |name| {
        LoadedTask::new(
            name,
            TaskEntry {
                agent: Some("claude".to_owned()),
                root: root.path().to_path_buf(),
                watch: Some(crate::config::WatchSpec::Command("cargo test".to_owned())),
                ..TaskEntry::default()
            },
            TaskSource::Config,
        )
    };
    let tasks = BTreeMap::from([
        ("Renamed".to_owned(), watch("Renamed")),
        ("valid".to_owned(), watch("valid")),
    ]);

    assert_eq!(
        fire_tasks(
            &runtime,
            Some(root.path()),
            tasks,
            &now,
            LoopRunHost::Detached,
            None,
        ),
        vec!["valid"]
    );
}

/// An ended pause becomes the effective last-fire edge, so a lifted task
/// waits out a full interval and never replays an occurrence it slept through.
#[test]
fn ended_pause_sets_the_effective_fire_edge() {
    let now = zdt(2026, 6, 24, 8, 0, 0);
    let interval_task = &task("/repo", "5m");
    let pause_end = seconds_before(now.timestamp(), 240);
    let stale_stamp = seconds_before(pause_end, 600);
    let resumed = || Tick::armed(stale_stamp).held(until(pause_end));

    assert_eq!(
        resumed().run(interval_task, &now),
        carry(stale_stamp),
        "the interval restarts from the pause end, not the stale stamp"
    );
    let interval_due = zdt(2026, 6, 24, 8, 1, 0);
    assert_eq!(
        resumed().run(interval_task, &interval_due),
        fire(interval_due.timestamp()),
        "a full interval past the pause end fires"
    );

    let daily = &loaded(TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("do it".to_owned()),
        root: PathBuf::from("/repo"),
        every: Some("day".to_owned()),
        at: Some("07:00".to_owned()),
        ..TaskEntry::default()
    });
    let slept_through = zdt(2026, 6, 23, 6, 0, 0).timestamp();
    let woke_at_0730 =
        || Tick::armed(slept_through).held(until(zdt(2026, 6, 24, 7, 30, 0).timestamp()));
    assert_eq!(
        woke_at_0730().run(daily, &zdt(2026, 6, 24, 8, 0, 0)),
        carry(slept_through),
        "the 07:00 occurrence crossed while paused is not replayed"
    );
    assert_eq!(
        woke_at_0730().run(daily, &zdt(2026, 6, 25, 7, 0, 0)).0,
        Some(Action::Fire),
        "the next day's occurrence fires normally"
    );
}

#[test]
fn fire_at_fires_once_at_or_after_its_instant_through_a_pause() {
    let at = zdt(2026, 6, 24, 8, 0, 0);
    let instant = &loaded(TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("do it".to_owned()),
        root: PathBuf::from("/repo"),
        fire_at: Some(at.timestamp()),
        ..TaskEntry::default()
    });
    let early = zdt(2026, 6, 24, 7, 0, 0);
    let late = zdt(2026, 6, 24, 8, 1, 0);
    assert_eq!(
        Tick::default().run(instant, &late),
        fire(late.timestamp()),
        "first sight past the instant fires"
    );
    let days_late = zdt(2026, 6, 27, 8, 0, 0);
    assert_eq!(
        Tick::default().run(instant, &days_late),
        fire(days_late.timestamp()),
        "first sight three days past the instant fires"
    );
    assert_eq!(
        Tick::default().run(instant, &early),
        arm(early.timestamp()),
        "first sight before the instant arms"
    );
    let armed = || Tick::armed(early.timestamp());
    assert_eq!(
        armed().run(instant, &zdt(2026, 6, 24, 7, 59, 59)),
        carry(early.timestamp()),
        "not yet"
    );
    assert_eq!(
        armed().run(instant, &at),
        fire(at.timestamp()),
        "fires at the instant"
    );
    let stamped = || Tick::armed(at.timestamp());
    let grace = zdt(2026, 6, 24, 8, 5, 0);
    let in_grace = zdt(2026, 6, 24, 8, 4, 59);
    assert_eq!(
        stamped().run(instant, &grace),
        carry(at.timestamp()),
        "not again while its runner is live"
    );
    assert_eq!(
        stamped().lost().run(instant, &in_grace),
        carry(at.timestamp()),
        "not again within the grace, even with no runner"
    );
    assert_eq!(
        stamped().lost().run(instant, &grace),
        fire(grace.timestamp()),
        "a lost runner past the grace fires again and stamps now"
    );
    assert_eq!(
        armed().lost().run(instant, &zdt(2026, 6, 24, 7, 59, 59)),
        carry(early.timestamp()),
        "a stamp before the instant is armed, not a lost runner"
    );
    let hourly = &task("/repo", "1h");
    let half_hour_ago = seconds_before(grace.timestamp(), 1_800);
    assert_eq!(
        Tick::armed(half_hour_ago).lost().run(hourly, &grace),
        carry(half_hour_ago),
        "a periodic row waits for its next occurrence"
    );

    let pause_end = zdt(2026, 6, 24, 9, 0, 0);
    let paused = || armed().held(until(pause_end.timestamp()));
    assert_eq!(
        paused().run(instant, &late),
        carry(early.timestamp()),
        "held while paused"
    );
    let woke = zdt(2026, 6, 24, 9, 1, 0);
    assert_eq!(
        paused().run(instant, &woke),
        fire(woke.timestamp()),
        "paused through the instant fires at the pause end"
    );
    let stranded_paused = || stamped().lost().held(until(pause_end.timestamp()));
    assert_eq!(
        stranded_paused().run(instant, &zdt(2026, 6, 24, 8, 30, 0)),
        carry(at.timestamp()),
        "a lost runner past the grace is carried while paused"
    );
    assert_eq!(
        stranded_paused().run(instant, &woke),
        fire(woke.timestamp()),
        "and fires at the first live tick after the pause"
    );
    assert_eq!(
        Tick::default()
            .held(until(pause_end.timestamp()))
            .run(instant, &late),
        (None, None),
        "first sight past the instant while paused waits unstamped"
    );
}

/// The pass reads a stamped `fire-at` row's runner past the grace: a held run
/// lock or a recorded attempt keeps it carried, and only a `start failed` row
/// since the stamp leaves it lost.
#[test]
fn stranded_fire_at_retries_only_when_its_runner_is_lost() {
    const STRANDED: &str = "stranded-fire-at";
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    std::fs::create_dir_all(&runtime.locks_dir).unwrap();
    let now = zdt(2026, 6, 24, 9, 0, 0);
    let stamp = seconds_before(now.timestamp(), FIRE_AT_RETRY_SECS);
    let task = LoadedTask::new(
        STRANDED,
        TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            root: root.path().to_path_buf(),
            fire_at: Some(seconds_before(stamp, 60)),
            ..TaskEntry::default()
        },
        TaskSource::Config,
    );
    let tasks = BTreeMap::from([(STRANDED.to_owned(), task.clone())]);
    let pass = || {
        write_temp_then_rename_cache(
            &state_path(&runtime),
            &BTreeMap::from([(STRANDED.to_owned(), stamp)]),
        )
        .unwrap();
        let fired = fire_tasks(
            &runtime,
            Some(root.path()),
            tasks.clone(),
            &now,
            LoopRunHost::Detached,
            None,
        );
        (fired, last_stamps(&runtime)[STRANDED])
    };
    let record = |result| {
        let _ = run_log::record_transition(
            &task,
            &LoopRunRecord::new(STRANDED, result, LoopRunMode::Scheduled, 0),
        );
    };

    let lock = runtime.lock_path(super::super::run_lock::file_name(STRANDED, None));
    let crate::disk::lease::LeaseAttempt::Acquired(held) =
        crate::disk::lease::claim(&lock, |_| Ok(())).unwrap()
    else {
        panic!("the test's run lock is free");
    };
    assert_eq!(pass(), (vec![], stamp), "a held run lock is a live runner");
    drop(held);

    record(LoopRunResult::StartFailed);
    assert_eq!(
        pass(),
        (vec![STRANDED.to_owned()], now.timestamp()),
        "a start failure since the stamp leaves the runner lost"
    );

    record(LoopRunResult::Delivered);
    assert_eq!(
        pass(),
        (vec![], stamp),
        "a recorded attempt since the stamp is not retried"
    );
}

/// A fired one-shot condition past the grace, still reading true, takes the
/// `fire-at` row's lost-runner test: a held run lock or a recorded attempt keeps
/// it carried, and a `start failed` row since the stamp fires it again with its
/// latch and hold start kept.
#[test]
fn stranded_once_condition_retries_only_when_its_runner_is_lost() {
    const STRANDED: &str = "stranded-once-condition";
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    std::fs::create_dir_all(&runtime.locks_dir).unwrap();
    std::fs::create_dir_all(root.path().join(".rimz/team")).unwrap();
    std::fs::write(
        root.path().join(".rimz/team/blackboard.md"),
        "Stage: Done\n",
    )
    .unwrap();
    let now = zdt(2026, 6, 24, 9, 0, 0);
    let stamp = seconds_before(now.timestamp(), FIRE_AT_RETRY_SECS);
    let entry = TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("do it".to_owned()),
        root: root.path().to_path_buf(),
        when: Some(vec!["team.stage=Done".to_owned()]),
        once: Some(true),
        ..TaskEntry::default()
    };
    let latch = BTreeMap::from([(
        STRANDED.to_owned(),
        WhenState {
            fingerprint: Some(("team.stage=Done".to_owned(), None, entry.run_dir())),
            since: seconds_before(stamp, 60),
            fired: true,
        },
    )]);
    let task = LoadedTask::new(STRANDED, entry, TaskSource::Config);
    let tasks = BTreeMap::from([(STRANDED.to_owned(), task.clone())]);
    let pass = || {
        write_temp_then_rename_cache(
            &state_path(&runtime),
            &BTreeMap::from([(STRANDED.to_owned(), stamp)]),
        )
        .unwrap();
        write_temp_then_rename_cache(&when_state_path(&runtime), &latch).unwrap();
        let fired = fire_tasks(
            &runtime,
            Some(root.path()),
            tasks.clone(),
            &now,
            LoopRunHost::Detached,
            None,
        );
        assert_eq!(last_when_states(&runtime), latch, "the latch is kept");
        (fired, last_stamps(&runtime)[STRANDED])
    };
    let record = |result| {
        let _ = run_log::record_transition(
            &task,
            &LoopRunRecord::new(STRANDED, result, LoopRunMode::Scheduled, 0),
        );
    };

    let lock = runtime.lock_path(super::super::run_lock::file_name(STRANDED, None));
    let crate::disk::lease::LeaseAttempt::Acquired(held) =
        crate::disk::lease::claim(&lock, |_| Ok(())).unwrap()
    else {
        panic!("the test's run lock is free");
    };
    assert_eq!(pass(), (vec![], stamp), "a held run lock is a live runner");
    drop(held);

    record(LoopRunResult::StartFailed);
    assert_eq!(
        pass(),
        (vec![STRANDED.to_owned()], now.timestamp()),
        "a start failure since the stamp leaves the runner lost"
    );

    record(LoopRunResult::Delivered);
    assert_eq!(
        pass(),
        (vec![], stamp),
        "a recorded attempt since the stamp is not retried"
    );
}

/// A recorded attempt is read from the history once per stamp; a missing one is
/// read again, and a later stamp is a new question.
#[test]
fn a_recorded_attempt_is_remembered_per_stamp() {
    let stamp = zdt(2026, 6, 24, 9, 0, 0).timestamp();
    let key = |stamp| (PathBuf::from("/memo-root"), "memo-task".to_owned(), stamp);
    let scans = &std::cell::Cell::new(0);
    let scan = |found| {
        move || {
            scans.set(scans.get() + 1);
            found
        }
    };

    assert!(!attempt_recorded_since(key(stamp), scan(false)));
    assert!(attempt_recorded_since(key(stamp), scan(true)));
    assert!(
        attempt_recorded_since(key(stamp), scan(false)),
        "a recorded attempt never un-records, even when the history drops it"
    );
    assert_eq!(scans.get(), 2, "the remembered verdict reads no history");
    let restamped = zdt(2026, 6, 24, 9, 5, 0).timestamp();
    assert!(!attempt_recorded_since(key(restamped), scan(false)));
    assert_eq!(scans.get(), 3);
}

/// A resident `fire-at` row whose launch is ledgered is left out of the pass,
/// stamp and all, and fires on its first live sight once the ledger clears.
#[test]
fn ledgered_resident_fire_at_waits_unstamped_and_fires_once_cleared() {
    let root = tempfile::tempdir().unwrap();
    let paths = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&paths).unwrap();
    let now = zdt(2026, 6, 24, 9, 0, 0);
    let stamp = seconds_before(now.timestamp(), 600);
    let tasks = one(loaded(TaskEntry {
        agent: Some("claude".to_owned()),
        prompt: Some("do it".to_owned()),
        root: root.path().to_path_buf(),
        stay: true,
        fire_at: Some(seconds_before(stamp, 60)),
        ..TaskEntry::default()
    }));
    let ledger = super::super::launch_ledger::resident_path(&paths);
    crate::disk::atomic::write_temp_then_rename(
        &ledger,
        &one(BTreeMap::from([(
            root.path().to_owned(),
            serde_json::json!({"at": stamp, "leader": "otter"}),
        )])),
    )
    .unwrap();
    write_temp_then_rename_cache(&state_path(&runtime), &one(stamp)).unwrap();
    let pass = || {
        fire_tasks(
            &runtime,
            Some(root.path()),
            tasks.clone(),
            &now,
            LoopRunHost::Detached,
            None,
        )
    };

    assert!(
        pass().is_empty(),
        "a launched resident row is not dispatched"
    );
    assert!(
        !last_stamps(&runtime).contains_key(NAME),
        "and its stamp is dropped"
    );
    std::fs::remove_file(&ledger).unwrap();
    assert_eq!(
        pass(),
        vec![NAME],
        "it fires on first live sight once cleared"
    );
    assert_eq!(last_stamps(&runtime)[NAME], now.timestamp());
}

#[test]
fn project_task_stays_held_until_enable_and_does_not_replay() {
    let now = zdt(2026, 6, 24, 8, 5, 0);
    let prior = seconds_before(now.timestamp(), 600);
    let task = loaded_from(
        TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            root: PathBuf::from("/repo"),
            every: Some("5m".to_owned()),
            ..TaskEntry::default()
        },
        TaskSource::Project {
            state: crate::trust::TrustState::Trusted,
        },
    );

    assert_eq!(
        Tick::default().run(&task, &now),
        arm(now.timestamp()),
        "an unstamped disabled task still gets its first-sight stamp"
    );
    assert_eq!(
        Tick::armed(prior).run(&task, &now),
        carry(prior),
        "a project task without a local enable stays held"
    );
    let enabled = Arming {
        enabled: true,
        at: Some(now.timestamp()),
        pause_until: None,
        strikes: None,
    };
    assert_eq!(
        Tick::armed(prior).held(enabled).run(&task, &now),
        carry(prior),
        "enabling establishes a fresh replay edge"
    );
    let due = zdt(2026, 6, 24, 8, 10, 0);
    assert_eq!(
        Tick::armed(prior).held(enabled).run(&task, &due),
        fire(due.timestamp()),
        "the next occurrence fires normally"
    );
}

/// Tasks are filtered by resolved root, so a `~` root reaches its own room.
#[test]
fn workspace_filter_keeps_only_this_rooms_roots() {
    let owned = WorkspaceId::from_project_root(Path::new("/repo/owned"));
    let filtered = workspace_tasks(
        BTreeMap::from([
            ("owned".to_owned(), task("/repo/owned", "5m")),
            ("foreign".to_owned(), task("/repo/foreign", "5m")),
        ]),
        &owned,
    );
    assert_eq!(filtered.keys().cloned().collect::<Vec<_>>(), vec!["owned"]);

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let home = home.canonicalize().unwrap_or(home);
    let filtered = workspace_tasks(
        BTreeMap::from([("home".to_owned(), task("~", "5m"))]),
        &WorkspaceId::from_project_root(&home),
    );
    assert_eq!(filtered.keys().cloned().collect::<Vec<_>>(), vec!["home"]);
}
