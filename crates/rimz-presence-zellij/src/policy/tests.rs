use super::*;

fn pane(id: u32) -> PaneFields {
    PaneFields {
        id,
        is_plugin: false,
        is_focused: false,
        is_suppressed: false,
        is_floating: false,
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

fn raw_hash_from_tabs(tabs: &BTreeMap<usize, Vec<PaneFields>>) -> u64 {
    raw_stable_hash(tabs.iter().flat_map(|(tab, panes)| {
        panes
            .iter()
            .map(move |pane| (*tab, RawStablePaneFields::from_projected(pane)))
    }))
}

fn pane_in_tab(id: u32, tab: usize) -> PaneFields {
    PaneFields {
        tab_position: tab as u64,
        tab_name: Some(format!("tab-{tab}")),
        ..pane(id)
    }
}

fn sidebar_pane(id: u32) -> PaneFields {
    PaneFields {
        title: SIDEBAR_PANE_TITLE.to_owned(),
        ..pane(id)
    }
}

fn plugin_pane(id: u32) -> PaneFields {
    PaneFields {
        is_plugin: true,
        ..pane(id)
    }
}

fn focused(mut pane: PaneFields) -> PaneFields {
    pane.is_focused = true;
    pane
}

// --- manifest_hash: the projected stable subset folds; the rest does not ---

#[test]
fn identical_manifests_hash_equal() {
    let a = manifest_hash(&tabs(vec![pane(1), pane(2)]), Some(0));
    let b = manifest_hash(&tabs(vec![pane(1), pane(2)]), Some(0));
    assert_eq!(a, b);
}

#[test]
fn every_stable_field_changes_the_hash() {
    type Mutate = fn(&mut PaneFields);
    let base = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let cases: &[(&str, Mutate)] = &[
        ("focus", |p| p.is_focused = true),
        ("terminal_command", |p| {
            p.terminal_command = Some("claude".to_owned())
        }),
        ("exited", |p| p.exited = true),
        ("suppressed", |p| p.is_suppressed = true),
        ("held", |p| p.is_held = true),
        ("plugin", |p| p.is_plugin = true),
        ("floating", |p| p.is_floating = true),
        ("tab_position", |p| p.tab_position = 7),
        ("tab_name", |p| p.tab_name = Some("renamed".to_owned())),
        ("pane_x", |p| p.pane_x = Some(20)),
        ("pane_columns", |p| p.pane_columns = Some(120)),
    ];
    for &(field, mutate) in cases {
        let mut changed = pane(1);
        mutate(&mut changed);
        assert_ne!(
            base,
            manifest_hash(&tabs(vec![changed]), Some(0)),
            "{field} must fold into the roster hash",
        );
    }
    // Opening or closing a pane changes the set of folded ids.
    assert_ne!(
        base,
        manifest_hash(&tabs(vec![pane(1), pane(2)]), Some(0)),
        "pane count must fold into the roster hash",
    );
}

#[test]
fn excluded_fields_and_navigation_hold_the_hash() {
    let base = manifest_hash(&tabs(vec![pane(1)]), Some(0));

    // The active tab is navigation, not roster shape.
    assert_eq!(base, manifest_hash(&tabs(vec![pane(1)]), Some(1)));

    // Titles mutate per output line and the foreground command publishes
    // through CommandChanged; neither belongs in the roster hash.
    let mut renamed = pane(1);
    renamed.title = "line-mutated agent title".to_owned();
    assert_eq!(
        base,
        manifest_hash(&tabs(vec![renamed]), Some(0)),
        "title is excluded by projection",
    );

    let mut foreground = pane(1);
    foreground.pane_command = Some("codex".to_owned());
    assert_eq!(
        base,
        manifest_hash(&tabs(vec![foreground]), Some(0)),
        "pane_command is excluded by projection",
    );
}

#[test]
fn raw_stable_hash_ignores_titles_but_tracks_stable_fields() {
    let base = tabs(vec![pane(1)]);
    let mut renamed = pane(1);
    renamed.title = "line-mutated agent title".to_owned();
    assert_eq!(
        raw_hash_from_tabs(&base),
        raw_hash_from_tabs(&tabs(vec![renamed])),
        "title-only PaneUpdate events must stay on the cheap path",
    );

    let mut focused = pane(1);
    focused.is_focused = true;
    assert_ne!(
        raw_hash_from_tabs(&base),
        raw_hash_from_tabs(&tabs(vec![focused])),
        "focus changes are stable pane state",
    );

    let mut resized = pane(1);
    resized.pane_columns = Some(120);
    assert_ne!(
        raw_hash_from_tabs(&base),
        raw_hash_from_tabs(&tabs(vec![resized])),
        "geometry changes are stable pane state",
    );
}

// --- focus_shortcut: focus-only moves take the optimistic CLI patch ---

#[test]
fn focus_shortcut_patches_card_to_card_moves() {
    let previous = tabs(vec![focused(pane(1)), pane(2)]);
    let next = tabs(vec![pane(1), focused(pane(2))]);

    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &next),
        Some(vec![
            FocusPatch {
                id: 1,
                is_focused: false,
            },
            FocusPatch {
                id: 2,
                is_focused: true,
            },
        ])
    );
}

