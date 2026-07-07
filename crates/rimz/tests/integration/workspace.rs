//! Integration coverage for `rimz workspace migrate/rotate-events`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::WorkspaceId;
use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::agents::{AgentState, AgentStatus};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::message::{DeliveryGate, MessageRecord, MessageStatus};
use rimz::store::event::EventEnvelope;

use crate::common::{Env, canonical};

#[test]
fn workspace_migrate_moves_store_and_rewrites_workspace_ids() {
    let env = Env::new();
    let old_root = env.project_root.join("old-project");
    let new_root = env.project_root.join("new-project");
    std::fs::create_dir_all(&old_root).expect("mkdir old");
    std::fs::create_dir_all(&new_root).expect("mkdir new");

    let old_id = WorkspaceId::from_project_root(&canonical(&old_root));
    let new_id = WorkspaceId::from_project_root(&canonical(&new_root));
    let old_paths = env.state_path_for(&old_root);
    let new_paths = env.state_path_for(&new_root);

    let old_store = env.store_for(&old_root);

    let agent = message_agent();
    let pending_message = MessageRecord::new(
        old_id.clone(),
        &agent,
        "pending message".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let pending_message_id = pending_message.message_id.clone();
    old_store
        .queue_message(&pending_message, "old-session")
        .expect("queue pending message");
    let delivered_message = MessageRecord::new(
        old_id.clone(),
        &agent,
        "delivered message".to_owned(),
        true,
        DeliveryGate::Done,
    );
    let delivered_message_id = delivered_message.message_id.clone();
    old_store
        .queue_message(&delivered_message, "old-session")
        .expect("message delivered message");
    old_store
        .settle_message(
            &delivered_message_id,
            MessageStatus::Delivered,
            "old-session",
            None,
        )
        .expect("settle delivered message");

    std::fs::remove_dir_all(&old_root).expect("simulate moved project");

    env.rimz()
        .args([
            "workspace",
            "migrate",
            &old_root.display().to_string(),
            &new_root.display().to_string(),
        ])
        .assert()
        .success()
        .stdout(contains(format!("migrated {old_id} -> {new_id}")));

    assert!(!old_paths.root.exists(), "old store dir should be gone");
    assert!(new_paths.root.exists(), "new store dir should exist");

    let migrated = env.store_for(&new_root);
    let messages = migrated.list_messages().expect("list messages");
    let pending = messages
        .iter()
        .find(|message| message.message_id == pending_message_id)
        .expect("pending message");
    assert_eq!(pending.workspace_id, new_id);
    assert_eq!(pending.status, MessageStatus::Queued);
    let events = migrated.read_events().expect("read events");
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event.workspace_id == new_id));
    assert!(
        events.iter().any(|event| {
            event.method == "message.delivered"
                && event.params_value()["message_id"] == delivered_message_id.as_str()
        }),
        "delivered message is represented by the rewritten event log"
    );

    let record =
        rimz::store::workspace_record::read(&new_paths.workspace_record).expect("workspace record");
    assert_eq!(record.workspace_id, new_id);
    assert_eq!(record.project_root, canonical(&new_root));
}

fn message_agent() -> AgentState {
    let now = jiff::Timestamp::now();
    AgentState {
        agent_id: AgentSessionId::from("claude-migrate"),
        kind: AgentKind::new_unchecked("claude"),
        name: None,
        name_explicit: false,
        kind_ordinal: None,
        profile: None,
        role: None,
        team: None,
        launch_group: None,
        launch_ordinal: None,
        channel: None,
        status: AgentStatus::Idle,
        phase: rimz::agents::TurnPhase::Idle,
        pane: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        model: None,
        effort: None,
        recent_prompts: Vec::new(),
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        waiting_since: None,
        compacting_since: None,
        compaction_count: 0,
        last_compact_command_tokens: None,
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    }
}

