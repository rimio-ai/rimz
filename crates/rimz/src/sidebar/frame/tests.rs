use super::*;
use crate::ids::MuxName;

fn pane(raw: &str, view: &str, command: Option<&str>, focused: bool) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(ViewKind::Tab),
        view_name: None,
        title: None,
        is_focused: focused,
        is_floating: false,
        command: command.map(ToOwned::to_owned),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

#[test]
fn floating_flag_survives_frame_round_trip() {
    let mut pane = pane("terminal_1", "tab_0", Some("codex"), true);
    pane.is_floating = true;
    pane.foreground_cmdline = Some("codex resume session".to_owned());

    let frame = assemble_frame(vec![pane], 7, "rimz-test");
    assert!(frame.tabs[0].panes[0].is_floating);
    assert_eq!(
        frame.tabs[0].panes[0].current.foreground_cmdline.as_deref(),
        Some("codex resume session")
    );

    let projected = frame.to_pane_refs();
    assert_eq!(projected.len(), 1);
    assert!(projected[0].is_floating);
    assert_eq!(
        projected[0].foreground_cmdline.as_deref(),
        Some("codex resume session")
    );
}

#[test]
fn kiro_resume_id_is_stamped_from_direct_mux_command() {
    let session = "sess_11111111-1111-4111-8111-111111111111";
    for command in [
        format!("kiro-cli chat --v3 --resume-id {session}"),
        format!("kiro-cli-chat --resume-id={session}"),
    ] {
        let frame = assemble_frame(
            vec![pane("terminal_1", "tab_0", Some(&command), true)],
            7,
            "rimz-test",
        );
        assert_eq!(
            frame.tabs[0].panes[0].current.resumed_session_id.as_deref(),
            Some(session),
            "{command}"
        );
    }
}

#[test]
fn client_view_sets_session_focus_register() {
    let viewed = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let (frame, diagnostics) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_1", Some("codex"), false),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: std::slice::from_ref(&viewed),
        prior: None,
    });

    assert!(diagnostics.is_empty());
    assert_eq!(frame.focused_pane, Some(viewed));
}

#[test]
fn authoritative_focus_wins_when_live() {
    let authoritative = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let stale_client = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let prior = PaneFrame {
        focused_pane: Some(stale_client.clone()),
        ..assemble_frame(
            vec![
                pane("terminal_1", "tab_0", Some("zsh"), false),
                pane("terminal_2", "tab_1", Some("codex"), false),
            ],
            6,
            "rimz-test",
        )
    };

    let (frame, diagnostics) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_1", Some("codex"), false),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: Some(authoritative.clone()),
        client_viewed: std::slice::from_ref(&stale_client),
        prior: Some(&prior),
    });

    assert!(diagnostics.is_empty());
    assert_eq!(frame.focused_pane, Some(authoritative));
}

#[test]
fn authoritative_focus_ignores_dead_pane_and_falls_through() {
    let raw_focus = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let (frame, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![pane("terminal_1", "tab_0", Some("zsh"), true)],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: Some(PaneId::from_parts(MuxName::Zellij, "terminal_9")),
        client_viewed: &[],
        prior: None,
    });

    assert_eq!(frame.focused_pane, Some(raw_focus));
}

#[test]
fn multiple_client_views_stick_to_prior_or_take_freshest_entry() {
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let prior = PaneFrame {
        focused_pane: Some(second.clone()),
        ..assemble_frame(
            vec![
                pane("terminal_1", "tab_0", Some("zsh"), false),
                pane("terminal_2", "tab_1", Some("codex"), false),
            ],
            6,
            "rimz-test",
        )
    };

    let (sticky, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), false),
            pane("terminal_2", "tab_1", Some("codex"), false),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: &[first.clone(), second.clone()],
        prior: Some(&prior),
    });
    assert_eq!(sticky.focused_pane, Some(second));

    let (freshest, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), false),
            pane("terminal_2", "tab_1", Some("codex"), false),
        ],
        produced_at_ms: 8,
        observed_at_ms: 8,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: std::slice::from_ref(&first),
        prior: Some(&prior),
    });
    assert_eq!(freshest.focused_pane, Some(first));
}

