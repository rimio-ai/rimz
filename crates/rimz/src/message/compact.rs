//! Standalone native compaction through the durable, boundary-only command queue, and the
//! continue a compaction of a busy agent owes it afterward.

use jiff::Timestamp;

use crate::Store;
use crate::agents::AgentState;
use crate::ids::{MessageId, PaneId};
use crate::store::message::{
    DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, SoftCompactStep,
    idle_compaction_edge,
};
use crate::store::writer::DeliveryFailureDisposition;
use crate::workspace::ResolvedWorkspace;

use super::deliver;
use super::synthetic::{self, SyntheticMessage};

type Result<T> = std::result::Result<T, CompactErr>;

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
        "already compacted and has not taken a turn since; a compaction never follows a compaction"
    )]
    Repeated { at: Timestamp },
    #[error("compaction {message_id}: {reason}")]
    Settled {
        message_id: MessageId,
        reason: String,
    },
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    Deliver(#[from] deliver::DeliverErr),
}

impl CompactErr {
    pub fn live_message(&self, now: Timestamp) -> String {
        match self {
            Self::Repeated { at } => {
                let elapsed = crate::store::message::format_dwell(
                    now.duration_since(*at).as_secs().max(0) as u64,
                );
                format!(
                    "already compacted {elapsed} ago and has not taken a turn since; a compaction never follows a compaction"
                )
            }
            _ => self.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    Sent,
    /// The command waits for a turn boundary; `continues` when a continue is queued behind it.
    Queued {
        continues: bool,
    },
}

pub struct CompactRequest<'a> {
    pub message_id: MessageId,
    pub agent: &'a AgentState,
    pub pane_id: PaneId,
    pub command: String,
    pub sender: MessageSender,
    pub automated: bool,
    pub idle: bool,
    /// What must still hold for the compaction to be worth sending, beside the idle tag.
    pub premises: Vec<crate::store::message::Premise>,
    /// The continue owed once the compaction ends, queued behind the command when `agent`
    /// holds an open turn and the command queues; `None` owes none.
    pub continue_text: Option<String>,
}

pub fn refuse_repeat(store: &Store, agent: &AgentState, now: Timestamp) -> Result<()> {
    if agent.is_compacting(now) {
        return Err(CompactErr::Compacting);
    }
    if let Some(message) = store.list_messages()?.into_iter().find(|message| {
        message.body == MessageBody::Command
            && !message.status.is_terminal()
            && message.same_agent_card(agent)
            && !(message.status == MessageStatus::Queued && message.idle_compaction_lapsed(agent))
    }) {
        return Err(CompactErr::Pending {
            message_id: message.message_id,
            status: message.status,
        });
    }
    if let Some(at) = agent
        .compacted_awaiting_prompt
        .filter(|_| agent.compaction_unprompted(now))
    {
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
    let mut message = SyntheticMessage {
        agent: request.agent,
        text: request.command,
        sender: request.sender,
        gate: DeliveryGate::Done,
        premises: request.premises,
    }
    .record(workspace)
    .with_automated(request.automated)
    .with_body(MessageBody::Command);
    message.message_id = request.message_id;
    message.compacted_context_tokens = request.agent.occupied_context_tokens();
    if request.idle {
        message.idle_since = idle_compaction_edge(request.agent);
    }
    let continuation = request
        .continue_text
        .filter(|_| request.agent.holds_open_turn())
        .map(|text| continue_record(workspace, request.agent, &message.message_id, text));
    let continuation = match continuation {
        Some(tail) => {
            Some(store.queue_message_with_tail(&message, &tail, &workspace.session_name)?)
        }
        None => {
            store.queue_message(&message, &workspace.session_name)?;
            None
        }
    };
    let withdraw_continue = |reason: String| -> Result<()> {
        if let Some(tail) = &continuation {
            store.withdraw_messages(&workspace.session_name, |live| {
                (live.message_id == tail.message_id).then(|| reason.clone())
            })?;
        }
        Ok(())
    };
    let typed_now = format!("compaction {} was typed at once", message.message_id);
    if synthetic::attempt_now(
        workspace,
        store,
        &message.message_id,
        Some(&request.pane_id),
        DeliveryFailureDisposition::Retry,
        "compaction delivery gate closed",
    )? {
        withdraw_continue(typed_now)?;
        return Ok(CompactOutcome::Sent);
    }
    if store
        .list_messages()?
        .iter()
        .any(|queued| queued.message_id == message.message_id)
    {
        return Ok(CompactOutcome::Queued {
            continues: continuation.is_some(),
        });
    }
    let settled = store
        .list_message_history()?
        .into_iter()
        .find(|record| record.message_id == message.message_id);
    if settled
        .as_ref()
        .is_some_and(|record| record.status == MessageStatus::Delivered)
    {
        withdraw_continue(typed_now)?;
        return Ok(CompactOutcome::Sent);
    }
    if let Some(reason) = continuation
        .as_ref()
        .map(|tail| super::premise::unsent_command(store, tail))
        .transpose()?
        .flatten()
    {
        withdraw_continue(reason)?;
    }
    let reason = settled.map_or_else(
        || "outcome is no longer in the message history".to_owned(),
        |record| {
            record
                .last_error
                .unwrap_or_else(|| format!("settled as {}", record.status))
        },
    );
    Err(CompactErr::Settled {
        message_id: message.message_id,
        reason,
    })
}

/// The continue for the compaction command `command_id` on `agent`'s card: a system prompt at
/// the `Done` gate, which the fold marks owed (`SoftCompactMark::continue_owed`) once queued and
/// delivery withdraws once that mark clears or the command ends unsent.
fn continue_record(
    workspace: &ResolvedWorkspace,
    agent: &AgentState,
    command_id: &MessageId,
    text: String,
) -> MessageRecord {
    let mut message = SyntheticMessage {
        agent,
        text,
        sender: MessageSender::System,
        gate: DeliveryGate::Done,
        premises: crate::store::message::Premise::NONE,
    }
    .record(workspace);
    message.soft_compact = Some(SoftCompactStep::Continue);
    message.follows_command = Some(command_id.clone());
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::store::message::MessageRecord;
    use crate::{RuntimePaths, StatePaths};

    #[test]
    fn repeated_compaction_display_is_age_free() {
        let now = Timestamp::now();
        let texts = [now - std::time::Duration::from_secs(3600), now]
            .map(|at| CompactErr::Repeated { at }.to_string());
        assert_eq!(
            texts,
            ["already compacted and has not taken a turn since; a compaction never follows a compaction";
                2]
        );
    }

    #[test]
    fn repeated_compaction_live_message_uses_the_callers_time() {
        let now: Timestamp = "2026-10-31T12:00:00Z".parse().unwrap();
        let error = CompactErr::Repeated {
            at: now - std::time::Duration::from_secs(300),
        };
        assert_eq!(
            error.live_message(now),
            "already compacted 5m ago and has not taken a turn since; a compaction never follows a compaction"
        );
    }

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
        let command = MessageRecord::new(id, &agent, "/compact".to_owned(), DeliveryGate::Done)
            .with_body(MessageBody::Command);
        store.queue_message(&command, "session").unwrap();
        assert!(
            matches!(refuse_repeat(&store, &agent, now), Err(CompactErr::Pending { message_id, .. }) if message_id == command.message_id)
        );
        let other = AgentState::stub("claude", "session-2", AgentStatus::Idle);
        assert!(refuse_repeat(&store, &other, now).is_ok());

        store
            .cancel_message(&command.message_id, "session", "test")
            .unwrap();
        let mut tagged = MessageRecord::requeue_from(&command);
        tagged.idle_since = Some(now);
        store.queue_message(&tagged, "session").unwrap();
        agent.turn_started_at = Some(now);
        let tied = matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Pending { .. })
        );
        agent.turn_started_at = Some(now + std::time::Duration::from_secs(1));
        let lapsed = refuse_repeat(&store, &agent, now).is_ok();
        store
            .claim_message_for_steer(&tagged.message_id, now)
            .unwrap()
            .unwrap();
        let claimed = matches!(
            refuse_repeat(&store, &agent, now),
            Err(CompactErr::Pending { .. })
        );
        assert_eq!((tied, lapsed, claimed), (true, true, true));
    }

    #[test]
    fn standalone_compaction_persists_the_idle_decision_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = ResolvedWorkspace {
            workspace_id: WorkspaceId::from_project_root(dir.path()),
            project_root: dir.path().into(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: dir.path().into(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: None,
        };
        let store = Store::open(
            StatePaths::under(workspace.workspace_id.clone(), &dir.path().join("state")).unwrap(),
            RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("runtime"))
                .unwrap(),
        )
        .unwrap();
        let stamp = Timestamp::from_second(1_000).unwrap();
        let earlier = stamp - std::time::Duration::from_secs(1);
        let cases = [
            (false, Some(stamp), Some(earlier), None),
            (true, Some(stamp), Some(earlier), Some(stamp)),
            (true, Some(earlier), Some(stamp), Some(stamp)),
            (true, None, Some(stamp), Some(stamp)),
            (true, None, None, None),
        ];
        let actual: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(index, &(idle, started, ended, _))| {
                let mut agent =
                    AgentState::stub("claude", &format!("session-{index}"), AgentStatus::Running);
                agent.turn_started_at = started;
                agent.turn_ended_at = ended;
                let id = MessageId::new();
                let outcome = send_compact(
                    &workspace,
                    &store,
                    CompactRequest {
                        premises: crate::store::message::Premise::NONE,
                        message_id: id.clone(),
                        agent: &agent,
                        pane_id: PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_3"),
                        command: "/compact".to_owned(),
                        sender: MessageSender::System,
                        automated: true,
                        idle,
                        continue_text: None,
                    },
                )
                .unwrap();
                assert_eq!(outcome, CompactOutcome::Queued { continues: false });
                let queued = store
                    .list_messages()
                    .unwrap()
                    .into_iter()
                    .find(|message| message.message_id == id)
                    .unwrap();
                serde_json::to_value(queued)
                    .unwrap()
                    .get("idle_since")
                    .cloned()
            })
            .collect();
        assert_eq!(
            actual,
            cases.map(|(_, _, _, expected)| expected.map(|stamp| serde_json::json!(stamp)))
        );
    }
}
