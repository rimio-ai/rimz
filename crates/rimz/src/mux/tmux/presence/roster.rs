use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{MuxName, PaneId};
use crate::schema::sidebar_event::SidebarEvent;

use super::super::options::SIDEBAR_PANE_TITLE;
use super::ControlLine;

#[derive(Default)]
pub(crate) struct PresenceRoster {
    panes: BTreeMap<String, PaneEntry>,
    pending_unfocused: BTreeMap<String, String>,
}

#[derive(Clone)]
struct PaneEntry {
    window: String,
    command: Option<String>,
    active: bool,
    overlay_suppressed: bool,
}

impl PresenceRoster {
    pub(crate) fn apply(&mut self, line: ControlLine, seeding: bool) -> Vec<SidebarEvent> {
        match line {
            ControlLine::Subscription {
                pane,
                window,
                command,
                active,
                title,
            } => self.apply_subscription(pane, window, command, active, title, seeding),
            ControlLine::WindowClosed { window } => self.close_window(&window),
            ControlLine::LayoutChange { window, panes } => self.apply_layout(&window, panes),
            ControlLine::Nudge => vec![SidebarEvent::PanesChanged],
            ControlLine::Ignore => Vec::new(),
        }
    }

    fn apply_subscription(
        &mut self,
        pane: String,
        window: String,
        command: Option<String>,
        active: bool,
        title: Option<String>,
        seeding: bool,
    ) -> Vec<SidebarEvent> {
        let is_sidebar = title
            .as_deref()
            .is_some_and(|value| value.trim() == SIDEBAR_PANE_TITLE);
        let suppress_overlay = is_sidebar || title.is_none() && command.as_deref() == Some("rimz");
        let old = self.panes.get(&pane).cloned();
        let mut events = Vec::new();

        if !seeding && !suppress_overlay {
            match old.as_ref() {
                None => events.push(SidebarEvent::PaneOpened {
                    pane_id: pane_id(&pane),
                    command: command.clone(),
                }),
                Some(entry) if entry.command != command => {
                    if let Some(command) = command.clone() {
                        events.push(SidebarEvent::CommandChanged {
                            pane_id: pane_id(&pane),
                            command,
                        });
                    }
                }
                Some(_) => {}
            }
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
            if !seeding && !suppress_overlay {
                let pending = self.pending_unfocused.remove(&window);
                let unfocused = self
                    .prior_active_working_pane(&window, &pane)
                    .or(pending)
                    .filter(|raw| raw != &pane)
                    .map(|raw| pane_id(raw.as_str()))
                    .into_iter()
                    .collect();
                events.push(SidebarEvent::FocusChanged {
                    focused: vec![pane_id(&pane)],
                    unfocused,
                });
            } else {
                self.pending_unfocused.remove(&window);
            }
            self.clear_active_in_window(&window, &pane);
        }

        self.panes.insert(
            pane,
            PaneEntry {
                window,
                command,
                active,
                overlay_suppressed: suppress_overlay,
            },
        );
        events
    }

    fn close_window(&mut self, window: &str) -> Vec<SidebarEvent> {
        let closed = self
            .panes
            .iter()
            .filter(|(_, entry)| entry.window == window)
            .map(|(pane, entry)| (pane.clone(), entry.overlay_suppressed))
            .collect::<Vec<_>>();
        for (pane, _) in &closed {
            self.panes.remove(pane);
        }
        closed
            .into_iter()
            .filter_map(|(pane, overlay_suppressed)| {
                (!overlay_suppressed).then(|| SidebarEvent::PaneClosed {
                    pane_id: pane_id(&pane),
                })
            })
            .collect()
    }