#[test]
fn focus_shortcut_patches_focus_onto_the_sidebar() {
    let previous = tabs(vec![focused(pane(1)), sidebar_pane(2)]);
    let next = tabs(vec![pane(1), focused(sidebar_pane(2))]);

    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &next),
        Some(vec![
            FocusPatch {
                id: 1,
                is_focused: false,
            },
            FocusPatch {
                id: 2,
                is_focused: true,
            },
        ])
    );
}

#[test]
fn focus_shortcut_rejects_non_focus_changes() {
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
        "an opened pane is a topology change, not a focus-only patch",
    );

    let renamed = PaneFields {
        title: "new title".to_owned(),
        ..pane(1)
    };
    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &tabs(vec![renamed])),
        None,
        "a title-only change is not a focus change",
    );

    let foreground_changed = PaneFields {
        pane_command: Some("codex".to_owned()),
        ..pane(1)
    };
    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &tabs(vec![foreground_changed])),
        None,
        "a foreground change is not a focus-only patch",
    );

    let floating = PaneFields {
        is_floating: true,
        ..pane(1)
    };
    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &tabs(vec![floating])),
        None,
        "a tiled/floating change is a topology change, not a focus-only patch",
    );
}

#[test]
fn focus_shortcut_survives_a_partial_manifest_merge() {
    let previous = tabs_by_index(vec![
        (0, vec![focused(pane_in_tab(10, 0)), pane_in_tab(11, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let partial = tabs_by_index(vec![(
        0,
        vec![pane_in_tab(10, 0), focused(pane_in_tab(11, 0))],
    )]);

    let merged = merged_room(&previous, &partial);

    assert_eq!(
        focus_shortcut_if_only_focus_changed(&previous, &merged),
        Some(vec![
            FocusPatch {
                id: 10,
                is_focused: false,
            },
            FocusPatch {
                id: 11,
                is_focused: true,
            },
        ]),
        "a focus-only partial still patches after the omitted tab is merged back",
    );
}

// --- FocusResolution: which pane a contested manifest resolves to ---

#[test]
fn focus_resolution_prefers_the_false_to_true_transition() {
    let previous = tabs_by_index(vec![(
        0,
        vec![focused(pane_in_tab(10, 0)), pane_in_tab(11, 0)],
    )]);
    let next = tabs_by_index(vec![(
        0,
        vec![focused(pane_in_tab(10, 0)), focused(pane_in_tab(11, 0))],
    )]);

    let mut resolution = FocusResolution::default();
    resolution.fold_pane_update(&previous, &next);

    assert_eq!(resolution.focused_pane_id(Some(0)), Some(11));
    assert_eq!(resolution.active_panes_payload().get(&0_u64), Some(&11));
}

#[test]
fn focus_resolution_sticks_when_contested_without_a_transition() {
    let initial = tabs_by_index(vec![(
        0,
        vec![pane_in_tab(10, 0), focused(pane_in_tab(11, 0))],
    )]);
    let mut resolution = FocusResolution::default();
    resolution.fold_pane_update(&BTreeMap::new(), &initial);

    let contested = tabs_by_index(vec![(
        0,
        vec![focused(pane_in_tab(10, 0)), focused(pane_in_tab(11, 0))],
    )]);
    resolution.fold_pane_update(&contested, &contested);

    assert_eq!(resolution.focused_pane_id(Some(0)), Some(11));
}

#[test]
fn focus_resolution_holds_sticky_pane_after_focus_mark_drop() {
    let initial = tabs_by_index(vec![(
        1,
        vec![
            pane_in_tab(79, 1),
            pane_in_tab(141, 1),
            focused(pane_in_tab(200, 1)),
        ],
    )]);
    let mut resolution = FocusResolution::default();
    resolution.fold_pane_update(&BTreeMap::new(), &initial);

    let contested = tabs_by_index(vec![(
        1,
        vec![
            focused(pane_in_tab(79, 1)),
            focused(pane_in_tab(141, 1)),
            focused(pane_in_tab(200, 1)),
        ],
    )]);
    resolution.fold_pane_update(&contested, &contested);

    let mark_drop = tabs_by_index(vec![(
        1,
        vec![
            focused(pane_in_tab(79, 1)),
            focused(pane_in_tab(141, 1)),
            pane_in_tab(200, 1),
        ],
    )]);
    resolution.fold_pane_update(&contested, &mark_drop);

    assert_eq!(resolution.focused_pane_id(Some(1)), Some(200));
    assert_eq!(resolution.active_panes_payload().get(&1_u64), Some(&200));
}

#[test]
fn focus_resolution_real_flip_moves_off_sticky_pane() {
    let initial = tabs_by_index(vec![(
        0,
        vec![pane_in_tab(10, 0), focused(pane_in_tab(11, 0))],
    )]);
    let mut resolution = FocusResolution::default();
    resolution.fold_pane_update(&BTreeMap::new(), &initial);

    let next = tabs_by_index(vec![(
        0,
        vec![focused(pane_in_tab(10, 0)), pane_in_tab(11, 0)],
    )]);
    resolution.fold_pane_update(&initial, &next);

    assert_eq!(resolution.focused_pane_id(Some(0)), Some(10));
}

#[test]
fn focus_resolution_unresolves_ambiguous_simultaneous_card_flips() {
    let previous = tabs_by_index(vec![(0, vec![pane_in_tab(10, 0), pane_in_tab(11, 0)])]);
    let next = tabs_by_index(vec![(
        0,
        vec![focused(pane_in_tab(10, 0)), focused(pane_in_tab(11, 0))],
    )]);
    let mut resolution = FocusResolution::default();

    resolution.fold_pane_update(&previous, &next);

    assert_eq!(resolution.focused_pane_id(Some(0)), None);
}

// --- topology payload: the --topology wire view over the merged room ---

#[test]
fn topology_payload_serializes_projected_fields() {
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
    assert!(
        json["panes"][0].get("pane_command").is_none(),
        "pane_command is omitted when absent",
    );
}

#[test]
fn published_payload_requires_a_live_roster() {
    assert_eq!(
        published_topology_payload("rimz-test", 42, None, &BTreeMap::new()),
        None,
        "bootstrap pokes omit --topology until PaneUpdate supplies a live roster",
    );
    assert_eq!(
        published_topology_payload("rimz-test", 42, Some(&BTreeMap::new()), &BTreeMap::new()),
        None,
        "an empty live roster falls back to list-panes instead of publishing emptiness",
    );
}

#[test]
fn published_payload_over_a_merged_room_keeps_every_tab() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
    ]);
    let mut resized = pane_in_tab(10, 0);
    resized.pane_columns = Some(120);
    // A partial manifest carrying only tab 0 must not drop tabs 1 and 2.
    let merged = merged_room(&previous, &tabs_by_index(vec![(0, vec![resized.clone()])]));

    let payload = published_topology_payload("rimz-test", 42, Some(&merged), &BTreeMap::new())
        .expect("merged roster publishes");

    assert_eq!(
        payload.panes[0], resized,
        "the carried tab's field update propagates",
    );
    assert_eq!(
        payload
            .panes
            .iter()
            .map(|pane| pane.tab_position)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "tabs known only through earlier PaneUpdates are retained in order",
    );
}

#[test]
fn foreground_overlay_reaches_the_payload_without_replacing_the_spawn() {
    let foreground = BTreeMap::from([(7, "codex".to_owned())]);
    let roster = tabs(vec![PaneFields {
        id: 7,
        terminal_command: Some(
            "/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt".to_owned(),
        ),
        ..pane(7)
    }]);

    let payload = published_topology_payload("rimz-test", 42, Some(&roster), &foreground)
        .expect("live roster publishes");

    assert_eq!(payload.panes[0].pane_command.as_deref(), Some("codex"));
    assert_eq!(
        payload.panes[0].terminal_command.as_deref(),
        Some("/home/me/.cargo/bin/rimz agents exec codex --worktree-path /repo/wt"),
        "the exec spawn wrapper survives as terminal_command",
    );
    let json = serde_json::to_value(&payload).expect("payload serializes");
    assert_eq!(json["panes"][0]["pane_command"], "codex");
}

#[test]
fn launch_chrome_never_reaches_the_payload() {
    let launch = "rimz agents claude,codex --worktree=quality-pass";
    let foreground = BTreeMap::from([(7, launch.to_owned())]);
    let roster = tabs(vec![PaneFields {
        id: 7,
        pane_command: Some(launch.to_owned()),
        terminal_command: Some("zsh".to_owned()),
        ..pane(7)
    }]);

    let payload = published_topology_payload("rimz-test", 42, Some(&roster), &foreground)
        .expect("live roster publishes");

    assert_eq!(
        payload.panes[0].pane_command, None,
        "launch chrome is scrubbed from both the foreground map and a stale pane_command",
    );
    assert_eq!(payload.panes[0].terminal_command.as_deref(), Some("zsh"));
}

// --- foreground command tracking: classify events, retain across rebuilds ---

#[test]
fn launch_chrome_classifier_scopes_to_agents_launches() {
    assert!(command_args_are_launch_chrome(&[
        "rimz".to_owned(),
        "agents".to_owned(),
        "claude,codex".to_owned(),
        "--worktree=quality-pass".to_owned(),
    ]));
    assert!(command_args_are_launch_chrome(&[
        "/home/me/.cargo/bin/rimz agents claude,codex --worktree=quality-pass".to_owned(),
    ]));
    assert!(!command_args_are_launch_chrome(&[
        "rimz".to_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        "codex".to_owned(),
    ]));
    assert!(!command_args_are_launch_chrome(&[
        "rimz".to_owned(),
        "agents".to_owned(),
        "wait".to_owned(),
        "swift-otter".to_owned(),
    ]));
    assert!(!command_args_are_launch_chrome(&[
        "cargo".to_owned(),
        "build".to_owned(),
    ]));
}

#[test]
fn foreground_command_update_remembers_real_work_and_forgets_the_rest() {
    assert_eq!(
        foreground_command_update(
            &[
                "cargo".to_owned(),
                "clippy".to_owned(),
                "--all-targets".to_owned(),
            ],
            true,
        ),
        ForegroundCommandUpdate::Remember("cargo clippy --all-targets".to_owned()),
    );
    assert_eq!(
        foreground_command_update(&["cargo".to_owned(), "clippy".to_owned()], false),
        ForegroundCommandUpdate::Forget,
        "a foreground-end event clears the retained command",
    );
    assert_eq!(
        foreground_command_update(&["".to_owned()], true),
        ForegroundCommandUpdate::Forget,
        "an empty foreground event clears the retained command",
    );
    assert_eq!(
        foreground_command_update(&["rimz agents claude,codex".to_owned()], true),
        ForegroundCommandUpdate::Forget,
        "launch chrome clears instead of publishing as foreground work",
    );
}

#[test]
fn foreground_overlay_applies_and_clears_when_the_id_is_reused() {
    let command = joined_foreground_command(&["codex".to_owned(), "--foo".to_owned()])
        .expect("joined foreground");
    let mut foreground = BTreeMap::from([(9, command)]);

    let mut rebuilt = tabs(vec![pane(9)]);
    apply_foreground_commands(&mut rebuilt, &foreground);
    assert_eq!(rebuilt[&0][0].pane_command.as_deref(), Some("codex --foo"));

    // The foreground map is the source of truth: dropping the entry on close
    // means a reused pane id does not inherit the stale command.
    foreground.remove(&9);
    let mut reused = tabs(vec![pane(9)]);
    apply_foreground_commands(&mut reused, &foreground);
    assert_eq!(reused[&0][0].pane_command, None);
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
        "PaneClosed prunes the published roster immediately",
    );
}

// --- merged_room: partial Zellij manifests must not collapse the room ---

#[test]
fn merged_room_replaces_carried_tabs_exactly() {
    // The first manifest after load is the baseline, accepted whole.
    let first = tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)])]);
    assert_eq!(merged_room(&BTreeMap::new(), &first), first);

    // A carried tab overwrites wholesale: an updated field lands and a pane
    // that vanished from the carried tab is pruned.
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0), pane_in_tab(11, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);
    let mut resized = pane_in_tab(10, 0);
    resized.pane_columns = Some(120);
    let next = tabs_by_index(vec![
        (0, vec![resized.clone()]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);

    let merged = merged_room(&previous, &next);

    assert_eq!(merged, next, "carried tabs replace wholesale");
    assert!(
        !merged.values().flatten().any(|pane| pane.id == 11),
        "panes absent from a carried tab are pruned",
    );
}

#[test]
fn merged_room_retains_tabs_a_partial_manifest_omits() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
    ]);

    // A partial manifest carrying only tab 0 keeps tabs 1 and 2 intact.
    let merged = merged_room(
        &previous,
        &tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)])]),
    );
    assert_eq!(merged.keys().copied().collect::<Vec<_>>(), vec![0, 1, 2]);
    assert_eq!(merged.get(&2), previous.get(&2));

    // An empty manifest is "nothing carried", never a collapse.
    assert_eq!(merged_room(&previous, &BTreeMap::new()), previous);
}

