//! Integration coverage for `rimz workspace migrate/rotate-events`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::WorkspaceId;
use rimz::feed::{FeedItem, FeedKind, Surface};
use rimz::schema::event::EventEnvelope;
use serde_json::json;

use crate::common::{Env, canonical};

#[test]
fn workspace_migrate_moves_ledger_and_rewrites_workspace_ids() {
    let env = Env::new();
    let old_root = env.project_root.join("old-project");
    let new_root = env.project_root.join("new-project");
    std::fs::create_dir_all(&old_root).expect("mkdir old");
    std::fs::create_dir_all(&new_root).expect("mkdir new");

    let old_id = WorkspaceId::from_project_root(&canonical(&old_root));
    let new_id = WorkspaceId::from_project_root(&canonical(&new_root));
    let old_paths = env.state_path_for(&old_root);
    let new_paths = env.state_path_for(&new_root);

    let item = FeedItem::new(
        old_id.clone(),
        Surface::Script,
        FeedKind::Question,
        "deploy?",
        "rimz",
        "cli",
    );
    let request_id = item.request_id.clone();
    env.ledger_for(&old_root)
        .push_feed_item(&item, "old-session")
        .expect("push old item");

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

    assert!(!old_paths.root.exists(), "old ledger dir should be gone");
    assert!(new_paths.root.exists(), "new ledger dir should exist");

    let migrated = env.ledger_for(&new_root);
    let loaded = migrated.load_feed_item(&request_id).expect("load item");
    assert_eq!(loaded.workspace_id, new_id);

    let events = migrated.read_events().expect("read events");
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event.workspace_id == new_id));

    let record = rimz::ledger::workspace_record::read(&new_paths.workspace_record)
        .expect("workspace record");
    assert_eq!(record.workspace_id, new_id);
    assert_eq!(record.project_root, canonical(&new_root));
}

#[test]
fn workspace_rotate_events_archives_and_preserves_agent_rollup() {
    let env = Env::new();
    let project = env.project_root.join("project");
    std::fs::create_dir_all(&project).expect("mkdir project");

    let workspace_id = WorkspaceId::from_project_root(&canonical(&project));
    let ledger = env.ledger_for(&project);

    // Append two lifecycle events for the same agent so the rollup carries a
    // worktree branch we can assert on after rotation. Older first; newer wins.
    for (signal, branch) in [
        ("registered", "main"),
        ("turn_started", "feature-migration"),
    ] {
        let event = EventEnvelope::new(
            workspace_id.clone(),
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            json!({
                "agent_id": "claude-1",
                "signal": { "signal": signal },
                "worktree_branch": branch,
            }),
        );
        ledger.append_event(&event).expect("append lifecycle");
    }

    // A stale archive that the prune step should remove.
    let paths = env.state_path_for(&project);
    std::fs::create_dir_all(&paths.events_archive_dir).expect("mkdir archive");
    let stale_archive = paths
        .events_archive_dir
        .join("events.000000000000000000000000.jsonl");
    std::fs::write(&stale_archive, b"old\n").expect("write stale archive");
    let old = SystemTime::now() - Duration::from_secs(7 * 86_400);
    std::fs::File::open(&stale_archive)
        .expect("open stale")
        .set_modified(old)
        .expect("backdate stale");

    env.rimz()
        .current_dir(&project)
        .args([
            "workspace",
            "rotate-events",
            "--max-bytes",
            "1",
            "--archive-older-than",
            "1d",
        ])
        .assert()
        .success()
        .stdout(contains("event-log rotated"))
        .stdout(contains("pruned        : 1 archive(s)"));

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
    let projection = ledger
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

// --- session-pinned identity ---
//
// The split-brain regression this guards: a directory room at the harness
// root with an agent pane cwd'd inside a nested git repo. Without the pin the
// hook's static ladder resolves the *repo's* workspace, and its events land
// in a ledger the room's sidebar never reads.

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
        "the pinned room's ledger holds the hook's event",
    );
    assert!(
        !repo.events_log.exists(),
        "no split-brain ledger appears for the nested repo",
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

#[test]
fn doctor_reports_the_room_tree_with_root_classes() {
    let env = Env::new();
    let nested = env.project_root.join("inner");
    env.record(&env.project_root.clone());
    env.record(&nested);

    let output = env.rimz().arg("doctor").output().expect("run doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rooms         : 2 recorded"),
        "doctor counts both rooms:\n{stdout}",
    );
    assert!(
        stdout.contains(&format!("{} (directory)", env.project_root.display())),
        "the bare harness root is a directory room:\n{stdout}",
    );
    assert!(
        stdout.contains(&format!("{} (directory)", nested.display())),
        "the bare nested dir is a directory room:\n{stdout}",
    );
    assert!(
        stdout.contains("root class    :"),
        "the workspace block names its root class:\n{stdout}",
    );
}
