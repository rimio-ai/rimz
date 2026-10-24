use super::*;
use crate::cli::send;
use crate::cli::worktree_entry::{
    self, Choice, Offer, cohort_subject, launch_fresh, relaunch_commands,
};
use rimz::harness::ancestry::LaunchFocus;
use rimz::message::dispatch::{DispatchMode, DispatchRequest};
use rimz::store::message::DeliveryGate;
use std::io::IsTerminal;

pub(super) enum Reconciled {
    Absent,
    Continue,
    Removed,
    Done,
    PromptDispatched,
    Resume(PathBuf),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_cohort_launch(
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    globals: &GlobalFlags,
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    name: &str,
    path: &Path,
    spec_display: &str,
    team: Option<&str>,
    cells: &[rimz::harness::plan::CohortCell],
    leader_index: Option<usize>,
    prompt: Option<&str>,
    fresh: bool,
    focus: LaunchFocus,
    loop_fire: bool,
) -> Result<Reconciled> {
    if !path.exists() {
        return Ok(Reconciled::Absent);
    }
    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let subject = cohort_subject(spec_display, team);
    match rimz::harness::resume::inspect_cohort_relaunch(
        &projection.agents,
        path,
        cells,
        team,
        leader_index,
    ) {
        rimz::harness::resume::CohortRelaunchState::Absent => Ok(Reconciled::Absent),
        rimz::harness::resume::CohortRelaunchState::Present {
            mut focus_pane,
            leader,
            members,
        } => {
            let running = already_running(name, &subject, team);
            let peers = projection.agents.iter().collect::<Vec<_>>();
            let leader = leader
                .as_ref()
                .and_then(|id| projection.agents.iter().find(|agent| &agent.agent_id == id));
            if loop_fire {
                let leader = leader
                    .map(|agent| {
                        format!(
                            ", leader {}",
                            rimz::address::agent_handle(agent, &peers, true)
                        )
                    })
                    .unwrap_or_default();
                bail!(
                    "{running}{leader}; prompt was not delivered; stop the cohort or retry with --takeover"
                );
            }
            if let Some(prompt) = prompt {
                let Some(leader) = leader else {
                    let hints = members
                        .iter()
                        .filter_map(|id| {
                            projection.agents.iter().find(|agent| &agent.agent_id == id)
                        })
                        .map(|member| {
                            shell_command([
                                "rimz",
                                "message",
                                &rimz::address::agent_handle(member, &peers, true),
                                prompt,
                            ])
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    bail!("{running}, but its leader is not live; message a live member: {hints}");
                };
                let caller = send::resolve_caller(store)?;
                let channel = crate::cli::current_channel(workspace, Some(store));
                let target = format!("@{}", leader.agent_id);
                let result = rimz::message::dispatch::dispatch(
                    workspace,
                    store,
                    DispatchRequest {
                        resuming_recipient: None,
                        target: target.clone(),
                        text: prompt.to_owned(),
                        target_scope: leader.channel(),
                        current_channel: channel.address_context(),
                        sender: send::sender_for(caller.as_ref(), channel.as_deref(), false),
                        caller,
                        automated: false,
                        allow_fanout: false,
                        reply: None,
                        mux: Some(backend.name()),
                        enter: true,
                        force: false,
                        auto_compact: None,
                        mode: DispatchMode::Boundary {
                            gate: DeliveryGate::Done,
                            not_before: None,
                            after: Vec::new(),
                            when: Vec::new(),
                        },
                    },
                )?;
                writeln!(
                    std::io::stderr().lock(),
                    "{running}; prompt sent to its leader"
                )?;
                send::report_dispatch(
                    rimz::message::DeliveryKind::Boundary,
                    &target,
                    &result.outcomes,
                    &result.compacted,
                )?;
                focus_pane = leader.pane.as_ref().map(|pane| pane.pane_id.clone());
            }
            if focus.takes_focus()
                && let Some(pane_id) = focus_pane
            {
                let focused = focus_existing(workspace, backend, store, pane_id);
                if prompt.is_some() {
                    // The prompt is already sent or queued, so a failed focus must
                    // not exit 1 and invite a retry that sends it twice.
                    if let Err(error) = focused {
                        writeln!(
                            std::io::stderr().lock(),
                            "warning: could not focus the leader's pane: {error:#}"
                        )?;
                    }
                    return Ok(Reconciled::PromptDispatched);
                }
                focused?;
                writeln!(std::io::stderr().lock(), "{running}; focused it")?;
                return Ok(Reconciled::Done);
            }
            if prompt.is_some() {
                return Ok(Reconciled::PromptDispatched);
            }
            writeln!(std::io::stderr().lock(), "{running}")?;
            Ok(Reconciled::Done)
        }
        rimz::harness::resume::CohortRelaunchState::Closed { channel } => {
            let Some(marker) = rimz::worktree::read_marker_for_worktree(path)? else {
                return Ok(Reconciled::Continue);
            };
            if fresh {
                launch_fresh(name, &subject)?;
                return Ok(Reconciled::Continue);
            }
            let facts = worktree_entry::inspect(store, path, &marker)?;
            let (resume_command, fresh_command) =
                relaunch_commands(name, spec_display, team, channel.as_deref(), prompt, None);
            // Cohort liveness was already decided above, so Git state alone
            // separates "recreate it" from "resume into it". A concurrent
            // cleanup may still remove the checkout after this decision.
            // A loop fire is unattended even when `loop fire` runs from a
            // terminal: it refuses with the guidance instead of prompting.
            match worktree_entry::offer(
                &Offer {
                    name,
                    subject: &subject,
                    facts: &facts,
                    resume_command: Some(&resume_command),
                    fresh_command: &fresh_command,
                },
                !loop_fire && std::io::stdin().is_terminal(),
            )? {
                Choice::Fresh => {
                    launch_fresh(name, &subject)?;
                    Ok(Reconciled::Continue)
                }
                Choice::Resume => Ok(Reconciled::Resume(path.to_owned())),
                Choice::Remove => {
                    worktree_entry::remove(
                        workspace,
                        machine_config,
                        globals,
                        store,
                        name,
                        path,
                        &subject,
                    )?;
                    Ok(Reconciled::Removed)
                }
            }
        }
    }
}

pub(super) fn cohort_worktree_path(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::WorktreeConfig,
    name: &str,
) -> rimz::worktree::Result<PathBuf> {
    rimz::worktree::worktree_path(workspace.launch_repo_root(), config, name)
}

fn already_running(name: &str, subject: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("{team}#{name} is already running"),
        None => format!("{subject} is already running in worktree `{name}`"),
    }
}

fn focus_existing(
    workspace: &rimz::ResolvedWorkspace,
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    pane_id: rimz::ids::PaneId,
) -> Result<()> {
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)?;
    rimz::harness::hibernate::wake_pane(store, &pane_id)?;
    rimz::mux::focus_anchor::execute_action(backend, &runtime, &workspace.session_name, pane_id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_cohorts_name_a_team_address_or_an_inline_spec_and_worktree() {
        assert_eq!(
            already_running("topic", "`forge`", Some("forge")),
            "forge#topic is already running"
        );
        assert_eq!(
            already_running("topic", "`claude,codex`", None),
            "`claude,codex` is already running in worktree `topic`"
        );
    }

    #[test]
    fn cohort_reconciliation_resolves_worktree_from_current_repo() {
        let project_root = PathBuf::from("/repos/room");
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: rimz::WorkspaceId::from_project_root(&project_root),
            project_root: project_root.clone(),
            cwd_project_root: Some(PathBuf::from("/repos/current")),
            root_class: rimz::workspace::RootClass::Repo,
            worktree_root: project_root,
            worktree_branch: Some("main".to_owned()),
            session_name: "rimz-room".to_owned(),
            mux_hint: None,
        };

        let path = cohort_worktree_path(
            &workspace,
            &rimz::config::WorktreeConfig::default(),
            "cross-root",
        )
        .expect("worktree path");

        assert_eq!(
            path,
            PathBuf::from("/repos/current/../current-worktrees/cross-root")
        );
    }
}
