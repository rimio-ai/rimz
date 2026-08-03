//! Hidden helper that interrupts an over-budget agent and settles its run.

use anyhow::{Context, Result};

use rimz::harness::budget::BudgetParkRequest;
use rimz::mux::{NamedKey, press_pane_key};

use super::Ctx;

pub fn run_budget_park(request: BudgetParkRequest) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id, Some(request.pane_id.mux()))?;
    let store = &ctx.store;
    let snapshot = ctx
        .resolution_snapshot()
        .context("reading budget interrupt snapshot")?;
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind == request.kind
                && pane.agent_id.as_ref() == Some(&request.agent_id)
                && pane.pane_id == request.pane_id
        })
        .context("budget target pane is no longer bound to the agent")?;

    let at_cost = request.at_cost.or_else(|| {
        rimz::harness::budget::read_ledger(store.runtime_paths(), &request.kind, &request.agent_id)
            .and_then(|ledger| ledger.parked.map(|park| park.at_cost))
    });
    let interrupted = press_pane_key(&request.pane_id, NamedKey::Escape);

    for record in rimz::harness::run::list(store.paths())? {
        if record.kind != request.kind
            || record.agent_id.as_ref() != Some(&request.agent_id)
            || record.status.is_terminal()
        {
            continue;
        }
        let (record, wrote) =
            rimz::harness::run::budget_exceeded(store.paths(), &record.run_id, at_cost)?;
        if wrote {
            let _ = rimz::harness::run_wake::wake_run(store.runtime_paths(), &record);
        }
    }
    interrupted.context("interrupting over-budget agent")
}
