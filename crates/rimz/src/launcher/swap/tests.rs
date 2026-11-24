use super::*;
use std::path::PathBuf;

#[test]
fn a_briefing_lists_what_is_outstanding_and_nothing_when_nothing_is() {
    assert_eq!(briefing_section(&[], &[], &[], 0, 0), None);
    let section = briefing_section(&["@otter".into()], &[], &["ci-green".into()], 2, 1).unwrap();
    assert_eq!(
        section,
        "Type: SWAP_BRIEFING\nFrom: @rimz\nContent:\nThis is a fresh session of the same agent; \
         the earlier conversation is gone, and RimZ kept what was outstanding for it.\n\
         - 1 subagent still running (@otter); their results come to you: `rimz subagents list`\n\
         - 1 wait or delivery still armed for you (ci-green): `rimz wait list`\n\
         - 2 messages queued for your next turn: `rimz message list`\n\
         Sent into the earlier session and not yet confirmed as read; RimZ sends a \
         message again, a command it does not:\n\
         - 1 message: `rimz message list`\n"
    );
    let origins = crate::store::message::classify_submitted_prompt(&section, &[], &[])
        .into_iter()
        .map(|part| part.origin)
        .collect::<Vec<_>>();
    assert_eq!(
        origins,
        [crate::transcript::SectionOrigin::Notice("@rimz".into())],
        "the briefing reads as an @rimz notice, never as the user"
    );
}

#[test]
fn a_briefing_counts_owed_child_answers_and_only_conversation_messages() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(dir.path());
    let store = crate::Store::open(
        crate::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
        crate::RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    let parent = crate::testkit::agent_state("claude", "parent", jiff::Timestamp::now());
    let child = ended_child(&store, "child", "kit", "parent", 1);
    let mut run = child_run(&child);
    run.status = crate::store::run::RunStatus::Completed;
    crate::harness::run::create(store.paths(), &run).unwrap();
    let message = |text: &str| {
        crate::store::message::MessageRecord::new_for_card(
            workspace_id.clone(),
            parent.kind.clone(),
            parent.agent_id.clone(),
            None,
            text.to_owned(),
            crate::store::message::DeliveryGate::Done,
        )
    };
    let mut notice = message("a system notice");
    notice.sender = crate::store::message::MessageSender::System;
    for queued in [message("rebase first"), notice] {
        store.queue_message(&queued, "session").unwrap();
    }
    let briefing = swap_briefing(
        &store,
        &parent,
        std::slice::from_ref(&parent),
        dir.path(),
        None,
    )
    .unwrap()
    .unwrap();
    let lines = briefing.lines().filter(|line| line.starts_with("- "));
    assert_eq!(
        lines.collect::<Vec<_>>(),
        [
            "- 1 subagent finished with its answer still owed to you (@kit): \
             `rimz subagents list`",
            "- 1 message queued for your next turn: `rimz message list`",
        ]
    );
}

