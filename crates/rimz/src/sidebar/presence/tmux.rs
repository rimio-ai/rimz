use std::collections::{BTreeMap, BTreeSet};

use crate::diag::record::{DiagEvent, WorkPaneBoundaryMove};
use crate::ids::{MuxName, PaneId, ViewId};
use crate::mux::tmux::ControlLine;
use crate::pane::SIDEBAR_CHROME_TITLE;
use crate::sidebar::presence::projector::{
    PaneEventEligibility, PaneObservation, PresencePaneRole, PresenceTransition,
};

#[derive(Default)]
pub(crate) struct TmuxPresenceState {
    panes: BTreeMap<String, PaneEntry>,
    current_window: BTreeMap<String, String>,
    pending_unfocused: BTreeMap<String, String>,
    window_widths: BTreeMap<String, u64>,
}

#[derive(Clone)]
struct PaneEntry {
    window: String,
    command: Option<String>,
    active: bool,
    overlay_suppressed: bool,
    is_sidebar: bool,
    floating: bool,
    x: Option<u64>,
    width: Option<u64>,
}

struct SubscriptionUpdate {
    pane: String,
    window: String,
    command: Option<String>,
    active: bool,
    title: Option<String>,
    floating: bool,
}

impl TmuxPresenceState {
    pub(crate) fn apply(
        &mut self,
        line: ControlLine,
        seeding: bool,
    ) -> (Vec<PresenceTransition>, Option<DiagEvent>) {
        match line {
            ControlLine::Subscription {
                pane,
                window,
                command,
                active,
                title,
                floating,
            } => (
                self.apply_subscription(
                    SubscriptionUpdate {
                        pane,
                        window,
                        command,
                        active,
                        title,
                        floating,
                    },
                    seeding,
                ),
                None,
            ),
            ControlLine::SeedPane {
                pane,
                window,
                command,
                active,
                title,
                floating,
                x,
                width,
                window_width,
            } => {
                self.seed_pane(
                    SubscriptionUpdate {
                        pane,
                        window,
                        command,
                        active,
                        title,
                        floating,
                    },
                    x,
                    width,
                    window_width,
                );
                (Vec::new(), None)
            }
            ControlLine::WindowClosed { window } => (self.close_window(&window), None),
            ControlLine::LayoutChange {
                window,
                window_width,
                panes,
            } => self.apply_layout(&window, window_width, panes),
            ControlLine::WindowPaneChanged { window, pane } => {
                (self.window_pane_changed(window, pane, seeding), None)
            }
            ControlLine::SessionWindowChanged { session, window } => {
                (self.switch_window(session, window, seeding), None)
            }
            ControlLine::Nudge => (vec![PresenceTransition::Nudge], None),
            ControlLine::Ignore => (Vec::new(), None),
        }
    }

    fn seed_pane(&mut self, update: SubscriptionUpdate, x: u64, width: u64, window_width: u64) {
        let is_sidebar = update
            .title
            .as_deref()
            .is_some_and(|value| value.trim() == SIDEBAR_CHROME_TITLE);
        let overlay_suppressed =
            is_sidebar || update.title.is_none() && update.command.as_deref() == Some("rimz");
        self.window_widths
            .insert(update.window.clone(), window_width);
        self.panes.insert(
            update.pane,
            PaneEntry {
                window: update.window,
                command: update.command,
                active: update.active,
                overlay_suppressed,
                is_sidebar,
                floating: update.floating,
                x: Some(x),
                width: Some(width),
            },
        );
    }

    fn apply_subscription(
        &mut self,
        update: SubscriptionUpdate,
        seeding: bool,
    ) -> Vec<PresenceTransition> {
        let SubscriptionUpdate {
            pane,
            window,
            command,
            active,
            title,
            floating,
        } = update;
        let is_sidebar = title
            .as_deref()
            .is_some_and(|value| value.trim() == SIDEBAR_CHROME_TITLE);
        let suppress_overlay = is_sidebar || title.is_none() && command.as_deref() == Some("rimz");
        let role = pane_role(suppress_overlay, is_sidebar);
        let old = self.panes.get(&pane).cloned();
        let (x, width) = old
            .as_ref()
            .map_or((None, None), |entry| (entry.x, entry.width));
        let current = PaneObservation {
            pane_id: pane_id(&pane),
            view: window.clone(),
            command: command.clone(),
            role,
            events: tmux_event_eligibility(role),
        };
        let mut events = Vec::new();

        if !seeding {
            events.push(PresenceTransition::PaneObserved {
                current: current.clone(),
                previous: old.as_ref().map(|entry| pane_observation(&pane, entry)),
            });
        }

        if !seeding
            && !suppress_overlay
            && !active
            && old
                .as_ref()
                .is_some_and(|entry| entry.active && !entry.overlay_suppressed)
        {
            self.pending_unfocused.insert(window.clone(), pane.clone());
        }

        let became_active = active && old.as_ref().is_none_or(|entry| !entry.active);
        if became_active {
            let pending = self.pending_unfocused.remove(&window);
            events.extend(self.focus_became_active(&window, &pane, current, seeding, pending));
        }

        self.panes.insert(
            pane,
            PaneEntry {
                window,
                command,
                active,
                overlay_suppressed: suppress_overlay,
                is_sidebar,
                floating,
                x,
                width,
            },
        );
        events
    }

