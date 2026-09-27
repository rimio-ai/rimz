//! Detached prompt-cache ping. Revalidate the producer's anchor before attempting delivery.

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::cache_keepalive::{CacheKeepaliveRequest, prompt};
use rimz::message::deliver;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
use rimz::store::writer::DeliveryFailureDisposition;

use super::Ctx;

pub(super) fn run(request: CacheKeepaliveRequest) -> Result<()> {
    let config = rimz::config::MachineConfig::load_lenient();
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let snapshot = ctx
        .published_snapshot()
        .context("reading cache-keepalive snapshot")?;
    let now = Timestamp::now();
    let Some(agent) = request.target(&snapshot, &config.harness, now) else {
        return Ok(());
    };
    let message = MessageRecord::new(
        request.workspace_id.clone(),
        agent,
        prompt(agent, now),
        DeliveryGate::Done,
    )
    .with_channel(agent.channel())
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::CacheKeepalive,
    })
    .with_pane_id(request.pane_id);
    let outcome = deliver::deliver_now(&ctx.workspace, &ctx.store, &message);
    let delivered = matches!(outcome, Ok(true));
    let error = match &outcome {
        Ok(true) => None,
        Ok(false) => Some("cache keepalive delivery gate closed".to_owned()),
        Err(err) => Some(err.to_string()),
    };
    let finalized = if let Some(error) = &error {
        ctx.store
            .record_message_delivery_failures(
                std::slice::from_ref(&message.message_id),
                None,
                DeliveryFailureDisposition::Terminal,
                error,
                &ctx.workspace.session_name,
            )
            .map(|_| ())
    } else {
        Ok(())
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::CacheKeepalive {
            kind: request.kind,
            agent_id: request.agent_id,
            label: Some(request.label),
            idle_secs: now.duration_since(request.anchor).as_secs().max(0) as u64,
            waits: agent.pending_waits.len(),
            message_id: message.message_id.to_string(),
            delivered,
            error,
        },
    });
    finalized.context("finalizing missed cache keepalive")?;
    outcome.context("delivering cache keepalive")?;
    Ok(())
}
