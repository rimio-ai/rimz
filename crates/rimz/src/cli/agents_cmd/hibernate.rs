//! Hidden hibernation helper; the domain owns the decision and freeze transaction.

use anyhow::{Context, Result};
use rimz::harness::hibernate::{self, HibernateHelperRequest};

pub(super) fn wake(request: hibernate::WakeHelperRequest) -> Result<()> {
    let ctx = super::Ctx::for_workspace(request.workspace_id, Some(request.pane_id.mux()))?;
    hibernate::wake_pane(&ctx.store, &request.pane_id).context("waking hibernating agent")?;
    Ok(())
}

pub(super) fn run(request: HibernateHelperRequest) -> Result<()> {
    let config = rimz::config::MachineConfig::load_lenient();
    let ctx = super::Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let mut reader = rimz::sidebar::consumer::PublishedSnapshotReader::new(
        ctx.runtime().clone(),
        ctx.workspace.session_name.clone(),
        None,
    );
    let snapshot = reader
        .read(ctx.store.paths())
        .context("reading hibernation snapshot")?;
    hibernate::run_request(&ctx.store, &snapshot, &request, &config, || {
        reader.read(ctx.store.paths())
    })
    .context("hibernating idle agent")
}