    fn window_pane_changed(
        &mut self,
        window: String,
        pane: String,
        seeding: bool,
    ) -> Vec<PresenceTransition> {
        let Some(entry) = self.panes.get(&pane) else {
            // The active pane can win the race before its first subscription
            // line. Nudge now; the subscription names it shortly.
            return vec![PresenceTransition::IncompleteLayout];
        };
        if entry.active {
            return Vec::new();
        }
        let focused = pane_observation(&pane, entry);
        let events = self.focus_became_active(&window, &pane, focused, seeding, None);
        if let Some(entry) = self.panes.get_mut(&pane) {
            entry.active = true;
        }
        events
    }

    fn focus_became_active(
        &mut self,
        window: &str,
        pane: &str,
        focused: PaneObservation,
        seeding: bool,
        pending: Option<String>,
    ) -> Vec<PresenceTransition> {
        let mut events = Vec::new();
        if !seeding {
            let prior = self
                .prior_active_working_pane(window, pane)
                .or(pending)
                .filter(|raw| raw != pane)
                .and_then(|raw| {
                    self.panes
                        .get(&raw)
                        .map(|entry| pane_observation(&raw, entry))
                });
            events.push(PresenceTransition::PaneFocused {
                focused: Some(focused),
                prior,
            });
        }
        self.clear_active_in_window(window, pane);
        events
    }

    fn close_window(&mut self, window: &str) -> Vec<PresenceTransition> {
        self.window_widths.remove(window);
        let closed = self
            .panes
            .iter()
            .filter(|(_, entry)| entry.window == window)
            .map(|(pane, entry)| (pane.clone(), pane_observation(pane, entry)))
            .collect::<Vec<_>>();
        for (pane, _) in &closed {
            self.panes.remove(pane);
        }
        closed
            .into_iter()
            .map(|(_, pane)| PresenceTransition::PaneRemoved(pane))
            .collect()
    }

    fn apply_layout(
        &mut self,
        window: &str,
        window_width: u64,
        panes: Vec<(String, u64, u64)>,
    ) -> (Vec<PresenceTransition>, Option<DiagEvent>) {
        let present = panes
            .iter()
            .map(|(pane, _, _)| pane.clone())
            .collect::<BTreeSet<_>>();
        let previous = self
            .panes
            .iter()
            .filter(|(_, entry)| entry.window == window && !entry.floating)
            .filter_map(|(pane, entry)| Some((pane.clone(), (entry.x?, entry.width?))))
            .collect::<BTreeMap<_, _>>();
        let next = panes
            .iter()
            .map(|(pane, x, width)| (pane.clone(), (*x, *width)))
            .collect::<BTreeMap<_, _>>();
        let boundary_move = self
            .window_widths
            .get(window)
            .copied()
            .filter(|prior_width| *prior_width == window_width && previous.keys().eq(next.keys()))
            .and_then(|_| {
                let mut sidebars = self.panes.iter().filter(|(_, entry)| {
                    entry.window == window && !entry.floating && entry.is_sidebar
                });
                let (sidebar, sidebar_entry) = sidebars.next()?;
                if sidebars.next().is_some()
                    || next.get(sidebar).map(|(_, width)| *width) != sidebar_entry.width
                {
                    return None;
                }
                let moves = previous
                    .iter()
                    .filter(|(pane, _)| pane.as_str() != sidebar)
                    .filter_map(|(pane, (from_x, from_cols))| {
                        let (to_x, to_cols) = next.get(pane)?;
                        ((*from_x, *from_cols) != (*to_x, *to_cols)).then(|| WorkPaneBoundaryMove {
                            pane: pane_id(pane),
                            from_x: *from_x,
                            from_cols: *from_cols,
                            to_x: *to_x,
                            to_cols: *to_cols,
                        })
                    })
                    .collect::<Vec<_>>();
                if moves.is_empty() {
                    return None;
                }
                Some(DiagEvent::WorkPaneBoundaryMoved {
                    view_id: ViewId::new_unchecked(window),
                    view_cols: window_width,
                    moves,
                })
            });
        self.window_widths.insert(window.to_owned(), window_width);
        for (pane, x, width) in &panes {
            let entry = self.panes.entry(pane.clone()).or_insert_with(|| PaneEntry {
                window: window.to_owned(),
                command: None,
                active: false,
                overlay_suppressed: false,
                is_sidebar: false,
                floating: false,
                x: None,
                width: None,
            });
            entry.x = Some(*x);
            entry.width = Some(*width);
        }
        let has_floating = self
            .panes
            .values()
            .any(|entry| entry.window == window && entry.floating);
        let closed = self
            .panes
            .iter()
            // tmux 3.7 keeps floating panes outside `window_layout`. Preserve
            // them here and nudge the authoritative poll below because this
            // notification cannot prove whether one opened or closed.
            .filter(|(pane, entry)| {
                entry.window == window
                    && !entry.floating
                    && !present.contains(pane.as_str())
            })
            .map(|(pane, entry)| (pane.clone(), pane_observation(pane, entry)))
            .collect::<Vec<_>>();
        for (pane, _) in &closed {
            self.panes.remove(pane);
        }
        let mut events = closed
            .into_iter()
            .map(|(_, pane)| PresenceTransition::PaneRemoved(pane))
            .collect::<Vec<_>>();
        if has_floating {
            events.push(PresenceTransition::IncompleteLayout);
        }
        (events, boundary_move)
    }

