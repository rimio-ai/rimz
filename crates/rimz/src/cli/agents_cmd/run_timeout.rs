//! Hidden helper that settles an overdue supervised run and reclaims its pane.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::harness::run_timeout::RunTimeoutRequest;

use super::Ctx;

pub fn run_timeout(request: RunTimeoutRequest, globals: &super::GlobalFlags) -> Result<()> {
    let paths = rimz::StatePaths::for_workspace(request.workspace_id.clone())
        .context("preparing store paths")?;
    let initial = rimz::harness::run::load(&paths, &request.run_id).context("loading timed run")?;
    let mux_hint = initial.pane_id.as_ref().map(|pane| pane.mux());
    let ctx = Ctx::for_workspace(request.workspace_id, mux_hint)?;
    let provider_process = initial
        .subagent
        .then(|| provider_process_for_run(&initial))
        .flatten();
    let now = Timestamp::now();
    let (record, wrote) =
        rimz::harness::run::timeout_if_due(ctx.store.paths(), &request.run_id, now)?;
    let deadline_due = record.deadline_at.is_some_and(|deadline| deadline <= now);
    if !wrote && !(record.status == rimz::harness::run::RunStatus::TimedOut && deadline_due) {
        return Ok(());
    }
    if wrote {
        let _ = rimz::store::wakeup::wake_run(ctx.store.runtime_paths(), &record);
    }
    if retains_pane_after_timeout(&record) {
        if wrote && let Some((pid, process_start)) = provider_process {
            let _ = rimz::child_process::signal_process_term(pid, Some(&process_start));
        }
        // The wrapper observes the terminal record and stops the provider, but
        // a subagent pane is retained until explicit stop or parent exit.
        return Ok(());
    }
    super::supervised::stop_supervised_run(&ctx.workspace, &ctx.store, globals, &record)
        .context("reclaiming timed-out run pane")
}

fn retains_pane_after_timeout(record: &rimz::harness::run::RunRecord) -> bool {
    record.subagent
}

fn provider_process_for_run(record: &rimz::harness::run::RunRecord) -> Option<(u32, String)> {
    Some((record.provider_pid?, record.provider_process_start.clone()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_subagent_timeouts_retain_the_pane() {
        let mut record = rimz::harness::run::RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-run")),
            rimz::ids::AgentKind::new_unchecked("codex"),
            rimz::harness::run::PermissionMode::Auto,
            "test".to_owned(),
            std::path::PathBuf::from("/tmp/rimz-run"),
        );
        assert!(!retains_pane_after_timeout(&record));

        record.subagent = true;
        assert!(retains_pane_after_timeout(&record));
    }

    #[test]
    fn timeout_backstop_uses_persisted_provider_not_wrapper_owner() {
        let mut record = rimz::harness::run::RunRecord::new(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-run")),
            rimz::ids::AgentKind::new_unchecked("codex"),
            rimz::harness::run::PermissionMode::Auto,
            "test".to_owned(),
            std::path::PathBuf::from("/tmp/rimz-run"),
        );
        record.provider_pid = Some(42);
        record.provider_process_start = Some("provider-start".to_owned());
        let mut provisional = rimz::agents::AgentState::stub(
            "codex",
            "launch-child",
            rimz::agents::AgentStatus::Running,
        );
        provisional.runtime_owner = Some(rimz::store::runtime::process_owner(
            rimz::RuntimeOwnerKind::Agent,
            provisional.agent_id.as_str(),
            84,
        ));

        assert_eq!(
            provider_process_for_run(&record),
            Some((42, "provider-start".to_owned()))
        );
        assert_ne!(
            provider_process_for_run(&record).map(|(pid, _)| pid),
            provisional.runtime_owner.map(|owner| owner.pid)
        );
    }
}