#[test]
fn multiple_client_views_ignore_prior_missing_from_live_frame() {
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let stale = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let prior = PaneFrame {
        focused_pane: Some(stale.clone()),
        ..assemble_frame(
            vec![
                pane("terminal_1", "tab_0", Some("zsh"), false),
                pane("terminal_2", "tab_1", Some("codex"), false),
                pane("terminal_3", "tab_2", Some("vim"), false),
            ],
            6,
            "rimz-test",
        )
    };

    let (frame, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), false),
            pane("terminal_2", "tab_1", Some("codex"), false),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: &[first.clone(), second, stale],
        prior: Some(&prior),
    });

    assert_eq!(frame.focused_pane, Some(first));
}

#[test]
fn detached_focus_uses_prior_then_single_raw_mark() {
    let prior_focus = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let prior = PaneFrame {
        focused_pane: Some(prior_focus.clone()),
        ..assemble_frame(
            vec![
                pane("terminal_1", "tab_0", Some("zsh"), false),
                pane("terminal_2", "tab_0", Some("codex"), false),
            ],
            6,
            "rimz-test",
        )
    };
    let (carried, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("codex"), false),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: &[],
        prior: Some(&prior),
    });
    assert_eq!(carried.focused_pane, Some(prior_focus));

    let (raw, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), false),
            pane("terminal_2", "tab_0", Some("codex"), true),
        ],
        produced_at_ms: 8,
        observed_at_ms: 8,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: &[],
        prior: None,
    });
    assert_eq!(
        raw.focused_pane,
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_2"))
    );
}

#[test]
fn detached_ambiguous_raw_marks_clear_without_live_prior() {
    let (frame, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("codex"), true),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: &[],
        prior: None,
    });

    assert_eq!(frame.focused_pane, None);
}

#[test]
fn sidebar_pane_can_be_the_session_focus_register() {
    let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let (frame, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("rimz sidebar serve"), false),
            pane("terminal_2", "tab_0", Some("zsh"), false),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        authoritative_focus: None,
        client_viewed: std::slice::from_ref(&own),
        prior: None,
    });

    assert_eq!(frame.focused_pane, Some(own));
}

#[test]
fn duplicate_pane_ids_keep_first_and_report_diagnostic() {
    let (frame, diagnostics) = assemble_frame_with_diagnostics(
        vec![
            pane("terminal_1", "tab_0", Some("zsh"), false),
            pane("terminal_1", "tab_0", Some("cargo build"), true),
        ],
        7,
        "rimz-test",
    );

    assert_eq!(frame.pane_states().count(), 1);
    assert_eq!(
        frame.tabs[0].panes[0].current.command.as_deref(),
        Some("zsh")
    );
    assert!(matches!(
        diagnostics.as_slice(),
        [DiagEvent::DuplicatePaneId { pane_id }] if pane_id.raw() == "terminal_1"
    ));
}

#[test]
fn spawn_handoff_rotation_matches_wrapper_identity() {
    let old_start: Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let wrapper = "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main";
    for (name, command, spawn_command, expected_start, expected_previous) in [
        ("spawn wrapper changed", "zsh", "zsh", None, Some("codex")),
        (
            "same spawn wrapper",
            "/usr/bin/codex",
            wrapper,
            Some(old_start),
            None,
        ),
    ] {
        let mut prior = assemble_frame(
            vec![PaneRef {
                command: Some("codex".to_owned()),
                spawn_command: Some(wrapper.to_owned()),
                ..pane("terminal_1", "tab_0", Some("codex"), false)
            }],
            1,
            "rimz-test",
        );
        prior.tabs[0].panes[0].current.started_at = Some(old_start);
        let mut fresh = assemble_frame(
            vec![PaneRef {
                command: Some(command.to_owned()),
                spawn_command: Some(spawn_command.to_owned()),
                ..pane("terminal_1", "tab_0", Some(command), false)
            }],
            2,
            "rimz-test",
        );

        fresh.rotate_against_prior(&prior);

        let state = &fresh.tabs[0].panes[0];
        assert_eq!(state.current.command.as_deref(), Some(command), "{name}");
        assert_eq!(state.current.started_at, expected_start, "{name}");
        assert_eq!(
            state
                .previous
                .as_ref()
                .and_then(|previous| previous.command.as_deref()),
            expected_previous,
            "{name}"
        );
    }
}

