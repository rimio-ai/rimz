use super::*;
use rimz::harness::ancestry::{LaunchFocus, resolve_caller};
use rimz::harness::launch::{ExecAction, ExecRequest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreshReason {
    NoResumeSupport,
    NoRecordedConversation,
}

impl FreshReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NoResumeSupport => "no resume support",
            Self::NoRecordedConversation => "no recorded conversation",
        }
    }
}

pub(super) fn restart_agent(reference: String, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let agent = crate::cli::resolve_agent_one(
        &ctx.store,
        &snapshot,
        &reference,
        None,
        &ctx.address_context(),
    )?
    .clone();
    let peers = rimz::address::addressable_agents(&snapshot);
    let focus = LaunchFocus::resolve(false, resolve_caller(&snapshot.agents).as_ref());
    let message = restart_resolved(&ctx, &agent, &peers, focus)?;
    writeln!(crate::cli::render::out(), "{message}")?;
    Ok(())
}

pub(in crate::cli) fn restart_resolved(
    ctx: &Ctx,
    agent: &AgentState,
    peers: &[&AgentState],
    focus: LaunchFocus,
) -> Result<String> {
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let old_pane = agent
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone())
        .ok_or_else(|| anyhow::anyhow!("agent has no bound pane; nothing to restart"))?;
    let cwd = agent
        .worktree_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.worktree_root.clone());
    let machine_config = crate::cli::machine_config();
    let posture = restart_posture(agent, workspace, &machine_config)?;
    let adapter = rimz::agents::find_definition(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let isolation = rimz::config::Isolation::resolve(
        agent.isolation,
        posture.launch.isolation_default,
        machine_config.agents.isolation,
    );
    rimz::sandbox::preflight_launch(
        isolation,
        &agent.kind,
        posture.launch.skills.is_some(),
        adapter.manual_skill(),
    )?;
    let cell = restart_cell(agent, &posture);

    // Fail at the entry point if this project's configured launch environment
    // is not trusted, before the old pane is touched.
    rimz::harness::launch::preflight_agent_kind(
        &workspace.project_root,
        agent.kind.as_str(),
        &cwd,
    )?;

    let logins = rimz::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &rimz::config::MachineConfig::load_lenient(),
    )?;
    let catalog = rimz::agents::machine_login_catalog();
    let (action, login, fresh_reason) = relaunch_action(agent, &logins, &catalog, &cwd)?;
    rimz::agents::session_login(&agent.kind, login.as_ref(), &machine_config.accounts)?
        .health(&catalog.native_ambient(&agent.kind, &rimz::agents::ambient_env()))?;
    if isolation == rimz::config::Isolation::Host {
        rimz::harness::launch::preflight_agent_process(
            &workspace.project_root,
            &relaunch_request(agent, &posture, action.clone(), login.clone(), None),
            &cwd,
            Some(store.runtime_paths()),
        )?;
    }
    let fresh_batch = if fresh_reason.is_some() {
        Some(append_fresh_launch(
            store,
            workspace,
            agent,
            &cwd,
            cell,
            posture.launch.mode,
        )?)
    } else {
        None
    };
    let fresh_identity = fresh_batch
        .as_ref()
        .map(AgentLaunchBatch::single_identity)
        .transpose()?;
    let invocation = relaunch_request(agent, &posture, action, login, fresh_identity);
    let pane_name = invocation
        .identity
        .params
        .profile
        .clone()
        .unwrap_or_else(|| invocation.kind.to_string());
    let argv = rimz::harness::launch::exec_argv(
        &rimz::proc::rimz_exe(),
        store.runtime_paths(),
        &invocation,
    )?;
    let env = rimz::room::pane_identity_env(workspace, &cwd, agent.channel.as_deref(), false);
    let backend = rimz::mux::backend_for(old_pane.mux());
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();

    if focus.takes_focus()
        && let Err(err) = rimz::mux::focus_anchor::execute_action(
            backend.as_ref(),
            ctx.runtime(),
            &workspace.session_name,
            old_pane.clone(),
        )
        .context("focusing the agent pane for restart")
    {
        if let Some(batch) = &fresh_batch {
            let _ = store.fail_agent_launch_batch(batch);
        }
        return Err(err);
    }
    if let Err(err) = backend
        .split_pane(SplitPaneOptions {
            target: rimz::mux::SplitTarget::SessionPane {
                session_name: workspace.session_name.clone(),
                pane_id: old_pane.clone(),
            },
            cwd: Some(cwd.display().to_string()),
            command: Some(argv),
            title: Some(pane_name),
            close_on_exit: false,
            env,
            placement: rimz::mux::SplitPlacement::Directional(direction),
            focus: focus.takes_focus(),
        })
        .context("opening the replacement agent pane")
    {
        if let Some(batch) = &fresh_batch {
            let _ = store.fail_agent_launch_batch(batch);
        }
        return Err(err);
    }
    if let Err(err) = settle_peer_before_restart(store, agent) {
        let _ = writeln!(
            std::io::stderr(),
            "warning: could not settle the peer's open turn before restart: {err:#}"
        );
    }
    backend
        .close_pane(&workspace.session_name, &old_pane)
        .context("closing the replaced agent pane")?;

    // A fresh restart leaves the old identity behind, so every row pinned to it
    // is dead. A resumed one is the same session continuing: its declared
    // bindings carry over rather than depending on a re-arm nothing orders
    // against this removal, and only the rows no registration restores go.
    let scope = if fresh_reason.is_some() {
        rimz::harness::schedule::arm::RetireScope::Session
    } else {
        rimz::harness::schedule::arm::RetireScope::UnrestorableOnly
    };
    let retired = rimz::harness::schedule::arm::retire_session(
        &workspace.project_root,
        &agent.kind,
        &agent.agent_id,
        scope,
    );
    let note = dropped_note(retired_count(retired, &mut crate::cli::render::err())?);

    if let (Some(identity), Some(reason)) = (fresh_identity, fresh_reason) {
        Ok(format!(
            "restarted fresh as @{} — {reason}{note}",
            identity.name
        ))
    } else {
        let handle = rimz::address::agent_handle(agent, peers, true);
        Ok(format!(
            "restarted {handle} (resumed session {}){note}",
            agent.agent_id
        ))
    }
}

