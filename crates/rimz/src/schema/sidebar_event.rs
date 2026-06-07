//! Typed sidebar wakeup event envelope.
//!
//! These datagrams are latency hints. The ledger and producer pulls remain the
//! durable truth; a renderer may drop any event that is stale, malformed, for a
//! different workspace/session, or superseded by a newer pull.

use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, WorkspaceId};

pub const SIDEBAR_EVENT_VERSION: &str = "rimz.sidebar-event.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarEventEnvelope {
    pub v: String,
    pub workspace_id: WorkspaceId,
    /// `Some` scopes the event to one mux session — pane ids are only
    /// meaningful inside the session that issued them. `None` is
    /// workspace-scoped: ledger deltas, reloads, and pane-frame publications
    /// apply to every session renderer of the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Sender wall clock. Fusion compares this against the pulled frame's
    /// `produced_at_ms` for supersession; receivers TTL on their own clock, so
    /// sender skew can mis-order an overlay briefly but never pin it.
    pub sent_at_ms: u64,
    pub event: SidebarEvent,
}

impl SidebarEventEnvelope {
    pub fn new(
        workspace_id: WorkspaceId,
        session_name: Option<String>,
        sent_at_ms: u64,
        event: SidebarEvent,
    ) -> Self {
        Self {
            v: SIDEBAR_EVENT_VERSION.to_owned(),
            workspace_id,
            session_name,
            sent_at_ms,
            event,
        }
    }

    pub fn is_current_version(&self) -> bool {
        self.v == SIDEBAR_EVENT_VERSION
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidebarEvent {
    PaneClosed {
        pane_id: PaneId,
    },
    CommandChanged {
        pane_id: PaneId,
        command: String,
    },
    FocusChanged {
        focused: Vec<PaneId>,
        unfocused: Vec<PaneId>,
    },
    PaneOpened {
        pane_id: PaneId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    /// Pane topology changed somewhere in the session, identity unknown — the
    /// nudge backends emit when they can observe a change but not name the
    /// pane (the tmux control-mode watch, the Zellij plugin's manifest fold).
    PanesChanged,
    LedgerDelta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_event_name: Option<String>,
    },
    PaneFramePublished,
    Reload,
}

impl SidebarEvent {
    pub fn is_overlay(&self) -> bool {
        matches!(
            self,
            Self::PaneClosed { .. }
                | Self::CommandChanged { .. }
                | Self::FocusChanged { .. }
                | Self::PaneOpened {
                    command: Some(_),
                    ..
                }
        )
    }

    pub fn requests_producer_verification(&self) -> bool {
        matches!(
            self,
            Self::PaneClosed { .. }
                | Self::CommandChanged { .. }
                | Self::PaneOpened { .. }
                | Self::PanesChanged
        ) || matches!(
            self,
            Self::LedgerDelta {
                event_method: Some(method),
                agent_event_name: Some(name)
            } if method == "agent.lifecycle" && matches!(name.as_str(), "SessionStart" | "SessionEnd")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MuxName;

    fn envelope(event: SidebarEvent) -> SidebarEventEnvelope {
        SidebarEventEnvelope::new(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-schema")),
            Some("rimz-test".to_owned()),
            42,
            event,
        )
    }

    #[test]
    fn serde_round_trips_each_variant() {
        let variants = [
            SidebarEvent::PaneClosed {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            },
            SidebarEvent::CommandChanged {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
                command: "codex".to_owned(),
            },
            SidebarEvent::FocusChanged {
                focused: vec![PaneId::from_parts(MuxName::Zellij, "terminal_1")],
                unfocused: vec![PaneId::from_parts(MuxName::Zellij, "terminal_2")],
            },
            SidebarEvent::PaneOpened {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_3"),
                command: Some("zsh".to_owned()),
            },
            SidebarEvent::PanesChanged,
            SidebarEvent::LedgerDelta {
                event_method: Some("agent.lifecycle".to_owned()),
                agent_event_name: Some("SessionStart".to_owned()),
            },
            SidebarEvent::PaneFramePublished,
            SidebarEvent::Reload,
        ];
        for event in variants {
            let expected = envelope(event);
            let encoded = serde_json::to_vec(&expected).expect("serialize event envelope");
            let decoded: SidebarEventEnvelope =
                serde_json::from_slice(&encoded).expect("decode event envelope");
            assert_eq!(decoded, expected);
            assert!(decoded.is_current_version());
        }
    }

    #[test]
    fn workspace_scoped_envelope_round_trips_without_a_session() {
        let mut expected = envelope(SidebarEvent::Reload);
        expected.session_name = None;
        let encoded = serde_json::to_vec(&expected).expect("serialize event envelope");
        assert!(!String::from_utf8_lossy(&encoded).contains("session_name"));
        let decoded: SidebarEventEnvelope =
            serde_json::from_slice(&encoded).expect("decode event envelope");
        assert_eq!(decoded, expected);
    }
}