#[test]
fn workspace_rotate_events_archives_and_preserves_agent_rollup() {
    let env = Env::new();
    let project = env.project_root.join("project");
    std::fs::create_dir_all(&project).expect("mkdir project");

    let workspace_id = WorkspaceId::from_project_root(&canonical(&project));
    let store = env.store_for(&project);

    // Append two lifecycle events for the same agent so the rollup carries a
    // worktree branch we can assert on after rotation. Older first; newer wins.
    for (event_name, signal, branch) in [
        ("SessionStart", LifecycleSignal::Registered, "main"),
        (
            "SessionStart",
            LifecycleSignal::Registered,
            "feature-migration",
        ),
    ] {
        let event = EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            "session",
            "claude",
            event_name,
            &lifecycle_observation(signal, branch),
        );
        store.append_event(&event).expect("append lifecycle");
    }

    // A stale archive that the default 14d prune step should remove.
    let paths = env.state_path_for(&project);
    std::fs::create_dir_all(&paths.events_archive_dir).expect("mkdir archive");
    let stale_archive = paths
        .events_archive_dir
        .join("events.000000000000000000000000.jsonl");
    std::fs::write(&stale_archive, b"old\n").expect("write stale archive");
    let old = SystemTime::now() - Duration::from_secs(21 * 86_400);
    std::fs::File::open(&stale_archive)
        .expect("open stale")
        .set_modified(old)
        .expect("backdate stale");

    env.rimz()
        .current_dir(&project)
        .args(["workspace", "rotate-events", "--max-bytes", "1"])
        .assert()
        .success()
        .stdout(contains("event-log rotated"))
        .stdout(contains("pruned:        1 archive(s)"));

    assert!(!paths.events_log.exists(), "active log moved");
    assert!(paths.agents_carryover.exists(), "carryover persisted");
    assert!(!stale_archive.exists(), "stale archive pruned");

    let archives: Vec<PathBuf> = std::fs::read_dir(&paths.events_archive_dir)
        .expect("read archive dir")
        .map(|e| e.expect("entry").path())
        .collect();
    assert_eq!(archives.len(), 1, "exactly one fresh archive remains");

    // After rotation the sidebar snapshot should still know the latest agent
    // observation because it was folded into the carryover.
    let projection = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    assert_eq!(projection.agents.len(), 1);
    let agent = &projection.agents[0];
    assert_eq!(agent.agent_id, "claude-1");
    assert_eq!(agent.kind, "claude");
    assert_eq!(agent.worktree_branch.as_deref(), Some("feature-migration"));

    // Second invocation without any new events should be a no-op skip.
    env.rimz()
        .current_dir(&project)
        .args(["workspace", "rotate-events", "--max-bytes", "1MiB"])
        .assert()
        .success()
        .stdout(contains("event-log rotation skipped"));
}

fn lifecycle_observation(signal: LifecycleSignal, branch: &str) -> AgentLifecycleObservation {
    AgentLifecycleObservation {
        agent_id: Some(AgentSessionId::from("claude-1")),
        agent_name: None,
        launch: LaunchParams::default(),
        signal,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: None,
        worktree_branch: Some(branch.to_owned()),
        task: None,
        prompt: None,
        transcript_path: None,
        origin: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        pane_id: None,
        pane_stamp: None,
        parent_agent_id: None,
    }
}

// --- session-pinned identity ---
//
// The split-brain regression this guards: a directory room at the harness
// root with an agent pane cwd'd inside a nested git repo. Without the pin the
// hook's static ladder resolves the *repo's* workspace, and its events land
// in a store the room's sidebar never reads.

/// `git init` a nested repo, or skip when git is absent (host-dependency
/// self-skip, per the suite contract).
fn init_nested_repo(env: &Env) -> Option<PathBuf> {
    let nested = env.project_root.join("code").join("query-engine");
    std::fs::create_dir_all(&nested).expect("mkdir nested repo");
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&nested)
        .status();
    match status {
        Ok(status) if status.success() => Some(nested),
        _ => {
            tracing::warn!("skipping: git unavailable");
            None
        }
    }
}

#[test]
fn hook_inside_a_nested_repo_lands_in_the_pinned_room() {
    let env = Env::new();
    let Some(nested) = init_nested_repo(&env) else {
        return;
    };

    let mut cmd = env.hook_command("claude");
    cmd.current_dir(&nested)
        .env(rimz::workspace::ENV_WORKSPACE_ID, env.workspace_id.as_str())
        .env(rimz::workspace::ENV_PROJECT_ROOT, &env.project_root);
    let output = env
        .spawn_payload(cmd, &crate::common::permission_payload("Read"))
        .wait_with_output()
        .expect("wait hook");
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let pinned = env.state_path_for(&env.project_root);
    let repo = env.state_path_for(&nested);
    assert!(
        pinned.events_log.exists(),
        "the pinned room's store holds the hook's event",
    );
    assert!(
        !repo.events_log.exists(),
        "no split-brain store appears for the nested repo",
    );
}

