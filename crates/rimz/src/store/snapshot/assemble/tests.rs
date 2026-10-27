use std::time::SystemTime;

use super::*;

use crate::agents::AgentStatus;
use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentLifecycleObservation, LaunchParams};
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::store::event::EventEnvelope;

fn write_workspace_record(paths: &StatePaths, project_root: PathBuf, root_class: RootClass) {
    record::write(
        paths,
        &record::WorkspaceRecord {
            layout: 2,
            workspace_id: paths.workspace_id.clone(),
            project_root,
            worktree_root: None,
            session_name: "rimz-legacy".to_owned(),
            root_class,
            rimz_bin: None,
            rimz_build: None,
            pins: Default::default(),
            remote_control_off: Default::default(),
            agent_shell: None,
            skipped: Vec::new(),
            updated_at: Timestamp::UNIX_EPOCH,
        },
    )
    .expect("write workspace record");
}

#[test]
fn display_name_normalizes_a_legacy_dotted_root() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace, dir.path()).expect("state paths");
    paths.ensure_dirs().expect("state dirs");
    write_workspace_record(
        &paths,
        PathBuf::from("/srv/projects/rimz/child/.."),
        RootClass::Repo,
    );

    let identity = WorkspaceSnapshotIdentity::from_paths(&paths);
    assert_eq!(identity.display_name, "rimz");
    assert_eq!(
        identity.project_root.as_deref(),
        Some(std::path::Path::new("/srv/projects/rimz"))
    );
}

#[test]
fn display_name_falls_back_to_the_id_for_a_root_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).expect("state paths");
    paths.ensure_dirs().expect("state dirs");
    write_workspace_record(&paths, PathBuf::from("/tmp/.."), RootClass::Directory);

    let identity = WorkspaceSnapshotIdentity::from_paths(&paths);
    assert_eq!(identity.display_name, workspace.as_str());
    assert_eq!(identity.project_root, Some(PathBuf::from("/")));
    assert_eq!(identity.root_class, RootClass::Directory);
}

#[test]
fn build_applies_workspace_identity_from_one_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace, dir.path()).expect("state paths");
    paths.ensure_dirs().expect("state dirs");
    let project_root = PathBuf::from("/srv/projects/identity");
    write_workspace_record(&paths, project_root.clone(), RootClass::Marker);

    let snapshot = build_from(&paths).expect("build snapshot");

    assert_eq!(snapshot.display_name, "identity");
    assert_eq!(snapshot.project_root, Some(project_root));
    assert_eq!(snapshot.root_class, RootClass::Marker);
}

#[test]
fn workspace_identity_reuses_the_parse_until_the_record_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    write_workspace_record(&paths, PathBuf::from("/srv/root-a"), RootClass::Repo);
    let first = WorkspaceSnapshotIdentity::from_paths(&paths);
    let warm = WorkspaceSnapshotIdentity::from_paths(&paths);
    assert!(
        Arc::ptr_eq(&first, &warm),
        "a warm identity read must reuse its parse"
    );

    write_workspace_record(&paths, PathBuf::from("/srv/root-b"), RootClass::Marker);
    let replaced = WorkspaceSnapshotIdentity::from_paths(&paths);
    assert!(!Arc::ptr_eq(&first, &replaced));
    assert_eq!(
        replaced.project_root.as_deref(),
        Some(std::path::Path::new("/srv/root-b"))
    );
    assert_eq!(replaced.root_class, RootClass::Marker);
}

#[test]
fn missing_or_malformed_workspace_record_uses_identity_fallback() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).expect("state paths");
    paths.ensure_dirs().expect("state dirs");

    for corrupt in [false, true] {
        if corrupt {
            std::fs::write(&paths.workspace_record, b"not json").expect("corrupt record");
        }
        let identity = WorkspaceSnapshotIdentity::from_paths(&paths);
        assert_eq!(identity.display_name, workspace.as_str());
        assert_eq!(identity.project_root, None);
        assert_eq!(identity.root_class, RootClass::Repo);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn build_from_expels_dead_pid_agent_like_the_live_read() {
    // `latest.json` is written by `build_from`; it must apply the same
    // runtime liveness expel as `Store::snapshot` (`runtime_projection`),
    // or serving it O(1) would resurrect a dead-pid agent the live read
    // suppresses. A live (ownerless, abstaining) agent must survive, so the
    // filter expels without over-filtering.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    // No captured pid: the owner is unknown, so the agent abstains and stays.
    let alive = lifecycle(&workspace, "sess-live", None);
    // A pid that cannot be live (u32::MAX): the rollup derives a dead owner,
    // which the runtime expel must suppress.
    let dead = lifecycle(&workspace, "sess-dead", Some(u32::MAX));
    let ended_start = lifecycle(&workspace, "sess-ended", None);
    let ended = EventEnvelope::agent_lifecycle(
        workspace.clone(),
        "session",
        "claude",
        "SessionEnd",
        &AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-ended")),
            LifecycleSignal::Ended,
        ),
    );
    event_log::append(&paths.events_log, &alive).unwrap();
    event_log::append(&paths.events_log, &dead).unwrap();
    event_log::append(&paths.events_log, &ended_start).unwrap();
    event_log::append(&paths.events_log, &ended).unwrap();

    let snapshot = build_from(&paths).unwrap();
    let ids: Vec<&str> = snapshot
        .agents
        .iter()
        .map(|a| a.agent_id.as_str())
        .collect();
    assert!(
        ids.contains(&"sess-live"),
        "an ownerless (abstaining) agent must survive: {ids:?}"
    );
    assert!(
        !ids.contains(&"sess-dead"),
        "a dead-pid agent must be expelled so latest.json matches the live read: {ids:?}"
    );
    assert!(
        !ids.contains(&"sess-ended"),
        "an ended durable row must stay out of latest.json: {ids:?}"
    );
}