    fn apply_layout(&mut self, window: &str, panes: Vec<String>) -> Vec<SidebarEvent> {
        let present = panes.into_iter().collect::<BTreeSet<_>>();
        let closed = self
            .panes
            .iter()
            .filter(|(pane, entry)| entry.window == window && !present.contains(pane.as_str()))
            .map(|(pane, entry)| (pane.clone(), entry.overlay_suppressed))
            .collect::<Vec<_>>();
        for (pane, _) in &closed {
            self.panes.remove(pane);
        }
        closed
            .into_iter()
            .filter_map(|(pane, overlay_suppressed)| {
                (!overlay_suppressed).then(|| SidebarEvent::PaneClosed {
                    pane_id: pane_id(&pane),
                })
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(pane: &str, window: &str, command: Option<&str>, active: bool) -> ControlLine {
        ControlLine::Subscription {
            pane: pane.to_owned(),
            window: window.to_owned(),
            command: command.map(ToOwned::to_owned),
            active,
            title: None,
        }
    }

    fn sidebar_sub(pane: &str, window: &str, active: bool) -> ControlLine {
        ControlLine::Subscription {
            pane: pane.to_owned(),
            window: window.to_owned(),
            command: Some("rimz".to_owned()),
            active,
            title: Some(SIDEBAR_PANE_TITLE.to_owned()),
        }
    }

    fn untitled_rimz_sub(pane: &str, window: &str, active: bool) -> ControlLine {
        ControlLine::Subscription {
            pane: pane.to_owned(),
            window: window.to_owned(),
            command: Some("rimz".to_owned()),
            active,
            title: None,
        }
    }

    #[test]
    fn seed_updates_roster_without_events() {
        let mut roster = PresenceRoster::default();
        assert!(
            roster
                .apply(sub("%1", "@1", Some("zsh"), true), true)
                .is_empty()
        );
        assert_eq!(
            roster.apply(sub("%1", "@1", Some("claude"), true), false),
            vec![SidebarEvent::CommandChanged {
                pane_id: pane_id("%1"),
                command: "claude".to_owned(),
            }]
        );
    }

    #[test]
    fn new_pane_emits_open_and_focus_when_active() {
        let mut roster = PresenceRoster::default();
        assert_eq!(
            roster.apply(sub("%1", "@1", Some("zsh"), true), false),
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
        let mut roster = PresenceRoster::default();
        roster.apply(sub("%1", "@1", Some("zsh"), false), true);
        assert_eq!(
            roster.apply(sub("%1", "@1", Some("codex"), false), false),
            vec![SidebarEvent::CommandChanged {
                pane_id: pane_id("%1"),
                command: "codex".to_owned(),
            }]
        );
        assert!(roster.apply(sub("%1", "@1", None, false), false).is_empty());
    }

    #[test]
    fn focus_change_unfocuses_prior_active_working_pane() {
        let mut roster = PresenceRoster::default();
        roster.apply(sub("%1", "@1", Some("zsh"), true), true);
        roster.apply(sub("%2", "@1", Some("claude"), false), true);
        assert_eq!(
            roster.apply(sub("%2", "@1", Some("claude"), true), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%2")],
                unfocused: vec![pane_id("%1")],
            }]
        );
    }

    #[test]
    fn focus_change_keeps_unfocused_when_inactive_line_arrives_first() {
        let mut roster = PresenceRoster::default();
        roster.apply(sub("%1", "@1", Some("zsh"), true), true);
        roster.apply(sub("%2", "@1", Some("claude"), false), true);
        assert!(
            roster
                .apply(sub("%1", "@1", Some("zsh"), false), false)
                .is_empty()
        );
        assert_eq!(
            roster.apply(sub("%2", "@1", Some("claude"), true), false),
            vec![SidebarEvent::FocusChanged {
                focused: vec![pane_id("%2")],
                unfocused: vec![pane_id("%1")],
            }]
        );
    }

    #[test]
    fn sidebar_panes_are_tracked_but_do_not_emit() {
        let mut roster = PresenceRoster::default();
        assert!(
            roster
                .apply(sidebar_sub("%9", "@1", true), false)
                .is_empty()
        );
        assert!(
            roster
                .apply(
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
    fn untitled_rimz_panes_are_suppressed_until_proven_work() {
        let mut roster = PresenceRoster::default();
        assert!(
            roster
                .apply(untitled_rimz_sub("%9", "@1", true), false)
                .is_empty()
        );
        assert_eq!(
            roster.apply(sub("%9", "@1", Some("claude"), true), false),
            vec![SidebarEvent::CommandChanged {
                pane_id: pane_id("%9"),
                command: "claude".to_owned(),
            }]
        );
        assert_eq!(
            roster.apply(
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
        let mut roster = PresenceRoster::default();
        roster.apply(sub("%1", "@1", Some("zsh"), false), true);
        roster.apply(sub("%2", "@1", Some("claude"), false), true);
        roster.apply(sub("%3", "@2", Some("codex"), false), true);
        assert_eq!(
            roster.apply(
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
        let mut roster = PresenceRoster::default();
        roster.apply(sub("%1", "@1", Some("zsh"), false), true);
        roster.apply(sub("%2", "@1", Some("claude"), false), true);
        roster.apply(sub("%3", "@2", Some("codex"), false), true);
        assert_eq!(
            roster.apply(
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
    fn nudge_falls_back_to_panes_changed() {
        let mut roster = PresenceRoster::default();
        assert_eq!(
            roster.apply(ControlLine::Nudge, false),
            vec![SidebarEvent::PanesChanged]
        );
    }
}
