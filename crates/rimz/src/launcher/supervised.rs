use super::resolve::{check_supervised_subagent_allowed, launch_login, supervised_prompt};
use super::*;
use crate::agents::{AgentDefinition, HookPreflightErr, TurnLifecycleNeed, preflight_hooks};
use crate::harness::run::SupervisedRunRequest;

pub(super) fn prepare_run_layout(
    request: LayoutRequest,
    leader: LeaderRun,
    interaction: &mut dyn Interaction,
) -> Result<Preparation> {
    if request.launch.selection != Selection::Fresh {
        bail!("a run-bearing layout requires a fresh launch");
    }
    let mut supervised = SupervisedRunRequest::new(
        request.launch.spec.clone().unwrap_or_default(),
        request.launch.prompt.clone().unwrap_or_default(),
        None,
        crate::agents::ManagedLaunchState::PendingResolution,
    );
    supervised.cwd.clone_from(&request.cwd);
    supervised
        .worktree
        .clone_from(&request.launch.cohort.worktree);
    supervised.fresh = request.launch.cohort.fresh;
    supervised
        .from_pr
        .clone_from(&request.launch.cohort.from_pr);
    supervised.channel = request
        .launch
        .cohort
        .channel
        .clone()
        .or_else(|| request.context.channel.clone());
    supervised.name.clone_from(&request.launch.name);
    supervised
        .description
        .clone_from(&request.launch.cohort.description);
    supervised.workflow.clone_from(&leader.workflow);
    supervised.throttle_turn.clone_from(&leader.throttle_turn);
    supervised.subagent = leader.subagent;
    supervised.force_new_tab = true;
    supervised.report_to = leader.report_to;
    let context = SupervisedContext {
        root: Some(request.context.workspace.project_root.clone()),
        worktree_fresh_command: None,
        machine: request.context.machine.clone(),
        mux: request.context.mux,
        recovery: crate::room::AttendedRecovery::RequireExplicitReset,
        completion: leader.completion.clone(),
    };
    prepare(&supervised, context, Some((request, leader)), interaction)
}

pub fn prepare_supervised(
    request: &SupervisedRunRequest,
    context: SupervisedContext,
    interaction: &mut dyn Interaction,
) -> Result<Preparation> {
    prepare(request, context, None, interaction)
}

