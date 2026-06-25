//! Integration coverage for `rimz reset`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::bridge::{ExpectedRunFrame, RunWakeOutcome};
use rimz::feed::{FeedItem, FeedKind, Surface};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::run::{PermissionMode, RunRecord, RunStatus};
use rimz::schema::event::EventEnvelope;
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

    // Plant a serialized-session cache the way Zellij would, under HOME/.cache
    // (the harness pins HOME and leaves XDG_CACHE_HOME unset, so `cache_home()`
    // resolves there).
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
    let ledger = env.ledger();
    let mut item = FeedItem::new(
        env.workspace_id.clone(),
        Surface::Script,
        FeedKind::Question,
        "ship?",
        "rimz",
        "cli",
    );
    item.options = vec!["yes".to_owned(), "no".to_owned()];
    let request_id = item.request_id.clone();
    ledger
        .push_feed_item(&item, "rimz-test")
        .expect("push feed item");
    ledger
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
    let diag_log = diag.log_path();
    let diag_frames = diag.frames_dir();
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
        .stderr(contains("abandoned 1 pending item"))
        .stderr(contains("prior agent rollup kept"));

    assert!(paths.workspace_record.exists(), "workspace identity stays");
    assert!(!paths.events_log.exists(), "active log was archived");
    assert!(!diag_log.exists(), "diag log cleared");
    assert!(!diag_frames.exists(), "diag frame captures cleared");
    assert!(!runtime_root.exists(), "workspace runtime dir cleared");

    let feed_entries = fs::read_dir(&paths.feed_dir)
        .expect("read feed dir")
        .count();
    assert_eq!(feed_entries, 0, "feed coordination files cleared");

    let archives = archive_paths(&paths.events_archive_dir);
    assert_eq!(archives.len(), 1, "one reset archive written");
    let archived = rimz::ledger::event_log::read_all(&archives[0]).expect("read archive");
    assert!(archived.iter().any(|event| event.method == "feed.push"));
    assert!(
        archived
            .iter()
            .any(|event| event.method == "agent.lifecycle")
    );
    let abandon = archived
        .iter()
        .find(|event| event.method == "feed.abandon")
        .expect("feed abandon event");
    assert_eq!(abandon.params["request_id"], request_id.as_str());
    assert_eq!(abandon.params["reason"], "workspace_reset");

    let projection = env
        .ledger()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("projection");
    assert_eq!(projection.items.len(), 0, "feed files are derived state");
    assert_eq!(projection.agents.len(), 1, "soft reset keeps resume rollup");

    let hard = Env::new();
    hard.ledger()
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
        .ledger()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("projection");
    assert!(projection.agents.is_empty(), "hard reset starts blank");
}

#[test]
fn reset_wakes_blocking_feed_ask_before_clearing_runtime() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let mut child = env
        .rimz()
        .args(["feed", "ask", "--title", "ship?", "--options", "yes,no"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn blocking feed ask");
    let request_id = env
        .poll_pending_request_id(Instant::now() + Duration::from_secs(5))
        .expect("blocking ask should reach pending state");

    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success()
        .stderr(contains("abandoned 1 pending item"));

    let output = wait_child_output(&mut child, Duration::from_secs(5))
        .expect("reset should wake the blocked feed ask");
    assert!(
        !output.status.success(),
        "reset closes the ask without a decision"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "request {request_id} closed before a decision was delivered"
        )),
        "feed ask should report the reset terminal wake, stderr:\n{stderr}"
    );
}

#[test]
fn reset_cancels_active_runs_and_wakes_waiters() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }

    let ledger = env.ledger();
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "ship it".to_owned(),
        env.project_root.clone(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::run::create(ledger.paths(), &record).expect("create run");
    let (sock, _sock_path) =
        rimz::bridge::bind_run(ledger.runtime_paths(), &run_id).expect("bind run socket");

    env.rimz()
        .args(["--mux", "zellij", "reset", "--no-start", "--yes"])
        .assert()
        .success()
        .stderr(contains("canceled 1 run"));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let outcome = runtime
        .block_on(rimz::bridge::wait_for_run_completion_owning(
            sock,
            ExpectedRunFrame {
                workspace_id: env.workspace_id.clone(),
                run_id: run_id.clone(),
            },
            Some(Duration::from_secs(1)),
        ))
        .expect("wait for run wakeup");
    assert_eq!(outcome, RunWakeOutcome::Completed(RunStatus::Canceled));

    let after = rimz::run::load(ledger.paths(), &run_id).expect("load run");
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

fn wait_child_output(child: &mut Child, timeout: Duration) -> Option<Output> {
    let stderr = child.stderr.take().map(drain_pipe);
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Some(Output {
                    status,
                    stdout: Vec::new(),
                    stderr: join_pipe(stderr),
                });
            }
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn drain_pipe<R>(mut pipe: R) -> thread::JoinHandle<Vec<u8>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

fn join_pipe(handle: Option<thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

fn agent_observation(project_root: &Path) -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        agent_id: Some(AgentSessionId::from("claude-1")),
        agent_name: None,
        role: None,
        team: None,
        profile: None,
        kind_ordinal: None,
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(project_root.display().to_string()),
        worktree_branch: Some("main".to_owned()),
        task: None,
        prompt: None,
        transcript_path: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        pane_id: Some(PaneId::from_parts(MuxName::Zellij, "terminal_1")),
        parent_agent_id: None,
    }
}
