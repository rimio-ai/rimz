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

struct ReopenFixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    workspace: ResolvedWorkspace,
    store: Store,
    parent_pane: crate::ids::PaneId,
    child: AgentState,
    run: RunRecord,
}

impl ReopenFixture {
    fn new(
        parent_mode: Option<crate::agents::PermissionMode>,
        parent_isolation: Option<crate::config::Isolation>,
    ) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("reopen")
            .tempdir_in("/tmp")
            .unwrap();
        let home = dir.path().join("home");
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();
        let ambient = BTreeMap::from([("HOME".to_owned(), home.display().to_string())]);
        crate::agents::find_definition("claude")
            .unwrap()
            .managed_integration()
            .unwrap()
            .install(&ambient, Default::default())
            .unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(&cwd);
        let paths =
            crate::StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            crate::RuntimePaths::under(workspace_id.clone(), &dir.path().join("rt")).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();
        let workspace = ResolvedWorkspace {
            workspace_id: workspace_id.clone(),
            project_root: cwd.clone(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: cwd.clone(),
            worktree_branch: None,
            session_name: "room".to_owned(),
            mux_hint: Some(crate::ids::MuxName::Tmux),
        };
        let parent_kind = crate::ids::AgentKind::new_unchecked("claude");
        let parent_id = crate::ids::AgentSessionId::from("parent-1");
        let parent_pane = crate::ids::PaneId::parse("tmux:%1").unwrap();
        let mut parent_obs = crate::agents::AgentLifecycleObservation::new(
            Some(parent_id.clone()),
            crate::agents::LifecycleSignal::Registered,
        );
        parent_obs.agent_name = Some("parent".to_owned());
        parent_obs.worktree_path = Some(cwd.display().to_string());
        parent_obs.launch.mode = parent_mode;
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                workspace_id.clone(),
                "room",
                parent_kind.as_str(),
                "SessionStart",
                &parent_obs,
            ))
            .unwrap();
        store
            .attach_agent_pane(
                &parent_kind,
                &parent_id,
                Some(&parent_id),
                &crate::ids::LoginName::default(),
                "room",
                &parent_pane,
                crate::pane::RuntimeOwner::new(
                    crate::pane::RuntimeOwnerKind::Agent,
                    "parent",
                    999_991,
                    None,
                ),
                None,
                parent_isolation,
                Some(&parent_obs.launch),
            )
            .unwrap();

        let child_kind = crate::ids::AgentKind::new_unchecked("claude");
        let child_id = crate::ids::AgentSessionId::from("child-1");
        let child_launch = crate::ids::AgentSessionId::from("launch-child-1");
        let transcript = home.join(".claude/projects/test/child-1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();

        let mut child_obs = crate::agents::AgentLifecycleObservation::new(
            Some(child_id.clone()),
            crate::agents::LifecycleSignal::Registered,
        );
        child_obs.launch_id = Some(child_launch.clone());
        child_obs.agent_name = Some("otter".to_owned());
        child_obs.worktree_path = Some(cwd.display().to_string());
        child_obs.transcript_path = Some(transcript.display().to_string());
        child_obs.launch.parent_agent_id = Some(parent_id.clone());
        child_obs.launch.parent_agent_kind = Some(parent_kind.clone());
        child_obs.launch.launch_depth = Some(1);
        child_obs.launch.mode = Some(crate::agents::PermissionMode::Auto);
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                workspace_id.clone(),
                "room",
                child_kind.as_str(),
                "SessionStart",
                &child_obs,
            ))
            .unwrap();
        store
            .attach_agent_pane(
                &child_kind,
                &child_id,
                Some(&child_launch),
                &crate::ids::LoginName::default(),
                "room",
                &crate::ids::PaneId::parse("tmux:%9").unwrap(),
                crate::pane::RuntimeOwner::new(
                    crate::pane::RuntimeOwnerKind::Agent,
                    "otter",
                    999_990,
                    None,
                ),
                None,
                None,
                Some(&child_obs.launch),
            )
            .unwrap();
        let mut child_end = crate::agents::AgentLifecycleObservation::new(
            Some(child_id.clone()),
            crate::agents::LifecycleSignal::Ended,
        );
        child_end.launch_id = Some(child_launch.clone());
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                workspace_id.clone(),
                "room",
                child_kind.as_str(),
                "SessionEnd",
                &child_end,
            ))
            .unwrap();

        let mut run = RunRecord::new(
            workspace_id,
            child_kind,
            crate::agents::PermissionMode::Auto,
            "investigate".to_owned(),
            cwd.clone(),
        );
        run.agent_id = Some(child_id.clone());
        run.agent_name = Some("otter".to_owned());
        run.status = crate::store::run::RunStatus::Completed;
        run.completed_at = Some(Timestamp::now());
        crate::harness::run::create(&paths, &run).unwrap();

        let audit = store
            .runtime_projection(crate::RuntimeScope::Audit)
            .unwrap();
        let child = audit
            .agents
            .into_iter()
            .find(|agent| agent.agent_id == child_id)
            .unwrap();
        Self {
            _dir: dir,
            home,
            cwd,
            workspace,
            store,
            parent_pane,
            child,
            run,
        }
    }

    fn ambient(&self) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([("HOME".to_owned(), self.home.display().to_string())])
    }

    fn live_parent_pane(&self) -> crate::pane::PaneRef {
        let mut pane = crate::pane::PaneRef::from_id(self.parent_pane.clone());
        pane.session_name = "room".to_owned();
        pane.view_name = Some("work".to_owned());
        pane
    }

    fn bind_child_in_store(&self) {
        let launch_id = self.child.launch_id.as_ref().unwrap();
        self.store
            .attach_agent_pane(
                &self.child.kind,
                &self.child.agent_id,
                Some(launch_id),
                &crate::ids::LoginName::default(),
                "room",
                &crate::ids::PaneId::parse("tmux:%2").unwrap(),
                crate::pane::RuntimeOwner::new(
                    crate::pane::RuntimeOwnerKind::Agent,
                    "otter",
                    999_992,
                    None,
                ),
                None,
                None,
                None,
            )
            .unwrap();
        let mut resumed = crate::agents::AgentLifecycleObservation::new(
            Some(self.child.agent_id.clone()),
            crate::agents::LifecycleSignal::Registered,
        );
        resumed.launch_id = Some(launch_id.clone());
        self.store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                self.workspace.workspace_id.clone(),
                "room",
                self.child.kind.as_str(),
                "rimz.agent-resumed",
                &resumed,
            ))
            .unwrap();
    }
}

