use super::*;

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
    let agent = crate::cli::resolve_agent_one(&snapshot, &reference, None, ctx.channel())?.clone();
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    let message = restart_resolved(&ctx, &agent, &peers)?;
    writeln!(crate::cli::render::out(), "{message}")?;
    Ok(())
}

pub(in crate::cli) fn restart_resolved(
    ctx: &Ctx,
    agent: &AgentState,
    peers: &[&AgentState],
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
    let cell = restart_cell(agent, &posture);
    let extra_args = posture.args.clone();

    // Fail at the entry point if this project's configured launch environment
    // is not trusted, before the old pane is touched.
    rimz::harness::launch::preflight_agent_kind(
        &workspace.project_root,
        machine_config.harness.rtk,
        agent.kind.as_str(),
        &cwd,
    )?;

    let adapter = rimz::agents::find_definition(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let resume_support = !agent.agent_id.is_provisional()
        && agent.worktree_path.is_some()
        && rimz::harness::launch::compile_provider_argv(
            adapter,
            agent.kind.as_str(),
            &rimz::harness::launch::ExecAction::Resume {
                session_id: agent.agent_id.to_string(),
                extra_args: Vec::new(),
            },
            &cwd,
        )
        .is_ok();
    let session_present = rimz::harness::resume::resume_session_present(agent);
    let fresh_reason = fresh_reason(resume_support, session_present);
    let fresh_batch = if fresh_reason.is_some() {
        Some(append_fresh_launch(
            store,
            workspace,
            agent,
            &cwd,
            cell,
            posture.mode,
        )?)
    } else {
        None
    };
    let fresh_identity = fresh_batch
        .as_ref()
        .map(AgentLaunchBatch::single_identity)
        .transpose()?;
    let identity_name = fresh_identity.map_or(agent.name.as_deref(), |identity| {
        Some(identity.name.as_str())
    });
    let restart_params = rimz::agents::LaunchParams {
        parent_agent_id: agent.parent_agent_id.clone(),
        parent_agent_kind: agent.parent_agent_kind.clone(),
        launch_depth: agent.launch_depth,
        profile: agent.profile.clone(),
        role: agent.role.clone(),
        team: agent.team.clone(),
        launch_group: agent.launch_group.clone(),
        launch_ordinal: agent.launch_ordinal,
        channel: agent.channel.clone(),
        mode: posture.mode,
        model: posture.model.clone(),
        effort: posture.effort.clone(),
        budget: posture.budget.clone(),
        kind_ordinal: None,
    };
    let invocation = rimz::harness::launch::ExecRequest {
        kind: agent.kind.clone(),
        action: match fresh_identity {
            Some(_) => rimz::harness::launch::ExecAction::Launch {
                prompt: None,
                extra_args,
            },
            None => rimz::harness::launch::ExecAction::Resume {
                session_id: agent.agent_id.to_string(),
                extra_args,
            },
        },
        system_prompt_file: posture.system_prompt_file.clone(),
        append_system_prompt_files: posture.append_system_prompt_files.clone(),
        provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        subagent: false,
        identity: rimz::harness::launch::ExecIdentity {
            name: identity_name.map(ToOwned::to_owned),
            name_explicit: fresh_identity
                .map_or(agent.name_explicit, |identity| identity.name_explicit),
            launch_id: fresh_identity
                .map(|identity| identity.agent_id.to_string())
                .or_else(|| agent.launch_id.as_ref().map(ToString::to_string)),
            params: restart_params,
        },
    };
    let argv = rimz::harness::launch::exec_argv(&rimz::proc::rimz_exe(), &invocation)?;
    let mut env = rimz::room::pane_identity_env(workspace, agent.channel.as_deref(), false);
    env.insert(
        rimz::harness::run::ENV_WORKTREE_PATH.to_owned(),
        cwd.display().to_string(),
    );
    let backend = rimz::mux::backend_for(old_pane.mux());
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();

    if let Err(err) = rimz::sidebar::focus_anchor::execute_action(
        backend.as_ref(),
        ctx.runtime(),
        &workspace.session_name,
        old_pane.clone(),
        rimz::sidebar::focus_anchor::FocusOrigin::User,
        None,
    )
    .context("focusing the agent pane for restart")
    {
        mark_fresh_failed(store, workspace, fresh_identity, &cwd);
        return Err(err);
    }
    if let Err(err) = backend
        .split_pane(SplitPaneOptions {
            target: rimz::mux::SplitTarget::Pane(old_pane.clone()),
            cwd: Some(cwd.display().to_string()),
            command: Some(argv),
            title: None,
            close_on_exit: false,
            env,
            placement: rimz::mux::SplitPlacement::Directional(direction),
            focus: true,
        })
        .context("opening the replacement agent pane")
    {
        mark_fresh_failed(store, workspace, fresh_identity, &cwd);
        return Err(err);
    }
    backend
        .close_pane(&workspace.session_name, &old_pane)
        .context("closing the replaced agent pane")?;

    if let (Some(identity), Some(reason)) = (fresh_identity, fresh_reason) {
        Ok(format!(
            "restarted fresh as @{} — {}",
            identity.name,
            reason.as_str()
        ))
    } else {
        let handle = rimz::harness::target::agent_handle(agent, peers, true);
        Ok(format!(
            "restarted {handle} (resumed session {})",
            agent.agent_id
        ))
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
    let launch = rimz::config::effective::load(
        &machine_config.agents,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    let posture = rimz::harness::resume::resolve_posture(
        rimz::harness::resume::PostureRequest {
            profile: agent.profile.as_deref(),
            kind: &agent.kind,
            stamped_mode: agent.mode,
        },
        &launch.profiles,
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
        kind: agent.kind.clone(),
        args: posture.args.clone(),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        launch: rimz::agents::LaunchParams {
            profile: agent.profile.clone(),
            role: agent.role.clone(),
            mode: posture.mode,
            model: posture.model.clone(),
            effort: posture.effort.clone(),
            budget: posture.budget.clone(),
            ..Default::default()
        },
    })
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
    request.launch.mode = mode;
    request.launch.role = agent.role.clone();
    request.launch.team = agent.team.clone();
    request.launch.launch_group = agent.launch_group.clone();
    request.launch.launch_ordinal = agent.launch_ordinal;
    request.launch.channel = agent.channel.clone();
    let batch = store.begin_agent_launch_batch(
        &requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.to_path_buf(),
            worktree_name: agent.worktree_branch.clone(),
            channel: agent.channel.clone(),
            description: None,
        },
    )?;
    batch.single_identity()?;
    Ok(batch)
}

fn mark_fresh_failed(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    identity: Option<&LaunchIdentity>,
    cwd: &Path,
) {
    let Some(identity) = identity else {
        return;
    };
    let _ = store.fail_agent_launch(identity, &workspace.session_name, cwd);
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
