//! Supervised-run verification effects.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use rimz::harness::run::RunRecord;
use rimz::harness::schedule::runner::{CheckEcho, CheckOutcome, run_check};
use rimz::message::{DeliveryGate, deliver};

use super::pane;

pub(crate) fn run_verify(cwd: &Path, cmd: &str, cap: Duration) -> Result<CheckOutcome> {
    run_check(cwd, cmd, cap, CheckEcho::Capture)
}

pub(crate) fn deliver_reprompt(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    record: &RunRecord,
    text: String,
) -> Result<()> {
    let pane = pane::resolve_run_pane(store, &workspace.session_name, record)
        .context("resolving verify re-prompt pane")?;
    let snapshot =
        rimz::sidebar::produce::resolution_snapshot(workspace, store, Some(pane.pane_id.mux()))
            .context("reading verify re-prompt delivery snapshot")?;
    let agent_id = record
        .agent_id
        .as_ref()
        .context("verify re-prompt run has no bound agent session")?;
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == record.kind && &agent.agent_id == agent_id)
        .context("verify re-prompt target agent is no longer in the rollup")?;
    let (_, delivered) = deliver::nudge_now(
        workspace,
        store,
        agent,
        text,
        DeliveryGate::Any,
        &pane.pane_id,
    )
    .context("delivering verify re-prompt")?;
    if !delivered {
        bail!("verify re-prompt was queued but not delivered")
    }
    Ok(())
}
