use super::*;

fn pane(id: u32) -> PaneFields {
    PaneFields {
        id,
        is_plugin: false,
        is_focused: false,
        is_suppressed: false,
        exited: false,
        is_held: false,
        tab_position: 0,
        tab_name: Some("main".to_owned()),
        pane_x: Some(0),
        pane_columns: Some(80),
        title: format!("pane-{id}"),
        pane_command: None,
        terminal_command: Some("zsh".to_owned()),
    }
}

fn tabs(panes: Vec<PaneFields>) -> BTreeMap<usize, Vec<PaneFields>> {
    BTreeMap::from([(0, panes)])
}

fn tabs_by_index(entries: Vec<(usize, Vec<PaneFields>)>) -> BTreeMap<usize, Vec<PaneFields>> {
    entries.into_iter().collect()
}

fn pane_in_tab(id: u32, tab: usize) -> PaneFields {
    PaneFields {
        tab_position: tab as u64,
        tab_name: Some(format!("tab-{tab}")),
        ..pane(id)
    }
}

// --- manifest_hash: what changes the hash and what must not ---

#[test]
fn hash_is_stable_over_identical_manifests() {
    let a = manifest_hash(&tabs(vec![pane(1), pane(2)]), Some(0));
    let b = manifest_hash(&tabs(vec![pane(1), pane(2)]), Some(0));
    assert_eq!(a, b);
}

#[test]
fn pane_open_close_changes_the_hash() {
    let one = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let two = manifest_hash(&tabs(vec![pane(1), pane(2)]), Some(0));
    assert_ne!(one, two);
}

#[test]
fn focus_move_changes_the_hash() {
    let unfocused = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let mut focused_pane = pane(1);
    focused_pane.is_focused = true;
    let focused = manifest_hash(&tabs(vec![focused_pane]), Some(0));
    assert_ne!(unfocused, focused);
}

#[test]
fn focus_patch_reports_only_focus_moves() {
    let mut previous_a = pane(1);
    previous_a.is_focused = true;
    let previous_b = pane(2);
    let previous = tabs(vec![previous_a, previous_b]);

    let next_a = pane(1);
    let mut next_b = pane(2);
    next_b.is_focused = true;
    let next = tabs(vec![next_a, next_b]);

    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &next),
        Some(FocusShortcut::Patch(vec![
            FocusPatch {
                id: 1,
                is_focused: false,
            },
            FocusPatch {
                id: 2,
                is_focused: true,
            },
        ]))
    );
}

#[test]
fn focus_patch_ignores_focus_moves_to_sidebar() {
    let mut work = pane(1);
    work.is_focused = true;
    let mut sidebar = pane(2);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    let previous = tabs(vec![work, sidebar]);

    let work = pane(1);
    let mut sidebar = pane(2);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let next = tabs(vec![work, sidebar]);

    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &next),
        Some(FocusShortcut::Ignore)
    );
}

#[test]
fn focus_patch_rejects_non_focus_changes() {
    let previous = tabs(vec![pane(1)]);

    let mut command_changed = pane(1);
    command_changed.terminal_command = Some("codex".to_owned());
    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &tabs(vec![command_changed])),
        None,
    );

    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &tabs(vec![pane(1), pane(2)])),
        None,
    );

    let renamed = PaneFields {
        title: "new title".to_owned(),
        ..pane(1)
    };
    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &tabs(vec![renamed])),
        None,
        "title-only changes are not focus changes and do not need a shortcut"
    );

    let foreground_changed = PaneFields {
        pane_command: Some("codex".to_owned()),
        ..pane(1)
    };
    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &tabs(vec![foreground_changed])),
        None,
        "foreground changes are not focus-only patches"
    );
}

#[test]
fn active_tab_move_does_not_change_the_hash() {
    let on_zero = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let on_one = manifest_hash(&tabs(vec![pane(1)]), Some(1));
    assert_eq!(on_zero, on_one);
}

#[test]
fn command_change_changes_the_hash_and_exit_flag_too() {
    let shell = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let mut agent_pane = pane(1);
    agent_pane.terminal_command = Some("claude".to_owned());
    let agent = manifest_hash(&tabs(vec![agent_pane.clone()]), Some(0));
    assert_ne!(shell, agent);

    agent_pane.exited = true;
    let exited = manifest_hash(&tabs(vec![agent_pane]), Some(0));
    assert_ne!(agent, exited);
}

