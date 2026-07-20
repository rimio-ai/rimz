//! Typed sidebar wakeup events and loop-owned realtime sidebar event store.
//!
//! These datagrams are latency hints. The store and producer pulls remain the
//! durable truth; a renderer may drop any event that is stale, malformed, for a
//! different workspace/session, or superseded by a newer pull. The renderer
//! appends overlay datagrams to an in-memory per-process store and fuses them
//! with pulled truth on paint, so expiry and supersession are deliberately
//! simple.

use serde::{Deserialize, Serialize};

use crate::agents::LifecycleSignal;
use crate::ids::{PaneId, WorkspaceId};
use crate::sidebar::timing::EVENT_STORE_TTL;
use crate::store::event::AGENT_LIFECYCLE_METHOD;

pub const SIDEBAR_EVENT_VERSION: &str = "rimz.sidebar-event.v2";
/// Version-stable renderer reload control word. Reload uses this bare datagram
/// instead of the typed envelope so an older renderer can still receive the
/// message that moves it onto the current build.
pub const RELOAD_CONTROL_WORD: &str = "reload";
/// Supervisor-only request for a clean worker exit after a replacement build
/// has served through its stability window.
pub const SUPERVISOR_HANDOFF_CONTROL_WORD: &str = "supervisor-handoff";
const AGENT_REGISTERED_SIGNAL: &str = LifecycleSignal::Registered.tag();
const AGENT_ENDED_SIGNAL: &str = LifecycleSignal::Ended.tag();
pub const MAX_EVENTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarEventEnvelope {
    pub v: String,
    pub workspace_id: WorkspaceId,
    /// `Some` scopes the event to one mux session — pane ids are only
    /// meaningful inside the session that issued them. `None` is
    /// workspace-scoped: store deltas, reloads, and pane-frame publications
    /// apply to every session renderer of the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Sender wall clock. Fusion compares this against the pulled frame's pane
    /// observation stamp; receivers TTL on their own clock, so sender skew can
    /// mis-order an overlay briefly but never pin it.
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
    /// Latency hint that a switched-to tab/window restored focus to its sidebar
    /// pane. The renderer whose own pane matches decides whether to refocus a
    /// working sibling; other renderers ignore it.
    FocusStranded {
        pane_id: PaneId,
        generation: u64,
        clients: Vec<crate::mux::ClientPaneView>,
    },
    /// Store-less wakeup for a sidebar-initiated focus jump. The durable focus
    /// anchor carries the intent; this event only makes peer renderers fold it
    /// before the mux switch reveals the destination.
    FocusIntent {
        pane_id: PaneId,
        nonce: crate::sidebar::focus_anchor::FocusNonce,
    },
    CommandChanged {
        pane_id: PaneId,
        command: String,
    },
    /// Session presentation changed. Zellij emits client-derived transitions;
    /// tmux and compatibility producers may still send level-style sets.
    /// Fusion updates only the session register; pane rows carry no focus bit.
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
    StoreDelta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_signal: Option<String>,
    },
    /// The room-runtime absolute width target changed. Renderers re-read the
    /// atomically replaced override file and converge only their own pane.
    WidthTargetChanged,
    /// The shared cockpit lens changed. Renderers re-read the atomically
    /// replaced room-runtime filter file.
    BodyFilterChanged,
    PaneFramePublished {
        /// The producer-side change written into the shared pane frame. Older
        /// publishers omitted this field, which safely decodes as topology.
        #[serde(default)]
        publication: PaneFramePublicationKind,
    },
    Notify {
        title: String,
        body: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        panes: Vec<PaneId>,
        /// Whether the renderer rings the sticky tab bell only when the targeted
        /// agent row is unread — the durable unread episode bit folded onto
        /// `SidebarRow::unread`. Agent notifications set this so a read row does
        /// not ring; link reachability alerts clear it to ring directly.
        /// `#[serde(default)]` keeps the agent default (`true`) for older
        /// producers.
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

/// Which producer-owned input changed in a published pane frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneFramePublicationKind {
    #[default]
    Topology,
    Metrics,
    Presence,
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
            Self::StoreDelta {
                event_method: Some(method),
                agent_signal: Some(signal)
            } if method == AGENT_LIFECYCLE_METHOD
                && (signal.as_str() == AGENT_REGISTERED_SIGNAL
                    || signal.as_str() == AGENT_ENDED_SIGNAL)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredEvent {
    pub sent_at_ms: u64,
    pub received_at_ms: u64,
    pub event: SidebarEvent,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventStore {
    events: Vec<StoredEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EventKey {
    PaneClosed(PaneId),
    CommandChanged(PaneId),
    PaneOpened(PaneId),
    Focus,
}

impl EventStore {
    /// Append one overlay event. The caller has already scoped the envelope to
    /// this workspace and session; the store only needs the event body plus the
    /// sender stamp (supersession) and the receiver stamp (TTL). Nudge events
    /// are ignored — they drive fetches, never overlays.
    pub fn append(&mut self, event: SidebarEvent, sent_at_ms: u64, received_at_ms: u64) {
        self.prune(received_at_ms);
        let Some(key) = event_key(&event) else {
            return;
        };
        let next = StoredEvent {
            sent_at_ms,
            received_at_ms,
            event,
        };
        if let Some(existing) = self
            .events
            .iter_mut()
            .find(|event| event_key(&event.event).as_ref() == Some(&key))
        {
            if (next.sent_at_ms, next.received_at_ms)
                >= (existing.sent_at_ms, existing.received_at_ms)
            {
                *existing = next;
            }
        } else {
            self.events.push(next);
        }
        self.enforce_cap();
    }

    pub fn prune(&mut self, now_ms: u64) {
        let ttl_ms = EVENT_STORE_TTL.as_millis() as u64;
        self.events
            .retain(|event| now_ms.saturating_sub(event.received_at_ms) <= ttl_ms);
        self.enforce_cap();
    }

    pub fn active(&self, now_ms: u64) -> impl Iterator<Item = &StoredEvent> {
        let ttl_ms = EVENT_STORE_TTL.as_millis() as u64;
        self.events
            .iter()
            .filter(move |event| now_ms.saturating_sub(event.received_at_ms) <= ttl_ms)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn enforce_cap(&mut self) {
        if self.events.len() <= MAX_EVENTS {
            return;
        }
        self.events
            .sort_by_key(|event| (event.received_at_ms, event.sent_at_ms));
        let overflow = self.events.len() - MAX_EVENTS;
        self.events.drain(0..overflow);
    }
}

fn event_key(event: &SidebarEvent) -> Option<EventKey> {
    match event {
        SidebarEvent::PaneClosed { pane_id } => Some(EventKey::PaneClosed(pane_id.clone())),
        SidebarEvent::CommandChanged { pane_id, .. } => {
            Some(EventKey::CommandChanged(pane_id.clone()))
        }
        SidebarEvent::PaneOpened {
            pane_id,
            command: Some(_),
        } => Some(EventKey::PaneOpened(pane_id.clone())),
        SidebarEvent::FocusChanged { .. } => Some(EventKey::Focus),
        SidebarEvent::PaneOpened { command: None, .. }
        | SidebarEvent::FocusStranded { .. }
        | SidebarEvent::FocusIntent { .. }
        | SidebarEvent::PanesChanged
        | SidebarEvent::StoreDelta { .. }
        | SidebarEvent::WidthTargetChanged
        | SidebarEvent::BodyFilterChanged
        | SidebarEvent::PaneFramePublished { .. }
        | SidebarEvent::Notify { .. }
        | SidebarEvent::Reload => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MuxName;

    fn pane(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

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
                pane_id: pane("terminal_1"),
            },
            SidebarEvent::CommandChanged {
                pane_id: pane("terminal_1"),
                command: "codex".to_owned(),
            },
            SidebarEvent::FocusStranded {
                pane_id: pane("terminal_2"),
                generation: 7,
                clients: Vec::new(),
            },
            SidebarEvent::FocusIntent {
                pane_id: pane("terminal_2"),
                nonce: crate::sidebar::focus_anchor::FocusNonce::new(),
            },
            SidebarEvent::FocusChanged {
                focused: vec![pane("terminal_1")],
                unfocused: vec![pane("terminal_2")],
            },
            SidebarEvent::PaneOpened {
                pane_id: pane("terminal_3"),
                command: Some("zsh".to_owned()),
            },
            SidebarEvent::PanesChanged,
            SidebarEvent::StoreDelta {
                event_method: Some(AGENT_LIFECYCLE_METHOD.to_owned()),
                agent_signal: Some(LifecycleSignal::Registered.tag().to_owned()),
            },
            SidebarEvent::WidthTargetChanged,
            SidebarEvent::BodyFilterChanged,
            SidebarEvent::PaneFramePublished {
                publication: PaneFramePublicationKind::Topology,
            },
            SidebarEvent::PaneFramePublished {
                publication: PaneFramePublicationKind::Metrics,
            },
            SidebarEvent::PaneFramePublished {
                publication: PaneFramePublicationKind::Presence,
            },
            SidebarEvent::Notify {
                title: "RimZ: claude needs you".to_owned(),
                body: "claude sess-1 is waiting for input".to_owned(),
                panes: vec![pane("terminal_4")],
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
    fn legacy_pane_frame_published_decodes_as_topology() {
        let decoded: SidebarEvent =
            serde_json::from_str(r#"{"kind":"pane_frame_published"}"#).unwrap();

        assert_eq!(
            decoded,
            SidebarEvent::PaneFramePublished {
                publication: PaneFramePublicationKind::Topology,
            }
        );
    }

    #[test]
    fn typed_pane_frame_published_decodes_for_a_legacy_unit_consumer() {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum LegacySidebarEvent {
            PaneFramePublished,
        }

        let encoded = serde_json::to_vec(&SidebarEvent::PaneFramePublished {
            publication: PaneFramePublicationKind::Metrics,
        })
        .unwrap();
        let decoded: LegacySidebarEvent = serde_json::from_slice(&encoded).unwrap();

        assert!(matches!(decoded, LegacySidebarEvent::PaneFramePublished));
    }

    #[test]
    fn focus_stranded_is_a_renderer_action_not_an_overlay_or_verification_request() {
        let event = SidebarEvent::FocusStranded {
            pane_id: pane("terminal_2"),
            generation: 7,
            clients: Vec::new(),
        };

        assert!(!event.is_overlay());
        assert!(!event.requests_producer_verification());
    }

    #[test]
    fn width_target_change_is_a_renderer_action_not_an_overlay_or_verification_request() {
        let event = SidebarEvent::WidthTargetChanged;

        assert!(!event.is_overlay());
        assert!(!event.requests_producer_verification());
    }

    #[test]
    fn body_filter_change_is_a_renderer_action_not_an_overlay_or_verification_request() {
        let event = SidebarEvent::BodyFilterChanged;

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
            title: "RimZ: claude needs you".to_owned(),
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

    #[test]
    fn dedupes_latest_wins_per_pane_event_key() {
        let mut store = EventStore::default();
        store.append(
            SidebarEvent::CommandChanged {
                pane_id: pane("terminal_1"),
                command: "zsh".to_owned(),
            },
            10,
            100,
        );
        store.append(
            SidebarEvent::CommandChanged {
                pane_id: pane("terminal_1"),
                command: "codex".to_owned(),
            },
            11,
            101,
        );

        let events = store.active(101).collect::<Vec<_>>();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].event,
            SidebarEvent::CommandChanged { command, .. } if command == "codex"
        ));
    }

    #[test]
    fn keeps_close_and_command_for_the_same_pane() {
        let mut store = EventStore::default();
        store.append(
            SidebarEvent::CommandChanged {
                pane_id: pane("terminal_1"),
                command: "zsh".to_owned(),
            },
            10,
            100,
        );
        store.append(
            SidebarEvent::PaneClosed {
                pane_id: pane("terminal_1"),
            },
            11,
            101,
        );

        assert_eq!(store.active(101).count(), 2);
    }

    #[test]
    fn nudge_events_never_occupy_the_store() {
        let mut store = EventStore::default();
        store.append(SidebarEvent::PanesChanged, 10, 100);
        store.append(
            SidebarEvent::PaneOpened {
                pane_id: pane("terminal_1"),
                command: None,
            },
            11,
            101,
        );
        store.append(
            SidebarEvent::StoreDelta {
                event_method: None,
                agent_signal: None,
            },
            12,
            102,
        );
        store.append(
            SidebarEvent::FocusIntent {
                pane_id: pane("terminal_2"),
                nonce: crate::sidebar::focus_anchor::FocusNonce::new(),
            },
            13,
            103,
        );
        assert!(store.is_empty());
    }

    #[test]
    fn ttl_is_receiver_clock_boundary_exact() {
        let mut store = EventStore::default();
        store.append(
            SidebarEvent::PaneClosed {
                pane_id: pane("terminal_1"),
            },
            10,
            100,
        );
        let ttl = EVENT_STORE_TTL.as_millis() as u64;
        assert_eq!(store.active(100 + ttl).count(), 1);
        store.prune(101 + ttl);
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn cap_drops_oldest_received_events() {
        let mut store = EventStore::default();
        for idx in 0..(MAX_EVENTS + 1) {
            store.append(
                SidebarEvent::PaneClosed {
                    pane_id: pane(&format!("terminal_{idx}")),
                },
                idx as u64,
                idx as u64,
            );
        }
        assert_eq!(store.len(), MAX_EVENTS);
        assert!(!store.active(MAX_EVENTS as u64).any(|event| {
            matches!(
                &event.event,
                SidebarEvent::PaneClosed { pane_id } if pane_id == &pane("terminal_0")
            )
        }));
    }
}