#[test]
fn corrupt_pin_falls_back_to_the_repo_workspace() {
    let env = Env::new();
    let Some(nested) = init_nested_repo(&env) else {
        return;
    };

    // An id that does not hash from the pinned root: the verified-pin read
    // rejects it and the hook degrades to the static ladder — the repo's own
    // workspace — rather than erroring on the agent's critical path.
    let stale = WorkspaceId::from_project_root(&PathBuf::from("/somewhere/else"));
    let mut cmd = env.hook_command("claude");
    cmd.current_dir(&nested)
        .env(rimz::workspace::ENV_WORKSPACE_ID, stale.as_str())
        .env(rimz::workspace::ENV_PROJECT_ROOT, &env.project_root);
    let output = env
        .spawn_payload(cmd, &crate::common::permission_payload("Read"))
        .wait_with_output()
        .expect("wait hook");
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let repo = env.state_path_for(&nested);
    assert!(
        repo.events_log.exists(),
        "the static ladder resolves the nested repo's own workspace",
    );
}

/// A fake in-pane `codex` carrying the room's pin: a sleeper script whose
/// kernel `comm` is the script name `codex`, parked at `cwd` with the pin in
/// its environment — the sibling process a daemon-routed hook recovers from.
/// Killed on drop so a failing assertion never leaks the sleeper.
#[cfg(target_os = "linux")]
struct SiblingAgent {
    child: std::process::Child,
}

#[cfg(target_os = "linux")]
impl Drop for SiblingAgent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
fn spawn_sibling_codex(env: &Env, cwd: &std::path::Path) -> SiblingAgent {
    use std::os::unix::fs::PermissionsExt;

    let script = env.home_root.join("codex");
    // Plain `sleep` (no `exec`) keeps the interpreter — and so the `codex`
    // comm — alive for the scan.
    std::fs::write(&script, "#!/bin/sh\nsleep 30\n").expect("write sibling script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let child = std::process::Command::new(&script)
        .current_dir(cwd)
        .env(rimz::workspace::ENV_WORKSPACE_ID, env.workspace_id.as_str())
        .env(rimz::workspace::ENV_PROJECT_ROOT, &env.project_root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sibling codex");

    // `spawn` returns at fork; wait for the exec so the kernel comm reads
    // `codex` before the hook scans /proc.
    let sibling = SiblingAgent { child };
    let comm_path = format!("/proc/{}/comm", sibling.child.id());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::read_to_string(&comm_path) {
            Ok(comm) if comm.trim() == "codex" => break,
            _ => {
                assert!(std::time::Instant::now() < deadline, "sibling never exec'd");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    sibling
}

#[test]
fn codex_hook_recovers_pin_from_sibling_process_when_env_pin_absent() {
    // The daemon-routed regression this guards: Codex's per-user app-server
    // spawns hook children with the daemon's env — no session pin — and the
    // session cwd. Without recovery the static ladder mints a workspace at
    // the cwd (`cd ~; codex` lands in a hidden `$HOME` store the room's
    // sidebar never reads); recovery adopts the pin from the in-pane agent
    // process sharing that cwd.
    #[cfg(not(target_os = "linux"))]
    {
        tracing::warn!("skipping: /proc recovery is Linux-only");
    }
    #[cfg(target_os = "linux")]
    {
        let env = Env::new();
        // The agent's launch dir sits outside the room root, like `$HOME`.
        let elsewhere = env.home_root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("mkdir elsewhere");
        let _sibling = spawn_sibling_codex(&env, &elsewhere);

        // The harness scrubs the pin from the hook's own env (`Env::rimz`),
        // exactly like a daemon-spawned hook child.
        let mut cmd = env.hook_command("codex");
        cmd.current_dir(&elsewhere);
        let output = env
            .spawn_payload(cmd, &crate::common::codex_permission_payload())
            .wait_with_output()
            .expect("wait hook");
        assert!(
            output.status.success(),
            "hook failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );

        let pinned = env.state_path_for(&env.project_root);
        let stray = env.state_path_for(&elsewhere);
        let events = std::fs::read_to_string(&pinned.events_log)
            .expect("the recovered pin routes the hook into the room's store");
        assert!(
            events.contains("\"source\":\"codex\""),
            "the room's store holds the codex hook event:\n{events}",
        );
        assert!(
            !stray.events_log.exists(),
            "no hidden cwd-derived store appears beside the room",
        );
    }
}

#[test]
fn doctor_reports_workspace_root_class_without_room_inventory() {
    let env = Env::new();
    let nested = env.project_root.join("inner");
    env.record(&env.project_root.clone());
    env.record(&nested);

    let output = env
        .rimz()
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor --json emits valid json");

    assert!(
        report.get("rooms").is_none(),
        "doctor omits room inventory: {report}"
    );
    assert_eq!(
        report["workspace"]["ready"]["root_class"], "directory",
        "the workspace block names its root class: {report}",
    );
}