#[test]
fn live_state_flags_change_the_hash() {
    let live = manifest_hash(&tabs(vec![pane(1)]), Some(0));

    let mut suppressed = pane(1);
    suppressed.is_suppressed = true;
    assert_ne!(
        live,
        manifest_hash(&tabs(vec![suppressed]), Some(0)),
        "suppressed panes disappear from the sidebar's live roster"
    );

    let mut held = pane(1);
    held.is_held = true;
    assert_ne!(
        live,
        manifest_hash(&tabs(vec![held]), Some(0)),
        "held panes are no longer live working panes"
    );

    let mut plugin = pane(1);
    plugin.is_plugin = true;
    assert_ne!(
        live,
        manifest_hash(&tabs(vec![plugin]), Some(0)),
        "plugin panes are chrome, not work rows"
    );
}

#[test]
fn title_is_excluded_by_projection() {
    let mut renamed = pane(1);
    renamed.title = "line-mutated agent title".to_owned();
    let a = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let b = manifest_hash(&tabs(vec![renamed]), Some(0));
    assert_eq!(a, b);
}

#[test]
fn foreground_command_is_excluded_by_projection() {
    let mut changed = pane(1);
    changed.pane_command = Some("codex".to_owned());
    let a = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let b = manifest_hash(&tabs(vec![changed]), Some(0));
    assert_eq!(a, b);
}

#[test]
fn topology_payload_flattens_projected_panes() {
    let mut first = pane(1);
    first.tab_position = 1;
    first.tab_name = Some("agent".to_owned());
    first.pane_x = Some(20);
    first.pane_columns = Some(100);
    let payload = TopologyPayload::from_tabs(
        "rimz-test",
        42,
        &tabs_by_index(vec![(1, vec![first.clone()])]),
    );

    let json = serde_json::to_value(&payload).expect("topology payload serializes");
    assert_eq!(json["session_name"], "rimz-test");
    assert_eq!(json["produced_at_ms"], 42);
    assert_eq!(json["panes"][0]["id"], 1);
    assert_eq!(json["panes"][0]["tab_position"], 1);
    assert_eq!(json["panes"][0]["tab_name"], "agent");
    assert_eq!(json["panes"][0]["pane_x"], 20);
    assert_eq!(json["panes"][0]["pane_columns"], 100);
    assert_eq!(
        json["panes"][0]["terminal_command"],
        first.terminal_command.unwrap()
    );
    assert!(json["panes"][0].get("pane_command").is_none());
}

#[test]
fn topology_payload_waits_for_live_roster() {
    assert_eq!(
        published_topology_payload("rimz-test", 42, None, &BTreeMap::new()),
        None,
        "bootstrap pokes omit --topology until PaneUpdate supplies a live roster"
    );
}

#[test]
fn topology_payload_skips_empty_live_roster() {
    assert_eq!(
        published_topology_payload("rimz-test", 42, Some(&BTreeMap::new()), &BTreeMap::new()),
        None,
        "an empty live roster falls back to list-panes instead of publishing emptiness"
    );
}

#[test]
fn session_update_does_not_shrink_the_published_roster() {
    let full_roster = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
    ]);
    let partial_manifest = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let published_roster = merged_room(&full_roster, &partial_manifest);

    let previous_payload =
        published_topology_payload("rimz-test", 41, Some(&full_roster), &BTreeMap::new())
            .expect("previous roster publishes");
    let partial_payload =
        published_topology_payload("rimz-test", 42, Some(&published_roster), &BTreeMap::new())
            .expect("merged roster publishes");

    assert_eq!(previous_payload.panes.len(), 3);
    assert!(
        partial_payload
            .panes
            .iter()
            .any(|pane| pane.tab_position == 2),
        "partial session-like manifests retain tabs already known through PaneUpdate"
    );
    assert_eq!(partial_payload.panes.len(), 3);
}