#[test]
fn merged_room_retains_a_tab_a_partial_manifest_reports_empty() {
    // Zellij's serialization blip can carry an idle tab with an empty pane list
    // instead of omitting it. An empty entry is no roster, not a closed tab — a
    // live tab always holds a pane and panes leave through `PaneClosed` — so the
    // tab keeps what it last held rather than collapsing out of the room.
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
        (2, vec![pane_in_tab(30, 2)]),
    ]);

    let merged = merged_room(
        &previous,
        &tabs_by_index(vec![
            (0, vec![pane_in_tab(10, 0)]),
            (1, vec![]),
            (2, vec![]),
        ]),
    );

    assert_eq!(
        merged.keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2],
        "empty manifest entries retain their tabs instead of collapsing the room",
    );
    assert_eq!(merged.get(&1), previous.get(&1));
    assert_eq!(merged.get(&2), previous.get(&2));

    // The baseline drops empty entries too: an empty entry carries no roster.
    let baseline = merged_room(
        &BTreeMap::new(),
        &tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)]), (1, vec![])]),
    );
    assert_eq!(baseline.keys().copied().collect::<Vec<_>>(), vec![0]);
}

#[test]
fn merged_room_keeps_each_pane_under_one_tab() {
    // A renumbered tab stranded a copy of pane 10 in tab 1 while the live pane
    // lives in tab 0; a fresh manifest naming tab 0 evicts the stale copy and
    // drops the emptied tab. Without this the same pane leaks into both tabs.
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(10, 1)]),
    ]);

    let merged = merged_room(
        &previous,
        &tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)])]),
    );

    assert_eq!(merged.keys().copied().collect::<Vec<_>>(), vec![0]);
    assert_eq!(
        merged
            .values()
            .flatten()
            .filter(|pane| pane.id == 10)
            .count(),
        1,
        "a pane id lives under exactly one tab",
    );

    // The terminal and plugin id spaces are distinct: same id, both kept.
    let merged = merged_room(
        &tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)])]),
        &tabs_by_index(vec![(0, vec![pane_in_tab(10, 0), plugin_pane(10)])]),
    );
    assert_eq!(merged.get(&0).map(Vec::len), Some(2));
}

