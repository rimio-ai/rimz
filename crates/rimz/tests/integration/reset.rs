//! Integration coverage for `rimz reset`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::agents::PermissionMode;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::harness::run_wake::{ExpectedRunFrame, RunWaiter};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::store::event::EventEnvelope;
use rimz::store::run::{RunRecord, RunStatus};

use crate::common::{CommandTimeoutExt, Env};

#[cfg(unix)]
#[test]
fn reset_preserves_the_pre_teardown_roster_after_the_producer_exits() {
    let env = Env::new();
    let store = env.store();
    let paths = store.paths();
    rimz::store::live_roster::publish(
        &paths.live_roster,
        [(
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("sess-reset"),
        )]
        .into_iter()
        .collect(),
    )
    .expect("seed roster");
    let before: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.live_roster).unwrap()).unwrap();
    let mut empty = before.clone();
    empty["agents"] = serde_json::json!([]);
    let shim_dir = env.home_root.join("mux-bin");
    crate::common::write_path_shim(
        &shim_dir,
        "zellij",
        r#"
case "$1" in
    delete-session)
        printf '%s' "$EMPTY_ROSTER" > "$ROSTER"
        sh -c '
            trap '\''printf "%s" "$EMPTY_ROSTER" > "$ROSTER"; : > "$AFTER_KILL"; exit 0'\'' TERM
            echo $$ > "$PRODUCER_PID"
            while :; do :; done
        ' sidebar "$WORKSPACE_ID" "$2" </dev/null >/dev/null 2>&1 &
        while [ ! -s "$PRODUCER_PID" ]; do sleep 0.01; done
        ;;
esac
"#,
    );
    let after_kill = env.home_root.join("after-kill");
    let producer_pid = env.home_root.join("producer-pid");
    let path = std::env::join_paths(
        std::iter::once(shim_dir).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    env.rimz()
        .env("PATH", path)
        .env("ROSTER", &paths.live_roster)
        .env("EMPTY_ROSTER", empty.to_string())
        .env("WORKSPACE_ID", env.workspace_id.as_str())
        .env("AFTER_KILL", &after_kill)
        .env("PRODUCER_PID", &producer_pid)
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success();

    assert!(
        after_kill.exists(),
        "producer published after the mux kill returned"
    );
    let pid = fs::read_to_string(producer_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!rimz::proc::process_is_live(pid, None), "producer is gone");
    // Rebirth reads this persisted roster, not the reset command's report.
    let recovered: serde_json::Value =
        serde_json::from_slice(&fs::read(&paths.live_roster).unwrap()).unwrap();
    assert_eq!(
        recovered["agents"], before["agents"],
        "rebirth must see the pre-teardown roster"
    );
}