#[test]
fn topology_payload_publishes_the_merged_detection_map() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
    ]);
    let mut resized = pane_in_tab(10, 0);
    resized.pane_columns = Some(120);
    let partial_detection = tabs_by_index(vec![(0, vec![resized.clone()])]);
    let merged_detection = merged_room(&previous, &partial_detection);

    let payload =
        published_topology_payload("rimz-test", 42, Some(&merged_detection), &BTreeMap::new())
            .expect("merged detection map publishes");

    assert_eq!(partial_detection.len(), 1);
    assert_eq!(payload.panes[0], resized);
    assert_eq!(
        payload
            .panes
            .iter()
            .map(|pane| pane.tab_position)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
    );
}

#[test]
fn partial_session_update_after_full_roster_keeps_every_tab_published() {
    let live_roster = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
        (3, vec![pane_in_tab(40, 3)]),
    ]);
    let partial_session_update = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);

    let payload = published_topology_payload("rimz-test", 42, Some(&live_roster), &BTreeMap::new())
        .expect("live roster publishes");

    assert_eq!(
        partial_session_update.keys().copied().collect::<Vec<_>>(),
        vec![0, 1],
        "the modeled SessionUpdate omits later worktree tabs",
    );
    assert_eq!(
        payload
            .panes
            .iter()
            .map(|pane| pane.tab_position)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "publication follows the live PaneUpdate roster, not SessionUpdate"
    );
}

#[test]
fn topology_payload_carries_retained_foreground_without_replacing_spawn() {
    let mut foreground = BTreeMap::new();
    foreground.insert(7, "codex".to_owned());
    let mut projected = tabs(vec![PaneFields {
        id: 7,
        terminal_command: Some(
            "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt".to_owned(),
        ),
        ..pane(7)
    }]);

    apply_foreground_commands(&mut projected, &foreground);

    let pane = &projected[&0][0];
    assert_eq!(pane.pane_command.as_deref(), Some("codex"));
    assert_eq!(
        pane.terminal_command.as_deref(),
        Some("/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt")
    );
    let payload = TopologyPayload::from_tabs("rimz-test", 42, &projected);
    let json = serde_json::to_value(&payload).expect("topology payload serializes");
    assert_eq!(json["panes"][0]["pane_command"], "codex");
    assert_eq!(
        json["panes"][0]["terminal_command"],
        "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt"
    );
}

#[test]
fn foreground_overlay_reaches_live_roster_payload() {
    let foreground = BTreeMap::from([(7, "codex".to_owned())]);
    let live_roster = tabs(vec![PaneFields {
        id: 7,
        terminal_command: Some("zsh".to_owned()),
        ..pane(7)
    }]);

    let payload = published_topology_payload("rimz-test", 42, Some(&live_roster), &foreground)
        .expect("live roster publishes");

    assert_eq!(payload.panes[0].pane_command.as_deref(), Some("codex"));
    assert_eq!(payload.panes[0].terminal_command.as_deref(), Some("zsh"));
}

#[test]
fn foreground_retention_survives_rebuild_and_clears_on_close() {
    let command = joined_foreground_command(&["codex".to_owned(), "--foo".to_owned()])
        .expect("joined foreground");
    let mut foreground = BTreeMap::from([(9, command)]);

    let mut rebuilt = tabs(vec![pane(9)]);
    apply_foreground_commands(&mut rebuilt, &foreground);
    assert_eq!(rebuilt[&0][0].pane_command.as_deref(), Some("codex --foo"));

    let mut rebuilt_again = tabs(vec![pane(9)]);
    apply_foreground_commands(&mut rebuilt_again, &foreground);
    assert_eq!(
        rebuilt_again[&0][0].pane_command.as_deref(),
        Some("codex --foo"),
        "a manifest rebuild does not wipe the retained foreground command"
    );

    foreground.remove(&9);
    let mut reused = tabs(vec![pane(9)]);
    apply_foreground_commands(&mut reused, &foreground);
    assert_eq!(
        reused[&0][0].pane_command, None,
        "a pane id reused after close does not inherit stale foreground"
    );
}

#[test]
fn remove_pane_from_live_roster_prunes_empty_tab() {
    let mut live_roster = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);

    remove_pane_from_tabs(&mut live_roster, false, 20);

    assert_eq!(
        live_roster.keys().copied().collect::<Vec<_>>(),
        vec![0],
        "PaneClosed prunes the published roster immediately"
    );
}

