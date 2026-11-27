//! The parent-only resume entry for a message to an ended child: the caller's own child,
//! ended, is relaunched on its session and the message parks for it. `rimz message` and the
//! MCP `message` tool both reach it on an address that names no live agent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::relaunch::relaunch_action;
use super::stop::newest_run_for_child;
use super::swap::{relaunch_identity_env, relaunch_request, with_run};
use super::trust::Unattended;
use super::{Interaction, Notice};
use crate::agents::AgentState;
use crate::config::effective::ProfileScope;
use crate::harness::launch::{ExecAction, ExecRequest};
use crate::harness::resume::{PostureRequest, ResumePosture};
use crate::message::dispatch::{
    self, DispatchErr, DispatchOutcome, DispatchRequest, DispatchResult, ParkReason,
};
use crate::store::Store;
use crate::store::run::RunRecord;
use crate::store::writer::ResumeAttempt;
use crate::workspace::ResolvedWorkspace;

fn ended_child<'a>(
    agents: &'a [AgentState],
    caller: &AgentState,
    target: &str,
    scope: Option<&str>,
    channel: &crate::address::AddressContext,
) -> Option<&'a AgentState> {
    let children = crate::address::launched_children(agents, caller);
    crate::address::resolve_agent(target, scope, channel, &children)
        .ok()
        .filter(|child| child.ended_at.is_some())
}

pub enum ResumeChildOutcome {
    NotApplicable,
    AlreadyBound,
    Dispatched(DispatchResult),
    DispatchFailed(DispatchErr),
    DeliveryFailed(anyhow::Error),
    /// The child failed on a provider limit that has not refilled yet: nothing was queued.
    LimitRefusal(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReopenChildOutcome {
    Reopened,
    AlreadyBound,
}

/// Reopen an ended launched child into its live parent's subagent zone without
/// queuing a message. Parent mode, isolation, and pane come from durable records
/// rather than the calling process; trust preflights run attended for a human
/// caller and unattended when an agent calls.
pub fn reopen_child(
    workspace: &ResolvedWorkspace,
    store: &Store,
    child: &AgentState,
    caller_is_agent: bool,
    interaction: &mut impl Interaction,
) -> Result<ReopenChildOutcome> {
    let name = child.name.as_deref().unwrap_or(child.agent_id.as_str());
    let machine = crate::config::MachineConfig::load_lenient();
    let ambient = crate::agents::ambient_env();
    reopen_resolved(
        workspace,
        store,
        child,
        caller_is_agent,
        machine,
        &ambient,
        None,
        crate::launcher::placement::zone::SUBAGENT_REOPEN_BIND_TIMEOUT,
        interaction,
    )
    .map_err(|error| anyhow::anyhow!("cannot reopen @{name}: {error:#}"))
}

#[cfg(test)]
#[expect(
    clippy::too_many_arguments,
    reason = "test seam injects machine, env, backend, and timeout"
)]
fn reopen_child_with(
    workspace: &ResolvedWorkspace,
    store: &Store,
    child: &AgentState,
    caller_is_agent: bool,
    machine: crate::config::MachineConfig,
    ambient: &BTreeMap<String, String>,
    backend: &dyn crate::mux::MuxBackend,
    bind_timeout: Duration,
    interaction: &mut impl Interaction,
) -> Result<ReopenChildOutcome> {
    let name = child.name.as_deref().unwrap_or(child.agent_id.as_str());
    reopen_resolved(
        workspace,
        store,
        child,
        caller_is_agent,
        std::sync::Arc::new(machine),
        ambient,
        Some(backend),
        bind_timeout,
        interaction,
    )
    .map_err(|error| anyhow::anyhow!("cannot reopen @{name}: {error:#}"))
}

