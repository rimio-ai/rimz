//! Parent-only resume doorway for a message to an ended child.

use super::*;
use crate::cli::supervised;
use rimz::harness::launch::{ExecAction, ExecRequest};
use rimz::harness::resume::{PostureRequest, ResumePosture};
use rimz::store::run::RunRecord;

fn ended_child<'a>(
    agents: &'a [AgentState],
    caller: &AgentState,
    target: &str,
    scope: Option<&str>,
    channel: &rimz::address::AddressContext,
) -> Option<&'a AgentState> {
    let children = rimz::address::launched_children(agents, caller);
    rimz::address::resolve_agent(target, scope, channel, &children)
        .ok()
        .filter(|child| child.ended_at.is_some())
}

pub(in crate::cli) fn resume_child(
    ctx: &Ctx,
    target: &str,
    scope: Option<&str>,
    channel: &rimz::address::AddressContext,
) -> Result<bool> {
    let audit = ctx.store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let Ok(caller) = rimz::harness::ancestry::resolve_calling_agent(&audit.agents) else {
        return Ok(false);
    };
    let Some(child) = ended_child(&audit.agents, caller, target, scope, channel) else {
        return Ok(false);
    };
    resume_resolved(ctx, child, caller).with_context(|| format!("cannot resume {target}"))?;
    Ok(true)
}

fn resume_resolved(ctx: &Ctx, child: &AgentState, caller: &AgentState) -> Result<()> {
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let cwd = child
        .worktree_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.worktree_root.clone());
    if !cwd.is_dir() {
        bail!("its worktree {} is gone", cwd.display());
    }
    let machine = crate::cli::machine_config();
    let launch = rimz::config::effective::load(&machine, &workspace.project_root)?;
    launch.block_set_failure()?;
    let posture = rimz::harness::resume::resolve_posture(
        PostureRequest {
            record: child.record.as_deref(),
            profile: child.profile.as_deref(),
            kind: &child.kind,
            stamped_mode: child.mode,
            stamped_tier: child.tier.as_deref(),
        },
        launch.profiles_for(rimz::config::effective::ProfileScope::Subagents),
    );
    if let Some(reason) = &posture.degraded {
        bail!("{reason}");
    }
    let adapter = rimz::agents::find_definition(child.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", child.kind))?;
    let capped = rimz::harness::plan::cap_child_isolation(
        rimz::config::Isolation::ambient(&rimz::agents::ambient_env()),
        child.isolation,
        posture.launch.isolation_default,
        machine.agents.isolation,
    )?;
    if let Some(source) = &capped.clamped {
        supervised::note_isolation_clamp(child.profile.as_deref().unwrap_or(&child.kind), source)?;
    }
    let isolation = rimz::config::Isolation::resolve(
        capped.isolation,
        posture.launch.isolation_default,
        machine.agents.isolation,
    );
    rimz::sandbox::preflight_launch(
        isolation,
        &child.kind,
        posture.launch.skills.is_some(),
        adapter.manual_skill(),
    )?;
    rimz::harness::launch::preflight_agent_kind(
        &workspace.project_root,
        child.kind.as_str(),
        &cwd,
    )?;
    let logins = rimz::agents::room_accounts(
        &store.paths().workspace_record,
        &rimz::config::MachineConfig::load_lenient(),
    )?;
    let catalog = rimz::agents::machine_login_catalog();
    let (action, login, fresh_reason) =
        agents_cmd::relaunch_action(child, &logins, &catalog, &cwd)?;
    if let Some(reason) = fresh_reason {
        bail!("{reason}");
    }
    rimz::agents::session_login(&child.kind, login.as_ref(), &machine.accounts)?
        .health(&catalog.native_ambient(&child.kind, &rimz::agents::ambient_env()))?;
    let runs = rimz::harness::run::list(store.paths())?;
    let run = newest_run_for_child(&runs, child)
        .ok_or_else(|| anyhow::anyhow!("no supervised run recorded"))?;
    let mut request = resume_request(child, run, &posture, action, login);
    request.identity.params.isolation = capped.isolation;
    if isolation == rimz::config::Isolation::Host {
        rimz::harness::launch::preflight_agent_process(
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
    let room = rimz::room::RoomContext::from_resolved(
        &parent_workspace,
        machine,
        parent_pane.pane_id.mux(),
        rimz::room::RoomSizing::OrdinaryTab,
    )?;
    let pane = rimz::mux::PaneCmd {
        argv: rimz::harness::launch::exec_argv(
            &rimz::proc::rimz_exe(),
            store.runtime_paths(),
            &request,
        )?,
        name: child.name.clone(),
    };
    let env =
        rimz::room::pane_identity_env(&parent_workspace, &cwd, child.channel.as_deref(), false);
    let _guard = supervised::pane::lock_subagent_zone(store)?;
    let current = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    if supervised::pane::launch_has_bound_pane(&current.agents, &child.kind, launch_id) {
        return Ok(());
    }
    let sidebar = room.sidebar_options(&cwd);
    match supervised::pane::split_into_subagent_zone(
        room.backend(),
        store,
        &parent_workspace,
        &cwd,
        env.clone(),
        sidebar.clone(),
        &pane,
        child.name.as_deref().unwrap_or(child.agent_id.as_str()),
    ) {
        supervised::pane::SubagentZoneOpen::Opened => {}
        supervised::pane::SubagentZoneOpen::Failed(err) => return Err(err.into()),
        fallback => {
            let title = match fallback {
                supervised::pane::SubagentZoneOpen::CompanionTab => {
                    supervised::pane::subagent_companion_title(store)
                }
                _ => format!("run {}", child.kind),
            };
            room.backend().open_tab(&rimz::mux::TabOptions {
                env,
                title,
                panes: rimz::mux::LayoutPanes {
                    columns: vec![rimz::mux::LayoutColumn {
                        panes: vec![pane],
                        stacked: false,
                    }],
                    focused_pane: 0,
                },
                focus: false,
                dock_sidebar: true,
                after: Some(parent_pane.pane_id.clone()),
                sidebar,
            })?;
        }
    }
    if !supervised::pane::wait_for_subagent_pane_bind(store, &child.kind, launch_id) {
        return Err(resume_bind_timeout(child));
    }
    Ok(())
}

fn resume_bind_timeout(child: &AgentState) -> anyhow::Error {
    anyhow::anyhow!(
        "resumed @{} but its pane did not bind in time; check its pane, or launch a new child",
        child.name.as_deref().unwrap_or(child.agent_id.as_str())
    )
}

fn resume_request(
    child: &AgentState,
    run: &RunRecord,
    posture: &ResumePosture,
    action: ExecAction,
    login: Option<rimz::ids::LoginName>,
) -> ExecRequest {
    let mut request = agents_cmd::relaunch_request(child, posture, action, login, None);
    request.identity.name_explicit = true;
    let (close_pane_on_exit, exit_on_run_completion) = supervised::run_exit_policy(!run.keep);
    ExecRequest {
        run_id: Some(run.run_id.clone()),
        subagent: true,
        close_pane_on_exit,
        exit_on_run_completion,
        ..request
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
