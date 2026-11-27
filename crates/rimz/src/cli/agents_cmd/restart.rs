//! CLI selection, continue policy, and rendering for agent and cohort restarts.

use super::*;
use crate::cli::{render, select};
use rimz::harness::ancestry::{LaunchFocus, resolve_caller};
use rimz::harness::launch::ExecAction;
use rimz::launcher::relaunch::relaunch_plan;
use rimz::launcher::swap::{
    finish_replacement, fresh_launch_request, held_by, held_elsewhere, open_session_run,
    relaunch_request, replace, reserve_swap, restart_cell, restart_posture, swap_briefing,
    with_run,
};

/// What `rimz agents restart` adds over a bare relaunch. `rimz teams restart` passes none, so
/// it never hands over a run, checks spend, or briefs a member that comes back fresh, and the
/// only continue it brings is the one a resumed leader's wrapper owes it while no stage is open.
pub(in crate::cli) struct AgentRestart<'a> {
    account: Option<rimz::ids::LoginName>,
    no_continue: bool,
    interrupted: bool,
    caller: Option<&'a AgentState>,
}

pub(super) fn restart_agent(
    reference: Option<String>,
    filters: Vec<String>,
    all: bool,
    account: Option<rimz::ids::LoginName>,
    no_continue: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    if select::filter_help(&filters)? {
        return Ok(());
    }
    let ctx = Ctx::open(globals)?;
    let references = Vec::from_iter(reference);
    let flags = select::TargetFlags {
        references: &references,
        filters: &filters,
        all,
        room: false,
    };
    let selected = match select::select_targets(&ctx, "restart", flags)? {
        select::Targets::Done => return Ok(()),
        select::Targets::Selected(selected) => selected,
        select::Targets::References(references) => {
            let [reference] = references.as_slice() else {
                return Err(UsageError::new(
                    "name one reference: rimz agents restart <ref>, or select with --filter"
                        .to_owned(),
                )
                .into());
            };
            if !all {
                let snapshot = ctx.fold_agent_context(ctx.alive_snapshot()?);
                let agent = crate::cli::resolve_agent_one(
                    &ctx.store,
                    &snapshot,
                    reference,
                    None,
                    &ctx.address_context(),
                )
                .map_err(|mut err| {
                    if let Some(ambiguity) = err.downcast_mut::<render::AmbiguousAgents>() {
                        ambiguity.offer_all("restart");
                    }
                    err
                })?
                .clone();
                return restart_one(&ctx, &snapshot, agent, None, account, no_continue, globals);
            }
            let snapshot = ctx.alive_snapshot()?;
            let reader = ctx.address_context();
            rimz::address::resolve_many(&snapshot, reference, None, &reader)?
                .into_iter()
                .map(|agent| select::Selected {
                    address: rimz::address::paste_address(agent, &snapshot, &reader),
                    agent: Some((agent.kind.clone(), agent.agent_id.clone())),
                })
                .collect()
        }
    };
    let mut selected = selected;
    select::identify(&ctx, &ctx.alive_snapshot()?, &mut selected);
    let mut failed = 0;
    for target in &selected {
        // Each restart relaunches a pane, so the next agent is read again after it.
        let snapshot = ctx.fold_agent_context(ctx.alive_snapshot()?);
        let restarted = select::reread(&ctx, &snapshot, target).and_then(|agent| {
            let agent = agent.clone();
            restart_one(
                &ctx,
                &snapshot,
                agent,
                Some(&target.address),
                account.clone(),
                no_continue,
                globals,
            )
        });
        if let Err(err) = restarted {
            writeln!(render::out(), "error {}: {err:#}", target.address)?;
            failed += 1;
        }
    }
    select::batch_result(failed)
}

/// Restart one agent, read just before: a workflow step through its run, any
/// other agent in place, printing its receipt. A batch passes the selected
/// address, which starts the receipt.
fn restart_one(
    ctx: &Ctx,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    mut agent: AgentState,
    selected: Option<&str>,
    account: Option<rimz::ids::LoginName>,
    no_continue: bool,
    globals: &GlobalFlags,
) -> Result<()> {
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
            "{}restarting step {step} of run {run} via rimz workflow restart {run} --continue",
            receipt_prefix(selected)
        )?;
        return crate::cli::workflow::restart(run.as_str(), true, globals);
    }
    let former_workflow = ownership.former()?;
    if former_workflow.is_some() {
        agent.workflow = None;
    }
    let peers = rimz::address::addressable_agents(snapshot);
    let caller = resolve_caller(&snapshot.agents);
    let focus = LaunchFocus::resolve(false, caller.as_ref());
    let restart = AgentRestart {
        account,
        no_continue,
        interrupted: interrupted(&agent, jiff::Timestamp::now()),
        caller: rimz::harness::ancestry::resolve_calling_agent(&snapshot.agents).ok(),
    };
    let message = restart_resolved(
        ctx,
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
        clear_former_workflow_pin(ctx, &agent, &pin)?;
    }
    writeln!(
        crate::cli::render::out(),
        "{}{message}",
        receipt_prefix(selected)
    )?;
    Ok(())
}