// --- opened_card_panes: which manifest panes earn a card-create poke ---

#[test]
fn opened_card_panes_reports_only_new_card_panes() {
    let previous = tabs(vec![pane(1)]);
    let mut floating = pane(5);
    floating.is_floating = true;
    let next = tabs(vec![
        pane(1),
        pane(2),
        sidebar_pane(3),
        plugin_pane(4),
        floating,
    ]);

    assert_eq!(
        opened_card_panes(&previous, &next),
        vec![pane(2)],
        "existing, sidebar, plugin, and floating panes never read as opens",
    );
}

#[test]
fn first_manifest_after_load_reports_no_opens() {
    let next = tabs(vec![pane(1), pane(2)]);
    assert!(
        opened_card_panes(&BTreeMap::new(), &next).is_empty(),
        "the first manifest names every pre-existing pane; the pull covers the room",
    );
}

#[test]
fn a_reused_terminal_id_is_not_an_open_but_a_new_id_space_is() {
    // Terminal and plugin panes have separate id spaces: a terminal pane whose
    // id collides with a known plugin pane is still a genuine open.
    let previous = tabs(vec![plugin_pane(7)]);
    let next = tabs(vec![plugin_pane(7), pane(7)]);

    assert_eq!(opened_card_panes(&previous, &next), vec![pane(7)]);
}

