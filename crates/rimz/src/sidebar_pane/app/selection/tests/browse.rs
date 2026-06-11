use super::*;

#[test]
fn browse_roams_other_tabs_rows() {
    // The browse pick may walk every visible row — another tab's included
    // (the cross-tab peek that expands a remote card) — while the derived
    // baseline stays untouched underneath.
    let ws = workspace();
    let here = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let remote = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_9", "tab_7", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(here.clone()),
        baseline_pane: Some(here.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    // While browsing the user has the sidebar focused, so frames derive None.
    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, Some(remote), "the pick roams cross-tab");
    assert_eq!(ui.baseline_pane, Some(here), "the baseline never moves");
}
#[test]
fn browse_survives_a_jump_and_ends_on_baseline_change() {
    // A jump mutates nothing, the browse included: an Enter mid-browse
    // leaves the pick in place, so the highlight holds still until the
    // derived baseline catches up underneath it — no flicker back to the
    // old pane. The browse then ends on the genuine baseline change.
    let ws = workspace();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        baseline_pane: Some(anchor.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome.focus, Some(picked.clone()));
    assert!(ui.browse.is_some(), "the jump leaves the browse in place");

    // An inert fold (baseline unchanged) keeps the pick pinned.
    reconcile_selection(&mut ui, &snapshot, Some(anchor));
    assert!(ui.browse.is_some());
    assert_eq!(ui.selected_pane, Some(picked.clone()));

    // The fold that derives the jumped pane ends the browse seamlessly —
    // the baseline takes over on the same pane.
    reconcile_selection(&mut ui, &snapshot, Some(picked.clone()));
    assert_eq!(ui.browse, None, "a real baseline change ends the browse");
    assert_eq!(ui.selected_pane, Some(picked));
}
#[test]
fn continued_browse_keeps_the_first_anchor() {
    // The second arrow continues the browse: the pick moves, but the anchor
    // (baseline_at_start) stays the one captured when browsing began, so a
    // baseline change mid-browse still ends it.
    let ws = workspace();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        baseline_pane: Some(anchor.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    // The baseline advances mid-browse (rule 1 of an intervening fold)...
    ui.baseline_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_3"));
    select_row(&mut ui, &snapshot, 2);
    begin_or_continue_browse(&mut ui);

    assert_eq!(
        ui.browse.as_ref().map(|b| b.baseline_at_start.clone()),
        Some(Some(anchor)),
        "the anchor is the browse-start baseline, not the latest one"
    );
}
#[test]
fn browse_drops_when_its_pane_leaves_the_room() {
    // A browse picks terminal_9, which then closes. The pick must not keep
    // shadowing the baseline — it is dropped, so the highlight reconverges
    // on the next fold.
    let ws = workspace();
    let gone = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let real = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(gone.clone()),
        baseline_pane: Some(real.clone()),
        browse: Some(browse(&gone, Some(&real))),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.browse, None, "the dead pick is dropped");

    // The next fold reconverges on the live baseline.
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.selected_pane, Some(real));
}
