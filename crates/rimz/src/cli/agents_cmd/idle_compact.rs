//! `rimz agents idle-compact` — the hidden helper the sidebar producer spawns
//! to compact an eligible idle agent through the durable message path.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::Timestamp;

use rimz::agents::AgentStatus;
use rimz::config::{IdleCompactMode, MachineConfig};
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use rimz::message::{
    DeliveryGate, MessageBody, MessageRecord, MessageSender, deliver, send::already_compacted_at,
};

use super::Ctx;

#[derive(Debug, Args)]
pub struct IdleCompactArgs {
    #[arg(long)]
    workspace_id: String,
    #[arg(long)]
    kind: String,
    #[arg(long)]
    agent_id: String,
    /// Normalized pane id (`tmux:%3`, `zellij:terminal_3`).
    #[arg(long)]
    pane: String,
    #[arg(long)]
    command: String,
    #[arg(long)]
    occupied_tokens: u64,
    #[arg(long)]
    label: Option<String>,
}

pub fn run_idle_compact(args: IdleCompactArgs) -> Result<()> {
    let workspace_id: WorkspaceId = args.workspace_id.parse().context("parsing workspace id")?;
    let pane_id = PaneId::parse(&args.pane).context("parsing pane id")?;
    let kind = AgentKind::new_unchecked(args.kind.clone());
    let agent_id = AgentSessionId::from(args.agent_id.as_str());
    let expected_command = rimz::agents::spec_by_kind(kind.as_str())
        .and_then(|spec| spec.launch.compact_command())
        .context("idle-compaction target adapter has no compact command")?;
    if args.command != expected_command {
        bail!(
            "idle-compaction command `{}` does not match {} adapter command `{expected_command}`",
            args.command,
            kind
        );
    }

    let config = MachineConfig::load_lenient();
    if config.harness.idle_compact == IdleCompactMode::Off {
        return Ok(());
    }

    let ctx = Ctx::for_workspace(workspace_id.clone(), Some(pane_id.mux()))?;
    let snapshot = ctx
        .resolution_snapshot_with_context()
        .context("reading idle-compaction delivery snapshot")?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == kind && agent.agent_id == agent_id)
        .context("idle-compaction target agent is no longer in the rollup")?;
    snapshot
        .agent_panes
        .iter()
        .find(|pane| {
            pane.kind == kind
                && pane.agent_id.as_ref() == Some(&agent_id)
                && pane.pane_id == pane_id
        })
        .context("idle-compaction target pane is no longer bound to the agent")?;

    let idle_after = config.harness.idle_compact_after();
    let idle_secs = Timestamp::now().as_second() - agent.last_activity.as_second();
    if agent.parent_agent_id.is_some()
        || agent.agent_id.is_empty()
        || agent.compacting_since.is_some()
        || agent.budget_park.is_some()
        || agent.is_awaiting_input()
        || !matches!(
            agent.effective_status(),
            AgentStatus::Idle | AgentStatus::Success
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
    if args.occupied_tokens != occupied_tokens {
        tracing::debug!(
            producer_occupied = args.occupied_tokens,
            current_occupied = occupied_tokens,
            "idle-compaction context reading changed before helper validation",
        );
    }
    if already_compacted_at(store, agent, expected_command, occupied_tokens)
        || latest_delivered_was_compaction(
            &store
                .list_message_history()
                .context("reading idle-compaction message history")?,
            agent,
            expected_command,
        )
    {
        return Ok(());
    }

    let mut message = MessageRecord::new(
        workspace_id,
        agent,
        expected_command.to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_channel(rimz::harness::target::agent_channel(agent))
    .with_sender(MessageSender::System)
    .with_automated(true)
    .with_body(MessageBody::Command)
    .with_pane_id(pane_id.clone());
    message.compacted_context_tokens = Some(occupied_tokens);
    let message_id = message.message_id.clone();
    if let Err(err) = store.queue_message(&message, &workspace.session_name) {
        append_assist(
            &args,
            kind,
            agent_id,
            idle_secs,
            occupied_tokens,
            &message_id,
            false,
            Some(err.to_string()),
        );
        return Err(err).context("queueing idle-compaction command");
    }

    let delivered = match deliver::deliver_one(
        workspace,
        store,
        &message_id,
        Duration::ZERO,
        Some(pane_id.mux()),
        deliver::DeliveryPolicy::Boundary,
    ) {
        Ok(delivered) => delivered,
        Err(err) => {
            append_assist(
                &args,
                kind,
                agent_id,
                idle_secs,
                occupied_tokens,
                &message_id,
                false,
                Some(err.to_string()),
            );
            return Err(err).context("delivering idle-compaction command");
        }
    };
    let delivery_error = if delivered {
        None
    } else {
        let reason = "idle-compaction delivery gate closed".to_owned();
        match store.record_message_delivery_failures(
            std::slice::from_ref(&message_id),
            None,
            rimz::store::DeliveryFailureDisposition::Retry,
            &reason,
            &workspace.session_name,
        ) {
            Ok(_) => Some(reason),
            Err(err) => Some(format!("{reason}; recording retry failed: {err}")),
        }
    };
    append_assist(
        &args,
        kind,
        agent_id,
        idle_secs,
        occupied_tokens,
        &message_id,
        delivered,
        delivery_error.clone(),
    );
    if let Some(error) = delivery_error.filter(|error| error.contains("recording retry failed")) {
        bail!("{error}");
    }
    Ok(())
}

fn latest_delivered_was_compaction(
    history: &[MessageRecord],
    agent: &rimz::agents::AgentState,
    command: &str,
) -> bool {
    history
        .iter()
        .filter(|message| {
            message.status == rimz::message::MessageStatus::Delivered
                && message.same_agent_card(agent)
        })
        .max_by(|left, right| left.message_id.as_str().cmp(right.message_id.as_str()))
        .is_some_and(|message| {
            message.body == MessageBody::Command
                && message.text == command
                && message.compacted_context_tokens.is_some()
        })
}

#[allow(clippy::too_many_arguments)]
fn append_assist(
    args: &IdleCompactArgs,
    kind: AgentKind,
    agent_id: AgentSessionId,
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
            label: args.label.clone(),
            idle_secs: idle_secs.max(0) as u64,
            occupied_tokens,
            message_id: message_id.to_string(),
            delivered,
            error,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delivered(
        agent: &rimz::agents::AgentState,
        text: &str,
        body: MessageBody,
        tokens: Option<u64>,
    ) -> MessageRecord {
        let mut message = MessageRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            agent,
            text.to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_body(body);
        message.status = rimz::message::MessageStatus::Delivered;
        message.compacted_context_tokens = tokens;
        message
    }

    #[test]
    fn only_a_latest_delivered_compaction_suppresses_the_idle_reflex() {
        let agent = rimz::agents::AgentState::stub("claude", "session-1", AgentStatus::Idle);
        let compact = delivered(&agent, "/compact", MessageBody::Command, Some(80_000));
        assert!(latest_delivered_was_compaction(
            std::slice::from_ref(&compact),
            &agent,
            "/compact"
        ));

        let prompt = delivered(&agent, "continue", MessageBody::Prompt, None);
        assert!(!latest_delivered_was_compaction(
            &[compact, prompt],
            &agent,
            "/compact"
        ));
    }
}