#[test]
fn unchanged_command_repairs_raced_nulls_and_keeps_previous() {
    let mut prior = assemble_frame(
        vec![pane("terminal_1", "tab_0", Some("claude"), false)],
        1,
        "rimz-test",
    );
    prior.tabs[0].panes[0].current.pid = Some(42);
    prior.tabs[0].panes[0].current.foreground_cmdline = Some("claude --resume session".to_owned());
    prior.tabs[0].panes[0].current.spawn_command = Some("rimz agents exec claude".to_owned());
    prior.tabs[0].panes[0].previous = Some(PaneProcess {
        pid: Some(41),
        command: Some("zsh".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        started_at: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
    });
    let mut fresh = assemble_frame(
        vec![PaneRef {
            command: None,
            cwd: None,
            pane_pid: None,
            ..pane("terminal_1", "tab_0", None, false)
        }],
        2,
        "rimz-test",
    );

    fresh.rotate_against_prior(&prior);

    let state = &fresh.tabs[0].panes[0];
    assert_eq!(state.current.command.as_deref(), Some("claude"));
    assert_eq!(
        state.current.foreground_cmdline.as_deref(),
        Some("claude --resume session")
    );
    assert_eq!(
        state.current.spawn_command.as_deref(),
        Some("rimz agents exec claude")
    );
    assert_eq!(state.current.cwd.as_deref(), Some("/repo/main"));
    // The pid is never rotation-carried: only the metrics layer restores
    // it, behind its starttime pid-reuse guard.
    assert_eq!(state.current.pid, None);
    assert_eq!(
        state
            .previous
            .as_ref()
            .and_then(|previous| previous.command.as_deref()),
        Some("zsh")
    );
}

#[test]
fn null_fresh_command_backfill_matrix_matches_process_identity() {
    let start: Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    for (name, prior_command, prior_pid, fresh_pid, expected_command) in [
        (
            "active command without fresh pid",
            "git push",
            None,
            None,
            None,
        ),
        (
            "active command with same pid",
            "cargo build",
            Some(42),
            Some(42),
            Some("cargo build"),
        ),
        ("idle command", "zsh", None, None, Some("zsh")),
    ] {
        let mut prior = assemble_frame(
            vec![pane("terminal_1", "tab_0", Some(prior_command), false)],
            1,
            "rimz-test",
        );
        prior.tabs[0].panes[0].current.pid = prior_pid;
        prior.tabs[0].panes[0].current.spawn_command = Some("zsh".to_owned());
        prior.tabs[0].panes[0].current.started_at = Some(start);
        let mut fresh = assemble_frame(
            vec![PaneRef {
                cwd: None,
                pane_pid: fresh_pid,
                ..pane("terminal_1", "tab_0", None, false)
            }],
            2,
            "rimz-test",
        );

        fresh.rotate_against_prior(&prior);

        let state = &fresh.tabs[0].panes[0];
        assert_eq!(state.current.command.as_deref(), expected_command, "{name}");
        assert_eq!(
            state.current.spawn_command.as_deref(),
            Some("zsh"),
            "{name}"
        );
        assert_eq!(state.current.cwd.as_deref(), Some("/repo/main"), "{name}");
        assert_eq!(state.current.started_at, Some(start), "{name}");
    }
}

#[test]
fn pid_change_rejects_prior_tenant_stamp_even_with_same_command() {
    let old_start: Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut prior = assemble_frame(
        vec![pane("terminal_1", "tab_0", Some("codex"), false)],
        1,
        "rimz-test",
    );
    prior.tabs[0].panes[0].current.pid = Some(100);
    prior.tabs[0].panes[0].current.started_at = Some(old_start);
    let mut fresh = assemble_frame(
        vec![pane("terminal_1", "tab_0", Some("codex"), false)],
        2,
        "rimz-test",
    );
    fresh.tabs[0].panes[0].current.pid = Some(200);

    fresh.rotate_against_prior(&prior);

    let state = &fresh.tabs[0].panes[0];
    assert_eq!(state.current.pid, Some(200));
    assert_eq!(state.current.started_at, None);
    assert_eq!(
        state.previous.as_ref().and_then(|previous| previous.pid),
        Some(100)
    );
}

#[test]
fn own_view_derives_from_the_own_tab() {
    let own = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let sibling = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let frame = assemble_frame(
        vec![
            pane("terminal_1", "tab_0", Some("rimz-sidebar"), false),
            pane("terminal_2", "tab_0", Some("zsh"), true),
            pane("terminal_3", "tab_1", Some("cargo build"), true),
        ],
        1,
        "rimz-test",
    );

    // The pane in `tab_1` is not a sibling: the own view counts and names only
    // the panes sharing the own tab.
    let view = SidebarOwnView::from_frame(&own, &frame).expect("own pane is present");

    assert_eq!(view.sibling_count, 1);
    assert_eq!(
        view.working_pane_ids,
        vec![sibling],
        "the working set names only this tab's siblings"
    );
}

fn own_view(own: &str, panes: Vec<PaneRef>) -> Option<SidebarOwnView> {
    let own = PaneId::from_parts(MuxName::Zellij, own);
    SidebarOwnView::from_frame(&own, &assemble_frame(panes, 1, "rimz-test"))
}

#[test]
fn own_view_edge_cases_are_explicit() {
    let view = own_view(
        "terminal_1",
        vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("zsh"), false),
        ],
    )
    .expect("own pane is present");

    assert_eq!(
        view.working_pane_ids,
        vec![PaneId::from_parts(MuxName::Zellij, "terminal_2")]
    );

    // A view the caller cannot find itself in is unknowable — never close.
    let panes = vec![pane("terminal_1", "tab_0", Some("zsh"), true)];
    assert!(own_view("terminal_404", panes).is_none());

    let unfocused_view = own_view(
        "terminal_52",
        vec![
            pane("terminal_52", "tab_11", Some("zsh"), false),
            pane("terminal_53", "tab_11", Some("zsh"), true),
        ],
    )
    .expect("own pane is present");

    assert_eq!(
        unfocused_view.working_pane_ids,
        vec![PaneId::from_parts(MuxName::Zellij, "terminal_53")]
    );
}

