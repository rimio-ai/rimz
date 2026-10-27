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
    let caller = resolve_caller(&snapshot.agents);
    let focus = LaunchFocus::resolve(false, caller.as_ref());
    let message = restart_resolved(&ctx, &agent, &peers, focus, None, || {
        caller
            .as_ref()
            .map(|_| rimz::launcher::trust::Unattended::Agent)
    })?;
    writeln!(crate::cli::render::out(), "{message}")?;
    Ok(())
}

pub(in crate::cli) fn restart_resolved(
    ctx: &Ctx,
    agent: &AgentState,
    peers: &[&AgentState],
    focus: LaunchFocus,
    unattended: Option<rimz::launcher::trust::Unattended>,
    hook_unattended: impl FnOnce() -> Option<rimz::launcher::trust::Unattended>,
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
    if agent.checkout.is_some() {
        rimz::agents::empty_dir::prepare_reentry(store.paths(), &cwd)?;
    }
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
    let mut cell = restart_cell(agent, &posture);
    if let Cell::Agent(cell) = &mut cell {
        let profile = agent.profile.as_deref().unwrap_or(agent.kind.as_str());
        for warning in rimz::harness::plan::LaunchFinalizeWarning::take_cell_notices(cell, profile)
        {
            writeln!(crate::cli::render::err(), "{warning}")?;
        }
    }

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
    let provider_login =
        rimz::agents::session_login(&agent.kind, login.as_ref(), &machine_config.accounts)?;
    crate::cli::launch_interaction::preflight_hooks(
        provider_login.health(&rimz::agents::ambient_env()),
        hook_unattended,
        || {
            rimz::config::definitions::hook_trust_fix(
                &agent.kind,
                agent.profile.as_deref(),
                Some((agent, "restart")),
            )
        },
        &mut Default::default(),
        &mut crate::cli::render::err(),
    )?;
    let repo_root = if agent.checkout.is_none() {
        workspace.at_launch_dir(&cwd)?.cwd_project_root
    } else {
        None
    };
    crate::cli::launch_interaction::preflight(
        agent.kind.as_str(),
        &cwd,
        adapter.folder_trust(
            &cwd,
            repo_root.as_deref(),
            &provider_login.env(&rimz::agents::ambient_env()),
        ),
        unattended,
    )?;
    if !isolation.is_scoped() {
        rimz::harness::launch::preflight_agent_process(
            &workspace.project_root,
            &relaunch_request(agent, &posture, action.clone(), login.clone(), None),
            &cwd,
            Some(store.runtime_paths()),
        )?;
    }
    rimz::harness::hibernate::wake_pane(store, &old_pane)?;
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
    let env = rimz::room::pane_identity_env(
        workspace,
        agent.lane_path().map(Path::new).unwrap_or(&cwd),
        agent.channel.as_deref(),
        false,
    );
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
            placement: rimz::mux::SplitPlacement::Rejoin(direction),
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
    rimz::harness::hibernate::wake_pane(store, &old_pane)?;
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
        checkout: agent.checkout.clone(),
        molted_from: agent.molted_from.clone(),
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
        workflow: agent.workflow.clone(),
        launch_group: agent.launch_group.clone(),
        launch_ordinal: agent.launch_ordinal,
        channel: agent.channel.clone(),
        mode: posture.launch.mode,
        isolation: agent.isolation,
        model: posture.launch.model.clone(),
        effort: posture.launch.effort.clone(),
        fast: posture.launch.fast,
        budget: posture.launch.budget.clone(),
        kind_ordinal: None,
    };
    ExecRequest {
        close_pane_on_exit: true,
        subagent: agent.is_launched_child(),
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
/// Restart is interactive, so a profile that now names a different provider, a
/// prompt replacement the provider cannot take, and a team prompt layer the seat
/// cannot receive refuse here rather than degrading. Every other degrade prints
/// and continues.
pub(super) fn restart_posture(
    agent: &AgentState,
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
) -> Result<ResumePosture> {
    let launch = rimz::config::effective::load(machine_config, &workspace.project_root)?;
    launch.block_set_failure()?;
    let profiles = launch.profiles_for_launch(agent.is_launched_child(), agent.profile.as_deref());
    if agent
        .profile
        .as_deref()
        .is_some_and(|name| !profiles.0.contains_key(name))
    {
        launch.block_failed_reference(agent.profile.as_deref(), None)?;
    }
    let posture = member_posture(agent, profiles, &launch.teams);
    match &posture.degraded {
        Some(reason @ PostureDegrade::KindChanged { .. }) => {
            bail!(
                "{reason}; launch it fresh to change providers (rimz agents <profile> --agent <kind>)"
            )
        }
        Some(
            reason @ (PostureDegrade::PromptUnsupported { .. }
            | PostureDegrade::TeamPromptRejected { .. }),
        ) => bail!("{reason}"),
        Some(reason) => writeln!(
            crate::cli::render::err(),
            "rimz: {reason}; launching as bare {}",
            agent.kind
        )?,
        None => {}
    }
    Ok(posture)
}

pub(super) fn member_posture(
    agent: &AgentState,
    profiles: &rimz::config::ProfilesConfig,
    teams: &rimz::config::TeamsConfig,
) -> ResumePosture {
    rimz::harness::resume::resolve_member_posture(
        rimz::harness::resume::PostureRequest {
            record: agent.record.as_deref(),
            profile: agent.profile.as_deref(),
            kind: &agent.kind,
            stamped_mode: agent.mode,
            stamped_tier: agent.tier.as_deref(),
        },
        profiles,
        teams,
        agent.team.as_deref(),
        agent.role.as_deref(),
    )
}

/// The layout cell a fresh restart launches, carrying the replayed posture and
/// the agent's durable identity.
pub(super) fn restart_cell(agent: &AgentState, posture: &ResumePosture) -> Cell {
    Cell::Agent(AgentCell {
        notices: posture.launch.notices.clone(),
        empty_dir: None,
        resume_model_override: false,
        kind: agent.kind.clone(),
        args: posture.launch.args.clone(),
        auto_compact: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: posture.launch.skills.clone(),
        no_skill_tool: posture.launch.no_skill_tool,
        allowed_tools: posture.launch.allowed_tools.clone(),
        isolation_default: posture.launch.isolation_default,
        launch: rimz::agents::LaunchParams {
            checkout: agent.checkout.clone(),
            profile: agent.profile.clone(),
            tier: posture.launch.tier.clone(),
            record: Some(rimz::agents::LaunchRecord::replay_or_new(
                posture.launch.record.as_deref(),
                posture.launch.model.as_deref(),
                posture.launch.effort.as_deref(),
                posture.launch.fast,
            )),
            role: agent.role.clone(),
            mode: posture.launch.mode,
            isolation: agent.isolation,
            model: posture.launch.model.clone(),
            effort: posture.launch.effort.clone(),
            fast: posture.launch.fast,
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
    let request = fresh_launch_request(agent, cell, mode)?;
    let batch = store
        .begin_agent_launch_replacement(
            agent,
            &request,
            AgentLaunchScope {
                session_name: workspace.session_name.clone(),
                cwd: cwd.to_path_buf(),
                branch: None,
                description: None,
            },
        )
        .map_err(|err| match err {
            rimz::store::StoreErr::MoltSuccessor { successor } => anyhow::anyhow!(
                "restart refused: this agent molted into @{successor}, which holds its place now; \
             message or restart @{successor} instead; do not retry this command"
            ),
            err => err.into(),
        })?;
    batch.single_identity()?;
    Ok(batch)
}

pub(super) fn fresh_launch_request(
    agent: &AgentState,
    cell: Cell,
    mode: Option<PermissionMode>,
) -> Result<AgentLaunchRequest> {
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
    request.launch.checkout = agent.checkout.clone();
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
    request.launch.molted_from = agent.molted_from.clone();
    request.launch.channel = agent.channel.clone();
    Ok(request.clone())
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
mod tests;
