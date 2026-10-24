use super::worktree_entry::{self, cohort_subject, relaunch_commands};
use super::*;
use super::{ClosedCohort, CohortChoice, Interaction, Notice, Unattended, context};
use crate::harness::ancestry::LaunchFocus;
use crate::message::dispatch::{DispatchMode, DispatchRequest};
use crate::store::message::DeliveryGate;

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
    workspace: &crate::ResolvedWorkspace,
    machine_config: &crate::config::MachineConfig,
    root: Option<&Path>,
    mux: Option<crate::ids::MuxName>,
    backend: &dyn crate::mux::MuxBackend,
    store: &crate::Store,
    name: &str,
    path: &Path,
    spec_display: &str,
    team: Option<&str>,
    cells: &[crate::harness::plan::CohortCell],
    leader_index: Option<usize>,
    prompt: Option<&str>,
    fresh: bool,
    focus: LaunchFocus,
    loop_fire: bool,
    interaction: &mut dyn Interaction,
) -> Result<Reconciled> {
    if !path.exists() {
        return Ok(Reconciled::Absent);
    }
    let projection = store.runtime_projection(crate::RuntimeScope::Audit)?;
    let subject = cohort_subject(spec_display, team);
    match crate::harness::resume::inspect_cohort_relaunch(
        &projection.agents,
        path,
        cells,
        team,
        leader_index,
    ) {
        crate::harness::resume::CohortRelaunchState::Absent => Ok(Reconciled::Absent),
        crate::harness::resume::CohortRelaunchState::Present {
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
                            crate::address::agent_handle(agent, &peers, true)
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
                                &crate::address::agent_handle(member, &peers, true),
                                prompt,
                            ])
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    bail!("{running}, but its leader is not live; message a live member: {hints}");
                };
                let caller = context::resolve_caller(store)?;
                let channel = context::current_channel(workspace, Some(store));
                let target = format!("@{}", leader.agent_id);
                let result = crate::message::dispatch::dispatch(
                    workspace,
                    store,
                    DispatchRequest {
                        resuming_recipient: None,
                        target: target.clone(),
                        text: prompt.to_owned(),
                        target_scope: leader.channel(),
                        current_channel: channel.address_context(),
                        sender: context::sender_for(caller.as_ref(), channel.as_deref(), false),
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
                interaction.notice(Notice::Reconcile(&format!(
                    "{running}; prompt sent to its leader"
                )))?;
                interaction.notice(Notice::Dispatch {
                    target: &target,
                    result: &result,
                })?;
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
                        interaction.notice(Notice::Warning(&format!(
                            "warning: could not focus the leader's pane: {error:#}"
                        )))?;
                    }
                    return Ok(Reconciled::PromptDispatched);
                }
                focused?;
                interaction.notice(Notice::Reconcile(&format!("{running}; focused it")))?;
                return Ok(Reconciled::Done);
            }
            if prompt.is_some() {
                return Ok(Reconciled::PromptDispatched);
            }
            interaction.notice(Notice::Reconcile(&running))?;
            Ok(Reconciled::Done)
        }
        crate::harness::resume::CohortRelaunchState::Closed { channel } => {
            let Some(marker) = crate::worktree::read_marker_for_worktree(path)? else {
                return Ok(Reconciled::Continue);
            };
            if fresh {
                return launch_fresh(name, &subject, interaction);
            }
            let facts = worktree_entry::inspect(store, path, &marker)?;
            let (resume_command, fresh_command) =
                relaunch_commands(name, spec_display, team, channel.as_deref(), prompt, None);
            // A loop fire refuses with guidance even when fired from a terminal.
            let mut unattended = Unattended;
            let interaction: &mut dyn Interaction = if loop_fire {
                &mut unattended
            } else {
                interaction
            };
            match interaction.closed_cohort(ClosedCohort {
                name,
                subject: &subject,
                facts: &facts,
                resume_command: Some(&resume_command),
                fresh_command: &fresh_command,
            })? {
                CohortChoice::Fresh => launch_fresh(name, &subject, interaction),
                CohortChoice::Resume => Ok(Reconciled::Resume(path.to_owned())),
                CohortChoice::Recreate => {
                    worktree_entry::remove(
                        workspace,
                        machine_config,
                        root,
                        mux,
                        store,
                        name,
                        path,
                        &subject,
                        interaction,
                    )?;
                    Ok(Reconciled::Removed)
                }
                CohortChoice::Cancel => bail!(
                    "canceled; nothing launched; resume with `{resume_command}` or launch fresh with `{fresh_command}`"
                ),
            }
        }
    }
}

pub(super) fn cohort_worktree_path(
    workspace: &crate::ResolvedWorkspace,
    config: &crate::config::WorktreeConfig,
    name: &str,
) -> crate::worktree::Result<PathBuf> {
    crate::worktree::worktree_path(workspace.launch_repo_root(), config, name)
}

fn already_running(name: &str, subject: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("{team}#{name} is already running"),
        None => format!("{subject} is already running in worktree `{name}`"),
    }
}

fn focus_existing(
    workspace: &crate::ResolvedWorkspace,
    backend: &dyn crate::mux::MuxBackend,
    store: &crate::Store,
    pane_id: crate::ids::PaneId,
) -> Result<()> {
    let runtime = crate::RuntimePaths::for_project_root(&workspace.project_root)?;
    crate::harness::hibernate::wake_pane(store, &pane_id)?;
    crate::mux::focus_anchor::execute_action(backend, &runtime, &workspace.session_name, pane_id)?;
    Ok(())
}

fn launch_fresh(
    name: &str,
    subject: &str,
    interaction: &mut dyn Interaction,
) -> Result<Reconciled> {
    worktree_entry::launch_fresh(name, subject, interaction)?;
    Ok(Reconciled::Continue)
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
        let workspace = crate::ResolvedWorkspace {
            workspace_id: crate::WorkspaceId::from_project_root(&project_root),
            project_root: project_root.clone(),
            cwd_project_root: Some(PathBuf::from("/repos/current")),
            root_class: crate::workspace::RootClass::Repo,
            worktree_root: project_root,
            worktree_branch: Some("main".to_owned()),
            session_name: "rimz-room".to_owned(),
            mux_hint: None,
        };

        let path = cohort_worktree_path(
            &workspace,
            &crate::config::WorktreeConfig::default(),
            "cross-root",
        )
        .expect("worktree path");

        assert_eq!(
            path,
            PathBuf::from("/repos/current/../current-worktrees/cross-root")
        );
    }
}
