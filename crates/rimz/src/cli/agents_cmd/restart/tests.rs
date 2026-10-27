use super::*;

#[test]
fn a_fresh_restart_of_a_molt_successor_keeps_its_predecessor_superseded() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = rimz::workspace::WorkspaceResolver::resolve(dir.path(), None).unwrap();
    let store = rimz::Store::open(
        rimz::StatePaths::under(workspace.workspace_id.clone(), &dir.path().join("state")).unwrap(),
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("runtime"))
            .unwrap(),
    )
    .unwrap();
    let mut successor =
        rimz::testkit::agent_state("claude", "launch_middle", jiff::Timestamp::now());
    successor.launch_id = Some(successor.agent_id.clone());
    successor.molted_from = Some("launch_old".into());
    successor.role = Some("coder".into());
    let posture = ResumePosture::default();
    let mut predecessor = successor.clone();
    predecessor.agent_id = "launch_old".into();
    predecessor.launch_id = Some(predecessor.agent_id.clone());
    predecessor.molted_from = None;
    let mut predecessor_request =
        fresh_launch_request(&predecessor, restart_cell(&predecessor, &posture), None).unwrap();
    predecessor_request.agent_id = predecessor.agent_id;
    let mut request =
        fresh_launch_request(&successor, restart_cell(&successor, &posture), None).unwrap();
    request.agent_id = successor.agent_id.clone();
    store
        .begin_agent_launch_batch(
            &[predecessor_request, request],
            AgentLaunchScope {
                session_name: workspace.session_name.clone(),
                cwd: dir.path().into(),
                branch: None,
                description: None,
            },
        )
        .unwrap();
    let restarted = append_fresh_launch(
        &store,
        &workspace,
        &successor,
        dir.path(),
        restart_cell(&successor, &posture),
        None,
    );
    assert!(restarted.is_ok(), "{restarted:?}");
    let restarted = restarted.unwrap();
    let replacement = restarted.single_identity().unwrap();
    assert_eq!(replacement.launch.molted_from, successor.molted_from);
    let audit = store.runtime_projection(rimz::RuntimeScope::Audit).unwrap();
    let holders = rimz::address::role_holders(&audit.agents, "coder");
    assert!(holders.iter().all(|agent| agent.agent_id != "launch_old"));
    assert!(
        holders
            .iter()
            .any(|agent| agent.agent_id == replacement.agent_id)
    );
}

#[test]
fn a_failed_retirement_still_counts_the_waits_it_dropped() {
    let mut err = Vec::new();
    let failure = rimz::harness::schedule::arm::RetireFailure::after_dropping(2);
    assert_eq!(retired_count(Err(failure), &mut err).unwrap(), 2);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "rimz: retiring session deliveries: withdrawal failed\n"
    );
    assert_eq!(retired_count(Ok(3), &mut Vec::new()).unwrap(), 3);
}