/// A depth-1 subagent's Runtime rows drop its ended grandchild, whose parent
/// is not a visible root; the briefing still lists the answer it owes.
#[test]
fn a_subagents_briefing_lists_an_ended_grandchilds_owed_answer() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(dir.path());
    let store = crate::Store::open(
        crate::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
        crate::RuntimePaths::under(workspace_id, &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    observe_child(
        &store,
        "sub",
        "otter",
        "root",
        1,
        0,
        crate::agents::LifecycleSignal::Registered,
    );
    let parent = store
        .audit_rows()
        .unwrap()
        .iter()
        .find(|agent| agent.agent_id == "sub")
        .unwrap()
        .clone();
    let grandchild = ended_child(&store, "grand", "kit", "sub", 2);
    let mut run = child_run(&grandchild);
    run.status = crate::store::run::RunStatus::Completed;
    crate::harness::run::create(store.paths(), &run).unwrap();
    let briefing = swap_briefing(
        &store,
        &parent,
        std::slice::from_ref(&parent),
        dir.path(),
        None,
    )
    .unwrap()
    .unwrap_or_default();
    assert!(
        briefing.contains("answer still owed to you (@kit)"),
        "{briefing}"
    );
}

/// A child swapped under its launch keeps its replaced row beside its holder,
/// and both match its run by name: the run's answer is owed by a finished
/// child only once the holder has ended too.
#[test]
fn a_briefing_lists_a_swapped_childs_answer_as_owed_only_once_its_holder_ended() {
    use crate::agents::LifecycleSignal::{Ended, Registered};

    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(dir.path());
    let store = crate::Store::open(
        crate::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
        crate::RuntimePaths::under(workspace_id, &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    let parent = crate::testkit::agent_state("claude", "parent", jiff::Timestamp::now());
    let launch = |session: &str, epoch: u32| {
        let payload = serde_json::from_value(serde_json::json!({
            "agent_id": session, "agent_name": "kit", "launch_id": "launch-kit",
            "state": "bound", "epoch": epoch, "parent_agent_id": "parent",
            "parent_agent_kind": "claude", "launch_depth": 1,
        }))
        .unwrap();
        store
            .append_event(&crate::store::event::EventEnvelope::agent_launched(
                store.paths().workspace_id.clone(),
                "session",
                &crate::ids::AgentKind::new_unchecked("claude"),
                payload,
            ))
            .unwrap();
    };
    launch("replaced", 0);
    observe_child(&store, "replaced", "kit", "parent", 1, 0, Registered);
    observe_child(&store, "replaced", "kit", "parent", 1, 0, Ended);
    launch("holder", 1);
    observe_child(&store, "holder", "kit", "parent", 1, 1, Registered);
    let holder = store
        .audit_rows()
        .unwrap()
        .iter()
        .find(|agent| agent.agent_id == "holder")
        .unwrap()
        .clone();
    let mut run = child_run(&holder);
    run.agent_name = Some("kit".into());
    run.status = crate::store::run::RunStatus::Completed;
    crate::harness::run::create(store.paths(), &run).unwrap();
    let owed = || {
        swap_briefing(
            &store,
            &parent,
            std::slice::from_ref(&parent),
            dir.path(),
            None,
        )
        .unwrap()
        .unwrap_or_default()
        .contains("answer still owed to you (@kit)")
    };
    assert!(
        !owed(),
        "the live holder's answer is not a finished child's"
    );
    observe_child(&store, "holder", "kit", "parent", 1, 1, Ended);
    assert!(owed(), "once its holder ended, the child's answer is owed");
}

#[test]
fn restart_cell_carries_the_current_postures_auto_compact_count() {
    let mut agent = crate::testkit::agent_state("claude", "a1", jiff::Timestamp::UNIX_EPOCH);
    agent.profile = Some("worker".into());
    let profiles = serde_json::from_value(serde_json::json!({
        "worker": {"agent": "claude", "auto-compact": "372k"}
    }))
    .unwrap();
    let posture = crate::harness::resume::resolve_posture(
        crate::harness::resume::PostureRequest {
            record: None,
            profile: agent.profile.as_deref(),
            kind: &agent.kind,
            stamped_mode: None,
            stamped_tier: None,
        },
        &profiles,
        None,
    );
    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    let Cell::Agent(cell) = restart_cell(&agent, &posture) else {
        panic!("agent cell")
    };
    let request = fresh_launch_request(&agent, Cell::Agent(cell.clone()), None).unwrap();
    assert_eq!(
        (
            cell.auto_compact.as_deref(),
            serde_json::to_value(request.launch.record.unwrap()).unwrap()["auto_compact"].as_u64(),
        ),
        (Some("372000"), Some(372_000))
    );
    assert!(
        cell.args
            .windows(2)
            .any(|args| args == ["--autocompact", "372000"])
    );
}

#[test]
fn restart_requests_preserve_automation_for_resumed_and_fresh_sessions() {
    let stamp = serde_json::json!({"by": "workflow", "workflow": "land", "step": "triage"});
    let mut value = serde_json::to_value(crate::testkit::agent_state(
        "claude",
        "a1",
        jiff::Timestamp::UNIX_EPOCH,
    ))
    .unwrap();
    value["automation"] = stamp.clone();
    let agent = serde_json::from_value(value).unwrap();
    let posture = ResumePosture {
        launch: Default::default(),
        degraded: None,
    };
    let mut actual: Vec<_> = [
        ExecAction::Resume {
            session_id: "a1".into(),
            extra_args: Vec::new(),
        },
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    ]
    .into_iter()
    .map(|action| {
        let request = relaunch_request(&agent, &posture, action, None, None);
        serde_json::to_value(request.identity.params).unwrap()["automation"].clone()
    })
    .collect();
    let Cell::Agent(cell) = restart_cell(&agent, &posture) else {
        panic!("agent cell")
    };
    actual.push(serde_json::to_value(cell.launch).unwrap()["automation"].clone());
    assert_eq!(actual, vec![stamp.clone(), stamp.clone(), stamp]);
}

#[test]
fn relaunch_request_replays_loop_sections_without_replaying_supervision() {
    let mut agent = crate::testkit::agent_state("claude", "a1", jiff::Timestamp::UNIX_EPOCH);
    agent.name = Some("otter".to_owned());
    agent.role = Some("coder".to_owned());
    // A relaunch copies the lane without admitting it again, so a name a
    // worktree has since claimed stays the agent's lane.
    agent.lane = Some(
        serde_json::from_value(
            serde_json::json!({"home": "trunk", "path": "/repo", "name": "auth"}),
        )
        .expect("lane"),
    );
    // A restart replays the session's context, not the fire's supervision.
    agent.loop_task = Some("fixer".to_owned());
    agent.workflow = Some(crate::agents::WorkflowAttemptRef {
        run: crate::ids::WorkflowRunId::new(),
        seq: 1,
        role: crate::agents::WorkflowAttemptRole::Step,
    });
    let posture = ResumePosture {
        launch: crate::harness::plan::ResumeLaunchPosture {
            args: vec!["--model".to_owned(), "opus".to_owned()],
            system_prompt_file: Some("/prompts/coder.md".into()),
            append_system_prompt_files: vec!["/prompts/extra.md".into()],
            team_prompt: Some(crate::harness::team_prompt::TeamPrompt {
                consensus: crate::harness::team_prompt::Consensus::BuiltIn,
                files: vec!["/prompts/team.md".into()],
            }),
            skills: Some(vec!["merge".parse().unwrap()]),
            allowed_tools: Some(vec!["Read".parse().unwrap()]),
            isolation_default: Some(crate::config::Isolation::Scoped),
            model: Some("opus".to_owned()),
            record: Some(Box::new(
                serde_json::from_value(serde_json::json!({
                    "model": null, "effort": null, "agent": null,
                    "loop_section": "The original loop fire"
                }))
                .unwrap(),
            )),
            ..Default::default()
        },
        degraded: None,
    };
    let action = ExecAction::Resume {
        session_id: "a1".to_owned(),
        extra_args: Vec::new(),
    };

    let request = relaunch_request(&agent, &posture, action, None, None);
    let expected = Some(crate::harness::launch::ReminderSection::Loop(
        "The original loop fire".into(),
    ));
    let fresh = relaunch_request(
        &agent,
        &posture,
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        None,
        None,
    );
    assert_eq!(
        fresh.reminder_section, expected,
        "fresh fallback keeps the loop context"
    );
    assert_eq!(
        fresh.identity.params.lane, agent.lane,
        "fresh fallback keeps the lane"
    );

    let Cell::Agent(cell) = restart_cell(&agent, &posture) else {
        panic!("agent cell")
    };
    assert_eq!(cell.allowed_tools, posture.launch.allowed_tools);

    assert_eq!(
        request,
        ExecRequest {
            isolation_default: Some(crate::config::Isolation::Scoped),
            kind: agent.kind.clone(),
            action: ExecAction::Resume {
                session_id: "a1".to_owned(),
                extra_args: posture.launch.args.clone(),
            },
            system_prompt_file: posture.launch.system_prompt_file.clone(),
            append_system_prompt_files: posture.launch.append_system_prompt_files.clone(),
            team_prompt: posture.launch.team_prompt.clone(),
            skills: posture.launch.skills.clone(),
            seat_consensus_skill: posture.launch.seat_consensus_skill,
            mcps: None,
            no_skill_tool: posture.launch.no_skill_tool,
            rimz_capabilities: posture.launch.rimz_capabilities.clone(),
            allowed_tools: posture.launch.allowed_tools.clone(),
            tools: posture.launch.tools.clone(),
            provider_account: crate::harness::launch::ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            subagent: false,
            continue_session: false,
            follow_up_message: None,
            reminder_section: expected,
            identity: crate::harness::launch::ExecIdentity {
                row_key: None,
                resume_model_override: false,
                name: Some("otter".to_owned()),
                name_explicit: agent.name_explicit,
                launch_id: None,
                params: crate::agents::LaunchParams {
                    role: Some("coder".to_owned()),
                    model: Some("opus".to_owned()),
                    loop_task: Some("fixer".to_owned()),
                    lane: agent.lane.clone(),
                    record: posture.launch.record.clone(),
                    ..Default::default()
                },
            },
        }
    );
}

/// Append one lifecycle observation of a launched child of `parent`.
fn observe_child(
    store: &crate::Store,
    session: &str,
    name: &str,
    parent: &str,
    depth: u8,
    epoch: u32,
    signal: crate::agents::LifecycleSignal,
) {
    let kind = crate::ids::AgentKind::new_unchecked("claude");
    let mut observation =
        crate::agents::AgentLifecycleObservation::new(Some(session.into()), signal);
    observation.agent_name = Some(name.into());
    observation.launch_id = Some(format!("launch-{name}").into());
    observation.launch.epoch = Some(epoch);
    observation.launch.parent_agent_id = Some(parent.into());
    observation.launch.parent_agent_kind = Some(kind.clone());
    observation.launch.launch_depth = Some(depth);
    store
        .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
            session_name: "session",
            agent_kind: kind,
            event_name: "SessionStart",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

/// A launched child of `parent` that registered and ended, as the store holds it.
fn ended_child(
    store: &crate::Store,
    session: &str,
    name: &str,
    parent: &str,
    depth: u8,
) -> AgentState {
    for signal in [
        crate::agents::LifecycleSignal::Registered,
        crate::agents::LifecycleSignal::Ended,
    ] {
        observe_child(store, session, name, parent, depth, 0, signal);
    }
    store
        .audit_rows()
        .unwrap()
        .iter()
        .find(|agent| agent.agent_id == session)
        .unwrap()
        .clone()
}

fn child_run(child: &AgentState) -> crate::store::run::RunRecord {
    let mut run = crate::store::run::RunRecord::new(
        crate::ids::WorkspaceId::from_project_root(Path::new("/tmp")),
        child.kind.clone(),
        PermissionMode::Auto,
        "task".to_owned(),
        PathBuf::from("/tmp"),
    );
    run.agent_id = Some(child.agent_id.clone());
    run
}

#[test]
fn a_handed_over_child_carries_its_run_and_a_settled_one_carries_none() {
    let mut child = crate::testkit::agent_state("claude", "child", jiff::Timestamp::UNIX_EPOCH);
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    let request = relaunch_request(
        &child,
        &ResumePosture::default(),
        ExecAction::Resume {
            session_id: child.agent_id.to_string(),
            extra_args: Vec::new(),
        },
        None,
        None,
    );
    let summary = |request: &ExecRequest| {
        (
            request.subagent,
            request.run_id.clone(),
            request.close_pane_on_exit,
            request.exit_on_run_completion,
            request.follow_up_message.clone(),
        )
    };
    // Explain builds this request and never hands a run over; a molt adds only a leader's
    // workflow session run (`open_session_run`).
    assert_eq!(summary(&request), (true, None, true, false, None));

    let mut run = child_run(&child);
    let handed = with_run(request.clone(), &run);
    assert_eq!(
        summary(&handed),
        (true, Some(run.run_id.clone()), true, true, None)
    );
    run.keep = true;
    let kept = with_run(request, &run);
    assert_eq!(
        (
            kept.run_id,
            kept.close_pane_on_exit,
            kept.exit_on_run_completion
        ),
        (Some(run.run_id.clone()), false, false)
    );
}

#[test]
fn a_swap_hands_over_only_the_open_session_run_its_agent_leads() {
    use crate::store::run::{RunLifecycle, RunStatus};

    let mut leader = AgentState::stub("codex", "new", crate::agents::AgentStatus::Running);
    leader.name = Some("lead".to_owned());
    let attempt = crate::agents::WorkflowAttemptRef {
        run: crate::ids::WorkflowRunId::new(),
        seq: 2,
        role: crate::agents::WorkflowAttemptRole::Step,
    };
    leader.workflow = Some(attempt.clone());
    // The run still names the session a molt to another kind replaced.
    let mut run = child_run(&AgentState::stub(
        "claude",
        "old",
        crate::agents::AgentStatus::Idle,
    ));
    run.lifecycle = RunLifecycle::Session;
    run.status = RunStatus::Running;
    run.workflow = Some(attempt.clone());
    run.agent_name = leader.name.clone();
    let found = |run: &crate::store::run::RunRecord, agent: &AgentState| {
        open_session_run(std::slice::from_ref(run), agent).is_some()
    };
    assert!(found(&run, &leader));
    let mut companion = leader.clone();
    companion.name = Some("helper".to_owned());
    assert!(
        !found(&run, &companion),
        "a companion shares the pin, not the run"
    );
    let mut other = leader.clone();
    other.workflow = Some(crate::agents::WorkflowAttemptRef { seq: 3, ..attempt });
    assert!(!found(&run, &other), "another attempt's run");
    let mut turn = run.clone();
    turn.lifecycle = RunLifecycle::Turn;
    assert!(
        !found(&turn, &leader),
        "a turn step ends in its own session"
    );
    run.status = RunStatus::Completed;
    assert!(
        !found(&run, &leader),
        "a settled run stays where it settled"
    );
}

#[test]
fn relaunch_request_does_not_treat_a_peer_as_a_child() {
    let mut peer = crate::testkit::agent_state("claude", "peer", jiff::Timestamp::UNIX_EPOCH);
    peer.launch_depth = Some(1);
    peer.launched_by = Some(crate::agents::LaunchedBy {
        kind: peer.kind.clone(),
        agent_id: "launcher".into(),
    });

    let request = relaunch_request(
        &peer,
        &ResumePosture::default(),
        ExecAction::Resume {
            session_id: peer.agent_id.to_string(),
            extra_args: Vec::new(),
        },
        None,
        None,
    );

    assert!(!request.subagent);
}

#[test]
fn restart_posture_uses_the_profile_table_of_the_launch_doorway() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        crate::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine = crate::config::MachineConfig::default();
    for (profiles, tool) in [
        (&mut machine.agents.profiles, "Bash"),
        (&mut machine.subagents.profiles, "Read"),
    ] {
        let mut profile: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
        profile.allowed_tools = Some(vec![tool.parse().unwrap()]);
        profiles.0.insert("worker".to_owned(), profile);
    }
    let mut child = crate::testkit::agent_state("claude", "child", jiff::Timestamp::UNIX_EPOCH);
    child.profile = Some("worker".to_owned());
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    let mut peer = child.clone();
    peer.parent_agent_id = None;
    peer.parent_agent_kind = None;
    peer.launched_by = Some(crate::agents::LaunchedBy {
        kind: peer.kind.clone(),
        agent_id: "launcher".into(),
    });
    let mut top_level = peer.clone();
    top_level.launch_depth = None;
    top_level.launched_by = None;
    let mut fork = peer.clone();
    fork.profile_scope = Some(ProfileScope::Subagents);

    let tools = [&child, &peer, &top_level, &fork].map(|agent| {
        restart_posture(agent, &workspace, &machine)
            .unwrap()
            .0
            .launch
            .allowed_tools
    });

    assert_eq!(
        tools,
        [
            Some(vec!["Read".parse().unwrap()]),
            Some(vec!["Bash".parse().unwrap()]),
            Some(vec!["Bash".parse().unwrap()]),
            Some(vec!["Read".parse().unwrap()]),
        ]
    );
}

#[test]
fn restart_posture_keeps_a_team_role_childs_agent_profile() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        crate::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine = crate::config::MachineConfig::default();
    let mut profile: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
    profile.allowed_tools = Some(vec!["Read".parse().unwrap()]);
    machine
        .agents
        .profiles
        .0
        .insert("claude-agent".to_owned(), profile);
    machine.agents.teams.0.insert(
        "forge".to_owned(),
        toml::from_str("roles = [{ role = 'reviewer', profile = 'claude-agent' }]").unwrap(),
    );
    let launch = crate::config::effective::load(&machine, &workspace.project_root).unwrap();
    let resolved = crate::harness::plan::resolve_launch(
        &launch,
        crate::config::effective::ProfileScope::Subagents,
        &machine.agents.commands,
        Some("forge.reviewer"),
        None,
    )
    .unwrap();
    let cell = resolved.layout.agent_cells().next().unwrap();
    let mut child = crate::testkit::agent_state("claude", "child", jiff::Timestamp::UNIX_EPOCH);
    child.profile = cell.launch.profile.clone();
    child.role = cell.launch.role.clone();
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);

    let mut fork = child.clone();
    fork.parent_agent_id = None;
    fork.parent_agent_kind = None;
    fork.profile_scope = Some(ProfileScope::Subagents);
    let actual = [&child, &fork].map(|agent| {
        let (posture, _) = restart_posture(agent, &workspace, &machine).unwrap();
        (posture.degraded, posture.launch.allowed_tools)
    });
    assert_eq!(
        actual,
        std::array::from_fn::<_, 2, _>(|_| { (None, Some(vec!["Read".parse().unwrap()])) })
    );
}

#[test]
fn relaunch_requests_preserve_a_forks_profile_scope_without_child_treatment() {
    let mut agent = crate::testkit::agent_state("claude", "fork", jiff::Timestamp::UNIX_EPOCH);
    agent.profile_scope = Some(ProfileScope::Subagents);
    let posture = ResumePosture::default();
    let actual = [
        ExecAction::Resume {
            session_id: "fork".into(),
            extra_args: Vec::new(),
        },
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    ]
    .map(|action| {
        let request = relaunch_request(&agent, &posture, action, None, None);
        assert!(!request.subagent);
        request.identity.params.profile_scope
    });
    let fresh = fresh_launch_request(&agent, restart_cell(&agent, &posture), None).unwrap();
    assert_eq!(
        [actual[0], actual[1], fresh.launch.profile_scope],
        [Some(ProfileScope::Subagents); 3]
    );
}

#[test]
fn restart_posture_refuses_failed_kind_chains_before_degrading() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("agents")).unwrap();
    std::fs::write(dir.path().join("agents/claude.md"), "invalid frontmatter").unwrap();
    let workspace =
        crate::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine =
        crate::config::MachineConfig::parse_text(&dir.path().join("config.toml"), "", dir.path())
            .unwrap();
    for profiles in [
        &mut machine.agents.profiles,
        &mut machine.subagents.profiles,
    ] {
        for (name, kind) in [("worker", "claude"), ("safe", "codex")] {
            profiles.0.insert(
                name.into(),
                toml::from_str(&format!("agent = '{kind}'")).unwrap(),
            );
        }
    }
    let mut direct = crate::testkit::agent_state("claude", "direct", jiff::Timestamp::UNIX_EPOCH);
    direct.profile = Some("worker".into());
    let mut child = direct.clone();
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    let mut bare = direct.clone();
    bare.profile = None;
    bare.record = Some(Box::default());
    let mut overridden =
        crate::testkit::agent_state("codex", "override", jiff::Timestamp::UNIX_EPOCH);
    overridden.profile = Some("safe".into());
    overridden.record = Some(Box::new(crate::agents::LaunchRecord {
        agent: Some("worker".into()),
        ..Default::default()
    }));
    let mut rebased = overridden.clone();
    rebased.profile = Some("worker".into());
    rebased.record.as_mut().unwrap().agent = Some("codex".into());
    let actual = [&direct, &child, &bare, &overridden, &rebased].map(|agent| {
        restart_posture(agent, &workspace, &machine)
            .err()
            .map(|error| error.to_string())
    });
    let mut legacy = bare;
    legacy.record = None;
    assert!(
        restart_posture(&legacy, &workspace, &machine)
            .unwrap()
            .0
            .degraded
            .is_none()
    );
    assert_eq!(
        Vec::from(actual),
        vec![machine.definition_failure_for("claude"); 5]
    );
}

