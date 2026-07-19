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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneObservation {
    pub pane_id: PaneId,
    pub view: String,
    pub command: Option<String>,
    pub role: PresencePaneRole,
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
            PresenceTransition::PaneObserved { current, previous } => {
                if current.role != PresencePaneRole::Working {
                    continue;
                }
                match previous {
                    None => events.push(SidebarEvent::PaneOpened {
                        pane_id: current.pane_id,
                        command: current.command,
                    }),
                    Some(previous) if previous.command != current.command => {
                        if let Some(command) = current.command {
                            events.push(SidebarEvent::CommandChanged {
                                pane_id: current.pane_id,
                                command,
                            });
                        }
                    }
                    Some(_) => {}
                }
            }
            PresenceTransition::PaneRemoved(pane) if pane.role == PresencePaneRole::Working => {
                events.push(SidebarEvent::PaneClosed {
                    pane_id: pane.pane_id,
                });
            }
            PresenceTransition::PaneRemoved(_) => {}
            PresenceTransition::PaneFocused {
                focused: Some(focused),
                prior,
            } if focused.role != PresencePaneRole::LaunchChrome => {
                events.push(SidebarEvent::FocusChanged {
                    focused: vec![focused.pane_id],
                    unfocused: prior
                        .filter(|pane| pane.role != PresencePaneRole::LaunchChrome)
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
                        .filter(|pane| pane.role != PresencePaneRole::LaunchChrome)
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
            } if focused.role != PresencePaneRole::LaunchChrome => {
                events.push(SidebarEvent::FocusChanged {
                    focused: vec![focused.pane_id.clone()],
                    unfocused: prior
                        .filter(|pane| pane.role != PresencePaneRole::LaunchChrome)
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
    fn sidebar_and_launch_chrome_suppress_pane_overlays() {
        let transitions = [
            PresenceTransition::PaneObserved {
                current: pane(MuxName::Tmux, "%1", PresencePaneRole::Sidebar, Some("rimz")),
                previous: None,
            },
            PresenceTransition::PaneObserved {
                current: pane(
                    MuxName::Tmux,
                    "%2",
                    PresencePaneRole::LaunchChrome,
                    Some("rimz"),
                ),
                previous: None,
            },
            PresenceTransition::Nudge,
        ];
        assert_eq!(project_presence(transitions), [SidebarEvent::PanesChanged]);
    }
}