#[test]
fn reopen_child_opens_in_parent_zone_without_queuing_message_and_keeps_run_and_lineage() {
    let fixture = ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Host),
    );
    let store_clone = Store::open(
        fixture.store.paths().clone(),
        fixture.store.runtime_paths().clone(),
    )
    .unwrap();
    let workspace_id = fixture.workspace.workspace_id.clone();
    let child_kind = fixture.child.kind.clone();
    let child_id = fixture.child.agent_id.clone();
    let launch_id = fixture.child.launch_id.clone().unwrap();
    let backend = crate::launcher::test_backend::Backend {
        listed: Some(vec![fixture.live_parent_pane()]),
        split_ok: true,
        on_open: Some(Box::new(move || {
            store_clone
                .attach_agent_pane(
                    &child_kind,
                    &child_id,
                    Some(&launch_id),
                    &crate::ids::LoginName::default(),
                    "room",
                    &crate::ids::PaneId::parse("tmux:%2").unwrap(),
                    crate::pane::RuntimeOwner::new(
                        crate::pane::RuntimeOwnerKind::Agent,
                        "otter",
                        999_992,
                        None,
                    ),
                    None,
                    None,
                    None,
                )
                .unwrap();
            let mut resumed = crate::agents::AgentLifecycleObservation::new(
                Some(child_id.clone()),
                crate::agents::LifecycleSignal::Registered,
            );
            resumed.launch_id = Some(launch_id.clone());
            store_clone
                .append_event(&crate::EventEnvelope::agent_lifecycle(
                    workspace_id.clone(),
                    "room",
                    child_kind.as_str(),
                    "rimz.agent-resumed",
                    &resumed,
                ))
                .unwrap();
        })),
        ..Default::default()
    };

    let outcome = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &fixture.child,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(200),
        &mut crate::launcher::Unattended,
    )
    .unwrap();

    assert_eq!(outcome, ReopenChildOutcome::Reopened);
    let splits = backend.splits.lock().unwrap();
    assert_eq!(splits.len(), 1);
    assert_eq!(
        splits[0].target,
        crate::mux::SplitTarget::SessionPane {
            session_name: "room".to_owned(),
            pane_id: fixture.parent_pane.clone(),
        }
    );
    assert!(!splits[0].focus);
    assert!(
        fixture.store.list_messages().unwrap().is_empty(),
        "reopen must not queue any message"
    );
    let run = crate::harness::run::load(fixture.store.paths(), &fixture.run.run_id).unwrap();
    assert_eq!(run.status, crate::store::run::RunStatus::Completed);
    assert_eq!(run.keep, fixture.run.keep);
}

