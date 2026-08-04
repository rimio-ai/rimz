use std::cell::Cell;

use serde_json::json;

use super::*;
use crate::agents::{AgentLifecycleObservation, AgentState, LifecycleSignal};
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::message::{DeliveryGate, MessageRecord, MessageStatus};
use crate::store::event::MessageEventMethod;
use crate::store::paths::{RuntimePaths, StatePaths};
use crate::workspace::WorkspaceResolver;

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct CarryoverJson {
    #[serde(default)]
    agents: Vec<AgentState>,
    #[serde(default)]
    resume_outcomes: Vec<serde_json::Value>,
}

fn read_test_carryover(path: &Path) -> CarryoverJson {
    serde_json::from_slice(&std::fs::read(path).expect("read carryover")).expect("parse carryover")
}

fn write_test_carryover(path: &Path, agents: Vec<AgentState>) {
    crate::store::atomic::write_temp_then_rename(
        path,
        &CarryoverJson {
            agents,
            ..Default::default()
        },
    )
    .expect("write carryover");
}

#[test]
fn launch_event_builder_preserves_serialized_state_shapes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime).expect("open store");
    let run_id = crate::ids::RunId::new();
    let pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%7");
    let request = AgentLaunchRequest {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("launch_follow_up"),
        name: AgentLaunchName::Explicit("writer".to_owned()),
        launch: LaunchParams {
            parent_agent_id: Some(AgentSessionId::from("parent-session")),
            parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
            launch_depth: Some(2),
            profile: Some("codex-coder".to_owned()),
            mode: Some(crate::agents::PermissionMode::Yolo),
            role: Some("coder".to_owned()),
            model: Some("gpt-5.6-sol".to_owned()),
            effort: Some("xhigh".to_owned()),
            budget: Some("$2.00/day".to_owned()),
            team: Some("forge".to_owned()),
            launch_group: Some("launch_group_1".to_owned()),
            launch_ordinal: Some(1),
            channel: Some("identity-channel".to_owned()),
            kind_ordinal: Some(2),
        },
        run_id: Some(run_id.clone()),
        prompt: Some("  build it  ".to_owned()),
    };
    let batch = store
        .begin_agent_launch_batch(
            &[request],
            AgentLaunchScope {
                session_name: "rimz-test".to_owned(),
                cwd: PathBuf::from("/repo-worktrees/auth"),
                worktree_name: Some("auth".to_owned()),
                channel: Some("fallback-channel".to_owned()),
                description: Some("  launch description  ".to_owned()),
            },
        )
        .expect("begin launch batch");
    let identity = batch.single_identity().expect("one identity");
    store
        .bind_agent_launch(identity, "rimz-test", Path::new("/wrapper/cwd"), &pane_id)
        .expect("bind launch");
    store
        .fail_agent_launch(identity, "rimz-test", Path::new("/restart/cwd"))
        .expect("sparse fail launch");
    store
        .fail_agent_launch_batch(&batch)
        .expect("fail same-process batch");

    let events = store.read_events().expect("read launches");
    let payloads = events
        .iter()
        .map(|event| {
            let crate::store::event::EventKind::AgentLaunch(payload) = event.kind() else {
                panic!("agent launch event")
            };
            payload
        })
        .collect::<Vec<_>>();
    assert_eq!(payloads.len(), 4);
    for event in &events {
        assert_eq!(event.workspace_id, workspace_id);
        assert_eq!(event.session_name, "rimz-test");
        assert_eq!(event.source, "codex");
        assert_eq!(event.source_kind, "agent");
        assert_eq!(event.method, "agent.launched");
    }
    for payload in &payloads {
        assert_eq!(payload.agent_id, identity.agent_id);
        assert_eq!(payload.launch_id.as_ref(), Some(&identity.agent_id));
        assert_eq!(payload.agent_name, "writer");
        assert!(payload.agent_name_explicit);
        assert_eq!(payload.launch, identity.launch);
        assert_eq!(payload.run_id, Some(run_id.clone()));
        assert_eq!(payload.prompt.as_deref(), Some("  build it  "));
        assert_eq!(
            payload.launch.parent_agent_id.as_deref(),
            Some("parent-session")
        );
        assert_eq!(
            payload.launch.parent_agent_kind.as_ref(),
            Some(&AgentKind::new_unchecked("claude"))
        );
        assert_eq!(payload.launch.launch_depth, Some(2));
    }

    let starting = &payloads[0];
    assert_eq!(starting.launch.channel.as_deref(), Some("identity-channel"));
    assert_eq!(starting.state, AgentLaunchState::Starting);
    assert_eq!(starting.pane_id, None);
    assert_eq!(starting.runtime_owner, None);
    assert_eq!(
        starting.worktree_path.as_deref(),
        Some("/repo-worktrees/auth")
    );
    assert_eq!(starting.worktree_branch.as_deref(), Some("auth"));
    assert_eq!(starting.description.as_deref(), Some("launch description"));

    let bound = &payloads[1];
    assert_eq!(bound.state, AgentLaunchState::Bound);
    assert_eq!(bound.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(bound.worktree_path.as_deref(), Some("/wrapper/cwd"));
    assert_eq!(bound.worktree_branch, None);
    assert_eq!(bound.description, None);
    assert_eq!(bound.launch.channel.as_deref(), Some("identity-channel"));
    assert_eq!(
        bound.runtime_owner,
        Some(runtime::current_process_owner(
            RuntimeOwnerKind::Agent,
            "launch_follow_up"
        ))
    );

    let sparse_failed = &payloads[2];
    assert_eq!(sparse_failed.state, AgentLaunchState::Failed);
    assert_eq!(sparse_failed.worktree_path.as_deref(), Some("/restart/cwd"));
    assert_eq!(sparse_failed.worktree_branch, None);
    assert_eq!(sparse_failed.description, None);
    assert_eq!(sparse_failed.pane_id, None);
    assert_eq!(sparse_failed.runtime_owner, None);

    let same_process_failed = &payloads[3];
    assert_eq!(same_process_failed.state, AgentLaunchState::Failed);
    assert_eq!(
        same_process_failed.worktree_path.as_deref(),
        Some("/repo-worktrees/auth")
    );
    assert_eq!(same_process_failed.worktree_branch.as_deref(), Some("auth"));
    assert_eq!(same_process_failed.description, None);
    assert_eq!(
        same_process_failed.launch.channel.as_deref(),
        Some("identity-channel")
    );
}