fn receipt_prefix(selected: Option<&str>) -> String {
    selected.map_or_else(String::new, |address| format!("{address}: "))
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
    let cwd = agent
        .worktree_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.worktree_root.clone());
    if agent.checkout.is_some() {
        rimz::agents::empty_dir::prepare_reentry(store.paths(), &cwd)?;
    }
    let machine_config = crate::cli::machine_config();
    let (posture, warning) = restart_posture(agent, workspace, &machine_config)?;
    if let Some(warning) = warning {
        writeln!(render::err(), "{warning}")?;
    }
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
        let profile = match (agent.team.as_deref(), agent.role.as_deref()) {
            (Some(team), Some(role)) => std::borrow::Cow::Owned(format!("{team}.{role}")),
            _ => {
                std::borrow::Cow::Borrowed(agent.profile.as_deref().unwrap_or(agent.kind.as_str()))
            }
        };
        for warning in rimz::harness::plan::LaunchFinalizeWarning::take_cell_notices(cell, &profile)
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
    // The replacement launches with the posture's prompt sources, which `restart_cell` drops.
    if fresh_reason.is_some() {
        posture
            .launch
            .validate_fresh_agent_file_lists(&agent.kind)?;
    }
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
    // Where the agent is once its login's hooks pass, before the folder-trust and process
    // preflights; the swap replaces this pane, checked again once the preflights are done.
    let mut notices = Vec::new();
    let hosting = rimz::launcher::agent_pane::locate(
        workspace,
        store,
        ctx.mux(),
        &agent.kind,
        agent.launch_key(),
        &mut notices,
    );
    crate::cli::render::room::print_notices(notices)?;
    let old_pane = match hosting {
        Ok(Some(pane)) if !held_by(&pane, agent) => {
            bail!("{}; nothing was restarted", held_elsewhere(&pane))
        }
        Ok(Some(pane)) => pane.pane_id,
        Ok(None) => bail!("agent has no bound pane; nothing to restart"),
        Err(err) => bail!("{err}; nothing was restarted"),
    };
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
            rimz::harness::launch::PreflightCheckout::Cwd,
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
            &machine_config.harness.auto_redeem,
            jiff::Timestamp::now(),
        )?;
    }
    let child = restart.is_some() && agent.is_launched_child();
    let runs = if child || agent.workflow.is_some() {
        rimz::harness::run::list(store.paths())?
    } else {
        Vec::new()
    };
    let (child_handover, failed_run) = open_run_fate(
        child.then(|| newest_run_for_child(&runs, agent)).flatten(),
        fresh_reason.is_none(),
        skipped_continue.is_some(),
    );
    let handover = child_handover.or_else(|| open_session_run(&runs, agent));
    // An accepted window: the briefing and the reservation run after this read, so a move
    // during them can place the split beside a pane the agent left, while the close stays
    // guarded by `hosting_after_bind`.
    find_replaced_pane(ctx, agent, &old_pane)?;
    let mut request = fresh_launch_request(agent, cell, posture.launch.mode)?;
    request.login = match (fresh_reason, flag_account) {
        (Some(_), None) => request.login,
        (Some(_), Some(account)) => rimz::store::writer::LaunchLogin::Pinned(account.clone()),
        (None, _) => rimz::store::writer::LaunchLogin::Pinned(login.clone().unwrap_or_default()),
    };
    let action = match (action, fresh_reason) {
        // A team restart briefs nobody: the member its board wakes takes its stage notice.
        (ExecAction::Launch { extra_args, .. }, Some(_)) if restart.is_some() => {
            let prompt = swap_briefing(store, agent, agents, &workspace.project_root, None)?;
            request.prompt.clone_from(&prompt);
            ExecAction::Launch { prompt, extra_args }
        }
        (action, _) => action,
    };
    let batch = reserve_swap(
        store,
        agent,
        &request,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.clone(),
            branch: None,
            description: None,
        },
        "restart",
    )?;
    let identity = batch.single_identity()?;
    let mut invocation = relaunch_request(agent, &posture, action, login, Some(identity));
    if let Some(run) = handover {
        invocation = with_run(invocation, run);
    }
    invocation.continue_session = continues;
    invocation.no_continue = restart.as_ref().is_some_and(|restart| restart.no_continue);
    let account = invocation.identity.params.login.clone().unwrap_or_default();
    let backend = rimz::mux::backend_for(old_pane.mux());
    if let Some(warning) = replace(
        workspace,
        store,
        backend.as_ref(),
        agent,
        &old_pane,
        &invocation,
        &batch,
        identity,
        agents,
        &cwd,
        focus,
        handover,
        "restart",
    )? {
        let _ = writeln!(render::err(), "{warning}");
    }
    if let Some(run) = failed_run {
        warn_after_bound(
            "the child's open run was not failed",
            fail_restarted_run(store, run, fresh_reason, skipped_continue),
        );
    }
    if let Err(err) = settle_peer_before_restart(store, agent) {
        let _ = writeln!(
            std::io::stderr(),
            "warning: could not settle the peer's open turn before restart: {err:#}"
        );
    }
    finish_replacement(
        workspace,
        store,
        backend.as_ref(),
        agent,
        &old_pane,
        fresh_reason.is_some(),
        |notice| writeln!(render::err(), "{notice}"),
    );
    if let Some(run) = child_handover {
        let caller = restart.as_ref().and_then(|restart| restart.caller);
        if let Err(err) = notify_parent_restarted(ctx, agents, agent, run, &account, caller) {
            let _ = writeln!(
                crate::cli::render::err(),
                "rimz: could not tell the parent about the restart: {err:#}"
            );
        }
    }

    let mut note = String::new();
    if flag_account.is_some() {
        note.push_str(&format!("; on account `{account}`"));
    }
    if continues {
        note.push_str("; continue queued");
    }
    let handle = rimz::address::named_handle(agent, peers, true);
    Ok(match fresh_reason {
        Some(reason) => format!("restarted {handle} fresh — {reason}{note}"),
        None => format!(
            "restarted {handle} (resumed session {}){note}",
            agent.agent_id
        ),
    })
}