pub(in crate::cli) fn relaunch_action(
    agent: &AgentState,
    logins: &rimz::agents::RoomAccounts,
    catalog: &rimz::agents::LoginCatalog,
    cwd: &Path,
) -> Result<(
    rimz::harness::launch::ExecAction,
    Option<rimz::ids::LoginName>,
    Option<&'static str>,
)> {
    let login = rimz::harness::resume::relaunch_login(agent, logins, catalog)?;
    let adapter = rimz::agents::find_definition(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let resume_support = !agent.agent_id.is_provisional()
        && agent.worktree_path.is_some()
        && rimz::harness::launch::compile_provider_argv(
            adapter,
            agent.kind.as_str(),
            &ExecAction::Resume {
                session_id: agent.agent_id.to_string(),
                extra_args: Vec::new(),
            },
            cwd,
        )
        .is_ok();
    let session_present = rimz::harness::resume::resume_session_present(agent);
    let fresh_reason = fresh_reason(resume_support, session_present);
    let action = if fresh_reason.is_some() {
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        }
    } else {
        ExecAction::Resume {
            session_id: agent.agent_id.to_string(),
            extra_args: Vec::new(),
        }
    };
    Ok((action, login, fresh_reason.map(FreshReason::as_str)))
}

pub(in crate::cli) fn relaunch_request(
    agent: &AgentState,
    posture: &ResumePosture,
    action: ExecAction,
    login: Option<rimz::ids::LoginName>,
    fresh_identity: Option<&AgentLaunchIdentity>,
) -> ExecRequest {
    let identity_name = fresh_identity.map_or(agent.name.as_deref(), |identity| {
        Some(identity.name.as_str())
    });
    let restart_params = rimz::agents::LaunchParams {
        record: posture.launch.record.clone(),
        parent_agent_id: agent.parent_agent_id.clone(),
        parent_agent_kind: agent.parent_agent_kind.clone(),
        launch_depth: agent.launch_depth,
        launched_by: agent.launched_by.clone().map(Box::new),
        profile: agent.profile.clone(),
        login: fresh_identity.map_or(login, |identity| identity.launch.login.clone()),
        tier: posture.launch.tier.clone(),
        role: agent.role.clone(),
        team: agent.team.clone(),
        loop_task: agent.loop_task.clone(),
        launch_group: agent.launch_group.clone(),
        launch_ordinal: agent.launch_ordinal,
        channel: agent.channel.clone(),
        mode: posture.launch.mode,
        isolation: agent.isolation,
        model: posture.launch.model.clone(),
        effort: posture.launch.effort.clone(),
        budget: posture.launch.budget.clone(),
        kind_ordinal: None,
    };
    ExecRequest {
        close_pane_on_exit: true,
        identity: rimz::harness::launch::ExecIdentity {
            resume_model_override: false,
            name: identity_name.map(ToOwned::to_owned),
            name_explicit: fresh_identity
                .map_or(agent.name_explicit, |identity| identity.name_explicit),
            launch_id: fresh_identity
                .map(|identity| identity.agent_id.to_string())
                .or_else(|| agent.launch_id.as_ref().map(ToString::to_string)),
            params: restart_params,
        },
        ..posture.launch.exec_request(agent.kind.clone(), action)
    }
}

