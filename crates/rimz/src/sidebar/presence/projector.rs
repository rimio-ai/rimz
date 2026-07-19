//! Shared host policy for projecting normalized mux presence transitions.

use crate::ids::PaneId;
use crate::mux::ClientPaneView;
use crate::sidebar::events::SidebarEvent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresencePaneRole {
    Working,
    Sidebar,
    LaunchChrome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneEventEligibility {
    pub open: bool,
    pub close: bool,
    pub command: bool,
    pub direct_focus: bool,
}

impl PaneEventEligibility {
    pub const ALL: Self = Self {
        open: true,
        close: true,
        command: true,
        direct_focus: true,
    };

    pub const NONE: Self = Self {
        open: false,
        close: false,
        command: false,
        direct_focus: false,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneObservation {
    pub pane_id: PaneId,
    pub view: String,
    pub command: Option<String>,
    pub role: PresencePaneRole,
    pub events: PaneEventEligibility,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PresenceTransition {
    PaneObserved {
        current: PaneObservation,
        previous: Option<PaneObservation>,
    },
    PaneRemoved(PaneObservation),
    PaneFocused {
        focused: Option<PaneObservation>,
        prior: Option<PaneObservation>,
    },
    ViewSwitched {
        focused: Option<PaneObservation>,
        prior: Option<PaneObservation>,
        has_working: bool,
        generation: u64,
        clients: Vec<ClientPaneView>,
    },
    IncompleteLayout,
    Nudge,
}

/// Project transport-neutral observations into sidebar policy events. Nudge
/// transitions are fallbacks: one batch emits them only when no typed event was
/// derived from stronger evidence.
pub(crate) fn project_presence(
    transitions: impl IntoIterator<Item = PresenceTransition>,
) -> Vec<SidebarEvent> {
    let mut events = Vec::new();
    let mut fallback = false;
    for transition in transitions {
        match transition {
            PresenceTransition::PaneObserved { current, previous } => match previous {
                None if current.events.open => events.push(SidebarEvent::PaneOpened {
                    pane_id: current.pane_id,
                    command: current.command,
                }),
                Some(previous) if current.events.command && previous.command != current.command => {
                    if let Some(command) = current.command {
                        events.push(SidebarEvent::CommandChanged {
                            pane_id: current.pane_id,
                            command,
                        });
                    }
                }
                None => {}
                Some(_) => {}
            },
            PresenceTransition::PaneRemoved(pane) if pane.events.close => {
                events.push(SidebarEvent::PaneClosed {
                    pane_id: pane.pane_id,
                });
            }
            PresenceTransition::PaneRemoved(_) => {}
            PresenceTransition::PaneFocused {
                focused: Some(focused),
                prior,
            } if focused.events.direct_focus => {
                events.push(SidebarEvent::FocusChanged {
                    focused: vec![focused.pane_id],
                    unfocused: prior
                        .filter(|pane| pane.events.direct_focus)
                        .map(|pane| pane.pane_id)
                        .into_iter()
                        .collect(),
                });
            }
            PresenceTransition::PaneFocused {
                focused: None,
                prior,
            } => {
                events.push(SidebarEvent::FocusChanged {
                    focused: Vec::new(),
                    unfocused: prior
                        .filter(|pane| pane.events.direct_focus)
                        .map(|pane| pane.pane_id)
                        .into_iter()
                        .collect(),
                });
            }
            PresenceTransition::PaneFocused { .. } => {}
            PresenceTransition::ViewSwitched {
                focused: Some(focused),
                has_working: true,
                generation,
                clients,
                ..
            } if focused.role == PresencePaneRole::Sidebar => {
                events.push(SidebarEvent::FocusStranded {
                    pane_id: focused.pane_id,
                    generation,
                    clients,
                });
            }
            PresenceTransition::ViewSwitched {
                focused: Some(focused),
                prior,
                ..
            } => {
                events.push(SidebarEvent::FocusChanged {
                    focused: vec![focused.pane_id.clone()],
                    unfocused: prior
                        .filter(|pane| pane.pane_id != focused.pane_id)
                        .map(|pane| pane.pane_id)
                        .into_iter()
                        .collect(),
                });
            }
            PresenceTransition::IncompleteLayout => events.push(SidebarEvent::PanesChanged),
            PresenceTransition::ViewSwitched { .. } | PresenceTransition::Nudge => fallback = true,
        }
    }
    if events.is_empty() && fallback {
        events.push(SidebarEvent::PanesChanged);
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MuxName;

    fn pane(
        mux: MuxName,
        raw: &str,
        role: PresencePaneRole,
        command: Option<&str>,
    ) -> PaneObservation {
        PaneObservation {
            pane_id: PaneId::from_parts(mux, raw),
            view: "view".to_owned(),
            command: command.map(str::to_owned),
            role,
            events: PaneEventEligibility::ALL,
        }
    }

    #[test]
    fn equivalent_transports_project_identical_typed_events() {
        fn transitions(mux: MuxName, prefix: &str) -> Vec<PresenceTransition> {
            let old = pane(
                mux,
                &format!("{prefix}1"),
                PresencePaneRole::Working,
                Some("zsh"),
            );
            let changed = pane(
                mux,
                &format!("{prefix}1"),
                PresencePaneRole::Working,
                Some("codex"),
            );
            let opened = pane(
                mux,
                &format!("{prefix}2"),
                PresencePaneRole::Working,
                Some("cargo"),
            );
            vec![
                PresenceTransition::PaneRemoved(old.clone()),
                PresenceTransition::PaneObserved {
                    current: opened.clone(),
                    previous: None,
                },
                PresenceTransition::PaneObserved {
                    current: changed.clone(),
                    previous: Some(old.clone()),
                },
                PresenceTransition::PaneFocused {
                    focused: Some(changed),
                    prior: Some(old),
                },
            ]
        }

        let tmux = project_presence(transitions(MuxName::Tmux, "%"));
        let zellij = project_presence(transitions(MuxName::Zellij, "terminal_"));
        assert_eq!(tmux.len(), zellij.len());
        assert!(matches!(tmux[0], SidebarEvent::PaneClosed { .. }));
        assert!(matches!(tmux[1], SidebarEvent::PaneOpened { .. }));
        assert!(matches!(tmux[2], SidebarEvent::CommandChanged { .. }));
        assert!(matches!(tmux[3], SidebarEvent::FocusChanged { .. }));
        assert_eq!(
            tmux.iter().map(std::mem::discriminant).collect::<Vec<_>>(),
            zellij
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn eligibility_controls_direct_events_without_changing_view_switches() {
        let mut suppressed = pane(
            MuxName::Tmux,
            "%1",
            PresencePaneRole::LaunchChrome,
            Some("rimz"),
        );
        suppressed.events = PaneEventEligibility::NONE;
        let mut prior = suppressed.clone();
        prior.command = Some("old".to_owned());

        assert_eq!(
            project_presence([
                PresenceTransition::PaneObserved {
                    current: suppressed.clone(),
                    previous: None,
                },
                PresenceTransition::PaneObserved {
                    current: suppressed.clone(),
                    previous: Some(prior.clone()),
                },
                PresenceTransition::PaneRemoved(suppressed.clone()),
                PresenceTransition::PaneFocused {
                    focused: Some(suppressed.clone()),
                    prior: None,
                },
                PresenceTransition::Nudge,
            ]),
            [SidebarEvent::PanesChanged],
        );
        assert_eq!(
            project_presence([PresenceTransition::ViewSwitched {
                focused: Some(suppressed.clone()),
                prior: Some(prior),
                has_working: false,
                generation: 0,
                clients: Vec::new(),
            }]),
            [SidebarEvent::FocusChanged {
                focused: vec![suppressed.pane_id.clone()],
                unfocused: Vec::new(),
            }],
        );
    }

    #[test]
    fn partial_eligibility_suppresses_only_the_named_event_kinds() {
        let mut sidebar = pane(
            MuxName::Zellij,
            "terminal_1",
            PresencePaneRole::Sidebar,
            Some("new"),
        );
        sidebar.events = PaneEventEligibility {
            open: false,
            ..PaneEventEligibility::ALL
        };
        let mut previous = sidebar.clone();
        previous.command = Some("old".to_owned());

        assert_eq!(
            project_presence([
                PresenceTransition::PaneObserved {
                    current: sidebar.clone(),
                    previous: None,
                },
                PresenceTransition::PaneRemoved(sidebar.clone()),
                PresenceTransition::PaneObserved {
                    current: sidebar.clone(),
                    previous: Some(previous),
                },
                PresenceTransition::PaneFocused {
                    focused: Some(sidebar.clone()),
                    prior: None,
                },
            ]),
            [
                SidebarEvent::PaneClosed {
                    pane_id: sidebar.pane_id.clone(),
                },
                SidebarEvent::CommandChanged {
                    pane_id: sidebar.pane_id.clone(),
                    command: "new".to_owned(),
                },
                SidebarEvent::FocusChanged {
                    focused: vec![sidebar.pane_id],
                    unfocused: Vec::new(),
                },
            ],
        );
    }
}