#[test]
fn opened_card_panes_over_a_merged_partial_manifest() {
    let previous = tabs_by_index(vec![
        (0, vec![pane_in_tab(10, 0)]),
        (1, vec![pane_in_tab(20, 1)]),
    ]);

    // A partial re-sending only tab 0 (here a focus flip) opens nothing: the
    // omitted tab 1 is retained, not treated as closed-then-reopened.
    let focus_only = merged_room(
        &previous,
        &tabs_by_index(vec![(0, vec![focused(pane_in_tab(10, 0))])]),
    );
    assert!(opened_card_panes(&previous, &focus_only).is_empty());

    // A genuinely new pane in the carried tab is the one open reported.
    let with_open = merged_room(
        &previous,
        &tabs_by_index(vec![(0, vec![pane_in_tab(10, 0), pane_in_tab(11, 0)])]),
    );
    assert_eq!(
        opened_card_panes(&previous, &with_open),
        vec![pane_in_tab(11, 0)],
    );
}

// --- stranded_sidebar_pane: tab-switch classification reports the sidebar ---

#[test]
fn stranded_sidebar_pane_classifies_the_active_tab() {
    // Sidebar holds focus while a live working sibling exists: stranded.
    assert_eq!(
        stranded_sidebar_pane(&tabs(vec![focused(sidebar_pane(1)), pane(2)]), Some(0)),
        Some(1),
    );
    // Work holds focus: nothing to correct.
    assert_eq!(
        stranded_sidebar_pane(&tabs(vec![sidebar_pane(1), focused(pane(2))]), Some(0)),
        None,
    );
    // Sidebar holds focus but the only sibling is held, not live work.
    let mut held = pane(2);
    held.is_held = true;
    assert_eq!(
        stranded_sidebar_pane(&tabs(vec![focused(sidebar_pane(1)), held]), Some(0)),
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
fn focus_correction_clears_when_a_fresh_jump_lands_on_work() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let manifest = tabs_by_index(vec![
        (0, vec![pane(1)]),
        (1, vec![sidebar_pane(10), focused(pane(11))]),
    ]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), true, 1_001),
        CorrectionAction::Clear,
        "a fresh manifest from an explicit jump proves work holds focus before the settle deadline",
    );
    assert_eq!(correction.next_deadline(), None);
}

