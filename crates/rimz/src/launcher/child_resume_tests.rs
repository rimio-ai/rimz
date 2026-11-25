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
        None,
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
                seat_consensus_skill: posture.launch.seat_consensus_skill,
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

#[test]
fn child_resume_resolves_recorded_agent_override_from_agents_table() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("subagents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude house prompt.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/codex.md"),
        "---\ndescription: Codex base\n---\nCodex house prompt.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/astra.md"),
        "---\ndescription: Astra\nagent: codex\nmodel: astra\neffort: low\ntools: [Bash, Read]\n---\nAstra craft.\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("subagents/advisor.md"),
        "---\ndescription: Advisor\nagent: claude\nmodel: opus\neffort: high\ntools: [Read]\n---\nAdvise carefully.\n",
    )
    .unwrap();
    let machine =
        crate::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    let launch =
        crate::config::effective::load_with_roots(&machine, root.path(), root.path()).unwrap();
    let mut child = AgentState::stub("codex", "child", crate::agents::AgentStatus::Success);
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

    let posture = child_posture(&child, &launch);

    assert!(posture.degraded.is_none(), "{:?}", posture.degraded);
    assert_eq!(posture.launch.model.as_deref(), Some("astra"));
    assert_eq!(posture.launch.effort.as_deref(), Some("high"));
    let prompt_files: Vec<String> = posture
        .launch
        .system_prompt_file
        .iter()
        .chain(&posture.launch.append_system_prompt_files)
        .map(|src| src.origin().display().to_string())
        .collect();
    assert!(
        prompt_files.iter().any(|p| p.ends_with("agents/codex.md")),
        "{prompt_files:?}"
    );
    assert!(
        prompt_files
            .iter()
            .any(|p| p.ends_with("subagents/advisor.md")),
        "{prompt_files:?}"
    );
    assert!(
        !prompt_files.iter().any(|p| p.ends_with("agents/claude.md")),
        "{prompt_files:?}"
    );
}