#[test]
fn attach_agent_pane_records_process_owned_placement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime_paths =
        RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime_paths).expect("open store");
    let kind = AgentKind::new_unchecked("codex");
    let agent_id = AgentSessionId::from("sess-resumed");
    let launch_id = AgentSessionId::from("launch-resumed");
    let pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%4");

    store
        .attach_agent_pane(&kind, &agent_id, Some(&launch_id), "rimz-test", &pane_id)
        .expect("attach resumed agent");

    let events = store.read_events().expect("read attach");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.workspace_id, workspace_id);
    assert_eq!(event.session_name, "rimz-test");
    assert_eq!(event.source, "codex");
    assert_eq!(event.source_kind, "agent");
    assert_eq!(event.method, "agent.attached");
    let crate::store::event::EventKind::AgentAttach(payload) = event.kind() else {
        panic!("agent attach event")
    };
    assert_eq!(payload.agent_id, agent_id);
    assert_eq!(payload.launch_id, Some(launch_id));
    assert_eq!(payload.pane_id, pane_id);
    assert_eq!(payload.pane_pid, Some(std::process::id()));
    assert_eq!(
        payload.runtime_owner,
        runtime::current_process_owner(RuntimeOwnerKind::Agent, "sess-resumed")
    );
}

#[test]
fn launch_event_builder_uses_scope_channel_and_omits_blank_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime).expect("open store");
    let request = AgentLaunchRequest {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("launch_fallback"),
        name: AgentLaunchName::Mint,
        launch: LaunchParams::default(),
        run_id: None,
        prompt: Some("  ".to_owned()),
    };
    store
        .begin_agent_launch_batch(
            &[request],
            AgentLaunchScope {
                session_name: "rimz-test".to_owned(),
                cwd: PathBuf::from("/repo"),
                worktree_name: None,
                channel: Some("fallback-channel".to_owned()),
                description: Some("  ".to_owned()),
            },
        )
        .expect("begin launch batch");
    let events = store.read_events().expect("read launch");
    let crate::store::event::EventKind::AgentLaunch(payload) = events[0].kind() else {
        panic!("agent launch event")
    };
    assert_eq!(payload.launch.channel.as_deref(), Some("fallback-channel"));
    assert_eq!(payload.prompt, None);
    assert_eq!(payload.description, None);
}

