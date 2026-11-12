//! Room-list behavior at the CLI boundary.

use super::*;
use rimz::harness::schedule::TaskKind;
use rimz::harness::schedule::run_log::SignalRecord;
use std::path::PathBuf;

fn task(root: &Path) -> TaskEntry {
    TaskEntry {
        root: root.to_owned(),
        check: Some("true".into()),
        every: Some("1h".into()),
        ..TaskEntry::default()
    }
}

fn config(env: &Env, tasks: BTreeMap<String, TaskEntry>) {
    write_loop_config(
        env,
        &toml::to_string(&LoopConfig {
            tasks: Tasks(tasks),
            ..LoopConfig::default()
        })
        .unwrap(),
    );
}

fn record(name: &str, root: &Path, result: LoopRunResult, minutes: i64) -> LoopRunRecord {
    let mut record = LoopRunRecord::new(name, result, LoopRunMode::Manual, 0);
    record.root = Some(root.to_owned());
    record.at = Timestamp::now() - SignedDuration::from_mins(minutes);
    if result == LoopRunResult::SignalSkipped {
        record.signal = Some(SignalRecord {
            name: "ci.passed".parse().unwrap(),
            payload: serde_json::Map::new(),
        });
    }
    record
}

/// Seed an active run of loop task `task` at `checkout`, waiting as `wait` says when given.
fn seed_task_run(
    env: &Env,
    task: &str,
    checkout: &Path,
    wait: Option<serde_json::Value>,
) -> PathBuf {
    let paths = env.state_path_for(&env.project_root);
    let guard = rimz::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let now = Timestamp::now();
    let record: rimz::store::workflow::RunRecord = serde_json::from_value(json!({
        "version":rimz::store::workflow::VERSION, "id":rimz::ids::WorkflowRunId::new(), "workflow":"review",
        "task":task, "spec_dir":"/definitions/review", "spec_digest":"abc",
        "scope":{"kind":"worktree","path":checkout}, "project_root":env.project_root,
        "created_at":now, "entered_at":now, "state":"active", "node":"agent", "wait":wait,
        "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":1,
    }))
    .unwrap();
    rimz::store::workflow::initialize(&paths, &record, b"spec", &guard).unwrap();
    paths.workflow(&record.id).record
}

#[test]
fn capacity_waits_reach_list_and_show_as_text_and_json() {
    let env = Env::new();
    env.record(&env.project_root);
    config(
        &env,
        BTreeMap::from([(
            "owed".into(),
            TaskEntry {
                root: env.project_root.clone(),
                agent: Some("claude".into()),
                prompt: Some("repair".into()),
                when: Some(vec!["team.stage=Done".into()]),
                ..Default::default()
            },
        )]),
    );
    let since: Timestamp = "2026-06-01T15:02:00Z".parse().unwrap();
    let path = seed_task_run(
        &env,
        "owed",
        &env.project_root,
        Some(json!({
            "node":"agent", "outcome":"capacity", "detail":"claude: no capacity (your check)",
            "hold":"capacity", "kinds":["claude"], "first":since, "last":since, "count":2,
            "recheck_at":null,
        })),
    );
    let phrase = rimz::theme::fmt::held_start(since, None, jiff::Timestamp::now());
    let listed: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    let task = &listed["rooms"][0]["tasks"][0];
    assert_eq!(
        task["capacity_wait"],
        json!({"kinds": ["claude"], "since": since, "gives_up_at": null, "reason": "claude: no capacity (your check)", "hold": "capacity"})
    );
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "owed", "--json"])).unwrap();
    assert_eq!(shown["capacity_wait"], task["capacity_wait"]);
    let output = loop_ok(&env, &["loop", "show", "owed"]);
    assert!(output.contains(&phrase), "{output}");
    let output = loop_ok(&env, &["loop", "list"]);
    assert!(
        output.contains("HELD  starts waiting for capacity")
            && output.contains("○ 1 held")
            && !output.contains("NEEDS YOU"),
        "{output}"
    );
    let mut run: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    run["wait"]["hold"] = json!("disk");
    run["wait"]["detail"] = json!("free disk low at checkout");
    rimz::disk::atomic::write_temp_then_rename(&path, &run).unwrap();
    let needs_fix = loop_ok(&env, &["loop", "list"]);
    assert!(
        needs_fix.contains("NEEDS YOU")
            && needs_fix.contains("free disk low at checkout")
            && needs_fix.contains("→ rimz doctor")
            && !needs_fix.contains("HELD  starts"),
        "{needs_fix}"
    );
    let listed: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert_eq!(listed["rooms"][0]["tasks"][0]["attention"], "held");
}

#[test]
fn capacity_other_checkouts_stay_visible_while_one_checkout_runs() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    for name in ["capacity-running", "capacity-waiting"] {
        loop_ok(&env, &["worktree", "new", name]);
    }
    let owned = rimz::worktree::discover_owned(&env.project_root).unwrap();
    let running = owned[0].marker.worktree_path.canonicalize().unwrap();
    let waiting = owned[1].marker.worktree_path.canonicalize().unwrap();
    config(
        &env,
        BTreeMap::from([(
            "owed".into(),
            TaskEntry {
                root: env.project_root.clone(),
                agent: Some("claude".into()),
                prompt: Some("repair".into()),
                when: Some(vec!["team.stage=Done".into()]),
                each_worktree: true,
                ..Default::default()
            },
        )]),
    );
    let since: Timestamp = "2026-06-01T15:02:00Z".parse().unwrap();
    seed_task_run(&env, "owed", &running, None);
    seed_task_run(
        &env,
        "owed",
        &waiting,
        Some(json!({
            "node":"agent", "outcome":"capacity", "detail":"no capacity", "kinds":["claude"],
            "first":since, "last":since, "count":1, "recheck_at":null,
        })),
    );
    let shown: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "owed", "--json"])).unwrap();
    let waits = shown["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| !row["capacity_wait"].is_null())
        .map(|row| row["checkout"].clone())
        .collect::<Vec<_>>();
    assert_eq!(waits, [json!(waiting)], "{shown:#}");
    let text = loop_ok(&env, &["loop", "show", "owed"]);
    assert!(
        text.contains("held ") && text.contains("no capacity"),
        "{text}"
    );
}

#[test]
fn expanded_wakes_nest_live_children_and_fall_back_without_agent_rows() {
    use crate::common::wait::{register_agent, register_calling_agent};
    use rimz::agents::LaunchParams;

    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    register_agent(
        &env,
        "child-session",
        "child-launch",
        "modern-notch",
        LaunchParams {
            profile: Some("gatekeeper".into()),
            parent_agent_id: Some("launch-session".into()),
            launch_depth: Some(1),
            ..Default::default()
        },
    );
    let wait = |session: &str, handle: &str| TaskEntry {
        root: env.project_root.clone(),
        at: Some("23:59".into()),
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("claude"),
            session: session.into(),
            handle: handle.into(),
        }),
        wait_meta: Some(
            serde_json::from_value(json!({"armed_at": Timestamp::now(), "delay": "5m"})).unwrap(),
        ),
        ..Default::default()
    };
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([
            ("child-wait".into(), wait("child-session", "@stale-child")),
            ("root-wait".into(), wait("provider-session", "@stale-root")),
            ("orphan-wait".into(), wait("missing", "@stored")),
        ])),
    );
    let output = crate::common::wait::agent_wait(&env)
        .args(["loop", "list", "--all"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("  @planner#main › @gatekeeper (@modern-notch)\n"),
        "{text}"
    );
    assert!(text.contains("  @planner#main (you)\n"), "{text}");
    assert!(text.contains("  @stored\n"), "{text}");
    assert!(!text.contains("@stale"), "{text}");
    assert!(text.contains("3 waits · all healthy"), "{text}");
}

#[test]
fn expanded_waits_tolerate_an_unreadable_room_store() {
    let env = Env::new();
    env.record(&env.project_root);
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "orphan-wait".into(),
            TaskEntry {
                root: env.project_root.clone(),
                at: Some("23:59".into()),
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: "missing".into(),
                    handle: "@stored".into(),
                }),
                wait_meta: Some(
                    serde_json::from_value(json!({"armed_at": Timestamp::now(), "delay": "5m"}))
                        .unwrap(),
                ),
                ..Default::default()
            },
        )])),
    );
    let paths = env.state_path_for(&env.project_root);
    std::fs::remove_file(&paths.workspace_record).unwrap();
    std::fs::create_dir(&paths.workspace_record).unwrap();
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    assert!(
        text.contains("  @stored\n") && text.contains("orphan-wait"),
        "{text}"
    );
    let json = loop_ok(&env, &["loop", "list", "--json"]);
    assert!(json.contains("orphan-wait"), "{json}");
}

#[test]
fn show_resolves_other_rooms_and_retired_names_but_mutations_stay_local() {
    let env = Env::new();
    let other = env.home_root.join("other-room");
    std::fs::create_dir(&other).unwrap();
    config(&env, BTreeMap::from([("remote".into(), task(&other))]));
    let outside = env.home_root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let output = env
        .rimz()
        .current_dir(&outside)
        .args(["loop", "show", "remote"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains(&format!("other commands need --root {}", other.display())),
        "{text}"
    );
    write_loop_run_records(
        &env,
        &[record("retired", &other, LoopRunResult::Completed, 1440)],
    );
    for flags in [vec![], vec!["--json"]] {
        let output = env
            .rimz()
            .args(["loop", "show", "retired"])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "error: no loop task named `retired`; see `rimz loop list`\n  1 retained run, last 1d ago: `rimz --root {} loop logs retired`\n",
                other.display()
            )
        );
    }
}

fn assert_gone_task_refuses(kind: Option<TaskKind>) {
    let env = Env::new();
    let mut run = record("gone", &env.project_root, LoopRunResult::Completed, 1440);
    run.kind = kind;
    write_loop_run_records(&env, &[run]);
    for flags in [vec![], vec!["--json"]] {
        let output = env
            .rimz()
            .args(["loop", "show", "gone"])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "error: no loop task named `gone`; see `rimz loop list`\n  1 retained run, last 1d ago: `rimz loop logs gone`\n"
        );
    }
}

#[test]
fn show_refuses_legacy_history_without_a_definition() {
    assert_gone_task_refuses(None);
}

#[test]
fn show_refuses_routine_history_without_a_definition() {
    assert_gone_task_refuses(Some(TaskKind::Routine));
}

#[test]
fn show_history_hints_use_known_rooms_and_prefer_the_caller() {
    let env = Env::new();
    let roots = [
        env.home_root.join("first-room"),
        env.home_root.join("second-room"),
    ];
    for root in &roots {
        std::fs::create_dir(root).unwrap();
    }
    config(
        &env,
        roots
            .iter()
            .enumerate()
            .map(|(index, root)| (format!("remote-{index}"), task(root)))
            .collect(),
    );
    let mut records = roots
        .iter()
        .map(|root| record("gone", root, LoopRunResult::Completed, 1440))
        .collect::<Vec<_>>();
    write_loop_run_records(&env, &records);
    let (stdout, stderr) = loop_fail(&env, &["loop", "show", "gone"]);
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr,
        format!(
            "error: no loop task named `gone`; see `rimz loop list`\n  1 retained run, last 1d ago: `rimz --root {} loop logs gone`\n  1 retained run, last 1d ago: `rimz --root {} loop logs gone`\n",
            roots[0].display(),
            roots[1].display()
        )
    );
    records.extend(std::iter::repeat_n(
        record("gone", &env.project_root, LoopRunResult::Completed, 1440),
        1001,
    ));
    records.push(record(
        "gone",
        &env.project_root,
        LoopRunResult::SignalSkipped,
        0,
    ));
    write_loop_run_records(&env, &records);
    let (stdout, stderr) = loop_fail(&env, &["loop", "show", "gone"]);
    assert!(stdout.is_empty(), "{stdout}");
    assert_eq!(
        stderr,
        "error: no loop task named `gone`; see `rimz loop list`\n  1,001 retained runs, last 1d ago: `rimz loop logs gone`\n"
    );
}

