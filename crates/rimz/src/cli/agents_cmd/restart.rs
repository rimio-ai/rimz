use super::*;
use crate::cli::{render, select};
use rimz::config::effective::ProfileScope;
use rimz::harness::ancestry::{LaunchFocus, resolve_caller};
use rimz::harness::launch::{ExecAction, ExecRequest};
use rimz::launcher::agent_pane::AgentPane;
use rimz::sidebar::produce::hosting::HostingErr;
use rimz::store::writer::SwapAbort;

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
        (ExecAction::Launch { extra_args, .. }, Some(_)) => {
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
    let account = invocation.identity.params.login.clone().unwrap_or_default();
    let backend = rimz::mux::backend_for(old_pane.mux());
    let placed = place_replacement(
        ctx,
        backend.as_ref(),
        agent,
        &old_pane,
        &invocation,
        identity,
        agents,
        &cwd,
        focus,
        handover,
    );
    settle_placement(store, &batch, identity, agent, "restart", placed, || {
        if let Some(run) = handover {
            let _ = rimz::harness::run::hand_over(store.paths(), &run.run_id, None);
        }
    })?;
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
    let after_bind = hosting_after_bind(ctx, agent, &old_pane);
    release_replaced(ctx, agent, &old_pane, fresh_reason.is_some());
    close_replaced(ctx, backend.as_ref(), &old_pane, after_bind);
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
    let handle = rimz::address::agent_handle(agent, peers, true);
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

/// Whether the lookup holds `agent`'s own session in `pane`, not another session of its
/// launch: a revival can leave two live rows of one launch in two panes.
pub(super) fn held_by(pane: &AgentPane, agent: &AgentState) -> bool {
    pane.holder.0 == agent.kind && pane.holder.1 == agent.agent_id
}

/// The refusal's cause when [`held_by`] is false.
pub(super) fn held_elsewhere(pane: &AgentPane) -> String {
    format!(
        "another session of the agent, {}, holds its pane {}",
        pane.holder.1, pane.pane_id
    )
}

/// Where the agent is once its replacement bound, read on the replaced pane's backend before
/// that session is released; [`close_replaced`] closes the pane only on this answer.
pub(super) fn hosting_after_bind(
    ctx: &Ctx,
    agent: &AgentState,
    old_pane: &rimz::ids::PaneId,
) -> Result<Option<AgentPane>, HostingErr> {
    let mut notices = Vec::new();
    let hosting = rimz::launcher::agent_pane::locate(
        &ctx.workspace,
        &ctx.store,
        Some(old_pane.mux()),
        &agent.kind,
        agent.launch_key(),
        &mut notices,
    );
    warn_after_bound(
        "the multiplexer's notices were not printed",
        crate::cli::render::room::print_notices(notices),
    );
    hosting
}

/// Reserve a swap of `source`'s agent, naming a refusal for the command
/// (`verb`) that asked.
pub(super) fn reserve_swap(
    store: &rimz::Store,
    source: &AgentState,
    request: &AgentLaunchRequest,
    scope: AgentLaunchScope,
    verb: &str,
) -> Result<AgentLaunchBatch> {
    store
        .begin_agent_swap(source, request, scope)
        .map_err(|err| match err {
            rimz::store::StoreErr::SwapNotHolder { agent } => anyhow::anyhow!(
                "{verb} refused: @{agent} is not running in the session this {verb} names, or has \
                 ended; nothing was changed"
            ),
            rimz::store::StoreErr::SwapInFlight { agent } => anyhow::anyhow!(
                "{verb} refused: @{agent} already has a restart or molt starting; wait until it \
                 registers or fails, then retry"
            ),
            err => err.into(),
        })
}

/// Open the replacement pane beside `old_pane` and wait for its wrapper to bind.
#[expect(
    clippy::too_many_arguments,
    reason = "the placement inputs are the restart's resolved launch"
)]
pub(super) fn place_replacement(
    ctx: &Ctx,
    backend: &dyn rimz::mux::MuxBackend,
    agent: &AgentState,
    old_pane: &rimz::ids::PaneId,
    invocation: &ExecRequest,
    identity: &AgentLaunchIdentity,
    agents: &[AgentState],
    cwd: &Path,
    focus: LaunchFocus,
    handover: Option<&rimz::store::run::RunRecord>,
) -> Result<()> {
    let workspace = &ctx.workspace;
    let store = &ctx.store;
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
        invocation,
    )?;
    let env = relaunch_identity_env(
        workspace,
        agent.lane_path().map(Path::new).unwrap_or(cwd),
        invocation,
    );
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    if let Some(run) = handover {
        rimz::harness::run::hand_over(store.paths(), &run.run_id, Some(old_pane.clone()))?;
    }
    if focus.takes_focus() {
        rimz::mux::focus_anchor::execute_action(
            backend,
            ctx.runtime(),
            &workspace.session_name,
            old_pane.clone(),
        )
        .context("focusing the agent pane for restart")?;
    }
    let new_pane = backend
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
        .context("opening the replacement agent pane")?;
    #[cfg(any(test, feature = "testkit"))]
    if let Some(bind) = std::env::var_os("RIMZ_TEST_SWAP_BIND_AS_WRAPPER")
        .filter(|value| value == "1" || value == "old")
    {
        // The integration mux shims never run the pane's command, so the
        // test stands in for the replacement's wrapper and binds it here,
        // in the replaced pane itself when it asks for `old`.
        let pane = if bind == "old" {
            old_pane.clone()
        } else {
            new_pane.unwrap_or_else(|| {
                let raw = match old_pane.mux() {
                    rimz::ids::MuxName::Tmux => "%9999",
                    rimz::ids::MuxName::Zellij => "terminal_9999",
                };
                rimz::ids::PaneId::from_parts(old_pane.mux(), raw)
            })
        };
        store.bind_agent_launch(identity, &workspace.session_name, cwd, &pane, None)?;
    }
    #[cfg(not(any(test, feature = "testkit")))]
    let _ = new_pane;
    wait_for_swap_bind(store, identity)
}