#[test]
fn restarting_peer_fails_only_its_open_turn() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = rimz::WorkspaceId::from_project_root(dir.path());
    let store = rimz::Store::open(
        rimz::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap(),
        rimz::RuntimePaths::under(workspace_id, &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    let mut peer = rimz::testkit::agent_state("claude", "peer", jiff::Timestamp::now());
    peer.launch_id = Some("peer-launch".into());
    peer.launched_by = Some(rimz::agents::LaunchedBy {
        kind: peer.kind.clone(),
        agent_id: "launcher".into(),
    });
    let adapter = rimz::agents::find_definition("claude").unwrap();
    let record = rimz::harness::run::create_peer_prompt(
        store.paths(),
        &peer,
        None,
        adapter,
        "task",
        dir.path(),
        rimz::store::run::ReportTo::Launcher,
    )
    .unwrap()
    .unwrap();
    rimz::harness::run::record_lifecycle(
        store.paths(),
        &record.run_id,
        "claude",
        &rimz::agents::AgentLifecycleObservation::new(
            Some(peer.agent_id.clone()),
            rimz::agents::LifecycleSignal::TurnStarted { turn_id: None },
        ),
        None,
        || None,
    )
    .unwrap();
    settle_peer_before_restart(&store, &peer).unwrap();
    let failed = rimz::harness::run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(failed.status, rimz::store::run::RunStatus::Failed);
    assert_eq!(failed.failure_tail.as_deref(), Some("peer restarted"));
    assert!(
        failed.report_message_id.is_none(),
        "failed turn remains eligible for the fleet reporter"
    );
    settle_peer_before_restart(&store, &peer).unwrap();
    assert_eq!(
        rimz::harness::run::list(store.paths()).unwrap(),
        vec![failed]
    );
}

#[test]
fn resume_classification_names_each_fresh_reason() {
    assert_eq!(
        fresh_reason(false, true),
        Some(FreshReason::NoResumeSupport)
    );
    assert_eq!(
        fresh_reason(true, false),
        Some(FreshReason::NoRecordedConversation)
    );
    assert_eq!(fresh_reason(true, true), None);
}

#[test]
fn restart_reports_only_the_waits_no_registration_arms_again() {
    assert_eq!(dropped_note(0), "");
    assert_eq!(dropped_note(1), "; 1 armed wait dropped");
    assert_eq!(dropped_note(3), "; 3 armed waits dropped");
}

#[test]
fn relaunch_request_leaves_supervised_fields_at_launch_defaults() {
    let mut agent = rimz::testkit::agent_state("claude", "a1", jiff::Timestamp::UNIX_EPOCH);
    agent.name = Some("otter".to_owned());
    agent.role = Some("coder".to_owned());
    // A restart replays the session, not the fire that launched it.
    agent.loop_task = Some("fixer".to_owned());
    let posture = ResumePosture {
        launch: rimz::harness::plan::ResumeLaunchPosture {
            args: vec!["--model".to_owned(), "opus".to_owned()],
            system_prompt_file: Some("/prompts/coder.md".into()),
            append_system_prompt_files: vec!["/prompts/extra.md".into()],
            team_prompt: Some(rimz::harness::team_prompt::TeamPrompt {
                consensus: rimz::harness::team_prompt::Consensus::BuiltIn,
                files: vec!["/prompts/team.md".into()],
            }),
            skills: Some(vec!["merge".parse().unwrap()]),
            allowed_tools: Some(vec!["Read".parse().unwrap()]),
            isolation_default: Some(rimz::config::Isolation::Scoped),
            model: Some("opus".to_owned()),
            ..Default::default()
        },
        degraded: None,
    };
    let action = ExecAction::Resume {
        session_id: "a1".to_owned(),
        extra_args: Vec::new(),
    };

    let request = relaunch_request(&agent, &posture, action, None, None);

    let Cell::Agent(cell) = restart_cell(&agent, &posture) else {
        panic!("agent cell")
    };
    assert_eq!(cell.allowed_tools, posture.launch.allowed_tools);

    assert_eq!(
        request,
        ExecRequest {
            isolation_default: Some(rimz::config::Isolation::Scoped),
            kind: agent.kind.clone(),
            action: ExecAction::Resume {
                session_id: "a1".to_owned(),
                extra_args: posture.launch.args.clone(),
            },
            system_prompt_file: posture.launch.system_prompt_file.clone(),
            append_system_prompt_files: posture.launch.append_system_prompt_files.clone(),
            team_prompt: posture.launch.team_prompt.clone(),
            skills: posture.launch.skills.clone(),
            no_skill_tool: posture.launch.no_skill_tool,
            allowed_tools: posture.launch.allowed_tools.clone(),
            provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            subagent: false,
            follow_up_message: None,
            loop_reminder: None,
            headless: None,
            identity: rimz::harness::launch::ExecIdentity {
                resume_model_override: false,
                name: Some("otter".to_owned()),
                name_explicit: agent.name_explicit,
                launch_id: None,
                params: rimz::agents::LaunchParams {
                    role: Some("coder".to_owned()),
                    model: Some("opus".to_owned()),
                    loop_task: Some("fixer".to_owned()),
                    ..Default::default()
                },
            },
        }
    );
}

#[test]
fn relaunch_request_keeps_child_treatment_without_a_supervised_run() {
    let mut child = rimz::testkit::agent_state("claude", "child", jiff::Timestamp::UNIX_EPOCH);
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

    assert_eq!(
        (
            request.subagent,
            request.run_id,
            request.close_pane_on_exit,
            request.exit_on_run_completion,
            request.follow_up_message,
        ),
        (true, None, true, false, None)
    );
}

#[test]
fn relaunch_request_does_not_treat_a_peer_as_a_child() {
    let mut peer = rimz::testkit::agent_state("claude", "peer", jiff::Timestamp::UNIX_EPOCH);
    peer.launch_depth = Some(1);
    peer.launched_by = Some(rimz::agents::LaunchedBy {
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
        rimz::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine = rimz::config::MachineConfig::default();
    for (profiles, tool) in [
        (&mut machine.agents.profiles, "Bash"),
        (&mut machine.subagents.profiles, "Read"),
    ] {
        let mut profile: rimz::config::Profile = toml::from_str("agent = 'claude'").unwrap();
        profile.allowed_tools = Some(vec![tool.parse().unwrap()]);
        profiles.0.insert("worker".to_owned(), profile);
    }
    let mut child = rimz::testkit::agent_state("claude", "child", jiff::Timestamp::UNIX_EPOCH);
    child.profile = Some("worker".to_owned());
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    let mut peer = child.clone();
    peer.parent_agent_id = None;
    peer.parent_agent_kind = None;
    peer.launched_by = Some(rimz::agents::LaunchedBy {
        kind: peer.kind.clone(),
        agent_id: "launcher".into(),
    });
    let mut top_level = peer.clone();
    top_level.launch_depth = None;
    top_level.launched_by = None;

    let tools = [&child, &peer, &top_level].map(|agent| {
        restart_posture(agent, &workspace, &machine)
            .unwrap()
            .launch
            .allowed_tools
    });

    assert_eq!(
        tools,
        [
            Some(vec!["Read".parse().unwrap()]),
            Some(vec!["Bash".parse().unwrap()]),
            Some(vec!["Bash".parse().unwrap()]),
        ]
    );
}

#[test]
fn restart_posture_keeps_a_team_role_childs_agent_profile() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        rimz::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine = rimz::config::MachineConfig::default();
    let mut profile: rimz::config::Profile = toml::from_str("agent = 'claude'").unwrap();
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
    let launch = rimz::config::effective::load(&machine, &workspace.project_root).unwrap();
    let resolved = rimz::harness::plan::resolve_launch(
        &launch,
        rimz::config::effective::ProfileScope::Subagents,
        &machine.agents.commands,
        Some("forge.reviewer"),
        None,
    )
    .unwrap();
    let cell = resolved.layout.agent_cells().next().unwrap();
    let mut child = rimz::testkit::agent_state("claude", "child", jiff::Timestamp::UNIX_EPOCH);
    child.profile = cell.launch.profile.clone();
    child.role = cell.launch.role.clone();
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);

    let posture = restart_posture(&child, &workspace, &machine).unwrap();

    assert_eq!(
        (posture.degraded, posture.launch.allowed_tools),
        (None, Some(vec!["Read".parse().unwrap()]))
    );
}

#[test]
fn restart_posture_refuses_failed_kind_chains_before_degrading() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("agents")).unwrap();
    std::fs::write(dir.path().join("agents/claude.md"), "invalid frontmatter").unwrap();
    let workspace =
        rimz::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine =
        rimz::config::MachineConfig::parse_text(&dir.path().join("config.toml"), "", dir.path())
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
    let mut direct = rimz::testkit::agent_state("claude", "direct", jiff::Timestamp::UNIX_EPOCH);
    direct.profile = Some("worker".into());
    let mut child = direct.clone();
    child.parent_agent_id = Some("parent".into());
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    let mut bare = direct.clone();
    bare.profile = None;
    bare.record = Some(Box::default());
    let mut overridden =
        rimz::testkit::agent_state("codex", "override", jiff::Timestamp::UNIX_EPOCH);
    overridden.profile = Some("safe".into());
    overridden.record = Some(Box::new(rimz::agents::LaunchRecord {
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
            .degraded
            .is_none()
    );
    assert_eq!(
        Vec::from(actual),
        vec![machine.definition_failure_for("claude"); 5]
    );
}

#[test]
fn fresh_relaunch_exec_uses_the_allocated_login() {
    let agent = rimz::testkit::agent_state("claude", "old", jiff::Timestamp::UNIX_EPOCH);
    for allocated in [Some("team".parse().unwrap()), None] {
        let identity = AgentLaunchIdentity {
            kind: agent.kind.clone(),
            agent_id: "new".into(),
            name: "otter".to_owned(),
            name_explicit: false,
            launch: rimz::agents::LaunchParams {
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
        rimz::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let prompt = dir.path().join("base.md");
    std::fs::write(&prompt, "Base instructions.").unwrap();
    let mut machine = rimz::config::MachineConfig::default();
    let mut profile: rimz::config::Profile = toml::from_str("agent = 'claude'").unwrap();
    profile.system_prompt_file = Some(prompt.into());
    machine.agents.profiles.0.insert("worker".into(), profile);
    machine.agents.teams.0.insert(
        "forge".into(),
        toml::from_str("roles = [{ role = 'reviewer', profile = 'worker', owns = ['Review'] }]")
            .unwrap(),
    );
    let mut agent = rimz::testkit::agent_state("claude", "seat", jiff::Timestamp::UNIX_EPOCH);
    agent.profile = Some("worker".into());
    agent.team = Some("forge".into());
    agent.role = Some("reviewer".into());

    let posture = restart_posture(&agent, &workspace, &machine).unwrap();

    assert_eq!(posture.degraded, None);
    assert_eq!(
        posture.launch.team_prompt,
        Some(rimz::harness::team_prompt::TeamPrompt {
            consensus: rimz::harness::team_prompt::Consensus::BuiltIn,
            files: Vec::new(),
        })
    );
}

#[test]
fn restart_posture_refuses_undeliverable_team_prompt_layers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace =
        rimz::WorkspaceResolver::resolve_under(dir.path(), None, &dir.path().join("rimz-home"))
            .unwrap();
    let mut machine = rimz::config::MachineConfig::default();
    machine
        .agents
        .profiles
        .0
        .insert("worker".into(), toml::from_str("agent = 'claude'").unwrap());
    let mut agent = rimz::testkit::agent_state("claude", "seat", jiff::Timestamp::UNIX_EPOCH);
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
                "team `forge` sets consensus-file or append-system-prompt-files but role \
                     `reviewer` (claude) has no base system prompt to compose them onto; add \
                     system-prompt-file to role reviewer or its profile, or write {} with a \
                     nonempty body as the claude kind base \
                     (run `rimz agents validate` if it did not load)",
                rimz::disk::paths::agents_home()
                    .join("agents/claude.md")
                    .display()
            )),
            Some(
                "team `forge` sets consensus-file or append-system-prompt-files but declares no \
                 stages; add stages (or owns) or remove the team prompt fields"
                    .into()
            ),
        ]
    );
}