#[test]
fn show_ignores_unrelated_unknown_room_and_heard_only_history() {
    let env = Env::new();
    for records in [
        vec![],
        vec![record(
            "other",
            &env.project_root,
            LoopRunResult::Completed,
            1440,
        )],
        vec![record(
            "gone",
            &env.home_root.join("unknown-room"),
            LoopRunResult::Completed,
            1440,
        )],
        vec![record(
            "gone",
            &env.project_root,
            LoopRunResult::SignalSkipped,
            1440,
        )],
    ] {
        write_loop_run_records(&env, &records);
        for flags in [vec![], vec!["--json"]] {
            let output = env
                .rimz()
                .args(["loop", "show", "gone"])
                .args(flags)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(output.stdout.is_empty(), "{output:?}");
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                "error: no loop task named `gone`; see `rimz loop list`\n"
            );
        }
    }
}

#[test]
fn show_reads_a_consumed_one_shots_live_workflow_run_after_its_runner_exits() {
    let env = Env::new();
    let home = seed_task_run(&env, "once", &env.project_root, None);
    let id = home
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    let mut fire = record("once", &env.project_root, LoopRunResult::Launched, 1);
    fire.kind = Some(TaskKind::OneOff);
    fire.run_id = Some(id.clone().into_owned());
    write_loop_run_records(&env, &[fire]);
    let shown = loop_ok(&env, &["loop", "show", "once"]);
    assert!(
        shown.lines().next().is_some_and(|head| head
            .starts_with("once — one-shot fired · ▸ running ")
            && !head.contains("pid")),
        "{shown}"
    );
    assert!(
        shown.contains(id.as_ref()),
        "the fire names its run: {shown}"
    );
}

#[test]
fn show_names_retired_one_offs_and_limits_their_fires() {
    let env = Env::new();
    let other = env.home_root.join("other-room");
    std::fs::create_dir(&other).unwrap();
    config(&env, BTreeMap::from([("remote".into(), task(&other))]));
    for root in [&env.project_root, &other] {
        let mut older = record("once", root, LoopRunResult::Completed, 2880);
        older.kind = Some(TaskKind::OneOff);
        older.last_message = Some("older fire".into());
        let mut newer = record("once", root, LoopRunResult::StartFailed, 1440);
        newer.error = Some("newer fire".into());
        write_loop_run_records(&env, &[older, newer]);
        let room = if root == &env.project_root {
            String::new()
        } else {
            format!(
                "room ~/other-room · other commands need --root {}\n",
                root.display()
            )
        };
        let shown = loop_ok(&env, &["loop", "show", "once", "--runs", "1"]);
        assert!(
            shown.starts_with(&format!("{room}once — one-shot fired · finished\n\n")),
            "{shown}"
        );
        assert!(
            shown.contains("newer fire") && !shown.contains("older fire"),
            "{shown}"
        );
        let shown = loop_ok(&env, &["loop", "show", "once"]);
        assert!(
            shown.find("older fire").unwrap() < shown.find("newer fire").unwrap(),
            "{shown}"
        );
        let (stdout, stderr) = loop_fail(&env, &["loop", "show", "once", "--json"]);
        assert!(stdout.is_empty(), "{stdout}");
        let command = if root == &env.project_root {
            "rimz loop logs once".to_owned()
        } else {
            format!("rimz --root {} loop logs once", root.display())
        };
        assert_eq!(
            stderr,
            format!(
                "error: no loop task named `once`; see `rimz loop list`\n  2 retained runs, last 1d ago: `{command}`\n"
            )
        );
    }
}

#[test]
fn show_resolves_worktree_names_and_lists_ambiguous_tasks_and_wakeups() {
    let env = Env::new();
    let checkout = env.home_root.join("card-ux");
    let bound = TaskEntry {
        dir: Some(checkout.clone()),
        ..task(&env.project_root)
    };
    config(
        &env,
        BTreeMap::from([("card-ux-merged".into(), bound.clone())]),
    );
    let mut removed = record("card-ux", &env.project_root, LoopRunResult::Completed, 1);
    removed.kind = Some(TaskKind::Routine);
    write_loop_run_records(&env, &[removed]);
    let text = loop_ok(&env, &["loop", "show", "card-ux"]);
    assert!(
        text.starts_with("worktree card-ux → card-ux-merged\n"),
        "{text}"
    );
    loop_ok(&env, &["loop", "remove", "card-ux"]);
    let listed: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert!(
        listed["rooms"][0]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|task| task["name"] == "card-ux-merged"),
        "mutations must not accept worktree aliases"
    );
    config(
        &env,
        BTreeMap::from([
            ("card-ux-merged".into(), bound.clone()),
            ("review".into(), bound.clone()),
        ]),
    );
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "wake".into(),
            TaskEntry {
                check: None,
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: "session".into(),
                    handle: "@coder#card-ux".into(),
                }),
                ..bound
            },
        )])),
    );
    let output = env
        .rimz()
        .args(["loop", "show", "card-ux"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for fragment in [
        "rimz loop show card-ux-merged",
        "rimz loop show review",
        "+ 1 wake-ups",
        "every 1h",
    ] {
        assert!(text.contains(fragment), "missing {fragment}: {text}");
    }
    let output = env
        .rimz()
        .args(["loop", "show", "card-ux", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("rimz loop show review"),
        "{output:?}"
    );
}

#[test]
fn worktree_ambiguity_commands_select_the_matching_room() {
    let env = Env::new();
    let other = env.home_root.join("other-room");
    std::fs::create_dir(&other).unwrap();
    let mut tasks = BTreeMap::new();
    for (name, root) in [("local", &env.project_root), ("remote", &other)] {
        let dir = root.join("lane");
        std::fs::create_dir(&dir).unwrap();
        tasks.insert(
            name.into(),
            TaskEntry {
                dir: Some(dir),
                ..task(root)
            },
        );
    }
    config(&env, tasks);
    let output = env.rimz().args(["loop", "show", "lane"]).output().unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("rimz loop show local"), "{text}");
    assert!(
        text.contains(&format!("rimz --root {} loop show remote", other.display())),
        "{text}"
    );
}

#[test]
fn show_resolves_team_owners_and_disambiguates_exact_names_by_room() {
    let env = Env::new();
    let wake = TaskEntry {
        team: Some("forge#lane".parse().unwrap()),
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("claude"),
            session: "session".into(),
            handle: "@coder#lane".into(),
        }),
        check: None,
        ..task(&env.project_root)
    };
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "generated-subscription".into(),
            wake.clone(),
        )])),
    );
    let history = ["forge", "forge#lane"].map(|name| {
        let mut run = record(name, &env.project_root, LoopRunResult::Completed, 1440);
        run.kind = Some(TaskKind::Routine);
        run
    });
    write_loop_run_records(&env, &history);
    for owner in ["forge", "forge#lane"] {
        let text = loop_ok(&env, &["loop", "show", owner]);
        assert!(text.contains("SUBSCRIPTIONS"), "{text}");
        assert!(text.contains("generated-subscription"), "{text}");
    }
    loop_ok(&env, &["loop", "remove", "generated-subscription"]);
    for name in ["first-room", "second-room"] {
        let root = env.home_root.join(name);
        std::fs::create_dir(&root).unwrap();
        let path = env
            .state_path_for(&root)
            .root
            .join("records/loop-instances.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::to_vec(&Tasks(BTreeMap::from([(
                "same".into(),
                TaskEntry {
                    root,
                    ..wake.clone()
                },
            )])))
            .unwrap(),
        )
        .unwrap();
    }
    let output = env.rimz().args(["loop", "show", "same"]).output().unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for room in ["first-room", "second-room"] {
        assert!(
            text.contains(&format!(
                "rimz --root {} loop show same",
                env.home_root.join(room).display()
            )),
            "{text}"
        );
    }
    let output = env
        .rimz()
        .args(["loop", "show", "same", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("loop show same"),
        "{output:?}"
    );
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([("same".into(), task(&env.project_root))])),
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "same", "--json"])).unwrap();
    assert_eq!(
        value["entry"]["root"],
        env.project_root.to_string_lossy().as_ref()
    );
}

#[test]
fn lifecycle_sections_and_retired_one_offs_are_explicit() {
    let env = Env::new();
    let clock = task(&env.project_root);
    let one_off = TaskEntry {
        every: None,
        at: Some("08:00".into()),
        ..clock.clone()
    };
    let resident = TaskEntry {
        every: None,
        signal: Some("pr.merged".into()),
        stay: true,
        agent: Some("claude".into()),
        prompt: Some("inspect".into()),
        check: None,
        ..clock.clone()
    };
    config(
        &env,
        BTreeMap::from([
            ("clock".into(), clock.clone()),
            ("once".into(), one_off),
            ("resident".into(), resident.clone()),
            (
                "fleet".into(),
                TaskEntry {
                    signal: None,
                    when: Some(vec!["pr=merged".into()]),
                    each_worktree: true,
                    ..resident
                },
            ),
        ]),
    );
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([
            (
                "wake".into(),
                TaskEntry {
                    wait: Some(TaskTarget {
                        kind: AgentKind::new_unchecked("claude"),
                        session: "session".into(),
                        handle: "@coder".into(),
                    }),
                    check: None,
                    ..clock.clone()
                },
            ),
            (
                "owned".into(),
                TaskEntry {
                    team: Some("forge#lane".parse().unwrap()),
                    wait: Some(TaskTarget {
                        kind: AgentKind::new_unchecked("claude"),
                        session: "session".into(),
                        handle: "@coder".into(),
                    }),
                    check: None,
                    ..clock
                },
            ),
        ])),
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    for (name, kind, section) in [
        ("clock", "routine", "routine"),
        ("fleet", "routine", "routine"),
        ("once", "one_off", "waiting"),
        ("resident", "one_off", "waiting"),
        ("wake", "wake_up", "wake_ups"),
        ("owned", "wake_up", "wake_ups"),
    ] {
        let row = value["rooms"][0]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["kind"], kind, "{row}");
        assert_eq!(row["section"], section, "{row}");
    }
    let mut history = (0..7)
        .map(|index| {
            let mut run = record(
                &format!("retired-{index}"),
                &env.project_root,
                LoopRunResult::Completed,
                index + 1,
            );
            run.kind = Some(TaskKind::OneOff);
            run
        })
        .collect::<Vec<_>>();
    let mut routine = record(
        "removed-routine",
        &env.project_root,
        LoopRunResult::Completed,
        1,
    );
    routine.kind = Some(TaskKind::Routine);
    history.push(routine);
    let mut old = record("legacy", &env.project_root, LoopRunResult::Completed, 1);
    old.kind = None;
    history.push(old);
    history.push(record(
        "retired-0",
        &env.project_root,
        LoopRunResult::Completed,
        0,
    ));
    let mut changed = record(
        "formerly-once",
        &env.project_root,
        LoopRunResult::Completed,
        2,
    );
    changed.kind = Some(TaskKind::OneOff);
    history.push(changed.clone());
    changed.at = Timestamp::now();
    changed.kind = Some(TaskKind::Routine);
    history.push(changed);
    write_loop_run_records(&env, &history);
    let paths = env.state_path_for(&env.project_root);
    let ledger = paths.root.join("records/loop-launches.json");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(ledger, serde_json::to_vec(&json!({"resident": {env.project_root.to_str().unwrap(): {"at": Timestamp::now(), "leader": "@leader"}}})).unwrap()).unwrap();
    let text = loop_ok(&env, &["loop", "list"]);
    for section in ["ROUTINE", "WAITING", "DONE", "WAKE-UPS"] {
        assert!(text.contains(section), "{text}");
    }
    assert!(text.contains("2 subscriptions · all healthy"), "{text}");
    assert!(text.contains("launched claude @leader"), "{text}");
    assert!(text.contains("(and 3 older · --all)"), "{text}");
    assert!(
        !text.contains("retired-6")
            && !text.contains("removed-routine")
            && !text.contains("legacy")
            && !text.contains("formerly-once"),
        "{text}"
    );
    assert!(
        text.find("retired-0").unwrap() < text.find("retired-1").unwrap(),
        "{text}"
    );
    let all = loop_ok(&env, &["loop", "list", "--all"]);
    assert!(
        all.contains("retired-6") && all.contains("team forge#lane") && all.contains("wake"),
        "{all}"
    );
    for index in 0..7 {
        let name = format!("retired-{index}");
        let shown = loop_ok(&env, &["loop", "show", &name]);
        assert!(
            shown.starts_with(&format!("{name} — one-shot fired · finished\n")),
            "{shown}"
        );
    }
    for name in ["removed-routine", "legacy", "formerly-once"] {
        let (stdout, stderr) = loop_fail(&env, &["loop", "show", name]);
        assert!(stdout.is_empty(), "{stdout}");
        let count = history.iter().filter(|run| run.task == name).count();
        assert_eq!(stderr, retained_runs_refusal(&stderr, name, count));
    }
    let outside = tempfile::tempdir().unwrap();
    let output = env
        .rimz()
        .current_dir(outside.path())
        .args(["loop", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outside_text = String::from_utf8(output.stdout).unwrap();
    assert!(
        outside_text.contains("WAKE-UPS")
            && !outside_text.contains("retired-6")
            && !outside_text.contains("team forge#lane ·"),
        "{outside_text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert_eq!(
        value["rooms"][0]["retired"].as_array().unwrap().len(),
        7,
        "{value}"
    );
    let resident = value["rooms"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "resident")
        .unwrap();
    assert_eq!(resident["section"], "done", "{resident}");
}

#[test]
fn disabled_one_off_displays_its_state() {
    let env = Env::new();
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "disabled".into(),
            TaskEntry {
                every: None,
                at: Some("23:59".into()),
                ..task(&env.project_root)
            },
        )])),
    );
    loop_ok(&env, &["loop", "disable", "disabled"]);
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(text.contains("once at 23:59 · off"), "{text}");
}

