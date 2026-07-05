use super::*;

#[test]
fn tab_keys_cycle_the_dashboard_and_wrap() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    let mut ui = UiState::default();
    // Selected row 0 is the claude agent, so the derived tab starts there.
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("claude")
    );

    let outcome = handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("codex")
    );
    // The first pick captures the derived kind it began from.
    assert_eq!(
        ui.dashboard_tab
            .as_ref()
            .unwrap()
            .derived_at_start
            .as_deref(),
        Some("claude")
    );

    // A later pick only moves the tab — the anchor holds.
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("pi")
    );
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("claude"),
        "→ wraps past the last tab"
    );
    handle_key(KeyAction::TabPrev, &mut ui, &snapshot);
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("pi"),
        "← wraps back from the first tab"
    );
    assert_eq!(
        ui.dashboard_tab
            .as_ref()
            .unwrap()
            .derived_at_start
            .as_deref(),
        Some("claude"),
        "the browse anchor survives every pick"
    );
}
#[test]
fn tab_keys_noop_without_a_second_cyclable_panel() {
    // TabNext is inert and leaves no browse pick whenever there is nothing to
    // cycle through: a single account, or two auto-mode panels that the
    // dashboard stacks rather than rendering as cyclable tabs.
    let ws = workspace();
    let cases = [
        ("one account: nothing to cycle", vec!["claude"]),
        ("two auto-mode panels are stacked", vec!["claude", "codex"]),
    ];
    for (label, kinds) in cases {
        let mut snapshot = clickable_block_snapshot(&ws);
        snapshot.providers = kinds.iter().map(|k| provider(k)).collect();
        let mut ui = UiState::default();

        let outcome = handle_key(KeyAction::TabNext, &mut ui, &snapshot);

        assert_eq!(outcome, InputOutcome::default(), "{label}");
        assert!(ui.dashboard_tab.is_none(), "{label}");
        // With no tab pick the dashboard shows its first, derived account.
        assert_eq!(
            render::active_dashboard_tab(&snapshot, &ui).as_deref(),
            Some("claude"),
            "{label}"
        );
    }
}

#[test]
fn pets_enabled_keeps_provider_default_and_cycles_providers_only() {
    let ws = workspace();
    let mut snapshot = tabbed_snapshot(&ws);
    snapshot.theme.pets.enabled = true;
    let mut ui = UiState::default();

    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui),
        Some("claude".to_owned())
    );

    let outcome = handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui),
        Some("codex".to_owned())
    );

    handle_key(KeyAction::TabPrev, &mut ui, &snapshot);
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui),
        Some("claude".to_owned()),
        "cycling walks provider tabs only"
    );
}

#[test]
fn tab_pick_holds_until_the_derived_kind_genuinely_changes() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    let agent_pane = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let process_pane = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(agent_pane.clone()));
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("codex")
    );

    // Re-deriving the same claude row keeps the pick.
    reconcile_selection(&mut ui, &snapshot, Some(agent_pane.clone()));
    assert!(ui.dashboard_tab.is_some(), "same derived kind: pick holds");

    // A process-row selection derives no kind — the pick survives the hop.
    reconcile_selection(&mut ui, &snapshot, Some(process_pane));
    assert!(
        ui.dashboard_tab.is_some(),
        "a None derivation never ends the pick"
    );

    // The selected agent row turning into another provider's ends it: the
    // derived kind genuinely changed, so the derived default takes over.
    let mut moved = tabbed_snapshot(&ws);
    moved.worktree_groups[0].rows[0].name = "pi".to_owned();
    reconcile_selection(&mut ui, &moved, Some(agent_pane));
    assert!(
        ui.dashboard_tab.is_none(),
        "a genuine derived-kind change hands the tab back"
    );
    assert_eq!(
        render::active_dashboard_tab(&moved, &ui).as_deref(),
        Some("pi")
    );
}

#[test]
fn dashboard_holds_the_last_agent_across_a_non_agent_selection() {
    let ws = workspace();
    let mut snapshot = tabbed_snapshot(&ws);
    snapshot.worktree_groups[0].rows[0].name = "codex".to_owned();
    let agent_pane = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let process_pane = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &snapshot, Some(agent_pane));
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("codex")
    );

    reconcile_selection(&mut ui, &snapshot, Some(process_pane));
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("codex"),
        "a non-agent selection holds the last agent's tab"
    );
}

#[test]
fn dashboard_ignores_agent_kinds_without_a_panel_for_hold_last() {
    let ws = workspace();
    let mut snapshot = tabbed_snapshot(&ws);
    snapshot.worktree_groups[0].rows[0].name = "codex".to_owned();
    let agent_pane = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &snapshot, Some(agent_pane.clone()));
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("codex")
    );

    let mut unpanelled = tabbed_snapshot(&ws);
    unpanelled.worktree_groups[0].rows[0].name = "opencode".to_owned();
    reconcile_selection(&mut ui, &unpanelled, Some(agent_pane));

    assert_eq!(
        render::active_dashboard_tab(&unpanelled, &ui).as_deref(),
        Some("codex"),
        "only an agent kind with a dashboard panel advances the remembered tab"
    );
}

#[test]
fn tab_pick_drops_when_its_panel_leaves_the_dashboard() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    let agent_pane = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let mut ui = UiState::default();
    reconcile_selection(&mut ui, &snapshot, Some(agent_pane.clone()));
    handle_key(KeyAction::TabNext, &mut ui, &snapshot);

    let mut shrunk = tabbed_snapshot(&ws);
    shrunk.providers = vec![provider("claude"), provider("pi")];
    reconcile_selection(&mut ui, &shrunk, Some(agent_pane));

    assert!(
        ui.dashboard_tab.is_none(),
        "a pick whose panel left the dashboard is dropped"
    );
}
#[test]
fn clicking_a_tab_label_picks_that_tab_in_place() {
    let ws = workspace();
    let snapshot = tabbed_snapshot(&ws);
    // The rail's geometry after the gutter translation: the active
    // `─ Claude ─` chip footprint edge to edge, then the inactive
    // `─ Codex ─` footprint past the 2-cell `──` gap.
    let mut ui = UiState {
        tab_hits: vec![
            crate::sidebar_pane::render::ProviderTabHit {
                line: 30,
                col_start: 3,
                col_end: 13,
                kind: "claude".to_owned(),
            },
            crate::sidebar_pane::render::ProviderTabHit {
                line: 30,
                col_start: 15,
                col_end: 24,
                kind: "codex".to_owned(),
            },
        ],
        ..Default::default()
    };

    let outcome = handle_mouse_click(17, 30, &mut ui, &snapshot);

    // A tab click repaints in place — never a jump.
    assert_eq!(outcome, InputOutcome::redraw());
    assert!(outcome.focus.is_none());
    assert_eq!(
        render::active_dashboard_tab(&snapshot, &ui).as_deref(),
        Some("codex")
    );

    // The hit range is half-open: the cell past the tab falls through to the
    // row hit-test (and lands nowhere on this chrome line).
    let outcome = handle_mouse_click(24, 30, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}
