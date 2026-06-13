//! Durable per-agent message queue domain model.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::lifecycle::LifecycleSignal;
use crate::feed::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, MessageId, WorkspaceId};

pub const DEFAULT_SETTLE: Duration = Duration::from_millis(400);
pub const SETTLE_ENV: &str = "RIMZ_QUEUE_SETTLE_MS";
pub const MAX_DELIVERY_ATTEMPTS: u32 = 5;
pub const CLAIM_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGate {
    Done,
    Any,
}

impl DeliveryGate {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Any => "any",
        }
    }
}

impl std::fmt::Display for DeliveryGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    Pending,
    Claimed,
    Delivered,
    Removed,
    Abandoned,
}

impl MessageStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Removed | Self::Abandoned)
    }

    pub const fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::Claimed)
    }

    pub const fn leaves_pending_queue(self) -> bool {
        matches!(
            self,
            Self::Claimed | Self::Delivered | Self::Removed | Self::Abandoned
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Delivered => "delivered",
            Self::Removed => "removed",
            Self::Abandoned => "abandoned",
        }
    }
}

impl std::fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageRecord {
    pub message_id: MessageId,
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    pub text: String,
    pub enter: bool,
    pub gate: DeliveryGate,
    pub status: MessageStatus,
    pub enqueued_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<Timestamp>,
}

impl MessageRecord {
    pub fn new(
        workspace_id: WorkspaceId,
        agent: &AgentState,
        text: String,
        enter: bool,
        gate: DeliveryGate,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            message_id: MessageId::new(),
            workspace_id,
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            agent_name: agent.name.clone(),
            text,
            enter,
            gate,
            status: MessageStatus::Pending,
            enqueued_at: now,
            updated_at: now,
            attempts: 0,
            last_attempt_at: None,
            last_error: None,
            delivered_at: None,
        }
    }

    pub fn same_agent(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> bool {
        self.kind == *kind && self.agent_id == *agent_id
    }

    pub fn same_agent_card(&self, agent: &AgentState) -> bool {
        self.same_agent(&agent.kind, &agent.agent_id)
            || (self.kind == agent.kind
                && self
                    .agent_name
                    .as_deref()
                    .is_some_and(|name| agent.name.as_deref() == Some(name)))
    }
}

pub fn gate_open(gate: DeliveryGate, status: AgentStatus) -> bool {
    match gate {
        DeliveryGate::Done => matches!(status, AgentStatus::Idle | AgentStatus::Success),
        DeliveryGate::Any => matches!(
            status,
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Failed
        ),
    }
}

pub fn queue_head<'a>(
    pending: impl IntoIterator<Item = &'a MessageRecord>,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Option<&'a MessageRecord> {
    pending
        .into_iter()
        .filter(|message| {
            message.status == MessageStatus::Pending && message.same_agent(kind, agent_id)
        })
        .min_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()))
}

pub fn delivery_checkpoint(signal: &LifecycleSignal) -> bool {
    matches!(
        signal,
        LifecycleSignal::TurnEnded {
            parked_on_background: false,
            ..
        }
    )
}

pub fn settle_duration_from_env() -> Duration {
    std::env::var(SETTLE_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_SETTLE)
}

pub fn claim_expired(last_attempt_at: Option<Timestamp>, now: Timestamp) -> bool {
    let Some(last) = last_attempt_at else {
        return true;
    };
    let age = now.duration_since(last);
    age.is_negative() || (age.as_secs() as u64) >= CLAIM_TTL.as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gates_open_only_on_resting_statuses() {
        assert!(gate_open(DeliveryGate::Done, AgentStatus::Idle));
        assert!(gate_open(DeliveryGate::Done, AgentStatus::Success));
        assert!(!gate_open(DeliveryGate::Done, AgentStatus::Failed));
        assert!(gate_open(DeliveryGate::Any, AgentStatus::Failed));
        for status in [
            AgentStatus::Running,
            AgentStatus::Waiting,
            AgentStatus::Paused,
        ] {
            assert!(!gate_open(DeliveryGate::Done, status));
            assert!(!gate_open(DeliveryGate::Any, status));
        }
    }

    #[test]
    fn delivery_checkpoint_is_only_unparked_turn_end() {
        assert!(delivery_checkpoint(&LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }));
        assert!(delivery_checkpoint(&LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }));
        assert!(!delivery_checkpoint(&LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        }));
        assert!(!delivery_checkpoint(&LifecycleSignal::Registered));
        assert!(!delivery_checkpoint(&LifecycleSignal::SubagentStopped {
            errored: false
        }));
    }

    #[test]
    fn claim_ttl_treats_future_stamp_as_expired() {
        let now = Timestamp::now();
        assert!(claim_expired(None, now));
        assert!(!claim_expired(
            Some(now - jiff::SignedDuration::from_secs(1)),
            now
        ));
        assert!(claim_expired(
            Some(now - jiff::SignedDuration::from_secs(15)),
            now
        ));
        assert!(claim_expired(
            Some(now + jiff::SignedDuration::from_secs(60)),
            now
        ));
    }

    #[test]
    fn message_matches_registered_card_by_remembered_name() {
        let mut provisional = agent("launch_1", Some("lucid-atlas"));
        let message = MessageRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
            &provisional,
            "next".to_owned(),
            true,
            DeliveryGate::Done,
        );
        provisional.agent_id = AgentSessionId::from("real-session");

        assert!(message.same_agent_card(&provisional));
    }

    fn agent(id: &str, name: Option<&str>) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked("claude"),
            name: name.map(ToOwned::to_owned),
            kind_ordinal: Some(1),
            status: AgentStatus::Idle,
            phase: crate::agents::TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