#[test]
fn plain_one_off_ignores_a_former_resident_launch() {
    let env = Env::new();
    let entry = TaskEntry {
        every: None,
        at: Some("23:59".into()),
        ..task(&env.project_root)
    };
    write_loop_instances(&env, Tasks(BTreeMap::from([("reused".into(), entry)])));
    let paths = env.state_path_for(&env.project_root);
    let ledger = paths.root.join("records/loop-launches.json");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(ledger, serde_json::to_vec(&json!({"reused": {env.project_root.to_str().unwrap(): {"at": Timestamp::now(), "leader": "@old"}}})).unwrap()).unwrap();
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert_eq!(
        value["rooms"][0]["tasks"][0]["section"], "waiting",
        "{value}"
    );
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(text.contains("once at 23:59"), "{text}");
    assert!(!text.contains("launched"), "{text}");
}

#[test]
fn condition_counts_and_unhealthy_wake_ups_are_visible() {
    let env = Env::new();
    config(
        &env,
        BTreeMap::from([
            (
                "and".into(),
                TaskEntry {
                    every: None,
                    once: Some(true),
                    when: Some(vec!["pr=merged && ci=passed && pr.queue=queued".into()]),
                    ..task(&env.project_root)
                },
            ),
            (
                "or".into(),
                TaskEntry {
                    every: None,
                    when: Some(vec!["pr=merged || ci=passed".into()]),
                    ..task(&env.project_root)
                },
            ),
        ]),
    );
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "wake".into(),
            TaskEntry {
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: "session".into(),
                    handle: "@coder".into(),
                }),
                check: None,
                ..task(&env.project_root)
            },
        )])),
    );
    write_loop_run_records(
        &env,
        &[record("wake", &env.project_root, LoopRunResult::Failed, 1)],
    );
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(!text.contains("of 3 met"), "{text}");
    assert!(text.contains("1 subscription · 1 need you"), "{text}");
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    let rows = value["rooms"][0]["tasks"].as_array().unwrap();
    assert!(
        rows.iter()
            .find(|row| row["name"] == "and")
            .unwrap()
            .get("condition")
            .is_none()
    );
    assert!(
        rows.iter()
            .find(|row| row["name"] == "or")
            .unwrap()
            .get("condition")
            .is_none()
    );
}

#[test]
fn acting_history_and_heard_are_not_skips() {
    let env = Env::new();
    config(
        &env,
        ["acted", "heard"]
            .map(|name| (name.into(), task(&env.project_root)))
            .into(),
    );
    let mut acted = record("acted", &env.project_root, LoopRunResult::Completed, 10);
    acted.check = Some(
        serde_json::from_value(json!({
            "code": 0, "timed_out": false, "output": "1\n2\n3\n4\n5\n6\n7\n"
        }))
        .unwrap(),
    );
    write_loop_run_records(
        &env,
        &[
            acted,
            record("acted", &env.project_root, LoopRunResult::SignalSkipped, 7),
            record("acted", &env.project_root, LoopRunResult::SignalSkipped, 6),
            record("acted", &env.project_root, LoopRunResult::SignalSkipped, 5),
            record("heard", &env.project_root, LoopRunResult::SignalSkipped, 3),
            record(
                "retired",
                &env.project_root,
                LoopRunResult::SignalSkipped,
                2,
            ),
        ],
    );
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(text.contains("✓ 10m ago"), "{text}");
    assert!(
        text.contains("never fired · heard ci.passed 3m ago") && !text.contains("skipped"),
        "{text}"
    );
    let show = loop_ok(&env, &["loop", "show", "acted"]);
    assert!(show.contains("healthy"));
    assert!(show.contains("LAST RUN — ✓ completed"), "{show}");
    assert!(
        show.contains("heard 3 other ci.* signals since, latest ci.passed 5m ago"),
        "{show}"
    );
    assert!(!show.contains("RECENT RUNS"), "{show}");
    assert!(show.contains("rimz loop logs acted -n 1"), "{show}");
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "acted", "--json"])).unwrap();
    assert_eq!(value["runs"].as_array().unwrap().len(), 1);
    assert_eq!(value["heard"]["count"], 3);
    assert_eq!(value["heard"]["latest"], "ci.passed");
    assert_eq!(value["heard"]["since"], value["runs"][0]["at"]);
    let logs = loop_ok(&env, &["loop", "logs", "acted"]);
    assert!(!logs.contains("ci.passed"), "{logs}");
    let logs = loop_ok(&env, &["loop", "logs", "acted", "--heard", "-n", "2"]);
    assert_eq!(logs.matches("○ heard").count(), 2, "{logs}");
    let logs = loop_ok(&env, &["loop", "logs", "retired"]);
    assert!(
        logs.contains("rimz loop logs retired --heard") && !logs.contains("loop fire"),
        "{logs}"
    );
    let show = loop_ok(&env, &["loop", "show", "heard"]);
    assert!(
        show.contains("heard 1 other ci.* signal, latest ci.passed 3m ago"),
        "{show}"
    );
}

#[test]
fn show_reused_wait_names_ignore_history_before_arming() {
    let env = Env::new();
    let armed_at = Timestamp::now() - SignedDuration::from_mins(12);
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "wait-reused".into(),
            TaskEntry {
                root: env.project_root.clone(),
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: "current".into(),
                    handle: "@coder".into(),
                }),
                watch: Some(rimz::config::WatchSpec::Command("true".into())),
                wait_meta: Some(serde_json::from_value(json!({"armed_at": armed_at})).unwrap()),
                ..TaskEntry::default()
            },
        )])),
    );
    write_loop_run_records(
        &env,
        &[
            record(
                "wait-reused",
                &env.project_root,
                LoopRunResult::Delivered,
                30,
            ),
            record(
                "wait-reused",
                &env.project_root,
                LoopRunResult::SignalSkipped,
                20,
            ),
        ],
    );
    let text = loop_ok(&env, &["loop", "show", "wait-reused"]);
    assert!(
        text.contains("wakes @coder; not fired yet")
            && !text.contains("loop fire")
            && !text.contains("LAST RUN"),
        "{text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "wait-reused", "--json"])).unwrap();
    assert!(value["runs"].as_array().unwrap().is_empty(), "{value}");
    let logs = loop_ok(&env, &["loop", "logs", "wait-reused", "--heard"]);
    assert!(
        logs.contains("wakes @coder; not fired yet") && !logs.contains("loop fire"),
        "{logs}"
    );
    let mut current = record(
        "wait-reused",
        &env.project_root,
        LoopRunResult::Delivered,
        0,
    );
    current.at = armed_at;
    write_loop_run_records(&env, &[current]);
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "wait-reused", "--json"])).unwrap();
    assert_eq!(value["runs"].as_array().unwrap().len(), 1);
    assert_eq!(value["runs"][0]["result"], "delivered");
}

#[test]
fn gone_checkout_recovers_without_mutating_definition() {
    let env = Env::new();
    let checkout = env.home_root.join("returning");
    let mut entry = task(&env.project_root);
    entry.dir = Some(checkout.clone());
    config(&env, BTreeMap::from([("bound".into(), entry)]));
    let before = std::fs::read(loop_config_path(&env)).unwrap();
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("NEEDS YOU") && text.contains("checkout returning is gone"),
        "{text}"
    );
    assert!(text.contains("→ rimz loop remove bound"), "{text}");
    std::fs::create_dir(&checkout).unwrap();
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("ROUTINE") && !text.contains("NEEDS YOU"),
        "{text}"
    );
    assert_eq!(std::fs::read(loop_config_path(&env)).unwrap(), before);
}

#[test]
fn checkout_path_forms_share_resolved_group_and_json_dir() {
    let env = Env::new();
    let checkout = env.project_root.join("feature");
    std::fs::create_dir(&checkout).unwrap();
    let home_relative = PathBuf::from("~").join(checkout.strip_prefix(&env.home_root).unwrap());
    let entries = [home_relative, PathBuf::from("feature"), checkout.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, dir)| {
            (
                format!("bound-{index}"),
                TaskEntry {
                    dir: Some(dir),
                    ..task(&env.project_root)
                },
            )
        })
        .collect();
    config(&env, entries);
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(!text.contains("NEEDS YOU"), "{text}");
    assert_eq!(text.matches("feature").count(), 3, "{text}");
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    for row in value["rooms"][0]["tasks"].as_array().unwrap() {
        assert_eq!(row["dir"], checkout.to_str().unwrap(), "{row}");
        assert_eq!(row["section"], "routine", "{row}");
    }
}

#[test]
fn default_scope_only_names_other_rooms_attention() {
    let env = Env::new();
    let other = env.home_root.join("elsewhere");
    std::fs::create_dir(&other).unwrap();
    config(
        &env,
        BTreeMap::from([
            ("local".into(), task(&env.project_root)),
            ("remote-healthy".into(), task(&other)),
            ("remote-disabled".into(), task(&other)),
        ]),
    );
    let mut arming = read_loop_arming(&env);
    arming.insert(
        machine_task_key("remote-disabled"),
        Arming {
            enabled: false,
            strikes: Some(3),
            at: None,
            pause_until: None,
        },
    );
    write_loop_arming(&env, &arming);
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(!text.contains("remote-healthy"), "{text}");
    assert!(
        text.lines()
            .any(|line| line.starts_with("elsewhere: remote-disabled")
                && line.contains("disabled after 3 strikes")),
        "{text}"
    );
    let all = loop_ok(&env, &["loop", "list", "--all"]);
    assert!(
        all.contains("remote-healthy") && !all.contains("elsewhere:"),
        "{all}"
    );
}