#[test]
fn restart_posture_resolves_subagent_recorded_override_from_agents_table() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("rimz-home");
    std::fs::create_dir_all(home.join("agents")).unwrap();
    std::fs::create_dir_all(home.join("subagents")).unwrap();
    std::fs::write(
        home.join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        home.join("agents/codex.md"),
        "---\ndescription: Codex base\n---\nCodex house.\n",
    )
    .unwrap();
    std::fs::write(
        home.join("agents/astra.md"),
        "---\ndescription: Astra\nagent: codex\nmodel: astra\neffort: low\ntools: [Bash, Read]\n---\nAstra craft.\n",
    )
    .unwrap();
    std::fs::write(
        home.join("subagents/advisor.md"),
        "---\ndescription: Advisor\nagent: claude\nmodel: opus\neffort: high\ntools: [Read]\n---\nAdvise.\n",
    )
    .unwrap();
    let workspace = crate::WorkspaceResolver::resolve_under(dir.path(), None, &home).unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&home.join("config.toml"), "", &home).unwrap();

    let mut child = crate::testkit::agent_state("codex", "child", jiff::Timestamp::UNIX_EPOCH);
    child.profile = Some("advisor".into());
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(crate::ids::AgentKind::new_unchecked("claude"));
    child.launch_depth = Some(1);
    child.record = Some(Box::new(crate::agents::LaunchRecord {
        model: Some("astra".into()),
        effort: Some("high".into()),
        agent: Some("astra".into()),
        ..Default::default()
    }));
    let mut forked = child.clone();
    forked.parent_agent_id = None;
    forked.parent_agent_kind = None;
    forked.launch_depth = None;
    forked.profile_scope = Some(ProfileScope::Subagents);

    for agent in [&child, &forked] {
        let (posture, warning) = restart_posture(agent, &workspace, &machine).unwrap();
        assert!(warning.is_none(), "{warning:?}");
        assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
        assert_eq!(posture.launch.model.as_deref(), Some("astra"));
        assert_eq!(posture.launch.effort.as_deref(), Some("high"));
        assert!(
            posture
                .launch
                .system_prompt_file
                .as_ref()
                .is_some_and(|src| src.origin().ends_with("agents/codex.md")),
            "{:?}",
            posture.launch.system_prompt_file
        );
        assert!(
            posture
                .launch
                .append_system_prompt_files
                .iter()
                .any(|src| src.origin().ends_with("subagents/advisor.md")),
            "{:?}",
            posture.launch.append_system_prompt_files
        );
    }

    // A recorded agents-table override whose kind base failed must refuse before degrading.
    let bad_dir = tempfile::tempdir().unwrap();
    let bad_home = bad_dir.path().join("rimz-home");
    std::fs::create_dir_all(bad_home.join("agents")).unwrap();
    std::fs::write(bad_home.join("agents/codex.md"), "invalid frontmatter").unwrap();
    let bad_ws = crate::WorkspaceResolver::resolve_under(bad_dir.path(), None, &bad_home).unwrap();
    let mut bad_machine =
        crate::config::MachineConfig::parse_text(&bad_home.join("config.toml"), "", &bad_home)
            .unwrap();
    bad_machine.subagents.profiles.0.insert(
        "advisor".into(),
        toml::from_str("agent = 'claude'").unwrap(),
    );
    bad_machine
        .agents
        .profiles
        .0
        .insert("astra".into(), toml::from_str("agent = 'codex'").unwrap());
    let err = restart_posture(&child, &bad_ws, &bad_machine)
        .expect_err("failed kind base on agents override must refuse");
    assert_eq!(
        Some(err.to_string()),
        bad_machine.definition_failure_for("codex")
    );
}

