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
use std::time::Duration;

use rimz::harness::AutoContinueRequest;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::message::{DeliveryGate, deliver};

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
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == request.kind && agent.agent_id == request.agent_id)
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

    let message_id = if let Some(message_id) = request.message_id {
        message_id
    } else {
        let gate = if request.reason == "budget_day_reset" {
            DeliveryGate::Done
        } else {
            DeliveryGate::Resume
        };
        deliver::queue_nudge(
            workspace,
            store,
            agent,
            text.to_owned(),
            gate,
            Some(&request.pane_id),
        )
        .context("queueing auto-continue resume message")?
    };
    let delivered = deliver::deliver_one(
        workspace,
        store,
        &message_id,
        Duration::ZERO,
        Some(request.pane_id.mux()),
        deliver::DeliveryPolicy::Boundary,
    )
    .context("delivering auto-continue resume message")?;
    let delivery_failure = if !delivered {
        let failure_reason = format!("resume delivery gate closed ({})", request.reason);
        store
            .record_message_delivery_failures(
                std::slice::from_ref(&message_id),
                None,
                rimz::store::writer::DeliveryFailureDisposition::Retry,
                &failure_reason,
                &workspace.session_name,
            )
            .context("recording auto-continue delivery miss")
            .err()
    } else {
        None
    };
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
    if let Some(err) = delivery_failure {
        return Err(err);
    }
    Ok(())
}