#[test]
fn other_room_hints_act_on_that_room() {
    let env = Env::new();
    let other = env.home_root.join("other ' room");
    std::fs::create_dir(&other).unwrap();
    env.record(&other);
    config(&env, BTreeMap::from([("discover".into(), task(&other))]));
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([(
            "missing".into(),
            task(&env.project_root),
        )])),
    );
    write_project_config(&env, "[tasks.local]\ncheck = \"true\"\nevery = \"1h\"\n");
    write_project_config_at(
        &other,
        "[tasks.blocked]\ncheck = \"true\"\nevery = \"1h\"\n",
    );
    let instances = env
        .state_path_for(&other)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(instances.parent().unwrap()).unwrap();
    std::fs::write(
        &instances,
        serde_json::to_vec(&Tasks(BTreeMap::from([
            (
                "missing".into(),
                TaskEntry {
                    dir: Some(other.join("gone")),
                    ..task(&other)
                },
            ),
            ("failed".into(), task(&other)),
        ])))
        .unwrap(),
    )
    .unwrap();
    write_loop_run_records(&env, &[record("failed", &other, LoopRunResult::Failed, 2)]);
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    let remote_text = text.split("other ' room · ").nth(1).unwrap();
    let hints = remote_text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("→ "))
        .map(|line| shlex::split(line).unwrap())
        .collect::<Vec<_>>();
    for suffix in [
        vec!["trust", "grant"],
        vec!["loop", "show", "failed"],
        vec!["loop", "remove", "missing"],
    ] {
        let words = hints
            .iter()
            .find(|words| {
                words.ends_with(
                    &suffix
                        .iter()
                        .map(|word| word.to_string())
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap();
        assert_eq!(
            &words[..3],
            ["rimz", "--root", other.to_str().unwrap()],
            "{text}"
        );
        let output = env.rimz().args(&words[1..]).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let remote: serde_json::Value = serde_json::from_str(&loop_ok(
        &env,
        &["--root", other.to_str().unwrap(), "loop", "list", "--json"],
    ))
    .unwrap();
    assert!(
        remote["rooms"][0]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "blocked" && row.get("attention").is_none()),
        "{remote}"
    );
    let local = loop_ok(&env, &["loop", "list"]);
    assert!(
        local.contains("blocked · project untrusted") && local.contains("missing"),
        "{local}"
    );
    let remaining: Tasks = serde_json::from_slice(&std::fs::read(instances).unwrap()).unwrap();
    assert!(!remaining.0.contains_key("missing"));
}

#[test]
fn all_json_loads_instances_and_history_per_room() {
    let env = Env::new();
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    config(
        &env,
        BTreeMap::from([("same".into(), task(&env.project_root))]),
    );
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([("same".into(), task(&env.project_root))])),
    );
    let other_paths = env.state_path_for(&other);
    let path = other_paths.root.join("records/loop-instances.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::to_vec(&Tasks(BTreeMap::from([("same".into(), task(&other))]))).unwrap(),
    )
    .unwrap();
    write_loop_run_records(
        &env,
        &[
            record("same", &env.project_root, LoopRunResult::Completed, 10),
            record("same", &other, LoopRunResult::Failed, 2),
        ],
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--all", "--json"])).unwrap();
    assert_eq!(value["rooms"].as_array().unwrap().len(), 2, "{value}");
    assert_eq!(value["rooms"][0]["tasks"][0]["last"]["result"], "completed");
    assert_eq!(value["rooms"][1]["tasks"][0]["last"]["result"], "failed");
    assert!(value["rooms"][0]["tasks"][0].get("heard").is_none());
    let local: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert_eq!(local["rooms"].as_array().unwrap().len(), 1);
}

#[test]
fn show_uses_the_selected_tasks_room_history() {
    let env = Env::new();
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    config(&env, BTreeMap::from([("remote".into(), task(&other))]));
    write_loop_run_records(
        &env,
        &[
            record("remote", &env.project_root, LoopRunResult::Failed, 1),
            record("remote", &other, LoopRunResult::Completed, 2),
        ],
    );
    let text = loop_ok(&env, &["loop", "show", "remote"]);
    assert!(
        text.contains("healthy") && !text.contains("failing"),
        "{text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "remote", "--json"])).unwrap();
    assert_eq!(value["runs"].as_array().unwrap().len(), 1, "{value}");
    assert_eq!(value["runs"][0]["root"], other.to_str().unwrap());
    assert_eq!(value["runs"][0]["result"], "completed");
}

#[test]
fn subscriptions_collapse_but_json_keeps_real_names() {
    let env = Env::new();
    let checkout = env.home_root.join("feature");
    std::fs::create_dir(&checkout).unwrap();
    let entries = ["ci.failed", "pr.conflicted", "pr.dequeued", "pr.merged"]
        .into_iter()
        .enumerate()
        .map(|(index, signal)| {
            (
                format!("generated-{index}"),
                TaskEntry {
                    root: env.project_root.clone(),
                    dir: Some(checkout.clone()),
                    loop_task: Some("sweep".into()),
                    wait: Some(TaskTarget {
                        kind: AgentKind::new_unchecked("claude"),
                        session: "session".into(),
                        handle: "@coder#feature".into(),
                    }),
                    signal: Some(signal.into()),
                    matches: Some(BTreeMap::from([(
                        "path".into(),
                        checkout.to_string_lossy().into_owned(),
                    )])),
                    ..TaskEntry::default()
                },
            )
        })
        .collect();
    write_loop_instances(&env, Tasks(entries));
    write_loop_run_records(
        &env,
        &[
            record(
                "generated-0",
                &env.project_root,
                LoopRunResult::Delivered,
                10,
            ),
            record(
                "generated-1",
                &env.project_root,
                LoopRunResult::Delivered,
                5,
            ),
            record(
                "generated-2",
                &env.project_root,
                LoopRunResult::SignalSkipped,
                1,
            ),
        ],
    );
    let folded = loop_ok(&env, &["loop", "list"]);
    assert!(folded.contains("4 subscriptions · all healthy"), "{folded}");
    assert!(
        !folded.contains("generated-") && !folded.contains("sweep"),
        "{folded}"
    );
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    assert!(
        text.contains("ci.failed pr.{conflicted,dequeued,merged}"),
        "{text}"
    );
    assert_eq!(text.matches("sweep ·").count(), 1, "{text}");
    assert!(text.contains("in 1 worktree\n"), "{text}");
    assert!(!text.contains("rimz loop list --all"), "{text}");
    assert!(
        text.contains("4 tasks")
            && !text.contains("generated-")
            && !text.contains("[path=")
            && !text.contains(checkout.to_str().unwrap()),
        "{text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert_eq!(value["rooms"][0]["tasks"].as_array().unwrap().len(), 4);
    assert_eq!(value["rooms"][0]["tasks"][0]["name"], "generated-0");

    let mut tasks = read_loop_instances(&env);
    let mut named = tasks.0["generated-0"].clone();
    named.loop_task = None;
    tasks.0.insert("user-one".into(), named.clone());
    tasks.0.insert("user-two".into(), named);
    write_loop_instances(&env, tasks);
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    assert!(
        text.contains("user-one") && text.contains("user-two") && text.contains("6 tasks"),
        "{text}"
    );
}

#[test]
fn wake_ups_group_each_owner_once_across_states_and_suffixes() {
    let env = Env::new();
    let checkout = env.home_root.join("feature");
    std::fs::create_dir(&checkout).unwrap();
    let entries = ["off", "live", "paused", "labelled"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            (
                name.into(),
                TaskEntry {
                    root: env.project_root.clone(),
                    dir: Some(checkout.clone()),
                    loop_task: Some("sweep".into()),
                    wait: Some(TaskTarget {
                        kind: AgentKind::new_unchecked("claude"),
                        session: "current".into(),
                        handle: "@coder#feature".into(),
                    }),
                    signal: Some(format!("ci.signal{index}")),
                    label: (name == "labelled").then(|| "gate docs".into()),
                    ..TaskEntry::default()
                },
            )
        })
        .collect();
    write_loop_instances(&env, Tasks(entries));
    let now = Timestamp::now();
    write_loop_arming(
        &env,
        &BTreeMap::from([
            (
                project_task_key(&env.project_root, "off"),
                Arming {
                    enabled: false,
                    at: Some(now),
                    pause_until: None,
                    strikes: None,
                },
            ),
            (
                project_task_key(&env.project_root, "paused"),
                Arming {
                    enabled: true,
                    at: Some(now),
                    pause_until: Some(now + SignedDuration::from_mins(20)),
                    strikes: None,
                },
            ),
        ]),
    );
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    assert_eq!(text.matches("sweep ·").count(), 1, "{text}");
    assert!(
        text.contains("ci.{signal0,signal1,signal2,signal3}"),
        "{text}"
    );
}

#[test]
fn empty_room_read_leaves_state_absent() {
    let env = Env::new();
    let other = env.home_root.join("other");
    std::fs::create_dir(&other).unwrap();
    config(&env, BTreeMap::from([("remote".into(), task(&other))]));
    let paths = env.state_path_for(&env.project_root);
    assert!(!paths.root.exists());
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("ROUTINE") && text.contains("remote"),
        "{text}"
    );
    let scoped = loop_ok(
        &env,
        &["--root", env.project_root.to_str().unwrap(), "loop", "list"],
    );
    assert!(scoped.contains("no loop tasks in"), "{scoped}");
    assert!(!paths.root.exists());
}

#[test]
fn inactive_tasks_do_not_request_attention_for_old_failures() {
    let env = Env::new();
    config(
        &env,
        BTreeMap::from([("off".into(), task(&env.project_root))]),
    );
    write_loop_arming(
        &env,
        &BTreeMap::from([(
            machine_task_key("off"),
            Arming {
                enabled: false,
                at: None,
                pause_until: None,
                strikes: None,
            },
        )]),
    );
    write_project_config(
        &env,
        "[tasks.not-enabled]\ncheck = \"true\"\nevery = \"1h\"\n",
    );
    loop_ok(&env, &["trust", "grant"]);
    write_loop_run_records(
        &env,
        &[
            record("off", &env.project_root, LoopRunResult::Failed, 2),
            record("not-enabled", &env.project_root, LoopRunResult::Failed, 2),
        ],
    );
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(!text.contains("NEEDS YOU"), "{text}");
    assert!(
        text.contains("off · repo task, enable here to run"),
        "{text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    for row in value["rooms"][0]["tasks"].as_array().unwrap() {
        assert!(row.get("attention").is_none(), "{row}");
        assert_eq!(row["section"], "routine");
    }
}

#[test]
fn remote_malformed_instances_warn_and_skip_only_remote_room() {
    let env = Env::new();
    let other = env.home_root.join("broken");
    std::fs::create_dir(&other).unwrap();
    config(
        &env,
        BTreeMap::from([
            ("local".into(), task(&env.project_root)),
            ("remote".into(), task(&other)),
        ]),
    );
    let path = env
        .state_path_for(&other)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "not json").unwrap();
    let output = env.rimz().args(["loop", "list", "--all"]).output().unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{error}");
    assert!(text.contains("local") && !text.contains("remote"), "{text}");
    assert!(
        error.contains(other.to_str().unwrap()) && error.contains("loop-instances.json"),
        "{error}"
    );
}

