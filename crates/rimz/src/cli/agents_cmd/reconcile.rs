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
    assessment: rimz::worktree::RemovalAssessment,
) -> ReconcileAction {
    if present_members {
        return ReconcileAction::Focus;
    }
    if !has_history {
        return ReconcileAction::FreshLaunch;
    }
    if assessment == rimz::worktree::RemovalAssessment::Removable {
        ReconcileAction::Recreate
    } else {
        ReconcileAction::Resume
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reconcile_cohort_launch(
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    name: &str,
    spec_display: &str,
    team: Option<&str>,
    cells: &[rimz::harness::resume::CohortCell],
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
    let members = cohort_members(&projection.agents, &path, cells, team);
    let present_members = members.iter().any(|member| member_is_present(member));
    let status = if present_members || members.is_empty() {
        rimz::worktree::WorktreeStatus::default()
    } else {
        rimz::worktree::status(&path, &marker)?
    };
    let assessment = rimz::worktree::removal_assessment(
        &path,
        status,
        &rimz::worktree::RemovalProtection::default(),
    );

    let subject = cohort_subject(spec_display, team);
    match reconcile_action(present_members, !members.is_empty(), assessment) {
        ReconcileAction::FreshLaunch => Ok(Reconciled::Continue),
        ReconcileAction::Focus => {
            if let Some(member) = newest_present_member_with_pane(&members)
                && let Some(pane) = member.pane.as_ref()
            {
                backend.focus_pane(&pane.pane_id, Some(&workspace.session_name))?;
                writeln!(
                    std::io::stderr().lock(),
                    "{subject} is already running in worktree `{name}`; focused it"
                )?;
                return Ok(Reconciled::Done);
            }
            writeln!(
                std::io::stderr().lock(),
                "{subject} is already running in worktree `{name}`"
            )?;
            Ok(Reconciled::Done)
        }
        ReconcileAction::Resume => resume_or_done(name, spec_display, &subject, &path),
        ReconcileAction::Recreate => {
            recreate_or_done(workspace, machine_config, store, name, &subject)
        }
    }
}

fn cohort_members<'a>(
    agents: &'a [AgentState],
    worktree: &Path,
    cells: &[rimz::harness::resume::CohortCell],
    team: Option<&str>,
) -> Vec<&'a AgentState> {
    let target = rimz::worktree::normalize_path_lexical(worktree);
    let candidates = agents
        .iter()
        .filter(|agent| {
            agent.parent_agent_id.is_none()
                && agent.worktree_path.as_deref().is_some_and(|path| {
                    rimz::worktree::normalize_path_lexical(Path::new(path)) == target
                })
        })
        .collect::<Vec<_>>();
    match team {
        Some(team) => candidates
            .into_iter()
            .filter(|agent| agent.team.as_deref() == Some(team))
            .collect(),
        None => {
            let candidates = candidates
                .into_iter()
                .filter(|agent| !agent.agent_id.is_empty())
                .collect::<Vec<_>>();
            rimz::harness::resume::match_cohort(&candidates, cells, None)
                .into_iter()
                .flatten()
                .collect()
        }
    }
}

fn cohort_subject(spec_display: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("team `{team}`"),
        None => format!("`{spec_display}`"),
    }
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

