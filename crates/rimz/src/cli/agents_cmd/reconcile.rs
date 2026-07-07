use super::*;
use std::io::IsTerminal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReconcileAction {
    FreshLaunch,
    Focus,
    Resume,
    Recreate,
}

pub(super) enum Reconciled {
    Continue,
    Done,
    Resume(PathBuf),
}

pub(super) fn reconcile_action(
    present_members: bool,
    has_history: bool,
    status: rimz::worktree::WorktreeStatus,
) -> ReconcileAction {
    if present_members {
        return ReconcileAction::Focus;
    }
    if !has_history {
        return ReconcileAction::FreshLaunch;
    }
    if status.safe_to_remove() {
        ReconcileAction::Recreate
    } else {
        ReconcileAction::Resume
    }
}

pub(super) fn reconcile_named_team_launch(
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    name: &str,
    team: &str,
) -> Result<Reconciled> {
    let path = rimz::worktree::worktree_path(
        &workspace.project_root,
        &machine_config.agents.worktree,
        name,
    )?;
    if !path.exists() {
        return Ok(Reconciled::Continue);
    }
    let Some(marker) = rimz::worktree::read_marker_for_worktree(&path)? else {
        return Ok(Reconciled::Continue);
    };

    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let members = named_team_members(&projection.agents, team, &path);
    let present_members = members.iter().any(|member| member_is_present(member));
    let status = if present_members || members.is_empty() {
        rimz::worktree::WorktreeStatus::default()
    } else {
        rimz::worktree::status(&path, &marker)?
    };

    match reconcile_action(present_members, !members.is_empty(), status) {
        ReconcileAction::FreshLaunch => Ok(Reconciled::Continue),
        ReconcileAction::Focus => {
            if let Some(member) = newest_present_member_with_pane(&members)
                && let Some(pane) = member.pane.as_ref()
            {
                backend.focus_pane(&pane.pane_id, Some(&workspace.session_name))?;
                writeln!(
                    std::io::stderr().lock(),
                    "team `{team}` is already running in worktree `{name}`; focused it"
                )?;
                return Ok(Reconciled::Done);
            }
            writeln!(
                std::io::stderr().lock(),
                "team `{team}` is already running in worktree `{name}`"
            )?;
            Ok(Reconciled::Done)
        }
        ReconcileAction::Resume => resume_or_done(name, team, &path),
        ReconcileAction::Recreate => {
            recreate_or_done(workspace, machine_config, store, name, team, marker)
        }
    }
}

fn named_team_members<'a>(
    agents: &'a [AgentState],
    team: &str,
    worktree: &Path,
) -> Vec<&'a AgentState> {
    let target = rimz::worktree::normalize_path_lexical(worktree);
    agents
        .iter()
        .filter(|agent| {
            agent.parent_agent_id.is_none()
                && agent.team.as_deref() == Some(team)
                && agent.worktree_path.as_deref().is_some_and(|path| {
                    rimz::worktree::normalize_path_lexical(Path::new(path)) == target
                })
        })
        .collect()
}

fn newest_present_member_with_pane<'a>(members: &[&'a AgentState]) -> Option<&'a AgentState> {
    members
        .iter()
        .copied()
        .filter(|member| member.pane.is_some() && member_is_present(member))
        .max_by(|a, b| a.last_activity.cmp(&b.last_activity))
}

fn member_is_present(member: &AgentState) -> bool {
    match rimz::store::runtime::agent_liveness(member) {
        rimz::store::runtime::AgentLiveness::Live { .. } => true,
        rimz::store::runtime::AgentLiveness::Unknown => member.pane.is_some(),
        rimz::store::runtime::AgentLiveness::Dead => false,
    }
}

fn resume_or_done(name: &str, team: &str, path: &Path) -> Result<Reconciled> {
    if !std::io::stdin().is_terminal() {
        writeln!(
            std::io::stderr().lock(),
            "worktree `{name}` has work in progress; resume with `rimz agents {team} --resume`"
        )?;
        return Ok(Reconciled::Done);
    }
    if crate::cli::confirm_with_default(
        &format!("worktree `{name}` has work in progress; resume team `{team}`?"),
        true,
    )? {
        Ok(Reconciled::Resume(path.to_owned()))
    } else {
        Ok(Reconciled::Done)
    }
}

fn recreate_or_done(
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    store: &rimz::Store,
    name: &str,
    team: &str,
    marker: rimz::worktree::WorktreeMarker,
) -> Result<Reconciled> {
    if !std::io::stdin().is_terminal() {
        writeln!(
            std::io::stderr().lock(),
            "worktree `{name}` is clean and merged; recreate with `rimz worktree remove {name}` then relaunch"
        )?;
        return Ok(Reconciled::Done);
    }
    if !crate::cli::confirm_with_default(
        &format!("worktree `{name}` is clean and merged; gc it and recreate team `{team}` fresh?"),
        false,
    )? {
        return Ok(Reconciled::Done);
    }
    let removed = crate::cli::worktree::remove_and_archive(
        &marker,
        || {
            rimz::worktree::remove(
                &workspace.project_root,
                &machine_config.agents.worktree,
                name,
                false,
            )
            .map_err(Into::into)
        },
        |channel, _reason| {
            store
                .archive_channel_messages(channel, "worktree recreated", &workspace.session_name)
                .map(|_| ())
                .map_err(Into::into)
        },
    )?;
    removed
        .archive
        .context("archiving messages for recreated worktree channel")?;
    Ok(Reconciled::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_action_table() {
        let clean = rimz::worktree::WorktreeStatus::default();
        let dirty = rimz::worktree::WorktreeStatus {
            dirty: true,
            landed: rimz::worktree::LandedVerdict::Landed,
        };
        let pending = rimz::worktree::WorktreeStatus {
            dirty: false,
            landed: rimz::worktree::LandedVerdict::Pending,
        };

        assert_eq!(reconcile_action(true, false, clean), ReconcileAction::Focus);
        assert_eq!(reconcile_action(true, true, dirty), ReconcileAction::Focus);
        assert_eq!(
            reconcile_action(false, false, dirty),
            ReconcileAction::FreshLaunch
        );
        assert_eq!(
            reconcile_action(false, true, clean),
            ReconcileAction::Recreate
        );
        assert_eq!(
            reconcile_action(false, true, dirty),
            ReconcileAction::Resume
        );
        assert_eq!(
            reconcile_action(false, true, pending),
            ReconcileAction::Resume
        );
    }

    #[test]
    fn member_presence_needs_live_owner_or_pane_evidence() {
        let mut agent = test_agent("sess-paneless");

        assert!(!member_is_present(&agent));

        agent.pane = Some(rimz::pane::PaneRef::from_id(rimz::PaneId::from_parts(
            rimz::MuxName::Zellij,
            "terminal_1",
        )));
        assert!(member_is_present(&agent));
    }

    fn test_agent(id: &str) -> AgentState {
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            name_explicit: false,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            waiting_since: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: jiff::Timestamp::UNIX_EPOCH,
            last_activity: jiff::Timestamp::UNIX_EPOCH,
            registered_at: Some(jiff::Timestamp::UNIX_EPOCH),
        }
    }
}
