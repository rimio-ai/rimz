//! Parent-only resume doorway for a message to an ended child.

use super::*;
use crate::cli::supervised;
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest};
use rimz::harness::resume::{PostureRequest, ResumePosture};
use rimz::store::run::RunRecord;

fn ended_child<'a>(
    agents: &'a [AgentState],
    caller: &AgentState,
    target: &str,
    scope: Option<&str>,
    channel: Option<&str>,
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
    channel: Option<&str>,
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
            profile: child.profile.as_deref(),
            kind: &child.kind,
            stamped_mode: child.mode,
        },
        launch.profiles_for(rimz::config::effective::ProfileScope::Subagents),
    );
    if let Some(reason) = &posture.degraded {
        bail!("{reason}");
    }
    let adapter = rimz::agents::find_definition(child.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", child.kind))?;
    let isolation = rimz::config::Isolation::resolve(
        child.isolation,
        posture.launch.isolation_default,
        machine.agents.isolation,
    );
    rimz::sandbox::preflight_skills(
        isolation,
        &child.kind,
        posture.launch.skills.is_some(),
        adapter.manual_skill(),
    )?;
    rimz::sandbox::preflight(isolation)?;
    rimz::harness::launch::preflight_agent_kind(
        &workspace.project_root,
        child.kind.as_str(),
        &cwd,
    )?;
    let logins = rimz::agents::room_logins(&store.paths().workspace_record)?;
    let (action, fresh_reason) = agents_cmd::relaunch_action(child, &logins, &cwd)?;
    if let Some(reason) = fresh_reason {
        bail!("{reason}");
    }
    let runs = rimz::harness::run::list(store.paths())?;
    let run = newest_run_for_child(&runs, child)
        .ok_or_else(|| anyhow::anyhow!("no supervised run recorded"))?;
    let request = resume_request(child, run, &posture, action);
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
    let mut env = rimz::room::pane_identity_env(&parent_workspace, child.channel.as_deref(), false);
    env.insert(
        rimz::workspace::ENV_WORKTREE_PATH.to_owned(),
        cwd.display().to_string(),
    );
    let _guard = supervised::pane::lock_subagent_zone(store)?;
    let current = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    if supervised::pane::launch_has_bound_pane(&current.agents, &child.kind, launch_id) {
        return Ok(());
    }
    let sidebar = room.sidebar_options(&cwd, Vec::new(), None);
    match supervised::pane::split_into_subagent_zone(
        room.backend(),
        store,
        &parent_workspace,
        &cwd,
        env,
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
        bail!(
            "resumed @{} but its pane did not bind in time",
            child.name.as_deref().unwrap_or(child.agent_id.as_str())
        );
    }
    Ok(())
}

fn resume_request(
    child: &AgentState,
    run: &RunRecord,
    posture: &ResumePosture,
    mut action: ExecAction,
) -> ExecRequest {
    *action.extra_args_mut() = posture.launch.args.clone();
    let (close_pane_on_exit, exit_on_run_completion) = supervised::run_exit_policy(!run.keep);
    ExecRequest {
        action,
        run_id: Some(run.run_id.clone()),
        subagent: true,
        close_pane_on_exit,
        exit_on_run_completion,
        system_prompt_file: posture.launch.system_prompt_file.clone(),
        append_system_prompt_files: posture.launch.append_system_prompt_files.clone(),
        team_prompt: posture.launch.team_prompt.clone(),
        skills: posture.launch.skills.clone(),
        isolation_default: posture.launch.isolation_default,
        identity: ExecIdentity {
            name: child.name.clone(),
            name_explicit: true,
            launch_id: child.launch_id.as_ref().map(ToString::to_string),
            params: rimz::agents::LaunchParams {
                parent_agent_id: child.parent_agent_id.clone(),
                parent_agent_kind: child.parent_agent_kind.clone(),
                launch_depth: child.launch_depth,
                launched_by: child.launched_by.clone().map(Box::new),
                profile: child.profile.clone(),
                login: child.login.clone(),
                role: child.role.clone(),
                team: child.team.clone(),
                launch_group: child.launch_group.clone(),
                launch_ordinal: child.launch_ordinal,
                channel: child.channel.clone(),
                mode: posture.launch.mode,
                isolation: child.isolation,
                model: posture.launch.model.clone(),
                effort: posture.launch.effort.clone(),
                budget: posture.launch.budget.clone(),
                kind_ordinal: None,
            },
        },
        ..ExecRequest::bare_launch(child.kind.clone(), Vec::new())
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
