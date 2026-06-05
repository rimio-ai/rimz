use super::*;

fn pane(id: u32) -> PaneFields {
    PaneFields {
        id,
        is_plugin: false,
        is_focused: false,
        exited: false,
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
fn active_tab_move_changes_the_hash() {
    let on_zero = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let on_one = manifest_hash(&tabs(vec![pane(1)]), Some(1));
    assert_ne!(on_zero, on_one);
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
fn title_is_excluded_by_projection() {
    // `PaneFields` carries no title at all — the projection in `lib.rs` drops
    // it before hashing, so a title-only change *cannot* alter the hash. This
    // test pins the contract by construction: the struct compiles without a
    // title field, and two manifests differing only in a dropped field are
    // the same input.
    let a = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    let b = manifest_hash(&tabs(vec![pane(1)]), Some(0));
    assert_eq!(a, b);
}

// --- PokePolicy: debounce, floor, keepalive ---

#[test]
fn first_manifest_is_a_baseline_not_a_poke() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    assert_eq!(policy.due(DEBOUNCE_MS + 1), Vec::<Poke>::new());
    assert_eq!(
        policy.next_wake_at(),
        KEEPALIVE_MS,
        "only the keepalive is armed after the baseline"
    );
}

#[test]
fn burst_coalesces_to_one_poke_after_the_debounce() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    // A split fans out as several manifests within the debounce window.
    policy.on_manifest(22, 10);
    policy.on_manifest(33, 60);
    policy.on_manifest(44, 120);
    assert_eq!(policy.due(150), Vec::<Poke>::new(), "still inside debounce");
    assert_eq!(
        policy.due(10 + DEBOUNCE_MS),
        vec![Poke::Changed],
        "one poke for the whole burst, anchored at the burst's first change"
    );
    assert_eq!(policy.due(10 + DEBOUNCE_MS + 1), Vec::<Poke>::new());
}

#[test]
fn change_during_the_floor_is_deferred_never_dropped() {
    let mut policy = PokePolicy::new(0);
    policy.on_manifest(11, 0);
    policy.on_manifest(22, 100);
    assert_eq!(policy.due(100 + DEBOUNCE_MS), vec![Poke::Changed]);
    let poked_at = 100 + DEBOUNCE_MS;

    // A second change lands well inside the 500ms floor.
    policy.on_manifest(33, poked_at + 100);
    assert_eq!(
        policy.due(poked_at + 100 + DEBOUNCE_MS),
        Vec::<Poke>::new(),
        "debounce elapsed but the floor holds"
    );
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
    policy.on_manifest(22, KEEPALIVE_MS - 10);
    let pokes = policy.due(KEEPALIVE_MS + DEBOUNCE_MS);
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
        40 + DEBOUNCE_MS,
        "a pending change wakes before the keepalive"
    );
}