#[test]
fn a_limit_failure_refuses_until_its_recorded_refill_on_the_same_login_when_no_window_reads_spent()
{
    use crate::store::run::{LimitFailure, LimitFailureCause};

    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let store = Store::open(
        crate::disk::paths::StatePaths::under(workspace_id.clone(), &dir.path().join("state"))
            .unwrap(),
        crate::disk::paths::RuntimePaths::under(workspace_id.clone(), &dir.path().join("rt"))
            .unwrap(),
    )
    .unwrap();
    let mut child = AgentState::stub("claude", "child", crate::agents::AgentStatus::Failed);
    child.name = Some("otter".to_owned());
    let now = Timestamp::now();
    let failed = |cause, refill_at| {
        let mut run = RunRecord::new(
            workspace_id.clone(),
            child.kind.clone(),
            crate::agents::PermissionMode::Auto,
            "task".to_owned(),
            PathBuf::from("/tmp/project"),
        );
        run.limit_failure = Some(LimitFailure {
            cause,
            refill_at,
            label: None,
        });
        run
    };
    let ahead = Some(now + std::time::Duration::from_secs(2 * 3600 - 30));
    let same = crate::agents::RoomAccounts::default();
    let catalog = crate::agents::LoginCatalog::default();
    let refusal = limit_refusal(
        &store,
        &child,
        &failed(LimitFailureCause::RefillKnown, ahead),
        &same,
        &catalog,
    )
    .expect("a refill recorded ahead refuses the message");
    assert!(
        refusal.starts_with("not sent: @otter's provider limit refills in 2h00m (at ")
            && refusal.ends_with("Run `rimz wait --in 120m`, then send it again."),
        "{refusal}"
    );
    for run in [
        failed(
            LimitFailureCause::RefillKnown,
            Some(now - std::time::Duration::from_secs(60)),
        ),
        failed(LimitFailureCause::RefillUnknown, None),
    ] {
        assert_eq!(limit_refusal(&store, &child, &run, &same, &catalog), None);
    }

    // On another login the run's refill names another account's quota: only that
    // account's cache can refuse, as it did before the run carried one.
    let room: crate::agents::RoomAccounts =
        crate::ids::RoomLogins::from([(child.kind.clone(), "work".parse().unwrap())]).into();
    for history in ["shared", "standalone"] {
        let accounts = crate::agents::LoginCatalog::from_config(
            &toml::from_str(&format!(
                "[claude.work]\nhome = \"/srv/claude-work\"\nhistory = \"{history}\"\n"
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            limit_refusal(
                &store,
                &child,
                &failed(LimitFailureCause::RefillKnown, ahead),
                &room,
                &accounts,
            ),
            None,
            "{history}"
        );
    }
}

#[test]
fn a_recorded_refill_yields_on_the_same_login_to_a_newer_available_read_or_identity_switch() {
    use crate::agents::account::{
        AccountStamps, AccountsCache, ProviderRecord, RateLimitCacheEntry, RateLimitsCache,
    };
    use crate::store::run::{LimitFailure, LimitFailureCause};
    use std::time::Duration;

    let now = Timestamp::now();
    let failed_at = now - Duration::from_secs(600);
    let mut child = AgentState::stub("claude", "child", crate::agents::AgentStatus::Failed);
    child.name = Some("otter".to_owned());
    let key = child.login_key();
    let same = crate::agents::RoomAccounts::default();
    let catalog = crate::agents::LoginCatalog::default();
    // An available 5-hour window observed `read_secs_ago`, and a login whose identity last
    // switched `floor_secs_ago`.
    let refusal = |read_secs_ago: u64, floor_secs_ago: Option<u64>| {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime =
            crate::disk::paths::RuntimePaths::under(workspace_id.clone(), &dir.path().join("rt"))
                .unwrap();
        let store = Store::open(
            crate::disk::paths::StatePaths::under(workspace_id.clone(), &dir.path().join("state"))
                .unwrap(),
            runtime.clone(),
        )
        .unwrap();
        crate::disk::atomic::write_temp_then_rename_cache(
            &RateLimitsCache::shared_path(&runtime),
            &RateLimitsCache {
                entries: [(
                    key.clone().into(),
                    RateLimitCacheEntry {
                        limits: crate::agents::AgentRateLimits {
                            windows: vec![crate::agents::RateLimitWindow {
                                used_percentage: Some(20),
                                resets_at: Some(now + Duration::from_secs(3 * 3600)),
                                duration_mins: Some(300),
                                observed_at: Some(now - Duration::from_secs(read_secs_ago)),
                                ..Default::default()
                            }],
                        },
                        ..Default::default()
                    },
                )]
                .into(),
                ..Default::default()
            },
        )
        .unwrap();
        crate::disk::atomic::write_temp_then_rename_cache(
            &runtime.shared_accounts_path(),
            &AccountsCache {
                logins: [(
                    key.clone(),
                    ProviderRecord {
                        consecutive_failures: 0,
                        version_probed_at_ms: None,
                        inputs_stamp: None,
                        profile: None,
                        probe_error: None,
                        login: None,
                        probed_at_ms: 1,
                        ok: true,
                        account: None,
                        login_at_ms: None,
                        stamps: AccountStamps {
                            floor_at_ms: floor_secs_ago.map(|secs| {
                                u64::try_from((now - Duration::from_secs(secs)).as_millisecond())
                                    .unwrap()
                            }),
                            ..Default::default()
                        },
                        live_credentials: Default::default(),
                    },
                )]
                .into(),
            },
        )
        .unwrap();
        let mut run = RunRecord::new(
            workspace_id,
            child.kind.clone(),
            crate::agents::PermissionMode::Auto,
            "task".to_owned(),
            PathBuf::from("/tmp/project"),
        );
        run.completed_at = Some(failed_at);
        run.limit_failure = Some(LimitFailure {
            cause: LimitFailureCause::RefillKnown,
            refill_at: Some(now + Duration::from_secs(2 * 3600)),
            label: None,
        });
        limit_refusal(&store, &child, &run, &same, &catalog)
    };

    assert!(
        refusal(1200, None).is_some(),
        "quota available before the failure predates the limit"
    );
    assert!(
        refusal(1200, Some(1800)).is_some(),
        "an identity switch before the failure changes nothing"
    );
    assert_eq!(
        refusal(60, None),
        None,
        "quota available in a read newer than the failure"
    );
    assert_eq!(
        refusal(1200, Some(300)),
        None,
        "the login switched identity since the failure"
    );
}
