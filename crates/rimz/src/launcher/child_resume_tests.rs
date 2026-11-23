use super::*;
use crate::harness::launch::ExecIdentity;
use jiff::Timestamp;

#[test]
fn only_the_parent_can_resolve_an_ended_child_for_resume() {
    let parent = AgentState::stub("claude", "parent", crate::agents::AgentStatus::Running);
    let peer = AgentState::stub("claude", "peer", crate::agents::AgentStatus::Running);
    let mut child = AgentState::stub("codex", "child", crate::agents::AgentStatus::Success);
    child.name = Some("otter".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.ended_at = Some(Timestamp::now());
    let mut agents = vec![parent.clone(), peer.clone(), child];
    let context = crate::address::AddressContext {
        channel: None,
        origin: crate::address::ChannelOrigin::Stamped,
    };
    assert!(ended_child(&agents, &parent, "@otter", None, &context).is_some());
    assert!(ended_child(&agents, &peer, "@otter", None, &context).is_none());
    assert!(ended_child(&agents, &agents[2], "@otter", None, &context).is_none());
    assert!(ended_child(&agents, &parent, "@missing", None, &context).is_none());
    agents[2].ended_at = None;
    assert!(ended_child(&agents, &parent, "@otter", None, &context).is_none());
}

#[test]
fn resumed_child_reuses_newest_run_and_preserves_keep_and_lineage() {
    let mut child = AgentState::stub("claude", "child", crate::agents::AgentStatus::Success);
    child.name = Some("otter".to_owned());
    child.launch_id = Some("launch_child".into());
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    child.worktree_path = Some("/tmp/project".to_owned());
    child.record = Some(Box::new(
        serde_json::from_value(serde_json::json!({
            "model": null, "effort": null, "agent": null,
            "loop_section": "The original loop fire"
        }))
        .unwrap(),
    ));
    let mut posture = crate::harness::resume::resolve_posture(
        PostureRequest {
            record: child.record.as_deref(),
            profile: None,
            kind: &child.kind,
            stamped_mode: None,
            stamped_tier: None,
        },
        &Default::default(),
    );
    posture.launch.args = vec!["--model".to_owned(), "opus".to_owned()];
    posture.launch.system_prompt_file = Some("/prompts/coder.md".into());
    posture.launch.append_system_prompt_files = vec!["/prompts/extra.md".into()];
    posture.launch.team_prompt = Some(crate::harness::team_prompt::TeamPrompt {
        consensus: crate::harness::team_prompt::Consensus::BuiltIn,
        files: vec!["/prompts/team.md".into()],
    });
    posture.launch.skills = Some(vec!["merge".parse().unwrap()]);
    posture.launch.allowed_tools = Some(vec!["Bash(git *)".parse().unwrap()]);
    posture.launch.isolation_default = Some(crate::config::Isolation::Scoped);
    let mut older = RunRecord::new(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/project")),
        child.kind.clone(),
        crate::agents::PermissionMode::Auto,
        "task".to_owned(),
        PathBuf::from("/tmp/project"),
    );
    older.agent_id = Some(child.agent_id.clone());
    older.started_at = "2026-01-01T00:00:00Z".parse().unwrap();
    let mut newer = older.clone();
    newer.run_id = crate::RunId::new();
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
            Some("work".parse().unwrap()),
            None,
        );
        assert_eq!(
            request,
            ExecRequest {
                kind: child.kind.clone(),
                action: ExecAction::Resume {
                    session_id: child.agent_id.to_string(),
                    extra_args: posture.launch.args.clone(),
                },
                system_prompt_file: posture.launch.system_prompt_file.clone(),
                append_system_prompt_files: posture.launch.append_system_prompt_files.clone(),
                team_prompt: posture.launch.team_prompt.clone(),
                skills: posture.launch.skills.clone(),
                mcps: None,
                no_skill_tool: posture.launch.no_skill_tool,
                rimz_capabilities: posture.launch.rimz_capabilities.clone(),
                allowed_tools: posture.launch.allowed_tools.clone(),
                tools: posture.launch.tools.clone(),
                isolation_default: posture.launch.isolation_default,
                provider_account: crate::harness::launch::ProviderAccountState::Unbound,
                run_id: Some(newer.run_id.clone()),
                worktree_path: None,
                close_pane_on_exit: !keep,
                exit_on_run_completion: !keep,
                subagent: true,
                continue_session: false,
                follow_up_message: None,
                reminder_section: Some(crate::harness::launch::ReminderSection::Loop(
                    "The original loop fire".into()
                )),
                identity: ExecIdentity {
                    row_key: None,
                    resume_model_override: false,
                    name: Some("otter".to_owned()),
                    name_explicit: true,
                    launch_id: Some("launch_child".to_owned()),
                    params: crate::agents::LaunchParams {
                        parent_agent_id: child.parent_agent_id.clone(),
                        parent_agent_kind: child.parent_agent_kind.clone(),
                        launch_depth: Some(1),
                        login: Some("work".parse().unwrap()),
                        mode: posture.launch.mode,
                        record: posture.launch.record.clone(),
                        lane: child.lane.clone(),
                        ..Default::default()
                    },
                },
            }
        );
    }
}