#[test]
fn malformed_launch_memory_warns_without_hiding_local_or_remote_tasks() {
    for workflow in [true, false] {
        for remote in [false, true] {
            let env = Env::new();
            let other = env.home_root.join("other-room");
            std::fs::create_dir(&other).unwrap();
            let root = if remote { &other } else { &env.project_root };
            let entry = TaskEntry {
                root: root.clone(),
                when: Some(vec!["pr=open".into()]),
                each_worktree: true,
                workflow: workflow.then(|| "review".into()),
                agent: (!workflow).then(|| "codex".into()),
                prompt: (!workflow).then(|| "review".into()),
                stay: !workflow,
                ..TaskEntry::default()
            };
            let healthy = TaskEntry {
                workflow: (!workflow).then(|| "review".into()),
                agent: workflow.then(|| "codex".into()),
                prompt: workflow.then(|| "review".into()),
                stay: workflow,
                ..entry.clone()
            };
            config(
                &env,
                BTreeMap::from([
                    ("review".into(), entry),
                    ("healthy".into(), healthy),
                    ("ordinary".into(), task(root)),
                ]),
            );
            let file = if workflow {
                "loop-workflow-launches.json"
            } else {
                "loop-launches.json"
            };
            let path = env.state_path_for(root).root.join("records").join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "not json").unwrap();
            let launch = if workflow {
                json!({"at": Timestamp::now(), "leader": "@coder"})
            } else {
                json!({
                    "at": Timestamp::now(),
                    "run": rimz::ids::WorkflowRunId::new(),
                    "scope": "worktree"
                })
            };
            let tasks = json!({"healthy": {root.to_str().unwrap(): launch}});
            let (healthy_file, ledger) = if workflow {
                ("loop-launches.json", tasks)
            } else {
                (
                    "loop-workflow-launches.json",
                    json!({"version": 1, "tasks": tasks}),
                )
            };
            std::fs::write(
                env.state_path_for(root)
                    .root
                    .join("records")
                    .join(healthy_file),
                serde_json::to_vec(&ledger).unwrap(),
            )
            .unwrap();
            for flags in [vec!["--all"], vec!["--all", "--json"]] {
                let output = env
                    .rimz()
                    .args(["loop", "list"])
                    .args(flags)
                    .output()
                    .unwrap();
                let error = String::from_utf8_lossy(&output.stderr);
                let text = String::from_utf8_lossy(&output.stdout);
                assert!(output.status.success(), "{file}, remote={remote}: {error}");
                assert!(
                    text.contains("review")
                        && text.contains("ordinary")
                        && text.contains("healthy"),
                    "{text}"
                );
                assert!(
                    !text.contains("1 worktrees"),
                    "one memory serves both: {text}"
                );
                assert!(
                    error.contains("warning:")
                        && error.contains("cannot load loop launch memory")
                        && error.contains(root.to_str().unwrap()),
                    "{error}"
                );
            }
        }
    }
}

#[test]
fn each_worktree_uses_acting_history_and_launch_count() {
    let env = Env::new();
    let mut entry = task(&env.project_root);
    entry.every = None;
    entry.check = None;
    entry.agent = Some("codex".into());
    entry.prompt = Some("review".into());
    entry.stay = true;
    entry.each_worktree = true;
    entry.when = Some(vec!["pr=merged".into()]);
    entry.hold = Some("3m".into());
    config(&env, BTreeMap::from([("sweep".into(), entry)]));
    let mut launched = record("sweep", &env.project_root, LoopRunResult::Launched, 30);
    launched.checkout = Some(env.home_root.join("one"));
    write_loop_run_records(&env, &[launched]);
    let paths = env.state_path_for(&env.project_root);
    let ledger_path = paths.root.join("records/loop-launches.json");
    std::fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    std::fs::write(ledger_path, serde_json::to_vec(&json!({"sweep": {env.home_root.join("one").to_str().unwrap(): {"at": Timestamp::now(), "leader": "@coder"}, env.home_root.join("two").to_str().unwrap(): {"at": Timestamp::now(), "leader": "@other"}}})).unwrap()).unwrap();
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("✓ 30m ago · 2 worktrees") && !text.contains("never fired"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.trim() == "for 3m · each worktree"),
        "{text}"
    );
}

#[test]
fn lost_watch_needs_attention_and_keeps_caller_markers() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.record(&env.project_root);
    let checkout = env.home_root.join("feature");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            checkout.to_str().unwrap()
        ]
    ));
    seed_agent_launch(&env, &checkout, "coder", Default::default(), None);
    let mut hook = env.hook_command("claude");
    hook.current_dir(&checkout)
        .env("RIMZ_AGENT_ID", "launch_coder")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "coder");
    let output = env
        .spawn_payload(
            hook,
            &json!({"hook_event_name": "SessionStart", "session_id": "caller", "cwd": checkout})
                .to_string(),
        )
        .wait_with_output()
        .unwrap();
    env.drain_hooks();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let entry = TaskEntry {
        root: env.project_root.clone(), dir: Some(checkout.clone()),
        label: Some("gate docs".into()),
        wait: Some(TaskTarget { kind: AgentKind::new_unchecked("claude"), session: "caller".into(), handle: "@coder#feature".into() }),
        watch: Some(rimz::config::WatchSpec::Command("private command preview".into())),
        wait_meta: Some(serde_json::from_value(json!({"armed_at": Timestamp::now() - SignedDuration::from_mins(12), "reader": "coder"})).unwrap()),
        ..TaskEntry::default()
    };
    write_loop_instances(&env, Tasks(BTreeMap::from([("wait-docs".into(), entry)])));
    let output = env
        .rimz()
        .current_dir(&checkout)
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch_coder")
        .args(["loop", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("feature (here)") && text.contains("@coder (you)"),
        "{text}"
    );
    assert!(
        text.contains("on command exit · gate docs")
            && text.contains("watcher gone · will not wake @coder (you)")
            && text.contains("NEEDS YOU")
            && text.contains("→ rimz loop remove wait-docs")
            && !text.contains("private command"),
        "{text}"
    );
    let expanded = env
        .rimz()
        .current_dir(&checkout)
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch_coder")
        .args(["loop", "list", "--all"])
        .output()
        .unwrap();
    assert!(expanded.status.success());
    let expanded = String::from_utf8(expanded.stdout).unwrap();
    assert!(expanded.contains("  @coder#feature (you)\n"), "{expanded}");
    assert!(expanded.contains("lost · armed 12m ago"), "{expanded}");
    let runtime = env.runtime_paths();
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_mins(12),
    };
    let _watcher = hold_loop_run_lock(&runtime.lock_path("loop-watch-wait-docs.lock"), &holder);
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    assert!(
        text.contains("watching 12m") && !text.contains("lost"),
        "{text}"
    );
}

#[test]
fn reused_wait_names_ignore_history_before_arming() {
    let env = Env::new();
    let armed_at = Timestamp::now() - SignedDuration::from_mins(12);
    let entry = TaskEntry {
        root: env.project_root.clone(),
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("claude"),
            session: "current".into(),
            handle: "@coder".into(),
        }),
        watch: Some(rimz::config::WatchSpec::Command("true".into())),
        wait_meta: Some(serde_json::from_value(json!({"armed_at": armed_at})).unwrap()),
        ..TaskEntry::default()
    };
    write_loop_instances(
        &env,
        Tasks(BTreeMap::from([
            ("wait-reused".into(), entry.clone()),
            ("wait-failed".into(), entry),
        ])),
    );
    write_loop_run_records(
        &env,
        &[
            record(
                "wait-reused",
                &env.project_root,
                LoopRunResult::Delivered,
                30,
            ),
            record(
                "wait-reused",
                &env.project_root,
                LoopRunResult::SignalSkipped,
                20,
            ),
            record("wait-failed", &env.project_root, LoopRunResult::Failed, 30),
        ],
    );
    let runtime = env.runtime_paths();
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: armed_at,
    };
    let _watcher = hold_loop_run_lock(&runtime.lock_path("loop-watch-wait-reused.lock"), &holder);
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    assert!(
        text.contains("watching 12m")
            && text.contains("watcher gone")
            && text.contains("NEEDS YOU")
            && !text.contains("failing"),
        "{text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    for row in value["rooms"][0]["tasks"].as_array().unwrap() {
        assert!(
            row.get("last").is_none() && row.get("heard").is_none(),
            "{row}"
        );
    }
    let mut current = record(
        "wait-reused",
        &env.project_root,
        LoopRunResult::Delivered,
        0,
    );
    current.at = armed_at;
    write_loop_run_records(&env, &[current]);
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    let row = value["rooms"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "wait-reused")
        .unwrap();
    assert_eq!(row["last"]["result"], "delivered");
}

#[cfg(unix)]
#[test]
fn stop_cancels_every_live_run_before_failing_on_one_that_would_not_cancel() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    env.record(&env.project_root);
    let runs = [
        seed_task_run(&env, "dup", &env.project_root, None),
        seed_task_run(&env, "dup", &env.project_root, None),
    ];
    let id = |home: &PathBuf| {
        home.parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    for home in &runs {
        std::fs::write(
            home.with_file_name("workflow.toml"),
            "[workflow]\nscope='worktree'\nstart='agent'\n[test.agent]\nshell='true'\non={pass='@done',fail='@failed'}\n",
        )
        .unwrap();
    }
    let wrapper = env.home_root.join("rimz-refusing");
    let refused = env.home_root.join("refused");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ ! -e {refused} ]; then : > {refused}; echo 'pane stayed open' >&2; exit 1; fi\nexec {} \"$@\"\n",
            env.rimz_bin().display(),
            refused = refused.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = env
        .rimz()
        .env("RIMZ_BIN", &wrapper)
        .args(["loop", "stop", "dup"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    let state = |home: &PathBuf| {
        let run = run_paths(&env, &env.project_root, &id(home).parse().unwrap());
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(run.record).unwrap()).unwrap();
        record["state"].as_str().unwrap().to_owned()
    };
    let (stuck, other) = match state(&runs[0]).as_str() {
        "active" => (id(&runs[0]), &runs[1]),
        _ => (id(&runs[1]), &runs[0]),
    };
    assert_eq!(
        state(other),
        "canceled",
        "the run after the refusal is still canceled: {stdout}{stderr}"
    );
    assert!(
        stdout.contains(&format!("loop `dup`: stopped · run {}", id(other))),
        "{stdout}"
    );
    assert!(
        stderr.contains(&format!(
            "loop `dup`: workflow run {stuck} not canceled: pane stayed open"
        )),
        "{stderr}"
    );
}

/// A no-room project with a routine clock, a one-off clock, and a signal task,
/// and a `systemctl` stand-in that exits `code` and logs each call. The timer's
/// unit files exist when `units` holds.
#[cfg(target_os = "linux")]
fn timer_fixture(code: i32, units: bool) -> (Env, PathBuf, PathBuf) {
    let env = Env::new();
    env.record(&env.project_root);
    let clock = task(&env.project_root);
    config(
        &env,
        BTreeMap::from([
            ("nightly".into(), clock.clone()),
            (
                "once".into(),
                TaskEntry {
                    every: None,
                    at: Some("23:59".into()),
                    ..clock.clone()
                },
            ),
            (
                "merged".into(),
                TaskEntry {
                    every: None,
                    signal: Some("pr.merged".into()),
                    ..clock
                },
            ),
        ]),
    );
    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "nightly".to_owned(),
            Timestamp::now() - SignedDuration::from_mins(10),
        )]),
    );
    let shims = env.home_root.join("timer-shims");
    let log = env.home_root.join("systemctl.log");
    write_path_shim(
        &shims,
        "systemctl",
        &format!("echo \"$@\" >> '{}'\nexit {code}", log.display()),
    );
    if units {
        let dir = env.config_root().join("systemd/user");
        std::fs::create_dir_all(&dir).unwrap();
        for unit in ["rimz-loop.timer", "rimz-loop.service"] {
            std::fs::write(dir.join(unit), "").unwrap();
        }
    }
    (env, shims, log)
}

/// Run `loop list` with the stand-in first on `PATH`; return stdout and the
/// number of timer status reads the invocation made.
#[cfg(target_os = "linux")]
fn list_with_timer(env: &Env, shims: &Path, log: &Path, args: &[&str]) -> (String, usize) {
    std::fs::write(log, "").unwrap();
    let output = env
        .rimz()
        .args(["loop", "list"])
        .args(args)
        .env("PATH", path_with_front(shims))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reads = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .filter(|line| *line == "--user is-active rimz-loop.timer")
        .count();
    (String::from_utf8(output.stdout).unwrap(), reads)
}

