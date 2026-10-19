//! Room-list behavior at the CLI boundary.

use super::*;
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
        &[record("retired", &other, LoopRunResult::Completed, 1)],
    );
    let text = loop_ok(&env, &["loop", "show", "retired"]);
    assert!(text.contains("completed"), "{text}");
    assert!(
        text.contains(&format!("other commands need --root {}", other.display())),
        "{text}"
    );
    let json = env
        .rimz()
        .args(["loop", "show", "retired", "--json"])
        .output()
        .unwrap();
    assert!(!json.status.success());
    assert!(json.stdout.is_empty());
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
    use rimz::harness::schedule::TaskKind;
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
    assert!(text.contains("2 wake agents · all healthy"), "{text}");
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
    assert!(text.contains("1 wake agents · 1 need you"), "{text}");
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
    assert!(
        show.contains("RECENT RUNS (newest first · 1 of 1)"),
        "{show}"
    );
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
        text.contains("not fired yet") && !text.contains("loop fire") && !text.contains("LAST RUN"),
        "{text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "wait-reused", "--json"])).unwrap();
    assert!(value["runs"].as_array().unwrap().is_empty(), "{value}");
    let logs = loop_ok(&env, &["loop", "logs", "wait-reused", "--heard"]);
    assert!(
        logs.contains("not fired yet") && !logs.contains("loop fire"),
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
    assert!(folded.contains("4 wake agents · all healthy"), "{folded}");
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