fn resume_or_done(
    name: &str,
    spec_display: &str,
    subject: &str,
    path: &Path,
) -> Result<Reconciled> {
    if !std::io::stdin().is_terminal() {
        writeln!(
            std::io::stderr().lock(),
            "worktree `{name}` has work in progress; resume with `rimz agents {spec_display} -w {name} --resume`"
        )?;
        return Ok(Reconciled::Done);
    }
    if crate::cli::confirm_with_default(
        &format!("worktree `{name}` has work in progress; resume {subject}?"),
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
    subject: &str,
) -> Result<Reconciled> {
    if !std::io::stdin().is_terminal() {
        writeln!(
            std::io::stderr().lock(),
            "worktree `{name}` is clean and merged; recreate {subject} with `rimz worktree remove {name}` then relaunch"
        )?;
        return Ok(Reconciled::Done);
    }
    if !crate::cli::confirm_with_default(
        &format!("worktree `{name}` is clean and merged; gc it and recreate {subject} fresh?"),
        false,
    )? {
        return Ok(Reconciled::Done);
    }
    let removed = rimz::worktree::remove(
        &workspace.project_root,
        &machine_config.agents.worktree,
        name,
        false,
    )?;
    store
        .archive_channel_messages(
            removed.worktree_name(),
            "worktree recreated",
            &workspace.session_name,
        )
        .context("archiving messages for recreated worktree channel")?;
    Ok(Reconciled::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconcile_action_table() {
        let removable = rimz::worktree::RemovalAssessment::Removable;
        let dirty = rimz::worktree::RemovalAssessment::Kept(rimz::worktree::RemovalReason::Dirty);
        let pending =
            rimz::worktree::RemovalAssessment::Kept(rimz::worktree::RemovalReason::NotLanded);

        assert_eq!(
            reconcile_action(true, false, removable),
            ReconcileAction::Focus
        );
        assert_eq!(reconcile_action(true, true, dirty), ReconcileAction::Focus);
        assert_eq!(
            reconcile_action(false, false, dirty),
            ReconcileAction::FreshLaunch
        );
        assert_eq!(
            reconcile_action(false, true, removable),
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

    #[test]
    fn cohort_members_match_inline_cells_in_the_target_worktree() {
        let mut planner = test_agent_kind("claude", "planner");
        planner.worktree_path = Some("/code/feature".to_owned());
        planner.launch_group = Some("launch_feature".to_owned());
        planner.launch_ordinal = Some(0);
        planner.role = Some("planner".to_owned());
        let mut coder = test_agent_kind("codex", "coder");
        coder.worktree_path = Some("/code/feature".to_owned());
        coder.launch_group = Some("launch_feature".to_owned());
        coder.launch_ordinal = Some(1);
        coder.role = Some("coder".to_owned());
        let mut unrelated = test_agent_kind("pi", "researcher");
        unrelated.worktree_path = Some("/code/feature".to_owned());
        unrelated.launch_group = Some("launch_unrelated".to_owned());
        unrelated.role = Some("researcher".to_owned());
        let agents = vec![planner, coder, unrelated];
        let cells = vec![
            rimz::harness::resume::CohortCell {
                kind: rimz::ids::AgentKind::new_unchecked("claude"),
                role: Some("planner".to_owned()),
            },
            rimz::harness::resume::CohortCell {
                kind: rimz::ids::AgentKind::new_unchecked("codex"),
                role: Some("coder".to_owned()),
            },
        ];

        let members = cohort_members(&agents, Path::new("/code/feature"), &cells, None);
        let unrelated_members = cohort_members(
            &agents,
            Path::new("/code/feature"),
            &[rimz::harness::resume::CohortCell {
                kind: rimz::ids::AgentKind::new_unchecked("opencode"),
                role: Some("reviewer".to_owned()),
            }],
            None,
        );

        assert_eq!(
            members
                .iter()
                .map(|agent| agent.agent_id.as_str())
                .collect::<Vec<_>>(),
            ["planner", "coder"]
        );
        assert!(unrelated_members.is_empty());
    }

    #[test]
    fn cohort_members_keep_all_named_team_roles_for_partial_specs() {
        let mut planner = test_agent_kind("claude", "planner");
        planner.worktree_path = Some("/code/feature".to_owned());
        planner.team = Some("forge".to_owned());
        planner.role = Some("planner".to_owned());
        let mut reviewer = test_agent_kind("codex", "reviewer");
        reviewer.worktree_path = Some("/code/feature".to_owned());
        reviewer.team = Some("forge".to_owned());
        reviewer.role = Some("reviewer".to_owned());
        let mut unrelated = test_agent_kind("pi", "unrelated");
        unrelated.worktree_path = Some("/code/feature".to_owned());
        unrelated.team = Some("other".to_owned());
        unrelated.role = Some("reviewer".to_owned());
        let agents = vec![planner, reviewer, unrelated];
        let reviewer_cell = rimz::harness::resume::CohortCell {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            role: Some("reviewer".to_owned()),
        };

        let members = cohort_members(
            &agents,
            Path::new("/code/feature"),
            &[reviewer_cell],
            Some("forge"),
        );

        assert_eq!(
            members
                .iter()
                .map(|agent| agent.agent_id.as_str())
                .collect::<Vec<_>>(),
            ["planner", "reviewer"]
        );
    }

    fn test_agent(id: &str) -> AgentState {
        test_agent_kind("codex", id)
    }

    fn test_agent_kind(kind: &str, id: &str) -> AgentState {
        rimz::testkit::agent_state(kind, id, jiff::Timestamp::UNIX_EPOCH)
    }
}