/// The posture this restart replays, from the same seam resume uses.
///
/// Restart is interactive, so a profile that now names a different provider
/// refuses here rather than degrading — switching providers under a running
/// agent is the user's call. Every other degrade prints and continues.
fn restart_posture(
    agent: &AgentState,
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
) -> Result<ResumePosture> {
    let launch = rimz::config::effective::load(machine_config, &workspace.project_root)?;
    launch.block_set_failure()?;
    if agent
        .profile
        .as_deref()
        .is_some_and(|name| !launch.profiles.0.contains_key(name))
    {
        launch.block_failed_reference(agent.profile.as_deref(), None)?;
    }
    let posture = rimz::harness::resume::resolve_member_posture(
        rimz::harness::resume::PostureRequest {
            record: agent.record.as_deref(),
            profile: agent.profile.as_deref(),
            kind: &agent.kind,
            stamped_mode: agent.mode,
            stamped_tier: agent.tier.as_deref(),
        },
        &launch.profiles,
        &launch.teams,
        agent.team.as_deref(),
        agent.role.as_deref(),
    );
    match &posture.degraded {
        Some(reason @ PostureDegrade::KindChanged { .. }) => {
            bail!(
                "{reason}; launch it fresh to change providers (rimz agents <profile> --agent <kind>)"
            )
        }
        Some(reason @ PostureDegrade::PromptUnsupported { .. }) => bail!("{reason}"),
        Some(reason) => writeln!(
            crate::cli::render::err(),
            "rimz: {reason}; restarting as bare {}",
            agent.kind
        )?,
        None => {}
    }
    Ok(posture)
}

/// The layout cell a fresh restart launches, carrying the replayed posture and
/// the agent's durable identity.
fn restart_cell(agent: &AgentState, posture: &ResumePosture) -> Cell {
    Cell::Agent(AgentCell {
        resume_model_override: false,
        kind: agent.kind.clone(),
        args: posture.launch.args.clone(),
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: posture.launch.skills.clone(),
        allowed_tools: posture.launch.allowed_tools.clone(),
        isolation_default: posture.launch.isolation_default,
        launch: rimz::agents::LaunchParams {
            profile: agent.profile.clone(),
            tier: posture.launch.tier.clone(),
            record: Some(rimz::agents::LaunchRecord::replay_or_new(
                posture.launch.record.as_deref(),
                posture.launch.model.as_deref(),
                posture.launch.effort.as_deref(),
            )),
            role: agent.role.clone(),
            mode: posture.launch.mode,
            isolation: agent.isolation,
            model: posture.launch.model.clone(),
            effort: posture.launch.effort.clone(),
            budget: posture.launch.budget.clone(),
            ..Default::default()
        },
    })
}

