//! Detached soft-compaction delivery, rechecked against the published card.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::cache_keepalive::SeatPolicy;
use rimz::harness::soft_compact::{SoftCompactRequest, due_step};
use rimz::message::soft_compact::{self, WrapUpOutcome};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, MessageSender, SoftCompactStep};
use rimz::store::snapshot::find_agent;

use super::Ctx;

pub(super) fn run(request: SoftCompactRequest) -> Result<()> {
    let config = MachineConfig::load_lenient();
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let policy = SeatPolicy::load(&config, Some(&ctx.workspace.project_root));
    let snapshot = ctx
        .published_snapshot()
        .context("reading soft-compaction snapshot")?;
    let Some(agent) = find_agent(&snapshot.agents, &request.kind, &request.agent_id) else {
        return Ok(());
    };
    let threshold = policy.smart_compact(agent, &config.harness);
    let continue_text = config.resume.auto_continue_text.trim();
    if snapshot
        .live_agent_pane(&request.kind, &request.agent_id)
        .as_ref()
        != Some(&request.pane_id)
        || due_step(&snapshot, agent, threshold, continue_text, Timestamp::now())
            != Some(request.step)
    {
        return Ok(());
    }
    let text = match request.step {
        SoftCompactStep::WrapUp => rimz::agents::soft_compact_prompt(agent, &config.harness)
            .context("soft-compaction adapter has no compact command")?,
        SoftCompactStep::Continue => continue_text.to_owned(),
    };
    let mut message = SyntheticMessage {
        agent,
        text,
        sender: MessageSender::System,
        gate: match request.step {
            SoftCompactStep::WrapUp => DeliveryGate::Any,
            SoftCompactStep::Continue => DeliveryGate::Done,
        },
        pane_id: Some(request.pane_id),
    }
    .record(&ctx.workspace)
    .with_automated(request.step == SoftCompactStep::WrapUp);
    message.soft_compact = Some(request.step);
    if request.step == SoftCompactStep::Continue {
        message.auto_compact = threshold;
        return soft_compact::send_continue(&ctx.workspace, &ctx.store, &message)
            .context("sending soft-compaction continue");
    }
    let threshold = threshold.context("soft-compaction wrap-up has no threshold")?;
    let outcome = soft_compact::send_wrap_up(&ctx.workspace, &ctx.store, &message);
    let error = match &outcome {
        Ok(WrapUpOutcome::Sent) => None,
        Ok(WrapUpOutcome::Missed(reason)) => Some(reason.clone()),
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
            delivered: matches!(outcome, Ok(WrapUpOutcome::Sent)),
            error,
        },
    });
    outcome
        .context("sending soft-compaction wrap-up")
        .map(|_| ())
}