#[test]
fn fresh_relaunch_exec_uses_the_allocated_login() {
    let agent = crate::testkit::agent_state("claude", "old", jiff::Timestamp::UNIX_EPOCH);
    for allocated in [Some("team".parse().unwrap()), None] {
        let identity = AgentLaunchIdentity {
            launch_id: None,
            kind: agent.kind.clone(),
            agent_id: "new".into(),
            name: "otter".to_owned(),
            name_explicit: false,
            launch: crate::agents::LaunchParams {
                login: allocated.clone(),
                ..Default::default()
            },
            run_id: None,
            prompt: None,
        };
        let request = relaunch_request(
            &agent,
            &ResumePosture::default(),
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
            Some("work".parse().unwrap()),
            Some(&identity),
        );
        assert_eq!(
            request.identity.params.login, allocated,
            "exec and the allocated stamp must use the same account"
        );
    }
}

#[test]
fn restart_posture_keeps_the_team_prompt_layer() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        crate::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let prompt = dir.path().join("base.md");
    std::fs::write(&prompt, "Base instructions.").unwrap();
    let mut machine = crate::config::MachineConfig::default();
    let mut profile: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
    profile.system_prompt_file = Some(prompt.into());
    machine.agents.profiles.0.insert("worker".into(), profile);
    machine.agents.teams.0.insert(
        "forge".into(),
        toml::from_str("roles = [{ role = 'reviewer', profile = 'worker', owns = ['Review'] }]")
            .unwrap(),
    );
    let mut agent = crate::testkit::agent_state("claude", "seat", jiff::Timestamp::UNIX_EPOCH);
    agent.profile = Some("worker".into());
    agent.team = Some("forge".into());
    agent.role = Some("reviewer".into());

    let (posture, _) = restart_posture(&agent, &workspace, &machine).unwrap();

    assert_eq!(posture.degraded, None);
    assert_eq!(
        posture.launch.team_prompt,
        Some(crate::harness::team_prompt::TeamPrompt {
            consensus: crate::harness::team_prompt::Consensus::BuiltIn,
            files: Vec::new(),
        })
    );
}

