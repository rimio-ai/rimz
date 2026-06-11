use super::*;
use crate::ids::MuxName;

fn pane(raw: &str, view: &str, command: Option<&str>, focused: bool) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(ViewKind::Tab),
        view_name: None,
        is_focused: focused,
        command: command.map(ToOwned::to_owned),
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

#[test]
fn contested_focus_without_better_signal_uses_lowest_candidate_and_reports() {
    let (frame, diagnostics) = assemble_frame_with_diagnostics(
        vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("cargo build"), true),
            pane("terminal_3", "tab_1", Some("zsh"), true),
        ],
        7,
        "rimz-test",
    );

    assert_eq!(
        frame.tabs[0].active_pane,
        Some(frame.tabs[0].panes[0].pane_id.clone())
    );
    assert!(frame.tabs[0].focus_contested);
    assert_eq!(
        frame.tabs[1].active_pane,
        Some(frame.tabs[1].panes[0].pane_id.clone())
    );
    assert!(!frame.tabs[1].focus_contested);
    assert!(matches!(
        diagnostics.as_slice(),
        [DiagEvent::FocusContested {
            view_id,
            candidates,
            resolved
        }] if view_id.as_str() == "tab_0"
            && candidates.len() == 2
            && resolved.raw() == "terminal_1"
    ));
    let projected = frame.to_pane_refs();
    assert!(projected[0].is_focused);
    assert!(!projected[1].is_focused);
    assert!(projected[2].is_focused);
    let (numeric, diagnostics) = assemble_frame_with_diagnostics(
        vec![
            pane("terminal_10", "tab_0", Some("zsh"), true),
            pane("terminal_9", "tab_0", Some("cargo build"), true),
        ],
        7,
        "rimz-test",
    );

    assert_eq!(
        numeric.tabs[0].active_pane.as_ref().map(PaneId::raw),
        Some("terminal_9")
    );
    assert!(matches!(
        diagnostics.as_slice(),
        [DiagEvent::FocusContested { resolved, .. }] if resolved.raw() == "terminal_9"
    ));
}

#[test]
fn contested_focus_prefers_source_active_candidate() {
    let source = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let (frame, diagnostics) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("cargo build"), true),
        ],
        produced_at_ms: 8,
        observed_at_ms: 4,
        session_name: "rimz-test".to_owned(),
        source_active: BTreeMap::from([(ViewId::new_unchecked("tab_0"), source.clone())]),
        prior: None,
    });

    assert_eq!(frame.observed_at_ms, Some(4));
    assert_eq!(frame.tabs[0].active_pane, Some(source.clone()));
    assert!(matches!(
        diagnostics.as_slice(),
        [DiagEvent::FocusContested { resolved, .. }] if resolved == &source
    ));
}

#[test]
fn contested_focus_prefers_newly_marked_candidate() {
    let prior = assemble_frame(
        vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("cargo build"), false),
        ],
        7,
        "rimz-test",
    );
    let (frame, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("cargo build"), true),
        ],
        produced_at_ms: 8,
        observed_at_ms: 8,
        session_name: "rimz-test".to_owned(),
        source_active: BTreeMap::new(),
        prior: Some(&prior),
    });

    assert_eq!(
        frame.tabs[0].active_pane.as_ref().map(PaneId::raw),
        Some("terminal_2")
    );
}

#[test]
fn contested_focus_sticks_to_prior_when_no_transition_is_visible() {
    let prior = assemble_frame(
        vec![
            pane("terminal_1", "tab_0", Some("zsh"), false),
            pane("terminal_2", "tab_0", Some("cargo build"), true),
            pane("terminal_3", "tab_0", Some("zsh"), false),
        ],
        7,
        "rimz-test",
    );
    let (frame, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("cargo build"), true),
            pane("terminal_3", "tab_0", Some("zsh"), true),
        ],
        produced_at_ms: 8,
        observed_at_ms: 8,
        session_name: "rimz-test".to_owned(),
        source_active: BTreeMap::new(),
        prior: Some(&prior),
    });

    assert_eq!(
        frame.tabs[0].active_pane.as_ref().map(PaneId::raw),
        Some("terminal_2")
    );
}

#[test]
fn two_candidate_contested_focus_sticks_after_prior_contest() {
    let (prior, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("cargo build"), true),
        ],
        produced_at_ms: 7,
        observed_at_ms: 7,
        session_name: "rimz-test".to_owned(),
        source_active: BTreeMap::new(),
        prior: None,
    });
    assert_eq!(
        prior.tabs[0].active_pane.as_ref().map(PaneId::raw),
        Some("terminal_1")
    );
    assert!(prior.tabs[0].focus_contested);

    let (frame, _) = assemble_frame_from_inputs(FrameInputs {
        panes: vec![
            pane("terminal_1", "tab_0", Some("zsh"), true),
            pane("terminal_2", "tab_0", Some("cargo build"), true),
        ],
        produced_at_ms: 8,
        observed_at_ms: 8,
        session_name: "rimz-test".to_owned(),
        source_active: BTreeMap::new(),
        prior: Some(&prior),
    });

    assert_eq!(
        frame.tabs[0].active_pane.as_ref().map(PaneId::raw),
        prior.tabs[0].active_pane.as_ref().map(PaneId::raw)
    );
}