#[test]
fn launch_batch_keeps_request_and_follow_up_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime).expect("open store");
    let requests = ["first", "second"].map(|name| AgentLaunchRequest {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from(format!("launch-{name}")),
        name: AgentLaunchName::Explicit(name.to_owned()),
        launch: LaunchParams::default(),
        run_id: None,
        prompt: None,
    });
    let batch = store
        .begin_agent_launch_batch(
            &requests,
            AgentLaunchScope {
                session_name: "rimz-test".to_owned(),
                cwd: dir.path().to_path_buf(),
                worktree_name: None,
                channel: None,
                description: None,
            },
        )
        .expect("begin launch batch");
    assert_eq!(
        batch
            .identities()
            .iter()
            .map(|identity| identity.name.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );

    store
        .fail_agent_launch_batch(&batch)
        .expect("fail launch batch");
    let events = store.read_events().expect("read launch events");
    let states = events
        .iter()
        .filter_map(|event| {
            let crate::store::event::EventKind::AgentLaunch(payload) = event.kind() else {
                return None;
            };
            Some((payload.agent_name, payload.state))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        [
            ("first".to_owned(), AgentLaunchState::Starting),
            ("second".to_owned(), AgentLaunchState::Starting),
            ("first".to_owned(), AgentLaunchState::Failed),
            ("second".to_owned(), AgentLaunchState::Failed),
        ]
    );
}

#[test]
fn launch_batch_failure_keeps_earlier_identity_committed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime).expect("open store");
    let requests = ["first", "second"].map(|name| AgentLaunchRequest {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from(format!("launch-{name}")),
        name: AgentLaunchName::Explicit(name.to_owned()),
        launch: LaunchParams::default(),
        run_id: None,
        prompt: None,
    });
    let batch = store
        .begin_agent_launch_batch(
            &requests,
            AgentLaunchScope {
                session_name: "rimz-test".to_owned(),
                cwd: dir.path().to_path_buf(),
                worktree_name: None,
                channel: None,
                description: None,
            },
        )
        .expect("begin launch batch");
    let mut attempted = 0;

    let result = store.fail_agent_launch_batch_with(&batch, |store, identity, scope| {
        attempted += 1;
        if attempted == 2 {
            return Err(StoreErr::AgentLaunchIdentity(
                "injected second append failure".to_owned(),
            ));
        }
        store.fail_agent_launch_in_scope(identity, scope)
    });

    assert!(result.is_err());
    let failures = store
        .read_events()
        .expect("read launch events")
        .into_iter()
        .filter_map(|event| {
            let crate::store::event::EventKind::AgentLaunch(payload) = event.kind() else {
                return None;
            };
            (payload.state == AgentLaunchState::Failed).then_some(payload.agent_name)
        })
        .collect::<Vec<_>>();
    assert_eq!(failures, ["first"]);
}

