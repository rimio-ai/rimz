use super::*;
use crate::agents::AgentStatus;
use crate::harness::run::SupervisedRunRequest;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::writer::AgentLaunchScope;
#[test]
fn held_checkout_guidance_names_the_board_command() {
    let hold = crate::harness::resume::TeamHold {
        team: "forge".into(),
        channel: "feature".into(),
        checkout: "/repo/feature".into(),
        reason: crate::harness::resume::TeamHoldReason::BoardStage("Build".into()),
    };
    let (_, release, resume) = team_hold_guidance(&hold);
    assert!(
        release.contains("rimz teams flip 'forge#feature' Done"),
        "{release}"
    );
    assert!(
        resume.contains("rimz teams resume 'forge#feature'"),
        "{resume}"
    );
}

#[test]
fn resume_prompt_is_queued_to_the_leader_before_registration() {
    use crate::harness::plan::CohortSeed;
    use crate::store::message::{DeliveryGate, MessageSender};
    for fresh in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let id = crate::WorkspaceId::from_project_root(dir.path());
        let store = crate::Store::open(
            crate::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
            crate::RuntimePaths::under(id, &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let mut leader = test_agent("leader-session");
        leader.name = Some("leader".into());
        let seeds = vec![
            CohortSeed::Fresh,
            CohortSeed::Resume(test_agent("worker-session").into()),
            if fresh {
                CohortSeed::Fresh
            } else {
                CohortSeed::Resume(leader.into())
            },
        ];
        let identities = vec![
            launch_identity("codex", "worker"),
            launch_identity("codex", "leader"),
        ];
        queue_resume_prompt(&store, "room", &seeds, &identities, "say hi", 2).unwrap();
        let messages = store.list_messages().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "prompt must be durable before any pane opens"
        );
        let message = &messages[0];
        assert_eq!(message.sender, MessageSender::Human);
        assert_eq!(message.gate, DeliveryGate::Done);
        assert_eq!(message.text, "say hi");
        assert_eq!(message.agent_name.as_deref(), Some("leader"));
        assert_eq!(
            message.agent_id.as_str(),
            if fresh {
                "launch-leader"
            } else {
                "leader-session"
            }
        );
        assert!(message.enter);
    }
}

