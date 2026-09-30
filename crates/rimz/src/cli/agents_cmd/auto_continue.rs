//! `rimz agents auto-continue` — the hidden helper the sidebar producer spawns to
//! resume a parked agent when its class-specific condition is due.
//!
//! The producer decides *which* agent and *when* (`sidebar::enrich`
//! auto-continue, opt-in via `[resume] auto_continue*`); this helper performs the
//! side effect the sidebar's read-only import graph must not: it queues or
//! redelivers a resume-gated message through the shared delivery pipeline.
//! Best-effort by contract — it inherits the producer's frame-validated target,
//! so a vanished pane leaves a message error instead of a false resume audit.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::harness::AutoContinueRequest;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::message::synthetic::{self, SyntheticMessage};
use rimz::store::message::{DeliveryGate, MessageSender};
use rimz::store::snapshot::find_agent;
use rimz::store::writer::DeliveryFailureDisposition;

use super::Ctx;

pub fn run_auto_continue(request: AutoContinueRequest) -> Result<()> {
    let text = request.text.trim();
    if text.is_empty() {
        return Ok(());
    }

    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let snapshot = ctx
        .resolution_snapshot_with_context()
        .context("reading auto-continue delivery snapshot")?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let agent = find_agent(&snapshot.agents, &request.kind, &request.agent_id)
        .context("auto-continue target agent is no longer in the rollup")?;
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind == request.kind
                && pane.agent_id.as_ref() == Some(&request.agent_id)
                && pane.pane_id == request.pane_id
        })
        .context("auto-continue target pane is no longer bound to the agent")?;

    let reason = format!("resume delivery gate closed ({})", request.reason);
    let (message_id, outcome) = match request.message_id {
        Some(message_id) => {
            let outcome = synthetic::attempt_now(
                workspace,
                store,
                &message_id,
                Some(&request.pane_id),
                DeliveryFailureDisposition::Retry,
                &reason,
            )
            .context("delivering auto-continue resume message");
            (message_id, outcome)
        }
        None => {
            let gate = if request.reason == "budget_day_reset" {
                DeliveryGate::Done
            } else {
                DeliveryGate::Resume
            };
            let message = SyntheticMessage {
                agent,
                text: text.to_owned(),
                sender: MessageSender::System,
                gate,
                pane_id: Some(request.pane_id),
            }
            .record(workspace);
            let outcome = synthetic::deliver_now(
                workspace,
                store,
                &message,
                DeliveryFailureDisposition::Retry,
                &reason,
            )
            .context("queueing and delivering auto-continue resume message");
            (message.message_id, outcome)
        }
    };
    let delivered = matches!(outcome, Ok(true));
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::AutoContinue {
            kind: request.kind,
            agent_id: request.agent_id,
            label: request.label,
            park: request.reason,
            parked_since: Some(request.parked_since),
            delivered,
            message_id: message_id.to_string(),
        },
    });
    outcome?;
    Ok(())
}