// --- manifest merging: partial Zellij manifests must not collapse the room ---

#[test]
fn partial_manifest_merge_retains_absent_tabs() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
    ]);
    let mut updated = pane_in_tab(10, 0);
    updated.pane_columns = Some(120);
    let next = tabs_by_index(vec![(0, vec![updated.clone()])]);

    let merged = merged_room(&previous, &next);

    assert_eq!(merged.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(merged[&0], vec![updated]);
    assert_eq!(merged.get(&1), previous.get(&1));
    assert_eq!(merged.get(&2), previous.get(&2));
}

#[test]
fn carried_tabs_replace_and_prune_closed_panes() {
    let removed = pane_in_tab(11, 0);
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0), removed.clone()]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let next = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);

    let merged = merged_room(&previous, &next);

    assert_eq!(merged, next);
    assert!(!merged.values().flatten().any(|pane| pane.id == removed.id));
}

#[test]
fn empty_manifest_never_prunes() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let next = BTreeMap::new();

    let merged = merged_room(&previous, &next);

    assert_eq!(merged.keys().copied().collect::<Vec<_>>(), vec![0, 1]);
}

#[test]
fn manifest_missing_known_tab_merges_not_collapses() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
    ]);
    let next = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);

    let merged = merged_room(&previous, &next);

    assert_eq!(merged.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(merged.get(&2), previous.get(&2));
}

#[test]
fn first_manifest_replaces_regardless_of_coverage() {
    let next = tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)])]);

    let merged = merged_room(&BTreeMap::new(), &next);

    assert_eq!(merged, next);
}

#[test]
fn opened_card_panes_no_spurious_opens_on_partial() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let mut focused = pane_in_tab(10, 0);
    focused.is_focused = true;
    let partial = tabs_by_index(vec![(0, vec![focused])]);

    let merged = merged_room(&previous, &partial);

    assert!(opened_card_panes(&previous, &merged).is_empty());
}

#[test]
fn focus_only_partial_still_takes_shortcut() {
    let mut previous_focused = pane_in_tab(10, 0);
    previous_focused.is_focused = true;
    let previous = tabs_by_index(vec![
        (0, vec![previous_focused, pane_in_tab(11, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let mut next_focused = pane_in_tab(11, 0);
    next_focused.is_focused = true;
    let partial = tabs_by_index(vec![(0, vec![pane_in_tab(10, 0), next_focused])]);

    let merged = merged_room(&previous, &partial);

    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &merged),
        Some(FocusShortcut::Patch(vec![
            FocusPatch {
                id: 10,
                is_focused: false,
            },
            FocusPatch {
                id: 11,
                is_focused: true,
            },
            FocusPatch {
                id: 20,
                is_focused: false,
            },
        ]))
    );
}

#[test]
fn genuine_open_in_carried_tab_is_reported() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let partial = tabs_by_index(vec![(0, vec![pane_in_tab(10, 0), pane_in_tab(11, 0)])]);

    let merged = merged_room(&previous, &partial);

    assert_eq!(
        opened_card_panes(&previous, &merged),
        vec![pane_in_tab(11, 0)]
    );
}

// --- stranded_sidebar_pane: tab-switch classification reports the sidebar ---

#[test]
fn stranded_sidebar_pane_reports_focused_sidebar_with_working_pane() {
    let mut sidebar = pane(1);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let work = pane(2);

    assert_eq!(
        stranded_sidebar_pane(&tabs(vec![sidebar, work]), Some(0)),
        Some(1),
    );
}

#[test]
fn stranded_sidebar_pane_leaves_working_focus_alone() {
    let mut sidebar = pane(1);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    let mut work = pane(2);
    work.is_focused = true;

    assert_eq!(
        stranded_sidebar_pane(&tabs(vec![sidebar, work]), Some(0)),
        None,
    );
}

#[test]
fn stranded_sidebar_pane_requires_a_live_working_pane() {
    let mut sidebar = pane(1);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let mut held_work = pane(2);
    held_work.is_held = true;

    assert_eq!(
        stranded_sidebar_pane(&tabs(vec![sidebar, held_work]), Some(0)),
        None,
    );
}

// --- FocusCorrection: settle-gated tab-switch classification ---

#[test]
fn focus_correction_does_not_arm_on_load() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(None, Some(0), 10);

    assert_eq!(correction.next_deadline(), None);
    assert_eq!(
        correction.resolve(&tabs(vec![pane(1)]), Some(0), true, 10),
        CorrectionAction::Wait,
    );
}

#[test]
fn focus_correction_clears_on_jump_landing_on_work() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let mut sidebar = pane(10);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    let mut work = pane(11);
    work.is_focused = true;
    let manifest = tabs_by_index(vec![(0, vec![pane(1)]), (1, vec![sidebar, work])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), true, 1_001),
        CorrectionAction::Clear,
        "a fresh manifest from an explicit jump proves work holds focus before the settle deadline"
    );
    assert_eq!(correction.next_deadline(), None);
}