/// A pane fixture with a view name, so a test can build the `rimzd` daemon
/// view the tmux window-name fallback recognises.
fn pane_named(raw: &str, view: &str, command: &str, view_name: &str) -> PaneRef {
    PaneRef {
        view_name: Some(view_name.to_owned()),
        ..pane(raw, view, Some(command), false)
    }
}

#[test]
fn own_view_daemon_detection_matches_host_panes() {
    // No view_name on these fixtures: the daemon view is recognised by the
    // host command markers alone, covering builds that omit tab names.
    let zellij = own_view(
        "terminal_0",
        vec![
            pane("terminal_0", "tab_0", Some("rimz-sidebar"), false),
            pane(
                "terminal_1",
                "tab_0",
                Some("claude remote-control --spawn worktree"),
                false,
            ),
            pane(
                "terminal_2",
                "tab_0",
                Some("rimz codex app-server serve"),
                false,
            ),
        ],
    )
    .expect("own pane present");
    assert!(zellij.own_view_is_daemon);

    // tmux: daemon infrastructure is recognised by the window-name fallback
    // even when its command carries no marker.
    let tmux = own_view(
        "terminal_0",
        vec![
            pane_named("terminal_0", "rimzd", "rimz-sidebar", "rimzd"),
            pane_named("terminal_1", "rimzd", "claude", "rimzd"),
        ],
    )
    .expect("own pane present");
    assert!(tmux.own_view_is_daemon);

    let working = own_view(
        "terminal_0",
        vec![
            pane("terminal_0", "tab_1", Some("rimz-sidebar"), false),
            pane("terminal_1", "tab_1", Some("zsh"), false),
        ],
    )
    .expect("own pane present");
    assert!(!working.own_view_is_daemon);
}
