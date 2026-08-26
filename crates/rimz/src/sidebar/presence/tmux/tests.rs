use super::*;
use crate::sidebar::events::SidebarEvent;
use crate::sidebar::presence::projector::project_presence;

fn project_apply(
    state: &mut TmuxPresenceState,
    line: ControlLine,
    seeding: bool,
) -> Vec<SidebarEvent> {
    project_presence(state.apply(line, seeding).0)
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

fn layout(window: &str, window_width: u64, panes: &[(&str, u64, u64)]) -> ControlLine {
    ControlLine::LayoutChange {
        window: window.to_owned(),
        window_width,
        panes: panes
            .iter()
            .map(|(id, x, width)| ((*id).to_owned(), *x, *width))
            .collect(),
    }
}

#[test]
fn seed_updates_roster_without_events() {
    let mut roster = TmuxPresenceState::default();
    assert!(
        roster
            .apply(sub("%1", "@1", Some("zsh"), true), true)
            .0
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
        project_apply(&mut roster, layout("@1", 100, &[("%1", 0, 100)]), false),
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
        project_apply(&mut roster, layout("@1", 100, &[("%1", 0, 100)]), false,),
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
        project_apply(&mut roster, layout("@1", 100, &[("%1", 0, 100)]), false,),
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

#[test]
fn layout_audits_only_stable_work_boundary_moves() {
    let mut state = TmuxPresenceState::default();
    state.apply(sidebar_sub("%1", "@7", false), true);
    state.apply(sub("%2", "@7", Some("architect"), false), true);
    state.apply(sub("%3", "@7", Some("zsh"), false), true);
    assert!(
        state
            .apply(
                layout("@7", 213, &[("%1", 0, 55), ("%2", 55, 79), ("%3", 134, 79)],),
                true,
            )
            .1
            .is_none()
    );

    let moved = state.apply(
        layout(
            "@7",
            213,
            &[("%1", 0, 55), ("%2", 55, 47), ("%3", 102, 111)],
        ),
        false,
    );
    assert_eq!(
        moved.1,
        Some(DiagEvent::WorkPaneBoundaryMoved {
            view_id: ViewId::new_unchecked("@7"),
            view_cols: 213,
            moves: vec![
                WorkPaneBoundaryMove {
                    pane: pane_id("%2"),
                    from_x: 55,
                    from_cols: 79,
                    to_x: 55,
                    to_cols: 47,
                },
                WorkPaneBoundaryMove {
                    pane: pane_id("%3"),
                    from_x: 134,
                    from_cols: 79,
                    to_x: 102,
                    to_cols: 111,
                },
            ],
        }),
    );

    assert!(
        state
            .apply(
                layout(
                    "@7",
                    214,
                    &[("%1", 0, 55), ("%2", 55, 47), ("%3", 102, 112)],
                ),
                false,
            )
            .1
            .is_none()
    );
    assert!(
        state
            .apply(
                layout(
                    "@7",
                    214,
                    &[("%1", 0, 56), ("%2", 56, 46), ("%3", 102, 112)],
                ),
                false,
            )
            .1
            .is_none()
    );
    assert!(
        state
            .apply(layout("@7", 214, &[("%1", 0, 56), ("%2", 56, 158)]), false,)
            .1
            .is_none()
    );
}
