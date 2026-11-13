use super::resolve::{check_supervised_subagent_allowed, launch_login, supervised_prompt};
use super::*;
use crate::agents::{AgentDefinition, HookPreflightErr, TurnLifecycleNeed, preflight_hooks};
use crate::harness::run::SupervisedRunRequest;
use std::time::Duration;

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
    supervised
        .channel
        .clone_from(&request.launch.cohort.channel);
    supervised.name.clone_from(&request.launch.name);
    supervised
        .description
        .clone_from(&request.launch.cohort.description);
    supervised.workflow.clone_from(&leader.workflow);
    supervised.loop_task.clone_from(&leader.loop_task);
    supervised.loop_booked_by.clone_from(&leader.loop_booked_by);
    supervised
        .reminder_section
        .clone_from(&leader.reminder_section);
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
    let mut add_dirs = Vec::new();
    for dir in &request.add_dirs {
        // A supplied path always resolves to Some.
        let dir = checkout::resolve_launch_cwd(Some(dir), store.paths())
            .context("resolving --add-dir")?
            .expect("an explicit directory resolves to a path")
            .display()
            .to_string();
        if !add_dirs.contains(&dir) {
            add_dirs.push(dir);
        }
    }
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
    let caller_lane = super::context::caller_lane(&workspace.project_root, caller, harness_owned);
    check_supervised_subagent_allowed(request, caller, &effective.profiles)?;
    checkout::admit_launch_worktree_name(
        &workspace,
        &machine_config.agents.worktree,
        &projection.agents,
        request.worktree.as_deref(),
    )?;
    let lane = request
        .channel
        .as_deref()
        .map(LaneName::dashed)
        .or_else(|| {
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
        .as_ref()
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
            lane.as_ref(),
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
                cell.launch.mode = adapter.spec().launch.settle_permission_mode(
                    &mut cell.args,
                    None,
                    Some(PermissionMode::Auto),
                );
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
    let mut layout = layout;
    let automation = run_layout
        .as_ref()
        .and_then(|(layout, _)| layout.automation.clone())
        .or_else(|| {
            request
                .loop_task
                .is_some()
                .then_some(crate::agents::Automation::Loop)
        })
        .or_else(|| caller.and_then(|caller| caller.automation.clone()));
    for row in layout
        .columns
        .iter_mut()
        .flat_map(|column| &mut column.rows)
    {
        if let Cell::Agent(cell) = row {
            cell.launch.automation.clone_from(&automation);
            cell.launch.add_dirs.clone_from(&add_dirs);
        }
    }
    let agent_cell = layout
        .agent_cells()
        .nth(leader_index)
        .context("run-bearing layout needs an agent leader")?;
    let (timeout, warn, grace) = if run_layout.is_none() && request.subagent {
        let ladder = resolve_deadline_ladder(
            request,
            agent_cell.timeout,
            &machine_config.agents.subagents,
            jiff::Timestamp::now(),
        )?;
        (Some(ladder.timeout), ladder.warn, Some(ladder.grace))
    } else {
        (
            request.timeout,
            request.warn.clone().unwrap_or_default(),
            request.grace,
        )
    };
    let adapter = crate::agents::find_definition(&agent_cell.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent_cell.kind))?;
    if !add_dirs.is_empty() && !adapter.supports_added_dirs() {
        let warning = crate::harness::launch_plan::LaunchPlanWarning::AddedDirsUnsupported {
            kind: agent_cell.kind.clone(),
        };
        interaction.notice(Notice::Warning(&warning.to_string()))?;
    }
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
    let logins = crate::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &machine_config,
    )?;
    let resume = run_layout
        .as_ref()
        .and_then(|(_, leader)| leader.resume.as_ref());
    let requested_login = if resume.is_some() {
        crate::store::writer::LaunchLogin::RoomDefault
    } else if run_layout.is_some() {
        agent_cell.launch.login.clone().map_or(
            crate::store::writer::LaunchLogin::RoomDefault,
            crate::store::writer::LaunchLogin::Pinned,
        )
    } else {
        launch_login(request, caller, &agent_cell.kind)
    };
    let login = requested_login.resolve(&agent_cell.kind, &logins, &machine_config.accounts)?;
    login.preflight(&crate::agents::ambient_env())?;
    if let Some(session) = resume {
        let previous =
            crate::store::snapshot::find_agent(&projection.agents, &agent_cell.kind, session)
                .with_context(|| {
                    format!(
                        "no recorded {} session {session}; restart fresh instead",
                        agent_cell.kind
                    )
                })?;
        crate::harness::resume::relaunch_login(
            previous,
            &logins,
            &crate::agents::LoginCatalog::from_config(&machine_config.accounts)?,
        )?;
        if !crate::harness::resume::resume_session_present(previous) {
            bail!("session {session} is no longer available; restart fresh instead");
        }
    }
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
        if team_name.is_some() || request.from_trunk {
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
    let home_fix = if request.workflow.is_some() {
        "start the workflow from a project directory"
    } else if request.loop_task.is_some() {
        "re-add the loop task from a project directory"
    } else if request.subagent {
        "pass `--cwd <DIR>`, or launch the parent agent from a project directory"
    } else {
        "run it from a project directory, or pass `--cwd <DIR>`"
    };
    if request.empty_dir {
        crate::agents::preflight_launch_dir(
            adapter,
            &launch.cwd,
            launch.repo_root.as_deref(),
            &login.env(&crate::agents::ambient_env()),
            crate::agents::LaunchDirMode::Displaced,
        )?;
    }
    let fallback =
        if request.subagent && cwd.is_some() && !request.empty_dir && adapter.supports_added_dirs()
        {
            match crate::agents::preflight_launch_dir(
                adapter,
                &launch.cwd,
                launch.repo_root.as_deref(),
                &login.env(&crate::agents::ambient_env()),
                crate::agents::LaunchDirMode::Supervised,
            ) {
                Err(refusal @ crate::agents::LaunchDirRefusal::Undecided { .. }) => Some(refusal),
                Err(error @ crate::agents::LaunchDirRefusal::Home { .. }) => {
                    bail!("{error}; {home_fix}")
                }
                Err(error) => bail!("`rimz agents -p` supervised launch refused: {error}"),
                Ok(()) => None,
            }
        } else {
            None
        };
    let mut preflight_launch = agent_cell.launch.clone();
    if fallback.is_some() {
        let target = launch.cwd.display().to_string();
        if !preflight_launch.add_dirs.contains(&target) {
            preflight_launch.add_dirs.push(target);
        }
        preflight_launch.untrusted_checkout = true;
    }
    preflight_launch.login = (!login.is_default()).then(|| login.name().clone());
    let mut checkout = None;
    let mut empty_dir_name = None;
    let prepared = prepare_empty_dir(
        store.paths(),
        &projection.agents,
        request.name.as_deref(),
        agent_cell,
        &login,
        if request.empty_dir || fallback.is_some() {
            EmptyDirRequest::Required
        } else if layout
            .columns
            .iter()
            .map(|column| column.rows.len())
            .sum::<usize>()
            == 1
            && !worktree_launch
            && request.cwd.is_none()
        {
            EmptyDirRequest::Profile
        } else {
            EmptyDirRequest::Disabled
        },
        interaction,
    );
    let prepared = match fallback.as_ref() {
        Some(refusal) => {
            prepared.with_context(|| format!("supervised launch refused: {refusal}"))?
        }
        None => prepared?,
    };
    if let Some(prepared) = prepared {
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
    preflight_agent(adapter, &launch, &login, home_fix)?;
    preflight_program(adapter, &process, &launch.cwd)?;
    if !request.subagent {
        super::lsp_admission::admit(&launch.cwd, &machine_config, interaction)?;
    }
    let room_lane = crate::harness::lanes::decide(&crate::harness::lanes::LaneRequest {
        project_root: &workspace.project_root,
        target: checkout.as_deref().map(Path::new).unwrap_or(&launch.cwd),
        typed: request.channel.as_deref(),
        team: team_name.as_deref(),
        joins_cohort: inferred_lane.is_some(),
        caller: caller_lane,
        agents: &projection.agents,
    })?;
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
    let mut fallback_assist = None;
    if fallback.is_some() {
        // Required placement can return neither Skip nor a successful Fallback.
        let target = Path::new(
            checkout
                .as_deref()
                .expect("a fallback requires empty placement"),
        );
        interaction.notice(Notice::UntrustedCwdFallback {
            provider: adapter.spec().display_name,
            profile: &request.spec,
            agent: request.agent.as_deref(),
            target,
            cwd: &launch.cwd,
        })?;
        fallback_assist = Some(crate::harness::assist_log::Assist::UntrustedCwdFallback {
            kind: agent_cell.kind.clone(),
            login: login.key(),
            target: target.to_path_buf(),
            cwd: launch.cwd.clone(),
            profile: Some(request.spec.clone()),
        });
        for row in layout
            .columns
            .iter_mut()
            .flat_map(|column| &mut column.rows)
        {
            if let Cell::Agent(cell) = row {
                cell.launch
                    .add_dirs
                    .clone_from(&launch_invocation.identity.params.add_dirs);
                cell.launch.untrusted_checkout = true;
            }
        }
    }
    let entry = run_layout
        .as_ref()
        .map_or(RoomEntry::Birth, |(layout, _)| layout.entry);
    let leader = run_layout
        .map(|(_, leader)| leader)
        .unwrap_or_else(|| LeaderRun {
            resume: None,
            lifecycle: crate::store::run::RunLifecycle::Turn,
            keep: request.keep,
            report_to: request.report_to,
            timeout,
            retry_of: None,
            cleanup: request.self_cleanup_on_completion && !request.keep,
            subagent: request.subagent,
            warn,
            grace,
            loop_task: request.loop_task.clone(),
            loop_booked_by: request.loop_booked_by.clone(),
            workflow: request.workflow.clone(),
            reminder_section: request.reminder_section.clone(),
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
                channel: room_lane.pin_name(),
            },
            layout,
            launch,
            checkout,
            name: request.name.clone(),
            generated_name: empty_dir_name,
            team: None,
            lane: room_lane,
            logins: preflighted_logins,
            ancestry,
            focus: LaunchFocus::Keep,
            prompt: Some(prompt.into_owned()),
            prompt_index: Some(leader_index),
            leader_index: Some(leader_index),
            placement: Placement::NewTab,
            pick_session: false,
            reminder_section: request.reminder_section.clone(),
            description: request.description.clone(),
            report_to: request.report_to,
            managed_launches,
            loop_task: None,
            cleanup_worktree: request.retries == 0,
            force_new_tab: request.force_new_tab,
            agent_shell_persistence: request.agent_shell_persistence,
            room,
            recovery: context.recovery,
            entry,
            soft_name: false,
            throttle_turn: request.throttle_turn.clone(),
            fallback_assist,
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

/// A subagent run's resolved deadline ladder, as its run record stores it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct DeadlineLadder {
    pub(super) timeout: Duration,
    pub(super) warn: Vec<Duration>,
    pub(super) grace: Duration,
}

/// Resolve a subagent launch's ladder: the launch's own values, then the definition's timeout,
/// then `[agents.subagents]`. Warn offsets are kept only strictly inside the final timeout,
/// longest first, since the deadline check trusts the stored list.
pub(super) fn resolve_deadline_ladder(
    request: &SupervisedRunRequest,
    definition_timeout: Option<Duration>,
    defaults: &crate::config::SubagentsConfig,
    now: jiff::Timestamp,
) -> Result<DeadlineLadder> {
    let parse = |raw: &str, key: &str| {
        crate::utils::time::parse_compact_duration(raw)
            .with_context(|| format!("parsing agents.subagents.{key}"))
    };
    let timeout = match request.timeout.or(definition_timeout) {
        Some(timeout) => timeout,
        None => parse(&defaults.timeout, "timeout")?,
    };
    let mut warn = match &request.warn {
        Some(warn) => warn.clone(),
        None => defaults
            .warn
            .iter()
            .map(|offset| parse(offset, "warn"))
            .collect::<Result<_>>()?,
    };
    warn.retain(|offset| !offset.is_zero() && *offset < timeout);
    warn.sort_unstable_by(|a, b| b.cmp(a));
    warn.dedup();
    let grace = match request.grace {
        Some(grace) => grace,
        None => parse(&defaults.grace, "grace")?,
    };
    now.checked_add(timeout)
        .and_then(|deadline| deadline.checked_add(grace))
        .context("computing subagent kill deadline")?;
    Ok(DeadlineLadder {
        timeout,
        warn,
        grace,
    })
}

#[cfg(test)]
mod tests;
