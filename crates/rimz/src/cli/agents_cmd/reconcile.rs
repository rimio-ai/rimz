use super::*;
use std::io::IsTerminal;

pub(super) enum Reconciled {
    Continue,
    Done,
    Resume(PathBuf),
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
    cells: &[rimz::harness::plan::CohortCell],
    fresh: bool,
) -> Result<Reconciled> {
    let path = cohort_worktree_path(workspace, &machine_config.agents.worktree, name)?;
    if !path.exists() {
        return Ok(Reconciled::Continue);
    }
    let Some(marker) = rimz::worktree::read_marker_for_worktree(&path)? else {
        return Ok(Reconciled::Continue);
    };

    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let subject = cohort_subject(spec_display, team);
    match rimz::harness::resume::inspect_cohort_relaunch(&projection.agents, &path, cells, team) {
        rimz::harness::resume::CohortRelaunchState::Absent => Ok(Reconciled::Continue),
        rimz::harness::resume::CohortRelaunchState::Present { focus_pane } => {
            if let Some(pane_id) = focus_pane {
                let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
                rimz::mux::focus_anchor::execute_action(
                    backend,
                    &runtime,
                    &workspace.session_name,
                    pane_id,
                )?;
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
        rimz::harness::resume::CohortRelaunchState::Closed => {
            if fresh {
                return launch_fresh(name, &subject);
            }
            let (resume_command, fresh_command) = relaunch_commands(name, spec_display, team);
            let status = rimz::worktree::status(&path, &marker)?;
            // Cohort liveness was already decided above, so Git state alone
            // separates "recreate it" from "resume into it". A concurrent
            // cleanup may still remove the checkout after this decision.
            let protections = rimz::worktree::ProtectionSet::default();
            if protections.assess(&path, status) == rimz::worktree::RemovalAssessment::Removable {
                recreate_or_done(
                    workspace,
                    machine_config,
                    store,
                    name,
                    &subject,
                    &protections,
                    &fresh_command,
                )
            } else {
                resume_or_done(name, &subject, &path, &resume_command, &fresh_command)
            }
        }
    }
}

fn cohort_worktree_path(
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::WorktreeConfig,
    name: &str,
) -> rimz::worktree::Result<PathBuf> {
    rimz::worktree::worktree_path(workspace.launch_repo_root(), config, name)
}

fn cohort_subject(spec_display: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("team `{team}`"),
        None => format!("`{spec_display}`"),
    }
}

fn resume_or_done(
    name: &str,
    subject: &str,
    path: &Path,
    resume_command: &str,
    fresh_command: &str,
) -> Result<Reconciled> {
    if !std::io::stdin().is_terminal() {
        writeln!(
            std::io::stderr().lock(),
            "worktree `{name}` has work in progress; resume with `{resume_command}` or launch fresh with `{fresh_command}`"
        )?;
        return Ok(Reconciled::Done);
    }
    match crate::cli::choose(
        &format!(
            "worktree `{name}` has work in progress; resume {subject}, launch it fresh, or cancel?"
        ),
        &["resume", "fresh", "cancel"],
        0,
    )? {
        Some(0) => Ok(Reconciled::Resume(path.to_owned())),
        Some(1) => launch_fresh(name, subject),
        _ => canceled(),
    }
}

fn recreate_or_done(
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    store: &rimz::Store,
    name: &str,
    subject: &str,
    protections: &rimz::worktree::ProtectionSet,
    fresh_command: &str,
) -> Result<Reconciled> {
    if !std::io::stdin().is_terminal() {
        writeln!(
            std::io::stderr().lock(),
            "worktree `{name}` is clean and merged; recreate {subject} with `rimz worktree remove {name}` then relaunch, or launch fresh into it with `{fresh_command}`"
        )?;
        return Ok(Reconciled::Done);
    }
    match crate::cli::choose(
        &format!(
            "worktree `{name}` is clean and merged; remove it and recreate {subject}, launch it fresh, or cancel?"
        ),
        &["remove", "fresh", "cancel"],
        2,
    )? {
        Some(0) => {}
        Some(1) => return launch_fresh(name, subject),
        _ => return canceled(),
    }
    let removed = match rimz::worktree::remove(
        workspace.launch_repo_root(),
        &machine_config.agents.worktree,
        name,
        false,
        protections,
    ) {
        Err(rimz::worktree::WorktreeErr::Missing { .. }) => {
            writeln!(
                std::io::stderr().lock(),
                "worktree `{name}` was already removed; recreating {subject}"
            )?;
            return Ok(Reconciled::Continue);
        }
        result => result?,
    };
    let retirement = rimz::worktree::retire_removal(
        store,
        &removed,
        "worktree recreated",
        &workspace.session_name,
    );
    let session_retirement = retirement
        .session_retirement
        .context("retiring sessions for recreated worktree");
    let message_archival = retirement
        .message_archival
        .context("archiving messages for recreated worktree channel");
    session_retirement?;
    message_archival?;
    Ok(Reconciled::Continue)
}

fn launch_fresh(name: &str, subject: &str) -> Result<Reconciled> {
    writeln!(
        std::io::stderr().lock(),
        "launching {subject} fresh in worktree `{name}`"
    )?;
    Ok(Reconciled::Continue)
}

fn canceled() -> Result<Reconciled> {
    writeln!(std::io::stderr().lock(), "canceled; nothing launched")?;
    Ok(Reconciled::Done)
}

fn relaunch_commands(name: &str, spec_display: &str, team: Option<&str>) -> (String, String) {
    match team {
        Some(team) => (
            format!("rimz teams resume {team} -w {name}"),
            format!("rimz teams {team} -w {name} --fresh"),
        ),
        None => (
            format!("rimz agents {spec_display} -w {name} --resume"),
            format!("rimz agents {spec_display} -w {name} --fresh"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_commands_cover_teams_and_inline_specs() {
        assert_eq!(
            relaunch_commands("topic", "forge", Some("forge")),
            (
                "rimz teams resume forge -w topic".to_owned(),
                "rimz teams forge -w topic --fresh".to_owned(),
            )
        );
        assert_eq!(
            relaunch_commands("topic", "claude,codex", None),
            (
                "rimz agents claude,codex -w topic --resume".to_owned(),
                "rimz agents claude,codex -w topic --fresh".to_owned(),
            )
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
