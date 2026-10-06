//! Detached prompt-cache ping. Revalidate the producer's anchor before attempting delivery.

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::cache_keepalive::{CacheKeepaliveRequest, KeepWarmPolicy, final_ping, prompt};
use rimz::message::synthetic::{self, SyntheticMessage};
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};
use rimz::store::writer::DeliveryFailureDisposition;

use super::Ctx;

const CLOSED_GATE: &str = "cache keepalive delivery gate closed";

pub(super) fn run(request: CacheKeepaliveRequest) -> Result<()> {
    let config = rimz::config::MachineConfig::load_lenient();
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let snapshot = ctx
        .published_snapshot()
        .context("reading cache-keepalive snapshot")?;
    let policy = KeepWarmPolicy::load(&config, Some(&ctx.workspace.project_root));
    let now = Timestamp::now();
    let Some((agent, holding)) = request.target(&snapshot, &config.harness, &policy, now) else {
        return Ok(());
    };
    let limit = final_ping(agent, &config.harness, now);
    let message = SyntheticMessage {
        agent,
        text: prompt(agent, now, limit),
        sender: MessageSender::Harness {
            notice: HarnessNotice::CacheKeepalive,
        },
        gate: DeliveryGate::Done,
        pane_id: Some(request.pane_id),
    }
    .record(&ctx.workspace);
    let outcome = synthetic::deliver_now(
        &ctx.workspace,
        &ctx.store,
        &message,
        DeliveryFailureDisposition::Terminal,
        CLOSED_GATE,
    );
    let delivered = matches!(outcome, Ok(true));
    let error = match &outcome {
        Ok(true) => None,
        Ok(false) => Some(CLOSED_GATE.to_owned()),
        Err(err) => Some(err.to_string()),
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::CacheKeepalive {
            kind: request.kind,
            agent_id: request.agent_id,
            label: Some(request.label),
            idle_secs: now.duration_since(request.anchor).as_secs().max(0) as u64,
            waits: agent.pending_waits.len(),
            horizon_secs: holding.map(|horizon| horizon.as_secs()),
            message_id: message.message_id.to_string(),
            delivered,
            error,
            capped: limit.is_some(),
        },
    });
    outcome.context("delivering cache keepalive")?;
    Ok(())
}