#[test]
fn restart_posture_refuses_undeliverable_team_prompt_layers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        crate::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine = crate::config::MachineConfig::default();
    machine
        .agents
        .profiles
        .0
        .insert("worker".into(), toml::from_str("agent = 'claude'").unwrap());
    let mut agent = crate::testkit::agent_state("claude", "seat", jiff::Timestamp::UNIX_EPOCH);
    agent.profile = Some("worker".into());
    agent.team = Some("forge".into());
    agent.role = Some("reviewer".into());
    let mut actual = Vec::new();
    for owns in ["['Review']", "[]"] {
        let team = toml::from_str(&format!(
            "consensus-file = 'consensus.md'\n\
             roles = [{{ role = 'reviewer', profile = 'worker', owns = {owns} }}]"
        ))
        .unwrap();
        machine.agents.teams.0.insert("forge".into(), team);
        actual.push(
            restart_posture(&agent, &workspace, &machine)
                .err()
                .map(|reason| reason.to_string()),
        );
    }
    assert_eq!(
        actual,
        [
            Some(format!(
                "team `forge` sets consensus-file, consensus, or \
                     append-system-prompt-files but role \
                     `reviewer` (claude) has no base system prompt to compose them onto; add \
                     system-prompt-file to role reviewer or its profile, or write {} with a \
                     nonempty body as the claude kind base \
                     (run `rimz agents validate` if it did not load)",
                crate::disk::paths::agents_home()
                    .join("agents/claude.md")
                    .display()
            )),
            Some(
                "team `forge` sets consensus-file, consensus, or \
                 append-system-prompt-files but declares no \
                 stages; add stages (or owns) or remove the team prompt fields"
                    .into()
            ),
        ]
    );
}