#[cfg(target_os = "linux")]
fn json_tasks(stdout: &str) -> (serde_json::Value, BTreeMap<String, serde_json::Value>) {
    let listed: serde_json::Value = serde_json::from_str(stdout).unwrap();
    let room = listed["rooms"][0].clone();
    let tasks = room["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| (task["name"].as_str().unwrap().to_owned(), task.clone()))
        .collect();
    (room, tasks)
}

/// Names the file the reader re-exec reads its rooms from and writes its readings to.
#[cfg(target_os = "linux")]
const CAUSE_READS: &str = "RIMZ_TEST_ATTENTION_CAUSE_READS";

/// Seed a workflow run of loop task `task` in the room at `root`, parked at `park` or
/// active and waiting as `wait` says.
#[cfg(target_os = "linux")]
fn seed_room_run(
    env: &Env,
    root: &Path,
    task: &str,
    park: Option<Timestamp>,
    wait: Option<serde_json::Value>,
) {
    let state = if park.is_some() { "parked" } else { "active" };
    seed_room_run_at(env, root, task, (state, Timestamp::now()), park, wait);
}

/// Seed a workflow run of loop task `task` in the room at `root`, in `state` since its
/// creation at `created_at`.
#[cfg(target_os = "linux")]
fn seed_room_run_at(
    env: &Env,
    root: &Path,
    task: &str,
    (state, created_at): (&str, Timestamp),
    park: Option<Timestamp>,
    wait: Option<serde_json::Value>,
) {
    let paths = env.state_path_for(root);
    let guard = rimz::disk::lock::WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
    let record: rimz::store::workflow::RunRecord = serde_json::from_value(json!({
        "version":rimz::store::workflow::VERSION, "id":rimz::ids::WorkflowRunId::new(), "workflow":"review",
        "task":task, "spec_dir":"/definitions/review", "spec_digest":"abc",
        "scope":{"kind":"worktree","path":root}, "project_root":root,
        "created_at":created_at, "entered_at":created_at, "node":"agent", "wait":wait,
        "state":state,
        "park":park.map(|at| json!({"reason":"user","text":"needs a look","at":at})),
        "visits":{}, "visit_grant":0, "deadline_fired":false, "next_seq":1,
    }))
    .unwrap();
    rimz::store::workflow::initialize(&paths, &record, b"spec", &guard).unwrap();
}

/// Every cause the D9 test leaves out, each in a room of its own, read by `loop list` and
/// by the library reader: both name the same cause with the same reason, and a running
/// task whose last fire failed has none on either, whether its run lock is held or a run
/// it started after the failure is still active. A newer run that executes reads running in
/// the list; one that waits keeps the failure in LAST. The reader runs in a re-exec of this
/// test under the fixture's home, since the machine records it reads (strikes, arming, the
/// start queue, the run log) resolve from the process's home.
#[cfg(target_os = "linux")]
#[test]
fn every_cause_reads_alike_in_list_and_reader() {
    if let Some(path) = std::env::var_os(CAUSE_READS) {
        return read_causes_in_child(Path::new(&path));
    }
    let env = Env::new();
    let old: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
    let wake = |root: &Path| TaskEntry {
        root: root.to_owned(),
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("claude"),
            session: "session".into(),
            handle: "@coder".into(),
        }),
        watch: Some(rimz::config::WatchSpec::Command("true".into())),
        ..TaskEntry::default()
    };
    let rooms = [
        "failing",
        "failing-strikes",
        "failing-exit",
        "strikes",
        "held-ticket",
        "held-wait",
        "parked-one",
        "parked-two",
        "parked-lost",
        "parked-failing",
        "lost",
        "blocked",
        "invalid",
        "running-failed",
        "running-each",
        "retrying",
        "retry-waiting",
        "retried",
        "retry-sibling",
        "retry-struck",
        "first-fire",
        "first-wait",
    ]
    .into_iter()
    .map(|name| {
        let root = env.home_root.join(name);
        env.record(&root);
        (name.to_owned(), root)
    })
    .collect::<BTreeMap<_, _>>();
    let mut runs = Vec::new();
    for (name, root) in &rooms {
        let mut entry = match name.as_str() {
            "lost" | "parked-lost" => wake(root),
            "invalid" => TaskEntry {
                every: Some("soon".into()),
                ..task(root)
            },
            "running-each" => TaskEntry {
                root: root.clone(),
                agent: Some("codex".into()),
                prompt: Some("review".into()),
                stay: true,
                each_worktree: true,
                when: Some(vec!["pr=merged".into()]),
                hold: Some("3m".into()),
                ..TaskEntry::default()
            },
            _ => task(root),
        };
        if matches!(name.as_str(), "failing" | "parked-failing") {
            entry.max_strikes = Some(0);
        }
        if name == "blocked" {
            write_project_config_at(root, "[tasks.blocked]\ncheck = \"true\"\nevery = \"1h\"\n");
            continue;
        }
        let path = env
            .state_path_for(root)
            .root
            .join("records/loop-instances.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::to_vec(&Tasks(BTreeMap::from([(name.clone(), entry)]))).unwrap(),
        )
        .unwrap();
        if name.starts_with("failing")
            || name.starts_with("running")
            || name.starts_with("retr")
            || name == "parked-failing"
        {
            let mut failed = record(name, root, LoopRunResult::Failed, 5);
            if name == "failing-exit" {
                failed.check = Some(
                    serde_json::from_value(json!({"code": 2, "timed_out": false, "output": ""}))
                        .unwrap(),
                );
            }
            runs.push(failed);
        }
        if name.starts_with("parked") {
            seed_room_run(&env, root, name, Some(old), None);
        }
        if name.starts_with("first") {
            // A first fire's only row is its launch, which is no result.
            let mut launched = record(name, root, LoopRunResult::Launched, 2);
            launched.run_id = Some(rimz::ids::WorkflowRunId::new().to_string());
            runs.push(launched);
        }
    }
    seed_room_run(&env, &rooms["parked-two"], "parked-two", Some(old), None);
    let minutes_ago = |minutes| Timestamp::now() - SignedDuration::from_mins(minutes);
    let mut created_at = BTreeMap::new();
    for (name, state, created) in [
        ("retrying", "active", 2),
        ("retry-waiting", "active", 2),
        ("retried", "failed", 3),
        ("retry-sibling", "active", 7),
        ("retry-struck", "active", 2),
        ("first-fire", "active", 2),
        ("first-wait", "active", 2),
    ] {
        let created = (state, minutes_ago(created));
        created_at.insert(name, created.1);
        // A deduping task's shell decline waits out its resident retry inside the run.
        let wait = matches!(name, "retry-waiting" | "first-wait").then(|| {
            json!({
                "node":"recheck-check", "outcome":"sleeping", "first":created.1,
                "last":created.1, "count":1, "recheck_at":null,
            })
        });
        seed_room_run_at(&env, &rooms[name], name, created, None, wait);
    }
    // The retry that ended failed reported its own failure after the first.
    runs.push(record(
        "retried",
        &rooms["retried"],
        LoopRunResult::Failed,
        1,
    ));
    write_loop_run_records(&env, &runs);
    let key = |name: &str| project_task_key(&rooms[name], name);
    std::fs::create_dir_all(loop_strikes_path(&env).parent().unwrap()).unwrap();
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([
            (key("failing-strikes"), 1_u32),
            (key("failing-exit"), 2),
            (key("retrying"), 1),
            (key("retried"), 2),
        ]))
        .unwrap(),
    )
    .unwrap();
    let struck = || Arming {
        enabled: false,
        at: Some(old),
        pause_until: None,
        strikes: Some(3),
    };
    std::fs::write(
        loop_arming_path(&env),
        serde_json::to_vec(&BTreeMap::from([
            (key("strikes"), struck()),
            (key("retry-struck"), struck()),
        ]))
        .unwrap(),
    )
    .unwrap();
    let queue = env.rimz_home().join("loops/throttle");
    std::fs::create_dir_all(&queue).unwrap();
    let pid = std::process::id();
    let enqueued_ms = old.as_millisecond();
    std::fs::write(
        queue.join(format!("{enqueued_ms:020}-{}", uuid::Uuid::from_u128(1))),
        serde_json::to_vec(&json!({
            "owner": {"pid": pid, "start": rimz::proc::process_start_token(pid)},
            "task": "held-ticket", "root": rooms["held-ticket"],
            "checkout": rooms["held-ticket"], "enqueued_ms": enqueued_ms,
            "state": "waiting", "reason": {"kind": "disk", "text": "free disk low at the queue"},
        }))
        .unwrap(),
    )
    .unwrap();
    seed_room_run(
        &env,
        &rooms["held-wait"],
        "held-wait",
        None,
        Some(json!({
            "node":"agent", "outcome":"capacity", "detail":"free disk low at the wait",
            "hold":"disk", "kinds":["claude"], "first":old, "last":old, "count":2,
            "recheck_at":null,
        })),
    );
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now(),
    };
    let lock = |name: &str, file: String| {
        let state = env.state_path_for(&rooms[name]);
        let runtime = rimz::RuntimePaths::for_state_under(&state, &env.runtime_root);
        hold_loop_run_lock(&runtime.lock_path(file), &holder)
    };
    let _bare = lock("running-failed", "loop-run-running-failed.lock".into());
    let checkout = WorkspaceId::from_project_root(&env.home_root.join("one"));
    let _each = lock(
        "running-each",
        format!("loop-run-running-each-{checkout}.lock"),
    );

    let reads = env.home_root.join("cause-reads.json");
    std::fs::write(&reads, serde_json::to_vec(&rooms).unwrap()).unwrap();
    let output = env
        .rimz_at(&std::env::current_exe().unwrap())
        .args([
            "--exact",
            "loop_schedule::list_tests::every_cause_reads_alike_in_list_and_reader",
            "--nocapture",
        ])
        .env(CAUSE_READS, &reads)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "the child must run exactly this test:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let read: BTreeMap<String, Option<(serde_json::Value, String)>> =
        serde_json::from_slice(&std::fs::read(&reads).unwrap()).unwrap();
    // `--all` finds a room by its loop rows, which a project task's room has none of.
    let blocked = ["--root", rooms["blocked"].to_str().unwrap()];
    let all = ["loop", "list", "--all"];
    let blocked = [&blocked[..], &["loop", "list"][..]].concat();
    let mut listed = BTreeMap::new();
    let mut text = String::new();
    for args in [&all[..], &blocked[..]] {
        let json: serde_json::Value =
            serde_json::from_str(&loop_ok(&env, &[args, &["--json"][..]].concat())).unwrap();
        listed.extend(
            json["rooms"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|room| room["tasks"].as_array().unwrap())
                .map(|row| (row["name"].as_str().unwrap().to_owned(), row.clone())),
        );
        text.push_str(&loop_ok(&env, args));
    }

    let expected: [(&str, Option<&str>, Option<&str>); 22] = [
        ("failing", Some("failing"), Some("✗ failing")),
        (
            "failing-strikes",
            Some("failing"),
            Some("✗ failing · 1/3 strikes"),
        ),
        (
            "failing-exit",
            Some("failing"),
            Some("✗ failing · 2/3 strikes · exit 2"),
        ),
        ("strikes", Some("strikes"), Some("disabled after 3 strikes")),
        (
            "held-ticket",
            Some("held"),
            Some("free disk low at the queue"),
        ),
        ("held-wait", Some("held"), Some("free disk low at the wait")),
        ("parked-one", Some("parked"), None),
        ("parked-two", Some("parked"), None),
        ("parked-lost", Some("parked"), None),
        ("parked-failing", Some("failing"), Some("✗ failing")),
        (
            "lost",
            Some("lost"),
            Some("watcher gone · will not wake @coder"),
        ),
        (
            "blocked",
            Some("blocked"),
            Some("blocked · project untrusted"),
        ),
        ("invalid", Some("invalid"), None),
        ("running-failed", None, None),
        ("running-each", None, None),
        ("retrying", None, None),
        ("retry-waiting", None, None),
        ("retried", Some("failing"), Some("✗ failing · 2/3 strikes")),
        (
            "retry-sibling",
            Some("failing"),
            Some("✗ failing · 0/3 strikes"),
        ),
        (
            "retry-struck",
            Some("strikes"),
            Some("disabled after 3 strikes"),
        ),
        ("first-fire", None, None),
        ("first-wait", None, None),
    ];
    for (name, cause, reason) in expected {
        let row = listed
            .get(name)
            .unwrap_or_else(|| panic!("{name}: not listed\n{text}"));
        assert_eq!(row["attention"].as_str(), cause, "{name}: {row}");
        let (read_cause, read_reason) = match &read[name] {
            Some((cause, reason)) => (cause.as_str(), reason.as_str()),
            None => (None, ""),
        };
        assert_eq!(read_cause, cause, "{name}: the reader and the list agree");
        if let Some(reason) = reason {
            assert_eq!(read_reason, reason, "{name}");
        }
        if cause.is_none() {
            continue;
        }
        assert!(
            text.lines().any(|line| {
                line.trim_start().starts_with(&format!("{name} ")) && line.contains(read_reason)
            }),
            "{name}: list prints the reader's reason `{read_reason}`\n{text}"
        );
    }
    // The executing retry reads as a lock-held run does, aged from the run's creation; the
    // waiting one keeps the failure it follows. A first fire's run reads the same, with no
    // result before it.
    for (name, running, last) in [
        ("retrying", true, "▸ running 2m"),
        ("retry-waiting", false, "✗ failed 5m ago"),
        ("first-fire", true, "▸ running 2m"),
        ("first-wait", false, "launched 2m ago"),
    ] {
        let row = &listed[name];
        assert_eq!(
            row["state"].as_str(),
            Some(if running { "running" } else { "waits_for_room" }),
            "{name}: {row}"
        );
        assert_eq!(
            row["running_since"],
            if running {
                json!(created_at[name])
            } else {
                serde_json::Value::Null
            },
            "{name}: {row}"
        );
        assert!(
            text.lines()
                .any(|line| line.trim_start().starts_with(&format!("{name} "))
                    && line.contains(last)),
            "{name}: LAST reads `{last}`\n{text}"
        );
    }
    for (name, more) in [
        ("parked-one", ""),
        ("parked-two", " (+1 more)"),
        ("parked-lost", ""),
    ] {
        let reason = read[name]
            .as_ref()
            .map(|(_, reason)| reason.as_str())
            .unwrap();
        assert!(
            reason.starts_with(if more.is_empty() {
                "1 run(s) parked: "
            } else {
                "2 run(s) parked: "
            }) && reason.contains("agent user: needs a look")
                && reason.ends_with(more),
            "{name}: {reason}"
        );
    }
    assert!(
        read["invalid"].as_ref().unwrap().1.starts_with("invalid: "),
        "{:?}",
        read["invalid"]
    );
}