#[test]
fn focus_correction_broadcasts_on_native_switch_at_deadline() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let mut sidebar = pane(10);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let manifest = tabs_by_index(vec![(0, vec![pane(1)]), (1, vec![sidebar, pane(11)])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), false, 1_000 + FOCUS_SETTLE_MS - 1),
        CorrectionAction::Wait,
    );
    assert_eq!(
        correction.resolve(&manifest, Some(1), false, 1_000 + FOCUS_SETTLE_MS),
        CorrectionAction::Broadcast(10),
    );
    assert_eq!(correction.next_deadline(), None);
}

#[test]
fn focus_correction_waits_until_deadline_on_fresh_sidebar_manifest() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let mut sidebar = pane(10);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let manifest = tabs_by_index(vec![(1, vec![sidebar, pane(11)])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), true, 1_001),
        CorrectionAction::Wait,
        "a fresh manifest that still shows sidebar focus may predate an explicit jump's focus mark",
    );
    assert_eq!(
        correction.resolve(&manifest, Some(1), false, 1_000 + FOCUS_SETTLE_MS),
        CorrectionAction::Broadcast(10),
    );
}

#[test]
fn focus_correction_retargets_on_rapid_switches() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);
    correction.on_active_tab_change(Some(1), Some(2), 1_050);

    assert_eq!(correction.next_deadline(), Some(1_050 + FOCUS_SETTLE_MS));

    let mut sidebar_1 = pane(10);
    sidebar_1.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar_1.is_focused = true;
    let mut sidebar_2 = pane(20);
    sidebar_2.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar_2.is_focused = true;
    let manifest = tabs_by_index(vec![
        (1, vec![sidebar_1, pane(11)]),
        (2, vec![sidebar_2, pane(21)]),
    ]);

    assert_eq!(
        correction.resolve(&manifest, Some(2), false, 1_050 + FOCUS_SETTLE_MS),
        CorrectionAction::Broadcast(20),
    );
}

#[test]
fn focus_correction_drops_pending_when_tab_closes() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    assert_eq!(
        correction.resolve(
            &tabs_by_index(vec![(0, vec![pane(1)])]),
            Some(1),
            false,
            1_250
        ),
        CorrectionAction::Clear,
    );
    assert_eq!(correction.next_deadline(), None);
}

#[test]
fn focus_correction_no_broadcast_without_a_live_working_pane() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let mut sidebar = pane(10);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let mut held_work = pane(11);
    held_work.is_held = true;
    let manifest = tabs_by_index(vec![(1, vec![sidebar, held_work])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), false, 1_000 + FOCUS_SETTLE_MS),
        CorrectionAction::Clear,
    );
}

#[test]
fn focus_correction_clears_when_tab_position_renumbered_under_same_focused_pane() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change_with_focus(Some(2), Some(1), Some(42), 1_000);

    let mut sidebar = pane(42);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let manifest = tabs_by_index(vec![(1, vec![sidebar, pane(43)])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), true, 1_001),
        CorrectionAction::Clear,
        "the same focused pane under a new tab position is a renumber, not navigation"
    );
    assert_eq!(correction.next_deadline(), None);
}

#[test]
fn focus_correction_next_deadline_is_settle_after_arm() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 7_000);

    assert_eq!(correction.next_deadline(), Some(7_000 + FOCUS_SETTLE_MS));
}

// --- PokePolicy: immediate change, duplicate floor, keepalive ---

