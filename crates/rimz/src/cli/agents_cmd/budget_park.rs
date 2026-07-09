//! Hidden helper that interrupts an over-budget agent and settles its run.

use anyhow::{Context, Result};
use clap::Args;

use rimz::harness::budget::read_ledger;
use rimz::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use rimz::mux::{NamedKey, press_pane_key};
use rimz::store::workspace_record;
use rimz::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

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
}

pub fn run_budget_park(args: BudgetParkArgs) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let kind = AgentKind::new_unchecked(args.kind);
    let agent_id = AgentSessionId::from(args.agent_id.as_str());
    let pane_id = PaneId::parse(&args.pane).context("parsing pane id")?;
    let runtime = RuntimePaths::for_workspace(workspace_id.clone())
        .context("preparing budget runtime paths")?;
    let paths =
        StatePaths::for_workspace(workspace_id.clone()).context("preparing budget state paths")?;
    let workspace_record = workspace_record::read(&paths.workspace_record)
        .context("reading budget workspace record")?;
    let store = Store::open(paths, runtime).context("opening budget store")?;
    let workspace = ResolvedWorkspace {
        workspace_id,
        project_root: workspace_record.project_root.clone(),
        root_class: workspace_record.root_class,
        worktree_root: workspace_record.project_root,
        worktree_branch: None,
        session_name: workspace_record.session_name,
        mux_hint: Some(pane_id.mux()),
    };
    let snapshot =
        rimz::sidebar::produce::resolution_snapshot(&workspace, &store, Some(pane_id.mux()))
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

    let ledger = read_ledger(store.runtime_paths(), &kind, &agent_id)
        .context("budget ledger disappeared before the interrupt")?;
    let at_cost = ledger.parked.as_ref().map_or(0.0, |park| park.at_cost);
    let interrupted = press_pane_key(&pane_id, NamedKey::Escape);

    for record in rimz::harness::run::list(store.paths())? {
        if record.kind != kind
            || record.agent_id.as_ref() != Some(&agent_id)
            || record.status.is_terminal()
        {
            continue;
        }
        let (record, wrote) =
            rimz::harness::run::budget_exceeded(store.paths(), &record.run_id, Some(at_cost))?;
        if wrote {
            let _ = rimz::store::wakeup::wake_run(store.runtime_paths(), &record);
        }
    }
    interrupted.context("interrupting over-budget agent")
}
