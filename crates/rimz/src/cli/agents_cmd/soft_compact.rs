//! Detached smart-compaction reminder delivery, rechecked against the published card.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::agents::ProviderCapacity;
use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::cache_keepalive::SeatPolicy;
use rimz::harness::soft_compact::{SoftCompactRequest, reminder_due};
use rimz::message::soft_compact::{self, ReminderOutcome};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, MessageSender, SoftCompactStep};
use rimz::store::snapshot::find_agent;

use super::Ctx;

pub(super) fn run(request: SoftCompactRequest) -> Result<()> {
    // An older host's continue request: the continue now follows a compaction command.
    if request.step != SoftCompactStep::WrapUp {
        return Ok(());
    }
    let config = MachineConfig::load_lenient();
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let policy = SeatPolicy::load(&config, Some(&ctx.workspace.project_root));
    let snapshot = ctx
        .published_snapshot()
        .context("reading smart-compaction snapshot")?;
    let Some(agent) = find_agent(&snapshot.agents, &request.kind, &request.agent_id) else {
        return Ok(());
    };
    let threshold = policy.smart_compact(agent, &config.harness);
    let now = Timestamp::now();
    if snapshot
        .live_agent_pane(&request.kind, &request.agent_id)
        .as_ref()
        != Some(&request.pane_id)
        || !reminder_due(
            &snapshot,
            agent,
            threshold,
            &config.resume.auto_continue_text,
            now,
            || ProviderCapacity::login_compaction_blocked(ctx.runtime(), agent, now),
        )
    {
        return Ok(());
    }
    let threshold = threshold.context("smart-compaction reminder has no threshold")?;
    let text = rimz::agents::compact_reminder(agent)
        .context("smart-compaction adapter has no compact command")?;
    let mut message = SyntheticMessage {
        agent,
        text,
        sender: MessageSender::System,
        gate: DeliveryGate::Any,
        premises: rimz::store::message::Premise::NONE,
    }
    .record(&ctx.workspace)
    .with_automated(true);
    message.soft_compact = Some(SoftCompactStep::WrapUp);
    let outcome =
        soft_compact::send_reminder(&ctx.workspace, &ctx.store, &message, &request.pane_id);
    let error = match &outcome {
        Ok(ReminderOutcome::Sent) => None,
        Ok(ReminderOutcome::Missed(reason)) => Some(reason.clone()),
        Err(error) => Some(error.to_string()),
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::SoftCompact {
            kind: request.kind,
            agent_id: request.agent_id,
            label: Some(request.label),
            threshold,
            occupied_tokens: agent.occupied_context_tokens(),
            message_id: message.message_id.to_string(),
            delivered: matches!(outcome, Ok(ReminderOutcome::Sent)),
            error,
        },
    });
    outcome
        .context("sending smart-compaction reminder")
        .map(|_| ())
}