#[test]
fn focus_correction_broadcasts_a_stranded_sidebar_at_the_deadline() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let manifest = tabs_by_index(vec![
        (0, vec![pane(1)]),
        (1, vec![focused(sidebar_pane(10)), pane(11)]),
    ]);

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
fn focus_correction_waits_out_the_window_on_a_fresh_sidebar_manifest() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let manifest = tabs_by_index(vec![(1, vec![focused(sidebar_pane(10)), pane(11)])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), true, 1_001),
        CorrectionAction::Wait,
        "a fresh manifest still showing sidebar focus may predate an explicit jump's focus mark",
    );
    assert_eq!(
        correction.resolve(&manifest, Some(1), false, 1_000 + FOCUS_SETTLE_MS),
        CorrectionAction::Broadcast(10),
    );
}

#[test]
fn focus_correction_retargets_to_the_latest_switch() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);
    correction.on_active_tab_change(Some(1), Some(2), 1_050);

    assert_eq!(correction.next_deadline(), Some(1_050 + FOCUS_SETTLE_MS));

    let manifest = tabs_by_index(vec![
        (1, vec![focused(sidebar_pane(10)), pane(11)]),
        (2, vec![focused(sidebar_pane(20)), pane(21)]),
    ]);

    assert_eq!(
        correction.resolve(&manifest, Some(2), false, 1_050 + FOCUS_SETTLE_MS),
        CorrectionAction::Broadcast(20),
    );
}

