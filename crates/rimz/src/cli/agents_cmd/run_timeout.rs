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
    super::supervised::stop_supervised_run(&ctx.workspace, &ctx.store, globals, &record)
        .context("reclaiming timed-out run pane")
}