#[test]
fn reset_refuses_a_held_old_layout_room_even_with_its_sidebar_heartbeat() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &workspace.session_name,
        "old-room",
    );
    fs::create_dir_all(&paths.root).unwrap();
    let record = serde_json::to_vec(&serde_json::json!({
        "workspace_id": workspace.workspace_id,
        "project_root": workspace.project_root,
        "session_name": workspace.session_name,
        "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    fs::write(&paths.workspace_record, &record).unwrap();
    let _held = rimz::disk::lock::RoomLock::hold(&runtime.room_lock()).unwrap();
    let trace = env.project_root.join("reset-zellij.log");

    for confirm in [vec!["--yes"], vec![]] {
        let output = env
            .rimz()
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
            .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
            .args(["--mux", "zellij", "reset", "--no-start"])
            .args(confirm)
            .bounded_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "held room must refuse: {stderr}");
        assert!(
            stderr.contains(
                "already held by another running room (another multiplexer, or a renamed session)"
            ),
            "refusal must precede confirmation: {stderr}"
        );
        assert!(stderr.contains(&workspace.session_name), "{stderr}");
        assert!(!stderr.contains("older RimZ"), "{stderr}");
        assert!(!stderr.contains("Room torn down"), "{stderr}");
        assert!(paths.root.exists());
        assert!(runtime.root.exists());
        assert_eq!(fs::read(&paths.workspace_record).unwrap(), record);
    }
    assert!(!trace.exists(), "refusal must precede any backend call");
}

#[test]
fn reset_replaces_an_old_layout_room_without_starting() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    let runtime = env.runtime_paths();
    fs::create_dir_all(&runtime.root).unwrap();
    fs::create_dir_all(paths.root.join("messages")).unwrap();
    fs::write(
        &paths.workspace_record,
        serde_json::to_vec(&serde_json::json!({
            "workspace_id": workspace.workspace_id,
            "project_root": workspace.project_root,
            "session_name": workspace.session_name,
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(paths.root.join("events.log.jsonl"), b"old history").unwrap();
    fs::write(paths.root.join("messages/messages.jsonl"), b"old messages").unwrap();
    let notice = format!(
        "rimz: room {} was written by an older RimZ (layout 1); it was torn down and this project starts with a fresh room. Its history was not carried over.",
        paths.dir_name
    );
    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success()
        .stderr(contains(notice))
        .stderr(contains("Room torn down"));
    assert!(!paths.root.exists());
    assert!(!runtime.root.exists());
}

/// `rimz reset --no-start --yes` deletes the room's serialized-session cache and
/// reports what it removed, without trying to rebirth or attach. `--mux zellij`
/// forces the Zellij backend so the cache purge runs regardless of which mux the
/// host has installed; the purge itself is filesystem-only.
#[test]
fn reset_purges_the_resurrection_cache() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);

    // Plant a serialized-session cache the way Zellij would, under HOME/.cache;
    // the harness pins XDG_CACHE_HOME to that disposable fallback path.
    let session_info = env
        .home_root
        .join(".cache/zellij/contract_version_1/session_info");
    fs::create_dir_all(&session_info).expect("mkdir cache");
    let cache_entry = session_info.join(&workspace.session_name);
    fs::write(&cache_entry, b"serialized").expect("write cache");

    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success()
        .stderr(contains("cache entr"))
        .stderr(contains("Run `rimz start`"));

    assert!(
        !cache_entry.exists(),
        "reset should purge the serialized-session cache"
    );
}

#[test]
fn reset_archives_records_and_clears_disposable_classes() {
    let env = Env::new();
    let store = env.store();
    store
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &agent_observation(&env.project_root),
        ))
        .expect("append lifecycle");

    let paths = env.state_path_for(&env.project_root);
    assert!(!paths.tmp_dir.exists(), "host store creates no tmp");
    assert!(
        !paths.skills_dir.exists(),
        "host store creates no skill copies"
    );
    fs::create_dir_all(&paths.skills_dir).expect("skills dir");
    let skill_copy = paths.skills_dir.join("skill-digest");
    fs::create_dir(&skill_copy).expect("skill copy dir");
    fs::write(skill_copy.join("SKILL.md"), b"user-only skill").expect("write skill copy");
    let unit = paths.ensure_temp_unit(None).expect("tmp dir");
    fs::write(unit.join("agent-work"), b"tmp").expect("write tmp");
    paths
        .ensure_temp_unit(None)
        .expect("tmp ensure is idempotent");
    assert_eq!(fs::read(unit.join("agent-work")).expect("read tmp"), b"tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&unit)
                .expect("tmp metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let diag = rimz::diag::DiagSink::under(
        paths.root.clone(),
        env.workspace_id.clone(),
        "rimz-test",
        None,
    );
    let diag_log = diag.log_path().unwrap();
    let diag_frames = rimz::diag::frames_dir_under(&paths.root);
    fs::create_dir_all(&diag_frames).expect("mkdir diag frames");
    fs::write(&diag_log, b"diag\n").expect("write diag");
    fs::write(diag_frames.join("frame.1.0.test.json"), b"{}").expect("write frame");

    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("mkdir runtime");
    fs::write(paths.audit_path("binding.log.jsonl"), b"binding\n").expect("write binding");

    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success()
        .stderr(contains("Records: archived"))
        .stderr(contains("prior agent rollup kept"));

    assert!(paths.workspace_record.exists(), "workspace identity stays");
    assert!(!paths.events_log.exists(), "active log was archived");
    for audit in [diag_log, diag_frames, paths.audit_path("binding.log.jsonl")] {
        assert!(
            audit.exists(),
            "soft reset keeps the audit class: {audit:?}"
        );
    }
    for class_dir in [&runtime.sock_dir, &runtime.live_dir, &runtime.lanes_dir] {
        assert!(!class_dir.exists(), "disposable runtime class cleared");
    }
    assert!(runtime.locks_dir.exists(), "runtime locks survive reset");
    assert!(
        unit.join("agent-work").exists(),
        "soft reset keeps the temp unit"
    );
    assert!(
        !paths.skills_dir.exists(),
        "soft reset clears handleless cached skill copies"
    );

    let archives = archive_paths(&paths.events_archive_dir);
    assert_eq!(archives.len(), 1, "one reset archive written");
    let archived = rimz::store::event_log::read_all(&archives[0]).expect("read archive");
    assert!(
        archived
            .iter()
            .any(|event| event.method == "agent.lifecycle")
    );

    let projection = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("projection");
    assert_eq!(projection.agents.len(), 1, "soft reset keeps resume rollup");

    let hard = Env::new();
    hard.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            hard.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &agent_observation(&hard.project_root),
        ))
        .expect("append lifecycle");
    let paths = hard.state_path_for(&hard.project_root);
    hard.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes", "--hard"])
        .assert()
        .success()
        .stderr(contains("prior agent rollup cleared"));

    assert!(
        !paths.agents_carryover.exists(),
        "hard reset clears carryover"
    );
    assert_eq!(archive_paths(&paths.events_archive_dir).len(), 1);
    let projection = hard
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("projection");
    assert!(projection.agents.is_empty(), "hard reset starts blank");
}