/// `loop show` (text and `--json`), the rowless show, `loop logs` and `loop watch` answer
/// whether a task has a run in progress as `loop list` does, with no run lock held: a retry
/// or a first fire whose run executes reads running from the run's creation, one whose run
/// waits keeps its timing state, and a run created before the result, or parked, is no run
/// in progress. A consumed one-shot whose run executes reads running in DONE, in watch, and
/// in its rowless show.
#[cfg(target_os = "linux")]
#[test]
fn show_logs_and_watch_read_a_lock_free_run_as_list_does() {
    let env = Env::new();
    let root = env.project_root.clone();
    env.record(&root);
    seed_loop_watch_room(&env);
    let names = ["retry", "first", "first-wait", "sibling", "first-parked"];
    config(
        &env,
        names
            .into_iter()
            .map(|name| (name.to_owned(), task(&root)))
            .collect(),
    );
    let minutes_ago = |minutes| Timestamp::now() - SignedDuration::from_mins(minutes);
    let launched = |name: &str, minutes| {
        let mut launched = record(name, &root, LoopRunResult::Launched, minutes);
        launched.run_id = Some(rimz::ids::WorkflowRunId::new().to_string());
        launched
    };
    let mut consumed = launched("consumed", 4);
    consumed.kind = Some(TaskKind::OneOff);
    let mut ended = launched("consumed-ended", 6);
    ended.kind = Some(TaskKind::OneOff);
    write_loop_run_records(
        &env,
        &[
            record("retry", &root, LoopRunResult::Failed, 5),
            record("sibling", &root, LoopRunResult::Failed, 5),
            launched("first", 2),
            launched("first-wait", 2),
            launched("first-parked", 2),
            consumed,
            ended,
        ],
    );
    let waiting = json!({
        "node":"recheck-check", "outcome":"sleeping", "first":minutes_ago(2),
        "last":minutes_ago(2), "count":1, "recheck_at":null,
    });
    let mut created_at = BTreeMap::new();
    for (name, state, minutes, wait) in [
        ("retry", "active", 2, None),
        ("first", "active", 2, None),
        ("first-wait", "active", 2, Some(waiting)),
        ("sibling", "active", 7, None),
        ("first-parked", "parked", 2, None),
        ("consumed", "active", 4, None),
        ("consumed-ended", "failed", 6, None),
    ] {
        let created = minutes_ago(minutes);
        created_at.insert(name, created);
        let park = (state == "parked").then_some(created);
        seed_room_run_at(&env, &root, name, (state, created), park, wait);
    }

    let list: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--all", "--json"])).unwrap();
    let listed = list["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|room| room["tasks"].as_array().unwrap())
        .map(|row| (row["name"].as_str().unwrap().to_owned(), row.clone()))
        .collect::<BTreeMap<_, _>>();
    let text = loop_ok(&env, &["loop", "list", "--all"]);
    for name in [
        "retry",
        "first",
        "first-wait",
        "sibling",
        "first-parked",
        "consumed",
    ] {
        let running = matches!(name, "retry" | "first" | "consumed");
        let row = listed
            .get(name)
            .unwrap_or_else(|| panic!("{name}: not listed\n{list:#}"));
        assert_eq!(row["state"] == "running", running, "{name}: {row}");
        let show: serde_json::Value = if name == "consumed" {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&loop_ok(&env, &["loop", "show", name, "--json"])).unwrap()
        };
        if running {
            assert_eq!(
                row["running_since"],
                json!(created_at[name]),
                "{name}: {row}"
            );
            if name != "consumed" {
                assert_eq!(
                    show["running"],
                    json!({"pid": null, "started_at": row["running_since"], "run_id": null}),
                    "{name}"
                );
            }
        } else {
            assert_eq!(
                row["running_since"],
                serde_json::Value::Null,
                "{name}: {row}"
            );
            assert_eq!(show["running"], serde_json::Value::Null, "{name}");
        }
        if name == "consumed" {
            continue;
        }
        let show = loop_ok(&env, &["loop", "show", name]);
        let lines = show.lines().collect::<Vec<_>>();
        assert_eq!(
            lines[0].ends_with(" · ▸ running 2m"),
            running,
            "{name}:\n{show}"
        );
        assert!(!lines[0].contains("pid"), "{name}:\n{show}");
        assert_eq!(
            show.contains(&format!("stop with `rimz loop stop {name}`")),
            running,
            "{name}:\n{show}"
        );
        let verdict = match name {
            "retry" => Some("  ✗ last finished run 5m ago, failed"),
            "sibling" => Some("  ✗ failing · last run 5m ago, failed"),
            _ => None,
        };
        match verdict {
            Some(verdict) => assert!(lines[1].starts_with(verdict), "{name}:\n{show}"),
            None => assert!(
                !show.contains("healthy") && !show.contains("finished run"),
                "{name}: a first fire has no verdict\n{show}"
            ),
        }
        assert_eq!(
            show.contains("failing"),
            name == "sibling",
            "{name}:\n{show}"
        );
        let logs = loop_ok(&env, &["loop", "logs", name]);
        assert_eq!(
            logs.trim_end().ends_with("▸ running 2m"),
            running,
            "{name}:\n{logs}"
        );
    }
    // The consumed one-shot's executing run reads running in DONE and its rowless show; the
    // one whose run ended reads as it always did.
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("consumed ") && line.contains("▸ running 4m")),
        "{text}"
    );
    assert!(
        text.lines().any(|line| {
            line.trim_start().starts_with("consumed-ended ") && !line.contains("▸ running")
        }),
        "{text}"
    );
    let show = loop_ok(&env, &["loop", "show", "consumed"]);
    assert!(
        show.starts_with(
            "consumed — one-shot fired · ▸ running 4m\n  stop with `rimz loop stop consumed`\n\n"
        ),
        "{show}"
    );
    let show = loop_ok(&env, &["loop", "show", "consumed-ended"]);
    assert!(
        show.lines().next().is_some_and(|line| line
            .starts_with("consumed-ended — one-shot fired · ")
            && !line.contains("▸ running")),
        "{show}"
    );
    let watch = loop_watch_frames(&env, &["consumed", "▸ running 4m"]);
    assert!(
        watch
            .lines()
            .any(|line| line.contains("consumed") && line.contains("▸ running 4m")),
        "{watch}"
    );
}

/// A row edited mid-run so it no longer compiles to a workflow (here `workflow` beside
/// `agent`, or an unguarded `--wait`) still has the run it started: `loop show` text and
/// `--json` and `loop logs` read it as `loop list` does.
#[cfg(target_os = "linux")]
#[test]
fn a_row_edited_mid_run_reads_its_run_alike_in_list_show_and_logs() {
    let env = Env::new();
    let root = env.project_root.clone();
    env.record(&root);
    let invalid = TaskEntry {
        workflow: Some("review".into()),
        agent: Some("claude".into()),
        ..task(&root)
    };
    let wait = TaskEntry {
        check: None,
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("codex"),
            session: "session".into(),
            handle: "coder".into(),
        }),
        ..task(&root)
    };
    config(
        &env,
        BTreeMap::from([("invalid".to_owned(), invalid), ("wait".to_owned(), wait)]),
    );
    let created = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_run_records(
        &env,
        &["invalid", "wait"].map(|name| record(name, &root, LoopRunResult::Failed, 5)),
    );
    for name in ["invalid", "wait"] {
        seed_room_run_at(&env, &root, name, ("active", created), None, None);
    }
    let list: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--all", "--json"])).unwrap();
    for name in ["invalid", "wait"] {
        let row = list["rooms"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|room| room["tasks"].as_array().unwrap())
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("{name}: not listed\n{list:#}"));
        assert_eq!(row["state"], "running", "{name}: {row}");
        let show: serde_json::Value =
            serde_json::from_str(&loop_ok(&env, &["loop", "show", name, "--json"])).unwrap();
        assert_eq!(
            show["running"],
            json!({"pid": null, "started_at": row["running_since"], "run_id": null}),
            "{name}: {row}"
        );
        let text = loop_ok(&env, &["loop", "show", name]);
        assert!(
            text.lines().next().unwrap().ends_with(" · ▸ running 2m"),
            "{name}:\n{text}"
        );
        assert!(!text.contains("failing"), "{name}:\n{text}");
        let logs = loop_ok(&env, &["loop", "logs", name]);
        assert!(logs.trim_end().ends_with("▸ running 2m"), "{name}:\n{logs}");
    }
}

/// The re-exec half: read each room's task through the library reader, with the run-log
/// stats of the library's own fold over the same log.
#[cfg(target_os = "linux")]
fn read_causes_in_child(path: &Path) {
    use rimz::harness::schedule::{attention, run_log};
    let rooms: BTreeMap<String, PathBuf> =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let home = PathBuf::from(std::env::var_os("RIMZ_HOME").unwrap());
    let runtime_root = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap());
    let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    let reads = rooms
        .iter()
        .map(|(name, root)| {
            let state = rimz::StatePaths::for_project_root_under(root, &home).unwrap();
            let runtime = rimz::RuntimePaths::for_state_under(&state, &runtime_root);
            let stats = run_log::stats(&home.join("logs"), &now, Some(root));
            let read = attention::read_room(
                root,
                &state,
                &runtime,
                attention::ReadContext {
                    open: false,
                    stats: &stats,
                    now: &now,
                },
            )
            .remove(name)
            .unwrap_or_else(|| panic!("the reader finds {name}"));
            let cause = read
                .attention
                .map(|(cause, reason)| (serde_json::to_value(cause).unwrap(), reason));
            (name.clone(), cause)
        })
        .collect::<BTreeMap<_, _>>();
    std::fs::write(path, serde_json::to_vec(&reads).unwrap()).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn active_timer_shows_closed_room_clocks_as_live() {
    let (env, shims, log) = timer_fixture(0, true);
    let (text, text_reads) = list_with_timer(&env, &shims, &log, &[]);
    assert!(text.contains("no room open"), "{text}");
    assert!(text.contains("clocks use the active timer"), "{text}");
    assert_eq!(text.matches("waits for room").count(), 0, "{text}");
    let nightly = text
        .lines()
        .find(|line| line.trim_start().starts_with("nightly"))
        .unwrap();
    assert!(!nightly.contains('—'), "{text}");
    let (json, json_reads) = list_with_timer(&env, &shims, &log, &["--json"]);
    let (room, tasks) = json_tasks(&json);
    assert_eq!(room["open"], false, "{json}");
    for name in ["nightly", "once", "merged"] {
        let task = &tasks[name];
        assert_eq!(task["state"], "live", "{json}");
        assert!(
            !task["trigger"].as_str().unwrap().contains("waits for room"),
            "{json}"
        );
    }
    assert!(tasks["nightly"]["next_at"].is_string(), "{json}");
    assert_eq!((text_reads, json_reads), (1, 1));
}

