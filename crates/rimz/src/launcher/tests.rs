use super::*;
use crate::agents::AgentStatus;
use crate::harness::run::SupervisedRunRequest;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::writer::AgentLaunchScope;

/// A Git repository at `repo` with one empty commit on `main`.
fn init_repo(repo: &std::path::Path) {
    std::fs::create_dir(repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=RimZ Test",
            "-c",
            "user.email=rimz@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}

/// A refused launch removes a tree it made under a generated name, which each recheck would
/// make again, and keeps a named one for the user to trust and the recheck to reuse.
#[test]
fn a_refused_launch_discards_only_a_generated_tree() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let config = crate::config::MachineConfig::default();
    let workspace = crate::workspace::WorkspaceResolver::resolve(&repo, None).unwrap();
    for (name, kept) in [("", false), ("named", true)] {
        let launch = crate::worktree::resolve_launch_checkout(
            &workspace,
            &config.agents.worktree,
            Some(name),
            None,
            None,
            None,
            crate::worktree::LaunchBase::Configured,
        )
        .unwrap();
        assert!(launch.created.is_some(), "{name:?}: {launch:?}");
        checkout::discard_created_tree(&launch, &config.agents.worktree.hooks);
        assert_eq!(
            launch.cwd.exists(),
            kept,
            "{name:?}: {}",
            launch.cwd.display()
        );
    }
}

#[test]
fn worktree_recreation_refuses_an_unfinished_workflow_run() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let config = crate::config::MachineConfig::default();
    let created = crate::worktree::create(
        &repo,
        &config.agents.worktree,
        Some("held"),
        None,
        None,
        false,
        crate::worktree::LaunchBase::Configured,
    )
    .unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(&repo, None).unwrap();
    let state = crate::StatePaths::for_project_root(&repo).unwrap();
    let store = crate::Store::open(
        state.clone(),
        crate::RuntimePaths::for_state(&state).unwrap(),
    )
    .unwrap();
    crate::workspace::record::write(
        &state,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
    let run: crate::store::workflow::RunRecord = serde_json::from_value(serde_json::json!({
        "version":crate::store::workflow::VERSION, "id":crate::ids::WorkflowRunId::new(), "workflow":"land",
        "spec_dir":dir.path(), "project_root":repo,
        "scope":{"kind":"worktree","path":created.marker.worktree_path},
        "created_at":"2026-01-01T00:00:00Z", "state":"active", "node":"check",
        "entered_at":"2026-01-01T00:00:00Z", "deadline_fired":false,
        "next_seq":1, "visits":{}
    }))
    .unwrap();
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    crate::store::workflow::initialize(&state, &run, &guard).unwrap();
    drop(guard);
    let result = worktree_entry::remove(
        &workspace,
        &config,
        None,
        None,
        &store,
        "held",
        &created.marker.worktree_path,
        "team",
        &mut Unattended,
    );
    let error = result
        .expect_err("a run must prevent recreation")
        .to_string();
    assert!(
        error.contains("is in use by workflow run land#held; nothing removed; nothing launched"),
        "{error}"
    );
    assert!(created.marker.worktree_path.exists());
}

#[test]
fn explicit_account_keeps_the_tier_provider_and_stamps_every_agent_cell() {
    let root = tempfile::tempdir().unwrap();
    let state = crate::StatePaths::for_project_root(root.path()).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let mut machine = crate::config::MachineConfig {
        accounts: toml::from_str(&format!(
            "[use]\nclaude = 'rimio'\n[claude.rimio]\nhome = {:?}\n",
            root.path().join("claude-rimio"),
        ))
        .unwrap(),
        ..Default::default()
    };
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("codex", ""),
        ("worker", "agent: claude\ntier: junior\ntools: [Read]\n"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Probe\n{fields}---\nCraft."),
        )
        .unwrap();
    }
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &root.path().join("config.toml"),
        &machine.tiers,
        None,
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    crate::disk::atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &crate::agents::account::AccountsCache {
            logins: [(
                "claude@rimio".parse().unwrap(),
                crate::agents::account::ProviderRecord {
                    consecutive_failures: 0,
                    version_probed_at_ms: None,
                    inputs_stamp: None,
                    profile: None,
                    probe_error: None,
                    login: Some(crate::agents::account::RecordedLogin::LoggedOut),
                    probed_at_ms: 1,
                    ok: true,
                    account: None,
                    login_at_ms: None,
                    stamps: Default::default(),
                    live_credentials: Default::default(),
                },
            )]
            .into(),
        },
    )
    .unwrap();
    let effective = crate::config::effective::load(&machine, root.path()).unwrap();
    for (login, kind, model) in [(None, "codex", "sol"), (Some("rimio"), "claude", "sonnet")] {
        let overrides = LaunchOverrides {
            login: login.map(|name| name.parse().unwrap()),
            ..Default::default()
        };
        let finalized = resolve_finalized_layout(
            None,
            &machine,
            &effective,
            crate::config::effective::ProfileScope::Agents,
            Some("worker,worker+term"),
            None,
            &overrides,
            None,
            None,
            None,
            false,
            Some((&runtime, &state)),
        )
        .unwrap();
        let cells = finalized.resolved.layout.agent_cells().collect::<Vec<_>>();
        assert_eq!(cells.len(), 2);
        for cell in cells {
            assert_eq!(cell.kind.as_str(), kind, "account {login:?}");
            assert_eq!(cell.launch.model.as_deref(), Some(model));
            assert_eq!(
                cell.launch
                    .tier
                    .as_ref()
                    .map(|stamp| (stamp.tier, stamp.model.as_str())),
                Some((crate::config::tiers::ModelTier::Junior, model)),
                "account {login:?}"
            );
            assert_eq!(cell.launch.login, overrides.login);
        }
    }
}

#[test]
fn explicit_account_refuses_mixed_providers_but_stamps_one_provider() {
    let root = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let effective = crate::config::effective::load(&machine, root.path()).unwrap();
    let overrides = LaunchOverrides {
        login: Some("rimio".parse().unwrap()),
        ..Default::default()
    };
    let resolve = |spec| {
        resolve_finalized_layout(
            None,
            &machine,
            &effective,
            crate::config::effective::ProfileScope::Agents,
            Some(spec),
            None,
            &overrides,
            None,
            None,
            None,
            false,
            None,
        )
    };
    let reason = resolve("claude,codex+term").unwrap_err().to_string();
    assert_eq!(
        reason,
        concat!(
            "--account names one provider's account, but this layout launches claude, codex; ",
            "launch each provider's cells separately, or switch the room with `rimz accounts use`"
        )
    );
    let finalized = resolve("claude,claude+term").unwrap();
    let cells = finalized.resolved.layout.agent_cells().collect::<Vec<_>>();
    assert_eq!(cells.len(), 2);
    assert!(
        cells
            .iter()
            .all(|cell| cell.launch.login == overrides.login)
    );
}

#[test]
fn explicit_account_refuses_both_cohort_resume_entrances() {
    let overrides = LaunchOverrides {
        login: Some("rimio".parse().unwrap()),
        ..Default::default()
    };
    let refusals = [ResumeEntrance::Flag, ResumeEntrance::Reconcile].map(|entrance| {
        validate_resume_inputs(&overrides, entrance)
            .err()
            .map(|error| error.to_string())
    });
    assert_eq!(
        refusals,
        [
            Some("resume reopens sessions on the room's account; rerun without --account".into()),
            Some(
                concat!(
                    "resume reopens sessions on the room's account; rerun without --account, ",
                    "or choose fresh instead of resume"
                )
                .into()
            ),
        ]
    );
}