#[test]
fn assemble_keeps_ended_launched_child_on_live_parent_card() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace, dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    let now = Timestamp::now();
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%parent");
    let mut pane = PaneRef::from_id(pane_id);
    pane.command = Some("claude".to_owned());
    pane.cwd = Some(dir.path().display().to_string());
    let mut parent = crate::testkit::agent_state("claude", "parent", now);
    parent.pane = Some(pane.clone());
    parent.worktree_path = pane.cwd.clone();

    let mut child = crate::testkit::agent_state("codex", "child", now);
    child.status = AgentStatus::Success;
    child.ended_at = Some(now);
    child.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "child",
        u32::MAX,
        None,
    ));
    child.parent_agent_kind = Some(parent.kind.clone());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.launch_depth = Some(1);

    let snapshot = assemble_snapshot(
        &paths,
        event_log::LogExtent {
            generation: 0,
            offset: 0,
        },
        &[parent, child],
        Vec::new(),
    )
    .unwrap()
    .with_live_panes(vec![pane], None);
    let card = snapshot
        .rows()
        .find(|row| row.id == "parent")
        .expect("live parent card");
    let children = card.sub_agents();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, "child");
    assert_eq!(children[0].status, AgentStatus::Success);
}

#[cfg(target_os = "linux")]
#[test]
fn build_from_fences_dead_pid_agent_before_reap() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    event_log::append(
        &paths.events_log,
        &lifecycle(&workspace, "sess-dead", Some(u32::MAX)),
    )
    .unwrap();

    let snapshot = build_from(&paths).unwrap();
    let dead_key = (
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-dead"),
    );
    assert!(snapshot.fenced_sessions.contains(&dead_key));
    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.agent_id != "sess-dead")
    );
}

#[test]
fn published_snapshot_carries_fenced_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    let live = lifecycle(&workspace, "sess-live", None);
    let ended_start = lifecycle(&workspace, "sess-ended", None);
    let ended = EventEnvelope::agent_lifecycle(
        workspace.clone(),
        "session",
        "claude",
        "SessionEnd",
        &AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-ended")),
            LifecycleSignal::Ended,
        ),
    );
    event_log::append(&paths.events_log, &live).unwrap();
    event_log::append(&paths.events_log, &ended_start).unwrap();
    event_log::append(&paths.events_log, &ended).unwrap();

    let ended_key = (
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-ended"),
    );
    let live_key = (
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-live"),
    );
    let snapshot = build_from(&paths).unwrap();
    assert!(snapshot.fenced_sessions.contains(&ended_key));
    assert!(!snapshot.fenced_sessions.contains(&live_key));

    rebuild(&paths).unwrap();
    let published = read_fresh_latest(&paths).expect("published snapshot is fresh");
    assert!(published.fenced_sessions.contains(&ended_key));
    assert!(!published.fenced_sessions.contains(&live_key));
    assert_eq!(published.agents, snapshot.agents);
    assert_eq!(published.agent_panes, snapshot.agent_panes);
    assert_eq!(published.resume_outcomes, snapshot.resume_outcomes);
}

