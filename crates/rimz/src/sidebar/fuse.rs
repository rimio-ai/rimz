//! Pure fusion of pulled sidebar truth, realtime events, and pending jump intent.
//!
//! Pulled truth remains authoritative for observations. Events newer than the
//! pane frame overlay latency-sensitive topology, command, and focus changes
//! until the next pull supersedes them. A pending user focus intent outranks
//! both observation channels until the mux confirms it or its anchor expires.

use std::collections::HashSet;

use crate::mux::focus_anchor::{FocusAnchor, FocusObservation, FocusPresentation};
use crate::sidebar::event_store::EventStore;
use crate::store::snapshot::SidebarSnapshot;
use crate::wakeup::events::SidebarEvent;

/// Move an unmodified pull straight into presentation when no realtime state
/// overlays it. The returned baseline exists only while an overlay or focus
/// fence can outlive this pull; the renderer uses it for later synthetic folds.
/// The owned event cut keeps snapshot cloning and fusion outside the shared
/// event-store lock.
pub fn fuse_owned(
    pulled: SidebarSnapshot,
    events: EventStore,
    intent: Option<&FocusPresentation>,
    now_ms: u64,
) -> (SidebarSnapshot, Option<SidebarSnapshot>, Option<u64>) {
    let active = active_events(&pulled, &events, now_ms);
    if active.is_empty() && intent.is_none() {
        return (pulled, None, None);
    }
    let (fused, focus_event_sent_at_ms) = apply_fusion(pulled.clone(), &active, intent);
    (fused, Some(pulled), focus_event_sent_at_ms)
}

fn active_events<'a>(
    pulled: &SidebarSnapshot,
    events: &'a EventStore,
    now_ms: u64,
) -> Vec<&'a crate::sidebar::event_store::StoredEvent> {
    let baseline = pulled
        .panes_observed_at_ms
        .or(pulled.panes_produced_at_ms)
        .unwrap_or(0);
    let carried_panes = pulled
        .truth_degraded
        .as_ref()
        .map(|notice| notice.pane_ids.iter().cloned().collect::<HashSet<_>>())
        .unwrap_or_default();
    events
        .active(now_ms)
        .filter(|event| event.sent_at_ms > baseline || closes_carried_pane(event, &carried_panes))
        .collect()
}

pub(crate) fn pane_is_focused(
    pulled: &SidebarSnapshot,
    events: &EventStore,
    pane: &crate::ids::PaneId,
    now_ms: u64,
) -> bool {
    active_events(pulled, events, now_ms)
        .into_iter()
        .rev()
        .find_map(|event| match &event.event {
            SidebarEvent::FocusChanged { focused, .. } if focused.len() == 1 => {
                Some(focused.contains(pane))
            }
            SidebarEvent::FocusChanged { focused, .. } if focused.contains(pane) => Some(true),
            SidebarEvent::FocusChanged { unfocused, .. } if unfocused.contains(pane) => Some(false),
            _ => None,
        })
        .unwrap_or_else(|| pulled.client_views.iter().any(|view| &view.pane_id == pane))
}

fn apply_fusion(
    mut fused: SidebarSnapshot,
    active: &[&crate::sidebar::event_store::StoredEvent],
    intent: Option<&FocusPresentation>,
) -> (SidebarSnapshot, Option<u64>) {
    let mut deleted = HashSet::new();
    for event in active {
        if let SidebarEvent::PaneClosed { pane_id } = &event.event {
            deleted.insert(pane_id.clone());
            fused.remove_pane_rows(pane_id);
            remove_carried_notice(&mut fused, pane_id);
        }
    }

    for event in active {
        if let SidebarEvent::CommandChanged { pane_id, command } = &event.event
            && !deleted.contains(pane_id)
        {
            fused.overlay_pane_command(pane_id, command);
        }
    }

    let focus = active
        .iter()
        .filter_map(|event| match &event.event {
            SidebarEvent::FocusChanged { focused, unfocused } => {
                Some((event.sent_at_ms, focused, unfocused))
            }
            _ => None,
        })
        .max_by_key(|(sent_at_ms, _, _)| *sent_at_ms);
    if let Some(focus) = focus {
        // An observed single-client switch replaces its old view; pending intents below stay additive.
        if fused.client_views.len() == 1 && focus.1.len() == 1 {
            fused.viewed_panes.clear();
        }
        fused.overlay_focus(focus.1, focus.2);
    }

    match intent {
        Some(FocusPresentation::Target(intent)) if snapshot_has_pane(&fused, &intent.pane_id) => {
            fused.overlay_focus(std::slice::from_ref(&intent.pane_id), &[]);
        }
        Some(FocusPresentation::Fence) => fused.focused_pane = None,
        _ => {}
    }

    (fused, focus.map(|focus| focus.0))
}

pub(crate) fn focus_intent_confirmed_from(
    pulled: &FocusObservation,
    events: &EventStore,
    intent: &FocusAnchor,
    now_ms: u64,
) -> bool {
    if intent.state != crate::mux::focus_anchor::FocusIntentState::Applied {
        return false;
    }
    let pulled_confirms = pulled
        .panes_observed_at_ms
        .is_some_and(|observed_at_ms| observed_at_ms >= intent.issued_at_ms)
        && !pulled.client_views.is_empty()
        && pulled
            .client_views
            .iter()
            .all(|view| view.pane_id == intent.pane_id);
    let event_confirms = events.active(now_ms).any(|event| {
        event.sent_at_ms >= intent.issued_at_ms
            && matches!(
                &event.event,
                SidebarEvent::FocusChanged { focused, .. }
                    if matches!(focused.as_slice(), [pane] if pane == &intent.pane_id)
            )
    });

    pulled_confirms || event_confirms
}

#[cfg(test)]
fn focus_intent_confirmed(
    pulled: &SidebarSnapshot,
    events: &EventStore,
    intent: &FocusAnchor,
    now_ms: u64,
) -> bool {
    focus_intent_confirmed_from(
        &FocusObservation::from_snapshot(pulled),
        events,
        intent,
        now_ms,
    )
}

fn snapshot_has_pane(snapshot: &SidebarSnapshot, pane_id: &crate::ids::PaneId) -> bool {
    snapshot.row_owning_pane(pane_id).is_some()
}

fn closes_carried_pane(
    event: &crate::sidebar::event_store::StoredEvent,
    carried_panes: &HashSet<crate::ids::PaneId>,
) -> bool {
    match &event.event {
        SidebarEvent::PaneClosed { pane_id } => carried_panes.contains(pane_id),
        _ => false,
    }
}

fn remove_carried_notice(snapshot: &mut SidebarSnapshot, pane_id: &crate::ids::PaneId) {
    let Some(notice) = &mut snapshot.truth_degraded else {
        return;
    };
    let before = notice.pane_ids.len();
    notice.pane_ids.retain(|carried| carried != pane_id);
    if before == notice.pane_ids.len() {
        return;
    }
    notice.carried = notice
        .carried
        .saturating_sub(before - notice.pane_ids.len());
    if notice.carried == 0 {
        snapshot.truth_degraded = None;
    }
}

#[cfg(test)]
mod tests;
