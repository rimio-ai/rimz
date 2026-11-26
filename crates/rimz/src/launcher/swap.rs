//! Session-swap reservation, placement, and cleanup shared by restart and molt.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};

use crate::agents::{AgentState, PermissionMode};
use crate::config::effective::ProfileScope;
use crate::harness::ancestry::LaunchFocus;
use crate::harness::launch::{ExecAction, ExecRequest};
use crate::harness::resume::{PostureDegrade, ResumePosture};
use crate::harness::spec::{AgentCell, Cell, LayoutSpec};
use crate::ids::PaneId;
use crate::mux::{MuxBackend, SplitPaneOptions};
use crate::sidebar::produce::hosting::HostingErr;
use crate::store::run::RunRecord;
use crate::store::writer::{
    AgentLaunchBatch, AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest, AgentLaunchScope,
    SwapAbort,
};
use crate::{ResolvedWorkspace, Store};

use super::agent_pane::{self, AgentPane};

/// Why a session swap could not reserve, reconstruct, or bind its replacement.
#[derive(Debug, thiserror::Error)]
pub enum SwapErr {
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Config(#[from] crate::config::effective::EffectiveConfigErr),
    // The launch planner and task catalog still return dynamic errors.
    #[error(transparent)]
    Planning(#[from] anyhow::Error),
}

type Result<T> = std::result::Result<T, SwapErr>;

/// Whether the lookup holds `agent`'s own session in `pane`, not another session of its
/// launch: a revival can leave two live rows of one launch in two panes.
pub fn held_by(pane: &AgentPane, agent: &AgentState) -> bool {
    pane.holder.0 == agent.kind && pane.holder.1 == agent.agent_id
}

/// The refusal's cause when [`held_by`] is false.
pub fn held_elsewhere(pane: &AgentPane) -> String {
    format!(
        "another session of the agent, {}, holds its pane {}",
        pane.holder.1, pane.pane_id
    )
}

/// Where the agent is once its replacement bound, read on the replaced pane's backend before
/// that session is released; [`close_replaced`] closes the pane only on this answer.
fn hosting_after_bind(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentState,
    old_pane: &PaneId,
    notices: &mut Vec<String>,
) -> std::result::Result<Option<AgentPane>, HostingErr> {
    agent_pane::locate(
        workspace,
        store,
        Some(old_pane.mux()),
        &agent.kind,
        agent.launch_key(),
        notices,
    )
}

/// Reserve a swap of `source`'s agent, naming a refusal for the command
/// (`verb`) that asked.
pub fn reserve_swap(
    store: &Store,
    source: &AgentState,
    request: &AgentLaunchRequest,
    scope: AgentLaunchScope,
    verb: &str,
) -> Result<AgentLaunchBatch> {
    store
        .begin_agent_swap(source, request, scope)
        .map_err(|err| match err {
            crate::store::StoreErr::SwapNotHolder { agent } => SwapErr::Refused(format!(
                "{verb} refused: @{agent} is not running in the session this {verb} names, or has \
                 ended; nothing was changed"
            )),
            crate::store::StoreErr::SwapInFlight { agent } => SwapErr::Refused(format!(
                "{verb} refused: @{agent} already has a restart or molt starting; wait until it \
                 registers or fails, then retry"
            )),
            err => err.into(),
        })
}

/// Place the reserved replacement and settle its bind. A replacement that bound despite a
/// placement error stands and returns a warning. Undo any run handover if the swap failed.
#[expect(
    clippy::too_many_arguments,
    reason = "placement uses the successor posture, settlement uses the source and reservation"
)]
pub fn replace(
    workspace: &ResolvedWorkspace,
    store: &Store,
    backend: &dyn MuxBackend,
    source: &AgentState,
    old_pane: &PaneId,
    invocation: &ExecRequest,
    batch: &AgentLaunchBatch,
    identity: &AgentLaunchIdentity,
    agents: &[AgentState],
    cwd: &Path,
    focus: LaunchFocus,
    handover: Option<&RunRecord>,
    verb: &str,
) -> Result<Option<String>> {
    let placed = place_replacement(
        workspace, store, backend, source, old_pane, invocation, identity, agents, cwd, focus,
        handover,
    );
    settle_placement(store, batch, identity, source, verb, placed, || {
        if let Some(run) = handover {
            let _ = crate::harness::run::hand_over(store.paths(), &run.run_id, None);
        }
    })
}

