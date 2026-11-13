//! RimZ-authored text for an agent card: composition, the boundary attempt, and the recorded miss.

use crate::Store;
use crate::agents::AgentState;
use crate::ids::{MessageId, PaneId};
use crate::store::message::{DeliveryGate, MessageRecord, MessageSender, MessageStatus};
use crate::store::writer::DeliveryFailureDisposition;
use crate::workspace::ResolvedWorkspace;

use super::deliver::{self, DeliveryPolicy, DeliveryReport, Result};

/// Text RimZ composes for one card. Every synthetic record carries the card's channel and submits
/// (Enter); it carries no pane, so delivery writes to the pane the card holds then.
pub struct SyntheticMessage<'a> {
    pub agent: &'a AgentState,
    pub text: String,
    pub sender: MessageSender,
    pub gate: DeliveryGate,
}

impl SyntheticMessage<'_> {
    /// Compose a record with the card's channel and the sender.
    pub fn record(self, workspace: &ResolvedWorkspace) -> MessageRecord {
        MessageRecord::new(
            workspace.workspace_id.clone(),
            self.agent,
            self.text,
            self.gate,
        )
        .with_channel(self.agent.lane_name())
        .with_sender(self.sender)
    }
}

/// Queue `message`, attempt one boundary delivery on the multiplexer of `pane`, the card's pane as
/// the caller read it, and record a miss under `on_miss`.
/// Returns whether the text reached the pane now (a miss whose record has meanwhile turned `Sent` counts as delivered).
pub fn deliver_now(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message: &MessageRecord,
    pane: Option<&PaneId>,
    on_miss: DeliveryFailureDisposition,
    fallback_reason: &str,
) -> Result<bool> {
    store.queue_message(message, &workspace.session_name)?;
    attempt_now(
        workspace,
        store,
        &message.message_id,
        pane,
        on_miss,
        fallback_reason,
    )
}

/// Attempt one boundary delivery of an already queued record and record a miss under `on_miss`.
pub fn attempt_now(
    workspace: &ResolvedWorkspace,
    store: &Store,
    message_id: &MessageId,
    pane: Option<&PaneId>,
    on_miss: DeliveryFailureDisposition,
    fallback_reason: &str,
) -> Result<bool> {
    let outcome = deliver::deliver_one_report(
        workspace,
        store,
        message_id,
        pane.map(PaneId::mux),
        DeliveryPolicy::Boundary,
    );
    let reason = match &outcome {
        Ok(DeliveryReport::Sent { .. }) => return Ok(true),
        Ok(DeliveryReport::Refused(deliver::DeliveryVerdict::TypingHeld { .. }))
            if on_miss == DeliveryFailureDisposition::Retry =>
        {
            None
        }
        Ok(DeliveryReport::Refused(verdict)) => Some(verdict.reason()),
        Ok(DeliveryReport::Stopped) if on_miss == DeliveryFailureDisposition::Terminal => {
            Some(fallback_reason.to_owned())
        }
        Ok(DeliveryReport::Stopped) => None,
        Err(err) => Some(err.to_string()),
    };
    let head_sent = match reason {
        Some(reason) => store
            .record_unheld_delivery_miss(message_id, on_miss, &reason, &workspace.session_name)
            .map(|result| result.head_sent),
        None => store.list_messages().map(|messages| {
            messages.iter().any(|message| {
                message.message_id == *message_id && message.status == MessageStatus::Sent
            })
        }),
    };
    if head_sent? {
        return Ok(true);
    }
    outcome.map(|_| false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::ids::WorkspaceId;
    use crate::store::message::{HarnessNotice, MessageStatus};
    use crate::workspace::RootClass;

    fn workspace(root: &std::path::Path) -> ResolvedWorkspace {
        ResolvedWorkspace {
            workspace_id: WorkspaceId::from_project_root(root),
            project_root: root.into(),
            cwd_project_root: None,
            root_class: RootClass::Directory,
            worktree_root: root.into(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: None,
        }
    }

    #[test]
    fn system_resume_carries_the_cards_channel_and_no_pane() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = workspace(dir.path());
        let mut agent = AgentState::stub("claude", "sess-parked", AgentStatus::Paused);
        agent.channel = Some("auth".to_owned());
        let message = SyntheticMessage {
            agent: &agent,
            text: "continue".to_owned(),
            sender: MessageSender::System,
            gate: DeliveryGate::Resume,
        }
        .record(&workspace);

        assert_eq!(
            message.channel.as_ref().map(crate::ids::LaneName::as_str),
            Some("auth")
        );
        assert_eq!(message.sender, MessageSender::System);
        assert_eq!(message.pane_id, None);
        assert_eq!(message.workspace_id, workspace.workspace_id);
        assert_eq!(message.agent_id, agent.agent_id);
        assert_eq!(message.text, "continue");
        assert_eq!(message.gate, DeliveryGate::Resume);
        assert_eq!(message.status, MessageStatus::Queued);
        assert!(message.enter);
        assert!(!message.is_user_input());
    }

    #[test]
    fn harness_notice_keeps_its_sender_without_a_pane() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = workspace(dir.path());
        let mut agent = AgentState::stub("claude", "sess-idle", AgentStatus::Idle);
        agent.channel = Some("auth".to_owned());
        let sender = MessageSender::Harness {
            notice: HarnessNotice::TeamReport,
        };
        let message = SyntheticMessage {
            agent: &agent,
            text: "team finished".to_owned(),
            sender: sender.clone(),
            gate: DeliveryGate::Done,
        }
        .record(&workspace);

        assert_eq!(message.sender, sender);
        assert_eq!(
            message.channel.as_ref().map(crate::ids::LaneName::as_str),
            Some("auth")
        );
        assert_eq!(message.pane_id, None);
        assert_eq!(message.gate, DeliveryGate::Done);
        assert!(message.enter);
    }
}
