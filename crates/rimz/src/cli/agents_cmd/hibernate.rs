//! Hidden hibernation helper; the domain owns the decision and freeze transaction.

use anyhow::{Context, Result};
use rimz::harness::hibernate::{self, HibernateHelperRequest};

pub(super) fn run(request: HibernateHelperRequest) -> Result<()> {
    let config = rimz::config::MachineConfig::load_lenient();
    let ctx = super::Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let snapshot = ctx
        .published_snapshot()
        .context("reading hibernation snapshot")?;
    hibernate::run_request(&ctx.store, &snapshot, &request, &config, &|agent| {
        rimz::sidebar::unread::seen_since(ctx.runtime(), agent, hibernate::rested_at(agent))
    })
    .context("hibernating idle agent")
}
