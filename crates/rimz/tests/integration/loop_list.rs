//! Room-list behavior at the CLI boundary.

use super::*;
use rimz::harness::schedule::run_log::SignalRecord;

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
        .env("RIMZ_AGENT_ID", "caller")
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