#[test]
fn reopen_child_caps_mode_and_isolation_from_parent_record_not_ambient_env() {
    let mut fixture = ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Sandbox),
    );
    let backend = crate::launcher::test_backend::Backend {
        listed: Some(vec![fixture.live_parent_pane()]),
        split_ok: true,
        ..Default::default()
    };
    fixture.child.mode = Some(crate::agents::PermissionMode::Yolo);
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &fixture.child,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(50),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("exceeds parent mode"),
        "expected parent mode cap refusal, got: {err}"
    );
    assert!(backend.splits.lock().unwrap().is_empty());

    fixture.child.mode = Some(crate::agents::PermissionMode::Auto);
    fixture.child.isolation = Some(crate::config::Isolation::Host);
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &fixture.child,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(50),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("isolation") || err.contains("sandbox"),
        "expected parent isolation cap refusal, got: {err}"
    );
    fixture.child.isolation = None;
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &fixture.child,
        true,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(50),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("supervised resume refused"),
        "agent-called reopen must run unattended folder-trust preflight, got: {err}"
    );
    assert!(backend.splits.lock().unwrap().is_empty());
}

#[test]
fn reopen_child_refuses_when_parent_ended_or_has_no_pane() {
    let fixture = ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Host),
    );
    let backend = crate::launcher::test_backend::Backend::default();
    fixture
        .store
        .append_event(&crate::EventEnvelope::agent_lifecycle(
            fixture.workspace.workspace_id.clone(),
            "room",
            "claude",
            "SessionEnd",
            &crate::agents::AgentLifecycleObservation::new(
                Some("parent-1".into()),
                crate::agents::LifecycleSignal::Ended,
            ),
        ))
        .unwrap();
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &fixture.child,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(50),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("parent @parent has ended"),
        "expected ended parent refusal, got: {err}"
    );
    assert!(!err.contains("rimz subagents"), "{err}");

    let fixture = ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Host),
    );
    let mut orphan = fixture.child.clone();
    orphan.parent_agent_id = Some("missing-parent".into());
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &orphan,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(50),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("parent"),
        "expected missing parent refusal, got: {err}"
    );
    assert!(!err.contains("rimz subagents"), "{err}");

    // Live parent that was never bound to a pane (`parent.pane == None`).
    let fixture = ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Host),
    );
    let mut unbound_obs = crate::agents::AgentLifecycleObservation::new(
        Some("parent-unbound".into()),
        crate::agents::LifecycleSignal::Registered,
    );
    unbound_obs.agent_name = Some("unbound-parent".to_owned());
    unbound_obs.worktree_path = Some(fixture.cwd.display().to_string());
    fixture
        .store
        .append_event(&crate::EventEnvelope::agent_lifecycle(
            fixture.workspace.workspace_id.clone(),
            "room",
            "claude",
            "SessionStart",
            &unbound_obs,
        ))
        .unwrap();
    let mut unbound_child = fixture.child.clone();
    unbound_child.parent_agent_id = Some("parent-unbound".into());
    let backend = crate::launcher::test_backend::Backend {
        listed: Some(Vec::new()),
        split_ok: true,
        ..Default::default()
    };
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &unbound_child,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(50),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("parent @unbound-parent has no") && err.contains("pane"),
        "expected unbound parent refusal, got: {err}"
    );
    assert!(!err.contains("rimz subagents"), "{err}");
    assert!(backend.splits.lock().unwrap().is_empty());
    assert!(backend.tabs.lock().unwrap().is_empty());

    // Live parent whose stored pane is no longer listed by the mux.
    let fixture = ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Host),
    );
    let backend = crate::launcher::test_backend::Backend {
        listed: Some(Vec::new()),
        split_ok: true,
        ..Default::default()
    };
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &fixture.child,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(50),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("parent @parent has no") && err.contains("pane"),
        "expected unlisted parent pane refusal, got: {err}"
    );
    assert!(!err.contains("rimz subagents"), "{err}");
    assert!(
        backend.splits.lock().unwrap().is_empty() && backend.tabs.lock().unwrap().is_empty(),
        "must not open a pane or companion tab when the parent's pane is gone"
    );
}

