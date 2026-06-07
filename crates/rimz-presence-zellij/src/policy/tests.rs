use super::*;

fn pane(id: u32) -> PaneFields {
    PaneFields {
        id,
        is_plugin: false,
        is_focused: false,
        is_suppressed: false,
        exited: false,
        is_held: false,
        title: format!("pane-{id}"),
        terminal_command: Some("zsh".to_owned()),
    }
}

fn tabs(panes: Vec<PaneFields>) -> BTreeMap<usize, Vec<PaneFields>> {
    BTreeMap::from([(0, panes)])
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

// --- switched_tab_focus_target: only tab-switch sidebar focus is corrected ---

#[test]
fn switched_tab_focus_target_moves_sidebar_focus_to_working_pane() {
    let mut sidebar = pane(1);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let work = pane(2);

    assert_eq!(
        switched_tab_focus_target(&tabs(vec![sidebar, work]), Some(0)),
        Some(2),
    );
}

#[test]
fn switched_tab_focus_target_leaves_working_focus_alone() {
    let mut sidebar = pane(1);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    let mut work = pane(2);
    work.is_focused = true;

    assert_eq!(
        switched_tab_focus_target(&tabs(vec![sidebar, work]), Some(0)),
        None,
    );
}

#[test]
fn switched_tab_focus_target_requires_a_live_working_pane() {
    let mut sidebar = pane(1);
    sidebar.title = SIDEBAR_PANE_TITLE.to_owned();
    sidebar.is_focused = true;
    let mut held_work = pane(2);
    held_work.is_held = true;

    assert_eq!(
        switched_tab_focus_target(&tabs(vec![sidebar, held_work]), Some(0)),
        None,
    );
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
