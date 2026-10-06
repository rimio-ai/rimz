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
fn acting_history_and_heard_are_not_skips() {
    let env = Env::new();
    config(
        &env,
        ["acted", "heard"]
            .map(|name| (name.into(), task(&env.project_root)))
            .into(),
    );
    write_loop_run_records(
        &env,
        &[
            record("acted", &env.project_root, LoopRunResult::Completed, 10),
            record("acted", &env.project_root, LoopRunResult::SignalSkipped, 7),
            record("acted", &env.project_root, LoopRunResult::SignalSkipped, 6),
            record("acted", &env.project_root, LoopRunResult::SignalSkipped, 5),
            record("heard", &env.project_root, LoopRunResult::SignalSkipped, 3),
        ],
    );
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(text.contains("✓ 10m ago"), "{text}");
    assert!(
        text.contains("heard ci.passed 3m ago") && !text.contains("skipped"),
        "{text}"
    );
    assert!(loop_ok(&env, &["loop", "show", "acted"]).contains("healthy"));
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
        text.contains("no runs recorded") && !text.contains("LAST RUN"),
        "{text}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "wait-reused", "--json"])).unwrap();
    assert!(value["runs"].as_array().unwrap().is_empty(), "{value}");
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
        text.contains("WORKTREES") && !text.contains("NEEDS YOU"),
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
    assert_eq!(text.matches("feature").count(), 1, "{text}");
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    for row in value["rooms"][0]["tasks"].as_array().unwrap() {
        assert_eq!(row["dir"], checkout.to_str().unwrap(), "{row}");
        assert_eq!(row["section"], "worktrees", "{row}");
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
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("on ci.failed pr.{conflicted,dequeued,merged}"),
        "{text}"
    );
    assert_eq!(text.matches("↳ sweep").count(), 1, "{text}");
    assert!(
        text.contains("✓ 5m ago") && !text.contains("heard ci.passed"),
        "{text}"
    );
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
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("user-one") && text.contains("user-two") && text.contains("6 tasks"),
        "{text}"
    );
}

#[test]
fn subscriptions_only_collapse_uniform_states_and_suffixes() {
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
    let text = loop_ok(&env, &["loop", "list"]);
    assert_eq!(text.matches("↳ sweep").count(), 4, "{text}");
    assert!(
        text.lines()
            .any(|line| line.contains("on ci.signal0") && line.contains("off")),
        "{text}"
    );
    assert!(
        text.lines().any(|line| line.contains("on ci.signal1")
            && !line.contains("paused")
            && !line.contains("gate docs")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.contains("on ci.signal2 · paused, resumes in")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.contains("on ci.signal3 · gate docs")),
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
        text.contains("no loop tasks in") && text.contains("1 in other rooms"),
        "{text}"
    );
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
        assert_eq!(row["section"], "room");
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
fn watch_label_lost_and_caller_markers_are_enrichment() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
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
            && text.contains("lost")
            && !text.contains("NEEDS YOU")
            && !text.contains("private command"),
        "{text}"
    );
    let runtime = env.runtime_paths();
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_mins(12),
    };
    let _watcher = hold_loop_run_lock(&runtime.lock_path("loop-watch-wait-docs.lock"), &holder);
    let text = loop_ok(&env, &["loop", "list"]);
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
    let text = loop_ok(&env, &["loop", "list"]);
    assert!(
        text.contains("watching 12m") && text.contains("lost") && !text.contains("NEEDS YOU"),
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
