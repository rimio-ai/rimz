//! Versioned sidebar wakeup wire vocabulary.

use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, WorkspaceId};

pub const SIDEBAR_EVENT_VERSION: &str = "rimz.sidebar-event.v2";
/// Version-stable renderer reload control word. Reload uses this bare datagram
/// instead of the typed envelope so an older renderer can still receive the
/// message that moves it onto the current build.
pub const RELOAD_CONTROL_WORD: &str = "reload";
/// Supervisor-only request for a clean worker exit after a replacement build
/// has served through its stability window.
pub const SUPERVISOR_HANDOFF_CONTROL_WORD: &str = "supervisor-handoff";

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
        clients: Vec<crate::pane::ClientPaneView>,
    },
    /// Store-less wakeup for a sidebar-initiated focus jump. The durable focus
    /// anchor carries the intent; this event only makes peer renderers fold it
    /// before the mux switch reveals the destination.
    FocusIntent {
        pane_id: PaneId,
        nonce: crate::ids::FocusNonce,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::LifecycleSignal;
    use crate::ids::MuxName;
    use crate::store::event::AGENT_LIFECYCLE_METHOD;

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
                nonce: crate::ids::FocusNonce::new(),
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
    fn focus_events_serialize_client_identity_and_nonce_verbatim() {
        use crate::ids::{FocusNonce, MuxClientId};
        use crate::pane::ClientPaneView;

        let pane_id = pane("terminal_2");
        let pane_json = serde_json::to_value(&pane_id).unwrap();
        let event = SidebarEvent::FocusStranded {
            pane_id: pane_id.clone(),
            generation: 3,
            clients: vec![
                ClientPaneView {
                    client_id: MuxClientId::Tmux("%3".into()),
                    pane_id: pane_id.clone(),
                },
                ClientPaneView {
                    client_id: MuxClientId::Zellij(7),
                    pane_id: pane_id.clone(),
                },
            ],
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "kind": "focus_stranded",
                "pane_id": pane_json,
                "generation": 3,
                "clients": [
                    {"client_id": {"mux": "tmux", "id": "%3"}, "pane_id": pane_json},
                    {"client_id": {"mux": "zellij", "id": 7}, "pane_id": pane_json}
                ]
            })
        );

        let nonce = FocusNonce::new();
        let nonce_json = serde_json::json!(nonce.to_string());
        let event = SidebarEvent::FocusIntent { pane_id, nonce };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["nonce"], nonce_json);
        assert_eq!(
            serde_json::from_value::<SidebarEvent>(value).unwrap(),
            event
        );

        let first = ClientPaneView {
            client_id: MuxClientId::Zellij(7),
            pane_id: pane("terminal_10"),
        };
        let second = ClientPaneView {
            pane_id: pane("terminal_2"),
            ..first.clone()
        };
        assert_eq!(first.cmp(&second), std::cmp::Ordering::Less);
        assert!(first < second);
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
}
