//! `rimz agents idle-compact` — the hidden helper the sidebar producer spawns
//! to compact an eligible idle agent through the durable message path.

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use rimz::agents::AgentStatus;
use rimz::config::{IdleCompactMode, MachineConfig};
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::idle_compact::IdleCompactRequest;
use rimz::ids::MessageId;
use rimz::message::compact::{
    CompactErr, CompactOutcome, CompactRequest, refuse_repeat, send_compact,
};
use rimz::message::send::already_compacted_at;
use rimz::store::message::MessageSender;

use super::Ctx;

pub fn run_idle_compact(request: IdleCompactRequest) -> Result<()> {
    let config = MachineConfig::load_lenient();
    let expected_command = rimz::agents::spec_by_kind(request.kind.as_str())
        .and_then(|spec| {
            spec.launch
                .compact_command(config.harness.compact_instruction())
        })
        .context("idle-compaction target adapter has no compact command")?;
    if request.command != expected_command {
        bail!(
            "idle-compaction command `{}` does not match {} adapter command `{expected_command}`",
            request.command,
            request.kind
        );
    }

    if config.harness.idle_compact == IdleCompactMode::Off {
        return Ok(());
    }

    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let snapshot = ctx
        .resolution_snapshot_with_context()
        .context("reading idle-compaction delivery snapshot")?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == request.kind && agent.agent_id == request.agent_id)
        .context("idle-compaction target agent is no longer in the rollup")?;
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind == request.kind
                && pane.agent_id.as_ref() == Some(&request.agent_id)
                && pane.pane_id == request.pane_id
        })
        .context("idle-compaction target pane is no longer bound to the agent")?;

    let idle_after = config.harness.idle_compact_after();
    let idle_secs = Timestamp::now().as_second() - agent.last_activity.as_second();
    if agent.is_provider_subagent()
        || agent.agent_id.is_empty()
        || agent.budget_park.is_some()
        || agent.is_awaiting_input()
        || !matches!(
            agent.effective_status(),
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Sleeping
        )
        || idle_secs < idle_after.as_secs().min(i64::MAX as u64) as i64
    {
        return Ok(());
    }
    let Some(occupied_tokens) = agent
        .occupied_context_tokens()
        .filter(|tokens| *tokens >= rimz::harness::idle_compact::IDLE_COMPACT_MIN_TOKENS)
    else {
        return Ok(());
    };
    if request.occupied_tokens != occupied_tokens {
        tracing::debug!(
            producer_occupied = request.occupied_tokens,
            current_occupied = occupied_tokens,
            "idle-compaction context reading changed before helper validation",
        );
    }
    if already_compacted_at(store, agent, occupied_tokens) {
        return Ok(());
    }
    match refuse_repeat(store, agent, Timestamp::now()) {
        Ok(()) => {}
        Err(CompactErr::Compacting | CompactErr::Pending { .. } | CompactErr::Repeated { .. }) => {
            return Ok(());
        }
        Err(err) => return Err(err).context("checking idle-compaction eligibility"),
    }
    let message_id = MessageId::new();
    let outcome = send_compact(
        workspace,
        store,
        CompactRequest {
            message_id: message_id.clone(),
            agent,
            pane_id: request.pane_id,
            command: expected_command,
            sender: MessageSender::System,
            automated: true,
        },
    );
    let delivery_error = match &outcome {
        Ok(CompactOutcome::Sent) => None,
        Ok(CompactOutcome::Queued) => Some("compaction delivery gate closed".to_owned()),
        Err(err) => Some(err.to_string()),
    };
    append_assist(
        &request.label,
        request.kind,
        request.agent_id,
        idle_secs,
        occupied_tokens,
        &message_id,
        matches!(outcome, Ok(CompactOutcome::Sent)),
        delivery_error,
    );
    outcome.context("sending idle-compaction command")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_assist(
    label: &str,
    kind: rimz::ids::AgentKind,
    agent_id: rimz::ids::AgentSessionId,
    idle_secs: i64,
    occupied_tokens: u64,
    message_id: &rimz::ids::MessageId,
    delivered: bool,
    error: Option<String>,
) {
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::IdleCompact {
            kind,
            agent_id,
            label: Some(label.to_owned()),
            idle_secs: idle_secs.max(0) as u64,
            occupied_tokens,
            message_id: message_id.to_string(),
            delivered,
            error,
        },
    });
}