#[test]
fn reopen_child_refuses_when_bind_does_not_land_within_ceiling() {
    let fixture = ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Host),
    );
    let backend = crate::launcher::test_backend::Backend {
        listed: Some(vec![fixture.live_parent_pane()]),
        split_ok: true,
        ..Default::default()
    };
    let err = reopen_child_with(
        &fixture.workspace,
        &fixture.store,
        &fixture.child,
        false,
        crate::config::MachineConfig::default(),
        &fixture.ambient(),
        &backend,
        std::time::Duration::from_millis(20),
        &mut crate::launcher::Unattended,
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("did not bind"),
        "expected bind ceiling refusal, got: {err}"
    );
    assert_eq!(backend.splits.lock().unwrap().len(), 1);
}

#[test]
fn concurrent_reopens_yield_one_pane_and_second_returns_already_bound() {
    let fixture = std::sync::Arc::new(ReopenFixture::new(
        Some(crate::agents::PermissionMode::Auto),
        Some(crate::config::Isolation::Host),
    ));
    let f_hook = std::sync::Arc::clone(&fixture);
    let backend = std::sync::Arc::new(crate::launcher::test_backend::Backend {
        listed: Some(vec![fixture.live_parent_pane()]),
        split_ok: true,
        on_open: Some(Box::new(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            f_hook.bind_child_in_store();
        })),
        ..Default::default()
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let f = std::sync::Arc::clone(&fixture);
        let b = std::sync::Arc::clone(&backend);
        let bar = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            bar.wait();
            reopen_child_with(
                &f.workspace,
                &f.store,
                &f.child,
                false,
                crate::config::MachineConfig::default(),
                &f.ambient(),
                b.as_ref(),
                std::time::Duration::from_millis(500),
                &mut crate::launcher::Unattended,
            )
            .unwrap()
        }));
    }
    let mut outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    outcomes.sort_by_key(|o| matches!(o, ReopenChildOutcome::AlreadyBound));
    assert_eq!(
        outcomes,
        vec![
            ReopenChildOutcome::Reopened,
            ReopenChildOutcome::AlreadyBound
        ]
    );
    assert_eq!(
        backend.splits.lock().unwrap().len(),
        1,
        "two concurrent reopens must open only one pane"
    );
}

