//! Hidden helper that interrupts an over-budget agent and settles its run.

use anyhow::{Context, Result, bail};

use rimz::harness::budget::BudgetParkRequest;
use rimz::launcher::agent_pane::FindErr;
use rimz::pane::keys::NamedKey;

use super::Ctx;

const UNBOUND: &str = "budget target pane is no longer bound to the agent";

pub fn run_budget_park(request: BudgetParkRequest) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id, Some(request.pane_id.mux()))?;
    let store = &ctx.store;
    let snapshot = ctx
        .resolution_snapshot()
        .context("reading budget interrupt snapshot")?;
    let agent =
        rimz::store::snapshot::find_agent(&snapshot.agents, &request.kind, &request.agent_id)
            .context(UNBOUND)?;
    let mut notices = Vec::new();
    let found = rimz::launcher::agent_pane::open_writer(
        &ctx.workspace,
        store,
        ctx.mux(),
        &agent.kind,
        agent.launch_key(),
        &mut notices,
    );
    crate::cli::render::room::print_notices(notices)?;
    // The ledger is the session's: a pane that holds another session of its launch, an agent
    // that moved, or a pane nobody can read presses nothing and settles nothing, and the
    // producer's next due interrupt retries. Only a failed lock keeps the settlement.
    let writer = match found {
        Ok((pane, writer)) if rimz::launcher::swap::held_by(&pane, agent) => Ok(writer),
        Ok(_) | Err(FindErr::Nowhere) => bail!(UNBOUND),
        Err(FindErr::Lock { source, .. }) => Err(source),
        Err(err) => return Err(err).context("finding the over-budget agent's pane"),
    };

    let at_cost = request.at_cost.or_else(|| {
        rimz::harness::budget::read_ledger(store.runtime_paths(), &request.kind, &request.agent_id)
            .and_then(|ledger| ledger.parked.map(|park| park.at_cost))
    });
    let interrupted = writer.and_then(|writer| writer.press(NamedKey::Escape));

    for record in rimz::harness::run::list(store.paths())? {
        if record.kind != request.kind
            || record.agent_id.as_ref() != Some(&request.agent_id)
            || record.status.is_terminal()
        {
            continue;
        }
        let (record, wrote) = rimz::harness::run::budget_exceeded(
            store.paths(),
            store.runtime_paths(),
            &record.run_id,
            at_cost,
        )?;
        if wrote {
            rimz::store::run::wake_run(store.runtime_paths(), &record);
        }
    }
    interrupted.context("interrupting over-budget agent")
}
