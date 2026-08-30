//! Public lifecycle-event envelope and const-friendly signal filters.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::{LifecycleSignal, Transition, TransitionKind, TurnPhase};
use crate::agents::AgentStatus;
use crate::ids::{AgentKind, AgentSessionId, EventId, WorkspaceId};

/// Current external lifecycle-event wire version.
pub const LIFECYCLE_EVENT_VERSION: u32 = 1;

/// One durable agent lifecycle transition, projected for reactors and streams.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LifecycleEvent {
    pub v: u32,
    pub event_id: EventId,
    pub at: Timestamp,
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<AgentSessionId>,
    pub signal: LifecycleSignal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_status: Option<AgentStatus>,
    pub status: AgentStatus,
    pub phase: TurnPhase,
    pub transition: LifecycleTransition,
    pub compaction_closed: bool,
    pub waiting_cleared: bool,
}

impl LifecycleEvent {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        event_id: EventId,
        at: Timestamp,
        workspace_id: WorkspaceId,
        kind: AgentKind,
        agent_id: AgentSessionId,
        agent_name: Option<String>,
        parent_agent_id: Option<AgentSessionId>,
        signal: LifecycleSignal,
        prior_status: Option<AgentStatus>,
        transition: Transition,
    ) -> Self {
        Self {
            v: LIFECYCLE_EVENT_VERSION,
            event_id,
            at,
            workspace_id,
            kind,
            agent_id,
            agent_name,
            parent_agent_id,
            signal,
            prior_status,
            status: transition.next.status,
            phase: transition.next.phase,
            transition: transition.kind.into(),
            compaction_closed: transition.compaction_closed,
            waiting_cleared: transition.waiting_cleared,
        }
    }
}

/// Stable wire projection of the state machine's transition classification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleTransition {
    Normal,
    Reconciled { from: AgentStatus, reason: String },
    Ignored { reason: String },
}

impl From<TransitionKind> for LifecycleTransition {
    fn from(value: TransitionKind) -> Self {
        match value {
            TransitionKind::Normal => Self::Normal,
            TransitionKind::Reconciled { from, reason } => Self::Reconciled {
                from,
                reason: reason.to_owned(),
            },
            TransitionKind::Ignored { reason } => Self::Ignored {
                reason: reason.to_owned(),
            },
        }
    }
}

/// Const-friendly set over the data-bearing [`LifecycleSignal`] variants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SignalSet(u16);

impl SignalSet {
    pub const REGISTERED: Self = Self(1 << 0);
    pub const TURN_STARTED: Self = Self(1 << 1);
    pub const TURN_ENDED: Self = Self(1 << 2);
    pub const TURN_INTERRUPTED: Self = Self(1 << 3);
    pub const SUBAGENT_STARTED: Self = Self(1 << 4);
    pub const SUBAGENT_STOPPED: Self = Self(1 << 5);
    pub const TOOL_USED: Self = Self(1 << 6);
    pub const AWAITING_INPUT: Self = Self(1 << 7);
    pub const COMPACTING: Self = Self(1 << 8);
    pub const COMPACTION_ENDED: Self = Self(1 << 9);
    pub const ENDED: Self = Self(1 << 10);
    pub const LOST: Self = Self(1 << 11);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, signal: &LifecycleSignal) -> bool {
        let bit = match signal {
            LifecycleSignal::Registered => Self::REGISTERED.0,
            LifecycleSignal::TurnStarted => Self::TURN_STARTED.0,
            LifecycleSignal::TurnEnded { .. } => Self::TURN_ENDED.0,
            LifecycleSignal::TurnInterrupted { .. } => Self::TURN_INTERRUPTED.0,
            LifecycleSignal::SubagentStarted => Self::SUBAGENT_STARTED.0,
            LifecycleSignal::SubagentStopped { .. } => Self::SUBAGENT_STOPPED.0,
            LifecycleSignal::ToolUsed { .. } => Self::TOOL_USED.0,
            LifecycleSignal::AwaitingInput { .. } => Self::AWAITING_INPUT.0,
            LifecycleSignal::Compacting => Self::COMPACTING.0,
            LifecycleSignal::CompactionEnded { .. } => Self::COMPACTION_ENDED.0,
            LifecycleSignal::Ended => Self::ENDED.0,
            LifecycleSignal::Lost => Self::LOST.0,
        };
        self.0 & bit != 0
    }
}

