//! `rimz agents idle-compact` — the hidden helper the sidebar producer spawns
//! to compact an eligible idle agent through the durable message path.

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::idle_compact::{
    IdleCompactRequest, fire_point, resolve_mode, resolve_teams, should_compact,
};
use rimz::ids::MessageId;
use rimz::message::compact::{
    CompactErr, CompactOutcome, CompactRequest, refuse_repeat, send_compact,
};
use rimz::message::send::already_compacted_at;
use rimz::store::message::MessageSender;

use super::Ctx;

pub fn run_idle_compact(request: IdleCompactRequest) -> Result<()> {
    let config = MachineConfig::load_lenient();
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
    let teams = resolve_teams(&config, Some(&workspace.project_root));
    let mode = resolve_mode(agent, &teams, config.harness.idle_compact);
    let account =
        rimz::sidebar::refresh::accounts::cached_account(ctx.runtime(), &agent.login_key());
    let window = fire_point(agent, mode, account.as_ref());
    let command = rimz::agents::compact_command(agent, &config.harness);
    let now = Timestamp::now();
    if !should_compact(agent, command.as_deref(), window, now) {
        return Ok(());
    }
    let expected_command =
        command.context("idle-compaction target adapter has no compact command")?;
    if request.command != expected_command {
        bail!(
            "idle-compaction command `{}` does not match {} adapter command `{expected_command}`",
            request.command,
            request.kind
        );
    }
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind == request.kind
                && pane.agent_id.as_ref() == Some(&request.agent_id)
                && pane.pane_id == request.pane_id
        })
        .context("idle-compaction target pane is no longer bound to the agent")?;

    let (Some(occupied_tokens), Some(turn_ended_at)) =
        (agent.occupied_context_tokens(), agent.turn_ended_at)
    else {
        return Ok(());
    };
    let idle_secs = now.as_second() - turn_ended_at.as_second();
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
        window.map(|window| window.fire_after.as_secs()),
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
    idle_after_secs: Option<u64>,
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
            idle_after_secs,
            occupied_tokens,
            message_id: message_id.to_string(),
            delivered,
            error,
        },
    });
}
