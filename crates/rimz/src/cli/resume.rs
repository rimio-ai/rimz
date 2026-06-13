use super::*;

pub(super) fn session_is_healthy_live(backend: &dyn MuxBackend, session_name: &str) -> bool {
    let exists = backend
        .list_sessions()
        .map(|sessions| sessions.iter().any(|name| name == session_name))
        .unwrap_or(false);
    exists
        && matches!(
            backend.probe_session_health(session_name),
            Ok(SessionHealth::Healthy)
        )
}

/// Plan the agents a reborn session re-seeds, reading the durable *audit*
/// rollup — the one that keeps the dead-process agents a runtime read would
/// expel, which is exactly the set a rebirth must bring back. Best-effort: a
/// disabled feature, the `--no-resume` override, or any ledger read error yields
/// an empty plan (the birth comes up bare) and never blocks the launch.
pub(super) fn plan_room_resume(
    workspace_id: &rimz::WorkspaceId,
    resume_cfg: &rimz::config::ResumeConfig,
    disabled: bool,
) -> rimz::resume::ResumePlan {
    if disabled || !resume_cfg.on_rebirth {
        return rimz::resume::ResumePlan::default();
    }
    let planned = (|| -> Result<rimz::resume::ResumePlan> {
        let paths = StatePaths::for_workspace(workspace_id.clone())?;
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        let ledger = Ledger::open(paths, runtime)?;
        let projection = ledger.runtime_projection(rimz::RuntimeScope::Audit)?;
        let ended = rimz::ledger::snapshot::agent_tombstones_for_events(&projection.events);
        let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
        Ok(rimz::resume::plan_resume(
            &projection.agents,
            &ended,
            resume_cfg.max,
            |path| path.is_dir(),
            &rimz_bin,
        ))
    })();
    planned.unwrap_or_else(|err| {
        tracing::warn!(workspace = %workspace_id, error = %err, "resume planning skipped");
        rimz::resume::ResumePlan::default()
    })
}

/// Draw the rebirth boundary in the ledger: a reborn mux session renumbers
/// panes from zero, so every pane stamp in the rollup now names a pane that no
/// longer exists — and the new session reuses those ids. The appended
/// `session.rebirth` event makes the fold clear all prior stamps, so a stale
/// session can never bind (or block stamp recovery of) a reborn pane id.
/// Called only on a genuine birth (`!was_live`), *after* resume planning —
/// the planner reads the old stamps to pick its candidates. Best-effort like
/// the plan itself: boundary hygiene never blocks the launch.
pub(super) fn record_rebirth_boundary(workspace_id: &rimz::WorkspaceId, session_name: &str) {
    let appended = (|| -> Result<()> {
        let paths = StatePaths::for_workspace(workspace_id.clone())?;
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        let ledger = Ledger::open(paths, runtime)?;
        let event = rimz::EventEnvelope::session_rebirth(workspace_id.clone(), session_name);
        ledger.append_event(&event)?;
        Ok(())
    })();
    if let Err(err) = appended {
        tracing::warn!(workspace = %workspace_id, error = %err, "rebirth boundary skipped");
    }
}

/// Tell the user which prior agents the reborn room brought back, and which it
/// could not — to stderr, so the attach command on stdout stays clean for
/// scripting. Silent when there is nothing to resume.
pub(super) fn report_resume(plan: &rimz::resume::ResumePlan) {
    if !plan.panes.is_empty() {
        let labels = plan
            .panes
            .iter()
            .map(|pane| pane.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            std::io::stderr(),
            "resumed {} agent{}: {labels}",
            plan.panes.len(),
            if plan.panes.len() == 1 { "" } else { "s" },
        );
    }
    if !plan.skipped.is_empty() {
        let detail = plan
            .skipped
            .iter()
            .map(|skip| format!("{} ({})", skip.label, resume_skip_reason(skip.reason)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(std::io::stderr(), "not resumed: {detail}");
    }
}

fn resume_skip_reason(reason: rimz::resume::ResumeSkipReason) -> &'static str {
    match reason {
        rimz::resume::ResumeSkipReason::NoResumeSupport => "no resume CLI",
        rimz::resume::ResumeSkipReason::WorktreeMissing => "worktree gone",
        rimz::resume::ResumeSkipReason::OverCap => "over the resume cap",
    }
}
