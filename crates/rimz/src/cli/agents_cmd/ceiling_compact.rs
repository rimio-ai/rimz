//! Detached RimZ-enforced auto-compact ceiling delivery, rechecked against fresh state.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::agents::ProviderCapacity;
use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::soft_compact::{CeilingCompactRequest, ceiling_due};
use rimz::ids::MessageId;
use rimz::message::compact::{CompactErr, CompactRequest, ReceiptOutcome};
use rimz::store::message::{AutoCompact, MessageSender};
use rimz::store::snapshot::find_agent;

use super::Ctx;

/// Hard-compacts the requested agent when the ceiling is still due on fresh state, through the
/// path `rimz agents compact --hard` takes, and appends one assist record for the attempt. The
/// command holds only while the open turn reads at or over the line, until RimZ's own key.
pub(super) fn run(request: CeilingCompactRequest) -> Result<()> {
    let config = MachineConfig::load_lenient();
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let snapshot = ctx
        .published_snapshot()
        .context("reading ceiling-compaction snapshot")?;
    let Some(agent) = find_agent(&snapshot.agents, &request.kind, &request.agent_id) else {
        return Ok(());
    };
    let now = Timestamp::now();
    if snapshot
        .live_agent_pane(&request.kind, &request.agent_id)
        .as_ref()
        != Some(&request.pane_id)
    {
        return Ok(());
    }
    let Some(threshold) = ceiling_due(
        &snapshot,
        agent,
        &config.resume.auto_continue_text,
        now,
        || ProviderCapacity::login_compaction_blocked(ctx.runtime(), agent, now),
    ) else {
        return Ok(());
    };
    let command = rimz::agents::compact_command(agent, &config.harness)
        .context("ceiling-compaction adapter has no compact command")?;
    let queued_id = MessageId::new();
    let outcome = rimz::message::compact::compact_at_ceiling(
        &ctx.workspace,
        &ctx.store,
        CompactRequest {
            message_id: queued_id.clone(),
            agent,
            pane_id: request.pane_id.clone(),
            command,
            sender: MessageSender::System,
            automated: true,
            idle: false,
            premises: vec![rimz::store::message::Premise::OverCeiling { threshold }],
            continue_text: Some(config.resume.auto_continue_text.trim().to_owned()),
            hard: true,
        },
    );
    let mut failed = None;
    let (message_id, error) = match outcome {
        Ok((message_id, ReceiptOutcome::Sent { .. })) => (message_id, None),
        Ok((message_id, ReceiptOutcome::Queued { .. } | ReceiptOutcome::Joined { .. })) => {
            let reason = untyped_reason(&ctx.store, &message_id);
            (message_id, Some(reason))
        }
        // Another compaction waits or ran since the recheck: nothing fired.
        Err(CompactErr::Compacting | CompactErr::Pending { .. } | CompactErr::Repeated { .. }) => {
            return Ok(());
        }
        Err(CompactErr::Settled { message_id, reason }) => (message_id, Some(reason)),
        Err(error) => {
            let reason = error.to_string();
            failed = Some(error);
            (queued_id, Some(reason))
        }
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::CeilingCompact {
            kind: request.kind,
            agent_id: request.agent_id,
            label: Some(request.label),
            threshold: AutoCompact::Tokens(threshold),
            occupied_tokens: agent.occupied_context_tokens(),
            message_id: message_id.to_string(),
            delivered: error.is_none(),
            error,
        },
    });
    failed.map_or(Ok(()), |error| {
        Err(error).context("sending ceiling compaction")
    })
}

/// Why the hard attempt typed nothing, for a command left waiting for the turn end: the miss its
/// record carries, else that it waits.
fn untyped_reason(store: &rimz::Store, message_id: &MessageId) -> String {
    store
        .list_messages()
        .ok()
        .and_then(|live| {
            live.into_iter()
                .find(|record| record.message_id == *message_id)
                .and_then(|record| record.last_error)
        })
        .unwrap_or_else(|| "not typed at once; waits for the turn boundary".to_owned())
}