#[test]
fn launch_prompt_enrolls_only_the_prompted_peer_before_registration() {
    let dir = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(dir.path());
    let store = crate::Store::open(
        crate::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
        crate::RuntimePaths::under(id, &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    let mut identities = [
        launch_identity("claude", "worker"),
        launch_identity("claude", "leader"),
    ];
    for identity in &mut identities {
        identity.launch.launched_by = Some(Box::new(crate::agents::LaunchedBy {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: "launcher".into(),
        }));
    }
    identities[1].prompt = Some("launch task".into());
    let requests = identities
        .iter()
        .map(|identity| crate::store::writer::AgentLaunchRequest {
            login: crate::store::writer::LaunchLogin::RoomDefault,
            kind: identity.kind.clone(),
            agent_id: identity.agent_id.clone(),
            name: crate::store::writer::AgentLaunchName::Explicit(identity.name.clone()),
            launch: identity.launch.clone(),
            run_id: None,
            prompt: identity.prompt.clone(),
        })
        .collect::<Vec<_>>();
    let batch = store
        .begin_agent_launch_batch(
            &requests,
            AgentLaunchScope {
                session_name: "room".into(),
                cwd: dir.path().to_owned(),
                branch: None,
                description: None,
            },
        )
        .unwrap();
    let (peer, run) =
        prepare_peer_prompt(&store, batch.identities(), dir.path(), ReportTo::Launcher)
            .unwrap()
            .expect("prompt leader owes a report before opening the pane");
    assert_eq!(run.status, crate::store::run::RunStatus::Pending);
    assert_eq!(run.agent_name.as_deref(), Some("leader"));
    assert_eq!(run.peer.as_ref().unwrap().launch_id, identities[1].agent_id);
    assert!(run.agent_id.is_none());
    assert_eq!(crate::harness::run::list(store.paths()).unwrap().len(), 1);
    assert_eq!(
        run.reader, None,
        "an unresolved launcher leaves the reader to the fallback"
    );
    let (_, reused) = prepare_peer_prompt(&store, batch.identities(), dir.path(), ReportTo::Nobody)
        .unwrap()
        .unwrap();
    assert_eq!(
        (&reused.run_id, reused.report_to),
        (&run.run_id, ReportTo::Launcher),
        "a reused open run keeps its recorded policy"
    );
    crate::harness::run::fail_peer_run(&store, &peer, "launch failed").unwrap();
    for identity in &mut identities {
        identity.prompt = None;
    }
    assert!(
        prepare_peer_prompt(&store, &identities, dir.path(), ReportTo::Launcher)
            .unwrap()
            .is_none()
    );
    identities[1].prompt = Some("human task".into());
    identities[1].launch.launched_by = None;
    assert!(
        prepare_peer_prompt(&store, &identities, dir.path(), ReportTo::Launcher)
            .unwrap()
            .is_none()
    );
    assert_eq!(crate::harness::run::list(store.paths()).unwrap().len(), 1);

    let (_, detached) =
        prepare_peer_prompt(&store, batch.identities(), dir.path(), ReportTo::Nobody)
            .unwrap()
            .unwrap();
    assert_eq!(detached.report_to, ReportTo::Nobody);
    assert_eq!(
        crate::harness::run::load(store.paths(), &detached.run_id)
            .unwrap()
            .report_to,
        ReportTo::Nobody
    );
}

#[test]
fn team_tab_focuses_the_leader_pane_past_command_cells() {
    let team = toml::from_str::<crate::config::Team>(
        r#"leader = "planner"
roles = [{role = "coder", profile = "codex"}, {role = "planner", profile = "claude"}]"#,
    )
    .unwrap();
    let layout = crate::harness::spec::parse_layout_spec(
        "codex:coder+term,claude:planner",
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(team_leader_pane(&layout, Some(&team)), 2);
    assert_eq!(team_leader_pane(&layout, None), 0);
}

#[test]
fn worktree_filter_matches_normalized_agent_paths() {
    let target = crate::utils::path::normalize_path_lexical(Path::new("/repo-worktrees/demo"));
    let mut agent = test_agent("sess-demo");
    agent.worktree_path = Some("/repo/../repo-worktrees/demo".to_owned());

    assert!(agent_matches_worktree_filter(&agent, &target));

    agent.worktree_path = Some("/repo-worktrees/other".to_owned());
    assert!(!agent_matches_worktree_filter(&agent, &target));

    agent.worktree_path = None;
    assert!(!agent_matches_worktree_filter(&agent, &target));
}

#[test]
fn resume_worktree_scope_resolves_named_worktree() {
    let expected = PathBuf::from("/repo-worktrees/restore-living-team");
    let called = std::cell::Cell::new(false);

    let scope = resume_worktree_scope_with(
        Some(" restore-living-team "),
        Path::new("/repo"),
        Path::new("/repo"),
        |name| {
            called.set(true);
            assert_eq!(name, "restore-living-team");
            Ok(expected.clone())
        },
    )
    .expect("named worktree scope");

    assert_eq!(scope, Some(expected));
    assert!(called.get());
}

#[test]
fn resume_named_worktree_uses_the_launch_repo_root() {
    let project_root = PathBuf::from("/rooms/marker");
    let workspace = crate::ResolvedWorkspace {
        workspace_id: crate::WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        cwd_project_root: Some(PathBuf::from("/repos/current")),
        root_class: crate::workspace::RootClass::Marker,
        worktree_root: PathBuf::from("/repos/current"),
        worktree_branch: Some("main".to_owned()),
        session_name: "rimz-room".to_owned(),
        mux_hint: None,
    };

    let scope = resume_worktree_scope(
        Some("feat-x"),
        &workspace,
        &crate::config::MachineConfig::default(),
    )
    .expect("named resume scope");

    assert_eq!(
        scope,
        Some(PathBuf::from("/repos/current/../current-worktrees/feat-x"))
    );
}

#[test]
fn resume_worktree_scope_uses_cwd_worktree_when_unnamed() {
    let worktree = Path::new("/repo-worktrees/restore-living-team");

    let scope =
        resume_worktree_scope_with(None, worktree, Path::new("/repo"), |_| -> Result<PathBuf> {
            panic!("unnamed scope must not resolve a worktree name")
        })
        .expect("cwd worktree scope");

    assert_eq!(scope.as_deref(), Some(worktree));
}

#[test]
fn resume_worktree_scope_keeps_repo_root_global_when_unnamed() {
    let scope = resume_worktree_scope_with(
        None,
        Path::new("/repo"),
        Path::new("/repo"),
        |_| -> Result<PathBuf> { panic!("repo-root scope must stay global") },
    )
    .expect("global resume scope");

    assert_eq!(scope, None);
}

#[test]
fn resume_worktree_scope_treats_bare_worktree_flag_as_unnamed() {
    let worktree = Path::new("/repo-worktrees/restore-living-team");

    let scope = resume_worktree_scope_with(
        Some("  "),
        worktree,
        Path::new("/repo"),
        |_| -> Result<PathBuf> { panic!("bare -w must not resolve a generated worktree name") },
    )
    .expect("bare worktree flag scope");

    assert_eq!(scope.as_deref(), Some(worktree));
}

#[test]
fn team_resume_without_rows_names_a_team_launch() {
    for lane in [None, Some("project/forge")] {
        let error = team_resume_error(
            crate::harness::resume::CohortResumeErr::NothingToResume {
                spec: "forge".into(),
            },
            "forge",
            lane,
            &[],
            &[],
            &crate::config::TeamsConfig::default(),
            |_| true,
            crate::store::runtime::agent_liveness,
        )
        .to_string();
        let address = lane.map_or_else(|| "forge".to_owned(), |lane| format!("forge#{lane}"));
        assert_eq!(
            error,
            format!("nothing to resume for `{address}`; launch with `rimz teams forge`")
        );
        assert!(
            !error.contains("--continue") && !error.contains("rimz agents"),
            "{error}"
        );
    }
}

#[test]
fn team_resume_lists_other_existing_lanes() {
    let mut member = test_agent("sess-forge");
    member.team = Some("forge".into());
    member.channel = Some("project/forge".into());
    member.worktree_path = Some("/repo".into());
    let mut removed = member.clone();
    removed.channel = Some("removed".into());
    removed.worktree_path = Some("/removed".into());
    let error = team_resume_error(
        crate::harness::resume::CohortResumeErr::NothingToResume {
            spec: "forge".into(),
        },
        "forge",
        Some("typo"),
        &[],
        &[member.clone(), member, removed],
        &crate::config::TeamsConfig::default(),
        |path| path == Path::new("/repo"),
        crate::store::runtime::agent_liveness,
    )
    .to_string();
    assert!(
        error.starts_with("nothing to resume for `forge#typo`;"),
        "{error}"
    );
    assert!(error.contains("cohorts are in #project/forge"), "{error}");
    assert!(
        error.contains("`rimz teams resume 'forge#project/forge'`"),
        "{error}"
    );
    assert!(!error.contains("removed"), "{error}");
    assert!(
        !error.contains("--continue") && !error.contains("rimz agents"),
        "{error}"
    );
}

#[test]
fn team_resume_retries_only_configured_teams_in_the_typed_lane() {
    let mut member = test_agent("sess-other");
    member.team = Some("other".into());
    member.role = Some("coder".into());
    member.channel = Some("feature-team".into());
    member.worktree_path = Some("/repo".into());
    member.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
    let mut plain = test_agent("sess-plain");
    plain.worktree_path = Some("/repo".into());
    let mut teams = crate::config::TeamsConfig::default();
    teams.0.insert("other".into(), Default::default());
    let mut reviewer = member.clone();
    reviewer.agent_id = "sess-reviewer".into();
    reviewer.role = Some("reviewer".into());
    for lane in [None, Some("feature-team")] {
        let error = team_resume_error(
            crate::harness::resume::CohortResumeErr::NothingToResume {
                spec: "forge".into(),
            },
            "forge",
            lane,
            &[plain.clone(), member.clone(), reviewer.clone()],
            &[],
            &teams,
            |_| true,
            crate::store::runtime::agent_liveness,
        )
        .to_string();
        let retry = lane.map_or(
            "`rimz teams resume other`",
            |_| "`rimz teams resume 'other#feature-team'`",
        );
        assert!(error.contains("resumable here: other"), "{error}");
        assert!(!error.contains("other, other"), "{error}");
        assert!(error.contains(retry), "{error}");
        assert!(!error.contains("codex"), "{error}");
        assert!(
            !error.contains("--continue") && !error.contains("rimz agents"),
            "{error}"
        );
    }
}

#[test]
fn team_resume_still_live_names_focus_and_stop_for_the_typed_address() {
    for lane in [None, Some("my repo/forge")] {
        let error = team_resume_error(
            crate::harness::resume::CohortResumeErr::MembersStillLive {
                labels: vec!["@coder".into()],
            },
            "forge",
            lane,
            &[],
            &[],
            &crate::config::TeamsConfig::default(),
            |_| true,
            crate::store::runtime::agent_liveness,
        )
        .to_string();
        let address = lane.map_or_else(|| "forge".to_owned(), |lane| format!("forge#{lane}"));
        assert!(
            error.starts_with(&format!("cannot resume `{address}`; still live: @coder;")),
            "{error}"
        );
        for verb in ["focus", "stop"] {
            let command = shell_command(["rimz", "teams", verb, &address]);
            assert!(error.contains(&format!("`{command}`")), "{error}");
        }
        assert!(
            !error.contains("--continue") && !error.contains("rimz agents"),
            "{error}"
        );
    }
}

#[test]
fn team_resume_keeps_account_refusals() {
    let error = team_resume_error(
        crate::harness::resume::CohortResumeErr::LoginMismatch(
            crate::harness::resume::RelaunchLoginErr::Account {
                message: "use `rimz accounts use claude work`".into(),
            },
        ),
        "forge",
        Some("review"),
        &[],
        &[],
        &crate::config::TeamsConfig::default(),
        |_| true,
        crate::store::runtime::agent_liveness,
    );
    assert_eq!(error.to_string(), "use `rimz accounts use claude work`");
}

#[test]
fn team_resume_does_not_suggest_covered_provisional_or_live_lanes() {
    use crate::store::runtime::AgentLiveness;

    let mut closed = test_agent("closed");
    closed.team = Some("forge".into());
    closed.channel = Some("feature-team".into());
    closed.worktree_path = Some("/repo".into());
    closed.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
    let provisional = AgentState {
        agent_id: "launch_unregistered".into(),
        ..closed.clone()
    };
    let live = AgentState {
        agent_id: "live".into(),
        ended_at: None,
        ..closed.clone()
    };
    let failures = [
        (
            "typed lane's own rows",
            Some("feature/team"),
            vec![closed.clone()],
            vec![closed.clone()],
        ),
        (
            "room scope already covered every lane",
            None,
            vec![closed.clone()],
            vec![closed.clone()],
        ),
        (
            "provisional-only lane",
            Some("typo"),
            vec![],
            vec![provisional],
        ),
        (
            "live lane also has an old ended member",
            Some("typo"),
            vec![],
            vec![closed, live],
        ),
    ]
    .into_iter()
    .filter_map(|(label, lane, agents, room_agents)| {
        let error = team_resume_error(
            crate::harness::resume::CohortResumeErr::NothingToResume {
                spec: "forge".into(),
            },
            "forge",
            lane,
            &agents,
            &room_agents,
            &Default::default(),
            |_| true,
            |agent| match agent.agent_id.as_str() {
                "live" => AgentLiveness::Live { pid: 1 },
                _ => AgentLiveness::Unknown,
            },
        )
        .to_string();
        let address = lane.map_or_else(|| "forge".to_owned(), |lane| format!("forge#{lane}"));
        let expected = format!("nothing to resume for `{address}`; launch with `rimz teams forge`");
        (error != expected).then_some((label, error))
    })
    .collect::<Vec<_>>();
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn agents_resume_refusal_wording_stays_in_the_agents_scene() {
    use crate::harness::resume::CohortResumeErr;
    for (err, expected) in [
        (
            CohortResumeErr::NothingToResume {
                spec: "claude".into(),
            },
            "nothing to resume for `claude` in worktree `review`; launch without `--continue`",
        ),
        (
            CohortResumeErr::MembersStillLive {
                labels: vec!["@coder".into()],
            },
            "cannot resume `claude` in worktree `review`; still live: @coder; close them first or drop `--continue`",
        ),
        (
            CohortResumeErr::SessionNotResumable {
                label: "@coder".into(),
            },
            "cannot continue `claude` in worktree `review`; @coder has no resumable session; use `rimz agents claude --resume` to choose a provider session, or `rimz agents claude` to launch fresh",
        ),
    ] {
        assert_eq!(
            cohort_resume_error(err, "claude", Some("review"), &[], &Default::default())
                .to_string(),
            expected
        );
    }
}

fn test_agent(id: &str) -> AgentState {
    crate::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH)
}
fn launch_identity(kind: &str, name: &str) -> AgentLaunchIdentity {
    AgentLaunchIdentity {
        kind: AgentKind::new_unchecked(kind),
        agent_id: AgentSessionId::from(format!("launch-{name}")),
        name: name.to_owned(),
        name_explicit: false,
        launch: crate::agents::LaunchParams::default(),
        run_id: None,
        prompt: None,
    }
}
#[test]
fn a_request_account_pin_outranks_a_subagents_parent() {
    use crate::store::writer::LaunchLogin;
    let codex = AgentKind::new_unchecked("codex");
    let mut caller = AgentState::stub("codex", "parent", AgentStatus::Running);
    caller.login = Some("spare".parse().unwrap());
    let mut request = supervised_request("fix-it", true);
    assert_eq!(
        resolve::launch_login(&request, Some(&caller), &codex),
        LaunchLogin::Pinned("spare".parse().unwrap())
    );
    request.login = LaunchLogin::Pinned("work".parse().unwrap());
    assert_eq!(
        resolve::launch_login(&request, Some(&caller), &codex),
        request.login
    );
}

#[test]
fn supervised_selection_checks_subagent_allowlist() {
    let mut caller = AgentState::stub("claude", "parent", AgentStatus::Running);
    caller.profile = Some("planner".to_owned());
    let profiles: crate::config::ProfilesConfig = toml::from_str(
        r#"
        [planner]
        agent = "claude"
        subagents = ["explorer"]
        "#,
    )
    .expect("profiles");
    let mut request = supervised_request("fix-it", true);
    request.spec = "explorer".to_owned();

    resolve::check_supervised_subagent_allowed(&request, Some(&caller), &profiles)
        .expect("allowed subagent");

    request.spec = "codex".to_owned();
    let error = resolve::check_supervised_subagent_allowed(&request, Some(&caller), &profiles)
        .expect_err("unlisted subagent");
    assert!(error.to_string().contains("profile `planner`"), "{error:#}");
}

#[test]
fn unsupported_adapter_keeps_subagent_reminder_in_user_prompt() {
    let request = supervised_request("amp", true);
    let adapter = crate::agents::find_definition("amp").unwrap();
    let prompt = resolve::supervised_prompt(&request, adapter);
    assert_eq!(
        prompt,
        format!(
            "amp\n\n{}",
            crate::harness::launch_reminders::subagent_reminder()
        )
    );

    let ordinary = supervised_request("amp", false);
    assert_eq!(resolve::supervised_prompt(&ordinary, adapter), "amp");
}

#[test]
fn native_system_text_adapters_keep_subagent_reminder_out_of_user_prompt() {
    for kind in ["claude", "codex"] {
        let request = supervised_request(kind, true);
        let adapter = crate::agents::find_definition(kind).unwrap();

        assert_eq!(resolve::supervised_prompt(&request, adapter), kind);
    }
}

fn supervised_request(prompt: &str, subagent: bool) -> SupervisedRunRequest {
    SupervisedRunRequest {
        spec: "codex".to_owned(),
        throttle_turn: None,
        prompt: prompt.to_owned(),
        description: None,
        worktree: None,
        fresh: false,
        cwd: None,
        from_pr: None,
        channel: None,
        name: None,
        background: false,
        self_cleanup_on_completion: false,
        subagent,
        force_new_tab: false,
        agent_shell_persistence: crate::harness::launch::AgentShellPersistence::Room,
        permission_mode: None,
        isolation: None,
        agent: None,
        tier: None,
        model: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        effort: None,
        fast: None,
        budget: None,
        max_turns: None,
        timeout: None,
        warn: Vec::new(),
        grace: None,
        keep: false,
        report_to: ReportTo::Launcher,
        retries: 0,
        verify: None,
        max_attempts: None,
        loop_task: None,
        workflow: None,
        loop_reminder: None,
        passthrough: Vec::new(),
        managed_launch: crate::agents::ManagedLaunchState::PendingResolution,
        login: crate::store::writer::LaunchLogin::RoomDefault,
    }
}
