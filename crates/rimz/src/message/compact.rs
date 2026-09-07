//! Standalone native compaction through the durable, boundary-only command queue.

use std::time::Duration;

use jiff::Timestamp;

use crate::Store;
use crate::agents::AgentState;
use crate::ids::{MessageId, PaneId};
use crate::store::message::{
    DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus,
};
use crate::store::writer::DeliveryFailureDisposition;
use crate::workspace::ResolvedWorkspace;

use super::deliver::{self, DeliveryPolicy};

pub type Result<T> = std::result::Result<T, CompactErr>;

#[derive(Debug, thiserror::Error)]
pub enum CompactErr {
    #[error("already compacting; wait for compaction to finish and take another turn")]
    Compacting,
    #[error("compaction {message_id} is still {status}; a compaction never follows a compaction")]
    Pending {
        message_id: MessageId,
        status: MessageStatus,
    },
    #[error(
        "already compacted at {at} and has not taken a turn since; a compaction never follows a compaction"
    )]
    Repeated { at: Timestamp },
    #[error(
        "compaction {message_id} was settled before delivery; inspect it with `rimz message show {message_id}`"
    )]
    Settled { message_id: MessageId },
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Deliver(#[from] deliver::DeliverErr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    Sent,
    Queued,
}

pub struct CompactRequest<'a> {
    pub message_id: MessageId,
    pub agent: &'a AgentState,
    pub pane_id: PaneId,
    pub command: String,
    pub sender: MessageSender,
    pub automated: bool,
}

pub fn refuse_repeat(store: &Store, agent: &AgentState, now: Timestamp) -> Result<()> {
    if agent.is_compacting(now) {
        return Err(CompactErr::Compacting);
    }
    if let Some(message) = store.list_messages()?.into_iter().find(|message| {
        message.body == MessageBody::Command
            && !message.status.is_terminal()
            && message.same_agent_card(agent)
    }) {
        return Err(CompactErr::Pending {
            message_id: message.message_id,
            status: message.status,
        });
    }
    if let Some(at) = agent.compacted_awaiting_prompt {
        return Err(CompactErr::Repeated { at });
    }
    Ok(())
}

pub fn send_compact(
    workspace: &ResolvedWorkspace,
    store: &Store,
    request: CompactRequest<'_>,
) -> Result<CompactOutcome> {
    refuse_repeat(store, request.agent, Timestamp::now())?;
    let mux = request.pane_id.mux();
    let mut message = MessageRecord::new(
        workspace.workspace_id.clone(),
        request.agent,
        request.command,
        true,
        DeliveryGate::Done,
    )
    .with_channel(request.agent.channel())
    .with_sender(request.sender)
    .with_automated(request.automated)
    .with_body(MessageBody::Command)
    .with_pane_id(request.pane_id);
    message.message_id = request.message_id;
    message.compacted_context_tokens = request.agent.occupied_context_tokens();
    store.queue_message(&message, &workspace.session_name)?;
    if deliver::deliver_one(
        workspace,
        store,
        &message.message_id,
        Duration::ZERO,
        Some(mux),
        DeliveryPolicy::Boundary,
    )? {
        return Ok(CompactOutcome::Sent);
    }
    let retry = store.record_message_delivery_failures(
        std::slice::from_ref(&message.message_id),
        None,
        DeliveryFailureDisposition::Retry,
        "compaction delivery gate closed",
        &workspace.session_name,
    )?;
    if retry.head_sent {
        return Ok(CompactOutcome::Sent);
    }
    if !store
        .list_messages()?
        .iter()
        .any(|queued| queued.message_id == message.message_id)
    {
        return Err(CompactErr::Settled {
            message_id: message.message_id,
        });
    }
    Ok(CompactOutcome::Queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::{RuntimePaths, StatePaths};

    #[test]
    fn standalone_compaction_refuses_brackets_pending_commands_and_completed_compactions() {
        let dir = tempfile::tempdir().unwrap();
        let id = WorkspaceId::from_project_root(dir.path());
        let store = Store::open(
            StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
            RuntimePaths::under(id.clone(), &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let now = Timestamp::now();
        let mut agent = AgentState::stub("claude", "session-1", AgentStatus::Idle);
        assert!(refuse_repeat(&store, &agent, now).is_ok());
        agent.compacting_since = Some(now);
        assert!(matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Compacting)
        ));
        agent.compacting_since = None;
        agent.compacted_awaiting_prompt = Some(now);
        assert!(matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Repeated { .. })
        ));
        agent.compacted_awaiting_prompt = None;
        let command =
            MessageRecord::new(id, &agent, "/compact".to_owned(), true, DeliveryGate::Done)
                .with_body(MessageBody::Command);
        store.queue_message(&command, "session").unwrap();
        assert!(
            matches!(refuse_repeat(&store, &agent, now), Err(CompactErr::Pending { message_id, .. }) if message_id == command.message_id)
        );
        let other = AgentState::stub("claude", "session-2", AgentStatus::Idle);
        assert!(refuse_repeat(&store, &other, now).is_ok());
    }
}