/// Signals that release the head of a queued `--after` delivery.
pub const DELIVERY_CHECKPOINT: SignalSet = SignalSet::TURN_ENDED
    .union(SignalSet::TURN_INTERRUPTED)
    .union(SignalSet::COMPACTION_ENDED);

/// Signals that may satisfy a queued message's durable `--when` condition.
pub const CONDITION_CHECKPOINT: SignalSet = SignalSet::REGISTERED
    .union(SignalSet::TURN_STARTED)
    .union(SignalSet::TURN_ENDED)
    .union(SignalSet::TURN_INTERRUPTED)
    .union(SignalSet::AWAITING_INPUT)
    .union(SignalSet::SUBAGENT_STARTED)
    .union(SignalSet::SUBAGENT_STOPPED)
    .union(SignalSet::COMPACTION_ENDED);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::lifecycle::{LifecycleState, TransitionKind};

    fn event(signal: LifecycleSignal, transition: Transition) -> LifecycleEvent {
        LifecycleEvent::new(
            EventId::parse("evt_018f47a2c00070008000000000000000").unwrap(),
            "2026-06-01T12:00:00Z".parse().unwrap(),
            WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("session-1"),
            Some("coder".to_owned()),
            None,
            signal,
            Some(AgentStatus::Running),
            transition,
        )
    }

    #[test]
    fn lifecycle_event_wire_shape_is_pinned_and_round_trips() {
        let value = event(
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            Transition {
                next: LifecycleState {
                    status: AgentStatus::Success,
                    phase: TurnPhase::Idle,
                    compacting: false,
                },
                kind: TransitionKind::Normal,
                compaction_closed: true,
                waiting_cleared: false,
                opened_turn: false,
            },
        );
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"event_id":"evt_018f47a2c00070008000000000000000","at":"2026-06-01T12:00:00Z","workspace_id":"ws_0123456789abcdef01234567","kind":"claude","agent_id":"session-1","agent_name":"coder","signal":{"signal":"turn_ended","errored":false,"parked_on_background":false},"prior_status":"running","status":"success","phase":"idle","transition":{"kind":"normal"},"compaction_closed":true,"waiting_cleared":false}"#,
        );
        assert_eq!(
            serde_json::from_str::<LifecycleEvent>(&json).unwrap(),
            value
        );
    }

    #[test]
    fn signal_sets_distinguish_all_checkpoint_variants() {
        let turn_end = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        };
        assert!(DELIVERY_CHECKPOINT.contains(&turn_end));
        assert!(DELIVERY_CHECKPOINT.contains(&LifecycleSignal::TurnInterrupted { turn_id: None }));
        assert!(
            DELIVERY_CHECKPOINT.contains(&LifecycleSignal::CompactionEnded {
                auto: None,
                failed: false,
            })
        );
        assert!(
            DELIVERY_CHECKPOINT.contains(&LifecycleSignal::CompactionEnded {
                auto: Some(false),
                failed: true,
            })
        );
        assert!(!DELIVERY_CHECKPOINT.contains(&LifecycleSignal::TurnStarted));
        assert!(
            CONDITION_CHECKPOINT.contains(&LifecycleSignal::AwaitingInput {
                kind: super::super::AskKind::Question,
                ask_id: None,
                detail: None,
                native_key: None,
            })
        );
        assert!(!CONDITION_CHECKPOINT.contains(&LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
            name: None,
            native_key: None,
            turn_id: None,
        }));
    }
}
