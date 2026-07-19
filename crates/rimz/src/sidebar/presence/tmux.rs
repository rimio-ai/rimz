use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{MuxName, PaneId};
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
}

#[derive(Clone)]
struct PaneEntry {
    window: String,
    command: Option<String>,
    active: bool,
    overlay_suppressed: bool,
    is_sidebar: bool,
    floating: bool,
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
    pub(crate) fn apply(&mut self, line: ControlLine, seeding: bool) -> Vec<PresenceTransition> {
        match line {
            ControlLine::Subscription {
                pane,
                window,
                command,
                active,
                title,
                floating,
            } => self.apply_subscription(
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
            ControlLine::WindowClosed { window } => self.close_window(&window),
            ControlLine::LayoutChange { window, panes } => self.apply_layout(&window, panes),
            ControlLine::WindowPaneChanged { window, pane } => {
                self.window_pane_changed(window, pane, seeding)
            }
            ControlLine::SessionWindowChanged { session, window } => {
                self.switch_window(session, window, seeding)
            }
            ControlLine::Nudge => vec![PresenceTransition::Nudge],
            ControlLine::Ignore => Vec::new(),
        }
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

    fn apply_layout(&mut self, window: &str, panes: Vec<String>) -> Vec<PresenceTransition> {
        let present = panes.into_iter().collect::<BTreeSet<_>>();
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
        events
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
mod tests {
    use super::*;
    use crate::sidebar::events::SidebarEvent;
    use crate::sidebar::presence::projector::project_presence;

    fn project_apply(
        state: &mut TmuxPresenceState,
        line: ControlLine,
        seeding: bool,
    ) -> Vec<SidebarEvent> {
        project_presence(state.apply(line, seeding))
    }

    fn sub(pane: &str, window: &str, command: Option<&str>, active: bool) -> ControlLine {
        ControlLine::Subscription {
            pane: pane.to_owned(),
            window: window.to_owned(),
            command: command.map(ToOwned::to_owned),
            active,
            title: None,
            floating: false,
        }
    }

    fn floating_sub(pane: &str, window: &str, command: Option<&str>) -> ControlLine {
        ControlLine::Subscription {
            pane: pane.to_owned(),
            window: window.to_owned(),
            command: command.map(ToOwned::to_owned),
            active: false,
            title: None,
            floating: true,
        }
    }

    fn sidebar_sub(pane: &str, window: &str, active: bool) -> ControlLine {
        ControlLine::Subscription {
            pane: pane.to_owned(),
            window: window.to_owned(),
            command: Some("rimz".to_owned()),
            active,
            title: Some(SIDEBAR_CHROME_TITLE.to_owned()),
            floating: false,
        }
    }

    fn untitled_rimz_sub(pane: &str, window: &str, active: bool) -> ControlLine {
        ControlLine::Subscription {
            pane: pane.to_owned(),
            window: window.to_owned(),
            command: Some("rimz".to_owned()),
            active,
            title: None,
            floating: false,
        }
    }

    fn swin(session: &str, window: &str) -> ControlLine {
        ControlLine::SessionWindowChanged {
            session: session.to_owned(),
            window: window.to_owned(),
        }
    }

    fn wpane(window: &str, pane: &str) -> ControlLine {
        ControlLine::WindowPaneChanged {
            window: window.to_owned(),
            pane: pane.to_owned(),
        }
    }

    #[test]
    fn seed_updates_roster_without_events() {
        let mut roster = TmuxPresenceState::default();
        assert!(
            roster
                .apply(sub("%1", "@1", Some("zsh"), true), true)
                .is_empty()
        );
        assert_eq!(
            project_apply(&mut roster, sub("%1", "@1", Some("claude"), true), false),
            vec![SidebarEvent::CommandChanged {
                pane_id: pane_id("%1"),
                command: "claude".to_owned(),
            }]
        );
    }

    #[test]
    fn new_pane_emits_open_and_focus_when_active() {
        let mut roster = TmuxPresenceState::default();
        assert_eq!(
            project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), false),
            vec![
                SidebarEvent::PaneOpened {
                    pane_id: pane_id("%1"),
                    command: Some("zsh".to_owned()),
                },
                SidebarEvent::FocusChanged {
                    focused: vec![pane_id("%1")],
                    unfocused: Vec::new(),
                },
            ]
        );
    }

    #[test]
    fn existing_pane_command_change_emits_command_event() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), false), true);
        assert_eq!(
            project_apply(&mut roster, sub("%1", "@1", Some("codex"), false), false),
            vec![SidebarEvent::CommandChanged {
                pane_id: pane_id("%1"),
                command: "codex".to_owned(),
            }]
        );
        assert!(project_apply(&mut roster, sub("%1", "@1", None, false), false).is_empty());
    }

    #[test]
    fn seed_window_switch_records_current_window_without_events() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sub("%2", "@2", Some("claude"), true), true);
        assert!(project_apply(&mut roster, swin("$1", "@1"), true).is_empty());
        assert_eq!(
            roster.current_window.get("$1").map(String::as_str),
            Some("@1")
        );
        assert_eq!(
            project_apply(&mut roster, swin("$1", "@2"), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%2")],
                unfocused: vec![pane_id("%1")],
            }]
        );
    }

    #[test]
    fn window_switch_can_focus_sidebar_pane() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sidebar_sub("%9", "@2", true), true);
        project_apply(&mut roster, swin("$1", "@1"), true);
        // A sidebar-only window has no working sibling to refocus, so the
        // switch remains a plain focus overlay.
        assert_eq!(
            project_apply(&mut roster, swin("$1", "@2"), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%9")],
                unfocused: vec![pane_id("%1")],
            }]
        );
    }

    #[test]
    fn window_switch_focuses_launch_chrome_in_both_directions() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, untitled_rimz_sub("%9", "@2", true), true);
        project_apply(&mut roster, swin("$1", "@1"), true);

        assert_eq!(
            project_apply(&mut roster, swin("$1", "@2"), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%9")],
                unfocused: vec![pane_id("%1")],
            }],
        );
        assert_eq!(
            project_apply(&mut roster, swin("$1", "@1"), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%1")],
                unfocused: vec![pane_id("%9")],
            }],
        );
    }

    #[test]
    fn window_switch_onto_sidebar_with_work_sibling_strands() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sub("%2", "@2", Some("claude"), false), true);
        project_apply(&mut roster, sidebar_sub("%9", "@2", true), true);
        project_apply(&mut roster, swin("$1", "@1"), true);
        assert_eq!(
            project_apply(&mut roster, swin("$1", "@2"), false),
            vec![SidebarEvent::FocusStranded {
                pane_id: pane_id("%9"),
                generation: 0,
                clients: Vec::new(),
            }]
        );
    }

    #[test]
    fn window_switch_with_unknown_active_pane_falls_back_to_panes_changed() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sub("%2", "@2", Some("claude"), false), true);
        project_apply(&mut roster, swin("$1", "@1"), true);
        assert_eq!(
            project_apply(&mut roster, swin("$1", "@2"), false),
            vec![SidebarEvent::PanesChanged]
        );
    }

    #[test]
    fn window_switch_to_current_window_emits_nothing() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, swin("$1", "@1"), true);
        assert!(project_apply(&mut roster, swin("$1", "@1"), false).is_empty());
    }

    #[test]
    fn focus_change_unfocuses_prior_active_working_pane() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sub("%2", "@1", Some("claude"), false), true);
        assert_eq!(
            project_apply(&mut roster, sub("%2", "@1", Some("claude"), true), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%2")],
                unfocused: vec![pane_id("%1")],
            }]
        );
    }

    #[test]
    fn focus_change_keeps_unfocused_when_inactive_line_arrives_first() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sub("%2", "@1", Some("claude"), false), true);
        assert!(project_apply(&mut roster, sub("%1", "@1", Some("zsh"), false), false).is_empty());
        assert_eq!(
            project_apply(&mut roster, sub("%2", "@1", Some("claude"), true), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%2")],
                unfocused: vec![pane_id("%1")],
            }]
        );
    }

    #[test]
    fn window_pane_change_focuses_new_active_working_pane() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sub("%2", "@1", Some("claude"), false), true);
        assert_eq!(
            project_apply(&mut roster, wpane("@1", "%2"), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%2")],
                unfocused: vec![pane_id("%1")],
            }]
        );
        assert!(!roster.panes.get("%1").is_some_and(|entry| entry.active));
        assert!(roster.panes.get("%2").is_some_and(|entry| entry.active));
    }

    #[test]
    fn window_pane_change_for_already_active_pane_emits_nothing() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        assert!(project_apply(&mut roster, wpane("@1", "%1"), false).is_empty());
        assert!(roster.panes.get("%1").is_some_and(|entry| entry.active));
    }

    #[test]
    fn window_pane_change_for_unknown_pane_falls_back_to_panes_changed() {
        let mut roster = TmuxPresenceState::default();
        assert_eq!(
            project_apply(&mut roster, wpane("@1", "%2"), false),
            vec![SidebarEvent::PanesChanged]
        );
    }

    #[test]
    fn window_pane_change_can_focus_sidebar_pane() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sidebar_sub("%9", "@1", false), true);
        assert_eq!(
            project_apply(&mut roster, wpane("@1", "%9"), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%9")],
                unfocused: vec![pane_id("%1")],
            }]
        );
        assert!(!roster.panes.get("%1").is_some_and(|entry| entry.active));
        assert!(roster.panes.get("%9").is_some_and(|entry| entry.active));
    }

    #[test]
    fn window_pane_change_suppresses_untitled_rimz_overlay() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, untitled_rimz_sub("%9", "@1", false), true);
        assert!(project_apply(&mut roster, wpane("@1", "%9"), false).is_empty());
        assert!(!roster.panes.get("%1").is_some_and(|entry| entry.active));
        assert!(roster.panes.get("%9").is_some_and(|entry| entry.active));
    }

    #[test]
    fn window_pane_change_seeds_state_without_events() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        project_apply(&mut roster, sub("%2", "@1", Some("claude"), false), true);
        assert!(project_apply(&mut roster, wpane("@1", "%2"), true).is_empty());
        assert!(!roster.panes.get("%1").is_some_and(|entry| entry.active));
        assert!(roster.panes.get("%2").is_some_and(|entry| entry.active));
    }

    #[test]
    fn sidebar_pane_focus_names_sidebar_pane() {
        let mut roster = TmuxPresenceState::default();
        assert_eq!(
            project_apply(&mut roster, sidebar_sub("%9", "@1", true), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%9")],
                unfocused: Vec::new(),
            }]
        );
        assert!(
            project_apply(
                &mut roster,
                ControlLine::WindowClosed {
                    window: "@1".to_owned()
                },
                false
            )
            .is_empty()
        );
        assert!(!roster.panes.contains_key("%9"));
    }

    #[test]
    fn sidebar_pane_focus_unfocuses_prior_active_working_pane() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), true), true);
        assert_eq!(
            project_apply(&mut roster, sidebar_sub("%9", "@1", true), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%9")],
                unfocused: vec![pane_id("%1")],
            }]
        );
    }

    #[test]
    fn untitled_rimz_panes_are_suppressed_until_proven_work() {
        let mut roster = TmuxPresenceState::default();
        assert!(project_apply(&mut roster, untitled_rimz_sub("%9", "@1", true), false).is_empty());
        assert_eq!(
            project_apply(&mut roster, sub("%9", "@1", Some("claude"), true), false),
            vec![SidebarEvent::CommandChanged {
                pane_id: pane_id("%9"),
                command: "claude".to_owned(),
            }]
        );
        assert_eq!(
            project_apply(
                &mut roster,
                ControlLine::WindowClosed {
                    window: "@1".to_owned()
                },
                false
            ),
            vec![SidebarEvent::PaneClosed {
                pane_id: pane_id("%9"),
            }]
        );
    }

    #[test]
    fn window_close_drains_working_panes() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), false), true);
        project_apply(&mut roster, sub("%2", "@1", Some("claude"), false), true);
        project_apply(&mut roster, sub("%3", "@2", Some("codex"), false), true);
        assert_eq!(
            project_apply(
                &mut roster,
                ControlLine::WindowClosed {
                    window: "@1".to_owned()
                },
                false
            ),
            vec![
                SidebarEvent::PaneClosed {
                    pane_id: pane_id("%1"),
                },
                SidebarEvent::PaneClosed {
                    pane_id: pane_id("%2"),
                },
            ]
        );
        assert!(!roster.panes.contains_key("%1"));
        assert!(!roster.panes.contains_key("%2"));
        assert!(roster.panes.contains_key("%3"));
    }

    #[test]
    fn layout_change_closes_roster_panes_missing_from_window() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), false), true);
        project_apply(&mut roster, sub("%2", "@1", Some("claude"), false), true);
        project_apply(&mut roster, sub("%3", "@2", Some("codex"), false), true);
        assert_eq!(
            project_apply(
                &mut roster,
                ControlLine::LayoutChange {
                    window: "@1".to_owned(),
                    panes: vec!["%1".to_owned()],
                },
                false
            ),
            vec![SidebarEvent::PaneClosed {
                pane_id: pane_id("%2"),
            }]
        );
        assert!(roster.panes.contains_key("%1"));
        assert!(!roster.panes.contains_key("%2"));
        assert!(roster.panes.contains_key("%3"));
    }

    #[test]
    fn layout_change_preserves_floating_panes_omitted_from_tmux_layout() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), false), true);
        project_apply(&mut roster, floating_sub("%2", "@1", Some("codex")), true);
        assert_eq!(
            project_apply(
                &mut roster,
                ControlLine::LayoutChange {
                    window: "@1".to_owned(),
                    panes: vec!["%1".to_owned()],
                },
                false,
            ),
            vec![SidebarEvent::PanesChanged],
        );
        assert!(roster.panes.contains_key("%1"));
        assert!(roster.panes.contains_key("%2"));
    }

    #[test]
    fn layout_change_closes_tiled_pane_while_preserving_floating_sibling() {
        let mut roster = TmuxPresenceState::default();
        project_apply(&mut roster, sub("%1", "@1", Some("zsh"), false), true);
        project_apply(&mut roster, sub("%2", "@1", Some("claude"), false), true);
        project_apply(&mut roster, floating_sub("%3", "@1", Some("codex")), true);
        // The tiled `%2` drops out of the layout and closes; the floating `%3`
        // is absent from the layout string too but must survive, so the event
        // names only `%2` and appends a nudge for the floating-pane poll.
        assert_eq!(
            project_apply(
                &mut roster,
                ControlLine::LayoutChange {
                    window: "@1".to_owned(),
                    panes: vec!["%1".to_owned()],
                },
                false,
            ),
            vec![
                SidebarEvent::PaneClosed {
                    pane_id: pane_id("%2"),
                },
                SidebarEvent::PanesChanged,
            ],
        );
        assert!(roster.panes.contains_key("%1"));
        assert!(!roster.panes.contains_key("%2"));
        assert!(roster.panes.contains_key("%3"));
    }

    #[test]
    fn nudge_falls_back_to_panes_changed() {
        let mut roster = TmuxPresenceState::default();
        assert_eq!(
            project_apply(&mut roster, ControlLine::Nudge, false),
            vec![SidebarEvent::PanesChanged]
        );
    }
}