#[test]
fn launch_state_appends_preserve_allocated_identity_and_fold_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths, runtime).expect("open store");
    let request = AgentLaunchRequest {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launch_state_flow"),
        name: AgentLaunchName::Explicit("planner".to_owned()),
        launch: LaunchParams {
            profile: Some("claude-planner".to_owned()),
            role: Some("planner".to_owned()),
            team: Some("forge".to_owned()),
            launch_group: Some("launch_group_1".to_owned()),
            launch_ordinal: Some(3),
            channel: Some("auth".to_owned()),
            model: Some("sonnet".to_owned()),
            effort: Some("high".to_owned()),
            ..LaunchParams::default()
        },
        run_id: None,
        prompt: None,
    };
    let batch = store
        .begin_agent_launch_batch(
            &[request],
            AgentLaunchScope {
                session_name: "rimz-test".to_owned(),
                cwd: dir.path().to_path_buf(),
                worktree_name: Some("auth".to_owned()),
                channel: Some("fallback".to_owned()),
                description: None,
            },
        )
        .expect("allocate launch");
    let pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_4");
    store
        .bind_agent_launch(
            batch.single_identity().expect("one identity"),
            "rimz-test",
            dir.path(),
            &pane_id,
        )
        .expect("bind launch");
    store.fail_agent_launch_batch(&batch).expect("fail launch");

    let projection = store
        .runtime_projection(crate::store::runtime::RuntimeScope::Audit)
        .expect("project launches");
    let [agent] = projection.agents.as_slice() else {
        panic!("one launch")
    };
    assert_eq!(agent.agent_id, batch.identities()[0].agent_id);
    assert_eq!(agent.name.as_deref(), Some("planner"));
    assert!(agent.name_explicit);
    assert_eq!(agent.profile.as_deref(), Some("claude-planner"));
    assert_eq!(agent.role.as_deref(), Some("planner"));
    assert_eq!(agent.team.as_deref(), Some("forge"));
    assert_eq!(agent.launch_group.as_deref(), Some("launch_group_1"));
    assert_eq!(agent.launch_ordinal, Some(3));
    assert_eq!(agent.channel.as_deref(), Some("auth"));
    assert_eq!(agent.model.as_deref(), Some("sonnet"));
    assert_eq!(agent.effort.as_deref(), Some("high"));
    assert_eq!(agent.status, crate::agents::AgentStatus::Failed);
    assert_eq!(
        agent.pane.as_ref().map(|pane| &pane.pane_id),
        Some(&pane_id)
    );
}

#[test]
fn record_room_bin_publishes_a_sweep_safe_spawn_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let workspace = WorkspaceResolver::resolve(&project, None).expect("workspace");
    let paths = StatePaths::under(workspace.workspace_id.clone(), dir.path()).expect("state");
    let runtime = RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).expect("runtime");
    let store = Store::open(paths.clone(), runtime).expect("open store");
    let first_dir = dir.path().join("builds/first");
    let first = first_dir.join("rimz");
    crate::store::atomic::write_executable_bytes_atomically(&first, b"first build")
        .expect("write first build");

    store
        .record_room_bin(&workspace, first.clone(), "build-1".to_owned())
        .expect("record owner bin");
    assert_eq!(std::fs::read(&paths.room_bin).unwrap(), b"first build");
    std::fs::remove_dir_all(first_dir).expect("sweep first build");
    assert_eq!(std::fs::read(&paths.room_bin).unwrap(), b"first build");

    store
        .record_workspace(&workspace)
        .expect("generic rerecord preserves owner bin");

    let preserved = workspace_record::read(&paths.workspace_record).expect("read record");
    assert_eq!(preserved.rimz_bin.as_deref(), Some(first.as_path()));
    assert_eq!(preserved.rimz_build.as_deref(), Some("build-1"));

    let second = dir.path().join("builds/second/rimz");
    crate::store::atomic::write_executable_bytes_atomically(&second, b"second build")
        .expect("write second build");
    store
        .record_room_bin(&workspace, second.clone(), "build-2".to_owned())
        .expect("replace owner bin");

    assert_eq!(std::fs::read(&paths.room_bin).unwrap(), b"second build");
    let replaced = workspace_record::read(&paths.workspace_record).expect("read replacement");
    assert_eq!(replaced.rimz_bin.as_deref(), Some(second.as_path()));
    assert_eq!(replaced.rimz_build.as_deref(), Some("build-2"));
}