#[test]
fn first_manifest_is_a_baseline_not_a_poke() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    assert_eq!(policy.due(0), Vec::<Poke>::new());
    assert_eq!(
        policy.next_wake_at(),
        KEEPALIVE_MS,
        "only the keepalive is armed after the baseline"
    );
}

#[test]
fn manifest_change_pokes_immediately() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    policy.on_manifest(22, 10);

    assert_eq!(
        policy.due(10),
        vec![Poke::Changed],
        "a sidebar-changing manifest should wake the producer now"
    );
    assert_eq!(policy.due(11), Vec::<Poke>::new());
}

#[test]
fn explicit_signal_pokes_immediately_without_a_manifest_baseline() {
    let mut policy = PokePolicy::new(0);
    policy.on_signal(10);

    assert_eq!(policy.due(10), vec![Poke::Changed]);
}

#[test]
fn change_pokes_once_more_after_the_settle_window() {
    let mut policy = PokePolicy::new(0);
    policy.on_signal(10);

    assert_eq!(policy.due(10), vec![Poke::Changed]);
    assert_eq!(
        policy.next_wake_at(),
        10 + SETTLE_POKE_MS,
        "the post-change settle poke is armed after the immediate one"
    );
    assert_eq!(policy.due(10 + SETTLE_POKE_MS - 1), Vec::<Poke>::new());
    assert_eq!(policy.due(10 + SETTLE_POKE_MS), vec![Poke::Changed]);
    assert_eq!(policy.due(10 + SETTLE_POKE_MS + 1), Vec::<Poke>::new());
}

#[test]
fn optimistic_signal_skips_immediate_poke_but_still_settles() {
    let mut policy = PokePolicy::new(0);
    policy.on_optimistic_signal(10);

    assert_eq!(policy.due(10), Vec::<Poke>::new());
    assert_eq!(policy.next_wake_at(), 10 + SETTLE_POKE_MS);
    assert_eq!(policy.due(10 + SETTLE_POKE_MS), vec![Poke::Changed]);
}

#[test]
fn duplicate_changes_inside_the_floor_defer_once() {
    let mut policy = PokePolicy::new(0);
    policy.on_signal(100);
    assert_eq!(policy.due(100), vec![Poke::Changed]);

    // A split or command handoff can fan out several events. The first one
    // already refreshed panes, so duplicates inside the 100ms floor wait and
    // collapse into one follow-up.
    policy.on_signal(150);
    policy.on_signal(180);
    assert_eq!(policy.due(199), Vec::<Poke>::new());
    assert_eq!(
        policy.next_wake_at(),
        100 + POKE_FLOOR_MS,
        "the follow-up is armed for the floor's end"
    );
    assert_eq!(policy.due(200), vec![Poke::Changed]);
    assert_eq!(
        policy.next_wake_at(),
        200 + SETTLE_POKE_MS,
        "the duplicate-burst poke gets its own settled read"
    );
    assert_eq!(policy.due(201), Vec::<Poke>::new());
}

#[test]
fn explicit_signals_coalesce_with_manifest_changes_inside_the_floor() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    policy.on_signal(10);
    assert_eq!(policy.due(10), vec![Poke::Changed]);

    policy.on_manifest(22, 50);
    policy.on_signal(90);

    assert_eq!(
        policy.due(10 + POKE_FLOOR_MS - 1),
        Vec::<Poke>::new(),
        "the duplicate floor holds the burst"
    );
    assert_eq!(
        policy.due(10 + POKE_FLOOR_MS),
        vec![Poke::Changed],
        "manifest and explicit signals collapse into one follow-up"
    );
}

#[test]
fn change_during_the_floor_is_deferred_never_dropped() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    policy.on_manifest(22, 100);
    assert_eq!(policy.due(100), vec![Poke::Changed]);
    let poked_at = 100;

    // A second change lands well inside the duplicate floor.
    policy.on_manifest(33, poked_at + 50);
    assert_eq!(policy.due(poked_at + POKE_FLOOR_MS - 1), Vec::<Poke>::new());
    assert_eq!(
        policy.next_wake_at(),
        poked_at + POKE_FLOOR_MS,
        "the wake is re-armed for the floor's end"
    );
    assert_eq!(
        policy.due(poked_at + POKE_FLOOR_MS),
        vec![Poke::Changed],
        "the deferred change fires when the floor lifts"
    );
}

