use super::super::catalog::TaskSource;
use super::super::tests::{seconds_before, zdt};
use super::*;
use crate::config::TaskEntry;

const NAME: &str = "task";

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
    let scoped = scoped_tasks(&tasks, &BTreeMap::new(), &owned);
    let scope = scoped.values().next().unwrap();
    assert_eq!(
        scope.checkout.as_deref(),
        Some(root.path().join("a").as_path())
    );
}

#[test]
fn worktree_conditions_select_the_checkout_and_skip_ledgered_edges() {
    let (_root, tasks, owned) = worktree_tasks(None);
    let now = zdt(2026, 6, 24, 8, 0, 0);
    std::fs::write(owned[0].join("blackboard.md"), "Stage: Done\n").unwrap();
    std::fs::write(owned[1].join("blackboard.md"), "Stage: Review\n").unwrap();
    let mut ledger = super::super::launch_ledger::Ledger::new();
    for stage in [None, Some("Review"), Some("Done")] {
        if let Some(stage) = stage {
            std::fs::write(owned[0].join("blackboard.md"), format!("Stage: {stage}\n")).unwrap();
        }
        let scoped = scoped_tasks(&tasks, &ledger, &owned);
        assert_eq!(scoped.len(), if ledger.is_empty() { 2 } else { 1 });
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
        );
        let (actions, _, _) = plan(
            &planned,
            &stamps,
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
            &verdicts,
        );
        if ledger.is_empty() {
            assert_eq!(actions.len(), 1);
            assert_eq!(actions[0].1, Action::Fire);
            assert_eq!(scoped[&actions[0].0].checkout.as_ref(), Some(&owned[0]));
            ledger.insert(
                NAME.into(),
                BTreeMap::from([(
                    owned[0].clone(),
                    super::super::launch_ledger::LaunchRecord {
                        at: now.timestamp(),
                        leader: "otter".into(),
                    },
                )]),
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
    let scoped = scoped_tasks(&tasks, &BTreeMap::new(), &owned);
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
                ok: true,
                readings: BTreeMap::new(),
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
    let scoped = scoped_tasks(&tasks, &BTreeMap::new(), &owned);
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
                        ok: minute >= 20 || scope.checkout.as_ref() == Some(&owned[0]),
                        readings: BTreeMap::new(),
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
                when: Some(vec!["!ci=failed".to_owned()]),
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
            &runtime.shared_rate_limits_path(),
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
            &runtime.shared_rate_limits_path(),
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
                    ok: true,
                    readings: BTreeMap::new(),
                }),
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
                    ok,
                    readings: BTreeMap::new(),
                })
            })
            .unwrap_or_default(),
    )
}

#[test]
fn resident_conditions_retry_at_five_minutes_until_ledgered() {
    let root = tempfile::tempdir().unwrap();
    let now = zdt(2026, 6, 24, 8, 0, 0);
    for stay in [true, false] {
        let tasks = one(loaded(TaskEntry {
            root: root.path().to_owned(),
            agent: Some("claude".into()),
            prompt: Some("repair".into()),
            stay,
            when: Some(vec!["team.stage=Done".into()]),
            ..Default::default()
        }));
        let verdicts = one(Verdict {
            ok: true,
            readings: BTreeMap::new(),
        });
        let (_, mut stamps, mut states) = plan(
            &tasks,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &now,
            &BTreeMap::new(),
            &BTreeMap::new(),
        );
        for (minute, second, expected) in [
            (0, 0, true),
            (4, 59, false),
            (5, 0, stay),
            (9, 59, false),
            (10, 0, stay),
        ] {
            let tick = zdt(2026, 6, 24, 8, minute, second);
            let (actions, next_stamps, next_states) =
                plan(&tasks, &stamps, &BTreeMap::new(), &tick, &states, &verdicts);
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
            &super::super::launch_ledger::path(&paths),
            &one(BTreeMap::from([(
                root.path().to_owned(),
                super::super::launch_ledger::LaunchRecord {
                    at: now.timestamp(),
                    leader: "otter".into(),
                },
            )])),
        )
        .unwrap();
        write_temp_then_rename_cache(&state_path(&runtime), &stamps).unwrap();
        write_temp_then_rename_cache(&when_state_path(&runtime), &states).unwrap();
        std::fs::write(root.path().join("blackboard.md"), "Stage: Done\n").unwrap();
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
    assert_eq!(
        Tick::armed(at.timestamp()).run(instant, &late),
        carry(at.timestamp()),
        "never again once stamped"
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
    assert_eq!(
        Tick::default()
            .held(until(pause_end.timestamp()))
            .run(instant, &late),
        (None, None),
        "first sight past the instant while paused waits unstamped"
    );
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