#[test]
fn record_workspace_republishes_only_when_snapshot_record_fields_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first_project = dir.path().join("first-project");
    let second_project = dir.path().join("second-project");
    std::fs::create_dir_all(&first_project).expect("first project dir");
    std::fs::create_dir_all(&second_project).expect("second project dir");
    let first = WorkspaceResolver::resolve(&first_project, None).expect("first workspace");
    let second = WorkspaceResolver::resolve(&second_project, None).expect("second workspace");
    let paths = StatePaths::under(first.workspace_id.clone(), dir.path()).expect("state");
    let runtime = RuntimePaths::under(first.workspace_id.clone(), dir.path()).expect("runtime");
    let store = Store::open(paths.clone(), runtime).expect("open store");

    store
        .record_workspace(&first)
        .expect("record first workspace");
    let initial_bytes = std::fs::read(&paths.latest_snapshot).expect("initial latest snapshot");
    let initial: snapshot::SidebarSnapshot =
        serde_json::from_slice(&initial_bytes).expect("parse initial snapshot");
    assert_eq!(initial.display_name, "first-project");

    store
        .record_workspace(&first)
        .expect("re-record identical workspace");
    assert_eq!(
        std::fs::read(&paths.latest_snapshot).expect("unchanged latest snapshot"),
        initial_bytes,
        "an identical record must not republish latest.json",
    );

    store
        .record_workspace(&second)
        .expect("record changed workspace");
    let changed_bytes = std::fs::read(&paths.latest_snapshot).expect("changed latest snapshot");
    let changed: snapshot::SidebarSnapshot =
        serde_json::from_slice(&changed_bytes).expect("parse changed snapshot");
    assert_eq!(changed.display_name, "second-project");
    assert_eq!(
        changed.project_root.as_deref(),
        Some(second.project_root.as_path())
    );
    assert!(
        !paths.events_log.exists(),
        "record-only publication must not need an event"
    );
}

#[test]
fn rotate_event_log_writes_carryover_before_archiving_active_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths.clone(), runtime).expect("open store");
    let mut message = MessageRecord::new(
        workspace_id.clone(),
        &agent_state("claude", "sess-resume", Some("lucid-atlas")),
        "continue".to_owned(),
        true,
        DeliveryGate::Resume,
    );
    message.status = MessageStatus::Delivered;
    event_log::append(
        &paths.events_log,
        &EventEnvelope::message_event(&message, "rimz-test", MessageEventMethod::Delivered, None),
    )
    .expect("seed resume event");

    let rotate_called = Cell::new(false);
    store
        .rotate_event_log_with(1, None, |events_log, archive_dir, min_bytes| {
            rotate_called.set(true);
            assert!(
                paths.agents_carryover.exists(),
                "rotation must persist carryover before archiving the only active-log copy"
            );
            let carryover = read_test_carryover(&paths.agents_carryover);
            assert_eq!(
                carryover.resume_outcomes.len(),
                1,
                "rotation carryover must include terminal resume outcomes"
            );
            event_log::rotate(events_log, archive_dir, min_bytes)
        })
        .expect("rotate event log");

    assert!(rotate_called.get(), "test rotate hook should run");
}

