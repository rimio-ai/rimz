//! Hidden helper that interrupts an over-budget agent and settles its run.

use anyhow::{Context, Result};
use clap::Args;

use rimz::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use rimz::mux::{NamedKey, press_pane_key};

use super::Ctx;

#[derive(Debug, Args)]
pub struct BudgetParkArgs {
    #[arg(long)]
    workspace_id: String,
    #[arg(long)]
    kind: String,
    #[arg(long)]
    agent_id: String,
    #[arg(long)]
    pane: String,
    #[arg(long)]
    at_cost: Option<f64>,
}

pub fn run_budget_park(args: BudgetParkArgs) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let kind = AgentKind::new_unchecked(args.kind);
    let agent_id = AgentSessionId::from(args.agent_id.as_str());
    let pane_id = PaneId::parse(&args.pane).context("parsing pane id")?;
    let ctx = Ctx::for_budget_workspace(workspace_id, Some(pane_id.mux()))?;
    let store = &ctx.store;
    let snapshot = ctx
        .resolution_snapshot()
        .context("reading budget interrupt snapshot")?;
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind == kind
                && pane.agent_id.as_ref() == Some(&agent_id)
                && pane.pane_id == pane_id
        })
        .context("budget target pane is no longer bound to the agent")?;

    let at_cost = args.at_cost.or_else(|| {
        rimz::harness::budget::read_ledger(store.runtime_paths(), &kind, &agent_id)
            .and_then(|ledger| ledger.parked.map(|park| park.at_cost))
    });
    let interrupted = press_pane_key(&pane_id, NamedKey::Escape);

    for record in rimz::harness::run::list(store.paths())? {
        if record.kind != kind
            || record.agent_id.as_ref() != Some(&agent_id)
            || record.status.is_terminal()
        {
            continue;
        }
        let (record, wrote) =
            rimz::harness::run::budget_exceeded(store.paths(), &record.run_id, at_cost)?;
        if wrote {
            let _ = rimz::store::wakeup::wake_run(store.runtime_paths(), &record);
        }
    }
    interrupted.context("interrupting over-budget agent")
}