#[test]
fn reset_cancels_active_runs_and_wakes_waiters() {
    reset_cancels_runs(false);
}

#[test]
fn hard_reset_cancels_active_runs_and_wakes_waiters() {
    reset_cancels_runs(true);
}

fn reset_cancels_runs(hard: bool) {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let store = env.store();
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "ship it".to_owned(),
        env.project_root.clone(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");
    let waiter = RunWaiter::bind(
        store.runtime_paths(),
        ExpectedRunFrame {
            workspace_id: env.workspace_id.clone(),
            run_id: run_id.clone(),
        },
        rimz::harness::run::RunCancellation::new(),
    )
    .expect("bind run socket");

    let mut command = env.rimz();
    command.args(["--mux", "zellij", "reset", "--no-start", "--yes"]);
    if hard {
        command.arg("--hard");
    }
    command
        .assert()
        .success()
        .stderr(contains("canceled 1 run"));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let terminal = runtime
        .block_on(waiter.wait_terminal(&store, Some(Duration::from_secs(1)), None))
        .expect("wait for run wakeup");
    assert_eq!(terminal.status, RunStatus::Canceled);

    let after = rimz::harness::run::load(store.paths(), &run_id).expect("load run");
    assert_eq!(after.status, RunStatus::Canceled);
}

/// Without a terminal to confirm and without `--yes`, `rimz reset` refuses rather
/// than destroying a session unattended — the fail-fast-with-the-fix contract.
#[test]
fn reset_without_a_tty_or_yes_refuses() {
    let env = Env::new();
    env.rimz()
        .args(["reset", "--no-start"])
        .assert()
        .failure()
        .stderr(contains("pass --yes"));
}

fn archive_paths(dir: &Path) -> Vec<PathBuf> {
    let mut archives = fs::read_dir(dir)
        .expect("read archive dir")
        .map(|entry| entry.expect("archive entry").path())
        .collect::<Vec<_>>();
    archives.sort();
    archives
}

fn agent_observation(project_root: &Path) -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        ask_queue: None,
        agent_id: Some(AgentSessionId::from("claude-1")),
        agent_name: None,
        launch: LaunchParams::default(),
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        account_key: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(project_root.display().to_string()),
        worktree_branch: Some("main".to_owned()),
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        compacted_from: None,
        usage: rimz::agents::AgentUsageSummary::default(),
        pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_1")),
        pane_stamp: None,
        parent_agent_id: None,
        background_shells: None,
    }
}