/// Wake the pane the restart replaces, `old_pane`, and refuse unless the lookup still finds
/// the agent there after the wake.
fn find_replaced_pane(ctx: &Ctx, agent: &AgentState, old_pane: &rimz::ids::PaneId) -> Result<()> {
    let mut notices = Vec::new();
    let found = rimz::launcher::agent_pane::find(
        &ctx.workspace,
        &ctx.store,
        Some(old_pane.mux()),
        &agent.kind,
        agent.launch_key(),
        &mut notices,
    );
    crate::cli::render::room::print_notices(notices)?;
    match found {
        Ok(pane) if !held_by(&pane, agent) => {
            bail!("{}; nothing was restarted", held_elsewhere(&pane))
        }
        Ok(pane) if pane.pane_id == *old_pane => Ok(()),
        Ok(pane) => bail!(
            "the agent moved from pane {old_pane} to pane {} during the restart's checks; \
             nothing was restarted",
            pane.pane_id
        ),
        Err(rimz::launcher::agent_pane::FindErr::Nowhere) => bail!(
            "the agent left pane {old_pane} during the restart's checks and is in no pane now; \
             nothing was restarted"
        ),
        Err(err) => bail!("{err}; nothing was restarted"),
    }
}

/// Report on stderr what a swap left undone once its replacement had bound. The swap has
/// succeeded by then, so the command still reports it and exits 0.
fn warn_after_bound(undone: &str, result: Result<()>) {
    if let Err(err) = result {
        let _ = writeln!(crate::cli::render::err(), "warning: {undone}: {err:#}");
    }
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
    config: &rimz::config::AutoRedeemConfig,
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
    let capacity = rimz::agents::account::ProviderCapacity::read(runtime, &key);
    if let Some(model) = model
        && capacity
            .as_ref()
            .is_some_and(|capacity| capacity.model_refused(Some(model)))
    {
        bail!(
            "restart refused: the provider refuses {} on {} account `{}`, so the continue for \
             {}'s interrupted turn would stop again; pass `--account <other>` to run it on \
             another account, or `--no-continue` to reopen it at rest",
            rimz::agents::model_display::display_model_for_kind(model, Some(key.kind.as_str())),
            key.kind,
            key.name,
            handle(agent),
        );
    }
    let reset = capacity
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
    if let Some(record) = rimz::harness::run::fail_if_nonterminal(
        store.paths(),
        store.runtime_paths(),
        &run.run_id,
        &reason,
    )? {
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
        premises: rimz::store::message::Premise::NONE,
    }
    .record(&ctx.workspace)
    .with_about_run(Some(run.run_id.clone()));
    ctx.store
        .queue_message(&message, &ctx.workspace.session_name)
        .context("queueing the restart notice")?;
    if let Some(pane) = &parent.pane {
        rimz::message::deliver::deliver_one(
            &ctx.workspace,
            &ctx.store,
            &message.message_id,
            Some(pane.pane_id.mux()),
            rimz::message::deliver::DeliveryPolicy::Boundary,
        )
        .context("delivering the restart notice")?;
    }
    Ok(())
}

fn settle_peer_before_restart(store: &rimz::Store, agent: &AgentState) -> Result<()> {
    if rimz::harness::run::fail_peer_turn(store, agent, "peer restarted")?.is_some()
        && let Some((_, launcher)) = agent.launcher()
    {
        rimz::harness::orphan_sweep::spawn_digest_helper(store.runtime_paths(), launcher.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