#[test]
fn rotation_reseeds_before_archive_prune_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths.clone(), runtime).expect("open store");
    event_log::append(
        &paths.events_log,
        &EventEnvelope::new(
            workspace_id,
            "rimz-test",
            "test",
            "test",
            "test.event",
            json!({}),
        ),
    )
    .expect("seed event");
    snapshot::rebuild(&paths).expect("seed pre-rotation cache");

    let error = store
        .rotate_event_log_with(
            1,
            Some(Duration::ZERO),
            |events_log, archive_dir, min_bytes| {
                let outcome = event_log::rotate(events_log, archive_dir, min_bytes)?;
                std::fs::create_dir(archive_dir.join("events.prune-trap.jsonl"))
                    .expect("create archive prune trap");
                Ok(outcome)
            },
        )
        .expect_err("archive prune trap fails after rotation");
    assert!(error.to_string().contains("events.prune-trap.jsonl"));

    let cache: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&paths.rollup_cache).expect("reseeded rollup cache survives prune error"),
    )
    .expect("parse rollup cache");
    assert_eq!(
        cache["extent"]["offset"], 0,
        "cache describes fresh active log"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn rotation_carryover_keeps_live_and_recently_ended_agents() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime_paths =
        RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths.clone(), runtime_paths).expect("open store");
    let kind = AgentKind::new_unchecked("claude");
    let launch = |agent_id: &str, name: &str, pid| {
        EventEnvelope::agent_launched(
            workspace_id.clone(),
            "rimz-test",
            &kind,
            AgentLaunchPayload {
                agent_id: AgentSessionId::from(agent_id),
                launch_id: None,
                agent_name: name.to_owned(),
                agent_name_explicit: false,
                launch: LaunchParams::default(),
                state: crate::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: Some(runtime::process_owner(
                    RuntimeOwnerKind::Agent,
                    agent_id,
                    pid,
                )),
                worktree_path: Some(dir.path().to_string_lossy().into_owned()),
                worktree_branch: Some("main".to_owned()),
                prompt: Some("boot".to_owned()),
                description: None,
            },
        )
    };
    event_log::append(
        &paths.events_log,
        &launch("sess-live", "lucid-atlas", std::process::id()),
    )
    .expect("append live launch");
    event_log::append(
        &paths.events_log,
        &launch("sess-dead", "solid-lumen", u32::MAX),
    )
    .expect("append dead launch");
    event_log::append(
        &paths.events_log,
        &EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            "rimz-test",
            "claude",
            "ReapedDead",
            &AgentLifecycleObservation::new(
                Some(AgentSessionId::from("sess-dead")),
                LifecycleSignal::Ended,
            ),
        ),
    )
    .expect("append end observation");

    store.rotate_event_log(1, None).expect("rotate event log");

    let carryover = read_test_carryover(&paths.agents_carryover);
    let ids: Vec<&str> = carryover
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert!(
        ids.contains(&"sess-live"),
        "live-owner agent must survive rotation carryover: {ids:?}"
    );
    assert!(
        ids.contains(&"sess-dead"),
        "recent ended identity must survive rotation carryover: {ids:?}"
    );
    assert!(
        carryover
            .agents
            .iter()
            .any(|agent| agent.agent_id == "sess-dead" && agent.ended_at.is_some())
    );
}

#[test]
fn prune_carryover_drops_old_agents_without_live_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
    let store = Store::open(paths.clone(), runtime).expect("open store");
    let mut old = agent_state("claude", "old", Some("lucid-atlas"));
    old.last_seen = jiff::Timestamp::now() - Duration::from_secs(30 * 86_400);
    old.last_activity = old.last_seen;
    old.ended_at = Some(old.last_seen);
    let fresh = agent_state("claude", "fresh", Some("solid-lumen"));
    write_test_carryover(&paths.agents_carryover, vec![old, fresh]);

    let removed = store
        .prune_carryover(Duration::from_secs(14 * 86_400))
        .expect("prune carryover");

    assert_eq!(removed, 1);
    let carryover = read_test_carryover(&paths.agents_carryover);
    let ids: Vec<&str> = carryover
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, vec!["fresh"]);
}