/// The waits a retirement dropped, counted whether or not it then failed;
/// the failure goes to `err`.
fn retired_count(
    retired: Result<usize, rimz::harness::schedule::arm::RetireFailure>,
    err: &mut impl Write,
) -> Result<usize> {
    match retired {
        Ok(dropped) => Ok(dropped),
        Err(failure) => {
            writeln!(err, "rimz: {failure}")?;
            Ok(failure.dropped())
        }
    }
}

/// What the restart line says about waits the replaced session took with it.
/// Team-declared bindings come back at the resumed registration, so only the
/// rest are worth reporting.
fn dropped_note(dropped: usize) -> String {
    match dropped {
        0 => String::new(),
        1 => "; 1 armed wait dropped".to_owned(),
        count => format!("; {count} armed waits dropped"),
    }
}

fn fresh_reason(resume_support: bool, session_present: bool) -> Option<FreshReason> {
    if !resume_support {
        Some(FreshReason::NoResumeSupport)
    } else if !session_present {
        Some(FreshReason::NoRecordedConversation)
    } else {
        None
    }
}

fn append_fresh_launch(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    agent: &AgentState,
    cwd: &Path,
    cell: Cell,
    mode: Option<PermissionMode>,
) -> Result<AgentLaunchBatch> {
    let layout = LayoutSpec::single(cell);
    let mut requests = rimz::harness::plan::launch_identity_requests(
        &layout,
        None,
        None,
        agent.team.as_deref(),
        None,
        agent.channel.as_deref(),
        None,
        None,
        None,
    )?;
    let request = requests
        .first_mut()
        .context("restart produced no fresh launch request")?;
    request.name = agent.name.as_ref().map_or(AgentLaunchName::Mint, |name| {
        AgentLaunchName::Soft(name.clone())
    });
    request.launch.profile = agent.profile.clone();
    request.launch.parent_agent_id = agent.parent_agent_id.clone();
    request.launch.parent_agent_kind = agent.parent_agent_kind.clone();
    request.launch.launch_depth = agent.launch_depth;
    request.launch.launched_by = agent.launched_by.clone().map(Box::new);
    request.launch.mode = mode;
    request.launch.isolation = agent.isolation;
    request.launch.role = agent.role.clone();
    request.launch.team = agent.team.clone();
    request.launch.loop_task = agent.loop_task.clone();
    request.launch.launch_group = agent.launch_group.clone();
    request.launch.launch_ordinal = agent.launch_ordinal;
    request.launch.channel = agent.channel.clone();
    let batch = store.begin_agent_launch_batch(
        &requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.to_path_buf(),
            branch: None,
            description: None,
        },
    )?;
    batch.single_identity()?;
    Ok(batch)
}

fn settle_peer_before_restart(store: &rimz::Store, agent: &AgentState) -> Result<()> {
    if rimz::harness::run::fail_peer_run(store, agent, "peer restarted")?.is_some()
        && let Some((_, launcher)) = agent.launcher()
    {
        rimz::harness::orphan_sweep::spawn_digest_helper(store.runtime_paths(), launcher.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                isolation_default: Some(rimz::config::Isolation::Sandbox),
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
                isolation_default: Some(rimz::config::Isolation::Sandbox),
                kind: agent.kind.clone(),
                action: ExecAction::Resume {
                    session_id: "a1".to_owned(),
                    extra_args: posture.launch.args.clone(),
                },
                system_prompt_file: posture.launch.system_prompt_file.clone(),
                append_system_prompt_files: posture.launch.append_system_prompt_files.clone(),
                team_prompt: posture.launch.team_prompt.clone(),
                skills: posture.launch.skills.clone(),
                allowed_tools: posture.launch.allowed_tools.clone(),
                provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
                run_id: None,
                worktree_path: None,
                close_pane_on_exit: true,
                exit_on_run_completion: false,
                subagent: false,
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

        let err =
            relaunch_action(&agent, &room("work"), &standalone, Path::new("/repo")).unwrap_err();
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
}
