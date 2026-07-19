use super::*;
use std::collections::BTreeMap;

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

fn raw_hash_from_tabs(tabs: &BTreeMap<usize, Vec<PaneFields>>) -> u64 {
    raw_stable_hash(tabs.iter().flat_map(|(tab, panes)| {
        panes
            .iter()
            .map(move |pane| (*tab, RawStablePaneFields::from_projected(pane)))
    }))
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
    let manifest = vec![pane(1), pane(2)];
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
        &[pane(1)],
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
fn published_topology_payload_carries_event_enrichment() {
    let enriched = PaneFields {
        pane_command: Some("zsh".to_owned()),
        pane_cwd: Some("/repo/main".to_owned()),
        pane_pid: Some(101),
        ..pane(1)
    };
    let manifest = vec![enriched];
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

// --- PokePolicy: immediate change, duplicate floor, settle, keepalive ---

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
    policy.on_signal(KEEPALIVE_MS);
    let pokes = policy.due(KEEPALIVE_MS);
    assert!(pokes.contains(&Poke::Changed));
    assert!(pokes.contains(&Poke::Alive));
}

#[test]
fn next_wake_is_the_earlier_of_change_and_keepalive() {
    let mut policy = PokePolicy::new(0);
    policy.on_signal(40);
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
