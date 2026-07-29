//! Hidden helper that settles an overdue supervised run and reclaims its pane.

use anyhow::{Context, Result};
use clap::Args;
use jiff::Timestamp;

use rimz::ids::{RunId, WorkspaceId};

use super::Ctx;

#[derive(Debug, Args)]
pub struct RunTimeoutArgs {
    #[arg(long)]
    workspace_id: String,
    #[arg(long)]
    run_id: String,
}

pub fn run_timeout(args: RunTimeoutArgs, globals: &super::GlobalFlags) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let run_id = RunId::parse(&args.run_id).context("parsing run id")?;
    let paths =
        rimz::StatePaths::for_workspace(workspace_id.clone()).context("preparing store paths")?;
    let initial = rimz::harness::run::load(&paths, &run_id).context("loading timed run")?;
    let mux_hint = initial.pane_id.as_ref().map(|pane| pane.mux());
    let ctx = Ctx::for_workspace(workspace_id, mux_hint)?;
    let now = Timestamp::now();
    let (record, wrote) = rimz::harness::run::timeout_if_due(ctx.store.paths(), &run_id, now)?;
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