pub fn resume_child(
    workspace: &ResolvedWorkspace,
    store: &Store,
    target: &str,
    scope: Option<&str>,
    channel: &crate::address::AddressContext,
    request: DispatchRequest,
    interaction: &mut impl Interaction,
) -> Result<ResumeChildOutcome> {
    let audit = store.runtime_projection(crate::RuntimeScope::Audit)?;
    let Ok(caller) = crate::harness::ancestry::resolve_calling_agent(&audit.agents) else {
        return Ok(ResumeChildOutcome::NotApplicable);
    };
    let Some(child) = ended_child(&audit.agents, caller, target, scope, channel) else {
        return Ok(ResumeChildOutcome::NotApplicable);
    };
    let resume_context = || format!("cannot resume {target}");
    let outcome = resume_resolved(workspace, store, child, caller, request, interaction)
        .with_context(resume_context)?;
    Ok(match outcome {
        ResumeChildOutcome::DeliveryFailed(error) => {
            ResumeChildOutcome::DeliveryFailed(error.context(resume_context()))
        }
        outcome => outcome,
    })
}

/// Refuses a message to a child whose current answer failed on a provider limit while the
/// account a resume would run on still reads a refill ahead. On another account only its usage
/// cache's spent window refuses; on the child's own, see [`same_login_refill`].
fn limit_refusal(
    store: &crate::store::Store,
    child: &AgentState,
    run: &RunRecord,
    logins: &crate::agents::RoomAccounts,
    catalog: &crate::agents::LoginCatalog,
) -> Option<String> {
    let failure = run.limit_failure.as_ref()?;
    let runtime = store.runtime_paths();
    let now = jiff::Timestamp::now();
    let refill = match crate::harness::resume::resume_login(child, Some(logins), catalog) {
        crate::harness::resume::ResumeLogin::Same => {
            same_login_refill(runtime, child, failure, run.completed_at, now)
        }
        crate::harness::resume::ResumeLogin::Changed(key) => {
            crate::harness::park_notice::refill_ahead(runtime, &key, child.model.as_deref(), now)
        }
        crate::harness::resume::ResumeLogin::Unreadable => return None,
    }?;
    let name = child
        .name
        .as_deref()
        .or(run.agent_name.as_deref())
        .unwrap_or_else(|| child.agent_id.as_str());
    let minutes = u64::try_from(refill.duration_since(now).as_secs())
        .unwrap_or(0)
        .div_ceil(60)
        // Under a second before the refill the ceiling is 0, and the delay parser refuses `0m`.
        .max(1);
    Some(format!(
        "not sent: @{name}'s provider limit refills in {}. Run `rimz wait --in {minutes}m`, \
         then send it again.",
        crate::theme::fmt::refill_in(refill, now, &jiff::tz::TimeZone::system()),
    ))
}

/// The refill ahead on the child's own login for a limit failure settled at `failed_at`: a refill
/// the child's limit reading names counts until it, the later of it and a stated refill the
/// failure recorded; otherwise an identity switch on the login since the failure reads none, and
/// so does quota available in a usage read newer than the failure, unless the recorded refill is
/// the one the provider's stop stated (`LimitFailure::refill_stated`: such a provider can read
/// below 100% while its quota is spent); otherwise the recorded refill counts while it is ahead,
/// since a stop that names its refill leaves no spent window behind. Any provider's recorded
/// refill counts.
fn same_login_refill(
    runtime: &crate::RuntimePaths,
    child: &AgentState,
    failure: &crate::store::run::LimitFailure,
    failed_at: Option<jiff::Timestamp>,
    now: jiff::Timestamp,
) -> Option<jiff::Timestamp> {
    use crate::harness::park_notice::LimitReading;

    let since = failed_at.unwrap_or(now);
    let recorded = failure.refill_at.filter(|at| *at > now);
    match crate::harness::park_notice::limit_reading(runtime, child, now, since) {
        LimitReading::Refill(refill) => {
            Some(refill).max(recorded.filter(|_| failure.refill_stated))
        }
        LimitReading::Refilled if !failure.refill_stated => None,
        _ if identity_switched_since(runtime, &child.login_key(), since) => None,
        _ => recorded,
    }
}

/// Whether `key`'s login last switched identity or logged out after `since`.
fn identity_switched_since(
    runtime: &crate::RuntimePaths,
    key: &crate::ids::LoginKey,
    since: jiff::Timestamp,
) -> bool {
    crate::agents::account::read_accounts_cache(&runtime.shared_accounts_path())
        .logins
        .get(key)
        .and_then(|record| record.stamps.floor_at_ms)
        .and_then(|floor| i64::try_from(floor).ok())
        .is_some_and(|floor| floor > since.as_millisecond())
}

