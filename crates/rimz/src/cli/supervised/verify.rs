//! Supervised-run verification effects.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use rimz::harness::schedule::runner::{CheckEcho, CheckOutcome, run_check};
use rimz::message::synthetic::{self, SyntheticMessage};
use rimz::store::message::{DeliveryGate, MessageSender};
use rimz::store::run::RunRecord;
use rimz::store::snapshot::find_agent;
use rimz::store::writer::DeliveryFailureDisposition;

use super::pane;

pub(super) fn run_verify(cwd: &Path, cmd: &str, cap: Duration) -> Result<CheckOutcome> {
    run_check(
        cwd,
        cmd,
        cap,
        CheckEcho::Capture,
        &std::collections::BTreeMap::new(),
    )
}

pub(super) fn deliver_reprompt(
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
    let agent = find_agent(&snapshot.agents, &record.kind, agent_id)
        .context("verify re-prompt target agent is no longer in the rollup")?;
    let message = SyntheticMessage {
        agent,
        text,
        sender: MessageSender::System,
        gate: DeliveryGate::Any,
        pane_id: Some(pane.pane_id),
    }
    .record(workspace);
    let delivered = synthetic::deliver_now(
        workspace,
        store,
        &message,
        DeliveryFailureDisposition::Retry,
        "verify re-prompt delivery gate closed",
    )
    .context("delivering verify re-prompt")?;
    if !delivered {
        bail!("verify re-prompt was queued but not delivered")
    }
    Ok(())
}