#[test]
fn held_checkout_guidance_names_the_board_command() {
    let hold = crate::harness::resume::TeamHold {
        instance: "forge#feature".parse().unwrap(),
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
    let agent = MessageSender::Agent {
        agent_id: Some(AgentSessionId::from("caller-session")),
        kind: AgentKind::new_unchecked("claude"),
        name: Some("coder".into()),
        profile: None,
        role: None,
        channel: None,
    };
    for (fresh, sender) in [
        (false, MessageSender::Human),
        (true, MessageSender::Human),
        (false, agent.clone()),
        (true, agent),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let id = crate::WorkspaceId::from_project_root(dir.path());
        let store = crate::Store::open(
            crate::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
            crate::RuntimePaths::under(id, &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let mut leader = test_agent("leader-session");
        leader.name = Some("leader".into());
        leader.launch_id = Some("launch-resumed-leader".into());
        let mut worker = test_agent("worker-session");
        worker.launch_id = Some("launch-resumed-worker".into());
        worker.epoch = 2;
        let seeds = vec![
            CohortSeed::Fresh,
            CohortSeed::Resume(worker.into()),
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
        queue_resume_prompt(
            &store,
            "room",
            &seeds,
            &identities,
            "say hi",
            2,
            sender.clone(),
            sender == MessageSender::Human,
        )
        .unwrap();
        let messages = store.list_messages().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "prompt must be durable before any pane opens"
        );
        let message = &messages[0];
        assert_eq!(message.sender, sender);
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
        let returning = |launch: &str, epoch| crate::store::message::ReturningMember {
            launch_id: launch.into(),
            epoch,
        };
        assert_eq!(
            message.team_return,
            match (sender == MessageSender::Human, fresh) {
                (false, _) => Vec::new(),
                (true, fresh) => vec![
                    returning("launch-worker", 0),
                    returning("launch-resumed-worker", 2),
                    returning(
                        if fresh {
                            "launch-leader"
                        } else {
                            "launch-resumed-leader"
                        },
                        0,
                    ),
                ],
            },
            "a team's prompt names every member its return brings back; any other names none"
        );
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
    member.worktree_path = Some("/repo".into());
    crate::testkit::set_trunk(&mut member);
    crate::testkit::set_lane(&mut member, "forge");
    let mut removed = member.clone();
    removed.worktree_path = Some("/removed".into());
    removed.lane = None;
    crate::testkit::set_lane(&mut removed, "removed");
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
    assert!(error.contains("cohorts are in #forge"), "{error}");
    assert!(
        error.contains("`rimz teams resume 'forge#forge'`"),
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
fn team_resume_lists_a_board_held_trunk_cohort_as_main() {
    let root = tempfile::tempdir().unwrap();
    let board = root.path().join(".rimz/team/blackboard.md");
    std::fs::create_dir_all(board.parent().unwrap()).unwrap();
    std::fs::write(&board, "Stage: Plan").unwrap();
    let mut held = test_agent("launch_unregistered");
    held.team = Some("forge".into());
    held.worktree_path = Some(root.path().display().to_string());
    crate::testkit::set_trunk(&mut held);
    let error = team_resume_error(
        crate::harness::resume::CohortResumeErr::NothingToResume {
            spec: "forge".into(),
        },
        "forge",
        Some("typo"),
        &[],
        &[held],
        &crate::config::TeamsConfig::default(),
        |_| true,
        |_| crate::store::runtime::AgentLiveness::Unknown,
    )
    .to_string();
    assert!(error.contains("cohorts are in #main"), "{error}");
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
        launch_id: None,
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
    request.agent = Some("astra".to_owned());

    let empty = crate::config::ProfilesConfig::default();
    resolve::check_supervised_subagent_allowed(&request, Some(&caller), &profiles, &empty)
        .expect("allowed subagent with unlisted --agent");

    request.spec = "codex".to_owned();
    request.agent = Some("explorer".to_owned());
    let error =
        resolve::check_supervised_subagent_allowed(&request, Some(&caller), &profiles, &empty)
            .expect_err("unlisted subagent");
    assert!(error.to_string().contains("profile `planner`"), "{error:#}");
}

#[test]
fn unsupported_adapter_keeps_subagent_reminder_in_user_prompt() {
    let request = supervised_request("amp", true);
    let adapter = crate::agents::find_definition("amp").unwrap();
    let prompt = resolve::supervised_prompt(&request, adapter, &agent_cell("amp"));
    assert_eq!(
        prompt,
        format!(
            "amp\n\n{}",
            crate::harness::launch_reminders::subagent_reminder()
        )
    );

    let ordinary = supervised_request("amp", false);
    assert_eq!(
        resolve::supervised_prompt(&ordinary, adapter, &agent_cell("amp")),
        "amp"
    );
}

#[test]
fn native_system_text_adapters_keep_subagent_reminder_out_of_user_prompt() {
    for kind in ["claude", "codex"] {
        let request = supervised_request(kind, true);
        let adapter = crate::agents::find_definition(kind).unwrap();

        assert_eq!(
            resolve::supervised_prompt(&request, adapter, &agent_cell(kind)),
            kind
        );
    }
}

#[test]
fn agent_file_adapters_keep_subagent_reminder_out_of_user_prompt_only_with_prompt_sources() {
    let request = supervised_request("agy", true);
    let adapter = crate::agents::find_definition("antigravity").unwrap();
    let mut cell = agent_cell("antigravity");
    assert_eq!(
        resolve::supervised_prompt(&request, adapter, &cell),
        format!(
            "agy\n\n{}",
            crate::harness::launch_reminders::subagent_reminder()
        )
    );

    cell.system_prompt_file = Some(crate::config::PromptSource::Text {
        origin: "antigravity.md".into(),
        text: "Base.".to_owned(),
    });
    assert_eq!(resolve::supervised_prompt(&request, adapter, &cell), "agy");
}

fn agent_cell(kind: &str) -> crate::harness::spec::AgentCell {
    let crate::harness::spec::Cell::Agent(cell) =
        crate::harness::spec::Cell::agent(AgentKind::new_unchecked(kind))
    else {
        unreachable!("Cell::agent builds an agent cell");
    };
    cell
}

fn supervised_request(prompt: &str, subagent: bool) -> SupervisedRunRequest {
    SupervisedRunRequest {
        spec: "codex".to_owned(),
        throttle_turn: None,
        prompt: prompt.to_owned(),
        description: None,
        worktree: None,
        from_trunk: false,
        fresh: false,
        cwd: None,
        add_dirs: Vec::new(),
        empty_dir: false,
        from_pr: None,
        channel: None,
        name: None,
        background: false,
        self_cleanup_on_completion: false,
        subagent,
        subagent_profile: false,
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
        warn: None,
        grace: None,
        keep: false,
        report_to: ReportTo::Launcher,
        retries: 0,
        verify: None,
        max_attempts: None,
        loop_task: None,
        loop_fire: None,
        loop_booked_by: Vec::new(),
        workflow: None,
        reminder_section: None,
        passthrough: Vec::new(),
        managed_launch: crate::agents::ManagedLaunchState::PendingResolution,
        login: crate::store::writer::LaunchLogin::RoomDefault,
    }
}

#[test]
fn launch_worktree_admission_refuses_main_before_any_work() {
    let project_root = PathBuf::from("/rooms/absent");
    let workspace = crate::ResolvedWorkspace {
        workspace_id: crate::WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        cwd_project_root: None,
        root_class: crate::workspace::RootClass::Marker,
        worktree_root: project_root,
        worktree_branch: Some("main".to_owned()),
        session_name: "rimz-room".to_owned(),
        mux_hint: None,
    };
    let config = crate::config::WorktreeConfig::default();

    let err =
        crate::harness::lanes::admit_launch_worktree_name(&workspace, &config, &[], Some("main"))
            .expect_err("main is trunk's lane");

    assert_eq!(
        err.to_string(),
        "`main` is the project checkout's lane; pick another worktree name"
    );
    crate::harness::lanes::admit_launch_worktree_name(&workspace, &config, &[], Some("docs"))
        .expect("another name");
}

#[test]
fn check_cell_copies_seat_consensus_skill_marker_to_host_preflight() {
    if std::env::var_os("RIMZ_TEST_CHECK_CELL_SEAT_SKILL").is_none() {
        let home = tempfile::tempdir().unwrap();
        let claude_dir = home.path().join(".claude");
        let rimz_dir = home.path().join(".rimz");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "launcher::tests::check_cell_copies_seat_consensus_skill_marker_to_host_preflight",
                "--nocapture",
            ])
            .env("HOME", home.path())
            .env("CLAUDE_CONFIG_DIR", &claude_dir)
            .env("RIMZ_HOME", &rimz_dir)
            .env("RIMZ_AGENTS_HOME", &rimz_dir)
            .env("RIMZ_UNIT_TEST_RIMZ_HOME", &rimz_dir)
            .env("RIMZ_TEST_CHECK_CELL_SEAT_SKILL", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    struct AcceptAll;
    impl Interaction for AcceptAll {
        fn cross_repo(&mut self, _: &crate::ResolvedWorkspace) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn pr_branch(
            &mut self,
            _: &crate::worktree::WorktreeErr,
        ) -> anyhow::Result<Option<crate::worktree::PrBranchChoice>> {
            Ok(None)
        }
        fn unmanaged_checkout(&mut self, _: &str, _: &std::path::Path) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn closed_cohort(&mut self, _: ClosedCohort<'_>) -> anyhow::Result<CohortChoice> {
            Ok(CohortChoice::Fresh)
        }
        fn notice(&mut self, _: Notice<'_>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    let claude_dir = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").unwrap());
    let work_skill = claude_dir.join("skills/work");
    std::fs::create_dir_all(&work_skill).unwrap();
    std::fs::write(
        work_skill.join("SKILL.md"),
        "---\nname: work\ndescription: Work.\n---\nWork.\n",
    )
    .unwrap();
    let adapter = crate::agents::find_definition("claude").unwrap();
    adapter
        .install_hooks(
            &crate::agents::ambient_env(),
            crate::agents::HookInstallOptions::default(),
        )
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    init_repo(&repo);
    let workspace = crate::workspace::WorkspaceResolver::resolve(&repo, None).unwrap();
    let state = crate::StatePaths::for_project_root(&repo).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();

    let mut cell = agent_cell("claude");
    cell.skills = Some(vec![
        "work".parse().unwrap(),
        "rimz-message".parse().unwrap(),
    ]);
    cell.seat_consensus_skill = true;

    let mut checked_folder_trust = HashSet::new();
    let mut noticed_hook_trust = HashSet::new();
    let mut interaction = AcceptAll;
    check_cell(
        &workspace,
        &runtime,
        &cell,
        crate::config::Isolation::Host,
        CellStart::Launch(None),
        &mut checked_folder_trust,
        &mut noticed_hook_trust,
        || "rimz hooks trust claude".to_owned(),
        None,
        None,
        &mut interaction,
    )
    .unwrap_or_else(|err| {
        panic!("check_cell must carry seat_consensus_skill into preflight: {err}")
    });

    cell.seat_consensus_skill = false;
    let err = check_cell(
        &workspace,
        &runtime,
        &cell,
        crate::config::Isolation::Host,
        CellStart::Launch(None),
        &mut checked_folder_trust,
        &mut noticed_hook_trust,
        || "rimz hooks trust claude".to_owned(),
        None,
        None,
        &mut interaction,
    )
    .expect_err("author-written missing rimz-message must refuse in check_cell");
    assert!(
        err.to_string().contains("unknown skill 'rimz-message'"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn one_chain_rule_and_tier_fallback_through_direct_entry() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("subagents")).unwrap();
    for (rel, content) in [
        (
            "agents/claude.md",
            "---\ndescription: Claude base\n---\nClaude house.",
        ),
        (
            "agents/codex.md",
            "---\ndescription: Codex base\n---\nCodex house.",
        ),
        (
            "agents/antigravity.md",
            "---\ndescription: Antigravity base\n---\nAntigravity house.",
        ),
        (
            "agents/sol.md",
            "---\ndescription: Sol\nagent: codex\nmodel: sol\neffort: low\n\
             tools: [Bash, Read]\n---\nSol craft.",
        ),
        (
            "agents/astra.md",
            "---\ndescription: Astra\nagent: codex\nmodel: astra\neffort: low\n\
             mode: plan\ntools: [Bash, Read]\n---\nAstra craft.",
        ),
        (
            "agents/reviewer.md",
            "---\ndescription: Reviewer\nagent: claude\nmodel: opus\neffort: high\n\
             tools: [Read, Grep]\n---\nReviewer craft.",
        ),
        (
            "agents/reviewer_direct_sol.md",
            "---\ndescription: Reviewer on sol\nagent: sol\neffort: high\n\
             tools: [Read, Grep]\n---\nReviewer craft.",
        ),
        (
            "agents/tiered.md",
            "---\ndescription: Tiered\nagent: claude\ntier: senior\n\
             tools: [Bash, Read]\n---\nTiered craft.",
        ),
        (
            "subagents/advisor.md",
            "---\ndescription: Advisor\nagent: antigravity\nmodel: neon\neffort: high\n\
             tools: [Read, Grep]\n---\nAdvisor craft.",
        ),
        (
            "subagents/finder.md",
            "---\ndescription: Finder\nagent: codex\nmodel: astra\neffort: low\n\
             tools: [Read, Grep]\n---\nFinder craft.",
        ),
    ] {
        std::fs::write(root.path().join(rel), content).unwrap();
    }
    let mut machine = crate::config::MachineConfig::default();
    let definitions = crate::config::definitions::load(
        root.path(),
        crate::config::definitions::SkillCheck::Skip,
        &machine.agents.commands,
        &root.path().join("config.toml"),
        &machine.tiers,
        None,
    );
    assert!(definitions.errors.is_empty(), "{:?}", definitions.errors);
    machine.agents.profiles = definitions.agent_profiles;
    machine.subagents.profiles = definitions.subagent_profiles;
    let effective =
        crate::config::effective::load_with_roots(&machine, root.path(), root.path()).unwrap();

    // One rule at direct entry: reviewer --agent sol matches reviewer_direct_sol.
    let via_flag = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        crate::config::effective::ProfileScope::Agents,
        Some("reviewer"),
        None,
        &super::LaunchOverrides {
            agent: Some("sol".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("reviewer --agent sol");
    let direct = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        crate::config::effective::ProfileScope::Agents,
        Some("reviewer_direct_sol"),
        None,
        &super::LaunchOverrides::default(),
        None,
        None,
        None,
        true,
        None,
    )
    .expect("reviewer_direct_sol");
    let flag_cell = via_flag.resolved.layout.agent_cells().next().unwrap();
    let direct_cell = direct.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(flag_cell.kind.as_str(), "codex");
    assert_eq!(flag_cell.launch.model.as_deref(), Some("sol"));
    assert_eq!(flag_cell.launch.effort.as_deref(), Some("high"));
    assert_eq!(flag_cell.launch.effort, direct_cell.launch.effort);
    assert_eq!(flag_cell.system_prompt_file, direct_cell.system_prompt_file);
    assert!(
        flag_cell
            .system_prompt_file
            .as_ref()
            .unwrap()
            .origin()
            .ends_with("agents/codex.md")
    );
    assert_eq!(
        flag_cell.append_system_prompt_files.len(),
        direct_cell.append_system_prompt_files.len()
    );
    assert_eq!(
        flag_cell.launch.record.as_ref().unwrap().agent.as_deref(),
        Some("sol")
    );

    // Subagent scope at direct entry (--subagent-profile): advisor --agent astra and finder --model neon.
    let sub_flag = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        crate::config::effective::ProfileScope::Subagents,
        Some("advisor"),
        None,
        &super::LaunchOverrides {
            agent: Some("astra".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("advisor --subagent-profile --agent astra");
    let sub_cell = sub_flag.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(sub_cell.kind.as_str(), "codex");
    assert_eq!(sub_cell.launch.model.as_deref(), Some("astra"));
    assert_eq!(sub_cell.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        sub_cell.launch.record.as_ref().unwrap().agent.as_deref(),
        Some("astra")
    );

    let finder_neon = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        crate::config::effective::ProfileScope::Subagents,
        Some("finder"),
        None,
        &super::LaunchOverrides {
            preset: crate::agents::LaunchPreset {
                model: Some("neon".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("finder --subagent-profile --model neon");
    let finder_cell = finder_neon.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(finder_cell.kind.as_str(), "antigravity");
    assert_eq!(finder_cell.launch.model.as_deref(), Some("neon"));
    assert_eq!(finder_cell.launch.effort.as_deref(), Some("low"));

    // Tier fallback under --agent X: codex unavailable falls back to claude while keeping astra's
    // non-engine lines (mode: plan, craft) and recording astra.
    let fallback = crate::config::Selection::new(&machine.tiers)
        .agent(Some("astra"))
        .tier(Some(crate::config::tiers::ModelTier::Senior))
        .availability(|kind, _| {
            (kind == "codex").then_some(crate::agents::TierSkipReason::LoggedOut)
        });
    let resolved_fb = crate::harness::plan::resolve_launch(
        &effective,
        crate::config::effective::ProfileScope::Agents,
        &machine.agents.commands,
        Some("tiered"),
        &fallback,
    )
    .expect("tier fallback under astra");
    let fb_cell = resolved_fb.layout.agent_cells().next().unwrap();
    assert_eq!(fb_cell.kind.as_str(), "claude");
    assert_eq!(
        fb_cell.launch.mode,
        Some(crate::agents::PermissionMode::Plan)
    );
    assert_eq!(fb_cell.launch.effort.as_deref(), Some("low"));
    assert!(fb_cell.launch.tier.is_some());
    assert!(
        fb_cell
            .system_prompt_file
            .as_ref()
            .unwrap()
            .origin()
            .ends_with("agents/claude.md")
    );
    assert_eq!(fb_cell.append_system_prompt_files.len(), 2);
}

#[test]
fn bare_kind_with_markdown_override_and_routing_keeps_override_lines_and_tuning_sources() {
    use crate::config::effective::ProfileScope;
    use crate::harness::spec::TuningSource;

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(project.join(".rimz")).unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("subagents")).unwrap();
    for (rel, content) in [
        (
            "agents/claude.md",
            "---\ndescription: Claude base\ntools: [Read]\neffort: medium\n---\nClaude house.",
        ),
        (
            "agents/codex.md",
            "---\ndescription: Codex base\ntools: [Read]\neffort: medium\nfast: off\n---\nCodex house.",
        ),
        (
            "agents/mdp.md",
            "---\ndescription: Mdp\nagent: codex\nmodel: sol\neffort: low\nfast: on\n\
             budget: 7\nsubagents: [advisor]\ntools: [Bash, Read]\n---\nMdp craft.",
        ),
        (
            "agents/lead.md",
            "---\ndescription: Lead\nagent: claude\nmodel: opus\ntools: [Read]\n---\nLead craft.",
        ),
        (
            "subagents/advisor.md",
            "---\ndescription: Advisor\nagent: codex\ntools: [Read]\n---\nAdvisor craft.",
        ),
    ] {
        std::fs::write(root.path().join(rel), content).unwrap();
    }
    std::fs::write(
        root.path().join("config.toml"),
        "[tiers]\nintern = ['luna', 'haiku']\njunior = ['sol', 'sonnet']\n\
         senior = ['opus', 'astra']\nprincipal = ['fable']\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".rimz/config.toml"),
        "[profiles.tl]\ndescription = 'Tl'\nagent = 'claude'\neffort = 'high'\n\
         [profiles.tk]\ndescription = 'Tk'\nagent = 'codex'\neffort = 'low'\nfast = 'off'\n\
         [subagents.profiles.tsub]\ndescription = 'Tsub'\nagent = 'claude'\neffort = 'high'\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(&project, root.path()).unwrap();

    let machine = crate::config::MachineConfig::parse_text(
        &root.path().join("config.toml"),
        &std::fs::read_to_string(root.path().join("config.toml")).unwrap(),
        root.path(),
    )
    .unwrap();
    let effective = crate::config::effective::load_with_roots(&machine, &project, root.path())
        .expect("effective config");

    // Kind-base L under Markdown X with `--tier`: keeps X's effort, fast, budget, and craft,
    // attributed to X, with chain [claude, mdp, codex].
    let bare_tier = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Agents,
        Some("claude"),
        None,
        &super::LaunchOverrides {
            agent: Some("mdp".into()),
            tier: Some(crate::config::tiers::ModelTier::Senior),
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("claude --agent mdp --tier senior");
    let cell = bare_tier.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(cell.kind.as_str(), "codex");
    assert_eq!(cell.launch.model.as_deref(), Some("astra"));
    assert_eq!(cell.launch.effort.as_deref(), Some("low"));
    assert_eq!(
        cell.tuning.effort,
        Some(TuningSource::Profile("mdp".into()))
    );
    assert_eq!(cell.launch.fast, Some(true));
    assert_eq!(cell.tuning.fast, Some(TuningSource::Profile("mdp".into())));
    assert_eq!(cell.launch.budget.as_deref(), Some("$7.00"));
    assert!(
        cell.append_system_prompt_files
            .iter()
            .any(|src| src.origin().ends_with("agents/mdp.md")),
        "expected mdp craft in {:?}",
        cell.append_system_prompt_files
    );
    assert_eq!(
        crate::harness::spec::explain_profile_chain(
            "claude",
            Some("mdp"),
            cell.kind.as_str(),
            false,
            &effective.profiles,
            None,
        ),
        vec!["claude", "mdp", "codex"]
    );

    // Kind-base L under Markdown X with `--model`: keeps X's lines and prompt too.
    let bare_model = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Agents,
        Some("claude"),
        None,
        &super::LaunchOverrides {
            agent: Some("mdp".into()),
            preset: crate::agents::LaunchPreset {
                model: Some("astra".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("claude --agent mdp --model astra");
    let bare_model_cell = bare_model.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(bare_model_cell.kind.as_str(), "codex");
    assert_eq!(bare_model_cell.launch.model.as_deref(), Some("astra"));
    assert_eq!(bare_model_cell.launch.effort.as_deref(), Some("low"));
    assert_eq!(bare_model_cell.launch.budget.as_deref(), Some("$7.00"));
    assert!(
        bare_model_cell
            .append_system_prompt_files
            .iter()
            .any(|src| src.origin().ends_with("agents/mdp.md"))
    );

    // Markdown L under Markdown X with `--tier` and `--model`: tuning sources credit X, not L.
    for (label, overrides) in [
        (
            "lead --agent mdp --tier senior",
            super::LaunchOverrides {
                agent: Some("mdp".into()),
                tier: Some(crate::config::tiers::ModelTier::Senior),
                ..Default::default()
            },
        ),
        (
            "lead --agent mdp --model astra",
            super::LaunchOverrides {
                agent: Some("mdp".into()),
                preset: crate::agents::LaunchPreset {
                    model: Some("astra".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
    ] {
        let finalized = resolve::resolve_finalized_layout(
            None,
            &machine,
            &effective,
            ProfileScope::Agents,
            Some("lead"),
            None,
            &overrides,
            None,
            None,
            None,
            true,
            None,
        )
        .expect(label);
        let cell = finalized.resolved.layout.agent_cells().next().unwrap();
        assert_eq!(cell.launch.effort.as_deref(), Some("low"), "{label}");
        assert_eq!(
            cell.tuning.effort,
            Some(TuningSource::Profile("mdp".into())),
            "{label}"
        );
        assert_eq!(cell.launch.fast, Some(true), "{label}");
        assert_eq!(
            cell.tuning.fast,
            Some(TuningSource::Profile("mdp".into())),
            "{label}"
        );
    }

    // Project TOML combinations: TOML L under Markdown X, Markdown L under TOML X, and
    // project TOML child under direct Markdown X.
    let tl_under_mdp = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Agents,
        Some("tl"),
        None,
        &super::LaunchOverrides {
            agent: Some("mdp".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("tl --agent mdp");
    let tl_cell = tl_under_mdp.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(tl_cell.kind.as_str(), "codex");
    assert_eq!(tl_cell.launch.model.as_deref(), Some("sol"));
    assert_eq!(tl_cell.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        tl_cell.tuning.effort,
        Some(TuningSource::Profile("tl".into()))
    );

    let lead_under_tk = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Agents,
        Some("lead"),
        None,
        &super::LaunchOverrides {
            agent: Some("tk".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("lead --agent tk");
    let tk_cell = lead_under_tk.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(tk_cell.kind.as_str(), "codex");
    assert_eq!(tk_cell.launch.effort.as_deref(), Some("low"));
    assert_eq!(
        tk_cell.tuning.effort,
        Some(TuningSource::Profile("tk".into()))
    );
    assert_eq!(tk_cell.launch.fast, Some(false));
    assert_eq!(
        tk_cell.tuning.fast,
        Some(TuningSource::Profile("tk".into()))
    );

    let tsub_under_mdp = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Subagents,
        Some("tsub"),
        None,
        &super::LaunchOverrides {
            agent: Some("mdp".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        None,
    )
    .expect("tsub --subagent-profile --agent mdp");
    let tsub_cell = tsub_under_mdp.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(tsub_cell.kind.as_str(), "codex");
    assert_eq!(tsub_cell.launch.model.as_deref(), Some("sol"));
    assert_eq!(tsub_cell.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        crate::harness::subagent_policy::allowed_specs(
            Some("tsub"),
            Some("mdp"),
            Some("codex"),
            ProfileScope::Subagents,
            &effective.profiles,
            &effective.subagent_profiles,
        ),
        None
    );

    // Inline layout holding a bare/kind-base cell and a Markdown cell under one Markdown X,
    // routed with `--tier` and with `--model`: both cells keep the routed model, tier stamp,
    // and X's lines without rebuilding `lead` away from its routed row.
    for (label, overrides, expected_model, expect_tier) in [
        (
            "claude,lead --agent mdp --tier senior",
            super::LaunchOverrides {
                agent: Some("mdp".into()),
                tier: Some(crate::config::tiers::ModelTier::Senior),
                ..Default::default()
            },
            "astra",
            Some(crate::config::tiers::ModelTier::Senior),
        ),
        (
            "claude,lead --agent mdp --model gpt-6",
            super::LaunchOverrides {
                agent: Some("mdp".into()),
                preset: crate::agents::LaunchPreset {
                    model: Some("gpt-6".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            "gpt-6",
            None,
        ),
    ] {
        let finalized = resolve::resolve_finalized_layout(
            None,
            &machine,
            &effective,
            ProfileScope::Agents,
            Some("claude,lead"),
            None,
            &overrides,
            None,
            None,
            None,
            false,
            None,
        )
        .expect(label);
        let cells: Vec<_> = finalized.resolved.layout.agent_cells().collect();
        assert_eq!(cells.len(), 2, "{label}");
        for (idx, cell) in cells.iter().enumerate() {
            assert_eq!(cell.kind.as_str(), "codex", "{label} cell {idx}");
            assert_eq!(
                cell.launch.model.as_deref(),
                Some(expected_model),
                "{label} cell {idx}"
            );
            assert_eq!(
                cell.launch
                    .tier
                    .as_ref()
                    .map(|stamp| (stamp.tier, stamp.model.as_str())),
                expect_tier.map(|tier| (tier, expected_model)),
                "{label} cell {idx}"
            );
            assert_eq!(
                cell.launch.effort.as_deref(),
                Some("low"),
                "{label} cell {idx}"
            );
            assert_eq!(
                cell.tuning.effort,
                Some(TuningSource::Profile("mdp".into())),
                "{label} cell {idx}"
            );
            assert_eq!(cell.launch.fast, Some(true), "{label} cell {idx}");
            assert_eq!(
                cell.tuning.fast,
                Some(TuningSource::Profile("mdp".into())),
                "{label} cell {idx}"
            );
            assert_eq!(
                cell.launch.budget.as_deref(),
                Some("$7.00"),
                "{label} cell {idx}"
            );
            assert!(
                cell.append_system_prompt_files
                    .iter()
                    .any(|src| src.origin().ends_with("agents/mdp.md")),
                "{label} cell {idx}: expected mdp craft in {:?}",
                cell.append_system_prompt_files
            );
        }
        assert!(
            cells[1]
                .append_system_prompt_files
                .iter()
                .any(|src| src.origin().ends_with("agents/lead.md")),
            "{label}: expected lead craft in {:?}",
            cells[1].append_system_prompt_files
        );
    }
}

#[test]
fn review_findings_toml_role_ancestor_tiered_model_and_variant() {
    use crate::config::effective::ProfileScope;
    let mini_args: Vec<String> = crate::agents::find_runtime("opencode-mini")
        .unwrap()
        .variant
        .unwrap()
        .args
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("agents")).unwrap();
    std::fs::create_dir(home.path().join("teams")).unwrap();
    for (name, body) in [
        (
            "agents/claude.md",
            "---\ndescription: Claude base\neffort: medium\ntools: [Read]\n---\nClaude house.\n",
        ),
        (
            "agents/codex.md",
            "---\ndescription: Codex base\neffort: medium\ntools: [Read]\n---\nCodex house.\n",
        ),
        (
            "agents/opencode.md",
            "---\ndescription: OpenCode base\neffort: medium\ntools: [Read]\n---\nOpenCode house.\n",
        ),
        (
            "agents/psol.md",
            "---\ndescription: Sol ancestor\nagent: codex\nmodel: sol\neffort: low\ntools: [+Bash]\n---\nPsol craft.\n",
        ),
        (
            "agents/tiered_anc.md",
            "---\ndescription: Tiered ancestor\nagent: codex\ntier: senior\ntools: [+Bash]\n---\nTiered anc craft.\n",
        ),
        (
            "agents/oc_base.md",
            "---\ndescription: OpenCode ancestor\nagent: opencode\ntools: [+Bash]\n---\nOc base craft.\n",
        ),
        (
            "agents/reviewer.md",
            "---\ndescription: Reviewer\nagent: psol\nmodel: opus\neffort: high\n---\nReviewer craft.\n",
        ),
        (
            "agents/tiered_child.md",
            "---\ndescription: Tiered child\nagent: psol\ntier: senior\n---\nTiered child craft.\n",
        ),
        (
            "agents/model_child.md",
            "---\ndescription: Model child\nagent: tiered_anc\nmodel: opus\n---\nModel child craft.\n",
        ),
        (
            "agents/oc_reviewer.md",
            "---\ndescription: Oc reviewer\nagent: oc_base\nmodel: opus\n---\nOc reviewer craft.\n",
        ),
        (
            "teams/crew.md",
            "---\ndescription: Crew\nleader: reviewer\nstages: [Review]\nroles:\n  - agent: psol\n    role: reviewer\n    model: opus\n    effort: high\n    owns: [Review]\n---\nTeam.\n",
        ),
    ] {
        std::fs::write(home.path().join(name), body).unwrap();
    }
    std::fs::create_dir(repo.path().join(".rimz")).unwrap();
    std::fs::write(
        repo.path().join(".rimz/config.toml"),
        r#"
[profiles.toml_claude]
description = "TOML claude"
agent = "claude"
model = "opus"

[profiles.toml_codex]
description = "TOML codex"
agent = "codex"
model = "sol"

[profiles.toml_sub]
description = "TOML sub"
agent = "toml_codex"

[profiles.toml_sub_opus]
description = "TOML sub opus"
agent = "toml_codex"
model = "opus"

[profiles.toml_other]
description = "TOML other"
agent = "toml_codex"
model = "astra"

[agents.teams.toml_squad]
roles = [{ role = "lead", profile = "toml_claude", model = "sonnet" }]

[agents.teams.toml_squad_native]
roles = [{ role = "lead", profile = "toml_claude", model = "sol" }]
"#,
    )
    .unwrap();
    crate::trust::grant_with_roots(repo.path(), home.path()).unwrap();
    let machine = crate::config::MachineConfig::parse_text(
        &home.path().join("config.toml"),
        "[tiers]\nintern = [\"luna\"]\njunior = [\"opencode/gpt-5.4-mini\", \"haiku\"]\n",
        home.path(),
    )
    .unwrap();
    let effective =
        crate::config::effective::load_with_roots(&machine, repo.path(), home.path()).unwrap();

    // Finding 1: TOML role writing foreign model drops both role and profile foreign models with
    // one warning naming the role's model.
    let f1 = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Agents,
        Some("toml_squad"),
        None,
        &super::LaunchOverrides {
            agent: Some("codex".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        false,
        None,
    )
    .expect("toml_squad --agent codex");
    let f1_cell = f1.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(f1_cell.kind.as_str(), "codex");
    assert_eq!(f1_cell.launch.model, None, "opus must not leak onto seat");
    let f1_warnings: Vec<String> = f1.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(f1_warnings.len(), 1, "{f1_warnings:?}");
    assert!(
        f1_warnings[0].contains("drops model `sonnet`"),
        "{f1_warnings:?}"
    );
    let f1_native = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Agents,
        Some("toml_squad_native"),
        None,
        &super::LaunchOverrides {
            agent: Some("codex".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        false,
        None,
    )
    .expect("toml_squad_native --agent codex");
    let f1_native_cell = f1_native.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(f1_native_cell.kind.as_str(), "codex");
    assert_eq!(f1_native_cell.launch.model.as_deref(), Some("sol"));
    assert!(f1_native.warnings.is_empty(), "{:?}", f1_native.warnings);

    // The final kind base still fills the model the dropped role model leaves unset.
    let based = tempfile::tempdir().unwrap();
    std::fs::create_dir(based.path().join("agents")).unwrap();
    std::fs::write(
        based.path().join("agents/claude.md"),
        "---\ndescription: Claude base\ntools: [Read]\n---\nClaude house.\n",
    )
    .unwrap();
    std::fs::write(
        based.path().join("agents/codex.md"),
        "---\ndescription: Codex base\nmodel: gpt-6\ntools: [Read]\n---\nCodex house.\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(repo.path(), based.path()).unwrap();
    let based_machine = crate::config::MachineConfig::parse_text(
        &based.path().join("config.toml"),
        "",
        based.path(),
    )
    .unwrap();
    let based_effective =
        crate::config::effective::load_with_roots(&based_machine, repo.path(), based.path())
            .unwrap();
    let f1_based = resolve::resolve_finalized_layout(
        None,
        &based_machine,
        &based_effective,
        ProfileScope::Agents,
        Some("toml_squad"),
        None,
        &super::LaunchOverrides {
            agent: Some("codex".into()),
            ..Default::default()
        },
        None,
        None,
        None,
        false,
        None,
    )
    .expect("toml_squad --agent codex over a base model");
    let f1_based_cell = f1_based.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(f1_based_cell.launch.model.as_deref(), Some("gpt-6"));
    assert!(f1_based.warnings.is_empty(), "{:?}", f1_based.warnings);

    // Finding 2: TOML profile under itself or a TOML ancestor keeps X's nearest model.
    for (spec, agent_override, expected_model) in [
        ("toml_codex", "toml_codex", "sol"),
        ("toml_sub", "toml_codex", "sol"),
        ("toml_sub_opus", "toml_codex", "sol"),
        ("toml_codex", "toml_other", "astra"),
    ] {
        let finalized = resolve::resolve_finalized_layout(
            None,
            &machine,
            &effective,
            ProfileScope::Agents,
            Some(spec),
            None,
            &super::LaunchOverrides {
                agent: Some(agent_override.into()),
                ..Default::default()
            },
            None,
            None,
            None,
            false,
            None,
        )
        .unwrap_or_else(|err| panic!("{spec} --agent {agent_override}: {err}"));
        let cell = finalized.resolved.layout.agent_cells().next().unwrap();
        assert_eq!(
            cell.kind.as_str(),
            "codex",
            "{spec} --agent {agent_override}"
        );
        assert_eq!(
            cell.launch.model.as_deref(),
            Some(expected_model),
            "{spec} --agent {agent_override}"
        );
    }

    // Finding 3: ancestor X whose engine L overrides applies C1's engine rule across direct,
    // supervised, and replay paths, with crafts entering once.
    for (label, spec, agent_override, model_flag, expected_kind, expected_model, expect_tier) in [
        (
            "reviewer --agent psol",
            "reviewer",
            "psol",
            None,
            "codex",
            "sol",
            None,
        ),
        (
            "crew --agent psol",
            "crew",
            "psol",
            None,
            "codex",
            "sol",
            None,
        ),
        (
            "oc_reviewer --agent oc_base --model sonnet",
            "oc_reviewer",
            "oc_base",
            Some("sonnet"),
            "opencode",
            "sonnet",
            None,
        ),
        (
            "tiered_child --agent psol",
            "tiered_child",
            "psol",
            None,
            "codex",
            "sol",
            None,
        ),
        (
            "model_child --agent tiered_anc",
            "model_child",
            "tiered_anc",
            None,
            "codex",
            "astra",
            Some(crate::config::tiers::ModelTier::Senior),
        ),
    ] {
        let finalized = resolve::resolve_finalized_layout(
            None,
            &machine,
            &effective,
            ProfileScope::Agents,
            Some(spec),
            None,
            &super::LaunchOverrides {
                agent: Some(agent_override.into()),
                preset: crate::agents::LaunchPreset {
                    model: model_flag.map(str::to_owned),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
            None,
            None,
            false,
            None,
        )
        .unwrap_or_else(|err| panic!("{label}: {err}"));
        let cell = finalized.resolved.layout.agent_cells().next().unwrap();
        assert_eq!(cell.kind.as_str(), expected_kind, "{label}");
        assert_eq!(
            cell.launch.model.as_deref(),
            Some(expected_model),
            "{label}"
        );
        assert_eq!(
            cell.launch.tier.as_ref().map(|t| t.tier),
            expect_tier,
            "{label}"
        );
        // Verify supervised entry matches for non-team profiles.
        if spec != "crew" {
            let workspace =
                crate::workspace::WorkspaceResolver::resolve(repo.path(), None).unwrap();
            let mut req = crate::harness::run::SupervisedRunRequest::new(
                spec.to_owned(),
                "check".to_owned(),
                None,
                crate::agents::ManagedLaunchState::PendingResolution,
            );
            req.agent = Some(agent_override.to_owned());
            req.model = model_flag.map(str::to_owned);
            let planned = crate::harness::plan::plan_supervised_launch(
                &req,
                None,
                None,
                None,
                &workspace,
                &machine,
                effective.clone(),
                |_| crate::store::writer::LaunchLogin::RoomDefault,
                &mut Vec::new(),
            )
            .unwrap_or_else(|err| panic!("supervised {label}: {err}"));
            let sup_cell = planned.layout.agent_cells().next().unwrap();
            assert_eq!(sup_cell.kind.as_str(), expected_kind, "sup {label}");
            assert_eq!(
                sup_cell.launch.model.as_deref(),
                Some(expected_model),
                "sup {label}"
            );
            assert_eq!(
                sup_cell.launch.tier.as_ref().map(|t| t.tier),
                expect_tier,
                "sup {label}"
            );
        }
        // Replay from what fresh launch recorded must match with no degrade.
        let record = crate::agents::LaunchRecord {
            model: cell.launch.model.clone(),
            effort: cell.launch.effort.clone(),
            agent: Some(agent_override.into()),
            ..Default::default()
        };
        let posture = if spec == "crew" {
            crate::harness::resume::resolve_member_posture(
                crate::harness::resume::PostureRequest {
                    record: Some(&record),
                    profile: cell.launch.profile.as_deref(),
                    kind: &cell.kind,
                    stamped_mode: cell.launch.mode,
                    stamped_tier: cell.launch.tier.as_deref(),
                },
                &effective.profiles,
                None,
                &effective.teams,
                Some("crew"),
                Some("reviewer"),
            )
        } else {
            crate::harness::resume::resolve_posture(
                crate::harness::resume::PostureRequest {
                    record: Some(&record),
                    profile: cell.launch.profile.as_deref(),
                    kind: &cell.kind,
                    stamped_mode: cell.launch.mode,
                    stamped_tier: cell.launch.tier.as_deref(),
                },
                &effective.profiles,
                None,
            )
        };
        assert!(
            posture.degraded.is_none(),
            "replay {label}: {:?}",
            posture.degraded
        );
        assert_eq!(
            posture.launch.model.as_deref(),
            Some(expected_model),
            "replay {label}"
        );
        assert_eq!(
            posture.launch.tier.as_ref().map(|t| t.tier),
            expect_tier,
            "replay {label}"
        );
    }

    // Finding 7: TOML launch (and TOML team seat) under tiered Markdown X with `--model` runs the
    // requested model with no tier stamp.
    for spec in ["toml_claude", "toml_squad"] {
        let finalized = resolve::resolve_finalized_layout(
            None,
            &machine,
            &effective,
            ProfileScope::Agents,
            Some(spec),
            None,
            &super::LaunchOverrides {
                agent: Some("tiered_anc".into()),
                preset: crate::agents::LaunchPreset {
                    model: Some("sol".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
            None,
            None,
            false,
            None,
        )
        .unwrap_or_else(|err| panic!("{spec} --agent tiered_anc --model sol: {err}"));
        let cell = finalized.resolved.layout.agent_cells().next().unwrap();
        assert_eq!(cell.kind.as_str(), "codex", "{spec}");
        assert_eq!(cell.launch.model.as_deref(), Some("sol"), "{spec}");
        assert_eq!(cell.launch.tier, None, "{spec}: tier stamp must be cleared");
    }

    // Finding 8: `--agent <variant>` with `--tier` on a kind base keeps the variant when the
    // routed kind matches X's kind, and clears it on fallback to another kind.
    let f8_match = resolve::resolve_finalized_layout(
        None,
        &machine,
        &effective,
        ProfileScope::Agents,
        Some("claude"),
        None,
        &super::LaunchOverrides {
            agent: Some("opencode-mini".into()),
            tier: Some(crate::config::tiers::ModelTier::Junior),
            ..Default::default()
        },
        None,
        None,
        None,
        false,
        None,
    )
    .expect("claude --agent opencode-mini --tier junior");
    let f8_cell = f8_match.resolved.layout.agent_cells().next().unwrap();
    assert_eq!(f8_cell.kind.as_str(), "opencode");
    assert!(
        f8_cell.args.starts_with(&mini_args),
        "expected opencode-mini args {mini_args:?} in {:?}",
        f8_cell.args
    );

    // Replay on `opencode` keeps `opencode-mini` variant args; replay on another kind (`claude`)
    // drops them.
    let f8_record_oc = crate::agents::LaunchRecord {
        model: f8_cell.launch.model.clone(),
        effort: f8_cell.launch.effort.clone(),
        agent: Some("opencode-mini".into()),
        ..Default::default()
    };
    let f8_replay_oc = crate::harness::resume::resolve_posture(
        crate::harness::resume::PostureRequest {
            record: Some(&f8_record_oc),
            profile: Some("claude"),
            kind: &crate::ids::AgentKind::new_unchecked("opencode"),
            stamped_mode: f8_cell.launch.mode,
            stamped_tier: f8_cell.launch.tier.as_deref(),
        },
        &effective.profiles,
        None,
    );
    assert!(
        f8_replay_oc.degraded.is_none(),
        "{:?}",
        f8_replay_oc.degraded
    );
    assert!(
        f8_replay_oc.launch.args.starts_with(&mini_args),
        "opencode replay must keep opencode-mini args: {:?}",
        f8_replay_oc.launch.args
    );
    let f8_replay_claude = crate::harness::resume::resolve_posture(
        crate::harness::resume::PostureRequest {
            record: Some(&f8_record_oc),
            profile: Some("claude"),
            kind: &crate::ids::AgentKind::new_unchecked("claude"),
            stamped_mode: f8_cell.launch.mode,
            stamped_tier: None,
        },
        &effective.profiles,
        None,
    );
    assert!(
        f8_replay_claude.degraded.is_none(),
        "{:?}",
        f8_replay_claude.degraded
    );
    assert!(
        !f8_replay_claude.launch.args.starts_with(&mini_args),
        "claude replay must drop opencode-mini args: {:?}",
        f8_replay_claude.launch.args
    );
}

#[test]
fn launch_and_replay_engine_matrix_matches_one_chain_rule() {
    use crate::config::effective::ProfileScope;
    let mini_args: Vec<String> = crate::agents::find_runtime("opencode-mini")
        .unwrap()
        .variant
        .unwrap()
        .args
        .iter()
        .map(|a| (*a).to_owned())
        .collect();
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("agents")).unwrap();
    std::fs::create_dir(home.path().join("teams")).unwrap();
    // Intentionally omit `agents/antigravity.md`: a bare `antigravity` stays native, and a
    // definition onto it runs on the built-in base.
    for (name, body) in [
        (
            "agents/claude.md",
            "---\ndescription: Claude base\neffort: medium\ntools: [Read]\n---\nClaude house.\n",
        ),
        (
            "agents/codex.md",
            "---\ndescription: Codex base\neffort: medium\ntools: [Read]\n---\nCodex house.\n",
        ),
        (
            "agents/opencode.md",
            "---\ndescription: OpenCode base\neffort: medium\ntools: [Read]\n---\nOpenCode house.\n",
        ),
        (
            "agents/md_anc.md",
            "---\ndescription: Markdown ancestor\nagent: codex\nmodel: sol\neffort: low\n---\nAnc craft.\n",
        ),
        (
            "agents/md_l.md",
            "---\ndescription: Markdown leaf\nagent: md_anc\nmodel: opus\neffort: high\n---\nLeaf craft.\n",
        ),
        (
            "agents/x_model.md",
            "---\ndescription: X model\nagent: codex\nmodel: sol\n---\nX model craft.\n",
        ),
        (
            "agents/x_tier.md",
            "---\ndescription: X tier\nagent: codex\ntier: senior\n---\nX tier craft.\n",
        ),
        (
            "agents/x_plain.md",
            "---\ndescription: X plain\nagent: codex\n---\nX plain craft.\n",
        ),
        (
            "teams/md_team.md",
            "---\ndescription: Markdown team\nleader: lead\nstages: [Plan]\nroles:\n  - agent: md_anc\n    role: lead\n    model: sonnet\n    owns: [Plan]\n---\nMd team.\n",
        ),
    ] {
        std::fs::write(home.path().join(name), body).unwrap();
    }
    std::fs::create_dir(repo.path().join(".rimz")).unwrap();
    std::fs::write(
        repo.path().join(".rimz/config.toml"),
        r#"
[profiles.toml_anc]
description = "TOML ancestor"
agent = "codex"
model = "sol"

[profiles.toml_l]
description = "TOML leaf"
agent = "toml_anc"
model = "opus"

[profiles.x_toml]
description = "TOML X"
agent = "codex"
model = "astra"

[agents.teams.toml_team]
roles = [{ role = "lead", profile = "toml_l", model = "sonnet" }]
"#,
    )
    .unwrap();
    crate::trust::grant_with_roots(repo.path(), home.path()).unwrap();
    let machine = crate::config::MachineConfig::parse_text(
        &home.path().join("config.toml"),
        "[tiers]\nsenior = [\"opus\", \"astra\"]\n",
        home.path(),
    )
    .unwrap();
    let effective =
        crate::config::effective::load_with_roots(&machine, repo.path(), home.path()).unwrap();

    #[derive(Clone, Copy)]
    enum LForm {
        Markdown,
        Toml,
        KindBase,
        BareKind,
        TomlSeat,
        MdSeat,
    }
    #[derive(Clone, Copy)]
    enum XForm {
        None,
        MdModel,
        MdTier,
        MdPlain,
        TomlModel,
        KindBase,
        BareKind,
        Variant,
        SelfL,
        Ancestor,
    }
    #[derive(Clone, Copy)]
    enum Sel {
        None,
        Model,
        Tier,
    }

    let l_spec = |l: LForm| match l {
        LForm::Markdown => "md_l",
        LForm::Toml => "toml_l",
        LForm::KindBase => "claude",
        LForm::BareKind => "antigravity",
        LForm::TomlSeat => "toml_team.lead",
        LForm::MdSeat => "md_team.lead",
    };

    for l in [
        LForm::Markdown,
        LForm::Toml,
        LForm::KindBase,
        LForm::BareKind,
        LForm::TomlSeat,
        LForm::MdSeat,
    ] {
        for x in [
            XForm::None,
            XForm::MdModel,
            XForm::MdTier,
            XForm::MdPlain,
            XForm::TomlModel,
            XForm::KindBase,
            XForm::BareKind,
            XForm::Variant,
            XForm::SelfL,
            XForm::Ancestor,
        ] {
            let x_name: Option<&str> = match x {
                XForm::None => None,
                XForm::MdModel => Some("x_model"),
                XForm::MdTier => Some("x_tier"),
                XForm::MdPlain => Some("x_plain"),
                XForm::TomlModel => Some("x_toml"),
                XForm::KindBase => Some("codex"),
                XForm::BareKind => Some("antigravity"),
                XForm::Variant => Some("opencode-mini"),
                XForm::SelfL => match l {
                    LForm::Markdown => Some("md_l"),
                    LForm::Toml | LForm::TomlSeat => Some("toml_l"),
                    LForm::KindBase => Some("claude"),
                    LForm::BareKind => Some("antigravity"),
                    LForm::MdSeat => Some("md_team.lead"),
                },
                XForm::Ancestor => match l {
                    LForm::Markdown | LForm::MdSeat => Some("md_anc"),
                    LForm::Toml | LForm::TomlSeat => Some("toml_anc"),
                    LForm::KindBase | LForm::BareKind => continue,
                },
            };

            // Expected X kind, variant, and engine write.
            let (x_kind, x_variant, x_model, x_tier): (
                Option<&str>,
                Option<&str>,
                Option<&str>,
                bool,
            ) = match x {
                XForm::None => (None, None, None, false),
                XForm::MdModel => (Some("codex"), None, Some("sol"), false),
                XForm::MdTier => (Some("codex"), None, None, true),
                XForm::MdPlain => (Some("codex"), None, None, false),
                XForm::TomlModel => (Some("codex"), None, Some("astra"), false),
                XForm::KindBase => (Some("codex"), None, None, false),
                XForm::BareKind => (Some("antigravity"), None, None, false),
                XForm::Variant => (Some("opencode"), Some("mini"), None, false),
                XForm::SelfL => match l {
                    LForm::Markdown | LForm::Toml | LForm::TomlSeat => {
                        (Some("claude"), None, Some("opus"), false)
                    }
                    LForm::KindBase => (Some("claude"), None, None, false),
                    LForm::BareKind => (Some("antigravity"), None, None, false),
                    LForm::MdSeat => (Some("claude"), None, Some("sonnet"), false),
                },
                XForm::Ancestor => (Some("codex"), None, Some("sol"), false),
            };
            let l_orig_kind = match l {
                LForm::Markdown
                | LForm::Toml
                | LForm::KindBase
                | LForm::TomlSeat
                | LForm::MdSeat => "claude",
                LForm::BareKind => "antigravity",
            };
            let l_own_model = match l {
                LForm::Markdown | LForm::Toml => Some("opus"),
                LForm::TomlSeat | LForm::MdSeat => Some("sonnet"),
                LForm::KindBase | LForm::BareKind => None,
            };

            for sel in [Sel::None, Sel::Model, Sel::Tier] {
                // Pick a `--model` compatible with X's kind (or L's kind when X is None).
                let model_flag = match sel {
                    Sel::Model => Some(match x_kind.unwrap_or(l_orig_kind) {
                        "codex" => "gpt-6",
                        "opencode" => "sonnet",
                        "antigravity" => "neon",
                        _ => "haiku",
                    }),
                    _ => None,
                };
                let tier_flag =
                    matches!(sel, Sel::Tier).then_some(crate::config::tiers::ModelTier::Senior);
                let label = format!(
                    "L={} X={:?} sel={}",
                    l_spec(l),
                    x_name,
                    match sel {
                        Sel::None => "none",
                        Sel::Model => "model",
                        Sel::Tier => "tier",
                    }
                );

                // Compute expected (kind, variant, model, tier) from the single C1 engine rule.
                let (exp_kind, exp_variant, exp_model, exp_tier): (
                    &str,
                    Option<&str>,
                    Option<&str>,
                    Option<crate::config::tiers::ModelTier>,
                ) = match sel {
                    Sel::Model => {
                        let k = x_kind.unwrap_or(l_orig_kind);
                        let v = if x_kind == Some(k) { x_variant } else { None };
                        (k, v, model_flag, None)
                    }
                    Sel::Tier => {
                        // Senior tier table is ["opus" (claude), "astra" (codex)].
                        // Prefer X's family if X is set, else L's natural family.
                        // `opencode` runs other providers' models so it picks `opus` on `opencode`.
                        let pref = x_kind.unwrap_or(l_orig_kind);
                        let (k, m) = match pref {
                            "codex" => ("codex", "astra"),
                            "opencode" => ("opencode", "opus"),
                            _ => ("claude", "opus"),
                        };
                        let v = if x_kind == Some(k) { x_variant } else { None };
                        (k, v, Some(m), Some(crate::config::tiers::ModelTier::Senior))
                    }
                    Sel::None => {
                        let k = x_kind.unwrap_or(l_orig_kind);
                        let v = if x_kind == Some(k) { x_variant } else { None };
                        if x_tier {
                            // `x_tier` prefers codex -> senior picks `astra` on `codex`.
                            (
                                "codex",
                                None,
                                Some("astra"),
                                Some(crate::config::tiers::ModelTier::Senior),
                            )
                        } else if let Some(xm) = x_model {
                            (k, v, Some(xm), None)
                        } else if x_name.is_some() {
                            let kept = l_own_model.filter(|m| {
                                crate::config::engine::keeps_inherited_model(l_orig_kind, k, m)
                            });
                            (k, v, kept, None)
                        } else {
                            (k, v, l_own_model, None)
                        }
                    }
                };

                let finalized = resolve::resolve_finalized_layout(
                    None,
                    &machine,
                    &effective,
                    ProfileScope::Agents,
                    Some(l_spec(l)),
                    None,
                    &super::LaunchOverrides {
                        agent: x_name.map(str::to_owned),
                        tier: tier_flag,
                        preset: crate::agents::LaunchPreset {
                            model: model_flag.map(str::to_owned),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    None,
                    None,
                    None,
                    false,
                    None,
                )
                .unwrap_or_else(|err| panic!("fresh {label}: {err}"));
                let cell = finalized.resolved.layout.agent_cells().next().unwrap();
                assert_eq!(cell.kind.as_str(), exp_kind, "fresh kind {label}");
                assert_eq!(
                    cell.args.starts_with(&mini_args),
                    exp_variant.is_some(),
                    "fresh variant {label}: {:?}",
                    cell.args
                );
                assert_eq!(
                    cell.launch.model.as_deref(),
                    exp_model,
                    "fresh model {label}"
                );
                assert_eq!(
                    cell.launch.tier.as_ref().map(|t| t.tier),
                    exp_tier,
                    "fresh tier {label}"
                );

                // Replay from what fresh launch recorded: must end on identical
                // (kind, variant, model, tier) with no degrade.
                let record = crate::agents::LaunchRecord {
                    model: cell.launch.model.clone(),
                    effort: cell.launch.effort.clone(),
                    agent: x_name.map(str::to_owned),
                    ..Default::default()
                };
                let (team_name, role_name) = match l {
                    LForm::TomlSeat => (Some("toml_team"), Some("lead")),
                    LForm::MdSeat => (Some("md_team"), Some("lead")),
                    _ => (None, None),
                };
                let posture = if team_name.is_some() {
                    crate::harness::resume::resolve_member_posture(
                        crate::harness::resume::PostureRequest {
                            record: Some(&record),
                            profile: cell.launch.profile.as_deref(),
                            kind: &cell.kind,
                            stamped_mode: cell.launch.mode,
                            stamped_tier: cell.launch.tier.as_deref(),
                        },
                        &effective.profiles,
                        None,
                        &effective.teams,
                        team_name,
                        role_name,
                    )
                } else {
                    crate::harness::resume::resolve_posture(
                        crate::harness::resume::PostureRequest {
                            record: Some(&record),
                            profile: cell.launch.profile.as_deref(),
                            kind: &cell.kind,
                            stamped_mode: cell.launch.mode,
                            stamped_tier: cell.launch.tier.as_deref(),
                        },
                        &effective.profiles,
                        None,
                    )
                };
                assert!(
                    posture.degraded.is_none(),
                    "replay {label}: degraded {:?}",
                    posture.degraded
                );
                assert_eq!(
                    posture.launch.args.starts_with(&mini_args),
                    exp_variant.is_some(),
                    "replay variant {label}: {:?}",
                    posture.launch.args
                );
                assert_eq!(
                    posture.launch.model.as_deref(),
                    exp_model,
                    "replay model {label}"
                );
                assert_eq!(
                    posture.launch.tier.as_ref().map(|t| t.tier),
                    exp_tier,
                    "replay tier {label}"
                );
            }
        }
    }
}