fn prepare(
    request: &SupervisedRunRequest,
    context: SupervisedContext,
    run_layout: Option<(LayoutRequest, LeaderRun)>,
    interaction: &mut dyn Interaction,
) -> Result<Preparation> {
    // Harness-owned launches have no agent caller, from either env or ancestry.
    let harness_owned = request.loop_task.is_some()
        || request.workflow.is_some()
        || run_layout
            .as_ref()
            .is_some_and(|(layout, _)| matches!(layout.starter, Starter::Workflow));
    if !harness_owned {
        checkout::check_launch_room(context.root.as_deref())?;
    }
    let workspace = match &run_layout {
        Some((layout, _)) => layout.context.workspace.clone(),
        None => crate::WorkspaceResolver::resolve_participant(".", context.root.clone())
            .context("resolving current workspace")?,
    };
    let machine_config = context.machine;
    machine_config.agents.startup_relaunch_wait()?;
    let worktree_launch = request.worktree.is_some() || request.from_pr.is_some();
    if worktree_launch {
        machine_config.require_readable_core("create a worktree")?;
    }
    let store = match &run_layout {
        Some((layout, _)) => layout.context.store.clone(),
        None => checkout::open_launch_store(&workspace)?,
    };
    let cwd = checkout::resolve_launch_cwd(request.cwd.as_deref(), store.paths())?;
    // Inside a team's lane, a bare role names that team's role, exactly as it
    // does for an interactive launch: in `#forge`, `reviewer` means
    // `forge.reviewer`.
    let effective = crate::config::effective::load(&machine_config, &workspace.project_root)?;
    let projection = store.runtime_projection(crate::RuntimeScope::Audit)?;
    let caller_identity = if harness_owned {
        None
    } else {
        crate::harness::ancestry::resolve_caller(&projection.agents)
    };
    let caller = caller_identity
        .as_ref()
        .map(|identity| {
            crate::harness::ancestry::resolve_launch_caller(&projection.agents, identity)
        })
        .transpose()?;
    let ancestry = crate::harness::ancestry::resolve_launch_ancestry(
        caller,
        request.subagent,
        machine_config.agents.max_chain_length,
    )?;
    let agent_launched = caller.is_some();
    let reader = caller.and_then(|caller| caller.name.clone());
    // The room pin keeps the store and effective config on the same project root.
    let workspace = checkout::anchor_caller_workspace(workspace, caller, context.root.as_deref())?;
    check_supervised_subagent_allowed(request, caller, &effective.profiles)?;
    checkout::admit_launch_worktree_name(
        &workspace,
        &machine_config.agents.worktree,
        &projection.agents,
        request.worktree.as_deref(),
    )?;
    let lane = request.channel.clone().or_else(|| {
        if harness_owned {
            super::context::directory_channel(
                &workspace,
                cwd.as_deref().unwrap_or(&workspace.worktree_root),
            )
        } else {
            super::context::current_channel(&workspace, Some(&store)).into_name()
        }
    });
    let snapshot = lane
        .is_some()
        .then(|| store.snapshot_cached())
        .transpose()
        .context("reading agent snapshot")?;
    let team_lane = lane
        .as_deref()
        .zip(snapshot.as_ref())
        .and_then(|(channel, snapshot)| {
            crate::address::channel_team(&snapshot.agents, channel).map(|team| (channel, team))
        });
    let (layout, team_name, inferred_lane, leader_index) = if let Some((args, leader)) = &run_layout
    {
        let finalized = resolve_finalized_layout(
            snapshot.as_ref(),
            &machine_config,
            &effective,
            crate::config::effective::ProfileScope::Agents,
            args.launch.spec.as_deref(),
            args.launch.prompt.as_deref(),
            &args.launch.overrides,
            args.launch.cohort.budget,
            args.launch.max_turns,
            lane.as_deref(),
            args.launch.name.is_some(),
            Some((store.runtime_paths(), store.paths())),
        )?;
        for warning in &finalized.warnings {
            interaction.notice(Notice::LaunchWarning(warning))?;
        }
        if finalized.resolved.team_name.is_some() {
            bail!("a run-bearing layout cannot launch a team; use an agent layout instead");
        }
        let mut layout = finalized.resolved.layout;
        let leader_index = crate::harness::spec::prompt_leader(&layout, None)?;
        for (index, cell) in layout
            .columns
            .iter_mut()
            .flat_map(|column| &mut column.rows)
            .filter_map(|row| match row {
                Cell::Agent(cell) => Some(cell),
                _ => None,
            })
            .enumerate()
        {
            cell.launch.workflow.clone_from(&leader.workflow);
            if index == leader_index && cell.launch.mode.is_none() {
                let adapter = crate::agents::find_definition(&cell.kind)
                    .with_context(|| format!("unknown agent kind `{}`", cell.kind))?;
                cell.args
                    .extend(adapter.spec().launch.permission_args(PermissionMode::Auto));
                cell.launch.mode = Some(PermissionMode::Auto);
            }
        }
        (layout, None, finalized.inferred_lane, leader_index)
    } else {
        let mut warnings = Vec::new();
        let planned = crate::harness::plan::plan_supervised_launch(
            request,
            caller,
            team_lane,
            crate::config::Isolation::ambient(&crate::agents::ambient_env()),
            &workspace,
            &machine_config,
            effective,
            |kind| launch_login(request, caller, kind),
            &mut warnings,
        );
        let printed = warnings
            .iter()
            .try_for_each(|warning| interaction.notice(Notice::LaunchWarning(warning)));
        let crate::harness::plan::SupervisedLaunch {
            layout,
            team_name,
            inferred_lane,
            clamped,
            permission_clamp,
            yolo_sandbox,
        } = planned?;
        printed?;
        // The plan guarantees exactly one cell, an agent cell.
        let agent_cell = layout
            .agent_cells()
            .next()
            .expect("a supervised launch plan holds exactly one agent cell");
        let profile = agent_cell
            .launch
            .profile
            .as_deref()
            .unwrap_or(&agent_cell.kind);
        if let Some(source) = &clamped {
            interaction.notice(Notice::IsolationClamp { profile, source })?;
        }
        if let Some(clamp) = &permission_clamp {
            interaction.notice(Notice::PermissionClamp { profile, clamp })?;
        }
        if yolo_sandbox {
            interaction.notice(Notice::YoloSandbox(profile))?;
        }
        (layout, team_name, inferred_lane, 0)
    };
    let agent_cell = layout
        .agent_cells()
        .nth(leader_index)
        .context("run-bearing layout needs an agent leader")?;
    let adapter = crate::agents::find_definition(&agent_cell.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent_cell.kind))?;
    let isolation = crate::config::Isolation::resolve(
        agent_cell.launch.isolation,
        agent_cell.isolation_default,
        machine_config.agents.isolation,
    );
    crate::sandbox::preflight_launch(
        isolation,
        &agent_cell.kind,
        agent_cell.skills.is_some(),
        adapter.manual_skill(),
    )?;
    let prompt = supervised_prompt(request, adapter);
    let mut companion_logins = Vec::new();
    if let Some((args, _)) = &run_layout {
        let mut prospective = cwd
            .as_deref()
            .map(|cwd| workspace.at_launch_dir(cwd))
            .transpose()?
            .unwrap_or_else(|| workspace.clone());
        if worktree_launch {
            prospective.cwd_project_root = Some(workspace.launch_repo_root().to_path_buf());
        }
        let mut checked_folder_trust = HashSet::new();
        let mut noticed_hook_trust = HashSet::new();
        for (index, cell) in layout.agent_cells().enumerate() {
            if index == leader_index {
                continue;
            }
            let (login, _) = check_cell(
                &prospective,
                store.runtime_paths(),
                cell,
                crate::config::Isolation::resolve(
                    cell.launch.isolation,
                    cell.isolation_default,
                    machine_config.agents.isolation,
                ),
                CellStart::Launch(None),
                &mut checked_folder_trust,
                &mut noticed_hook_trust,
                || {
                    crate::config::definitions::hook_trust_fix(
                        &cell.kind,
                        cell.launch.profile.as_deref(),
                        None,
                    )
                },
                match args.starter {
                    Starter::Workflow => Some(trust::Unattended::Workflow),
                    Starter::Loop => Some(trust::Unattended::Loop),
                    Starter::Team => Some(trust::Unattended::Team),
                    Starter::Agent => Some(trust::Unattended::Agent),
                    Starter::Human => agent_launched.then_some(trust::Unattended::Agent),
                },
                None,
                interaction,
            )?;
            companion_logins.push(login);
        }
    }
    if worktree_launch && !checkout::confirm_cross_repo_worktree(&workspace, interaction)? {
        return Ok(Preparation::Handled(LaunchOutcome::Declined));
    }
    if !harness_owned
        && request.from_pr.is_none()
        && let Some(name) = request.worktree.as_deref().filter(|name| !name.is_empty())
    {
        let name = crate::worktree::parse_requested_name(name)?.name;
        worktree_entry::check(
            &workspace,
            &machine_config,
            context.root.as_deref(),
            context.mux,
            &store,
            &name,
            &request.spec,
            team_name.as_deref(),
            None,
            request.fresh,
            context.worktree_fresh_command.as_deref(),
            interaction,
        )?;
    }
    let Some(mut launch) = checkout::resolve_launch_checkout(
        &workspace,
        &machine_config,
        request.worktree.as_deref(),
        request.from_pr.as_ref(),
        cwd.as_deref(),
        if team_name.is_some() {
            crate::worktree::LaunchBase::Team
        } else {
            crate::worktree::LaunchBase::Configured
        },
        interaction,
    )?
    else {
        return Ok(Preparation::Handled(LaunchOutcome::Declined));
    };
    if cwd.is_some() {
        launch.repo_root = workspace.at_launch_dir(&launch.cwd)?.cwd_project_root;
    }
    let mut preflight_launch = agent_cell.launch.clone();
    preflight_launch.channel.clone_from(&request.channel);
    let logins = crate::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &machine_config,
    )?;
    let requested_login = if run_layout.is_some() {
        agent_cell.launch.login.clone().map_or(
            crate::store::writer::LaunchLogin::RoomDefault,
            crate::store::writer::LaunchLogin::Pinned,
        )
    } else {
        launch_login(request, caller, &agent_cell.kind)
    };
    let login = requested_login.resolve(&agent_cell.kind, &logins, &machine_config.accounts)?;
    preflight_launch.login = (!login.is_default()).then(|| login.name().clone());
    login.preflight(&crate::agents::ambient_env())?;
    let mut checkout = None;
    let mut empty_dir_name = None;
    if let Some(prepared) = prepare_empty_dir(
        store.paths(),
        &projection.agents,
        request.name.as_deref(),
        agent_cell,
        &login,
        layout
            .columns
            .iter()
            .map(|column| column.rows.len())
            .sum::<usize>()
            == 1
            && !worktree_launch
            && request.cwd.is_none(),
        interaction,
    )? {
        checkout = Some(launch.cwd.display().to_string());
        launch.cwd = prepared.cwd;
        launch.repo_root = None;
        empty_dir_name = Some(prepared.name);
    }
    preflight_launch.checkout.clone_from(&checkout);
    let launch_invocation = crate::harness::launch::ExecRequest {
        action: crate::harness::launch::ExecAction::Launch {
            prompt: Some(prompt.to_string()),
            extra_args: agent_cell.args.clone(),
        },
        subagent: request.subagent,
        ..crate::harness::launch::ExecRequest::fresh(
            agent_cell,
            crate::harness::launch::ExecIdentity {
                params: preflight_launch,
                ..Default::default()
            },
            None,
            false,
        )
    };
    let (process, managed_launch) = crate::harness::launch::compile_managed_agent_process(
        &workspace.project_root,
        &launch_invocation,
        &launch.cwd,
        &request.managed_launch,
        (!isolation.is_scoped()).then_some(store.runtime_paths()),
    )?;
    // Judge the agent's hooks in the account home it will run under before
    // probing the program or touching the multiplexer.
    let home_fix = if request.workflow.is_some() {
        "start the workflow from a project directory"
    } else if request.loop_task.is_some() {
        "re-add the loop task from a project directory"
    } else if request.subagent {
        "pass `--cwd <DIR>`, or launch the parent agent from a project directory"
    } else {
        "run it from a project directory, or pass `--cwd <DIR>`"
    };
    preflight_agent(adapter, &launch, &login, home_fix)?;
    preflight_program(adapter, &process, &launch.cwd)?;
    if !request.subagent {
        super::lsp_admission::admit(&launch.cwd, &machine_config, interaction)?;
    }
    if let Some(channel) = request.channel.as_deref() {
        crate::channel::admit_launch(&workspace, channel)?;
    }
    // An inferred lane joins the exact channel it was inferred from, rather than
    // one recomputed from the caller's cwd.
    let room_channel = crate::harness::spec::resolve_room_channel(
        &workspace.project_root,
        checkout.as_deref().map(Path::new).unwrap_or(&launch.cwd),
        team_name.as_deref(),
        request
            .channel
            .as_deref()
            .or(inferred_lane.as_deref().filter(|_| cwd.is_none())),
    );
    let mut preflighted_logins = Vec::new();
    let mut managed_launches = Vec::new();
    let mut companion_logins = companion_logins.into_iter();
    for (index, cell) in layout.agent_cells().enumerate() {
        if index == leader_index {
            preflighted_logins.push(login.name().clone());
            managed_launches.push(managed_launch.clone());
            continue;
        }
        // Preflight skips this same leader and returns one login per companion.
        let login = companion_logins
            .next()
            .expect("each companion preflight returned one login");
        let mut params = cell.launch.clone();
        params.login = Some(login.clone());
        let invocation = crate::harness::launch::ExecRequest::fresh(
            cell,
            crate::harness::launch::ExecIdentity {
                params,
                ..Default::default()
            },
            None,
            false,
        );
        let isolation = crate::config::Isolation::resolve(
            cell.launch.isolation,
            cell.isolation_default,
            machine_config.agents.isolation,
        );
        let (_, managed) = crate::harness::launch::compile_managed_agent_process(
            &workspace.project_root,
            &invocation,
            &launch.cwd,
            &crate::agents::ManagedLaunchState::PendingResolution,
            (!isolation.is_scoped()).then_some(store.runtime_paths()),
        )?;
        preflighted_logins.push(login);
        managed_launches.push(managed);
    }
    let entry = run_layout
        .as_ref()
        .map_or(RoomEntry::Birth, |(layout, _)| layout.entry);
    let leader = run_layout
        .map(|(_, leader)| leader)
        .unwrap_or_else(|| LeaderRun {
            lifecycle: crate::store::run::RunLifecycle::Turn,
            keep: request.keep,
            report_to: request.report_to,
            timeout: request.timeout,
            retry_of: None,
            cleanup: request.self_cleanup_on_completion && !request.keep,
            subagent: request.subagent,
            warn: request.warn.clone(),
            grace: request.grace,
            loop_task: request.loop_task.clone(),
            workflow: request.workflow.clone(),
            throttle_turn: None,
            reader,
            response_receipt: agent_launched,
            completion: context.completion,
        });
    let room = if matches!(entry, RoomEntry::Live) {
        Some(Arc::new(RoomContext::live_tab(
            &workspace,
            machine_config.clone(),
            context.mux,
        )?))
    } else {
        None
    };
    Ok(Preparation::Prepared(Box::new(PreparedLaunch {
        state: PreparedState::Ready(Box::new(FreshLaunch {
            context: LaunchContext {
                workspace,
                store,
                machine: machine_config,
                mux: context.mux,
                root: context.root,
                channel: room_channel.clone(),
            },
            layout,
            launch,
            checkout,
            name: request.name.clone(),
            generated_name: empty_dir_name,
            team: None,
            channel: room_channel,
            logins: preflighted_logins,
            ancestry,
            focus: LaunchFocus::Keep,
            prompt: Some(prompt.into_owned()),
            prompt_index: Some(leader_index),
            leader_index: Some(leader_index),
            placement: Placement::NewTab,
            pick_session: false,
            loop_reminder: request.loop_reminder.clone(),
            description: request.description.clone(),
            report_to: request.report_to,
            managed_launches,
            loop_task: None,
            cleanup_worktree: request.retries == 0,
            inherit_cwd: request.worktree.is_none()
                && request.from_pr.is_none()
                && request.loop_task.is_none(),
            force_new_tab: request.force_new_tab,
            agent_shell_persistence: request.agent_shell_persistence,
            room,
            recovery: context.recovery,
            entry,
            soft_name: false,
            throttle_turn: request.throttle_turn.clone(),
        })),
        leader_run: Some(leader),
    })))
}

