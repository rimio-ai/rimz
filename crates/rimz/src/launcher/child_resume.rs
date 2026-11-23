//! The parent-only resume entry for a message to an ended child: the caller's own child,
//! ended, is relaunched on its session and the message parks for it. `rimz message` and the
//! MCP `message` tool both reach it on an address that names no live agent.

use std::path::{Path, PathBuf};

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
/// account a resume would run on still reads a refill ahead.
fn limit_refusal(
    store: &crate::store::Store,
    child: &AgentState,
    run: &RunRecord,
    logins: &crate::agents::RoomAccounts,
    catalog: &crate::agents::LoginCatalog,
) -> Option<String> {
    run.limit_failure.as_ref()?;
    let key = match crate::harness::resume::resume_login(child, Some(logins), catalog) {
        crate::harness::resume::ResumeLogin::Same => child.login_key(),
        crate::harness::resume::ResumeLogin::Changed(key) => key,
        crate::harness::resume::ResumeLogin::Unreadable => return None,
    };
    let now = jiff::Timestamp::now();
    let refill = crate::harness::park_notice::refill_ahead(
        store.runtime_paths(),
        &key,
        child.model.as_deref(),
        now,
    )?;
    let name = child
        .name
        .as_deref()
        .or(run.agent_name.as_deref())
        .unwrap_or_else(|| child.agent_id.as_str());
    let minutes = u64::try_from(refill.duration_since(now).as_secs())
        .unwrap_or(0)
        .div_ceil(60);
    Some(format!(
        "not sent: @{name}'s provider limit refills in {}. Run `rimz wait --in {minutes}m`, \
         then send it again.",
        crate::theme::fmt::refill_in(refill, now, &jiff::tz::TimeZone::system()),
    ))
}

fn resume_resolved(
    workspace: &ResolvedWorkspace,
    store: &Store,
    child: &AgentState,
    caller: &AgentState,
    mut dispatch_request: DispatchRequest,
    interaction: &mut impl Interaction,
) -> Result<ResumeChildOutcome> {
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
    let machine = crate::config::MachineConfig::load_lenient();
    let launch = crate::config::effective::load(&machine, &workspace.project_root)?;
    launch.block_set_failure()?;
    let posture = crate::harness::resume::resolve_posture(
        PostureRequest {
            record: child.record.as_deref(),
            profile: child.profile.as_deref(),
            kind: &child.kind,
            stamped_mode: child.mode,
            stamped_tier: child.tier.as_deref(),
        },
        launch.profiles_for_launch(
            ProfileScope::for_launch(child.profile_scope, child.is_launched_child()),
            child.profile.as_deref(),
        ),
    );
    if let Some(reason) = &posture.degraded {
        bail!("{reason}");
    }
    crate::harness::plan::check_child_permission_mode(
        &child.kind,
        caller.mode,
        posture.launch.mode,
    )?;
    let adapter = crate::agents::find_definition(child.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", child.kind))?;
    let capped = crate::harness::plan::cap_child_isolation(
        crate::config::Isolation::ambient(&crate::agents::ambient_env()),
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
        &crate::config::MachineConfig::load_lenient(),
    )?;
    let catalog = crate::agents::machine_login_catalog();
    let (action, login, fresh_reason) = relaunch_action(child, &logins, &catalog, &cwd)?;
    if let Some(reason) = fresh_reason {
        bail!("{reason}");
    }
    let provider_login =
        crate::agents::session_login(&child.kind, login.as_ref(), &machine.accounts)?;
    if let Some(notice) = super::trust::preflight_hooks(
        provider_login.health(&crate::agents::ambient_env()),
        || Some(Unattended::Subagent),
        || crate::config::definitions::hook_trust_fix(&child.kind, child.profile.as_deref(), None),
        &mut Default::default(),
    )? {
        interaction.notice(Notice::Trust(&notice))?;
    }
    if child.checkout.is_some()
        && let Some(notice) = super::trust::preflight(
            child.kind.as_str(),
            &cwd,
            adapter.folder_trust(
                &cwd,
                None,
                &provider_login.env(&crate::agents::ambient_env()),
            ),
            Some(Unattended::Subagent),
        )?
    {
        interaction.notice(Notice::Trust(&notice))?;
    }
    let repo_root = if child.checkout.is_none() {
        workspace.at_launch_dir(&cwd)?.cwd_project_root
    } else {
        None
    };
    match crate::agents::preflight_launch_dir(
        adapter,
        &cwd,
        repo_root.as_deref(),
        &provider_login.env(&crate::agents::ambient_env()),
        crate::agents::LaunchDirMode::Supervised,
    ) {
        Ok(()) => {}
        Err(error @ crate::agents::LaunchDirRefusal::Home { .. }) => bail!(
            "supervised resume refused: {error}; the child cannot move, so launch a new child with `--cwd <DIR>`"
        ),
        Err(error) => return Err(error).context("supervised resume refused"),
    }
    let runs = crate::harness::run::list(store.paths())?;
    let run = newest_run_for_child(&runs, child)
        .ok_or_else(|| anyhow::anyhow!("no supervised run recorded"))?;
    let mut request = resume_request(child, run, &posture, action.clone(), login.clone(), None);
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
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no launch identity recorded"))?;
    let parent_pane = caller
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("parent has no bound pane"))?;
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
    let _guard = crate::launcher::placement::lock_subagent_zone(store)?;
    let current = store.runtime_projection(crate::RuntimeScope::Audit)?;
    if crate::launcher::placement::launch_has_bound_pane(&current.agents, &child.kind, launch_id)
        || current
            .agents
            .iter()
            .any(|agent| agent.ended_at.is_none() && child.card_ref().matches(agent.card_ref()))
    {
        return Ok(ResumeChildOutcome::AlreadyBound);
    }
    if let Some(refusal) = limit_refusal(store, child, run, &logins, &catalog) {
        return Ok(ResumeChildOutcome::LimitRefusal(refusal));
    }
    dispatch_request.resuming_recipient = Some(child.clone());
    let result = match dispatch::dispatch(&parent_workspace, store, dispatch_request) {
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
            run,
            &posture,
            action,
            login,
            Some(message_id.clone()),
        );
        request.identity.params.isolation = capped.isolation;
        let pane = crate::mux::PaneCmd {
            argv: crate::harness::launch::exec_argv(
                &crate::proc::rimz_exe(),
                store.runtime_paths(),
                &request,
            )?,
            name: Some(crate::agents::pane_title::title(
                crate::agents::pane_title::Seat::from_launch(
                    &request.kind,
                    request.identity.name.as_deref(),
                    &request.identity.params,
                ),
                &current.agents,
            )),
        };
        let sidebar = room.sidebar_options(&cwd);
        let open_started = std::time::Instant::now();
        match crate::launcher::placement::split_into_subagent_zone(
            room.backend(),
            store,
            &parent_workspace,
            &cwd,
            env,
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
            Some(parent_pane.pane_id.clone()),
            &run.run_id,
        ) {
            crate::launcher::placement::SubagentZoneOpen::Opened => {}
            crate::launcher::placement::SubagentZoneOpen::Failed(error) => return Err(error.into()),
        }
        let open = open_started.elapsed();
        crate::launcher::placement::wait_for_subagent_pane_bind(
            store,
            &parent_workspace,
            crate::launcher::placement::PaneBindWait {
                run_id: &run.run_id,
                launch_id,
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
            &parent_workspace.session_name,
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