#[cfg(target_os = "linux")]
#[test]
fn clocks_wait_for_the_room_without_an_active_timer() {
    for (code, units) in [(3, true), (0, false)] {
        let (env, shims, log) = timer_fixture(code, units);
        let (text, text_reads) = list_with_timer(&env, &shims, &log, &[]);
        if !units {
            assert_eq!(std::fs::read_to_string(&log).unwrap(), "", "{text}");
        }
        assert!(
            text.contains("clocks and CI/PR signals wait until"),
            "{text}"
        );
        for name in ["nightly", "once"] {
            let row = text
                .lines()
                .find(|line| line.trim_start().starts_with(name))
                .unwrap();
            assert_eq!(row.matches("waits for room").count(), 1, "{text}");
        }
        let (json, json_reads) = list_with_timer(&env, &shims, &log, &["--json"]);
        assert_eq!(
            (text_reads, json_reads),
            (usize::from(units), usize::from(units))
        );
        let (_, tasks) = json_tasks(&json);
        assert_eq!(tasks["nightly"]["state"], "waits_for_room", "{json}");
        assert_eq!(tasks["once"]["state"], "waits_for_room", "{json}");
        assert_eq!(tasks["merged"]["state"], "live", "{json}");
        if !units {
            assert_eq!(std::fs::read_to_string(&log).unwrap(), "");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn open_room_lists_clocks_without_reading_the_timer() {
    let (env, shims, log) = timer_fixture(0, true);
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "list-timer",
    );
    let (text, reads) = list_with_timer(&env, &shims, &log, &[]);
    assert!(
        text.contains("room open") && !text.contains("no room open"),
        "{text}"
    );
    assert_eq!(reads, 0, "{text}");
    let (json, reads) = list_with_timer(&env, &shims, &log, &["--json"]);
    let (room, tasks) = json_tasks(&json);
    assert_eq!(room["open"], true, "{json}");
    assert_eq!(tasks["nightly"]["state"], "live", "{json}");
    assert_eq!(tasks["once"]["state"], "live", "{json}");
    assert_eq!(reads, 0);
}

/// D9's two rules, read by `loop list` and by the library reader on the same room: a
/// booking waiting on a closed pull request needs you, and a fired booking whose checkout
/// is gone is done.
#[cfg(target_os = "linux")]
#[test]
fn closed_pull_requests_and_fired_gone_checkouts_read_alike_in_list_and_reader() {
    let env = Env::new();
    env.record(&env.project_root);
    let state = env.state_path_for(&env.project_root);
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "list-d9",
    );
    let booking = |when: &str, dir: Option<PathBuf>| TaskEntry {
        root: env.project_root.clone(),
        agent: Some("claude".into()),
        prompt: Some("start the next stage".into()),
        stay: true,
        once: Some(true),
        when: Some(vec![when.into()]),
        dir,
        ..TaskEntry::default()
    };
    let fired_checkout = env.home_root.join("fired-checkout");
    let tasks = BTreeMap::from([
        ("closed-x".to_owned(), booking("pr[x]=merged", None)),
        (
            "either-xy".to_owned(),
            booking("pr[x]=merged || pr[y]=merged", None),
        ),
        (
            "fired-gone".to_owned(),
            booking("pr[y]=merged", Some(fired_checkout.clone())),
        ),
        (
            "unfired-gone".to_owned(),
            booking("pr[y]=merged", Some(env.home_root.join("never-made"))),
        ),
    ]);
    let records = state.root.join("records");
    std::fs::create_dir_all(&records).unwrap();
    std::fs::write(
        records.join("loop-instances.json"),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    std::fs::write(
        records.join("loop-workflow-launches.json"),
        serde_json::to_vec(&json!({"version": 2, "tasks": {"fired-gone": {
            fired_checkout.to_str().unwrap(): {"at": Timestamp::now(), "scope": "worktree"},
        }}}))
        .unwrap(),
    )
    .unwrap();
    let stamps = runtime.lane_path("loop-fire.json");
    std::fs::create_dir_all(stamps.parent().unwrap()).unwrap();
    std::fs::write(
        &stamps,
        serde_json::to_vec(
            &tasks
                .keys()
                .map(|name| (name.clone(), Timestamp::UNIX_EPOCH))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap(),
    )
    .unwrap();
    let pr = |state: &str| json!({"pr": {"state": state, "number": 1640}});
    std::fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
    std::fs::write(
        runtime.lane_path("pr-state.json"),
        serde_json::to_vec(&json!({
            "branch_ci": {},
            "path_repos": {env.project_root.to_str().unwrap(): "repo"},
            "repos": {"repo": {"ok": true, "refreshed_at_ms": 1_000}},
            "branches": {"repo": {"x": pr("closed"), "y": pr("open")}},
        }))
        .unwrap(),
    )
    .unwrap();

    let (_, listed) = json_tasks(&loop_ok(&env, &["loop", "list", "--json"]));
    let text = loop_ok(&env, &["loop", "list"]);

    let expected = [
        ("closed-x", Some("pr_closed"), "needs_you"),
        ("either-xy", None, "waiting"),
        ("fired-gone", None, "done"),
        ("unfired-gone", Some("checkout_gone"), "needs_you"),
    ];
    let now = Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC);
    let mut reads = rimz::harness::schedule::attention::read_room(
        &env.project_root,
        &state,
        &runtime,
        rimz::harness::schedule::attention::ReadContext {
            open: true,
            stats: &BTreeMap::new(),
            now: &now,
        },
    );
    for (name, attention, section) in expected {
        let row = &listed[name];
        assert_eq!(
            (row["attention"].as_str(), row["section"].as_str()),
            (attention, Some(section)),
            "{name}: {row}"
        );
        let read = reads.remove(name).expect("the reader finds the task");
        assert_eq!(
            read.attention
                .map(|(cause, _)| serde_json::to_value(cause).unwrap()),
            attention.map(|cause| json!(cause)),
            "{name}: the reader and the list agree"
        );
        assert_eq!(read.fired, name == "fired-gone", "{name}");
    }
    assert!(
        text.contains("pr[x] closed without merging")
            && text.contains("checkout never-made is gone")
            && text.contains("→ rimz loop remove closed-x")
            && text.contains("→ rimz loop remove unfired-gone"),
        "{text}"
    );
    assert!(!text.contains("fired-checkout is gone"), "{text}");
}

/// The booking chain `loop add` stored for `name`, or `None` with no row.
fn stored_chain(env: &Env, name: &str) -> Option<Vec<String>> {
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(env)).unwrap()).unwrap();
    let entry = config.tasks.0.get(name)?;
    assert_eq!(entry.resolved_root(), env.project_root, "{name}: workspace");
    Some(
        entry
            .booked_by
            .iter()
            .map(|link| link.agent_id.to_string())
            .collect(),
    )
}

fn add_as(env: &Env, caller: Option<&str>, args: &[&str]) {
    let mut command = match caller {
        Some(id) => calling_loop(env, id),
        None => env.rimz(),
    };
    let output = command.args(["loop", "add"]).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_agent_booking_records_its_chain_and_shows_its_booker() {
    let env = Env::new();
    env.record(&env.project_root);
    seed_agent_launch(&env, &env.project_root, "lead", Default::default(), None);
    let coder = rimz::agents::LaunchParams {
        launched_by: Some(Box::new(rimz::agents::LaunchedBy {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("launch_lead"),
        })),
        ..Default::default()
    };
    seed_agent_launch(&env, &env.project_root, "coder", coder, None);
    let check = ["--check", "true", "--every", "1h"];
    let booking = |name: &'static str| [&[name][..], &check[..]].concat();

    add_as(&env, Some("launch_coder"), &booking("booked"));
    add_as(&env, None, &booking("shell"));
    add_as(&env, Some("launch_ghost"), &booking("stale"));
    add_as(&env, Some("launch_coder"), &booking("readded"));
    add_as(&env, None, &booking("readded"));
    let mut hook = env.hook_command("claude");
    hook.current_dir(&env.project_root)
        .env("RIMZ_AGENT_ID", "launch_coder")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "coder");
    let registered = env
        .spawn_payload(
            hook,
            &json!({"hook_event_name": "SessionStart", "session_id": "coder", "cwd": env.project_root})
                .to_string(),
        )
        .wait_with_output()
        .unwrap();
    env.drain_hooks();
    assert!(
        registered.status.success(),
        "{}",
        String::from_utf8_lossy(&registered.stderr)
    );
    add_as(
        &env,
        Some("launch_coder"),
        &["delivery", "--wait", "--in", "1h"],
    );
    add_as(
        &env,
        Some("launch_coder"),
        &[&booking("shared")[..], &["--project"][..]].concat(),
    );

    let delivery = read_loop_instances(&env).0.remove("delivery");
    let delivery = delivery
        .or_else(|| {
            let config: LoopConfig =
                toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
            config.tasks.0.get("delivery").cloned()
        })
        .expect("the delivery's row");
    assert_eq!(delivery.booked_by, Vec::new(), "a delivery is not stamped");
    let project: toml::Value = toml::from_str(
        &std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap(),
    )
    .unwrap();
    let shared = &project["tasks"]["shared"];
    assert!(
        shared.get("check").is_some() && shared.get("booked-by").is_none(),
        "a project task is not stamped: {project}"
    );
    assert_eq!(stored_chain(&env, "shared"), None, "{project}");

    assert_eq!(
        stored_chain(&env, "booked"),
        Some(vec!["launch_coder".to_owned(), "launch_lead".to_owned()])
    );
    for name in ["shell", "stale", "readded"] {
        assert_eq!(stored_chain(&env, name), Some(Vec::new()), "{name}");
    }
    let text = loop_ok(&env, &["loop", "show", "booked"]);
    let booker = text
        .lines()
        .find(|line| line.trim_start().starts_with("booked by"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(booker.contains("@coder"), "{text}");
    assert!(
        !loop_ok(&env, &["loop", "show", "shell"]).contains("booked by"),
        "a shell booking has no booker"
    );
    let chain = json!([
        {"kind": "claude", "agent_id": "launch_coder"},
        {"kind": "claude", "agent_id": "launch_lead"},
    ]);
    let show: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "booked", "--json"])).unwrap();
    assert_eq!(show["booked_by"], chain);
    let list: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    let rows = list["rooms"][0]["tasks"]
        .as_array()
        .unwrap_or_else(|| panic!("{list}"));
    for row in rows {
        let expected = if row["name"] == "booked" {
            &chain
        } else {
            &serde_json::Value::Null
        };
        assert_eq!(&row["booked_by"], expected, "{row}");
    }

    let other = env.home_root.join("other");
    env.record(&other);
    let other_root = other.to_str().unwrap();
    add_as(
        &env,
        Some("launch_coder"),
        &[&booking("elsewhere")[..], &["--root", other_root][..]].concat(),
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    assert_eq!(
        config.tasks.0["elsewhere"].booked_by,
        Vec::new(),
        "a booking in another room is not stamped"
    );
}
