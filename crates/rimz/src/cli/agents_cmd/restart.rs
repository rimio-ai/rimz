use super::*;
use rimz::config::effective::ProfileScope;
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

/// What `rimz agents restart` adds over a bare relaunch. `rimz teams restart` passes none, so
/// it never continues, hands over a run, or checks spend.
pub(in crate::cli) struct AgentRestart<'a> {
    account: Option<rimz::ids::LoginName>,
    no_continue: bool,
    interrupted: bool,
    caller: Option<&'a AgentState>,
}

pub(super) fn restart_agent(
    reference: String,
    account: Option<rimz::ids::LoginName>,
    no_continue: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.fold_agent_context(ctx.alive_snapshot()?);
    let agent = crate::cli::resolve_agent_one(
        &ctx.store,
        &snapshot,
        &reference,
        None,
        &ctx.address_context(),
    )?
    .clone();
    let ownership = workflow_pin(&ctx.store, &agent)?;
    if let WorkflowPin::Current {
        run,
        step,
        role: rimz::agents::WorkflowAttemptRole::Step,
    } = &ownership
    {
        if account.is_some() || no_continue {
            bail!(
                "restart refused: this agent runs step {step} of workflow run {run}, which \
                 `--account` and `--no-continue` do not apply to; run `rimz workflow restart \
                 {run} --continue` instead"
            );
        }
        writeln!(
            crate::cli::render::out(),
            "restarting step {step} of run {run} via rimz workflow restart {run} --continue"
        )?;
        return crate::cli::workflow::restart(run.as_str(), true, globals);
    }
    let former_workflow = ownership.former()?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let caller = resolve_caller(&snapshot.agents);
    let focus = LaunchFocus::resolve(false, caller.as_ref());
    let restart = AgentRestart {
        account,
        no_continue,
        interrupted: interrupted(&agent, jiff::Timestamp::now()),
        caller: rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents).ok(),
    };
    let message = restart_resolved(
        &ctx,
        &agent,
        &peers,
        &snapshot.agents,
        focus,
        None,
        || {
            caller
                .as_ref()
                .map(|_| rimz::launcher::trust::Unattended::Agent)
        },
        Some(restart),
    )?;
    if let Some(pin) = former_workflow {
        clear_former_workflow_pin(&ctx, &agent, &pin)?;
    }
    writeln!(crate::cli::render::out(), "{message}")?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the agent restart options are independent of the shared relaunch inputs"
)]
pub(in crate::cli) fn restart_resolved(
    ctx: &Ctx,
    agent: &AgentState,
    peers: &[&AgentState],
    agents: &[AgentState],
    focus: LaunchFocus,
    unattended: Option<rimz::launcher::trust::Unattended>,
    hook_unattended: impl FnOnce() -> Option<rimz::launcher::trust::Unattended>,
    restart: Option<AgentRestart<'_>>,
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

    let catalog = rimz::agents::machine_login_catalog();
    let flag_account = restart
        .as_ref()
        .and_then(|restart| restart.account.as_ref());
    let (action, fresh_reason) = relaunch_plan(agent, &cwd)?;
    let login = match flag_account {
        Some(account) => flag_login(
            agent,
            account,
            fresh_reason.is_none(),
            &machine_config.accounts,
            &catalog,
        )?,
        None => {
            let logins = rimz::agents::room_accounts(
                &store.paths().workspace_record,
                Some(&workspace.project_root),
                &rimz::config::MachineConfig::load_lenient(),
            )?;
            rimz::harness::resume::relaunch_login(agent, &logins, &catalog)?
        }
    };
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
    let skipped_continue = restart
        .as_ref()
        .filter(|restart| restart.interrupted)
        .and_then(|restart| {
            if restart.no_continue {
                Some("--no-continue")
            } else if super::exec::continue_text(&machine_config.resume.auto_continue_text)
                .is_none()
            {
                Some("a blank `resume.auto_continue_text`")
            } else {
                None
            }
        });
    let continues = fresh_reason.is_none()
        && restart.as_ref().is_some_and(|restart| restart.interrupted)
        && skipped_continue.is_none();
    if continues {
        refuse_spent(
            store.runtime_paths(),
            agent,
            login.as_ref(),
            &machine_config.resume,
            jiff::Timestamp::now(),
        )?;
    }
    let runs = match &restart {
        Some(_) if agent.is_launched_child() => rimz::harness::run::list(store.paths())?,
        _ => Vec::new(),
    };
    let (handover, failed_run) = open_run_fate(
        newest_run_for_child(&runs, agent),
        fresh_reason.is_none(),
        skipped_continue.is_some(),
    );
    rimz::harness::hibernate::wake_pane(store, &old_pane)?;
    let fresh_batch = if fresh_reason.is_some() {
        Some(append_fresh_launch(
            store,
            workspace,
            agent,
            &cwd,
            cell,
            posture.launch.mode,
            flag_account.cloned(),
        )?)
    } else {
        None
    };
    let fresh_identity = fresh_batch
        .as_ref()
        .map(AgentLaunchBatch::single_identity)
        .transpose()?;
    let mut invocation = relaunch_request(agent, &posture, action, login, fresh_identity);
    if let Some(run) = handover {
        invocation = with_run(invocation, run);
    }
    invocation.continue_session = continues;
    let account = invocation.identity.params.login.clone().unwrap_or_default();
    let pane_name = rimz::agents::pane_title::title(
        rimz::agents::pane_title::Seat::from_launch(
            &invocation.kind,
            invocation.identity.name.as_deref(),
            &invocation.identity.params,
        ),
        agents,
    );
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

    if let Some(run) = handover {
        rimz::harness::run::hand_over(store.paths(), &run.run_id, Some(old_pane.clone()))?;
    }
    let abandon = |err: anyhow::Error| {
        if let Some(batch) = &fresh_batch {
            let _ = store.fail_agent_launch_batch(batch);
        }
        if let Some(run) = handover {
            let _ = rimz::harness::run::hand_over(store.paths(), &run.run_id, None);
        }
        err
    };
    if focus.takes_focus()
        && let Err(err) = rimz::mux::focus_anchor::execute_action(
            backend.as_ref(),
            ctx.runtime(),
            &workspace.session_name,
            old_pane.clone(),
        )
        .context("focusing the agent pane for restart")
    {
        return Err(abandon(err));
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
        return Err(abandon(err));
    }
    if let Some(run) = failed_run {
        fail_restarted_run(store, run, fresh_reason, skipped_continue)?;
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
    if let Some(run) = handover {
        let caller = restart.as_ref().and_then(|restart| restart.caller);
        if let Err(err) = notify_parent_restarted(ctx, agents, agent, run, &account, caller) {
            let _ = writeln!(
                crate::cli::render::err(),
                "rimz: could not tell the parent about the restart: {err:#}"
            );
        }
    }

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
    let mut note = dropped_note(retired_count(retired, &mut crate::cli::render::err())?);
    if flag_account.is_some() {
        note.push_str(&format!("; on account `{account}`"));
    }
    if continues {
        note.push_str("; continue queued");
    }

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
    let (action, fresh_reason) = relaunch_plan(agent, cwd)?;
    Ok((action, login, fresh_reason))
}

/// Whether the agent's session resumes, and the reason it launches fresh when it cannot.
fn relaunch_plan(
    agent: &AgentState,
    cwd: &Path,
) -> Result<(rimz::harness::launch::ExecAction, Option<&'static str>)> {
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
    Ok((action, fresh_reason.map(FreshReason::as_str)))
}

/// The login `--account` names: an account of the agent's own kind and, when the session
/// resumes, one in its history pool. The room's selection is neither read nor written.
fn flag_login(
    agent: &AgentState,
    account: &rimz::ids::LoginName,
    resumes: bool,
    accounts: &rimz::config::AccountsConfig,
    catalog: &rimz::agents::LoginCatalog,
) -> Result<Option<rimz::ids::LoginName>> {
    rimz::agents::session_login(&agent.kind, Some(account), accounts)?;
    let session = agent.login.clone().unwrap_or_default();
    let pool = |name: &rimz::ids::LoginName| {
        catalog.pool(&rimz::ids::LoginKey {
            kind: agent.kind.clone(),
            name: name.clone(),
        })
    };
    if resumes && pool(&session) != pool(account) {
        bail!(
            "restart refused: {}'s session belongs to {} account `{session}`, and `{account}` \
             keeps its history apart, so the session cannot resume there; pass an account that \
             shares `{session}`'s history, or `{session}` itself",
            handle(agent),
            agent.kind
        );
    }
    Ok((!account.is_default()).then(|| account.clone()))
}

/// Whether the agent was stopped mid-turn: its row, with context folded, displays as running,
/// paused, or blocked on its own prompt.
fn interrupted(agent: &AgentState, now: jiff::Timestamp) -> bool {
    matches!(
        agent.effective_status(now),
        rimz::agents::AgentStatus::Running
            | rimz::agents::AgentStatus::Paused
            | rimz::agents::AgentStatus::Waiting
    )
}

/// Refuse a continue on an account the spend ladder reads as spent for the agent's model: the
/// continue would only stop on the limit again.
fn refuse_spent(
    runtime: &rimz::RuntimePaths,
    agent: &AgentState,
    login: Option<&rimz::ids::LoginName>,
    config: &rimz::config::ResumeConfig,
    now: jiff::Timestamp,
) -> Result<()> {
    let key = rimz::ids::LoginKey {
        kind: agent.kind.clone(),
        name: login.cloned().unwrap_or_default(),
    };
    let model = agent.model.as_deref();
    if rimz::harness::spend_ladder::SpendFacts::read(runtime, &key, now).step(config, model, now)
        != rimz::harness::spend_ladder::SpendStep::Spent
    {
        return Ok(());
    }
    let reset = rimz::agents::account::ProviderCapacity::read(runtime, &key)
        .and_then(|capacity| capacity.latest_spent_window_reset_for_model(now, model))
        .map_or_else(String::new, |reset| {
            format!(
                " until {}",
                rimz::utils::time::format_local_timestamp(reset)
            )
        });
    bail!(
        "restart refused: {} account `{}` is spent{reset}, so the continue for {}'s interrupted \
         turn would stop again; pass `--account <other>` to run it on another account, or \
         `--no-continue` to reopen it at rest",
        key.kind,
        key.name,
        handle(agent),
    )
}

/// The agent as a message names it: its handle, or its session id when it has no name.
fn handle(agent: &AgentState) -> String {
    agent
        .name
        .as_ref()
        .map_or_else(|| agent.agent_id.to_string(), |name| format!("@{name}"))
}

/// The newest supervised run recorded for `child`, the run the resume doorway reopens.
pub(in crate::cli) fn newest_run_for_child<'a>(
    runs: &'a [rimz::store::run::RunRecord],
    child: &AgentState,
) -> Option<&'a rimz::store::run::RunRecord> {
    runs.iter()
        .filter(|run| {
            run.kind == child.kind
                && (run.agent_id.as_ref() == Some(&child.agent_id)
                    || run
                        .agent_name
                        .as_ref()
                        .zip(child.name.as_ref())
                        .is_some_and(|(run, child)| run == child))
        })
        .max_by_key(|run| run.started_at)
}