/// Open the replacement pane beside `old_pane` and wait for its wrapper to bind.
#[expect(
    clippy::too_many_arguments,
    reason = "the placement inputs are the resolved swap"
)]
fn place_replacement(
    workspace: &ResolvedWorkspace,
    store: &Store,
    backend: &dyn MuxBackend,
    source: &AgentState,
    old_pane: &PaneId,
    invocation: &ExecRequest,
    identity: &AgentLaunchIdentity,
    agents: &[AgentState],
    cwd: &Path,
    focus: LaunchFocus,
    handover: Option<&RunRecord>,
) -> anyhow::Result<()> {
    let pane_name = crate::agents::pane_title::title(
        crate::agents::pane_title::Seat::from_launch(
            &invocation.kind,
            invocation.identity.name.as_deref(),
            &invocation.identity.params,
        ),
        agents,
    );
    let argv = crate::harness::launch::exec_argv(
        &crate::proc::rimz_exe(),
        store.runtime_paths(),
        invocation,
    )?;
    let env = relaunch_identity_env(
        workspace,
        source.lane_path().map(Path::new).unwrap_or(cwd),
        invocation,
    );
    let direction = crate::mux::detect_terminal_size()
        .map(|(cols, rows)| crate::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    if let Some(run) = handover {
        crate::harness::run::hand_over(store.paths(), &run.run_id, Some(old_pane.clone()))?;
    }
    if focus.takes_focus() {
        crate::mux::focus_anchor::execute_action(
            backend,
            store.runtime_paths(),
            &workspace.session_name,
            old_pane.clone(),
        )
        .context("focusing the agent pane for restart")?;
    }
    let new_pane = backend
        .split_pane(SplitPaneOptions {
            target: crate::mux::SplitTarget::SessionPane {
                session_name: workspace.session_name.clone(),
                pane_id: old_pane.clone(),
            },
            cwd: Some(cwd.display().to_string()),
            command: Some(argv),
            title: Some(pane_name),
            close_on_exit: false,
            env,
            placement: crate::mux::SplitPlacement::Rejoin(direction),
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
                    crate::ids::MuxName::Tmux => "%9999",
                    crate::ids::MuxName::Zellij => "terminal_9999",
                };
                crate::ids::PaneId::from_parts(old_pane.mux(), raw)
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
fn wait_for_swap_bind(store: &Store, identity: &AgentLaunchIdentity) -> anyhow::Result<()> {
    let (launch, epoch) = (identity.launch_id(), identity.launch.epoch.unwrap_or(0));
    let timeout = swap_bind_wait();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let rows = store.audit_rows()?;
        if crate::agents::state::epoch_stood(rows.iter(), launch, epoch) {
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
                crate::harness::schedule::arm::duration_label(timeout)
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
    crate::store::writer::SWAP_BIND_WAIT
}

/// The swap's one error boundary: a placement that failed before the
/// replacement bound fails its reservation, and the command fails saying the
/// agent stays in its old session, or that a stop ended it. One that bound
/// anyway carries on.
fn settle_placement(
    store: &Store,
    batch: &AgentLaunchBatch,
    identity: &AgentLaunchIdentity,
    agent: &AgentState,
    verb: &str,
    placed: anyhow::Result<()>,
    undo: impl FnOnce(),
) -> Result<Option<String>> {
    let Err(err) = placed else {
        return Ok(None);
    };
    let handle = handle(agent);
    let aborted = match store.abort_agent_swap(identity, batch.scope()) {
        Ok(SwapAbort::Cancelled) if stop_ends_source(store, identity) => Ok(SwapAbort::Ended),
        aborted => aborted,
    };
    match aborted {
        Ok(SwapAbort::Bound) => Ok(Some(format!(
            "rimz: the replacement bound despite an error: {err:#}"
        ))),
        Ok(SwapAbort::Ended) => {
            undo();
            Err(SwapErr::Refused(format!(
                "{verb} of {handle} failed: the replacement did not start ({err:#}); a stop \
                 ended {handle} while its replacement was starting, so no session of it is running"
            )))
        }
        aborted => {
            undo();
            let unrecorded = aborted.err().map_or_else(String::new, |write| {
                format!(" (its reservation could not be marked failed: {write:#})")
            });
            Err(SwapErr::Refused(format!(
                "{verb} of {handle} failed: the replacement did not start ({err:#}){unrecorded}; \
                 {handle} is still running in its old session, and nothing owed to it was touched"
            )))
        }
    }
}

/// Whether the stop that cancelled a swap's reservation goes on to end the
/// session it would replace. Such a stop cancels before it closes the holder,
/// so the end can land after the abort: wait for it within the stop's own
/// owner-exit bound.
fn stop_ends_source(store: &Store, identity: &AgentLaunchIdentity) -> bool {
    let deadline = std::time::Instant::now() + crate::store::runtime::OWNER_EXIT_BOUND;
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

/// Read the holder, release the replaced session, and close its pane only if the read permits
/// it. Cleanup notices are returned in order and may also be presented while the pane is open.
/// Presentation and cleanup failures are non-fatal once the replacement has bound.
pub fn finish_replacement(
    workspace: &ResolvedWorkspace,
    store: &Store,
    backend: &dyn MuxBackend,
    agent: &AgentState,
    old_pane: &PaneId,
    conversation_over: bool,
    mut present: impl FnMut(&str) -> std::io::Result<()>,
) -> Vec<String> {
    let mut messages = Vec::new();
    let mut notices = Vec::new();
    let after_bind = hosting_after_bind(workspace, store, agent, old_pane, &mut notices);
    let mut presenting = true;
    for notice in notices {
        let notice = format!("note: {notice}");
        let failed = if presenting {
            present(&notice).err()
        } else {
            None
        };
        messages.push(notice);
        if let Some(err) = failed {
            presenting = false;
            let err = anyhow::Error::from(err);
            let warning = format!("warning: the multiplexer's notices were not printed: {err:#}");
            let _ = present(&warning);
            messages.push(warning);
        }
    }
    let mut report = |warning: String| {
        let _ = present(&warning);
        messages.push(warning);
    };
    release_replaced(
        workspace,
        store,
        agent,
        old_pane,
        conversation_over,
        &mut report,
    );
    if let Some(warning) = close_replaced(workspace, backend, old_pane, after_bind) {
        report(warning);
    }
    messages
}

/// Withdraw what belonged to the session a swap replaced: its hibernation and
/// idle stop, and, once its conversation is over, its raised attention. The
/// agent's waits stay armed for the session that holds it now.
pub(super) fn release_replaced(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentState,
    old_pane: &PaneId,
    conversation_over: bool,
    report: &mut impl FnMut(String),
) {
    if let Err(err) = crate::harness::schedule::arm::release_replaced_session(
        &workspace.project_root,
        &agent.kind,
        &agent.agent_id,
        conversation_over,
    ) {
        report(format!(
            "warning: the replaced session's idle stop and hibernation were not withdrawn: {err}"
        ));
    }
    if let Err(err) = crate::harness::hibernate::wake_pane(store, old_pane) {
        let err = anyhow::Error::from(err);
        report(format!(
            "warning: the replaced session's pane {old_pane} was not woken from hibernation: {err:#}"
        ));
    }
}

/// Close the pane of the session a swap replaced, warning with the pane left open when the
/// multiplexer refuses. Nothing is closed, with a warning, when the read after the bind
/// still finds the agent in that pane or could not be made.
fn close_replaced(
    workspace: &ResolvedWorkspace,
    backend: &dyn MuxBackend,
    old_pane: &PaneId,
    after_bind: std::result::Result<Option<AgentPane>, HostingErr>,
) -> Option<String> {
    match after_bind {
        Ok(Some(pane)) if pane.pane_id == *old_pane => Some(format!(
            "warning: the agent is still in pane {old_pane} after its replacement bound; \
             the pane was not closed"
        )),
        Err(err) => Some(format!(
            "warning: the replaced session's pane {old_pane} was not closed: reading where \
             the agent is after its replacement bound: {err}"
        )),
        Ok(_) => backend
            .close_pane(&workspace.session_name, old_pane)
            .context("closing the replaced agent pane")
            .err()
            .map(|err| {
                format!("warning: the replaced session's pane {old_pane} is still open: {err:#}")
            }),
    }
}

/// The `@rimz` section a fresh session of a swapped agent starts with, listing
/// what is still outstanding for it, or `None` when nothing is. Its waits are
/// the ones the agent armed itself: a team binding or a loop row that targets
/// it stays armed and is not its own to tell. A molt's `note` goes first.
pub fn swap_briefing(
    store: &Store,
    agent: &AgentState,
    agents: &[AgentState],
    project_root: &Path,
    note: Option<&str>,
) -> Result<Option<String>> {
    let children = crate::address::launched_children(agents, agent)
        .into_iter()
        .filter(|child| !child.session_over())
        .map(handle)
        .collect::<Vec<_>>();
    let runs = crate::harness::run::list(store.paths()).map_err(anyhow::Error::from)?;
    // Runtime drops a subagent's ended children, so the answers they owe are
    // read over every retained row: each launch's holder, so a swapped
    // child's replaced session never stands in for the live one.
    let audit = store.audit_rows()?;
    let holders = crate::agents::state::LaunchHolders::new(audit.iter());
    let held = audit.iter().filter(|row| !holders.replaced(row));
    let owed = crate::harness::fleet::FleetRuns::of(held, &runs, agent)
        .unreported()
        .into_iter()
        .filter(|(child, _)| child.is_launched_child() && child.session_over())
        .map(|(child, _)| handle(child))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let catalog = crate::harness::schedule::catalog::TaskCatalog::load(Some(project_root))?;
    let rows = audit.iter().collect::<Vec<_>>();
    let waits = crate::harness::schedule::pending::session_deliveries(
        &catalog,
        project_root,
        Some(agent),
        &rows,
    )
    .filter(|(name, task)| {
        matches!(
            crate::harness::schedule::delivery_owner(name, task.entry()),
            crate::harness::schedule::DeliveryOwner::SelfWait
        )
    })
    .map(|(name, _)| name.to_owned())
    .collect::<Vec<_>>();
    let messages = store
        .list_messages()?
        .into_iter()
        .filter(|message| {
            message.sender.is_conversation() && agent.card_ref().matches(message.card_ref())
        })
        .collect::<Vec<_>>();
    let count = |status: crate::store::message::MessageStatus| {
        messages
            .iter()
            .filter(|message| message.status == status)
            .count()
    };
    let queued = count(crate::store::message::MessageStatus::Queued)
        + count(crate::store::message::MessageStatus::Claimed);
    let unconfirmed = count(crate::store::message::MessageStatus::Sent);
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
            plural(waits.len(), "wait", "waits"),
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

/// The agent as a message names it: its handle, or its session id when it has no name.
fn handle(agent: &AgentState) -> String {
    agent
        .name
        .as_ref()
        .map_or_else(|| agent.agent_id.to_string(), |name| format!("@{name}"))
}

/// The open workflow session run `agent` leads. A restart or molt hands it to the replacement,
/// so only the agent's holder session settles it, and a replaced session's exit never does.
pub fn open_session_run<'a>(runs: &'a [RunRecord], agent: &AgentState) -> Option<&'a RunRecord> {
    let attempt = agent.workflow.as_ref()?;
    runs.iter().find(|run| {
        run.lifecycle == crate::store::run::RunLifecycle::Session
            && !run.status.is_terminal()
            && run.workflow.as_ref() == Some(attempt)
            && run.agent_name.is_some()
            && run.agent_name == agent.name
    })
}

/// A relaunch request that carries `run` with its exit policy, as the resume doorway sets them.
pub fn with_run(request: ExecRequest, run: &RunRecord) -> ExecRequest {
    let (close_pane_on_exit, exit_on_run_completion) =
        crate::harness::plan::run_exit_policy(!run.keep);
    ExecRequest {
        run_id: Some(run.run_id.clone()),
        close_pane_on_exit,
        exit_on_run_completion,
        ..request
    }
}

/// The relaunch pane's identity env, naming the lane the relaunch carries.
pub fn relaunch_identity_env(
    workspace: &ResolvedWorkspace,
    cwd: &Path,
    request: &ExecRequest,
) -> std::collections::BTreeMap<String, String> {
    let channel = (request.identity.params.lane.as_ref()).and_then(crate::ids::Lane::pin_name);
    crate::room::pane_identity_env(
        workspace,
        cwd,
        channel.as_ref().map(crate::ids::LaneName::as_str),
    )
}

/// Build the provider request from a replayed posture and the reserved identity, if any.
pub fn relaunch_request(
    agent: &AgentState,
    posture: &ResumePosture,
    action: ExecAction,
    login: Option<crate::ids::LoginName>,
    fresh_identity: Option<&AgentLaunchIdentity>,
) -> ExecRequest {
    let identity_name = fresh_identity.map_or(agent.name.as_deref(), |identity| {
        Some(identity.name.as_str())
    });
    let restart_params = crate::agents::LaunchParams {
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
        code_mode: posture.launch.code_mode,
        budget: posture.launch.budget.clone(),
        epoch: fresh_identity.map_or((agent.epoch > 0).then_some(agent.epoch), |identity| {
            identity.launch.epoch
        }),
        kind_ordinal: None,
    };
    ExecRequest {
        close_pane_on_exit: true,
        subagent: agent.is_launched_child(),
        identity: crate::harness::launch::ExecIdentity {
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
/// cannot receive refuse here rather than degrading. Every other degrade warns
/// and continues.
pub fn restart_posture(
    agent: &AgentState,
    workspace: &ResolvedWorkspace,
    machine_config: &crate::config::MachineConfig,
) -> Result<(ResumePosture, Option<String>)> {
    let launch = crate::config::effective::load(machine_config, &workspace.project_root)?;
    launch.block_set_failure()?;
    let scope = ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child());
    let profiles = launch.profiles_for_launch(scope, agent.profile.as_deref());
    let fallback = (scope == ProfileScope::Subagents).then_some(&launch.profiles);
    launch.block_failed_launch(
        &agent.kind,
        agent.profile.as_deref(),
        agent.record.as_deref(),
        profiles,
        fallback,
    )?;
    let posture = member_posture(agent, profiles, fallback, &launch.teams);
    let warning = match &posture.degraded {
        Some(reason @ PostureDegrade::KindChanged { .. }) => {
            return Err(SwapErr::Refused(format!(
                "{reason}; launch it fresh to change providers (rimz agents <profile> --agent <kind>)"
            )));
        }
        Some(
            reason @ (PostureDegrade::PromptUnsupported { .. }
            | PostureDegrade::TeamPromptRejected { .. }),
        ) => return Err(SwapErr::Refused(reason.to_string())),
        Some(reason) => Some(format!("rimz: {reason}; launching as bare {}", agent.kind)),
        None => None,
    };
    Ok((posture, warning))
}

/// Replay the recorded member posture without applying restart's refusal policy.
pub fn member_posture(
    agent: &AgentState,
    profiles: &crate::config::ProfilesConfig,
    fallback: Option<&crate::config::ProfilesConfig>,
    teams: &crate::config::TeamsConfig,
) -> ResumePosture {
    crate::harness::resume::resolve_member_posture(
        crate::harness::resume::PostureRequest {
            record: agent.record.as_deref(),
            profile: agent.profile.as_deref(),
            kind: &agent.kind,
            stamped_mode: agent.mode,
            stamped_tier: agent.tier.as_deref(),
        },
        profiles,
        fallback,
        teams,
        agent.team.as_deref(),
        agent.role.as_deref(),
    )
}

/// The layout cell a fresh restart launches, carrying the replayed posture and
/// the agent's durable identity.
pub fn restart_cell(agent: &AgentState, posture: &ResumePosture) -> Cell {
    Cell::Agent(AgentCell {
        notices: posture.launch.notices.clone(),
        empty_dir: None,
        timeout: None,
        resume_model_override: false,
        kind: agent.kind.clone(),
        args: posture.launch.args.clone(),
        auto_compact: posture.launch.auto_compact.clone(),
        tuning: posture.launch.tuning.clone(),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: posture.launch.skills.clone(),
        seat_consensus_skill: posture.launch.seat_consensus_skill,
        mcps: posture.launch.mcps.clone(),
        no_skill_tool: posture.launch.no_skill_tool,
        rimz_capabilities: posture.launch.rimz_capabilities.clone(),
        allowed_tools: posture.launch.allowed_tools.clone(),
        tools: posture.launch.tools.clone(),
        isolation_default: posture.launch.isolation_default,
        launch: crate::agents::LaunchParams {
            automation: agent.automation.clone(),
            checkout: agent.checkout.clone(),
            add_dirs: agent.add_dirs.clone(),
            untrusted_checkout: agent.untrusted_checkout,
            profile: agent.profile.clone(),
            tier: posture.launch.tier.clone(),
            record: Some(crate::agents::LaunchRecord::replay_or_new(
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
            code_mode: posture.launch.code_mode,
            budget: posture.launch.budget.clone(),
            ..Default::default()
        },
    })
}

/// Copy the agent's durable seat and lineage into a fresh-launch reservation request.
pub fn fresh_launch_request(
    agent: &AgentState,
    cell: Cell,
    mode: Option<PermissionMode>,
) -> Result<AgentLaunchRequest> {
    let layout = LayoutSpec::single(cell);
    let mut requests = crate::harness::plan::launch_identity_requests(
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
        .ok_or_else(|| SwapErr::Refused("restart produced no fresh launch request".into()))?;
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

#[cfg(test)]
mod tests;
