use super::*;
use jiff::Timestamp;

#[test]
fn only_the_parent_can_resolve_an_ended_child_for_resume() {
    let parent = AgentState::stub("claude", "parent", rimz::agents::AgentStatus::Running);
    let peer = AgentState::stub("claude", "peer", rimz::agents::AgentStatus::Running);
    let mut child = AgentState::stub("codex", "child", rimz::agents::AgentStatus::Success);
    child.name = Some("otter".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.ended_at = Some(Timestamp::now());
    let mut agents = vec![parent.clone(), peer.clone(), child];
    assert!(ended_child(&agents, &parent, "@otter", None, None).is_some());
    assert!(ended_child(&agents, &peer, "@otter", None, None).is_none());
    assert!(ended_child(&agents, &agents[2], "@otter", None, None).is_none());
    assert!(ended_child(&agents, &parent, "@missing", None, None).is_none());
    agents[2].ended_at = None;
    assert!(ended_child(&agents, &parent, "@otter", None, None).is_none());
}

#[test]
fn resumed_child_reuses_newest_run_and_preserves_keep_and_lineage() {
    let mut child = AgentState::stub("claude", "child", rimz::agents::AgentStatus::Success);
    child.name = Some("otter".to_owned());
    child.launch_id = Some("launch_child".into());
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    child.worktree_path = Some("/tmp/project".to_owned());
    let posture = rimz::harness::resume::resolve_posture(
        PostureRequest {
            profile: None,
            kind: &child.kind,
            stamped_mode: None,
        },
        &Default::default(),
    );
    let mut older = RunRecord::new(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/project")),
        child.kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "task".to_owned(),
        PathBuf::from("/tmp/project"),
    );
    older.agent_id = Some(child.agent_id.clone());
    older.started_at = "2026-01-01T00:00:00Z".parse().unwrap();
    let mut newer = older.clone();
    newer.run_id = rimz::RunId::new();
    newer.started_at = "2026-01-01T00:01:00Z".parse().unwrap();
    for keep in [false, true] {
        newer.keep = keep;
        let runs = [newer.clone(), older.clone()];
        let run = newest_run_for_child(&runs, &child).unwrap();
        let request = resume_request(
            &child,
            run,
            &posture,
            ExecAction::Resume {
                session_id: child.agent_id.to_string(),
                extra_args: Vec::new(),
            },
        );
        assert!(
            matches!(request.action, ExecAction::Resume { ref session_id, .. } if session_id == child.agent_id.as_str())
        );
        assert_eq!(request.run_id.as_ref(), Some(&newer.run_id));
        assert!(request.subagent);
        assert_eq!(request.close_pane_on_exit, !keep);
        assert_eq!(request.exit_on_run_completion, !keep);
        assert!(
            request.worktree_path.is_none(),
            "resume must not own checkout cleanup"
        );
        assert_eq!(request.identity.name, child.name);
        assert!(request.identity.name_explicit);
        assert_eq!(
            request.identity.launch_id,
            child.launch_id.as_ref().map(ToString::to_string)
        );
        assert_eq!(
            request.identity.params.parent_agent_id,
            child.parent_agent_id
        );
        assert_eq!(
            request.identity.params.parent_agent_kind,
            child.parent_agent_kind
        );
    }
}