#[test]
fn populated_snapshot_preserves_json_round_trip() {
    use crate::store::snapshot::testkit::{epoch, room_with_agent_panes};
    let mut snapshot =
        room_with_agent_panes(vec![crate::testkit::agent_state("claude", "live", epoch())]);
    snapshot.resume_outcomes = Some(
        vec![ResumeOutcome {
            message_id: crate::ids::MessageId::new(),
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("ended"),
            agent_name: Some("ended".into()),
            status: crate::store::message::MessageStatus::Delivered,
            enqueued_at: epoch(),
            updated_at: epoch(),
        }]
        .into(),
    );
    Arc::make_mut(&mut snapshot.fenced_sessions).insert((
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("ended"),
    ));
    snapshot.worktree_roots.push(PathBuf::from("/tmp/x"));
    assert!(!snapshot.agent_panes.is_empty());
    assert_eq!(snapshot.rows().count(), 1);
    let bytes = serde_json::to_vec(&snapshot).unwrap();
    let decoded: SidebarSnapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&decoded).unwrap());
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for name in [
        "agents",
        "agent_panes",
        "resume_outcomes",
        "fenced_sessions",
        "worktree_roots",
    ] {
        assert!(json[name].is_array(), "{name} keeps its array wire shape");
        assert_eq!(json[name].as_array().unwrap().len(), 1);
    }
    assert!(json["worktree_groups"][0]["rows"].is_array());
}

#[test]
fn read_fresh_latest_serves_only_when_it_reflects_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    // Absent `latest.json`: nothing to serve, so the caller re-projects.
    assert!(read_fresh_latest(&paths).is_none());

    // Seed an event log and a rebuilt `latest.json`.
    event_log::append(&paths.events_log, &lifecycle(&workspace, "a", None)).unwrap();
    rebuild(&paths).unwrap();

    // The published stamp claims exactly the live log's length → served O(1).
    assert!(
        read_fresh_latest(&paths).is_some(),
        "stamp matches the live log → serve the published view"
    );

    // A write raced the read: the log moved past the stamp → stale, so the
    // guard declines and the caller folds the delta itself. Backdating the
    // log's mtime proves mtime carries no authority — only the stamp does.
    event_log::append(&paths.events_log, &lifecycle(&workspace, "b", None)).unwrap();
    std::fs::File::open(&paths.events_log)
        .unwrap()
        .set_modified(SystemTime::now() - std::time::Duration::from_secs(60))
        .unwrap();
    assert!(
        read_fresh_latest(&paths).is_none(),
        "log outran the stamp → a just-appended event is unreflected; re-project"
    );

    // Republishing catches the stamp up; the guard serves again, on this
    // thread's warm parse cache too: the republish is a fresh inode, so
    // even a same-tick, equal-length rename never serves the prior parse.
    rebuild(&paths).unwrap();
    assert!(
        read_fresh_latest(&paths).is_some(),
        "republish reflects the appended event → served again"
    );
}

#[test]
fn read_fresh_latest_rejects_version_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(&paths.events_log, &lifecycle(&workspace, "a", None)).unwrap();
    rebuild(&paths).unwrap();
    assert!(
        read_fresh_latest(&paths).is_some(),
        "rebuilt snapshots carry the current version"
    );

    let bytes = std::fs::read(&paths.latest_snapshot).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    legacy["snapshot_version"] = serde_json::json!(56);
    atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &legacy).unwrap();

    // Rewriting the version keeps the byte length identical. A production
    // version change is always a cold-cache event — a new binary rebuilds
    // from scratch — so a cold reader is the faithful check that the
    // on-disk mismatch is rejected.
    assert!(
        read_fresh_latest_cold(&paths).is_none(),
        "old latest.json with a mismatched version is not fresh"
    );
    let rebuilt = build_from(&paths).unwrap();
    assert!(
        rebuilt.snapshot_version == SNAPSHOT_VERSION,
        "fallback rebuild stamps the current snapshot version"
    );
}

#[test]
fn pre_molt_latest_snapshot_refolds_the_succession_link() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(&paths.events_log, &EventEnvelope::new(
        workspace, "session", "claude", "agent", "agent.launched",
        serde_json::json!({"agent_id": "launch_new", "agent_name": "successor", "molted_from": "launch_old"}),
    )).unwrap();
    let mut legacy = rebuild(&paths).unwrap();
    // The last version written without `molted_from`.
    legacy.snapshot_version = 39;
    for agent in Arc::make_mut(&mut legacy.agents) {
        agent.molted_from = None;
    }
    atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &legacy).unwrap();
    assert!(
        read_fresh_latest_cold(&paths).is_none(),
        "a pre-molt latest.json cannot serve a lost succession link"
    );
    let rebuilt = build_from(&paths).unwrap();
    assert_eq!(
        rebuilt
            .agents
            .iter()
            .find(|agent| agent.agent_id == "launch_new")
            .unwrap()
            .molted_from
            .as_deref(),
        Some("launch_old")
    );
}

