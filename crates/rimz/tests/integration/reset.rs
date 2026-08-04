//! Integration coverage for `rimz reset`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::agents::PermissionMode;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::harness::run::{RunRecord, RunStatus};
use rimz::harness::run_wake::{ExpectedRunFrame, RunWaiter};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::store::event::EventEnvelope;
use rimz::workspace::WorkspaceResolver;

use crate::common::Env;

/// `rimz reset --no-start --yes` deletes the room's serialized-session cache and
/// reports what it removed, without trying to rebirth or attach. `--mux zellij`
/// forces the Zellij backend so the cache purge runs regardless of which mux the
/// host has installed; the purge itself is filesystem-only.
#[test]
fn reset_purges_the_resurrection_cache() {
    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve");

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
fn reset_archives_records_and_clears_room_state() {
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
    let diag = rimz::diag::DiagSink::under(
        paths.root.clone(),
        env.workspace_id.clone(),
        "rimz-test",
        None,
    );
    let diag_log = diag.log_path().unwrap();
    let diag_frames = diag.frames_dir().unwrap();
    fs::write(&diag_log, b"diag\n").expect("write diag");
    fs::create_dir_all(&diag_frames).expect("mkdir diag frames");
    fs::write(diag_frames.join("frame.1.0.test.json"), b"{}").expect("write frame");

    let runtime = env.runtime_paths();
    fs::create_dir_all(&runtime.root).expect("mkdir runtime");
    fs::write(runtime.root.join("binding.log.jsonl"), b"binding\n").expect("write binding");
    let runtime_root = runtime.root.clone();

    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success()
        .stderr(contains("Records: archived"))
        .stderr(contains("prior agent rollup kept"));

    assert!(paths.workspace_record.exists(), "workspace identity stays");
    assert!(!paths.events_log.exists(), "active log was archived");
    assert!(!diag_log.exists(), "diag log cleared");
    assert!(!diag_frames.exists(), "diag frame captures cleared");
    assert!(!runtime_root.exists(), "workspace runtime dir cleared");

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

    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
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
        agent_id: Some(AgentSessionId::from("claude-1")),
        agent_name: None,
        launch: LaunchParams::default(),
        signal: LifecycleSignal::Registered,
        agent_pid: None,
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
    }
}