#[test]
fn relaunches_of_a_lane_less_row_carry_no_lane_from_its_old_channel_stamp() {
    let mut agent = crate::testkit::agent_state("claude", "old", jiff::Timestamp::UNIX_EPOCH);
    agent.worktree_path = Some("/repo".into());
    agent.channel = Some("design".into());
    let posture = ResumePosture::default();
    let fresh = fresh_launch_request(&agent, restart_cell(&agent, &posture), None).unwrap();
    let relaunch = relaunch_request(
        &agent,
        &posture,
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        None,
        None,
    );
    assert_eq!(
        [fresh.launch.lane, relaunch.identity.params.lane],
        [None, None]
    );
}

#[test]
fn a_relaunch_pane_states_the_carried_lanes_name_and_none_for_trunk() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(dir.path(), None).unwrap();
    let root = workspace.project_root.clone();
    let mut agent = crate::testkit::agent_state("claude", "old", jiff::Timestamp::UNIX_EPOCH);
    agent.worktree_path = Some(root.display().to_string());
    for (stamp, expected) in [("main", None), ("design", Some("design"))] {
        agent.lane = Some(crate::worktree::own_lane(&root, &root).adopt(Some(stamp)));
        let request = relaunch_request(
            &agent,
            &ResumePosture::default(),
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
            None,
            None,
        );
        let env = relaunch_identity_env(&workspace, &root, &request);
        assert_eq!(
            env.get("RIMZ_CHANNEL")
                .map(String::as_str)
                .filter(|name| !name.is_empty()),
            expected,
            "{stamp}"
        );
    }
    let foreign = tempfile::tempdir().unwrap();
    agent.lane = Some(crate::worktree::own_lane(&root, foreign.path()));
    let request = relaunch_request(
        &agent,
        &ResumePosture::default(),
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        None,
        None,
    );
    let env = relaunch_identity_env(&workspace, foreign.path(), &request);
    assert_eq!(
        env.get("RIMZ_CHANNEL").filter(|name| !name.is_empty()),
        None,
        "a foreign checkout's own lane pins no channel"
    );
}

#[test]
fn an_in_place_relaunch_carries_the_rows_epoch() {
    let mut agent = crate::testkit::agent_state("claude", "a1", jiff::Timestamp::UNIX_EPOCH);
    let posture = ResumePosture {
        launch: Default::default(),
        degraded: None,
    };
    let epoch = |agent: &AgentState| {
        let action = ExecAction::Resume {
            session_id: "a1".into(),
            extra_args: Vec::new(),
        };
        relaunch_request(agent, &posture, action, None, None)
            .identity
            .params
            .epoch
    };
    assert_eq!(epoch(&agent), None);
    agent.epoch = 2;
    assert_eq!(epoch(&agent), Some(2));
}