fn preflight_agent(
    adapter: &AgentDefinition,
    launch: &crate::worktree::LaunchCheckout,
    login: &crate::agents::ProviderLogin,
    home_fix: &str,
) -> Result<()> {
    let definition = adapter.spec();
    let kind = definition.kind;
    let login_env = login.env(&crate::agents::ambient_env());
    match preflight_hooks(adapter, &login_env, TurnLifecycleNeed::Wired) {
        Ok(()) => {}
        Err(HookPreflightErr::TurnLifecycleUnsupported { reason }) => bail!(
            "`rimz agents -p` cannot supervise {kind}: a verified executable turn-lifecycle signal is required; {}",
            reason
        ),
        Err(HookPreflightErr::HooksMissing) => bail!(
            "`rimz agents -p` requires {kind} hooks so the supervised turn can report completion; run `rimz hooks install {kind}`"
        ),
        Err(HookPreflightErr::HooksUntrusted { hooks, fix }) => bail!(
            "{kind} hooks are installed but not trusted ({}); {}",
            hooks,
            fix
        ),
        Err(HookPreflightErr::HooksDisabled { reason, fix }) => {
            bail!("`rimz agents -p` cannot supervise {kind}: {reason}; {fix}")
        }
    }
    match crate::agents::preflight_launch_dir(
        adapter,
        &launch.cwd,
        launch.repo_root.as_deref(),
        &login_env,
        crate::agents::LaunchDirMode::Supervised,
    ) {
        Ok(()) => Ok(()),
        Err(error @ crate::agents::LaunchDirRefusal::Home { .. }) => bail!("{error}; {home_fix}"),
        Err(error) => bail!("`rimz agents -p` supervised launch refused: {error}"),
    }
}

fn preflight_program(
    adapter: &AgentDefinition,
    process: &crate::harness::launch::CompiledAgentProcess,
    cwd: &Path,
) -> Result<()> {
    let mut launch_env = crate::agents::ambient_env();
    launch_env.extend(process.env.clone());
    if let Some(error) = adapter.launch_blocker(cwd, &launch_env, &process.provider_argv) {
        return Err(error.into());
    }
    let program = &process.provider_program;
    let path = process
        .resolve_program_after_shell_rc()
        .with_context(|| format!("checking `{program}` after shell startup"))?;
    let Some(path) = path else {
        bail!("finding `{program}` on PATH after shell startup");
    };
    crate::agents::version::check_launch_version_floor(adapter, &path)?;
    Ok(())
}

#[cfg(test)]
mod tests;