    fn switch_window(
        &mut self,
        session: String,
        window: String,
        seeding: bool,
    ) -> Vec<PresenceTransition> {
        let previous = self.current_window.insert(session, window.clone());
        if seeding || previous.as_deref() == Some(window.as_str()) {
            return Vec::new();
        }
        let focused = self.active_pane_in_window(&window).and_then(|raw| {
            self.panes
                .get(&raw)
                .map(|entry| pane_observation(&raw, entry))
        });
        let prior = previous
            .as_deref()
            .and_then(|prev| self.active_pane_in_window(prev))
            .and_then(|raw| {
                self.panes
                    .get(&raw)
                    .map(|entry| pane_observation(&raw, entry))
            });
        vec![PresenceTransition::ViewSwitched {
            focused,
            prior,
            has_working: self.window_has_working_pane(&window),
            generation: 0,
            clients: Vec::new(),
        }]
    }

    fn active_pane_in_window(&self, window: &str) -> Option<String> {
        self.panes
            .iter()
            .find(|(_, entry)| entry.window == window && entry.active)
            .map(|(pane, _)| pane.clone())
    }

    fn window_has_working_pane(&self, window: &str) -> bool {
        self.panes
            .values()
            .any(|entry| entry.window == window && !entry.overlay_suppressed)
    }

    fn prior_active_working_pane(&self, window: &str, new_active: &str) -> Option<String> {
        self.panes
            .iter()
            .find(|(pane, entry)| {
                pane.as_str() != new_active
                    && entry.window == window
                    && entry.active
                    && !entry.overlay_suppressed
            })
            .map(|(pane, _)| pane.clone())
    }

    fn clear_active_in_window(&mut self, window: &str, active_pane: &str) {
        for (pane, entry) in &mut self.panes {
            if pane != active_pane && entry.window == window {
                entry.active = false;
            }
        }
    }
}

fn pane_id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Tmux, raw)
}

fn pane_role(overlay_suppressed: bool, is_sidebar: bool) -> PresencePaneRole {
    if is_sidebar {
        PresencePaneRole::Sidebar
    } else if overlay_suppressed {
        PresencePaneRole::LaunchChrome
    } else {
        PresencePaneRole::Working
    }
}

fn pane_observation(raw: &str, entry: &PaneEntry) -> PaneObservation {
    let role = pane_role(entry.overlay_suppressed, entry.is_sidebar);
    PaneObservation {
        pane_id: pane_id(raw),
        view: entry.window.clone(),
        command: entry.command.clone(),
        role,
        events: tmux_event_eligibility(role),
    }
}

const fn tmux_event_eligibility(role: PresencePaneRole) -> PaneEventEligibility {
    match role {
        PresencePaneRole::Working => PaneEventEligibility::ALL,
        PresencePaneRole::Sidebar => PaneEventEligibility {
            direct_focus: true,
            ..PaneEventEligibility::NONE
        },
        PresencePaneRole::LaunchChrome => PaneEventEligibility::NONE,
    }
}

#[cfg(test)]
mod tests;
