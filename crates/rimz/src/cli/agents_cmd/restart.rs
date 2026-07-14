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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let store = crate::cli::open_store(&workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let snapshot = crate::cli::alive_snapshot(&store, &runtime, &workspace.session_name)?;
    let agent = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    )?
    .clone();
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
    let adapter = rimz::agents::find_adapter(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let machine_config = crate::cli::machine_config();
    let mut cell = restart_cell(&agent, &workspace, &machine_config, adapter)?;
    let Cell::Agent {
        args: extra_args,
        mode,
        budget,
        ..
    } = &mut cell
    else {
        bail!("restart profile did not resolve to an agent");
    };
    let extra_args = std::mem::take(extra_args);
    let mode = *mode;
    let budget = budget.clone();

    // Fail at the entry point if this project's configured launch environment
    // is not trusted, before the old pane is touched.
    super::launch::agent_launch_env(&workspace.project_root, agent.kind.as_str())?;

    let resume_support = !agent.agent_id.is_provisional()
        && agent.worktree_path.is_some()
        && adapter
            .resume_command(agent.agent_id.as_str(), &cwd)
            .is_some();
    let session_present = rimz::harness::resume::resume_session_present(&agent);
    let fresh_reason = fresh_reason(resume_support, session_present);
    let fresh_identity = if fresh_reason.is_some() {
        Some(append_fresh_launch(
            &store, &workspace, &agent, &cwd, cell, mode,
        )?)
    } else {
        None
    };
    let identity_name = fresh_identity
        .as_ref()
        .map_or(agent.name.as_deref(), |identity| {
            Some(identity.name.as_str())
        });
    let invocation = rimz::harness::launch::ExecInvocation {
        kind: agent.kind.as_str(),
        action: match fresh_identity.as_ref() {
            Some(_) => rimz::harness::launch::ExecAction::Launch {
                prompt: None,
                extra_args: &extra_args,
            },
            None => rimz::harness::launch::ExecAction::Resume {
                session_id: agent.agent_id.as_str(),
                extra_args: &extra_args,
            },
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity {
            name: identity_name,
            name_explicit: fresh_identity
                .as_ref()
                .map_or(agent.name_explicit, |identity| identity.name_explicit),
            launch_id: fresh_identity
                .as_ref()
                .map(|identity| identity.agent_id.as_str()),
            profile: agent.profile.as_deref(),
            mode,
            role: agent.role.as_deref(),
            team: agent.team.as_deref(),
            launch_group: agent.launch_group.as_deref(),
            launch_ordinal: agent.launch_ordinal,
            channel: agent.channel.as_deref(),
            // One-off model and effort flags were not durable launch identity.
            model: None,
            effort: None,
            budget: budget.as_deref(),
        },
    };
    let argv = rimz::harness::launch::exec_argv(&rimz::proc::rimz_exe(), &invocation);
    let mut env =
        crate::cli::agents_launch::launch_identity_env(&workspace, agent.channel.as_deref(), false);
    env.insert(
        rimz::harness::run::ENV_WORKTREE_PATH.to_owned(),
        cwd.display().to_string(),
    );
    let backend = rimz::mux::backend_for(old_pane.mux());
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();

    if let Err(err) = backend
        .focus_pane(&old_pane, Some(&workspace.session_name))
        .context("focusing the agent pane for restart")
    {
        mark_fresh_failed(&store, &workspace, fresh_identity.as_ref(), &cwd);
        return Err(err);
    }
    if let Err(err) = backend
        .split_pane(SplitPaneOptions {
            session_name: None,
            target_view_id: None,
            target_pane_id: Some(old_pane.clone()),
            cwd: Some(cwd.display().to_string()),
            command: Some(argv),
            title: None,
            env,
            stacked: false,
            direction,
            focus: true,
        })
        .context("opening the replacement agent pane")
    {
        mark_fresh_failed(&store, &workspace, fresh_identity.as_ref(), &cwd);
        return Err(err);
    }
    backend
        .close_pane(&workspace.session_name, &old_pane)
        .context("closing the replaced agent pane")?;

    let mut out = crate::cli::render::out();
    if let (Some(identity), Some(reason)) = (fresh_identity.as_ref(), fresh_reason) {
        writeln!(
            out,
            "restarted fresh as @{} — {}",
            identity.name,
            reason.as_str()
        )?;
    } else {
        let peers = snapshot
            .agents
            .iter()
            .filter(|candidate| candidate.parent_agent_id.is_none())
            .collect::<Vec<_>>();
        let handle = rimz::harness::target::agent_handle(&agent, &peers, true);
        writeln!(
            out,
            "restarted {handle} (resumed session {})",
            agent.agent_id
        )?;
    }
    Ok(())
}

fn restart_cell(
    agent: &AgentState,
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    adapter: &dyn AgentAdapter,
) -> Result<Cell> {
    let launch = super::launch::effective_launch_agents(machine_config, workspace)?;
    let configured_profile = agent
        .profile
        .as_deref()
        .filter(|profile| launch.profiles.0.contains_key(*profile));
    let mut cell = match configured_profile {
        Some(profile) => {
            let layout =
                super::launch::resolve_launch_layout(Some(profile), &launch, machine_config)?;
            super::launch::ensure_profile_prompt_files(&layout)?;
            let mut cells = layout.agent_cells();
            let cell = cells
                .next()
                .cloned()
                .context("restart profile produced no agent cell")?;
            if cells.next().is_some() {
                bail!("restart profile `{profile}` produced more than one agent cell");
            }
            cell
        }
        None => {
            if let Some(profile) = agent.profile.as_deref() {
                writeln!(
                    crate::cli::render::err(),
                    "rimz: profile `{profile}` is no longer configured; restarting as bare {}",
                    agent.kind
                )?;
            }
            Cell::agent(agent.kind.clone())
        }
    };
    let Cell::Agent {
        kind,
        args,
        mode,
        profile,
        role,
        budget,
        ..
    } = &mut cell
    else {
        bail!("restart profile did not resolve to an agent");
    };
    if kind != &agent.kind {
        bail!(
            "profile `{}` now resolves to {}, but the running agent is {}; launch it fresh to change providers",
            agent.profile.as_deref().unwrap_or("<unknown>"),
            kind,
            agent.kind
        );
    }
    let permission_args = agent
        .mode
        .map(|mode| adapter.permission_args(mode))
        .unwrap_or_default();
    let (replayed_args, replayed_mode) =
        replay_posture(std::mem::take(args), *mode, agent.mode, &permission_args);
    *args = replayed_args;
    *mode = replayed_mode;
    *profile = agent.profile.clone();
    *role = agent.role.clone();
    *budget = agent.budget.clone();
    Ok(cell)
}

fn replay_posture(
    mut profile_args: Vec<String>,
    profile_mode: Option<PermissionMode>,
    stamped_mode: Option<PermissionMode>,
    stamped_permission_args: &[String],
) -> (Vec<String>, Option<PermissionMode>) {
    if profile_mode.is_none() && stamped_mode.is_some() {
        profile_args.extend(stamped_permission_args.iter().cloned());
    }
    (profile_args, profile_mode.or(stamped_mode))
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
) -> Result<LaunchIdentity> {
    let layout = LayoutSpec::single(cell);
    let mut requests = rimz::harness::plan::launch_identity_requests(
        &layout,
        None,
        None,
        agent.team.as_deref(),
        None,
        agent.channel.as_deref(),
        None,
    )?;
    let request = requests
        .first_mut()
        .context("restart produced no fresh launch request")?;
    request.name = agent.name.as_ref().map_or(AgentLaunchName::Mint, |name| {
        AgentLaunchName::Soft(name.clone())
    });
    request.launch.profile = agent.profile.clone();
    request.launch.mode = mode;
    request.launch.role = agent.role.clone();
    request.launch.team = agent.team.clone();
    request.launch.launch_group = agent.launch_group.clone();
    request.launch.launch_ordinal = agent.launch_ordinal;
    request.launch.channel = agent.channel.clone();
    let mut identities = store.append_agent_launches_allocating(
        &requests,
        &AgentLaunchAppend {
            workspace_id: workspace.workspace_id.clone(),
            session_name: workspace.session_name.clone(),
            cwd: cwd.to_path_buf(),
            worktree_name: agent.worktree_branch.clone(),
            channel: agent.channel.clone(),
            description: None,
            state: rimz::store::event::AgentLaunchState::Starting,
            pane_id: None,
        },
    )?;
    identities
        .pop()
        .context("restart fresh launch allocated no identity")
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
    let _ = super::launch::append_launch_event(
        store,
        workspace,
        identity,
        LaunchEventParams {
            cwd,
            worktree_name: None,
            channel: identity.launch.channel.as_deref(),
            state: rimz::store::event::AgentLaunchState::Failed,
            pane_id: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn profile_mode_wins_over_stamped_mode() {
        let (replayed, mode) = replay_posture(
            args(&["--profile-auto"]),
            Some(PermissionMode::Auto),
            Some(PermissionMode::Yolo),
            &args(&["--stamped-yolo"]),
        );

        assert_eq!(replayed, args(&["--profile-auto"]));
        assert_eq!(mode, Some(PermissionMode::Auto));
    }

    #[test]
    fn stamped_mode_fills_profile_without_posture() {
        let (replayed, mode) = replay_posture(
            args(&["--model", "opus"]),
            None,
            Some(PermissionMode::Yolo),
            &args(&["--dangerously-skip-permissions"]),
        );

        assert_eq!(
            replayed,
            args(&["--model", "opus", "--dangerously-skip-permissions"])
        );
        assert_eq!(mode, Some(PermissionMode::Yolo));
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
}