#[test]
fn reopen_child_succeeds_when_parent_is_hibernated_booked_to_stop_or_mid_turn() {
    for case in ["hibernated", "booked", "running"] {
        let fixture = ReopenFixture::new(
            Some(crate::agents::PermissionMode::Auto),
            Some(crate::config::Isolation::Host),
        );
        match case {
            "hibernated" => {
                crate::store::hibernate::record(
                    fixture.store.paths(),
                    crate::store::hibernate::Entry {
                        kind: crate::ids::AgentKind::new_unchecked("claude"),
                        agent_id: "parent-1".into(),
                        pane_id: fixture.parent_pane.clone(),
                        scope: fixture.cwd.clone(),
                        provider_pid: 999_991,
                        frozen_at: Timestamp::now(),
                        reclaimed_bytes: 0,
                        anon_at_freeze: 0,
                        step: 1,
                        wake_error: None,
                    },
                )
                .unwrap();
            }
            "booked" => {
                crate::store::idle_stop::arm(
                    fixture.store.paths(),
                    crate::store::idle_stop::IdleStopRequest {
                        kind: crate::ids::AgentKind::new_unchecked("claude"),
                        agent_id: "parent-1".into(),
                        stop: crate::agents::IdleStop {
                            after_secs: 600,
                            requested_at: Timestamp::now(),
                            requested_by: None,
                        },
                    },
                )
                .unwrap();
            }
            "running" => {
                fixture
                    .store
                    .append_event(&crate::EventEnvelope::agent_lifecycle(
                        fixture.workspace.workspace_id.clone(),
                        "room",
                        "claude",
                        "UserPromptSubmit",
                        &crate::agents::AgentLifecycleObservation::new(
                            Some("parent-1".into()),
                            crate::agents::LifecycleSignal::TurnStarted { turn_id: None },
                        ),
                    ))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let store_clone = Store::open(
            fixture.store.paths().clone(),
            fixture.store.runtime_paths().clone(),
        )
        .unwrap();
        let workspace_id = fixture.workspace.workspace_id.clone();
        let child_kind = fixture.child.kind.clone();
        let child_id = fixture.child.agent_id.clone();
        let launch_id = fixture.child.launch_id.clone().unwrap();
        let backend = crate::launcher::test_backend::Backend {
            listed: Some(vec![fixture.live_parent_pane()]),
            split_ok: true,
            on_open: Some(Box::new(move || {
                store_clone
                    .attach_agent_pane(
                        &child_kind,
                        &child_id,
                        Some(&launch_id),
                        &crate::ids::LoginName::default(),
                        "room",
                        &crate::ids::PaneId::parse("tmux:%2").unwrap(),
                        crate::pane::RuntimeOwner::new(
                            crate::pane::RuntimeOwnerKind::Agent,
                            "otter",
                            999_992,
                            None,
                        ),
                        None,
                        None,
                        None,
                    )
                    .unwrap();
                let mut resumed = crate::agents::AgentLifecycleObservation::new(
                    Some(child_id.clone()),
                    crate::agents::LifecycleSignal::Registered,
                );
                resumed.launch_id = Some(launch_id.clone());
                store_clone
                    .append_event(&crate::EventEnvelope::agent_lifecycle(
                        workspace_id.clone(),
                        "room",
                        child_kind.as_str(),
                        "rimz.agent-resumed",
                        &resumed,
                    ))
                    .unwrap();
            })),
            ..Default::default()
        };
        let outcome = reopen_child_with(
            &fixture.workspace,
            &fixture.store,
            &fixture.child,
            false,
            crate::config::MachineConfig::default(),
            &fixture.ambient(),
            &backend,
            std::time::Duration::from_millis(200),
            &mut crate::launcher::Unattended,
        )
        .unwrap_or_else(|err| panic!("parent case {case} must allow reopen: {err:#}"));
        assert_eq!(outcome, ReopenChildOutcome::Reopened, "case {case}");
    }
}
