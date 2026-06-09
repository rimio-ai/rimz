use super::*;

#[test]
fn process_state_marks_zombie_and_persistent_uninterruptible_sleep_as_stuck() {
    assert_eq!(
        process_state_from_stat(Some('Z'), None),
        Some(ProcessState::Stuck)
    );
    assert_eq!(process_state_from_stat(Some('D'), None), None);
    assert_eq!(
        process_state_from_stat(Some('D'), Some('D')),
        Some(ProcessState::Stuck)
    );
    assert_eq!(process_state_from_stat(Some('R'), Some('D')), None);
    assert_eq!(process_state_from_stat(None, Some('D')), None);
}

#[test]
fn cached_root_pid_restores_only_a_live_unchanged_binding() {
    let entry = binding_entry(42, 777, "zsh");
    let alive = |pid: u32| (pid == 42).then_some(777);
    // Hit: pid alive with the recorded starttime.
    assert_eq!(cached_root_pid(&entry, &alive), Some(42));
    // Pid gone.
    assert_eq!(cached_root_pid(&entry, &|_| None), None);
    // Pid recycled: alive, but a different starttime — never a stranger's pid.
    assert_eq!(cached_root_pid(&entry, &|_| Some(778)), None);
    // An old cache shape with no binding recorded re-derives.
    let unbound = MetricsSampleEntry {
        pane_pid: None,
        root_start_ticks: None,
        ..binding_entry(42, 777, "zsh")
    };
    assert_eq!(cached_root_pid(&unbound, &alive), None);
}

/// The due set `enrich_pane_metrics` builds when every sampleable pane is due
/// — the cold-cache shape the restore tests exercise.
fn all_sampleable_due(frame: &crate::sidebar::frame::PaneFrame) -> HashSet<String> {
    frame
        .pane_states()
        .filter(|pane| pane_sampleable(pane))
        .map(|pane| pane.pane_id.to_string())
        .collect()
}

#[test]
fn stable_panes_restore_their_bindings_and_skip_the_walk() {
    // The steady-state contract: every due pidless pane hits its guarded
    // binding, so the tick walks zero processes.
    let panes = vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("node claude"), Some("/repo")),
    ];
    let mut frame = frame_from_panes(panes.clone());
    let mut prior = MetricsSampleCache::default();
    prior
        .entries
        .insert(panes[0].pane_id.to_string(), binding_entry(42, 700, "zsh"));
    prior.entries.insert(
        panes[1].pane_id.to_string(),
        binding_entry(43, 701, "node claude"),
    );
    let starts = |pid: u32| match pid {
        42 => Some(700),
        43 => Some(701),
        _ => None,
    };

    let due = all_sampleable_due(&frame);
    let needs_walk = restore_cached_bindings(&mut frame, &prior, &due, &starts);

    assert!(!needs_walk, "an all-hit room never walks the process table");
    assert_eq!(state(&frame, "terminal_1").current.pid, Some(42));
    assert_eq!(state(&frame, "terminal_2").current.pid, Some(43));
}

#[test]
fn binding_misses_drive_the_walk_and_unbindable_panes_do_not() {
    // A due pidless pane with no usable binding needs the walk…
    let mut missing = frame_from_panes(vec![pane("terminal_2", Some("zsh"), None)]);
    let due = all_sampleable_due(&missing);
    assert!(restore_cached_bindings(
        &mut missing,
        &MetricsSampleCache::default(),
        &due,
        &|_| None,
    ));
    assert_eq!(
        state(&missing, "terminal_2").current.pid,
        None,
        "a miss restores nothing"
    );

    // …while panes the walk could never bind — no command, sidebar chrome —
    // sit outside the sampleable due set, and a natively-pidded (tmux) pane
    // is left alone.
    let mut pidded = pane("terminal_9", Some("zsh"), None);
    pidded.pane_pid = Some(9);
    let mut inert = frame_from_panes(vec![
        pane("terminal_3", None, None),
        pane(
            "terminal_4",
            Some(crate::mux::zellij::SIDEBAR_PANE_NAME),
            None,
        ),
        pidded,
    ]);
    let due = all_sampleable_due(&inert);
    assert!(!restore_cached_bindings(
        &mut inert,
        &MetricsSampleCache::default(),
        &due,
        &|_| None,
    ));
    assert_eq!(state(&inert, "terminal_9").current.pid, Some(9));
}

#[test]
fn unbound_panes_between_samples_never_drive_the_walk() {
    // The mixed-room case: a hot pane is due while an idle unbound pane's
    // fresh entry is not — the missing binding must not drag the table walk
    // back onto every hot tick.
    let panes = vec![
        pane("terminal_1", Some("cargo build"), Some("/repo")),
        pane("terminal_2", Some("zsh"), Some("/repo")),
    ];
    let mut frame = frame_from_panes(panes.clone());
    let mut prior = MetricsSampleCache::default();
    prior.entries.insert(
        panes[0].pane_id.to_string(),
        binding_entry(42, 700, "cargo build"),
    );
    prior.entries.insert(
        panes[1].pane_id.to_string(),
        unbound_entry(Some("zsh".to_owned()), 1_000),
    );
    let due = HashSet::from([panes[0].pane_id.to_string()]);

    let needs_walk =
        restore_cached_bindings(&mut frame, &prior, &due, &|pid| (pid == 42).then_some(700));

    assert!(
        !needs_walk,
        "the due pane restored its binding; the unbound idle pane backs off"
    );
    assert_eq!(state(&frame, "terminal_1").current.pid, Some(42));
    assert_eq!(state(&frame, "terminal_2").current.pid, None);
}