#[test]
fn unchanged_manifest_arms_nothing() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    policy.on_manifest(11, 50);
    policy.on_manifest(11, 90);
    assert_eq!(policy.due(1_000), Vec::<Poke>::new());
}

#[test]
fn keepalive_fires_on_cadence_without_changes() {
    let mut policy = PokePolicy::new(0);
    assert_eq!(policy.due(KEEPALIVE_MS - 1), Vec::<Poke>::new());
    assert_eq!(policy.due(KEEPALIVE_MS), vec![Poke::Alive]);
    assert_eq!(
        policy.next_wake_at(),
        2 * KEEPALIVE_MS,
        "the next keepalive re-arms from the firing instant"
    );
}

#[test]
fn change_and_keepalive_can_fire_together() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    policy.on_manifest(22, KEEPALIVE_MS);
    let pokes = policy.due(KEEPALIVE_MS);
    assert!(pokes.contains(&Poke::Changed));
    assert!(pokes.contains(&Poke::Alive));
}

#[test]
fn next_wake_is_the_earlier_of_change_and_keepalive() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    policy.on_manifest(22, 40);
    assert_eq!(
        policy.next_wake_at(),
        40,
        "a pending change wakes immediately before the keepalive"
    );
}

// --- TimerGate: host-timer dedup across superseded chains ---

#[test]
fn timer_gate_dedupes_equal_and_later_deadlines() {
    let mut gate = TimerGate::default();
    assert!(gate.should_arm(1_000), "nothing armed: arm");
    assert!(!gate.should_arm(1_000), "same deadline: already covered");
    assert!(
        !gate.should_arm(5_000),
        "later deadline: the armed timer wakes first"
    );
    assert!(gate.should_arm(500), "earlier deadline supersedes");
}

#[test]
fn timer_gate_collapses_a_superseded_chain() {
    // The load arms the keepalive, then an earlier change supersedes it — two host
    // timers are now outstanding, and Zellij fires both.
    let mut gate = TimerGate::default();
    assert!(gate.should_arm(60_000));
    assert!(gate.should_arm(30_100));

    gate.on_fire(30_100);
    assert!(
        gate.should_arm(60_000),
        "after the earlier timer fires the keepalive re-arms"
    );
    gate.on_fire(60_000);
    assert!(
        gate.should_arm(120_000),
        "the fired keepalive chains forward"
    );

    // The timer superseded at 30_200 fires late, while the 120s chain is
    // outstanding: it must read as stale — clearing the mark here would arm
    // a duplicate for a deadline already covered, and since every fire
    // re-arms one successor, the duplicate would be a chain that never
    // collapses.
    gate.on_fire(60_005);
    assert!(
        !gate.should_arm(120_000),
        "a stale fire arms no duplicate chain"
    );
}

// --- opened_card_panes: which manifest panes earn a card-create poke ---

#[test]
fn opened_card_panes_reports_only_genuinely_new_card_panes() {
    let previous = tabs(vec![pane(1)]);
    let mut sidebar = pane(3);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    let mut plugin = pane(4);
    plugin.is_plugin = true;
    let next = tabs(vec![pane(1), pane(2), sidebar, plugin]);

    let opened = opened_card_panes(&previous, &next);
    assert_eq!(
        opened,
        vec![pane(2)],
        "existing, sidebar, and plugin panes never read as opens"
    );
}

#[test]
fn first_manifest_after_load_reports_no_opens() {
    let next = tabs(vec![pane(1), pane(2)]);
    assert!(
        opened_card_panes(&BTreeMap::new(), &next).is_empty(),
        "the first manifest names every pre-existing pane; the pull covers the room"
    );
}

#[test]
fn a_reused_terminal_id_is_not_an_open_but_a_new_id_space_is() {
    // Terminal and plugin panes have separate id spaces: a terminal pane whose
    // id collides with a known plugin pane is still a genuine open.
    let mut plugin = pane(7);
    plugin.is_plugin = true;
    let previous = tabs(vec![plugin.clone()]);
    let next = tabs(vec![plugin, pane(7)]);

    let opened = opened_card_panes(&previous, &next);
    assert_eq!(opened, vec![pane(7)]);
}