#[test]
fn focus_correction_clears_when_the_target_tab_closes() {
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
fn focus_correction_does_not_broadcast_without_live_work() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change(Some(0), Some(1), 1_000);

    let mut held_work = pane(11);
    held_work.is_held = true;
    let manifest = tabs_by_index(vec![(1, vec![focused(sidebar_pane(10)), held_work])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), false, 1_000 + FOCUS_SETTLE_MS),
        CorrectionAction::Clear,
    );
}

#[test]
fn focus_correction_clears_on_a_tab_renumber_under_the_same_pane() {
    let mut correction = FocusCorrection::default();
    correction.on_active_tab_change_with_focus(Some(2), Some(1), Some(42), 1_000);

    let manifest = tabs_by_index(vec![(1, vec![focused(sidebar_pane(42)), pane(43)])]);

    assert_eq!(
        correction.resolve(&manifest, Some(1), true, 1_001),
        CorrectionAction::Clear,
        "the same focused pane under a new tab position is a renumber, not navigation",
    );
    assert_eq!(correction.next_deadline(), None);
}

// --- PokePolicy: immediate change, duplicate floor, settle, keepalive ---

#[test]
fn first_manifest_is_a_baseline_not_a_poke() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    assert_eq!(policy.due(0), Vec::<Poke>::new());
    assert_eq!(
        policy.next_wake_at(),
        KEEPALIVE_MS,
        "only the keepalive is armed after the baseline",
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
        "a sidebar-changing manifest should wake the producer now",
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
        "the post-change settle poke is armed after the immediate one",
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
fn same_pane_optimistic_pokes_are_floored_and_settle() {
    let mut policy = PokePolicy::new(0);

    assert!(policy.optimistic_pane_poke_allowed(7, 10));
    policy.accept_optimistic_pane_poke(7, 10);
    assert_eq!(
        policy.due(10),
        Vec::<Poke>::new(),
        "the immediate command-changed poke is emitted outside the policy",
    );

    assert!(!policy.optimistic_pane_poke_allowed(7, 50));
    policy.on_signal(50);
    assert_eq!(policy.due(10 + POKE_FLOOR_MS - 1), Vec::<Poke>::new());
    assert_eq!(
        policy.due(10 + POKE_FLOOR_MS),
        vec![Poke::Changed],
        "same-pane duplicates collapse into one floored verifying pull",
    );
    assert_eq!(
        policy.due(10 + POKE_FLOOR_MS + SETTLE_POKE_MS),
        vec![Poke::Changed],
        "the floored pull keeps the normal settled read",
    );
}

#[test]
fn optimistic_poke_floor_is_per_pane() {
    let mut policy = PokePolicy::new(0);

    policy.accept_optimistic_pane_poke(7, 10);

    assert!(
        policy.optimistic_pane_poke_allowed(8, 50),
        "one pane's command churn does not throttle another pane's first change",
    );
    assert!(!policy.optimistic_pane_poke_allowed(7, 50));
}

#[test]
fn closed_pane_clears_its_optimistic_poke_floor() {
    let mut policy = PokePolicy::new(0);

    policy.accept_optimistic_pane_poke(7, 10);
    assert!(!policy.optimistic_pane_poke_allowed(7, 50));

    policy.forget_pane(7);

    assert!(
        policy.optimistic_pane_poke_allowed(7, 50),
        "a reused pane id starts with a clean command-poke floor",
    );
}

#[test]
fn duplicate_changes_inside_the_floor_defer_once_and_never_drop() {
    let mut policy = PokePolicy::new(0);
    policy.on_signal(100);
    assert_eq!(policy.due(100), vec![Poke::Changed]);

    // A split or command handoff fans out several events. The first refreshed
    // panes already, so duplicates inside the 100ms floor wait and collapse
    // into one follow-up — deferred, never dropped.
    policy.on_signal(150);
    policy.on_signal(180);
    assert_eq!(policy.due(199), Vec::<Poke>::new());
    assert_eq!(
        policy.next_wake_at(),
        100 + POKE_FLOOR_MS,
        "the follow-up is armed for the floor's end",
    );
    assert_eq!(policy.due(200), vec![Poke::Changed]);
    assert_eq!(
        policy.next_wake_at(),
        200 + SETTLE_POKE_MS,
        "the duplicate-burst poke gets its own settled read",
    );
    assert_eq!(policy.due(201), Vec::<Poke>::new());
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
        "the next keepalive re-arms from the firing instant",
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
        "a pending change wakes immediately before the keepalive",
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
        "later deadline: the armed timer wakes first",
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
        "after the earlier timer fires the keepalive re-arms",
    );
    gate.on_fire(60_000);
    assert!(
        gate.should_arm(120_000),
        "the fired keepalive chains forward",
    );

    // The timer superseded at 30_200 fires late, while the 120s chain is
    // outstanding: it must read as stale — clearing the mark here would arm
    // a duplicate for a deadline already covered, and since every fire
    // re-arms one successor, the duplicate would be a chain that never
    // collapses.
    gate.on_fire(60_005);
    assert!(
        !gate.should_arm(120_000),
        "a stale fire arms no duplicate chain",
    );
}

