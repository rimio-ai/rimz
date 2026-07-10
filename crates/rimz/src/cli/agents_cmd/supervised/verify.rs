use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use rimz::harness::run::RunRecord;
use rimz::harness::schedule::runner::{CheckEcho, CheckOutcome, run_check};
use rimz::message::{DeliveryGate, MessageRecord, MessageSender, deliver};

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
    let message = MessageRecord::new(
        workspace.workspace_id.clone(),
        agent,
        text,
        true,
        DeliveryGate::Any,
    )
    .with_channel(rimz::harness::target::agent_channel(agent))
    .with_sender(MessageSender::Human)
    .with_pane_id(pane.pane_id.clone());
    let message_id = message.message_id.clone();
    store
        .queue_message(&message, &workspace.session_name)
        .context("queueing verify re-prompt")?;
    let delivered = deliver::deliver_one(
        workspace,
        store,
        &message_id,
        Duration::ZERO,
        Some(pane.pane_id.mux()),
        deliver::DeliveryPolicy::Boundary,
    )
    .context("delivering verify re-prompt")?;
    if !delivered {
        bail!("verify re-prompt was queued but not delivered")
    }
    Ok(())
}