fn child_posture(
    child: &AgentState,
    launch: &crate::config::effective::LaunchAgents,
) -> crate::harness::resume::ResumePosture {
    let scope = ProfileScope::for_launch(child.profile_scope, child.is_launched_child());
    let fallback = (scope == ProfileScope::Subagents).then_some(&launch.profiles);
    crate::harness::resume::resolve_posture(
        PostureRequest {
            record: child.record.as_deref(),
            profile: child.profile.as_deref(),
            kind: &child.kind,
            stamped_mode: child.mode,
            stamped_tier: child.tier.as_deref(),
        },
        launch.profiles_for_launch(scope, child.profile.as_deref()),
        fallback,
    )
}

struct PreparedChildResume {
    cwd: PathBuf,
    posture: ResumePosture,
    capped_isolation: Option<crate::config::Isolation>,
    logins: crate::agents::RoomAccounts,
    catalog: crate::agents::LoginCatalog,
    action: ExecAction,
    login: Option<crate::ids::LoginName>,
    run: RunRecord,
    request: ExecRequest,
    launch_id: crate::ids::AgentSessionId,
    parent_pane_id: crate::ids::PaneId,
    parent_workspace: ResolvedWorkspace,
    room: crate::room::RoomContext,
    env: BTreeMap<String, String>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared preflight contract for message resume and promptless reopen"
)]
fn prepare_child_resume(
    workspace: &ResolvedWorkspace,
    store: &Store,
    child: &AgentState,
    parent: &AgentState,
    parent_isolation: Option<crate::config::Isolation>,
    unattended: Option<Unattended>,
    machine: std::sync::Arc<crate::config::MachineConfig>,
    ambient: &BTreeMap<String, String>,
    interaction: &mut impl Interaction,
) -> Result<PreparedChildResume> {
    let cwd = child
        .worktree_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.worktree_root.clone());
    if child.checkout.is_some() {
        crate::agents::empty_dir::prepare_reentry(store.paths(), &cwd)?;
    } else if !cwd.is_dir() {
        bail!("its worktree {} is gone", cwd.display());
    }
    let launch = crate::config::effective::load(&machine, &workspace.project_root)?;
    launch.block_set_failure()?;
    let scope = ProfileScope::for_launch(child.profile_scope, child.is_launched_child());
    launch.block_failed_launch(
        &child.kind,
        child.profile.as_deref(),
        child.record.as_deref(),
        scope,
    )?;
    let posture = child_posture(child, &launch);
    if let Some(reason) = &posture.degraded {
        bail!("{reason}");
    }
    crate::harness::plan::check_child_permission_mode(
        &child.kind,
        parent.mode,
        posture.launch.mode,
    )?;
    let adapter = crate::agents::find_definition(child.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", child.kind))?;
    let capped = crate::harness::plan::cap_child_isolation(
        parent_isolation,
        child.isolation,
        posture.launch.isolation_default,
        machine.agents.isolation,
    )?;
    if let Some(source) = &capped.clamped {
        interaction.notice(Notice::IsolationClamp {
            profile: child.profile.as_deref().unwrap_or(&child.kind),
            source,
        })?;
    }
    let isolation = crate::config::Isolation::resolve(
        capped.isolation,
        posture.launch.isolation_default,
        machine.agents.isolation,
    );
    crate::sandbox::preflight_launch(
        isolation,
        &child.kind,
        posture.launch.skills.is_some(),
        adapter.manual_skill(),
    )?;
    crate::harness::launch::preflight_agent_kind(
        &workspace.project_root,
        child.kind.as_str(),
        &cwd,
    )?;
    let logins = crate::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &machine,
    )?;
    let catalog = crate::agents::machine_login_catalog();
    let (action, login, fresh_reason) = relaunch_action(child, &logins, &catalog, &cwd)?;
    if let Some(reason) = fresh_reason {
        bail!("{reason}");
    }
    let provider_login =
        crate::agents::session_login(&child.kind, login.as_ref(), &machine.accounts)?;
    if let Some(notice) = super::trust::preflight_hooks(
        provider_login.health(ambient),
        || unattended,
        || crate::config::definitions::hook_trust_fix(&child.kind, child.profile.as_deref(), None),
        &mut Default::default(),
    )? {
        interaction.notice(Notice::Trust(&notice))?;
    }
    let repo_root = if child.checkout.is_none() {
        workspace.at_launch_dir(&cwd)?.cwd_project_root
    } else {
        None
    };
    if let Some(unattended) = unattended {
        if child.checkout.is_some()
            && let Some(notice) = super::trust::preflight(
                child.kind.as_str(),
                &cwd,
                adapter.folder_trust(&cwd, None, &provider_login.env(ambient)),
                Some(unattended),
            )?
        {
            interaction.notice(Notice::Trust(&notice))?;
        }
        match crate::agents::preflight_launch_dir(
            adapter,
            &cwd,
            repo_root.as_deref(),
            &provider_login.env(ambient),
            crate::agents::LaunchDirMode::Supervised,
        ) {
            Ok(()) => {}
            Err(error @ crate::agents::LaunchDirRefusal::Home { .. }) => bail!(
                "supervised resume refused: {error}; the child cannot move, so launch a new child with `--cwd <DIR>`"
            ),
            Err(error) => return Err(error).context("supervised resume refused"),
        }
    } else if let Some(notice) = super::trust::preflight(
        child.kind.as_str(),
        &cwd,
        adapter.folder_trust(&cwd, repo_root.as_deref(), &provider_login.env(ambient)),
        None,
    )? {
        interaction.notice(Notice::Trust(&notice))?;
    }
    let runs = crate::harness::run::list(store.paths())?;
    let run = newest_run_for_child(&runs, child)
        .ok_or_else(|| anyhow::anyhow!("no supervised run recorded"))?
        .clone();
    let mut request = resume_request(child, &run, &posture, action.clone(), login.clone(), None);
    request.identity.params.isolation = capped.isolation;
    if !isolation.is_scoped() {
        crate::harness::launch::preflight_agent_process(
            &workspace.project_root,
            &request,
            &cwd,
            Some(store.runtime_paths()),
        )?;
    }
    let launch_id = child
        .launch_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no launch identity recorded"))?;
    let parent_pane = parent.pane.as_ref().ok_or_else(|| {
        let parent_name = parent.name.as_deref().unwrap_or(parent.agent_id.as_str());
        anyhow::anyhow!("parent @{parent_name} has no bound pane")
    })?;
    let mut parent_workspace = workspace.clone();
    if !parent_pane.session_name.is_empty() {
        parent_workspace
            .session_name
            .clone_from(&parent_pane.session_name);
    }
    let room = crate::room::RoomContext::from_resolved(
        &parent_workspace,
        machine,
        parent_pane.pane_id.mux(),
        crate::room::RoomSizing::OrdinaryTab,
    )?;
    let env = relaunch_identity_env(
        &parent_workspace,
        child.lane_path().map(Path::new).unwrap_or(&cwd),
        &request,
    );
    Ok(PreparedChildResume {
        cwd,
        posture,
        capped_isolation: capped.isolation,
        logins,
        catalog,
        action,
        login,
        run,
        request,
        launch_id,
        parent_pane_id: parent_pane.pane_id.clone(),
        parent_workspace,
        room,
        env,
    })
}