#[test]
fn focus_chord_parses_alt_and_ctrl_with_separators() {
    assert_eq!(
        FocusChord::parse("Alt+p"),
        Some(FocusChord {
            modifier: ChordModifier::Alt,
            key: 'p',
        }),
    );
    // `-` is an accepted separator, the modifier is case-insensitive, and the
    // aliases match the host grammar.
    assert_eq!(
        FocusChord::parse("ctrl-S"),
        Some(FocusChord {
            modifier: ChordModifier::Ctrl,
            key: 'S',
        }),
    );
    assert_eq!(
        FocusChord::parse("  m+j  "),
        Some(FocusChord {
            modifier: ChordModifier::Alt,
            key: 'j',
        }),
    );
}

#[test]
fn focus_chord_rejects_malformed_shapes() {
    // No separator, unknown modifier, multi-char key, and a non-graphic key
    // all skip the bind rather than register something broken.
    assert_eq!(FocusChord::parse("p"), None);
    assert_eq!(FocusChord::parse("super+p"), None);
    assert_eq!(FocusChord::parse("alt+pp"), None);
    assert_eq!(FocusChord::parse("alt+ "), None);
    assert_eq!(FocusChord::parse(""), None);
}

#[test]
fn focus_keybind_kdl_targets_plugin_id_in_normal_and_locked_modes() {
    let kdl = focus_keybind_kdl(
        FocusChord {
            modifier: ChordModifier::Alt,
            key: 'p',
        },
        42,
    );

    assert_eq!(kdl.matches("bind \"Alt p\"").count(), 2);
    assert_eq!(kdl.matches("MessagePluginId 42").count(), 2);
    assert_eq!(kdl.matches("name \"rimz:focus_sidebar\"").count(), 2);
    assert!(kdl.contains("normal {"));
    assert!(kdl.contains("locked {"));
    assert!(!kdl.contains("MessagePlugin \""));
    assert!(!kdl.contains("plugin_url"));
}

#[test]
fn focus_keybind_kdl_formats_ctrl_chords() {
    let kdl = focus_keybind_kdl(
        FocusChord {
            modifier: ChordModifier::Ctrl,
            key: 's',
        },
        7,
    );

    assert_eq!(kdl.matches("bind \"Ctrl s\"").count(), 2);
    assert!(kdl.contains("MessagePluginId 7"));
}
