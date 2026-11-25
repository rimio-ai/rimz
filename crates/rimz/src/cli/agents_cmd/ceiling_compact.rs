//! Detached RimZ-enforced auto-compact ceiling delivery, rechecked against fresh state.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::agents::ProviderCapacity;
use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::soft_compact::{CeilingCompactRequest, ceiling_due};
use rimz::ids::MessageId;
use rimz::message::soft_compact::{self, ReminderOutcome};
use rimz::store::message::AutoCompact;
use rimz::store::snapshot::find_agent;

use super::Ctx;

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
    if rimz::message::compact::refuse_repeat(&ctx.store, agent, now).is_err() {
        return Ok(());
    }
    let command = rimz::agents::spec_by_kind(agent.kind.as_str())
        .and_then(|spec| spec.launch.compact_command)
        .map(|compact| compact.command.to_owned())
        .context("ceiling-compaction adapter has no compact command")?;
    let message_id = MessageId::new();
    let outcome = soft_compact::send_ceiling_compact(
        &ctx.workspace,
        &ctx.store,
        message_id.clone(),
        agent,
        command,
        config.resume.auto_continue_text.trim().to_owned(),
        threshold,
        &request.pane_id,
    );
    let error = match &outcome {
        Ok(ReminderOutcome::Sent) => None,
        Ok(ReminderOutcome::Missed(reason)) => Some(reason.clone()),
        Err(error) => Some(error.to_string()),
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
            delivered: matches!(outcome, Ok(ReminderOutcome::Sent)),
            error,
        },
    });
    outcome.context("sending ceiling compaction").map(|_| ())
}