#[test]
fn pre_supersession_latest_snapshot_reprojects_the_molt_chain() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    for (id, name, predecessor, pane) in [
        ("launch_old", "predecessor", None, "tmux:%3"),
        ("launch_new", "successor", Some("launch_old"), "tmux:%4"),
    ] {
        event_log::append(&paths.events_log, &EventEnvelope::new(
            workspace.clone(), "session", "claude", "agent", "agent.launched",
            serde_json::json!({"agent_id": id, "agent_name": name, "molted_from": predecessor, "pane_id": pane}),
        )).unwrap();
    }
    let snapshot = rebuild(&paths).unwrap();
    assert_eq!(snapshot.agents.len(), 2);
    let mut legacy = serde_json::to_value(snapshot).unwrap();
    legacy["snapshot_version"] = serde_json::json!(41);
    for agent in legacy["agents"].as_array_mut().unwrap() {
        agent.as_object_mut().unwrap().remove("molt_superseded");
    }
    atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &legacy).unwrap();
    assert!(
        read_fresh_latest_cold(&paths).is_none(),
        "a pre-42 latest.json cannot restore a superseded role holder"
    );
    let rebuilt = build_from(&paths).unwrap();
    assert!(
        rebuilt
            .agents
            .iter()
            .find(|agent| agent.agent_id == "launch_old")
            .unwrap()
            .molt_superseded
    );
}

#[test]
fn pre_rewarm_estimate_snapshot_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace, dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut legacy = serde_json::to_value(rebuild(&paths).unwrap()).unwrap();
    legacy["snapshot_version"] = serde_json::json!(42);
    atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &legacy).unwrap();
    assert!(
        read_fresh_latest_cold(&paths).is_none(),
        "pre-estimate snapshots must be reprojected"
    );
}

#[test]
fn a_reaped_unended_molt_successor_keeps_its_predecessor_superseded() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace, dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let now = Timestamp::now();
    let mut predecessor = crate::testkit::agent_state("claude", "predecessor", now);
    predecessor.launch_id = Some("launch_old".into());
    predecessor.role = Some("reviewer".to_owned());
    predecessor.pane = Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, "%3")));
    let stale = Timestamp::from_second(
        now.as_second() - crate::store::session_death::GHOST_SESSION_TTL_SECS - 1,
    )
    .unwrap();
    let mut successor = crate::testkit::agent_state("claude", "successor", stale);
    successor.launch_id = Some("launch_new".into());
    successor.molted_from = predecessor.launch_id.clone();
    successor.role = predecessor.role.clone();
    successor.pane = Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, "%4")));

    let snapshot = assemble_snapshot(
        &paths,
        event_log::LogExtent {
            generation: 0,
            offset: 0,
        },
        &[predecessor, successor],
        Vec::new(),
    )
    .unwrap();

    assert_eq!(
        snapshot
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        ["predecessor"]
    );
    assert!(crate::address::role_holders(snapshot.agents.iter(), "reviewer").is_empty());
}

#[test]
fn pre_workflow_snapshot_is_not_a_fresh_view() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace, dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let snapshot = rebuild(&paths).unwrap();
    let mut legacy = serde_json::to_value(snapshot).unwrap();
    legacy["snapshot_version"] = serde_json::json!(44);
    atomic::write_temp_then_rename_cache(&paths.latest_snapshot, &legacy).unwrap();
    assert!(
        read_fresh_latest_cold(&paths).is_none(),
        "a pre-workflow view must be refolded"
    );
}

/// [`read_fresh_latest`] on a fresh thread, past this thread's parse
/// cache: the reader a new binary starts with, for a check whose
/// production path is always a cold-cache event.
fn read_fresh_latest_cold(paths: &StatePaths) -> Option<SidebarSnapshot> {
    std::thread::scope(|scope| scope.spawn(|| read_fresh_latest(paths)).join().unwrap())
}

fn lifecycle(workspace: &WorkspaceId, agent_id: &str, agent_pid: Option<u32>) -> EventEnvelope {
    let observation = AgentLifecycleObservation {
        on_screen: None,
        state_snapshot: None,
        ask_queue: None,
        agent_id: Some(AgentSessionId::from(agent_id)),
        agent_name: None,
        launch: LaunchParams::default(),
        signal: LifecycleSignal::Registered,
        agent_pid,
        account_key: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        description: None,
        transcript_path: None,
        origin: None,
        compacted_from: None,
        usage: crate::agents::AgentUsageSummary::default(),
        pane_id: None,
        pane_stamp: None,
        parent_agent_id: None,
        background_shells: None,
    };
    EventEnvelope::agent_lifecycle(
        workspace.clone(),
        "session",
        "claude",
        "SessionStart",
        &observation,
    )
}