fn child_already_bound(
    agents: &[AgentState],
    child: &AgentState,
    launch_id: &crate::ids::AgentSessionId,
) -> bool {
    crate::launcher::placement::launch_has_bound_pane(agents, &child.kind, launch_id)
        || agents
            .iter()
            .any(|agent| agent.ended_at.is_none() && child.card_ref().matches(agent.card_ref()))
}

fn child_pane_cmd(
    store: &Store,
    request: &ExecRequest,
    agents: &[AgentState],
) -> Result<crate::mux::PaneCmd> {
    Ok(crate::mux::PaneCmd {
        argv: crate::harness::launch::exec_argv(
            &crate::proc::rimz_exe(),
            store.runtime_paths(),
            request,
        )?,
        name: Some(crate::agents::pane_title::title(
            crate::agents::pane_title::Seat::from_launch(
                &request.kind,
                request.identity.name.as_deref(),
                &request.identity.params,
            ),
            agents,
        )),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "shared implementation for production reopen and backend-injected unit tests"
)]
fn reopen_resolved(
    workspace: &ResolvedWorkspace,
    store: &Store,
    child: &AgentState,
    caller_is_agent: bool,
    machine: std::sync::Arc<crate::config::MachineConfig>,
    ambient: &BTreeMap<String, String>,
    backend_override: Option<&dyn crate::mux::MuxBackend>,
    bind_timeout: Duration,
    interaction: &mut impl Interaction,
) -> Result<ReopenChildOutcome> {
    let audit = store.runtime_projection(crate::RuntimeScope::Audit)?;
    let Some(parent) = crate::address::launched_parent(&audit.agents, child) else {
        bail!("parent session is gone");
    };
    let parent_name = parent.name.as_deref().unwrap_or(parent.agent_id.as_str());
    if parent.ended_at.is_some() {
        bail!("parent @{parent_name} has ended");
    }
    if parent.pane.is_none() {
        bail!("parent @{parent_name} has no bound pane");
    }
    let parent_isolation = Some(parent.runs_in(machine.agents.isolation));
    let unattended = caller_is_agent.then_some(Unattended::Subagent);
    let prepared = prepare_child_resume(
        workspace,
        store,
        child,
        parent,
        parent_isolation,
        unattended,
        machine,
        ambient,
        interaction,
    )?;
    let _guard = crate::launcher::placement::lock_subagent_zone(store)?;
    let current = store.runtime_projection(crate::RuntimeScope::Audit)?;
    if child_already_bound(&current.agents, child, &prepared.launch_id) {
        return Ok(ReopenChildOutcome::AlreadyBound);
    }
    let pane = child_pane_cmd(store, &prepared.request, &current.agents)?;
    let sidebar = prepared.room.sidebar_options(&prepared.cwd);
    let backend = backend_override.unwrap_or_else(|| prepared.room.backend());
    let live_panes = crate::launcher::placement::zone::list_subagent_zone_panes(
        backend,
        store,
        &prepared.parent_workspace,
    )
    .with_context(|| format!("checking parent @{parent_name}'s pane"))?;
    if !live_panes
        .panes
        .iter()
        .any(|live| live.pane_id == prepared.parent_pane_id)
    {
        bail!("parent @{parent_name} has no live pane");
    }
    let open_started = std::time::Instant::now();
    match crate::launcher::placement::split_into_subagent_zone(
        backend,
        store,
        &prepared.parent_workspace,
        &prepared.cwd,
        prepared.env,
        sidebar,
        &pane,
        prepared
            .request
            .identity
            .params
            .lane
            .as_ref()
            .and_then(crate::ids::Lane::pin_name)
            .as_ref()
            .map(crate::ids::LaneName::as_str),
        Some(prepared.parent_pane_id),
        Some((&parent.kind, parent.launch_key())),
        &prepared.run.run_id,
    ) {
        crate::launcher::placement::SubagentZoneOpen::Opened => {}
        crate::launcher::placement::SubagentZoneOpen::Failed(error) => return Err(error.into()),
    }
    let open = open_started.elapsed();
    let bound = crate::launcher::placement::zone::wait_for_subagent_pane_bind_for(
        store,
        &prepared.parent_workspace,
        crate::launcher::placement::PaneBindWait {
            run_id: &prepared.run.run_id,
            launch_id: &prepared.launch_id,
            kind: &child.kind,
            caller: crate::diag::record::SubagentPaneBindCaller::Resume,
            open,
        },
        bind_timeout,
    );
    if !bound {
        bail!(
            "reopened pane did not bind within {}s",
            bind_timeout.as_secs().max(1)
        );
    }
    Ok(ReopenChildOutcome::Reopened)
}

fn resume_resolved(
    workspace: &ResolvedWorkspace,
    store: &Store,
    child: &AgentState,
    caller: &AgentState,
    mut dispatch_request: DispatchRequest,
    interaction: &mut impl Interaction,
) -> Result<ResumeChildOutcome> {
    let machine = crate::config::MachineConfig::load_lenient();
    let ambient = crate::agents::ambient_env();
    let prepared = prepare_child_resume(
        workspace,
        store,
        child,
        caller,
        crate::config::Isolation::ambient(&ambient),
        Some(Unattended::Subagent),
        machine,
        &ambient,
        interaction,
    )?;
    let _guard = crate::launcher::placement::lock_subagent_zone(store)?;
    let current = store.runtime_projection(crate::RuntimeScope::Audit)?;
    if child_already_bound(&current.agents, child, &prepared.launch_id) {
        return Ok(ResumeChildOutcome::AlreadyBound);
    }
    if let Some(refusal) = limit_refusal(
        store,
        child,
        &prepared.run,
        &prepared.logins,
        &prepared.catalog,
    ) {
        return Ok(ResumeChildOutcome::LimitRefusal(refusal));
    }
    dispatch_request.resuming_recipient = Some(child.clone());
    let result = match dispatch::dispatch(&prepared.parent_workspace, store, dispatch_request) {
        Ok(result) => result,
        Err(DispatchErr::RecipientBecameLive) => return Ok(ResumeChildOutcome::AlreadyBound),
        Err(error) => return Ok(ResumeChildOutcome::DispatchFailed(error)),
    };
    let [
        DispatchOutcome::Queued {
            message_id,
            reason: Some(ParkReason::Resuming { attempt, .. }),
            ..
        },
    ] = result.outcomes.as_slice()
    else {
        unreachable!("one resuming recipient always parks one record");
    };
    if *attempt == ResumeAttempt::Joined {
        return Ok(ResumeChildOutcome::Dispatched(result));
    }
    let opened = (|| -> Result<()> {
        let mut request = resume_request(
            child,
            &prepared.run,
            &prepared.posture,
            prepared.action,
            prepared.login,
            Some(message_id.clone()),
        );
        request.identity.params.isolation = prepared.capped_isolation;
        let pane = child_pane_cmd(store, &request, &current.agents)?;
        let sidebar = prepared.room.sidebar_options(&prepared.cwd);
        let open_started = std::time::Instant::now();
        match crate::launcher::placement::split_into_subagent_zone(
            prepared.room.backend(),
            store,
            &prepared.parent_workspace,
            &prepared.cwd,
            prepared.env,
            sidebar,
            &pane,
            request
                .identity
                .params
                .lane
                .as_ref()
                .and_then(crate::ids::Lane::pin_name)
                .as_ref()
                .map(crate::ids::LaneName::as_str),
            Some(prepared.parent_pane_id),
            Some((&caller.kind, caller.launch_key())),
            &prepared.run.run_id,
        ) {
            crate::launcher::placement::SubagentZoneOpen::Opened => {}
            crate::launcher::placement::SubagentZoneOpen::Failed(error) => return Err(error.into()),
        }
        let open = open_started.elapsed();
        crate::launcher::placement::wait_for_subagent_pane_bind(
            store,
            &prepared.parent_workspace,
            crate::launcher::placement::PaneBindWait {
                run_id: &prepared.run.run_id,
                launch_id: &prepared.launch_id,
                kind: &child.kind,
                caller: crate::diag::record::SubagentPaneBindCaller::Resume,
                open,
            },
        );
        Ok(())
    })();
    if let Err(error) = opened {
        store.record_unheld_delivery_miss(
            message_id,
            crate::store::writer::DeliveryFailureDisposition::Terminal,
            &format!("{error:#}"),
            &prepared.parent_workspace.session_name,
        )?;
        return Ok(ResumeChildOutcome::DeliveryFailed(error));
    }
    Ok(ResumeChildOutcome::Dispatched(result))
}

fn resume_request(
    child: &AgentState,
    run: &RunRecord,
    posture: &ResumePosture,
    action: ExecAction,
    login: Option<crate::ids::LoginName>,
    follow_up_message: Option<crate::ids::MessageId>,
) -> ExecRequest {
    let mut request = relaunch_request(child, posture, action, login, None);
    request.identity.name_explicit = true;
    ExecRequest {
        follow_up_message,
        ..with_run(request, run)
    }
}

#[cfg(test)]
#[path = "child_resume_tests.rs"]
mod tests;