/// Wait until the swap's reservation stands: its wrapper bound it (a fresh
/// launch) or the resumed session took its epoch. A reservation its wrapper
/// or a stop failed, or one still unbound at the timeout, is an error.
pub(super) fn wait_for_swap_bind(
    store: &rimz::Store,
    identity: &AgentLaunchIdentity,
) -> Result<()> {
    let (launch, epoch) = (identity.launch_id(), identity.launch.epoch.unwrap_or(0));
    let timeout = swap_bind_wait();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let rows = store.audit_rows()?;
        if rimz::agents::state::epoch_stood(rows.iter(), launch, epoch) {
            return Ok(());
        }
        let failed = rows.iter().any(|row| {
            row.launch_key() == launch
                && row.epoch == epoch
                && row.agent_id.is_provisional()
                && row.session_over()
        });
        if failed {
            bail!("its reservation failed before binding");
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "it did not bind within {}",
                rimz::harness::schedule::arm::duration_label(timeout)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn swap_bind_wait() -> Duration {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(ms) = std::env::var("RIMZ_TEST_SWAP_BIND_WAIT_MS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    rimz::store::writer::SWAP_BIND_WAIT
}

/// The swap's one error boundary: a placement that failed before the
/// replacement bound fails its reservation, and the command fails saying the
/// agent stays in its old session, or that a stop ended it. One that bound
/// anyway carries on.
pub(super) fn settle_placement(
    store: &rimz::Store,
    batch: &AgentLaunchBatch,
    identity: &AgentLaunchIdentity,
    agent: &AgentState,
    verb: &str,
    placed: Result<()>,
    undo: impl FnOnce(),
) -> Result<()> {
    let Err(err) = placed else {
        return Ok(());
    };
    let handle = handle(agent);
    let aborted = match store.abort_agent_swap(identity, batch.scope()) {
        Ok(SwapAbort::Cancelled) if stop_ends_source(store, identity) => Ok(SwapAbort::Ended),
        aborted => aborted,
    };
    match aborted {
        Ok(SwapAbort::Bound) => {
            // The swap carries on, so the caller's cleanup must still run.
            let _ = writeln!(
                crate::cli::render::err(),
                "rimz: the replacement bound despite an error: {err:#}"
            );
            Ok(())
        }
        Ok(SwapAbort::Ended) => {
            undo();
            bail!(
                "{verb} of {handle} failed: the replacement did not start ({err:#}); a stop \
                 ended {handle} while its replacement was starting, so no session of it is running"
            )
        }
        aborted => {
            undo();
            let unrecorded = aborted.err().map_or_else(String::new, |write| {
                format!(" (its reservation could not be marked failed: {write:#})")
            });
            bail!(
                "{verb} of {handle} failed: the replacement did not start ({err:#}){unrecorded}; \
                 {handle} is still running in its old session, and nothing owed to it was touched"
            )
        }
    }
}

/// Whether the stop that cancelled a swap's reservation goes on to end the
/// session it would replace. Such a stop cancels before it closes the holder,
/// so the end can land after the abort: wait for it within the stop's own
/// owner-exit bound.
fn stop_ends_source(store: &rimz::Store, identity: &AgentLaunchIdentity) -> bool {
    let deadline = std::time::Instant::now() + rimz::store::runtime::OWNER_EXIT_BOUND;
    loop {
        match store.swap_source_ended(identity) {
            Ok(true) => return true,
            Err(_) => return false,
            Ok(false) if std::time::Instant::now() >= deadline => return false,
            Ok(false) => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Report on stderr what a swap left undone once its replacement had bound. The swap has
/// succeeded by then, so the command still reports it and exits 0.
fn warn_after_bound(undone: &str, result: Result<()>) {
    if let Err(err) = result {
        let _ = writeln!(crate::cli::render::err(), "warning: {undone}: {err:#}");
    }
}

/// Withdraw what belonged to the session a swap replaced: its hibernation and
/// idle stop, and, once its conversation is over, its raised attention. The
/// agent's waits stay armed for the session that holds it now. Runs after the
/// replacement has bound, so a failure is a warning.
pub(super) fn release_replaced(
    ctx: &Ctx,
    agent: &AgentState,
    old_pane: &rimz::ids::PaneId,
    conversation_over: bool,
) {
    warn_after_bound(
        "the replaced session's idle stop and hibernation were not withdrawn",
        rimz::harness::schedule::arm::release_replaced_session(
            &ctx.workspace.project_root,
            &agent.kind,
            &agent.agent_id,
            conversation_over,
        )
        .map_err(anyhow::Error::msg),
    );
    warn_after_bound(
        &format!("the replaced session's pane {old_pane} was not woken from hibernation"),
        rimz::harness::hibernate::wake_pane(&ctx.store, old_pane)
            .map(drop)
            .map_err(Into::into),
    );
}

/// Close the pane of the session a swap replaced, warning with the pane left open when the
/// multiplexer refuses. Nothing is closed, with a warning, when the read after the bind
/// still finds the agent in that pane or could not be made.
pub(super) fn close_replaced(
    ctx: &Ctx,
    backend: &dyn rimz::mux::MuxBackend,
    old_pane: &rimz::ids::PaneId,
    after_bind: Result<Option<AgentPane>, HostingErr>,
) {
    match after_bind {
        Ok(Some(pane)) if pane.pane_id == *old_pane => {
            let _ = writeln!(
                crate::cli::render::err(),
                "warning: the agent is still in pane {old_pane} after its replacement bound; \
                 the pane was not closed"
            );
            return;
        }
        Err(err) => {
            let _ = writeln!(
                crate::cli::render::err(),
                "warning: the replaced session's pane {old_pane} was not closed: reading where \
                 the agent is after its replacement bound: {err}"
            );
            return;
        }
        Ok(_) => {}
    }
    warn_after_bound(
        &format!("the replaced session's pane {old_pane} is still open"),
        backend
            .close_pane(&ctx.workspace.session_name, old_pane)
            .context("closing the replaced agent pane"),
    );
}

/// The `@rimz` section a fresh session of a swapped agent starts with, listing
/// what is still outstanding for it, or `None` when nothing is. A molt's
/// `note` goes first.
pub(super) fn swap_briefing(
    store: &rimz::Store,
    agent: &AgentState,
    agents: &[AgentState],
    project_root: &Path,
    note: Option<&str>,
) -> Result<Option<String>> {
    let children = rimz::address::launched_children(agents, agent)
        .into_iter()
        .filter(|child| !child.session_over())
        .map(handle)
        .collect::<Vec<_>>();
    let runs = rimz::harness::run::list(store.paths())?;
    // Runtime drops a subagent's ended children, so the answers they owe are
    // read over every retained row: each launch's holder, so a swapped
    // child's replaced session never stands in for the live one.
    let audit = store.audit_rows()?;
    let holders = rimz::agents::state::LaunchHolders::new(audit.iter());
    let held = audit.iter().filter(|row| !holders.replaced(row));
    let owed = rimz::harness::fleet::FleetRuns::of(held, &runs, agent)
        .unreported()
        .into_iter()
        .filter(|(child, _)| child.is_launched_child() && child.session_over())
        .map(|(child, _)| handle(child))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let catalog = rimz::harness::schedule::catalog::TaskCatalog::load(Some(project_root))?;
    let rows = audit.iter().collect::<Vec<_>>();
    let waits = rimz::harness::schedule::pending::session_deliveries(
        &catalog,
        project_root,
        Some(agent),
        &rows,
    )
    .map(|(name, _)| name.to_owned())
    .collect::<Vec<_>>();
    let messages = store
        .list_messages()?
        .into_iter()
        .filter(|message| {
            message.sender.is_conversation() && agent.card_ref().matches(message.card_ref())
        })
        .collect::<Vec<_>>();
    let count = |status: rimz::store::message::MessageStatus| {
        messages
            .iter()
            .filter(|message| message.status == status)
            .count()
    };
    let queued = count(rimz::store::message::MessageStatus::Queued)
        + count(rimz::store::message::MessageStatus::Claimed);
    let unconfirmed = count(rimz::store::message::MessageStatus::Sent);
    let section = briefing_section(&children, &owed, &waits, queued, unconfirmed);
    Ok(match (note, section) {
        (None, None) => None,
        (Some(note), None) => Some(note.to_owned()),
        (None, Some(section)) => Some(section),
        (Some(note), Some(section)) => Some(format!("{note}\n\n{section}")),
    })
}

fn briefing_section(
    children: &[String],
    owed: &[String],
    waits: &[String],
    queued: usize,
    unconfirmed: usize,
) -> Option<String> {
    let plural = |count: usize, one: &str, many: &str| {
        if count == 1 {
            format!("1 {one}")
        } else {
            format!("{count} {many}")
        }
    };
    let mut lines = Vec::new();
    if !children.is_empty() {
        lines.push(format!(
            "- {} still running ({}); their results come to you: `rimz subagents list`",
            plural(children.len(), "subagent", "subagents"),
            children.join(", ")
        ));
    }
    if !owed.is_empty() {
        lines.push(format!(
            "- {} finished with {} still owed to you ({}): `rimz subagents list`",
            plural(owed.len(), "subagent", "subagents"),
            if owed.len() == 1 {
                "its answer"
            } else {
                "their answers"
            },
            owed.join(", ")
        ));
    }
    if !waits.is_empty() {
        lines.push(format!(
            "- {} still armed for you ({}): `rimz wait list`",
            plural(waits.len(), "wait or delivery", "waits or deliveries"),
            waits.join(", ")
        ));
    }
    if queued > 0 {
        lines.push(format!(
            "- {} queued for your next turn: `rimz message list`",
            plural(queued, "message", "messages")
        ));
    }
    if lines.is_empty() && unconfirmed == 0 {
        return None;
    }
    let mut section = String::from(
        "Type: SWAP_BRIEFING\nFrom: @rimz\nContent:\nThis is a fresh session of the same \
         agent; the earlier conversation is gone, and RimZ kept what was outstanding for it.\n",
    );
    for line in &lines {
        section.push_str(line);
        section.push('\n');
    }
    if unconfirmed > 0 {
        section.push_str(&format!(
            "Sent into the earlier session and not yet confirmed as read; RimZ sends a \
             message again, a command it does not:\n- {}: `rimz message list`\n",
            plural(unconfirmed, "message", "messages")
        ));
    }
    Some(section)
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
            rimz::agents::model_display::model_label_parts(model).0,
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

/// The open workflow session run `agent` leads. A restart or molt hands it to the replacement,
/// so only the agent's holder session settles it, and a replaced session's exit never does.
pub(super) fn open_session_run<'a>(
    runs: &'a [rimz::store::run::RunRecord],
    agent: &AgentState,
) -> Option<&'a rimz::store::run::RunRecord> {
    let attempt = agent.workflow.as_ref()?;
    runs.iter().find(|run| {
        run.lifecycle == rimz::store::run::RunLifecycle::Session
            && !run.status.is_terminal()
            && run.workflow.as_ref() == Some(attempt)
            && run.agent_name.is_some()
            && run.agent_name == agent.name
    })
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

/// The relaunch pane's identity env, naming the lane the relaunch carries.
pub(in crate::cli) fn relaunch_identity_env(
    workspace: &rimz::workspace::ResolvedWorkspace,
    cwd: &Path,
    request: &ExecRequest,
) -> std::collections::BTreeMap<String, String> {
    let channel = (request.identity.params.lane.as_ref()).and_then(rimz::ids::Lane::pin_name);
    rimz::room::pane_identity_env(
        workspace,
        cwd,
        channel.as_ref().map(rimz::ids::LaneName::as_str),
    )
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
        molted_from: None,
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
        loop_booked_by: agent.loop_booked_by.clone(),
        loop_fire: agent.loop_fire.clone(),
        workflow: None,
        launch_group: agent.launch_group.clone(),
        launch_ordinal: agent.launch_ordinal,
        channel: None,
        lane: agent.lane.clone(),
        mode: posture.launch.mode,
        isolation: agent.isolation,
        model: posture.launch.model.clone(),
        effort: posture.launch.effort.clone(),
        fast: posture.launch.fast,
        budget: posture.launch.budget.clone(),
        epoch: fresh_identity.map_or((agent.epoch > 0).then_some(agent.epoch), |identity| {
            identity.launch.epoch
        }),
        kind_ordinal: None,
    };
    ExecRequest {
        close_pane_on_exit: true,
        subagent: agent.is_launched_child(),
        identity: rimz::harness::launch::ExecIdentity {
            row_key: fresh_identity.map(|identity| identity.agent_id.to_string()),
            resume_model_override: false,
            name: identity_name.map(ToOwned::to_owned),
            name_explicit: fresh_identity
                .map_or(agent.name_explicit, |identity| identity.name_explicit),
            launch_id: fresh_identity
                .map(|identity| identity.launch_id().to_string())
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
        timeout: None,
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

fn fresh_reason(resume_support: bool, session_present: bool) -> Option<FreshReason> {
    if !resume_support {
        Some(FreshReason::NoResumeSupport)
    } else if !session_present {
        Some(FreshReason::NoRecordedConversation)
    } else {
        None
    }
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
        agent.lane.as_ref(),
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
    request.launch.loop_booked_by = agent.loop_booked_by.clone();
    request.launch.loop_fire = agent.loop_fire.clone();
    request.launch.workflow = agent.workflow.clone();
    request.launch.launch_group = agent.launch_group.clone();
    request.launch.launch_ordinal = agent.launch_ordinal;
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