/// What a restart does with a child's newest run, as `(handed over, failed)`: an open run goes
/// to the replacement when the session resumes and no interrupted turn is abandoned, and fails
/// otherwise. A settled run stays unbound.
fn open_run_fate(
    run: Option<&rimz::store::run::RunRecord>,
    resumes: bool,
    abandons_turn: bool,
) -> (
    Option<&rimz::store::run::RunRecord>,
    Option<&rimz::store::run::RunRecord>,
) {
    match run.filter(|run| !run.status.is_terminal()) {
        Some(run) if resumes && !abandons_turn => (Some(run), None),
        open => (None, open),
    }
}

/// A relaunch request that carries `run` with its exit policy, as the resume doorway sets them.
pub(in crate::cli) fn with_run(
    request: ExecRequest,
    run: &rimz::store::run::RunRecord,
) -> ExecRequest {
    let (close_pane_on_exit, exit_on_run_completion) =
        rimz::harness::plan::run_exit_policy(!run.keep);
    ExecRequest {
        run_id: Some(run.run_id.clone()),
        close_pane_on_exit,
        exit_on_run_completion,
        ..request
    }
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
        automation: agent.automation.clone(),
        checkout: agent.checkout.clone(),
        add_dirs: agent.add_dirs.clone(),
        untrusted_checkout: agent.untrusted_checkout,
        molted_from: agent.molted_from.clone(),
        record: posture.launch.record.clone(),
        parent_agent_id: agent.parent_agent_id.clone(),
        parent_agent_kind: agent.parent_agent_kind.clone(),
        launch_depth: agent.launch_depth,
        launched_by: agent.launched_by.clone().map(Box::new),
        profile: agent.profile.clone(),
        profile_scope: agent.profile_scope,
        login: fresh_identity.map_or(login, |identity| identity.launch.login.clone()),
        tier: posture.launch.tier.clone(),
        role: agent.role.clone(),
        team: agent.team.clone(),
        loop_task: agent.loop_task.clone(),
        workflow: None,
        launch_group: agent.launch_group.clone(),
        launch_ordinal: agent.launch_ordinal,
        channel: agent.channel.clone(),
        lane: agent.lane.clone(),
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
    let scope = ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child());
    let profiles = launch.profiles_for_launch(scope, agent.profile.as_deref());
    launch.block_failed_launch(
        &agent.kind,
        agent.profile.as_deref(),
        agent.record.as_deref(),
        profiles,
    )?;
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
        auto_compact: posture.launch.auto_compact.clone(),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: posture.launch.skills.clone(),
        no_skill_tool: posture.launch.no_skill_tool,
        allowed_tools: posture.launch.allowed_tools.clone(),
        isolation_default: posture.launch.isolation_default,
        launch: rimz::agents::LaunchParams {
            automation: agent.automation.clone(),
            checkout: agent.checkout.clone(),
            add_dirs: agent.add_dirs.clone(),
            untrusted_checkout: agent.untrusted_checkout,
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
    login: Option<rimz::ids::LoginName>,
) -> Result<AgentLaunchBatch> {
    let mut request = fresh_launch_request(agent, cell, mode)?;
    if let Some(login) = login {
        request.login = rimz::store::writer::LaunchLogin::Pinned(login);
    }
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
    request.launch.profile_scope = agent.profile_scope;
    request.launch.checkout = agent.checkout.clone();
    request.launch.add_dirs.clone_from(&agent.add_dirs);
    request.launch.untrusted_checkout = agent.untrusted_checkout;
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

/// Fail a child's open run that this restart cannot carry over: the session launches fresh, or
/// its interrupted turn is abandoned for `skipped_continue`. The old wrapper's exit then reports
/// it to the parent.
fn fail_restarted_run(
    store: &rimz::Store,
    run: &rimz::store::run::RunRecord,
    fresh_reason: Option<&str>,
    skipped_continue: Option<&str>,
) -> Result<()> {
    let reason = fresh_reason.map_or_else(
        || {
            format!(
                "restarted with {}; its interrupted turn was abandoned",
                skipped_continue.unwrap_or("--no-continue")
            )
        },
        |reason| format!("restarted fresh ({reason}); its session did not resume"),
    );
    if let Some(record) =
        rimz::harness::run::fail_if_nonterminal(store.paths(), &run.run_id, &reason)?
    {
        rimz::store::run::wake_run(store.runtime_paths(), &record);
    }
    Ok(())
}

/// Tell the child's parent its run carried over to the replacement pane, unless the parent ran
/// the restart itself.
fn notify_parent_restarted(
    ctx: &Ctx,
    agents: &[AgentState],
    child: &AgentState,
    run: &rimz::store::run::RunRecord,
    account: &rimz::ids::LoginName,
    caller: Option<&AgentState>,
) -> Result<()> {
    use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};
    let Some(parent) = rimz::address::launched_parent(agents, child) else {
        return Ok(());
    };
    if caller.is_some_and(|caller| caller.card_ref().matches(parent.card_ref())) {
        return Ok(());
    }
    let handle = handle(child);
    let message = rimz::message::synthetic::SyntheticMessage {
        agent: parent,
        text: format!(
            "{handle} was restarted in a new pane and now runs on {} account `{account}`. Its \
             run is still open and carries on; nothing is needed from you.",
            child.kind
        ),
        sender: MessageSender::Harness {
            notice: HarnessNotice::SubagentRestarted,
        },
        gate: DeliveryGate::Done,
        pane_id: parent.pane.as_ref().map(|pane| pane.pane_id.clone()),
    }
    .record(&ctx.workspace)
    .with_about_run(Some(run.run_id.clone()));
    ctx.store
        .queue_message(&message, &ctx.workspace.session_name)
        .context("queueing the restart notice")?;
    if let Some(pane_id) = &message.pane_id {
        rimz::message::deliver::deliver_one(
            &ctx.workspace,
            &ctx.store,
            &message.message_id,
            Some(pane_id.mux()),
            rimz::message::deliver::DeliveryPolicy::Boundary,
        )
        .context("delivering the restart notice")?;
    }
    Ok(())
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
