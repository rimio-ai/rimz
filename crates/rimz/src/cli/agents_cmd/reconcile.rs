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
    let subject = cohort_subject(spec_display, team);
    match rimz::harness::resume::inspect_cohort_relaunch(&projection.agents, &path, cells, team) {
        rimz::harness::resume::CohortRelaunchState::Absent => Ok(Reconciled::Continue),
        rimz::harness::resume::CohortRelaunchState::Present { focus_pane } => {
            if let Some(pane_id) = focus_pane {
                let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
                rimz::sidebar::focus_anchor::execute_action(
                    backend,
                    &runtime,
                    &workspace.session_name,
                    pane_id,
                    rimz::sidebar::focus_anchor::FocusOrigin::User,
                    None,
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
            let status = rimz::worktree::status(&path, &marker)?;
            let assessment = rimz::worktree::ProtectionSet::default().assess(&path, status);
            if assessment == rimz::worktree::RemovalAssessment::Removable {
                recreate_or_done(workspace, machine_config, store, name, &subject)
            } else {
                resume_or_done(name, spec_display, &subject, &path)
            }
        }
    }
}

fn cohort_subject(spec_display: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("team `{team}`"),
        None => format!("`{spec_display}`"),
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
