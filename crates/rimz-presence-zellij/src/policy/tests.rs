use super::*;

fn pane(id: u32) -> PaneFields {
    PaneFields {
        id,
        is_plugin: false,
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
        pane_cwd: None,
        pane_pid: None,
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

// --- manifest_hash: the projected stable subset folds; the rest does not ---

#[test]
fn identical_manifests_hash_equal() {
    let a = manifest_hash(&tabs(vec![pane(1), pane(2)]));
    let b = manifest_hash(&tabs(vec![pane(1), pane(2)]));
    assert_eq!(a, b);
}

#[test]
fn every_stable_field_changes_the_hash() {
    type Mutate = fn(&mut PaneFields);
    let base = manifest_hash(&tabs(vec![pane(1)]));
    let cases: &[(&str, Mutate)] = &[
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
            manifest_hash(&tabs(vec![changed])),
            "{field} must fold into the roster hash",
        );
    }
    // Opening or closing a pane changes the set of folded ids.
    assert_ne!(
        base,
        manifest_hash(&tabs(vec![pane(1), pane(2)])),
        "pane count must fold into the roster hash",
    );
}

#[test]
fn excluded_fields_hold_the_hash() {
    let base = manifest_hash(&tabs(vec![pane(1)]));

    // Titles mutate per output line and the foreground command publishes
    // through CommandChanged; neither belongs in the roster hash.
    let mut renamed = pane(1);
    renamed.title = "line-mutated agent title".to_owned();
    assert_eq!(
        base,
        manifest_hash(&tabs(vec![renamed])),
        "title is excluded by projection",
    );

    let mut foreground = pane(1);
    foreground.pane_command = Some("codex".to_owned());
    assert_eq!(
        base,
        manifest_hash(&tabs(vec![foreground])),
        "pane_command is excluded by projection",
    );

    let mut with_cwd = pane(1);
    with_cwd.pane_cwd = Some("/repo/main".to_owned());
    assert_eq!(
        base,
        manifest_hash(&tabs(vec![with_cwd])),
        "pane_cwd is excluded by projection",
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

    let mut resized = pane(1);
    resized.pane_columns = Some(120);
    assert_ne!(
        raw_hash_from_tabs(&base),
        raw_hash_from_tabs(&tabs(vec![resized])),
        "geometry changes are stable pane state",
    );
}

#[test]
fn published_topology_payload_carries_session_focus_without_pane_marks() {
    let manifest = tabs(vec![pane(1), pane(2)]);
    let payload = published_topology_payload("rimz-test", 42, None, Some(2), None, &manifest)
        .expect("topology payload publishes");

    assert_eq!(payload.focused_pane, Some(2));
    let json = serde_json::to_value(payload).expect("topology serializes");
    assert!(json["panes"].as_array().unwrap().iter().all(|pane| {
        pane.as_object()
            .is_some_and(|pane| !pane.contains_key("is_focused"))
    }));
}

#[test]
fn published_topology_payload_carries_writer() {
    let payload = published_topology_payload(
        "rimz-test",
        42,
        Some(TopologyWriter {
            plugin_id: 9,
            loaded_at_ms: 1000,
            build: Some("wasm-build".to_owned()),
            config: Some("config-hash".to_owned()),
        }),
        None,
        None,
        &tabs(vec![pane(1)]),
    )
    .expect("topology payload publishes");

    assert_eq!(
        payload.writer,
        Some(TopologyWriter {
            plugin_id: 9,
            loaded_at_ms: 1000,
            build: Some("wasm-build".to_owned()),
            config: Some("config-hash".to_owned()),
        }),
    );
}

#[test]
fn panes_needing_pid_selects_unprobed_live_terminals() {
    let implicit = pane(1);
    let spawn_command = pane(2);
    let with_pid = pane(3);
    let probed = pane(4);
    let plugin = plugin_pane(5);
    let mut floating = pane(6);
    floating.is_floating = true;
    let mut held = pane(7);
    held.is_held = true;
    let room = tabs(vec![
        implicit,
        spawn_command,
        with_pid,
        probed,
        plugin,
        floating,
        held,
    ]);
    let pids = BTreeMap::from([(3, 300)]);
    let probed = BTreeSet::from([4]);

    assert_eq!(panes_needing_pid(&room, &pids, &probed), vec![1, 2]);
}

#[test]
fn apply_foreground_commands_uses_foreground_then_shell_and_enrichment() {
    let mut room = tabs(vec![pane(1), pane(2)]);
    let foreground = BTreeMap::from([(1, "vim README.md".to_owned())]);
    let shell = BTreeMap::from([(1, "zsh".to_owned()), (2, "fish".to_owned())]);
    let cwd = BTreeMap::from([(1, "/repo/main".to_owned()), (2, "/repo/side".to_owned())]);
    let pids = BTreeMap::from([(1, 101), (2, 202)]);

    apply_foreground_commands(&mut room, &foreground, &shell, &cwd, &pids);

    let panes = room.get(&0).expect("tab exists");
    assert_eq!(panes[0].pane_command.as_deref(), Some("vim README.md"));
    assert_eq!(panes[0].pane_cwd.as_deref(), Some("/repo/main"));
    assert_eq!(panes[0].pane_pid, Some(101));
    assert_eq!(panes[1].pane_command.as_deref(), Some("fish"));
    assert_eq!(panes[1].pane_cwd.as_deref(), Some("/repo/side"));
    assert_eq!(panes[1].pane_pid, Some(202));
}

#[test]
fn published_topology_payload_carries_event_enrichment() {
    let mut manifest = tabs(vec![pane(1)]);
    apply_foreground_commands(
        &mut manifest,
        &BTreeMap::new(),
        &BTreeMap::from([(1, "zsh".to_owned())]),
        &BTreeMap::from([(1, "/repo/main".to_owned())]),
        &BTreeMap::from([(1, 101)]),
    );
    let payload = published_topology_payload("rimz-test", 42, None, Some(1), None, &manifest)
        .expect("topology payload publishes");
    let encoded = serde_json::to_value(payload).expect("payload serializes");

    assert_eq!(encoded["panes"][0]["pane_command"], "zsh");
    assert_eq!(encoded["panes"][0]["pane_cwd"], "/repo/main");
    assert_eq!(encoded["panes"][0]["pane_pid"], 101);
}

#[test]
fn command_updates_distinguish_foreground_shell_and_empty() {
    assert_eq!(
        foreground_command_update(&["vim".to_owned()], true),
        ForegroundCommandUpdate::Remember("vim".to_owned()),
    );
    assert_eq!(
        foreground_command_update(&["zsh".to_owned()], false),
        ForegroundCommandUpdate::Shell("zsh".to_owned()),
    );
    assert_eq!(
        foreground_command_update(&[], false),
        ForegroundCommandUpdate::Forget,
    );

    let mut room = tabs(vec![pane(1)]);
    let mut foreground = BTreeMap::from([(1, "sleep 5".to_owned())]);
    let shell = BTreeMap::from([(1, "zsh".to_owned())]);

    apply_foreground_commands(
        &mut room,
        &foreground,
        &shell,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );
    assert_eq!(
        room.get(&0).unwrap()[0].pane_command.as_deref(),
        Some("sleep 5"),
    );
    foreground.remove(&1);
    apply_foreground_commands(
        &mut room,
        &foreground,
        &shell,
        &BTreeMap::new(),
        &BTreeMap::new(),
    );

    let pane = &room.get(&0).unwrap()[0];
    assert_eq!(pane.pane_command.as_deref(), Some("zsh"));
}

#[test]
fn pane_fields_deserialize_legacy_payload_without_pid() {
    let mut with_pid = pane(1);
    with_pid.pane_pid = Some(101);
    let encoded = serde_json::to_value(with_pid).expect("pane serializes");
    let decoded: PaneFields = serde_json::from_value(encoded).expect("pane round-trips");
    assert_eq!(decoded.pane_pid, Some(101));

    let mut legacy = serde_json::to_value(pane(2)).expect("pane serializes");
    legacy.as_object_mut().unwrap().remove("pane_pid");
    let decoded: PaneFields = serde_json::from_value(legacy).expect("legacy pane parses");
    assert_eq!(decoded.pane_pid, None);
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

    // A partial re-sending only tab 0 opens nothing: the omitted tab 1 is
    // retained, not treated as closed-then-reopened.
    let partial = merged_room(
        &previous,
        &tabs_by_index(vec![(0, vec![pane_in_tab(10, 0)])]),
    );
    assert!(opened_card_panes(&previous, &partial).is_empty());

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