#[test]
fn relaunch_refuses_a_session_from_another_account_and_reopens_its_own() {
    let agent = AgentState {
        login: Some("personal".parse().expect("login name")),
        name: Some("x".to_owned()),
        ..rimz::testkit::agent_state("claude", "session-1", jiff::Timestamp::now())
    };
    let room = |name: &str| {
        rimz::agents::RoomAccounts::from(rimz::ids::RoomLogins::from([(
            rimz::ids::AgentKind::new_unchecked("claude"),
            name.parse().expect("login name"),
        )]))
    };

    let accounts = |work_history: &str| {
        rimz::agents::LoginCatalog::from_config(
            &toml::from_str(&format!(
                "[claude.work]\nhome = \"/srv/claude-work\"\nhistory = \"{work_history}\"\n\
                 [claude.personal]\nhome = \"/srv/claude-personal\"\n"
            ))
            .expect("accounts toml"),
        )
        .expect("login catalog")
    };
    let standalone = accounts("standalone");

    let err = relaunch_action(&agent, &room("work"), &standalone, Path::new("/repo")).unwrap_err();
    assert_eq!(
        err.to_string(),
        "@x's session belongs to claude account `personal`; this room now launches claude \
         on `work`. Run `rimz accounts use claude personal` to resume it, then switch \
         back."
    );

    // Resume and fresh alike: a pooled relaunch runs under the room's account.
    let (action, login, _) = relaunch_action(
        &agent,
        &room("work"),
        &accounts("shared"),
        Path::new("/repo"),
    )
    .expect("pooled relaunch");
    assert_eq!(login, Some("work".parse().expect("login name")));
    let request = relaunch_request(&agent, &ResumePosture::default(), action, login, None);
    assert_eq!(
        request.identity.params.login,
        Some("work".parse().expect("login name"))
    );
    let (_, login, _) = relaunch_action(
        &agent,
        &room("default"),
        &accounts("shared"),
        Path::new("/repo"),
    )
    .expect("pooled relaunch on the default account");
    assert_eq!(login, None);

    let (action, login, _) =
        relaunch_action(&agent, &room("personal"), &standalone, Path::new("/repo"))
            .expect("same-account relaunch");
    let request = relaunch_request(&agent, &ResumePosture::default(), action, login, None);
    let dir = tempfile::tempdir().unwrap();
    rimz::harness::launch_plan::testkit::assert_claude_stamped_home(
        &request,
        dir.path(),
        Some("personal"),
    );
}