#[test]
fn rotation_carryover_drops_consumed_launch_tombstones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
    let store = Store::open(paths.clone(), runtime).expect("open store");
    let kind = AgentKind::new_unchecked("claude");
    event_log::append(
        &paths.events_log,
        &EventEnvelope::agent_launched(
            workspace_id.clone(),
            "rimz-test",
            &kind,
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("launch_a"),
                launch_id: None,
                agent_name: "lucid-atlas".to_owned(),
                agent_name_explicit: false,
                launch: LaunchParams::default(),
                state: crate::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(dir.path().to_string_lossy().into_owned()),
                worktree_branch: Some("main".to_owned()),
                prompt: Some("boot".to_owned()),
                description: None,
            },
        ),
    )
    .expect("append launch");
    event_log::append(
        &paths.events_log,
        &EventEnvelope::new(
            workspace_id,
            "rimz-test",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            json!({
                "agent_id": "real-session",
                "agent_name": "lucid-atlas",
                "signal": { "signal": "registered" },
            }),
        ),
    )
    .expect("append lifecycle");

    store.rotate_event_log(1, None).expect("rotate event log");

    let carryover = std::fs::read_to_string(&paths.agents_carryover).expect("read carryover");
    assert!(carryover.contains("real-session"));
    assert!(
        !carryover.contains("consumed_launches"),
        "launch replay tombstones are active-log state and must not grow across rotations"
    );
}

#[test]
fn launch_identity_allocation_rejects_explicit_live_name_or_session_prefix() {
    let agents = vec![
        agent_state("claude", "sess-live-alpha", Some("lucid-atlas")),
        agent_state("claude", "prefix-session", Some("solid-lumen")),
    ];
    let duplicate = AgentLaunchRequest {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launch_a"),
        name: AgentLaunchName::Explicit("lucid-atlas".to_owned()),
        launch: LaunchParams::default(),
        run_id: None,
        prompt: None,
    };
    let prefix = AgentLaunchRequest {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launch_b"),
        name: AgentLaunchName::Explicit("prefix".to_owned()),
        launch: LaunchParams::default(),
        run_id: None,
        prompt: None,
    };

    assert!(allocate_agent_launch_identities(&[duplicate], &agents).is_err());
    assert!(allocate_agent_launch_identities(&[prefix], &agents).is_err());
}

#[test]
fn soft_launch_name_falls_back_when_it_collides() {
    let agents = vec![agent_state(
        "claude",
        "sess-live-alpha",
        Some("lucid-atlas"),
    )];
    let request = AgentLaunchRequest {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launch_a"),
        name: AgentLaunchName::Soft("lucid-atlas".to_owned()),
        launch: LaunchParams::default(),
        run_id: None,
        prompt: None,
    };

    let identities = allocate_agent_launch_identities(&[request], &agents).unwrap();

    assert_eq!(identities.len(), 1);
    assert_ne!(identities[0].name, "lucid-atlas");
    assert!(crate::harness::petname::valid_agent_name(
        &identities[0].name
    ));
}

#[test]
fn launch_identity_tracks_explicit_name_provenance() {
    let agents = Vec::new();
    let requests = [
        launch_request(
            "launch_explicit",
            AgentLaunchName::Explicit("writer".to_owned()),
        ),
        launch_request("launch_soft", AgentLaunchName::Soft("docs".to_owned())),
        launch_request("launch_mint", AgentLaunchName::Mint),
    ];

    let identities = allocate_agent_launch_identities(&requests, &agents).unwrap();

    assert_eq!(identities[0].name, "writer");
    assert!(identities[0].name_explicit);
    assert_eq!(identities[1].name, "docs");
    assert!(!identities[1].name_explicit);
    assert!(crate::harness::petname::valid_agent_name(
        &identities[2].name
    ));
    assert!(!identities[2].name_explicit);
}

fn launch_request(id: &str, name: AgentLaunchName) -> AgentLaunchRequest {
    AgentLaunchRequest {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from(id),
        name,
        launch: LaunchParams::default(),
        run_id: None,
        prompt: None,
    }
}

fn agent_state(kind: &str, id: &str, name: Option<&str>) -> AgentState {
    let now = jiff::Timestamp::now();
    AgentState {
        name: name.map(ToOwned::to_owned),
        kind_ordinal: Some(1),
        ..crate::testkit::agent_state(kind, id, now)
    }
}
