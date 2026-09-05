//! Renderer-owned realtime event overlay policy and storage.

use crate::agents::LifecycleSignal;
use crate::ids::PaneId;
use crate::sidebar::timing::EVENT_STORE_TTL;
use crate::store::event::AGENT_LIFECYCLE_METHOD;
use crate::wakeup::events::SidebarEvent;

const AGENT_REGISTERED_SIGNAL: &str = LifecycleSignal::Registered.tag();
const AGENT_ENDED_SIGNAL: &str = LifecycleSignal::Ended.tag();
const MAX_EVENTS: usize = 256;

impl SidebarEvent {
    pub(crate) fn is_overlay(&self) -> bool {
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

    pub(crate) fn requests_producer_verification(&self) -> bool {
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
                nonce: crate::mux::focus_anchor::FocusNonce::new(),
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