#[test]
fn legacy_frame_without_observed_time_or_focus_contested_parses() {
    let legacy = r#"{
        "produced_at_ms": 7,
        "session_name": "rimz-test",
        "tabs": [{
            "view_id": "tab_0",
            "kind": "tab",
            "active_pane": "zellij:terminal_1",
            "panes": [{
                "pane_id": "zellij:terminal_1",
                "current": {
                    "command": "zsh",
                    "cwd": "/repo/main"
                }
            }]
        }]
    }"#;

    let frame: PaneFrame = serde_json::from_str(legacy).expect("legacy frame parses");

    assert_eq!(frame.produced_at_ms, 7);
    assert_eq!(frame.observed_at_ms, None);
    assert!(frame.carried_panes.is_empty());
    assert!(!frame.tabs[0].focus_contested);
    assert_eq!(
        frame.observed_or_produced_at_ms(),
        frame.produced_at_ms,
        "legacy frames fall back to the publish stamp",
    );
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
fn foreground_change_with_stable_spawn_does_not_rotate() {
    let old_start: Timestamp = "2026-06-05T12:00:00Z".parse().unwrap();
    let mut prior = assemble_frame(
        vec![PaneRef {
            command: Some("codex".to_owned()),
            spawn_command: Some(
                "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main".to_owned(),
            ),
            ..pane("terminal_1", "tab_0", Some("codex"), false)
        }],
        1,
        "rimz-test",
    );
    prior.tabs[0].panes[0].current.started_at = Some(old_start);
    let mut fresh = assemble_frame(
        vec![PaneRef {
            command: Some("/usr/bin/codex".to_owned()),
            spawn_command: Some(
                "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/main".to_owned(),
            ),
            ..pane("terminal_1", "tab_0", Some("codex"), false)
        }],
        2,
        "rimz-test",
    );
    fresh.tabs[0].panes[0].current.started_at = Some(old_start);

    fresh.rotate_against_prior(&prior);

    let state = &fresh.tabs[0].panes[0];
    assert_eq!(state.current.command.as_deref(), Some("/usr/bin/codex"));
    assert_eq!(state.current.started_at, Some(old_start));
    assert!(state.previous.is_none());
}

#[test]
fn unchanged_command_repairs_raced_nulls_and_keeps_previous() {
    let mut prior = assemble_frame(
        vec![pane("terminal_1", "tab_0", Some("claude"), false)],
        1,
        "rimz-test",
    );
    prior.tabs[0].panes[0].current.pid = Some(42);
    prior.tabs[0].panes[0].current.spawn_command = Some("rimz agents exec claude".to_owned());
    prior.tabs[0].panes[0].previous = Some(PaneProcess {
        pid: Some(41),
        command: Some("zsh".to_owned()),
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        started_at: None,
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
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let frame = assemble_frame(
        vec![
            pane("terminal_1", "tab_0", Some("rimz-sidebar"), false),
            pane("terminal_2", "tab_0", Some("zsh"), true),
            pane("terminal_3", "tab_1", Some("cargo build"), true),
        ],
        1,
        "rimz-test",
    );

    let view = SidebarOwnView::from_frame(&own, &frame).expect("own pane is present");

    assert_eq!(view.sibling_count, 1);
    assert!(!view.own_is_active);
    assert_eq!(view.active_pane_id, Some(active.clone()));
    assert_eq!(view.working_pane_ids, vec![active]);
}

fn own_view(own: &str, panes: Vec<PaneRef>) -> Option<SidebarOwnView> {
    let own = PaneId::from_parts(MuxName::Zellij, own);
    SidebarOwnView::from_frame(&own, &assemble_frame(panes, 1, "rimz-test"))
}

#[test]
fn own_view_counts_only_siblings_sharing_the_tab() {
    let focused_here = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let view = own_view(
        "terminal_1",
        vec![
            pane("terminal_1", "tab_0", Some("zsh"), false),
            pane("terminal_2", "tab_0", Some("zsh"), true),
            // Another tab — not a sibling.
            pane("terminal_3", "tab_1", Some("zsh"), true),
        ],
    )
    .expect("own pane is present");

    assert_eq!(view.sibling_count, 1);
    assert!(!view.own_is_active);
    assert_eq!(view.active_pane_id, Some(focused_here.clone()));
    assert_eq!(
        view.working_pane_ids,
        vec![focused_here],
        "the working set names only this tab's siblings — the fused \
         focus filter rides it"
    );
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

    assert!(view.own_is_active);
    assert_eq!(view.active_pane_id, None);

    // A view the caller cannot find itself in is unknowable — never close.
    let panes = vec![pane("terminal_1", "tab_0", Some("zsh"), true)];
    assert!(own_view("terminal_404", panes).is_none());

    // The tab has an active pane but no client is looking at it. The
    // baseline is the tab's active pane, defined regardless of where any
    // client is — so the sidebar in an unviewed tab still points at the
    // pane the user would land on.
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_53");
    let unfocused_view = own_view(
        "terminal_52",
        vec![
            pane("terminal_52", "tab_11", Some("zsh"), false),
            pane("terminal_53", "tab_11", Some("zsh"), true),
        ],
    )
    .expect("own pane is present");

    assert!(!unfocused_view.own_is_active);
    assert_eq!(unfocused_view.active_pane_id, Some(active));
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

    // tmux: a host pane is recognised by the window-name fallback even when
    // its command carries no marker.
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
