//! Typed sidebar wakeup event envelope.
//!
//! These datagrams are latency hints. The ledger and producer pulls remain the
//! durable truth; a renderer may drop any event that is stale, malformed, for a
//! different workspace/session, or superseded by a newer pull.

use serde::{Deserialize, Serialize};

use crate::agents::LifecycleSignal;
use crate::ids::{PaneId, WorkspaceId};

pub const SIDEBAR_EVENT_VERSION: &str = "rimz.sidebar-event.v2";
/// Version-stable renderer reload control word. Reload uses this bare datagram
/// instead of the typed envelope so an older renderer can still receive the
/// message that moves it onto the current build.
pub const RELOAD_CONTROL_WORD: &str = "reload";
const AGENT_LIFECYCLE_METHOD: &str = "agent.lifecycle";
const AGENT_REGISTERED_SIGNAL: &str = LifecycleSignal::Registered.tag();
const AGENT_ENDED_SIGNAL: &str = LifecycleSignal::Ended.tag();

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
    /// Sender wall clock. Fusion compares this against the pulled frame's pane
    /// observation stamp, falling back to `produced_at_ms` for legacy frames;
    /// receivers TTL on their own clock, so sender skew can mis-order an overlay
    /// briefly but never pin it.
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
    /// Latency hint that a switched-to tab restored focus to its sidebar pane.
    /// The renderer whose own pane matches decides whether to refocus a working
    /// sibling; other renderers ignore it.
    FocusStranded {
        pane_id: PaneId,
    },
    CommandChanged {
        pane_id: PaneId,
        command: String,
    },
    /// Focus changed. New Zellij presence plugins emit transitions only; old
    /// plugins and other producers may still send level-style focused/unfocused
    /// sets. Fusion mirrors the bits onto every row and retargets a renderer's
    /// own-view baseline only when the patch names one of that view's own
    /// working panes.
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
        agent_signal: Option<String>,
    },
    PaneFramePublished,
    Notify {
        title: String,
        body: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        panes: Vec<PaneId>,
        /// Whether the renderer rings the sticky tab bell only when the targeted
        /// agent row is unread — the same `UnreadTracker` signal stamped onto
        /// `SidebarRow::unread`. Agent notifications set this so a row that has
        /// returned to running (no longer unread) does not ring; link
        /// reachability alerts clear it to ring directly. `#[serde(default)]`
        /// keeps the agent default (`true`) for older producers.
        #[serde(default = "default_recheck_unread")]
        recheck_unread: bool,
        /// The producer's notification kind (`waiting`/`success`/`reminder`/…),
        /// carried so the renderer's bell-decision trace is self-explanatory.
        /// Absent on older producers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notification_kind: Option<String>,
    },
    /// Reload request. Current renderers also accept [`RELOAD_CONTROL_WORD`] so
    /// reload survives sidebar-event envelope version skew.
    Reload,
}

fn default_recheck_unread() -> bool {
    true
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
                agent_signal: Some(signal)
            } if method == AGENT_LIFECYCLE_METHOD
                && (signal.as_str() == AGENT_REGISTERED_SIGNAL
                    || signal.as_str() == AGENT_ENDED_SIGNAL)
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
            SidebarEvent::FocusStranded {
                pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_2"),
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
                event_method: Some(AGENT_LIFECYCLE_METHOD.to_owned()),
                agent_signal: Some(LifecycleSignal::Registered.tag().to_owned()),
            },
            SidebarEvent::PaneFramePublished,
            SidebarEvent::Notify {
                title: "Rimz: claude needs you".to_owned(),
                body: "claude sess-1 is waiting for input".to_owned(),
                panes: vec![PaneId::from_parts(MuxName::Zellij, "terminal_4")],
                recheck_unread: true,
                notification_kind: Some("waiting".to_owned()),
            },
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
    fn focus_stranded_is_a_renderer_action_not_an_overlay_or_verification_request() {
        let event = SidebarEvent::FocusStranded {
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_2"),
        };

        assert!(!event.is_overlay());
        assert!(!event.requests_producer_verification());
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

    #[test]
    fn notify_omits_empty_panes_and_defaults_old_events() {
        let expected = envelope(SidebarEvent::Notify {
            title: "Rimz: claude needs you".to_owned(),
            body: "claude sess-1 is waiting for input".to_owned(),
            panes: Vec::new(),
            recheck_unread: true,
            notification_kind: None,
        });
        let encoded = serde_json::to_vec(&expected).expect("serialize event envelope");
        assert!(!String::from_utf8_lossy(&encoded).contains("panes"));
        assert!(!String::from_utf8_lossy(&encoded).contains("notification_kind"));

        let decoded: SidebarEventEnvelope =
            serde_json::from_slice(&encoded).expect("decode event envelope");
        assert_eq!(decoded, expected);

        // An older producer omits `recheck_unread`; it defaults to the agent
        // path (`true`) so the renderer still gates the bell on row unread.
        let mut legacy = serde_json::to_value(&expected).expect("event to value");
        legacy["event"]
            .as_object_mut()
            .expect("event object")
            .remove("recheck_unread");
        let decoded_legacy: SidebarEventEnvelope =
            serde_json::from_value(legacy).expect("decode legacy notify");
        assert!(matches!(
            decoded_legacy.event,
            SidebarEvent::Notify {
                recheck_unread: true,
                ..
            }
        ));
    }
}
